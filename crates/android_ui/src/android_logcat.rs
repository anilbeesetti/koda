use super::*;
use android_tools::logcat::{self, Buffer, Decoder, Entry, FilterContext, Level, Process, Query};
use chrono::Utc;
use editor::Editor;
use futures::{AsyncReadExt as _, SinkExt as _, channel::mpsc};
use gpui::{ClipboardItem, FollowMode, ListAlignment, ListState, PathPromptOptions, list};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, io::Read as _, time::Instant};
use theme_settings::ThemeSettings;
use ui::WithScrollbar;
use ui_input::{ErasedEditorEvent, InputField};
use util::command::Stdio;
use workspace::{
    SplitDirection,
    item::{Item, ItemEvent},
};

actions!(
    android_logcat,
    [
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
    let existing = workspace
        .items_of_type::<LogcatView>(cx)
        .find(|view| view.read(cx).root == root);
    if let Some(view) = existing {
        view.update(cx, |view, _| view.targets = targets);
        workspace.activate_item(&view, true, true, window, cx);
        return;
    }
    let view = cx.new(|cx| {
        LogcatView::new(
            workspace.weak_handle(),
            workspace.project().clone(),
            root,
            serial,
            targets,
            window,
            cx,
        )
    });
    view.update(cx, |view, cx| view.watch_devices(cx));
    workspace.add_item_to_active_pane(Box::new(view), None, true, window, cx);
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Preferences {
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
            filter: String::new(),
            match_case: false,
            wrap: true,
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

pub(super) struct LogcatView {
    workspace: WeakEntity<Workspace>,
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
    selected_package: Option<String>,
    selected_process: Option<String>,
    minimum_level: Level,
    buffers: String,
    buffer: Buffer,
    cursor: Option<Arc<Entry>>,
    paused: Option<Vec<Arc<Entry>>>,
    visible: Vec<Arc<Entry>>,
    selected: HashSet<u64>,
    selection_anchor: Option<u64>,
    search: Option<logcat::Search>,
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
    control_task: Option<Task<()>>,
    persist_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl LogcatView {
    fn new(
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
        preferences.capacity = preferences.capacity.clamp(1024 * 1024, 64 * 1024 * 1024);
        if serial.is_some() {
            preferences.serial = serial;
        }
        let filter_input =
            cx.new(|cx| InputField::new(window, cx, "Filter: package:mine tag:MyTag level:WARN"));
        set_input_text(&filter_input, &preferences.filter, window, cx);
        let search_input = cx.new(|cx| InputField::new(window, cx, "Find in displayed messages"));
        let mut subscriptions = Vec::new();
        for (input, is_filter) in [(filter_input.clone(), true), (search_input.clone(), false)] {
            let view = cx.weak_entity();
            let editor = input.read(cx).editor().clone();
            let subscription = editor.subscribe(
                Box::new(move |event, _, cx| {
                    if event == ErasedEditorEvent::BufferEdited {
                        view.update(cx, |view, cx| {
                            if is_filter {
                                view.update_filter(cx);
                            } else {
                                view.update_search(cx);
                            }
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
        let list_state = ListState::new(0, ListAlignment::Top, px(1024.));
        list_state.set_follow_mode(FollowMode::Tail);
        Self {
            workspace,
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
            selected_package: None,
            selected_process: None,
            minimum_level: Level::Verbose,
            buffers: "main,system,crash".into(),
            paused: None,
            visible: Vec::new(),
            selected: HashSet::new(),
            selection_anchor: None,
            search: None,
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
            control_task: None,
            persist_task: None,
            _subscriptions: subscriptions,
        }
    }

    fn ensure_trusted(&self, cx: &App) -> Result<()> {
        let project = self.project.read(cx);
        ensure!(project.is_local(), "Logcat supports local projects only");
        ensure!(
            !TrustedWorktrees::has_restricted_worktrees(&project.worktree_store(), cx),
            "Trust this project before capturing device logs"
        );
        ensure!(
            project
                .worktrees(cx)
                .any(|worktree| self.root.starts_with(worktree.read(cx).abs_path().as_ref())),
            "The Logcat project is no longer open"
        );
        Ok(())
    }

    fn watch_devices(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self.ensure_trusted(cx) {
            self.error = Some(error.to_string());
            cx.notify();
            return;
        }
        let executor = cx.background_executor().clone();
        let root = self.root.clone();
        self.device_task = Some(cx.spawn(async move |view, cx| {
            loop {
                let Some((previous, serial, targets, file)) = view
                    .update(cx, |view, _| {
                        (
                            view.devices.clone(),
                            view.preferences.serial.clone(),
                            view.targets.clone(),
                            view.file.is_some(),
                        )
                    })
                    .log_err()
                else {
                    break;
                };
                let result = cx
                    .background_spawn({
                        let executor = executor.clone();
                        let root = root.clone();
                        async move {
                            discover_devices(previous, serial, targets, file, &root, &executor)
                                .await
                        }
                    })
                    .await;
                if view
                    .update(cx, |view, cx| view.apply_discovery(result, cx))
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
        self.stream_task = None;
        self.capturing = false;
        self.stopped = false;
        self.connected = false;
        self.file = None;
        self.processes.clear();
        self.cursor = None;
        self.paused = None;
        self.selected_package = None;
        self.selected_process = None;
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
        if let Err(error) = self.ensure_trusted(cx) {
            self.error = Some(error.to_string());
            cx.notify();
            return;
        }
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
            let (sender, mut receiver) = mpsc::channel(4);
            let worker = cx.background_spawn(async move {
                capture(serial, buffers, cursor, root, executor, sender).await
            });
            while let Some(batch) = receiver.next().await {
                if view.update(cx, |view, cx| view.receive(batch, cx)).is_err() {
                    return;
                }
            }
            let result = worker.await;
            view.update(cx, |view, cx| {
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
        for mut entry in entries {
            self.cursor = Some(Arc::new(entry.clone()));
            if matches!(entry.buffer, 2 | 5 | 6)
                && let Ok(id) = entry.tag.parse::<u32>()
                && let Some(tag) = self
                    .devices
                    .iter()
                    .find(|device| Some(&device.device.serial) == self.preferences.serial.as_ref())
                    .and_then(|device| device.event_tags.get(&id))
            {
                entry.tag = tag.clone();
            }
            if let Some(process) = self.processes.iter().find(|process| {
                process.pid == entry.pid && entry.uid.is_none_or(|uid| uid == process.uid)
            }) {
                entry.process = process.name.clone();
                entry.package = process.package.clone();
            } else if let Some(process) = self
                .processes
                .iter()
                .find(|process| Some(process.uid) == entry.uid && !process.package.is_empty())
            {
                entry.package = process.package.clone();
            }
            if entry.package.is_empty()
                && let Some(names) = self
                    .devices
                    .iter()
                    .find(|device| Some(&device.device.serial) == self.preferences.serial.as_ref())
                    .and_then(|device| entry.uid.and_then(|uid| device.packages.get(&uid)))
                && names.len() == 1
                && let Some(package) = names.first()
            {
                entry.package = package.clone();
            }
            self.buffer.push(entry);
        }
        if self.paused.is_none() {
            self.rebuild(cx);
        }
        cx.notify();
    }

    fn update_filter(&mut self, cx: &mut Context<Self>) {
        self.preferences.filter = self.filter_input.read(cx).text(cx);
        match Query::parse(&self.preferences.filter, self.preferences.match_case) {
            Ok(query) => {
                self.query = query;
                self.filter_error = None;
                self.rebuild(cx);
            }
            Err(error) => self.filter_error = Some(format!("{error:#}")),
        }
        self.persist(cx);
        cx.notify();
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
        let now_millis = Utc::now().timestamp_millis();
        let context = FilterContext {
            now_millis,
            project_packages: &self.project_packages,
        };
        let entries: Box<dyn Iterator<Item = &Arc<Entry>> + '_> = match &self.paused {
            Some(entries) => Box::new(entries.iter()),
            None => Box::new(self.buffer.entries.iter()),
        };
        let visible = entries
            .filter(|entry| {
                entry.level >= self.minimum_level
                    && self.selected_package.as_ref().is_none_or(|package| {
                        if package == "mine" {
                            self.project_packages.contains(&entry.package)
                        } else {
                            package == &entry.package
                        }
                    })
                    && self
                        .selected_process
                        .as_ref()
                        .is_none_or(|process| process == &entry.process)
                    && self.query.matches(entry, &context)
            })
            .cloned()
            .collect::<Vec<_>>();
        let old_ids: Vec<_> = self.visible.iter().map(|entry| entry.id).collect();
        let new_ids: Vec<_> = visible.iter().map(|entry| entry.id).collect();
        if old_ids != new_ids {
            let removed = old_ids
                .iter()
                .position(|id| new_ids.first() == Some(id))
                .unwrap_or(old_ids.len());
            let retained = old_ids.get(removed..).unwrap_or_default();
            if new_ids.starts_with(retained) {
                self.list_state.splice(0..removed, 0);
                self.list_state.splice(
                    retained.len()..retained.len(),
                    new_ids.len() - retained.len(),
                );
            } else {
                self.list_state.reset(visible.len());
            }
            self.visible = visible;
            let retained_ids: HashSet<_> = new_ids.into_iter().collect();
            self.selected.retain(|id| retained_ids.contains(id));
            self.selection_anchor = self.selection_anchor.filter(|id| retained_ids.contains(id));
            self.update_search(cx);
        }
        cx.notify();
    }

    fn update_search(&mut self, cx: &mut Context<Self>) {
        let text = self.search_input.read(cx).text(cx);
        match logcat::Search::new(&text, self.search_case, self.search_regex) {
            Ok(search) => {
                self.search = search;
                self.search_error = None;
            }
            Err(error) => {
                self.search = None;
                self.search_error = Some(error.to_string());
            }
        }
        self.search_matches = self
            .search
            .as_ref()
            .map(|search| {
                self.visible
                    .iter()
                    .enumerate()
                    .filter_map(|(index, entry)| search.matches(&entry.line()).then_some(index))
                    .collect()
            })
            .unwrap_or_default();
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
                self.selected.clear();
                self.selected.insert(entry.id);
            }
            self.list_state
                .splice(0..self.visible.len(), self.visible.len());
        }
        cx.notify();
    }

    fn pause(&mut self, cx: &mut Context<Self>) {
        if self.paused.is_some() {
            self.paused = None;
            self.rebuild(cx);
        } else {
            self.paused = Some(self.buffer.entries.iter().cloned().collect());
        }
        cx.notify();
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.buffer.clear();
        if let Some(entries) = &mut self.paused {
            entries.clear();
        }
        self.selected.clear();
        self.selection_anchor = None;
        self.rebuild(cx);
    }

    fn copy(&self, messages_only: bool, cx: &mut Context<Self>) {
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
                    Ok::<_, anyhow::Error>(Some((path, entries)))
                })
                .await
            }
            .await;
            view.update(cx, |view, cx| {
                match result {
                    Ok(Some((path, entries))) => {
                        view.stream_task = None;
                        view.capturing = false;
                        view.file = Some(path);
                        view.paused = None;
                        view.buffer.clear();
                        view.selected.clear();
                        view.selected_package = None;
                        view.selected_process = None;
                        for entry in entries {
                            view.buffer.push(entry);
                        }
                        view.error = None;
                        view.status =
                            "Viewing saved logs. Select a device to return to live capture.".into();
                        view.rebuild(cx);
                    }
                    Ok(None) => {}
                    Err(error) => view.error = Some(format!("Import failed: {error:#}")),
                }
                view.control_task = None;
                cx.notify();
            })
            .log_err();
        }));
    }

    fn device_command(&mut self, args: Vec<String>, cx: &mut Context<Self>) {
        let result = self.ensure_trusted(cx).and_then(|_| {
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
            self.preferences.serial.clone().context("Select a device")
        });
        let serial = match result {
            Ok(serial) => serial,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        let root = self.root.clone();
        let executor = cx.background_executor().clone();
        self.control_task = Some(cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move { adb_output(&serial, args, &root, &executor).await })
                .await;
            view.update(cx, |view, cx| {
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
        let view = cx.weak_entity();
        PopoverMenu::new("logcat-device")
            .trigger(
                Button::new("logcat-device-trigger", label)
                    .tab_index(0isize)
                    .end_icon(Icon::new(IconName::ChevronDown).size(IconSize::XSmall)),
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

    fn app_picker(&self, cx: &Context<Self>) -> impl IntoElement {
        let label = match self.selected_package.as_deref() {
            Some("mine") => "Project applications",
            Some(package) => package,
            None => "All applications",
        };
        let view = cx.weak_entity();
        PopoverMenu::new("logcat-app")
            .trigger(
                Button::new("logcat-app-trigger", label.to_string())
                    .tab_index(0isize)
                    .end_icon(Icon::new(IconName::ChevronDown).size(IconSize::XSmall)),
            )
            .menu(move |window, cx| {
                let view = view.clone();
                let packages = view
                    .read_with(cx, |view, _| {
                        let mut packages = view
                            .processes
                            .iter()
                            .map(|process| process.package.clone())
                            .chain(
                                view.buffer
                                    .entries
                                    .iter()
                                    .map(|entry| entry.package.clone()),
                            )
                            .filter(|package| !package.is_empty())
                            .collect::<Vec<_>>();
                        packages.sort();
                        packages.dedup();
                        packages
                    })
                    .log_err()?;
                Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                    for (label, package) in [
                        ("All applications".to_string(), None),
                        ("Project applications".into(), Some("mine".into())),
                    ]
                    .into_iter()
                    .chain(
                        packages
                            .into_iter()
                            .map(|package| (package.clone(), Some(package))),
                    ) {
                        let view = view.clone();
                        menu = menu.entry(label, None, move |_, cx| {
                            view.update(cx, |view, cx| {
                                view.selected_package = package.clone();
                                view.selected_process = None;
                                view.rebuild(cx);
                            })
                            .log_err();
                        });
                    }
                    menu
                }))
            })
    }

    fn process_picker(&self, cx: &Context<Self>) -> impl IntoElement {
        let view = cx.weak_entity();
        PopoverMenu::new("logcat-process")
            .trigger(
                Button::new(
                    "logcat-process-trigger",
                    self.selected_process
                        .clone()
                        .unwrap_or_else(|| "All processes".into()),
                )
                .tab_index(0isize)
                .end_icon(Icon::new(IconName::ChevronDown).size(IconSize::XSmall)),
            )
            .menu(move |window, cx| {
                let view = view.clone();
                let processes = view
                    .read_with(cx, |view, _| {
                        view.processes
                            .iter()
                            .filter(|process| {
                                view.selected_package.as_ref().is_none_or(|package| {
                                    if package == "mine" {
                                        view.project_packages.contains(&process.package)
                                    } else {
                                        package == &process.package
                                    }
                                })
                            })
                            .cloned()
                            .collect::<Vec<_>>()
                    })
                    .log_err()?;
                Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                    let all = view.clone();
                    menu = menu.entry("All processes", None, move |_, cx| {
                        all.update(cx, |view, cx| {
                            view.selected_process = None;
                            view.rebuild(cx);
                        })
                        .log_err();
                    });
                    for process in processes {
                        let view = view.clone();
                        menu = menu.entry(
                            format!("{} ({})", process.name, process.pid),
                            None,
                            move |_, cx| {
                                view.update(cx, |view, cx| {
                                    view.selected_process = Some(process.name.clone());
                                    view.rebuild(cx);
                                })
                                .log_err();
                            },
                        );
                    }
                    menu
                }))
            })
    }

    fn level_picker(&self, cx: &Context<Self>) -> impl IntoElement {
        let view = cx.weak_entity();
        PopoverMenu::new("logcat-level")
            .trigger(
                Button::new(
                    "logcat-level-trigger",
                    format!("{} and above", self.minimum_level.letter()),
                )
                .tab_index(0isize)
                .end_icon(Icon::new(IconName::ChevronDown).size(IconSize::XSmall)),
            )
            .menu(move |window, cx| {
                let view = view.clone();
                Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                    for level in Level::ALL {
                        let view = view.clone();
                        menu = menu.entry(format!("{level:?} and above"), None, move |_, cx| {
                            view.update(cx, |view, cx| {
                                view.minimum_level = level;
                                view.rebuild(cx);
                            })
                            .log_err();
                        });
                    }
                    menu
                }))
            })
    }

    fn suggestions(&self, cx: &Context<Self>) -> impl IntoElement {
        let view = cx.weak_entity();
        PopoverMenu::new("logcat-suggestions")
            .trigger(Button::new("logcat-suggestions-trigger", "Suggestions").tab_index(0isize))
            .menu(move |window, cx| {
                let view = view.clone();
                let (prefix, candidates) = view
                    .read_with(cx, |view, _| {
                        let text = &view.preferences.filter;
                        let (prefix, token) = text
                            .rsplit_once(char::is_whitespace)
                            .map(|(prefix, token)| (format!("{prefix} "), token))
                            .unwrap_or_else(|| (String::new(), text.as_str()));
                        let mut candidates = [
                            "package:mine",
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
                            "message:",
                            "message~:",
                            "tag:",
                            "package:",
                            "process:",
                            "line:",
                            "name:",
                        ]
                        .map(String::from)
                        .to_vec();
                        let mut tags = HashSet::new();
                        for entry in view.buffer.entries.iter().rev() {
                            if tags.len() >= 100 {
                                break;
                            }
                            tags.insert(entry.tag.clone());
                        }
                        candidates
                            .extend(tags.into_iter().map(|tag| format!("tag:{}", quote(&tag))));
                        candidates.extend(
                            view.processes
                                .iter()
                                .map(|process| format!("process:{}", quote(&process.name))),
                        );
                        candidates.extend(
                            view.processes
                                .iter()
                                .filter(|process| !process.package.is_empty())
                                .map(|process| format!("package:{}", quote(&process.package))),
                        );
                        candidates.sort();
                        candidates.dedup();
                        let candidates = candidates
                            .into_iter()
                            .filter(|candidate| {
                                candidate.to_lowercase().starts_with(&token.to_lowercase())
                            })
                            .take(50)
                            .collect::<Vec<_>>();
                        (prefix, candidates)
                    })
                    .log_err()?;
                Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                    for candidate in candidates {
                        let view = view.clone();
                        let filter = format!("{prefix}{candidate}");
                        menu = menu.entry(candidate, None, move |window, cx| {
                            view.update(cx, |view, cx| {
                                set_input_text(&view.filter_input, &filter, window, cx);
                            })
                            .log_err();
                        });
                    }
                    menu
                }))
            })
    }

    fn options_menu(&self, cx: &Context<Self>) -> impl IntoElement {
        let view = cx.weak_entity();
        PopoverMenu::new("logcat-options")
            .trigger(Button::new("logcat-options-trigger", "Options").tab_index(0isize))
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
                    menu = menu.entry("Terminate selected application", None, move |_, cx| {
                        terminate
                            .update(cx, |view, cx| {
                                if let Some(package) = view
                                    .selected_package
                                    .clone()
                                    .filter(|package| package != "mine")
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
                                        Some("Select a specific application to terminate".into());
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
                                let root = view.root.clone();
                                let serial = view.preferences.serial.clone();
                                let targets = view.targets.clone();
                                let project = view.project.clone();
                                window.defer(cx, move |window, cx| {
                                    workspace
                                        .update(cx, |workspace, cx| {
                                            let new_view = cx.new(|cx| {
                                                LogcatView::new(
                                                    workspace.weak_handle(),
                                                    project,
                                                    root,
                                                    serial,
                                                    targets,
                                                    window,
                                                    cx,
                                                )
                                            });
                                            new_view.update(cx, |view, cx| view.watch_devices(cx));
                                            let pane = workspace.split_pane(
                                                workspace.active_pane().clone(),
                                                SplitDirection::Right,
                                                window,
                                                cx,
                                            );
                                            workspace.add_item(
                                                pane,
                                                Box::new(new_view),
                                                None,
                                                true,
                                                true,
                                                window,
                                                cx,
                                            );
                                        })
                                        .log_err();
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
        let color = match entry.level {
            Level::Error | Level::Assert => cx.theme().status().error,
            Level::Warn => cx.theme().status().warning,
            Level::Info => cx.theme().status().success,
            Level::Verbose => cx.theme().colors().text_muted,
            Level::Debug => cx.theme().colors().text,
        };
        let message_text = if self.preferences.fold_stacktraces
            && entry.is_stacktrace()
            && !self.selected.contains(&entry.id)
        {
            format!(
                "{} … {} lines (select to expand)",
                entry.message.lines().next().unwrap_or_default(),
                entry.message.lines().count()
            )
        } else {
            entry.message.clone()
        };
        let highlights = self
            .search
            .as_ref()
            .map(|search| search.ranges(&message_text))
            .unwrap_or_default()
            .into_iter()
            .map(|range| {
                (
                    range,
                    gpui::HighlightStyle {
                        background_color: Some(cx.theme().colors().element_selected),
                        ..Default::default()
                    },
                )
            })
            .collect::<Vec<_>>();
        let selected = self.selected.contains(&entry.id);
        let search_match = self.search_matches.binary_search(&index).is_ok();
        let compact = self.preferences.compact;
        let id = entry.id;
        let view = cx.weak_entity();
        v_flex()
            .id(("logcat-entry", id))
            .w_full()
            .px_2()
            .py_1()
            .text_sm()
            .font_family(ThemeSettings::get_global(cx).buffer_font.family.clone())
            .when(selected, |row| row.bg(cx.theme().colors().element_selected))
            .when(search_match && !selected, |row| {
                row.bg(cx.theme().colors().element_hover)
            })
            .on_click(
                cx.listener(move |view, event: &gpui::ClickEvent, window, cx| {
                    window.focus(&view.focus_handle, cx);
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
                    view.list_state
                        .splice(0..view.visible.len(), view.visible.len());
                    cx.notify();
                }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .when(!compact, |row| {
                        row.child(Label::new(entry.timestamp()).color(Color::Muted))
                    })
                    .child(div().text_color(color).child(entry.level.letter()))
                    .when(!compact, |row| {
                        row.child(
                            Label::new(format!("{}:{}", entry.pid, entry.tid)).color(Color::Muted),
                        )
                        .when_some(entry.uid, |row, uid| {
                            row.child(Label::new(format!("uid:{uid}")).color(Color::Muted))
                        })
                        .child(
                            Label::new(entry.package.clone())
                                .buffer_font(cx)
                                .color(Color::Muted),
                        )
                        .child(
                            Label::new(entry.process.clone())
                                .buffer_font(cx)
                                .color(Color::Muted),
                        )
                    })
                    .child(Label::new(entry.tag.clone()).color(Color::Custom(color)))
                    .child(
                        PopoverMenu::new(("logcat-row-menu", id))
                            .trigger(Button::new(("logcat-row-actions", id), "…").tab_index(0isize))
                            .menu(move |window, cx| {
                                let view = view.clone();
                                let entry = entry.clone();
                                Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                                    let copy = view.clone();
                                    let message = entry.message.clone();
                                    menu = menu.entry("Copy message", None, move |_, cx| {
                                        copy.update(cx, |_, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                message.clone(),
                                            ))
                                        })
                                        .log_err();
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
                                                let filter = if term.starts_with('-')
                                                    && !view.preferences.filter.is_empty()
                                                {
                                                    format!(
                                                        "({}) & {term}",
                                                        view.preferences.filter
                                                    )
                                                } else {
                                                    term.clone()
                                                };
                                                set_input_text(
                                                    &view.filter_input,
                                                    &filter,
                                                    window,
                                                    cx,
                                                );
                                            })
                                            .log_err();
                                        });
                                    }
                                    menu
                                }))
                            }),
                    ),
            )
            .child(
                div()
                    .id(("logcat-message", id))
                    .text_color(color)
                    .when(!self.preferences.wrap, |text| {
                        text.whitespace_nowrap().overflow_x_scroll()
                    })
                    .child(gpui::StyledText::new(message_text).with_highlights(highlights)),
            )
            .when(selected && self.selected.len() == 1, |row| {
                row.children(self.source_buttons(index, cx))
            })
            .into_any_element()
    }

    fn source_buttons(&self, index: usize, cx: &Context<Self>) -> Vec<AnyElement> {
        let Some(entry) = self.visible.get(index) else {
            return Vec::new();
        };
        let mut locations = Vec::new();
        for line in entry.message.lines() {
            let Some((_, location)) = line.rsplit_once('(') else {
                continue;
            };
            let Some((file, line)) = location.trim_end_matches(')').rsplit_once(':') else {
                continue;
            };
            let Ok(line) = line.parse::<u32>() else {
                continue;
            };
            if !file.ends_with(".kt") && !file.ends_with(".java") {
                continue;
            }
            for worktree in self.project.read(cx).worktrees(cx) {
                let worktree = worktree.read(cx);
                for source in worktree
                    .snapshot()
                    .entries(false, 0)
                    .filter(|source| source.path.file_name() == Some(file))
                {
                    let path = project::ProjectPath {
                        worktree_id: worktree.id(),
                        path: source.path.clone(),
                    };
                    if !locations.contains(&(path.clone(), line)) {
                        locations.push((path, line));
                    }
                }
            }
        }
        locations
            .into_iter()
            .take(20)
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
    fn tab_content_text(&self, _: usize, _: &App) -> SharedString {
        "Logcat".into()
    }
    fn tab_icon(&self, _: &Window, _: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Terminal))
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
        let paused = self.paused.is_some();
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
                window.focus(&view.filter_input.focus_handle(cx), cx)
            }))
            .on_action(cx.listener(|view, _: &Find, window, cx| {
                window.focus(&view.search_input.focus_handle(cx), cx)
            }))
            .on_action(cx.listener(|view, _: &FindNext, _, cx| view.find(false, cx)))
            .on_action(cx.listener(|view, _: &FindPrevious, _, cx| view.find(true, cx)))
            .on_action(cx.listener(|view, _: &Copy, _, cx| view.copy(false, cx)))
            .on_action(cx.listener(|view, _: &SelectAll, _, cx| {
                view.selected = view.visible.iter().map(|entry| entry.id).collect();
                view.list_state
                    .splice(0..view.visible.len(), view.visible.len());
                cx.notify();
            }))
            .child(
                h_flex()
                    .p_2()
                    .gap_2()
                    .flex_wrap()
                    .child(self.device_picker(cx))
                    .child(self.app_picker(cx))
                    .child(self.process_picker(cx))
                    .child(self.level_picker(cx)),
            )
            .child(
                h_flex()
                    .px_2()
                    .gap_2()
                    .child(div().flex_1().child(self.filter_input.clone()))
                    .child(
                        Button::new("logcat-case", "Aa")
                            .toggle_state(self.preferences.match_case)
                            .tab_index(0isize)
                            .tooltip(Tooltip::text("Match case in filters"))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.preferences.match_case = !view.preferences.match_case;
                                view.update_filter(cx);
                            })),
                    )
                    .child(self.suggestions(cx))
                    .child(self.options_menu(cx)),
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
            .child(
                h_flex()
                    .p_2()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("logcat-pause", if paused { "Resume" } else { "Pause" })
                            .tab_index(0isize)
                            .on_click(cx.listener(|view, _, _, cx| view.pause(cx))),
                    )
                    .child(
                        Button::new("logcat-clear", "Clear view")
                            .tab_index(0isize)
                            .on_click(cx.listener(|view, _, _, cx| view.clear(cx))),
                    )
                    .child(
                        Button::new("logcat-restart", "Restart")
                            .disabled(self.file.is_some() || !self.connected)
                            .tab_index(0isize)
                            .on_click(cx.listener(|view, _, _, cx| view.start_capture(true, cx))),
                    )
                    .child(
                        Button::new("logcat-follow", "Scroll to end")
                            .toggle_state(self.list_state.is_following_tail())
                            .tab_index(0isize)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.list_state.set_follow_mode(FollowMode::Tail);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("logcat-wrap", "Wrap")
                            .toggle_state(self.preferences.wrap)
                            .tab_index(0isize)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.preferences.wrap = !view.preferences.wrap;
                                view.list_state.reset(view.visible.len());
                                view.persist(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("logcat-format", "Compact")
                            .toggle_state(self.preferences.compact)
                            .tab_index(0isize)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.preferences.compact = !view.preferences.compact;
                                view.list_state.reset(view.visible.len());
                                view.persist(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("logcat-copy", "Copy selected")
                            .disabled(self.selected.is_empty())
                            .tab_index(0isize)
                            .on_click(cx.listener(|view, _, _, cx| view.copy(false, cx))),
                    )
                    .child(
                        Button::new("logcat-export", "Export…")
                            .disabled(self.control_task.is_some())
                            .tab_index(0isize)
                            .on_click(cx.listener(|view, _, window, cx| view.export(window, cx))),
                    )
                    .child(
                        Button::new("logcat-import", "Open logs…")
                            .disabled(self.control_task.is_some())
                            .tab_index(0isize)
                            .on_click(cx.listener(|view, _, window, cx| view.import(window, cx))),
                    ),
            )
            .child(
                h_flex()
                    .px_2()
                    .pb_2()
                    .gap_2()
                    .child(div().flex_1().child(self.search_input.clone()))
                    .child(
                        Button::new("logcat-search-case", "Aa")
                            .toggle_state(self.search_case)
                            .tab_index(0isize)
                            .tooltip(Tooltip::text("Match case in search"))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.search_case = !view.search_case;
                                view.update_search(cx);
                            })),
                    )
                    .child(
                        Button::new("logcat-search-regex", ".*")
                            .toggle_state(self.search_regex)
                            .tab_index(0isize)
                            .tooltip(Tooltip::text("Use regular expressions in search"))
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
                        Button::new("logcat-previous", "Previous")
                            .disabled(self.search_matches.is_empty())
                            .tab_index(0isize)
                            .on_click(cx.listener(|view, _, _, cx| view.find(true, cx))),
                    )
                    .child(
                        Button::new("logcat-next", "Next")
                            .disabled(self.search_matches.is_empty())
                            .tab_index(0isize)
                            .on_click(cx.listener(|view, _, _, cx| view.find(false, cx))),
                    ),
            )
            .when_some(self.search_error.clone(), |view, error| {
                view.child(
                    div()
                        .px_2()
                        .text_sm()
                        .text_color(cx.theme().status().error)
                        .child(error),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(
                        list(self.list_state.clone(), cx.processor(Self::render_entry)).size_full(),
                    )
                    .vertical_scrollbar_for(&self.list_state, window, cx)
                    .when(self.visible.is_empty(), |view| {
                        view.child(
                            Label::new(if self.buffer.entries.is_empty() {
                                "Waiting for logs…"
                            } else {
                                "No messages match the filters"
                            })
                            .color(Color::Muted),
                        )
                    }),
            )
            .child(
                h_flex()
                    .p_2()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        Label::new(format!(
                            "{} · {}{} messages · {} MiB · {} evicted · {}",
                            self.status,
                            self.visible.len(),
                            if paused { " frozen" } else { "" },
                            self.preferences.capacity / (1024 * 1024),
                            self.buffer.dropped,
                            self.buffers
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
                            .pb_2()
                            .text_sm()
                            .text_color(cx.theme().status().error)
                            .child(error),
                    )
                },
            )
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
) -> Result<Discovery> {
    let output = tool_output(
        adb_path()?,
        vec!["devices".into(), "-l".into()],
        root,
        executor,
        Duration::from_secs(10),
    )
    .await?;
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
                    batch.push(entry);
                }
            }
            Either::Right(_) => {}
        }
        if !batch.is_empty()
            && (batch.len() >= 1024 || last_flush.elapsed() >= Duration::from_millis(50))
        {
            sender
                .send(std::mem::take(&mut batch))
                .await
                .context("Logcat view closed")?;
            last_flush = Instant::now();
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
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use project::FakeFs;
    use workspace::AppState;

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
