use super::*;
use android_tools::logcat::{self, Buffer, Decoder, Entry, FilterContext, Level, Process, Query};
use chrono::Utc;
use editor::{Editor, MultiBufferOffset, SelectionEffects, ToOffset as _};
use futures::{AsyncReadExt as _, SinkExt as _, channel::mpsc};
use gpui::{
    Bounds, ClipboardItem, FollowMode, Hsla, KeyContext, ListAlignment, ListState,
    PathPromptOptions, Point, ScrollHandle, TextRun, deferred, list, point,
};
use serde::{Deserialize, Serialize};
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    io::Read as _,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
use theme_settings::ThemeSettings;
use ui::{ButtonLike, ScrollAxes, ScrollableHandle, Scrollbars, WithScrollbar};
use ui_input::{ErasedEditorEvent, InputField};
use unicode_width::UnicodeWidthStr as _;
use util::command::Stdio;
use workspace::{
    SplitDirection,
    item::{Item, ItemEvent},
};

actions!(
    android_logcat,
    [
        /// Shows or hides the Logcat panel.
        Toggle,
        /// Opens another Logcat tab.
        NewViewer,
        /// Hides Find.
        CloseFind,
        /// Selects the next query suggestion.
        NextSuggestion,
        /// Selects the previous query suggestion.
        PreviousSuggestion,
        /// Inserts the selected query suggestion.
        AcceptSuggestion,
        /// Dismisses query suggestions.
        DismissSuggestions,
        /// Freezes or resumes the Logcat display while capture continues.
        Pause,
        /// Clears the current Logcat view.
        Clear,
        /// Restarts Logcat capture.
        Restart,
        /// Focuses the Logcat filter.
        FocusFilter,
        /// Focuses the Logcat text search.
        Find,
        /// Finds the next matching Logcat message.
        FindNext,
        /// Finds the previous matching Logcat message.
        FindPrevious,
        /// Copies selected Logcat messages.
        Copy,
        /// Selects all visible Logcat messages.
        SelectAll,
    ]
);

pub(super) fn open(
    workspace: &mut Workspace,
    root: PathBuf,
    serial: Option<String>,
    targets: Vec<AndroidTarget>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some(panel) = workspace.panel::<LogcatPanel>(cx) {
        panel.update(cx, |panel, cx| {
            panel.show_logs(root, serial, targets, window, cx)
        });
        workspace.focus_panel::<LogcatPanel>(window, cx);
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Preferences {
    #[serde(default)]
    version: u32,
    filter: String,
    match_case: bool,
    wrap: bool,
    compact: bool,
    fold_stacktraces: bool,
    capacity: usize,
    saved_filters: Vec<String>,
    serial: Option<String>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            version: 1,
            filter: "package:mine".into(),
            match_case: false,
            wrap: false,
            compact: false,
            fold_stacktraces: false,
            capacity: logcat::DEFAULT_CAPACITY,
            saved_filters: Vec::new(),
            serial: None,
        }
    }
}

#[derive(Clone)]
struct LogcatDevice {
    device: Device,
    release: String,
    api: String,
    name: String,
    packages: HashMap<u32, Vec<String>>,
    event_tags: HashMap<u32, String>,
}

impl LogcatDevice {
    fn label(&self) -> String {
        let version = if self.release.is_empty() {
            String::new()
        } else {
            format!(" · Android {} / API {}", self.release, self.api)
        };
        let state = if self.device.is_available() {
            String::new()
        } else {
            format!(" · {}", self.device.state)
        };
        format!("{} ({}){version}{state}", self.name, self.device.serial)
    }
}

const INLINE_FILTER_LIMIT: usize = 256;
const INLINE_FILTER_BYTES: usize = 64 * 1024;
const CAPTURE_BATCH_LIMIT: usize = 256;
const CAPTURE_BATCH_BYTES: usize = 128 * 1024;

#[derive(Default)]
struct WorkCancellation(Arc<AtomicBool>);

impl Drop for WorkCancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[derive(Clone)]
struct PaintedLogcatText {
    bounds: Bounds<Pixels>,
    layout: gpui::TextLayout,
}

pub(super) struct LogcatView {
    workspace: WeakEntity<Workspace>,
    controller: Option<WeakEntity<project_context::ProjectContextController>>,
    project: Entity<Project>,
    root: PathBuf,
    targets: Vec<AndroidTarget>,
    focus_handle: FocusHandle,
    filter_input: Entity<InputField>,
    search_input: Entity<InputField>,
    preferences: Preferences,
    query: Query,
    filter_error: Option<String>,
    devices: Vec<LogcatDevice>,
    device_error: Option<String>,
    process_error: Option<String>,
    processes: Vec<Process>,
    project_packages: Vec<String>,
    completions: Vec<String>,
    completion_range: std::ops::Range<usize>,
    completion_index: usize,
    completion_scroll: ScrollHandle,
    suppress_completion: bool,
    search_visible: bool,
    horizontal_scroll: ScrollHandle,
    unwrapped_columns: usize,
    measured_line_width: Rc<Cell<Pixels>>,
    buffers: String,
    buffer: Buffer,
    cursor: Option<Arc<Entry>>,
    paused: Option<Vec<Arc<Entry>>>,
    visible: Vec<Arc<Entry>>,
    selected: HashSet<u64>,
    selection_anchor: Option<u64>,
    text_selection: Option<((u64, usize), (u64, usize))>,
    selecting_text: bool,
    text_layouts: Rc<RefCell<HashMap<u64, PaintedLogcatText>>>,
    search: Option<logcat::Search>,
    pending_search: Option<logcat::Search>,
    search_tail: Vec<u64>,
    search_case: bool,
    search_regex: bool,
    search_error: Option<String>,
    search_matches: Vec<usize>,
    search_position: Option<usize>,
    list_state: ListState,
    file: Option<PathBuf>,
    connected: bool,
    capturing: bool,
    stopped: bool,
    status: String,
    error: Option<String>,
    stream_task: Option<Task<()>>,
    device_task: Option<Task<()>>,
    device_cancellation: Option<WorkCancellation>,
    device_owner: Option<android_tools::project_context::ActiveProjectToken>,
    watching_requested: bool,
    control_task: Option<Task<()>>,
    persist_task: Option<Task<()>>,
    filter_task: Option<Task<()>>,
    rebuild_task: Option<Task<()>>,
    search_task: Option<Task<()>>,
    source_task: Option<Task<()>>,
    source_cancellation: Option<WorkCancellation>,
    source_entry: Option<u64>,
    source_locations: Vec<(project::ProjectPath, u32)>,
    rebuild_cancellation: Option<WorkCancellation>,
    search_cancellation: Option<WorkCancellation>,
    rebuild_tail: Vec<Arc<Entry>>,
    rebuild_tail_columns: usize,
    _subscriptions: Vec<Subscription>,
}

impl LogcatView {
    pub(super) fn new(
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        root: PathBuf,
        serial: Option<String>,
        targets: Vec<AndroidTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut preferences: Preferences = KeyValueStore::global(cx)
            .read_kvp(&preference_key(&root))
            .log_err()
            .flatten()
            .and_then(|value| serde_json::from_str(&value).log_err())
            .unwrap_or_default();
        preferences.migrate();
        preferences.capacity = preferences.capacity.clamp(1024 * 1024, 64 * 1024 * 1024);
        if serial.is_some() {
            preferences.serial = serial;
        }
        let filter_input = cx.new(|cx| {
            InputField::new(window, cx, "Filter: package:mine tag:MyTag level:WARN")
                .start_icon(IconName::Filter)
        });
        set_input_text(&filter_input, &preferences.filter, window, cx);
        let search_input = cx.new(|cx| InputField::new(window, cx, "Find in displayed messages"));
        let mut subscriptions = Vec::new();
        let controller = project_context::for_workspace(&workspace, cx);
        if let Some(controller) = &controller {
            subscriptions.push(cx.observe(controller, |view, controller, cx| {
                let current = controller.read(cx).project_token(cx);
                if current != view.device_owner {
                    view.device_cancellation = None;
                    view.stream_task = None;
                    view.device_task = None;
                    view.control_task = None;
                    view.device_owner = None;
                    view.capturing = false;
                    cx.defer(|view, cx| {
                        if view.watching_requested && view.device_context_token(cx).is_ok() {
                            view.watch_devices(cx);
                        }
                    });
                }
            }));
        }
        if let Some(trust) = TrustedWorktrees::try_get_global(cx) {
            subscriptions.push(cx.subscribe(&trust, |view, _, event, cx| {
                if let project::trusted_worktrees::TrustedWorktreesEvent::Restricted(store, paths) =
                    event
                {
                    if *store == view.project.read(cx).worktree_store().downgrade()
                        && (view
                            .controller
                            .as_ref()
                            .and_then(WeakEntity::upgrade)
                            .is_some_and(|controller| {
                                controller.read(cx).owns_restricted_worktree(paths)
                            })
                            || paths.iter().any(|path| match path {
                                project::trusted_worktrees::PathTrust::Worktree(id) => view
                                    .project
                                    .read(cx)
                                    .worktree_for_id(*id, cx)
                                    .is_some_and(|worktree| {
                                        worktree.read(cx).abs_path().as_ref() == view.root.as_path()
                                    }),
                                project::trusted_worktrees::PathTrust::AbsPath(_) => false,
                            }))
                    {
                        view.device_cancellation = None;
                        view.device_owner = None;
                        view.stream_task = None;
                        view.device_task = None;
                        view.control_task = None;
                        view.capturing = false;
                        cx.notify();
                    }
                }
            }));
        }
        for (input, is_filter) in [(filter_input.clone(), true), (search_input.clone(), false)] {
            let view = cx.weak_entity();
            let editor = input.read(cx).editor().clone();
            let subscription = editor.subscribe(
                Box::new(move |event, window, cx| {
                    if event == ErasedEditorEvent::BufferEdited {
                        view.update(cx, |view, cx| {
                            if is_filter {
                                view.update_filter(cx);
                                view.update_completions(window, cx);
                            } else {
                                view.update_search(cx);
                            }
                        })
                        .log_err();
                    } else if is_filter && event == ErasedEditorEvent::Blurred {
                        view.update(cx, |view, cx| {
                            view.completions.clear();
                            cx.notify();
                        })
                        .log_err();
                    }
                }),
                window,
                cx,
            );
            subscriptions.push(subscription);
        }
        subscriptions.push(cx.observe(&project, |view, _, cx| {
            if let Err(error) = view.ensure_trusted(cx) {
                view.stream_task = None;
                view.device_task = None;
                view.device_cancellation = None;
                view.device_owner = None;
                view.control_task = None;
                view.capturing = false;
                view.error = Some(format!("{error:#}"));
                cx.notify();
            }
        }));
        let (query, filter_error) = match Query::parse(&preferences.filter, preferences.match_case)
        {
            Ok(query) => (query, None),
            Err(error) => (Query::default(), Some(error.to_string())),
        };
        let list_state =
            ListState::new(0, ListAlignment::Top, px(256.)).with_uniform_item_height(px(20.));
        list_state.set_follow_mode(FollowMode::Tail);
        Self {
            workspace,
            controller: controller.map(|controller| controller.downgrade()),
            project,
            root,
            targets,
            focus_handle: cx.focus_handle(),
            filter_input,
            search_input,
            buffer: Buffer::new(preferences.capacity),
            cursor: None,
            preferences,
            query,
            filter_error,
            devices: Vec::new(),
            device_error: None,
            process_error: None,
            processes: Vec::new(),
            project_packages: Vec::new(),
            completions: Vec::new(),
            completion_range: 0..0,
            completion_index: 0,
            completion_scroll: ScrollHandle::new(),
            suppress_completion: false,
            search_visible: false,
            horizontal_scroll: ScrollHandle::new(),
            unwrapped_columns: 0,
            measured_line_width: Rc::default(),
            buffers: "main,system,crash".into(),
            paused: None,
            visible: Vec::new(),
            selected: HashSet::new(),
            selection_anchor: None,
            text_selection: None,
            selecting_text: false,
            text_layouts: Rc::default(),
            search: None,
            pending_search: None,
            search_tail: Vec::new(),
            search_case: false,
            search_regex: false,
            search_error: None,
            search_matches: Vec::new(),
            search_position: None,
            list_state,
            file: None,
            connected: false,
            capturing: false,
            stopped: false,
            status: "Looking for Android devices…".into(),
            error: None,
            stream_task: None,
            device_task: None,
            device_cancellation: None,
            device_owner: None,
            watching_requested: false,
            control_task: None,
            persist_task: None,
            filter_task: None,
            rebuild_task: None,
            search_task: None,
            source_task: None,
            source_cancellation: None,
            source_entry: None,
            source_locations: Vec::new(),
            rebuild_cancellation: None,
            search_cancellation: None,
            rebuild_tail: Vec::new(),
            rebuild_tail_columns: 0,
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn root(&self) -> &PathBuf {
        &self.root
    }

    pub(super) fn capture_context(&self) -> (PathBuf, Option<String>, Vec<AndroidTarget>) {
        (
            self.root.clone(),
            self.preferences.serial.clone(),
            self.targets.clone(),
        )
    }

    pub(super) fn set_targets(&mut self, targets: Vec<AndroidTarget>, cx: &mut Context<Self>) {
        self.targets = targets;
        self.watch_devices(cx);
    }

    fn current_targets(&self, cx: &App) -> Vec<AndroidTarget> {
        self.workspace
            .read_with(cx, |workspace, cx| {
                workspace.panel::<AndroidPanel>(cx).and_then(|panel| {
                    let panel = panel.read(cx);
                    (panel.root.as_ref() == Some(&self.root)).then(|| {
                        panel
                            .selected_target
                            .clone()
                            .map(|target| vec![target])
                            .unwrap_or_else(|| panel.targets.clone())
                    })
                })
            })
            .log_err()
            .flatten()
            .unwrap_or_else(|| self.targets.clone())
    }

    fn update_completions(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.suppress_completion {
            self.suppress_completion = false;
            self.completions.clear();
            return;
        }
        if !self.filter_input.focus_handle(cx).is_focused(window) {
            self.completions.clear();
            return;
        }
        let text = self.filter_input.read(cx).text(cx);
        let cursor = input_cursor(&self.filter_input, cx).unwrap_or(text.len());
        let mut candidates = [
            "package:mine",
            "package:",
            "package=:",
            "package~:",
            "tag:",
            "tag=:",
            "tag~:",
            "process:",
            "message:",
            "message~:",
            "line:",
            "pid:",
            "tid:",
            "uid:",
            "name:",
            "level:VERBOSE",
            "level:DEBUG",
            "level:INFO",
            "level:WARN",
            "level:ERROR",
            "level:ASSERT",
            "is:crash",
            "is:stacktrace",
            "is:firebase",
            "age:30s",
            "age:5m",
        ]
        .map(String::from)
        .to_vec();
        candidates.extend(self.preferences.saved_filters.clone());
        let mut values = HashSet::new();
        let mut candidate_bytes: usize = candidates.iter().map(String::len).sum();
        let observed = self
            .buffer
            .entries
            .iter()
            .rev()
            .take(500)
            .flat_map(|entry| {
                [
                    ("tag", entry.tag.as_str()),
                    ("package", entry.package.as_str()),
                    ("process", entry.process.as_str()),
                ]
            })
            .chain(self.processes.iter().flat_map(|process| {
                [
                    ("process", process.name.as_str()),
                    ("package", process.package.as_str()),
                ]
            }));
        for (field, value) in observed {
            if candidate_bytes >= INLINE_FILTER_BYTES {
                break;
            }
            if !value.is_empty() && value.len() <= 256 && values.insert((field, value)) {
                let candidate = format!("{field}:{}", quote(value));
                candidate_bytes += candidate.len();
                candidates.push(candidate);
            }
        }
        let (range, completions) = complete_query(&text, cursor, candidates);
        self.completion_range = range;
        self.completions = completions;
        self.completion_index = 0;
        self.completion_scroll.set_offset(point(px(0.), px(0.)));
        self.completion_scroll.scroll_to_item(0);
        cx.notify();
    }

    fn move_completion(&mut self, previous: bool, cx: &mut Context<Self>) {
        let count = self.completions.len();
        if count > 0 {
            self.completion_index = if previous {
                (self.completion_index + count - 1) % count
            } else {
                (self.completion_index + 1) % count
            };
            self.completion_scroll.scroll_to_item(self.completion_index);
            cx.notify();
        }
    }

    fn accept_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(completion) = self.completions.get(self.completion_index).cloned() else {
            return;
        };
        let Some(editor) = input_editor(&self.filter_input, cx) else {
            return;
        };
        let text = self.filter_input.read(cx).text(cx);
        if query_token(
            &text,
            input_cursor(&self.filter_input, cx).unwrap_or(text.len()),
        ) != self.completion_range
        {
            self.update_completions(window, cx);
            return;
        }
        self.suppress_completion = true;
        let range = self.completion_range.clone();
        let cursor = range.start + completion.len();
        editor.update(cx, |editor, cx| {
            editor.edit(
                [(
                    MultiBufferOffset(range.start)..MultiBufferOffset(range.end),
                    completion,
                )],
                cx,
            );
            editor.change_selections(SelectionEffects::no_scroll(), window, cx, |selections| {
                selections.select_ranges([MultiBufferOffset(cursor)..MultiBufferOffset(cursor)])
            });
        });
        self.completions.clear();
        cx.notify();
    }

    fn show_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_visible = true;
        self.update_search(cx);
        self.completions.clear();
        window.focus(&self.search_input.focus_handle(cx), cx);
        cx.notify();
    }

    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_visible = false;
        self.search_task = None;
        self.search_cancellation = None;
        self.pending_search = None;
        self.search_tail.clear();
        self.search = None;
        self.search_error = None;
        self.search_matches.clear();
        self.search_position = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn toggle_wrap(&mut self, cx: &mut Context<Self>) {
        self.preferences.wrap = !self.preferences.wrap;
        self.horizontal_scroll.set_offset(point(px(0.), px(0.)));
        self.list_state.reset(self.visible.len());
        self.persist(cx);
        cx.notify();
    }

    fn content_width(&self, window: &Window, cx: &App) -> Pixels {
        let font_size = window.rem_size() * 0.875;
        let font = gpui::Font {
            family: ThemeSettings::get_global(cx).buffer_font.family.clone(),
            ..Default::default()
        };
        // Logcat uses the buffer's monospace font. Compute the column advance once,
        // rather than shaping every retained message on each frame.
        let advance = window
            .text_system()
            .shape_line(
                "M".into(),
                font_size,
                &[TextRun {
                    len: 1,
                    font,
                    color: Hsla::default(),
                    ..Default::default()
                }],
                None,
            )
            .width;
        let width =
            (advance * self.unwrapped_columns as f32).max(self.measured_line_width.get()) + px(16.);
        // A record-width message should not invalidate every list row for a
        // one-pixel increase. Painted rows can raise this estimate for fallback fonts.
        let viewport = self.horizontal_scroll.bounds().size.width;
        if width <= viewport || viewport == px(0.) {
            width.max(viewport)
        } else {
            px((f32::from(width) / 1024.).ceil() * 1024.)
        }
    }

    fn ensure_trusted(&self, cx: &App) -> Result<()> {
        let project = self.project.read(cx);
        ensure!(project.is_local(), "Logcat supports local projects only");
        if let Some(trust) = TrustedWorktrees::try_get_global(cx) {
            ensure!(
                !trust
                    .read(cx)
                    .restricted_worktrees(&project.worktree_store(), cx)
                    .iter()
                    .any(|(_, path)| path.as_ref() == self.root.as_path()),
                "Trust the owning project before capturing device logs"
            );
        } else {
            bail!("Trust the owning project before capturing device logs");
        }
        ensure!(
            project
                .worktrees(cx)
                .any(|worktree| self.root.as_path() == worktree.read(cx).abs_path().as_ref()),
            "The Logcat project is no longer open"
        );
        Ok(())
    }

    fn device_context_token(
        &self,
        cx: &App,
    ) -> Result<android_tools::project_context::ActiveProjectToken> {
        self.ensure_trusted(cx)?;
        let controller = self
            .controller
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .context("Select the owning Android project before using device tools")?;
        let controller = controller.read(cx);
        ensure!(
            controller.root(cx).as_ref() == Some(&self.root)
                && controller
                    .capabilities(
                        android_tools::project_context::OperationalReadiness::default(),
                        cx
                    )
                    .android_devices,
            "Select the owning Android project before using device tools"
        );
        controller
            .project_token(cx)
            .context("The owning Android project changed")
    }

    fn verify_device_context(
        &self,
        owner: &android_tools::project_context::ActiveProjectToken,
        cx: &App,
    ) -> Result<()> {
        ensure!(
            &self.device_context_token(cx)? == owner,
            "The active Android project changed during device work"
        );
        Ok(())
    }

    pub(super) fn watch_devices(&mut self, cx: &mut Context<Self>) {
        self.watching_requested = true;
        let owner = match self.device_context_token(cx) {
            Ok(owner) => owner,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        self.device_owner = Some(owner.clone());
        let cancellation = WorkCancellation::default();
        let cancelled = cancellation.0.clone();
        self.device_cancellation = Some(cancellation);
        let executor = cx.background_executor().clone();
        let root = self.root.clone();
        self.device_task = Some(cx.spawn(async move |view, cx| {
            while let Some((previous, serial, targets, file)) = view
                .update(cx, |view, cx| {
                    view.verify_device_context(&owner, cx)?;
                    let targets = view.current_targets(cx);
                    Ok::<_, anyhow::Error>((
                        view.devices.clone(),
                        view.preferences.serial.clone(),
                        targets,
                        view.file.is_some(),
                    ))
                })
                .and_then(|result| result)
                .log_err()
            {
                let result = cx
                    .background_spawn({
                        let executor = executor.clone();
                        let root = root.clone();
                        let cancelled = cancelled.clone();
                        async move {
                            discover_devices(
                                previous, serial, targets, file, &root, &executor, &cancelled,
                            )
                            .await
                        }
                    })
                    .await;
                if view
                    .update(cx, |view, cx| {
                        view.verify_device_context(&owner, cx)?;
                        view.apply_discovery(result, cx);
                        Ok::<_, anyhow::Error>(())
                    })
                    .and_then(|result| result)
                    .is_err()
                {
                    break;
                }
                executor.timer(Duration::from_secs(2)).await;
            }
        }));
        cx.notify();
    }

    fn apply_discovery(&mut self, result: Result<Discovery>, cx: &mut Context<Self>) {
        if let Err(error) = self.ensure_trusted(cx) {
            self.error = Some(error.to_string());
            cx.notify();
            return;
        }
        match result {
            Ok(discovery) => {
                self.device_error = discovery.metadata_error;
                // Keep disconnected devices in the selector so reconnecting never silently changes the source.
                let previous = std::mem::take(&mut self.devices);
                self.devices = discovery.devices;
                for mut device in previous {
                    if !self
                        .devices
                        .iter()
                        .any(|current| current.device.serial == device.device.serial)
                    {
                        device.device.state = "disconnected".into();
                        self.devices.push(device);
                    }
                }
                if self.preferences.serial.is_none() {
                    self.preferences.serial = self
                        .devices
                        .iter()
                        .find(|device| device.device.is_available())
                        .map(|device| device.device.serial.clone());
                }
                let connected = self.devices.iter().any(|device| {
                    Some(&device.device.serial) == self.preferences.serial.as_ref()
                        && device.device.is_available()
                });
                if self.file.is_none() {
                    if self.connected && !connected {
                        self.stream_task = None;
                        self.capturing = false;
                        self.stopped = false;
                        self.status =
                            "Device disconnected. Waiting for the selected device to reconnect…"
                                .into();
                    }
                    self.connected = connected;
                    match discovery.processes {
                        Ok(processes) => {
                            if connected && !self.processes.is_empty() {
                                let now = Utc::now();
                                let ended = self
                                    .processes
                                    .iter()
                                    .filter(|old| {
                                        !processes.iter().any(|new| {
                                            old.pid == new.pid
                                                && old.uid == new.uid
                                                && old.name == new.name
                                        })
                                    })
                                    .cloned()
                                    .map(|process| (process, false));
                                let started = processes
                                    .iter()
                                    .filter(|new| {
                                        !self.processes.iter().any(|old| {
                                            old.pid == new.pid
                                                && old.uid == new.uid
                                                && old.name == new.name
                                        })
                                    })
                                    .cloned()
                                    .map(|process| (process, true));
                                for (process, started) in ended.chain(started).collect::<Vec<_>>() {
                                    self.buffer.push(Entry {
                                        id: 0,
                                        timestamp_millis: now.timestamp_millis(),
                                        timestamp_nanos: now.timestamp_subsec_nanos(),
                                        pid: process.pid,
                                        tid: 0,
                                        uid: Some(process.uid),
                                        level: Level::Info,
                                        tag: "Process".into(),
                                        message: format!(
                                            "PROCESS {} ({}) for {}",
                                            if started { "STARTED" } else { "ENDED" },
                                            process.pid,
                                            process.name
                                        ),
                                        process: process.name,
                                        package: process.package,
                                        buffer: 0,
                                    });
                                }
                            }
                            self.processes = processes;
                            self.process_error = None;
                        }
                        Err(error) => {
                            self.process_error = Some(format!("{error:#}"));
                        }
                    }

                    self.project_packages = discovery.packages;
                    if connected && !self.capturing && !self.stopped {
                        self.start_capture(false, cx);
                    }
                    if !connected && self.preferences.serial.is_some() {
                        let state = self
                            .devices
                            .iter()
                            .find(|device| {
                                Some(&device.device.serial) == self.preferences.serial.as_ref()
                            })
                            .map(|device| device.device.state.as_str())
                            .unwrap_or("disconnected");
                        self.status = format!(
                            "Selected device is {state}. Waiting for it to become available…"
                        );
                    }
                    if self.preferences.serial.is_none() {
                        self.status = "No device selected. Connect a device with USB debugging or start an emulator.".into();
                    }
                    self.rebuild(cx);
                }
            }
            Err(error) => {
                self.device_error = Some(format!("{error:#}"));
                self.connected = false;
                self.stopped = false;
                self.stream_task = None;
                self.capturing = false;
                self.status = "ADB is unavailable. Device discovery will retry.".into();
            }
        }
        cx.notify();
    }

    fn select_device(&mut self, serial: String, cx: &mut Context<Self>) {
        self.preferences.serial = Some(serial);
        self.control_task = None;
        self.stream_task = None;
        self.capturing = false;
        self.stopped = false;
        self.connected = false;
        self.file = None;
        self.processes.clear();
        self.cursor = None;
        self.paused = None;
        self.clear(cx);
        self.persist(cx);
        self.watch_devices(cx);
    }

    fn start_capture(&mut self, clear: bool, cx: &mut Context<Self>) {
        if self.file.is_some() {
            self.error = Some("Select a device to return to live Logcat capture".into());
            cx.notify();
            return;
        }
        self.stream_task = None;
        self.capturing = false;
        let owner = match self.device_context_token(cx) {
            Ok(owner) => owner,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        self.device_owner = Some(owner.clone());
        let Some(serial) = self.preferences.serial.clone() else {
            self.error = Some("Select an Android device first".into());
            cx.notify();
            return;
        };
        if !self.connected {
            self.status = "Waiting for the selected device to become available…".into();
            cx.notify();
            return;
        }
        if clear {
            self.cursor = None;
            self.paused = None;
            self.clear(cx);
        }
        self.error = None;
        self.stopped = false;
        self.capturing = true;
        self.status = format!("Capturing · {serial}");
        let root = self.root.clone();
        let buffers = self.buffers.clone();
        let cursor = self.cursor.clone();
        let executor = cx.background_executor().clone();
        self.stream_task = Some(cx.spawn(async move |view, cx| {
            if view
                .read_with(cx, |view, cx| view.verify_device_context(&owner, cx))
                .and_then(|result| result)
                .is_err()
            {
                return;
            }
            let (sender, mut receiver) = mpsc::channel(4);
            let worker = cx.background_spawn(async move {
                capture(serial, buffers, cursor, root, executor, sender).await
            });
            while let Some(batch) = receiver.next().await {
                if view
                    .update(cx, |view, cx| {
                        view.verify_device_context(&owner, cx)?;
                        view.receive(batch, cx);
                        Ok::<_, anyhow::Error>(())
                    })
                    .and_then(|result| result)
                    .is_err()
                {
                    return;
                }
                // Backlogs must leave time for input and painting between bounded batches.
                cx.background_executor()
                    .timer(Duration::from_millis(8))
                    .await;
            }
            let result = worker.await;
            view.update(cx, |view, cx| {
                if view.verify_device_context(&owner, cx).is_err() {
                    return;
                }
                view.capturing = false;
                // Retry after disconnects, but leave persistent protocol/command errors visible until Restart.
                view.stopped = view.connected
                    && result.as_ref().err().is_none_or(|error| {
                        let message = format!("{error:#}").to_lowercase();
                        ![
                            "device offline",
                            "device disconnected",
                            "device not found",
                            "no devices",
                            "transport",
                            "connection reset",
                        ]
                        .iter()
                        .any(|pattern| message.contains(pattern))
                    });
                view.error = Some(match result {
                    Ok(()) => "Logcat stopped. Restart to resume capture.".into(),
                    Err(error) => format!("{error:#}"),
                });
                cx.notify();
            })
            .log_err();
        }));
        cx.notify();
    }

    fn receive(&mut self, entries: Vec<Entry>, cx: &mut Context<Self>) {
        if self.ensure_trusted(cx).is_err() {
            self.capturing = false;
            return;
        }
        let by_pid: HashMap<_, _> = self
            .processes
            .iter()
            .map(|process| (process.pid, process))
            .collect();
        let mut by_uid = HashMap::new();
        for process in &self.processes {
            if !process.package.is_empty() {
                by_uid.entry(process.uid).or_insert(process);
            }
        }
        let device = self
            .devices
            .iter()
            .find(|device| Some(&device.device.serial) == self.preferences.serial.as_ref());
        let context = FilterContext {
            now_millis: Utc::now().timestamp_millis(),
            project_packages: &self.project_packages,
        };
        let mut appended = Vec::new();
        if let Some(entry) = entries.last() {
            self.cursor = Some(Arc::new(entry.clone()));
        }
        for mut entry in entries {
            if matches!(entry.buffer, 2 | 5 | 6)
                && let Ok(id) = entry.tag.parse::<u32>()
                && let Some(tag) = device.and_then(|device| device.event_tags.get(&id))
            {
                entry.tag = tag.clone();
            }
            if let Some(process) = by_pid
                .get(&entry.pid)
                .filter(|process| entry.uid.is_none_or(|uid| uid == process.uid))
            {
                entry.process = process.name.clone();
                entry.package = process.package.clone();
            } else if let Some(process) = entry.uid.and_then(|uid| by_uid.get(&uid)) {
                entry.package = process.package.clone();
            }
            if entry.package.is_empty()
                && let Some(names) =
                    device.and_then(|device| entry.uid.and_then(|uid| device.packages.get(&uid)))
                && names.len() == 1
                && let Some(package) = names.first()
            {
                entry.package = package.clone();
            }
            self.buffer.push(entry);
            if self.paused.is_none()
                && let Some(entry) = self.buffer.entries.back()
                && self.query.matches(entry, &context)
            {
                appended.push(entry.clone());
            }
        }
        if self.paused.is_none() {
            let first_id = self
                .buffer
                .entries
                .front()
                .map(|entry| entry.id)
                .unwrap_or(u64::MAX);
            let removed = self.visible.partition_point(|entry| entry.id < first_id);
            self.visible.drain(..removed);
            self.list_state.splice(0..removed, 0);
            appended.retain(|entry| entry.id >= first_id);
            let previous_length = self.visible.len();
            for entry in &appended {
                let columns = line_columns(entry, self.preferences.compact);
                self.unwrapped_columns = self.unwrapped_columns.max(columns);
                if self.rebuild_task.is_some() {
                    self.rebuild_tail_columns = self.rebuild_tail_columns.max(columns);
                }
            }
            self.visible.extend(appended);
            self.list_state.splice(
                previous_length..previous_length,
                self.visible.len() - previous_length,
            );
            if removed > 0 {
                self.retain_selection();
                self.search_matches.retain_mut(|index| {
                    if let Some(shifted) = index.checked_sub(removed) {
                        *index = shifted;
                        true
                    } else {
                        false
                    }
                });
            }
            if self.rebuild_task.is_some() {
                let removed = self
                    .rebuild_tail
                    .partition_point(|entry| entry.id < first_id);
                self.rebuild_tail.drain(..removed);
                self.rebuild_tail
                    .extend(self.visible.iter().skip(previous_length).cloned());
            }
            if let Some(search) = &self.pending_search {
                let removed = self.search_tail.partition_point(|id| *id < first_id);
                self.search_tail.drain(..removed);
                self.search_tail.extend(
                    self.visible
                        .iter()
                        .skip(previous_length)
                        .filter_map(|entry| search.matches(&entry.line()).then_some(entry.id)),
                );
            }
            if let Some(search) = &self.search {
                self.search_matches.extend(
                    self.visible
                        .iter()
                        .enumerate()
                        .skip(previous_length)
                        .filter_map(|(index, entry)| {
                            search.matches(&entry.line()).then_some(index)
                        }),
                );
            }
        }
        self.search_position = self
            .search_position
            .filter(|position| *position < self.search_matches.len());
        cx.notify();
    }

    fn update_filter(&mut self, cx: &mut Context<Self>) {
        self.preferences.filter = self.filter_input.read(cx).text(cx);
        self.filter_task = None;
        if small_snapshot(self.buffer.entries.iter()) {
            self.apply_query(
                Query::parse(&self.preferences.filter, self.preferences.match_case),
                cx,
            );
        } else {
            let text = self.preferences.filter.clone();
            let match_case = self.preferences.match_case;
            self.filter_task = Some(cx.spawn(async move |view, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                let result = cx
                    .background_spawn(async move { Query::parse(&text, match_case) })
                    .await;
                view.update(cx, |view, cx| view.apply_query(result, cx))
                    .log_err();
            }));
        }
        self.persist(cx);
        cx.notify();
    }

    fn apply_query(&mut self, result: Result<Query>, cx: &mut Context<Self>) {
        self.filter_task = None;
        match result {
            Ok(query) => {
                self.query = query;
                self.filter_error = None;
                self.rebuild(cx);
            }
            Err(error) => {
                self.filter_error = Some(format!("{error:#}"));
                cx.notify();
            }
        }
    }

    fn persist(&mut self, cx: &mut Context<Self>) {
        let key = preference_key(&self.root);
        let preferences = self.preferences.clone();
        let database = KeyValueStore::global(cx);
        let executor = cx.background_executor().clone();
        self.persist_task = Some(cx.spawn(async move |_, cx| {
            executor.timer(Duration::from_millis(300)).await;
            let value = match serde_json::to_string(&preferences) {
                Ok(value) => value,
                Err(error) => {
                    log::error!("Failed to save Logcat preferences: {error}");
                    return;
                }
            };
            cx.background_spawn(async move { database.write_kvp(key, value).await })
                .await
                .log_err();
        }));
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.rebuild_task = None;
        self.rebuild_cancellation = None;
        self.rebuild_tail.clear();
        self.rebuild_tail_columns = 0;
        let entries = self
            .paused
            .clone()
            .unwrap_or_else(|| self.buffer.entries.iter().cloned().collect());
        let query = self.query.clone();
        let packages = self.project_packages.clone();
        let compact = self.preferences.compact;
        let now = Utc::now().timestamp_millis();
        if small_snapshot(entries.iter()) {
            let (visible, columns) =
                filter_entries(entries, &query, &packages, compact, now, None).unwrap_or_default();
            self.apply_visible(visible, columns, cx);
            return;
        }
        let cancellation = WorkCancellation::default();
        let cancelled = cancellation.0.clone();
        self.rebuild_cancellation = Some(cancellation);
        self.rebuild_task = Some(cx.spawn(async move |view, cx| {
            let Some((mut visible, mut columns)) = cx
                .background_spawn(async move {
                    filter_entries(entries, &query, &packages, compact, now, Some(&cancelled))
                })
                .await
            else {
                return;
            };
            view.update(cx, |view, cx| {
                if view.paused.is_none() {
                    let first_id = view
                        .buffer
                        .entries
                        .front()
                        .map(|entry| entry.id)
                        .unwrap_or(u64::MAX);
                    visible.drain(..visible.partition_point(|entry| entry.id < first_id));
                    let tail = std::mem::take(&mut view.rebuild_tail);
                    visible.extend(tail.into_iter().filter(|entry| entry.id >= first_id));
                    columns = columns.max(view.rebuild_tail_columns);
                }
                view.rebuild_task = None;
                view.rebuild_cancellation = None;
                view.rebuild_tail_columns = 0;
                view.apply_visible(visible, columns, cx);
            })
            .log_err();
        }));
    }

    fn retain_selection(&mut self) {
        let contains = |id| {
            self.visible
                .binary_search_by_key(&id, |entry| entry.id)
                .is_ok()
        };
        self.selected.retain(|id| contains(*id));
        self.text_selection = self
            .text_selection
            .filter(|(anchor, head)| contains(anchor.0) && contains(head.0));
        self.selection_anchor = self.selection_anchor.filter(|id| contains(*id));
        if self.text_selection.is_none() {
            self.selecting_text = false;
        }
    }

    fn single_selected(&self) -> Option<u64> {
        (self.selected.len() == 1)
            .then(|| self.selected.iter().next().copied())
            .flatten()
    }

    fn remeasure_selection(&self, previous: Option<u64>) {
        if self.preferences.fold_stacktraces {
            self.list_state.remeasure();
        } else {
            for id in previous.into_iter().chain(self.single_selected()) {
                if let Ok(index) = self.visible.binary_search_by_key(&id, |entry| entry.id) {
                    self.list_state.remeasure_items(index..index + 1);
                }
            }
        }
    }

    fn apply_visible(&mut self, visible: Vec<Arc<Entry>>, columns: usize, cx: &mut Context<Self>) {
        let removed = self
            .visible
            .partition_point(|entry| visible.first().is_none_or(|first| entry.id < first.id));
        let retained = self.visible.get(removed..).unwrap_or_default();
        if visible.len() >= retained.len()
            && visible
                .iter()
                .zip(retained)
                .all(|(new, old)| new.id == old.id)
        {
            self.list_state.splice(0..removed, 0);
            self.list_state.splice(
                retained.len()..retained.len(),
                visible.len() - retained.len(),
            );
        } else {
            self.list_state.reset(visible.len());
        }
        self.visible = visible;
        self.unwrapped_columns = columns;
        self.measured_line_width.set(px(0.));
        self.retain_selection();
        self.update_search(cx);
        cx.notify();
    }

    fn update_search(&mut self, cx: &mut Context<Self>) {
        self.search_task = None;
        self.search_cancellation = None;
        self.pending_search = None;
        self.search_tail.clear();
        if !self.search_visible {
            return;
        }
        let text = self.search_input.read(cx).text(cx);
        let match_case = self.search_case;
        let regex = self.search_regex;
        if small_snapshot(self.visible.iter()) {
            self.apply_search(logcat::Search::new(&text, match_case, regex), None, cx);
            return;
        }
        let cancellation = WorkCancellation::default();
        let cancelled = cancellation.0.clone();
        self.search_cancellation = Some(cancellation);
        self.search_task = Some(cx.spawn(async move |view, cx| {
            let search = cx
                .background_spawn(async move { logcat::Search::new(&text, match_case, regex) })
                .await;
            let Some(entries) = view
                .update(cx, |view, _| {
                    view.pending_search = search.as_ref().ok().cloned().flatten();
                    view.visible.clone()
                })
                .log_err()
            else {
                return;
            };
            let worker_search = search.as_ref().ok().cloned().flatten();
            let Some(matches) = cx
                .background_spawn(async move {
                    let mut matches = Vec::new();
                    if let Some(search) = worker_search {
                        for entry in &entries {
                            if cancelled.load(Ordering::Relaxed) {
                                return None;
                            }
                            if search.matches(&entry.line()) {
                                matches.push(entry.id);
                            }
                        }
                    }
                    Some(matches)
                })
                .await
            else {
                return;
            };
            view.update(cx, |view, cx| view.apply_search(search, Some(matches), cx))
                .log_err();
        }));
    }

    fn apply_search(
        &mut self,
        result: Result<Option<logcat::Search>>,
        snapshot: Option<Vec<u64>>,
        cx: &mut Context<Self>,
    ) {
        self.search_task = None;
        self.search_cancellation = None;
        self.pending_search = None;
        match result {
            Ok(search) => {
                self.search = search;
                self.search_error = None;
            }
            Err(error) => {
                self.search = None;
                self.search_error = Some(error.to_string());
            }
        }
        self.search_matches.clear();
        if let Some(search) = &self.search {
            if let Some(mut matches) = snapshot {
                matches.append(&mut self.search_tail);
                let mut matches = matches.into_iter().peekable();
                for (index, entry) in self.visible.iter().enumerate() {
                    while matches.peek().is_some_and(|id| *id < entry.id) {
                        matches.next();
                    }
                    if matches.peek() == Some(&entry.id) {
                        self.search_matches.push(index);
                    }
                }
            } else {
                self.search_matches = self
                    .visible
                    .iter()
                    .enumerate()
                    .filter_map(|(index, entry)| search.matches(&entry.line()).then_some(index))
                    .collect();
            }
        }
        self.search_tail.clear();
        self.search_position = self
            .search_position
            .filter(|position| *position < self.search_matches.len());
        cx.notify();
    }

    fn find(&mut self, previous: bool, cx: &mut Context<Self>) {
        if self.search_matches.is_empty() {
            return;
        }
        let count = self.search_matches.len();
        let position = match self.search_position {
            Some(position) if previous => (position + count - 1) % count,
            Some(position) => (position + 1) % count,
            None if previous => count - 1,
            None => 0,
        };
        self.search_position = Some(position);
        if let Some(&index) = self.search_matches.get(position) {
            self.list_state.pause_following_tail();
            self.list_state.scroll_to_reveal_item(index);
            if let Some(entry) = self.visible.get(index) {
                let previous = self.single_selected();
                self.text_selection = None;
                self.selected.clear();
                self.selected.insert(entry.id);
                self.remeasure_selection(previous);
            }
        }
        cx.notify();
    }

    fn pause(&mut self, cx: &mut Context<Self>) {
        if self.paused.is_some() {
            self.paused = None;
            self.rebuild(cx);
        } else {
            self.paused = Some(self.buffer.entries.iter().cloned().collect());
            self.rebuild(cx);
        }
        cx.notify();
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.source_task = None;
        self.source_cancellation = None;
        self.source_entry = None;
        self.source_locations.clear();
        self.rebuild_task = None;
        self.rebuild_cancellation = None;
        self.rebuild_tail.clear();
        self.rebuild_tail_columns = 0;
        let entries = std::mem::take(&mut self.buffer.entries);
        let visible = std::mem::take(&mut self.visible);
        let paused = self.paused.as_mut().map(std::mem::take);
        cx.background_spawn(async move {
            drop((entries, visible, paused));
        })
        .detach();
        self.buffer.clear();
        self.list_state.reset(0);
        self.selected.clear();
        self.selection_anchor = None;
        self.text_selection = None;
        self.selecting_text = false;
        self.rebuild(cx);
    }

    fn text_selection_extent(&self) -> Option<((usize, usize), (usize, usize))> {
        let (anchor, head) = self.text_selection?;
        let anchor_index = self
            .visible
            .binary_search_by_key(&anchor.0, |entry| entry.id)
            .ok()?;
        let head_index = self
            .visible
            .binary_search_by_key(&head.0, |entry| entry.id)
            .ok()?;
        let anchor = (anchor_index, anchor.1);
        let head = (head_index, head.1);
        Some((anchor.min(head), anchor.max(head)))
    }

    fn text_selection_range(&self, index: usize, length: usize) -> Option<std::ops::Range<usize>> {
        let (start, end) = self.text_selection_extent()?;
        if index < start.0 || index > end.0 {
            return None;
        }
        Some(
            if index == start.0 {
                start.1.min(length)
            } else {
                0
            }..if index == end.0 {
                end.1.min(length)
            } else {
                length
            },
        )
    }

    fn selected_text(&self) -> String {
        let Some((start, end)) = self.text_selection_extent() else {
            return String::new();
        };
        self.visible
            .get(start.0..=end.0)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .filter_map(|(offset, entry)| {
                let display = display_line(
                    entry,
                    self.preferences.compact,
                    self.preferences.fold_stacktraces && !self.selected.contains(&entry.id),
                );
                let start_offset = if offset == 0 { start.1 } else { 0 };
                let end_offset = if start.0 + offset == end.0 {
                    end.1
                } else {
                    display.text.len()
                };
                display
                    .text
                    .get(start_offset..end_offset)
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn copy(&self, messages_only: bool, cx: &mut Context<Self>) {
        let selected_text = self.selected_text();
        if !selected_text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(selected_text));
            return;
        }
        let text = self
            .visible
            .iter()
            .filter(|entry| self.selected.contains(&entry.id))
            .map(|entry| {
                if messages_only {
                    entry.message.clone()
                } else {
                    entry.line()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.control_task.is_some() {
            self.error = Some("Wait for the current Logcat operation to finish".into());
            cx.notify();
            return;
        }
        let path = cx.prompt_for_new_path(&self.root, Some("logcat.jsonl"));
        let entries = self.visible.clone();
        self.control_task = Some(cx.spawn_in(window, async move |view, cx| {
            let result = async {
                let Some(path) = path.await?? else {
                    return Ok(());
                };
                cx.background_spawn(async move {
                    let text = if path
                        .extension()
                        .is_some_and(|extension| extension == "json")
                    {
                        logcat::export_studio(&entries)?
                    } else {
                        logcat::export(entries)?
                    };
                    let directory = path.parent().context("Invalid export path")?;
                    let file = tempfile::NamedTempFile::new_in(directory)?;
                    std::fs::write(file.path(), text)?;
                    file.persist(path)?;
                    Ok::<_, anyhow::Error>(())
                })
                .await
            }
            .await;
            view.update(cx, |view, cx| {
                if let Err(error) = result {
                    view.error = Some(format!("Export failed: {error:#}"));
                }
                view.control_task = None;
                cx.notify();
            })
            .log_err();
        }));
    }

    fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.control_task.is_some() {
            self.error = Some("Wait for the current Logcat operation to finish".into());
            cx.notify();
            return;
        }
        let capacity = self.preferences.capacity;
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open a Logcat JSONL or threadtime file".into()),
        });
        self.control_task = Some(cx.spawn_in(window, async move |view, cx| {
            let result = async {
                let Some(path) = paths.await??.and_then(|paths| paths.into_iter().next()) else {
                    return Ok(None);
                };
                cx.background_spawn(async move {
                    let file = std::fs::File::open(&path)?;
                    ensure!(
                        file.metadata()?.len() <= 64 * 1024 * 1024,
                        "Logcat files must be smaller than 64 MiB"
                    );
                    let mut text = String::new();
                    file.take(64 * 1024 * 1024 + 1).read_to_string(&mut text)?;
                    ensure!(
                        text.len() <= 64 * 1024 * 1024,
                        "Logcat files must be smaller than 64 MiB"
                    );
                    let entries = logcat::import(&text)?;
                    let mut buffer = Buffer::new(capacity);
                    for entry in entries {
                        buffer.push(entry);
                    }
                    Ok::<_, anyhow::Error>(Some((path, buffer)))
                })
                .await
            }
            .await;
            view.update(cx, |view, cx| {
                match result {
                    Ok(Some((path, buffer))) => view.apply_import(path, buffer, cx),
                    Ok(None) => {}
                    Err(error) => view.error = Some(format!("Import failed: {error:#}")),
                }
                view.control_task = None;
                cx.notify();
            })
            .log_err();
        }));
    }

    fn apply_import(&mut self, path: PathBuf, buffer: Buffer, cx: &mut Context<Self>) {
        self.stream_task = None;
        self.capturing = false;
        self.file = Some(path);
        self.clear(cx);
        self.paused = None;
        self.cursor = None;
        self.buffer = buffer;
        self.error = None;
        self.status = "Viewing saved logs. Select a device to return to live capture.".into();
        self.rebuild(cx);
    }

    fn device_command(&mut self, args: Vec<String>, cx: &mut Context<Self>) {
        let result = self.device_context_token(cx).and_then(|owner| {
            ensure!(
                self.control_task.is_none(),
                "Wait for the current Logcat operation to finish"
            );
            if args.get(1).is_some_and(|argument| argument == "am") {
                ensure!(
                    args.last()
                        .is_some_and(|package| logcat::valid_package(package)),
                    "Invalid application ID"
                );
            }
            ensure!(
                self.file.is_none() && self.connected,
                "Select a connected device"
            );
            Ok((
                self.preferences.serial.clone().context("Select a device")?,
                owner,
            ))
        });
        let (serial, owner) = match result {
            Ok(result) => result,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        let root = self.root.clone();
        let executor = cx.background_executor().clone();
        self.control_task = Some(cx.spawn(async move |view, cx| {
            if view
                .read_with(cx, |view, cx| view.verify_device_context(&owner, cx))
                .and_then(|result| result)
                .is_err()
            {
                return;
            }
            let result = cx
                .background_spawn(async move { adb_output(&serial, args, &root, &executor).await })
                .await;
            view.update(cx, |view, cx| {
                if view.verify_device_context(&owner, cx).is_err() {
                    return;
                }
                if let Err(error) = result {
                    view.error = Some(format!("{error:#}"));
                }
                view.control_task = None;
                cx.notify();
            })
            .log_err();
        }));
    }

    fn device_picker(&self, cx: &Context<Self>) -> impl IntoElement {
        let label = if let Some(file) = &self.file {
            file.to_string_lossy().into_owned()
        } else {
            self.devices
                .iter()
                .find(|device| Some(&device.device.serial) == self.preferences.serial.as_ref())
                .map(LogcatDevice::label)
                .unwrap_or_else(|| {
                    self.preferences
                        .serial
                        .clone()
                        .unwrap_or_else(|| "Select device…".into())
                })
        };
        let tooltip = label.clone();
        let view = cx.weak_entity();
        PopoverMenu::new("logcat-device")
            .trigger(
                ButtonLike::new("logcat-device-trigger")
                    .full_width()
                    .tooltip(Tooltip::text(tooltip))
                    .tab_index(0isize)
                    .style(ButtonStyle::Outlined)
                    .child(
                        h_flex()
                            .w_full()
                            .gap_1()
                            .child(Icon::new(IconName::Screen))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_left()
                                    .debug_selector(|| "logcat-device-label".into())
                                    .child(Label::new(label).truncate()),
                            )
                            .child(Icon::new(IconName::ChevronDown).size(IconSize::XSmall)),
                    ),
            )
            .menu(move |window, cx| {
                let view = view.clone();
                let devices = view
                    .read_with(cx, |view, _| view.devices.clone())
                    .log_err()?;
                Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                    for device in devices {
                        let view = view.clone();
                        let serial = device.device.serial.clone();
                        menu = menu.entry(device.label(), None, move |_, cx| {
                            view.update(cx, |view, cx| view.select_device(serial.clone(), cx))
                                .log_err();
                        });
                    }
                    let refresh = view.clone();
                    menu = menu
                        .separator()
                        .entry("Refresh devices", None, move |_, cx| {
                            refresh
                                .update(cx, |view, cx| {
                                    for device in &mut view.devices {
                                        device.release.clear();
                                    }
                                    view.watch_devices(cx);
                                })
                                .log_err();
                        });
                    menu.entry("Open saved logs…", None, move |window, cx| {
                        view.update(cx, |view, cx| view.import(window, cx))
                            .log_err();
                    })
                }))
            })
    }

    fn options_menu(&self, cx: &Context<Self>) -> impl IntoElement {
        let view = cx.weak_entity();
        PopoverMenu::new("logcat-options")
            .trigger(IconButton::new("logcat-options-trigger", IconName::Ellipsis).tooltip(Tooltip::text("Logcat options")).tab_index(0isize))
            .menu(move |window, cx| {
                let view = view.clone();
                let saved = view
                    .read_with(cx, |view, _| view.preferences.saved_filters.clone())
                    .log_err()?;
                Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                    for (label, buffers) in [
                        ("Default buffers (main, system, crash)", "main,system,crash"),
                        ("All buffers", "all"),
                        ("Main", "main"),
                        ("System", "system"),
                        ("Crash", "crash"),
                        ("Radio", "radio"),
                        ("Events", "events"),
                    ] {
                        let view = view.clone();
                        menu = menu.entry(label, None, move |_, cx| {
                            view.update(cx, |view, cx| {
                                view.buffers = buffers.into();
                                if view.file.is_none() {
                                    view.start_capture(true, cx);
                                }
                            })
                            .log_err();
                        });
                    }
                    menu = menu.separator().header("Retention");
                    for megabytes in [1, 8, 16, 32, 64] {
                        let view = view.clone();
                        menu = menu.entry(format!("{megabytes} MiB"), None, move |_, cx| {
                            view.update(cx, |view, cx| {
                                view.preferences.capacity = megabytes * 1024 * 1024;
                                view.buffer.set_capacity(view.preferences.capacity);
                                view.rebuild(cx);
                                view.persist(cx);
                            })
                            .log_err();
                        });
                    }
                    let save = view.clone();
                    menu = menu
                        .separator()
                        .entry("Save current filter", None, move |_, cx| {
                            save.update(cx, |view, cx| {
                                let filter = view.preferences.filter.clone();
                                if !filter.is_empty()
                                    && !view.preferences.saved_filters.contains(&filter)
                                {
                                    view.preferences.saved_filters.insert(0, filter);
                                    view.preferences.saved_filters.truncate(20);
                                    view.persist(cx);
                                }
                            })
                            .log_err();
                        });
                    for filter in saved {
                        let view = view.clone();
                        menu = menu.entry(filter.clone(), None, move |window, cx| {
                            view.update(cx, |view, cx| {
                                set_input_text(&view.filter_input, &filter, window, cx);
                            })
                            .log_err();
                        });
                    }
                    let compact = view.clone();
                    menu = menu.entry("Toggle compact metadata", None, move |_, cx| {
                        compact.update(cx, |view, cx| {
                            view.preferences.compact = !view.preferences.compact;
                            view.text_selection = None;
                            view.rebuild(cx);
                            view.list_state.reset(view.visible.len());
                            view.persist(cx);
                            cx.notify();
                        }).log_err();
                    });
                    let clear_filters = view.clone();
                    menu = menu.entry("Remove saved filters", None, move |_, cx| {
                        clear_filters
                            .update(cx, |view, cx| {
                                view.preferences.saved_filters.clear();
                                view.persist(cx);
                            })
                            .log_err();
                    });
                    let fold = view.clone();
                    menu =
                        menu.separator()
                            .entry("Toggle stacktrace folding", None, move |_, cx| {
                                fold.update(cx, |view, cx| {
                                    view.preferences.fold_stacktraces =
                                        !view.preferences.fold_stacktraces;
                                    view.text_selection = None;
                                    view.list_state.reset(view.visible.len());
                                    view.persist(cx);
                                    cx.notify();
                                })
                                .log_err();
                            });
                    let clear = view.clone();
                    menu =
                        menu.separator()
                            .entry("Clear device log buffers", None, move |_, cx| {
                                clear
                                    .update(cx, |view, cx| {
                                        view.device_command(
                                            vec![
                                                "logcat".into(),
                                                "-b".into(),
                                                view.buffers.clone(),
                                                "-c".into(),
                                            ],
                                            cx,
                                        )
                                    })
                                    .log_err();
                            });
                    let terminate = view.clone();
                    menu = menu.entry("Terminate current application", None, move |_, cx| {
                        terminate
                            .update(cx, |view, cx| {
                                if let Some(package) = view.project_packages.first().cloned()
                                {
                                    view.device_command(
                                        vec![
                                            "shell".into(),
                                            "am".into(),
                                            "force-stop".into(),
                                            package,
                                        ],
                                        cx,
                                    );
                                } else {
                                    view.error =
                                        Some("Select an Android application in the project tools before terminating it".into());
                                    cx.notify();
                                }
                            })
                            .log_err();
                    });
                    let split = view.clone();
                    menu = menu.entry("New Logcat view in split", None, move |window, cx| {
                        split
                            .update(cx, |view, cx| {
                                let workspace = view.workspace.clone();
                                window.defer(cx, move |window, cx| {
                                    workspace.update(cx, |workspace, cx| {
                                        if let Some(panel) = workspace.panel::<LogcatPanel>(cx) {
                                            panel.update(cx, |panel, cx| panel.new_view(Some(SplitDirection::Right), window, cx));
                                        }
                                    }).log_err();
                                });
                            })
                            .log_err();
                    });
                    menu.entry("Filter syntax help", None, move |_, cx| {
                        cx.open_url(
                            "https://developer.android.com/studio/debug/logcat#key-value-search",
                        )
                    })
                }))
            })
    }

    fn render_entry(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(entry) = self.visible.get(index).cloned() else {
            return gpui::Empty.into_any_element();
        };
        if self.selected.contains(&entry.id) && self.selected.len() == 1 {
            self.update_source_locations(&entry, cx);
        }
        let color = match entry.level {
            Level::Error | Level::Assert => cx.theme().status().error,
            Level::Warn => cx.theme().status().warning,
            Level::Info => cx.theme().status().success,
            Level::Verbose => cx.theme().colors().text_muted,
            Level::Debug => cx.theme().colors().text_accent,
        };
        let selected = self.selected.contains(&entry.id);
        let display = display_line(
            &entry,
            self.preferences.compact,
            self.preferences.fold_stacktraces && !selected,
        );
        let mut highlights = vec![
            (
                0..display.metadata_end,
                gpui::HighlightStyle {
                    color: Some(cx.theme().colors().text_muted),
                    ..Default::default()
                },
            ),
            (
                display.tag,
                gpui::HighlightStyle {
                    color: Some(cx.theme().status().modified),
                    ..Default::default()
                },
            ),
            (
                display.level,
                gpui::HighlightStyle {
                    color: Some(cx.theme().colors().editor_background),
                    background_color: Some(color),
                    ..Default::default()
                },
            ),
        ];
        if let Some(search) = &self.search {
            highlights.extend(search.ranges(&display.text).into_iter().map(|range| {
                (
                    range,
                    gpui::HighlightStyle {
                        background_color: Some(cx.theme().colors().element_selected),
                        ..Default::default()
                    },
                )
            }));
        }
        if let Some(range) = self.text_selection_range(index, display.text.len()) {
            if !range.is_empty() {
                highlights.push((
                    range,
                    gpui::HighlightStyle {
                        background_color: Some(cx.theme().colors().element_selected),
                        ..Default::default()
                    },
                ));
            }
        }
        let highlights = gpui::combine_highlights(highlights, []).collect::<Vec<_>>();
        let text = gpui::StyledText::new(display.text).with_highlights(highlights);
        let layout = text.layout().clone();
        let painted_layouts = self.text_layouts.clone();
        let measured_width = self.measured_line_width.clone();
        let wrapped = self.preferences.wrap;
        let entity_id = cx.entity_id();
        let id = entry.id;
        let view = cx.weak_entity();
        let menu = ui::right_click_menu(("logcat-row-menu", id)).maybe_menu(move |window, cx| {
            let view = view.clone();
            let entry = entry.clone();
            let selected_text = view
                .read_with(cx, |view, _| view.selected_text())
                .log_err()?;
            Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                if !selected_text.is_empty() {
                    let text = selected_text.clone();
                    menu = menu.entry("Copy selection", None, move |_, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                    });
                }
                let message = entry.message.clone();
                menu = menu.entry("Copy message", None, move |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(message.clone()))
                });
                let line = entry.line();
                menu = menu.entry("Copy message with metadata", None, move |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(line.clone()))
                });
                for (label, term) in [
                    ("Show this tag", format!("tag=:{}", quote(&entry.tag))),
                    ("Ignore this tag", format!("-tag=:{}", quote(&entry.tag))),
                    (
                        "Show this application",
                        format!("package=:{}", quote(&entry.package)),
                    ),
                    (
                        "Ignore this application",
                        format!("-package=:{}", quote(&entry.package)),
                    ),
                ] {
                    if term.contains(":\"\"") {
                        continue;
                    }
                    let view = view.clone();
                    menu = menu.entry(label, None, move |window, cx| {
                        view.update(cx, |view, cx| {
                            let filter =
                                if term.starts_with('-') && !view.preferences.filter.is_empty() {
                                    format!("({}) & {term}", view.preferences.filter)
                                } else {
                                    term.clone()
                                };
                            set_input_text(&view.filter_input, &filter, window, cx);
                        })
                        .log_err();
                    });
                }
                menu
            }))
        });
        let row = v_flex()
            .id(("logcat-entry", id))
            .debug_selector(move || format!("logcat-entry-{index}"))
            .w_full()
            .px_2()
            .text_sm()
            .font_family(ThemeSettings::get_global(cx).buffer_font.family.clone())
            .when(selected && self.text_selection.is_none(), |row| {
                row.bg(cx.theme().colors().element_selected)
            })
            .when(
                self.search_matches.binary_search(&index).is_ok() && !selected,
                |row| row.bg(cx.theme().colors().element_hover),
            )
            .on_click(
                cx.listener(move |view, event: &gpui::ClickEvent, window, cx| {
                    window.focus(&view.focus_handle, cx);
                    if view
                        .text_selection
                        .is_some_and(|(anchor, head)| anchor != head)
                    {
                        return;
                    }
                    let previous = view.single_selected();
                    let Ok(index) = view.visible.binary_search_by_key(&id, |entry| entry.id) else {
                        return;
                    };
                    view.text_selection = None;
                    if event.modifiers().shift {
                        let anchor = view
                            .selection_anchor
                            .and_then(|id| view.visible.iter().position(|entry| entry.id == id))
                            .unwrap_or(index);
                        for entry in view
                            .visible
                            .get(anchor.min(index)..=anchor.max(index))
                            .unwrap_or_default()
                        {
                            view.selected.insert(entry.id);
                        }
                    } else if event.modifiers().platform {
                        if !view.selected.remove(&id) {
                            view.selected.insert(id);
                        }
                        view.selection_anchor = Some(id);
                    } else {
                        view.selected.clear();
                        view.selected.insert(id);
                        view.selection_anchor = Some(id);
                    }
                    view.remeasure_selection(previous);
                    cx.notify();
                }),
            )
            .child(
                h_flex().items_start().gap_1().child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(color)
                        .when(!self.preferences.wrap, |text| text.whitespace_nowrap())
                        .on_children_prepainted({
                            let layout = layout.clone();
                            move |bounds, _, cx| {
                                if let Some(bounds) = bounds.first() {
                                    if !wrapped {
                                        let width = layout
                                            .line_layouts()
                                            .iter()
                                            .map(|line| line.unwrapped_layout.width)
                                            .fold(px(0.), Pixels::max);
                                        if width > measured_width.get() {
                                            measured_width.set(width);
                                            cx.notify(entity_id);
                                        }
                                    }
                                    // List measures overscan rows without prepainting them. Only
                                    // publish layouts whose text is ready for hit testing.
                                    painted_layouts.borrow_mut().insert(
                                        id,
                                        PaintedLogcatText {
                                            bounds: *bounds,
                                            layout: layout.clone(),
                                        },
                                    );
                                }
                            }
                        })
                        .cursor_text()
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener({
                                move |view, event: &gpui::MouseDownEvent, window, cx| {
                                    if event.modifiers.platform || event.modifiers.shift {
                                        view.text_selection = None;
                                        view.selecting_text = false;
                                        return;
                                    }
                                    let offset = match layout.index_for_position(event.position) {
                                        Ok(offset) | Err(offset) => offset,
                                    };
                                    view.text_selection = Some(((id, offset), (id, offset)));
                                    view.selecting_text = true;
                                    view.list_state.pause_following_tail();
                                    window.focus(&view.focus_handle, cx);
                                    cx.notify();
                                }
                            }),
                        )
                        .child(text),
                ),
            )
            .when(selected && self.selected.len() == 1, |row| {
                row.children(self.source_buttons())
            });
        menu.trigger(move |_, _, _| row).into_any_element()
    }

    fn update_source_locations(&mut self, entry: &Arc<Entry>, cx: &mut Context<Self>) {
        if self.source_entry == Some(entry.id) {
            return;
        }
        self.source_task = None;
        self.source_cancellation = None;
        self.source_locations.clear();
        self.source_entry = Some(entry.id);
        let worktrees = self
            .project
            .read(cx)
            .worktrees(cx)
            .map(|worktree| worktree.read(cx).snapshot())
            .collect::<Vec<_>>();
        let entry = entry.clone();
        let id = entry.id;
        let cancellation = WorkCancellation::default();
        let cancelled = cancellation.0.clone();
        self.source_cancellation = Some(cancellation);
        self.source_task = Some(cx.spawn(async move |view, cx| {
            let locations = cx
                .background_spawn(async move {
                    let mut frames: HashMap<&str, Vec<u32>> = HashMap::new();
                    for frame in entry.message.lines() {
                        if cancelled.load(Ordering::Relaxed) {
                            return Vec::new();
                        }
                        let Some((_, location)) = frame.rsplit_once('(') else {
                            continue;
                        };
                        let Some((file, line)) = location.trim_end_matches(')').rsplit_once(':')
                        else {
                            continue;
                        };
                        if (file.ends_with(".kt") || file.ends_with(".java"))
                            && let Ok(line) = line.parse::<u32>()
                        {
                            let lines = frames.entry(file).or_default();
                            if lines.len() < 20 && !lines.contains(&line) {
                                lines.push(line);
                            }
                        }
                    }
                    let mut locations = Vec::new();
                    if frames.is_empty() {
                        return locations;
                    }
                    for worktree in worktrees {
                        for source in worktree.entries(false, 0) {
                            if cancelled.load(Ordering::Relaxed) {
                                return Vec::new();
                            }
                            if let Some(lines) =
                                source.path.file_name().and_then(|file| frames.get(file))
                            {
                                for &line in lines {
                                    locations.push((
                                        project::ProjectPath {
                                            worktree_id: worktree.id(),
                                            path: source.path.clone(),
                                        },
                                        line,
                                    ));
                                    if locations.len() == 20 {
                                        return locations;
                                    }
                                }
                            }
                        }
                    }
                    locations
                })
                .await;
            view.update(cx, |view, cx| {
                view.source_task = None;
                view.source_cancellation = None;
                view.source_locations = locations;
                if let Ok(index) = view.visible.binary_search_by_key(&id, |entry| entry.id) {
                    view.list_state.remeasure_items(index..index + 1);
                }
                cx.notify();
            })
            .log_err();
        }));
    }

    fn source_buttons(&self) -> Vec<AnyElement> {
        self.source_locations
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, (path, line))| {
                let workspace = self.workspace.clone();
                Button::new(
                    ("logcat-source", index),
                    format!("{}:{line}", path.path.as_unix_str()),
                )
                .tab_index(0isize)
                .on_click(move |_, window, cx| {
                    let opened = workspace.update(cx, |workspace, cx| {
                        workspace.open_path(path.clone(), None, true, window, cx)
                    });
                    match opened {
                        Ok(opened) => {
                            window
                                .spawn(cx, async move |cx| {
                                    let item = opened.await?;
                                    if let Some(editor) = item.downcast::<Editor>() {
                                        editor.update_in(cx, |editor, window, cx| {
                                            editor.go_to_singleton_buffer_point(
                                                language::Point::new(line.saturating_sub(1), 0),
                                                window,
                                                cx,
                                            )
                                        })?;
                                    }
                                    Ok::<_, anyhow::Error>(())
                                })
                                .detach_and_log_err(cx);
                        }
                        Err(error) => log::error!("Cannot open Logcat source: {error:#}"),
                    }
                })
                .into_any_element()
            })
            .collect()
    }
}

impl Focusable for LogcatView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl EventEmitter<ItemEvent> for LogcatView {}
impl Item for LogcatView {
    type Event = ItemEvent;
    fn active_project_path(&self, cx: &App) -> Option<project::ProjectPath> {
        self.project
            .read(cx)
            .visible_worktrees(cx)
            .find(|worktree| worktree.read(cx).abs_path().as_ref() == self.root.as_path())
            .map(|worktree| project::ProjectPath {
                worktree_id: worktree.read(cx).id(),
                path: RelPath::empty().into_arc(),
            })
    }
    fn tab_content_text(&self, _: usize, _: &App) -> SharedString {
        "Logcat".into()
    }
    fn tab_icon(&self, _: &Window, _: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Logcat))
    }
    fn show_toolbar(&self) -> bool {
        false
    }
    fn to_item_events(event: &Self::Event, callback: &mut dyn FnMut(ItemEvent)) {
        callback(*event);
    }
}

impl Render for LogcatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.text_layouts.borrow_mut().clear();
        let mut filter_context = KeyContext::new_with_defaults();
        filter_context.add("AndroidLogcatFilter");
        if !self.completions.is_empty() {
            filter_context.add("showing_suggestions");
        }
        let input_bounds = Rc::new(Cell::new(Bounds::default()));
        let suggestions = (!self.completions.is_empty()).then(|| {
            let input_bounds = input_bounds.clone();
            let completions = self.completions.clone();
            let selected = self.completion_index;
            let scroll = self.completion_scroll.clone();
            let view = cx.weak_entity();
            // Lay out the popup after this frame has measured the filter input.
            deferred(
                gpui::canvas(
                    move |_, window, cx| {
                        let bounds = suggestion_bounds(input_bounds.get(), window.viewport_size());
                        if bounds.size.height <= px(0.) {
                            return None;
                        }
                        let mut items = v_flex()
                            .id("logcat-suggestion-scroll")
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&scroll);
                        for (index, completion) in completions.into_iter().enumerate() {
                            let view = view.clone();
                            items = items.child(
                                div().flex_none().w_full().child(
                                    ButtonLike::new(("logcat-completion", index))
                                        .full_width()
                                        .toggle_state(index == selected)
                                        .child(
                                            div()
                                                .w_full()
                                                .text_left()
                                                .debug_selector(move || {
                                                    format!("logcat-completion-label-{index}")
                                                })
                                                .child(Label::new(completion).truncate()),
                                        )
                                        .on_click(move |_, window, cx| {
                                            view.update(cx, |view, cx| {
                                                view.completion_index = index;
                                                view.accept_completion(window, cx);
                                            })
                                            .log_err();
                                        }),
                                ),
                            );
                        }
                        let mut menu = v_flex()
                            .id("logcat-filter-suggestions")
                            .debug_selector(|| "logcat-filter-suggestions".into())
                            .w(bounds.size.width)
                            .max_h(bounds.size.height)
                            .p_1()
                            .rounded_md()
                            .occlude()
                            .border_1()
                            .border_color(cx.theme().colors().border)
                            .bg(cx.theme().colors().elevated_surface_background)
                            .shadow_lg()
                            .child(items)
                            .custom_scrollbars(
                                Scrollbars::always_visible(ScrollAxes::Vertical)
                                    .tracked_scroll_handle(&scroll)
                                    .tracked_entity(view.entity_id()),
                                window,
                                cx,
                            )
                            .into_any_element();
                        menu.layout_as_root(
                            gpui::size(
                                gpui::AvailableSpace::Definite(bounds.size.width),
                                gpui::AvailableSpace::MaxContent,
                            ),
                            window,
                            cx,
                        );
                        menu.prepaint_at(bounds.origin, window, cx);
                        Some(menu)
                    },
                    |_, menu, window, cx| {
                        if let Some(mut menu) = menu {
                            menu.paint(window, cx);
                        }
                    },
                )
                .absolute()
                .size_0(),
            )
            .with_priority(1)
        });
        let filter = h_flex()
            .id("logcat-filter-bar")
            .relative()
            .key_context(filter_context)
            .flex_1()
            .min_w_0()
            .gap_1()
            .on_action(
                cx.listener(|view, _: &NextSuggestion, _, cx| view.move_completion(false, cx)),
            )
            .on_action(
                cx.listener(|view, _: &PreviousSuggestion, _, cx| view.move_completion(true, cx)),
            )
            .on_action(cx.listener(|view, _: &AcceptSuggestion, window, cx| {
                view.accept_completion(window, cx)
            }))
            .on_action(cx.listener(|view, _: &DismissSuggestions, _, cx| {
                view.completions.clear();
                cx.notify();
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .debug_selector(|| "logcat-filter-input".into())
                    .on_children_prepainted(move |bounds, _, _| {
                        if let Some(bounds) = bounds.first() {
                            input_bounds.set(*bounds);
                        }
                    })
                    .child(self.filter_input.clone()),
            )
            .child(
                IconButton::new("logcat-clear-filter", IconName::Close)
                    .tooltip(Tooltip::text("Clear filter"))
                    .on_click(cx.listener(|view, _, window, cx| {
                        set_input_text(&view.filter_input, "", window, cx)
                    })),
            )
            .child(
                IconButton::new("logcat-case", IconName::CaseSensitive)
                    .toggle_state(self.preferences.match_case)
                    .tooltip(Tooltip::text("Match case in filters"))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.preferences.match_case = !view.preferences.match_case;
                        view.update_filter(cx);
                    })),
            )
            .children(suggestions);
        let actions = v_flex()
            .id("logcat-actions")
            .h_full()
            .overflow_y_scroll()
            .flex_none()
            .items_center()
            .gap_1()
            .p_1()
            .border_r_1()
            .border_color(cx.theme().colors().border)
            .role(gpui::Role::Toolbar)
            .aria_label("Logcat actions")
            .child(
                IconButton::new("logcat-clear", IconName::Trash)
                    .tooltip(Tooltip::text("Clear view"))
                    .on_click(cx.listener(|view, _, _, cx| view.clear(cx))),
            )
            .child(
                IconButton::new(
                    "logcat-pause",
                    if self.paused.is_some() {
                        IconName::PlayOutlined
                    } else {
                        IconName::DebugPause
                    },
                )
                .toggle_state(self.paused.is_some())
                .tooltip(Tooltip::text(if self.paused.is_some() {
                    "Resume Logcat"
                } else {
                    "Pause Logcat"
                }))
                .on_click(cx.listener(|view, _, _, cx| view.pause(cx))),
            )
            .child(
                IconButton::new("logcat-restart", IconName::RotateCw)
                    .disabled(self.file.is_some() || !self.connected)
                    .tooltip(Tooltip::text("Restart capture"))
                    .on_click(cx.listener(|view, _, _, cx| view.start_capture(true, cx))),
            )
            .child(
                IconButton::new("logcat-follow", IconName::ArrowDown)
                    .toggle_state(self.list_state.is_following_tail())
                    .tooltip(Tooltip::text("Scroll to end"))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.list_state.set_follow_mode(FollowMode::Tail);
                        cx.notify();
                    })),
            )
            .child(
                IconButton::new("logcat-wrap", IconName::TextWrap)
                    .toggle_state(self.preferences.wrap)
                    .tooltip(Tooltip::text("Wrap long lines"))
                    .on_click(cx.listener(|view, _, _, cx| view.toggle_wrap(cx))),
            )
            .child(
                IconButton::new("logcat-find", IconName::ToolSearch)
                    .tooltip(Tooltip::text("Find in Logcat"))
                    .on_click(cx.listener(|view, _, window, cx| view.show_find(window, cx))),
            )
            .child(
                IconButton::new("logcat-copy", IconName::Copy)
                    .disabled(
                        self.selected.is_empty()
                            && self
                                .text_selection
                                .is_none_or(|(anchor, head)| anchor == head),
                    )
                    .tooltip(Tooltip::text("Copy selection"))
                    .on_click(cx.listener(|view, _, _, cx| view.copy(false, cx))),
            )
            .child(
                IconButton::new("logcat-export", IconName::Download)
                    .disabled(self.control_task.is_some())
                    .tooltip(Tooltip::text("Export logs"))
                    .on_click(cx.listener(|view, _, window, cx| view.export(window, cx))),
            )
            .child(
                IconButton::new("logcat-import", IconName::FolderOpen)
                    .disabled(self.control_task.is_some())
                    .tooltip(Tooltip::text("Open saved logs"))
                    .on_click(cx.listener(|view, _, window, cx| view.import(window, cx))),
            )
            .child(self.options_menu(cx));
        let width = if self.preferences.wrap {
            px(0.)
        } else {
            self.content_width(window, cx)
        };
        let scroll = LogcatScrollHandle {
            horizontal: self.horizontal_scroll.clone(),
            vertical: self.list_state.clone(),
        };
        let scrolling_content = div()
            .id("logcat-scroll-view")
            .size_full()
            .overflow_x_scroll()
            .restrict_scroll_to_axis()
            .track_scroll(&self.horizontal_scroll)
            .child(
                list(self.list_state.clone(), cx.processor(Self::render_entry))
                    .h_full()
                    .when(self.preferences.wrap, |list| list.w_full())
                    .when(!self.preferences.wrap, |list| list.w(width))
                    .min_w_full(),
            );
        // The tracks must stay outside the horizontal scrolling transform.
        let content = div()
            .id("logcat-viewport")
            .relative()
            .size_full()
            .debug_selector(|| "logcat-scrollbar-frame".into())
            .on_mouse_move(cx.listener(|view, event: &gpui::MouseMoveEvent, _, cx| {
                if !view.selecting_text || !event.dragging() {
                    return;
                }
                let layouts = view.text_layouts.borrow();
                let nearest = layouts.iter().min_by_key(|(_, text)| {
                    let bounds = text.bounds;
                    if event.position.y < bounds.top() {
                        bounds.top() - event.position.y
                    } else if event.position.y > bounds.bottom() {
                        event.position.y - bounds.bottom()
                    } else {
                        px(0.)
                    }
                });
                if let Some((&id, text)) = nearest {
                    let offset = match text.layout.index_for_position(event.position) {
                        Ok(offset) | Err(offset) => offset,
                    };
                    if let Some((_, head)) = &mut view.text_selection {
                        if *head != (id, offset) {
                            *head = (id, offset);
                            cx.notify();
                        }
                    }
                }
            }))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|view, _, _, _| {
                    view.selecting_text = false;
                }),
            )
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                cx.listener(|view, _, _, _| {
                    view.selecting_text = false;
                }),
            )
            .child(scrolling_content)
            .custom_scrollbars(
                Scrollbars::new(if self.preferences.wrap {
                    ScrollAxes::Vertical
                } else {
                    ScrollAxes::Both
                })
                .tracked_scroll_handle(&scroll)
                .tracked_entity(cx.entity_id())
                .with_stable_track_along(
                    ScrollAxes::Horizontal,
                    cx.theme().colors().editor_background,
                ),
                window,
                cx,
            );
        v_flex()
            .id("android-logcat")
            .key_context("AndroidLogcat")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .role(gpui::Role::Region)
            .aria_label("Android Logcat")
            .on_action(cx.listener(|view, _: &Pause, _, cx| view.pause(cx)))
            .on_action(cx.listener(|view, _: &Clear, _, cx| view.clear(cx)))
            .on_action(cx.listener(|view, _: &Restart, _, cx| view.start_capture(true, cx)))
            .on_action(cx.listener(|view, _: &FocusFilter, window, cx| {
                window.focus(&view.filter_input.focus_handle(cx), cx);
                view.update_completions(window, cx);
            }))
            .on_action(cx.listener(|view, _: &Find, window, cx| view.show_find(window, cx)))
            .on_action(cx.listener(|view, _: &FindNext, _, cx| view.find(false, cx)))
            .on_action(cx.listener(|view, _: &FindPrevious, _, cx| view.find(true, cx)))
            .on_action(cx.listener(|view, _: &Copy, _, cx| view.copy(false, cx)))
            .on_action(cx.listener(|view, _: &SelectAll, _, cx| {
                let previous = view.single_selected();
                view.text_selection = None;
                view.selected = view.visible.iter().map(|entry| entry.id).collect();
                view.remeasure_selection(previous);
                cx.notify();
            }))
            .child(
                h_flex()
                    .p_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        div()
                            .debug_selector(|| "logcat-device-selector".into())
                            .w(gpui::relative(0.32))
                            .min_w(px(160.))
                            .max_w(px(480.))
                            .flex_none()
                            .child(self.device_picker(cx)),
                    )
                    .child(filter),
            )
            .when_some(self.filter_error.clone(), |view, error| {
                view.child(
                    div()
                        .px_2()
                        .text_sm()
                        .text_color(cx.theme().status().error)
                        .child(format!("{error} · showing the last valid filter")),
                )
            })
            .when(self.search_visible, |view| {
                view.child(
                    h_flex()
                        .key_context("AndroidLogcatSearch")
                        .px_2()
                        .pb_1()
                        .gap_1()
                        .on_action(cx.listener(|view, _: &CloseFind, window, cx| {
                            view.close_find(window, cx)
                        }))
                        .child(div().flex_1().min_w_0().child(self.search_input.clone()))
                        .child(
                            IconButton::new("logcat-search-case", IconName::CaseSensitive)
                                .toggle_state(self.search_case)
                                .tooltip(Tooltip::text("Match case in Find"))
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.search_case = !view.search_case;
                                    view.update_search(cx);
                                })),
                        )
                        .child(
                            IconButton::new("logcat-search-regex", IconName::Regex)
                                .toggle_state(self.search_regex)
                                .tooltip(Tooltip::text("Use regular expressions in Find"))
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.search_regex = !view.search_regex;
                                    view.update_search(cx);
                                })),
                        )
                        .child(
                            Label::new(format!(
                                "{}/{}",
                                self.search_position
                                    .map(|position| position + 1)
                                    .unwrap_or(0),
                                self.search_matches.len()
                            ))
                            .color(Color::Muted),
                        )
                        .child(
                            IconButton::new("logcat-previous", IconName::ArrowUp)
                                .disabled(self.search_matches.is_empty())
                                .tooltip(Tooltip::text("Previous match"))
                                .on_click(cx.listener(|view, _, _, cx| view.find(true, cx))),
                        )
                        .child(
                            IconButton::new("logcat-next", IconName::ArrowDown)
                                .disabled(self.search_matches.is_empty())
                                .tooltip(Tooltip::text("Next match"))
                                .on_click(cx.listener(|view, _, _, cx| view.find(false, cx))),
                        )
                        .child(
                            IconButton::new("logcat-close-find", IconName::Close)
                                .tooltip(Tooltip::text("Close Find"))
                                .on_click(
                                    cx.listener(|view, _, window, cx| view.close_find(window, cx)),
                                ),
                        ),
                )
            })
            .when_some(
                self.search_error.clone().filter(|_| self.search_visible),
                |view, error| {
                    view.child(
                        div()
                            .px_2()
                            .text_sm()
                            .text_color(cx.theme().status().error)
                            .child(error),
                    )
                },
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .child(actions)
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(content)
                            .when(self.visible.is_empty(), |view| {
                                view.child(
                                    div()
                                        .absolute()
                                        .inset_0()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            Label::new(if self.buffer.entries.is_empty() {
                                                "Waiting for logs…"
                                            } else {
                                                "No messages match the filter"
                                            })
                                            .color(Color::Muted),
                                        ),
                                )
                            }),
                    ),
            )
            .child(
                h_flex()
                    .px_2()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        Label::new(format!(
                            "{} · {}{} messages · {} MiB · {} evicted",
                            self.status,
                            self.visible.len(),
                            if self.paused.is_some() { " frozen" } else { "" },
                            self.preferences.capacity / (1024 * 1024),
                            self.buffer.dropped
                        ))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    ),
            )
            .when_some(
                self.error
                    .clone()
                    .or_else(|| self.device_error.clone())
                    .or_else(|| self.process_error.clone()),
                |view, error| {
                    view.child(
                        div()
                            .px_2()
                            .pb_1()
                            .text_sm()
                            .text_color(cx.theme().status().error)
                            .child(error),
                    )
                },
            )
    }
}

fn suggestion_bounds(input: Bounds<Pixels>, viewport: gpui::Size<Pixels>) -> Bounds<Pixels> {
    let margin = px(8.);
    let width = px(360.).min((viewport.width - margin * 2.).max(px(0.)));
    let origin = point(
        input
            .left()
            .max(margin)
            .min((viewport.width - width - margin).max(margin)),
        input.bottom() + px(4.),
    );
    Bounds::new(
        origin,
        gpui::size(
            width,
            px(320.).min((viewport.height - origin.y - margin).max(px(0.))),
        ),
    )
}

impl Preferences {
    fn migrate(&mut self) {
        if self.version == 0 && self.filter.is_empty() {
            self.filter = "package:mine".into();
        }
        self.version = 1;
    }
}

fn input_editor(input: &Entity<InputField>, cx: &App) -> Option<Entity<Editor>> {
    input
        .read(cx)
        .editor()
        .as_any()
        .downcast_ref::<Entity<Editor>>()
        .cloned()
}

fn input_cursor(input: &Entity<InputField>, cx: &App) -> Option<usize> {
    let editor = input_editor(input, cx)?;
    let editor = editor.read(cx);
    Some(
        editor
            .selections
            .newest_anchor()
            .head()
            .to_offset(&editor.buffer().read(cx).snapshot(cx))
            .0,
    )
}

fn query_token(text: &str, cursor: usize) -> std::ops::Range<usize> {
    let mut cursor = cursor.min(text.len());
    while !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == '"' {
            quoted = !quoted;
        }
        if !quoted && (character.is_whitespace() || matches!(character, '(' | ')' | '&' | '|')) {
            if index >= cursor {
                return start..index;
            }
            start = index + character.len_utf8();
        }
    }
    start..text.len()
}

fn complete_query(
    text: &str,
    cursor: usize,
    candidates: Vec<String>,
) -> (std::ops::Range<usize>, Vec<String>) {
    let range = query_token(text, cursor);
    let token = text
        .get(range.start..cursor.min(range.end))
        .unwrap_or_default();
    let (negative, token) = token
        .strip_prefix('-')
        .map(|token| ("-", token))
        .unwrap_or(("", token));
    let mut completions = candidates
        .into_iter()
        .filter_map(|candidate| {
            let candidate = if let Some((field, value)) = token.split_once(':') {
                let normalized_field = field.trim_end_matches(['=', '~']);
                let (candidate_field, candidate_value) = candidate.split_once(':')?;
                if candidate_field.trim_end_matches(['=', '~']) != normalized_field {
                    return None;
                }
                let value = value.trim_start_matches('"').to_lowercase();
                if !candidate_value
                    .trim_start_matches('"')
                    .to_lowercase()
                    .starts_with(&value)
                {
                    return None;
                }
                format!("{field}:{candidate_value}")
            } else {
                if !candidate.to_lowercase().starts_with(&token.to_lowercase()) {
                    return None;
                }
                candidate
            };
            let candidate = format!("{negative}{candidate}");
            (text.get(range.clone()) != Some(candidate.as_str())).then_some(candidate)
        })
        .collect::<Vec<_>>();
    completions.sort();
    completions.dedup();
    completions.truncate(12);
    (range, completions)
}

struct DisplayLine {
    text: String,
    metadata_end: usize,
    tag: std::ops::Range<usize>,
    level: std::ops::Range<usize>,
}

fn entry_bytes(entry: &Entry) -> usize {
    std::mem::size_of::<Entry>()
        + entry.tag.len()
        + entry.message.len()
        + entry.package.len()
        + entry.process.len()
}

fn small_snapshot<'a>(entries: impl ExactSizeIterator<Item = &'a Arc<Entry>>) -> bool {
    entries.len() <= INLINE_FILTER_LIMIT
        && entries.map(|entry| entry_bytes(entry)).sum::<usize>() <= INLINE_FILTER_BYTES
}

fn line_columns(entry: &Entry, compact: bool) -> usize {
    let digits = |value: u32| value.checked_ilog10().unwrap_or(0) as usize + 1;
    let mut metadata = entry.tag.width() + 5;
    if !compact {
        metadata +=
            27 + digits(entry.pid).max(5) + digits(entry.tid).max(5) + entry.package.width() + 2;
        if !entry.process.is_empty() && entry.process != entry.package {
            metadata += 2 + entry.process.width();
        }
        if let Some(uid) = entry.uid {
            metadata += 6 + digits(uid);
        }
    }
    entry
        .message
        .lines()
        .enumerate()
        .map(|(index, line)| line.width() + if index == 0 { metadata } else { 0 })
        .max()
        .unwrap_or(metadata)
}

fn filter_entries(
    entries: Vec<Arc<Entry>>,
    query: &Query,
    packages: &[String],
    compact: bool,
    now_millis: i64,
    cancelled: Option<&AtomicBool>,
) -> Option<(Vec<Arc<Entry>>, usize)> {
    let context = FilterContext {
        now_millis,
        project_packages: packages,
    };
    let mut columns = 0;
    let mut visible = Vec::new();
    for entry in entries {
        if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Relaxed)) {
            return None;
        }
        if query.matches(&entry, &context) {
            columns = columns.max(line_columns(&entry, compact));
            visible.push(entry);
        }
    }
    Some((visible, columns))
}

fn display_line(entry: &Entry, compact: bool, fold: bool) -> DisplayLine {
    let mut text = if compact {
        String::new()
    } else {
        format!("{} {:5}-{:5}  ", entry.timestamp(), entry.pid, entry.tid)
    };
    let tag_start = text.len();
    text.push_str(&entry.tag);
    let tag = tag_start..text.len();
    text.push_str("  ");
    if !compact {
        text.push_str(&entry.package);
        if !entry.process.is_empty() && entry.process != entry.package {
            text.push_str("  ");
            text.push_str(&entry.process);
        }
        if let Some(uid) = entry.uid {
            text.push_str(&format!("  uid:{uid}"));
        }
        text.push_str("  ");
    }
    let level_start = text.len();
    text.push_str(entry.level.letter());
    let level = level_start..text.len();
    text.push_str("  ");
    let metadata_end = text.len();
    if fold && entry.is_stacktrace() {
        text.push_str(&format!(
            "{} … {} lines (select to expand)",
            entry.message.lines().next().unwrap_or_default(),
            entry.message.lines().count()
        ));
    } else {
        text.push_str(&entry.message);
    }
    DisplayLine {
        text,
        metadata_end,
        tag,
        level,
    }
}

#[derive(Clone)]
struct LogcatScrollHandle {
    horizontal: ScrollHandle,
    vertical: ListState,
}

impl ScrollableHandle for LogcatScrollHandle {
    fn max_offset(&self) -> Point<Pixels> {
        point(
            self.horizontal.max_offset().x,
            self.vertical.max_offset_for_scrollbar().y,
        )
    }
    fn offset(&self) -> Point<Pixels> {
        point(
            self.horizontal.offset().x,
            self.vertical.scroll_px_offset_for_scrollbar().y,
        )
    }
    fn set_offset(&self, offset: Point<Pixels>) {
        self.horizontal.set_offset(point(offset.x, px(0.)));
        self.vertical
            .set_offset_from_scrollbar(point(px(0.), offset.y));
    }
    fn viewport(&self) -> Bounds<Pixels> {
        self.horizontal.bounds()
    }
    fn drag_started(&self) {
        self.vertical.scrollbar_drag_started();
    }
    fn drag_ended(&self) {
        self.vertical.scrollbar_drag_ended();
    }
}

fn set_input_text(input: &Entity<InputField>, text: &str, window: &mut Window, cx: &mut App) {
    let editor = input.read(cx).editor().clone();
    editor.set_text(text, window, cx);
}

fn preference_key(root: &Path) -> String {
    format!("android-logcat:{}", root.to_string_lossy())
}
fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

struct Discovery {
    devices: Vec<LogcatDevice>,
    processes: Result<Vec<Process>>,
    packages: Vec<String>,
    metadata_error: Option<String>,
}

async fn adb_output(
    serial: &str,
    args: Vec<String>,
    root: &Path,
    executor: &BackgroundExecutor,
) -> Result<String> {
    let mut arguments = vec!["-s".into(), serial.into()];
    arguments.extend(args);
    tool_output(
        adb_path()?,
        arguments,
        root,
        executor,
        Duration::from_secs(10),
    )
    .await
}

async fn discover_devices(
    previous: Vec<LogcatDevice>,
    serial: Option<String>,
    targets: Vec<AndroidTarget>,
    file: bool,
    root: &Path,
    executor: &BackgroundExecutor,
    cancelled: &AtomicBool,
) -> Result<Discovery> {
    ensure!(
        !cancelled.load(Ordering::Acquire),
        "Logcat device discovery was cancelled"
    );
    let output = tool_output(
        adb_path()?,
        vec!["devices".into(), "-l".into()],
        root,
        executor,
        Duration::from_secs(10),
    )
    .await?;
    ensure!(
        !cancelled.load(Ordering::Acquire),
        "Logcat device discovery was cancelled"
    );
    let mut devices = Vec::new();
    let mut metadata_error = None;
    for device in parse_devices(&output)? {
        if let Some(previous) = previous.iter().find(|previous| {
            previous.device.serial == device.serial
                && previous.device.state == device.state
                && !previous.release.is_empty()
        }) {
            devices.push(LogcatDevice {
                device,
                ..previous.clone()
            });
            continue;
        }
        let mut details = LogcatDevice {
            name: device.model.clone(),
            device,
            release: String::new(),
            api: String::new(),
            packages: HashMap::new(),
            event_tags: HashMap::new(),
        };
        if details.device.is_available() {
            ensure!(
                !cancelled.load(Ordering::Acquire),
                "Logcat device discovery was cancelled"
            );
            match adb_output(
                &details.device.serial,
                vec!["shell".into(), "getprop".into()],
                root,
                executor,
            )
            .await
            {
                Ok(output) => {
                    let properties = output
                        .lines()
                        .filter_map(|line| {
                            line.strip_prefix('[')?
                                .strip_suffix(']')?
                                .split_once("]: [")
                        })
                        .collect::<HashMap<_, _>>();
                    details.release = properties
                        .get("ro.build.version.release")
                        .copied()
                        .unwrap_or_default()
                        .into();
                    details.api = properties
                        .get("ro.build.version.sdk")
                        .copied()
                        .unwrap_or_default()
                        .into();
                    if let Some(name) = properties
                        .get("ro.boot.qemu.avd_name")
                        .or_else(|| properties.get("ro.kernel.qemu.avd_name"))
                    {
                        details.name = name.replace('_', " ");
                    }
                }
                Err(error) => {
                    metadata_error = Some(format!("Device details unavailable: {error:#}"))
                }
            }
        }
        if details.device.is_available() {
            ensure!(
                !cancelled.load(Ordering::Acquire),
                "Logcat device discovery was cancelled"
            );
            match adb_output(
                &details.device.serial,
                vec![
                    "shell".into(),
                    "pm".into(),
                    "list".into(),
                    "packages".into(),
                    "-U".into(),
                ],
                root,
                executor,
            )
            .await
            .and_then(|output| logcat::parse_package_uids(&output))
            {
                Ok(packages) => details.packages = packages,
                Err(error) => {
                    metadata_error = Some(format!("Application metadata unavailable: {error:#}"))
                }
            }
        }
        if details.device.is_available() {
            ensure!(
                !cancelled.load(Ordering::Acquire),
                "Logcat device discovery was cancelled"
            );
            match adb_output(
                &details.device.serial,
                vec![
                    "shell".into(),
                    "cat".into(),
                    "/system/etc/event-log-tags".into(),
                ],
                root,
                executor,
            )
            .await
            .and_then(|output| logcat::parse_event_tags(&output))
            {
                Ok(tags) => details.event_tags = tags,
                Err(error) => {
                    metadata_error = Some(format!("Event tag names unavailable: {error:#}"))
                }
            }
        }
        devices.push(details);
    }
    let serial = serial.or_else(|| {
        devices
            .iter()
            .find(|device| device.device.is_available())
            .map(|device| device.device.serial.clone())
    });
    let mut processes = if let Some(serial) = serial.clone().filter(|serial| {
        !file
            && devices
                .iter()
                .any(|device| &device.device.serial == serial && device.device.is_available())
    }) {
        ensure!(
            !cancelled.load(Ordering::Acquire),
            "Logcat device discovery was cancelled"
        );
        adb_output(
            &serial,
            vec![
                "shell".into(),
                "ps".into(),
                "-A".into(),
                "-o".into(),
                "PID,UID,ARGS".into(),
            ],
            root,
            executor,
        )
        .await
        .and_then(|output| logcat::parse_processes(&output))
    } else {
        Ok(Vec::new())
    };
    if let Ok(processes) = &mut processes
        && let Some(device) = devices
            .iter()
            .find(|device| Some(&device.device.serial) == serial.as_ref())
    {
        for process in processes {
            if let Some(packages) = device.packages.get(&process.uid)
                && !packages.contains(&process.package)
            {
                process.package = if packages.len() == 1 {
                    packages.first().cloned().unwrap_or_default()
                } else {
                    String::new()
                };
            }
        }
    }
    let mut packages = targets
        .iter()
        .filter_map(|target| target.apk().ok().and_then(|apk| apk.application_id))
        .collect::<Vec<_>>();
    packages.sort();
    packages.dedup();
    Ok(Discovery {
        devices,
        processes,
        packages,
        metadata_error,
    })
}

async fn capture(
    serial: String,
    buffers: String,
    cursor: Option<Arc<Entry>>,
    root: PathBuf,
    executor: BackgroundExecutor,
    mut sender: mpsc::Sender<Vec<Entry>>,
) -> Result<()> {
    let mut command = new_command(adb_path()?);
    command.args(["-s", &serial, "logcat", "-B", "-b", &buffers]);
    if let Some(cursor) = &cursor {
        command.args([
            "-T",
            &format!(
                "{}.{:09}",
                cursor.timestamp_millis / 1000,
                cursor.timestamp_nanos
            ),
        ]);
    }
    command
        .arg("*:V")
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().context("Cannot start ADB Logcat")?;
    let mut stdout = child.stdout.take().context("Missing ADB stdout")?;
    let mut stderr = child.stderr.take().context("Missing ADB stderr")?;
    let stderr = executor.spawn(async move {
        let mut bytes = Vec::new();
        let mut chunk = [0; 4096];
        loop {
            let count = stderr.read(&mut chunk).await?;
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(
                &chunk[..count.min((16 * 1024_usize).saturating_sub(bytes.len()))],
            );
        }
        Ok::<_, anyhow::Error>(String::from_utf8_lossy(&bytes).into_owned())
    });
    let mut decoder = Decoder::default();
    let mut last_flush = Instant::now();
    let mut batch = Vec::new();
    let mut batch_bytes = 0;
    let mut bytes = [0; 32 * 1024];
    let mut skip_cursor = cursor.is_some();
    loop {
        match select(
            Box::pin(stdout.read(&mut bytes)),
            Box::pin(
                executor.timer(Duration::from_millis(50).saturating_sub(last_flush.elapsed())),
            ),
        )
        .await
        {
            Either::Left((result, _)) => {
                let count = result?;
                if count == 0 {
                    break;
                }
                for entry in decoder.push(&bytes[..count])? {
                    if skip_cursor {
                        if let Some(cursor) = &cursor {
                            if entry.timestamp_millis < cursor.timestamp_millis {
                                continue;
                            }
                            if entry.timestamp_millis == cursor.timestamp_millis
                                && entry.timestamp_nanos == cursor.timestamp_nanos
                                && entry.pid == cursor.pid
                                && entry.tid == cursor.tid
                                && entry.tag == cursor.tag
                                && entry.message == cursor.message
                            {
                                skip_cursor = false;
                                continue;
                            }
                        }
                        skip_cursor = false;
                    }
                    batch_bytes += entry_bytes(&entry);
                    batch.push(entry);
                    if batch.len() == CAPTURE_BATCH_LIMIT || batch_bytes >= CAPTURE_BATCH_BYTES {
                        sender
                            .send(std::mem::take(&mut batch))
                            .await
                            .context("Logcat view closed")?;
                        last_flush = Instant::now();
                        batch_bytes = 0;
                    }
                }
            }
            Either::Right(_) => {}
        }
        if !batch.is_empty()
            && (batch.len() >= CAPTURE_BATCH_LIMIT
                || last_flush.elapsed() >= Duration::from_millis(50))
        {
            sender
                .send(std::mem::take(&mut batch))
                .await
                .context("Logcat view closed")?;
            last_flush = Instant::now();
            batch_bytes = 0;
        } else if batch.is_empty() {
            last_flush = Instant::now();
        }
    }
    if !batch.is_empty() {
        sender.send(batch).await.context("Logcat view closed")?;
    }
    let status = match select(
        Box::pin(child.status()),
        Box::pin(executor.timer(Duration::from_secs(2))),
    )
    .await
    {
        Either::Left((result, _)) => result?,
        Either::Right(_) => bail!("ADB did not exit after closing the Logcat stream"),
    };
    let stderr = stderr.await?;
    ensure!(
        status.success(),
        "ADB Logcat exited ({status}): {}",
        stderr.trim()
    );
    decoder.finish()?;
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use gpui::TestAppContext;
    use project::FakeFs;
    use workspace::AppState;

    #[test]
    fn preferences_default_to_current_app_and_preserve_explicit_all_logs() {
        let defaults = Preferences::default();
        assert_eq!(defaults.filter, "package:mine");
        assert!(!defaults.wrap);
        let mut legacy: Preferences =
            serde_json::from_str(r#"{"filter":"","wrap":true}"#).expect("Legacy preferences");
        legacy.migrate();
        assert_eq!(legacy.filter, "package:mine");
        assert!(legacy.wrap);
        let mut explicit: Preferences =
            serde_json::from_str(r#"{"version":1,"filter":""}"#).expect("Current preferences");
        explicit.migrate();
        assert_eq!(explicit.filter, "");
        let mut saved: Preferences =
            serde_json::from_str(r#"{"filter":"tag:Network"}"#).expect("Saved preferences");
        saved.migrate();
        assert_eq!(saved.filter, "tag:Network");
    }

    #[test]
    fn completions_preserve_groups_quoted_values_suffixes_and_operators() {
        let candidates = vec![
            "tag:\"Network request\"".into(),
            "tag:\"Database\"".into(),
            "package:mine".into(),
            "level:ERROR".into(),
        ];
        for (text, cursor, expected, completed) in [
            (
                "package:mine & (tag:Ne | level:ERROR)",
                22,
                "tag:\"Network request\"",
                "package:mine & (tag:\"Network request\" | level:ERROR)",
            ),
            (
                "-tag=:Ne",
                8,
                "-tag=:\"Network request\"",
                "-tag=:\"Network request\"",
            ),
            (
                "tag~:\"Network r",
                15,
                "tag~:\"Network request\"",
                "tag~:\"Network request\"",
            ),
            (
                "message:\"こんにちは world\" tag:Ne",
                "message:\"こんにちは world\" tag:Ne".len(),
                "tag:\"Network request\"",
                "message:\"こんにちは world\" tag:\"Network request\"",
            ),
            (
                "package:mine ",
                13,
                "level:ERROR",
                "package:mine level:ERROR",
            ),
        ] {
            let (range, suggestions) = complete_query(text, cursor, candidates.clone());
            assert!(
                suggestions.contains(&expected.to_string()),
                "{text}: {suggestions:?}"
            );
            let mut result = text.to_string();
            result.replace_range(range, expected);
            assert_eq!(result, completed);
            Query::parse(&result, false).expect("Completed query is valid");
        }
        let (range, _) = complete_query(
            "tag:\"a \\\"quoted\\\" value\" level:E",
            16,
            candidates.clone(),
        );
        assert_eq!(
            &"tag:\"a \\\"quoted\\\" value\" level:E"[range],
            "tag:\"a \\\"quoted\\\" value\""
        );
        let (_, suggestions) = complete_query("package:mine", 12, candidates);
        assert!(
            suggestions.is_empty(),
            "Do not suggest the already complete token"
        );
    }

    #[test]
    fn display_keeps_metadata_and_message_on_one_line_and_preserves_stacktraces() {
        let mut entry = logcat::import("2026-10-01 12:00:00.000 42 43 E Example: first message")
            .expect("Fixture")
            .remove(0);
        entry.package = "dev.example".into();
        entry.process = "dev.example:worker".into();
        entry.uid = Some(10123);
        let display = display_line(&entry, false, false);
        assert_eq!(display.text.lines().count(), 1);
        assert_eq!(&display.text[display.tag], "Example");
        assert_eq!(&display.text[display.level], "E");
        assert_eq!(&display.text[display.metadata_end..], "first message");
        assert!(display.text.contains("dev.example:worker"));
        assert!(display.text.contains("uid:10123"));
        entry.message = "failure\n\tat dev.example.Main.run(Main.kt:42)\n\tat dev.example.Main.start(Main.kt:50)".into();
        assert_eq!(display_line(&entry, false, false).text.lines().count(), 3);
        let folded = display_line(&entry, false, true);
        assert_eq!(folded.text.lines().count(), 1);
        assert!(folded.text.ends_with("3 lines (select to expand)"));
        let compact = display_line(&entry, true, false);
        assert!(!compact.text.contains("2026-10-01"));
        assert!(!compact.text.contains("uid:"));
        assert!(compact.text.ends_with(&entry.message));
    }

    pub(crate) async fn viewer(
        cx: &mut TestAppContext,
        displayed: bool,
    ) -> (
        Entity<Workspace>,
        Entity<LogcatView>,
        &mut gpui::VisualTestContext,
    ) {
        cx.update(|cx| {
            AppState::test(cx);
            editor::init(cx);
            project::trusted_worktrees::init(Default::default(), cx);
            cx.bind_keys([
                gpui::KeyBinding::new("cmd-f", Find, Some("AndroidLogcat")),
                gpui::KeyBinding::new("cmd-c", Copy, Some("AndroidLogcat")),
                gpui::KeyBinding::new("escape", CloseFind, Some("AndroidLogcatSearch")),
                gpui::KeyBinding::new(
                    "down",
                    NextSuggestion,
                    Some("AndroidLogcatFilter && showing_suggestions"),
                ),
                gpui::KeyBinding::new(
                    "up",
                    PreviousSuggestion,
                    Some("AndroidLogcatFilter && showing_suggestions"),
                ),
                gpui::KeyBinding::new(
                    "tab",
                    AcceptSuggestion,
                    Some("AndroidLogcatFilter && showing_suggestions"),
                ),
                gpui::KeyBinding::new(
                    "escape",
                    DismissSuggestions,
                    Some("AndroidLogcatFilter && showing_suggestions"),
                ),
            ]);
        });
        let filesystem = FakeFs::new(cx.executor());
        filesystem
            .insert_tree(
                "/logcat",
                serde_json::json!({"settings.gradle.kts":"", "gradlew":""}),
            )
            .await;
        let project = Project::test(filesystem, [Path::new("/logcat")], cx).await;
        let store = project.read_with(cx, |project, _| project.worktree_store());
        cx.update(|cx| {
            TrustedWorktrees::try_get_global(cx)
                .expect("Trust store")
                .update(cx, |trusted, cx| {
                    trusted.trust(
                        &store,
                        [project::trusted_worktrees::PathTrust::AbsPath(
                            PathBuf::from("/logcat"),
                        )]
                        .into_iter()
                        .collect(),
                        cx,
                    );
                })
        });
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let view = workspace.update_in(cx, |workspace, window, cx| {
            cx.new(|cx| {
                LogcatView::new(
                    workspace.weak_handle(),
                    project,
                    PathBuf::from("/logcat"),
                    None,
                    Vec::new(),
                    window,
                    cx,
                )
            })
        });
        view.update_in(cx, |view, window, cx| {
            view.preferences = Preferences::default();
            view.query = Query::parse("package:mine", false).expect("Default query");
            set_input_text(&view.filter_input, "package:mine", window, cx);
        });
        if displayed {
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.add_item_to_active_pane(Box::new(view.clone()), None, true, window, cx)
            });
            cx.run_until_parked();
        }
        (workspace, view, cx)
    }

    pub(super) fn draw_view(_: &Entity<LogcatView>, cx: &mut gpui::VisualTestContext) {
        cx.simulate_resize(gpui::size(px(640.), px(320.)));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn drag_text(cx: &mut gpui::VisualTestContext, start: Point<Pixels>, end: Point<Pixels>) {
        cx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: start,
            ..Default::default()
        });
        cx.simulate_event(gpui::MouseMoveEvent {
            position: end,
            pressed_button: Some(gpui::MouseButton::Left),
            ..Default::default()
        });
        cx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: end,
            ..Default::default()
        });
    }

    #[test]
    fn suggestion_geometry_stays_below_input_and_within_available_space() {
        let input = Bounds::new(point(px(550.), px(220.)), gpui::size(px(300.), px(32.)));
        let bounds = suggestion_bounds(input, gpui::size(px(640.), px(320.)));
        assert!(bounds.top() > input.bottom());
        assert!(bounds.right() <= px(632.));
        assert!(bounds.bottom() <= px(312.));
        assert!(bounds.size.height < px(100.));
        let no_space = suggestion_bounds(input, gpui::size(px(640.), px(250.)));
        assert_eq!(no_space.size.height, px(0.));
        assert!(no_space.top() > input.bottom(), "Never flip over the input");
    }

    #[gpui::test]
    async fn suggestions_are_below_input_left_aligned_and_scrollable(cx: &mut TestAppContext) {
        let (_, view, cx) = viewer(cx, true).await;
        view.update_in(cx, |view, window, cx| {
            window.focus(&view.filter_input.focus_handle(cx), cx);
            view.completions = (0..30)
                .map(|index| format!("tag:Suggestion{index}"))
                .collect();
            view.completion_index = 0;
            cx.notify();
        });
        draw_view(&view, cx);
        let input = cx.debug_bounds("logcat-filter-input").expect("Filter");
        let popup = cx.debug_bounds("logcat-filter-suggestions").expect("Popup");
        let label = cx
            .debug_bounds("logcat-completion-label-0")
            .expect("Suggestion label");
        let device = cx
            .debug_bounds("logcat-device-label")
            .expect("Device label");
        assert!(popup.top() >= input.bottom());
        assert!(popup.bottom() <= px(312.));
        assert!(label.left() - popup.left() < px(20.));
        assert!(label.size.width > popup.size.width / 2.);
        assert!(
            device.left()
                - cx.debug_bounds("logcat-device-selector")
                    .expect("Device selector")
                    .left()
                < px(40.),
            "Device text starts next to its icon"
        );
        view.read_with(cx, |view, _| {
            assert!(view.completion_scroll.max_offset().y > px(0.))
        });
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: popup.center(),
            delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-80.))),
            ..Default::default()
        });
        draw_view(&view, cx);
        view.read_with(cx, |view, _| {
            assert!(view.completion_scroll.offset().y < px(0.))
        });
        view.update_in(cx, |view, _, cx| {
            view.completion_index = 28;
            view.move_completion(false, cx);
        });
        draw_view(&view, cx);
        let last = cx
            .debug_bounds("logcat-completion-label-29")
            .expect("Last suggestion");
        assert!(last.top() >= popup.top());
        assert!(last.bottom() <= popup.bottom());
    }

    #[gpui::test]
    async fn scrollbar_tracks_stay_in_viewport_after_horizontal_scrolling(cx: &mut TestAppContext) {
        let (_, view, cx) = viewer(cx, true).await;
        view.update_in(cx, |view, _, cx| {
            view.query = Query::default();
            let mut entry = logcat::import("2026-10-01 12:00:00.000 42 43 I Example: message")
                .expect("Fixture")
                .remove(0);
            entry.message = "long line ".repeat(100);
            view.receive(vec![entry; 50], cx);
            view.list_state.pause_following_tail();
        });
        draw_view(&view, cx);
        view.update_in(cx, |view, _, _| {
            view.horizontal_scroll.set_offset(point(px(-2000.), px(0.)));
            view.list_state
                .set_offset_from_scrollbar(point(px(0.), px(0.)));
        });
        draw_view(&view, cx);
        let bounds = cx
            .debug_bounds("logcat-scrollbar-frame")
            .expect("Fixed frame");
        let before = view.read_with(cx, |view, _| view.horizontal_scroll.offset().x);
        cx.simulate_click(
            point(
                bounds.left() + bounds.size.width * 0.7,
                bounds.bottom() - px(7.),
            ),
            Default::default(),
        );
        draw_view(&view, cx);
        let after = view.read_with(cx, |view, _| view.horizontal_scroll.offset().x);
        assert!(
            after < before,
            "Horizontal track responds at the fixed bottom edge"
        );
        cx.simulate_click(
            point(
                bounds.left() + bounds.size.width * 0.2,
                bounds.bottom() - px(7.),
            ),
            Default::default(),
        );
        draw_view(&view, cx);
        view.read_with(cx, |view, _| {
            assert!(view.horizontal_scroll.offset().x > after)
        });
        let thumb = point(bounds.right() - px(7.), bounds.top() + px(10.));
        cx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: thumb,
            ..Default::default()
        });
        cx.simulate_event(gpui::MouseMoveEvent {
            position: point(thumb.x, bounds.bottom() - px(24.)),
            pressed_button: Some(gpui::MouseButton::Left),
            ..Default::default()
        });
        cx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: point(thumb.x, bounds.bottom() - px(24.)),
            ..Default::default()
        });
        draw_view(&view, cx);
        view.read_with(cx, |view, _| {
            assert!(
                view.list_state.scroll_px_offset_for_scrollbar().y < px(-100.),
                "Vertical thumb stays on the right when text is scrolled horizontally"
            )
        });
        assert_eq!(cx.debug_bounds("logcat-scrollbar-frame"), Some(bounds));
    }

    #[gpui::test]
    async fn dragging_text_copies_partial_unicode_and_right_click_opens_message_menu(
        cx: &mut TestAppContext,
    ) {
        let (_, view, cx) = viewer(cx, true).await;
        view.update_in(cx, |view, _, cx| {
            view.query = Query::default();
            view.preferences.compact = true;
            let entry = logcat::import("2026-10-01 12:00:00.000 42 43 I Tag: before 日本語 after")
                .expect("Fixture")
                .remove(0);
            view.receive(vec![entry.clone(), entry], cx);
        });
        draw_view(&view, cx);
        let (start, end) = view.read_with(cx, |view, _| {
            let entry = &view.visible[0];
            let display = display_line(entry, true, false);
            let offset = display.text.find("日本語").expect("Unicode");
            let layouts = view.text_layouts.borrow();
            let layout = &layouts[&entry.id].layout;
            (
                layout.position_for_index(offset).expect("Start") + point(px(0.), px(8.)),
                layout
                    .position_for_index(offset + "日本語".len())
                    .expect("End")
                    + point(px(0.), px(8.)),
            )
        });
        drag_text(cx, start, end);
        draw_view(&view, cx);
        view.read_with(cx, |view, _| assert_eq!(view.selected_text(), "日本語"));
        cx.simulate_keystrokes("cmd-c");
        assert_eq!(
            cx.read_from_clipboard().expect("Clipboard").text(),
            Some("日本語".into())
        );
        cx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Right,
            position: start,
            ..Default::default()
        });
        draw_view(&view, cx);
        assert!(cx.debug_bounds("MENU_ITEM-Copy selection").is_some());
        let item = cx
            .debug_bounds("MENU_ITEM-Copy message")
            .expect("Right-click menu");
        cx.simulate_click(item.center(), Default::default());
        assert_eq!(
            cx.read_from_clipboard().expect("Clipboard").text(),
            Some("before 日本語 after".into())
        );
        view.update_in(cx, |view, _, cx| {
            view.clear(cx);
            assert!(view.text_selection.is_none());
        });
    }

    #[gpui::test]
    async fn text_selection_reverses_across_wrapped_rows_and_tracks_live_updates(
        cx: &mut TestAppContext,
    ) {
        let (_, view, cx) = viewer(cx, true).await;
        view.update_in(cx, |view, _, cx| {
            view.query = Query::default();
            view.preferences.compact = true;
            view.preferences.wrap = true;
            let mut entry =
                logcat::import("2026-10-01 12:00:00.000 42 43 I Tag: before 日本語 after")
                    .expect("Fixture")
                    .remove(0);
            entry.message = "before 日本語 after ".repeat(9);
            view.receive(vec![entry.clone(), entry], cx);
        });
        draw_view(&view, cx);
        let (start, end, expected) = view.read_with(cx, |view, _| {
            let first = display_line(&view.visible[0], true, false).text;
            let second = display_line(&view.visible[1], true, false).text;
            let start = first.rfind("日本語").expect("Start");
            let end = second.find("日本語").expect("End") + "日本語".len();
            let layouts = view.text_layouts.borrow();
            let first_layout = &layouts[&view.visible[0].id].layout;
            assert!(
                first_layout.bounds().size.height > first_layout.line_height(),
                "Fixture wraps"
            );
            (
                first_layout
                    .position_for_index(start)
                    .expect("Start position")
                    + point(px(0.), px(8.)),
                layouts[&view.visible[1].id]
                    .layout
                    .position_for_index(end)
                    .expect("End position")
                    + point(px(0.), px(8.)),
                format!("{}\n{}", &first[start..], &second[..end]),
            )
        });
        for (start, end) in [(start, end), (end, start)] {
            drag_text(cx, start, end);
            draw_view(&view, cx);
            view.read_with(cx, |view, _| {
                assert_eq!(view.selected_text(), expected);
                assert!(!view.selecting_text);
            });
            cx.simulate_keystrokes("cmd-c");
            assert_eq!(
                cx.read_from_clipboard().expect("Clipboard").text(),
                Some(expected.clone())
            );
        }
        view.update_in(cx, |view, _, cx| {
            let entry = logcat::import("2026-10-01 12:00:01.000 42 43 I Other: new message")
                .expect("Fixture")
                .remove(0);
            view.receive(vec![entry], cx);
            assert_eq!(
                view.selected_text(),
                expected,
                "Stable IDs preserve selection during capture"
            );
            view.query = Query::parse("tag:Other", false).expect("Filter");
            view.rebuild(cx);
            assert!(
                view.text_selection.is_none(),
                "Hidden selection endpoints are discarded"
            );
            assert!(view.selected_text().is_empty());
        });
    }

    #[gpui::test]
    async fn find_is_hidden_until_invoked_and_escape_restores_log_focus(cx: &mut TestAppContext) {
        let (_, view, cx) = viewer(cx, true).await;
        view.update_in(cx, |view, window, cx| {
            view.query = Query::default();
            let entry = logcat::import("2026-10-01 12:00:00.000 42 43 I Example: needle")
                .expect("Fixture")
                .remove(0);
            view.receive(vec![entry.clone(), entry], cx);
            assert!(!view.search_visible);
            window.focus(&view.filter_input.focus_handle(cx), cx);
        });
        draw_view(&view, cx);
        cx.simulate_keystrokes("cmd-f");
        view.read_with(cx, |view, _| assert!(view.search_visible));
        draw_view(&view, cx);
        cx.simulate_input("needle");
        view.update_in(cx, |view, _, cx| {
            assert_eq!(view.search_matches, vec![0, 1]);
            view.find(true, cx);
            assert_eq!(view.search_position, Some(1));
            view.find(false, cx);
            assert_eq!(view.search_position, Some(0));
            view.find(false, cx);
            assert_eq!(view.search_position, Some(1));
        });
        draw_view(&view, cx);
        cx.simulate_keystrokes("escape");
        view.read_with(cx, |view, cx| {
            assert!(!view.search_visible);
            assert!(view.search_matches.is_empty());
            assert!(view.search.is_none());
            assert_eq!(view.search_input.read(cx).text(cx), "needle");
        });
        view.update_in(cx, |view, window, _| {
            assert!(view.focus_handle.is_focused(window))
        });
    }

    #[gpui::test]
    async fn query_popup_keyboard_completion_preserves_following_terms(cx: &mut TestAppContext) {
        let (_, view, cx) = viewer(cx, true).await;
        view.update_in(cx, |view, window, cx| {
            set_input_text(&view.filter_input, "tag:Old level:ERROR", window, cx);
            let editor = input_editor(&view.filter_input, cx).expect("Editor");
            editor.update(cx, |editor, cx| {
                editor.change_selections(SelectionEffects::no_scroll(), window, cx, |selections| {
                    selections.select_ranges([MultiBufferOffset(0)..MultiBufferOffset(7)])
                })
            });
            window.focus(&view.filter_input.focus_handle(cx), cx);
        });
        draw_view(&view, cx);
        cx.simulate_input("pa");
        view.read_with(cx, |view, _| {
            assert!(view.completions.contains(&"package:mine".into()))
        });
        draw_view(&view, cx);
        cx.simulate_keystrokes("down up");
        view.update_in(cx, |view, _, _| {
            view.completion_index = view
                .completions
                .iter()
                .position(|item| item == "package:mine")
                .expect("Completion")
        });
        draw_view(&view, cx);
        cx.simulate_keystrokes("tab");
        view.read_with(cx, |view, cx| {
            assert_eq!(
                view.filter_input.read(cx).text(cx),
                "package:mine level:ERROR"
            );
            assert!(view.completions.is_empty());
            assert_eq!(input_cursor(&view.filter_input, cx), Some(12));
        });
        cx.simulate_input(" ");
        draw_view(&view, cx);
        cx.simulate_keystrokes("escape");
        view.read_with(cx, |view, _| assert!(view.completions.is_empty()));
        view.update_in(cx, |view, window, cx| {
            view.update_completions(window, cx);
            window.focus(&view.focus_handle, cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert!(view.completions.is_empty()));
    }

    #[gpui::test]
    async fn current_app_query_excludes_other_apps_and_tracks_scope_changes(
        cx: &mut TestAppContext,
    ) {
        let (_, view, cx) = viewer(cx, true).await;
        view.update_in(cx, |view, _, cx| {
            let mut entry = logcat::import("2026-10-01 12:00:00.000 42 43 I Example: message")
                .expect("Fixture")
                .remove(0);
            entry.package = "dev.first".into();
            let mut second = entry.clone();
            second.package = "dev.second".into();
            view.project_packages = vec!["dev.first".into()];
            view.receive(vec![entry, second], cx);
            assert_eq!(view.visible.len(), 1);
            assert_eq!(view.visible[0].package, "dev.first");
            view.project_packages = vec!["dev.second".into()];
            view.rebuild(cx);
            assert_eq!(view.visible.len(), 1);
            assert_eq!(view.visible[0].package, "dev.second");
            view.project_packages.clear();
            view.rebuild(cx);
            assert!(view.visible.is_empty());
        });
    }

    #[gpui::test]
    async fn package_mine_uses_the_selected_android_target(cx: &mut TestAppContext) {
        let (workspace, view, cx) = viewer(cx, true).await;
        let first = AndroidTarget {
            module: ":first".into(),
            variant: "debug".into(),
            output_listing: "/logcat/first/output-metadata.json".into(),
        };
        let second = AndroidTarget {
            module: ":second".into(),
            variant: "release".into(),
            output_listing: "/logcat/second/output-metadata.json".into(),
        };
        let panel = workspace.update_in(cx, |workspace, window, cx| {
            let panel = cx.new(|cx| {
                AndroidPanel::new(workspace.weak_handle(), workspace.project().clone(), cx)
            });
            panel.update(cx, |panel, _| {
                panel.root = Some(PathBuf::from("/logcat"));
                panel.targets = vec![first.clone(), second.clone()];
                panel.selected_target = Some(first.clone());
            });
            workspace.add_panel(panel.clone(), window, cx);
            panel
        });
        view.read_with(cx, |view, cx| {
            assert_eq!(view.current_targets(cx), vec![first.clone()])
        });
        panel.update(cx, |panel, _| panel.selected_target = Some(second.clone()));
        view.read_with(cx, |view, cx| {
            assert_eq!(view.current_targets(cx), vec![second])
        });
        panel.update(cx, |panel, _| {
            panel.root = Some(PathBuf::from("/another-project"))
        });
        view.read_with(cx, |view, cx| {
            assert_eq!(view.current_targets(cx), view.targets)
        });
    }

    #[gpui::test]
    async fn unwrapped_lines_share_horizontal_scroll_and_wrap_resets_it(cx: &mut TestAppContext) {
        let (_, view, cx) = viewer(cx, true).await;
        view.update_in(cx, |view, _, cx| {
            view.query = Query::default();
            let mut entry = logcat::import("2026-10-01 12:00:00.000 42 43 I Example: message")
                .expect("Fixture")
                .remove(0);
            entry.message = "long Unicode 日本語 line ".repeat(50);
            view.receive(vec![entry; 50], cx);
        });
        draw_view(&view, cx);
        let (bounds, vertical_offset) = view.read_with(cx, |view, _| {
            (
                view.horizontal_scroll.bounds(),
                view.list_state.scroll_px_offset_for_scrollbar().y,
            )
        });
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: bounds.center(),
            delta: gpui::ScrollDelta::Pixels(point(px(-120.), px(0.))),
            ..Default::default()
        });
        draw_view(&view, cx);
        view.read_with(cx, |view, _| {
            assert!(
                view.horizontal_scroll.offset().x < px(0.),
                "Horizontal gestures scroll the shared viewport"
            );
            assert_eq!(
                view.list_state.scroll_px_offset_for_scrollbar().y,
                vertical_offset
            );
        });
        let horizontal_offset = view.read_with(cx, |view, _| view.horizontal_scroll.offset().x);
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: bounds.center(),
            delta: gpui::ScrollDelta::Pixels(point(px(0.), px(40.))),
            ..Default::default()
        });
        draw_view(&view, cx);
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.horizontal_scroll.offset().x,
                horizontal_offset,
                "Vertical scrolling does not shift text horizontally"
            );
            assert!(view.list_state.scroll_px_offset_for_scrollbar().y > vertical_offset);
        });
        view.update_in(cx, |view, _, _| {
            let scroll = LogcatScrollHandle {
                horizontal: view.horizontal_scroll.clone(),
                vertical: view.list_state.clone(),
            };
            assert!(scroll.viewport().size.width > px(100.));
            assert!(scroll.max_offset().x > px(1000.));
            assert!(scroll.max_offset().y > px(0.));
            assert!(
                (scroll.max_offset().y + scroll.viewport().size.height) / 50. < px(35.),
                "Unwrapped records occupy one row"
            );
            scroll.set_offset(point(-scroll.max_offset().x, px(-50.)));
        });
        draw_view(&view, cx);
        view.update_in(cx, |view, _, cx| {
            assert!(view.horizontal_scroll.offset().x < px(-1000.));
            assert!(view.list_state.scroll_px_offset_for_scrollbar().y < px(0.));
            view.toggle_wrap(cx);
            assert_eq!(view.horizontal_scroll.offset().x, px(0.));
        });
        draw_view(&view, cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.horizontal_scroll.max_offset().x, px(0.))
        });
        cx.simulate_resize(gpui::size(px(400.), px(320.)));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.horizontal_scroll.max_offset().x,
                px(0.),
                "Wrapped text follows a narrower viewport"
            )
        });
    }

    #[gpui::test]
    async fn frozen_logs_survive_eviction_and_disconnected_selection_is_preserved(
        cx: &mut TestAppContext,
    ) {
        let _state = cx.update(|cx| {
            let state = AppState::test(cx);
            editor::init(cx);
            project::trusted_worktrees::init(Default::default(), cx);
            state
        });
        let filesystem = FakeFs::new(cx.executor());
        filesystem
            .insert_tree(
                "/logcat",
                serde_json::json!({"settings.gradle.kts": "", "gradlew": ""}),
            )
            .await;
        let project = Project::test(filesystem, [Path::new("/logcat")], cx).await;
        let store = project.read_with(cx, |project, _| project.worktree_store());
        cx.update(|cx| {
            TrustedWorktrees::try_get_global(cx)
                .expect("Trust store")
                .update(cx, |trusted, cx| {
                    trusted.trust(
                        &store,
                        [project::trusted_worktrees::PathTrust::AbsPath(
                            PathBuf::from("/logcat"),
                        )]
                        .into_iter()
                        .collect(),
                        cx,
                    );
                });
        });
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let view = workspace.update_in(cx, |workspace, window, cx| {
            cx.new(|cx| {
                LogcatView::new(
                    workspace.weak_handle(),
                    project,
                    PathBuf::from("/logcat"),
                    Some("device-a".into()),
                    Vec::new(),
                    window,
                    cx,
                )
            })
        });
        view.update_in(cx, |view, window, cx| {
            view.buffer = Buffer::new(1024);
            view.query = Query::parse("", false).expect("Valid filter");
            let initial = logcat::import("2026-10-01 12:00:00.000 42 43 E Example: initial")
                .expect("Fixture")
                .remove(0);
            view.receive(vec![initial.clone()], cx);
            assert_eq!(view.visible.len(), 1);
            view.pause(cx);
            let mut live = initial;
            live.message = "x".repeat(800);
            view.receive(vec![live], cx);
            assert_eq!(view.visible[0].message, "initial");
            assert!(view.buffer.dropped > 0);
            view.pause(cx);
            assert_eq!(view.visible.len(), 1);
            assert_eq!(view.visible[0].message.len(), 800);
            set_input_text(&view.filter_input, "message:x", window, cx);
            view.update_filter(cx);
            set_input_text(&view.filter_input, "tag:", window, cx);
            view.update_filter(cx);
            assert!(view.filter_error.is_some());
            assert_eq!(view.visible.len(), 1);
            view.show_find(window, cx);
            view.search_regex = true;
            set_input_text(&view.search_input, "x{3}", window, cx);
            view.update_search(cx);
            view.find(false, cx);
            assert_eq!(view.search_position, Some(0));
            assert_eq!(view.selected.len(), 1);
            view.pause(cx);
            view.clear(cx);
            assert!(view.buffer.entries.is_empty());
            assert!(view.visible.is_empty());
            assert!(view.paused.as_ref().is_some_and(Vec::is_empty));

            let device = |serial: &str| LogcatDevice {
                device: Device {
                    serial: serial.into(),
                    state: "device".into(),
                    model: "Phone".into(),
                },
                release: "15".into(),
                api: "35".into(),
                name: "Phone".into(),
                packages: HashMap::new(),
                event_tags: HashMap::new(),
            };
            let discovery = |devices| {
                Ok(Discovery {
                    devices,
                    processes: Ok(Vec::new()),
                    packages: Vec::new(),
                    metadata_error: None,
                })
            };
            view.stopped = true;
            view.apply_discovery(discovery(vec![device("device-a"), device("device-b")]), cx);
            assert!(view.connected);
            view.apply_discovery(discovery(vec![device("device-b")]), cx);
            assert!(!view.connected);
            assert_eq!(view.preferences.serial.as_deref(), Some("device-a"));
            assert!(
                view.devices
                    .iter()
                    .any(|device| device.device.serial == "device-a"
                        && device.device.state == "disconnected")
            );
            view.stopped = true;
            view.apply_discovery(discovery(vec![device("device-a"), device("device-b")]), cx);
            assert!(view.connected);
            assert_eq!(view.preferences.serial.as_deref(), Some("device-a"));
        });
    }
}

#[cfg(test)]
#[path = "android_logcat_runtime_tests.rs"]
mod runtime_tests;

#[cfg(test)]
#[path = "android_logcat_paint_tests.rs"]
mod paint_tests;
#[gpui::test]
async fn logcat_owns_only_its_trusted_project_and_rejects_stale_device_dispatch(
    cx: &mut TestAppContext,
) -> Result<()> {
    cx.update(|cx| {
        let state = AppState::test(cx);
        editor::init(cx);
        workspace::init(state, cx);
        project::trusted_worktrees::init(Default::default(), cx);
        crate::init(cx);
    });
    let filesystem = FakeFs::new(cx.executor());
    filesystem
        .insert_tree(
            "/logcat-owner",
            serde_json::json!({"Main.kt":"fun main() {}", "Other.kt":"fun other() {}"}),
        )
        .await;
    filesystem
        .insert_tree(
            "/python-untrusted",
            serde_json::json!({"main.py":"print(1)"}),
        )
        .await;
    let project = Project::test_with_worktree_trust(
        filesystem,
        [Path::new("/logcat-owner"), Path::new("/python-untrusted")],
        cx,
    )
    .await;
    cx.update(|cx| {
        let store = project.read(cx).worktree_store();
        let root = project
            .read(cx)
            .visible_worktrees(cx)
            .find(|worktree| worktree.read(cx).abs_path().as_ref() == Path::new("/logcat-owner"))
            .context("Logcat root")?
            .read(cx)
            .id();
        TrustedWorktrees::try_get_global(cx)
            .context("Trust store")?
            .update(cx, |trust, cx| {
                trust.trust(
                    &store,
                    [project::trusted_worktrees::PathTrust::Worktree(root)]
                        .into_iter()
                        .collect(),
                    cx,
                )
            });
        project_surfaces::tests::publish_catalogue(
            &project,
            Path::new("/logcat-owner"),
            &[android_tools::project_context::PluginId::AndroidApplication],
            &[("android", "androidJvm")],
            true,
            cx,
        )
    })?;
    let (workspace, visual) =
        cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
    workspace
        .update_in(visual, |workspace, window, cx| {
            workspace.open_abs_path(
                Path::new("/logcat-owner/Main.kt"),
                Default::default(),
                window,
                cx,
            )
        })
        .await?;
    visual.run_until_parked();
    let view = workspace.update_in(visual, |workspace, window, cx| {
        cx.new(|cx| {
            LogcatView::new(
                workspace.weak_handle(),
                project.clone(),
                PathBuf::from("/logcat-owner"),
                None,
                Vec::new(),
                window,
                cx,
            )
        })
    });
    let owner = view.read_with(visual, |view, cx| {
        assert!(
            view.ensure_trusted(cx).is_ok(),
            "An unrelated restricted root must not block the owning Android root"
        );
        view.device_context_token(cx)
    })?;
    let cancellation = WorkCancellation::default();
    let cancelled = cancellation.0.clone();
    view.update(visual, |view, _| {
        view.device_owner = Some(owner.clone());
        view.device_cancellation = Some(cancellation);
        view.capturing = true;
        // Ready owned tasks prove observer cleanup without executing ADB.
        view.stream_task = Some(Task::ready(()));
        view.device_task = Some(Task::ready(()));
        view.control_task = Some(Task::ready(()));
    });
    workspace.update_in(visual, |workspace, window, cx| {
        workspace.open_abs_path(Path::new("/logcat-owner/Other.kt"), Default::default(), window, cx)
    }).await?;
    visual.run_until_parked();
    assert!(!cancelled.load(Ordering::Acquire), "Same-project editor changes must keep Logcat running");
    view.read_with(visual, |view, cx| {
        assert!(view.verify_device_context(&owner, cx).is_ok());
        assert!(view.capturing && view.device_task.is_some() && view.stream_task.is_some() && view.control_task.is_some());
    });
    workspace
        .update_in(visual, |workspace, window, cx| {
            workspace.open_abs_path(
                Path::new("/python-untrusted/main.py"),
                Default::default(),
                window,
                cx,
            )
        })
        .await?;
    visual.run_until_parked();
    assert!(cancelled.load(Ordering::Acquire));
    view.update(visual, |view, cx| {
        assert!(view.verify_device_context(&owner, cx).is_err());
        assert!(
            view.device_task.is_none() && view.stream_task.is_none() && view.control_task.is_none()
        );
        assert!(!view.capturing);
        view.watch_devices(cx);
        view.start_capture(false, cx);
        view.device_command(vec!["shell".into(), "true".into()], cx);
        assert!(
            view.device_task.is_none() && view.stream_task.is_none() && view.control_task.is_none(),
            "Stale direct entry points must reject before ADB starts"
        );
    });
    let python = workspace.read_with(visual, |workspace, cx| workspace.active_item(cx).context("Python item"))?;
    workspace.update_in(visual, |workspace, window, cx| {
        workspace.open_abs_path(Path::new("/logcat-owner/Main.kt"), Default::default(), window, cx)
    }).await?;
    visual.run_until_parked();
    let android = workspace.read_with(visual, |workspace, cx| workspace.active_item(cx).context("Android item"))?;
    let current = view.read_with(visual, |view, cx| view.device_context_token(cx))?;
    let rapid_cancellation = WorkCancellation::default();
    let rapid_cancelled = rapid_cancellation.0.clone();
    view.update(visual, |view, _| {
        view.device_owner = Some(current.clone());
        view.device_cancellation = Some(rapid_cancellation);
        view.capturing = true;
        view.stream_task = Some(Task::ready(()));
        view.device_task = Some(Task::ready(()));
        view.control_task = Some(Task::ready(()));
    });
    workspace.update_in(visual, |workspace, window, cx| {
        assert!(workspace.activate_item(python.as_ref(), false, false, window, cx));
        assert!(workspace.activate_item(android.as_ref(), false, false, window, cx));
    });
    visual.run_until_parked();
    assert!(rapid_cancelled.load(Ordering::Acquire), "A/B/A without pumping must not revive a Logcat stream");
    view.read_with(visual, |view, cx| {
        assert!(view.verify_device_context(&current, cx).is_err());
        assert!(!view.capturing && view.stream_task.is_none() && view.device_task.is_none() && view.control_task.is_none());
    });
    Ok(())
}
