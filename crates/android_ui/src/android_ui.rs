mod android_build;
mod android_debugger;
mod android_logcat;
mod android_logcat_panel;
mod android_preview;
mod android_status;
mod android_tool_setup;
#[cfg(test)]
mod first_launch_setup_tests;

use android_build::{BuildEvent, BuildStatus, BuildTab, ProcessOutput};
pub use android_build::{BuildPanel, ToggleBuild};
use android_logcat_panel::LogcatPanel;
use android_tools::{
    AndroidTarget, Device, adb_path, deployment, emulator_path, is_gradle_project,
    parse_device_abis, parse_devices, parse_emulators,
};
use anyhow::{Context as _, Result, bail, ensure};
use db::kvp::KeyValueStore;
use futures::{
    AsyncReadExt as _, FutureExt as _, StreamExt as _,
    channel::oneshot,
    future::{Either, Shared, select},
};
use gpui::{
    Action, App, BackgroundExecutor, Context, Entity, EntityId, EventEmitter, FocusHandle,
    Focusable, Global, Subscription, Task, WeakEntity, actions,
};
use project::{Project, WorktreeId, trusted_worktrees::TrustedWorktrees};
use settings::Settings;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use task::SaveStrategy;
use ui::{ContextMenu, PopoverMenu, Tooltip, prelude::*};
use util::{ResultExt as _, rel_path::RelPath};
use workspace::{
    Toast, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    notifications::NotificationId,
    tasks::ScheduledTaskResult,
};
pub use zed_actions::android::Logcat;

actions!(
    android,
    [
        /// Opens setup to download or repair Java and Android SDK tools.
        Setup,
        /// Opens Android setup and managed dependencies.
        ToggleFocus,
        /// Evaluates the Android project's modules and build variants.
        SyncProject,
        /// Refreshes connected Android devices.
        RefreshDevices,
        /// Starts the selected Android virtual device.
        StartEmulator,
        /// Stops the selected Android emulator and refreshes connected devices.
        StopEmulator,
        /// Builds the selected Android variant.
        Build,
        /// Builds and launches the selected Android variant on the selected device.
        Run,
        /// Builds and debugs the selected Android variant on the selected device.
        Debug,
        /// Runs local unit tests for the selected Android variant.
        Test,
        /// Runs Android lint for the selected variant.
        Lint,
        /// Configures the official Kotlin server with native Gradle import and JDK 21.
        ConfigureKotlin,
        /// Configures the official Kotlin server with native Gradle import and JDK 21.
        ConfigureOfficialKotlin,
        /// Builds the selected variant and configures Android-aware Java language support.
        ConfigureJava,
        /// Builds the selected variant and renders its Compose previews beside the code.
        ComposePreview,
        /// Shows or hides Compose previews for the active Kotlin file.
        ToggleComposePreview,
    ]
);

pub fn init(cx: &mut App) {
    dap::DapRegistry::global(cx).add_adapter(Arc::new(android_debugger::AndroidKotlinAdapter));
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else { return };
        let panel = cx
            .new(|cx| AndroidPanel::new(workspace.weak_handle(), workspace.project().clone(), cx));
        panel.update(cx, |panel, cx| panel.observe_project_open(window, cx));
        workspace.add_panel(panel.read(cx).build_panel.clone(), window, cx);
        android_status::register(&panel, window, cx);
        register_controller(workspace, &panel, cx);
        panel.update(cx, |panel, cx| {
            panel.observe_first_launch_setup(workspace.modal_layer(), window, cx)
        });
        let logcat_panel = cx.new(|cx| LogcatPanel::new(workspace, window, cx));
        workspace.add_panel(logcat_panel, window, cx);
        workspace
            .register_action(|workspace, _: &ToggleBuild, window, cx| {
                workspace.toggle_panel_focus::<BuildPanel>(window, cx);
            })
            .register_action(|workspace, _: &SyncProject, window, cx| {
                with_panel(workspace, window, cx, AndroidPanel::sync_project)
            })
            .register_action(|workspace, _: &RefreshDevices, window, cx| {
                with_panel(workspace, window, cx, |panel, _, cx| {
                    panel.refresh_devices(cx)
                })
            })
            .register_action(|workspace, _: &StartEmulator, window, cx| {
                with_panel(workspace, window, cx, |panel, window, cx| {
                    if let Some(name) = panel.selected_avd.clone() {
                        panel.start_emulator(name, window, cx);
                    } else {
                        panel.fail(anyhow::anyhow!("Refresh devices and select an available Android virtual device first."), window, cx);
                    }
                })
            })
            .register_action(|workspace, _: &StopEmulator, window, cx| {
                with_panel(workspace, window, cx, AndroidPanel::stop_emulator)
            })
            .register_action(|workspace, _: &Build, window, cx| {
                with_panel(workspace, window, cx, |panel, window, cx| {
                    panel.gradle(GradleOperation::Build, window, cx)
                })
            })
            .register_action(|workspace, _: &Run, window, cx| {
                with_panel(workspace, window, cx, |panel, window, cx| {
                    panel.gradle(GradleOperation::Run, window, cx)
                })
            })
            .register_action(|workspace, _: &Debug, window, cx| {
                with_panel(workspace, window, cx, |panel, window, cx| {
                    panel.gradle(GradleOperation::Debug, window, cx)
                })
            })
            .register_action(|workspace, _: &Test, window, cx| {
                with_panel(workspace, window, cx, |panel, window, cx| {
                    panel.gradle(GradleOperation::Test, window, cx)
                })
            })
            .register_action(|workspace, _: &Lint, window, cx| {
                with_panel(workspace, window, cx, |panel, window, cx| {
                    panel.gradle(GradleOperation::Lint, window, cx)
                })
            })
            .register_action(|workspace, _: &android_logcat::Toggle, window, cx| {
                if workspace
                    .panel::<LogcatPanel>(cx)
                    .is_some_and(|panel| panel.read(cx).has_views(cx))
                {
                    if !workspace.toggle_panel_focus::<LogcatPanel>(window, cx) {
                        workspace.close_panel::<LogcatPanel>(window, cx);
                    }
                } else {
                    with_panel(workspace, window, cx, AndroidPanel::logcat);
                }
            })
            .register_action(|workspace, _: &Logcat, window, cx| {
                with_panel(workspace, window, cx, AndroidPanel::logcat)
            })
            .register_action(|workspace, _: &ComposePreview, window, cx| {
                with_panel(workspace, window, cx, |panel, window, cx| {
                    panel.gradle(GradleOperation::Preview, window, cx)
                })
            })
            .register_action(|workspace, _: &ToggleComposePreview, window, cx| {
                android_preview::toggle_preview(workspace, window, cx);
            })
            .register_action(|workspace, _: &ConfigureJava, window, cx| {
                with_panel(workspace, window, cx, |panel, window, cx| {
                    panel.gradle(GradleOperation::Java, window, cx)
                })
            })
            .register_action(|workspace, _: &ConfigureKotlin, window, cx| {
                with_panel(workspace, window, cx, |panel, window, cx| {
                    panel.gradle(GradleOperation::Kotlin, window, cx)
                })
            })
            .register_action(|workspace, _: &ConfigureOfficialKotlin, window, cx| {
                with_panel(workspace, window, cx, |panel, window, cx| {
                    panel.gradle(GradleOperation::Kotlin, window, cx)
                })
            });
        cx.notify();
    })
    .detach();
}

#[derive(Default)]
struct AndroidControllers(HashMap<EntityId, WeakEntity<AndroidPanel>>);
impl Global for AndroidControllers {}

#[derive(Default)]
struct FirstLaunchSetup {
    enabled: bool,
    resolved: bool,
    observing_windows: bool,
    workspace: Option<WeakEntity<Workspace>>,
    window: Option<gpui::WindowId>,
}
impl Global for FirstLaunchSetup {}

pub fn enable_first_launch_setup(cx: &mut App) {
    observe_automatic_setup_windows(cx);
    cx.global_mut::<FirstLaunchSetup>().enabled = true;
}

fn observe_automatic_setup_windows(cx: &mut App) {
    if cx
        .try_global::<FirstLaunchSetup>()
        .is_some_and(|setup| setup.observing_windows)
    {
        return;
    }
    let setup = cx.default_global::<FirstLaunchSetup>();
    setup.observing_windows = true;
    cx.on_window_closed(|cx, window| {
        if cx.global::<FirstLaunchSetup>().window == Some(window) {
            let setup = cx.global_mut::<FirstLaunchSetup>();
            setup.workspace = None;
            setup.window = None;
        }
    })
    .detach();
}

fn first_launch_setup_key() -> String {
    first_launch_setup_key_for_root(&android_tools::managed::root())
}

fn first_launch_setup_key_for_root(root: &Path) -> String {
    format!("android-setup-onboarding-v1:{}", root.display())
}

fn record_first_launch_setup(state: &'static str, cx: &mut App) {
    let Some(setup) = cx.try_global::<FirstLaunchSetup>() else {
        return;
    };
    // Closing the completed wizard also passes through its dismissal handler.
    if !setup.enabled || (setup.resolved && state == "deferred") {
        return;
    }
    let setup = cx.global_mut::<FirstLaunchSetup>();
    setup.resolved = true;
    let key = first_launch_setup_key();
    let database = KeyValueStore::global(cx);
    db::write_and_log(cx, move || async move {
        database.write_kvp(key, state.to_owned()).await
    });
}

fn finish_first_launch_setup(cx: &mut App) {
    record_first_launch_setup("completed", cx);
}

fn defer_first_launch_setup(cx: &mut App) {
    record_first_launch_setup("deferred", cx);
}

fn register_controller(workspace: &mut Workspace, panel: &Entity<AndroidPanel>, cx: &mut App) {
    let controllers = cx.default_global::<AndroidControllers>();
    controllers
        .0
        .retain(|_, controller| controller.upgrade().is_some());
    controllers
        .0
        .insert(workspace.weak_handle().entity_id(), panel.downgrade());
    let controller = panel.clone();
    workspace.register_action(move |workspace, _: &Setup, window, cx| {
        android_tool_setup::open(workspace, controller.clone(), window, cx);
    });
    let controller = panel.clone();
    workspace.register_action(move |workspace, _: &ToggleFocus, window, cx| {
        android_tool_setup::open(workspace, controller.clone(), window, cx);
    });
}

pub fn controller(workspace: &Workspace, cx: &App) -> Option<Entity<AndroidPanel>> {
    cx.try_global::<AndroidControllers>()?
        .0
        .get(&workspace.weak_handle().entity_id())?
        .upgrade()
}

fn with_panel(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
    callback: impl FnOnce(&mut AndroidPanel, &mut Window, &mut Context<AndroidPanel>) + 'static,
) {
    if let Some(panel) = controller(workspace, cx) {
        // Task scheduling updates the workspace, so wait until its action handler has returned.
        window.defer(cx, move |window, cx| {
            panel.update(cx, |panel, cx| callback(panel, window, cx))
        });
    }
}

pub fn toolbar(workspace: &WeakEntity<Workspace>, cx: &App) -> Option<Entity<AndroidToolbar>> {
    let workspace = workspace.upgrade()?;
    let panel = controller(workspace.read(cx), cx)?;
    Some(panel.read(cx).toolbar.clone())
}

pub fn can_preview_compose(workspace: &Workspace, cx: &App) -> bool {
    controller(workspace, cx).is_some_and(|panel| panel.read(cx).auto_sync_candidate(cx).is_some())
}

#[derive(Clone, Copy)]
enum GradleOperation {
    Build,
    Run,
    Debug,
    Test,
    Lint,
    Kotlin,
    Java,
    Preview,
}

enum AfterTask {
    Deploy(AndroidTarget, String, bool),
    DeployOnEmulator(AndroidTarget, String, bool),
    AttachDebugger(PathBuf, String, String),
    Java(AndroidTarget),
    Preview(AndroidTarget),
    RefreshDevices,
}

const EMULATOR_BOOT_TIMEOUT: Duration = Duration::from_secs(120);
const EMULATOR_START_TIMEOUT: Duration = Duration::from_secs(180);

struct DeviceOperation {
    root: PathBuf,
    serial: String,
    model_token: android_tools::project_model::ModelToken,
    operation_id: u64,
    session_id: u64,
    target: Option<AndroidTarget>,
    lease: Arc<DeviceOperationLease>,
    allow_disconnect: bool,
    selected_avd: Option<String>,
}

#[derive(Clone)]
struct DebugDeployment {
    operation: Arc<DeviceOperation>,
    adb: PathBuf,
    environment: Arc<std::collections::BTreeMap<String, String>>,
    output: futures::channel::mpsc::Sender<android_build::OutputLine>,
}

static DEVICE_OPERATIONS: std::sync::LazyLock<std::sync::Mutex<HashSet<String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashSet::new()));

struct DeviceOperationLease {
    serial: String,
}

impl DeviceOperationLease {
    fn acquire(serial: &str) -> Result<Arc<Self>> {
        let mut operations = DEVICE_OPERATIONS.lock().map_err(|error| {
            anyhow::anyhow!("Could not coordinate Android device operations: {error}")
        })?;
        ensure!(
            operations.insert(serial.to_owned()),
            "Another Koda window is already installing, launching or stopping device {serial}. Wait for that operation to finish and retry."
        );
        Ok(Arc::new(Self {
            serial: serial.to_owned(),
        }))
    }
}

impl Drop for DeviceOperationLease {
    fn drop(&mut self) {
        match DEVICE_OPERATIONS.lock() {
            Ok(mut operations) => {
                operations.remove(&self.serial);
            }
            Err(error) => log::error!("Could not release the Android device operation: {error}"),
        }
    }
}

struct EmulatorStartup {
    root: PathBuf,
    name: String,
    ready: Shared<Task<std::result::Result<(), String>>>,
    error_reported: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OfficialKotlinState {
    Active,
    Paused,
}

#[derive(Clone, PartialEq, Eq)]
struct KotlinPreferences {
    enabled: bool,
    servers: Vec<lsp::LanguageServerName>,
    server: project::project_settings::LspSettings,
}

const UNAVAILABLE_ANDROID_VARIANT: &str = "ZED_ANDROID_VARIANT_UNAVAILABLE";
const PAUSED_JAVA_SERVERS: &str = "KODA_ANDROID_JAVA_PAUSED_SERVERS";

#[derive(serde::Deserialize, serde::Serialize)]
struct JavaPause {
    previous: Option<Vec<String>>,
    paused: Vec<String>,
}

fn java_pause_state(parsed: &serde_json::Value) -> Result<Option<JavaPause>> {
    parsed
        .pointer(&format!("/lsp/jdtls/binary/env/{PAUSED_JAVA_SERVERS}"))
        .and_then(serde_json::Value::as_str)
        .map(|marker| serde_json::from_str(marker).map_err(anyhow::Error::from))
        .transpose()
}

pub struct AndroidPanel {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    toolbar: Entity<AndroidToolbar>,
    build_panel: Entity<BuildPanel>,
    build_task: Option<Task<()>>,
    command_cancel: Option<oneshot::Sender<()>>,
    active_build_session: Option<(BuildTab, u64)>,
    last_build_operation: Option<GradleOperation>,
    next_operation_id: u64,
    active_operation_id: Option<u64>,
    pending_gradle_operation: Option<(PathBuf, GradleOperation)>,
    model_input_roots: Vec<(PathBuf, bool)>,
    followup_model_token: Option<android_tools::project_model::ModelToken>,
    _build_subscription: Subscription,
    root: Option<PathBuf>,
    targets: Vec<AndroidTarget>,
    selected_target: Option<AndroidTarget>,
    devices: Vec<Device>,
    emulators: Vec<String>,
    emulator_error: Option<String>,
    selected_serial: Option<String>,
    selected_avd: Option<String>,
    emulator_serials: HashMap<String, String>,
    emulator_task: Option<Task<()>>,
    emulator_startup: Option<EmulatorStartup>,
    emulator_error_sequence: usize,
    status: SharedString,
    error: Option<String>,
    device_error: Option<String>,
    syncing: bool,
    refreshing_devices: bool,
    running: bool,
    sync_task: Option<Task<()>>,
    device_task: Option<Task<()>>,
    deploy_task: Option<Task<()>>,
    auto_sync_root: Option<PathBuf>,
    startup_settings_ready: bool,
    onboarding_prompted_root: Option<PathBuf>,
    _startup_subscriptions: Vec<Subscription>,
    kotlin_task: Option<Task<()>>,
    kotlin_setup_error: Option<String>,
    kotlin_refresh_task: Option<Task<()>>,
    kotlin_refresh_pending: Option<PathBuf>,
    java_task: Option<Task<()>>,
    debug_task: Option<Task<()>>,
    debug_deployment: Option<DebugDeployment>,
    tool_setup: android_tool_setup::ToolSetup,
    preview_view: Option<WeakEntity<android_preview::ComposePreviewView>>,
    compose_preview_enabled: bool,
    debug_forward: Option<android_debugger::Forward>,
    _debug_subscriptions: Vec<Subscription>,
    java_refresh: Option<(
        PathBuf,
        serde_json::Value,
        android_tools::project_model::ModelToken,
    )>,
    java_status_subscription: Option<lsp::Subscription>,
    _project_subscription: Subscription,
    _project_model_subscription: Subscription,
}

impl AndroidPanel {
    fn new(
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        cx: &mut Context<Self>,
    ) -> Self {
        let build_panel = cx.new(|cx| BuildPanel::new(workspace.clone(), cx));
        let build_subscription =
            cx.subscribe(
                &build_panel,
                |panel, _, event: &BuildEvent, cx| match event {
                    BuildEvent::Stop(tab) => panel.cancel_build(*tab, cx),
                    BuildEvent::Rerun(_) => {}
                },
            );
        let project_subscription = cx.subscribe(&project, |panel, _, event, cx| {
            if let project::Event::LanguageServerAdded(id, name, worktree) = event
                && name.0.as_ref() == "jdtls"
            {
                panel.observe_java_import(*id, *worktree, cx);
            }
        });
        let project_model_subscription = cx.observe(&project, |panel, _, cx| {
            if panel
                .followup_model_token
                .as_ref()
                .is_some_and(|token| !panel.project.read(cx).android_model().is_current(token))
            {
                panel.cancel_model_followup(cx);
                cx.notify();
            }
        });
        let panel = cx.entity();
        let toolbar = cx.new(|cx| AndroidToolbar {
            panel: panel.downgrade(),
            _subscription: cx.observe(&panel, |_, _, cx| cx.notify()),
        });
        let mut panel = Self {
            workspace,
            project,
            toolbar,
            build_panel,
            build_task: None,
            command_cancel: None,
            active_build_session: None,
            last_build_operation: None,
            next_operation_id: 0,
            active_operation_id: None,
            pending_gradle_operation: None,
            model_input_roots: Vec::new(),
            followup_model_token: None,
            _build_subscription: build_subscription,
            root: None,
            targets: Vec::new(),
            selected_target: None,
            devices: Vec::new(),
            emulators: Vec::new(),
            emulator_error: None,
            selected_serial: None,
            selected_avd: None,
            emulator_serials: HashMap::new(),
            emulator_task: None,
            emulator_startup: None,
            emulator_error_sequence: 0,
            status: "Sync an Android project to discover its build variants.".into(),
            error: None,
            device_error: None,
            syncing: false,
            refreshing_devices: false,
            running: false,
            sync_task: None,
            device_task: None,
            deploy_task: None,
            auto_sync_root: None,
            startup_settings_ready: false,
            onboarding_prompted_root: None,
            _startup_subscriptions: Vec::new(),
            kotlin_task: None,
            kotlin_setup_error: None,
            kotlin_refresh_task: None,
            kotlin_refresh_pending: None,
            java_task: None,
            debug_task: None,
            debug_deployment: None,
            tool_setup: android_tool_setup::ToolSetup::new(),
            preview_view: None,
            compose_preview_enabled: false,
            debug_forward: None,
            _debug_subscriptions: Vec::new(),
            java_refresh: None,
            java_status_subscription: None,
            _project_subscription: project_subscription,
            _project_model_subscription: project_model_subscription,
        };
        panel._debug_subscriptions = panel.observe_debugger(cx);
        panel.refresh_tool_setup(cx);
        cx.on_release(|panel, _| {
            if let Some(cancel) = panel.tool_setup.native_cancel.take() {
                cancel.store(true, std::sync::atomic::Ordering::Release);
            }
        })
        .detach();
        panel
    }

    fn observe_first_launch_setup(
        &mut self,
        modal_layer: &Entity<workspace::ModalLayer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        observe_automatic_setup_windows(cx);
        self._startup_subscriptions.push(cx.observe_in(
            modal_layer,
            window,
            |panel, modal_layer, window, cx| {
                if !modal_layer.read(cx).has_active_modal()
                    && cx
                        .global::<FirstLaunchSetup>()
                        .workspace
                        .as_ref()
                        .is_some_and(|workspace| workspace == &panel.workspace)
                {
                    let setup = cx.global_mut::<FirstLaunchSetup>();
                    setup.workspace = None;
                    setup.window = None;
                }
                panel.queue_first_launch_setup(window, cx);
                panel.auto_sync_project(window, cx);
            },
        ));
        self._startup_subscriptions
            .push(
                cx.observe_global_in::<FirstLaunchSetup>(window, |panel, window, cx| {
                    panel.queue_first_launch_setup(window, cx);
                    panel.auto_sync_project(window, cx);
                }),
            );
        self.queue_first_launch_setup(window, cx);
    }

    fn queue_first_launch_setup(&self, window: &mut Window, cx: &mut Context<Self>) {
        if !cx.try_global::<FirstLaunchSetup>().is_some_and(|setup| {
            setup.enabled
                && !setup.resolved
                && setup
                    .workspace
                    .as_ref()
                    .is_none_or(|workspace| workspace.upgrade().is_none())
        }) {
            return;
        }
        let workspace = self.workspace.clone();
        let panel = cx.entity();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    let setup = cx.global::<FirstLaunchSetup>();
                    if !setup.enabled
                        || setup.resolved
                        || setup
                            .workspace
                            .as_ref()
                            .is_some_and(|workspace| workspace.upgrade().is_some())
                        || workspace.has_active_modal(window, cx)
                    {
                        return;
                    }
                    if KeyValueStore::global(cx)
                        .read_kvp(&first_launch_setup_key())
                        .log_err()
                        .flatten()
                        .is_some()
                    {
                        cx.global_mut::<FirstLaunchSetup>().resolved = true;
                        return;
                    }
                    let setup = cx.global_mut::<FirstLaunchSetup>();
                    setup.workspace = Some(workspace.weak_handle());
                    setup.window = Some(window.window_handle().window_id());
                    android_tool_setup::open(workspace, panel, window, cx);
                })
                .log_err();
        });
    }

    fn observe_project_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.observe_compose_preview(window, cx);
        self._startup_subscriptions.push(cx.subscribe_in(
            &self.build_panel,
            window,
            |panel, _, event: &BuildEvent, window, cx| {
                if let BuildEvent::Rerun(tab) = event {
                    match tab {
                        BuildTab::Sync => panel.sync_project(window, cx),
                        BuildTab::Output => {
                            if let Some((tool, operation)) = panel.tool_setup.last_operation {
                                panel.manage_tool(tool, operation, window, cx);
                            } else if let Some(operation) = panel.last_build_operation {
                                panel.gradle(operation, window, cx);
                            }
                        }
                    }
                }
            },
        ));
        self._startup_subscriptions.push(cx.observe_in(
            &cx.entity(),
            window,
            |panel, _, window, cx| {
                if panel.take_official_kotlin_refresh(cx) {
                    panel.sync_project(window, cx);
                } else {
                    panel.resume_pending_gradle_operation(window, cx);
                    panel.auto_sync_project(window, cx);
                }
            },
        ));
        self._startup_subscriptions.push(cx.subscribe_in(
            &self.project,
            window,
            |panel, _, event, window, cx| {
                if let project::Event::WorktreeAdded(worktree_id) = event
                    && let Some(worktree) = panel.project.read(cx).worktree_for_id(*worktree_id, cx)
                    && worktree.read(cx).is_visible()
                {
                    let root = worktree.read(cx).abs_path().to_path_buf();
                    panel.coordinate_kotlin_setup(root, true, window, cx);
                }
                if let project::Event::WorktreeUpdatedEntries(worktree_id, changes) = event {
                    let selected_root_changed = panel
                        .project
                        .read(cx)
                        .worktree_for_id(*worktree_id, cx)
                        .is_some_and(|worktree| {
                            Some(worktree.read(cx).abs_path().as_ref()) == panel.root.as_deref()
                        });
                    if selected_root_changed
                        && changes.iter().any(|(path, _, change)| {
                            *change != project::PathChange::Loaded
                                && panel.model_input_changed(path, cx)
                        })
                    {
                        panel.queue_official_kotlin_refresh(window, cx);
                    }
                }
                if matches!(
                    event,
                    project::Event::WorktreeAdded(_)
                        | project::Event::WorktreeRemoved(_)
                        | project::Event::WorktreeUpdatedEntries(_, _)
                ) {
                    cx.defer_in(window, |panel, window, cx| {
                        panel.auto_sync_project(window, cx)
                    });
                }
            },
        ));
        if let Some(trusted) = TrustedWorktrees::try_get_global(cx) {
            self._startup_subscriptions.push(cx.subscribe_in(
                &trusted,
                window,
                |_, _, _, window, cx| {
                    // Trust events are emitted while the trust store is being updated.
                    cx.defer_in(window, |panel, window, cx| {
                        panel.auto_sync_project(window, cx)
                    });
                },
            ));
        }
        for root in self.roots(cx) {
            self.coordinate_kotlin_setup(root, true, window, cx);
        }
    }

    fn coordinate_kotlin_setup(
        &self,
        root: PathBuf,
        startup: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = self.project.read(cx).lsp_store();
        let settings = store.update(cx, |store, cx| store.wait_for_local_settings(cx));
        let task = cx.spawn_in(window, {
            let root = root.clone();
            async move |panel, cx| {
                settings.await?;
                let (sender, receiver) = oneshot::channel();
                let mut sender = Some(sender);
                let subscription = panel.update_in(cx, |panel, window, cx| {
                    if startup {
                        panel.startup_settings_ready = true;
                        panel.auto_sync_project(window, cx);
                    }
                    let subscription = cx.observe(&cx.entity(), move |panel, _, cx| {
                        if panel.syncing
                            || panel.kotlin_task.is_some()
                            || panel.kotlin_refresh_task.is_some()
                            || panel.kotlin_refresh_pending.is_some()
                        {
                            return;
                        }
                        if let Some(sender) = sender.take() {
                            let result = if !panel.roots(cx).contains(&root) {
                                Err(anyhow::anyhow!(
                                    "The library document's owning workspace was removed"
                                ))
                            } else if panel.root.as_ref() == Some(&root)
                                && panel.selected_target.is_none()
                                && panel.official_kotlin_state(cx).is_some()
                            {
                                Err(anyhow::anyhow!(
                                    "The selected Android variant is no longer available"
                                ))
                            } else if panel.root.as_ref() == Some(&root)
                                && let Some(error) = &panel.kotlin_setup_error
                            {
                                Err(anyhow::anyhow!("{error}"))
                            } else {
                                Ok(())
                            };
                            sender
                                .send(result)
                                .map_err(|_| anyhow::anyhow!("Kotlin setup waiter was dropped"))
                                .log_err();
                        }
                    });
                    cx.notify();
                    subscription
                })?;
                let result = receiver
                    .await
                    .context("Android project setup was cancelled")?;
                drop(subscription);
                result
            }
        });
        store.update(cx, |store, cx| store.set_kotlin_setup_task(root, task, cx));
    }

    fn auto_sync_candidate(&self, cx: &App) -> Option<PathBuf> {
        let root = self.trusted_root(cx).ok()?;
        let worktree = self
            .project
            .read(cx)
            .visible_worktrees(cx)
            .find(|worktree| worktree.read(cx).abs_path().as_ref() == root.as_path())?;
        let snapshot = worktree.read(cx).snapshot();
        let has_file = |name: &str| {
            RelPath::new(Path::new(name), snapshot.path_style())
                .log_err()
                .and_then(|path| snapshot.entry_for_path(&path))
                .is_some_and(|entry| entry.is_file())
        };
        (["gradlew", "gradlew.bat"].into_iter().any(has_file)
            && [
                "settings.gradle.kts",
                "settings.gradle",
                "build.gradle.kts",
                "build.gradle",
            ]
            .into_iter()
            .any(has_file))
        .then_some(root)
    }

    fn cancel_build(&mut self, tab: BuildTab, cx: &mut Context<Self>) {
        let Some((active, id)) = self.active_build_session else {
            return;
        };
        if active != tab {
            return;
        }
        self.active_build_session = None;
        self.pending_gradle_operation = None;
        self.active_operation_id = None;
        self.followup_model_token = None;
        self.command_cancel = None;
        self.sync_task = None;
        self.build_task = None;
        self.deploy_task = None;
        self.debug_task = None;
        self.debug_deployment = None;
        self.tool_setup.operation = None;
        self.emulator_task = None;
        self.emulator_startup = None;
        self.java_task = None;
        if self.kotlin_task.take().is_some() {
            self.kotlin_setup_error = Some("Kotlin setup cancelled".into());
        }
        if self.syncing {
            self.invalidate_model(self.root.clone(), cx);
            self.selected_target = None;
        }
        self.syncing = false;
        self.running = false;
        self.status = "Operation cancelled".into();
        self.build_panel.update(cx, |panel, cx| {
            panel.finish(
                tab,
                id,
                BuildStatus::Cancelled,
                "Operation cancelled".into(),
                cx,
            )
        });
        cx.notify();
    }

    fn auto_sync_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_build_session.is_some() && self.trusted_root(cx).is_err() {
            self.cancel_build(BuildTab::Output, cx);
            self.cancel_build(BuildTab::Sync, cx);
        }
        if !self.startup_settings_ready {
            return;
        }
        if self.root.is_some()
            && self.trusted_root(cx).is_err()
            && self.project.read(cx).android_model().root().is_some()
        {
            self.pending_gradle_operation = None;
            self.active_operation_id = None;
            if let Some((tab, _)) = self.active_build_session {
                self.cancel_build(tab, cx);
            }
            self.deploy_task = None;
            self.debug_task = None;
            self.emulator_task = None;
            self.emulator_startup = None;
            self.invalidate_model(None, cx);
            self.running = false;
            self.targets.clear();
            self.selected_target = None;
            self.auto_sync_root = None;
        }
        if self
            .root
            .as_ref()
            .or_else(|| self.emulator_startup.as_ref().map(|startup| &startup.root))
            .is_some_and(|root| !self.roots(cx).contains(root))
        {
            if let Some(cancel) = self.command_cancel.take() {
                cancel
                    .send(())
                    .map_err(|_| anyhow::anyhow!("Android command already finished"))
                    .log_err();
            }
            if let Some((tab, id)) = self.active_build_session.take() {
                self.build_panel.update(cx, |panel, cx| {
                    panel.finish(
                        tab,
                        id,
                        BuildStatus::Cancelled,
                        "Project closed; operation cancelled".into(),
                        cx,
                    )
                });
                self.sync_task = None;
                if self.java_task.take().is_some() {
                    self.running = false;
                }
                if self.build_task.take().is_some() {
                    self.running = false;
                }
            }
            self.syncing = false;
            if self.emulator_startup.take().is_some() {
                self.emulator_task = None;
                self.running = false;
            }
            self.kotlin_refresh_task = None;
            if self.tool_setup.operation.take().is_some() {
                self.running = false;
            }
            self.kotlin_refresh_pending = None;
            if self.kotlin_task.take().is_some() {
                self.running = false;
            }
            self.invalidate_model(None, cx);
            self.pending_gradle_operation = None;
            self.active_operation_id = None;
            self.deploy_task = None;
            self.debug_task = None;
            self.root = None;
            self.auto_sync_root = None;
            self.targets.clear();
            self.selected_target = None;
            cx.notify();
        }
        if self.syncing || self.running || self.tool_setup.choosing {
            return;
        }
        let Some(root) = self.auto_sync_candidate(cx) else {
            return;
        };
        if cx.try_global::<FirstLaunchSetup>().is_some_and(|setup| {
            (setup.enabled && !setup.resolved)
                || setup
                    .workspace
                    .as_ref()
                    .is_some_and(|workspace| workspace.upgrade().is_some())
        }) {
            if cx.global::<FirstLaunchSetup>().workspace.as_ref() == Some(&self.workspace)
                && self.tool_setup.bootstrap_root.as_ref() == Some(&root)
                && self.tool_setup.bootstrap_ready == Some(false)
            {
                self.onboarding_prompted_root = Some(root);
            }
            return;
        }
        if self.tool_setup.bootstrap_root.as_ref() != Some(&root) {
            self.refresh_tool_setup(cx);
            return;
        }
        if self.tool_setup.bootstrap_ready != Some(true) {
            if self.tool_setup.bootstrap_ready == Some(false)
                && self.onboarding_prompted_root.as_ref() != Some(&root)
            {
                self.open_setup(root, window, cx);
            }
            return;
        }
        if self.auto_sync_root.as_ref() == Some(&root) {
            return;
        }
        self.auto_sync_root = Some(root);
        self.sync_project(window, cx);
    }

    fn roots(&self, cx: &App) -> Vec<PathBuf> {
        self.project
            .read(cx)
            .visible_worktrees(cx)
            .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
            .collect()
    }

    fn trusted_root(&self, cx: &App) -> Result<PathBuf> {
        let project = self.project.read(cx);
        ensure!(
            project.is_local(),
            "Android tools currently support local projects only."
        );
        ensure!(
            !TrustedWorktrees::has_restricted_worktrees(&project.worktree_store(), cx),
            "Trust the project using the title bar's Restricted Mode control before running Android tools."
        );
        if let Some(root) = &self.root {
            ensure!(
                self.roots(cx).contains(root),
                "The selected project is no longer open."
            );
            return Ok(root.clone());
        }
        let roots = self.roots(cx);
        ensure!(!roots.is_empty(), "Open an Android Gradle project first.");
        ensure!(
            roots.len() == 1,
            "Select a project folder in the project picker before syncing."
        );
        roots
            .into_iter()
            .next()
            .context("Open an Android Gradle project first.")
    }

    fn open_setup(&self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        let controller = cx.entity();
        window.defer(cx, move |window, cx| {
            let panel = controller.read(cx);
            if panel.auto_sync_candidate(cx).as_ref() != Some(&root)
                || panel.tool_setup.bootstrap_root.as_ref() != Some(&root)
                || panel.tool_setup.bootstrap_ready != Some(false)
                || panel.onboarding_prompted_root.as_ref() == Some(&root)
                || cx.try_global::<FirstLaunchSetup>().is_some_and(|setup| {
                    (setup.enabled && !setup.resolved)
                        || setup
                            .workspace
                            .as_ref()
                            .is_some_and(|workspace| workspace.upgrade().is_some())
                })
            {
                return;
            }
            workspace
                .update(cx, |workspace, cx| {
                    if !workspace.has_active_modal(window, cx) {
                        observe_automatic_setup_windows(cx);
                        let setup = cx.global_mut::<FirstLaunchSetup>();
                        setup.workspace = Some(workspace.weak_handle());
                        setup.window = Some(window.window_handle().window_id());
                        controller.update(cx, |panel, _| {
                            panel.onboarding_prompted_root = Some(root);
                        });
                        android_tool_setup::open(workspace, controller, window, cx);
                    }
                })
                .log_err();
        });
    }

    pub(crate) fn bootstrap_completed(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.auto_sync_root = None;
        self.error = None;
        self.refresh_tool_setup(cx);
        self.refresh_devices(cx);
        let other_controllers = cx
            .try_global::<AndroidControllers>()
            .into_iter()
            .flat_map(|controllers| controllers.0.values())
            .filter(|controller| controller.entity_id() != cx.entity_id())
            .cloned()
            .collect::<Vec<_>>();
        cx.defer(move |cx| {
            for controller in other_controllers {
                if let Some(controller) = controller.upgrade() {
                    controller.update(cx, |panel, cx| {
                        panel.auto_sync_root = None;
                        panel.refresh_tool_setup(cx);
                        panel.refresh_devices(cx);
                        cx.notify();
                    });
                }
            }
        });
        cx.notify();
    }

    pub(crate) fn managed_tool_succeeded(
        &mut self,
        tool: android_tools::managed::Tool,
        operation: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if tool == android_tools::managed::Tool::Kotlin
            && matches!(operation, "install" | "rollback")
        {
            // Existing project settings can still name the previous generation.
            // A fresh sync selects the verified runtime and restarts open buffers.
            self.bootstrap_completed(window, cx);
            self.recover_kotlin_buffers(cx);
            let others = cx
                .try_global::<AndroidControllers>()
                .into_iter()
                .flat_map(|controllers| controllers.0.values())
                .filter(|controller| controller.entity_id() != cx.entity_id())
                .cloned()
                .collect::<Vec<_>>();
            cx.defer(move |cx| {
                for controller in others {
                    if let Some(controller) = controller.upgrade() {
                        controller.update(cx, |panel, cx| panel.recover_kotlin_buffers(cx));
                    }
                }
            });
        } else {
            self.refresh_tool_setup(cx);
        }
    }

    fn recover_kotlin_buffers(&self, cx: &mut Context<Self>) {
        let project = self.project.read(cx);
        if !project.is_local()
            || TrustedWorktrees::has_restricted_worktrees(&project.worktree_store(), cx)
        {
            return;
        }
        let roots = project
            .visible_worktrees(cx)
            .map(|worktree| {
                (
                    worktree.read(cx).id(),
                    worktree.read(cx).abs_path().to_path_buf(),
                )
            })
            .collect::<HashMap<_, _>>();
        let buffers = project
            .buffer_store()
            .read(cx)
            .buffers()
            .filter(|buffer| {
                let buffer = buffer.read(cx);
                if !buffer
                    .language()
                    .is_some_and(|language| language.name().as_ref() == "Kotlin")
                {
                    return false;
                }
                let Some(file) = buffer.file() else {
                    return false;
                };
                let Some(root) = roots.get(&file.worktree_id(cx)) else {
                    return false;
                };
                let kotlin = language::language_settings::LanguageSettings::for_buffer(buffer, cx);
                if !kotlin.enable_language_server
                    || kotlin.customized_language_servers(&[lsp::LanguageServerName(
                        "kotlin-lsp".into(),
                    )]) != vec![lsp::LanguageServerName("kotlin-lsp".into())]
                {
                    return false;
                }
                let location = settings::SettingsLocation {
                    worktree_id: file.worktree_id(cx),
                    path: file.path().as_ref(),
                };
                let settings = project::project_settings::ProjectSettings::get(Some(location), cx);
                settings
                    .lsp
                    .get(&lsp::LanguageServerName("kotlin-lsp".into()))
                    .is_none_or(|server| {
                        default_kotlin_server_settings(server)
                            || server.binary.as_ref().is_some_and(|binary| {
                                official_kotlin_binary_is_managed(binary, root)
                            })
                    })
            })
            .collect::<Vec<_>>();
        if buffers.is_empty() {
            return;
        }
        let store = project.lsp_store();
        // Empty selectors restrict stopping to these eligible Kotlin buffers;
        // named selectors would stop custom Kotlin servers in unrelated roots.
        store
            .update(cx, |store, cx| {
                store.restart_language_servers_for_buffers_task(
                    buffers,
                    Default::default(),
                    false,
                    cx,
                )
            })
            .detach_and_log_err(cx);
    }

    fn fail(&mut self, error: anyhow::Error, window: &mut Window, cx: &mut Context<Self>) {
        let message = format!("{error:#}");
        self.error = Some(message.clone());
        self.status = "Android operation failed".into();
        let workspace = self.workspace.clone();
        window.defer(cx, move |_, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.show_toast(
                        Toast::new(NotificationId::unique::<AndroidFailure>(), message)
                            .on_click("Android Setup", |window, cx| {
                                window.dispatch_action(Setup.boxed_clone(), cx)
                            }),
                        cx,
                    );
                })
                .log_err();
        });
        cx.notify();
    }

    fn sync_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.syncing || self.running || self.tool_setup.choosing {
            return;
        }
        let root = match self.trusted_root(cx) {
            Ok(root) => root,
            Err(error) => {
                self.fail(error, window, cx);
                return;
            }
        };
        self.coordinate_kotlin_setup(root.clone(), false, window, cx);
        self.root = Some(root.clone());
        self.auto_sync_root = Some(root.clone());
        self.syncing = true;
        let model_token = self.invalidate_model(Some(root.clone()), cx);
        self.targets.clear();
        self.error = None;
        self.kotlin_setup_error = None;
        self.status = "Syncing Android project…".into();
        self.refresh_devices(cx);
        let (session_id, output, logs) = self.build_panel.update(cx, |panel, cx| {
            panel.begin(
                BuildTab::Sync,
                format!(
                    "Sync {}",
                    root.file_name().unwrap_or_default().to_string_lossy()
                ),
                false,
                window,
                cx,
            )
        });
        let (cancel, cancelled) = oneshot::channel();
        self.command_cancel = Some(cancel);
        self.active_build_session = Some((BuildTab::Sync, session_id));
        let executor = cx.background_executor().clone();
        self.sync_task = Some(cx.spawn_in(window, async move |panel, cx| {
            let expected_root = root.clone();
            let result = cx
                .background_spawn(async move {
                    ensure!(
                        is_gradle_project(&root),
                        "This folder has no Gradle wrapper. Open the project's Gradle root."
                    );
                    let init = android_tools::project_model::prepare()?;
                    let program = if cfg!(windows) {
                        root.join("gradlew.bat")
                    } else {
                        PathBuf::from("/bin/sh")
                    };
                    let mut arguments = if cfg!(windows) {
                        Vec::new()
                    } else {
                        vec!["./gradlew".to_owned()]
                    };
                    arguments.extend([
                        "--init-script".into(),
                        init.path()
                            .join("export.gradle")
                            .to_string_lossy()
                            .into_owned(),
                        android_tools::project_model::MODEL_TASK.into(),
                        "--no-configuration-cache".into(),
                        "--console=plain".into(),
                    ]);
                    let mut command = util::command::new_std_command(program);
                    command
                        .args(arguments)
                        .current_dir(&root)
                        .envs(android_tools::managed::command_environment()?);
                    match android_build::command_output(
                        command,
                        &executor,
                        Duration::from_secs(300),
                        output,
                        cancelled,
                        true,
                    )
                    .await?
                    {
                        ProcessOutput::Success(output) => {
                            android_tools::project_model::parse_model(&output, &root).map(Some)
                        }
                        ProcessOutput::Cancelled => Ok(None),
                    }
                })
                .await;
            logs.await;
            panel
                .update_in(cx, |panel, window, cx| {
                    if panel.active_build_session != Some((BuildTab::Sync, session_id)) {
                        return;
                    }
                    panel.active_build_session = None;
                    panel.command_cancel = None;
                    panel.syncing = false;
                    let result = result.and_then(|targets| {
                        ensure!(
                            panel.trusted_root(cx)? == expected_root
                                && panel
                                    .project
                                    .read(cx)
                                    .android_model()
                                    .is_current(&model_token),
                            "The Android project changed during sync"
                        );
                        Ok(targets)
                    });
                    let status = match &result {
                        Ok(Some(_)) => BuildStatus::Succeeded,
                        Ok(None) => BuildStatus::Cancelled,
                        Err(_) => BuildStatus::Failed,
                    };
                    let message = match &result {
                        Ok(Some(targets)) => {
                            let mut message = format!(
                                "Sync successful: {} build variants discovered",
                                targets.targets().len()
                            );
                            if !targets.diagnostics.is_empty() {
                                message.push('\n');
                                message.push_str(&targets.diagnostics.join("\n"));
                            }
                            message
                        }
                        Ok(None) => "Sync cancelled".into(),
                        Err(error) => format!("{error:#}"),
                    };
                    panel.build_panel.update(cx, |panel, cx| {
                        panel.finish(BuildTab::Sync, session_id, status, message, cx)
                    });
                    match result {
                        Ok(Some(model)) => {
                            let diagnostics = model.diagnostics.join("\n");
                            panel.model_input_roots =
                                model
                                    .modules
                                    .iter()
                                    .flat_map(|module| &module.variants)
                                    .flat_map(|variant| &variant.components)
                                    .flat_map(|component| &component.sources)
                                    .filter(|source| {
                                        matches!(source.kind,
                                    android_tools::project_model::SourceKind::Resources
                                    | android_tools::project_model::SourceKind::Manifest)
                                    })
                                    .filter_map(|source| {
                                        source.path.strip_prefix(&model.root).ok().map(|relative| {
                                            (expected_root.join(relative), source.generated)
                                        })
                                    })
                                    .collect();
                            let targets = model.targets();
                            if let Err(error) = panel.project.update(cx, |project, cx| {
                                project.publish_android_model(&model_token, model, cx)
                            }) {
                                panel.fail(error, window, cx);
                                return;
                            }
                            panel.apply_targets(targets, cx);
                            if panel.targets.is_empty() && !diagnostics.is_empty() {
                                panel.status = diagnostics.into();
                                panel.pending_gradle_operation = None;
                            }
                            if let Err(error) = panel.publish_selection(cx) {
                                panel.selected_target = None;
                                panel.pending_gradle_operation = None;
                                panel.fail(error, window, cx);
                                return;
                            }
                            if panel.should_configure_official_kotlin(cx) {
                                if let Some(target) = panel.selected_target.clone() {
                                    panel.configure_official_kotlin(target, true, window, cx);
                                } else if let Err(error) =
                                    panel.pause_official_kotlin(&expected_root, cx)
                                {
                                    panel.fail(error, window, cx);
                                }
                            } else if android_tools::java::is_configured(&expected_root)
                                && let Some(target) = panel.selected_target.clone()
                            {
                                panel.configure_java(target, window, cx);
                            }
                            cx.notify();
                        }
                        Ok(None) => {
                            panel.status = "Sync cancelled".into();
                            cx.notify();
                        }
                        Err(error) => {
                            panel.selected_target = None;
                            // Input changes during discovery already queued another sync.
                            if panel.kotlin_refresh_task.is_none()
                                && panel.kotlin_refresh_pending.is_none()
                            {
                                panel.pending_gradle_operation = None;
                            }
                            if panel.official_kotlin_state(cx).is_some() {
                                panel.pause_official_kotlin(&expected_root, cx).log_err();
                            }
                            panel.kotlin_setup_error = Some(format!("{error:#}"));
                            panel.fail(error, window, cx);
                        }
                    }
                })
                .log_err();
        }));
        cx.notify();
    }

    fn refresh_devices(&mut self, cx: &mut Context<Self>) {
        if self.refreshing_devices {
            return;
        }
        self.refreshing_devices = true;
        let executor = cx.background_executor().clone();
        self.device_task = Some(cx.spawn(async move |panel, cx| {
            let (devices, emulators) = cx
                .background_spawn(async move {
                    futures::join!(connected_devices(&executor), async {
                        let output = tool_output(
                            emulator_path()?,
                            vec!["-list-avds".into()],
                            Path::new("."),
                            &executor,
                            Duration::from_secs(15),
                        )
                        .await?;
                        parse_emulators(&output)
                    })
                })
                .await;
            panel
                .update(cx, |panel, cx| {
                    panel.refreshing_devices = false;
                    match devices {
                        Ok((devices, emulator_serials)) => {
                            panel.device_error = None;
                            if let Some(name) = &panel.selected_avd {
                                panel.selected_serial = emulator_serials.get(name).cloned();
                            } else if panel.selected_serial.is_none() {
                                let available = devices
                                    .iter()
                                    .filter(|device| device.is_available())
                                    .collect::<Vec<_>>();
                                if available.len() == 1 {
                                    panel.selected_serial =
                                        available.first().map(|device| device.serial.clone());
                                }
                            }
                            panel.devices = devices;
                            panel.emulator_serials = emulator_serials;
                        }
                        Err(error) => {
                            panel.devices.clear();
                            panel.emulator_serials.clear();
                            panel.device_error = Some(format!("{error:#}"));
                        }
                    }
                    match emulators {
                        Ok(emulators) => {
                            panel.emulators = emulators;
                            panel.emulator_error = None;
                        }
                        Err(error) => {
                            panel.emulators.clear();
                            panel.emulator_error = Some(format!("{error:#}"));
                        }
                    }
                    cx.notify();
                })
                .log_err();
        }));
        cx.notify();
    }

    fn selected_device(&self) -> Result<&Device> {
        self.devices.iter().find(|device| Some(&device.serial) == self.selected_serial.as_ref() && device.is_available())
            .context("Select an available Android device. Start an emulator or connect a device, then refresh the device list.")
    }

    fn can_run_on_selected_device(&self) -> bool {
        self.selected_device().is_ok()
            || self
                .selected_avd
                .as_ref()
                .is_some_and(|name| self.emulators.contains(name))
    }

    fn gradle(&mut self, operation: GradleOperation, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(operation, GradleOperation::Preview) {
            self.show_compose_preview(window, cx);
            return;
        }
        if self.running || self.syncing || self.tool_setup.choosing {
            return;
        }
        let result = (|| {
            let root = self.trusted_root(cx)?;
            if self.model_inputs_dirty(cx) {
                self.save_and_sync_for_operation(root, operation, window, cx);
                return Ok(());
            }
            if self.kotlin_refresh_task.is_some() || self.kotlin_refresh_pending.is_some() {
                self.pending_gradle_operation = Some((root, operation));
                cx.notify();
                return Ok(());
            }
            let target = self
                .selected_target
                .clone()
                .context("Sync the Android project and select a build variant first.")?;
            self.validate_model_target(&target, cx)?;
            if matches!(operation, GradleOperation::Debug) {
                ensure!(
                    self.debug_forward.is_none(),
                    "Disconnect the current Android debug session before starting another."
                );
            }
            let after_task = match operation {
                GradleOperation::Run | GradleOperation::Debug => {
                    let debug = matches!(operation, GradleOperation::Debug);
                    if let Some(name) = &self.selected_avd {
                        ensure!(
                            self.selected_device().is_ok() || self.emulators.contains(name),
                            "Refresh devices and select an available emulator first."
                        );
                        Some(AfterTask::DeployOnEmulator(
                            target.clone(),
                            name.clone(),
                            debug,
                        ))
                    } else {
                        Some(AfterTask::Deploy(
                            target.clone(),
                            self.selected_device()?.serial.clone(),
                            debug,
                        ))
                    }
                }
                GradleOperation::Java => Some(AfterTask::Java(target.clone())),
                GradleOperation::Preview => Some(AfterTask::Preview(target.clone())),
                _ => None,
            };
            let (name, gradle_task) = match operation {
                GradleOperation::Build
                | GradleOperation::Run
                | GradleOperation::Debug
                | GradleOperation::Java
                | GradleOperation::Preview => ("Build", target.gradle_task("assemble", "")),
                GradleOperation::Kotlin => {
                    self.configure_official_kotlin(target, false, window, cx);
                    return Ok(());
                }
                GradleOperation::Test => ("Test", target.gradle_task("test", "UnitTest")),
                GradleOperation::Lint => ("Lint", target.gradle_task("lint", "")),
            };
            let (program, args) = if cfg!(windows) {
                (
                    root.join("gradlew.bat"),
                    vec![gradle_task, "--console=plain".into()],
                )
            } else {
                (
                    PathBuf::from("/bin/sh"),
                    vec!["./gradlew".into(), gradle_task, "--console=plain".into()],
                )
            };
            self.last_build_operation = Some(operation);
            let emulator = match &after_task {
                Some(AfterTask::DeployOnEmulator(_, name, _)) => Some(name.clone()),
                _ => None,
            };
            let emulator_root = root.clone();
            self.schedule_build(
                format!(
                    "{name} {}",
                    root.file_name().unwrap_or_default().to_string_lossy()
                ),
                program,
                args,
                root,
                after_task,
                window,
                cx,
            )?;
            if let Some(name) = emulator {
                self.start_emulator_background(name, emulator_root, cx);
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.fail(error, window, cx);
        }
    }

    fn model_input_changed(&self, path: &RelPath, _cx: &App) -> bool {
        self.root
            .as_ref()
            .and_then(|root| {
                let absolute = root.join(path.as_std_path());
                self.model_input_roots
                    .iter()
                    .filter(|(source, _)| absolute.starts_with(source))
                    .max_by_key(|(source, _)| source.components().count())
                    .map(|(_, generated)| !generated)
            })
            .unwrap_or_else(|| android_model_input(path))
    }

    fn model_inputs_dirty(&self, cx: &App) -> bool {
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
                            .is_some_and(|worktree| {
                                Some(worktree.read(cx).abs_path().as_ref()) == self.root.as_deref()
                            })
                            && self.model_input_changed(file.path(), cx)
                    })
            })
    }

    fn save_and_sync_for_operation(
        &mut self,
        root: PathBuf,
        operation: GradleOperation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_gradle_operation = Some((root.clone(), operation));
        self.next_operation_id += 1;
        let operation_id = self.next_operation_id;
        self.active_operation_id = Some(operation_id);
        self.running = true;
        self.status = "Saving Android project inputs before syncing…".into();
        let workspace = self.workspace.clone();
        self.build_task = Some(cx.spawn_in(window, async move |panel, cx| {
            Workspace::save_for_task(&workspace, SaveStrategy::All, cx).await;
            panel
                .update_in(cx, |panel, window, cx| {
                    if panel.active_operation_id != Some(operation_id) {
                        return;
                    }
                    panel.active_operation_id = None;
                    panel.running = false;
                    if panel
                        .pending_gradle_operation
                        .as_ref()
                        .is_none_or(|pending| pending.0 != root)
                    {
                        return;
                    }
                    if let Err(error) = panel.trusted_root(cx).and_then(|current| {
                        ensure!(current == root, "The Android project changed while saving");
                        ensure!(
                            !panel.model_inputs_dirty(cx),
                            "Save the Android project inputs before building"
                        );
                        Ok(())
                    }) {
                        panel.pending_gradle_operation = None;
                        panel.fail(error, window, cx);
                        return;
                    }
                    panel.kotlin_refresh_task = None;
                    panel.kotlin_refresh_pending = None;
                    panel.sync_project(window, cx);
                })
                .log_err();
        }));
        cx.notify();
    }

    fn resume_pending_gradle_operation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tool_setup.choosing
            || self.running
            || self.syncing
            || self.kotlin_refresh_task.is_some()
            || self.kotlin_refresh_pending.is_some()
        {
            return;
        }
        let Some((root, operation)) = self.pending_gradle_operation.as_ref() else {
            return;
        };
        if !self.trusted_root(cx).is_ok_and(|current| &current == root) {
            self.pending_gradle_operation = None;
            return;
        }
        if self.project.read(cx).android_model().selected.is_none() {
            return;
        }
        let operation = *operation;
        self.pending_gradle_operation = None;
        self.gradle(operation, window, cx);
    }

    fn validate_model_target(&self, target: &AndroidTarget, cx: &App) -> Result<()> {
        self.project
            .read(cx)
            .android_model()
            .selected
            .as_ref()
            .context("Sync and select an Android variant before running Android tools")?
            .validate_target(target)
    }

    fn schedule_build(
        &mut self,
        label: String,
        program: PathBuf,
        args: Vec<String>,
        root: PathBuf,
        after_task: Option<AfterTask>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.tool_setup.last_operation = None;
        let operation = self.last_build_operation;
        let worktree_id = self
            .project
            .read(cx)
            .visible_worktrees(cx)
            .find(|worktree| worktree.read(cx).abs_path().as_ref() == root.as_path())
            .map(|worktree| worktree.read(cx).id())
            .context("The Android project is no longer open.")?;
        let environment =
            self.project
                .read(cx)
                .environment()
                .clone()
                .update(cx, |environment, cx| {
                    environment.local_directory_environment(
                        &task::Shell::Program(util::get_system_shell()),
                        Arc::from(root.as_path()),
                        cx,
                    )
                });
        let terminal_environment = self
            .project
            .read(cx)
            .terminal_settings(&Some(root.clone()), cx)
            .env
            .clone();
        let (session_id, output, logs) = self.build_panel.update(cx, |panel, cx| {
            panel.begin(BuildTab::Output, label.clone(), false, window, cx)
        });
        let (cancel, cancelled) = oneshot::channel();
        self.command_cancel = Some(cancel);
        self.active_build_session = Some((BuildTab::Output, session_id));
        let model_token = self.project.read(cx).android_model().token();
        self.running = true;
        self.error = None;
        self.status = label.into();
        let workspace = self.workspace.clone();
        let executor = cx.background_executor().clone();
        self.build_task = Some(cx.spawn_in(window, async move |panel, cx| {
            Workspace::save_for_task(&workspace, SaveStrategy::All, cx).await;
            let mut environment = environment.await.unwrap_or_default();
            environment.extend(terminal_environment);
            let valid = panel
                .read_with(cx, |panel, cx| {
                    panel.trusted_root(cx).and_then(|current| {
                        ensure!(
                            current == root,
                            "The Android project changed before building"
                        );
                        ensure!(
                            panel
                                .project
                                .read(cx)
                                .android_model()
                                .is_current(&model_token),
                            "The Android project model changed before building"
                        );
                        Ok(())
                    })
                })
                .and_then(|result| result);
            let result = match valid {
                Ok(()) => {
                    cx.background_spawn({
                        let root = root.clone();
                        async move {
                            match operation {
                                Some(GradleOperation::Debug) => {
                                    android_debugger::binary()?;
                                    android_tools::kotlin::java_home()?;
                                }
                                Some(GradleOperation::Preview) => {
                                    android_tools::preview::installation()?;
                                    android_tools::kotlin::java_home()?;
                                }
                                _ => {}
                            }
                            let mut command = util::command::new_std_command(program);
                            command.args(args).current_dir(&root).envs(
                                android_tools::managed::command_environment_with(
                                    environment.iter(),
                                )?,
                            );
                            android_build::command_output(
                                command,
                                &executor,
                                Duration::from_secs(3600),
                                output,
                                cancelled,
                                false,
                            )
                            .await
                        }
                    })
                    .await
                }
                Err(error) => {
                    drop(output);
                    Err(error)
                }
            };
            logs.await;
            panel
                .update_in(cx, |panel, window, cx| {
                    panel.complete_build(
                        &root,
                        worktree_id,
                        session_id,
                        &model_token,
                        after_task,
                        result,
                        window,
                        cx,
                    );
                })
                .log_err();
        }));
        cx.notify();
        Ok(())
    }

    fn complete_build(
        &mut self,
        root: &Path,
        worktree_id: WorktreeId,
        session_id: u64,
        model_token: &android_tools::project_model::ModelToken,
        after_task: Option<AfterTask>,
        result: Result<ProcessOutput>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_build_session != Some((BuildTab::Output, session_id)) {
            return;
        }
        self.command_cancel = None;
        let (status, result, message) = match result {
            Ok(ProcessOutput::Success(_)) => (
                BuildStatus::Succeeded,
                ScheduledTaskResult::Success,
                "Build completed successfully".into(),
            ),
            Ok(ProcessOutput::Cancelled) => (
                BuildStatus::Cancelled,
                ScheduledTaskResult::Cancelled,
                "Build cancelled".into(),
            ),
            Err(error) => (
                BuildStatus::Failed,
                ScheduledTaskResult::Failure,
                format!("{error:#}"),
            ),
        };
        if !matches!(result, ScheduledTaskResult::Success)
            || !matches!(&after_task, Some(AfterTask::DeployOnEmulator(..)))
        {
            self.active_build_session = None;
        }
        self.build_panel.update(cx, |panel, cx| {
            panel.finish(BuildTab::Output, session_id, status, message, cx)
        });
        self.complete_scheduled_task(
            root,
            worktree_id,
            model_token,
            after_task,
            result,
            window,
            cx,
        );
    }

    fn complete_terminal_task(
        &mut self,
        operation_id: u64,
        root: &Path,
        worktree_id: WorktreeId,
        model_token: &android_tools::project_model::ModelToken,
        after_task: Option<AfterTask>,
        result: ScheduledTaskResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_operation_id != Some(operation_id) {
            return;
        }
        self.active_operation_id = None;
        self.complete_scheduled_task(
            root,
            worktree_id,
            model_token,
            after_task,
            result,
            window,
            cx,
        );
    }

    fn complete_scheduled_task(
        &mut self,
        root: &Path,
        worktree_id: WorktreeId,
        model_token: &android_tools::project_model::ModelToken,
        after_task: Option<AfterTask>,
        result: ScheduledTaskResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.running = false;
        match result {
            ScheduledTaskResult::Success => {
                // A removed root can leave another workspace as trusted_root's
                // fallback. Never continue the original operation in that root.
                let context = (|| {
                    ensure!(
                        self.trusted_root(cx)? == root,
                        "The selected Android project changed during the task."
                    );
                    ensure!(
                        self.project
                            .read(cx)
                            .android_model()
                            .is_current(model_token),
                        "The Android project model changed during the task. Sync and retry."
                    );
                    ensure!(
                        self.project
                            .read(cx)
                            .worktree_for_id(worktree_id, cx)
                            .is_some_and(|worktree| {
                                worktree.read(cx).is_visible()
                                    && worktree.read(cx).abs_path().as_ref() == root
                            }),
                        "The original Android project is no longer open."
                    );
                    let target = match &after_task {
                        Some(AfterTask::Deploy(target, _, _))
                        | Some(AfterTask::DeployOnEmulator(target, _, _))
                        | Some(AfterTask::Java(target))
                        | Some(AfterTask::Preview(target)) => Some(target),
                        _ => None,
                    };
                    if let Some(target) = target {
                        ensure!(
                            self.selected_target.as_ref() == Some(target)
                                && self.targets.contains(target),
                            "The selected Android variant changed during the task."
                        );
                    }
                    Ok::<_, anyhow::Error>(())
                })();
                if let Err(error) = context {
                    self.emulator_task = None;
                    self.emulator_startup = None;
                    self.followup_model_token = None;
                    self.clear_emulator_wait(cx);
                    self.fail(error, window, cx);
                    return;
                }
                self.status = "Task completed successfully".into();
                match after_task {
                    Some(AfterTask::Deploy(target, serial, debug)) => {
                        self.deploy(target, serial, debug, model_token.clone(), window, cx)
                    }
                    Some(AfterTask::DeployOnEmulator(target, name, debug)) => self
                        .wait_for_emulator_with_model(
                            name,
                            root.to_path_buf(),
                            Some((target, debug)),
                            Some(model_token.clone()),
                            window,
                            cx,
                        ),
                    Some(AfterTask::AttachDebugger(root, serial, application_id)) => {
                        self.followup_model_token = Some(model_token.clone());
                        self.attach_debugger(root, serial, application_id, window, cx);
                        if let Some(task) = self.debug_task.take() {
                            let model_token = model_token.clone();
                            self.debug_task = Some(cx.spawn_in(window, async move |panel, cx| {
                                task.await;
                                panel
                                    .update_in(cx, |panel, _, cx| {
                                        if panel
                                            .project
                                            .read(cx)
                                            .android_model()
                                            .is_current(&model_token)
                                        {
                                            panel.followup_model_token = None;
                                        }
                                    })
                                    .log_err();
                            }));
                        } else {
                            self.followup_model_token = None;
                        }
                    }
                    Some(AfterTask::Java(target)) => self.configure_java(target, window, cx),
                    Some(AfterTask::Preview(target)) => self.generate_preview(target, window, cx),
                    Some(AfterTask::RefreshDevices) => self.refresh_devices(cx),
                    None => {}
                }
            }
            ScheduledTaskResult::Cancelled => {
                self.emulator_task = None;
                self.emulator_startup = None;
                self.status = "Task cancelled".into();
            }
            ScheduledTaskResult::Failure | ScheduledTaskResult::SpawnFailed => {
                self.emulator_task = None;
                self.emulator_startup = None;
                self.fail(
                    anyhow::anyhow!(
                        "Android command failed. See the Build pane or command output for the error and retry after fixing it."
                    ),
                    window,
                    cx,
                );
            }
        }
        cx.notify();
    }

    fn validate_device_operation(&self, operation: &DeviceOperation, cx: &App) -> Result<()> {
        ensure!(
            self.active_operation_id == Some(operation.operation_id)
                && self.active_build_session == Some((BuildTab::Output, operation.session_id)),
            "The Android device operation was cancelled."
        );
        ensure!(
            self.trusted_root(cx)? == operation.root,
            "The selected Android project changed during the device operation."
        );
        ensure!(
            self.project
                .read(cx)
                .android_model()
                .is_current(&operation.model_token),
            "The Android project model changed. Sync and retry the device operation."
        );
        if operation.allow_disconnect {
            ensure!(
                self.selected_avd == operation.selected_avd
                    && self
                        .selected_serial
                        .as_ref()
                        .is_none_or(|serial| *serial == operation.serial)
                    && (self.selected_serial.is_some() || operation.selected_avd.is_some()),
                "The selected emulator changed while stopping. Refresh devices and retry."
            );
        } else {
            ensure!(
                self.selected_device()?.serial == operation.serial,
                "The selected Android device changed. Retry the operation."
            );
        }
        if let Some(target) = &operation.target {
            ensure!(
                self.selected_target.as_ref() == Some(target) && self.targets.contains(target),
                "The selected Android variant changed. Retry the device operation."
            );
            self.validate_model_target(target, cx)?;
        } else if !operation.allow_disconnect {
            self.selected_emulator()?;
        }
        Ok(())
    }

    fn deploy(
        &mut self,
        target: AndroidTarget,
        serial: String,
        debug: bool,
        model_token: android_tools::project_model::ModelToken,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let lease = match DeviceOperationLease::acquire(&serial) {
            Ok(lease) => lease,
            Err(error) => {
                self.fail(error, window, cx);
                return;
            }
        };
        let root = match self.trusted_root(cx) {
            Ok(root) => root,
            Err(error) => {
                self.fail(error, window, cx);
                return;
            }
        };
        let Some(worktree_id) = self
            .project
            .read(cx)
            .visible_worktrees(cx)
            .find(|worktree| worktree.read(cx).abs_path().as_ref() == root)
            .map(|worktree| worktree.read(cx).id())
        else {
            self.fail(
                anyhow::anyhow!("The Android project was closed."),
                window,
                cx,
            );
            return;
        };
        let (session_id, output, logs) = self.build_panel.update(cx, |panel, cx| {
            panel.begin(
                BuildTab::Output,
                if debug {
                    "Android Debug"
                } else {
                    "Android Run"
                }
                .into(),
                true,
                window,
                cx,
            )
        });
        self.next_operation_id += 1;
        let operation = Arc::new(DeviceOperation {
            root,
            serial,
            model_token: model_token.clone(),
            operation_id: self.next_operation_id,
            session_id,
            target: Some(target.clone()),
            lease,
            allow_disconnect: false,
            selected_avd: self.selected_avd.clone(),
        });
        self.active_operation_id = Some(operation.operation_id);
        self.active_build_session = Some((BuildTab::Output, session_id));
        self.running = true;
        self.error = None;
        self.followup_model_token = Some(model_token);
        self.status = "Preparing APK for deployment…".into();
        self.deploy_task = Some(cx.spawn_in(window, async move |panel, cx| {
            let result = async {
                panel.read_with(cx, |panel, cx| panel.validate_device_operation(&operation, cx))??;
                let (adb, aapt2, environment) = cx.background_spawn(async {
                    Ok::<_, anyhow::Error>((adb_path()?, deployment::aapt2_path()?, android_tools::managed::command_environment()?))
                }).await?;
                let environment = Arc::new(environment);
                let properties = device_command(&panel, cx, &operation, adb.clone(), vec!["-s".into(), operation.serial.clone(), "shell".into(), "getprop".into()], environment.clone(), output.clone(), "Checking device compatibility…", Duration::from_secs(15)).await?;
                let api_level = deployment::device_api_level(&properties)?;
                ensure!(api_level.is_none_or(|level| level >= 21), "Run and Debug require Android 5 (API 21) or newer on the selected device.");
                let abis = parse_device_abis(&properties)?;
                let (apk, badging_arguments, install_arguments) = cx.background_spawn({
                    let serial = operation.serial.clone();
                    async move {
                        let apk = target.apk_for_device(&abis)?;
                        let apk_path = apk.paths.first().context("The selected variant has no installable APK.")?;
                        let badging_arguments = deployment::aapt2_arguments(apk_path)?;
                        let install_arguments = deployment::install_arguments(&serial, &apk.paths)?;
                        Ok::<_, anyhow::Error>((apk, badging_arguments, install_arguments))
                    }
                }).await?;
                let badging = device_command(&panel, cx, &operation, aapt2, badging_arguments, environment.clone(), output.clone(), "Reading APK package metadata…", Duration::from_secs(30)).await?;
                let identity = deployment::parse_apk_badging(&badging, apk.application_id.as_deref(), debug)?;
                let legacy_component = if api_level.is_some_and(|level| (21..24).contains(&level)) {
                    Some(identity.launchable_component.clone().context("This Android 5 or 6 device needs one unambiguous launcher in the APK metadata. Rebuild the application with a MAIN/LAUNCHER activity or use Android 7 or newer.")?)
                } else {
                    None
                };
                let installed = device_command(&panel, cx, &operation, adb.clone(), install_arguments, environment.clone(), output.clone(), "Installing APK on the selected device…", Duration::from_secs(180)).await?;
                deployment::validate_install_output(&installed)?;
                let component = if let Some(component) = legacy_component {
                    component
                } else {
                    let resolved = device_command(&panel, cx, &operation, adb.clone(), deployment::resolve_activity_arguments(&operation.serial, &identity.application_id)?, environment.clone(), output.clone(), "Resolving the installed launcher activity…", Duration::from_secs(15)).await?;
                    deployment::parse_launcher(&resolved, &identity.application_id)?
                };
                let launched = device_command(&panel, cx, &operation, adb.clone(), deployment::start_activity_arguments(&operation.serial, &component, debug)?, environment.clone(), output.clone(), if debug { "Launching the application and waiting for the debugger…" } else { "Launching the application…" }, Duration::from_secs(30)).await?;
                if debug {
                    deployment::validate_debug_start_output(&launched)?;
                } else {
                    deployment::validate_start_output(&launched)?;
                }
                panel.read_with(cx, |panel, cx| panel.validate_device_operation(&operation, cx))??;
                if debug {
                    let deployment = DebugDeployment { operation: operation.clone(), adb, environment, output: output.clone() };
                    let ready = panel.update_in(cx, |panel, window, cx| {
                        panel.validate_device_operation(&operation, cx)?;
                        panel.debug_deployment = Some(deployment);
                        panel.complete_scheduled_task(&operation.root, worktree_id, &operation.model_token,
                            Some(AfterTask::AttachDebugger(operation.root.clone(), operation.serial.clone(), identity.application_id.clone())),
                            ScheduledTaskResult::Success, window, cx);
                        let task = panel.debug_task.take().context("Debugger attachment could not start. Check Android setup and retry.")?;
                        let ready = task.shared();
                        let wait = ready.clone();
                        panel.debug_task = Some(cx.spawn(async move |_, _| wait.await));
                        Ok::<_, anyhow::Error>(ready)
                    })??;
                    ready.await;
                    panel.read_with(cx, |panel, cx| {
                        panel.validate_device_operation(&operation, cx)?;
                        if let Some(error) = &panel.error {
                            bail!("{error}");
                        }
                        Ok::<_, anyhow::Error>(())
                    })??;
                }
                Ok::<_, anyhow::Error>(identity.application_id)
            }.await;
            panel.update_in(cx, |panel, _, _| {
                if panel.debug_deployment.as_ref().is_some_and(|deployment| deployment.operation.operation_id == operation.operation_id) {
                    panel.debug_deployment = None;
                }
            }).log_err();
            drop(output);
            logs.await;
            panel.update_in(cx, |panel, window, cx| {
                if panel.active_operation_id != Some(operation.operation_id)
                    || panel.active_build_session != Some((BuildTab::Output, operation.session_id)) {
                    return;
                }
                let result = result.and_then(|application_id| {
                    panel.validate_device_operation(&operation, cx)?;
                    Ok(application_id)
                });
                panel.command_cancel = None;
                panel.active_build_session = None;
                let (status, message) = match &result {
                    Ok(_) => (BuildStatus::Succeeded, if debug { "Debugger attachment started" } else { "Application launched successfully" }.to_owned()),
                    Err(error) if error.is::<android_build::CommandCancelled>() => (BuildStatus::Cancelled, "Deployment cancelled; an APK already installed on the device is preserved.".into()),
                    Err(error) => (BuildStatus::Failed, format!("{error:#}")),
                };
                panel.build_panel.update(cx, |panel, cx| panel.finish(BuildTab::Output, operation.session_id, status, message, cx));
                match result {
                    Ok(_) => {
                        panel.complete_terminal_task(operation.operation_id, &operation.root, worktree_id, &operation.model_token,
                            None, ScheduledTaskResult::Success, window, cx);
                        if panel.error.is_none() {
                            panel.followup_model_token = None;
                            panel.status = if debug { "Android debugger attachment started" } else { "Application launched" }.into();
                        }
                    }
                    Err(error) => {
                        panel.active_operation_id = None;
                        panel.followup_model_token = None;
                        panel.running = false;
                        if error.is::<android_build::CommandCancelled>() {
                            panel.status = "Deployment cancelled".into();
                        } else {
                            panel.fail(error, window, cx);
                        }
                    }
                }
                cx.notify();
            }).log_err();
        }));
        cx.notify();
    }

    fn should_configure_official_kotlin(&self, cx: &App) -> bool {
        self.official_kotlin_state(cx).is_some()
            || (project::lsp_store::managed_kotlin_runtime_enabled(cx)
                && self.default_official_kotlin_eligible(cx))
    }

    /// Adopt Koda's server only after Android discovery, preserving explicit
    /// server, binary and importer choices from both user and project settings.
    fn default_official_kotlin_eligible(&self, cx: &App) -> bool {
        let (Some(root), Some(target)) = (&self.root, &self.selected_target) else {
            return false;
        };
        if !self.trusted_root(cx).is_ok_and(|trusted| trusted == *root)
            || !self.targets.contains(target)
        {
            return false;
        }
        let project = self.project.read(cx);
        let model = project.android_model();
        if model.root() != Some(root.as_path())
            || !model
                .selected
                .as_ref()
                .is_some_and(|selected| selected.validate_target(target).is_ok())
        {
            return false;
        }
        let Some(worktree) = project
            .visible_worktrees(cx)
            .find(|worktree| worktree.read(cx).abs_path().as_ref() == root.as_path())
        else {
            return false;
        };
        let location = settings::SettingsLocation {
            worktree_id: worktree.read(cx).id(),
            path: RelPath::empty(),
        };
        let languages = language::language_settings::AllLanguageSettings::get(Some(location), cx);
        let kotlin = languages.language(Some(location), Some(&"Kotlin".into()), cx);
        let servers =
            kotlin.customized_language_servers(&[lsp::LanguageServerName("kotlin-lsp".into())]);
        if !kotlin.enable_language_server
            || servers.len() != 1
            || servers[0].0.as_ref() != "kotlin-lsp"
        {
            return false;
        }
        let settings = project::project_settings::ProjectSettings::get(Some(location), cx);
        settings
            .lsp
            .get(&lsp::LanguageServerName("kotlin-lsp".into()))
            .is_none_or(default_kotlin_server_settings)
    }

    fn official_kotlin_state(&self, cx: &App) -> Option<OfficialKotlinState> {
        let Some(root) = self.root.as_ref() else {
            return None;
        };
        let Some(worktree) = self
            .project
            .read(cx)
            .visible_worktrees(cx)
            .find(|worktree| worktree.read(cx).abs_path().as_ref() == root.as_path())
        else {
            return None;
        };
        let location = settings::SettingsLocation {
            worktree_id: worktree.read(cx).id(),
            path: RelPath::empty(),
        };
        let languages = language::language_settings::AllLanguageSettings::get(Some(location), cx);
        let kotlin = languages.language(Some(location), Some(&"Kotlin".into()), cx);
        let servers =
            kotlin.customized_language_servers(&[lsp::LanguageServerName("kotlin-lsp".into())]);
        let settings = project::project_settings::ProjectSettings::get(Some(location), cx);
        let binary = settings
            .lsp
            .get(&lsp::LanguageServerName("kotlin-lsp".into()))?
            .binary
            .as_ref()?;
        if !kotlin.enable_language_server || !official_kotlin_binary_is_managed(binary, root) {
            return None;
        }
        if servers.len() == 1 && servers[0].0.as_ref() == "kotlin-lsp" {
            Some(OfficialKotlinState::Active)
        } else if servers.is_empty()
            && binary
                .env
                .as_ref()
                .and_then(|env| env.get(UNAVAILABLE_ANDROID_VARIANT))
                .is_some_and(|value| value == "true")
        {
            Some(OfficialKotlinState::Paused)
        } else {
            None
        }
    }

    fn pause_official_kotlin(&self, root: &Path, cx: &App) -> Result<()> {
        ensure!(
            self.trusted_root(cx)? == root && self.selected_target.is_none(),
            "The Android project or selected variant changed while pausing Kotlin"
        );
        let previous = android_tools::kotlin::read_settings(root)?;
        let updated = paused_official_kotlin_settings(previous.clone(), root, cx)?;
        android_tools::kotlin::finish_official(root, &previous, &updated)
    }

    fn pause_managed_java(&self, cx: &App) -> Result<()> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        if !android_tools::java::is_configured(root) {
            return Ok(());
        }
        let previous = android_tools::kotlin::read_settings(root)?;
        let updated = paused_java_settings(previous.clone(), root, cx)?;
        if previous != updated {
            android_tools::kotlin::finish_official(root, &previous, &updated)?;
        }
        Ok(())
    }

    fn queue_official_kotlin_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.invalidate_model(self.root.clone(), cx);
        if let Some(root) = self.root.clone() {
            self.coordinate_kotlin_setup(root, false, window, cx);
        }
        let root = self.root.clone();
        self.kotlin_refresh_pending = None;
        self.kotlin_refresh_task = Some(cx.spawn_in(window, async move |panel, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            panel
                .update_in(cx, |panel, window, cx| {
                    panel.kotlin_refresh_task = None;
                    panel.kotlin_refresh_pending = root;
                    if panel.take_official_kotlin_refresh(cx) {
                        panel.sync_project(window, cx);
                    }
                    cx.notify();
                })
                .log_err();
        }));
    }

    fn take_official_kotlin_refresh(&mut self, cx: &App) -> bool {
        let Some(root) = &self.kotlin_refresh_pending else {
            return false;
        };
        if self.root.as_ref() != Some(root) || self.trusted_root(cx).is_err() {
            self.kotlin_refresh_pending = None;
            return false;
        }
        if self.running || self.syncing || self.tool_setup.choosing {
            return false;
        }
        self.kotlin_refresh_pending = None;
        true
    }

    fn validate_official_kotlin_target(
        &self,
        root: &Path,
        target: &AndroidTarget,
        cx: &App,
    ) -> Result<()> {
        ensure!(
            self.trusted_root(cx)? == root,
            "The Android project changed during Kotlin setup"
        );
        ensure!(
            self.selected_target.as_ref() == Some(target) && self.targets.contains(target),
            "The selected Android variant changed or is no longer available. Sync and select a current variant before configuring Kotlin."
        );
        Ok(())
    }

    fn publish_official_kotlin_settings(
        &self,
        root: &Path,
        target: &AndroidTarget,
        previous: &str,
        updated: &str,
        cx: &App,
    ) -> Result<()> {
        self.validate_official_kotlin_target(root, target, cx)?;
        android_tools::kotlin::finish_official(root, previous, updated)
    }

    fn kotlin_preferences(&self, root: &Path, cx: &App) -> Option<KotlinPreferences> {
        let project = self.project.read(cx);
        let worktree = project
            .visible_worktrees(cx)
            .find(|worktree| worktree.read(cx).abs_path().as_ref() == root)?;
        let location = settings::SettingsLocation {
            worktree_id: worktree.read(cx).id(),
            path: RelPath::empty(),
        };
        let languages = language::language_settings::AllLanguageSettings::get(Some(location), cx);
        let kotlin = languages.language(Some(location), Some(&"Kotlin".into()), cx);
        let settings = project::project_settings::ProjectSettings::get(Some(location), cx);
        Some(KotlinPreferences {
            enabled: kotlin.enable_language_server,
            servers: kotlin
                .customized_language_servers(&[lsp::LanguageServerName("kotlin-lsp".into())]),
            server: settings
                .lsp
                .get(&lsp::LanguageServerName("kotlin-lsp".into()))
                .cloned()
                .unwrap_or_default(),
        })
    }

    fn validate_automatic_kotlin_preferences(
        &self,
        root: &Path,
        expected: &KotlinPreferences,
        cx: &App,
    ) -> Result<()> {
        ensure!(
            self.should_configure_official_kotlin(cx)
                && self.kotlin_preferences(root, cx).as_ref() == Some(expected),
            "Kotlin preferences changed during automatic setup. Your preferences were preserved; use Configure Kotlin explicitly to change them."
        );
        Ok(())
    }

    fn restart_language_server(&self, root: &Path, name: &str, cx: &mut App) -> Task<Result<()>> {
        let project = self.project.read(cx);
        let Some(worktree_id) = project
            .visible_worktrees(cx)
            .find(|worktree| worktree.read(cx).abs_path().as_ref() == root)
            .map(|worktree| worktree.read(cx).id())
        else {
            return Task::ready(Ok(()));
        };
        let store = project.lsp_store();
        let servers = store
            .read(cx)
            .language_server_statuses()
            .filter(|(_, status)| {
                status.name.0.as_ref() == name && status.worktree == Some(worktree_id)
            })
            .map(|(id, _)| lsp::LanguageServerSelector::Id(id))
            .collect::<collections::HashSet<_>>();
        if servers.is_empty() {
            return Task::ready(Ok(()));
        }
        let buffers = project
            .buffer_store()
            .read(cx)
            .buffers()
            .filter(|buffer| {
                buffer
                    .read(cx)
                    .file()
                    .is_some_and(|file| file.worktree_id(cx) == worktree_id)
            })
            .collect();
        store.update(cx, |store, cx| {
            store.restart_language_servers_for_buffers_task(buffers, servers, true, cx)
        })
    }

    fn configure_official_kotlin(
        &mut self,
        target: AndroidTarget,
        automatic: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use android_tools::kotlin;
        let root = match self.trusted_root(cx) {
            Ok(root) => root,
            Err(error) => {
                self.fail(error, window, cx);
                return;
            }
        };
        if self.running || self.syncing || self.tool_setup.choosing {
            return;
        }
        if let Err(error) = self.validate_official_kotlin_target(&root, &target, cx) {
            self.kotlin_setup_error = Some(format!("{error:#}"));
            self.fail(error, window, cx);
            return;
        }
        let automatic_preferences = if automatic {
            let Some(preferences) = self.kotlin_preferences(&root, cx) else {
                return;
            };
            if let Err(error) = self.validate_automatic_kotlin_preferences(&root, &preferences, cx)
            {
                self.fail(error, window, cx);
                return;
            }
            Some(preferences)
        } else {
            None
        };
        let model_token = self.project.read(cx).android_model().token();
        let selected_model = match self.project.read(cx).android_model().selected.clone() {
            Some(selected) => selected,
            None => {
                self.fail(
                    anyhow::anyhow!("Sync and select an Android variant before configuring Kotlin"),
                    window,
                    cx,
                );
                return;
            }
        };
        if let Err(error) = selected_model.validate_target(&target) {
            self.fail(error, window, cx);
            return;
        }
        let kotlin_variants = selected_model.kotlin_variants();
        self.running = true;
        self.error = None;
        self.coordinate_kotlin_setup(root.clone(), false, window, cx);
        self.kotlin_setup_error = None;
        self.status = "Configuring official Kotlin and generating Android resources…".into();
        let (session_id, output, logs) = self.build_panel.update(cx, |panel, cx| {
            panel.begin(
                BuildTab::Sync,
                "Generating Android resources for Kotlin import".into(),
                true,
                window,
                cx,
            )
        });
        let (cancel, cancelled) = oneshot::channel();
        self.command_cancel = Some(cancel);
        self.active_build_session = Some((BuildTab::Sync, session_id));
        let executor = cx.background_executor().clone();
        self.kotlin_task = Some(cx.spawn_in(window, async move |panel, cx| {
            let result = async {
                let (java_home, server_binary, previous, resource_guard, selection) = cx.background_spawn({
                    let root = root.clone();
                    let selected_model = selected_model.clone();
                    async move {
                        Ok::<_, anyhow::Error>((kotlin::java_home()?, kotlin::official_server_binary()?, kotlin::read_settings(&root)?, kotlin::prepare_official_resource_generation(&root)?, android_tools::project_model::install_selection(&root, &selected_model)?))
                    }
                }).await?;
                let generation_error = cx.background_spawn({
                    let root = root.clone();
                    let target = target.clone();
                    let java_home = java_home.clone();
                    let server_binary = server_binary.clone();
                    let kotlin_variants = kotlin_variants.clone();
                    let selection = selection.clone();
                    async move {
                        let program = if cfg!(windows) { root.join("gradlew.bat") } else { PathBuf::from("/bin/sh") };
                        let mut arguments = if cfg!(windows) { Vec::new() } else { vec!["./gradlew".into()] };
                        let server = server_binary.parent().and_then(Path::parent).context("The Kotlin server has no distribution directory")?;
                        arguments.extend(["--init-script".into(), resource_guard.to_string_lossy().into_owned(), format!("-Dzed.android.kotlinServer={}", server.display()), kotlin::RESOURCE_GENERATION_TASK.into(), "--no-configuration-cache".into(), "--console=plain".into()]);
                        let mut command = util::command::new_std_command(program);
                        command.args(arguments).current_dir(&root).envs(android_tools::managed::command_environment()?).env("JAVA_HOME", &java_home)
                            .env("LSP_ANDROID_MODULE", &target.module).env("LSP_ANDROID_VARIANT", &target.variant)
                            .env("LSP_ANDROID_VARIANTS", &kotlin_variants)
                            .env("LSP_ANDROID_MODEL", &selection);
                        android_build::command_output(command, &executor, Duration::from_secs(300), output, cancelled, false).await?.stdout()
                    }
                }).await.err();
                if generation_error.as_ref().is_some_and(|error| error.is::<android_build::CommandCancelled>()) { return Err(android_build::CommandCancelled.into()); }
                let mut refresh = panel.update_in(cx, |panel, _, cx| {
                    ensure!(panel.project.read(cx).android_model().is_current(&model_token), "Discarded outdated Kotlin setup after Android model changes");
                    panel.validate_official_kotlin_target(&root, &target, cx)?;
                    let updated = official_kotlin_settings(previous.clone(), &root, &target, &java_home, &server_binary, cx)?;
                    let updated = cx.global::<settings::SettingsStore>().new_text_for_update(updated, |content| {
                        let environment = content.project.lsp.0.entry("kotlin-lsp".into()).or_default().binary.get_or_insert_default().env.get_or_insert_default();
                        environment.insert("LSP_ANDROID_VARIANTS".into(), kotlin_variants.clone());
                        environment.insert("LSP_ANDROID_MODEL".into(), selection.to_string_lossy().into_owned());
                    })?;
                    if let Some(preferences) = &automatic_preferences {
                        panel.validate_automatic_kotlin_preferences(&root, preferences, cx)?;
                    }
                    panel.publish_official_kotlin_settings(&root, &target, &previous, &updated, cx)?;
                    let worktree = panel.project.read(cx).visible_worktrees(cx)
                        .find(|worktree| worktree.read(cx).abs_path().as_ref() == root.as_path())
                        .context("The Kotlin workspace was removed")?;
                    let refresh = worktree.read(cx).as_local().context("Kotlin setup requires a local workspace")?
                        .refresh_entries_for_paths(vec![RelPath::from_unix_str(".koda/settings.json")?.into_arc()]);
                    Ok::<_, anyhow::Error>(refresh)
                })??;
                refresh.next().await;
                panel.read_with(cx, |panel, cx| panel.project.read(cx).lsp_store())?
                    .update(cx, |store, cx| store.wait_for_local_settings(cx)).await?;
                panel.update_in(cx, |panel, _, cx| {
                    ensure!(panel.project.read(cx).android_model().is_current(&model_token), "Discarded outdated Kotlin restart after Android model changes");
                    panel.validate_official_kotlin_target(&root, &target, cx)?;
                    Ok::<_, anyhow::Error>(panel.restart_language_server(&root, "kotlin-lsp", cx))
                })??.await?;
                Ok::<_, anyhow::Error>(generation_error)
            }.await;
            logs.await;
            panel.update_in(cx, |panel, window, cx| {
                if panel.active_build_session != Some((BuildTab::Sync, session_id)) { return; }
                panel.active_build_session = None;
                panel.command_cancel = None;
                let (status, message) = match &result {
                    Ok(None) => (BuildStatus::Succeeded, "Kotlin import configured".into()),
                    Ok(Some(error)) => (BuildStatus::Failed, format!("Android resource generation failed: {error:#}")),
                    Err(error) if error.is::<android_build::CommandCancelled>() => (BuildStatus::Cancelled, "Kotlin setup cancelled".into()),
                    Err(error) => (BuildStatus::Failed, format!("{error:#}")),
                };
                panel.build_panel.update(cx, |panel, cx| panel.finish(BuildTab::Sync, session_id, status, message, cx));
                panel.running = false;
                panel.kotlin_task = None;
                let result = result.and_then(|value| {
                    ensure!(panel.project.read(cx).android_model().is_current(&model_token), "Discarded outdated Kotlin setup completion");
                    Ok(value)
                });
                match result {
                    Ok(generation_error) => {
                        panel.status = format!("Official Kotlin {} configured. Open a Kotlin file and wait for import and indexing. Variant and Gradle input changes refresh automatically.", kotlin::OFFICIAL_REVISION).into();
                        panel.error = generation_error.map(|error| format!("Android resource generation failed; generated symbols may be unavailable. Kotlin import can still proceed.\n{error:#}"));
                        if android_tools::java::is_configured(&root) {
                            panel.configure_java(target, window, cx);
                        }
                    }
                    Err(error) => {
                        let cancelled = error.is::<android_build::CommandCancelled>();
                        panel.kotlin_setup_error = Some(format!("{error:#}"));
                        panel.fail(error, window, cx);
                        if automatic && !cancelled {
                            panel.refresh_java_after_kotlin_failure(&root, target, &model_token, window, cx);
                        }
                    }
                }
                cx.notify();
            }).log_err();
        }));
        cx.notify();
    }

    fn refresh_java_after_kotlin_failure(
        &mut self,
        root: &Path,
        target: AndroidTarget,
        model_token: &android_tools::project_model::ModelToken,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .project
            .read(cx)
            .android_model()
            .is_current(model_token)
            && self
                .validate_official_kotlin_target(root, &target, cx)
                .is_ok()
            && android_tools::java::is_configured(root)
        {
            // Keep the Kotlin error while refreshing an already configured Java model.
            self.configure_java(target, window, cx);
        }
    }

    fn configure_java(
        &mut self,
        target: AndroidTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use android_tools::{java, kotlin};
        let root = match self.trusted_root(cx) {
            Ok(root) => root,
            Err(error) => {
                self.fail(error, window, cx);
                return;
            }
        };
        let selected_model = match self.project.read(cx).android_model().selected.clone() {
            Some(selected) => selected,
            None => {
                self.fail(
                    anyhow::anyhow!("Sync and select an Android variant before configuring Java"),
                    window,
                    cx,
                );
                return;
            }
        };
        if let Err(error) = selected_model.validate_target(&target) {
            self.fail(error, window, cx);
            return;
        }
        let model_token = self.project.read(cx).android_model().token();
        self.running = true;
        self.status = "Preparing the selected Android Java model…".into();
        let (session_id, output, logs) = self.build_panel.update(cx, |panel, cx| {
            panel.begin(
                BuildTab::Sync,
                "Importing Android Java model".into(),
                true,
                window,
                cx,
            )
        });
        let (cancel, cancelled) = oneshot::channel();
        self.command_cancel = Some(cancel);
        self.active_build_session = Some((BuildTab::Sync, session_id));
        let executor = cx.background_executor().clone();
        self.java_task = Some(cx.spawn_in(window, async move |panel, cx| {
            let result = async {
                let (models, previous) = cx.background_spawn({
                    let root = root.clone();
                    let target = target.clone();
                    let selected_model = selected_model.clone();
                    async move {
                        let (init, selection) = java::prepare_selected(&root, &selected_model)?;
                        let program = if cfg!(windows) { root.join("gradlew.bat") } else { PathBuf::from("/bin/sh") };
                        let mut args = if cfg!(windows) { Vec::new() } else { vec!["./gradlew".into()] };
                        args.extend(["--init-script".into(), init.to_string_lossy().into_owned(),
                            format!("-Dkoda.android.selection={}", selection.display()),
                            java::MODEL_TASK.into(), "--no-configuration-cache".into(), "--console=plain".into()]);
                        let mut command = util::command::new_std_command(program);
                        command.args(args).current_dir(&root);
                        command.envs(android_tools::managed::command_environment()?);
                        let output = android_build::command_output(command, &executor, Duration::from_secs(300), output, cancelled, true).await?.stdout()?;
                        let models = java::parse_model(&output, &root, &target)?;
                        java::validate_selection(&models, &selected_model)?;
                        Ok::<_, anyhow::Error>((models, kotlin::read_settings(&root)?))
                    }
                }).await?;
                let updated = panel.update_in(cx, |panel, _, cx| {
                    ensure!(panel.trusted_root(cx)? == root && panel.project.read(cx).android_model().is_current(&model_token), "The Android project model changed during Java setup");
                    ensure!(panel.selected_target.as_ref() == Some(&target) && panel.targets.contains(&target),
                        "The selected Android variant changed during Java setup. Retry to refresh the current variant.");
                    java_settings(previous.clone(), &root, cx)
                })??;
                // JDT LS refreshes persisted Gradle arguments only when the root project is updated.
                let uri = lsp::Uri::from_file_path(&root)
                    .map_err(|_| anyhow::anyhow!("Could not create the Java project URI"))?;
                // Check and publish in one foreground turn, as with Kotlin settings, so
                // a variant change cannot race the filesystem commit between awaits.
                panel.update_in(cx, |panel, _, cx| {
                    ensure!(panel.trusted_root(cx)? == root && panel.project.read(cx).android_model().is_current(&model_token), "Discarded outdated Java model publication");
                    java::finish(&root, &models, &previous, &updated)
                })??;
                Ok::<_, anyhow::Error>((root, serde_json::json!({"identifiers": [{"uri": uri}]}), model_token.clone()))
            }.await;
            logs.await;
            panel.update_in(cx, |panel, window, cx| {
                if panel.active_build_session != Some((BuildTab::Sync, session_id)) { return; }
                panel.active_build_session = None;
                panel.command_cancel = None;
                let (status, message) = match &result {
                    Ok(_) => (BuildStatus::Succeeded, "Java model imported".into()),
                    Err(error) if error.is::<android_build::CommandCancelled>() => (BuildStatus::Cancelled, "Java import cancelled".into()),
                    Err(error) => (BuildStatus::Failed, format!("{error:#}")),
                };
                panel.build_panel.update(cx, |panel, cx| panel.finish(BuildTab::Sync, session_id, status, message, cx));
                panel.running = false;
                match result {
                    Ok(refresh) => {
                        if !panel.project.read(cx).android_model().is_current(&refresh.2) {
                            cx.notify();
                            return;
                        }
                        let root = refresh.0.clone();
                        panel.java_refresh = Some(refresh);
                        panel.status = "Java model configured. Official Kotlin setup also refreshes this model after variant and Gradle input changes.".into();
                        panel.restart_language_server(&root, "jdtls", cx).detach_and_log_err(cx);
                    }
                    Err(error) => panel.fail(error, window, cx),
                }
                cx.notify();
            }).log_err();
        }));
        cx.notify();
    }

    fn observe_java_import(
        &mut self,
        id: lsp::LanguageServerId,
        worktree_id: Option<project::WorktreeId>,
        cx: &mut Context<Self>,
    ) {
        let Some((root, parameters, model_token)) = &self.java_refresh else {
            return;
        };
        let project = self.project.read(cx);
        let Some(worktree) = worktree_id.and_then(|id| project.worktree_for_id(id, cx)) else {
            return;
        };
        if worktree.read(cx).abs_path().as_ref() != root {
            return;
        }
        let Some(server) = project.lsp_store().read(cx).language_server_for_id(id) else {
            return;
        };
        let server = Arc::downgrade(&server);
        let root = root.clone();
        let parameters = parameters.clone();
        let model_token = model_token.clone();
        let panel = cx.weak_entity();
        self.java_status_subscription = project
            .lsp_store()
            .read(cx)
            .language_server_for_id(id)
            .map(|language_server| {
                language_server.on_notification::<JavaStatus, _>(move |status, cx| {
                    if status.get("type").and_then(|value| value.as_str()) != Some("ServiceReady") {
                        return;
                    }
                    panel
                        .update(cx, |panel, cx| {
                            if !panel
                                .project
                                .read(cx)
                                .android_model()
                                .is_current(&model_token)
                            {
                                return;
                            }
                            if panel
                                .java_refresh
                                .as_ref()
                                .is_none_or(|refresh| refresh.2 != model_token)
                            {
                                return;
                            }
                            let result = (|| {
                                ensure!(
                                    panel.trusted_root(cx)? == root,
                                    "The Android project changed before Java import"
                                );
                                server
                                    .upgrade()
                                    .context("The Java language server stopped before import")?
                                    .notify::<RefreshJavaProjects>(parameters.clone())
                            })();
                            panel.java_refresh = None;
                            if let Err(error) = result {
                                panel.error = Some(format!("{error:#}"));
                            }
                            cx.notify();
                        })
                        .log_err();
                })
            });
    }

    fn logcat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = (|| {
            let root = self.trusted_root(cx)?;
            let workspace = self.workspace.clone();
            let serial = self.selected_serial.clone();
            let targets = self
                .selected_target
                .clone()
                .map(|target| vec![target])
                .unwrap_or_else(|| self.targets.clone());
            window.defer(cx, move |window, cx| {
                workspace
                    .update(cx, |workspace, cx| {
                        android_logcat::open(workspace, root, serial, targets, window, cx);
                    })
                    .log_err();
            });
            Ok::<_, anyhow::Error>(())
        })();
        if let Err(error) = result {
            self.fail(error, window, cx);
        }
    }

    fn target_selection_key(&self) -> Option<String> {
        let root = serde_json::to_string(self.root.as_ref()?).log_err()?;
        Some(format!("android-selected-target:{root}"))
    }

    fn remember_target(&self, cx: &App) {
        let Some(key) = self.target_selection_key() else {
            return;
        };
        let Some(target) = &self.selected_target else {
            return;
        };
        let Some(value) = serde_json::to_string(&(&target.module, &target.variant)).log_err()
        else {
            return;
        };
        let database = KeyValueStore::global(cx);
        db::write_and_log(
            cx,
            move || async move { database.write_kvp(key, value).await },
        );
    }

    fn invalidate_model(
        &mut self,
        root: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> android_tools::project_model::ModelToken {
        self.pause_managed_java(cx).log_err();
        self.cancel_model_followup(cx);
        if self.root.as_ref() != root.as_ref() {
            self.model_input_roots.clear();
        }
        self.java_refresh = None;
        self.java_status_subscription = None;
        self.project
            .update(cx, |project, cx| project.invalidate_android_model(root, cx))
    }

    fn cancel_model_followup(&mut self, cx: &mut Context<Self>) {
        if self.followup_model_token.take().is_some() {
            if let Some((tab, _)) = self.active_build_session {
                self.cancel_build(tab, cx);
            }
            self.active_operation_id = None;
            self.command_cancel = None;
            self.deploy_task = None;
            self.debug_task = None;
            self.debug_deployment = None;
            self.emulator_task = None;
            self.emulator_startup = None;
            self.clear_emulator_wait(cx);
            self.running = false;
        }
    }

    fn publish_selection(&mut self, cx: &mut Context<Self>) -> Result<()> {
        self.pause_managed_java(cx).log_err();
        self.java_refresh = None;
        self.java_status_subscription = None;
        let id = self
            .selected_target
            .as_ref()
            .map(android_tools::project_model::VariantId::from);
        self.project
            .update(cx, |project, cx| project.select_android_variant(id, cx))
    }

    fn apply_targets(&mut self, targets: Vec<AndroidTarget>, cx: &App) {
        let preferred = self
            .selected_target
            .as_ref()
            .map(|target| (target.module.clone(), target.variant.clone()))
            .or_else(|| {
                let key = self.target_selection_key()?;
                let value = KeyValueStore::global(cx).read_kvp(&key).log_err()??;
                serde_json::from_str::<(String, String)>(&value).log_err()
            });
        // Restore only identity; build tasks and artifact paths must come from the fresh model.
        self.selected_target = if let Some((module, variant)) = &preferred {
            targets
                .iter()
                .find(|target| &target.module == module && &target.variant == variant)
                .cloned()
        } else {
            self.project
                .read(cx)
                .android_model()
                .model
                .as_ref()
                .and_then(|model| model.default_target())
                .filter(|target| targets.contains(target))
                .or_else(|| {
                    // Older model catalogs only expose flattened variant names.
                    targets
                        .iter()
                        .min_by_key(|target| {
                            (
                                !(target.variant == "debug" || target.variant.ends_with("Debug")),
                                &target.module,
                                &target.variant,
                            )
                        })
                        .cloned()
                })
        };
        if preferred.is_none() {
            self.remember_target(cx);
        }
        self.status = if preferred.is_some() && self.selected_target.is_none() {
            "The previous build variant is unavailable. Select a build variant to continue.".into()
        } else {
            format!("Sync complete · {} build variants", targets.len()).into()
        };
        self.targets = targets;
    }

    fn target_picker(&self, id: &'static str, cx: &Context<Self>) -> impl IntoElement {
        let targets = self.targets.clone();
        let root = self.root.clone();
        let panel = cx.weak_entity();
        let label = self
            .selected_target
            .as_ref()
            .map(AndroidTarget::label)
            .unwrap_or_else(|| {
                if self.syncing {
                    "Syncing project…"
                } else {
                    "Select build variant"
                }
                .into()
            });
        PopoverMenu::new(id)
            .trigger(
                Button::new("target", label)
                    .label_size(LabelSize::Small)
                    .end_icon(Icon::new(IconName::ChevronDown).size(IconSize::XSmall))
                    .disabled(self.syncing || self.running || targets.is_empty())
                    .tab_index(0isize),
            )
            .menu(move |window, cx| {
                Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                    for target in &targets {
                        let panel = panel.clone();
                        let target = target.clone();
                        let root = root.clone();
                        menu = menu.entry(target.label(), None, move |window, cx| {
                            panel
                                .update(cx, |panel, cx| {
                                    if panel.running
                                        || panel.syncing
                                        || panel.root != root
                                        || !panel.targets.contains(&target)
                                    {
                                        return;
                                    }
                                    let changed = panel.selected_target.as_ref() != Some(&target);
                                    panel.selected_target = Some(target.clone());
                                    if changed && let Err(error) = panel.publish_selection(cx) {
                                        panel.selected_target = None;
                                        panel.fail(error, window, cx);
                                        return;
                                    }
                                    panel.remember_target(cx);
                                    if changed && panel.should_configure_official_kotlin(cx) {
                                        panel.configure_official_kotlin(
                                            target.clone(),
                                            true,
                                            window,
                                            cx,
                                        );
                                    } else if changed
                                        && let Some(root) = &panel.root
                                        && android_tools::java::is_configured(root)
                                    {
                                        panel.configure_java(target.clone(), window, cx);
                                    }
                                    cx.notify();
                                })
                                .log_err();
                        });
                    }
                    menu
                }))
            })
    }

    fn device_picker(&self, id: &'static str, cx: &Context<Self>) -> impl IntoElement {
        let panel = cx.weak_entity();
        let label = self
            .selected_device()
            .map(|device| device.model.clone())
            .unwrap_or_else(|_| {
                self.selected_avd
                    .clone()
                    .unwrap_or_else(|| "Select device".into())
            });
        PopoverMenu::new(id)
            .trigger(
                Button::new("device", label)
                    .label_size(LabelSize::Small)
                    .end_icon(Icon::new(IconName::ChevronDown).size(IconSize::XSmall))
                    .disabled(self.running)
                    .tab_index(0isize),
            )
            .menu(move |window, cx| Some(Self::device_menu(panel.upgrade()?, window, cx)))
    }

    fn device_menu(panel: Entity<Self>, window: &mut Window, cx: &mut App) -> Entity<ContextMenu> {
        panel.update(cx, |panel, cx| panel.refresh_devices(cx));
        let menu = ContextMenu::build_persistent(window, cx, {
            let panel = panel.clone();
            move |mut menu, _, cx| {
                let state = panel.read(cx);
                if state.refreshing_devices {
                    menu = menu.label("Refreshing devices…");
                } else if state.device_error.is_some() || state.emulator_error.is_some() {
                    menu = menu.label("Device refresh failed. See Android tools for details.");
                }
                for device in &state.devices {
                    if state
                        .emulator_serials
                        .values()
                        .any(|serial| serial == &device.serial)
                    {
                        continue;
                    }
                    let panel = panel.downgrade();
                    let device = device.clone();
                    let label = format!("{} · {} · {}", device.model, device.serial, device.state);
                    menu = menu.toggleable_entry_disabled_when(
                        label,
                        state.selected_avd.is_none()
                            && state.selected_serial.as_ref() == Some(&device.serial),
                        state.refreshing_devices,
                        IconPosition::Start,
                        None,
                        move |_, cx| {
                            panel
                                .update(cx, |panel, cx| {
                                    panel.selected_serial = Some(device.serial.clone());
                                    panel.selected_avd = None;
                                    cx.notify();
                                })
                                .log_err();
                        },
                    );
                }
                if !state.emulators.is_empty() {
                    menu = menu.header("Virtual devices");
                }
                for name in &state.emulators {
                    let panel = panel.downgrade();
                    let serial = state.emulator_serials.get(name).cloned();
                    let label = format!(
                        "{} · {}",
                        name,
                        if serial.is_some() {
                            "running"
                        } else {
                            "stopped"
                        }
                    );
                    let name = name.clone();
                    menu = menu.toggleable_entry_disabled_when(
                        label,
                        state.selected_avd.as_ref() == Some(&name),
                        state.refreshing_devices,
                        IconPosition::Start,
                        None,
                        move |_, cx| {
                            panel
                                .update(cx, |panel, cx| {
                                    panel.selected_avd = Some(name.clone());
                                    panel.selected_serial = serial.clone();
                                    cx.notify();
                                })
                                .log_err();
                        },
                    );
                }
                let panel = panel.downgrade();
                menu.separator()
                    .entry("Refresh devices", None, move |_, cx| {
                        panel
                            .update(cx, |panel, cx| panel.refresh_devices(cx))
                            .log_err();
                    })
                    .keep_open_on_confirm(false)
            }
        });
        menu.update(cx, |_, cx| {
            cx.observe_in(&panel, window, |menu, _, window, cx| {
                menu.rebuild(window, cx);
                // The refresh can reorder rows; never confirm a different device
                // using the keyboard index from the previous list.
                menu.clear_selected();
                menu.select_toggled_or_first(window, cx);
            })
            .detach();
        });
        menu
    }

    fn notify_emulator_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.error = Some(message.clone());
        self.emulator_error_sequence += 1;
        let id = NotificationId::composite::<EmulatorStartup>(SharedString::from(format!(
            "emulator-error-{}",
            self.emulator_error_sequence
        )));
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.show_toast(Toast::new(id, message).autohide(), cx);
                })
                .log_err();
        });
        cx.notify();
    }

    fn start_emulator_background(&mut self, name: String, root: PathBuf, cx: &mut Context<Self>) {
        let environment =
            self.project
                .read(cx)
                .environment()
                .clone()
                .update(cx, |environment, cx| {
                    environment.local_directory_environment(
                        &task::Shell::Program(util::get_system_shell()),
                        Arc::from(root.as_path()),
                        cx,
                    )
                });
        let terminal_environment = self
            .project
            .read(cx)
            .terminal_settings(&Some(root.clone()), cx)
            .env
            .clone();
        let executor = cx.background_executor().clone();
        let ready = cx
            .spawn({
                let name = name.clone();
                let root = root.clone();
                async move |panel, cx| {
                    let running = cx
                        .background_spawn({
                            let executor = executor.clone();
                            let name = name.clone();
                            async move {
                                emulator_is_running_with_adb(&name, adb_path()?, &executor).await
                            }
                        })
                        .await;
                    match running {
                        Ok(true) => return Ok(()),
                        Ok(false) => {}
                        Err(error) => {
                            return Err(format!("Could not check emulator {name}: {error:#}"));
                        }
                    }
                    let mut environment = environment.await.unwrap_or_default();
                    environment.extend(terminal_environment);
                    let valid = panel
                        .read_with(cx, |panel, cx| {
                            ensure!(
                                panel.trusted_root(cx)? == root,
                                "The Android project changed before emulator startup."
                            );
                            Ok::<_, anyhow::Error>(())
                        })
                        .and_then(|result| result);
                    let result = match valid {
                        Ok(()) => {
                            let prepared = cx.background_spawn({
                                let name = name.clone();
                                let root = root.clone();
                                async move {
                                    let environment = android_tools::managed::command_environment_with(environment.iter())?;
                                    let adb = adb_path()?;
                                    let mut command = util::command::new_std_command(emulator_path()?);
                                    command.args(["-avd", &name, "-no-metrics"]).current_dir(&root).envs(environment);
                                    Ok::<_, anyhow::Error>((command, adb))
                                }
                            }).await;
                            match prepared {
                                Ok((command, adb)) => {
                                    let valid = panel.read_with(cx, |panel, cx| {
                                        ensure!(panel.trusted_root(cx)? == root, "The Android project changed before emulator startup.");
                                        ensure!(panel.emulator_startup.as_ref().is_some_and(|startup| startup.root == root && startup.name == name), "Emulator startup was cancelled.");
                                        Ok::<_, anyhow::Error>(())
                                    }).and_then(|result| result);
                                    match valid {
                                        Ok(()) => {
                                            let (cancel, cancelled) = oneshot::channel();
                                            let result = cx.background_spawn({
                                                let name = name.clone();
                                                let executor = executor.clone();
                                                async move { emulator_start_output_with_cancel(command, adb, &name, &executor, cancelled).await }
                                            }).await;
                                            drop(cancel);
                                            result
                                        }
                                        Err(error) => Err(error),
                                    }
                                }
                                Err(error) => Err(error),
                            }
                        }
                        Err(error) => Err(error),
                    };
                    let result = result.and_then(|mut process| {
                        panel.read_with(cx, |panel, cx| {
                            ensure!(panel.trusted_root(cx)? == root, "The Android project changed while the emulator was starting.");
                            ensure!(panel.emulator_startup.as_ref().is_some_and(|startup| startup.root == root && startup.name == name), "Emulator startup was cancelled.");
                            Ok::<_, anyhow::Error>(())
                        })??;
                        process.handoff(&executor)?;
                        Ok(())
                    });
                    result.map_err(|error| format!("Could not start emulator {name}: {error:#}"))
                }
            })
            .shared();
        self.emulator_startup = Some(EmulatorStartup {
            root: root.clone(),
            name: name.clone(),
            ready: ready.clone(),
            error_reported: false,
        });
        self.emulator_task = Some(cx.spawn(async move |panel, cx| {
            if let Err(message) = ready.await {
                panel
                    .update(cx, |panel, cx| {
                        if let Some(startup) = &mut panel.emulator_startup
                            && startup.root == root
                            && startup.name == name
                        {
                            startup.error_reported = true;
                            panel.notify_emulator_error(message, cx);
                        }
                    })
                    .log_err();
            }
        }));
    }

    fn start_emulator(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.running || self.syncing || self.tool_setup.choosing {
            return;
        }
        let result = (|| {
            let root = self.trusted_root(cx)?;
            ensure!(
                self.emulators.contains(&name),
                "Refresh devices and select an available emulator first."
            );
            self.selected_avd = Some(name.clone());
            self.start_emulator_background(name.clone(), root.clone(), cx);
            self.wait_for_emulator(name, root, None, window, cx);
            Ok::<_, anyhow::Error>(())
        })();
        if let Err(error) = result {
            self.notify_emulator_error(format!("{error:#}"), cx);
        }
    }

    fn clear_emulator_wait(&mut self, cx: &mut Context<Self>) {
        if let Some((tab, id)) = self.active_build_session.take() {
            self.build_panel.update(cx, |panel, cx| {
                panel.set_waiting_for_emulator(tab, id, false, cx);
            });
        }
    }

    fn wait_for_emulator(
        &mut self,
        name: String,
        root: PathBuf,
        deployment: Option<(AndroidTarget, bool)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let model_token = deployment
            .as_ref()
            .map(|_| self.project.read(cx).android_model().token());
        self.wait_for_emulator_with_model(name, root, deployment, model_token, window, cx);
    }

    fn wait_for_emulator_with_model(
        &mut self,
        name: String,
        root: PathBuf,
        deployment: Option<(AndroidTarget, bool)>,
        model_token: Option<android_tools::project_model::ModelToken>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(startup) = self
            .emulator_startup
            .as_ref()
            .filter(|startup| startup.name == name && startup.root == root)
        else {
            self.clear_emulator_wait(cx);
            self.notify_emulator_error(
                "Emulator startup is no longer active. Run again to retry.".into(),
                cx,
            );
            return;
        };
        let ready = startup.ready.clone();
        let executor = cx.background_executor().clone();
        self.running = true;
        self.followup_model_token = model_token.clone();
        self.status = format!("Waiting for {name} to finish booting…").into();
        if let Some((tab, id)) = self.active_build_session {
            self.build_panel.update(cx, |panel, cx| {
                panel.set_waiting_for_emulator(tab, id, true, cx);
            });
        }
        self.emulator_task = Some(cx.spawn_in(window, async move |panel, cx| {
            let boot = async {
                cx.background_spawn({
                    let executor = executor.clone();
                    let name = name.clone();
                    async move { booted_emulator(&name, &executor).await }
                }).await
            };
            let result = emulator_ready_with_timeout(&name, ready, boot, &executor, EMULATOR_BOOT_TIMEOUT).await;
            panel.update_in(cx, |panel, window, cx| {
                if model_token.as_ref().is_some_and(|token| !panel.project.read(cx).android_model().is_current(token)) {
                    return;
                }
                let Some(startup) = panel.emulator_startup.take() else { return; };
                panel.followup_model_token = None;
                panel.running = false;
                panel.clear_emulator_wait(cx);
                let result = result.and_then(|(devices, serials, serial)| {
                    ensure!(panel.trusted_root(cx)? == root, "The Android project changed while the emulator was starting.");
                    if let Some((target, _)) = &deployment {
                        ensure!(panel.selected_target.as_ref() == Some(target) && panel.targets.contains(target), "The selected Android variant changed while the emulator was starting.");
                    }
                    panel.selected_serial = Some(serial.clone());
                    panel.selected_avd = Some(name);
                    panel.devices = devices;
                    panel.emulator_serials.retain(|_, serial| panel.devices.iter().any(|device| &device.serial == serial && device.is_available()));
                    panel.emulator_serials.extend(serials);
                    Ok(serial)
                });
                match result {
                    Ok(serial) => {
                        panel.error = None;
                        if let Some((target, debug)) = deployment {
                            if let Some(model_token) = model_token {
                                panel.deploy(target, serial, debug, model_token, window, cx);
                            }
                        } else {
                            panel.status = "Emulator ready".into();
                        }
                    }
                    Err(error) => {
                        panel.status = "Emulator startup failed".into();
                        if !startup.error_reported {
                            panel.notify_emulator_error(format!("{error:#}"), cx);
                        }
                    }
                }
                cx.notify();
            }).log_err();
        }));
        cx.notify();
    }

    fn selected_emulator(&self) -> Result<&Device> {
        let device = self.selected_device()?;
        ensure!(
            device.serial.starts_with("emulator-"),
            "Select an Android emulator to stop; physical devices are not supported by this action."
        );
        Ok(device)
    }

    fn stop_emulator(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running || self.syncing || self.tool_setup.choosing {
            return;
        }
        let selected = (|| {
            let root = self.trusted_root(cx)?;
            let serial = self.selected_emulator()?.serial.clone();
            let name = self.emulator_serials.iter().find(|(_, value)| **value == serial)
                .map(|(name, _)| name.clone())
                .context("The selected emulator's AVD identity is unavailable. Refresh devices before stopping it.")?;
            let worktree_id = self
                .project
                .read(cx)
                .visible_worktrees(cx)
                .find(|worktree| worktree.read(cx).abs_path().as_ref() == root)
                .map(|worktree| worktree.read(cx).id())
                .context("The Android project was closed.")?;
            let lease = DeviceOperationLease::acquire(&serial)?;
            Ok::<_, anyhow::Error>((root, serial, name, worktree_id, lease))
        })();
        let (root, serial, name, worktree_id, lease) = match selected {
            Ok(selected) => selected,
            Err(error) => {
                self.fail(error, window, cx);
                return;
            }
        };
        let model_token = self.project.read(cx).android_model().token();
        let (session_id, output, logs) = self.build_panel.update(cx, |panel, cx| {
            panel.begin(
                BuildTab::Output,
                format!("Stop Android Emulator · {serial}"),
                false,
                window,
                cx,
            )
        });
        self.next_operation_id += 1;
        let mut operation = DeviceOperation {
            root,
            serial,
            model_token: model_token.clone(),
            operation_id: self.next_operation_id,
            session_id,
            target: None,
            lease,
            allow_disconnect: false,
            selected_avd: self.selected_avd.clone(),
        };
        self.active_operation_id = Some(operation.operation_id);
        self.active_build_session = Some((BuildTab::Output, operation.session_id));
        self.followup_model_token = Some(model_token);
        self.running = true;
        self.error = None;
        self.status = "Checking the selected emulator…".into();
        self.emulator_task = Some(cx.spawn_in(window, async move |panel, cx| {
            let result = async {
                panel.read_with(cx, |panel, cx| panel.validate_device_operation(&operation, cx))??;
                let (adb, environment) = cx.background_spawn(async {
                    Ok::<_, anyhow::Error>((adb_path()?, Arc::new(android_tools::managed::command_environment()?)))
                }).await?;
                let identity = device_command(&panel, cx, &operation, adb.clone(), vec!["-s".into(), operation.serial.clone(), "emu".into(), "avd".into(), "name".into()], environment.clone(), output.clone(), "Checking the selected emulator's identity…", Duration::from_secs(5)).await?;
                ensure!(parse_avd_identity(&identity)? == name, "The emulator at {} changed identity. Refresh devices and retry.", operation.serial);
                let stopped = device_command(&panel, cx, &operation, adb.clone(), deployment::emulator_kill_arguments(&operation.serial)?, environment.clone(), output.clone(), "Stopping the selected emulator…", Duration::from_secs(10)).await?;
                deployment::validate_emulator_kill_output(&stopped)?;
                operation.allow_disconnect = true;
                let executor = cx.background_executor().clone();
                let disconnect = async {
                    loop {
                        let devices = device_command(&panel, cx, &operation, adb.clone(), vec!["devices".into(), "-l".into()], environment.clone(), output.clone(), "Waiting for the selected emulator to stop…", Duration::from_secs(5)).await?;
                        if !parse_devices(&devices)?.iter().any(|device| device.serial == operation.serial) {
                            return Ok::<_, anyhow::Error>(());
                        }
                        executor.timer(Duration::from_millis(500)).await;
                    }
                };
                match select(Box::pin(disconnect), Box::pin(executor.timer(Duration::from_secs(15)))).await {
                    Either::Left((result, _)) => result,
                    Either::Right(_) => bail!("The emulator accepted the stop command but did not disconnect within 15 seconds. Check its window and refresh devices."),
                }
            }.await;
            drop(output);
            logs.await;
            panel.update_in(cx, |panel, window, cx| {
                if panel.active_operation_id != Some(operation.operation_id)
                    || panel.active_build_session != Some((BuildTab::Output, operation.session_id)) {
                    return;
                }
                let result = result.and_then(|()| panel.validate_device_operation(&operation, cx));
                panel.command_cancel = None;
                panel.active_build_session = None;
                panel.followup_model_token = None;
                let (status, message) = match &result {
                    Ok(()) => (BuildStatus::Succeeded, "Emulator stopped".to_owned()),
                    Err(error) if error.is::<android_build::CommandCancelled>() => (BuildStatus::Cancelled, "Stop operation cancelled; the emulator may already have received the stop command.".into()),
                    Err(error) => (BuildStatus::Failed, format!("{error:#}")),
                };
                panel.build_panel.update(cx, |panel, cx| panel.finish(BuildTab::Output, operation.session_id, status, message, cx));
                match result {
                    Ok(()) => panel.complete_terminal_task(operation.operation_id, &operation.root, worktree_id, &operation.model_token, Some(AfterTask::RefreshDevices), ScheduledTaskResult::Success, window, cx),
                    Err(error) => {
                        panel.active_operation_id = None;
                        panel.running = false;
                        if error.is::<android_build::CommandCancelled>() {
                            panel.status = "Stop operation cancelled".into();
                        } else {
                            panel.fail(error, window, cx);
                        }
                    }
                }
                cx.notify();
            }).log_err();
        }));
        cx.notify();
    }

    fn project_picker(&self, cx: &Context<Self>) -> impl IntoElement {
        let roots = self.roots(cx);
        let controller = cx.weak_entity();
        let label = self
            .root
            .as_ref()
            .or_else(|| roots.first())
            .and_then(|root| root.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Project".into());
        PopoverMenu::new("android-project")
            .trigger(
                Button::new("android-project", label)
                    .truncate(true)
                    .disabled(self.running || self.syncing || self.tool_setup.choosing)
                    .tab_index(0isize),
            )
            .menu(move |window, cx| {
                Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                    for root in &roots {
                        let controller = controller.clone();
                        let root = root.clone();
                        menu = menu.entry(
                            root.to_string_lossy().into_owned(),
                            None,
                            move |window, cx| {
                                controller
                                    .update(cx, |controller, cx| {
                                        if controller.running
                                            || controller.syncing
                                            || controller.tool_setup.choosing
                                            || !controller.roots(cx).contains(&root)
                                        {
                                            return;
                                        }
                                        controller.kotlin_refresh_task = None;
                                        controller.kotlin_refresh_pending = None;
                                        controller.pending_gradle_operation = None;
                                        controller.active_operation_id = None;
                                        controller.invalidate_model(Some(root.clone()), cx);
                                        controller.root = Some(root.clone());
                                        controller.targets.clear();
                                        controller.selected_target = None;
                                        controller.error = None;
                                        controller.auto_sync_root = None;
                                        controller.sync_project(window, cx);
                                    })
                                    .log_err();
                            },
                        );
                    }
                    menu
                }))
            })
    }

    fn render_toolbar(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .child(div().max_w(px(130.)).child(self.project_picker(cx)))
            .child(self.target_picker("toolbar-target", cx))
            .child(self.device_picker("toolbar-device", cx))
            .child(
                IconButton::new("android-run", IconName::PlayFilled)
                    .tab_index(0isize)
                    .aria_label("Run app")
                    .icon_color(Color::Success)
                    .disabled(
                        self.running
                            || self.syncing
                            || self.tool_setup.choosing
                            || self.selected_target.is_none()
                            || !self.can_run_on_selected_device(),
                    )
                    .tooltip(|_, cx| Tooltip::for_action("Run app", &Run, cx))
                    .on_click(cx.listener(|panel, _, window, cx| {
                        panel.gradle(GradleOperation::Run, window, cx)
                    })),
            )
            .child(
                IconButton::new("android-debug", IconName::Debug)
                    .tab_index(0isize)
                    .aria_label("Debug app")
                    .disabled(
                        self.running
                            || self.syncing
                            || self.tool_setup.choosing
                            || self.debug_forward.is_some()
                            || self.selected_target.is_none()
                            || !self.can_run_on_selected_device(),
                    )
                    .tooltip(|_, cx| Tooltip::for_action("Debug app", &Debug, cx))
                    .on_click(cx.listener(|panel, _, window, cx| {
                        panel.gradle(GradleOperation::Debug, window, cx)
                    })),
            )
            .child(
                IconButton::new("android-build", IconName::ToolHammer)
                    .tab_index(0isize)
                    .aria_label("Build selected variant")
                    .disabled(
                        self.running
                            || self.syncing
                            || self.tool_setup.choosing
                            || self.selected_target.is_none(),
                    )
                    .tooltip(|_, cx| Tooltip::for_action("Build selected variant", &Build, cx))
                    .on_click(cx.listener(|panel, _, window, cx| {
                        panel.gradle(GradleOperation::Build, window, cx)
                    })),
            )
            .child(
                IconButton::new("android-sync", IconName::RefreshTitle)
                    .tab_index(0isize)
                    .aria_label("Sync Android project")
                    .disabled(self.running || self.syncing || self.tool_setup.choosing)
                    .tooltip(|_, cx| Tooltip::for_action("Sync Android project", &SyncProject, cx))
                    .on_click(cx.listener(|panel, _, window, cx| panel.sync_project(window, cx))),
            )
    }
}

pub struct AndroidToolbar {
    panel: WeakEntity<AndroidPanel>,
    _subscription: Subscription,
}
impl Render for AndroidToolbar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().children(
            self.panel
                .update(cx, |panel, cx| {
                    panel.render_toolbar(window, cx).into_any_element()
                })
                .log_err(),
        )
    }
}

enum AndroidFailure {}

enum JavaStatus {}
impl lsp::notification::Notification for JavaStatus {
    type Params = serde_json::Value;
    const METHOD: &'static str = "language/status";
}
enum RefreshJavaProjects {}
impl lsp::notification::Notification for RefreshJavaProjects {
    type Params = serde_json::Value;
    const METHOD: &'static str = "java/projectConfigurationsUpdate";
}

fn java_settings(previous: String, root: &Path, cx: &App) -> Result<String> {
    let parsed: serde_json::Value = settings::parse_json_with_comments(&previous)?;
    let existing_environment = parsed
        .pointer("/lsp/jdtls/binary/env")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_owned())))
        .collect::<std::collections::BTreeMap<_, _>>();
    let managed_environment =
        android_tools::managed::language_environment_with(existing_environment.iter())?;
    let pause = java_pause_state(&parsed)?;
    let mut options = parsed
        .pointer("/lsp/jdtls/initialization_options")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    ensure!(
        options.is_object(),
        "Java initialization options must be an object"
    );
    let mut java_settings = parsed
        .pointer("/lsp/jdtls/settings")
        .or_else(|| parsed.pointer("/lsp/jdtls/initialization_options/settings"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let import = root
        .join(".koda/android-java/import.gradle")
        .to_string_lossy()
        .into_owned();
    let model = format!(
        "-Dzed.android.javaModel={}",
        root.join(".koda/android-java/model.json").display()
    );
    let arguments = java_settings
        .pointer("/java/import/gradle/arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let mut arguments: Vec<String> = serde_json::from_value(arguments)
        .context("Java Gradle arguments must be an array of strings")?;
    arguments.retain(|argument| !argument.starts_with("-Dzed.android.javaModel="));
    if !arguments
        .windows(2)
        .any(|pair| pair == ["--init-script", &import])
    {
        arguments.extend(["--init-script".into(), import]);
    }
    arguments.push(model);
    util::merge_json_value_into(
        serde_json::json!({"java": {
            "import": {"gradle": {"arguments": arguments}},
            "configuration": {"updateBuildConfiguration": "automatic"},
            "jdt": {"ls": {"androidSupport": {"enabled": false}}}
        }}),
        &mut java_settings,
    );
    options
        .as_object_mut()
        .context("Java initialization options must be an object")?
        .insert("settings".into(), java_settings.clone());
    cx.global::<settings::SettingsStore>()
        .new_text_for_update(previous, |content| {
            if let Some(JavaPause { previous, paused }) = pause {
                let java = content
                    .project
                    .all_languages
                    .languages
                    .0
                    .entry("Java".into())
                    .or_default();
                if java.language_servers.as_ref().is_some_and(|servers| {
                    servers
                        .iter()
                        .map(|server| {
                            if server.disabled {
                                format!("!{}", server.name)
                            } else {
                                server.name.to_string()
                            }
                        })
                        .collect::<Vec<_>>()
                        == paused
                }) {
                    java.language_servers = previous.map(|servers| {
                        servers
                            .iter()
                            .map(|server| server.as_str().into())
                            .collect()
                    });
                }
                if let Some(environment) = content
                    .project
                    .lsp
                    .0
                    .get_mut("jdtls")
                    .and_then(|settings| settings.binary.as_mut())
                    .and_then(|binary| binary.env.as_mut())
                {
                    environment.remove(PAUSED_JAVA_SERVERS);
                }
            }
            let settings = content.project.lsp.0.entry("jdtls".into()).or_default();
            settings.settings = Some(java_settings);
            settings.initialization_options = Some(options);
            if !managed_environment.is_empty() {
                settings
                    .binary
                    .get_or_insert_default()
                    .env
                    .get_or_insert_default()
                    .extend(managed_environment.clone());
            }
        })
}

fn paused_java_settings(previous: String, root: &Path, cx: &App) -> Result<String> {
    let parsed: serde_json::Value = settings::parse_json_with_comments(&previous)?;
    if parsed
        .pointer(&format!("/lsp/jdtls/binary/env/{PAUSED_JAVA_SERVERS}"))
        .is_some()
    {
        return Ok(previous);
    }
    let expected = format!(
        "-Dzed.android.javaModel={}",
        root.join(".koda/android-java/model.json").display()
    );
    ensure!(
        parsed
            .pointer("/lsp/jdtls/settings/java/import/gradle/arguments")
            .or_else(|| parsed
                .pointer("/lsp/jdtls/initialization_options/settings/java/import/gradle/arguments"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|arguments| arguments
                .iter()
                .any(|argument| argument.as_str() == Some(&expected))),
        "The Java backend changed while syncing; its settings were preserved"
    );
    let original: Option<Vec<String>> = parsed
        .pointer("/languages/Java/language_servers")
        .map(|servers| serde_json::from_value(servers.clone()))
        .transpose()?;
    let mut paused = original.clone().unwrap_or_else(|| vec!["...".into()]);
    paused.retain(|server| server != "jdtls" && server != "!jdtls");
    paused.insert(0, "!jdtls".into());
    let marker = serde_json::json!({"previous": original, "paused": paused}).to_string();
    cx.global::<settings::SettingsStore>()
        .new_text_for_update(previous, |content| {
            content
                .project
                .all_languages
                .languages
                .0
                .entry("Java".into())
                .or_default()
                .language_servers =
                Some(paused.iter().map(|server| server.as_str().into()).collect());
            content
                .project
                .lsp
                .0
                .entry("jdtls".into())
                .or_default()
                .binary
                .get_or_insert_default()
                .env
                .get_or_insert_default()
                .insert(PAUSED_JAVA_SERVERS.into(), marker);
        })
}

fn android_model_input(path: &RelPath) -> bool {
    let path = path.as_unix_str();
    let components = path.split('/').collect::<Vec<_>>();
    if components.iter().any(|component| {
        matches!(
            *component,
            "build" | "generated" | ".gradle" | ".koda" | ".git"
        )
    }) {
        return false;
    }
    let file_name = components.last().copied().unwrap_or_default();
    let build_logic = components
        .iter()
        .any(|component| matches!(*component, "buildSrc" | "build-logic"));
    path.ends_with(".gradle")
        || path.ends_with(".gradle.kts")
        || path.ends_with(".versions.toml")
        || (path.ends_with(".toml") && components.contains(&"gradle"))
        || (build_logic
            && [".kt", ".kts", ".java", ".groovy", ".properties"]
                .iter()
                .any(|extension| path.ends_with(extension)))
        || matches!(
            file_name,
            "buildSrc"
                | "build-logic"
                | "gradle.properties"
                | "gradle-wrapper.properties"
                | "local.properties"
                | "gradle.lockfile"
                | "AndroidManifest.xml"
        )
        || (components.contains(&"src") && components.contains(&"res"))
}

fn has_official_kotlin_system_path<'a>(
    root: &Path,
    arguments: impl IntoIterator<Item = &'a str>,
) -> bool {
    let expected = format!(
        "--system-path={}",
        android_tools::kotlin::official_system_path(root).display()
    );
    let legacy = format!(
        "--system-path={}",
        root.join(".koda/android-kotlin-official/system").display()
    );
    let mut system_paths = arguments
        .into_iter()
        .filter(|argument| *argument == "--system-path" || argument.starts_with("--system-path="));
    let system_path = system_paths.next();
    (system_path == Some(expected.as_str()) || system_path == Some(legacy.as_str()))
        && system_paths.next().is_none()
}

fn default_kotlin_server_settings(server: &project::project_settings::LspSettings) -> bool {
    server.binary.as_ref().is_none_or(|binary| {
        binary.path.is_none()
            && binary.arguments.as_ref().is_none_or(Vec::is_empty)
            && binary.env.as_ref().is_none_or(|env| env.is_empty())
            && binary.ignore_system_version.is_none()
    }) && server
        .initialization_options
        .as_ref()
        .is_none_or(|options| {
            options.is_null()
                || options
                    .as_object()
                    .is_some_and(|options| options.is_empty())
        })
}

fn official_kotlin_binary_is_managed(
    binary: &project::project_settings::BinarySettings,
    root: &Path,
) -> bool {
    if binary
        .env
        .as_ref()
        .and_then(|env| env.get(android_tools::kotlin::MANAGED_RUNTIME_ENV))
        .is_some_and(|marker| marker == "1")
    {
        // An explicit filesystem path belongs to the user, even if an old marker remains.
        return binary
            .path
            .as_deref()
            .is_none_or(|path| path == android_tools::kotlin::MANAGED_RUNTIME_PATH);
    }
    binary.arguments.as_ref().is_some_and(|arguments| {
        has_official_kotlin_system_path(root, arguments.iter().map(String::as_str))
    })
}

fn paused_official_kotlin_settings(previous: String, root: &Path, cx: &App) -> Result<String> {
    let parsed: serde_json::Value = settings::parse_json_with_comments(&previous)?;
    let binary = parsed
        .pointer("/lsp/kotlin-lsp/binary")
        .cloned()
        .map(serde_json::from_value::<project::project_settings::BinarySettings>)
        .transpose()?;
    let servers = parsed
        .pointer("/languages/Kotlin/language_servers")
        .and_then(serde_json::Value::as_array);
    let already_paused = parsed
        .pointer("/lsp/kotlin-lsp/binary/env/ZED_ANDROID_VARIANT_UNAVAILABLE")
        .and_then(serde_json::Value::as_str)
        == Some("true");
    ensure!(
        parsed
            .pointer("/languages/Kotlin/enable_language_server")
            .and_then(serde_json::Value::as_bool)
            != Some(false)
            && servers.is_some_and(|servers| (servers.len() == 1
                && servers[0].as_str() == Some("kotlin-lsp"))
                || (servers.is_empty() && already_paused))
            && binary
                .as_ref()
                .is_some_and(|binary| official_kotlin_binary_is_managed(binary, root)),
        "The Kotlin backend changed while syncing; its settings were preserved"
    );
    cx.global::<settings::SettingsStore>()
        .new_text_for_update(previous, |content| {
            if let Some(java) = content.project.all_languages.languages.0.get_mut("Java")
                && java
                    .language_servers
                    .as_ref()
                    .is_some_and(|servers| *servers == vec!["kotlin-lsp".into(), "jdtls".into()])
            {
                java.language_servers = Some(vec!["jdtls".into()]);
            }
            content
                .project
                .all_languages
                .languages
                .0
                .entry("Kotlin".into())
                .or_default()
                .language_servers = Some(Vec::new());
            content
                .project
                .lsp
                .0
                .entry("kotlin-lsp".into())
                .or_default()
                .binary
                .get_or_insert_default()
                .env
                .get_or_insert_default()
                .insert(UNAVAILABLE_ANDROID_VARIANT.into(), "true".into());
        })
}

fn official_kotlin_settings(
    previous: String,
    root: &Path,
    target: &AndroidTarget,
    java_home: &Path,
    server_binary: &Path,
    cx: &App,
) -> Result<String> {
    official_kotlin_settings_with_policy(
        previous,
        root,
        target,
        java_home,
        server_binary,
        project::lsp_store::managed_kotlin_runtime_enabled(cx),
        cx,
    )
}

fn official_kotlin_settings_with_policy(
    previous: String,
    root: &Path,
    target: &AndroidTarget,
    java_home: &Path,
    server_binary: &Path,
    managed_runtime: bool,
    cx: &App,
) -> Result<String> {
    let uri = lsp::Uri::from_file_path(root)
        .map_err(|_| anyhow::anyhow!("Could not create the Kotlin project URI"))?;
    let parsed: serde_json::Value = if previous.trim().is_empty() {
        serde_json::json!({})
    } else {
        settings::parse_json_with_comments(&previous)?
    };
    let existing_environment = parsed
        .pointer("/lsp/kotlin-lsp/binary/env")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_owned())))
        .collect::<std::collections::BTreeMap<_, _>>();
    let managed_environment =
        android_tools::managed::language_environment_with(existing_environment.iter())?;
    let inherited = project::project_settings::ProjectSettings::get_global(cx)
        .lsp
        .get(&lsp::LanguageServerName("kotlin-lsp".into()))
        .cloned()
        .unwrap_or_default();
    let mut options = inherited
        .initialization_options
        .clone()
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(local) = parsed
        .pointer("/lsp/kotlin-lsp/initialization_options")
        .filter(|options| !options.is_null())
    {
        util::merge_json_value_into(local.clone(), &mut options);
    }
    ensure!(
        options.is_object(),
        "Kotlin initialization options must be an object"
    );
    let mut projects = options
        .get("projects")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let projects_array = projects
        .as_array_mut()
        .context("Kotlin projects must be an array")?;
    let project = serde_json::json!({"type": "gradle", "path": uri, "java-home": java_home});
    if let Some(existing) = projects_array.iter_mut().find(|project| {
        project
            .get("path")
            .and_then(|path| path.as_str())
            .is_some_and(|path| path.trim_end_matches('/') == uri.as_str().trim_end_matches('/'))
    }) {
        ensure!(
            existing.get("type").and_then(|value| value.as_str()) == Some("gradle"),
            "This project has an explicit Kotlin importer. Preserve it by configuring Kotlin manually."
        );
        util::merge_json_value_into(project, existing);
    } else {
        projects_array.push(project);
    }
    util::merge_json_value_into(
        serde_json::json!({"defaultSdk": java_home, "projects": projects}),
        &mut options,
    );
    let inherited_java = language::language_settings::AllLanguageSettings::get_global(cx)
        .language(None, Some(&language::LanguageName::from("Java")), cx)
        .language_servers
        .clone();
    let java_pause = java_pause_state(&parsed)?;
    let paused_companion = java_pause.as_ref().is_some_and(|pause| {
        pause.previous.as_ref().is_none_or(|servers| {
            *servers == vec!["...".to_owned()] || *servers == vec!["jdtls".to_owned()]
        })
    });
    let resumed_java_companion = if paused_companion {
        Some(serde_json::to_string(&JavaPause {
            previous: Some(vec!["kotlin-lsp".into(), "jdtls".into()]),
            paused: vec!["kotlin-lsp".into(), "!jdtls".into()],
        })?)
    } else {
        None
    };
    cx.global::<settings::SettingsStore>()
        .new_text_for_update(previous, |content| {
            let java = content
                .project
                .all_languages
                .languages
                .0
                .entry("Java".into())
                .or_default();
            let servers = java.language_servers.as_ref().unwrap_or(&inherited_java);
            if *servers == vec!["...".into()] || *servers == vec!["jdtls".into()] {
                java.language_servers = Some(vec!["kotlin-lsp".into(), "jdtls".into()]);
            } else if let Some(marker) = resumed_java_companion {
                java.language_servers = Some(vec!["kotlin-lsp".into(), "!jdtls".into()]);
                content
                    .project
                    .lsp
                    .0
                    .entry("jdtls".into())
                    .or_default()
                    .binary
                    .get_or_insert_default()
                    .env
                    .get_or_insert_default()
                    .insert(PAUSED_JAVA_SERVERS.into(), marker);
            }
            content
                .project
                .all_languages
                .languages
                .0
                .entry("Kotlin".into())
                .or_default()
                .language_servers = Some(vec!["kotlin-lsp".into()]);
            let server = content
                .project
                .lsp
                .0
                .entry("kotlin-lsp".into())
                .or_default();
            server.initialization_options = Some(options);
            let binary = server.binary.get_or_insert_default();
            binary.path = Some(if managed_runtime {
                android_tools::kotlin::MANAGED_RUNTIME_PATH.to_owned()
            } else {
                server_binary.to_string_lossy().into_owned()
            });
            let arguments = binary.arguments.get_or_insert_with(|| {
                inherited
                    .binary
                    .and_then(|binary| binary.arguments)
                    .unwrap_or_default()
            });
            let mut previous_arguments = std::mem::take(arguments).into_iter();
            while let Some(argument) = previous_arguments.next() {
                if argument == "--system-path" {
                    previous_arguments.next();
                } else if !argument.starts_with("--system-path=") {
                    arguments.push(argument);
                }
            }
            if !arguments.iter().any(|argument| argument == "--stdio") {
                arguments.push("--stdio".into());
            }
            arguments.push(format!(
                "--system-path={}",
                android_tools::kotlin::official_system_path(root).display()
            ));
            binary
                .env
                .get_or_insert_default()
                .remove(UNAVAILABLE_ANDROID_VARIANT);
            binary.env.get_or_insert_default().extend([
                ("LSP_ANDROID_MODULE".into(), target.module.clone()),
                ("LSP_ANDROID_VARIANT".into(), target.variant.clone()),
            ]);
            binary
                .env
                .get_or_insert_default()
                .extend(managed_environment.clone());
            let environment = binary.env.get_or_insert_default();
            if managed_runtime {
                environment.insert(
                    android_tools::kotlin::MANAGED_RUNTIME_ENV.into(),
                    "1".into(),
                );
                for variable in ["JAVA_HOME", "ANDROID_HOME", "ANDROID_SDK_ROOT"] {
                    environment.remove(variable);
                }
            } else {
                environment.remove(android_tools::kotlin::MANAGED_RUNTIME_ENV);
            }
        })
}

async fn device_command(
    panel: &WeakEntity<AndroidPanel>,
    cx: &mut gpui::AsyncWindowContext,
    operation: &DeviceOperation,
    program: PathBuf,
    arguments: Vec<String>,
    environment: Arc<std::collections::BTreeMap<String, String>>,
    output: futures::channel::mpsc::Sender<android_build::OutputLine>,
    label: &'static str,
    timeout: Duration,
) -> Result<String> {
    let (cancel, cancelled) = oneshot::channel();
    panel.update_in(cx, |panel, _, cx| {
        panel.validate_device_operation(operation, cx)?;
        panel.command_cancel = Some(cancel);
        panel.status = label.into();
        cx.notify();
        Ok::<_, anyhow::Error>(())
    })??;
    let root = operation.root.clone();
    let lease = operation.lease.clone();
    let executor = cx.background_executor().clone();
    cx.background_spawn(async move {
        let mut command = util::command::new_std_command(program);
        command
            .args(arguments)
            .current_dir(root)
            .envs(environment.iter());
        let result =
            android_build::command_output_combined(command, &executor, timeout, output, cancelled)
                .await?
                .stdout();
        drop(lease);
        result
    })
    .await
}

async fn connected_devices(
    executor: &BackgroundExecutor,
) -> Result<(Vec<Device>, HashMap<String, String>)> {
    connected_devices_with_adb(adb_path()?, executor).await
}

async fn connected_devices_with_adb(
    adb: PathBuf,
    executor: &BackgroundExecutor,
) -> Result<(Vec<Device>, HashMap<String, String>)> {
    let output = tool_output(
        adb.clone(),
        vec!["devices".into(), "-l".into()],
        Path::new("."),
        executor,
        Duration::from_secs(15),
    )
    .await?;
    let devices = parse_devices(&output)?;
    let mut emulator_serials = HashMap::new();
    let candidates = devices
        .iter()
        .filter(|device| device.is_available() && device.serial.starts_with("emulator-"))
        .map(|device| device.serial.clone())
        .collect::<Vec<_>>();
    let mut lookups = futures::stream::iter(candidates)
        .map(|serial| {
            let adb = adb.clone();
            async move {
                let output = tool_output(
                    adb,
                    vec![
                        "-s".into(),
                        serial.clone(),
                        "emu".into(),
                        "avd".into(),
                        "name".into(),
                    ],
                    Path::new("."),
                    executor,
                    Duration::from_secs(5),
                )
                .await;
                (serial, output)
            }
        })
        .buffer_unordered(8);
    while let Some((serial, output)) = lookups.next().await {
        if let Some(output) = output.log_err()
            && let Some(name) = parse_avd_identity(&output).log_err()
        {
            ensure!(
                emulator_serials.insert(name.to_owned(), serial).is_none(),
                "More than one running device uses AVD {name}. Select its serial explicitly."
            );
        }
    }
    drop(lookups);
    Ok((devices, emulator_serials))
}

async fn emulator_is_running_with_adb(
    name: &str,
    adb: PathBuf,
    executor: &BackgroundExecutor,
) -> Result<bool> {
    let (devices, serials) = connected_devices_with_adb(adb, executor).await?;
    ensure!(
        devices
            .iter()
            .filter(|device| device.is_available() && device.serial.starts_with("emulator-"))
            .all(|device| serials.values().any(|serial| serial == &device.serial)),
        "A running emulator's AVD identity is unavailable. Check emulator console access and refresh devices before starting another emulator."
    );
    Ok(serials.contains_key(name))
}

type BootedEmulator = (Vec<Device>, HashMap<String, String>, String);

async fn emulator_ready_with_timeout(
    name: &str,
    startup: impl std::future::Future<Output = std::result::Result<(), String>>,
    boot: impl std::future::Future<Output = Result<BootedEmulator>>,
    executor: &BackgroundExecutor,
    timeout: Duration,
) -> Result<BootedEmulator> {
    let wait = async {
        startup.await.map_err(anyhow::Error::msg)?;
        boot.await
    };
    match select(Box::pin(wait), Box::pin(executor.timer(timeout))).await {
        Either::Left((result, _)) => result,
        Either::Right(_) => bail!(
            "Emulator {name} did not finish booting within {} seconds. Check the emulator and run again.",
            timeout.as_secs()
        ),
    }
}

async fn booted_emulator(name: &str, executor: &BackgroundExecutor) -> Result<BootedEmulator> {
    booted_emulator_with_adb(name, adb_path()?, executor).await
}

async fn booted_emulator_with_adb(
    name: &str,
    adb: PathBuf,
    executor: &BackgroundExecutor,
) -> Result<BootedEmulator> {
    let mut selected = None;
    loop {
        if selected.is_none() {
            selected = connected_devices_with_adb(adb.clone(), executor)
                .await
                .log_err()
                .and_then(|(devices, serials)| {
                    let serial = serials.get(name)?.clone();
                    Some((devices, serials, serial))
                });
        }
        if let Some((_, _, serial)) = &selected {
            let boot_completed = tool_output(
                adb.clone(),
                vec![
                    "-s".into(),
                    serial.clone(),
                    "shell".into(),
                    "getprop".into(),
                    "sys.boot_completed".into(),
                ],
                Path::new("."),
                executor,
                Duration::from_secs(5),
            )
            .await;
            match boot_completed {
                Ok(output) if output.trim() == "1" => {
                    let identity = tool_output(
                        adb.clone(),
                        vec![
                            "-s".into(),
                            serial.clone(),
                            "emu".into(),
                            "avd".into(),
                            "name".into(),
                        ],
                        Path::new("."),
                        executor,
                        Duration::from_secs(5),
                    )
                    .await
                    .log_err();
                    if identity.is_some_and(|output| parse_avd_identity(&output).ok() == Some(name))
                    {
                        return selected.context("The selected emulator disconnected");
                    }
                    selected = None;
                }
                Ok(_) => {}
                Err(error) => {
                    log::debug!("Emulator {name} is not ready: {error:#}");
                    selected = None;
                }
            }
        }
        executor.timer(Duration::from_secs(1)).await;
    }
}

fn parse_avd_identity(output: &str) -> Result<&str> {
    let lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    ensure!(
        lines.len() == 2 && lines.get(1) == Some(&"OK"),
        "The emulator console did not return a valid AVD identity: {}",
        output.chars().take(1000).collect::<String>()
    );
    let name = lines
        .first()
        .context("The emulator console returned no AVD name.")?;
    parse_emulators(name)?;
    Ok(name)
}

struct EmulatorProcess {
    child: Option<util::process::Child>,
    drain: Option<Task<Result<()>>>,
    diagnostics: Arc<std::sync::Mutex<String>>,
    executor: BackgroundExecutor,
}

impl EmulatorProcess {
    fn handoff(&mut self, executor: &BackgroundExecutor) -> Result<()> {
        let child = self
            .child
            .as_mut()
            .context("Emulator process is unavailable.")?;
        ensure!(
            child.try_status()?.is_none(),
            "The emulator exited before startup completed."
        );
        child.preserve_descendants()?;
        let mut child = self
            .child
            .take()
            .context("Emulator process is unavailable.")?;
        let drain = self.drain.take();
        let executor = executor.clone();
        executor
            .clone()
            .spawn(async move {
                let status = child.status().await;
                if let Some(drain) = drain {
                    finish_emulator_drain(drain, &executor).await.log_err();
                }
                match status {
                    Ok(status) if status.success() => {}
                    Ok(status) => log::warn!("Android emulator exited after startup ({status})."),
                    Err(error) => log::error!("Could not reap the Android emulator: {error:#}"),
                }
            })
            .detach();
        Ok(())
    }

    fn details(&self) -> String {
        match self.diagnostics.lock() {
            Ok(diagnostics) => diagnostics.clone(),
            Err(error) => {
                log::error!("Could not read emulator diagnostics: {error}");
                String::new()
            }
        }
    }
}

impl Drop for EmulatorProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            child.kill().log_err();
            let drain = self.drain.take();
            let executor = self.executor.clone();
            self.executor
                .spawn(async move {
                    child.status().await.log_err();
                    if let Some(drain) = drain {
                        finish_emulator_drain(drain, &executor).await.log_err();
                    }
                })
                .detach();
        }
    }
}

async fn finish_emulator_drain(
    drain: Task<Result<()>>,
    executor: &BackgroundExecutor,
) -> Result<()> {
    match select(
        Box::pin(drain),
        Box::pin(executor.timer(Duration::from_secs(2))),
    )
    .await
    {
        Either::Left((result, _)) => result,
        Either::Right(_) => bail!("The emulator's output remained open after its process exited."),
    }
}

async fn drain_emulator_output(
    mut reader: impl futures::AsyncRead + Unpin,
    diagnostics: Arc<std::sync::Mutex<String>>,
) -> Result<()> {
    let mut buffer = [0u8; 4096];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(());
        }
        let mut tail = diagnostics
            .lock()
            .map_err(|error| anyhow::anyhow!("Could not store emulator diagnostics: {error}"))?;
        tail.push_str(&String::from_utf8_lossy(&buffer[..count]));
        if tail.len() > 4000 {
            let mut start = tail.len() - 4000;
            while !tail.is_char_boundary(start) {
                start += 1;
            }
            tail.drain(..start);
        }
    }
}

#[cfg(test)]
async fn emulator_start_output(
    command: std::process::Command,
    adb: PathBuf,
    name: &str,
    executor: &BackgroundExecutor,
) -> Result<EmulatorProcess> {
    let (cancel, cancelled) = oneshot::channel();
    let result = emulator_start_output_with_cancel(command, adb, name, executor, cancelled).await;
    drop(cancel);
    result
}

async fn emulator_start_output_with_cancel(
    command: std::process::Command,
    adb: PathBuf,
    name: &str,
    executor: &BackgroundExecutor,
    mut cancelled: oneshot::Receiver<()>,
) -> Result<EmulatorProcess> {
    match cancelled.try_recv() {
        Ok(None) => {}
        Ok(Some(())) | Err(_) => return Err(android_build::CommandCancelled.into()),
    }
    let mut child = util::process::Child::spawn(command, std::process::Stdio::null(), std::process::Stdio::piped(), std::process::Stdio::piped())
        .context("Could not launch the Android SDK emulator. Install the emulator package and create an AVD with Android Studio.")?;
    let stdout = child.stdout.take().context("Missing emulator stdout.")?;
    let stderr = child.stderr.take().context("Missing emulator stderr.")?;
    let diagnostics = Arc::new(std::sync::Mutex::new(String::new()));
    let drain = executor.spawn({
        let diagnostics = diagnostics.clone();
        async move {
            futures::try_join!(
                drain_emulator_output(stdout, diagnostics.clone()),
                drain_emulator_output(stderr, diagnostics)
            )?;
            Ok(())
        }
    });
    let mut process = EmulatorProcess {
        child: Some(child),
        drain: Some(drain),
        diagnostics,
        executor: executor.clone(),
    };
    let result = {
        let child = process
            .child
            .as_mut()
            .context("Emulator process is unavailable.")?;
        let boot = booted_emulator_with_adb(name, adb, executor);
        let startup = async {
            match select(
                Box::pin(boot),
                Box::pin(select(
                    Box::pin(child.status()),
                    Box::pin(executor.timer(EMULATOR_START_TIMEOUT)),
                )),
            )
            .await
            {
                Either::Left((result, _)) => result.map(|_| ()),
                Either::Right((Either::Left((status, _)), _)) => {
                    let status = status?;
                    Err(anyhow::anyhow!(
                        "The emulator exited before boot completed ({status})."
                    ))
                }
                Either::Right((Either::Right(_), _)) => Err(anyhow::anyhow!(
                    "The emulator did not finish booting within {} seconds.",
                    EMULATOR_START_TIMEOUT.as_secs()
                )),
            }
        };
        match select(Box::pin(startup), Box::pin(cancelled)).await {
            Either::Left((result, _)) => result,
            Either::Right(_) => Err(android_build::CommandCancelled.into()),
        }
    };
    if let Err(error) = result {
        if let Some(drain) = process.drain.take() {
            process
                .child
                .as_mut()
                .context("Emulator process is unavailable.")?
                .kill()?;
            finish_emulator_drain(drain, executor).await.log_err();
        }
        let details = process.details();
        return if details.is_empty() {
            Err(error)
        } else {
            Err(error).context(details)
        };
    }
    Ok(process)
}

async fn tool_output(
    program: PathBuf,
    args: Vec<String>,
    root: &Path,
    executor: &BackgroundExecutor,
    timeout: Duration,
) -> Result<String> {
    let mut command = util::command::new_std_command(program);
    command
        .args(args)
        .current_dir(root)
        .envs(android_tools::managed::command_environment()?);
    let (sender, mut receiver) = futures::channel::mpsc::channel::<android_build::OutputLine>(128);
    let (cancel, cancelled) = oneshot::channel();
    let diagnostics = async move {
        let mut tail = String::new();
        while let Some(line) = receiver.next().await {
            tail.push_str(&line.text);
            tail.push('\n');
            if tail.len() > 4000 {
                let mut start = tail.len() - 4000;
                while !tail.is_char_boundary(start) {
                    start += 1;
                }
                tail.drain(..start);
            }
        }
        tail
    };
    let (result, diagnostics) = futures::join!(
        android_build::command_output_native_client(
            command, executor, timeout, sender, cancelled, false
        ),
        diagnostics
    );
    drop(cancel);
    match result {
        Ok(output) => output.stdout(),
        Err(error) if diagnostics.is_empty() => Err(error),
        Err(error) => Err(error).context(diagnostics),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use project::{
        FakeFs,
        trusted_worktrees::{self, PathTrust},
    };
    use serde_json::json;
    use workspace::AppState;

    #[gpui::test]
    async fn first_project_waits_for_detection_and_opens_setup_once_without_a_sidebar(
        cx: &mut TestAppContext,
    ) {
        let _state = cx.update(|cx| {
            let state = AppState::test(cx);
            trusted_worktrees::init(Default::default(), cx);
            init(cx);
            state
        });
        let filesystem = FakeFs::new(cx.executor());
        filesystem
            .insert_tree(
                "/android",
                json!({"gradlew": "", "settings.gradle.kts": ""}),
            )
            .await;
        let project =
            Project::test_with_worktree_trust(filesystem, [Path::new("/android")], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        cx.run_until_parked();
        let panel = workspace
            .read_with(cx, |workspace, cx| controller(workspace, cx))
            .expect("Workspace controller");
        let weak = panel.downgrade();
        panel.update(cx, |panel, _| {
            panel.tool_setup.bootstrap_root = Some("/android".into());
            panel.tool_setup.bootstrap_ready = None;
            panel.refreshing_devices = true;
        });
        drop(panel);
        assert!(
            weak.upgrade().is_some(),
            "Workspace actions retain the controller"
        );
        let store = project.read_with(cx, |project, _| project.worktree_store());
        cx.update(|_, cx| {
            TrustedWorktrees::try_get_global(cx)
                .expect("Trust store")
                .update(cx, |trusted, cx| {
                    trusted.trust(
                        &store,
                        [PathTrust::AbsPath("/android".into())]
                            .into_iter()
                            .collect(),
                        cx,
                    )
                })
        });
        cx.run_until_parked();
        workspace.read_with(cx, |workspace, cx| {
            assert!(workspace.panel::<BuildPanel>(cx).is_some());
        });
        weak.update_in(cx, |panel, window, cx| {
            assert!(panel.auto_sync_root.is_none());
            assert!(panel.onboarding_prompted_root.is_none());
            panel.tool_setup.bootstrap_ready = Some(false);
            panel.auto_sync_project(window, cx);
        })
        .expect("Live controller");
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            assert!(workspace.has_active_modal(window, cx));
        });
        weak.update_in(cx, |panel, window, cx| {
            assert_eq!(panel.onboarding_prompted_root, Some("/android".into()));
            assert!(!panel.syncing && !panel.running);
            assert!(panel.auto_sync_root.is_none());
            panel.auto_sync_project(window, cx);
            assert!(!panel.syncing);
        })
        .expect("Live controller");
    }

    #[gpui::test]
    async fn dependency_readiness_is_refreshed_when_the_selected_root_changes(
        cx: &mut TestAppContext,
    ) {
        let _state = cx.update(AppState::test);
        let filesystem = FakeFs::new(cx.executor());
        for root in ["/first", "/second"] {
            filesystem
                .insert_tree(root, json!({"gradlew": "", "settings.gradle.kts": ""}))
                .await;
        }
        let project =
            Project::test(filesystem, [Path::new("/first"), Path::new("/second")], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        panel.update_in(cx, |panel, window, cx| {
            panel.startup_settings_ready = true;
            panel.root = Some("/second".into());
            panel.tool_setup.bootstrap_root = Some("/first".into());
            panel.tool_setup.bootstrap_ready = Some(true);
            panel.auto_sync_project(window, cx);
            assert_eq!(panel.tool_setup.bootstrap_root, Some("/second".into()));
            assert!(panel.auto_sync_root.is_none());
            assert!(!panel.syncing);
        });
    }

    pub(super) fn publish_test_android_model(
        panel: &mut AndroidPanel,
        target: &AndroidTarget,
        cx: &mut Context<AndroidPanel>,
    ) {
        let root = panel.root.clone().expect("Android root");
        let model = serde_json::from_value(json!({
            "version": 1, "root": root, "diagnostics": [], "modules": [{
                "path": target.module, "directory": root.join("app"),
                "namespace": "example.app", "kind": "application", "variants": [{
                    "name": target.variant, "outputListing": target.output_listing,
                    "components": [{"name": target.variant, "scope": "main", "dependencies": [],
                        "sources": [{"path": root.join("app/resources"), "kind": "resources", "generated": false}]}]
                }]
            }]
        })).expect("Android fixture model");
        panel.project.update(cx, |project, cx| {
            let token = project.invalidate_android_model(Some(root), cx);
            project
                .publish_android_model(&token, model, cx)
                .expect("Publish model");
            project
                .select_android_variant(
                    Some(android_tools::project_model::VariantId::from(target)),
                    cx,
                )
                .expect("Select variant");
        });
    }

    #[gpui::test]
    async fn obsolete_terminal_callbacks_preserve_the_current_operation(cx: &mut TestAppContext) {
        let _state = cx.update(AppState::test);
        let filesystem = FakeFs::new(cx.executor());
        filesystem.insert_tree("/android", json!({"app": {}})).await;
        let project = Project::test(filesystem, [Path::new("/android")], cx).await;
        let worktree_id = project.read_with(cx, |project, cx| {
            project.visible_worktrees(cx).next().unwrap().read(cx).id()
        });
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        panel.update_in(cx, |panel, window, cx| {
            let root = PathBuf::from("/android");
            let target = AndroidTarget {
                module: ":app".into(),
                variant: "debug".into(),
                output_listing: root.join("output.json"),
            };
            panel.root = Some(root.clone());
            panel.selected_target = Some(target.clone());
            panel.targets = vec![target.clone()];
            publish_test_android_model(panel, &target, cx);
            let obsolete_token = panel.project.read(cx).android_model().token();
            publish_test_android_model(panel, &target, cx);
            panel.active_operation_id = Some(2);
            panel.running = true;
            panel.status = "Current operation".into();
            for result in [
                ScheduledTaskResult::Success,
                ScheduledTaskResult::Cancelled,
                ScheduledTaskResult::Failure,
            ] {
                panel.complete_terminal_task(
                    1,
                    &root,
                    worktree_id,
                    &obsolete_token,
                    Some(AfterTask::AttachDebugger(
                        root.clone(),
                        "device".into(),
                        "example.old".into(),
                    )),
                    result,
                    window,
                    cx,
                );
                assert!(panel.running);
                assert_eq!(panel.active_operation_id, Some(2));
                assert_eq!(panel.status.as_ref(), "Current operation");
                assert!(panel.debug_task.is_none());
            }
            panel.complete_terminal_task(
                2,
                &root,
                worktree_id,
                &obsolete_token,
                Some(AfterTask::AttachDebugger(
                    root.clone(),
                    "device".into(),
                    "example.old".into(),
                )),
                ScheduledTaskResult::Success,
                window,
                cx,
            );
            assert!(!panel.running);
            assert!(panel.debug_task.is_none());
            assert!(
                panel
                    .error
                    .as_ref()
                    .is_some_and(|error| error.contains("model changed"))
            );
        });
    }

    #[gpui::test]
    async fn invalidated_variants_wait_for_sync_before_building(cx: &mut TestAppContext) {
        let _state = cx.update(AppState::test);
        let filesystem = FakeFs::new(cx.executor());
        filesystem.insert_tree("/android", json!({"app": {}})).await;
        let project = Project::test(filesystem, [Path::new("/android")], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        panel.update_in(cx, |panel, window, cx| {
            let root = PathBuf::from("/android");
            let target = AndroidTarget {
                module: ":app".into(),
                variant: "debug".into(),
                output_listing: root.join("output.json"),
            };
            panel.root = Some(root.clone());
            panel.targets = vec![target.clone()];
            panel.selected_target = Some(target.clone());
            publish_test_android_model(panel, &target, cx);
            panel.invalidate_model(Some(root), cx);
            panel.gradle(GradleOperation::Build, window, cx);
            assert!(panel.build_task.is_none());
            assert!(
                panel
                    .error
                    .as_ref()
                    .is_some_and(|error| error.contains("Sync and select"))
            );
            panel.queue_official_kotlin_refresh(window, cx);
            panel.gradle(GradleOperation::Build, window, cx);
            assert!(panel.pending_gradle_operation.is_some());
            assert!(panel.build_task.is_none());
            panel.kotlin_refresh_task = None;
            panel.kotlin_refresh_pending = None;
            panel.resume_pending_gradle_operation(window, cx);
            assert!(panel.pending_gradle_operation.is_some());
            assert!(panel.build_task.is_none());
            publish_test_android_model(panel, &target, cx);
            panel.tool_setup.choosing = true;
            panel.resume_pending_gradle_operation(window, cx);
            assert!(panel.pending_gradle_operation.is_some());
            assert!(!panel.running);
            panel.tool_setup.choosing = false;
            panel.resume_pending_gradle_operation(window, cx);
            assert!(panel.pending_gradle_operation.is_none());
            assert!(panel.running);
            assert!(panel.build_task.is_some());
            panel.cancel_build(BuildTab::Output, cx);
        });
    }

    #[gpui::test]
    async fn dirty_custom_resources_are_saved_before_sync_and_build(cx: &mut TestAppContext) {
        let _state = cx.update(|cx| {
            let state = AppState::test(cx);
            editor::init(cx);
            state
        });
        let filesystem = FakeFs::new(cx.executor());
        filesystem
            .insert_tree(
                "/android",
                json!({"app": {"resources": {"values": {"strings.xml": "<resources/>"}}}}),
            )
            .await;
        let project = Project::test(filesystem, [Path::new("/android")], cx).await;
        let buffer = project
            .update(cx, |project, cx| {
                project.open_local_buffer("/android/app/resources/values/strings.xml", cx)
            })
            .await
            .expect("Open resource");
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project.clone(), cx));
        workspace.update_in(cx, |workspace, window, cx| {
            let editor = cx.new(|cx| {
                editor::Editor::for_buffer(buffer.clone(), Some(project.clone()), window, cx)
            });
            let pane = workspace.active_pane().clone();
            workspace.add_item(pane, Box::new(editor), None, true, true, window, cx);
        });
        buffer.update(cx, |buffer, cx| buffer.edit([(0..0, " ")], None, cx));
        panel.update_in(cx, |panel, window, cx| {
            let root = PathBuf::from("/android");
            let target = AndroidTarget {
                module: ":app".into(),
                variant: "debug".into(),
                output_listing: root.join("output.json"),
            };
            panel.root = Some(root);
            panel.model_input_roots = vec![("/android/app/resources".into(), false)];
            panel.targets = vec![target.clone()];
            panel.selected_target = Some(target.clone());
            publish_test_android_model(panel, &target, cx);
            assert!(panel.model_inputs_dirty(cx));
            panel.gradle(GradleOperation::Build, window, cx);
            assert!(panel.running);
            assert!(panel.pending_gradle_operation.is_some());
            assert!(
                panel.active_build_session.is_none(),
                "Assembly must wait for saved input reconciliation"
            );
        });
        cx.run_until_parked();
        assert!(!buffer.read_with(cx, |buffer, _| buffer.is_dirty()));
        panel.read_with(cx, |panel, _| {
            assert!(
                panel.sync_task.is_some(),
                "Saved inputs start model discovery before assembly"
            )
        });
    }

    #[gpui::test]
    async fn custom_input_roots_survive_invalidation_and_exclude_generated_roots(
        cx: &mut TestAppContext,
    ) {
        let _state = cx.update(AppState::test);
        let filesystem = FakeFs::new(cx.executor());
        filesystem.insert_tree("/android", json!({})).await;
        let project = Project::test(filesystem, [Path::new("/android")], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        panel.update(cx, |panel, cx| {
            panel.root = Some("/android".into());
            panel.model_input_roots = vec![
                ("/android/app/resources".into(), false),
                ("/android/app/config/manifest.xml".into(), false),
                ("/android/app/src/main/res/generated".into(), true),
            ];
            for path in [
                "app/resources/values/strings.xml",
                "app/config/manifest.xml",
            ] {
                assert!(panel.model_input_changed(RelPath::from_unix_str(path).unwrap(), cx));
            }
            assert!(!panel.model_input_changed(
                RelPath::from_unix_str("app/src/main/res/generated/values/strings.xml").unwrap(),
                cx
            ));
            panel.invalidate_model(panel.root.clone(), cx);
            assert!(panel.model_input_changed(
                RelPath::from_unix_str("app/resources/values/new.xml").unwrap(),
                cx
            ));
            panel.invalidate_model(Some("/another".into()), cx);
            assert!(panel.model_input_roots.is_empty());
        });
    }

    #[gpui::test]
    fn managed_java_pause_preserves_custom_settings_and_restores_server_selection(
        cx: &mut TestAppContext,
    ) {
        let _state = cx.update(AppState::test);
        cx.update(|cx| {
            let root = Path::new("/android");
            for servers in [
                None,
                Some(json!(["jdtls"])),
                Some(json!(["custom-java", "jdtls"])),
            ] {
                let mut previous = json!({"lsp": {"jdtls": {
                    "binary": {"path": "/custom/jdtls", "env": {"CUSTOM": "preserve"}},
                    "initialization_options": {"bundles": ["debug.jar"]},
                    "settings": {"java": {"format": {"enabled": false}}}
                }}, "languages": {"Java": {"tab_size": 3}}});
                if let Some(servers) = &servers {
                    previous["languages"]["Java"]["language_servers"] = servers.clone();
                }
                let managed =
                    java_settings(previous.to_string(), root, cx).expect("Managed settings");
                let paused =
                    paused_java_settings(managed.clone(), root, cx).expect("Pause managed Java");
                assert_eq!(
                    paused_java_settings(paused.clone(), root, cx).expect("Repeated pause"),
                    paused
                );
                let parsed: serde_json::Value =
                    settings::parse_json_with_comments(&paused).expect("Paused settings");
                assert!(
                    parsed["languages"]["Java"]["language_servers"]
                        .as_array()
                        .unwrap()
                        .contains(&json!("!jdtls"))
                );
                assert_eq!(parsed["lsp"]["jdtls"]["binary"]["path"], "/custom/jdtls");
                assert_eq!(
                    parsed["lsp"]["jdtls"]["binary"]["env"]["CUSTOM"],
                    "preserve"
                );
                assert_eq!(
                    parsed["lsp"]["jdtls"]["initialization_options"]["bundles"],
                    json!(["debug.jar"])
                );
                assert_eq!(
                    parsed["lsp"]["jdtls"]["settings"]["java"]["format"]["enabled"],
                    false
                );
                let restored =
                    java_settings(paused, root, cx).expect("Current Java model resumes settings");
                let restored: serde_json::Value =
                    settings::parse_json_with_comments(&restored).expect("Resumed settings");
                assert_eq!(
                    restored["languages"]["Java"]["language_servers"],
                    servers.unwrap_or(serde_json::Value::Null)
                );
                assert_eq!(
                    restored["lsp"]["jdtls"]["binary"]["env"]["CUSTOM"],
                    "preserve"
                );
                assert!(
                    restored["lsp"]["jdtls"]["binary"]["env"]
                        .get(PAUSED_JAVA_SERVERS)
                        .is_none()
                );
            }
            assert!(
                paused_java_settings("{}".into(), root, cx).is_err(),
                "Unmanaged JDT settings must be preserved"
            );
            let managed = java_settings("{}".into(), root, cx).expect("Managed settings");
            let paused = paused_java_settings(managed, root, cx).expect("Paused settings");
            let mut edited: serde_json::Value =
                settings::parse_json_with_comments(&paused).expect("Paused settings");
            edited["languages"]["Java"]["language_servers"] = json!(["user-selected-java"]);
            let restored =
                java_settings(edited.to_string(), root, cx).expect("Resume with user edit");
            let restored: serde_json::Value =
                settings::parse_json_with_comments(&restored).expect("Resumed settings");
            assert_eq!(
                restored["languages"]["Java"]["language_servers"],
                json!(["user-selected-java"])
            );
            let managed = java_settings("{}".into(), root, cx).expect("Managed settings");
            let paused = paused_java_settings(managed, root, cx).expect("Pause managed Java");
            let target = AndroidTarget {
                module: ":app".into(),
                variant: "debug".into(),
                output_listing: root.join("output.json"),
            };
            let kotlin = official_kotlin_settings(
                paused,
                root,
                &target,
                Path::new("/jdk"),
                Path::new("/server/bin/kotlin-lsp"),
                cx,
            )
            .expect("Kotlin setup preserves the Java pause");
            let kotlin_json: serde_json::Value =
                settings::parse_json_with_comments(&kotlin).expect("Kotlin settings");
            assert_eq!(
                kotlin_json["languages"]["Java"]["language_servers"],
                json!(["kotlin-lsp", "!jdtls"])
            );
            let resumed = java_settings(kotlin, root, cx).expect("Resume Java after Kotlin");
            let resumed: serde_json::Value =
                settings::parse_json_with_comments(&resumed).expect("Resumed settings");
            assert_eq!(
                resumed["languages"]["Java"]["language_servers"],
                json!(["kotlin-lsp", "jdtls"])
            );
        });
    }

    #[gpui::test]
    async fn shared_model_invalidation_cancels_emulator_deploy_and_debug_followups(
        cx: &mut TestAppContext,
    ) {
        let _state = cx.update(AppState::test);
        let filesystem = FakeFs::new(cx.executor());
        filesystem.insert_tree("/android", json!({})).await;
        let project = Project::test(filesystem, [Path::new("/android")], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project.clone(), cx));
        panel.update(cx, |panel, cx| {
            let root = PathBuf::from("/android");
            let target = AndroidTarget {
                module: ":app".into(),
                variant: "debug".into(),
                output_listing: root.join("output.json"),
            };
            panel.root = Some(root);
            publish_test_android_model(panel, &target, cx);
            panel.followup_model_token = Some(panel.project.read(cx).android_model().token());
            panel.running = true;
            panel.deploy_task = Some(cx.spawn(async |_, _| futures::future::pending::<()>().await));
            panel.debug_task = Some(cx.spawn(async |_, _| futures::future::pending::<()>().await));
            panel.emulator_task =
                Some(cx.spawn(async |_, _| futures::future::pending::<()>().await));
        });
        cx.run_until_parked();
        assert!(panel.read_with(cx, |panel, _| panel.running));
        project.update(cx, |project, cx| {
            project.invalidate_android_model(Some("/android".into()), cx);
        });
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(!panel.running);
            assert!(panel.followup_model_token.is_none());
            assert!(panel.deploy_task.is_none());
            assert!(panel.debug_task.is_none());
            assert!(panel.emulator_task.is_none());
        });
    }

    #[cfg(unix)]
    fn emulator_test_executor() -> BackgroundExecutor {
        static DISPATCHER: std::sync::LazyLock<Arc<gpui::ThreadedDispatcher>> =
            std::sync::LazyLock::new(|| Arc::new(gpui::ThreadedDispatcher::new()));
        BackgroundExecutor::new(DISPATCHER.clone())
    }

    #[cfg(unix)]
    #[test]
    fn background_emulator_output_keeps_bounded_error_details() -> Result<()> {
        let executor = emulator_test_executor();
        let mut command = util::command::new_std_command("/bin/sh");
        command.args(["-c", "printf 'starting\\n'; head -c 200000 /dev/zero | tr '\\0' x >&2; printf '\\nLast error: é\\n' >&2; exit 7"]);
        let error = futures::executor::block_on(emulator_start_output(
            command,
            PathBuf::from("/bin/false"),
            "Selected",
            &executor,
        ))
        .err()
        .context("The failed emulator launcher unexpectedly succeeded.")?;
        let details = format!("{error:#}");
        assert!(details.contains("Last error: é"));
        assert!(details.contains("exit status: 7"));
        assert!(
            details.len() < 4200,
            "Startup diagnostics must remain bounded"
        );
        Ok(())
    }

    #[test]
    fn emulator_console_identity_rejects_failed_and_ambiguous_output() {
        assert_eq!(
            parse_avd_identity("Selected\nOK\n").expect("Valid AVD"),
            "Selected"
        );
        for output in [
            "KO: authentication failed\n",
            "Selected\n",
            "Selected\nOther\nOK\n",
            "-Selected\nOK\n",
        ] {
            assert!(parse_avd_identity(output).is_err(), "{output}");
        }
    }

    #[test]
    fn device_operation_lease_serializes_windows_and_releases_after_cancel() -> Result<()> {
        let first = DeviceOperationLease::acquire("lease-test-selected")?;
        assert!(DeviceOperationLease::acquire("lease-test-selected").is_err());
        let other = DeviceOperationLease::acquire("lease-test-other")?;
        drop(first);
        let retry = DeviceOperationLease::acquire("lease-test-selected")?;
        drop(retry);
        drop(other);
        Ok(())
    }

    #[cfg(unix)]
    fn emulator_process_fixture(
        directory: &Path,
        ready: bool,
    ) -> Result<(std::process::Command, PathBuf)> {
        use std::os::unix::fs::PermissionsExt as _;
        let adb = directory.join("adb");
        std::fs::write(
            &adb,
            format!(
                r#"#!/bin/sh
if [ "$1" = devices ]; then
  printf 'List of devices attached\n'
  if [ -f "${{0%/*}}/emulator.pid" ] && [ {ready} = true ]; then printf 'emulator-5556 device model:Selected\n'; fi
elif [ "$3" = emu ]; then
  printf 'Selected\nOK\n'
elif [ "$3" = shell ]; then
  printf '1\n'
else
  exit 7
fi
"#
            ),
        )?;
        std::fs::set_permissions(&adb, std::fs::Permissions::from_mode(0o755))?;
        let mut command = util::command::new_std_command("/bin/sh");
        command.args(["-c", r#"printf '%s\n' "$$" > "$1/emulator.pid"; sleep 60 & worker=$!; printf '%s\n' "$worker" > "$1/worker.pid"; trap 'kill "$worker"; wait "$worker"; exit' TERM INT; wait "$worker""#, "emulator-test"]).arg(directory);
        Ok((command, adb))
    }

    #[cfg(unix)]
    async fn test_process_is_running(process: &str) -> Result<bool> {
        let output = util::command::new_command("ps")
            .args(["-o", "stat=", "-p", process])
            .output()
            .await?;
        Ok(output.status.success()
            && String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .next()
                .is_some_and(|state| !state.starts_with('Z')))
    }

    #[cfg(unix)]
    #[test]
    fn cancelling_emulator_startup_terminates_its_owned_process_tree() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let (command, adb) = emulator_process_fixture(directory.path(), false)?;
        let executor = emulator_test_executor();
        futures::executor::block_on(async {
            let cancel = async {
                let wait = async {
                    while !directory.path().join("worker.pid").is_file() {
                        executor.timer(Duration::from_millis(10)).await;
                    }
                };
                match select(
                    Box::pin(wait),
                    Box::pin(executor.timer(Duration::from_secs(5))),
                )
                .await
                {
                    Either::Left(_) => {}
                    Either::Right(_) => bail!("The test emulator did not start."),
                }
                Ok::<_, anyhow::Error>(())
            };
            match select(
                Box::pin(emulator_start_output(command, adb, "Selected", &executor)),
                Box::pin(cancel),
            )
            .await
            {
                Either::Left((result, _)) => {
                    drop(result);
                    bail!("The unbooted test emulator unexpectedly completed startup.");
                }
                Either::Right((result, startup)) => {
                    result?;
                    drop(startup);
                }
            }
            let parent = std::fs::read_to_string(directory.path().join("emulator.pid"))?;
            let worker = std::fs::read_to_string(directory.path().join("worker.pid"))?;
            let stopped = async {
                while test_process_is_running(parent.trim()).await?
                    || test_process_is_running(worker.trim()).await?
                {
                    executor.timer(Duration::from_millis(10)).await;
                }
                Ok::<_, anyhow::Error>(())
            };
            match select(
                Box::pin(stopped),
                Box::pin(executor.timer(Duration::from_secs(5))),
            )
            .await
            {
                Either::Left((result, _)) => result,
                Either::Right(_) => {
                    bail!("Cancelling startup left an owned emulator process running.")
                }
            }
        })
    }

    #[cfg(unix)]
    #[test]
    fn cancelled_emulator_startup_never_spawns_after_preflight() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let (command, adb) = emulator_process_fixture(directory.path(), false)?;
        let executor = emulator_test_executor();
        let (cancel, cancelled) = oneshot::channel();
        drop(cancel);
        let result = futures::executor::block_on(emulator_start_output_with_cancel(
            command, adb, "Selected", &executor, cancelled,
        ));
        let error = result
            .err()
            .context("A cancelled startup unexpectedly launched the emulator.")?;
        assert!(error.is::<android_build::CommandCancelled>());
        assert!(!directory.path().join("emulator.pid").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn healthy_emulator_handoff_preserves_the_process_until_explicit_stop() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let (command, adb) = emulator_process_fixture(directory.path(), true)?;
        let executor = emulator_test_executor();
        let mut process = futures::executor::block_on(emulator_start_output(
            command, adb, "Selected", &executor,
        ))?;
        let parent = std::fs::read_to_string(directory.path().join("emulator.pid"))?;
        process.handoff(&executor)?;
        drop(process);
        let alive = futures::executor::block_on(test_process_is_running(parent.trim()))?;
        let status = futures::executor::block_on(
            util::command::new_command("/bin/kill")
                .args(["-TERM", parent.trim()])
                .status(),
        )?;
        ensure!(status.success(), "Could not stop the test emulator.");
        ensure!(
            alive,
            "A successful startup must preserve the emulator after handoff."
        );
        futures::executor::block_on(async {
            let stopped = async {
                while test_process_is_running(parent.trim()).await? {
                    executor.timer(Duration::from_millis(10)).await;
                }
                Ok::<_, anyhow::Error>(())
            };
            match select(
                Box::pin(stopped),
                Box::pin(executor.timer(Duration::from_secs(5))),
            )
            .await
            {
                Either::Left((result, _)) => result,
                Either::Right(_) => bail!("The handed-off test emulator did not stop."),
            }
        })
    }

    #[gpui::test]
    async fn deployment_stop_cancels_the_owned_followup_and_invalidates_its_command_guard(
        cx: &mut TestAppContext,
    ) {
        let _state = cx.update(AppState::test);
        let filesystem = FakeFs::new(cx.executor());
        filesystem.insert_tree("/android", json!({})).await;
        let project = Project::test(filesystem, [Path::new("/android")], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        panel.update_in(cx, |panel, window, cx| {
            let root = PathBuf::from("/android");
            let target = AndroidTarget {
                module: ":app".into(),
                variant: "debug".into(),
                output_listing: root.join("output.json"),
            };
            panel.root = Some(root.clone());
            panel.selected_target = Some(target.clone());
            panel.targets = vec![target.clone()];
            panel.devices = vec![Device {
                serial: "selected".into(),
                state: "device".into(),
                model: "Selected".into(),
            }];
            panel.selected_serial = Some("selected".into());
            publish_test_android_model(panel, &target, cx);
            let token = panel.project.read(cx).android_model().token();
            let (session_id, output, logs) = panel.build_panel.update(cx, |panel, cx| {
                panel.begin(BuildTab::Output, "Android Run".into(), false, window, cx)
            });
            logs.detach();
            let operation = Arc::new(DeviceOperation {
                root,
                serial: "selected".into(),
                model_token: token.clone(),
                operation_id: 1,
                session_id,
                target: Some(target),
                lease: DeviceOperationLease::acquire("selected").expect("Unclaimed test device"),
                allow_disconnect: false,
                selected_avd: None,
            });
            panel.active_operation_id = Some(1);
            panel.active_build_session = Some((BuildTab::Output, session_id));
            panel.followup_model_token = Some(token);
            panel.running = true;
            panel.debug_deployment = Some(DebugDeployment {
                operation: operation.clone(),
                adb: "/fake/adb".into(),
                environment: Arc::new(Default::default()),
                output,
            });
            panel.debug_task = Some(cx.spawn(async |_, _| futures::future::pending::<()>().await));
            panel.deploy_task = Some(cx.spawn(async |_, _| futures::future::pending::<()>().await));
            assert!(panel.validate_device_operation(&operation, cx).is_ok());
            panel.selected_serial = Some("other".into());
            assert!(panel.validate_device_operation(&operation, cx).is_err());
            panel.selected_serial = Some("selected".into());
            panel.cancel_build(BuildTab::Output, cx);
            assert!(panel.deploy_task.is_none());
            assert!(panel.debug_task.is_none());
            assert!(panel.debug_deployment.is_none());
            assert!(panel.followup_model_token.is_none());
            assert!(panel.active_operation_id.is_none());
            assert!(panel.validate_device_operation(&operation, cx).is_err());
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn acknowledged_emulator_stop_allows_refresh_disconnection_but_not_selection_changes(
        cx: &mut TestAppContext,
    ) {
        let _state = cx.update(AppState::test);
        let filesystem = FakeFs::new(cx.executor());
        filesystem.insert_tree("/android", json!({})).await;
        let project = Project::test(filesystem, [Path::new("/android")], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        panel.update_in(cx, |panel, window, cx| {
            panel.root = Some("/android".into());
            let target = AndroidTarget {
                module: ":app".into(),
                variant: "debug".into(),
                output_listing: "/android/output.json".into(),
            };
            publish_test_android_model(panel, &target, cx);
            panel.devices = vec![Device {
                serial: "emulator-5556".into(),
                state: "device".into(),
                model: "Selected".into(),
            }];
            panel.selected_serial = Some("emulator-5556".into());
            panel.selected_avd = Some("Selected".into());
            let (session_id, output, logs) = panel.build_panel.update(cx, |panel, cx| {
                panel.begin(BuildTab::Output, "Stop Emulator".into(), false, window, cx)
            });
            drop(output);
            logs.detach();
            panel.active_operation_id = Some(1);
            panel.active_build_session = Some((BuildTab::Output, session_id));
            let mut operation = DeviceOperation {
                root: "/android".into(),
                serial: "emulator-5556".into(),
                model_token: panel.project.read(cx).android_model().token(),
                operation_id: 1,
                session_id,
                target: None,
                lease: DeviceOperationLease::acquire("stop-refresh-test")
                    .expect("Unclaimed test device"),
                allow_disconnect: false,
                selected_avd: Some("Selected".into()),
            };
            assert!(panel.validate_device_operation(&operation, cx).is_ok());
            panel.devices.clear();
            panel.selected_serial = None;
            assert!(
                panel.validate_device_operation(&operation, cx).is_err(),
                "Missing devices must not be stopped before an acknowledged kill"
            );
            operation.allow_disconnect = true;
            assert!(
                panel.validate_device_operation(&operation, cx).is_ok(),
                "The expected device disappearance confirms successful shutdown"
            );
            panel.selected_avd = Some("Other".into());
            assert!(panel.validate_device_operation(&operation, cx).is_err());
            panel.cancel_build(BuildTab::Output, cx);
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn run_schedules_build_and_background_emulator_together_and_cancels_both(
        cx: &mut TestAppContext,
    ) {
        let _app_state = cx.update(AppState::test);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/android",
            json!({"settings.gradle.kts": "", "gradlew": ""}),
        )
        .await;
        let project = Project::test(fs, [Path::new("/android")], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        panel.update_in(cx, |panel, window, cx| {
            panel.root = Some(PathBuf::from("/android"));
            let target = AndroidTarget {
                module: ":app".into(),
                variant: "debug".into(),
                output_listing: "/android/output.json".into(),
            };
            panel.targets = vec![target.clone()];
            panel.selected_target = Some(target.clone());
            publish_test_android_model(panel, &target, cx);
            panel.emulators = vec!["Selected".into()];
            panel.selected_avd = Some("Selected".into());
            panel.gradle(GradleOperation::Run, window, cx);
            assert!(panel.running);
            assert!(
                panel.build_task.is_some(),
                "Build starts before emulator readiness"
            );
            assert!(
                panel
                    .active_build_session
                    .is_some_and(|(tab, _)| tab == BuildTab::Output)
            );
            assert!(panel.emulator_startup.is_some());
            assert!(panel.emulator_task.is_some());
            assert!(panel.deploy_task.is_none());
            assert!(panel.error.is_none());
            panel.cancel_build(BuildTab::Output, cx);
            assert!(!panel.running);
            assert!(panel.build_task.is_none());
            assert!(panel.emulator_startup.is_none());
            assert!(panel.emulator_task.is_none());
        });
    }

    #[gpui::test]
    async fn emulator_wait_handles_ready_failed_and_unfinished_startup(cx: &mut TestAppContext) {
        let executor = cx.background_executor.clone();
        let ready = emulator_ready_with_timeout(
            "Selected",
            futures::future::ready(Ok(())),
            futures::future::ready(Ok((Vec::new(), HashMap::new(), "emulator-5556".into()))),
            &executor,
            EMULATOR_BOOT_TIMEOUT,
        )
        .await
        .expect("Already booted emulator");
        assert_eq!(ready.2, "emulator-5556");
        let failed = emulator_ready_with_timeout(
            "Selected",
            futures::future::ready(Err("Emulator launch failed".into())),
            futures::future::pending(),
            &executor,
            EMULATOR_BOOT_TIMEOUT,
        )
        .await
        .expect_err("Launch failure must prevent deployment");
        assert_eq!(failed.to_string(), "Emulator launch failed");
        let timed_out = emulator_ready_with_timeout(
            "Selected",
            futures::future::pending(),
            futures::future::pending(),
            &executor,
            Duration::from_secs(2),
        )
        .await
        .expect_err("Unfinished startup must time out");
        assert!(timed_out.to_string().contains("within 2 seconds"));
    }

    #[gpui::test]
    async fn run_waits_for_emulator_only_after_build_and_times_out_without_deploying(
        cx: &mut TestAppContext,
    ) {
        check_post_build_emulator_wait(cx, WaitCancellation::Timeout).await;
    }

    #[gpui::test]
    async fn stop_build_cancels_post_build_emulator_wait_without_deploying(
        cx: &mut TestAppContext,
    ) {
        check_post_build_emulator_wait(cx, WaitCancellation::BuildPane).await;
    }

    #[gpui::test]
    async fn status_bar_cancels_post_build_emulator_wait_without_deploying(
        cx: &mut TestAppContext,
    ) {
        check_post_build_emulator_wait(cx, WaitCancellation::StatusBar).await;
    }

    enum WaitCancellation {
        Timeout,
        BuildPane,
        StatusBar,
    }

    async fn check_post_build_emulator_wait(
        cx: &mut TestAppContext,
        cancellation: WaitCancellation,
    ) {
        let _app_state = cx.update(AppState::test);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/android",
            json!({"settings.gradle.kts": "", "gradlew": ""}),
        )
        .await;
        let project = Project::test(fs, [Path::new("/android")], cx).await;
        let root = PathBuf::from("/android");
        let worktree_id = project.read_with(cx, |project, cx| {
            project
                .visible_worktrees(cx)
                .next()
                .expect("Worktree")
                .read(cx)
                .id()
        });
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        let activity = cx.new(|cx| android_status::AndroidActivity::new(&panel, cx));
        workspace.update_in(cx, |workspace, window, cx| {
            workspace
                .status_bar()
                .update(cx, |bar, cx| bar.add_right_item(activity, window, cx));
        });
        cx.update(|_, cx| cx.set_reduce_motion(true));
        let target = AndroidTarget {
            module: ":app".into(),
            variant: "debug".into(),
            output_listing: root.join("output.json"),
        };
        let (started, startup) = oneshot::channel::<()>();
        panel.update(cx, |panel, cx| {
            panel.root = Some(root.clone());
            panel.targets = vec![target.clone()];
            panel.selected_target = Some(target.clone());
            panel.running = true;
            panel.emulator_startup = Some(EmulatorStartup {
                root: root.clone(),
                name: "Selected".into(),
                error_reported: false,
                ready: cx
                    .spawn(async move |_, _| startup.await.map_err(|error| error.to_string()))
                    .shared(),
            });
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(300));
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(panel.running);
            assert!(
                panel.emulator_task.is_none(),
                "Build does not wait for startup"
            );
            assert!(panel.error.is_none());
        });
        panel.update_in(cx, |panel, window, cx| {
            let id = panel.build_panel.update(cx, |pane, cx| {
                let (id, output, logs) =
                    pane.begin(BuildTab::Output, "Run Android".into(), false, window, cx);
                drop(output);
                logs.detach();
                id
            });
            panel.active_build_session = Some((BuildTab::Output, id));
            publish_test_android_model(panel, &target, cx);
            let model_token = panel.project.read(cx).android_model().token();
            panel.complete_build(
                &root,
                worktree_id,
                id,
                &model_token,
                Some(AfterTask::DeployOnEmulator(
                    target,
                    "Selected".into(),
                    false,
                )),
                Ok(ProcessOutput::Success(String::new())),
                window,
                cx,
            )
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(119));
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(panel.running);
            assert!(panel.emulator_task.is_some());
            assert!(
                panel.build_task.is_none(),
                "Emulator readiness must not rebuild"
            );
            assert!(panel.deploy_task.is_none());
            assert!(panel.error.is_none());
        });
        if !matches!(cancellation, WaitCancellation::Timeout) {
            match cancellation {
                WaitCancellation::BuildPane => panel.update(cx, |panel, cx| {
                    assert!(panel.active_build_session.is_some());
                    panel
                        .build_panel
                        .update(cx, |_, cx| cx.emit(BuildEvent::Stop(BuildTab::Output)));
                }),
                WaitCancellation::StatusBar => {
                    let cancel = cx
                        .debug_bounds("cancel-android-operation")
                        .expect("Status bar cancellation during boot wait");
                    cx.simulate_click(cancel.center(), Default::default());
                }
                WaitCancellation::Timeout => unreachable!(),
            }
            cx.run_until_parked();
            panel.read_with(cx, |panel, _| {
                assert!(!panel.running);
                assert!(panel.active_build_session.is_none());
                assert!(panel.emulator_task.is_none());
                assert!(panel.emulator_startup.is_none());
                assert!(panel.deploy_task.is_none());
                assert!(panel.error.is_none());
            });
            assert!(started.send(()).is_err(), "Stop cancels pending startup");
            cx.executor().advance_clock(Duration::from_secs(2));
            cx.run_until_parked();
            assert!(panel.read_with(cx, |panel, _| panel.error.is_none()));
            return;
        }
        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(!panel.running);
            assert!(panel.active_build_session.is_none());
            assert!(panel.emulator_startup.is_none());
            assert!(panel.deploy_task.is_none());
            assert!(
                panel
                    .error
                    .as_ref()
                    .is_some_and(|error| error.contains("within 120 seconds"))
            );
        });
        assert!(started.send(()).is_err(), "Timeout cancels pending startup");
    }

    #[cfg(unix)]
    #[test]
    fn emulator_startup_checks_live_devices_after_an_emulator_closes() -> Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir()?;
        let adb = directory.path().join("adb");
        std::fs::write(
            &adb,
            r#"#!/bin/sh
if [ "$1" = devices ]; then
  printf 'List of devices attached\n'
  if [ ! -f "${0%/*}/closed" ]; then printf 'emulator-5556 device model:Selected\n'; fi
elif [ "$3" = emu ]; then
  printf 'Selected\nOK\n'
else
  exit 7
fi
"#,
        )?;
        std::fs::set_permissions(&adb, std::fs::Permissions::from_mode(0o755))?;
        let executor = emulator_test_executor();
        let (cached_devices, cached_serials) =
            futures::executor::block_on(connected_devices_with_adb(adb.clone(), &executor))?;
        assert!(futures::executor::block_on(emulator_is_running_with_adb(
            "Selected",
            adb.clone(),
            &executor
        ))?);
        assert!(!futures::executor::block_on(emulator_is_running_with_adb(
            "Other",
            adb.clone(),
            &executor
        ))?);
        std::fs::write(directory.path().join("closed"), "")?;
        assert!(cached_devices[0].is_available());
        assert!(cached_serials.contains_key("Selected"));
        assert!(
            !futures::executor::block_on(emulator_is_running_with_adb("Selected", adb, &executor))?,
            "A stale UI snapshot must not skip startup"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn emulator_readiness_checks_selected_avd_and_waits_for_full_boot() -> Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir()?;
        let adb = directory.path().join("adb");
        std::fs::write(
            &adb,
            r#"#!/bin/sh
if [ "$1" = devices ]; then
  printf 'List of devices attached\nemulator-5554 device model:Other\nemulator-5556 device model:Selected\n'
elif [ "$3" = emu ]; then
  if [ "$2" = emulator-5554 ]; then printf 'Other\nOK\n'; else printf x >> "${0%/*}/name-queries"; printf 'Selected\nOK\n'; fi
elif [ "$3" = shell ]; then
  [ "$2" = emulator-5556 ] || exit 9
  [ "$4" = getprop ] && [ "$5" = sys.boot_completed ] || exit 8
  boot_marker="${0%/*}/boot-checked"
  if [ ! -f "$boot_marker" ]; then printf 1 > "$boot_marker"; printf '0\n'; elif [ "$(cat "$boot_marker")" = 1 ]; then printf 2 > "$boot_marker"; printf '0\n'; else printf '1\n'; fi
else
  exit 7
fi
"#,
        )?;
        std::fs::set_permissions(&adb, std::fs::Permissions::from_mode(0o755))?;
        let executor = emulator_test_executor();
        let (devices, serials, serial) = futures::executor::block_on(emulator_ready_with_timeout(
            "Selected",
            futures::future::ready(Ok(())),
            booted_emulator_with_adb("Selected", adb, &executor),
            &executor,
            Duration::from_secs(5),
        ))?;
        assert_eq!(serial, "emulator-5556");
        assert_eq!(serials.get("Selected"), Some(&serial));
        assert_eq!(devices.len(), 2);
        assert!(directory.path().join("boot-checked").is_file());
        assert_eq!(
            std::fs::read_to_string(directory.path().join("name-queries"))?,
            "xx",
            "Resolve once and revalidate identity after boot; only the boot property is polled in between"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn emulator_readiness_revalidates_avd_when_a_serial_is_reused() -> Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir()?;
        let adb = directory.path().join("adb");
        std::fs::write(
            &adb,
            r#"#!/bin/sh
swap_marker="${0%/*}/serial-reused"
if [ "$1" = devices ]; then
  if [ -f "$swap_marker" ]; then printf 'List of devices attached\nemulator-5556 device model:Other\nemulator-5558 device model:Selected\n'; else printf 'List of devices attached\nemulator-5556 device model:Selected\n'; fi
elif [ "$3" = emu ]; then
  if [ "$2" = emulator-5556 ] && [ -f "$swap_marker" ]; then printf 'Other\nOK\n'; else printf 'Selected\nOK\n'; fi
elif [ "$3" = shell ]; then
  touch "$swap_marker"
  printf '1\n'
else
  exit 7
fi
"#,
        )?;
        std::fs::set_permissions(&adb, std::fs::Permissions::from_mode(0o755))?;
        let executor = emulator_test_executor();
        let (_, _, serial) = futures::executor::block_on(emulator_ready_with_timeout(
            "Selected",
            futures::future::ready(Ok(())),
            booted_emulator_with_adb("Selected", adb, &executor),
            &executor,
            Duration::from_secs(5),
        ))?;
        assert_eq!(
            serial, "emulator-5558",
            "Never deploy to a different AVD that reused the cached serial"
        );
        Ok(())
    }

    #[test]
    fn android_model_inputs_include_build_logic_and_exclude_outputs() {
        for path in [
            "build.gradle.kts",
            "settings.gradle",
            "buildSrc/src/main/kotlin/AndroidPlugin.kt",
            "build-logic/src/main/groovy/conventions.groovy",
            "build-logic/src/main/resources/META-INF/gradle-plugins/android.properties",
            "gradle/catalogs/custom.toml",
            "libs.versions.toml",
            "mobile/gradle/dependencies.toml",
            "gradle.lockfile",
            "mobile/gradle.lockfile",
            "gradle/wrapper/gradle-wrapper.properties",
            "mobile/src/main/AndroidManifest.xml",
            "mobile/src/demo/res/values/strings.xml",
            "src/main/res",
        ] {
            assert!(
                android_model_input(
                    &RelPath::new(Path::new(path), util::paths::PathStyle::Unix).unwrap()
                ),
                "{path}"
            );
        }
        for path in [
            "mobile/build/generated/res/values/values.xml",
            "buildSrc/build/classes/AndroidPlugin.kt",
            "build-logic/.gradle/state.gradle",
            ".koda/settings.json",
            ".koda/android-kotlin-official/system/gradle.properties",
            ".git/config",
            "generated/AndroidManifest.xml",
            "mobile/src/main/kotlin/Main.kt",
            "README.md",
            "catalog.toml",
        ] {
            assert!(
                !android_model_input(
                    &RelPath::new(Path::new(path), util::paths::PathStyle::Unix).unwrap()
                ),
                "{path}"
            );
        }
    }

    #[gpui::test]
    async fn stop_sync_cancels_pending_import_after_the_command_exits(cx: &mut TestAppContext) {
        let _app_state = cx.update(AppState::test);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/android", json!({"README.md": ""})).await;
        let project = Project::test(fs, [Path::new("/android")], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        let (send_cancel, receiver) = oneshot::channel();
        drop(receiver);
        panel.update_in(cx, |panel, window, cx| {
            let (id, output, logs) = panel.build_panel.update(cx, |pane, cx| {
                pane.begin(
                    BuildTab::Sync,
                    "Waiting for Kotlin import".into(),
                    false,
                    window,
                    cx,
                )
            });
            drop(output);
            logs.detach();
            panel.command_cancel = Some(send_cancel);
            panel.active_build_session = Some((BuildTab::Sync, id));
            panel.running = true;
            panel.kotlin_task = Some(cx.spawn(async |_, _| futures::future::pending().await));
            panel.cancel_build(BuildTab::Sync, cx);
            assert!(!panel.running);
            assert!(panel.active_build_session.is_none() && panel.kotlin_task.is_none());
            assert_eq!(
                panel.kotlin_setup_error.as_deref(),
                Some("Kotlin setup cancelled")
            );
        });
    }

    #[gpui::test]
    async fn managed_tool_setup_cancel_and_root_removal_clear_busy_state(cx: &mut TestAppContext) {
        let _app_state = cx.update(AppState::test);
        let filesystem = FakeFs::new(cx.executor());
        filesystem
            .insert_tree("/android", json!({"README.md": ""}))
            .await;
        let project = Project::test(filesystem, [Path::new("/android")], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        for closed in [false, true] {
            panel.update_in(cx, |panel, window, cx| {
                let (session, output, logs) = panel.build_panel.update(cx, |build, cx| {
                    build.begin(BuildTab::Output, "Install tools".into(), false, window, cx)
                });
                drop(output);
                logs.detach();
                panel.active_build_session = Some((BuildTab::Output, session));
                panel.root = Some(if closed {
                    PathBuf::from("/closed")
                } else {
                    PathBuf::from("/android")
                });
                panel.running = true;
                panel.tool_setup.operation =
                    Some(cx.spawn(async |_, _| futures::future::pending().await));
                if closed {
                    panel.auto_sync_project(window, cx);
                } else {
                    panel.cancel_build(BuildTab::Output, cx);
                }
                assert!(!panel.running);
                assert!(panel.active_build_session.is_none());
                assert!(panel.tool_setup.operation.is_none());
                assert!(panel.command_cancel.is_none());
            });
        }
    }

    #[gpui::test]
    async fn managed_tool_setup_rerun_retains_tool_intent(cx: &mut TestAppContext) {
        let _app_state = cx.update(AppState::test);
        let filesystem = FakeFs::new(cx.executor());
        filesystem
            .insert_tree("/android", json!({"README.md": ""}))
            .await;
        let project = Project::test(filesystem, [Path::new("/android")], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        panel.update_in(cx, |panel, window, cx| {
            panel.observe_project_open(window, cx);
            panel.last_build_operation = Some(GradleOperation::Run);
            panel.tool_setup.last_operation =
                Some((android_tools::managed::Tool::Debugger, "invalid"));
            panel
                .build_panel
                .update(cx, |_, cx| cx.emit(BuildEvent::Rerun(BuildTab::Output)));
        });
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(panel.last_build_operation.is_none());
            assert_eq!(
                panel.tool_setup.last_operation,
                Some((android_tools::managed::Tool::Debugger, "invalid"))
            );
            assert!(panel.error.as_ref().is_some_and(|error| {
                error.contains("provisioning supports Apple Silicon")
                    || error.contains("Invalid tool operation")
            }));
            assert!(!panel.running);
        });
    }

    #[gpui::test]
    async fn completed_build_does_not_continue_in_a_different_project(cx: &mut TestAppContext) {
        let _app_state = cx.update(AppState::test);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/android-a",
            json!({"settings.gradle.kts": "", "gradlew": ""}),
        )
        .await;
        fs.insert_tree(
            "/android-b",
            json!({"settings.gradle.kts": "", "gradlew": ""}),
        )
        .await;
        let project =
            Project::test(fs, [Path::new("/android-a"), Path::new("/android-b")], cx).await;
        let root = PathBuf::from("/android-a");
        let worktree_id = project.read_with(cx, |project, cx| {
            project
                .visible_worktrees(cx)
                .find(|worktree| worktree.read(cx).abs_path().as_ref() == root.as_path())
                .unwrap()
                .read(cx)
                .id()
        });
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project.clone(), cx));
        let target = AndroidTarget {
            module: ":mobile".into(),
            variant: "debug".into(),
            output_listing: root.join("output.json"),
        };
        panel.update(cx, |panel, _| {
            panel.root = Some(root.clone());
            panel.targets = vec![target.clone()];
            panel.selected_target = Some(target.clone());
            panel.startup_settings_ready = true;
            panel.running = true;
        });
        project.update(cx, |project, cx| project.remove_worktree(worktree_id, cx));
        panel.update_in(cx, |panel, window, cx| {
            panel.auto_sync_project(window, cx);
            assert!(panel.running, "the original Gradle task is still pending");
            assert!(panel.root.is_none());
            assert_eq!(panel.trusted_root(cx).unwrap(), PathBuf::from("/android-b"));
            for after_task in [
                AfterTask::Java(target.clone()),
                AfterTask::DeployOnEmulator(target.clone(), "Selected".into(), false),
                AfterTask::Preview(target),
            ] {
                panel.running = true;
                let model_token = panel.project.read(cx).android_model().token();
                panel.complete_scheduled_task(
                    &root,
                    worktree_id,
                    &model_token,
                    Some(after_task),
                    ScheduledTaskResult::Success,
                    window,
                    cx,
                );
                assert!(!panel.running);
                assert!(
                    panel.java_task.is_none(),
                    "must not start B's Gradle export"
                );
                assert!(panel.preview_view.is_none(), "must not start B's renderer");
                assert!(
                    panel.emulator_task.is_none(),
                    "must not deploy in another root"
                );
                assert!(
                    panel
                        .error
                        .as_ref()
                        .is_some_and(|error| { error.contains("project changed during the task") })
                );
            }
        });
    }

    #[gpui::test]
    async fn official_kotlin_refresh_preserves_pending_work_and_rejects_stale_publication(
        cx: &mut TestAppContext,
    ) {
        let _app_state = cx.update(AppState::test);
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let settings = json!({"languages": {"Kotlin": {"language_servers": ["kotlin-lsp"]}}, "lsp": {"kotlin-lsp": {"binary": {"arguments": [format!("--system-path={}", root.join(".koda/android-kotlin-official/system").display())]}}}}).to_string();
        std::fs::create_dir(root.join(".koda")).unwrap();
        std::fs::write(root.join(".koda/settings.json"), &settings).unwrap();
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            &root,
            json!({".koda": {"settings.json": settings}, "main.kt": "fun main() {}"}),
        )
        .await;
        let project = Project::test(fs, [root.as_path()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project.clone(), cx));
        let target = AndroidTarget {
            module: ":mobile".into(),
            variant: "demoDebug".into(),
            output_listing: root.join("output.json"),
        };
        panel.update(cx, |panel, cx| {
            panel.root = Some(root.clone());
            panel.targets = vec![target.clone()];
            panel.selected_target = Some(target.clone());
            panel.running = true;
            assert!(panel.official_kotlin_state(cx).is_some());
        });
        panel.update_in(cx, |panel, window, cx| {
            panel.queue_official_kotlin_refresh(window, cx)
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(300));
        panel.update_in(cx, |panel, window, cx| {
            panel.queue_official_kotlin_refresh(window, cx)
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(300));
        cx.run_until_parked();
        assert!(panel.read_with(cx, |panel, _| panel.kotlin_refresh_pending.is_none()));
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        panel.update(cx, |panel, cx| {
            assert_eq!(panel.kotlin_refresh_pending.as_ref(), Some(&root));
            assert!(!panel.take_official_kotlin_refresh(cx));
            panel.running = false;
            panel.syncing = true;
            assert!(!panel.take_official_kotlin_refresh(cx));
            panel.syncing = false;
            assert!(panel.take_official_kotlin_refresh(cx));
            assert!(!panel.take_official_kotlin_refresh(cx));
            panel.kotlin_refresh_pending = Some(root.join("closed-project"));
            assert!(!panel.take_official_kotlin_refresh(cx));
            assert!(panel.kotlin_refresh_pending.is_none());
        });
        let updated = "{\"published\":true}";
        panel.update(cx, |panel, cx| {
            panel.selected_target = Some(AndroidTarget {
                variant: "fullRelease".into(),
                ..target.clone()
            });
            assert!(
                panel
                    .publish_official_kotlin_settings(&root, &target, &settings, updated, cx)
                    .is_err()
            );
            assert_eq!(
                std::fs::read_to_string(root.join(".koda/settings.json")).unwrap(),
                settings
            );
            panel.selected_target = Some(target.clone());
            panel.targets.clear();
            assert!(
                panel
                    .publish_official_kotlin_settings(&root, &target, &settings, updated, cx)
                    .is_err()
            );
            panel.targets.push(target.clone());
            panel
                .publish_official_kotlin_settings(&root, &target, &settings, updated, cx)
                .unwrap();
            assert_eq!(
                std::fs::read_to_string(root.join(".koda/settings.json")).unwrap(),
                updated
            );
            assert!(
                panel
                    .publish_official_kotlin_settings(&root, &target, &settings, "{}", cx)
                    .is_err()
            );
            assert_eq!(
                std::fs::read_to_string(root.join(".koda/settings.json")).unwrap(),
                updated
            );
        });
        project.update(cx, |project, cx| {
            let id = project.worktrees(cx).next().unwrap().read(cx).id();
            project.remove_worktree(id, cx);
        });
        panel.update(cx, |panel, cx| {
            assert!(
                panel
                    .publish_official_kotlin_settings(&root, &target, updated, "{}", cx)
                    .is_err()
            );
            panel.kotlin_refresh_pending = Some(root.clone());
            assert!(!panel.take_official_kotlin_refresh(cx));
        });
    }

    #[gpui::test]
    async fn official_kotlin_setup_coordinates_library_restoration(cx: &mut TestAppContext) {
        use futures::{FutureExt as _, StreamExt as _};
        use language::{FakeLspAdapter, Language, LanguageConfig, LanguageMatcher};

        enum ImportState {}
        impl lsp::notification::Notification for ImportState {
            type Params = serde_json::Value;
            const METHOD: &'static str = "intellij/workspaceImportState";
        }
        let imported = json!({"phase": "FINISHED", "folders": [{"status": "SUCCESS"}]});
        let _app_state = cx.update(AppState::test);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/project", json!({"main.kt": "fun main() {}"}))
            .await;
        let project = Project::test(fs, [Path::new("/project")], cx).await;
        let languages = project.read_with(cx, |project, _| project.languages().clone());
        languages.add(Arc::new(Language::new(
            LanguageConfig {
                name: "Kotlin".into(),
                matcher: LanguageMatcher {
                    path_suffixes: vec!["kt".into()],
                    ..Default::default()
                }
                .into(),
                ..Default::default()
            },
            None,
        )));
        let mut capabilities = lsp::LanguageServer::full_capabilities();
        capabilities.execute_command_provider = Some(lsp::ExecuteCommandOptions {
            commands: vec!["decompile".into()],
            ..Default::default()
        });
        let mut servers = languages.register_fake_lsp(
            "Kotlin",
            FakeLspAdapter {
                name: "kotlin-lsp",
                capabilities,
                initializer: Some(Box::new(|server| {
                    server.set_request_handler::<lsp::request::Shutdown, _, _>(|_, _| async {
                        Ok(())
                    });
                    server.set_request_handler::<lsp::request::ExecuteCommand, _, _>(
                        |_, _| async {
                            Ok(Some(
                                json!({"code": "fun library() {}", "language": "Kotlin"}),
                            ))
                        },
                    );
                })),
                ..Default::default()
            },
        );
        let (_source, _handle) = project
            .update(cx, |project, cx| {
                project.open_local_buffer_with_lsp(Path::new("/project/main.kt"), cx)
            })
            .await
            .unwrap();
        let server = servers.next().await.unwrap();
        server.notify::<ImportState>(imported.clone());
        cx.run_until_parked();
        let library = project
            .update(cx, |project, cx| {
                project.open_local_buffer_via_lsp(
                    "jar:///cache/library.jar!/Library.class".parse().unwrap(),
                    server.server.server_id(),
                    cx,
                )
            })
            .await
            .unwrap();
        let store = project.read_with(cx, |project, _| project.lsp_store());
        let location = store
            .read_with(cx, |store, cx| {
                store.language_server_document_location(library.read(cx), cx)
            })
            .unwrap()
            .unwrap();
        let location = serde_json::from_value(serde_json::to_value(location).unwrap()).unwrap();
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project.clone(), cx));
        panel.update_in(cx, |panel, window, cx| {
            panel.root = Some(PathBuf::from("/project"));
            panel.syncing = true;
            panel.observe_project_open(window, cx);
        });
        let mut restore = store.update(cx, |store, cx| {
            store.restore_language_server_document(location, cx)
        });
        cx.run_until_parked();
        assert!(panel.read_with(cx, |panel, _| panel.startup_settings_ready));
        assert!(
            (&mut restore).now_or_never().is_none(),
            "Startup sync must hold persisted restoration even when the old import succeeded"
        );
        panel.update_in(cx, |panel, window, cx| {
            panel.coordinate_kotlin_setup(PathBuf::from("/project"), false, window, cx);
            panel
                .restart_language_server(Path::new("/project"), "kotlin-lsp", cx)
                .detach_and_log_err(cx);
        });
        let replacement = servers.next().await.unwrap();
        replacement.notify::<ImportState>(imported);
        cx.run_until_parked();
        assert!(
            (&mut restore).now_or_never().is_none(),
            "A replacement import cannot bypass unfinished managed setup"
        );
        panel.update(cx, |panel, cx| {
            panel.syncing = false;
            cx.notify();
        });
        let restored = restore.await.unwrap();
        restored.read_with(cx, |buffer, _| {
            assert_eq!(buffer.capability(), language::Capability::ReadOnly);
            assert_eq!(
                buffer.language_server_document().unwrap().server_id,
                replacement.server.server_id()
            );
        });
    }

    #[gpui::test]
    async fn official_kotlin_restart_preserves_other_roots_and_servers(cx: &mut TestAppContext) {
        use futures::{FutureExt as _, StreamExt as _};
        use language::{FakeLspAdapter, Language, LanguageConfig, LanguageMatcher};

        let _app_state = cx.update(AppState::test);
        let fs = FakeFs::new(cx.executor());
        for root in ["/first", "/second", "/third"] {
            let servers = if root == "/first" {
                json!(["kotlin-lsp"])
            } else {
                json!(["kotlin-lsp", "other-server"])
            };
            fs.insert_tree(
                root,
                json!({"main.kt": "fun main() {}", "other.kt": "fun other() {}", ".koda": {"settings.json": json!({"languages": {"Kotlin": {"language_servers": servers}}}).to_string()}}),
            )
            .await;
        }
        let project = Project::test(
            fs,
            [
                Path::new("/first"),
                Path::new("/second"),
                Path::new("/third"),
            ],
            cx,
        )
        .await;
        let languages = project.read_with(cx, |project, _| project.languages().clone());
        languages.add(Arc::new(Language::new(
            LanguageConfig {
                name: "Kotlin".into(),
                matcher: LanguageMatcher {
                    path_suffixes: vec!["kt".into()],
                    ..Default::default()
                }
                .into(),
                ..Default::default()
            },
            None,
        )));
        let adapter = |name| FakeLspAdapter {
            name,
            initializer: Some(Box::new(|server| {
                server.set_request_handler::<lsp::request::Shutdown, _, _>(|_, _| async { Ok(()) });
            })),
            ..Default::default()
        };
        let mut official_servers = languages.register_fake_lsp("Kotlin", adapter("kotlin-lsp"));
        let mut other_servers = languages.register_fake_lsp("Kotlin", adapter("other-server"));
        let mut buffers = Vec::new();
        let mut old_official = Vec::new();
        let mut unrelated = Vec::new();
        for root in ["/first", "/second"] {
            for filename in ["main.kt", "other.kt"] {
                buffers.push(
                    project
                        .update(cx, |project, cx| {
                            project.open_local_buffer_with_lsp(Path::new(root).join(filename), cx)
                        })
                        .await
                        .unwrap(),
                );
            }
            old_official.push(official_servers.next().await.unwrap());
            if root == "/second" {
                unrelated.push(other_servers.next().await.unwrap());
            }
            cx.run_until_parked();
        }
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project.clone(), cx));
        panel.update(cx, |panel, cx| {
            panel
                .restart_language_server(Path::new("/first"), "kotlin-lsp", cx)
                .detach_and_log_err(cx)
        });
        cx.run_until_parked();
        let mut restarted = official_servers
            .next()
            .now_or_never()
            .flatten()
            .expect("Selected root must restart by server ID");
        cx.run_until_parked();
        let store = project.read_with(cx, |project, _| project.lsp_store());
        store.read_with(cx, |store, _| {
            assert!(
                store
                    .language_server_for_id(old_official[0].server.server_id())
                    .is_none()
            );
            assert!(
                store
                    .language_server_for_id(old_official[1].server.server_id())
                    .is_some()
            );
            assert!(
                store
                    .language_server_for_id(restarted.server.server_id())
                    .is_some()
            );
            for server in &unrelated {
                assert!(
                    store
                        .language_server_for_id(server.server.server_id())
                        .is_some()
                );
            }
        });
        let mut opened = Vec::new();
        for _ in 0..2 {
            let notification = restarted
                .receive_notification::<lsp::notification::DidOpenTextDocument>()
                .now_or_never()
                .expect("Both source buffers must register after restart");
            opened.push(notification.text_document.uri);
        }
        opened.sort();
        assert_eq!(
            opened,
            vec![
                lsp::Uri::from_file_path("/first/main.kt").unwrap(),
                lsp::Uri::from_file_path("/first/other.kt").unwrap()
            ]
        );
        assert!(official_servers.try_recv().is_err());
        assert!(other_servers.try_recv().is_err());
        let unopened = project
            .update(cx, |project, cx| {
                project.open_local_buffer(Path::new("/third/main.kt"), cx)
            })
            .await
            .unwrap();
        cx.run_until_parked();
        store.update(cx, |store, cx| {
            store.restart_language_servers_for_buffers(
                vec![unopened],
                collections::HashSet::from_iter([lsp::LanguageServerSelector::Id(
                    old_official[0].server.server_id(),
                )]),
                true,
                cx,
            );
        });
        cx.run_until_parked();
        assert!(
            official_servers.try_recv().is_err(),
            "A stale explicit restart ID must not start other owners"
        );
        assert!(
            other_servers.try_recv().is_err(),
            "A stale explicit restart ID must not start unrelated adapters"
        );
        let current = json!({"languages": {"Kotlin": {"language_servers": ["kotlin-lsp"]}}, "lsp": {"kotlin-lsp": {"binary": {"arguments": ["--system-path=/first/.koda/android-kotlin-official/system"], "env": {"LSP_ANDROID_VARIANT": "demoDebug"}}}}}).to_string();
        let paused = cx
            .update(|_, cx| paused_official_kotlin_settings(current, Path::new("/first"), cx))
            .unwrap();
        let worktree_id = project.read_with(cx, |project, cx| {
            project
                .find_project_path(Path::new("/first/main.kt"), cx)
                .unwrap()
                .worktree_id
        });
        cx.update(|_, cx| {
            cx.update_global::<settings::SettingsStore, _>(|settings, cx| {
                settings
                    .set_local_settings(
                        worktree_id,
                        settings::LocalSettingsPath::InWorktree(RelPath::empty_arc()),
                        settings::LocalSettingsKind::Settings,
                        Some(&paused),
                        cx,
                    )
                    .unwrap();
            })
        });
        cx.run_until_parked();
        panel.update(cx, |panel, cx| {
            panel.root = Some(PathBuf::from("/first"));
            assert_eq!(
                panel.official_kotlin_state(cx),
                Some(OfficialKotlinState::Paused)
            );
        });
        store.read_with(cx, |store, _| {
            assert!(
                store
                    .language_server_for_id(restarted.server.server_id())
                    .is_none(),
                "Unavailable variant must stop the obsolete engine"
            );
            assert!(
                store
                    .language_server_for_id(old_official[1].server.server_id())
                    .is_some(),
                "Other root keeps its official server"
            );
            assert!(
                store
                    .language_server_for_id(unrelated[0].server.server_id())
                    .is_some()
            );
        });
        let custom_settings =
            json!({"languages": {"Kotlin": {"language_servers": ["custom-kotlin"]}}}).to_string();
        cx.update(|_, cx| {
            cx.update_global::<settings::SettingsStore, _>(|settings, cx| {
                settings
                    .set_local_settings(
                        worktree_id,
                        settings::LocalSettingsPath::InWorktree(RelPath::empty_arc()),
                        settings::LocalSettingsKind::Settings,
                        Some(&custom_settings),
                        cx,
                    )
                    .unwrap();
            })
        });
        panel.read_with(cx, |panel, cx| {
            assert_eq!(
                panel.official_kotlin_state(cx),
                None,
                "A deliberate custom server selection must not auto-resume"
            )
        });
    }

    #[gpui::test]
    async fn default_kotlin_configuration_requires_android_discovery_and_preserves_overrides(
        cx: &mut TestAppContext,
    ) {
        let _state = cx.update(|cx| {
            let state = AppState::test(cx);
            trusted_worktrees::init(Default::default(), cx);
            state
        });
        let root = PathBuf::from("/default-android-kotlin");
        let filesystem = FakeFs::new(cx.executor());
        filesystem.insert_tree(&root, json!({"app": {}})).await;
        let project = Project::test_with_worktree_trust(filesystem, [root.as_path()], cx).await;
        let worktrees = project.read_with(cx, |project, _| project.worktree_store());
        cx.update(|cx| {
            TrustedWorktrees::try_get_global(cx)
                .unwrap()
                .update(cx, |trusted, cx| {
                    trusted.trust(
                        &worktrees,
                        [PathTrust::AbsPath(root.clone())].into_iter().collect(),
                        cx,
                    );
                });
        });
        let worktree_id = project.read_with(cx, |project, cx| {
            project.visible_worktrees(cx).next().unwrap().read(cx).id()
        });
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project.clone(), cx));
        let target = AndroidTarget {
            module: ":app".into(),
            variant: "debug".into(),
            output_listing: root.join("output.json"),
        };
        panel.update(cx, |panel, cx| {
            panel.root = Some(root.clone());
            panel.targets = vec![target.clone()];
            panel.selected_target = Some(target.clone());
            assert!(
                !panel.default_official_kotlin_eligible(cx),
                "A Gradle folder without a discovered Android model must keep its backend"
            );
            publish_test_android_model(panel, &target, cx);
            assert!(panel.default_official_kotlin_eligible(cx));
            assert!(
                !panel.should_configure_official_kotlin(cx),
                "A shared Zed/test application must not opt into Koda's runtime policy"
            );
        });

        let set_local = |value: &serde_json::Value, cx: &mut gpui::VisualTestContext| {
            cx.update(|_, cx| {
                cx.update_global::<settings::SettingsStore, _>(|store, cx| {
                    store
                        .set_local_settings(
                            worktree_id,
                            settings::LocalSettingsPath::InWorktree(RelPath::empty_arc()),
                            settings::LocalSettingsKind::Settings,
                            Some(&value.to_string()),
                            cx,
                        )
                        .unwrap();
                });
            });
        };
        for (preferences, expected) in [
            (json!({}), true),
            (
                json!({"languages": {"Kotlin": {"language_servers": ["kotlin-lsp"]}}}),
                true,
            ),
            (
                json!({"lsp": {"kotlin-lsp": {"binary": {"arguments": [], "env": {}}, "initialization_options": {}}}}),
                true,
            ),
            (
                json!({"lsp": {"kotlin-lsp": {"initialization_options": null}}}),
                true,
            ),
            (
                json!({"languages": {"Kotlin": {"enable_language_server": false}}}),
                false,
            ),
            (
                json!({"languages": {"Kotlin": {"language_servers": []}}}),
                false,
            ),
            (
                json!({"languages": {"Kotlin": {"language_servers": ["!kotlin-lsp", "..."]}}}),
                false,
            ),
            (
                json!({"languages": {"Kotlin": {"language_servers": ["custom-kotlin"]}}}),
                false,
            ),
            (
                json!({"languages": {"Kotlin": {"language_servers": ["kotlin-lsp", "custom-kotlin"]}}}),
                false,
            ),
            (
                json!({"lsp": {"kotlin-lsp": {"binary": {"path": "/custom/server"}}}}),
                false,
            ),
            (
                json!({"lsp": {"kotlin-lsp": {"binary": {"arguments": ["--system-path=/custom"]}}}}),
                false,
            ),
            (
                json!({"lsp": {"kotlin-lsp": {"binary": {"env": {"CUSTOM": "kept"}}}}}),
                false,
            ),
            (
                json!({"lsp": {"kotlin-lsp": {"binary": {"ignore_system_version": true}}}}),
                false,
            ),
            (
                json!({"lsp": {"kotlin-lsp": {"initialization_options": {"projects": [{"type": "json", "path": "file:///default-android-kotlin"}]}}}}),
                false,
            ),
        ] {
            set_local(&preferences, cx);
            panel.read_with(cx, |panel, cx| {
                assert_eq!(
                    panel.default_official_kotlin_eligible(cx),
                    expected,
                    "Project preferences must be preserved: {preferences}"
                )
            });
        }
        let portable = json!({"languages": {"Kotlin": {"language_servers": ["kotlin-lsp"]}}, "lsp": {"kotlin-lsp": {"binary": {"arguments": ["--system-path=/other-profile/cache"], "env": {"KODA_MANAGED_KOTLIN_RUNTIME": "1"}}}}});
        set_local(&portable, cx);
        panel.read_with(cx, |panel, cx| {
            assert_eq!(
                panel.official_kotlin_state(cx),
                Some(OfficialKotlinState::Active),
                "A portable managed marker recognizes settings from another Koda profile"
            )
        });
        let mut mixed_managed = portable.clone();
        mixed_managed["languages"]["Kotlin"]["language_servers"] =
            json!(["kotlin-lsp", "custom-kotlin"]);
        set_local(&mixed_managed, cx);
        panel.read_with(cx, |panel, cx| {
            assert_eq!(
                panel.official_kotlin_state(cx),
                None,
                "Adding a custom server to a managed project must stop automatic settings refresh"
            );
            assert!(!panel.default_official_kotlin_eligible(cx));
            assert!(!panel.should_configure_official_kotlin(cx));
        });
        assert!(
            cx.update(|_, cx| paused_official_kotlin_settings(
                mixed_managed.to_string(),
                &root,
                cx
            ))
            .is_err(),
            "A missing variant must also preserve an explicitly mixed server list"
        );
        let paused = cx
            .update(|_, cx| paused_official_kotlin_settings(portable.to_string(), &root, cx))
            .unwrap();
        set_local(&serde_json::from_str(&paused).unwrap(), cx);
        panel.read_with(cx, |panel, cx| {
            assert_eq!(
                panel.official_kotlin_state(cx),
                Some(OfficialKotlinState::Paused)
            )
        });
        let mut custom_portable = portable;
        custom_portable["lsp"]["kotlin-lsp"]["binary"]["path"] = json!("/custom/server");
        set_local(&custom_portable, cx);
        panel.read_with(cx, |panel, cx| {
            assert_eq!(
                panel.official_kotlin_state(cx),
                None,
                "An explicit binary wins over a retained portable marker"
            )
        });
        assert!(
            cx.update(|_, cx| paused_official_kotlin_settings(
                custom_portable.to_string(),
                &root,
                cx
            ))
            .is_err()
        );
        set_local(&json!({}), cx);
        for inherited in [
            json!({"languages": {"Kotlin": {"enable_language_server": false}}}),
            json!({"languages": {"Kotlin": {"language_servers": ["custom-kotlin"]}}}),
            json!({"lsp": {"kotlin-lsp": {"binary": {"path": "/global/server"}}}}),
            json!({"lsp": {"kotlin-lsp": {"initialization_options": {"defaultSdk": "/custom/jdk"}}}}),
        ] {
            cx.update(|_, cx| {
                cx.update_global::<settings::SettingsStore, _>(|store, cx| {
                    store.set_user_settings(&inherited.to_string(), cx).unwrap();
                })
            });
            panel.read_with(cx, |panel, cx| {
                assert!(
                    !panel.default_official_kotlin_eligible(cx),
                    "Inherited choices must be preserved: {inherited}"
                )
            });
        }
        cx.update(|_, cx| {
            cx.update_global::<settings::SettingsStore, _>(|store, cx| {
                store
                    .set_user_settings(
                        &json!({"lsp": {"kotlin-lsp": {"binary": {"path": "/global/server"}}}})
                            .to_string(),
                        cx,
                    )
                    .unwrap();
            })
        });
        panel.read_with(cx, |panel, cx| {
            assert!(
                !panel.default_official_kotlin_eligible(cx),
                "Automatic setup must preserve a global custom binary"
            );
        });
        let explicitly_configured = cx
            .update(|_, cx| {
                official_kotlin_settings_with_policy(
                    "{}".into(),
                    &root,
                    &target,
                    Path::new("/managed/jdk"),
                    Path::new("/managed/kotlin-server"),
                    true,
                    cx,
                )
            })
            .unwrap();
        set_local(&serde_json::from_str(&explicitly_configured).unwrap(), cx);
        panel.read_with(cx, |panel, cx| {
            assert_eq!(
                panel
                    .kotlin_preferences(&root, cx)
                    .unwrap()
                    .server
                    .binary
                    .unwrap()
                    .path
                    .as_deref(),
                Some(android_tools::kotlin::MANAGED_RUNTIME_PATH),
                "Explicit project Configure must override the inherited binary with a portable selector"
            );
            assert_eq!(
                panel.official_kotlin_state(cx),
                Some(OfficialKotlinState::Active),
                "The effective managed selector must remain recognizable after settings merge"
            );
        });
        set_local(&json!({}), cx);
        cx.update(|_, cx| {
            cx.update_global::<settings::SettingsStore, _>(|store, cx| {
                store.set_user_settings("{}", cx).unwrap();
            })
        });
        panel.update(cx, |panel, cx| {
            assert!(panel.default_official_kotlin_eligible(cx));
            panel.selected_target.as_mut().unwrap().variant = "missing".into();
            assert!(
                !panel.default_official_kotlin_eligible(cx),
                "A stale variant must not configure a new backend"
            );
            panel.selected_target = Some(target.clone());
            panel.project.update(cx, |project, cx| {
                project.invalidate_android_model(Some(root.clone()), cx);
            });
            assert!(
                !panel.default_official_kotlin_eligible(cx),
                "Invalidated Android discovery must block automatic configuration"
            );
            publish_test_android_model(panel, &target, cx);
            assert!(panel.default_official_kotlin_eligible(cx));
        });
        cx.update(|_, cx| {
            TrustedWorktrees::try_get_global(cx)
                .unwrap()
                .update(cx, |trusted, cx| {
                    trusted.restrict(
                        worktrees.downgrade(),
                        [PathTrust::Worktree(worktree_id)].into_iter().collect(),
                        cx,
                    );
                })
        });
        panel.read_with(cx, |panel, cx| {
            assert!(
                !panel.default_official_kotlin_eligible(cx),
                "Restricted projects must not configure Kotlin"
            )
        });
    }

    #[gpui::test]
    async fn automatic_kotlin_publication_rechecks_delayed_global_preferences(
        cx: &mut TestAppContext,
    ) {
        let _state = cx.update(AppState::test);
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let previous = json!({"languages": {"Kotlin": {"language_servers": ["kotlin-lsp"]}}, "lsp": {"kotlin-lsp": {"binary": {"env": {"KODA_MANAGED_KOTLIN_RUNTIME": "1"}}}}}).to_string();
        std::fs::create_dir(root.join(".koda")).unwrap();
        std::fs::write(root.join(".koda/settings.json"), &previous).unwrap();
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            &root,
            json!({"app": {}, ".koda": {"settings.json": previous}}),
        )
        .await;
        let project = Project::test(fs, [root.as_path()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        let target = AndroidTarget {
            module: ":app".into(),
            variant: "debug".into(),
            output_listing: root.join("output.json"),
        };
        panel.update(cx, |panel, cx| {
            panel.root = Some(root.clone());
            panel.targets = vec![target.clone()];
            panel.selected_target = Some(target.clone());
            publish_test_android_model(panel, &target, cx);
        });
        let (finish_generation, generation_done) = oneshot::channel();
        let publication = panel.update_in(cx, |panel, window, cx| {
            let expected = panel.kotlin_preferences(&root, cx).unwrap();
            panel
                .validate_automatic_kotlin_preferences(&root, &expected, cx)
                .unwrap();
            let root = root.clone();
            let previous = previous.clone();
            let target = target.clone();
            cx.spawn_in(window, async move |panel, cx| {
                generation_done
                    .await
                    .context("Resource generation was cancelled")?;
                panel.update_in(cx, |panel, _, cx| {
                    panel.validate_automatic_kotlin_preferences(&root, &expected, cx)?;
                    panel.publish_official_kotlin_settings(
                        &root,
                        &target,
                        &previous,
                        "{\"overwritten\":true}",
                        cx,
                    )
                })?
            })
        });
        cx.run_until_parked();
        cx.update(|_, cx| {
            cx.update_global::<settings::SettingsStore, _>(|store, cx| {
                store
                    .set_user_settings(
                        r#"{"languages":{"Kotlin":{"enable_language_server":false}}}"#,
                        cx,
                    )
                    .unwrap();
            })
        });
        finish_generation.send(()).unwrap();
        let error = publication.await.unwrap_err();
        assert!(error.to_string().contains("preferences changed"));
        assert_eq!(
            std::fs::read_to_string(root.join(".koda/settings.json")).unwrap(),
            previous,
            "Automatic setup must leave project settings byte-for-byte intact when user preferences change while Gradle is running"
        );
    }

    #[gpui::test]
    async fn automatic_kotlin_failure_keeps_existing_java_refresh_and_error(
        cx: &mut TestAppContext,
    ) {
        let _state = cx.update(AppState::test);
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        android_tools::java::prepare(&root).unwrap();
        std::fs::write(root.join(".koda/android-java/model.json"), "[]").unwrap();
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(&root, json!({"app": {}})).await;
        let project = Project::test(fs, [root.as_path()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        let target = AndroidTarget {
            module: ":app".into(),
            variant: "debug".into(),
            output_listing: root.join("output.json"),
        };
        panel.update_in(cx, |panel, window, cx| {
            panel.root = Some(root.clone());
            panel.targets = vec![target.clone()];
            panel.selected_target = Some(target.clone());
            publish_test_android_model(panel, &target, cx);
            let token = panel.project.read(cx).android_model().token();
            panel.error = Some("Managed Kotlin runtime missing".into());
            panel.kotlin_setup_error = panel.error.clone();
            panel.refresh_java_after_kotlin_failure(&root, target.clone(), &token, window, cx);
            assert!(
                panel.java_task.is_some(),
                "A missing Kotlin runtime must still queue an already configured Java model refresh"
            );
            assert_eq!(
                panel.kotlin_setup_error.as_deref(),
                Some("Managed Kotlin runtime missing")
            );
            assert_eq!(
                panel.error.as_deref(),
                Some("Managed Kotlin runtime missing")
            );
            // Cancel before the queued task runs; this test does not invoke Gradle.
            panel.cancel_build(BuildTab::Sync, cx);
            panel.project.update(cx, |project, cx| {
                project.invalidate_android_model(Some(root.clone()), cx);
            });
            panel.refresh_java_after_kotlin_failure(&root, target, &token, window, cx);
            assert!(
                panel.java_task.is_none(),
                "Outdated Kotlin failure must not refresh Java for a changed model"
            );
        });
    }

    #[gpui::test]
    async fn successful_kotlin_generation_change_recovers_plain_files_in_all_windows(
        cx: &mut TestAppContext,
    ) {
        use android_tools::managed::Tool;
        use futures::{FutureExt as _, StreamExt as _};
        use language::{
            FakeLspAdapter, Language, LanguageConfig, LanguageMatcher, ManifestName,
            ManifestProvider, ManifestQuery,
        };
        struct KotlinTestManifest;
        impl ManifestProvider for KotlinTestManifest {
            fn name(&self) -> ManifestName {
                gpui::SharedString::new_static("build.gradle.kts").into()
            }

            fn search(&self, query: ManifestQuery) -> Option<Arc<RelPath>> {
                query
                    .path
                    .ancestors()
                    .take(query.depth)
                    .find(|path| {
                        query.delegate.exists(
                            &path.join(RelPath::from_unix_str("build.gradle.kts").unwrap()),
                            Some(false),
                        )
                    })
                    .map(Arc::from)
            }
        }
        let _state = cx.update(AppState::test);
        cx.update(|cx| {
            project::ManifestProvidersStore::global(cx).register(Arc::new(KotlinTestManifest))
        });
        let mut projects = Vec::new();
        let mut receivers = Vec::new();
        let mut old_servers = Vec::new();
        let mut custom_servers = Vec::new();
        let mut buffers = Vec::new();
        for index in 0..2 {
            let root = PathBuf::from(format!("/plain-kotlin-{index}"));
            let custom_root = PathBuf::from(format!("/custom-kotlin-{index}"));
            let fs = FakeFs::new(cx.executor());
            fs.insert_tree(&root, json!({"main.kt": "fun main() {}", ".koda": {"settings.json": json!({"languages": {"Kotlin": {"language_servers": ["kotlin-lsp"]}}}).to_string()}, "nested-custom": {"custom.kt": "fun nested() {}", "build.gradle.kts": "", ".koda": {"settings.json": json!({"languages": {"Kotlin": {"language_servers": ["custom-kotlin"]}}}).to_string()}}, "nested-disabled": {"disabled.kt": "fun disabled() {}", "build.gradle.kts": "", ".koda": {"settings.json": json!({"languages": {"Kotlin": {"enable_language_server": false}}}).to_string()}}})).await;
            fs.insert_tree(&custom_root, json!({"custom.kt": "fun custom() {}", ".koda": {"settings.json": json!({"languages": {"Kotlin": {"language_servers": ["kotlin-lsp", "custom-kotlin"]}}}).to_string()}})).await;
            let project = Project::test(fs, [root.as_path(), custom_root.as_path()], cx).await;
            project
                .update(cx, |project, cx| {
                    project
                        .lsp_store()
                        .update(cx, |store, cx| store.wait_for_local_settings(cx))
                })
                .await
                .unwrap();
            // Explicitly seed the real settings scopes after initial observation
            // settles. Recovery must use each buffer's effective preferences,
            // independently of fixture discovery and scan ordering.
            for (settings_root, directory, preferences) in [
                (
                    &root,
                    "",
                    json!({"languages": {"Kotlin": {"language_servers": ["kotlin-lsp"]}}}),
                ),
                (
                    &root,
                    "nested-custom",
                    json!({"languages": {"Kotlin": {"language_servers": ["custom-kotlin"]}}}),
                ),
                (
                    &root,
                    "nested-disabled",
                    json!({"languages": {"Kotlin": {"enable_language_server": false}}}),
                ),
                (
                    &custom_root,
                    "",
                    json!({"languages": {"Kotlin": {"language_servers": ["kotlin-lsp", "custom-kotlin"]}}}),
                ),
            ] {
                let worktree_id = project.read_with(cx, |project, cx| {
                    project
                        .visible_worktrees(cx)
                        .find(|worktree| worktree.read(cx).abs_path().as_ref() == settings_root)
                        .unwrap()
                        .read(cx)
                        .id()
                });
                cx.update(|cx| {
                    cx.update_global::<settings::SettingsStore, _>(|store, cx| {
                        store
                            .set_local_settings(
                                worktree_id,
                                settings::LocalSettingsPath::InWorktree(
                                    RelPath::from_unix_str(directory).unwrap().into_arc(),
                                ),
                                settings::LocalSettingsKind::Settings,
                                Some(&preferences.to_string()),
                                cx,
                            )
                            .unwrap();
                    })
                });
            }
            cx.run_until_parked();
            let languages = project.read_with(cx, |project, _| project.languages().clone());
            languages.add(Arc::new(
                Language::new(
                    LanguageConfig {
                        name: "Kotlin".into(),
                        matcher: LanguageMatcher {
                            path_suffixes: vec!["kt".into()],
                            ..Default::default()
                        }
                        .into(),
                        ..Default::default()
                    },
                    None,
                )
                .with_manifest(Some(KotlinTestManifest.name())),
            ));
            let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let mut servers = languages.register_fake_lsp(
                "Kotlin",
                FakeLspAdapter {
                    name: "kotlin-lsp",
                    initializer: Some(Box::new(move |server| {
                        if index == 0
                            && attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
                        {
                            server.set_request_handler::<lsp::request::Initialize, _, _>(
                                |_, _| async { anyhow::bail!("Missing managed runtime") },
                            );
                        }
                    })),
                    ..Default::default()
                },
            );
            let mut custom = languages.register_fake_lsp(
                "Kotlin",
                FakeLspAdapter {
                    name: "custom-kotlin",
                    ..Default::default()
                },
            );
            let buffer = project
                .update(cx, |project, cx| {
                    project.open_local_buffer_with_lsp(root.join("main.kt"), cx)
                })
                .await
                .unwrap();
            cx.run_until_parked();
            let old = servers
                .next()
                .now_or_never()
                .flatten()
                .expect("Initial default Kotlin server must be created");
            if index == 0 {
                project.read_with(cx, |project, cx| {
                    assert!(
                        project
                            .lsp_store()
                            .read(cx)
                            .language_server_for_id(old.server.server_id())
                            .is_none(),
                        "The initial plain-file server must actually fail"
                    )
                });
            }
            let nested_buffer = project
                .update(cx, |project, cx| {
                    project.open_local_buffer_with_lsp(root.join("nested-custom/custom.kt"), cx)
                })
                .await
                .unwrap();
            cx.run_until_parked();
            nested_buffer.0.read_with(cx, |buffer, cx| {
                let preferences =
                    language::language_settings::LanguageSettings::for_buffer(buffer, cx);
                assert!(preferences.enable_language_server);
                assert_eq!(
                    preferences.customized_language_servers(&[
                        lsp::LanguageServerName("kotlin-lsp".into()),
                        lsp::LanguageServerName("custom-kotlin".into()),
                    ]),
                    vec![lsp::LanguageServerName("custom-kotlin".into())],
                    "Nested buffer must actually resolve the custom settings scope before recovery"
                );
            });
            let nested_server = custom
                .next()
                .now_or_never()
                .flatten()
                .expect("Nested custom Kotlin settings must start the custom adapter");
            let disabled_buffer = project
                .update(cx, |project, cx| {
                    project.open_local_buffer_with_lsp(root.join("nested-disabled/disabled.kt"), cx)
                })
                .await
                .unwrap();
            cx.run_until_parked();
            assert!(
                servers.next().now_or_never().is_none(),
                "Nested disabled Kotlin must not start a server"
            );
            let custom_buffer = project
                .update(cx, |project, cx| {
                    project.open_local_buffer_with_lsp(custom_root.join("custom.kt"), cx)
                })
                .await
                .unwrap();
            cx.run_until_parked();
            let custom_kotlin = servers
                .next()
                .now_or_never()
                .flatten()
                .expect("Separate mixed Kotlin root must start its Kotlin adapter");
            let other = custom
                .next()
                .now_or_never()
                .flatten()
                .expect("Separate mixed Kotlin root must start its custom adapter");
            buffers.push((buffer, custom_buffer, nested_buffer, disabled_buffer));
            old_servers.push(old);
            custom_servers.push((custom_kotlin, other, nested_server));
            receivers.push(servers);
            projects.push(project);
        }
        let first_project = projects[0].clone();
        let second_project = projects[1].clone();
        let first_workspace = cx
            .add_window_view(|window, cx| Workspace::test_new(first_project.clone(), window, cx))
            .0;
        let (second_workspace, cx) = cx
            .add_window_view(|window, cx| Workspace::test_new(second_project.clone(), window, cx));
        let first = cx.new(|cx| AndroidPanel::new(first_workspace.downgrade(), first_project, cx));
        let second =
            cx.new(|cx| AndroidPanel::new(second_workspace.downgrade(), second_project, cx));
        first_workspace.update(cx, |workspace, cx| {
            register_controller(workspace, &first, cx)
        });
        second_workspace.update(cx, |workspace, cx| {
            register_controller(workspace, &second, cx)
        });
        for panel in [&first, &second] {
            panel.update(cx, |panel, _| {
                panel.refreshing_devices = true;
            });
        }
        for (tool, operation) in [(Tool::Debugger, "install"), (Tool::Kotlin, "validate")] {
            first.update_in(cx, |panel, window, cx| {
                panel.managed_tool_succeeded(tool, operation, window, cx)
            });
            cx.run_until_parked();
            for receiver in &mut receivers {
                assert!(
                    receiver.next().now_or_never().is_none(),
                    "Validation/debugger changes must not restart Kotlin"
                );
            }
        }
        first.update_in(cx, |panel, window, cx| {
            panel.managed_tool_succeeded(Tool::Kotlin, "install", window, cx)
        });
        cx.run_until_parked();
        let mut replacements = Vec::new();
        for (index, receiver) in receivers.iter_mut().enumerate() {
            let mut replacement = receiver.next().now_or_never().flatten().expect(
                "Both a failed plain-file server and the other window's old server must start",
            );
            cx.run_until_parked();
            let opened = replacement
                .receive_notification::<lsp::notification::DidOpenTextDocument>()
                .now_or_never()
                .expect("Recovered server must receive the open plain Kotlin file");
            assert!(opened.text_document.uri.as_str().ends_with("/main.kt"));
            projects[index].read_with(cx, |project, cx| {
                let store = project.lsp_store();
                assert!(
                    store
                        .read(cx)
                        .language_server_for_id(old_servers[index].server.server_id())
                        .is_none()
                );
                assert!(
                    store
                        .read(cx)
                        .language_server_for_id(replacement.server.server_id())
                        .is_some()
                );
                for custom in [
                    &custom_servers[index].0,
                    &custom_servers[index].1,
                    &custom_servers[index].2,
                ] {
                    assert!(
                        store
                            .read(cx)
                            .language_server_for_id(custom.server.server_id())
                            .is_some(),
                        "Custom/mixed-server roots must keep their original processes"
                    );
                }
            });
            replacements.push(replacement);
        }
        // A user stop is distinct from a failed server and remains suppressed.
        projects[0]
            .update(cx, |project, cx| {
                project.lsp_store().update(cx, |store, cx| {
                    store.stop_language_servers_for_buffers(
                        vec![buffers[0].0.0.clone()],
                        [lsp::LanguageServerSelector::Id(
                            replacements[0].server.server_id(),
                        )]
                        .into_iter()
                        .collect(),
                        cx,
                    )
                })
            })
            .await
            .unwrap();
        cx.run_until_parked();
        first.update_in(cx, |panel, window, cx| {
            panel.managed_tool_succeeded(Tool::Kotlin, "rollback", window, cx)
        });
        cx.run_until_parked();
        assert!(
            receivers[0].next().now_or_never().is_none(),
            "A manually stopped Kotlin server must stay stopped"
        );
        assert!(
            receivers[1].next().now_or_never().flatten().is_some(),
            "Rollback still refreshes the other window's active default server"
        );
    }

    #[gpui::test]
    fn official_kotlin_settings_preserve_preferences_and_refresh_variants(cx: &mut TestAppContext) {
        cx.update(|cx| {
            settings::init(cx);
            let previous = r#"{
                // keep Kotlin preferences
                "tab_size": 2,
                "languages": {"Kotlin": {"format_on_save": "off", "language_servers": ["custom-kotlin"]}},
                "lsp": {
                    "custom-kotlin": {"settings": {"custom": true}},
                    "kotlin-lsp": {
                        "initialization_options": {"custom": true, "projects": [{"type": "gradle", "path": "file:///other", "java-home": "/other-jdk"}]},
                        "binary": {"arguments": ["--stdio", "--system-path=/old", "--system-path", "/also old", "--data-sharing=none"], "env": {"CUSTOM": "kept"}}
                    }
                }
            }"#;
            let root = Path::new("/android project");
            let target = AndroidTarget { module: ":mobile".into(), variant: "demoDebug".into(), output_listing: PathBuf::new() };
            let server = Path::new("/official/bin/intellij-server");
            let updated = official_kotlin_settings(previous.into(), root, &target, Path::new("/jdk 21"), server, cx).expect("Official settings should update");
            assert!(updated.contains("// keep Kotlin preferences"));
            let parsed: serde_json::Value = settings::parse_json_with_comments(&updated).expect("Valid settings");
            assert_eq!(parsed["tab_size"], 2);
            assert_eq!(parsed["languages"]["Kotlin"]["format_on_save"], "off");
            assert_eq!(parsed["languages"]["Kotlin"]["language_servers"], json!(["kotlin-lsp"]));
            assert_eq!(parsed["languages"]["Java"]["language_servers"], json!(["kotlin-lsp", "jdtls"]));
            for java_servers in [json!([]), json!(["!kotlin-lsp", "..."]), json!(["custom-java"])] {
                let mut custom = parsed.clone();
                custom["languages"]["Java"]["language_servers"] = java_servers.clone();
                let updated = official_kotlin_settings(custom.to_string(), root, &target, Path::new("/jdk 21"), server, cx).expect("Custom Java settings preserved");
                let custom: serde_json::Value = settings::parse_json_with_comments(&updated).unwrap();
                assert_eq!(custom["languages"]["Java"]["language_servers"], java_servers);
            }
            assert_eq!(parsed["lsp"]["custom-kotlin"]["settings"]["custom"], true);
            let official = &parsed["lsp"]["kotlin-lsp"];
            assert_eq!(official["binary"]["path"], "/official/bin/intellij-server");
            let system_path = format!("--system-path={}", android_tools::kotlin::official_system_path(root).display());
            assert_eq!(official["binary"]["arguments"], json!(["--stdio", "--data-sharing=none", system_path]));
            let legacy_system_path = "--system-path=/android project/.koda/android-kotlin-official/system";
            assert!(has_official_kotlin_system_path(root, [system_path.as_str()]));
            assert!(has_official_kotlin_system_path(root, [legacy_system_path]), "Previously managed project caches are recognized for migration");
            assert!(!has_official_kotlin_system_path(root, ["--system-path=/custom"]));
            assert!(!has_official_kotlin_system_path(root, [legacy_system_path, system_path.as_str()]), "Multiple cache overrides must not be claimed as managed");
            let portable_previous = json!({"lsp": {"kotlin-lsp": {"binary": {"path": "/old-profile/server", "env": {"CUSTOM": "kept", "JAVA_HOME": "/old-profile/java", "ANDROID_HOME": "/old-profile/sdk", "ANDROID_SDK_ROOT": "/old-profile/sdk"}}}}});
            let portable = official_kotlin_settings_with_policy(portable_previous.to_string(), root, &target, Path::new("/jdk 21"), server, true, cx).expect("Production managed settings should be portable");
            let portable: serde_json::Value = settings::parse_json_with_comments(&portable).unwrap();
            assert_eq!(portable["lsp"]["kotlin-lsp"]["binary"]["path"], android_tools::kotlin::MANAGED_RUNTIME_PATH);
            let environment = &portable["lsp"]["kotlin-lsp"]["binary"]["env"];
            assert_eq!(environment[android_tools::kotlin::MANAGED_RUNTIME_ENV], "1");
            assert_eq!(environment["CUSTOM"], "kept");
            for variable in ["JAVA_HOME", "ANDROID_HOME", "ANDROID_SDK_ROOT"] { assert!(environment[variable].is_null(), "Managed runtime paths must be resolved in the launching profile"); }
            let mut expected_environment = json!({"CUSTOM": "kept", "LSP_ANDROID_MODULE": ":mobile", "LSP_ANDROID_VARIANT": "demoDebug"});
            for (name, value) in android_tools::managed::language_environment().expect("Managed language environment") {
                expected_environment[&name] = value.into();
            }
            assert_eq!(official["binary"]["env"], expected_environment);
            assert_eq!(official["initialization_options"], json!({"custom": true, "defaultSdk": "/jdk 21", "projects": [
                {"type": "gradle", "path": "file:///other", "java-home": "/other-jdk"},
                {"type": "gradle", "path": "file:///android%20project", "java-home": "/jdk 21"}
            ]}));
            let mut directory_uri = parsed.clone();
            directory_uri["lsp"]["kotlin-lsp"]["initialization_options"]["projects"][1]["path"] = json!("file:///android%20project/");
            let directory_uri = official_kotlin_settings(directory_uri.to_string(), root, &target, Path::new("/jdk 21"), server, cx).expect("Directory URI should update without a duplicate import");
            let directory_uri: serde_json::Value = settings::parse_json_with_comments(&directory_uri).expect("Valid directory URI settings");
            assert_eq!(directory_uri["lsp"]["kotlin-lsp"], *official);
            let paused = paused_official_kotlin_settings(updated, root, cx).expect("Pause the obsolete variant");
            let parsed_paused: serde_json::Value = settings::parse_json_with_comments(&paused).unwrap();
            assert_eq!(parsed_paused["languages"]["Kotlin"]["language_servers"], json!([]));
            assert_eq!(parsed_paused["languages"]["Java"]["language_servers"], json!(["jdtls"]));
            assert_eq!(parsed_paused["lsp"]["kotlin-lsp"]["binary"]["env"][UNAVAILABLE_ANDROID_VARIANT], "true");
            assert_eq!(parsed_paused["lsp"]["kotlin-lsp"]["initialization_options"], official["initialization_options"]);
            let resumed = official_kotlin_settings(paused, root, &target, Path::new("/jdk 21"), server, cx).unwrap();
            let resumed: serde_json::Value = settings::parse_json_with_comments(&resumed).unwrap();
            assert_eq!(resumed["lsp"]["kotlin-lsp"], *official);
            assert_eq!(resumed["languages"]["Java"]["language_servers"], json!(["kotlin-lsp", "jdtls"]));
            let custom = json!({"languages": {"Kotlin": {"language_servers": ["custom-kotlin"]}}}).to_string();
            assert!(paused_official_kotlin_settings(custom, root, cx).is_err());
            let mut disabled = parsed.clone();
            disabled["languages"]["Kotlin"]["enable_language_server"] = json!(false);
            assert!(paused_official_kotlin_settings(disabled.to_string(), root, cx).is_err());
            for arguments in [
                json!(["--stdio", "--system-path=/custom"]),
                json!(["--system-path=/android project/.koda/android-kotlin-official/system", "--system-path", "/custom"]),
            ] {
                let mut unmanaged = parsed.clone();
                unmanaged["lsp"]["kotlin-lsp"]["binary"]["arguments"] = arguments;
                assert!(paused_official_kotlin_settings(unmanaged.to_string(), root, cx).is_err());
            }
            let restarted = official_kotlin_settings(resumed.to_string(), root, &AndroidTarget { variant: "fullRelease".into(), ..target.clone() }, Path::new("/jdk 21"), server, cx).expect("Repeated settings should update");
            let restarted: serde_json::Value = settings::parse_json_with_comments(&restarted).expect("Valid restarted settings");
            assert_eq!(restarted["lsp"]["kotlin-lsp"]["initialization_options"], official["initialization_options"]);
            assert_eq!(restarted["lsp"]["kotlin-lsp"]["binary"]["arguments"], official["binary"]["arguments"]);
            assert_eq!(restarted["lsp"]["kotlin-lsp"]["binary"]["env"]["LSP_ANDROID_VARIANT"], "fullRelease");
            for options in [json!([]), json!({"projects": {}}), json!({"projects": [{"type": "json", "path": "file:///android%20project"}]})] {
                let previous = json!({"lsp": {"kotlin-lsp": {"initialization_options": options}}}).to_string();
                assert!(official_kotlin_settings(previous, root, &target, Path::new("/jdk 21"), server, cx).is_err());
            }
            cx.update_global(|store: &mut settings::SettingsStore, cx| {
                store.set_user_settings(r#"{"lsp":{"kotlin-lsp":{
                    "binary":{"arguments":["--stdio","--data-sharing=none"]},
                    "initialization_options":{"projects":[{"type":"gradle","path":"file:///other","java-home":"/other-jdk"}]}
                }}}"#, cx).expect("Inherited settings should load");
            });
            for previous in ["", r#"{"lsp":{"kotlin-lsp":{"initialization_options":null}}}"#] {
                let updated = official_kotlin_settings(previous.into(), root, &target, Path::new("/jdk 21"), server, cx).expect("Unset local settings should inherit user preferences");
                let updated: serde_json::Value = settings::parse_json_with_comments(&updated).expect("Valid inherited settings");
                assert_eq!(updated["lsp"]["kotlin-lsp"]["binary"]["arguments"], official["binary"]["arguments"]);
                assert_eq!(updated["lsp"]["kotlin-lsp"]["initialization_options"]["projects"], official["initialization_options"]["projects"]);
            }
            cx.update_global(|store: &mut settings::SettingsStore, cx| {
                store.set_user_settings(r#"{"lsp":{"kotlin-lsp":{"initialization_options":{"projects":[{"type":"json","path":"file:///android%20project/"}]}}}}"#, cx).expect("Inherited importer should load");
            });
            assert!(official_kotlin_settings("{}".into(), root, &target, Path::new("/jdk 21"), server, cx).is_err());
        });
    }

    #[gpui::test]
    async fn device_menu_uses_refreshed_devices_and_preserves_selection(cx: &mut TestAppContext) {
        let _app_state = cx.update(AppState::test);
        let project = Project::test(FakeFs::new(cx.executor()), [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        panel.update(cx, |panel, _| {
            // Supply completion below instead of running host SDK processes.
            panel.refreshing_devices = true;
            panel.devices = parse_devices(
                "List of devices attached\nold device model:Old\nselected device model:Selected\n",
            )
            .expect("Valid devices");
            panel.selected_serial = Some("selected".into());
        });
        let menu = cx.update(|window, cx| AndroidPanel::device_menu(panel.clone(), window, cx));
        panel.update(cx, |panel, cx| {
            panel.devices.reverse();
            panel.refreshing_devices = false;
            cx.notify();
        });
        cx.run_until_parked();
        menu.update_in(cx, |menu, window, cx| {
            menu.confirm(&Default::default(), window, cx);
        });
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.selected_serial.as_deref(), Some("selected"));
        });
        panel.update(cx, |panel, cx| {
            panel.devices =
                parse_devices("List of devices attached\nreplacement device model:Replacement\n")
                    .expect("Valid replacement device");
            cx.notify();
        });
        cx.run_until_parked();
        menu.update_in(cx, |menu, window, cx| {
            menu.confirm(&Default::default(), window, cx);
        });
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.selected_serial.as_deref(), Some("replacement"));
            assert!(panel.selected_device().is_ok());
        });
    }

    #[gpui::test]
    async fn default_build_variant_uses_the_gradle_model(cx: &mut TestAppContext) {
        let _app_state = cx.update(AppState::test);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/gradle-default-model", json!({"settings.gradle.kts": ""}))
            .await;
        let project = Project::test(fs, [Path::new("/gradle-default-model")], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        panel.update(cx, |panel, cx| {
            let root = PathBuf::from("/gradle-default-model");
            panel.root = Some(root.clone());
            let variants = ["demoDebug", "fullRelease"].map(|name| json!({
                "name": name, "outputListing": root.join(name).join("output.json"),
                "components": [{"name": name, "scope": "main", "sources": [], "dependencies": []}]
            }));
            let model: android_tools::project_model::ProjectModel = serde_json::from_value(json!({
                "version": 1, "root": root, "diagnostics": [], "modules": [{
                    "path": ":mobile", "directory": root.join("mobile"),
                    "kind": "application", "defaultVariant": "fullRelease",
                    "variants": variants
                }]
            }))
            .expect("Gradle model");
            let targets = model.targets();
            panel.project.update(cx, |project, cx| {
                let token = project.invalidate_android_model(Some(root), cx);
                project
                    .publish_android_model(&token, model, cx)
                    .expect("Publish model");
            });
            panel.apply_targets(targets, cx);
            panel
                .publish_selection(cx)
                .expect("Publish default selection");
            assert_eq!(
                panel
                    .selected_target
                    .as_ref()
                    .expect("Gradle default")
                    .variant,
                "fullRelease"
            );
            assert_eq!(
                panel
                    .project
                    .read(cx)
                    .android_model()
                    .selected
                    .as_ref()
                    .expect("Shared selection")
                    .selected
                    .variant,
                "fullRelease"
            );
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn default_build_variant_is_selected_and_remembered(cx: &mut TestAppContext) {
        let _app_state = cx.update(AppState::test);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/android", json!({"settings.gradle.kts": ""}))
            .await;
        let project = Project::test(fs, [Path::new("/android")], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        for (index, (variants, expected)) in [
            (
                vec![
                    (":mobile", "fullDebug"),
                    (":mobile", "demoRelease"),
                    (":mobile", "demoDebug"),
                ],
                (":mobile", "demoDebug"),
            ),
            (
                vec![(":app", "release"), (":app", "debug")],
                (":app", "debug"),
            ),
            (
                vec![(":wear", "debug"), (":app", "debug")],
                (":app", "debug"),
            ),
            (
                vec![(":app", "release"), (":wear", "demoDebug")],
                (":wear", "demoDebug"),
            ),
            (
                vec![(":mobile", "fullRelease"), (":mobile", "demoRelease")],
                (":mobile", "demoRelease"),
            ),
            (
                vec![(":app", "debugger"), (":app", "aRelease")],
                (":app", "aRelease"),
            ),
            (vec![(":mobile", "staging")], (":mobile", "staging")),
        ]
        .into_iter()
        .enumerate()
        {
            panel.update(cx, |panel, cx| {
                panel.root = Some(PathBuf::from(format!("/default-variants-{index}")));
                panel.selected_target = None;
                let targets = variants
                    .into_iter()
                    .map(|(module, variant)| AndroidTarget {
                        module: module.into(),
                        variant: variant.into(),
                        output_listing: PathBuf::from("/android/fresh/output.json"),
                    })
                    .collect();
                panel.apply_targets(targets, cx);
                let selected = panel.selected_target.as_ref().expect("Default variant");
                assert_eq!(
                    (selected.module.as_str(), selected.variant.as_str()),
                    expected
                );
            });
        }
        cx.run_until_parked();
        panel.update(cx, |panel, cx| {
            // Reopening and adding an earlier flavor must retain the original default.
            panel.root = Some(PathBuf::from("/default-variants-0"));
            panel.selected_target = None;
            let targets = ["aaaDebug", "demoDebug", "demoRelease"]
                .map(|variant| AndroidTarget {
                    module: ":mobile".into(),
                    variant: variant.into(),
                    output_listing: PathBuf::from("/android/refreshed/output.json"),
                })
                .to_vec();
            panel.apply_targets(targets.clone(), cx);
            assert_eq!(
                panel
                    .selected_target
                    .as_ref()
                    .expect("Remembered default")
                    .variant,
                "demoDebug"
            );
            assert_eq!(
                panel
                    .selected_target
                    .as_ref()
                    .expect("Fresh artifacts")
                    .output_listing,
                Path::new("/android/refreshed/output.json")
            );
            panel.selected_target = targets
                .into_iter()
                .find(|target| target.variant == "demoRelease");
            panel.remember_target(cx);
        });
        cx.run_until_parked();
        panel.update(cx, |panel, cx| {
            panel.selected_target = None;
            panel.apply_targets(panel.targets.clone(), cx);
            assert_eq!(
                panel
                    .selected_target
                    .as_ref()
                    .expect("User selection")
                    .variant,
                "demoRelease"
            );
            panel.selected_target = None;
            panel.root = Some(PathBuf::from("/empty-variants"));
            panel.apply_targets(Vec::new(), cx);
            assert!(panel.selected_target.is_none());
        });
    }

    #[gpui::test]
    async fn android_panel_respects_trust_roots_and_device_state(cx: &mut TestAppContext) {
        let _app_state = cx.update(|cx| {
            let state = AppState::test(cx);
            trusted_worktrees::init(Default::default(), cx);
            init(cx);
            state
        });
        cx.update(|cx| {
            for (platform, shortcuts) in [
                (
                    "macos",
                    [
                        ("cmd-f9", "android::Build"),
                        ("ctrl-r", "android::Run"),
                        ("cmd-6", "android::Logcat"),
                        ("cmd-alt-y", "android::SyncProject"),
                    ],
                ),
                (
                    "linux",
                    [
                        ("ctrl-f9", "android::Build"),
                        ("shift-f10", "android::Run"),
                        ("alt-6", "android::Logcat"),
                        ("ctrl-alt-y", "android::SyncProject"),
                    ],
                ),
            ] {
                let mut bindings = settings::KeymapFile::load_asset_allow_partial_failure(
                    &format!("keymaps/default-{platform}.json"),
                    cx,
                )
                .expect("Default keymap should load");
                for binding in &mut bindings {
                    binding.set_meta(settings::KeybindSource::Default.meta());
                }
                let mut studio = settings::KeymapFile::load_asset_allow_partial_failure(
                    &format!("keymaps/{platform}/jetbrains.json"),
                    cx,
                )
                .expect("JetBrains keymap should load");
                for binding in &mut studio {
                    binding.set_meta(settings::KeybindSource::Base.meta());
                }
                bindings.extend(studio);
                let keymap = gpui::Keymap::new(bindings);
                let contexts = ["Workspace", "Pane", "Editor mode=full"]
                    .map(|context| gpui::KeyContext::parse(context).expect("Valid key context"));
                for (shortcut, action) in shortcuts {
                    let keystroke = gpui::Keystroke::parse(shortcut).expect("Valid shortcut");
                    let (matches, _) = keymap.bindings_for_input(&[keystroke], &contexts);
                    assert_eq!(
                        matches.first().map(|binding| binding.action().name()),
                        Some(action),
                        "{platform}: {shortcut}"
                    );
                }
            }
            let previous = r#"{// keep Java preferences
                "tab_size": 2,
                "lsp": {"jdtls": {
                    "initialization_options": {"bundles": ["debug.jar"]},
                    "settings": {"java": {"format": {"enabled": false}, "import": {"gradle": {"arguments": ["--offline"]}}}}
                }}
            }"#;
            let updated = java_settings(previous.into(), Path::new("/android project"), cx).expect("Java settings should update");
            assert!(updated.contains("// keep Java preferences"));
            let parsed: serde_json::Value = settings::parse_json_with_comments(&updated).expect("Java settings should parse");
            assert_eq!(parsed["tab_size"], 2);
            let server = &parsed["lsp"]["jdtls"];
            assert_eq!(server["initialization_options"]["bundles"], json!(["debug.jar"]));
            assert_eq!(server["settings"]["java"]["format"]["enabled"], false);
            assert_eq!(server["settings"], server["initialization_options"]["settings"]);
            assert_eq!(server["settings"]["java"]["import"]["gradle"]["arguments"], json!([
                "--offline", "--init-script", "/android project/.koda/android-java/import.gradle",
                "-Dzed.android.javaModel=/android project/.koda/android-java/model.json"
            ]));
            assert_eq!(java_settings(updated.clone(), Path::new("/android project"), cx).expect("Setup should be repeatable"), updated);
            assert!(java_settings(r#"{"lsp":{"jdtls":{"initialization_options":[]}}}"#.into(), Path::new("/android"), cx).is_err());
            assert!(java_settings(r#"{"lsp":{"jdtls":{"settings":{"java":{"import":{"gradle":{"arguments":"invalid"}}}}}}}"#.into(), Path::new("/android"), cx).is_err());

        });
        let filesystem = FakeFs::new(cx.executor());
        filesystem
            .insert_tree(
                "/android",
                json!({"settings.gradle.kts": "", "gradlew": ""}),
            )
            .await;
        let project =
            Project::test_with_worktree_trust(filesystem.clone(), [Path::new("/android")], cx)
                .await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        cx.run_until_parked();
        let panel = workspace
            .read_with(cx, |workspace, cx| controller(workspace, cx))
            .expect("Android controller should register with a new workspace");
        panel.read_with(cx, |panel, cx| {
            assert!(panel.trusted_root(cx).is_err());
            assert!(panel.auto_sync_candidate(cx).is_none());
            assert!(panel.auto_sync_root.is_none());
            assert!(panel.selected_device().is_err());
            assert!(!panel.running);
            assert!(!panel.syncing);
        });
        let store = project.read_with(cx, |project, _| project.worktree_store());
        // Device subprocesses use the host SDK, outside the deterministic fake filesystem.
        panel.update(cx, |panel, _| panel.refreshing_devices = true);
        cx.update(|_, cx| {
            TrustedWorktrees::try_get_global(cx)
                .expect("Trust store should exist")
                .update(cx, |trusted, cx| {
                    trusted.trust(
                        &store,
                        [PathTrust::AbsPath(PathBuf::from("/android"))]
                            .into_iter()
                            .collect(),
                        cx,
                    )
                });
        });
        cx.run_until_parked();
        panel.read_with(cx, |panel, cx| {
            assert_eq!(
                panel.auto_sync_candidate(cx),
                Some(PathBuf::from("/android"))
            );
            assert_eq!(panel.auto_sync_root, Some(PathBuf::from("/android")));
        });
        panel.update(cx, |panel, cx| {
            assert_eq!(panel.trusted_root(cx).ok(), Some(PathBuf::from("/android")));
            panel.root = Some(PathBuf::from("/closed-project"));
            assert!(panel.trusted_root(cx).is_err());
            panel.root = None;
            panel.devices = parse_devices("List of devices attached\nemulator-1 offline\nemulator-2 device model:Test_Phone\n")
                .expect("Valid device list");
            panel.selected_serial = Some("emulator-1".into());
            assert!(panel.selected_device().is_err());
            assert!(!panel.can_run_on_selected_device());
            panel.emulators = vec!["medium_phone".into()];
            panel.selected_avd = Some("medium_phone".into());
            assert!(panel.can_run_on_selected_device());
            panel.selected_avd = Some("removed_avd".into());
            assert!(!panel.can_run_on_selected_device());
            panel.selected_avd = None;
            panel.selected_serial = Some("emulator-2".into());
            assert_eq!(panel.selected_device().map(|device| device.serial.as_str()).ok(), Some("emulator-2"));
        });
        panel.update_in(cx, |panel, window, cx| {
            assert!(panel.selected_emulator().is_ok());
            panel.devices.push(Device {
                serial: "usb-phone".into(),
                state: "device".into(),
                model: "Phone".into(),
            });
            panel.selected_serial = Some("usb-phone".into());
            panel.stop_emulator(window, cx);
            assert!(!panel.running);
            assert!(
                panel
                    .error
                    .as_ref()
                    .is_some_and(|error| error.contains("physical devices"))
            );
            panel.start_emulator("--help".into(), window, cx);
            assert!(!panel.running);
            assert!(
                panel
                    .error
                    .as_ref()
                    .is_some_and(|error| error.contains("select an available emulator"))
            );
        });
        workspace.update_in(cx, |workspace, window, cx| {
            with_panel(workspace, window, cx, |panel, window, cx| {
                panel.gradle(GradleOperation::Build, window, cx)
            });
        });
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(!panel.running);
            assert!(
                panel
                    .error
                    .as_ref()
                    .is_some_and(|error| error.contains("select a build variant"))
            );
        });
        assert!(workspace.read_with(cx, |workspace, cx| {
            toolbar(&workspace.weak_handle(), cx).is_some()
        }));
        let (database, key) = panel.update(cx, |panel, cx| {
            panel.root = Some(PathBuf::from("/android"));
            (
                KeyValueStore::global(cx),
                panel.target_selection_key().expect("Project key"),
            )
        });
        database
            .write_kvp(
                key,
                serde_json::to_string(&(":mobile", "fullDebug")).expect("Target identity"),
            )
            .await
            .expect("Remember selected variant");
        panel.update(cx, |panel, cx| {
            let full = AndroidTarget {
                module: ":mobile".into(),
                variant: "fullDebug".into(),
                output_listing: PathBuf::from("/android/fresh/output.json"),
            };
            let demo = AndroidTarget {
                variant: "demoDebug".into(),
                ..full.clone()
            };
            panel.selected_target = None;
            panel.apply_targets(vec![demo.clone(), full.clone()], cx);
            assert_eq!(panel.selected_target, Some(full.clone()));
            let changed = AndroidTarget {
                output_listing: PathBuf::from("/android/new/output.json"),
                ..full
            };
            panel.apply_targets(vec![changed.clone()], cx);
            assert_eq!(panel.selected_target, Some(changed));
            panel.apply_targets(vec![demo], cx);
            assert!(panel.selected_target.is_none());
            assert!(panel.status.contains("unavailable"));
            panel.selected_target = None;
            panel.root = Some(PathBuf::from("/another-project"));
            let debug = AndroidTarget {
                module: ":app".into(),
                variant: "debug".into(),
                output_listing: PathBuf::from("/another-project/output.json"),
            };
            panel.apply_targets(vec![debug.clone()], cx);
            assert_eq!(panel.selected_target, Some(debug));
            panel.root = None;
        });
        assert!(workspace.read_with(cx, |workspace, cx| {
            workspace.left_dock().read(cx).visible_panel().is_none()
                && workspace.right_dock().read(cx).visible_panel().is_none()
        }));
    }
}
