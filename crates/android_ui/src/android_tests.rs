use super::*;
use android_tools::project_model::ModelToken;
use android_tools::testing::{self, SourceLocation, TestCase, TestKind, TestStatus};
use gpui::{ListHorizontalSizingBehavior, ScrollStrategy, UniformListScrollHandle, uniform_list};
use std::collections::{BTreeMap, BTreeSet};

actions!(
    android,
    [
        /// Opens the Android test runner.
        ToggleTests,
        /// Discovers Kotlin and Java tests in the selected variant.
        DiscoverTests,
        /// Runs instrumentation and Compose tests on the selected connected device.
        InstrumentationTest,
    ]
);

#[derive(Clone)]
enum TestEvent {
    Discover(TestKind),
    Run(TestKind, Vec<String>),
    Stop,
    Source(SourceLocation),
}
#[derive(Clone, PartialEq, Eq)]
enum TestRow {
    Suite,
    Class(String),
    Case(usize),
}

#[derive(Clone)]
pub(super) struct TestRequest {
    id: u64,
    root: PathBuf,
    target: AndroidTarget,
    kind: TestKind,
    serial: Option<String>,
    selectors: Vec<String>,
    discover_only: bool,
    initial_token: Option<ModelToken>,
    inputs_dirty: bool,
    waiting_for_model: bool,
    cancel_requested: bool,
    saved_revision: Option<u64>,
}

impl TestRequest {
    fn context_matches(&self, panel: &AndroidPanel, cx: &App) -> bool {
        panel.trusted_root(cx).is_ok_and(|root| root == self.root)
            && panel.selected_target.as_ref().is_some_and(|target| {
                target.module == self.target.module && target.variant == self.target.variant
            })
            && self.serial.as_ref().is_none_or(|serial| {
                panel.selected_serial.as_ref() == Some(serial) && panel.selected_device().is_ok()
            })
    }
}

struct TestRun {
    request: TestRequest,
    token: ModelToken,
    session: u64,
    revision: u64,
}
type TestOutcome = (
    Vec<TestCase>,
    Vec<TestCase>,
    Result<ProcessOutput>,
    Option<String>,
);

pub struct TestPanel {
    focus_handle: FocusHandle,
    root: Option<PathBuf>,
    target: Option<AndroidTarget>,
    serial: Option<String>,
    model_token: Option<ModelToken>,
    kind: TestKind,
    sources: Vec<TestCase>,
    cases: Vec<TestCase>,
    rows: Vec<TestRow>,
    collapsed: BTreeSet<String>,
    class_status: BTreeMap<String, TestStatus>,
    suite_status: TestStatus,
    selected: TestRow,
    scroll: UniformListScrollHandle,
    pub(super) busy: bool,
    waiting_for_model: bool,
    stale: bool,
    revision: u64,
    last_selectors: Option<Vec<String>>,
    message: SharedString,
}
impl TestPanel {
    pub(super) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            root: None,
            target: None,
            serial: None,
            model_token: None,
            kind: TestKind::Unit,
            sources: Vec::new(),
            cases: Vec::new(),
            rows: vec![TestRow::Suite],
            collapsed: BTreeSet::new(),
            class_status: BTreeMap::new(),
            suite_status: TestStatus::NotRun,
            selected: TestRow::Suite,
            scroll: UniformListScrollHandle::new(),
            busy: false,
            waiting_for_model: false,
            stale: false,
            revision: 0,
            last_selectors: None,
            message: "Discover tests or run the selected variant's suite.".into(),
        }
    }
    pub(super) fn invalidate(&mut self, cx: &mut Context<Self>) {
        if self.root.is_some() {
            self.revision += 1;
            self.stale = true;
            cx.notify();
        }
    }
    fn begin(
        &mut self,
        root: PathBuf,
        target: AndroidTarget,
        kind: TestKind,
        serial: Option<String>,
        selectors: Option<Vec<String>>,
        cx: &mut Context<Self>,
    ) -> u64 {
        if self.root.as_ref() != Some(&root)
            || self.target.as_ref() != Some(&target)
            || self.kind != kind
        {
            self.sources.clear();
            self.collapsed.clear();
        }
        self.root = Some(root);
        self.target = Some(target);
        self.kind = kind;
        self.serial = serial;
        self.model_token = None;
        self.cases.clear();
        self.selected = TestRow::Suite;
        self.last_selectors = selectors;
        self.busy = true;
        self.waiting_for_model = false;
        self.stale = false;
        self.revision += 1;
        self.message = if self.last_selectors.is_some() {
            "Running tests…"
        } else {
            "Discovering tests…"
        }
        .into();
        self.rebuild_rows();
        cx.notify();
        self.revision
    }
    fn replace_cases(&mut self, cases: Vec<TestCase>) {
        let selected = self.selected_case().map(|case| case.id.clone());
        self.cases = cases;
        if let Some(id) = selected {
            self.selected = self
                .cases
                .iter()
                .position(|case| case.id == id)
                .map(TestRow::Case)
                .unwrap_or(TestRow::Suite);
        }
        self.rebuild_rows();
    }
    fn rebuild_rows(&mut self) {
        self.rows = vec![TestRow::Suite];
        self.class_status.clear();
        for case in &self.cases {
            self.class_status
                .entry(case.id.class.clone())
                .and_modify(|status| *status = aggregate(*status, case.status))
                .or_insert(case.status);
        }
        self.suite_status = self
            .class_status
            .values()
            .copied()
            .reduce(aggregate)
            .unwrap_or_default();
        let mut last_class = None;
        for (index, case) in self.cases.iter().enumerate() {
            if last_class != Some(&case.id.class) {
                self.rows.push(TestRow::Class(case.id.class.clone()));
                last_class = Some(&case.id.class);
            }
            if !self.collapsed.contains(&case.id.class) {
                self.rows.push(TestRow::Case(index));
            }
        }
    }
    fn selectors(&self) -> Vec<String> {
        match &self.selected {
            TestRow::Suite => Vec::new(),
            TestRow::Class(class) => vec![class.clone()],
            TestRow::Case(index) => self
                .cases
                .get(*index)
                .map(|case| vec![case.selector(self.kind)])
                .unwrap_or_default(),
        }
    }
    fn failed_selectors(&self) -> Vec<String> {
        self.cases
            .iter()
            .filter(|case| case.status == TestStatus::Failed)
            .map(|case| case.selector(self.kind))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    fn selected_case(&self) -> Option<&TestCase> {
        if let TestRow::Case(index) = self.selected {
            self.cases.get(index)
        } else {
            None
        }
    }
    fn navigate(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let current = self
            .rows
            .iter()
            .position(|row| row == &self.selected)
            .unwrap_or(0);
        let next = match key {
            "down" => current.saturating_add(1),
            "up" => current.saturating_sub(1),
            "home" => 0,
            "end" => self.rows.len().saturating_sub(1),
            "pagedown" => current.saturating_add(10),
            "pageup" => current.saturating_sub(10),
            "left" | "right" => {
                if let TestRow::Class(class) = &self.selected {
                    if key == "left" {
                        self.collapsed.insert(class.clone());
                    } else {
                        self.collapsed.remove(class);
                    }
                    self.rebuild_rows();
                    cx.notify();
                    return true;
                }
                return false;
            }
            _ => return false,
        }
        .min(self.rows.len().saturating_sub(1));
        if let Some(row) = self.rows.get(next) {
            self.selected = row.clone();
            self.scroll.scroll_to_item(next, ScrollStrategy::Center);
            cx.notify();
        }
        true
    }
    fn summary(&self) -> String {
        let count = |status| {
            self.cases
                .iter()
                .filter(|case| case.status == status)
                .count()
        };
        format!(
            "{} passed · {} failed · {} skipped · {} not run",
            count(TestStatus::Passed),
            count(TestStatus::Failed),
            count(TestStatus::Skipped),
            count(TestStatus::NotRun)
        )
    }
}
fn aggregate(left: TestStatus, right: TestStatus) -> TestStatus {
    for status in [
        TestStatus::Failed,
        TestStatus::NotRun,
        TestStatus::Passed,
        TestStatus::Skipped,
    ] {
        if left == status || right == status {
            return status;
        }
    }
    TestStatus::NotRun
}
impl EventEmitter<TestEvent> for TestPanel {}
impl EventEmitter<PanelEvent> for TestPanel {}
impl Focusable for TestPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl Panel for TestPanel {
    fn persistent_name() -> &'static str {
        "Android Tests"
    }
    fn panel_key() -> &'static str {
        "android_tests"
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
        Some(IconName::Check)
    }
    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Android Tests")
    }
    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleTests)
    }
    fn activation_priority(&self) -> u32 {
        4
    }
}
impl Render for TestPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_source = self.selected_case().and_then(|case| case.source.clone());
        let details = self
            .selected_case()
            .map(|case| {
                case.detail
                    .lines()
                    .map(SharedString::from)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let widest_detail = details
            .iter()
            .enumerate()
            .max_by_key(|(_, line)| unicode_width::UnicodeWidthStr::width(line.as_ref()))
            .map(|(index, _)| index);
        let tree = uniform_list(
            "android-test-tree",
            self.rows.len(),
            cx.processor(|panel, range: std::ops::Range<usize>, _, cx| {
                range
                    .filter_map(|index| {
                        let row = panel.rows.get(index)?.clone();
                        let (label, status, depth) = match &row {
                            TestRow::Suite => (
                                panel
                                    .target
                                    .as_ref()
                                    .map(AndroidTarget::label)
                                    .unwrap_or_else(|| "Selected variant".into()),
                                panel.suite_status,
                                0,
                            ),
                            TestRow::Class(class) => {
                                let status =
                                    panel.class_status.get(class).copied().unwrap_or_default();
                                (class.clone(), status, 1)
                            }
                            TestRow::Case(index) => {
                                let case = panel.cases.get(*index)?;
                                (
                                    format!("{} · {} ms", case.id.method, case.duration_ms),
                                    case.status,
                                    2,
                                )
                            }
                        };
                        let (icon, color) = match status {
                            TestStatus::NotRun => (IconName::Circle, Color::Muted),
                            TestStatus::Passed => (IconName::Check, Color::Success),
                            TestStatus::Failed => (IconName::Warning, Color::Error),
                            TestStatus::Skipped => (IconName::Dash, Color::Muted),
                        };
                        let collapse = if let TestRow::Class(class) = &row {
                            Some(class.clone())
                        } else {
                            None
                        };
                        let selected = panel.selected == row;
                        Some(
                            h_flex()
                                .id(("accessible-test-row", index))
                                .role(gpui::Role::TreeItem)
                                .aria_label(format!("{label}, {status:?}"))
                                .when(selected, |row| row.aria_active_descendant())
                                .debug_selector(move || format!("test-tree-row-{index}"))
                                .h_7()
                                .pl(px(8. + depth as f32 * 16.))
                                .pr_2()
                                .gap_1()
                                .when_some(collapse, |element, class| {
                                    let collapsed = panel.collapsed.contains(&class);
                                    element.child(
                                        IconButton::new(
                                            ("collapse-test-class", index),
                                            if collapsed {
                                                IconName::ChevronRight
                                            } else {
                                                IconName::ChevronDown
                                            },
                                        )
                                        .tab_index(0isize)
                                        .aria_label(if collapsed {
                                            "Expand test class"
                                        } else {
                                            "Collapse test class"
                                        })
                                        .on_click(
                                            cx.listener(move |panel, _, _, cx| {
                                                if !panel.collapsed.remove(&class) {
                                                    panel.collapsed.insert(class.clone());
                                                }
                                                panel.rebuild_rows();
                                                cx.notify();
                                            }),
                                        ),
                                    )
                                })
                                .child(
                                    Button::new(("test-row", index), label.clone())
                                        .aria_label(format!("{label}, {status:?}"))
                                        .tab_index(0isize)
                                        .full_width()
                                        .toggle_state(selected)
                                        .start_icon(Icon::new(icon).color(color))
                                        .on_click(cx.listener(move |panel, _, window, cx| {
                                            panel.selected = row.clone();
                                            panel.focus_handle.focus(window, cx);
                                            cx.notify();
                                        })),
                                )
                                .into_any_element(),
                        )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .debug_selector(|| "android-tests-tree".into())
        .track_scroll(&self.scroll)
        .size_full();
        let busy = self.busy || self.waiting_for_model;
        let controls = h_flex()
            .flex_wrap()
            .min_h_9()
            .py_1()
            .px_2()
            .gap_2()
            .child(Label::new("Tests"))
            .children([TestKind::Unit, TestKind::Device].into_iter().map(|kind| {
                Button::new(kind.label(), kind.label())
                    .tab_index(0isize)
                    .toggle_state(self.kind == kind)
                    .disabled(busy)
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(TestEvent::Discover(kind))))
            }))
            .child(
                Button::new("discover-tests", "Discover")
                    .tab_index(0isize)
                    .disabled(busy)
                    .on_click(
                        cx.listener(|panel, _, _, cx| cx.emit(TestEvent::Discover(panel.kind))),
                    ),
            )
            .child(
                Button::new("run-selected-test", "Run selected")
                    .tab_index(0isize)
                    .disabled(busy || self.stale)
                    .on_click(cx.listener(|panel, _, _, cx| {
                        cx.emit(TestEvent::Run(panel.kind, panel.selectors()))
                    })),
            )
            .child(
                Button::new("run-test-suite", "Run suite")
                    .tab_index(0isize)
                    .disabled(busy)
                    .on_click(cx.listener(|panel, _, _, cx| {
                        cx.emit(TestEvent::Run(panel.kind, Vec::new()))
                    })),
            )
            .child(
                IconButton::new("rerun-tests", IconName::RotateCw)
                    .tab_index(0isize)
                    .aria_label("Rerun last selection")
                    .tooltip(Tooltip::text("Rerun last selection"))
                    .disabled(busy || self.stale || self.last_selectors.is_none())
                    .on_click(cx.listener(|panel, _, _, cx| {
                        if let Some(selectors) = &panel.last_selectors {
                            cx.emit(TestEvent::Run(panel.kind, selectors.clone()));
                        }
                    })),
            )
            .child(
                Button::new("rerun-failed-tests", "Rerun failed")
                    .tab_index(0isize)
                    .disabled(
                        busy || self.stale
                            || !self
                                .cases
                                .iter()
                                .any(|case| case.status == TestStatus::Failed),
                    )
                    .on_click(cx.listener(|panel, _, _, cx| {
                        cx.emit(TestEvent::Run(panel.kind, panel.failed_selectors()))
                    })),
            )
            .child(
                IconButton::new("stop-tests", IconName::Stop)
                    .tab_index(0isize)
                    .aria_label("Cancel tests")
                    .tooltip(Tooltip::text("Cancel tests"))
                    .disabled(!busy)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(TestEvent::Stop))),
            )
            .child(div().flex_1())
            .child(
                IconButton::new("hide-tests", IconName::Dash)
                    .tab_index(0isize)
                    .aria_label("Hide test runner")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(PanelEvent::Close))),
            );
        v_flex()
            .id("android-test-panel")
            .key_context("AndroidTests")
            .on_key_down(
                cx.listener(|panel, event: &gpui::KeyDownEvent, window, cx| {
                    if !event.keystroke.modifiers.modified()
                        && panel.navigate(&event.keystroke.key, cx)
                    {
                        panel.focus_handle.focus(window, cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .track_focus(&self.focus_handle)
            .role(gpui::Role::Complementary)
            .aria_label("Android test runner")
            .size_full()
            .min_h_0()
            .bg(cx.theme().colors().panel_background)
            .child(controls)
            .when_some(self.serial.clone(), |panel, serial| {
                panel.child(Label::new(format!("Device: {serial}")).size(LabelSize::Small))
            })
            .child(
                h_flex()
                    .flex_wrap()
                    .px_3()
                    .gap_3()
                    .child(Label::new(self.message.clone()))
                    .child(Label::new(self.summary()).color(Color::Muted))
                    .when(self.stale, |element| {
                        element.child(
                            Label::new("Results are stale — discover or run suite again.")
                                .color(Color::Warning),
                        )
                    }),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .child(div().w(gpui::relative(0.5)).h_full().child(tree))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(
                                Button::new("open-test-source", "Open test / failure source")
                                    .tab_index(0isize)
                                    .disabled(self.stale || selected_source.is_none())
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        if let Some(source) = &selected_source {
                                            cx.emit(TestEvent::Source(source.clone()));
                                        }
                                    })),
                            )
                            .child(
                                uniform_list(
                                    "test-failure-details",
                                    details.len(),
                                    move |range, _, cx| {
                                        range
                                            .filter_map(|index| details.get(index))
                                            .map(|line| {
                                                div()
                                                    .h_6()
                                                    .px_3()
                                                    .font_buffer(cx)
                                                    .whitespace_nowrap()
                                                    .child(
                                                        Label::new(line.clone())
                                                            .size(LabelSize::Small),
                                                    )
                                            })
                                            .collect::<Vec<_>>()
                                    },
                                )
                                .with_horizontal_sizing_behavior(
                                    ListHorizontalSizingBehavior::Unconstrained,
                                )
                                .with_width_from_item(widest_detail)
                                .size_full(),
                            ),
                    ),
            )
    }
}

impl AndroidPanel {
    pub(super) fn rerun_tests(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tests = self.test_panel.read(cx);
        if tests.stale || tests.busy || tests.waiting_for_model {
            return;
        }
        let kind = tests.kind;
        let discover = tests.last_selectors.is_none();
        let selectors = tests.last_selectors.clone().unwrap_or_default();
        self.start_tests(kind, selectors, discover, window, cx);
    }
    pub(super) fn observe_tests(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._startup_subscriptions.push(cx.subscribe_in(
            &self.test_panel,
            window,
            |panel, _, event, window, cx| match event {
                TestEvent::Discover(kind) => panel.start_tests(*kind, Vec::new(), true, window, cx),
                TestEvent::Run(kind, selectors) => {
                    panel.start_tests(*kind, selectors.clone(), false, window, cx)
                }
                TestEvent::Stop => panel.cancel_tests(cx),
                TestEvent::Source(source) => panel.open_test_source(source.clone(), window, cx),
            },
        ));
        self._startup_subscriptions
            .push(cx.observe(&cx.entity(), |panel, _, cx| panel.validate_test_context(cx)));
    }
    pub(super) fn validate_test_context(&mut self, cx: &mut Context<Self>) {
        let tests = self.test_panel.read(cx);
        let valid = self.pending_test.as_ref().map_or_else(
            || {
                tests.root.as_ref().is_none_or(|root| {
                    self.trusted_root(cx).is_ok_and(|current| &current == root)
                        && self.selected_target == tests.target
                        && tests.serial.as_ref().is_none_or(|serial| {
                            self.selected_serial.as_ref() == Some(serial)
                                && self.selected_device().is_ok()
                        })
                })
            },
            |request| request.context_matches(self, cx),
        ) && tests
            .model_token
            .as_ref()
            .is_none_or(|token| self.project.read(cx).android_model().is_current(token));
        if !valid {
            if !tests.stale {
                self.test_panel.update(cx, |tests, cx| tests.invalidate(cx));
            }
            self.cancel_tests(cx);
        }
    }

    pub(super) fn cancel_tests(&mut self, cx: &mut Context<Self>) {
        if let Some(request) = &mut self.pending_test {
            request.cancel_requested = true;
        }
        if let Some(cancel) = self.test_cancel.take() {
            if cancel.send(()).is_err() {
                log::debug!("Android test command already finished");
            }
            self.test_panel.update(cx, |tests, cx| {
                tests.message = "Cancelling tests…".into();
                cx.notify();
            });
        } else if self
            .pending_test
            .as_ref()
            .is_some_and(|request| request.waiting_for_model)
        {
            self.finish_pending_tests("Test request cancelled", cx);
        }
    }

    pub(super) fn start_tests(
        &mut self,
        kind: TestKind,
        selectors: Vec<String>,
        discover_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.running || self.syncing || self.test_operation_id.is_some() {
            return;
        }
        let prepared = (|| {
            let root = self.trusted_root(cx)?;
            let target = self
                .selected_target
                .clone()
                .context("Sync and select an Android build variant first.")?;
            let serial = if kind == TestKind::Device && !discover_only {
                Some(self.selected_device()?.serial.clone())
            } else {
                None
            };
            let model = self.project.read(cx).android_model();
            let initial_token = model
                .selected
                .as_ref()
                .filter(|selected| selected.validate_target(&target).is_ok())
                .map(|_| model.token());
            Ok::<_, anyhow::Error>((root, target, serial, initial_token))
        })();
        let (root, target, serial, initial_token) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.fail(error, window, cx);
                return;
            }
        };
        self.next_operation_id += 1;
        let id = self.next_operation_id;
        self.pending_test = Some(TestRequest {
            id,
            root: root.clone(),
            target: target.clone(),
            kind,
            serial: serial.clone(),
            selectors: selectors.clone(),
            discover_only,
            initial_token,
            inputs_dirty: self.model_inputs_dirty(cx),
            waiting_for_model: false,
            cancel_requested: false,
            saved_revision: None,
        });
        self.test_operation_id = Some(id);
        self.test_panel.update(cx, |tests, cx| {
            tests.begin(
                root,
                target,
                kind,
                serial,
                (!discover_only).then_some(selectors),
                cx,
            );
            tests.message = "Saving files before testing…".into();
            cx.notify();
        });
        self.running = true;
        self.error = None;
        self.status = "Saving files before testing…".into();
        let workspace = self.workspace.clone();
        let reveal_workspace = workspace.clone();
        window.defer(cx, move |window, cx| {
            reveal_workspace
                .update(cx, |workspace, cx| {
                    workspace.reveal_panel::<TestPanel>(window, cx)
                })
                .log_err();
        });
        let (cancel, mut cancelled) = oneshot::channel();
        self.test_cancel = Some(cancel);
        self.test_task = Some(cx.spawn_in(window, async move |panel, cx| {
            let save = async {
                Workspace::save_for_task(&workspace, SaveStrategy::All, cx).await;
                workspace
                    .read_with(cx, |workspace, cx| {
                        !workspace.items(cx).any(|item| item.is_dirty(cx))
                    })
                    .unwrap_or(false)
            }
            .boxed_local();
            let saved = match select(save, &mut cancelled).await {
                Either::Left((saved, _)) => Some(saved),
                Either::Right((_, save)) => {
                    drop(save);
                    None
                }
            };
            panel
                .update_in(cx, |panel, window, cx| {
                    panel.complete_test_save(id, saved, window, cx)
                })
                .log_err();
        }));
        cx.notify();
    }

    fn complete_test_save(
        &mut self,
        id: u64,
        saved: Option<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.test_operation_id != Some(id) {
            return;
        }
        self.test_cancel = None;
        let Some(mut request) = self.pending_test.take() else {
            return;
        };
        let cancelled = saved.is_none() || request.cancel_requested;
        if saved != Some(true) || cancelled {
            self.pending_test = Some(request);
            self.finish_pending_tests(
                if cancelled {
                    "Test request cancelled"
                } else {
                    "Save modified files before testing"
                },
                cx,
            );
            cx.defer_in(window, |panel, window, cx| {
                panel.auto_sync_project(window, cx)
            });
            return;
        }
        let model = self.project.read(cx).android_model();
        let selection_changed = !request.inputs_dirty
            && request
                .initial_token
                .as_ref()
                .is_some_and(|token| model.selected.is_some() && !model.is_current(token));
        if !request.context_matches(self, cx) || selection_changed || self.model_inputs_dirty(cx) {
            self.pending_test = Some(request);
            self.finish_pending_tests(
                "The test selection changed or project inputs remain unsaved; select and run again",
                cx,
            );
            cx.defer_in(window, |panel, window, cx| {
                panel.auto_sync_project(window, cx)
            });
            return;
        }
        let needs_sync = request.inputs_dirty
            || request
                .initial_token
                .as_ref()
                .is_none_or(|token| !model.is_current(token));
        request.saved_revision = Some(self.test_panel.update(cx, |tests, cx| {
            tests.stale = false;
            cx.notify();
            tests.revision
        }));
        self.running = false;
        self.test_panel.update(cx, |tests, cx| {
            tests.busy = false;
            tests.waiting_for_model = true;
            tests.message = "Waiting for the selected Android project model…".into();
            cx.notify();
        });
        request.waiting_for_model = true;
        self.pending_test = Some(request);
        if needs_sync {
            self.kotlin_refresh_task = None;
            self.kotlin_refresh_pending = None;
            self.sync_project(window, cx);
        } else {
            self.resume_pending_tests(window, cx);
        }
        cx.notify();
    }

    pub(super) fn resume_pending_tests(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running
            || self.syncing
            || self.kotlin_refresh_task.is_some()
            || self.kotlin_refresh_pending.is_some()
        {
            return;
        }
        let Some(request) = self
            .pending_test
            .as_ref()
            .filter(|request| request.waiting_for_model)
        else {
            return;
        };
        if !request.context_matches(self, cx) {
            self.finish_pending_tests(
                "The requested test variant or device is no longer selected",
                cx,
            );
            return;
        }
        if self.model_inputs_dirty(cx) || self.test_sources_dirty(&request.root, cx) {
            self.finish_pending_tests(
                "Files changed while waiting for the Android model; save and run again",
                cx,
            );
            return;
        }
        let prepared = (|| {
            let target = self
                .selected_target
                .as_ref()
                .context("Select an Android variant after syncing")?;
            let state = self.project.read(cx).android_model();
            let selected = state
                .selected
                .as_ref()
                .context("Android sync did not publish a selected model; sync and try again")?;
            let sources = testing::component_sources(selected, target, request.kind)?;
            Ok::<_, anyhow::Error>((state.token(), target.clone(), sources))
        })();
        let (token, target, sources) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.finish_pending_tests(&format!("Cannot start tests: {error:#}"), cx);
                return;
            }
        };
        let Some(mut request) = self.pending_test.take() else {
            return;
        };
        request.target = target;
        self.launch_tests(request, token, sources, window, cx);
    }

    fn finish_pending_tests(&mut self, message: &str, cx: &mut Context<Self>) {
        let saving = self
            .pending_test
            .as_ref()
            .is_some_and(|request| !request.waiting_for_model);
        self.pending_test = None;
        self.test_operation_id = None;
        self.test_cancel = None;
        if saving
            && self.active_build_session.is_none()
            && self.active_operation_id.is_none()
            && self.followup_model_token.is_none()
        {
            self.running = false;
            self.status = message.to_owned().into();
        }
        self.test_panel.update(cx, |tests, cx| {
            tests.busy = false;
            tests.waiting_for_model = false;
            tests.stale = true;
            tests.message = message.to_owned().into();
            cx.notify();
        });
        cx.notify();
    }

    fn test_sources_dirty(&self, root: &Path, cx: &App) -> bool {
        self.project
            .read(cx)
            .buffer_store()
            .read(cx)
            .buffers()
            .any(|buffer| {
                let buffer = buffer.read(cx);
                buffer.is_dirty()
                    && buffer.file().is_some_and(|file| {
                        self.project
                            .read(cx)
                            .worktree_for_id(file.worktree_id(cx), cx)
                            .is_some_and(|worktree| worktree.read(cx).abs_path().as_ref() == root)
                    })
            })
    }

    fn launch_tests(
        &mut self,
        request: TestRequest,
        token: ModelToken,
        sources: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prepared = (|| {
            let directory = tempfile::Builder::new().prefix("koda-tests-").tempdir()?;
            std::fs::write(directory.path().join("runner.gradle"), testing::INIT_SCRIPT)?;
            let arguments = testing::arguments(
                &request.target,
                request.kind,
                directory.path(),
                &request.selectors,
            )?;
            Ok::<_, anyhow::Error>((directory, arguments))
        })();
        let (directory, arguments) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.finish_pending_tests(&format!("Cannot prepare tests: {error:#}"), cx);
                return;
            }
        };
        let environment =
            self.project
                .read(cx)
                .environment()
                .clone()
                .update(cx, |environment, cx| {
                    environment.local_directory_environment(
                        &task::Shell::Program(util::get_system_shell()),
                        Arc::from(request.root.as_path()),
                        cx,
                    )
                });
        let terminal_environment = self
            .project
            .read(cx)
            .terminal_settings(&Some(request.root.clone()), cx)
            .env
            .clone();
        let Some(revision) = request.saved_revision else {
            self.finish_pending_tests("Save files before starting tests", cx);
            return;
        };
        self.test_panel.update(cx, |tests, cx| {
            tests.busy = true;
            tests.waiting_for_model = false;
            tests.stale |= tests.revision != revision;
            tests.model_token = Some(token.clone());
            tests.target = Some(request.target.clone());
            tests.message = if request.discover_only {
                "Discovering tests…"
            } else {
                "Running tests…"
            }
            .into();
            cx.notify();
        });
        let (session, output, logs) = self.build_panel.update(cx, |panel, cx| {
            panel.begin(
                BuildTab::Output,
                format!("{} · {}", request.kind.label(), request.target.label()),
                false,
                window,
                cx,
            )
        });
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.reveal_panel::<TestPanel>(window, cx)
                })
                .log_err();
        });
        let (cancel, mut cancelled) = oneshot::channel();
        self.test_cancel = Some(cancel);
        self.active_build_session = Some((BuildTab::Output, session));
        self.status = format!("{} · {}", request.kind.label(), request.target.label()).into();
        self.last_build_operation = Some(GradleOperation::Test);
        self.running = true;
        let executor = cx.background_executor().clone();
        let (progress, mut progress_receiver) = futures::channel::mpsc::channel(1);
        let id = request.id;
        let progress_token = token.clone();
        let progress_task = cx.spawn(async move |panel, cx| {
            while let Some(cases) = progress_receiver.next().await {
                panel
                    .update(cx, |panel, cx| {
                        panel.publish_test_progress(id, &progress_token, cases, cx);
                    })
                    .log_err();
            }
        });
        self.test_task = Some(cx.spawn_in(window, async move |panel, cx| {
            let prepare_environment = async {
                let mut environment = environment.await.unwrap_or_default();
                environment.extend(terminal_environment);
                if let Some(serial) = &request.serial {
                    environment.insert("ANDROID_SERIAL".into(), serial.clone());
                }
                environment
            }
            .boxed_local();
            let environment = match select(prepare_environment, &mut cancelled).await {
                Either::Left((environment, _)) => Some(environment),
                Either::Right((_, environment)) => {
                    drop(environment);
                    None
                }
            };
            let valid = panel
                .read_with(cx, |panel, cx| {
                    panel.test_operation_id == Some(id)
                        && panel.test_cancel.is_some()
                        && request.context_matches(panel, cx)
                        && panel.project.read(cx).android_model().is_current(&token)
                        && !panel.model_inputs_dirty(cx)
                        && !panel.test_sources_dirty(&request.root, cx)
                })
                .unwrap_or(false);
            let outcome = if valid && let Some(environment) = environment {
                cx.background_spawn(execute_tests(
                    TestExecution {
                        root: request.root.clone(),
                        environment,
                        directory,
                        sources,
                        run: arguments,
                        discover_only: request.discover_only,
                    },
                    executor,
                    output,
                    cancelled,
                    progress,
                ))
                .await
            } else {
                drop(output);
                drop(progress);
                (Vec::new(), Vec::new(), Ok(ProcessOutput::Cancelled), None)
            };
            progress_task.await;
            logs.await;
            panel
                .update_in(cx, |panel, window, cx| {
                    if panel.finish_test_run(
                        TestRun {
                            request,
                            token,
                            session,
                            revision,
                        },
                        outcome,
                        cx,
                    ) {
                        cx.defer_in(window, |panel, window, cx| {
                            panel.auto_sync_project(window, cx)
                        });
                    }
                })
                .log_err();
        }));
        cx.notify();
    }

    fn finish_test_run(
        &mut self,
        run: TestRun,
        outcome: TestOutcome,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.test_operation_id != Some(run.request.id) {
            return false;
        }
        let current = run.request.context_matches(self, cx)
            && self.project.read(cx).android_model().is_current(&run.token);
        let (sources, results, result, report_error) = outcome;
        self.test_cancel = None;
        self.test_operation_id = None;
        let (status, mut message) = match result {
            _ if !current => (BuildStatus::Cancelled, "The Android test model or selection changed; previous completed results retained as stale".to_owned()),
            Ok(ProcessOutput::Cancelled) => (BuildStatus::Cancelled, "Test run cancelled; completed results retained".to_owned()),
            Err(error) => (BuildStatus::Failed, format!("Test process failed: {error:#}. See Build Output.")),
            Ok(_) if report_error.is_some() => (BuildStatus::Failed, "Test reports are incomplete".into()),
            Ok(_) if run.request.discover_only => (BuildStatus::Succeeded, format!("Discovered {} tests. Select a method, class, or run suite.", sources.len())),
            Ok(_) if results.is_empty() => (BuildStatus::Failed, "No test results were produced. Check the test task, runner and Build Output.".into()),
            Ok(_) if results.iter().any(|case| case.status == TestStatus::Failed) => (BuildStatus::Failed, "Tests failed".into()),
            Ok(_) => (BuildStatus::Succeeded, "Tests completed".into()),
        };
        if current && let Some(report_error) = report_error {
            message.push_str(&format!("\nPartial results: {report_error}"));
        }
        if self.active_build_session == Some((BuildTab::Output, run.session)) {
            self.active_build_session = None;
            self.running = false;
            self.status = message.clone().into();
        }
        self.build_panel.update(cx, |build, cx| {
            build.finish(BuildTab::Output, run.session, status, message.clone(), cx)
        });
        self.test_panel.update(cx, |tests, cx| {
            tests.busy = false;
            tests.waiting_for_model = false;
            tests.stale |= !current || tests.revision != run.revision;
            if current {
                tests.sources = sources;
                tests.replace_cases(if run.request.discover_only {
                    tests.sources.clone()
                } else {
                    results
                });
            }
            tests.message = message.into();
            cx.notify();
        });
        cx.notify();
        true
    }

    fn publish_test_progress(
        &mut self,
        id: u64,
        token: &ModelToken,
        cases: Vec<TestCase>,
        cx: &mut Context<Self>,
    ) {
        if self.test_operation_id == Some(id)
            && self.project.read(cx).android_model().is_current(token)
            && self.test_panel.read(cx).model_token.as_ref() == Some(token)
        {
            self.test_panel.update(cx, |tests, cx| {
                tests.replace_cases(cases);
                cx.notify();
            });
        }
    }

    fn open_test_source(
        &self,
        source: SourceLocation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.test_panel.read(cx).stale {
            return;
        }
        let workspace = self.workspace.clone();
        let project_path =
            Workspace::project_path_for_path(self.project.clone(), &source.path, false, cx);
        cx.spawn_in(window, async move |_, cx| {
            let (_, path) = project_path.await?;
            let task = workspace.update_in(cx, |workspace, window, cx| {
                workspace.open_path(path, None, true, window, cx)
            })?;
            let item = task.await?;
            if let Some(editor) = item.downcast::<editor::Editor>() {
                editor.update_in(cx, |editor, window, cx| {
                    let point = language::Point::new(source.line.saturating_sub(1), 0);
                    editor.change_selections(Default::default(), window, cx, |selections| {
                        selections.select_ranges([point..point])
                    });
                })?;
            }
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }
}

struct TestExecution {
    root: PathBuf,
    environment: collections::HashMap<String, String>,
    directory: tempfile::TempDir,
    sources: Vec<PathBuf>,
    run: Vec<String>,
    discover_only: bool,
}
async fn execute_tests(
    execution: TestExecution,
    executor: BackgroundExecutor,
    output: futures::channel::mpsc::Sender<android_build::OutputLine>,
    mut cancelled: oneshot::Receiver<()>,
    mut progress: futures::channel::mpsc::Sender<Vec<TestCase>>,
) -> (
    Vec<TestCase>,
    Vec<TestCase>,
    Result<ProcessOutput>,
    Option<String>,
) {
    if !matches!(cancelled.try_recv(), Ok(None)) {
        return (Vec::new(), Vec::new(), Ok(ProcessOutput::Cancelled), None);
    }
    let command = |arguments: Vec<String>| {
        let mut command = if cfg!(windows) {
            util::command::new_std_command(execution.root.join("gradlew.bat"))
        } else {
            let mut command = util::command::new_std_command("/bin/sh");
            command.arg("./gradlew");
            command
        };
        command
            .args(arguments)
            .current_dir(&execution.root)
            .envs(&execution.environment);
        command
    };
    let mut sources = Vec::new();
    let operation = async {
        sources = testing::discover(&execution.sources)?;
        if execution.discover_only {
            return Ok(ProcessOutput::Success(String::new()));
        }
        let (_keep_cancel, never_cancel) = oneshot::channel();
        let mut run = android_build::command_output(
            command(execution.run.clone()),
            &executor,
            Duration::from_secs(3600),
            output.clone(),
            never_cancel,
            false,
        )
        .boxed();
        let mut last_count = 0;
        loop {
            match select(run, executor.timer(Duration::from_millis(300)).boxed()).await {
                Either::Left((result, _)) => return result,
                Either::Right((_, pending)) => {
                    run = pending;
                    let snapshot = testing::read_results(execution.directory.path(), &sources);
                    if snapshot.cases.len() != last_count {
                        let count = snapshot.cases.len();
                        match progress.try_send(snapshot.cases) {
                            Ok(()) => last_count = count,
                            Err(error) if error.is_full() => {}
                            Err(_) => log::debug!("Android test progress view closed"),
                        }
                    }
                }
            }
        }
    }
    .boxed();
    let result = match select(operation, &mut cancelled).await {
        Either::Left((result, _)) => result,
        Either::Right((_, operation)) => {
            drop(operation);
            Ok(ProcessOutput::Cancelled)
        }
    };
    let reports = testing::read_results(execution.directory.path(), &sources);
    let report_error = (!reports.diagnostics.is_empty()).then(|| reports.diagnostics.join("\n"));
    (sources, reports.cases, result, report_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn runner_fixture(
        cx: &mut gpui::TestAppContext,
    ) -> (
        Arc<workspace::AppState>,
        Entity<AndroidPanel>,
        &mut gpui::VisualTestContext,
    ) {
        let state = cx.update(|cx| {
            let state = workspace::AppState::test(cx);
            editor::init(cx);
            state
        });
        let filesystem = project::FakeFs::new(cx.executor());
        filesystem.insert_tree("/android", serde_json::json!({"settings.gradle.kts":"", "build.gradle.kts":"// original", "gradlew":"", "app":{"src":{"test":{"java":{"Tests.kt":"class Tests {}"}}}}})).await;
        let project = Project::test(filesystem, [Path::new("/android")], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        panel.update(cx, |panel, _| {
            panel.root = Some("/android".into());
            let target = AndroidTarget {
                module: ":app".into(),
                variant: "debug".into(),
                output_listing: "/android/output.json".into(),
            };
            panel.targets = vec![target.clone()];
            panel.selected_target = Some(target);
            panel.selected_serial = Some("selected".into());
            panel.devices = parse_devices("List of devices attached\nselected device\n").unwrap();
        });
        (state, panel, cx)
    }

    fn publish_runner_model(
        panel: &mut AndroidPanel,
        cx: &mut Context<AndroidPanel>,
    ) -> ModelToken {
        let root = panel.root.clone().unwrap();
        let target = panel.selected_target.clone().unwrap();
        let variants = ["debug", "release"].map(|name| serde_json::json!({
            "name":name,"outputListing":target.output_listing,"components":[
                {"name":name,"scope":"main","sources":[],"dependencies":[]},
                {"name":format!("{name}UnitTest"),"scope":"unitTest","sources":[],"dependencies":[]},
                {"name":format!("{name}AndroidTest"),"scope":"androidTest","sources":[],"dependencies":[]}
            ]
        }));
        let model = serde_json::from_value(serde_json::json!({"version":1,"root":root,"diagnostics":[],"modules":[{"path":target.module,"directory":root.join("app"),"kind":"application","variants":variants}]})).unwrap();
        panel.project.update(cx, |project, cx| {
            let token = project.invalidate_android_model(Some(root), cx);
            project.publish_android_model(&token, model, cx).unwrap();
            project
                .select_android_variant(
                    Some(android_tools::project_model::VariantId::from(&target)),
                    cx,
                )
                .unwrap();
            project.android_model().token()
        })
    }

    fn request(panel: &AndroidPanel, id: u64, cx: &App) -> TestRequest {
        TestRequest {
            id,
            root: panel.root.clone().unwrap(),
            target: panel.selected_target.clone().unwrap(),
            kind: TestKind::Unit,
            serial: None,
            selectors: vec!["dev.Tests.method".into()],
            discover_only: false,
            initial_token: panel
                .project
                .read(cx)
                .android_model()
                .selected
                .as_ref()
                .map(|_| panel.project.read(cx).android_model().token()),
            inputs_dirty: false,
            waiting_for_model: false,
            cancel_requested: false,
            saved_revision: None,
        }
    }

    fn result(name: &str) -> TestCase {
        TestCase {
            id: testing::TestId {
                class: "dev.Tests".into(),
                method: name.into(),
            },
            status: TestStatus::Passed,
            duration_ms: 1,
            detail: String::new(),
            source: None,
            parameterized: false,
        }
    }

    #[gpui::test]
    async fn missing_snapshot_waits_for_sync_and_preserves_device_method_request(
        cx: &mut gpui::TestAppContext,
    ) {
        let (_state, panel, cx) = runner_fixture(cx).await;
        panel.update_in(cx, |panel, window, cx| {
            let mut request = request(panel, 1, cx);
            request.kind = TestKind::Device;
            request.serial = Some("selected".into());
            request.selectors = vec!["dev.Tests#method".into()];
            panel.test_panel.update(cx, |tests, cx| {
                tests.begin(
                    request.root.clone(),
                    request.target.clone(),
                    request.kind,
                    request.serial.clone(),
                    Some(request.selectors.clone()),
                    cx,
                );
            });
            panel.pending_test = Some(request.clone());
            panel.test_operation_id = Some(1);
            panel.running = true;
            panel.complete_test_save(1, Some(true), window, cx);
            assert!(panel.syncing);
            assert!(panel.sync_task.is_some());
            assert!(panel.pending_test.as_ref().unwrap().waiting_for_model);
            assert!(panel.test_panel.read(cx).model_token.is_none());
            assert!(!panel.test_panel.read(cx).busy);
            panel.sync_task = None;
            panel.command_cancel = None;
            panel.active_build_session = None;
            panel.syncing = false;
            let token = publish_runner_model(panel, cx);
            panel.resume_pending_tests(window, cx);
            let tests = panel.test_panel.read(cx);
            assert_eq!(tests.model_token.as_ref(), Some(&token));
            assert_eq!(tests.kind, TestKind::Device);
            assert_eq!(tests.serial.as_deref(), Some("selected"));
            assert_eq!(tests.last_selectors.as_ref().unwrap(), &request.selectors);
            assert!(panel.pending_test.is_none());
            assert!(tests.busy);
            panel.cancel_tests(cx);
            panel.test_task = None;
            let session = panel.active_build_session.unwrap().1;
            let revision = panel.test_panel.read(cx).revision;
            panel.finish_test_run(
                TestRun {
                    request,
                    token,
                    session,
                    revision,
                },
                (Vec::new(), Vec::new(), Ok(ProcessOutput::Cancelled), None),
                cx,
            );
            assert!(panel.test_operation_id.is_none());
            panel.start_tests(TestKind::Unit, Vec::new(), true, window, cx);
        });
        cx.run_until_parked();
        panel.read_with(cx, |panel, cx| {
            assert!(panel.test_operation_id.is_none());
            assert!(!panel.test_panel.read(cx).busy);
            assert!(!panel.test_panel.read(cx).stale);
            assert!(panel.test_panel.read(cx).message.starts_with("Discovered"));
        });
    }

    #[gpui::test]
    async fn dirty_model_inputs_save_before_test_sync_and_never_launch_old_snapshot(
        cx: &mut gpui::TestAppContext,
    ) {
        let (_state, panel, cx) = runner_fixture(cx).await;
        let project = panel.read_with(cx, |panel, _| panel.project.clone());
        let buffer = project
            .update(cx, |project, cx| {
                project.open_local_buffer("/android/build.gradle.kts", cx)
            })
            .await
            .unwrap();
        panel.update_in(cx, |panel, window, cx| {
            panel
                .workspace
                .update(cx, |workspace, cx| {
                    let editor = cx.new(|cx| {
                        editor::Editor::for_buffer(
                            buffer.clone(),
                            Some(project.clone()),
                            window,
                            cx,
                        )
                    });
                    workspace.add_item(
                        workspace.active_pane().clone(),
                        Box::new(editor),
                        None,
                        true,
                        true,
                        window,
                        cx,
                    );
                })
                .unwrap();
            publish_runner_model(panel, cx);
            buffer.update(cx, |buffer, cx| {
                buffer.edit([(0..0, "// change\n")], None, cx)
            });
            assert!(panel.model_inputs_dirty(cx));
            panel.start_tests(
                TestKind::Device,
                vec!["dev.Tests#method".into()],
                false,
                window,
                cx,
            );
            assert!(panel.pending_test.as_ref().unwrap().inputs_dirty);
            assert!(panel.active_build_session.is_none());
        });
        cx.run_until_parked();
        assert!(!buffer.read_with(cx, |buffer, _| buffer.is_dirty()));
        panel.read_with(cx, |panel, cx| {
            assert!(
                panel.sync_task.is_some(),
                "Saved model inputs must sync before a test task starts"
            );
            assert!(
                panel.test_panel.read(cx).model_token.is_none(),
                "An old snapshot must not launch tests"
            );
            assert!(panel.last_build_operation.is_none());
        });
    }

    #[gpui::test]
    async fn edits_during_model_wait_reject_dirty_sources_and_gradle_inputs(
        cx: &mut gpui::TestAppContext,
    ) {
        let (_state, panel, cx) = runner_fixture(cx).await;
        let project = panel.read_with(cx, |panel, _| panel.project.clone());
        for path in [
            "/android/app/src/test/java/Tests.kt",
            "/android/build.gradle.kts",
        ] {
            let buffer = project
                .update(cx, |project, cx| project.open_local_buffer(path, cx))
                .await
                .unwrap();
            panel.update_in(cx, |panel, window, cx| {
                publish_runner_model(panel, cx);
                let mut request = request(panel, 1, cx);
                let revision = panel.test_panel.update(cx, |tests, cx| {
                    tests.begin(
                        request.root.clone(),
                        request.target.clone(),
                        TestKind::Unit,
                        None,
                        None,
                        cx,
                    )
                });
                request.saved_revision = Some(revision);
                request.waiting_for_model = true;
                panel.pending_test = Some(request);
                panel.test_operation_id = Some(1);
                buffer.update(cx, |buffer, cx| {
                    buffer.edit([(0..0, "// late edit\n")], None, cx)
                });
                if path.ends_with("gradle.kts") {
                    assert!(panel.model_inputs_dirty(cx));
                } else {
                    assert!(panel.test_sources_dirty(Path::new("/android"), cx));
                }
                panel.resume_pending_tests(window, cx);
                assert!(panel.pending_test.is_none());
                assert!(panel.test_operation_id.is_none());
                assert!(panel.active_build_session.is_none());
                assert!(panel.test_panel.read(cx).stale);
                assert!(
                    panel
                        .test_panel
                        .read(cx)
                        .message
                        .starts_with("Files changed")
                );
                buffer.update(cx, |buffer, cx| buffer.undo(cx));
            });
        }
    }

    #[gpui::test]
    async fn post_save_revision_stays_stale_through_discovery(cx: &mut gpui::TestAppContext) {
        let (_state, panel, cx) = runner_fixture(cx).await;
        panel.update_in(cx, |panel, window, cx| {
            let token = publish_runner_model(panel, cx);
            let mut request = request(panel, 1, cx);
            request.discover_only = true;
            request.saved_revision = Some(panel.test_panel.update(cx, |tests, cx| {
                tests.begin(
                    request.root.clone(),
                    request.target.clone(),
                    TestKind::Unit,
                    None,
                    None,
                    cx,
                )
            }));
            panel
                .test_panel
                .update(cx, |tests, cx| tests.invalidate(cx));
            panel.test_operation_id = Some(1);
            panel.launch_tests(request, token, Vec::new(), window, cx);
            assert!(panel.test_panel.read(cx).stale);
        });
        cx.run_until_parked();
        panel.read_with(cx, |panel, cx| {
            assert!(panel.test_operation_id.is_none());
            assert!(panel.test_panel.read(cx).stale);
        });
    }

    #[gpui::test]
    async fn selection_a_b_a_during_save_rejects_pending_request(cx: &mut gpui::TestAppContext) {
        let (_state, panel, cx) = runner_fixture(cx).await;
        panel.update_in(cx, |panel, window, cx| {
            publish_runner_model(panel, cx);
            panel.pending_test = Some(request(panel, 1, cx));
            panel.test_operation_id = Some(1);
            panel.running = true;
            panel.project.update(cx, |project, cx| {
                for variant in ["release", "debug"] {
                    project
                        .select_android_variant(
                            Some(android_tools::project_model::VariantId {
                                module: ":app".into(),
                                variant: variant.into(),
                            }),
                            cx,
                        )
                        .unwrap();
                }
            });
            panel.complete_test_save(1, Some(true), window, cx);
            assert!(panel.test_operation_id.is_none());
            assert!(panel.sync_task.is_none());
            assert!(panel.active_build_session.is_none());
            assert!(!panel.running);
        });
    }

    #[gpui::test]
    async fn selection_generation_rejects_stale_reports_and_callbacks_after_a_b_a_and_rerun(
        cx: &mut gpui::TestAppContext,
    ) {
        let (_state, panel, cx) = runner_fixture(cx).await;
        panel.update_in(cx, |panel, window, cx| {
            let token = publish_runner_model(panel, cx);
            let request = request(panel, 1, cx);
            panel.test_panel.update(cx, |tests, cx| {
                tests.begin(
                    request.root.clone(),
                    request.target.clone(),
                    TestKind::Unit,
                    None,
                    Some(request.selectors.clone()),
                    cx,
                );
                tests.model_token = Some(token.clone());
                tests.replace_cases(vec![result("beforeChange")]);
            });
            let (session, _, _) = panel.build_panel.update(cx, |build, cx| {
                build.begin(BuildTab::Output, "Tests".into(), false, window, cx)
            });
            let run = TestRun {
                request: request.clone(),
                token: token.clone(),
                session,
                revision: panel.test_panel.read(cx).revision,
            };
            panel.test_operation_id = Some(1);
            panel.active_build_session = Some((BuildTab::Output, session));
            panel.running = true;
            let (cancel, mut cancelled) = oneshot::channel();
            panel.test_cancel = Some(cancel);
            panel.project.update(cx, |project, cx| {
                project
                    .select_android_variant(
                        Some(android_tools::project_model::VariantId {
                            module: ":app".into(),
                            variant: "release".into(),
                        }),
                        cx,
                    )
                    .unwrap();
                project
                    .select_android_variant(
                        Some(android_tools::project_model::VariantId::from(
                            &request.target,
                        )),
                        cx,
                    )
                    .unwrap();
            });
            panel.publish_test_progress(1, &token, vec![result("lateOldResult")], cx);
            assert_eq!(panel.test_panel.read(cx).cases[0].id.method, "beforeChange");
            panel.validate_test_context(cx);
            assert_eq!(cancelled.try_recv().unwrap(), Some(()));
            assert!(panel.test_panel.read(cx).stale);
            panel.finish_test_run(
                run,
                (
                    Vec::new(),
                    vec![result("lateFinal")],
                    Ok(ProcessOutput::Success(String::new())),
                    None,
                ),
                cx,
            );
            assert_eq!(panel.test_panel.read(cx).cases[0].id.method, "beforeChange");
            let new_token = panel.project.read(cx).android_model().token();
            panel.test_operation_id = Some(2);
            panel.active_build_session = Some((BuildTab::Output, session + 1));
            panel.running = true;
            panel.status = "New run".into();
            panel.test_panel.update(cx, |tests, _| {
                tests.busy = true;
                tests.model_token = Some(new_token.clone());
            });
            panel.publish_test_progress(2, &new_token, vec![result("newResult")], cx);
            let old = TestRun {
                request,
                token,
                session,
                revision: 0,
            };
            assert!(!panel.finish_test_run(
                old,
                (Vec::new(), Vec::new(), Ok(ProcessOutput::Cancelled), None),
                cx
            ));
            panel.complete_test_save(1, Some(true), window, cx);
            assert!(panel.running);
            assert_eq!(panel.status.as_ref(), "New run");
            assert_eq!(panel.test_operation_id, Some(2));
            assert_eq!(panel.test_panel.read(cx).cases[0].id.method, "newResult");
            panel.publish_test_progress(1, &new_token, vec![result("oldRunSameGeneration")], cx);
            assert_eq!(panel.test_panel.read(cx).cases[0].id.method, "newResult");
        });
    }

    #[gpui::test]
    async fn repeated_cancel_preserves_save_and_shared_sync_ownership(
        cx: &mut gpui::TestAppContext,
    ) {
        let (_state, panel, cx) = runner_fixture(cx).await;
        panel.update_in(cx, |panel, window, cx| {
            publish_runner_model(panel, cx);
            let request = request(panel, 1, cx);
            panel.pending_test = Some(request.clone());
            panel.test_operation_id = Some(1);
            panel.running = true;
            panel.test_panel.update(cx, |tests, cx| {
                tests.begin(
                    request.root.clone(),
                    request.target.clone(),
                    TestKind::Unit,
                    None,
                    Some(request.selectors.clone()),
                    cx,
                );
            });
            let (cancel, mut cancelled) = oneshot::channel();
            panel.test_cancel = Some(cancel);
            panel.cancel_tests(cx);
            panel.cancel_tests(cx);
            assert_eq!(cancelled.try_recv().unwrap(), Some(()));
            assert_eq!(panel.test_operation_id, Some(1));
            assert!(panel.test_panel.read(cx).busy);
            panel.complete_test_save(1, Some(true), window, cx);
            assert!(panel.test_operation_id.is_none());
            assert!(!panel.running);
            let mut request = request;
            request.id = 2;
            request.waiting_for_model = true;
            panel.pending_test = Some(request);
            panel.test_operation_id = Some(2);
            panel.running = true;
            panel.syncing = true;
            panel.active_build_session = Some((BuildTab::Sync, 9));
            panel.status = "Shared model sync".into();
            panel.test_panel.update(cx, |tests, _| {
                tests.waiting_for_model = true;
            });
            panel.cancel_build(BuildTab::Output, cx);
            panel.cancel_build(BuildTab::Output, cx);
            assert!(panel.pending_test.is_none());
            assert!(panel.running);
            assert!(panel.syncing);
            assert_eq!(panel.active_build_session, Some((BuildTab::Sync, 9)));
            assert_eq!(panel.status.as_ref(), "Shared model sync");
        });
    }

    #[gpui::test]
    async fn context_changes_cancel_even_when_results_are_already_stale(
        cx: &mut gpui::TestAppContext,
    ) {
        let _app_state = cx.update(workspace::AppState::test);
        let filesystem = project::FakeFs::new(cx.executor());
        filesystem
            .insert_tree(
                "/android",
                serde_json::json!({"settings.gradle.kts": "", "gradlew": ""}),
            )
            .await;
        let project = Project::test(filesystem, [Path::new("/android")], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        panel.update(cx, |panel, cx| {
            let target = AndroidTarget {
                module: ":app".into(),
                variant: "debug".into(),
                output_listing: PathBuf::new(),
            };
            for change in ["variant", "offline", "device", "root"] {
                panel.root = Some("/android".into());
                panel.selected_target = Some(target.clone());
                panel.selected_serial = Some("selected".into());
                panel.devices = parse_devices("List of devices attached\nselected device\n")
                    .expect("Valid devices");
                panel.test_panel.update(cx, |tests, cx| {
                    tests.begin(
                        "/android".into(),
                        target.clone(),
                        TestKind::Device,
                        Some("selected".into()),
                        Some(Vec::new()),
                        cx,
                    );
                    tests.invalidate(cx);
                });
                let (cancel, mut cancelled) = oneshot::channel();
                panel.test_cancel = Some(cancel);
                panel.test_operation_id = Some(1);
                panel.validate_test_context(cx);
                assert!(panel.test_cancel.is_some(), "The captured context is valid");
                match change {
                    "variant" => panel.selected_target = None,
                    "offline" => panel.devices.clear(),
                    "device" => panel.selected_serial = Some("replacement".into()),
                    "root" => panel.root = Some("/closed-project".into()),
                    _ => unreachable!(),
                }
                panel.validate_test_context(cx);
                assert_eq!(
                    cancelled.try_recv().expect("Cancellation delivered"),
                    Some(())
                );
                assert!(panel.test_cancel.is_none());
                assert!(panel.test_panel.read(cx).stale);
                panel.running = true;
                panel.active_build_session = Some((BuildTab::Output, 1));
                panel.cancel_build(BuildTab::Output, cx);
                assert!(panel.running, "Repeated cancellation awaits runner cleanup");
                assert!(panel.active_build_session.is_some());
            }
        });
    }

    #[gpui::test]
    fn test_pane_filters_reruns_and_invalidates_results(cx: &mut gpui::TestAppContext) {
        let panel = cx.new(TestPanel::new);
        panel.update(cx, |panel, cx| {
            let target = AndroidTarget {
                module: ":app".into(),
                variant: "debug".into(),
                output_listing: PathBuf::new(),
            };
            panel.begin(
                "/project".into(),
                target,
                TestKind::Unit,
                None,
                Some(Vec::new()),
                cx,
            );
            panel.cases = vec![TestCase {
                id: testing::TestId {
                    class: "dev.Example".into(),
                    method: "parameter[0]".into(),
                },
                status: TestStatus::Failed,
                duration_ms: 1,
                detail: "Failure".into(),
                source: None,
                parameterized: false,
            }];
            panel.rebuild_rows();
            assert_eq!(panel.suite_status, TestStatus::Failed);
            assert_eq!(panel.rows.len(), 3);
            assert!(panel.selectors().is_empty());
            panel.selected = TestRow::Class("dev.Example".into());
            assert_eq!(panel.selectors(), ["dev.Example"]);
            panel.selected = TestRow::Case(0);
            assert_eq!(panel.selectors(), ["dev.Example"]);
            assert_eq!(panel.failed_selectors(), ["dev.Example"]);
            let selected = panel.cases[0].clone();
            let earlier = TestCase {
                id: testing::TestId {
                    class: "a.Earlier".into(),
                    method: "test".into(),
                },
                ..selected.clone()
            };
            panel.replace_cases(vec![earlier, selected.clone()]);
            assert_eq!(panel.selected_case().unwrap().id, selected.id);
            panel.invalidate(cx);
            assert!(panel.stale);
            panel.collapsed.insert("dev.Example".into());
            panel.rebuild_rows();
            assert_eq!(panel.rows.len(), 4);
        });
    }

    #[gpui::test]
    fn test_pane_renders_narrow_layout_and_navigates_virtualized_results(
        cx: &mut gpui::TestAppContext,
    ) {
        let _app_state = cx.update(workspace::AppState::test);
        let (panel, cx) = cx.add_window_view(|_, cx| TestPanel::new(cx));
        panel.update_in(cx, |panel, window, cx| {
            panel.cases = (0..2000)
                .map(|index| TestCase {
                    id: testing::TestId {
                        class: "dev.Tests".into(),
                        method: format!("test{index}"),
                    },
                    status: TestStatus::Passed,
                    duration_ms: 1,
                    source: None,
                    parameterized: false,
                    detail: "日本語 long failure output ".repeat(100),
                })
                .collect();
            panel.rebuild_rows();
            panel.focus_handle.focus(window, cx);
            cx.notify();
        });
        cx.simulate_resize(gpui::size(px(640.), px(480.)));
        cx.run_until_parked();
        let first_case = cx.debug_bounds("test-tree-row-2").unwrap().center();
        cx.simulate_click(first_case, gpui::Modifiers::default());
        cx.simulate_keystrokes("down space");
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(matches!(panel.selected, TestRow::Case(1)));
        });
        cx.simulate_keystrokes("end");
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(matches!(panel.selected, TestRow::Case(1999)));
            assert_eq!(panel.suite_status, TestStatus::Passed);
        });
        assert!(cx.debug_bounds("test-tree-row-2001").is_some());
        assert!(
            cx.debug_bounds("test-tree-row-2").is_none(),
            "Only the visible results should render"
        );
        cx.simulate_keystrokes("home down left");
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| assert_eq!(panel.rows.len(), 2));
        cx.simulate_keystrokes("right");
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| assert_eq!(panel.rows.len(), 2002));
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_keeps_live_results_beside_interrupted_xml() {
        let executor = BackgroundExecutor::new(Arc::new(gpui::ThreadedDispatcher::new()));
        let root = tempfile::tempdir().unwrap();
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("gradlew"), r#"case "$1" in
run)
printf '{"id":{"class":"dev.Tests","method":"completed"},"status":"Passed"}\n' > "$KODA_TEST_OUTPUT/events.jsonl"
mkdir "$KODA_TEST_OUTPUT/unit"
printf '<testsuite><testcase' > "$KODA_TEST_OUTPUT/unit/TEST-interrupted.xml"
sleep 30 ;;
esac
"#).unwrap();
        let environment = collections::HashMap::from_iter([(
            "KODA_TEST_OUTPUT".into(),
            directory.path().to_string_lossy().into_owned(),
        )]);
        let execution = TestExecution {
            root: root.path().into(),
            environment,
            directory,
            sources: Vec::new(),
            run: vec!["run".into()],
            discover_only: false,
        };
        let (output, mut logs) = futures::channel::mpsc::channel(16);
        let (progress, mut snapshots) = futures::channel::mpsc::channel(1);
        let (cancel, cancelled) = oneshot::channel();
        let (completion, observed, ()) = futures::executor::block_on(async {
            futures::join!(
                execute_tests(execution, executor.clone(), output, cancelled, progress),
                async {
                    let observed = match select(
                        snapshots.next().boxed(),
                        executor.timer(Duration::from_secs(5)).boxed(),
                    )
                    .await
                    {
                        Either::Left((Some(cases), _)) => cases.len() == 1,
                        _ => false,
                    };
                    cancel
                        .send(())
                        .expect("Runner must remain active until cancellation");
                    while snapshots.next().await.is_some() {}
                    observed
                },
                async { while logs.next().await.is_some() {} }
            )
        });
        assert!(
            observed,
            "Completed tests should reach the pane while the process is running"
        );
        assert!(matches!(completion.2, Ok(ProcessOutput::Cancelled)));
        assert_eq!(completion.1.len(), 1);
        assert_eq!(completion.1[0].status, TestStatus::Passed);
        assert!(
            completion.3.is_some(),
            "Interrupted reports should be diagnosed"
        );
    }
}
