use anyhow::{Context as _, Result, bail, ensure};
use futures::{
    AsyncRead, AsyncReadExt as _, FutureExt as _, SinkExt as _, StreamExt as _,
    channel::{mpsc, oneshot},
    future::{Either, select},
};
use gpui::{
    App, BackgroundExecutor, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable,
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
    next_id: u64,
    clock_task: Option<Task<()>>,
    notification_task: Option<Task<()>>,
}

impl BuildPanel {
    pub(crate) fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        Self {
            workspace,
            focus_handle: cx.focus_handle(),
            sessions: [None, None],
            selected: BuildTab::Sync,
            next_id: 0,
            clock_task: None,
            notification_task: None,
        }
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
        self.selected = tab;
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

    fn select(&mut self, tab: BuildTab, cx: &mut Context<Self>) {
        self.selected = tab;
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
        let running = session.status == BuildStatus::Running;
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
                                let state = if session.status == BuildStatus::Running {
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
                                let label = if session.status == BuildStatus::Running
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
                                    session.status.icon(),
                                    session.status.color(),
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
    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::ToolHammer)
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
impl Render for BuildPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = self.render_session(cx);
        v_flex()
            .id("android-build-panel")
            .track_focus(&self.focus_handle)
            .size_full()
            .min_h_0()
            .bg(cx.theme().colors().panel_background)
            .child(
                h_flex()
                    .h_9()
                    .px_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .child(Label::new("Build")),
                    )
                    .children([BuildTab::Sync, BuildTab::Output].into_iter().map(|tab| {
                        let status = self.sessions[tab.index()]
                            .as_ref()
                            .map(|session| session.status);
                        Button::new(tab.label(), tab.label())
                            .tab_index(0isize)
                            .toggle_state(self.selected == tab)
                            .when_some(status, |button, status| {
                                button.start_icon(Icon::new(status.icon()).color(status.color()))
                            })
                            .on_click(cx.listener(move |panel, _, _, cx| panel.select(tab, cx)))
                    }))
                    .child(div().flex_1())
                    .child(
                        IconButton::new("hide-build", IconName::Dash)
                            .tab_index(0isize)
                            .aria_label("Hide Build window")
                            .tooltip(Tooltip::text("Hide Build window"))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(PanelEvent::Close))),
                    ),
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

async fn read_output(
    reader: impl AsyncRead + Unpin,
    stderr: bool,
    mut sender: mpsc::Sender<OutputLine>,
    capture: bool,
) -> Result<String> {
    let mut reader = reader;
    let mut buffer = [0; 8192];
    let mut pending = Vec::new();
    let mut captured = Vec::new();
    let mut carriage_return = false;
    loop {
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
            if byte == b'\n' && carriage_return {
                carriage_return = false;
                continue;
            }
            carriage_return = byte == b'\r';
            if byte == b'\n' || byte == b'\r' {
                sender
                    .send(OutputLine {
                        text: String::from_utf8_lossy(&pending).into_owned(),
                        stderr,
                    })
                    .await
                    .context("Build output window closed")?;
                pending.clear();
            } else {
                pending.push(byte);
                if pending.len() >= MAX_LINE_BYTES {
                    let end = match std::str::from_utf8(&pending) {
                        Err(error) if error.error_len().is_none() => error.valid_up_to(),
                        _ => pending.len(),
                    };
                    sender
                        .send(OutputLine {
                            text: String::from_utf8_lossy(&pending[..end]).into_owned(),
                            stderr,
                        })
                        .await
                        .context("Build output window closed")?;
                    pending.drain(..end);
                }
            }
        }
    }
    if !pending.is_empty() {
        sender
            .send(OutputLine {
                text: String::from_utf8_lossy(&pending).into_owned(),
                stderr,
            })
            .await
            .context("Build output window closed")?;
    }
    Ok(String::from_utf8_lossy(&captured).into_owned())
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
    mut cancel: oneshot::Receiver<()>,
    capture: bool,
    deadline: impl std::future::Future<Output = ()> + Send,
) -> Result<ProcessOutput> {
    if cancel.try_recv()?.is_some() {
        return Ok(ProcessOutput::Cancelled);
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
            read_output(stdout, false, sender.clone(), capture),
            read_output(stderr, true, sender, false)
        )?;
        let status = child.status().await?;
        ensure!(status.success(), "{program} failed ({status}).");
        Ok::<_, anyhow::Error>(ProcessOutput::Success(stdout))
    }
    .boxed();
    let deadline = deadline.boxed();
    let result = match select(run, select(deadline, cancel).boxed()).await {
        Either::Left((result, _)) => result,
        Either::Right((Either::Left(_), _)) => bail!(
            "{program} timed out after {} seconds. Check the SDK, JDK, and network, then retry.",
            timeout.as_secs()
        ),
        Either::Right((Either::Right(_), _)) => Ok(ProcessOutput::Cancelled),
    };
    if matches!(result, Ok(ProcessOutput::Success(_))) {
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
        pane.update(cx, |pane, cx| pane.select(BuildTab::Sync, cx));
        cx.run_until_parked();
        pane.read_with(cx, |pane, _| {
            assert!(pane.sessions.iter().all(Option::is_some))
        });
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
