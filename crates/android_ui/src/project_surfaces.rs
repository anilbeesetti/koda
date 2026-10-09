use crate::{
    AndroidPanel, Build, BuildPanel, ComposePreview, ConfigureJava, ConfigureKotlin,
    ConfigureOfficialKotlin, Debug, GradleOperation, Lint, Logcat, RefreshDevices, Run,
    StopEmulator, SyncProject, Test, ToggleBuild, ToggleComposePreview, ToggleFocus,
    android_logcat, android_logcat_panel::LogcatPanel, project_context, with_panel,
    with_source_panel,
};
use android_tools::project_context::{ContextCapabilities, OperationalReadiness};
use gpui::{
    Action, App, Context, Focusable as _, Global, Menu, MenuItem, OsMenu, OwnedMenu, OwnedMenuItem,
    WeakEntity, Window,
};
use ui::prelude::*;
use util::ResultExt as _;
use workspace::Workspace;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SurfaceState {
    pub capabilities: ContextCapabilities,
    pub build: bool,
    pub configuration: bool,
    pub build_window: bool,
}

impl SurfaceState {
    pub(crate) fn for_workspace(workspace: &Workspace, cx: &App) -> Self {
        let project = workspace.project().read(cx);
        let model = project.android_model();
        let Some(controller) = project_context::for_workspace(&workspace.weak_handle(), cx) else {
            return Self::default();
        };
        let controller = controller.read(cx);
        let capabilities = controller.capabilities(
            OperationalReadiness {
                application_module: model
                    .selected
                    .as_ref()
                    .map(|selected| selected.selected.module.as_str()),
                model_current: model.model.is_some(),
                model_root: model.root(),
                android_renderer_supported: cfg!(feature = "bundled-preview"),
            },
            cx,
        );
        let build = capabilities.android_devices
            && model.model.is_some()
            && controller.root(cx).as_deref() == model.root();
        Self {
            capabilities,
            build,
            configuration: build
                && model.selected.as_ref().is_some_and(|selected| {
                    selected
                        .model
                        .variant(&selected.selected)
                        .is_some_and(|(module, variant)| {
                            module.kind == android_tools::project_model::ModuleKind::Application
                                && variant.output_listing.is_some()
                        })
                }),
            build_window: capabilities.android_sync || controller.owns_sync_session(cx),
        }
    }

    pub(crate) fn for_panel(panel: &AndroidPanel, cx: &App) -> Self {
        let Some(controller) = project_context::for_workspace(&panel.workspace, cx) else {
            return Self::default();
        };
        let project = panel.project.read(cx);
        let model = project.android_model();
        let controller = controller.read(cx);
        let capabilities = controller.capabilities(
            OperationalReadiness {
                application_module: panel
                    .selected_target
                    .as_ref()
                    .map(|target| target.module.as_str()),
                model_current: model.model.is_some(),
                model_root: model.root(),
                android_renderer_supported: cfg!(feature = "bundled-preview"),
            },
            cx,
        );
        Self {
            capabilities,
            // A library can build without any application or deployment target.
            build: capabilities.android_devices
                && model.model.is_some()
                && controller.root(cx).as_deref() == model.root(),
            configuration: panel.selected_target.is_some()
                && capabilities.android_devices
                && model.model.is_some()
                && controller.root(cx).as_deref() == model.root(),
            build_window: capabilities.android_sync || controller.owns_sync_session(cx),
        }
    }

    pub(crate) fn qualified(self) -> bool {
        self.capabilities.ecosystems.qualifies()
    }

    fn permits(self, action: &dyn Action) -> bool {
        let action = action.as_any();
        if action.is::<ToggleFocus>() {
            self.qualified()
        } else if action.is::<ToggleBuild>() {
            self.build_window
        } else if action.is::<SyncProject>() {
            self.capabilities.android_sync
        } else if action.is::<RefreshDevices>()
            || action.is::<StopEmulator>()
            || action.is::<Logcat>()
            || action.is::<android_logcat::Toggle>()
        {
            self.capabilities.android_devices
        } else if action.is::<Run>() || action.is::<Debug>() {
            self.capabilities.android_run
        } else if action.is::<Build>() || action.is::<Test>() || action.is::<Lint>() {
            self.build
        } else if action.is::<ConfigureJava>()
            || action.is::<ConfigureKotlin>()
            || action.is::<ConfigureOfficialKotlin>()
        {
            self.configuration
        } else if action.is::<ComposePreview>() || action.is::<ToggleComposePreview>() {
            self.capabilities.android_compose_preview
        } else {
            true
        }
    }
}

pub fn action_available(workspace: &Workspace, action: &dyn Action, cx: &App) -> bool {
    SurfaceState::for_workspace(workspace, cx).permits(action)
}

pub(crate) fn context_capabilities(
    workspace: &WeakEntity<Workspace>,
    cx: &App,
) -> ContextCapabilities {
    project_context::for_workspace(workspace, cx).map_or_else(Default::default, |controller| {
        controller
            .read(cx)
            .capabilities(OperationalReadiness::default(), cx)
    })
}

pub(crate) fn build_window_available(workspace: &WeakEntity<Workspace>, cx: &App) -> bool {
    project_context::for_workspace(workspace, cx).is_some_and(|controller| {
        let controller = controller.read(cx);
        controller
            .capabilities(OperationalReadiness::default(), cx)
            .android_sync
            || controller.owns_sync_session(cx)
    })
}

pub(crate) fn register_actions(workspace: &mut Workspace) {
    macro_rules! register {
        ($action:ty, $callback:expr) => {
            workspace.register_action_renderer(|element, workspace, _, cx| {
                if action_available(workspace, &<$action>::default(), cx) {
                    element.on_action(cx.listener(|workspace, action: &$action, window, cx| {
                        if action_available(workspace, action, cx) {
                            ($callback)(workspace, action, window, cx);
                        } else {
                            cx.propagate();
                        }
                    }))
                } else {
                    element
                }
            });
        };
    }
    register!(ToggleBuild, |workspace: &mut Workspace,
                            _: &ToggleBuild,
                            window,
                            cx| {
        workspace.toggle_panel_focus::<BuildPanel>(window, cx);
    });
    register!(ToggleFocus, |workspace: &mut Workspace,
                            _: &ToggleFocus,
                            window,
                            cx| {
        workspace.toggle_panel_focus::<AndroidPanel>(window, cx);
    });
    register!(SyncProject, |workspace: &mut Workspace,
                            _: &SyncProject,
                            window,
                            cx| {
        with_panel(workspace, window, cx, AndroidPanel::sync_project);
    });
    register!(RefreshDevices, |workspace: &mut Workspace,
                               _: &RefreshDevices,
                               window,
                               cx| {
        with_panel(workspace, window, cx, |panel, _, cx| {
            panel.refresh_devices(cx)
        });
    });
    register!(StopEmulator, |workspace: &mut Workspace,
                             _: &StopEmulator,
                             window,
                             cx| {
        with_panel(workspace, window, cx, AndroidPanel::stop_emulator);
    });
    macro_rules! operation {
        ($action:ty, $operation:ident) => {
            register!($action, |workspace: &mut Workspace,
                                _: &$action,
                                window,
                                cx| {
                with_panel(workspace, window, cx, |panel, window, cx| {
                    panel.gradle(GradleOperation::$operation, window, cx)
                });
            });
        };
    }
    operation!(Build, Build);
    operation!(Run, Run);
    operation!(Debug, Debug);
    operation!(Test, Test);
    operation!(Lint, Lint);
    operation!(ConfigureJava, Java);
    operation!(ConfigureKotlin, Kotlin);
    operation!(ConfigureOfficialKotlin, Kotlin);
    register!(ComposePreview, |workspace: &mut Workspace,
                               _: &ComposePreview,
                               window,
                               cx| {
        with_source_panel(workspace, window, cx, |panel, window, cx| {
            panel.gradle(GradleOperation::Preview, window, cx);
        });
    });
    register!(
        ToggleComposePreview,
        |workspace: &mut Workspace, _: &ToggleComposePreview, window, cx| {
            crate::android_preview::toggle_preview(workspace, window, cx);
        }
    );
    register!(Logcat, |workspace: &mut Workspace,
                       _: &Logcat,
                       window,
                       cx| {
        with_panel(workspace, window, cx, AndroidPanel::logcat);
    });
    register!(
        android_logcat::Toggle,
        |workspace: &mut Workspace,
         _: &android_logcat::Toggle,
         window: &mut Window,
         cx: &mut Context<Workspace>| {
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
        }
    );
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
        .and_then(|workspace| workspace.read_with(cx, SurfaceState::for_workspace).ok())
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

pub(crate) fn observe_surfaces(
    panel: &gpui::Entity<AndroidPanel>,
    window: &mut Window,
    cx: &mut App,
) {
    let workspace = panel.read(cx).workspace.clone();
    let Some(controller) = project_context::for_workspace(&workspace, cx) else {
        return;
    };
    panel.update(cx, |panel, cx| {
        panel._startup_subscriptions.push(cx.observe_in(
            &controller,
            window,
            |panel, _, window, cx| {
                panel.context_surfaces_changed(false, window, cx);
            },
        ));
        panel._startup_subscriptions.push(cx.observe_in(
            &panel.project,
            window,
            |panel, _, window, cx| {
                panel.context_surfaces_changed(false, window, cx);
            },
        ));
        panel
            ._startup_subscriptions
            .push(cx.observe_window_activation(window, |panel, window, cx| {
                panel.context_surfaces_changed(true, window, cx);
            }));
        panel.context_surfaces_changed(true, window, cx);
    });
    let panel = panel.downgrade();
    // A Workspace may be constructed while its MultiWorkspace root is still
    // being installed. Retained workspaces later change that root's chrome
    // ownership without changing project facts or platform window activation.
    window.defer(cx, move |window, cx| {
        let Some(multi_workspace) = window.root::<workspace::MultiWorkspace>().flatten() else {
            return;
        };
        panel
            .update(cx, |panel, cx| {
                panel._startup_subscriptions.push(cx.subscribe_in(
                    &multi_workspace,
                    window,
                    |panel, multi, event, window, cx| {
                        if matches!(
                            event,
                            workspace::MultiWorkspaceEvent::ActiveWorkspaceChanged { .. }
                        ) && multi.read(cx).workspace().entity_id()
                            == panel.workspace.entity_id()
                        {
                            panel.context_surfaces_changed(true, window, cx);
                        }
                    },
                ));
            })
            .log_err();
    });
}

impl AndroidPanel {
    fn context_surfaces_changed(
        &mut self,
        activated: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = SurfaceState::for_panel(self, cx);
        let owner = project_context::for_workspace(&self.workspace, cx)
            .and_then(|controller| controller.read(cx).action_token(cx));
        if !activated && state == self.surface_state && owner == self.surface_owner {
            return;
        }
        self.surface_state = state;
        self.surface_owner = owner;
        cx.notify();
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    let state = SurfaceState::for_workspace(workspace, cx);
                    if !state.qualified() {
                        close_active_panel::<AndroidPanel>(workspace, window, cx);
                    }
                    if !state.build_window {
                        close_active_panel::<BuildPanel>(workspace, window, cx);
                    }
                    if !state.capabilities.android_devices {
                        close_active_panel::<LogcatPanel>(workspace, window, cx);
                    }
                    for dock in workspace.all_docks() {
                        dock.update(cx, |_, cx| cx.notify());
                    }
                    cx.notify();
                })
                .log_err();
            if window.is_window_active() {
                if let Some(current) = Workspace::for_window(window, cx)
                    .or_else(|| window.root::<Workspace>().flatten())
                {
                    set_native_menus(SurfaceState::for_workspace(current.read(cx), cx), cx);
                }
            }
        });
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
pub(crate) mod tests {
    use super::*;
    use android_tools::project_context::{ActiveContext, PluginId, decode_context_record};
    use anyhow::{Context as _, Result};
    use gpui::{Entity, TestAppContext};
    use project::{
        FakeFs, Project,
        trusted_worktrees::{self, PathTrust, TrustedWorktrees},
    };
    use serde_json::json;
    use std::{cell::Cell, path::Path, rc::Rc};
    use workspace::AppState;

    fn initialize(cx: &mut App) {
        let app_state = AppState::test(cx);
        editor::init(cx);
        workspace::init(app_state, cx);
        trusted_worktrees::init(Default::default(), cx);
        crate::init(cx);
        install_application_menus(
            vec![Menu::new("Run").items([
                MenuItem::separator(),
                MenuItem::action("Android Tools", ToggleFocus),
                MenuItem::action("Run App", Run),
                MenuItem::action("Debug App", Debug),
                MenuItem::action("Build", Build),
                MenuItem::separator(),
                MenuItem::action("Run Unit Tests", Test),
                MenuItem::action("Run Android Lint", Lint),
                MenuItem::action("Sync Android Project", SyncProject),
                MenuItem::action("Refresh Android Devices", RefreshDevices),
                MenuItem::action("Compose Preview", ComposePreview),
                MenuItem::separator(),
                MenuItem::action("Toggle Terminal", workspace::ToggleBottomDock),
            ])],
            cx,
        );
    }

    pub(crate) fn publish_catalogue(
        project: &Entity<Project>,
        root: &Path,
        plugins: &[PluginId],
        platforms: &[(&str, &str)],
        complete: bool,
        cx: &mut App,
    ) -> Result<()> {
        // Synthetic evaluated-getter output isolates production UI behavior;
        // official Gradle execution and reference parity are separate gates.
        let record = json!({"schema":1,"root":root,"gradleVersion":"9.4",
            "phase":if complete { "complete" } else { "partial" },
            "modules":[{"path":":","directory":root,
                "plugins":PluginId::ALL.into_iter().filter(|plugin| complete || plugins.contains(plugin))
                    .map(|plugin| json!({"plugin":plugin,"applied":plugins.contains(&plugin)})).collect::<Vec<_>>(),
                "targets":{"status":"available","value":platforms.iter().map(|(name,platform)|json!({"name":name,"platform":platform})).collect::<Vec<_>>()}}]});
        let snapshot = decode_context_record(&serde_json::to_vec(&record)?, root)?;
        project.update(cx, |project, cx| {
            let worktree = project
                .visible_worktrees(cx)
                .find(|worktree| worktree.read(cx).abs_path().as_ref() == root)
                .context("Fixture root")?
                .read(cx)
                .id();
            let handle = project.ensure_android_context(worktree, true, cx)?;
            let discovery = project.begin_android_context_import(handle, cx)?;
            let mut active = ActiveContext::default();
            active.select(Some(handle), None)?;
            let owner = active
                .discovery_token(project.android_context())
                .context("Fixture owner")?;
            project.publish_android_context(&active, &owner, &discovery, snapshot, cx)
        })
    }

    pub(crate) fn trust(project: &Entity<Project>, cx: &mut App) -> Result<()> {
        let store = project.read(cx).worktree_store().clone();
        let roots = project
            .read(cx)
            .visible_worktrees(cx)
            .map(|worktree| PathTrust::Worktree(worktree.read(cx).id()))
            .collect();
        TrustedWorktrees::try_get_global(cx)
            .context("Trust store")?
            .update(cx, |trust, cx| trust.trust(&store, roots, cx));
        Ok(())
    }

    fn menu_has(menus: &[OwnedMenu], action: &dyn Action) -> bool {
        menus.iter().any(|menu| menu.items.iter().any(|item| matches!(item, OwnedMenuItem::Action { action: candidate, .. } if candidate.partial_eq(action))))
    }

    #[gpui::test]
    async fn generic_editors_keep_workspace_actions_without_android_surfaces(
        cx: &mut TestAppContext,
    ) {
        generic_editors_keep_workspace_actions_without_android_surfaces_case(cx)
            .await
            .expect("Android project-context fixture must complete successfully");
    }

    async fn generic_editors_keep_workspace_actions_without_android_surfaces_case(
        cx: &mut TestAppContext,
    ) -> Result<()> {
        cx.update(initialize);
        let filesystem = FakeFs::new(cx.executor());
        filesystem.insert_tree("/generic", json!({"main.py":"print(1)","index.html":"<p>Hello</p>","Main.kt":"fun main() {}","Main.java":"class Main {}","layout.xml":"<item/>"})).await;
        let project =
            Project::test_with_worktree_trust(filesystem, [Path::new("/generic")], cx).await;
        cx.update(|cx| trust(&project, cx))?;
        let (workspace, visual) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        for path in [
            "main.py",
            "index.html",
            "Main.kt",
            "Main.java",
            "layout.xml",
        ] {
            workspace
                .update_in(visual, |workspace, window, cx| {
                    workspace.open_abs_path(
                        Path::new("/generic").join(path),
                        Default::default(),
                        window,
                        cx,
                    )
                })
                .await?;
            visual.run_until_parked();
            workspace.read_with(visual, |workspace, cx| {
                assert!(crate::toolbar(&workspace.weak_handle(), cx).is_none());
                assert!(!action_available(workspace, &ToggleFocus, cx));
                assert!(!action_available(workspace, &Run, cx));
                assert!(!action_available(workspace, &RefreshDevices, cx));
                let menus = application_menus(Some(&workspace.weak_handle()), cx);
                assert!(!menu_has(&menus, &ToggleFocus));
                assert!(!menu_has(&menus, &Run));
                assert!(menu_has(&menus, &workspace::ToggleBottomDock));
            });
            visual.update(|window, cx| {
                assert!(
                    !window
                        .available_actions(cx)
                        .iter()
                        .any(|action| action.as_any().is::<ToggleFocus>())
                );
                assert!(
                    window
                        .available_actions(cx)
                        .iter()
                        .any(|action| action.as_any().is::<workspace::ToggleBottomDock>())
                );
            });
            assert!(visual.debug_bounds("android-panel").is_none());
            assert!(visual.debug_bounds("tool-window-button-Android").is_none());
            assert!(
                visual
                    .debug_bounds("tool-window-button-LogcatPanel")
                    .is_none()
            );
            for group in [
                "android-manual-sync-controls",
                "android-device-controls",
                "android-build-controls",
                "android-configuration-controls",
                "android-compose-controls",
            ] {
                assert!(
                    visual.debug_bounds(group).is_none(),
                    "Generic project exposed {group}"
                );
            }
        }
        Ok(())
    }

    #[gpui::test]
    async fn desktop_multiplatform_tools_do_not_expose_android_operations(cx: &mut TestAppContext) {
        desktop_multiplatform_tools_do_not_expose_android_operations_case(cx)
            .await
            .expect("Android project-context fixture must complete successfully");
    }

    async fn desktop_multiplatform_tools_do_not_expose_android_operations_case(
        cx: &mut TestAppContext,
    ) -> Result<()> {
        cx.update(initialize);
        let filesystem = FakeFs::new(cx.executor());
        filesystem
            .insert_tree("/desktop", json!({"Main.kt":"fun main() {}"}))
            .await;
        let project =
            Project::test_with_worktree_trust(filesystem, [Path::new("/desktop")], cx).await;
        cx.update(|cx| trust(&project, cx))?;
        let (workspace, visual) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        visual.update(|_, cx| {
            publish_catalogue(
                &project,
                Path::new("/desktop"),
                &[
                    PluginId::KotlinMultiplatform,
                    PluginId::ComposeMultiplatform,
                    PluginId::ComposeCompiler,
                ],
                &[("desktop", "jvm")],
                true,
                cx,
            )
        })?;
        visual.run_until_parked();
        visual.dispatch_action(ToggleFocus);
        visual.run_until_parked();
        assert!(visual.debug_bounds("android-panel").is_some());
        for control in [
            "refresh-devices",
            "start-emulator",
            "stop-emulator",
            "logcat",
            "sync-project",
            "configure-java",
            "configure-official-kotlin",
            "android-compose-preview",
        ] {
            assert!(
                visual.debug_bounds(control).is_none(),
                "Unsupported {control}"
            );
        }
        for group in [
            "android-manual-sync-controls",
            "android-device-controls",
            "android-build-controls",
            "android-configuration-controls",
            "android-compose-controls",
        ] {
            assert!(
                visual.debug_bounds(group).is_none(),
                "Desktop project exposed {group}"
            );
        }
        workspace.read_with(visual, |workspace, cx| {
            assert!(crate::toolbar(&workspace.weak_handle(), cx).is_some());
            for action in [
                &Run as &dyn Action,
                &Debug,
                &Build,
                &Test,
                &Lint,
                &SyncProject,
                &RefreshDevices,
                &ComposePreview,
            ] {
                assert!(!action_available(workspace, action, cx));
                assert!(!menu_has(
                    &application_menus(Some(&workspace.weak_handle()), cx),
                    action
                ));
            }
        });
        Ok(())
    }

    #[gpui::test]
    async fn partial_android_keeps_manual_sync_without_device_or_run_controls(
        cx: &mut TestAppContext,
    ) {
        partial_android_keeps_manual_sync_without_device_or_run_controls_case(cx)
            .await
            .expect("Android project-context fixture must complete successfully");
    }

    async fn partial_android_keeps_manual_sync_without_device_or_run_controls_case(
        cx: &mut TestAppContext,
    ) -> Result<()> {
        cx.update(initialize);
        let filesystem = FakeFs::new(cx.executor());
        filesystem
            .insert_tree("/partial", json!({"Main.java":"class Main {}"}))
            .await;
        let project =
            Project::test_with_worktree_trust(filesystem, [Path::new("/partial")], cx).await;
        cx.update(|cx| trust(&project, cx))?;
        let (workspace, visual) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        visual.update(|_, cx| {
            publish_catalogue(
                &project,
                Path::new("/partial"),
                &[PluginId::AndroidApplication],
                &[],
                false,
                cx,
            )
        })?;
        visual.run_until_parked();
        visual.dispatch_action(ToggleFocus);
        visual.run_until_parked();
        assert!(visual.debug_bounds("android-panel").is_some());
        assert!(
            visual
                .debug_bounds("android-manual-sync-controls")
                .is_some()
        );
        for group in [
            "android-device-controls",
            "android-build-controls",
            "android-configuration-controls",
            "android-compose-controls",
        ] {
            assert!(
                visual.debug_bounds(group).is_none(),
                "Partial model exposed {group}"
            );
        }
        workspace.read_with(visual, |workspace, cx| {
            assert!(action_available(workspace, &SyncProject, cx));
            assert!(!action_available(workspace, &Run, cx));
            assert!(!action_available(workspace, &RefreshDevices, cx));
            assert!(!action_available(workspace, &ComposePreview, cx));
            assert!(menu_has(
                &application_menus(Some(&workspace.weak_handle()), cx),
                &SyncProject
            ));
        });
        Ok(())
    }

    #[gpui::test]
    async fn current_library_model_exposes_build_without_synthesizing_run(cx: &mut TestAppContext) {
        current_library_model_exposes_build_without_synthesizing_run_case(cx)
            .await
            .expect("Android project-context fixture must complete successfully");
    }

    async fn current_library_model_exposes_build_without_synthesizing_run_case(
        cx: &mut TestAppContext,
    ) -> Result<()> {
        cx.update(initialize);
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        std::fs::write(root.join("gradlew"), "")?;
        std::fs::write(root.join("settings.gradle"), "")?;
        let filesystem = FakeFs::new(cx.executor());
        filesystem
            .insert_tree(
                root,
                json!({"gradlew":"","settings.gradle":"","Main.kt":"class Library"}),
            )
            .await;
        let project = Project::test_with_worktree_trust(filesystem, [root], cx).await;
        cx.update(|cx| trust(&project, cx))?;
        let (workspace, visual) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = workspace
            .read_with(visual, |workspace, cx| workspace.panel::<AndroidPanel>(cx))
            .context("Project tools")?;
        panel.update(visual, |panel, _| {
            panel.root = Some(root.to_path_buf());
            // Keep this UI acceptance fixture out of host Gradle and ADB.
            panel.syncing = true;
            panel.refreshing_devices = true;
        });
        visual.update(|_, cx| {
            publish_catalogue(&project, root, &[PluginId::AndroidLibrary], &[("android","androidJvm")], true, cx)?;
            let model = serde_json::from_value(json!({"version":1,"root":root,"diagnostics":[],
                "modules":[{"path":":","directory":root,"namespace":"example.library","kind":"library",
                    "variants":[{"name":"debug","outputListing":null,"components":[]}]}]}))?;
            project.update(cx, |project, cx| {
                let token = project.invalidate_android_model(Some(root.to_path_buf()), cx);
                project.publish_android_model(&token, model, cx)?;
                project.select_android_variant(Some(android_tools::project_model::VariantId { module:":".into(), variant:"debug".into() }), cx)
            })
        })?;
        visual.run_until_parked();
        workspace.read_with(visual, |workspace, cx| {
            for action in [&Build as &dyn Action, &Test, &Lint] {
                assert!(action_available(workspace, action, cx));
                assert!(menu_has(
                    &application_menus(Some(&workspace.weak_handle()), cx),
                    action
                ));
            }
            assert!(!action_available(workspace, &Run, cx));
            assert!(!action_available(workspace, &Debug, cx));
            assert!(!action_available(workspace, &ComposePreview, cx));
        });
        // Library dispatch still requires its own backend variant representation;
        // eligibility must not pretend that an application target exists.
        assert!(panel.read_with(visual, |panel, _| panel.selected_target.is_none()));
        visual.dispatch_action(ToggleFocus);
        visual.run_until_parked();
        for group in [
            "android-manual-sync-controls",
            "android-device-controls",
            "android-build-controls",
            "android-configuration-controls",
        ] {
            assert!(
                visual.debug_bounds(group).is_some(),
                "Current library omitted {group}"
            );
        }
        assert!(visual.debug_bounds("android-compose-controls").is_none());
        visual.update(|_, cx| {
            publish_catalogue(
                &project,
                root,
                &[PluginId::AndroidApplication],
                &[("android", "androidJvm")],
                true,
                cx,
            )
        })?;
        panel.update(visual, |panel, cx| {
            let target = android_tools::AndroidTarget {
                module: ":".into(),
                variant: "debug".into(),
                output_listing: root.join("output.json"),
            };
            panel.targets = vec![target.clone()];
            panel.selected_target = Some(target.clone());
            crate::tests::publish_test_android_model(panel, &target, cx);
        });
        visual.run_until_parked();
        for group in [
            "android-manual-sync-controls",
            "android-device-controls",
            "android-build-controls",
            "android-configuration-controls",
        ] {
            assert!(
                visual.debug_bounds(group).is_some(),
                "Current application omitted {group}"
            );
        }
        workspace.read_with(visual, |workspace, cx| {
            assert!(action_available(workspace, &Run, cx));
            assert!(action_available(workspace, &Debug, cx));
            assert!(menu_has(
                &application_menus(Some(&workspace.weak_handle()), cx),
                &Run
            ));
        });
        visual.update(|window, cx| {
            assert!(
                window
                    .available_actions(cx)
                    .iter()
                    .any(|action| action.as_any().is::<Run>())
            );
            assert!(
                window
                    .available_actions(cx)
                    .iter()
                    .any(|action| action.as_any().is::<Build>())
            );
        });
        Ok(())
    }

    #[gpui::test]
    async fn windows_sharing_a_project_keep_distinct_active_root_surfaces(cx: &mut TestAppContext) {
        windows_sharing_a_project_keep_distinct_active_root_surfaces_case(cx)
            .await
            .expect("Android project-context fixture must complete successfully");
    }

    async fn windows_sharing_a_project_keep_distinct_active_root_surfaces_case(
        cx: &mut TestAppContext,
    ) -> Result<()> {
        cx.update(initialize);
        let filesystem = FakeFs::new(cx.executor());
        filesystem
            .insert_tree("/android-window", json!({"Main.java":"class Main {}"}))
            .await;
        filesystem
            .insert_tree("/python-window", json!({"main.py":"print(1)"}))
            .await;
        let project = Project::test_with_worktree_trust(
            filesystem,
            [Path::new("/android-window"), Path::new("/python-window")],
            cx,
        )
        .await;
        cx.update(|cx| trust(&project, cx))?;
        let (android, android_visual) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let android_window = android_visual.update(|window, _| window.window_handle());
        let (generic, generic_visual) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let generic_window = generic_visual.update(|window, _| window.window_handle());
        let mut android_visual = gpui::VisualTestContext::from_window(android_window, cx);
        let mut generic_visual = gpui::VisualTestContext::from_window(generic_window, cx);
        android
            .update_in(&mut android_visual, |workspace, window, cx| {
                workspace.open_abs_path(
                    "/android-window/Main.java".into(),
                    Default::default(),
                    window,
                    cx,
                )
            })
            .await?;
        generic
            .update_in(&mut generic_visual, |workspace, window, cx| {
                workspace.open_abs_path(
                    "/python-window/main.py".into(),
                    Default::default(),
                    window,
                    cx,
                )
            })
            .await?;
        android_visual.update(|_, cx| {
            publish_catalogue(
                &project,
                Path::new("/android-window"),
                &[PluginId::AndroidApplication],
                &[],
                false,
                cx,
            )
        })?;
        android_visual.run_until_parked();
        generic_visual.run_until_parked();
        android.read_with(&android_visual, |workspace, cx| {
            assert!(crate::toolbar(&workspace.weak_handle(), cx).is_some());
            assert!(menu_has(
                &application_menus(Some(&workspace.weak_handle()), cx),
                &SyncProject
            ));
        });
        generic.read_with(&generic_visual, |workspace, cx| {
            assert!(crate::toolbar(&workspace.weak_handle(), cx).is_none());
            assert!(!menu_has(
                &application_menus(Some(&workspace.weak_handle()), cx),
                &SyncProject
            ));
            assert!(menu_has(
                &application_menus(Some(&workspace.weak_handle()), cx),
                &workspace::ToggleBottomDock
            ));
        });
        android_visual.dispatch_action(ToggleFocus);
        android_visual.run_until_parked();
        assert!(android_visual.debug_bounds("android-panel").is_some());
        assert!(generic_visual.debug_bounds("android-panel").is_none());
        android
            .update_in(&mut android_visual, |workspace, window, cx| {
                workspace.open_abs_path(
                    "/python-window/main.py".into(),
                    Default::default(),
                    window,
                    cx,
                )
            })
            .await?;
        android_visual.run_until_parked();
        assert!(android_visual.debug_bounds("android-panel").is_none());
        assert!(android.read_with(&android_visual, |workspace, cx| {
            crate::toolbar(&workspace.weak_handle(), cx).is_none()
        }));
        Ok(())
    }

    #[gpui::test]
    async fn native_menus_follow_retained_workspace_switches_in_the_same_window(
        cx: &mut TestAppContext,
    ) {
        native_menus_follow_retained_workspace_switches_in_the_same_window_case(cx)
            .await
            .expect("Android project-context fixture must complete successfully");
    }

    async fn native_menus_follow_retained_workspace_switches_in_the_same_window_case(
        cx: &mut TestAppContext,
    ) -> Result<()> {
        cx.update(initialize);
        let filesystem = FakeFs::new(cx.executor());
        filesystem
            .insert_tree("/retained-android", json!({"Main.java":"class Main {}"}))
            .await;
        filesystem
            .insert_tree("/retained-generic", json!({"main.py":"print(1)"}))
            .await;
        let android_project = Project::test_with_worktree_trust(
            filesystem.clone(),
            [Path::new("/retained-android")],
            cx,
        )
        .await;
        let generic_project =
            Project::test_with_worktree_trust(filesystem, [Path::new("/retained-generic")], cx)
                .await;
        cx.update(|cx| {
            trust(&android_project, cx)?;
            trust(&generic_project, cx)
        })?;
        let (multi, visual) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(android_project.clone(), window, cx)
        });
        let android = multi.read_with(visual, |multi, _| multi.workspace().clone());
        visual.update(|window, _| window.activate_window());
        visual.update(|_, cx| {
            publish_catalogue(
                &android_project,
                Path::new("/retained-android"),
                &[PluginId::AndroidApplication],
                &[],
                false,
                cx,
            )
        })?;
        visual.run_until_parked();
        visual.update(|window, cx| {
            assert!(window.is_window_active());
            let menus = cx.get_menus().expect("Installed native menus");
            assert!(menu_has(&menus, &SyncProject));
            assert!(menu_has(&menus, &workspace::ToggleBottomDock));
        });
        let generic = multi.update_in(visual, |multi, window, cx| {
            multi.retain_active_workspace(cx);
            let generic = multi.test_add_workspace(generic_project, window, cx);
            multi.retain_active_workspace(cx);
            generic
        });
        visual.run_until_parked();
        for _ in 0..2 {
            visual.update(|window, cx| {
                assert!(window.is_window_active());
                let menus = cx.get_menus().expect("Generic native menus");
                assert!(!menu_has(&menus, &SyncProject));
                assert!(!menu_has(&menus, &ToggleFocus));
                assert!(menu_has(&menus, &workspace::ToggleBottomDock));
                assert!(menus.iter().all(|menu| !matches!(
                    menu.items.first(),
                    Some(OwnedMenuItem::Separator)
                ) && !matches!(
                    menu.items.last(),
                    Some(OwnedMenuItem::Separator)
                )));
            });
            multi.update_in(visual, |multi, window, cx| {
                multi.activate(android.clone(), None, window, cx)
            });
            visual.run_until_parked();
            visual.update(|window, cx| {
                assert!(window.is_window_active());
                let menus = cx.get_menus().expect("Android native menus");
                assert!(menu_has(&menus, &SyncProject));
                assert!(menu_has(&menus, &ToggleFocus));
                assert!(menu_has(&menus, &workspace::ToggleBottomDock));
            });
            multi.update_in(visual, |multi, window, cx| {
                multi.activate(generic.clone(), None, window, cx)
            });
            visual.run_until_parked();
        }
        Ok(())
    }

    #[gpui::test]
    async fn deferred_project_dispatch_survives_files_but_source_dispatch_and_rapid_roots_do_not(
        cx: &mut TestAppContext,
    ) {
        async {
            cx.update(initialize);
            let filesystem = FakeFs::new(cx.executor());
            filesystem
                .insert_tree(
                    "/dispatch-owner",
                    json!({"Main.kt":"class Main", "Other.kt":"class Other"}),
                )
                .await;
            filesystem
                .insert_tree("/dispatch-other", json!({"main.py":"print(1)"}))
                .await;
            let project = Project::test_with_worktree_trust(
                filesystem,
                [Path::new("/dispatch-owner"), Path::new("/dispatch-other")],
                cx,
            )
            .await;
            cx.update(|cx| trust(&project, cx))?;
            let (workspace, visual) =
                cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
            let panel = workspace
                .read_with(visual, |workspace, cx| workspace.panel::<AndroidPanel>(cx))
                .context("Panel")?;
            panel.update(visual, |panel, _| {
                panel.root = Some(Path::new("/dispatch-owner").to_path_buf());
                panel.syncing = true;
                panel.refreshing_devices = true;
            });
            visual.update(|_, cx| {
                publish_catalogue(
                    &project,
                    Path::new("/dispatch-owner"),
                    &[PluginId::AndroidApplication, PluginId::ComposeCompiler],
                    &[("android", "androidJvm")],
                    true,
                    cx,
                )
            })?;
            let mut items = Vec::new();
            for path in [
                "/dispatch-owner/Other.kt",
                "/dispatch-other/main.py",
                "/dispatch-owner/Main.kt",
            ] {
                workspace
                    .update_in(visual, |workspace, window, cx| {
                        workspace.open_abs_path(
                            Path::new(path).to_path_buf(),
                            Default::default(),
                            window,
                            cx,
                        )
                    })
                    .await?;
                visual.run_until_parked();
                items.push(workspace.read_with(visual, |workspace, cx| {
                    workspace.active_item(cx).expect("Fixture item")
                }));
            }
            let controller = visual
                .update(|_, cx| project_context::for_workspace(&workspace.downgrade(), cx))
                .context("Controller")?;
            let project_owner = controller
                .read_with(visual, |controller, cx| controller.project_token(cx))
                .context("Project owner")?;
            let source_owner = controller
                .read_with(visual, |controller, cx| controller.action_token(cx))
                .context("Source owner")?;
            let project_entered = Rc::new(Cell::new(0));
            let source_entered = Rc::new(Cell::new(0));
            workspace.update_in(visual, |workspace, window, cx| {
                assert!(workspace.activate_item(items[0].as_ref(), false, false, window, cx));
                // A captured Workspace transition already queued before a deferred
                // callback must retain its complete path and ordering, not be folded
                // into a later read of the final pane state.
                cx.emit(workspace::Event::ActiveProjectPathChanged(
                    workspace.active_item(cx).and_then(|item| item.project_path(cx)),
                ));
                let entered = project_entered.clone();
                with_panel(workspace, window, cx, move |_, _, _| {
                    entered.set(entered.get() + 1)
                });
                let entered = source_entered.clone();
                with_source_panel(workspace, window, cx, move |_, _, _| {
                    entered.set(entered.get() + 1)
                });
            });
            visual.run_until_parked();
            assert_eq!(
                project_entered.get(),
                1,
                "Same-root project dispatch must enter exactly once"
            );
            assert_eq!(
                source_entered.get(),
                0,
                "A queued preview must not redirect to the new source"
            );
            controller.read_with(visual, |controller, cx| {
                assert!(controller.project_is_current(&project_owner, cx));
                assert!(!controller.action_is_current(&source_owner, cx));
            });
            project_entered.set(0);
            workspace.update_in(visual, |workspace, window, cx| {
                assert!(workspace.activate_item(items[1].as_ref(), false, false, window, cx));
                cx.emit(workspace::Event::ActiveProjectPathChanged(
                    workspace.active_item(cx).and_then(|item| item.project_path(cx)),
                ));
                assert!(workspace.activate_item(items[2].as_ref(), false, false, window, cx));
                cx.emit(workspace::Event::ActiveProjectPathChanged(
                    workspace.active_item(cx).and_then(|item| item.project_path(cx)),
                ));
                let entered = project_entered.clone();
                with_panel(workspace, window, cx, move |_, _, _| {
                    entered.set(entered.get() + 1)
                });
            });
            visual.run_until_parked();
            assert_eq!(
                project_entered.get(),
                0,
                "Captured root A/B/A must not revive deferred dispatch"
            );
            assert!(!controller.read_with(visual, |controller, cx| {
                controller.project_is_current(&project_owner, cx)
            }));
            Ok::<_, anyhow::Error>(())
        }
        .await
        .expect("Deferred UX regression must reach every assertion");
    }

    #[gpui::test]
    async fn context_loss_rejects_deferred_actions_and_preserves_a_generic_dock(
        cx: &mut TestAppContext,
    ) {
        context_loss_rejects_deferred_actions_and_preserves_a_generic_dock_case(cx)
            .await
            .expect("Android project-context fixture must complete successfully");
    }

    async fn context_loss_rejects_deferred_actions_and_preserves_a_generic_dock_case(
        cx: &mut TestAppContext,
    ) -> Result<()> {
        cx.update(initialize);
        let filesystem = FakeFs::new(cx.executor());
        filesystem
            .insert_tree(
                "/owned",
                json!({"Main.kt":"fun main() {}","other.py":"print(1)"}),
            )
            .await;
        let project =
            Project::test_with_worktree_trust(filesystem, [Path::new("/owned")], cx).await;
        cx.update(|cx| trust(&project, cx))?;
        let (workspace, visual) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        visual.update(|_, cx| {
            publish_catalogue(
                &project,
                Path::new("/owned"),
                &[PluginId::KotlinMultiplatform],
                &[("desktop", "jvm")],
                true,
                cx,
            )
        })?;
        visual.run_until_parked();
        let generic = visual.new(|cx| {
            workspace::dock::test::TestPanel::new(workspace::dock::DockPosition::Bottom, 100, cx)
        });
        workspace.update_in(visual, |workspace, window, cx| {
            workspace.add_panel(generic.clone(), window, cx);
            workspace.reveal_panel::<workspace::dock::test::TestPanel>(window, cx);
        });
        let invoked = Rc::new(Cell::new(false));
        let invoked_for_callback = invoked.clone();
        workspace.update_in(visual, |workspace, window, cx| {
            with_panel(workspace, window, cx, move |_, _, _| {
                invoked_for_callback.set(true)
            });
            let root = project
                .read(cx)
                .android_context()
                .handles()
                .next()
                .expect("Owned root");
            project.update(cx, |project, cx| {
                let worktree = project
                    .visible_worktrees(cx)
                    .next()
                    .expect("Root")
                    .read(cx)
                    .id();
                project
                    .ensure_android_context(worktree, false, cx)
                    .expect("Revoke root");
            });
            assert!(project.read(cx).android_context().token(root).is_none());
        });
        visual.run_until_parked();
        assert!(!invoked.get(), "Old active-context token must not dispatch");
        workspace.read_with(visual, |workspace, cx| {
            assert_eq!(
                workspace
                    .bottom_dock()
                    .read(cx)
                    .visible_panel()
                    .map(|panel| panel.panel_id()),
                Some(generic.entity_id())
            );
        });
        Ok(())
    }
}
