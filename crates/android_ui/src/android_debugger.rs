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
use task::{DebugScenario, ZedDebugConfig};

pub(super) const ADAPTER: &str = "Android Kotlin";

pub(super) struct AndroidKotlinAdapter;

pub(super) fn binary() -> Result<PathBuf> {
    let path = std::env::var_os("ANDROID_IDE_KOTLIN_DEBUGGER").map(PathBuf::from)
        .context("Run script/install-android-debugger, then relaunch script/android-ide to enable Android debugging.")?;
    ensure!(
        path.is_absolute() && path.is_file(),
        "ANDROID_IDE_KOTLIN_DEBUGGER must point to the installed debugger executable"
    );
    let revision = path
        .parent()
        .and_then(Path::parent)
        .map(|root| root.join(".revision"))
        .context("Android debugger installation layout is invalid")?;
    ensure!(std::fs::read_to_string(revision).context("Android debugger revision marker is missing; reinstall script/install-android-debugger")?.trim()
        == "7f05669b642d21afa46ac7b75307fa5d523a7263+android-2", "Android debugger update required. Move the old installation aside and run script/install-android-debugger, then relaunch.");
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
        _: &mut AsyncApp,
    ) -> Result<DebugAdapterBinary> {
        let executable = match user_installed_path {
            Some(path) => path,
            None => binary()?,
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
        envs.entry("JAVA_HOME".into()).or_insert(
            android_tools::kotlin::java_home()?
                .to_string_lossy()
                .into_owned(),
        );
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

    pub(super) fn disconnect_debugger(&mut self, cx: &mut Context<Self>) {
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

    pub(super) fn refresh_run_configurations(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.running || self.syncing {
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

    pub(super) fn run_configuration_picker(&self, cx: &Context<Self>) -> impl IntoElement {
        let configurations = self.run_configurations.clone();
        let panel = cx.weak_entity();
        PopoverMenu::new("android-run-configuration")
            .trigger(
                Button::new(
                    "android-run-configuration",
                    self.selected_run_configuration
                        .clone()
                        .unwrap_or_else(|| "Default launcher".into()),
                )
                .disabled(self.running || self.syncing)
                .tab_index(0isize),
            )
            .menu(move |window, cx| {
                Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                    let default_panel = panel.clone();
                    menu = menu.entry("Default launcher", None, move |_, cx| {
                        default_panel
                            .update(cx, |panel, cx| {
                                if panel.running || panel.syncing {
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
                                    if panel.running || panel.syncing {
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
            })
    }

    pub(super) fn refresh_debug_processes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running || self.syncing || self.debug_forward.is_some() {
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
                    if !panel.project.read(cx).android_model().is_current(&token) {
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

    pub(super) fn debug_process_picker(&self, cx: &Context<Self>) -> impl IntoElement {
        let processes = self.debug_processes.clone();
        let context = self.debug_process_context.clone();
        let panel = cx.weak_entity();
        PopoverMenu::new("android-debug-process")
            .trigger(Button::new("android-debug-process", "Attach process…").disabled(self.running || self.syncing || self.debug_forward.is_some() || processes.is_empty()).tab_index(0isize))
            .menu(move |window, cx| Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                for process in &processes {
                    let panel = panel.clone(); let process = process.clone(); let context = context.clone();
                    menu = menu.entry(format!("{} · PID {}", process.name, process.pid), None, move |window, cx| {
                        panel.update(cx, |panel, cx| {
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
        if (self.running && selected_process.is_some()) || self.debug_forward.is_some() {
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
        self.debug_task = Some(cx.spawn_in(window, async move |panel, cx| {
            let result = cx.background_spawn({
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
                    let port: u16 = positive_number(&output, "debugger port")?;
                    Ok::<_, anyhow::Error>(Forward { model_token, executor, adb, serial, port,
                        label: format!("Android · {application_id} · {process}:{port} · {attachment_id}").into(), session: None })
                }
            }).await;
            panel.update_in(cx, |panel, window, cx| {
                if !panel.project.read(cx).android_model().is_current(&token) { return; }
                panel.running = false;
                panel.followup_model_token = None;
                let result = result.and_then(|forward| {
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
