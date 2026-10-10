use crate::tabbed_toolbar::TabbedToolbar;
use anyhow::{Context as _, Result, bail, ensure};
use futures::{
    AsyncRead, AsyncReadExt as _, FutureExt as _, SinkExt as _, StreamExt as _,
    channel::{mpsc, oneshot},
    future::{Either, select},
};
use gpui::{
    App, BackgroundExecutor, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, ListHorizontalSizingBehavior, ScrollStrategy, Task, UniformListScrollHandle,
    WeakEntity, uniform_list,
};
use std::process::{Command, Stdio};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
use ui::{Tooltip, prelude::*};
use util::ResultExt as _;
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

pub use zed_actions::android::ToggleBuild;

const MAX_LINES: usize = 10_000;
const MAX_LOG_BYTES: usize = 4 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 16 * 1024;
const MAX_TASKS: usize = 2_000;
const MAX_TASK_LABEL_BYTES: usize = 1024;
const MAX_MODEL_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BuildTab {
    Sync,
    Output,
}

impl BuildTab {
    fn label(self) -> &'static str {
        match self {
            Self::Sync => "Sync",
            Self::Output => "Build Output",
        }
    }
    fn index(self) -> usize {
        match self {
            Self::Sync => 0,
            Self::Output => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BuildStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl BuildStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Running => "Running…",
            Self::Succeeded => "Successful",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }
    fn icon(self) -> IconName {
        match self {
            Self::Running => IconName::RotateCw,
            Self::Succeeded => IconName::Check,
            Self::Failed => IconName::Warning,
            Self::Cancelled => IconName::Stop,
        }
    }
    fn color(self) -> Color {
        match self {
            Self::Running => Color::Accent,
            Self::Succeeded => Color::Success,
            Self::Failed => Color::Error,
            Self::Cancelled => Color::Muted,
        }
    }
}

pub(crate) enum BuildEvent {
    Stop(BuildTab),
    Rerun(BuildTab),
}

struct LogLine {
    text: SharedString,
    stderr: bool,
}
struct BuildTask {
    name: SharedString,
    label: SharedString,
    line: usize,
    status: BuildStatus,
    phase_line: Option<usize>,
}

struct BuildPhase {
    label: SharedString,
    line: usize,
    status: BuildStatus,
    started: Instant,
    elapsed: Option<Duration>,
}

struct BuildMessage {
    label: SharedString,
    line: usize,
    task_line: Option<usize>,
    phase_line: Option<usize>,
    error: bool,
}

#[derive(Clone, Copy)]
enum TreeRow {
    Root,
    Phase(usize),
    Downloads,
    Task(usize),
    Message(usize),
}

struct BuildSession {
    id: u64,
    label: SharedString,
    status: BuildStatus,
    waiting_for_emulator: bool,
    started: Instant,
    elapsed: Option<Duration>,
    previous_elapsed: Duration,
    lines: VecDeque<LogLine>,
    bytes: usize,
    discarded: usize,
    tasks: VecDeque<BuildTask>,
    task_indexes: std::collections::HashMap<(Option<usize>, SharedString), usize>,
    task_offset: usize,
    task_name_bytes: usize,
    current_task: Option<usize>,
    phases: VecDeque<BuildPhase>,
    messages: VecDeque<BuildMessage>,
    download_line: Option<usize>,
    show_successful: bool,
    selected_line: Option<usize>,
    widths: VecDeque<(usize, usize)>,
    follow: bool,
    expanded: bool,
    scroll: UniformListScrollHandle,
}

impl BuildSession {
    fn new(id: u64, label: String) -> Self {
        Self {
            id,
            label: label.into(),
            status: BuildStatus::Running,
            waiting_for_emulator: false,
            started: Instant::now(),
            elapsed: None,
            previous_elapsed: Duration::ZERO,
            lines: VecDeque::new(),
            bytes: 0,
            discarded: 0,
            tasks: VecDeque::new(),
            task_indexes: std::collections::HashMap::new(),
            task_offset: 0,
            task_name_bytes: 0,
            current_task: None,
            phases: VecDeque::new(),
            messages: VecDeque::new(),
            download_line: None,
            show_successful: false,
            selected_line: None,
            widths: VecDeque::new(),
            follow: true,
            expanded: true,
            scroll: UniformListScrollHandle::new(),
        }
    }

    fn activity_status(&self) -> BuildStatus {
        if self.waiting_for_emulator {
            BuildStatus::Running
        } else {
            self.status
        }
    }

    fn append(&mut self, line: OutputLine) {
        for text in line.text.split('\n') {
            let mut end = text.len().min(MAX_LINE_BYTES);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            let text = &text[..end];
            let line_index = self.discarded + self.lines.len();
            let trimmed = text.trim();
            if let Some(task) = trimmed.strip_prefix("> Task ") {
                let name: SharedString = task
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_owned()
                    .into();
                let status = if task.ends_with(" FAILED") {
                    BuildStatus::Failed
                } else {
                    BuildStatus::Succeeded
                };
                // Gradle can emit a plain-console task header at completion or when
                // output is flushed. It does not provide a task start time.
                let phase_line = self.phases.back().map(|phase| phase.line);
                let key = (phase_line, name.clone());
                if let Some(index) = self.task_indexes.get(&key).copied()
                    && let Some(previous) = self.tasks.get_mut(index - self.task_offset)
                {
                    if previous.status != BuildStatus::Failed {
                        previous.status = status;
                    }
                    self.current_task = Some(index);
                } else {
                    let index = self.task_offset + self.tasks.len();
                    self.task_indexes.insert(key, index);
                    self.current_task = Some(index);
                    self.task_name_bytes += name.len();
                    self.tasks.push_back(BuildTask {
                        label: if name.len() <= MAX_TASK_LABEL_BYTES {
                            name.clone()
                        } else {
                            bounded_label(&name)
                        },
                        name,
                        line: line_index,
                        status,
                        phase_line,
                    });
                }
                while self.tasks.len() > MAX_TASKS
                    || self.task_name_bytes > MAX_TASKS * MAX_TASK_LABEL_BYTES
                {
                    if let Some(task) = self.tasks.pop_front() {
                        self.task_name_bytes -= task.name.len();
                        self.task_indexes.remove(&(task.phase_line, task.name));
                        self.task_offset += 1;
                    }
                }
            }
            if trimmed.starts_with("Downloading ") || trimmed.starts_with("Download ") {
                self.download_line.get_or_insert(line_index);
            }
            let error = trimmed.starts_with("e: ")
                || trimmed.starts_with("error:")
                || trimmed.contains(": error:")
                || trimmed.starts_with("Execution failed for task ")
                || trimmed.starts_with("FAILURE:");
            let warning = trimmed.starts_with("w: ")
                || trimmed.starts_with("warning:")
                || trimmed.contains(": warning:");
            if error || warning {
                let phase_line = self.phases.back().map(|phase| phase.line);
                let task_index = if let Some(task_name) = trimmed
                    .strip_prefix("Execution failed for task '")
                    .and_then(|message| message.split_once('\'').map(|(task, _)| task))
                {
                    self.task_indexes
                        .get(&(phase_line, task_name.to_owned().into()))
                        .copied()
                } else {
                    self.current_task
                };
                let task_line = task_index
                    .and_then(|index| index.checked_sub(self.task_offset))
                    .and_then(|index| self.tasks.get_mut(index))
                    .filter(|task| {
                        task.phase_line == phase_line && !trimmed.starts_with("FAILURE:")
                    })
                    .map(|task| task.line);
                self.messages.push_back(BuildMessage {
                    label: bounded_label(trimmed),
                    line: line_index,
                    task_line,
                    phase_line,
                    error,
                });
                if self.messages.len() > MAX_TASKS {
                    self.messages.pop_front();
                }
            }
            let width = text.chars().count();
            while self
                .widths
                .back()
                .is_some_and(|(_, previous)| *previous <= width)
            {
                self.widths.pop_back();
            }
            self.widths
                .push_back((self.discarded + self.lines.len(), width));
            self.bytes += text.len();
            self.lines.push_back(LogLine {
                text: text.to_owned().into(),
                stderr: line.stderr,
            });
            while self.lines.len() > MAX_LINES || self.bytes > MAX_LOG_BYTES {
                if let Some(line) = self.lines.pop_front() {
                    self.bytes -= line.text.len();
                    self.discarded += 1;
                }
            }
            while self
                .widths
                .front()
                .is_some_and(|(index, _)| *index < self.discarded)
            {
                self.widths.pop_front();
            }
        }
    }

    fn tree_rows(&self) -> Vec<TreeRow> {
        let mut rows = vec![TreeRow::Root];
        if !self.expanded {
            return rows;
        }
        if self.download_line.is_some() {
            rows.push(TreeRow::Downloads);
        }
        let task_lines: std::collections::HashSet<_> =
            self.tasks.iter().map(|task| task.line).collect();
        let mut messages = std::collections::HashMap::<usize, Vec<usize>>::new();
        let mut phase_messages = std::collections::HashMap::<usize, Vec<TreeRow>>::new();
        for (index, message) in self.messages.iter().enumerate() {
            if let Some(task_line) = message.task_line.filter(|line| task_lines.contains(line)) {
                messages.entry(task_line).or_default().push(index);
            } else if let Some(phase_line) = message.phase_line {
                phase_messages
                    .entry(phase_line)
                    .or_default()
                    .push(TreeRow::Message(index));
            }
        }
        let mut phase_tasks = std::collections::HashMap::<usize, Vec<TreeRow>>::new();
        let phase_lines: std::collections::HashSet<_> =
            self.phases.iter().map(|phase| phase.line).collect();
        for (index, task) in self.tasks.iter().enumerate() {
            if self.show_successful
                || task.status == BuildStatus::Failed
                || messages.contains_key(&task.line)
                || (self.status == BuildStatus::Running
                    && Some(index + self.task_offset) == self.current_task
                    && task.phase_line == self.phases.back().map(|phase| phase.line))
            {
                let children = if let Some(phase_line) = task.phase_line
                    && phase_lines.contains(&phase_line)
                {
                    phase_tasks.entry(phase_line).or_default()
                } else {
                    &mut rows
                };
                children.push(TreeRow::Task(index));
                if let Some(diagnostics) = messages.remove(&task.line) {
                    children.extend(diagnostics.into_iter().map(TreeRow::Message));
                }
            }
        }
        for (index, phase) in self.phases.iter().enumerate() {
            let mut children = phase_tasks.remove(&phase.line).unwrap_or_default();
            children.extend(phase_messages.remove(&phase.line).unwrap_or_default());
            if self.show_successful
                || phase.status != BuildStatus::Succeeded
                || !children.is_empty()
            {
                rows.push(TreeRow::Phase(index));
                rows.extend(children);
            }
        }
        rows.extend(
            self.messages
                .iter()
                .enumerate()
                .filter_map(|(index, message)| {
                    ((message.task_line.is_none()
                        || message
                            .task_line
                            .is_some_and(|line| !task_lines.contains(&line)))
                        && (message.phase_line.is_none()
                            || message
                                .phase_line
                                .is_some_and(|line| !phase_lines.contains(&line))))
                    .then_some(TreeRow::Message(index))
                }),
        );
        rows
    }

    fn clear(&mut self) {
        self.lines.clear();
        self.tasks.clear();
        self.task_indexes.clear();
        self.task_offset = 0;
        self.task_name_bytes = 0;
        self.current_task = None;
        self.phases
            .retain(|phase| phase.status == BuildStatus::Running);
        for phase in &mut self.phases {
            phase.line = 0;
        }
        self.messages.clear();
        self.download_line = None;
        self.selected_line = None;
        self.widths.clear();
        self.bytes = 0;
        self.discarded = 0;
        self.scroll = UniformListScrollHandle::new();
    }
}

fn bounded_label(text: &str) -> SharedString {
    let mut end = text.len().min(MAX_TASK_LABEL_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned().into()
}

fn elapsed_label(elapsed: Duration) -> String {
    if elapsed.as_secs() == 0 {
        format!("{} ms", elapsed.as_millis())
    } else {
        format!("{} sec", elapsed.as_secs())
    }
}

pub struct BuildPanel {
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    sessions: [Option<BuildSession>; 2],
    selected: BuildTab,
    toolbar: Entity<TabbedToolbar>,
    toolbar_statuses: [Option<BuildStatus>; 2],
    next_id: u64,
    clock_task: Option<Task<()>>,
    notification_task: Option<Task<()>>,
}

impl BuildPanel {
    pub(crate) fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        let panel = cx.entity().downgrade();
        let title = cx.new(|_| BuildToolbarTitle);
        let toolbar = cx.new(|cx| {
            let mut toolbar = TabbedToolbar::new(title, cx);
            for tab in [BuildTab::Sync, BuildTab::Output] {
                let panel = panel.clone();
                toolbar.add_tab(
                    tab.label(),
                    cx,
                    move |_, cx| {
                        if let Some(panel) = panel.upgrade() {
                            panel.update(cx, |panel, cx| {
                                panel.selected = tab;
                                cx.notify();
                            });
                        }
                    },
                    None,
                );
            }
            toolbar.add_action(
                Icon::new(IconName::Dash),
                "Hide Build window",
                cx,
                move |_, cx| {
                    if let Some(panel) = panel.upgrade() {
                        panel.update(cx, |_, cx| cx.emit(PanelEvent::Close));
                    }
                },
            );
            toolbar.set_active_tab(BuildTab::Sync.index(), cx).log_err();
            toolbar
        });
        Self {
            workspace,
            focus_handle: cx.focus_handle(),
            sessions: [None, None],
            selected: BuildTab::Sync,
            toolbar,
            toolbar_statuses: [None, None],
            next_id: 0,
            clock_task: None,
            notification_task: None,
        }
    }

    pub(crate) fn session_id(&self, tab: BuildTab) -> Option<u64> {
        self.sessions[tab.index()]
            .as_ref()
            .map(|session| session.id)
    }

    pub(crate) fn begin(
        &mut self,
        tab: BuildTab,
        label: String,
        retain_output: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (u64, mpsc::Sender<OutputLine>, Task<()>) {
        self.next_id += 1;
        let id = self.next_id;
        let previous = self.sessions[tab.index()].take().filter(|_| retain_output);
        let mut session = BuildSession::new(id, label);
        let phase_label = session.label.clone();
        if let Some(previous) = previous {
            session.label = previous.label;
            session.previous_elapsed = previous.previous_elapsed
                + previous
                    .elapsed
                    .unwrap_or_else(|| previous.started.elapsed());
            session.lines = previous.lines;
            session.bytes = previous.bytes;
            session.discarded = previous.discarded;
            session.tasks = previous.tasks;
            session.task_indexes = previous.task_indexes;
            session.task_offset = previous.task_offset;
            session.task_name_bytes = previous.task_name_bytes;
            session.current_task = previous.current_task;
            session.phases = previous.phases;
            session.messages = previous.messages;
            session.download_line = previous.download_line;
            session.show_successful = previous.show_successful;
            session.widths = previous.widths;
        }
        if tab == BuildTab::Sync {
            session.phases.push_back(BuildPhase {
                label: if phase_label.starts_with("Sync ") {
                    "Load Android project model".into()
                } else {
                    phase_label.clone()
                },
                line: session.discarded + session.lines.len(),
                status: BuildStatus::Running,
                started: Instant::now(),
                elapsed: None,
            });
            if session.phases.len() > MAX_TASKS {
                session.phases.pop_front();
            }
        }
        session.append(OutputLine {
            text: phase_label.to_string(),
            stderr: false,
        });
        self.sessions[tab.index()] = Some(session);
        self.select(tab, cx);
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.reveal_panel::<BuildPanel>(window, cx)
                })
                .log_err();
        });
        let (sender, receiver) = mpsc::channel(128);
        let logs = cx.spawn(async move |panel, cx| {
            let mut batches = receiver.ready_chunks(128);
            while let Some(lines) = batches.next().await {
                if panel
                    .update(cx, |panel, cx| {
                        if let Some(session) = &mut panel.sessions[tab.index()]
                            && session.id == id
                        {
                            for line in lines {
                                session.append(line);
                            }
                            panel.notify_throttled(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
                let mut yielded = false;
                futures::future::poll_fn(|task_context| {
                    if std::mem::replace(&mut yielded, true) {
                        std::task::Poll::Ready(())
                    } else {
                        task_context.waker().wake_by_ref();
                        std::task::Poll::Pending
                    }
                })
                .await;
            }
            panel.update(cx, |_, cx| cx.notify()).log_err();
        });
        self.clock_task = Some(cx.spawn(async move |panel, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let running = panel.update(cx, |panel, cx| {
                    cx.notify();
                    panel
                        .sessions
                        .iter()
                        .flatten()
                        .any(|session| session.status == BuildStatus::Running)
                });
                if !running.unwrap_or(false) {
                    break;
                }
            }
        }));
        cx.notify();
        (id, sender, logs)
    }

    fn notify_throttled(&mut self, cx: &mut Context<Self>) {
        if self.notification_task.is_none() {
            self.notification_task = Some(cx.spawn(async move |panel, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(33))
                    .await;
                panel
                    .update(cx, |panel, cx| {
                        panel.notification_task = None;
                        cx.notify();
                    })
                    .log_err();
            }));
        }
    }

    pub(crate) fn finish(
        &mut self,
        tab: BuildTab,
        id: u64,
        status: BuildStatus,
        message: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = &mut self.sessions[tab.index()]
            && session.id == id
        {
            session.status = status;
            session.waiting_for_emulator = false;
            session.elapsed = Some(session.started.elapsed());
            if let Some(phase) = session.phases.back_mut() {
                phase.status = status;
                phase.elapsed = Some(phase.started.elapsed());
            }
            if session
                .phases
                .iter()
                .any(|phase| phase.status == BuildStatus::Failed)
            {
                session.status = BuildStatus::Failed;
            } else if session
                .phases
                .iter()
                .any(|phase| phase.status == BuildStatus::Cancelled)
            {
                session.status = BuildStatus::Cancelled;
            }
            if status == BuildStatus::Failed {
                session.messages.push_back(BuildMessage {
                    label: bounded_label(&message),
                    line: session.discarded + session.lines.len(),
                    task_line: None,
                    phase_line: session.phases.back().map(|phase| phase.line),
                    error: true,
                });
                if session.messages.len() > MAX_TASKS {
                    session.messages.pop_front();
                }
            }
            session.append(OutputLine {
                text: message,
                stderr: status == BuildStatus::Failed,
            });
            cx.notify();
        }
    }

    pub(crate) fn is_waiting_for_emulator(&self, tab: BuildTab, id: u64) -> bool {
        self.sessions[tab.index()]
            .as_ref()
            .is_some_and(|session| session.id == id && session.waiting_for_emulator)
    }

    pub(crate) fn set_waiting_for_emulator(
        &mut self,
        tab: BuildTab,
        id: u64,
        waiting: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = &mut self.sessions[tab.index()]
            && session.id == id
        {
            session.waiting_for_emulator = waiting;
            cx.notify();
        }
    }

    pub(crate) fn select(&mut self, tab: BuildTab, cx: &mut Context<Self>) {
        self.selected = tab;
        self.toolbar.update(cx, |toolbar, cx| {
            toolbar.set_active_tab(tab.index(), cx).log_err()
        });
        cx.notify();
    }

    fn render_session(&self, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.selected;
        let Some(session) = &self.sessions[tab.index()] else {
            return v_flex()
                .debug_selector(|| "build-empty".into())
                .size_full()
                .items_center()
                .justify_center()
                .child(Label::new("Nothing to show").color(Color::Muted))
                .into_any_element();
        };
        let running = session.activity_status() == BuildStatus::Running;
        let toolbar = v_flex()
            .debug_selector(|| "build-left-actions".into())
            .h_full()
            .justify_start()
            .flex_shrink_0()
            .p_1()
            .gap_1()
            .border_r_1()
            .border_color(cx.theme().colors().border_variant)
            .child(
                IconButton::new("rerun-build", IconName::RotateCcw)
                    .tab_index(0isize)
                    .aria_label("Rerun")
                    .disabled(running)
                    .tooltip(Tooltip::text("Rerun"))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(BuildEvent::Rerun(tab)))),
            )
            .child(
                IconButton::new("stop-build", IconName::Stop)
                    .tab_index(0isize)
                    .aria_label("Stop")
                    .disabled(!running)
                    .tooltip(Tooltip::text("Stop"))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(BuildEvent::Stop(tab)))),
            )
            .child(
                IconButton::new("show-successful-build-steps", IconName::Eye)
                    .tab_index(0isize)
                    .aria_label("Show successful steps")
                    .toggle_state(session.show_successful)
                    .tooltip(Tooltip::text("Show successful steps"))
                    .on_click(cx.listener(move |panel, _, _, cx| {
                        if let Some(session) = &mut panel.sessions[tab.index()] {
                            session.show_successful = !session.show_successful;
                        }
                        cx.notify();
                    })),
            );
        let rows = session.tree_rows();
        let tree = uniform_list("build-task-tree", rows.len(), {
            let panel = cx.entity();
            move |range, _, cx| {
                let entity = panel.clone();
                let panel = panel.read(cx);
                let Some(session) = &panel.sessions[tab.index()] else {
                    return Vec::new();
                };
                range
                    .filter_map(|index| {
                        let row = *rows.get(index)?;
                        let (label, icon, color, line, depth, duration) = match row {
                            TreeRow::Root => {
                                let state = if session.waiting_for_emulator {
                                    "Waiting for emulator…"
                                } else if session.status == BuildStatus::Running {
                                    match tab {
                                        BuildTab::Sync => "Syncing…",
                                        BuildTab::Output => "Building…",
                                    }
                                } else {
                                    session.status.label()
                                };
                                let root_label = session
                                    .label
                                    .strip_prefix("Sync ")
                                    .unwrap_or(&session.label);
                                let label = if !session.waiting_for_emulator
                                    && session.status == BuildStatus::Running
                                    && let Some(task) = session
                                        .current_task
                                        .and_then(|index| index.checked_sub(session.task_offset))
                                        .and_then(|index| session.tasks.get(index))
                                        .filter(|task| {
                                            task.phase_line
                                                == session.phases.back().map(|phase| phase.line)
                                        }) {
                                    format!("{}: {} {}", root_label, state, task.label)
                                } else {
                                    format!("{}: {}", root_label, state)
                                };
                                (
                                    SharedString::from(label),
                                    session.activity_status().icon(),
                                    session.activity_status().color(),
                                    None,
                                    0,
                                    Some(
                                        session.previous_elapsed
                                            + session
                                                .elapsed
                                                .unwrap_or_else(|| session.started.elapsed()),
                                    ),
                                )
                            }
                            TreeRow::Phase(index) => {
                                let phase = session.phases.get(index)?;
                                (
                                    phase.label.clone(),
                                    phase.status.icon(),
                                    phase.status.color(),
                                    Some(phase.line),
                                    1,
                                    Some(phase.elapsed.unwrap_or_else(|| phase.started.elapsed())),
                                )
                            }
                            TreeRow::Downloads => (
                                "Download info".into(),
                                IconName::Download,
                                Color::Muted,
                                session.download_line,
                                1,
                                None,
                            ),
                            TreeRow::Task(index) => {
                                let task = session.tasks.get(index)?;
                                (
                                    task.label.clone(),
                                    if task.status == BuildStatus::Failed {
                                        IconName::Warning
                                    } else {
                                        IconName::ToolHammer
                                    },
                                    if task.status == BuildStatus::Failed {
                                        Color::Error
                                    } else {
                                        Color::Muted
                                    },
                                    Some(task.line),
                                    if task.phase_line.is_some() { 2 } else { 1 },
                                    None,
                                )
                            }
                            TreeRow::Message(index) => {
                                let message = session.messages.get(index)?;
                                (
                                    message.label.clone(),
                                    IconName::Warning,
                                    if message.error {
                                        Color::Error
                                    } else {
                                        Color::Warning
                                    },
                                    Some(message.line),
                                    if let Some(task) = session
                                        .tasks
                                        .iter()
                                        .find(|task| Some(task.line) == message.task_line)
                                    {
                                        if task.phase_line.is_some() { 3 } else { 2 }
                                    } else if message.phase_line.is_some_and(|line| {
                                        session.phases.iter().any(|phase| phase.line == line)
                                    }) {
                                        2
                                    } else {
                                        1
                                    },
                                    None,
                                )
                            }
                        };
                        let is_root = matches!(row, TreeRow::Root);
                        Some(
                            h_flex()
                                .id(("build-tree-row", index))
                                .when(is_root, |row| {
                                    row.debug_selector(|| "build-root-row".into())
                                })
                                .h_7()
                                .pl(px(8. + depth as f32 * 16.))
                                .pr_2()
                                .gap_1()
                                .overflow_hidden()
                                .when(line.is_some() && line == session.selected_line, |row| {
                                    row.bg(cx.theme().colors().element_selected)
                                })
                                .hover(|row| row.bg(cx.theme().colors().element_hover))
                                .when(is_root, |row| {
                                    row.child(
                                        Icon::new(if session.expanded {
                                            IconName::ChevronDown
                                        } else {
                                            IconName::ChevronRight
                                        })
                                        .size(IconSize::Small),
                                    )
                                })
                                .child(Icon::new(icon).size(IconSize::Small).color(color))
                                .child(
                                    div().flex_1().min_w_0().child(
                                        Label::new(label)
                                            .when(is_root, |label| {
                                                label.weight(FontWeight::SEMIBOLD)
                                            })
                                            .truncate(),
                                    ),
                                )
                                .when_some(duration, |row, duration| {
                                    row.child(
                                        Label::new(elapsed_label(duration))
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    )
                                })
                                .on_click({
                                    let entity = entity.clone();
                                    move |_, _, cx| {
                                        entity.update(cx, |panel, cx| {
                                            if let Some(session) = &mut panel.sessions[tab.index()]
                                            {
                                                if is_root {
                                                    session.expanded = !session.expanded;
                                                    session.selected_line = None;
                                                } else if let Some(line) = line {
                                                    session.selected_line = Some(line);
                                                    if line >= session.discarded {
                                                        session.follow = false;
                                                        session.scroll.scroll_to_item(
                                                            line - session.discarded,
                                                            ScrollStrategy::Top,
                                                        );
                                                    }
                                                }
                                            }
                                            cx.notify();
                                        });
                                    }
                                })
                                .into_any_element(),
                        )
                    })
                    .collect()
            }
        })
        .size_full();
        if session.follow && !session.lines.is_empty() {
            session
                .scroll
                .scroll_to_item(session.lines.len() - 1, ScrollStrategy::Bottom);
        }
        let console = uniform_list("build-console", session.lines.len(), {
            let panel = cx.entity();
            move |range, _, cx| {
                let panel = panel.read(cx);
                let Some(session) = &panel.sessions[tab.index()] else {
                    return Vec::new();
                };
                range
                    .filter_map(|index| session.lines.get(index))
                    .map(|line| {
                        div()
                            .h_6()
                            .px_3()
                            .font_buffer(cx)
                            .text_ui_sm(cx)
                            .whitespace_nowrap()
                            .text_color(if line.stderr {
                                cx.theme().status().error
                            } else {
                                cx.theme().colors().text
                            })
                            .child(line.text.clone())
                    })
                    .collect()
            }
        })
        .debug_selector(|| "build-console".into())
        .track_scroll(&session.scroll)
        .with_width_from_item(
            session
                .widths
                .front()
                .map(|(index, _)| index.saturating_sub(session.discarded)),
        )
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .size_full()
        .on_scroll_wheel(cx.listener(
            move |panel, event: &gpui::ScrollWheelEvent, _, cx| {
                if event.delta.pixel_delta(px(24.)).y != px(0.) {
                    if let Some(session) = &mut panel.sessions[tab.index()] {
                        session.follow = false;
                    }
                    cx.notify();
                }
            },
        ));
        let console_tools = v_flex()
            .debug_selector(|| "build-right-actions".into())
            .h_full()
            .justify_start()
            .flex_shrink_0()
            .p_1()
            .gap_1()
            .child(
                IconButton::new("follow-build-output", IconName::ArrowDown)
                    .tab_index(0isize)
                    .aria_label("Scroll to the end")
                    .toggle_state(session.follow)
                    .tooltip(Tooltip::text("Scroll to the end"))
                    .on_click(cx.listener(move |panel, _, _, cx| {
                        if let Some(session) = &mut panel.sessions[tab.index()] {
                            session.follow = !session.follow;
                        }
                        cx.notify();
                    })),
            )
            .child(
                IconButton::new("copy-build-output", IconName::Copy)
                    .tab_index(0isize)
                    .aria_label("Copy output")
                    .tooltip(Tooltip::text("Copy output"))
                    .on_click(cx.listener(move |panel, _, _, cx| {
                        if let Some(session) = &panel.sessions[tab.index()] {
                            cx.write_to_clipboard(ClipboardItem::new_string(
                                session
                                    .lines
                                    .iter()
                                    .map(|line| line.text.as_ref())
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                            ));
                        }
                    })),
            )
            .child(
                IconButton::new("clear-build-output", IconName::Trash)
                    .tab_index(0isize)
                    .aria_label("Clear output")
                    .tooltip(Tooltip::text("Clear output"))
                    .on_click(cx.listener(move |panel, _, _, cx| {
                        if let Some(session) = &mut panel.sessions[tab.index()] {
                            session.clear();
                        }
                        cx.notify();
                    })),
            );
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                h_flex()
                    .items_start()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(toolbar)
                    .child(
                        div()
                            .debug_selector(|| "build-tree".into())
                            .w(gpui::relative(0.32))
                            .min_w(px(180.))
                            .h_full()
                            .border_r_1()
                            .border_color(cx.theme().colors().border_variant)
                            .child(tree),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .when(session.discarded > 0, |this| {
                                this.child(
                                    Label::new(format!(
                                        "{} earlier lines omitted",
                                        session.discarded
                                    ))
                                    .color(Color::Muted)
                                    .size(LabelSize::Small),
                                )
                            })
                            .child(console),
                    )
                    .child(console_tools),
            )
            .into_any_element()
    }
}

impl EventEmitter<BuildEvent> for BuildPanel {}
impl EventEmitter<PanelEvent> for BuildPanel {}
impl Focusable for BuildPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl Panel for BuildPanel {
    fn persistent_name() -> &'static str {
        "Build"
    }
    fn panel_key() -> &'static str {
        "android_build"
    }
    fn position(&self, _: &Window, _: &App) -> DockPosition {
        DockPosition::Bottom
    }
    fn position_is_valid(&self, position: DockPosition) -> bool {
        position == DockPosition::Bottom
    }
    fn set_position(&mut self, _: DockPosition, _: &mut Window, _: &mut Context<Self>) {}
    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        px(320.)
    }
    fn enabled(&self, cx: &App) -> bool {
        crate::project_surfaces::build_window_available(&self.workspace, cx)
    }
    fn icon(&self, _: &Window, cx: &App) -> Option<IconName> {
        self.enabled(cx).then_some(IconName::ToolHammer)
    }
    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Build")
    }
    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(ToggleBuild)
    }
    fn activation_priority(&self) -> u32 {
        5
    }
}
struct BuildToolbarTitle;

impl Render for BuildToolbarTitle {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .debug_selector(|| "build-toolbar-title".into())
            .child(Label::new("Build").weight(FontWeight::BOLD))
    }
}

impl Render for BuildPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = self.render_session(cx);
        for tab in [BuildTab::Sync, BuildTab::Output] {
            let status = self.sessions[tab.index()]
                .as_ref()
                .map(BuildSession::activity_status);
            if self.toolbar_statuses[tab.index()] != status {
                self.toolbar_statuses[tab.index()] = status;
                self.toolbar.update(cx, |toolbar, cx| {
                    toolbar
                        .set_tab_icon(
                            tab.index(),
                            status.map(|status| Icon::new(status.icon()).color(status.color())),
                            cx,
                        )
                        .log_err();
                });
            }
        }
        v_flex()
            .id("android-build-panel")
            .track_focus(&self.focus_handle)
            .size_full()
            .min_h_0()
            .bg(cx.theme().colors().panel_background)
            .child(
                div()
                    .px_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(self.toolbar.clone()),
            )
            .child(content)
    }
}

pub(crate) struct OutputLine {
    pub text: String,
    pub stderr: bool,
}
pub(crate) enum ProcessOutput {
    Success(String),
    Cancelled,
}

#[derive(Debug)]
pub(crate) struct CommandCancelled;
impl std::fmt::Display for CommandCancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Operation cancelled")
    }
}
impl std::error::Error for CommandCancelled {}
impl ProcessOutput {
    pub(crate) fn stdout(self) -> Result<String> {
        match self {
            Self::Success(output) => Ok(output),
            Self::Cancelled => Err(CommandCancelled.into()),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputPolicy {
    Preserve,
    AndroidProjectModel,
}

#[derive(Clone, Copy)]
enum LinePresentation {
    Prefix,
    Visible,
    Hidden,
}

struct OutputPresentation {
    pending: Vec<u8>,
    line: LinePresentation,
    hide_model: bool,
    carriage_return: bool,
}

impl OutputPresentation {
    fn new(hide_model: bool) -> Self {
        Self {
            pending: Vec::new(),
            line: if hide_model {
                LinePresentation::Prefix
            } else {
                LinePresentation::Visible
            },
            hide_model,
            carriage_return: false,
        }
    }

    fn push(&mut self, byte: u8) -> Option<String> {
        if byte == b'\n' && self.carriage_return {
            self.carriage_return = false;
            return None;
        }
        self.carriage_return = byte == b'\r';
        if byte == b'\n' || byte == b'\r' {
            let text = match self.line {
                LinePresentation::Hidden => None,
                _ => Some(String::from_utf8_lossy(&self.pending).into_owned()),
            };
            self.pending.clear();
            self.line = if self.hide_model {
                LinePresentation::Prefix
            } else {
                LinePresentation::Visible
            };
            return text;
        }
        if matches!(self.line, LinePresentation::Hidden) {
            return None;
        }
        self.pending.push(byte);
        if matches!(self.line, LinePresentation::Prefix) {
            let prefixes = [
                android_tools::project_model::MODEL_OUTPUT_PREFIX.as_bytes(),
                android_tools::project_context::CONTEXT_OUTPUT_PREFIX.as_bytes(),
            ];
            if prefixes.contains(&self.pending.as_slice()) {
                self.pending.clear();
                self.line = LinePresentation::Hidden;
                return None;
            } else if !prefixes
                .iter()
                .any(|prefix| prefix.starts_with(&self.pending))
            {
                self.line = LinePresentation::Visible;
            }
        }
        if self.pending.len() >= MAX_LINE_BYTES {
            let end = match std::str::from_utf8(&self.pending) {
                Err(error) if error.error_len().is_none() => error.valid_up_to(),
                _ => self.pending.len(),
            };
            let text = String::from_utf8_lossy(&self.pending[..end]).into_owned();
            self.pending.drain(..end);
            return Some(text);
        }
        None
    }

    fn finish(self) -> Option<String> {
        if self.pending.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(&self.pending).into_owned())
        }
    }
}

async fn read_output(
    reader: impl AsyncRead + Unpin,
    stderr: bool,
    sender: mpsc::Sender<OutputLine>,
    capture: bool,
) -> Result<String> {
    read_output_with_policy(reader, stderr, sender, capture, OutputPolicy::Preserve).await
}

async fn read_output_with_policy(
    reader: impl AsyncRead + Unpin,
    stderr: bool,
    mut sender: mpsc::Sender<OutputLine>,
    capture: bool,
    policy: OutputPolicy,
) -> Result<String> {
    let mut reader = reader;
    let mut buffer = [0; 8192];
    let hide_model = policy == OutputPolicy::AndroidProjectModel && !stderr;
    let mut presentation = OutputPresentation::new(hide_model);
    let mut captured = Vec::new();
    loop {
        if hide_model {
            ensure!(!sender.is_closed(), "Build output window closed");
        }
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        if capture {
            ensure!(
                captured.len() + count <= MAX_MODEL_BYTES,
                "Android project description exceeded 16 MiB"
            );
            captured.extend_from_slice(&buffer[..count]);
        }
        for &byte in &buffer[..count] {
            if let Some(text) = presentation.push(byte) {
                sender
                    .send(OutputLine { text, stderr })
                    .await
                    .context("Build output window closed")?;
            }
        }
        if hide_model {
            // Hidden transport can stay readable without ever awaiting the output
            // sink. Yield so cancellation and deadlines can interrupt the capture.
            let mut yielded = false;
            futures::future::poll_fn(|task_context| {
                if std::mem::replace(&mut yielded, true) {
                    std::task::Poll::Ready(())
                } else {
                    task_context.waker().wake_by_ref();
                    std::task::Poll::Pending
                }
            })
            .await;
        }
    }
    if let Some(text) = presentation.finish() {
        sender
            .send(OutputLine { text, stderr })
            .await
            .context("Build output window closed")?;
    }
    if hide_model {
        ensure!(!sender.is_closed(), "Build output window closed");
        String::from_utf8(captured).context("Android project description is not valid UTF-8")
    } else {
        Ok(String::from_utf8_lossy(&captured).into_owned())
    }
}

pub(crate) async fn project_model_output(
    command: Command,
    executor: &BackgroundExecutor,
    timeout: Duration,
    sender: mpsc::Sender<OutputLine>,
    cancel: oneshot::Receiver<()>,
) -> Result<ProcessOutput> {
    command_output_inner_with_policy(
        command,
        timeout,
        sender,
        cancel,
        true,
        executor.timer(timeout),
        OutputPolicy::AndroidProjectModel,
    )
    .await
}

pub(crate) async fn command_output(
    command: Command,
    executor: &BackgroundExecutor,
    timeout: Duration,
    sender: mpsc::Sender<OutputLine>,
    cancel: oneshot::Receiver<()>,
    capture: bool,
) -> Result<ProcessOutput> {
    command_output_inner(
        command,
        timeout,
        sender,
        cancel,
        capture,
        executor.timer(timeout),
    )
    .await
}

async fn command_output_inner(
    command: Command,
    timeout: Duration,
    sender: mpsc::Sender<OutputLine>,
    cancel: oneshot::Receiver<()>,
    capture: bool,
    deadline: impl std::future::Future<Output = ()> + Send,
) -> Result<ProcessOutput> {
    command_output_inner_with_policy(
        command,
        timeout,
        sender,
        cancel,
        capture,
        deadline,
        OutputPolicy::Preserve,
    )
    .await
}

async fn command_output_inner_with_policy(
    command: Command,
    timeout: Duration,
    sender: mpsc::Sender<OutputLine>,
    mut cancel: oneshot::Receiver<()>,
    capture: bool,
    deadline: impl std::future::Future<Output = ()> + Send,
    policy: OutputPolicy,
) -> Result<ProcessOutput> {
    let program = command.get_program().to_string_lossy().into_owned();
    if cancel.try_recv()?.is_some() {
        return Ok(ProcessOutput::Cancelled);
    }
    match captured_command_output(
        command,
        timeout,
        sender,
        cancel.map(|_| ()),
        capture,
        deadline,
        policy,
    )
    .await?
    {
        CapturedProcessOutput::Completed { stdout, status } => {
            ensure!(status.success(), "{program} failed ({status}).");
            Ok(ProcessOutput::Success(stdout))
        }
        CapturedProcessOutput::Cancelled => Ok(ProcessOutput::Cancelled),
    }
}

pub(crate) enum CapturedProcessOutput {
    Completed {
        stdout: String,
        status: std::process::ExitStatus,
    },
    Cancelled,
}

pub(crate) async fn project_context_output(
    command: Command,
    executor: &BackgroundExecutor,
    timeout: Duration,
    sender: mpsc::Sender<OutputLine>,
    cancel: impl std::future::Future<Output = ()> + Send,
) -> Result<CapturedProcessOutput> {
    captured_command_output(
        command,
        timeout,
        sender,
        cancel,
        true,
        executor.timer(timeout),
        OutputPolicy::AndroidProjectModel,
    )
    .await
}

async fn captured_command_output(
    command: Command,
    timeout: Duration,
    sender: mpsc::Sender<OutputLine>,
    cancel: impl std::future::Future<Output = ()> + Send,
    capture: bool,
    deadline: impl std::future::Future<Output = ()> + Send,
    policy: OutputPolicy,
) -> Result<CapturedProcessOutput> {
    let mut cancel = cancel.boxed();
    if matches!(futures::poll!(&mut cancel), std::task::Poll::Ready(())) {
        return Ok(CapturedProcessOutput::Cancelled);
    }
    let program = command.get_program().to_string_lossy().into_owned();
    let mut process = BuildProcess {
        child: util::process::Child::spawn(command, Stdio::null(), Stdio::piped(), Stdio::piped())
            .with_context(|| format!("Could not start {program}"))?,
        completed: false,
    };
    let child = &mut process.child;
    let stdout = child.stdout.take().context("Missing command stdout")?;
    let stderr = child.stderr.take().context("Missing command stderr")?;
    let run = async {
        let (stdout, _) = futures::try_join!(
            read_output_with_policy(stdout, false, sender.clone(), capture, policy),
            read_output(stderr, true, sender, false)
        )?;
        let status = child.status().await?;
        Ok::<_, anyhow::Error>(CapturedProcessOutput::Completed { stdout, status })
    }
    .boxed();
    let deadline = deadline.boxed();
    let result = match select(run, select(deadline, cancel).boxed()).await {
        Either::Left((result, _)) => result,
        Either::Right((Either::Left(_), _)) => bail!(
            "{program} timed out after {} seconds. Check the SDK, JDK, and network, then retry.",
            timeout.as_secs()
        ),
        Either::Right((Either::Right(_), _)) => Ok(CapturedProcessOutput::Cancelled),
    };
    if matches!(result, Ok(CapturedProcessOutput::Completed { status, .. }) if status.success()) {
        process.child.preserve_descendants()?;
        process.completed = true;
    }
    result
}

struct BuildProcess {
    child: util::process::Child,
    completed: bool,
}
impl Drop for BuildProcess {
    fn drop(&mut self) {
        if !self.completed {
            self.child.kill().log_err();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{executor::block_on, future, io::Cursor};
    use util::command::new_std_command as new_command;

    struct ChunkedReader {
        bytes: Vec<u8>,
        offset: usize,
        chunks: VecDeque<usize>,
        maximum: usize,
        read_bytes: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    }

    impl AsyncRead for ChunkedReader {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buffer: &mut [u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            let count = self
                .chunks
                .pop_front()
                .unwrap_or(self.maximum)
                .max(1)
                .min(buffer.len())
                .min(self.bytes.len() - self.offset);
            buffer[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
            self.offset += count;
            if let Some(read_bytes) = &self.read_bytes {
                read_bytes.fetch_add(count, std::sync::atomic::Ordering::SeqCst);
            }
            std::task::Poll::Ready(Ok(count))
        }
    }

    async fn present_model_chunks(
        bytes: &[u8],
        chunks: VecDeque<usize>,
        maximum: usize,
        stderr: bool,
    ) -> Result<(String, Vec<OutputLine>)> {
        let (sender, receiver) = mpsc::channel(1);
        let (captured, lines) = futures::join!(
            read_output_with_policy(
                ChunkedReader {
                    bytes: bytes.to_vec(),
                    offset: 0,
                    chunks,
                    maximum,
                    read_bytes: None,
                },
                stderr,
                sender,
                true,
                OutputPolicy::AndroidProjectModel,
            ),
            receiver.collect::<Vec<_>>()
        );
        Ok((captured?, lines))
    }

    #[test]
    fn context_console_preserves_raw_records_and_filters_exact_stdout_at_every_split() -> Result<()>
    {
        block_on(async {
            let prefix = android_tools::project_context::CONTEXT_OUTPUT_PREFIX;
            let input = format!(
                "{prefix}{{\"phase\":\"partial\"}}\r\n {prefix}visible\nwarning {prefix}visible\n{prefix}{{\"phase\":\"complete\"}}"
            );
            for split in 1..=prefix.len() + 1 {
                let (captured, lines) =
                    present_model_chunks(input.as_bytes(), VecDeque::from([split]), 1, false)
                        .await?;
                assert_eq!(captured, input);
                assert_eq!(
                    lines
                        .iter()
                        .map(|line| line.text.as_str())
                        .collect::<Vec<_>>(),
                    vec![
                        format!(" {prefix}visible"),
                        format!("warning {prefix}visible")
                    ]
                );
                let (captured, lines) =
                    present_model_chunks(input.as_bytes(), VecDeque::from([split]), 1, true)
                        .await?;
                assert_eq!(captured, input);
                assert_eq!(lines.len(), 4);
                assert!(lines.iter().all(|line| line.stderr));
            }
            Ok(())
        })
    }

    #[test]
    fn context_console_hidden_records_remain_bounded_and_partial_prefixes_stay_visible()
    -> Result<()> {
        block_on(async {
            let prefix = android_tools::project_context::CONTEXT_OUTPUT_PREFIX;
            let mut presentation = OutputPresentation::new(true);
            for byte in prefix
                .bytes()
                .chain(std::iter::repeat_n(b'x', MAX_CONTEXT_TRANSPORT_TEST_BYTES))
            {
                assert!(presentation.push(byte).is_none());
                assert!(presentation.pending.len() <= prefix.len());
            }
            assert!(presentation.finish().is_none());
            for length in 1..prefix.len() {
                let partial = &prefix[..length];
                let (captured, lines) =
                    present_model_chunks(partial.as_bytes(), VecDeque::new(), 1, false).await?;
                assert_eq!(captured, partial);
                assert_eq!(lines.len(), 1);
                assert_eq!(lines[0].text, partial);
            }
            Ok(())
        })
    }

    const MAX_CONTEXT_TRANSPORT_TEST_BYTES: usize =
        android_tools::project_context::MAX_CONTEXT_RECORD_BYTES;

    #[cfg(unix)]
    #[test]
    fn failed_context_command_retains_raw_stdout_and_visible_diagnostics() -> Result<()> {
        block_on(async {
            let mut command = new_command("/bin/sh");
            command.args(["-c", "printf 'KODA_PROJECT_CONTEXT={\"phase\":\"partial\"}\\n'; printf 'SDK configuration failed\\n' >&2; exit 7"]);
            let (sender, receiver) = mpsc::channel(1);
            let (completion, lines) = futures::join!(
                captured_command_output(
                    command,
                    Duration::from_secs(5),
                    sender,
                    future::pending(),
                    true,
                    future::pending(),
                    OutputPolicy::AndroidProjectModel
                ),
                receiver.collect::<Vec<_>>()
            );
            let CapturedProcessOutput::Completed { stdout, status } = completion? else {
                anyhow::bail!("Command was unexpectedly cancelled")
            };
            assert_eq!(status.code(), Some(7));
            assert_eq!(stdout, "KODA_PROJECT_CONTEXT={\"phase\":\"partial\"}\n");
            assert_eq!(lines.len(), 1);
            assert_eq!(lines[0].text, "SDK configuration failed");
            assert!(lines[0].stderr);
            Ok(())
        })
    }

    #[test]
    fn model_console_filters_only_exact_leading_stdout_records() -> Result<()> {
        block_on(async {
            let input = concat!(
                "KODA_ANDROID_PROJECT_MODEL={\"machine\":true}\n",
                " KODA_ANDROID_PROJECT_MODEL=ordinary\n",
                "warning: KODA_ANDROID_PROJECT_MODEL=ordinary\n",
                "KODA_ANDROID_PROJECT_MODEL ordinary\n",
                "koda_android_project_model=ordinary\n",
                "AGPBI: {\"kind\":\"warning\"}\n",
                "[databinding] {\"msg\":\"error\"}\n",
                "{\"ordinary\":\"JSON\"}\n"
            );
            let (captured, lines) =
                present_model_chunks(input.as_bytes(), VecDeque::new(), 1, false).await?;
            assert_eq!(captured.as_bytes(), input.as_bytes());
            assert_eq!(
                lines
                    .iter()
                    .map(|line| line.text.as_str())
                    .collect::<Vec<_>>(),
                input.lines().skip(1).collect::<Vec<_>>()
            );
            assert!(lines.iter().all(|line| !line.stderr));
            let (captured, lines) =
                present_model_chunks(input.as_bytes(), VecDeque::new(), 7, true).await?;
            assert_eq!(captured, input);
            assert_eq!(
                lines
                    .iter()
                    .map(|line| line.text.as_str())
                    .collect::<Vec<_>>(),
                input.lines().collect::<Vec<_>>()
            );
            assert!(lines.iter().all(|line| line.stderr));
            Ok(())
        })
    }

    #[test]
    fn model_console_matches_prefix_across_every_read_split() -> Result<()> {
        block_on(async {
            let prefix = android_tools::project_model::MODEL_OUTPUT_PREFIX;
            let input = format!("{prefix}{{\"name\":\"東京€\"}}\nwarning: café 東京€\n");
            for split in 1..=prefix.len() {
                let (captured, lines) =
                    present_model_chunks(input.as_bytes(), VecDeque::from([split]), 1, false)
                        .await?;
                assert_eq!(captured, input, "split {split}");
                assert_eq!(lines.len(), 1, "split {split}");
                assert_eq!(lines[0].text, "warning: café 東京€", "split {split}");
            }
            Ok(())
        })
    }

    #[test]
    fn model_console_preserves_line_endings_blanks_and_unterminated_lines() -> Result<()> {
        block_on(async {
            for ending in ["\n", "\r", "\r\n"] {
                for final_record in [false, true] {
                    let input = format!(
                        "first{ending}{ending}KODA_ANDROID_PROJECT_MODEL={{}}{ending}last{ending}{}",
                        if final_record {
                            "KODA_ANDROID_PROJECT_MODEL={}"
                        } else {
                            "tail"
                        }
                    );
                    let (captured, lines) =
                        present_model_chunks(input.as_bytes(), VecDeque::new(), 1, false).await?;
                    assert_eq!(captured, input);
                    let mut expected = vec!["first", "", "last"];
                    if !final_record {
                        expected.push("tail");
                    }
                    assert_eq!(
                        lines
                            .iter()
                            .map(|line| line.text.as_str())
                            .collect::<Vec<_>>(),
                        expected
                    );
                }
            }
            Ok(())
        })
    }

    #[test]
    fn model_console_retains_partial_prefix_at_eof_and_after_mismatch() -> Result<()> {
        block_on(async {
            let prefix = android_tools::project_model::MODEL_OUTPUT_PREFIX;
            for end in 1..prefix.len() {
                let text = &prefix[..end];
                let (captured, lines) =
                    present_model_chunks(text.as_bytes(), VecDeque::new(), 1, false).await?;
                assert_eq!(captured, text);
                assert_eq!(lines.len(), 1);
                assert_eq!(lines[0].text, text);
                let text = format!("{text}!ordinary\n");
                let (captured, lines) =
                    present_model_chunks(text.as_bytes(), VecDeque::new(), 1, false).await?;
                assert_eq!(captured, text);
                assert_eq!(lines[0].text, text.trim_end_matches('\n'));
            }
            Ok(())
        })
    }

    #[test]
    fn model_console_chunks_unicode_without_restarting_prefix_detection() -> Result<()> {
        block_on(async {
            let body = format!(
                "{}€東京KODA_ANDROID_PROJECT_MODEL=ordinary{}",
                "a".repeat(MAX_LINE_BYTES - 1),
                "é".repeat(MAX_LINE_BYTES)
            );
            let input = format!("{body}\nKODA_ANDROID_PROJECT_MODEL={{}}\n");
            let (captured, lines) =
                present_model_chunks(input.as_bytes(), VecDeque::new(), 1, false).await?;
            assert_eq!(captured, input);
            assert!(lines.len() >= 3);
            assert!(lines.iter().all(|line| line.text.len() <= MAX_LINE_BYTES));
            assert!(lines.iter().all(|line| !line.text.contains('\u{fffd}')));
            assert_eq!(
                lines
                    .iter()
                    .map(|line| line.text.as_str())
                    .collect::<String>(),
                body
            );
            Ok(())
        })
    }

    #[test]
    fn model_console_hidden_record_does_not_buffer_its_payload() {
        let mut presentation = OutputPresentation::new(true);
        let prefix = android_tools::project_model::MODEL_OUTPUT_PREFIX.as_bytes();
        for &byte in prefix {
            assert!(presentation.push(byte).is_none());
            assert!(presentation.pending.len() < prefix.len());
        }
        for _ in 0..4 * 1024 * 1024 {
            assert!(presentation.push(b'x').is_none());
            assert!(presentation.pending.is_empty());
        }
        assert!(presentation.push(b'\r').is_none());
        assert!(presentation.push(b'\n').is_none());
        for &byte in b"warning" {
            assert!(presentation.push(byte).is_none());
        }
        assert_eq!(presentation.finish().as_deref(), Some("warning"));
    }

    #[test]
    fn model_console_ready_hidden_capture_yields_before_eof() -> Result<()> {
        block_on(async {
            use std::sync::{
                Arc,
                atomic::{AtomicUsize, Ordering},
            };
            let prefix = android_tools::project_model::MODEL_OUTPUT_PREFIX;
            let input = format!("{prefix}{}", "x".repeat(MAX_MODEL_BYTES - prefix.len()));
            let read_bytes = Arc::new(AtomicUsize::new(0));
            let (sender, mut receiver) = mpsc::channel(0);
            let read = read_output_with_policy(
                ChunkedReader {
                    bytes: input.as_bytes().to_vec(),
                    offset: 0,
                    chunks: VecDeque::new(),
                    maximum: 8192,
                    read_bytes: Some(read_bytes.clone()),
                },
                false,
                sender,
                true,
                OutputPolicy::AndroidProjectModel,
            );
            futures::pin_mut!(read);
            assert!(read.as_mut().now_or_never().is_none());
            let consumed_before_yield = read_bytes.load(Ordering::SeqCst);
            assert!(consumed_before_yield > 0);
            assert!(consumed_before_yield <= MAX_LINE_BYTES);
            assert!(consumed_before_yield < input.len());
            assert!(receiver.next().now_or_never().is_none());
            let (captured, lines) = futures::join!(read, receiver.collect::<Vec<_>>());
            assert_eq!(captured?, input);
            assert_eq!(read_bytes.load(Ordering::SeqCst), input.len());
            assert!(lines.is_empty());
            Ok(())
        })
    }

    #[test]
    fn model_console_capture_limit_applies_to_hidden_records() -> Result<()> {
        block_on(async {
            let prefix = android_tools::project_model::MODEL_OUTPUT_PREFIX;
            let input = format!("{prefix}{}", "x".repeat(MAX_MODEL_BYTES - prefix.len()));
            let (captured, lines) =
                present_model_chunks(input.as_bytes(), VecDeque::new(), 8192, false).await?;
            assert_eq!(captured, input);
            assert!(lines.is_empty());
            let oversized = format!("{input}x");
            let error = present_model_chunks(oversized.as_bytes(), VecDeque::new(), 8192, false)
                .await
                .err()
                .context("Oversized hidden capture must fail")?;
            assert!(error.to_string().contains("exceeded 16 MiB"));
            Ok(())
        })
    }

    #[test]
    fn model_console_invalid_utf8_does_not_become_a_successful_raw_model() -> Result<()> {
        block_on(async {
            let mut input = b"KODA_ANDROID_PROJECT_MODEL={\"name\":\"".to_vec();
            input.extend_from_slice(&[0xff, b'"', b'}', b'\n']);
            let error = present_model_chunks(&input, VecDeque::new(), 1, false)
                .await
                .err()
                .context("Invalid UTF-8 must fail raw capture")?;
            assert!(error.to_string().contains("not valid UTF-8"));
            Ok(())
        })
    }

    #[test]
    fn model_console_preserves_upstream_diagnostic_fixture_contents() -> Result<()> {
        block_on(async {
            for fixture in [
                include_str!("../tests/fixtures/sync_console/androidGradlePluginErrors.txt"),
                include_str!("../tests/fixtures/sync_console/xmlParsingError.txt"),
                include_str!("../tests/fixtures/sync_console/xmlParsingErrorsDuringSync.txt"),
            ] {
                let mut input = String::new();
                for line in fixture.split_inclusive('\n') {
                    input.push_str("KODA_ANDROID_PROJECT_MODEL={\"supplemental\":true}\n");
                    input.push_str(line);
                }
                let (captured, lines) =
                    present_model_chunks(input.as_bytes(), VecDeque::new(), 13, false).await?;
                assert_eq!(captured.as_bytes(), input.as_bytes());
                assert_eq!(
                    lines
                        .iter()
                        .map(|line| line.text.as_str())
                        .collect::<Vec<_>>(),
                    fixture.lines().collect::<Vec<_>>()
                );
            }
            Ok(())
        })
    }

    #[test]
    fn model_console_retains_basic_and_v2_decoding_and_validation() -> Result<()> {
        block_on(async {
            use android_tools::{
                generated_artifacts::{ModelConsumerVersion, parse_generated_artifacts},
                project_model::parse_model,
                project_tree_facts::FactsUnavailableReason,
            };
            let directory = tempfile::tempdir()?;
            let root = directory.path().canonicalize()?;
            let mut value = serde_json::json!({
                "version": 1, "root": root, "diagnostics": [],
                "modules": [{"path": ":app", "directory": root,
                    "namespace": "dev.sample", "kind": "application", "variants": []}]
            });
            let prefix = android_tools::project_model::MODEL_OUTPUT_PREFIX;
            let consumer = ModelConsumerVersion {
                major: 66,
                minor: 1,
                description: None,
            };
            for v2 in [false, true] {
                if v2 {
                    value["generatedArtifacts"] = serde_json::json!({
                        "schema": 1, "root": root,
                        "modules": [{"module": ":app", "directory": root,
                            "versions": {"status": "unavailable", "value": {
                                "capability": "Versions", "detail": "Supplemental fixture getter unavailable"}},
                            "buildFolder": {"status": "available", "value": root.join("build")},
                            "variants": {"status": "available", "value": []}}]
                    });
                }
                let input = format!("Configure project\n{prefix}{value}\nBUILD SUCCESSFUL\n");
                let (captured, lines) =
                    present_model_chunks(input.as_bytes(), VecDeque::new(), 3, false).await?;
                assert_eq!(captured, input);
                let expected = parse_model(&input, &root)?;
                let actual = parse_model(&captured, &root)?;
                assert_eq!(
                    serde_json::to_value(&actual)?,
                    serde_json::to_value(&expected)?
                );
                assert_eq!(lines.len(), 2);
                if v2 {
                    let snapshot = parse_generated_artifacts(&captured, &actual, 8, &consumer)?;
                    snapshot.ensure_current(&actual, 8)?;
                    assert_eq!(snapshot.modules().len(), 1);
                    assert_eq!(snapshot.modules()[0].module, ":app");
                    assert!(snapshot.modules()[0].variants.available()?.is_empty());
                } else {
                    assert_eq!(
                        parse_generated_artifacts(&captured, &actual, 8, &consumer)
                            .expect_err("Legacy sidecar remains unavailable")
                            .reason,
                        FactsUnavailableReason::MissingMetadata
                    );
                }
                let duplicate = format!("{captured}{prefix}{value}\n");
                let (captured, lines) =
                    present_model_chunks(duplicate.as_bytes(), VecDeque::new(), 5, false).await?;
                assert_eq!(captured, duplicate);
                assert_eq!(lines.len(), 2);
                assert!(parse_model(&captured, &root).is_err());
                assert_eq!(
                    parse_generated_artifacts(&captured, &actual, 8, &consumer)
                        .expect_err("Duplicate model remains malformed")
                        .reason,
                    FactsUnavailableReason::Malformed
                );
            }
            let malformed = "KODA_ANDROID_PROJECT_MODEL={malformed}\nwarning: retained\n";
            let (captured, lines) =
                present_model_chunks(malformed.as_bytes(), VecDeque::new(), 1, false).await?;
            assert_eq!(captured, malformed);
            assert_eq!(lines[0].text, "warning: retained");
            assert!(parse_model(&captured, &root).is_err());
            Ok(())
        })
    }

    #[test]
    fn model_console_propagates_closed_output_even_for_hidden_only_input() -> Result<()> {
        block_on(async {
            let (sender, receiver) = mpsc::channel(1);
            drop(receiver);
            let result = read_output_with_policy(
                Cursor::new(b"KODA_ANDROID_PROJECT_MODEL={}\n"),
                false,
                sender,
                true,
                OutputPolicy::AndroidProjectModel,
            )
            .await;
            assert!(result.is_err_and(|error| error.to_string().contains("output window closed")));
            Ok(())
        })
    }

    #[test]
    fn model_console_visible_output_obeys_backpressure() -> Result<()> {
        block_on(async {
            let input = format!(
                "KODA_ANDROID_PROJECT_MODEL={{}}\n{}",
                "warning: live\n".repeat(50)
            );
            let (sender, receiver) = mpsc::channel(0);
            let read = read_output_with_policy(
                Cursor::new(input.as_bytes()),
                false,
                sender,
                true,
                OutputPolicy::AndroidProjectModel,
            );
            futures::pin_mut!(read);
            assert!(read.as_mut().now_or_never().is_none());
            let (captured, lines) = futures::join!(read, receiver.collect::<Vec<_>>());
            assert_eq!(captured?, input);
            assert_eq!(lines.len(), 50);
            assert!(lines.iter().all(|line| line.text == "warning: live"));
            Ok(())
        })
    }

    #[test]
    fn model_console_sync_and_build_sessions_keep_distinct_visible_output() -> Result<()> {
        block_on(async {
            let text = "> Task :app:model\nKODA_ANDROID_PROJECT_MODEL={}\nw: visible warning\n";
            let (captured, lines) =
                present_model_chunks(text.as_bytes(), VecDeque::new(), 2, false).await?;
            assert_eq!(captured, text);
            let mut sync = BuildSession::new(1, "Sync project".into());
            for line in lines {
                sync.append(line);
            }
            let (sender, receiver) = mpsc::channel(1);
            let (captured, lines) = futures::join!(
                read_output(Cursor::new(text.as_bytes()), false, sender, true),
                receiver.collect::<Vec<_>>()
            );
            assert_eq!(captured?, text);
            let mut build = BuildSession::new(2, "Build project".into());
            for line in lines {
                build.append(line);
            }
            assert_eq!(sync.lines.len(), 2);
            assert_eq!(build.lines.len(), 3);
            assert_eq!(sync.tasks.len(), 1);
            assert_eq!(build.tasks.len(), 1);
            assert_eq!(sync.messages.len(), 1);
            assert_eq!(build.messages.len(), 1);
            assert!(
                sync.lines
                    .iter()
                    .all(|line| !line.text.starts_with("KODA_ANDROID_PROJECT_MODEL="))
            );
            assert!(
                build
                    .lines
                    .iter()
                    .any(|line| line.text.as_ref() == "KODA_ANDROID_PROJECT_MODEL={}")
            );
            Ok(())
        })
    }

    #[cfg(unix)]
    #[test]
    fn model_console_real_command_preserves_stderr_failure_and_normal_command_output() -> Result<()>
    {
        block_on(async {
            for (policy, succeeds) in [
                (OutputPolicy::AndroidProjectModel, true),
                (OutputPolicy::AndroidProjectModel, false),
                (OutputPolicy::Preserve, true),
            ] {
                let mut command = new_command("/bin/sh");
                command.args(["-c", if succeeds {
                    "printf 'progress\\nKODA_ANDROID_PROJECT_MODEL={}\\n'; printf 'KODA_ANDROID_PROJECT_MODEL=stderr\\nwarning: retained\\n' >&2"
                } else {
                    "printf 'progress\\nKODA_ANDROID_PROJECT_MODEL={}\\n'; printf 'warning: retained\\n' >&2; exit 7"
                }]);
                let (sender, receiver) = mpsc::channel(1);
                let (_cancel, cancelled) = oneshot::channel();
                let (result, lines) = futures::join!(
                    command_output_inner_with_policy(
                        command,
                        Duration::from_secs(30),
                        sender,
                        cancelled,
                        true,
                        future::pending(),
                        policy
                    ),
                    receiver.collect::<Vec<_>>()
                );
                if succeeds {
                    assert_eq!(
                        result?.stdout()?,
                        "progress\nKODA_ANDROID_PROJECT_MODEL={}\n"
                    );
                } else {
                    assert!(result.is_err());
                }
                assert!(
                    lines
                        .iter()
                        .any(|line| line.text == "progress" && !line.stderr)
                );
                assert!(
                    lines
                        .iter()
                        .any(|line| line.text == "warning: retained" && line.stderr)
                );
                assert_eq!(
                    lines
                        .iter()
                        .any(|line| line.text == "KODA_ANDROID_PROJECT_MODEL={}" && !line.stderr),
                    policy == OutputPolicy::Preserve
                );
                if succeeds {
                    assert!(lines.iter().any(|line| line.text
                        == "KODA_ANDROID_PROJECT_MODEL=stderr"
                        && line.stderr));
                }
            }
            Ok(())
        })
    }

    #[cfg(unix)]
    #[test]
    fn model_console_real_command_can_cancel_after_hidden_transport() -> Result<()> {
        block_on(async {
            let mut command = new_command("/bin/sh");
            command.args([
                "-c",
                "printf 'KODA_ANDROID_PROJECT_MODEL={}\\nready\\n'; exec sleep 30",
            ]);
            let (sender, mut receiver) = mpsc::channel::<OutputLine>(1);
            let (cancel, cancelled) = oneshot::channel();
            let receive = async {
                let line = receiver
                    .next()
                    .await
                    .context("Missing live Sync progress")?;
                assert_eq!(line.text, "ready");
                cancel
                    .send(())
                    .map_err(|_| anyhow::anyhow!("Sync cancellation receiver closed"))?;
                while receiver.next().await.is_some() {}
                Ok::<_, anyhow::Error>(())
            };
            let (result, received) = futures::join!(
                command_output_inner_with_policy(
                    command,
                    Duration::from_secs(30),
                    sender,
                    cancelled,
                    true,
                    future::pending(),
                    OutputPolicy::AndroidProjectModel
                ),
                receive
            );
            received?;
            assert!(matches!(result?, ProcessOutput::Cancelled));
            Ok(())
        })
    }

    #[cfg(unix)]
    #[test]
    fn model_console_timeout_and_preexisting_cancel_prevent_success() -> Result<()> {
        block_on(async {
            let mut command = new_command("/bin/sh");
            command.args(["-c", "exec sleep 30"]);
            let (sender, receiver) = mpsc::channel(1);
            let (_cancel, cancelled) = oneshot::channel();
            let (result, _) = futures::join!(
                command_output_inner_with_policy(
                    command,
                    Duration::from_secs(30),
                    sender,
                    cancelled,
                    true,
                    future::ready(()),
                    OutputPolicy::AndroidProjectModel
                ),
                receiver.collect::<Vec<_>>()
            );
            assert!(result.is_err_and(|error| error.to_string().contains("timed out")));
            let (sender, _receiver) = mpsc::channel(1);
            let (cancel, cancelled) = oneshot::channel();
            cancel
                .send(())
                .map_err(|_| anyhow::anyhow!("Missing Sync cancellation receiver"))?;
            assert!(matches!(
                command_output_inner_with_policy(
                    new_command("/nonexistent/program"),
                    Duration::from_secs(30),
                    sender,
                    cancelled,
                    true,
                    future::pending(),
                    OutputPolicy::AndroidProjectModel,
                )
                .await?,
                ProcessOutput::Cancelled
            ));
            Ok(())
        })
    }

    #[gpui::test]
    async fn pane_renders_empty_and_split_output_and_keeps_both_sessions(
        cx: &mut gpui::TestAppContext,
    ) {
        let _app_state = cx.update(workspace::AppState::test);
        let fs = project::FakeFs::new(cx.executor());
        fs.insert_tree("/project", serde_json::json!({"README.md": ""}))
            .await;
        let project = project::Project::test(fs, [std::path::Path::new("/project")], cx).await;
        let workspace = cx
            .add_window_view(|window, cx| Workspace::test_new(project, window, cx))
            .0;
        let (pane, cx) = cx.add_window_view(|_, cx| BuildPanel::new(workspace.downgrade(), cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("build-empty").is_some());
        pane.update_in(cx, |pane, window, cx| {
            for tab in [BuildTab::Sync, BuildTab::Output] {
                let (id, output, logs) = pane.begin(tab, tab.label().into(), false, window, cx);
                drop(output);
                logs.detach();
                pane.finish(
                    tab,
                    id,
                    BuildStatus::Succeeded,
                    format!("{} completed", tab.label()),
                    cx,
                );
            }
            if let Some(session) = &mut pane.sessions[BuildTab::Output.index()] {
                session.append(OutputLine {
                    text: format!("> Task :app:assembleDebug {}", "a".repeat(300)),
                    stderr: false,
                });
            }
        });
        cx.run_until_parked();
        assert_eq!(
            pane.read_with(cx, |pane, _| pane.selected),
            BuildTab::Output
        );
        assert!(
            cx.debug_bounds("tabbed-toolbar-active-1").is_some(),
            "Starting output activates its real toolbar tab"
        );
        assert!(cx.debug_bounds("tabbed-toolbar-tab-icon-0").is_some());
        assert!(cx.debug_bounds("tabbed-toolbar-tab-icon-1").is_some());
        let tree = cx
            .debug_bounds("build-tree")
            .expect("Task tree should render");
        let console = cx
            .debug_bounds("build-console")
            .expect("Console should render");
        assert!(tree.size.width > px(0.) && tree.size.height > px(0.));
        assert!(console.origin.x >= tree.origin.x + tree.size.width);
        assert!(console.size.width > tree.size.width);
        let root = cx.debug_bounds("build-root-row").expect("Build root row");
        for selector in ["build-left-actions", "build-right-actions"] {
            let actions = cx.debug_bounds(selector).expect("Build action rail");
            assert_eq!(actions.origin.y, tree.origin.y);
            assert_eq!(actions.size.height, tree.size.height);
        }
        for selector in ["ICON-RotateCcw", "ICON-ArrowDown"] {
            let action = cx.debug_bounds(selector).expect("First rail action");
            assert!(action.origin.y >= root.origin.y);
            assert!(action.origin.y < root.origin.y + root.size.height);
        }
        pane.read_with(cx, |pane, _| {
            assert!(pane.sessions.iter().all(Option::is_some));
            let session = pane.sessions[BuildTab::Output.index()]
                .as_ref()
                .expect("Build output");
            let scroll = session.scroll.0.borrow();
            assert!(
                scroll
                    .last_item_size
                    .is_some_and(|size| size.contents.width > console.size.width)
            );
        });
        let stops = std::rc::Rc::new(std::cell::Cell::new(0));
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&pane, {
                let stops = stops.clone();
                move |_, event, _| {
                    if matches!(event, BuildEvent::Stop(BuildTab::Output)) {
                        stops.set(stops.get() + 1);
                    }
                }
            })
        });
        let id = pane.read_with(cx, |pane, _| {
            pane.sessions[BuildTab::Output.index()]
                .as_ref()
                .expect("Output")
                .id
        });
        let stop = cx.debug_bounds("ICON-Stop").expect("Stop control");
        cx.simulate_click(stop.center(), Default::default());
        assert_eq!(stops.get(), 0, "Finished build cannot be stopped");
        pane.update(cx, |pane, cx| {
            pane.set_waiting_for_emulator(BuildTab::Output, id, true, cx)
        });
        cx.run_until_parked();
        let stop = cx.debug_bounds("ICON-Stop").expect("Waiting Stop control");
        cx.simulate_click(stop.center(), Default::default());
        assert_eq!(
            stops.get(),
            1,
            "Stop remains enabled after Gradle succeeds while the emulator boots"
        );
        pane.read_with(cx, |pane, _| {
            assert_eq!(
                pane.sessions[BuildTab::Output.index()]
                    .as_ref()
                    .expect("Output")
                    .status,
                BuildStatus::Succeeded
            )
        });
        pane.update(cx, |pane, cx| {
            pane.set_waiting_for_emulator(BuildTab::Output, id, false, cx)
        });
        cx.run_until_parked();
        cx.simulate_click(stop.center(), Default::default());
        assert_eq!(stops.get(), 1, "Stop is disabled once the wait ends");
        pane.update(cx, |pane, cx| pane.select(BuildTab::Sync, cx));
        cx.run_until_parked();
        pane.read_with(cx, |pane, _| {
            assert!(pane.sessions.iter().all(Option::is_some))
        });
        let output = cx
            .debug_bounds("tabbed-toolbar-label-Build Output")
            .expect("Output tab");
        cx.simulate_click(output.center(), Default::default());
        cx.run_until_parked();
        assert_eq!(
            pane.read_with(cx, |pane, _| pane.selected),
            BuildTab::Output
        );
        assert!(cx.debug_bounds("tabbed-toolbar-active-1").is_some());
        let sync = cx
            .debug_bounds("tabbed-toolbar-label-Sync")
            .expect("Sync tab");
        cx.simulate_click(sync.center(), Default::default());
        cx.run_until_parked();
        assert_eq!(pane.read_with(cx, |pane, _| pane.selected), BuildTab::Sync);
        assert!(cx.debug_bounds("tabbed-toolbar-active-0").is_some());
        assert!(pane.read_with(cx, |pane, _| pane.sessions.iter().all(Option::is_some)));
        let closes = std::rc::Rc::new(std::cell::Cell::new(0));
        let _close_subscription = cx.update(|_, cx| {
            cx.subscribe(&pane, {
                let closes = closes.clone();
                move |_, event, _| {
                    if matches!(event, PanelEvent::Close) {
                        closes.set(closes.get() + 1);
                    }
                }
            })
        });
        let hide = cx
            .debug_bounds("tabbed-toolbar-action-0")
            .expect("Hide Build action");
        cx.simulate_click(hide.center(), Default::default());
        assert_eq!(closes.get(), 1);
    }

    #[test]
    fn tree_filters_completed_steps_and_groups_task_diagnostics() {
        let mut session = BuildSession::new(1, "Build project".into());
        for text in [
            "Task: :app",
            "Variant: debug",
            "Downloading https://example.com/gradle.zip",
            "> Task :app:preBuild UP-TO-DATE",
            "> Task :app:compileDebugKotlin",
            "w: /project/Main.kt: unused parameter",
            "e: /project/Main.kt: unresolved reference",
            "> Task :app:compileDebugKotlin FAILED",
        ] {
            session.append(OutputLine {
                text: text.into(),
                stderr: false,
            });
        }
        assert_eq!(session.tasks.len(), 2);
        assert_eq!(
            session.tasks.back().expect("Compile task").label.as_ref(),
            ":app:compileDebugKotlin"
        );
        assert_eq!(
            session.tasks.back().expect("Compile task").status,
            BuildStatus::Failed
        );
        let rows = session.tree_rows();
        assert!(matches!(
            rows.as_slice(),
            [
                TreeRow::Root,
                TreeRow::Downloads,
                TreeRow::Task(1),
                TreeRow::Message(0),
                TreeRow::Message(1)
            ]
        ));
        session.status = BuildStatus::Failed;
        assert_eq!(session.tree_rows().len(), rows.len());
        session.show_successful = true;
        assert_eq!(session.tree_rows().len(), rows.len() + 1);
        session.expanded = false;
        assert!(matches!(session.tree_rows().as_slice(), [TreeRow::Root]));
    }

    #[test]
    fn repeated_task_names_and_diagnostics_stay_in_their_sync_phase() {
        let mut session = BuildSession::new(1, "Sync project".into());
        session.phases.push_back(BuildPhase {
            label: "First import".into(),
            line: 0,
            status: BuildStatus::Succeeded,
            started: Instant::now(),
            elapsed: Some(Duration::from_secs(1)),
        });
        session.append(OutputLine {
            text: "> Task :app:prepareModel".into(),
            stderr: false,
        });
        session.phases.push_back(BuildPhase {
            label: "Second import".into(),
            line: 1,
            status: BuildStatus::Running,
            started: Instant::now(),
            elapsed: None,
        });
        session.append(OutputLine {
            text: "w: warning before any task".into(),
            stderr: true,
        });
        assert_eq!(session.messages[0].task_line, None);
        assert_eq!(session.messages[0].phase_line, Some(1));
        session.append(OutputLine {
            text: "> Task :app:prepareModel FAILED".into(),
            stderr: false,
        });
        assert_eq!(session.tasks.len(), 2);
        assert_eq!(session.tasks[0].status, BuildStatus::Succeeded);
        assert_eq!(session.tasks[1].status, BuildStatus::Failed);
        session.append(OutputLine {
            text: "FAILURE: Build failed with an exception.".into(),
            stderr: true,
        });
        assert_eq!(session.messages[1].task_line, None);
        assert!(matches!(
            session.tree_rows().as_slice(),
            [
                TreeRow::Root,
                TreeRow::Phase(1),
                TreeRow::Task(1),
                TreeRow::Message(0),
                TreeRow::Message(1)
            ]
        ));
    }

    #[test]
    fn interleaved_task_output_deduplicates_and_attributes_named_failures() {
        let mut session = BuildSession::new(1, "Build project".into());
        for text in [
            "> Task :app:compileDebugKotlin",
            "e: Main.kt: unresolved reference",
            "> Task :lib:processResources UP-TO-DATE",
            "> Task :app:compileDebugKotlin FAILED",
            "> Task :lib:processResources UP-TO-DATE",
            "Execution failed for task ':app:compileDebugKotlin'.",
        ] {
            session.append(OutputLine {
                text: text.into(),
                stderr: false,
            });
        }
        assert_eq!(session.tasks.len(), 2);
        assert_eq!(session.tasks[0].status, BuildStatus::Failed);
        assert_eq!(session.tasks[1].status, BuildStatus::Succeeded);
        assert!(
            session
                .messages
                .iter()
                .all(|message| message.task_line == Some(0))
        );
        session.status = BuildStatus::Failed;
        assert!(matches!(
            session.tree_rows().as_slice(),
            [
                TreeRow::Root,
                TreeRow::Task(0),
                TreeRow::Message(0),
                TreeRow::Message(1)
            ]
        ));
    }

    #[test]
    fn long_task_names_keep_distinct_identities_with_bounded_storage() {
        let mut session = BuildSession::new(1, "Build project".into());
        let prefix = format!(":app:{}", "a".repeat(MAX_TASK_LABEL_BYTES));
        for suffix in ["first", "second"] {
            session.append(OutputLine {
                text: format!("> Task {prefix}{suffix}"),
                stderr: false,
            });
        }
        assert_eq!(session.tasks.len(), 2);
        assert_eq!(session.tasks[0].label, session.tasks[1].label);
        assert_ne!(session.tasks[0].name, session.tasks[1].name);
        for index in 0..300 {
            session.append(OutputLine {
                text: format!("> Task :app:{}{index}", "b".repeat(MAX_LINE_BYTES - 100)),
                stderr: false,
            });
        }
        assert!(session.task_name_bytes <= MAX_TASKS * MAX_TASK_LABEL_BYTES);
        assert_eq!(session.task_indexes.len(), session.tasks.len());
    }

    #[gpui::test]
    async fn sync_retains_project_root_and_completed_import_phases(cx: &mut gpui::TestAppContext) {
        let _app_state = cx.update(workspace::AppState::test);
        let fs = project::FakeFs::new(cx.executor());
        fs.insert_tree("/project", serde_json::json!({"README.md": ""}))
            .await;
        let project = project::Project::test(fs, [std::path::Path::new("/project")], cx).await;
        let workspace = cx
            .add_window_view(|window, cx| Workspace::test_new(project, window, cx))
            .0;
        let (pane, cx) = cx.add_window_view(|_, cx| BuildPanel::new(workspace.downgrade(), cx));
        pane.update_in(cx, |pane, window, cx| {
            let (id, output, logs) =
                pane.begin(BuildTab::Sync, "Sync project".into(), false, window, cx);
            drop(output);
            logs.detach();
            pane.finish(
                BuildTab::Sync,
                id,
                BuildStatus::Succeeded,
                "Sync complete".into(),
                cx,
            );
            let session = pane.sessions[BuildTab::Sync.index()]
                .as_mut()
                .expect("Sync");
            session.elapsed = Some(Duration::from_secs(2));
            session.started = Instant::now()
                .checked_sub(Duration::from_secs(3600))
                .expect("Past instant");
            let (id, output, logs) = pane.begin(
                BuildTab::Sync,
                "Generating Android resources for Kotlin import".into(),
                true,
                window,
                cx,
            );
            drop(output);
            logs.detach();
            let session = pane.sessions[BuildTab::Sync.index()]
                .as_mut()
                .expect("Sync session");
            assert_eq!(session.label.as_ref(), "Sync project");
            assert_eq!(session.previous_elapsed, Duration::from_secs(2));
            assert_eq!(session.phases.len(), 2);
            assert_eq!(session.phases[0].status, BuildStatus::Succeeded);
            assert!(session.phases[0].elapsed.is_some());
            assert_eq!(session.phases[1].status, BuildStatus::Running);
            assert!(matches!(
                session.tree_rows().as_slice(),
                [TreeRow::Root, TreeRow::Phase(1)]
            ));
            session.append(OutputLine {
                text: "> Task :app:generateDebugResources".into(),
                stderr: false,
            });
            assert!(matches!(
                session.tree_rows().as_slice(),
                [TreeRow::Root, TreeRow::Phase(1), TreeRow::Task(0)]
            ));
            session.show_successful = true;
            assert!(matches!(
                session.tree_rows().as_slice(),
                [
                    TreeRow::Root,
                    TreeRow::Phase(0),
                    TreeRow::Phase(1),
                    TreeRow::Task(0)
                ]
            ));
            pane.finish(
                BuildTab::Sync,
                id,
                BuildStatus::Failed,
                "Resource generation failed".into(),
                cx,
            );
            let (id, output, logs) = pane.begin(
                BuildTab::Sync,
                "Importing Android Java model".into(),
                true,
                window,
                cx,
            );
            drop(output);
            logs.detach();
            pane.finish(
                BuildTab::Sync,
                id,
                BuildStatus::Succeeded,
                "Java import complete".into(),
                cx,
            );
            let session = pane.sessions[BuildTab::Sync.index()]
                .as_ref()
                .expect("Sync");
            assert_eq!(session.status, BuildStatus::Failed);
            assert_eq!(session.phases[1].status, BuildStatus::Failed);
            assert_eq!(session.phases[2].status, BuildStatus::Succeeded);
        });
    }

    #[test]
    fn output_preserves_blank_lines_unicode_and_model_text() -> Result<()> {
        block_on(async {
            let text = format!("first\r\n\r\n{}€\nlast", "a".repeat(MAX_LINE_BYTES - 1));
            let (sender, receiver) = mpsc::channel::<OutputLine>(2);
            let (captured, lines) = futures::join!(
                read_output(Cursor::new(text.as_bytes()), false, sender, true),
                receiver.collect::<Vec<_>>()
            );
            assert_eq!(captured?, text);
            assert_eq!(lines.first().map(|line| line.text.as_str()), Some("first"));
            assert_eq!(lines.get(1).map(|line| line.text.as_str()), Some(""));
            assert!(
                lines.iter().all(
                    |line| !line.text.contains('\u{fffd}') && line.text.len() <= MAX_LINE_BYTES
                )
            );
            assert_eq!(
                lines
                    .iter()
                    .skip(2)
                    .map(|line| line.text.as_str())
                    .collect::<String>(),
                format!("{}€last", "a".repeat(MAX_LINE_BYTES - 1))
            );
            Ok(())
        })
    }

    #[test]
    fn logs_remain_bounded_and_task_offsets_survive_eviction() {
        let mut session = BuildSession::new(1, "Build".into());
        for index in 0..MAX_LINES + 10 {
            session.append(OutputLine {
                text: format!("> Task :app:task{index} UP-TO-DATE"),
                stderr: false,
            });
        }
        assert_eq!(session.lines.len(), MAX_LINES);
        assert_eq!(session.discarded, 10);
        assert_eq!(session.tasks.len(), MAX_TASKS);
        assert_eq!(session.task_indexes.len(), MAX_TASKS);
        assert_eq!(
            session.tasks.back().map(|task| task.line),
            Some(MAX_LINES + 9)
        );
        assert!(session.bytes <= MAX_LOG_BYTES);
        for _ in 0..300 {
            session.append(OutputLine {
                text: "x".repeat(MAX_LINE_BYTES),
                stderr: true,
            });
        }
        assert!(session.bytes <= MAX_LOG_BYTES);
        assert!(session.lines.len() < MAX_LINES);
        session.clear();
        assert!(session.lines.is_empty() && session.tasks.is_empty());
        assert_eq!(session.bytes, 0);
        assert_eq!(session.status, BuildStatus::Running);
        assert!(session.widths.is_empty());
    }

    #[test]
    fn widest_console_row_tracks_eviction_and_import_phases() {
        let mut session = BuildSession::new(1, "Build".into());
        session.append(OutputLine {
            text: "short".into(),
            stderr: false,
        });
        session.append(OutputLine {
            text: "a much longer diagnostic with a file path".into(),
            stderr: true,
        });
        assert_eq!(session.widths.front().map(|(index, _)| *index), Some(1));
        for _ in 0..MAX_LINES {
            session.append(OutputLine {
                text: "new".into(),
                stderr: false,
            });
        }
        assert!(
            session
                .widths
                .front()
                .is_some_and(|(index, _)| *index >= session.discarded)
        );
        session.clear();
        session.append(OutputLine {
            text: "new import".into(),
            stderr: false,
        });
        assert_eq!(session.widths.front().map(|(index, _)| *index), Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_terminates_descendant_processes() -> Result<()> {
        block_on(async {
            let mut command = new_command("/bin/sh");
            command.args(["-c", "sleep 30 & printf '%s\\n' \"$!\"; wait"]);
            let (sender, mut receiver) = mpsc::channel::<OutputLine>(2);
            let (cancel, cancelled) = oneshot::channel();
            let receive = async {
                let pid = receiver.next().await.context("No descendant PID")?.text;
                cancel
                    .send(())
                    .map_err(|_| anyhow::anyhow!("Missing cancellation receiver"))?;
                while receiver.next().await.is_some() {}
                Ok::<_, anyhow::Error>(pid)
            };
            let (result, pid) = futures::join!(
                command_output_inner(
                    command,
                    Duration::from_secs(30),
                    sender,
                    cancelled,
                    false,
                    future::pending()
                ),
                receive
            );
            assert!(matches!(result?, ProcessOutput::Cancelled));
            let pid = pid?;
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let state = util::command::new_command("ps")
                    .args(["-o", "stat=", "-p", &pid])
                    .output()
                    .await?;
                let state = String::from_utf8_lossy(&state.stdout);
                if state.trim().is_empty() || state.trim().starts_with('Z') {
                    break;
                }
                ensure!(
                    Instant::now() < deadline,
                    "Descendant {pid} is still running after cancellation"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        })
    }

    #[cfg(unix)]
    #[test]
    fn live_output_arrives_before_exit_and_can_cancel_the_process() -> Result<()> {
        block_on(async {
            let mut command = new_command("/bin/sh");
            command.args(["-c", "printf 'live output\\n'; exec sleep 30"]);
            let (sender, mut receiver) = mpsc::channel::<OutputLine>(2);
            let (cancel, cancelled) = oneshot::channel();
            let run = command_output_inner(
                command,
                Duration::from_secs(30),
                sender,
                cancelled,
                false,
                future::pending(),
            );
            let receive = async {
                let line = receiver.next().await.context("No live output")?;
                assert_eq!(line.text, "live output");
                cancel
                    .send(())
                    .map_err(|_| anyhow::anyhow!("Command exited before output was received"))?;
                while receiver.next().await.is_some() {}
                Ok::<_, anyhow::Error>(())
            };
            let (result, received) = futures::join!(run, receive);
            received?;
            assert!(matches!(result?, ProcessOutput::Cancelled));
            Ok(())
        })
    }

    #[cfg(unix)]
    #[test]
    fn captures_stdout_separately_and_reports_stderr_and_failures() -> Result<()> {
        block_on(async {
            for (script, succeeds) in [
                ("printf 'model\\n'; printf 'warning\\n' >&2", true),
                ("printf 'error\\n' >&2; exit 1", false),
            ] {
                let mut command = new_command("/bin/sh");
                command.args(["-c", script]);
                let (sender, receiver) = mpsc::channel::<OutputLine>(2);
                let (_cancel, cancelled) = oneshot::channel();
                let (result, lines) = futures::join!(
                    command_output_inner(
                        command,
                        Duration::from_secs(30),
                        sender,
                        cancelled,
                        true,
                        future::pending()
                    ),
                    receiver.collect::<Vec<_>>()
                );
                if succeeds {
                    assert_eq!(result?.stdout()?, "model\n");
                } else {
                    assert!(result.is_err());
                }
                assert!(lines.iter().any(|line| line.stderr));
            }
            Ok(())
        })
    }

    #[cfg(unix)]
    #[test]
    fn timeout_and_preexisting_cancellation_prevent_success() -> Result<()> {
        block_on(async {
            let mut command = new_command("/bin/sh");
            command.args(["-c", "exec sleep 30"]);
            let (sender, receiver) = mpsc::channel::<OutputLine>(2);
            let (_cancel, cancelled) = oneshot::channel();
            let (result, _) = futures::join!(
                command_output_inner(
                    command,
                    Duration::from_secs(30),
                    sender,
                    cancelled,
                    false,
                    future::ready(())
                ),
                receiver.collect::<Vec<_>>()
            );
            assert!(result.is_err_and(|error| error.to_string().contains("timed out")));
            let command = new_command("/nonexistent/program");
            let (sender, _receiver) = mpsc::channel::<OutputLine>(2);
            let (cancel, cancelled) = oneshot::channel();
            cancel
                .send(())
                .map_err(|_| anyhow::anyhow!("Missing cancellation receiver"))?;
            assert!(matches!(
                command_output_inner(
                    command,
                    Duration::from_secs(30),
                    sender,
                    cancelled,
                    false,
                    future::pending()
                )
                .await?,
                ProcessOutput::Cancelled
            ));
            Ok(())
        })
    }
}
