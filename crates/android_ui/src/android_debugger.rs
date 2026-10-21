use super::*;
use async_trait::async_trait;
use collections::HashMap;
use dap::{
    adapters::{
        DapDelegate, DebugAdapter, DebugAdapterBinary, DebugAdapterName, DebugTaskDefinition,
        StartDebuggingRequestArguments, StartDebuggingRequestArgumentsRequest,
    },
    client::SessionId,
};
use gpui::AsyncApp;
use project::debugger::dap_store::DapStoreEvent;
use serde_json::{Value, json};
use task::{DebugScenario, TaskContext, ZedDebugConfig};

pub(super) const ADAPTER: &str = "Android Kotlin";

pub(super) struct AndroidKotlinAdapter;

pub(super) fn binary() -> Result<PathBuf> {
    let path = match std::env::var_os("ANDROID_IDE_KOTLIN_DEBUGGER") {
        Some(path) => PathBuf::from(path),
        None => android_tools::managed::resolve(android_tools::managed::Tool::Debugger)?,
    };
    ensure!(
        path.is_absolute() && path.is_file(),
        "ANDROID_IDE_KOTLIN_DEBUGGER must point to the installed debugger executable"
    );
    Ok(path)
}

#[async_trait(?Send)]
impl DebugAdapter for AndroidKotlinAdapter {
    fn name(&self) -> DebugAdapterName {
        ADAPTER.into()
    }

    async fn config_from_zed_format(&self, _: ZedDebugConfig) -> Result<DebugScenario> {
        bail!("Use Android: Debug to build and attach to the selected Android device")
    }

    async fn get_binary(
        &self,
        _: &Arc<dyn DapDelegate>,
        config: &DebugTaskDefinition,
        user_installed_path: Option<PathBuf>,
        user_args: Option<Vec<String>>,
        user_env: Option<HashMap<String, String>>,
        cx: &mut AsyncApp,
    ) -> Result<DebugAdapterBinary> {
        let executable = match user_installed_path {
            Some(path) => path,
            None => cx.background_spawn(async { binary() }).await?,
        };
        ensure!(
            executable.is_absolute() && executable.is_file(),
            "Android debugger executable is missing"
        );
        ensure!(
            config.config["request"] == "attach",
            "Android debugging requires an attach configuration"
        );
        let mut envs = user_env.unwrap_or_default();
        if !envs.contains_key("JAVA_HOME") {
            let java_home = cx
                .background_spawn(async { android_tools::kotlin::java_home() })
                .await?;
            envs.insert("JAVA_HOME".into(), java_home.to_string_lossy().into_owned());
        }
        Ok(DebugAdapterBinary {
            command: Some(executable.to_string_lossy().into_owned()),
            arguments: user_args.unwrap_or_default(),
            envs,
            cwd: None,
            connection: None,
            request_args: StartDebuggingRequestArguments {
                request: StartDebuggingRequestArgumentsRequest::Attach,
                configuration: config.config.clone(),
            },
        })
    }

    fn dap_schema(&self) -> Value {
        json!({"type":"object", "required":["request", "hostName", "port", "projectRoot"],
            "properties": {"request":{"const":"attach"}, "hostName":{"type":"string"},
                "port":{"type":"integer", "minimum":1, "maximum":65535},
                "projectRoot":{"type":"string"}, "timeout":{"type":"integer"}}})
    }
}

pub(super) struct Forward {
    pub(super) model_token: android_tools::project_model::ModelToken,
    executor: BackgroundExecutor,
    adb: PathBuf,
    serial: String,
    port: u16,
    label: SharedString,
    session: Option<SessionId>,
}

impl Drop for Forward {
    fn drop(&mut self) {
        let adb = self.adb.clone();
        let args = vec![
            "-s".into(),
            self.serial.clone(),
            "forward".into(),
            "--remove".into(),
            format!("tcp:{}", self.port),
        ];
        let executor = self.executor.clone();
        self.executor
            .spawn(async move {
                tool_output(
                    adb,
                    args,
                    Path::new("/"),
                    &executor,
                    Duration::from_secs(10),
                )
                .await
                .log_err();
            })
            .detach();
    }
}

fn positive_number<T: std::str::FromStr + PartialEq + Default>(
    output: &str,
    name: &str,
) -> Result<T> {
    let text = output.trim();
    ensure!(
        !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()),
        "Invalid {name} from adb: {text}"
    );
    let value = text
        .parse::<T>()
        .map_err(|_| anyhow::anyhow!("Invalid {name} from adb: {text}"))?;
    ensure!(value != T::default(), "Invalid zero {name} from adb");
    Ok(value)
}

fn forward_from_reply(
    model_token: android_tools::project_model::ModelToken,
    attachment_id: u64,
    executor: BackgroundExecutor,
    adb: PathBuf,
    serial: String,
    application_id: &str,
    process: u32,
    reply: &str,
) -> Result<Forward> {
    let mut ports = reply
        .lines()
        .filter_map(|line| positive_number::<u16>(line, "debugger port").ok());
    let port = ports.next().filter(|_| ports.next().is_none());
    let forward = port.map(|port| Forward {
        model_token,
        executor,
        adb,
        serial,
        port,
        label: format!("Android · {application_id} · {process}:{port} · {attachment_id}").into(),
        session: None,
    });
    positive_number::<u16>(reply, "debugger port")?;
    forward.context("ADB did not return an unambiguous debugger forwarding port")
}

async fn wait_for_application_process<ReadProcess: Future<Output = Result<String>>>(
    mut read_process: impl FnMut() -> ReadProcess,
    executor: &BackgroundExecutor,
) -> Result<u32> {
    let mut attempts = 0;
    loop {
        match read_process().await {
            Ok(output) => return positive_number(&output, "application process ID"),
            Err(error) => {
                attempts += 1;
                if attempts == 20 {
                    return Err(error.context("The Android app did not start a debuggable process"));
                }
                // Android CLI can return before ActivityManager has created the process.
                executor.timer(Duration::from_millis(250)).await;
            }
        }
    }
}

async fn read_processes(
    root: &Path,
    serial: &str,
    application: &str,
    executor: &BackgroundExecutor,
) -> Result<Vec<android_tools::debugging::Process>> {
    let adb = adb_path()?;
    let jdwp = tool_output(
        adb.clone(),
        vec!["-s".into(), serial.into(), "jdwp".into()],
        root,
        executor,
        Duration::from_secs(10),
    )
    .await?;
    let processes = tool_output(
        adb,
        vec![
            "-s".into(),
            serial.into(),
            "shell".into(),
            "ps".into(),
            "-A".into(),
            "-o".into(),
            "PID,NAME".into(),
        ],
        root,
        executor,
        Duration::from_secs(10),
    )
    .await?;
    android_tools::debugging::processes(&jdwp, &processes, application)
}

async fn debugger_command_output(
    panel: &WeakEntity<AndroidPanel>,
    cx: &mut gpui::AsyncWindowContext,
    deployment: &DebugDeployment,
    arguments: Vec<String>,
    label: &'static str,
    timeout: Duration,
) -> Result<String> {
    let (cancel, cancelled) = oneshot::channel();
    panel.update_in(cx, |panel, _, cx| {
        panel.validate_device_operation(&deployment.operation, cx)?;
        panel.command_cancel = Some(cancel);
        panel.status = label.into();
        cx.notify();
        Ok::<_, anyhow::Error>(())
    })??;
    let deployment = deployment.clone();
    let executor = cx.background_executor().clone();
    cx.background_spawn(async move {
        let mut command = util::command::new_std_command(&deployment.adb);
        command
            .args(arguments)
            .current_dir(&deployment.operation.root)
            .envs(deployment.environment.iter());
        android_build::command_output_native_client(
            command,
            &executor,
            timeout,
            deployment.output.clone(),
            cancelled,
            false,
        )
        .await?
        .stdout()
    })
    .await
}

impl AndroidPanel {
    pub(super) fn debug_inputs_dirty(&self, include_configuration: bool, cx: &App) -> bool {
        let project = self.project.read(cx);
        let Some(selected) = project.android_model().selected.as_ref() else {
            return true;
        };
        project.buffer_store().read(cx).buffers().any(|buffer| {
            let buffer = buffer.read(cx);
            if !buffer.is_dirty() {
                return false;
            }
            let Some(file) = buffer.file() else {
                return false;
            };
            let Some(worktree) = project.worktree_for_id(file.worktree_id(cx), cx) else {
                return false;
            };
            let absolute = worktree.read(cx).abs_path().join(file.path().as_std_path());
            (include_configuration
                && self
                    .root
                    .as_ref()
                    .is_some_and(|root| absolute == root.join(".zed/android-run.json")))
                || selected.modules().any(|(_, variant)| {
                    variant.components.iter().any(|component| {
                        component.sources.iter().any(|source| {
                            matches!(
                                source.kind,
                                android_tools::project_model::SourceKind::Java
                                    | android_tools::project_model::SourceKind::Kotlin
                            ) && absolute.starts_with(&source.path)
                        })
                    })
                })
                || (Some(worktree.read(cx).abs_path().as_ref()) == self.root.as_deref()
                    && self.model_input_changed(file.path(), cx))
        })
    }

    fn finish_debug_operation(
        &mut self,
        operation_id: u64,
        token: &android_tools::project_model::ModelToken,
        cx: &App,
    ) -> bool {
        if self.debug_operation_id != Some(operation_id)
            || !self.project.read(cx).android_model().is_current(token)
        {
            return false;
        }
        self.debug_operation_id = None;
        true
    }

    pub(super) fn disconnect_debugger(&mut self, cx: &mut Context<Self>) {
        if self.debug_operation_id.take().is_some() {
            // Let the bounded ADB request finish so a just-allocated forwarding
            // port is still owned by Forward and removed when its result drops.
            if let Some(task) = self.debug_task.take() {
                let command_cancel = self
                    .debug_deployment
                    .as_ref()
                    .and_then(|_| self.command_cancel.take());
                cx.spawn(async move |_, _| {
                    task.await;
                    drop(command_cancel);
                })
                .detach();
            }
            if self.debug_deployment.is_some() {
                self.cancel_build(BuildTab::Output, cx);
            } else {
                self.running = false;
                self.followup_model_token = None;
                self.status = "Android debugger operation cancelled".into();
            }
        }
        self.debug_monitor = None;
        if let Some(forward) = self.debug_forward.take() {
            let store = self.project.read(cx).dap_store();
            let session = forward.session.or_else(|| {
                store.read(cx).sessions().find_map(|session| {
                    let session = session.read(cx);
                    (session.label().as_ref() == Some(&forward.label)
                        && session.adapter().as_ref() == ADAPTER)
                        .then(|| session.session_id())
                })
            });
            if session.is_none() {
                let label = forward.label.clone();
                let store = store.clone();
                cx.spawn(async move |_, cx| {
                    for _ in 0..60 {
                        cx.background_executor()
                            .timer(Duration::from_millis(500))
                            .await;
                        let session = store.read_with(cx, |store, cx| {
                            store.sessions().find_map(|session| {
                                let session = session.read(cx);
                                (session.label().as_ref() == Some(&label)
                                    && session.adapter().as_ref() == ADAPTER)
                                    .then(|| session.session_id())
                            })
                        });
                        if let Some(session) = session {
                            store
                                .update(cx, |store, cx| store.shutdown_session(session, cx))
                                .await?;
                            break;
                        }
                    }
                    Ok::<_, anyhow::Error>(())
                })
                .detach_and_log_err(cx);
            }
            if let Some(session) = session {
                store
                    .update(cx, |store, cx| store.shutdown_session(session, cx))
                    .detach_and_log_err(cx);
            }
            self.status = "Android debugger detached; application left running".into();
        }
        cx.notify();
    }

    fn debugger_session_timed_out(&mut self, cx: &mut Context<Self>) {
        self.disconnect_debugger(cx);
        self.status =
            "Android debugger did not create a session. Check debugger output and retry.".into();
        cx.notify();
    }

    fn debugger_controls_busy(&self) -> bool {
        self.running || self.syncing || self.tool_setup.choosing || self.test_operation_id.is_some()
    }

    pub(super) fn refresh_run_configurations(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.debugger_controls_busy() {
            return;
        }
        let result = self
            .trusted_root(cx)
            .and_then(|root| android_tools::debugging::read_configurations(&root));
        match result {
            Ok(configurations) => {
                self.error = None;
                self.run_configurations = configurations;
                if self
                    .selected_run_configuration
                    .as_ref()
                    .is_some_and(|name| {
                        !self
                            .run_configurations
                            .iter()
                            .any(|configuration| &configuration.name == name)
                    })
                {
                    self.selected_run_configuration = None;
                }
                self.status = format!(
                    "Loaded {} Android run configurations",
                    self.run_configurations.len()
                )
                .into();
            }
            Err(error) => self.fail(error, window, cx),
        }
        cx.notify();
    }

    pub(super) fn run_configuration_picker(
        &self,
        compact: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let configurations = self.run_configurations.clone();
        let panel = cx.weak_entity();
        div().max_w(px(140.)).min_w_0().child(
            PopoverMenu::new("android-run-configuration")
                .trigger(
                    Button::new(
                        "android-run-configuration",
                        if compact {
                            "Launches".to_owned()
                        } else {
                            self.selected_run_configuration
                                .clone()
                                .unwrap_or_else(|| "Default launcher".into())
                        },
                    )
                    .truncate(true)
                    .tooltip(Tooltip::text("Choose an Android run configuration"))
                    .disabled(self.debugger_controls_busy())
                    .tab_index(0isize),
                )
                .menu(move |window, cx| {
                    Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                        let reload_panel = panel.clone();
                        menu = menu
                            .entry("Reload run configurations", None, move |window, cx| {
                                reload_panel
                                    .update(cx, |panel, cx| {
                                        panel.refresh_run_configurations(window, cx)
                                    })
                                    .log_err();
                            })
                            .separator();
                        let default_panel = panel.clone();
                        menu = menu.entry("Default launcher", None, move |_, cx| {
                            default_panel
                                .update(cx, |panel, cx| {
                                    if panel.debugger_controls_busy() {
                                        return;
                                    }
                                    panel.selected_run_configuration = None;
                                    cx.notify();
                                })
                                .log_err();
                        });
                        for configuration in &configurations {
                            let panel = panel.clone();
                            let name = configuration.name.clone();
                            menu = menu.entry(name.clone(), None, move |_, cx| {
                                panel
                                    .update(cx, |panel, cx| {
                                        if panel.debugger_controls_busy()
                                            || !panel
                                                .run_configurations
                                                .iter()
                                                .any(|configuration| configuration.name == name)
                                        {
                                            return;
                                        }
                                        panel.selected_run_configuration = Some(name.clone());
                                        cx.notify();
                                    })
                                    .log_err();
                            });
                        }
                        menu
                    }))
                }),
        )
    }

    pub(super) fn refresh_debug_processes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running
            || self.syncing
            || self.debug_forward.is_some()
            || self.tool_setup.choosing
            || self.test_operation_id.is_some()
        {
            return;
        }
        let context = (|| {
            let root = self.trusted_root(cx)?;
            let target = self
                .selected_target
                .as_ref()
                .context("Select an Android variant first")?;
            self.validate_model_target(target, cx)?;
            let application = target.application_id()?;
            Ok::<_, anyhow::Error>((
                root,
                self.selected_device()?.serial.clone(),
                application,
                self.project.read(cx).android_model().token(),
            ))
        })();
        let (root, serial, application, token) = match context {
            Ok(context) => context,
            Err(error) => {
                self.fail(error, window, cx);
                return;
            }
        };
        self.running = true;
        self.debug_processes.clear();
        self.debug_process_context = None;
        self.followup_model_token = Some(token.clone());
        self.next_operation_id += 1;
        let operation_id = self.next_operation_id;
        self.debug_operation_id = Some(operation_id);
        let executor = cx.background_executor().clone();
        self.debug_task = Some(cx.spawn_in(window, async move |panel, cx| {
            let result = cx
                .background_spawn({
                    let root = root.clone();
                    let serial = serial.clone();
                    let application = application.clone();
                    async move { read_processes(&root, &serial, &application, &executor).await }
                })
                .await;
            panel
                .update_in(cx, |panel, window, cx| {
                    if !panel.finish_debug_operation(operation_id, &token, cx) {
                        return;
                    }
                    panel.running = false;
                    panel.followup_model_token = None;
                    let result = result.and_then(|processes| {
                        ensure!(
                            panel.trusted_root(cx)? == root
                                && panel.selected_device()?.serial == serial,
                            "The project or device changed during process discovery"
                        );
                        panel.status = if processes.is_empty() {
                            "No debuggable application processes. Launch a debug APK, then refresh."
                                .into()
                        } else {
                            "Choose a process to attach without building or redeploying.".into()
                        };
                        panel.error = None;
                        panel.debug_processes = processes;
                        panel.debug_process_context = Some((token, serial, application));
                        Ok(())
                    });
                    if let Err(error) = result {
                        panel.fail(error, window, cx);
                    }
                    cx.notify();
                })
                .log_err();
        }));
        cx.notify();
    }

    pub(super) fn debug_process_picker(
        &self,
        compact: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let processes = self.debug_processes.clone();
        let context = self.debug_process_context.clone();
        let panel = cx.weak_entity();
        div().max_w(px(130.)).min_w_0().child(
            PopoverMenu::new("android-debug-process")
                .trigger(Button::new("android-debug-process", if compact { "Attach…" } else { "Attach process…" })
                    .truncate(true)
                    .tooltip(Tooltip::text("Refresh and attach to an Android application process"))
                    .disabled(self.debugger_controls_busy() || self.debug_forward.is_some()).tab_index(0isize))
                .menu(move |window, cx| Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                    let refresh_panel = panel.clone();
                    menu = menu.entry("Refresh processes", None, move |window, cx| {
                        refresh_panel.update(cx, |panel, cx| panel.refresh_debug_processes(window, cx)).log_err();
                    });
                    if !processes.is_empty() { menu = menu.separator(); }
                    for process in &processes {
                        let panel = panel.clone(); let process = process.clone(); let context = context.clone();
                        menu = menu.entry(format!("{} · PID {}", process.name, process.pid), None, move |window, cx| {
                            panel.update(cx, |panel, cx| {
                                if panel.debugger_controls_busy() || panel.debug_forward.is_some() { return; }
                                let result = (|| {
                                    let (token, serial, application) = context.clone().context("Refresh debug processes first")?;
                                    ensure!(panel.project.read(cx).android_model().is_current(&token) && panel.selected_device()?.serial == serial, "Variant or device changed. Refresh debug processes.");
                                    ensure!(!panel.debug_inputs_dirty(false, cx), "Save source edits before attaching. Use Debug to rebuild changed sources.");
                                    let root = panel.trusted_root(cx)?;
                                    panel.attach_debugger_process(root, serial, application, Some(process.clone()), window, cx);
                                    Ok::<_, anyhow::Error>(())
                                })();
                                if let Err(error) = result { panel.fail(error, window, cx); }
                            }).log_err();
                        });
                    }
                    menu
                })))
        )
    }

    pub(super) fn observe_debugger(&self, cx: &mut Context<Self>) -> Vec<Subscription> {
        let store = self.project.read(cx).dap_store();
        vec![
            cx.observe(&store, |panel, store, cx| {
                if let Some(forward) = &mut panel.debug_forward
                    && forward.session.is_none()
                {
                    forward.session = store.read(cx).sessions().find_map(|session| {
                        let session = session.read(cx);
                        (session.label().as_ref() == Some(&forward.label)
                            && session.adapter().as_ref() == ADAPTER)
                            .then(|| session.session_id())
                    });
                }
            }),
            cx.subscribe(&store, |panel, _, event, cx| {
                if let DapStoreEvent::DebugClientShutdown(id) = event
                    && panel
                        .debug_forward
                        .as_ref()
                        .is_some_and(|forward| forward.session == Some(*id))
                {
                    panel.debug_forward = None;
                    panel.debug_monitor = None;
                    panel.status = "Android debugger disconnected".into();
                    cx.notify();
                }
            }),
        ]
    }

    pub(super) fn attach_debugger(
        &mut self,
        root: PathBuf,
        serial: String,
        application_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.attach_debugger_process(root, serial, application_id, None, window, cx);
    }

    fn attach_debugger_process(
        &mut self,
        root: PathBuf,
        serial: String,
        application_id: String,
        selected_process: Option<android_tools::debugging::Process>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if (self.running && self.debug_deployment.is_none())
            || self.syncing
            || self.debug_forward.is_some()
            || self.tool_setup.choosing
            || self.test_operation_id.is_some()
        {
            return;
        }
        if self.debug_inputs_dirty(false, cx) {
            self.running = false;
            self.fail(
                anyhow::anyhow!(
                    "Save source/model edits and rebuild before attaching the Android debugger."
                ),
                window,
                cx,
            );
            return;
        }
        if let Err(error) = binary().and_then(|_| android_tools::kotlin::java_home().map(|_| ())) {
            self.fail(error, window, cx);
            return;
        }
        let token = self.project.read(cx).android_model().token();
        self.followup_model_token = Some(token.clone());
        let source_roots = self
            .project
            .read(cx)
            .android_model()
            .selected
            .as_ref()
            .map(|selected| {
                selected
                    .modules()
                    .flat_map(|(_, variant)| &variant.components)
                    .filter(|component| {
                        component.scope == android_tools::project_model::SourceScope::Main
                    })
                    .flat_map(|component| &component.sources)
                    .filter(|source| {
                        matches!(
                            source.kind,
                            android_tools::project_model::SourceKind::Java
                                | android_tools::project_model::SourceKind::Kotlin
                        )
                    })
                    .map(|source| source.path.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let Err(error) = self.trusted_root(cx).and_then(|current| {
            ensure!(
                current == root,
                "The Android project changed before debugger attachment"
            );
            Ok(())
        }) {
            self.fail(error, window, cx);
            return;
        }
        self.running = true;
        self.error = None;
        self.status = "Attaching Android debugger…".into();
        let executor = cx.background_executor().clone();
        self.next_operation_id += 1;
        let attachment_id = self.next_operation_id;
        self.debug_operation_id = Some(attachment_id);
        let deployment = self.debug_deployment.clone();
        self.debug_task = Some(cx.spawn_in(window, async move |panel, cx| {
            let result = if let Some(deployment) = &deployment {
                async {
                    let process = wait_for_application_process(|| {
                        let panel = panel.clone();
                        let deployment = deployment.clone();
                        let mut cx = cx.clone();
                        let application_id = application_id.clone();
                        async move {
                            debugger_command_output(&panel, &mut cx, &deployment,
                                vec!["-s".into(), deployment.operation.serial.clone(), "shell".into(), "pidof".into(), "-s".into(), application_id],
                                "Waiting for the application process…", Duration::from_secs(1)).await
                        }
                    }, &executor).await?;
                    let output = debugger_command_output(&panel, cx, deployment,
                        vec!["-s".into(), serial.clone(), "forward".into(), "tcp:0".into(), format!("jdwp:{process}")],
                        "Opening the debugger connection…", Duration::from_secs(10)).await?;
                    forward_from_reply(token.clone(), attachment_id, executor.clone(), deployment.adb.clone(), serial.clone(), &application_id, process, &output)
                }.await
            } else { cx.background_spawn({
                let root = root.clone();
                let model_token = token.clone();
                async move {
                    let adb = adb_path()?;
                    let process = if let Some(selected_process) = selected_process {
                        ensure!(read_processes(&root, &serial, &application_id, &executor).await?.contains(&selected_process), "The selected process exited or is no longer debuggable. Refresh processes.");
                        selected_process.pid
                    } else { wait_for_application_process(
                        || tool_output(adb.clone(), vec!["-s".into(), serial.clone(), "shell".into(), "pidof".into(), "-s".into(), application_id.clone()], &root, &executor, Duration::from_secs(1)),
                        &executor,
                    ).await? };
                    let output = tool_output(adb.clone(), vec!["-s".into(), serial.clone(), "forward".into(), "tcp:0".into(), format!("jdwp:{process}")], &root, &executor, Duration::from_secs(10)).await?;
                    forward_from_reply(model_token, attachment_id, executor, adb, serial, &application_id, process, &output)
                }
            }).await };
            panel.update_in(cx, |panel, window, cx| {
                if !panel.finish_debug_operation(attachment_id, &token, cx) { return; }
                panel.running = deployment.is_some();
                if deployment.is_none() { panel.followup_model_token = None; }
                let result = result.and_then(|forward| {
                    if let Some(deployment) = &deployment { panel.validate_device_operation(&deployment.operation, cx)?; }
                    ensure!(panel.trusted_root(cx)? == root, "The Android project changed during debugger attachment");
                    ensure!(panel.selected_device()?.serial == forward.serial, "The selected device changed during debugger attachment");
                    ensure!(!panel.debug_inputs_dirty(false, cx), "Sources/model inputs changed during debugger attachment. Save and rebuild before retrying.");
                    let worktree = panel.project.read(cx).visible_worktrees(cx)
                        .find(|worktree| worktree.read(cx).abs_path().as_ref() == root)
                        .map(|worktree| worktree.read(cx).id()).context("The Android project was closed")?;
                    let provider = panel.workspace.read_with(cx, |workspace, _| workspace.debugger_provider())?
                        .context("The native debugger is unavailable")?;
                    let scenario = DebugScenario { adapter: ADAPTER.into(), label: forward.label.clone(), build: None,
                        config: json!({"request":"attach", "hostName":"127.0.0.1", "port":forward.port, "projectRoot":root, "sourceRoots":source_roots, "timeout":5000}), tcp_connection: None };
                    panel.debug_forward = Some(forward);
                    panel.debug_monitor = Some(cx.spawn(async move |panel, cx| {
                        let result = async {
                        for _ in 0..60 {
                            cx.background_executor().timer(Duration::from_millis(500)).await;
                            let connected = panel.update(cx, |panel, cx| {
                                let store = panel.project.read(cx).dap_store();
                                if let Some(forward) = &mut panel.debug_forward {
                                    forward.session = store.read(cx).sessions().find_map(|session| {
                                        let session = session.read(cx);
                                        (session.label().as_ref() == Some(&forward.label) && session.adapter().as_ref() == ADAPTER).then(|| session.session_id())
                                    });
                                    forward.session.is_some()
                                } else { true }
                            })?;
                            if connected { return Ok::<_, anyhow::Error>(()); }
                        }
                        panel.update(cx, |panel, cx| {
                            panel.debugger_session_timed_out(cx);
                        })?;
                        Ok(())
                        }.await;
                        result.log_err();
                    }));
                    provider.start_session(scenario, TaskContext { cwd: Some(root), ..Default::default() }.into(), None, Some(worktree), window, cx);
                    panel.status = "Android debugger started. Use the debugger controls to inspect, step, resume, or disconnect.".into();
                    Ok(())
                });
                if let Err(error) = result { panel.fail(error, window, cx); }
                cx.notify();
            }).log_err();
        }));
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn malformed_forward_reply_cleans_up_its_unambiguous_allocation() -> Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir()?;
        let adb = directory.path().join("adb");
        std::fs::write(
            &adb,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"${0%/*}/removed.tmp\"\nmv \"${0%/*}/removed.tmp\" \"${0%/*}/removed\"\n",
        )?;
        std::fs::set_permissions(&adb, std::fs::Permissions::from_mode(0o755))?;
        let executor = BackgroundExecutor::new(Arc::new(gpui::ThreadedDispatcher::new()));
        let result = forward_from_reply(
            android_tools::project_model::ModelState::default().token(),
            1,
            executor.clone(),
            adb,
            "owned-forward".into(),
            "com.example",
            123,
            "31345\nunexpected diagnostic\n",
        );
        assert!(result.is_err());
        futures::executor::block_on(async {
            let wait = async {
                while !directory.path().join("removed").exists() {
                    executor.timer(Duration::from_millis(10)).await;
                }
            };
            match select(
                Box::pin(wait),
                Box::pin(executor.timer(Duration::from_secs(5))),
            )
            .await
            {
                Either::Left(_) => Ok::<_, anyhow::Error>(()),
                Either::Right(_) => bail!("The malformed forward allocation was not cleaned up."),
            }
        })?;
        assert_eq!(
            std::fs::read_to_string(directory.path().join("removed"))?,
            "-s\nowned-forward\nforward\n--remove\ntcp:31345\n"
        );
        Ok(())
    }

    #[gpui::test]
    async fn active_debugger_rejects_repeated_unit_device_and_discovery_requests(
        cx: &mut gpui::TestAppContext,
    ) {
        let _state = cx.update(workspace::AppState::test);
        let project = Project::test(project::FakeFs::new(cx.executor()), [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project.clone(), cx));
        panel.update_in(cx, |panel, window, cx| {
            panel.debug_forward = Some(Forward {
                model_token: panel.project.read(cx).android_model().token(),
                executor: cx.background_executor().clone(),
                adb: "/missing-test-adb".into(),
                serial: "device".into(),
                port: 6000,
                label: "active-debug".into(),
                session: None,
            });
            let operation = panel.next_operation_id;
            for _ in 0..2 {
                for kind in [
                    android_tools::testing::TestKind::Unit,
                    android_tools::testing::TestKind::Device,
                ] {
                    for discover_only in [false, true] {
                        panel.error = None;
                        panel.start_tests(kind, Vec::new(), discover_only, window, cx);
                        assert!(panel.error.as_ref().is_some_and(|error| {
                            error.contains("Disconnect the Android debugger")
                        }));
                        assert!(panel.debug_forward.is_some());
                        assert!(panel.test_operation_id.is_none());
                        assert!(panel.pending_test.is_none());
                        assert!(panel.test_task.is_none());
                        assert!(panel.test_cancel.is_none());
                        assert!(!panel.test_panel.read(cx).busy);
                        assert_eq!(panel.next_operation_id, operation);
                    }
                }
            }
            let token = panel.project.read(cx).android_model().token();
            for _ in 0..2 {
                for request in [GradleOperation::Run, GradleOperation::Debug] {
                    panel.error = None;
                    panel.gradle(request, window, cx);
                    assert!(panel.error.as_ref().is_some_and(|error| {
                        error.contains("Disconnect the current Android debug session")
                    }));
                    assert!(
                        panel
                            .debug_forward
                            .as_ref()
                            .is_some_and(|forward| forward.label.as_ref() == "active-debug")
                    );
                    assert!(panel.pending_gradle_operation.is_none());
                    assert!(panel.build_task.is_none());
                    assert!(panel.deploy_task.is_none());
                    assert!(panel.active_operation_id.is_none());
                    assert!(panel.active_build_session.is_none());
                    assert_eq!(panel.next_operation_id, operation);
                    assert!(panel.project.read(cx).android_model().is_current(&token));
                }
            }
            panel.disconnect_debugger(cx);
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn running_and_cancelling_tests_reject_repeated_debug_actions(
        cx: &mut gpui::TestAppContext,
    ) {
        let _state = cx.update(workspace::AppState::test);
        let project = Project::test(project::FakeFs::new(cx.executor()), [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project.clone(), cx));
        panel.update_in(cx, |panel, window, cx| {
            let (cancel, mut cancellation) = futures::channel::oneshot::channel();
            panel.test_operation_id = Some(17);
            panel.test_cancel = Some(cancel);
            let operation = panel.next_operation_id;
            for cancelling in [false, true] {
                if cancelling {
                    panel.cancel_tests(cx);
                    assert_eq!(
                        cancellation.try_recv().expect("Test cancellation channel"),
                        Some(())
                    );
                }
                let status = panel.status.clone();
                for _ in 0..2 {
                    panel.refresh_debug_processes(window, cx);
                    panel.refresh_run_configurations(window, cx);
                    for selected in [
                        None,
                        Some(android_tools::debugging::Process {
                            pid: 42,
                            name: "dev.app:worker".into(),
                        }),
                    ] {
                        panel.attach_debugger_process(
                            "/android".into(),
                            "device".into(),
                            "dev.app".into(),
                            selected,
                            window,
                            cx,
                        );
                    }
                    assert!(panel.error.is_none());
                    assert_eq!(panel.status, status);
                    assert_eq!(panel.test_operation_id, Some(17));
                    assert_eq!(panel.next_operation_id, operation);
                    assert!(panel.debug_forward.is_none());
                    assert!(panel.debug_task.is_none());
                    assert!(panel.debug_monitor.is_none());
                    assert!(panel.followup_model_token.is_none());
                    assert!(!panel.running);
                    if !cancelling {
                        assert_eq!(
                            cancellation.try_recv().expect("Test cancellation channel"),
                            None
                        );
                    }
                }
            }
            panel.test_operation_id = None;
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn setup_and_running_operations_reject_repeated_debug_controls(
        cx: &mut gpui::TestAppContext,
    ) {
        let _state = cx.update(workspace::AppState::test);
        let project = Project::test(project::FakeFs::new(cx.executor()), [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project.clone(), cx));
        panel.update_in(cx, |panel, window, cx| {
            for setup in [false, true] {
                panel.running = !setup;
                panel.tool_setup.choosing = setup;
                let status = panel.status.clone();
                let operation = panel.next_operation_id;
                for _ in 0..2 {
                    panel.refresh_debug_processes(window, cx);
                    panel.refresh_run_configurations(window, cx);
                    panel.attach_debugger_process(
                        "/android".into(),
                        "device".into(),
                        "dev.app".into(),
                        None,
                        window,
                        cx,
                    );
                    assert!(panel.error.is_none());
                    assert_eq!(panel.status, status);
                    assert_eq!(panel.next_operation_id, operation);
                    assert!(panel.debug_task.is_none());
                    assert!(panel.debug_forward.is_none());
                    assert_eq!(panel.running, !setup);
                }
            }
            panel.running = false;
            panel.tool_setup.choosing = false;
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn cancelling_deployment_preserves_the_pending_forward_reply(
        cx: &mut gpui::TestAppContext,
    ) {
        let _state = cx.update(workspace::AppState::test);
        let project = Project::test(project::FakeFs::new(cx.executor()), [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project.clone(), cx));
        let (release, waiting) = oneshot::channel();
        let (completed, completion) = oneshot::channel();
        panel.update_in(cx, |panel, window, cx| {
            let token = panel.project.read(cx).android_model().token();
            let (session_id, output, logs) = panel.build_panel.update(cx, |panel, cx| {
                panel.begin(BuildTab::Output, "Android Debug".into(), false, window, cx)
            });
            logs.detach();
            let operation = Arc::new(DeviceOperation {
                root: "/android".into(),
                serial: "cancel-forward-reply".into(),
                model_token: token.clone(),
                operation_id: 51,
                session_id,
                target: None,
                lease: DeviceOperationLease::acquire("cancel-forward-reply")
                    .expect("Exclusive regression device"),
                allow_disconnect: false,
                selected_avd: None,
            });
            panel.active_operation_id = Some(51);
            panel.active_build_session = Some((BuildTab::Output, session_id));
            panel.followup_model_token = Some(token.clone());
            panel.debug_operation_id = Some(52);
            panel.running = true;
            panel.debug_deployment = Some(DebugDeployment {
                operation,
                adb: "/missing-test-adb".into(),
                environment: Arc::new(Default::default()),
                output,
            });
            let (cancel, mut cancelled) = oneshot::channel();
            panel.command_cancel = Some(cancel);
            panel.debug_task = Some(cx.spawn(async move |panel, cx| {
                waiting.await.expect("Release ADB forwarding reply");
                assert_eq!(
                    cancelled
                        .try_recv()
                        .expect("Keep pending ADB reply alive through cancellation"),
                    None
                );
                panel
                    .update(cx, |panel, cx| {
                        let forward = forward_from_reply(
                            token.clone(),
                            52,
                            cx.background_executor().clone(),
                            "/missing-test-adb".into(),
                            "cancel-forward-reply".into(),
                            "dev.app",
                            42,
                            "6001",
                        )
                        .expect("Late ADB reply still produces an owned forwarding allocation");
                        assert!(!panel.finish_debug_operation(52, &token, cx));
                        drop(forward);
                    })
                    .expect("Controller retained while draining forwarding reply");
                completed.send(()).expect("Regression completion receiver");
            }));
            panel.cancel_build(BuildTab::Output, cx);
            panel.disconnect_debugger(cx);
            assert!(panel.debug_task.is_none());
            assert!(panel.debug_operation_id.is_none());
            assert!(panel.command_cancel.is_none());
            assert!(panel.debug_deployment.is_none());
            assert!(panel.active_operation_id.is_none());
            assert!(!panel.running);
        });
        release
            .send(())
            .expect("Cancelled deployment still drains its request");
        completion
            .await
            .expect("Forward reply drained and stale result discarded");
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn cancelled_debug_operation_cannot_finish_a_replacement(cx: &mut gpui::TestAppContext) {
        let _state = cx.update(workspace::AppState::test);
        let project = Project::test(project::FakeFs::new(cx.executor()), [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project.clone(), cx));
        let (release, waiting) = oneshot::channel();
        panel.update(cx, |panel, cx| {
            let token = panel.project.read(cx).android_model().token();
            panel.debug_operation_id = Some(41);
            panel.followup_model_token = Some(token.clone());
            panel.running = true;
            panel.debug_task = Some(cx.spawn(async move |panel, cx| {
                waiting.await.expect("Release cancelled debugger result");
                panel
                    .update(cx, |panel, cx| {
                        assert!(!panel.finish_debug_operation(41, &token, cx));
                    })
                    .expect("Debugger controller survives cancellation");
            }));
            panel.disconnect_debugger(cx);
            panel.disconnect_debugger(cx);
            assert!(panel.debug_operation_id.is_none());
            assert!(panel.debug_task.is_none());
            assert!(panel.followup_model_token.is_none());
            assert!(!panel.running);
            panel.debug_operation_id = Some(42);
            panel.followup_model_token = Some(panel.project.read(cx).android_model().token());
            panel.running = true;
            panel.status = "Replacement attachment".into();
        });
        release
            .send(())
            .expect("Detached request remains alive to clean up");
        cx.run_until_parked();
        panel.update(cx, |panel, cx| {
            assert_eq!(panel.debug_operation_id, Some(42));
            assert!(panel.running);
            assert!(panel.followup_model_token.is_some());
            assert_eq!(panel.status.as_ref(), "Replacement attachment");
            let token = panel.project.read(cx).android_model().token();
            assert!(panel.finish_debug_operation(42, &token, cx));
            panel.running = false;
            panel.followup_model_token = None;
        });
    }

    #[gpui::test]
    async fn cancelled_deferred_attach_shuts_down_only_its_session(cx: &mut gpui::TestAppContext) {
        deferred_attach_cleanup(cx, false).await;
    }

    #[gpui::test]
    async fn timed_out_deferred_attach_shuts_down_only_its_session(cx: &mut gpui::TestAppContext) {
        deferred_attach_cleanup(cx, true).await;
    }

    async fn deferred_attach_cleanup(cx: &mut gpui::TestAppContext, timed_out: bool) {
        let _state = cx.update(workspace::AppState::test);
        let project = Project::test(project::FakeFs::new(cx.executor()), [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project.clone(), cx));
        let store = project.read_with(cx, |project, _| project.dap_store());
        panel.update(cx, |panel, cx| {
            panel.debug_forward = Some(Forward {
                model_token: panel.project.read(cx).android_model().token(),
                executor: cx.background_executor().clone(),
                adb: "/missing-test-adb".into(),
                serial: "device".into(),
                port: 6000,
                label: "cancelled-attach".into(),
                session: None,
            });
            if timed_out {
                panel.debugger_session_timed_out(cx);
            } else {
                panel.disconnect_debugger(cx);
            }
            panel.disconnect_debugger(cx);
            assert!(panel.debug_forward.is_none());
        });
        let (cancelled, unrelated) = store.update(cx, |store, cx| {
            let cancelled = store.new_session(
                Some("cancelled-attach".into()),
                ADAPTER.into(),
                TaskContext::default().into(),
                None,
                Default::default(),
                cx,
            );
            let unrelated = store.new_session(
                Some("different-attach".into()),
                ADAPTER.into(),
                TaskContext::default().into(),
                None,
                Default::default(),
                cx,
            );
            (
                cancelled.read(cx).session_id(),
                unrelated.read(cx).session_id(),
            )
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        store.read_with(cx, |store, _| {
            assert!(
                store.session_by_id(cancelled).is_none(),
                "A deferred start after cancellation must be shut down"
            );
            assert!(
                store.session_by_id(unrelated).is_some(),
                "Other debug sessions must remain active"
            );
        });
    }

    #[gpui::test]
    async fn active_debugger_detaches_after_variant_changes_and_switching_back(
        cx: &mut gpui::TestAppContext,
    ) {
        let _state = cx.update(workspace::AppState::test);
        let project = Project::test(project::FakeFs::new(cx.executor()), [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project.clone(), cx));
        let target = AndroidTarget {
            module: ":app".into(),
            variant: "debug".into(),
            output_listing: "/android/output.json".into(),
        };
        panel.update(cx, |panel, cx| {
            panel.root = Some("/android".into());
            crate::tests::publish_test_android_model(panel, &target, cx);
        });
        cx.run_until_parked();
        let store = project.read_with(cx, |project, _| project.dap_store());
        let (owned, unrelated) = store.update(cx, |store, cx| {
            let session =
                |store: &mut project::debugger::dap_store::DapStore,
                 label: &str,
                 cx: &mut Context<project::debugger::dap_store::DapStore>| {
                    store
                        .new_session(
                            Some(label.into()),
                            ADAPTER.into(),
                            TaskContext::default().into(),
                            None,
                            Default::default(),
                            cx,
                        )
                        .read(cx)
                        .session_id()
                };
            (
                session(store, "active-attach", cx),
                session(store, "unrelated-attach", cx),
            )
        });
        panel.update(cx, |panel, cx| {
            panel.debug_forward = Some(Forward {
                model_token: panel.project.read(cx).android_model().token(),
                executor: cx.background_executor().clone(),
                adb: "/missing-test-adb".into(),
                serial: "device".into(),
                port: 6000,
                label: "active-attach".into(),
                session: Some(owned),
            });
            let other_variant = AndroidTarget {
                variant: "release".into(),
                ..target.clone()
            };
            crate::tests::publish_test_android_model(panel, &other_variant, cx);
            crate::tests::publish_test_android_model(panel, &target, cx);
        });
        cx.run_until_parked();
        assert!(panel.read_with(cx, |panel, _| panel.debug_forward.is_none()));
        store.read_with(cx, |store, _| {
            assert!(store.session_by_id(owned).is_none());
            assert!(store.session_by_id(unrelated).is_some());
        });
    }

    #[gpui::test]
    async fn attach_ignores_unrelated_dirty_buffers_but_rejects_selected_sources(
        cx: &mut gpui::TestAppContext,
    ) {
        let _state = cx.update(workspace::AppState::test);
        let filesystem = project::FakeFs::new(cx.executor());
        filesystem
            .insert_tree(
                "/android",
                json!({"notes.txt": "notes", ".zed": {"android-run.json": "[]"}, "app": {"src": {"Main.kt": "fun main() {}"}}}),
            )
            .await;
        let project = Project::test(filesystem, [Path::new("/android")], cx).await;
        let notes = project
            .update(cx, |project, cx| {
                project.open_local_buffer("/android/notes.txt", cx)
            })
            .await
            .expect("Open notes");
        let configuration = project
            .update(cx, |project, cx| {
                project.open_local_buffer("/android/.zed/android-run.json", cx)
            })
            .await
            .expect("Open configuration");
        let source = project
            .update(cx, |project, cx| {
                project.open_local_buffer("/android/app/src/Main.kt", cx)
            })
            .await
            .expect("Open source");
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        panel.update(cx, |panel, cx| {
            let target = AndroidTarget {
                module: ":app".into(),
                variant: "debug".into(),
                output_listing: "/android/output.json".into(),
            };
            panel.root = Some("/android".into());
            crate::tests::publish_test_android_model(panel, &target, cx);
        });
        notes.update(cx, |buffer, cx| buffer.edit([(0..0, "edited ")], None, cx));
        assert!(!panel.read_with(cx, |panel, cx| panel.debug_inputs_dirty(false, cx)));
        configuration.update(cx, |buffer, cx| buffer.edit([(0..0, " ")], None, cx));
        assert!(!panel.read_with(cx, |panel, cx| panel.debug_inputs_dirty(false, cx)));
        assert!(panel.read_with(cx, |panel, cx| panel.debug_inputs_dirty(true, cx)));
        source.update(cx, |buffer, cx| {
            buffer.edit([(0..0, "// edited\n")], None, cx)
        });
        assert!(panel.read_with(cx, |panel, cx| panel.debug_inputs_dirty(false, cx)));
    }

    #[gpui::test]
    async fn waits_for_application_process(executor: BackgroundExecutor) {
        let mut attempts = 0;
        let process = wait_for_application_process(
            || {
                attempts += 1;
                std::future::ready(if attempts < 3 {
                    Err(anyhow::anyhow!("process not created"))
                } else {
                    Ok("1234\n".into())
                })
            },
            &executor,
        )
        .await
        .expect("The app process should appear after two retries");
        assert_eq!(process, 1234);
        assert_eq!(attempts, 3);

        let mut attempts = 0;
        let error = wait_for_application_process(
            || {
                attempts += 1;
                std::future::ready(Err(anyhow::anyhow!("device offline")))
            },
            &executor,
        )
        .await
        .expect_err("An unavailable device must stop retrying");
        assert_eq!(attempts, 20);
        assert!(format!("{error:#}").contains("device offline"));
        assert!(
            wait_for_application_process(|| std::future::ready(Ok("1234 5678".into())), &executor,)
                .await
                .is_err()
        );
    }

    #[test]
    fn validates_adb_process_and_port() -> Result<()> {
        assert_eq!(positive_number::<u16>("62983\n", "port")?, 62983);
        assert_eq!(positive_number::<u32>("1234", "process")?, 1234);
        for output in ["", "0", "-1", "1 2", "1\n2", "123;echo", "65536"] {
            assert!(positive_number::<u16>(output, "port").is_err());
        }
        Ok(())
    }
}
