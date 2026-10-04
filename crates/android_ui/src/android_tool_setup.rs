use super::*;
use android_tools::managed::{self, Dependency, Tool};

#[derive(Default)]
pub(super) struct ToolSetup {
    lines: Vec<String>,
    checking: Option<Task<()>>,
    pub(super) operation: Option<Task<()>>,
    pub(super) last_operation: Option<(Tool, &'static str)>,
    pub(super) choosing: bool,
    offline: bool,
    expanded: bool,
}

impl ToolSetup {
    pub(super) fn new() -> Self {
        Self {
            expanded: true,
            ..Default::default()
        }
    }
}

impl AndroidPanel {
    pub(super) fn refresh_tool_setup(&mut self, cx: &mut Context<Self>) {
        self.tool_setup.checking = Some(cx.spawn(async move |panel, cx| {
            let lines = cx.background_spawn(async move { managed::status() }).await;
            panel
                .update(cx, |panel, cx| {
                    panel.tool_setup.lines = lines;
                    panel.tool_setup.checking = None;
                    cx.notify();
                })
                .log_err();
        }));
        cx.notify();
    }

    fn choose_dependency(
        &mut self,
        dependency: Dependency,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let files = matches!(dependency, Dependency::AndroidCli);
        if self.running || self.syncing || self.tool_setup.choosing {
            return;
        }
        self.tool_setup.choosing = true;
        let selected = cx.prompt_for_paths(gpui::PathPromptOptions {
            files,
            directories: !files,
            multiple: false,
            prompt: Some(
                match dependency {
                    Dependency::Sdk => "Choose Android SDK (contains platform-tools)",
                    Dependency::Jdk => "Choose JDK 21 home (contains bin and release)",
                    Dependency::AndroidCli => "Choose Google's Android CLI executable",
                }
                .into(),
            ),
        });
        cx.spawn_in(window, async move |panel, cx| {
            let result = async {
                let paths = selected.await??;
                if let Some(path) = paths.and_then(|paths| paths.into_iter().next()) {
                    cx.background_spawn(async move { managed::save_dependency(dependency, &path) })
                        .await?;
                }
                anyhow::Ok(())
            }
            .await;
            panel
                .update_in(cx, |panel, window, cx| {
                    panel.tool_setup.choosing = false;
                    if let Err(error) = result {
                        panel.fail(error, window, cx);
                    }
                    panel.refresh_tool_setup(cx);
                    panel.refresh_devices(cx);
                })
                .log_err();
        })
        .detach();
    }

    pub(super) fn manage_tool(
        &mut self,
        tool: Tool,
        operation: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.running
            || self.syncing
            || self.tool_setup.choosing
            || self.tool_setup.operation.is_some()
        {
            return;
        }
        let root = match self.trusted_root(cx) {
            Ok(root) => root,
            Err(error) => {
                self.fail(error, window, cx);
                return;
            }
        };
        if self.debug_forward.is_some() {
            self.fail(
                anyhow::anyhow!("Disconnect the Android debugger before changing managed tools."),
                window,
                cx,
            );
            return;
        }
        let offline = self.tool_setup.offline;
        self.root = Some(root.clone());
        self.last_build_operation = None;
        self.tool_setup.last_operation = Some((tool, operation));
        let label = format!("{}: {operation}", tool.label());
        let (session, output, logs) = self.build_panel.update(cx, |panel, cx| {
            panel.begin(BuildTab::Output, label.clone(), false, window, cx)
        });
        let (cancel, cancelled) = oneshot::channel();
        self.command_cancel = Some(cancel);
        self.active_build_session = Some((BuildTab::Output, session));
        self.running = true;
        self.error = None;
        self.status = label.into();
        let executor = cx.background_executor().clone();
        self.tool_setup.operation = Some(cx.spawn_in(window, async move |panel, cx| {
            let result = cx.background_spawn(async move {
                let prepared = managed::prepare(tool, operation, offline)?;
                let mut command = util::command::new_std_command(&prepared.program);
                command.args(&prepared.arguments).envs(&prepared.environment).current_dir(&root);
                let result = android_build::command_output_with_cleanup(command, &executor, Duration::from_secs(1800), output, cancelled, false).await;
                // Embedded recipes must remain available until every subprocess exits.
                drop(prepared.directory);
                result
            }).await;
            logs.await;
            panel.update_in(cx, |panel, window, cx| {
                if panel.active_build_session != Some((BuildTab::Output, session)) { return; }
                panel.active_build_session = None;
                panel.command_cancel = None;
                panel.running = false;
                panel.tool_setup.operation = None;
                let (status, message) = match result {
                    Ok(ProcessOutput::Success(_)) => (BuildStatus::Succeeded, "Managed tool ready. Configure Kotlin or retry Run / Debug / Preview.".to_owned()),
                    Ok(ProcessOutput::Cancelled) => (BuildStatus::Cancelled, "Tool setup cancelled. The previous runtime is preserved; retry when ready.".to_owned()),
                    Err(error) => {
                        let message = format!("{error:#} See Build Output for the installer error. The previous runtime is preserved; repair and retry.");
                        panel.fail(anyhow::anyhow!(message.clone()), window, cx);
                        (BuildStatus::Failed, message)
                    }
                };
                panel.status = message.clone().into();
                panel.build_panel.update(cx, |panel, cx| panel.finish(BuildTab::Output, session, status, message, cx));
                panel.refresh_tool_setup(cx);
                cx.notify();
            }).log_err();
        }));
        cx.notify();
    }

    pub(super) fn render_tool_setup(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.running || self.syncing || self.tool_setup.choosing;
        let details = v_flex().gap_2()
            .child(Label::new("Managed Kotlin, Debug and Preview: Apple Silicon macOS. SDK and JDK are discovered or chosen below.").size(LabelSize::Small))
            .child(Label::new("Requires JDK 21, Python 3.12+ and Apple's Command Line Tools. Install the SDK and Google's Android CLI separately; Koda does not accept SDK licenses.").size(LabelSize::Small).color(Color::Muted))
            .child(Label::new("Verified pinned downloads: JetBrains Kotlin server, Google preview tools, fwcd debugger sources and Adoptium JDK. Debugger builds also fetch Gradle dependencies over HTTPS.").size(LabelSize::Small).color(Color::Muted))
            .children(self.tool_setup.lines.iter().map(|line| Label::new(line.clone()).size(LabelSize::Small).line_clamp(4)))
            .child(h_flex().gap_1().flex_wrap()
                .child(Button::new("choose-sdk", "Choose SDK").disabled(busy).tab_index(0isize).on_click(cx.listener(|panel, _, window, cx| panel.choose_dependency(Dependency::Sdk, window, cx))))
                .child(Button::new("choose-jdk", "Choose JDK 21").disabled(busy).tab_index(0isize).on_click(cx.listener(|panel, _, window, cx| panel.choose_dependency(Dependency::Jdk, window, cx))))
                .child(Button::new("choose-android-cli", "Choose Android CLI").disabled(busy).tab_index(0isize).on_click(cx.listener(|panel, _, window, cx| panel.choose_dependency(Dependency::AndroidCli, window, cx)))))
            .child(h_flex().gap_1().flex_wrap()
                .child(Button::new("check-tool-setup", "Detect dependencies").tab_index(0isize).on_click(cx.listener(|panel, _, _, cx| panel.refresh_tool_setup(cx))))
                .child(Button::new("tool-storage", "Reveal managed storage").tab_index(0isize).on_click(|_, _, cx| cx.reveal_path(&managed::root())))
                .child(Button::new("offline-tools", if self.tool_setup.offline { "Offline: on" } else { "Offline: off" }).tab_index(0isize).disabled(busy).on_click(cx.listener(|panel, _, _, cx| { panel.tool_setup.offline = !panel.tool_setup.offline; cx.notify(); }))))
            .children(Tool::ALL.into_iter().map(|tool| {
                v_flex().gap_1().child(Label::new(tool.label())).child(h_flex().gap_1().flex_wrap().children([
                    ("install", "Install / repair"), ("validate", "Validate"), ("rollback", "Roll back"),
                ].into_iter().map(|(operation, label)| {
                    Button::new(format!("{}-{operation}", tool.name()), label).tab_index(0isize).disabled(busy || !managed::supported())
                        .on_click(cx.listener(move |panel, _, window, cx| panel.manage_tool(tool, operation, window, cx)))
                })))
            }))
            .when(self.tool_setup.operation.is_some(), |element| element.child(
                Button::new("cancel-tool-setup", "Cancel tool setup").tab_index(0isize).on_click(cx.listener(|panel, _, _, cx| panel.cancel_build(BuildTab::Output, cx)))
            ));
        v_flex()
            .gap_2()
            .child(
                Button::new(
                    "toggle-tool-setup",
                    if self.tool_setup.expanded {
                        "Tool setup ▾"
                    } else {
                        "Tool setup ▸"
                    },
                )
                .tab_index(0isize)
                .on_click(cx.listener(|panel, _, _, cx| {
                    panel.tool_setup.expanded = !panel.tool_setup.expanded;
                    cx.notify();
                })),
            )
            .when(self.tool_setup.expanded, |element| element.child(details))
    }
}
