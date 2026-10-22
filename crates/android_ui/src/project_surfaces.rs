use super::*;
use gpui::{Menu, MenuItem, OsMenu, OwnedMenu, OwnedMenuItem};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SurfaceState {
    pub eligible: bool,
    pub android: bool,
    pub build_window: bool,
}

impl SurfaceState {
    pub(crate) fn for_workspace(workspace: &Workspace, cx: &App) -> Self {
        controller(workspace, cx)
            .map(|panel| panel.read(cx).surface_state())
            .unwrap_or_default()
    }

    fn permits(self, action: &dyn Action) -> bool {
        let action = action.as_any();
        if action.is::<Setup>() {
            return true;
        }
        if action.is::<ToggleFocus>() {
            return self.eligible;
        }
        if action.is::<ToggleBuild>() {
            return self.build_window;
        }
        if action.is::<ConfigureKotlin>() || action.is::<ConfigureOfficialKotlin>() {
            return self.android;
        }
        if action.is::<SyncProject>()
            || action.is::<RefreshDevices>()
            || action.is::<StartEmulator>()
            || action.is::<StopEmulator>()
            || action.is::<Build>()
            || action.is::<Run>()
            || action.is::<Debug>()
            || action.is::<Test>()
            || action.is::<Lint>()
            || action.is::<Logcat>()
            || action.is::<android_logcat::Toggle>()
            || action.is::<android_logcat::NewViewer>()
            || action.is::<ComposePreview>()
            || action.is::<ToggleComposePreview>()
            || action.is::<ConfigureJava>()
        {
            return self.android;
        }
        true
    }
}

pub fn action_available(workspace: &Workspace, action: &dyn Action, cx: &App) -> bool {
    SurfaceState::for_workspace(workspace, cx).permits(action)
}

fn workspace_controller(
    workspace: &WeakEntity<Workspace>,
    cx: &App,
) -> Option<Entity<AndroidPanel>> {
    cx.try_global::<AndroidControllers>()?
        .0
        .get(&workspace.entity_id())?
        .upgrade()
}

pub(crate) fn android_available(workspace: &WeakEntity<Workspace>, cx: &App) -> bool {
    workspace_controller(workspace, cx).is_some_and(|panel| panel.read(cx).surface_state().android)
}

pub(crate) fn root_available(workspace: &WeakEntity<Workspace>, root: &Path, cx: &App) -> bool {
    workspace_controller(workspace, cx).is_some_and(|panel| {
        let panel = panel.read(cx);
        panel.surface_state().android && panel.context_root().as_deref() == Some(root)
    })
}

pub(crate) fn build_available(workspace: &WeakEntity<Workspace>, cx: &App) -> bool {
    workspace_controller(workspace, cx)
        .is_some_and(|panel| panel.read(cx).surface_state().build_window)
}

pub(crate) fn register_action<A: Action + Default>(
    workspace: &mut Workspace,
    callback: impl Fn(&mut Workspace, &A, &mut Window, &mut Context<Workspace>) + 'static,
) {
    let callback = std::rc::Rc::new(callback);
    workspace.register_action_renderer(move |element, workspace, _, cx| {
        if !action_available(workspace, &A::default(), cx) {
            return element;
        }
        let callback = callback.clone();
        element.on_action(cx.listener(move |workspace, action: &A, window, cx| {
            if action_available(workspace, action, cx) {
                callback(workspace, action, window, cx);
            } else {
                cx.propagate();
            }
        }))
    });
}

#[derive(Default)]
pub struct ApplicationMenuTemplates(Vec<OwnedMenu>);
impl Global for ApplicationMenuTemplates {}

pub fn install_application_menus(menus: Vec<Menu>, cx: &mut App) {
    cx.set_global(ApplicationMenuTemplates(
        menus.into_iter().map(Menu::owned).collect(),
    ));
    let state =
        cx.active_window()
            .and_then(|window| {
                window
                    .downcast::<workspace::MultiWorkspace>()
                    .and_then(|window| {
                        window.read(cx).ok().map(|multi| {
                            SurfaceState::for_workspace(multi.workspace().read(cx), cx)
                        })
                    })
            })
            .unwrap_or_default();
    set_native_menus(state, cx);
}

pub fn application_menus(workspace: Option<&WeakEntity<Workspace>>, cx: &App) -> Vec<OwnedMenu> {
    let state = workspace
        .and_then(|workspace| workspace_controller(workspace, cx))
        .map(|panel| panel.read(cx).surface_state())
        .unwrap_or_default();
    let menus = cx
        .try_global::<ApplicationMenuTemplates>()
        .map(|templates| templates.0.clone())
        .unwrap_or_else(|| cx.get_menus().unwrap_or_default());
    filter_menus(menus, state)
}

fn filter_menus(menus: Vec<OwnedMenu>, state: SurfaceState) -> Vec<OwnedMenu> {
    menus
        .into_iter()
        .map(|mut menu| {
            menu.items = filter_items(menu.items, state);
            menu
        })
        .collect()
}

fn filter_items(items: Vec<OwnedMenuItem>, state: SurfaceState) -> Vec<OwnedMenuItem> {
    let mut filtered = Vec::new();
    for item in items {
        match item {
            OwnedMenuItem::Action { ref action, .. } if !state.permits(action.as_ref()) => {}
            OwnedMenuItem::Separator => {
                if !filtered.is_empty()
                    && !matches!(filtered.last(), Some(OwnedMenuItem::Separator))
                {
                    filtered.push(item);
                }
            }
            OwnedMenuItem::Submenu(mut submenu) => {
                submenu.items = filter_items(submenu.items, state);
                if !submenu.items.is_empty() {
                    filtered.push(OwnedMenuItem::Submenu(submenu));
                }
            }
            item => filtered.push(item),
        }
    }
    if matches!(filtered.last(), Some(OwnedMenuItem::Separator)) {
        filtered.pop();
    }
    filtered
}

fn convert_menu(menu: OwnedMenu) -> Menu {
    Menu {
        name: menu.name,
        disabled: menu.disabled,
        items: menu
            .items
            .into_iter()
            .map(|item| match item {
                OwnedMenuItem::Separator => MenuItem::Separator,
                OwnedMenuItem::Submenu(submenu) => MenuItem::Submenu(convert_menu(submenu)),
                OwnedMenuItem::SystemMenu(system) => MenuItem::SystemMenu(OsMenu {
                    name: system.name,
                    menu_type: system.menu_type,
                }),
                OwnedMenuItem::Action {
                    name,
                    action,
                    os_action,
                    checked,
                    disabled,
                } => MenuItem::Action {
                    name: name.into(),
                    action,
                    os_action,
                    checked,
                    disabled,
                },
            })
            .collect(),
    }
}

fn set_native_menus(state: SurfaceState, cx: &App) {
    if let Some(templates) = cx.try_global::<ApplicationMenuTemplates>() {
        cx.set_menus(
            filter_menus(templates.0.clone(), state)
                .into_iter()
                .map(convert_menu),
        );
    }
}

impl AndroidPanel {
    pub(crate) fn context_root(&self) -> Option<PathBuf> {
        self.root
            .as_ref()
            .filter(|root| self.project_surfaces.contains_key(*root))
            .cloned()
            .or_else(|| {
                (self.project_surfaces.len() == 1)
                    .then(|| self.project_surfaces.keys().next().cloned())
                    .flatten()
            })
    }

    pub(crate) fn surface_state(&self) -> SurfaceState {
        let mut state = self
            .context_root()
            .and_then(|root| self.project_surfaces.get(&root).copied())
            .unwrap_or_default();
        state.build_window = state.android
            || self.tool_setup.last_operation.is_some()
            || self.tool_setup.operation.is_some();
        state
    }

    pub(super) fn refresh_project_surfaces(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.surface_scan = None;
        self.surface_revision += 1;
        let revision = self.surface_revision;
        let project = self.project.read(cx);
        let snapshots = project
            .visible_worktrees(cx)
            .map(|worktree| worktree.read(cx).snapshot())
            .collect::<Vec<_>>();
        let roots = self.roots(cx);
        self.project_surfaces.retain(|root, _| roots.contains(root));
        let fs = project.fs().clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let scan_cancelled = cancelled.clone();
        let task = cx.spawn_in(window, async move |panel, cx| {
            let states = cx
                .background_spawn(async move {
                    let mut states = HashMap::new();
                    for snapshot in snapshots {
                        let root = snapshot.abs_path().to_path_buf();
                        let mut files = HashMap::new();
                        for (index, entry) in snapshot.files(false, 0).enumerate() {
                            if index % 128 == 0 && scan_cancelled.load(Ordering::Relaxed) {
                                return None;
                            }
                            let path = entry.path.as_unix_str();
                            if !is_context_path(path) {
                                continue;
                            }
                            if path.ends_with("AndroidManifest.xml") {
                                files.insert(path.to_owned(), String::new());
                            } else if path.ends_with(".gradle")
                                || path.ends_with(".gradle.kts")
                                || path.ends_with(".toml")
                            {
                                let absolute = root.join(entry.path.as_std_path());
                                let metadata = match fs.metadata(&absolute).await {
                                    Ok(Some(metadata))
                                        if !metadata.is_dir
                                            && !metadata.is_fifo
                                            && metadata.len <= 1024 * 1024 =>
                                    {
                                        metadata
                                    }
                                    _ => continue,
                                };
                                match fs.load(&absolute).await {
                                    Ok(text)
                                        if text.len() as u64 <= metadata.len
                                            && text.len() <= 1024 * 1024 =>
                                    {
                                        files.insert(path.to_owned(), text);
                                    }
                                    Ok(_) => log::debug!(
                                        "Gradle context input changed during inspection"
                                    ),
                                    Err(error) => {
                                        log::debug!("Cannot inspect Gradle project: {error:#}")
                                    }
                                }
                                if scan_cancelled.load(Ordering::Relaxed) {
                                    return None;
                                }
                            }
                        }
                        let ecosystems = android_tools::project_detection::detect_project(&files);
                        states.insert(
                            root,
                            SurfaceState {
                                eligible: ecosystems.qualifies(),
                                android: ecosystems.android,
                                build_window: ecosystems.android,
                            },
                        );
                    }
                    Some(states)
                })
                .await;
            if let Some(states) = states {
                panel
                    .update_in(cx, |panel, window, cx| {
                        if panel.surface_revision != revision {
                            return;
                        }
                        panel.project_surfaces = states;
                        panel.surfaces_changed(window, cx);
                        panel.auto_sync_project(window, cx);
                    })
                    .log_err();
            }
        });
        self.surface_scan = Some(SurfaceScan {
            _task: task,
            cancelled,
        });
        self.surfaces_changed(window, cx);
    }

    pub(super) fn surfaces_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.surface_state().android {
            self.auto_sync_root = None;
            if self.tool_setup.operation.is_none() {
                if let Some((tab, _)) = self.active_build_session {
                    self.cancel_build(tab, cx);
                }
                self.pending_gradle_operation = None;
                self.active_operation_id = None;
                self.kotlin_refresh_task = None;
                self.kotlin_refresh_pending = None;
                self.deploy_task = None;
                self.debug_task = None;
                self.emulator_task = None;
                self.emulator_startup = None;
                self.kotlin_task = None;
                self.build_task = None;
                self.sync_task = None;
                self.running = false;
                self.syncing = false;
                self.targets.clear();
                self.selected_target = None;
                if self.project.read(cx).android_model().root().is_some() {
                    self.invalidate_model(None, cx);
                }
            }
            self.device_task = None;
            self.refreshing_devices = false;
            self.devices.clear();
            self.emulators.clear();
            self.selected_serial = None;
            self.selected_avd = None;
        }
        cx.notify();
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    let state = SurfaceState::for_workspace(workspace, cx);
                    if !state.build_window {
                        close_active_panel::<BuildPanel>(workspace, window, cx);
                    }
                    if !state.android {
                        close_active_panel::<LogcatPanel>(workspace, window, cx);
                    }
                    if let Some(panel) = workspace.panel::<LogcatPanel>(cx) {
                        panel.update(cx, |panel, cx| panel.context_changed(cx));
                    }
                    if let Some(panel) = workspace.panel::<BuildPanel>(cx) {
                        panel.update(cx, |_, cx| cx.notify());
                    }
                    for dock in workspace.all_docks() {
                        dock.update(cx, |_, cx| cx.notify());
                    }
                    cx.notify();
                })
                .log_err();
            if window.is_window_active() {
                if let Some(workspace) = Workspace::for_window(window, cx)
                    .or_else(|| window.root::<Workspace>().flatten())
                {
                    set_native_menus(SurfaceState::for_workspace(workspace.read(cx), cx), cx);
                }
            }
        });
    }
}

pub(super) fn is_context_path(path: &str) -> bool {
    android_tools::project_detection::is_project_context_path(path)
}

pub(super) struct SurfaceScan {
    _task: Task<()>,
    cancelled: Arc<AtomicBool>,
}

impl Drop for SurfaceScan {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

fn close_active_panel<P: workspace::dock::Panel>(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(panel) = workspace.panel::<P>(cx) else {
        return;
    };
    for dock in workspace.all_docks() {
        if dock
            .read(cx)
            .visible_panel()
            .is_some_and(|visible| visible.panel_id() == panel.entity_id())
        {
            let was_focused = panel.read(cx).focus_handle(cx).contains_focused(window, cx);
            if panel.read(cx).is_zoomed(window, cx) {
                panel.update(cx, |panel, cx| {
                    panel.set_zoomed(false, window, cx);
                    cx.emit(workspace::dock::PanelEvent::ZoomOut);
                });
            }
            dock.update(cx, |dock, cx| dock.set_open(false, window, cx));
            if was_focused {
                workspace
                    .active_pane()
                    .update(cx, |pane, cx| window.focus(&pane.focus_handle(cx), cx));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use project::{FakeFs, trusted_worktrees};
    use serde_json::json;
    use workspace::AppState;

    #[test]
    fn surfaces_keep_setup_and_generic_actions_without_android_commands() {
        let generic = SurfaceState::default();
        assert!(generic.permits(&Setup));
        assert!(!generic.permits(&SyncProject));
        assert!(!generic.permits(&Logcat));
        let multiplatform = SurfaceState {
            eligible: true,
            ..Default::default()
        };
        assert!(multiplatform.permits(&ToggleFocus));
        assert!(!multiplatform.permits(&Run));
        assert!(!multiplatform.permits(&ConfigureKotlin));
        let setup_output = SurfaceState {
            build_window: true,
            ..Default::default()
        };
        assert!(setup_output.permits(&ToggleBuild));
        assert!(!setup_output.permits(&Logcat));
        let menu = Menu::new("Run")
            .items([
                MenuItem::separator(),
                MenuItem::action("Android", Run),
                MenuItem::separator(),
                MenuItem::action("Setup", Setup),
                MenuItem::separator(),
            ])
            .owned();
        let menus = filter_menus(vec![menu], generic);
        assert_eq!(menus[0].items.len(), 1);
        assert!(
            matches!(&menus[0].items[0], OwnedMenuItem::Action { name, .. } if name == "Setup")
        );
    }

    #[gpui::test]
    async fn eligibility_tracks_selected_roots_and_live_context_changes(cx: &mut TestAppContext) {
        let _state = cx.update(|cx| {
            let state = AppState::test(cx);
            trusted_worktrees::init(Default::default(), cx);
            state
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/android",
            json!({"build.gradle.kts": "plugins { id(\"com.android.application\") }"}),
        )
        .await;
        fs.insert_tree(
            "/desktop",
            json!({"build.gradle.kts": "plugins { kotlin(\"multiplatform\") }"}),
        )
        .await;
        fs.insert_tree("/generic", json!({"main.py": "print('hello')", "index.html": "<html/>", "settings.gradle.kts": "", "gradlew": ""})).await;
        let project = Project::test(
            fs.clone(),
            [
                Path::new("/android"),
                Path::new("/desktop"),
                Path::new("/generic"),
            ],
            cx,
        )
        .await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project.clone(), cx));
        workspace.update_in(cx, |workspace, window, cx| {
            register_controller(workspace, &panel, cx);
            workspace.add_panel(panel.read(cx).build_panel.clone(), window, cx);
            let logcat = cx.new(|cx| LogcatPanel::new(workspace, window, cx));
            workspace.add_panel(logcat, window, cx);
        });
        panel.update_in(cx, |panel, window, cx| {
            panel.observe_project_open(window, cx)
        });
        cx.run_until_parked();
        assert!(
            workspace.read_with(cx, |_, cx| toolbar(&workspace.downgrade(), cx).is_some()),
            "Multi-root chooser remains reachable"
        );
        for root in [
            "/android", "/generic", "/desktop", "/android", "/generic", "/android",
        ] {
            panel.update_in(cx, |panel, window, cx| {
                panel.root = Some(root.into());
                panel.surfaces_changed(window, cx);
            });
            cx.run_until_parked();
            workspace.read_with(cx, |workspace, cx| {
                let state = SurfaceState::for_workspace(workspace, cx);
                assert_eq!(state.android, root == "/android");
                assert_eq!(state.eligible, root != "/generic");
                assert_eq!(action_available(workspace, &Logcat, cx), root == "/android");
                assert!(workspace.panel::<BuildPanel>(cx).is_some());
            });
        }
        panel.update(cx, |panel, cx| {
            panel.running = true;
            panel.emulator_task = Some(cx.spawn(async |_, _| futures::future::pending().await));
        });
        fs.insert_tree(
            "/android",
            json!({"build.gradle.kts": "plugins { kotlin(\"jvm\") }"}),
        )
        .await;
        cx.run_until_parked();
        assert!(!panel.read_with(cx, |panel, _| panel.surface_state().android));
        assert!(panel.read_with(cx, |panel, _| !panel.running
            && !panel.syncing
            && panel.emulator_task.is_none()));
        assert!(panel.read_with(cx, |panel, _| panel.auto_sync_root.is_none()));
        fs.insert_tree(
            "/android",
            json!({"build.gradle.kts": "plugins { id(\"com.android.library\") }"}),
        )
        .await;
        cx.run_until_parked();
        assert!(panel.read_with(cx, |panel, _| panel.surface_state().android));
        let id = project.read_with(cx, |project, cx| {
            project
                .visible_worktrees(cx)
                .find(|tree| tree.read(cx).abs_path().as_ref() == Path::new("/android"))
                .expect("Android worktree")
                .read(cx)
                .id()
        });
        project.update(cx, |project, cx| project.remove_worktree(id, cx));
        cx.run_until_parked();
        assert!(!panel.read_with(cx, |panel, _| panel.surface_state().android));
    }
}
