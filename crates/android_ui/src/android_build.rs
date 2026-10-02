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
    label: SharedString,
    line: usize,
    failed: bool,
}

struct BuildSession {
    id: u64,
    label: SharedString,
    status: BuildStatus,
    started: Instant,
    elapsed: Option<Duration>,
    lines: VecDeque<LogLine>,
    bytes: usize,
    discarded: usize,
    tasks: VecDeque<BuildTask>,
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
            lines: VecDeque::new(),
            bytes: 0,
            discarded: 0,
            tasks: VecDeque::new(),
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
            if let Some(task) = text
                .trim()
                .strip_prefix("> Task ")
                .or_else(|| text.trim().strip_prefix("Task: "))
            {
                let mut label_end = task.len().min(MAX_TASK_LABEL_BYTES);
                while !task.is_char_boundary(label_end) {
                    label_end -= 1;
                }
                self.tasks.push_back(BuildTask {
                    label: task[..label_end].to_owned().into(),
                    line: self.discarded + self.lines.len(),
                    failed: task.ends_with(" FAILED"),
                });
                if self.tasks.len() > MAX_TASKS {
                    self.tasks.pop_front();
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

    fn clear(&mut self) {
        self.lines.clear();
        self.tasks.clear();
        self.widths.clear();
        self.bytes = 0;
        self.discarded = 0;
        self.scroll = UniformListScrollHandle::new();
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
        if let Some(previous) = previous {
            session.lines = previous.lines;
            session.bytes = previous.bytes;
            session.discarded = previous.discarded;
            session.tasks = previous.tasks;
            session.widths = previous.widths;
        }
        session.append(OutputLine {
            text: session.label.to_string(),
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
        let expanded = session.expanded;
        let label = session.label.clone();
        let status = session.status;
        let elapsed = session.elapsed.unwrap_or_else(|| session.started.elapsed());
        let toolbar = v_flex()
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
            );
        let tree = uniform_list(
            "build-task-tree",
            1 + if expanded { session.tasks.len() } else { 0 },
            {
                let panel = cx.entity();
                move |range, _, cx| {
                    let entity = panel.clone();
                    let panel = panel.read(cx);
                    let Some(session) = &panel.sessions[tab.index()] else {
                        return Vec::new();
                    };
                    range
                        .map(|index| {
                            if index == 0 {
                                h_flex()
                                    .id("build-root")
                                    .h_8()
                                    .px_2()
                                    .gap_1()
                                    .overflow_hidden()
                                    .child(
                                        Icon::new(if session.expanded {
                                            IconName::ChevronDown
                                        } else {
                                            IconName::ChevronRight
                                        })
                                        .size(IconSize::Small),
                                    )
                                    .child(
                                        Icon::new(session.status.icon())
                                            .size(IconSize::Small)
                                            .color(session.status.color()),
                                    )
                                    .child(Label::new(session.label.clone()).truncate())
                                    .on_click({
                                        let entity = entity.clone();
                                        move |_, _, cx| {
                                            entity.update(cx, |panel, cx| {
                                                if let Some(session) =
                                                    &mut panel.sessions[tab.index()]
                                                {
                                                    session.expanded = !session.expanded;
                                                }
                                                cx.notify();
                                            });
                                        }
                                    })
                                    .into_any_element()
                            } else if let Some(task) = session.tasks.get(index - 1) {
                                let line = task.line;
                                h_flex()
                                    .id(("build-task", index))
                                    .h_8()
                                    .pl_8()
                                    .pr_2()
                                    .gap_1()
                                    .overflow_hidden()
                                    .child(
                                        Icon::new(if task.failed {
                                            IconName::Warning
                                        } else {
                                            IconName::ToolHammer
                                        })
                                        .size(IconSize::Small)
                                        .color(
                                            if task.failed {
                                                Color::Error
                                            } else {
                                                Color::Muted
                                            },
                                        ),
                                    )
                                    .child(Label::new(task.label.clone()).truncate())
                                    .on_click({
                                        let entity = entity.clone();
                                        move |_, _, cx| {
                                            entity.update(cx, |panel, cx| {
                                                if let Some(session) =
                                                    &mut panel.sessions[tab.index()]
                                                {
                                                    session.follow = false;
                                                    session.scroll.scroll_to_item(
                                                        line.saturating_sub(session.discarded),
                                                        ScrollStrategy::Top,
                                                    );
                                                }
                                                cx.notify();
                                            });
                                        }
                                    })
                                    .into_any_element()
                            } else {
                                div().h_8().into_any_element()
                            }
                        })
                        .collect()
                }
            },
        )
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
                    .px_3()
                    .h_7()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(
                        Label::new(format!("{}: {}", label, status.label()))
                            .color(status.color())
                            .truncate(),
                    )
                    .child(div().flex_1())
                    .child(Label::new(format!("{} sec", elapsed.as_secs())).color(Color::Muted)),
            )
            .child(
                h_flex()
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
        ensure!(
            status.success(),
            "{program} failed ({status}). See Build output for details."
        );
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
