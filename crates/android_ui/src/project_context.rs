use crate::android_build::{
    self, BuildEvent, BuildPanel, BuildStatus, BuildTab, CapturedProcessOutput,
};
use android_tools::project_context::{
    ActiveContext, ActiveContextToken, ContextCapabilities, ContextSnapshot, DiscoveryToken,
    ModuleOwner, ObservationPhase, OperationalReadiness, RootHandle, decode_context_output,
};
use anyhow::{Context as _, Result, ensure};
use futures::{
    FutureExt as _,
    channel::{mpsc, oneshot},
    future::{BoxFuture, Shared},
};
use gpui::{
    App, BackgroundExecutor, Context, Entity, EntityId, Global, InteractiveElement as _,
    Subscription, Task, WeakEntity, Window, actions,
};
use project::{
    Project, WorktreeId,
    git_store::{GitStoreEvent, RepositoryEvent},
    trusted_worktrees::{TrustedWorktrees, TrustedWorktreesEvent},
};
use std::{collections::HashMap, path::PathBuf, time::Duration};
use util::{ResultExt as _, rel_path::RelPath};
use workspace::{Toast, Workspace, notifications::NotificationId};

actions!(
    project,
    [
        /// Evaluates this trusted Gradle project's applied plugins and targets.
        ImportGradleProject,
    ]
);

#[derive(Default)]
struct Controllers(HashMap<EntityId, WeakEntity<ProjectContextController>>);
impl Global for Controllers {}

pub(crate) fn for_workspace(
    workspace: &WeakEntity<Workspace>,
    cx: &App,
) -> Option<Entity<ProjectContextController>> {
    cx.try_global::<Controllers>()?
        .0
        .get(&workspace.entity_id())?
        .upgrade()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, TestAppContext};
    use project::trusted_worktrees::{self, PathTrust};
    use serde_json::json;
    use workspace::AppState;

    #[gpui::test]
    async fn exact_source_restriction_is_visible_with_other_restricted_directory_roots(
        cx: &mut TestAppContext,
    ) -> Result<()> {
        cx.update(|cx| {
            AppState::test(cx);
            trusted_worktrees::init(Default::default(), cx);
        });
        let filesystem = project::FakeFs::new(cx.executor());
        filesystem
            .insert_tree("/restricted-directory", json!({"main.py":"print(1)"}))
            .await;
        filesystem
            .insert_tree("/single-source", json!({"Main.kt":"fun main() {}"}))
            .await;
        let project = Project::test_with_worktree_trust(
            filesystem,
            [
                std::path::Path::new("/restricted-directory"),
                std::path::Path::new("/single-source/Main.kt"),
            ],
            cx,
        )
        .await;
        let (directory, source, store) = project.read_with(cx, |project, cx| {
            let directory = project
                .visible_worktrees(cx)
                .find(|worktree| {
                    worktree.read(cx).abs_path().as_ref()
                        == std::path::Path::new("/restricted-directory")
                })
                .context("Directory root")?
                .read(cx)
                .id();
            let source = project
                .visible_worktrees(cx)
                .find(|worktree| {
                    worktree.read(cx).abs_path().as_ref()
                        == std::path::Path::new("/single-source/Main.kt")
                })
                .context("Single source root")?;
            assert!(source.read(cx).is_single_file());
            Ok::<_, anyhow::Error>((directory, source.read(cx).id(), project.worktree_store()))
        })?;
        let trust = cx
            .read(TrustedWorktrees::try_get_global)
            .context("Trust store")?;
        trust.update(cx, |trust, cx| {
            trust.restrict(
                store.downgrade(),
                [PathTrust::Worktree(directory), PathTrust::Worktree(source)]
                    .into_iter()
                    .collect(),
                cx,
            );
            assert!(trust.is_worktree_restricted(&store, source));
            assert!(trust.is_worktree_restricted(&store, directory));
            assert!(
                trust
                    .restricted_worktrees(&store, cx)
                    .iter()
                    .all(|(id, _)| *id != source),
                "Display lists may omit a restricted single file"
            );
            trust.trust(
                &store,
                [PathTrust::Worktree(source)].into_iter().collect(),
                cx,
            );
            assert!(!trust.is_worktree_restricted(&store, source));
            assert!(trust.is_worktree_restricted(&store, directory));
        });
        Ok(())
    }

    #[gpui::test]
    async fn untitled_editor_retains_selected_project_without_borrowing_ambiguous_root(
        cx: &mut TestAppContext,
    ) -> Result<()> {
        cx.update(|cx| {
            let state = AppState::test(cx);
            editor::init(cx);
            workspace::init(state, cx);
            trusted_worktrees::init(Default::default(), cx);
            crate::init(cx);
        });
        let filesystem = project::FakeFs::new(cx.executor());
        filesystem
            .insert_tree("/scratch-android", json!({"Main.kt":"fun main() {}"}))
            .await;
        filesystem
            .insert_tree("/scratch-python", json!({"main.py":"print(1)"}))
            .await;
        let project = Project::test_with_worktree_trust(
            filesystem.clone(),
            [
                std::path::Path::new("/scratch-android"),
                std::path::Path::new("/scratch-python"),
            ],
            cx,
        )
        .await;
        cx.update(|cx| {
            crate::project_surfaces::tests::trust(&project, cx)?;
            crate::project_surfaces::tests::publish_catalogue(
                &project,
                std::path::Path::new("/scratch-android"),
                &[
                    android_tools::project_context::PluginId::AndroidApplication,
                    android_tools::project_context::PluginId::ComposeCompiler,
                ],
                &[("android", "androidJvm")],
                true,
                cx,
            )
        })?;
        let (workspace, visual) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let add_untitled = |workspace: &mut Workspace,
                            window: &mut Window,
                            cx: &mut Context<Workspace>| {
            let buffer = cx.new(|cx| language::Buffer::local("", cx));
            let editor = cx.new(|cx| {
                editor::Editor::for_buffer(buffer, Some(workspace.project().clone()), window, cx)
            });
            workspace.active_pane().update(cx, |pane, cx| {
                pane.add_item(Box::new(editor), true, true, None, window, cx)
            });
        };
        workspace.update_in(visual, add_untitled);
        visual.run_until_parked();
        let controller = visual
            .update(|_, cx| for_workspace(&workspace.downgrade(), cx))
            .context("Controller")?;
        controller.read_with(visual, |controller, cx| {
            assert!(
                controller.root(cx).is_none(),
                "Two unselected roots must not be guessed for an untitled file"
            );
            assert_eq!(
                controller.capabilities(Default::default(), cx),
                Default::default()
            );
        });
        workspace
            .update_in(visual, |workspace, window, cx| {
                workspace.open_abs_path(
                    std::path::Path::new("/scratch-android/Main.kt"),
                    Default::default(),
                    window,
                    cx,
                )
            })
            .await?;
        visual.run_until_parked();
        let source_token = controller.read_with(visual, |controller, cx| {
            controller.action_token(cx).context("Android source owner")
        })?;
        workspace.update_in(visual, add_untitled);
        visual.run_until_parked();
        controller.read_with(visual, |controller, cx| {
            let root = PathBuf::from("/scratch-android");
            assert_eq!(controller.root(cx), Some(root.clone()));
            assert!(!controller.action_is_current(&source_token, cx));
            let capabilities = controller.capabilities(
                OperationalReadiness {
                    application_module: Some(":"),
                    model_current: true,
                    model_root: Some(&root),
                    android_renderer_supported: true,
                },
                cx,
            );
            assert!(capabilities.android_devices && capabilities.android_run);
            assert!(
                !capabilities.android_compose_preview,
                "An untitled buffer cannot borrow a Compose source-file owner"
            );
        });
        let single = Project::test_with_worktree_trust(
            filesystem,
            [std::path::Path::new("/scratch-android")],
            &mut visual.cx,
        )
        .await;
        visual.update(|_, cx| {
            crate::project_surfaces::tests::trust(&single, cx)?;
            crate::project_surfaces::tests::publish_catalogue(
                &single,
                std::path::Path::new("/scratch-android"),
                &[android_tools::project_context::PluginId::AndroidApplication],
                &[("android", "androidJvm")],
                true,
                cx,
            )
        })?;
        let (single_workspace, single_visual) =
            visual.add_window_view(|window, cx| Workspace::test_new(single, window, cx));
        single_workspace.update_in(single_visual, add_untitled);
        single_visual.run_until_parked();
        let single_controller = single_visual
            .update(|_, cx| for_workspace(&single_workspace.downgrade(), cx))
            .context("Single-root controller")?;
        single_controller.read_with(single_visual, |controller, cx| {
            assert_eq!(controller.root(cx), Some(PathBuf::from("/scratch-android")))
        });
        Ok(())
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn deferred_import_action_enters_current_workspace_without_reentry(
        cx: &mut TestAppContext,
    ) -> Result<()> {
        cx.executor().allow_parking();
        let _app_state = cx.update(|cx| {
            let state = AppState::test(cx);
            trusted_worktrees::init(Default::default(), cx);
            state
        });
        let fixture = tempfile::TempDir::new()?;
        let root = fixture.path().to_path_buf();
        let payload = json!({"schema":1,"root":&root,"gradleVersion":"9.6.1","phase":"complete",
            "modules":[{"path":":","directory":&root,
                "plugins":android_tools::project_context::PluginId::ALL.map(|plugin| json!({"plugin":plugin,"applied":false})),
                "targets":{"status":"available","value":[]}}]});
        let record = format!(
            "{}{}",
            android_tools::project_context::CONTEXT_OUTPUT_PREFIX,
            serde_json::to_string(&payload)?
        );
        // This private wrapper stub exercises production action/process transport;
        // it is not an evaluated Gradle fixture or original reference-test port.
        std::fs::write(
            root.join("gradlew"),
            format!("printf '%s\\n' '{}'\n", record.replace('\'', "'\\''")),
        )?;
        let filesystem = project::FakeFs::new(cx.executor());
        filesystem
            .insert_tree(
                &root,
                json!({"gradlew":"", "build.gradle":"", "main.py":"print(1)"}),
            )
            .await;
        let project = Project::test_with_worktree_trust(filesystem, [root.as_path()], cx).await;
        let worktree = project
            .read_with(cx, |project, cx| {
                project
                    .visible_worktrees(cx)
                    .next()
                    .map(|worktree| worktree.read(cx).id())
            })
            .context("Root worktree")?;
        let store = project.read_with(cx, |project, _| project.worktree_store().clone());
        let trust = cx
            .read(TrustedWorktrees::try_get_global)
            .context("Trust store")?;
        trust.update(cx, |trust, cx| {
            trust.trust(
                &store,
                [PathTrust::Worktree(worktree)].into_iter().collect(),
                cx,
            )
        });
        let (workspace, visual) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let build_panel = visual.new(|cx| BuildPanel::new(workspace.downgrade(), cx));
        workspace.update_in(visual, |workspace, window, cx| {
            workspace.add_panel(build_panel.clone(), window, cx);
            register(workspace, build_panel.clone(), window, cx);
        });
        visual.run_until_parked();
        let controller = visual
            .update(|_, cx| for_workspace(&workspace.downgrade(), cx))
            .context("Context controller")?;
        assert_eq!(
            controller.read_with(visual, |controller, cx| controller.import_candidate(cx)),
            Some(root.clone())
        );
        let handle = project
            .read_with(visual, |project, _| {
                project.android_context().handle(worktree.to_proto())
            })
            .context("Fixture root handle")?;
        assert!(!project.read_with(visual, |project, _| {
            project.android_context_observes(handle, &root)
        }));
        visual.dispatch_action(ImportGradleProject);
        visual.run_until_parked();
        assert!(build_panel.read_with(visual, |panel, _| {
            panel.session_id(BuildTab::Sync).is_some()
        }));
        cx.condition(&project, |project, _| {
            project.android_context().snapshot(handle).is_some()
        })
        .await;
        controller.read_with(cx, |controller, cx| {
            assert_eq!(
                controller.capabilities(OperationalReadiness::default(), cx),
                ContextCapabilities::default()
            );
            assert_eq!(controller.root(cx), Some(root));
        });
        Ok(())
    }
}

pub(crate) fn register(
    workspace: &mut Workspace,
    build_panel: Entity<BuildPanel>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if !cx.has_global::<Controllers>() {
        cx.set_global(Controllers::default());
    }
    let project = workspace.project().clone();
    let weak_workspace = workspace.weak_handle();
    let controller = cx.new(|cx| {
        ProjectContextController::new(weak_workspace.clone(), project, build_panel, window, cx)
    });
    cx.global_mut::<Controllers>()
        .0
        .insert(weak_workspace.entity_id(), controller.downgrade());
    workspace.register_action_renderer(move |element, _, _, cx| {
        if controller.read(cx).import_candidate(cx).is_some() {
            let controller = controller.clone();
            element.on_action(cx.listener(move |_, _: &ImportGradleProject, window, cx| {
                let owner = controller
                    .read(cx)
                    .active
                    .discovery_token(controller.read(cx).project.read(cx).android_context());
                let controller = controller.downgrade();
                // Import reconciliation reads the Workspace; release the action
                // listener's Workspace lease before resolving its selected root.
                window.defer(cx, move |window, cx| {
                    controller
                        .update(cx, |controller, cx| {
                            let result = controller.reconcile(cx).and_then(|()| {
                                ensure!(
                                    owner.as_ref().is_some_and(
                                        |owner| controller.action_is_current(owner, cx)
                                    ),
                                    "Project context changed before Gradle import dispatch"
                                );
                                controller.import(window, cx);
                                Ok(())
                            });
                            if let Err(error) = result {
                                controller.notify_import_error(error, cx);
                            }
                        })
                        .log_err();
                });
            }))
        } else {
            element
        }
    });
}

struct ImportOwner {
    root: RootHandle,
    active: ActiveContextToken,
    discovery: DiscoveryToken,
    session: u64,
}

type Cancellation = Shared<BoxFuture<'static, ()>>;

fn observer_directories(snapshot: &ContextSnapshot) -> Result<Vec<PathBuf>> {
    snapshot.observer_directories()
}

async fn evaluate(
    root: PathBuf,
    executor: BackgroundExecutor,
    output: mpsc::Sender<android_build::OutputLine>,
    cancelled: Cancellation,
) -> Result<CapturedProcessOutput> {
    let adapter = android_tools::project_context::prepare()?;
    let program = if cfg!(windows) {
        root.join("gradlew.bat")
    } else {
        PathBuf::from("/bin/sh")
    };
    let mut command = util::command::new_std_command(program);
    if !cfg!(windows) {
        command.arg("./gradlew");
    }
    command
        .arg("--init-script")
        .arg(adapter.path().join("context.gradle"))
        .arg(format!(":{}", android_tools::project_context::CONTEXT_TASK))
        .args(["--no-configuration-cache", "--console=plain"])
        .current_dir(&root);
    android_build::project_context_output(
        command,
        &executor,
        Duration::from_secs(300),
        output,
        cancelled,
    )
    .await
}

pub(crate) struct ProjectContextController {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    build_panel: Entity<BuildPanel>,
    active: ActiveContext,
    selected_root: Option<WorktreeId>,
    source_worktree: Option<WorktreeId>,
    import_owner: Option<ImportOwner>,
    last_import_session: Option<(RootHandle, u64)>,
    cancel: Option<oneshot::Sender<()>>,
    task: Option<Task<()>>,
    reconcile_scheduled: bool,
    _subscriptions: Vec<Subscription>,
}

impl ProjectContextController {
    #[cfg(test)]
    pub(crate) fn select_fixture_root(
        &mut self,
        root: WorktreeId,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.selected_root = Some(root);
        self.reconcile(cx)
    }

    fn new(
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        build_panel: Entity<BuildPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = Vec::new();
        let workspace_id = workspace.entity_id();
        subscriptions.push(cx.on_release(move |_, cx| {
            if cx.has_global::<Controllers>() {
                cx.global_mut::<Controllers>().0.remove(&workspace_id);
            }
        }));
        if let Some(workspace) = workspace.upgrade() {
            subscriptions.push(cx.subscribe_in(
                &workspace,
                window,
                |this, _, event, window, cx| {
                    if matches!(event, workspace::Event::ActiveItemChanged) {
                        this.clear_active(cx);
                        this.request_reconcile(window, cx);
                    } else if matches!(event, workspace::Event::Activate) {
                        this.request_reconcile(window, cx);
                    }
                },
            ));
        }
        subscriptions.push(
            cx.subscribe_in(&project, window, |this, _, event, window, cx| {
                if matches!(
                    event,
                    project::Event::AndroidProjectContextChanged
                        | project::Event::WorktreeAdded(_)
                        | project::Event::WorktreeRemoved(_)
                        | project::Event::WorktreePathsChanged { .. }
                        | project::Event::WorktreeOrderChanged
                ) {
                    this.request_reconcile(window, cx);
                }
            }),
        );
        let git_store = project.read(cx).git_store().clone();
        subscriptions.push(cx.subscribe_in(
            &git_store,
            window,
            |this, store, event, window, cx| match event {
                GitStoreEvent::ActiveRepositoryChanged(_) if window.is_window_active() => {
                    let directory = store
                        .read(cx)
                        .active_repository()
                        .map(|repository| repository.read(cx).work_directory_abs_path.clone());
                    // The Git store is shared by Project windows. Resolve the
                    // current Workspace after chooser/editor leases have closed.
                    cx.defer_in(window, move |this, window, cx| {
                        let current = Workspace::for_window(window, cx)
                            .or_else(|| window.root::<Workspace>().flatten());
                        if !window.is_window_active()
                            || current.is_none_or(|workspace| {
                                workspace.entity_id() != this.workspace.entity_id()
                            })
                        {
                            return;
                        }
                        this.clear_active(cx);
                        this.selected_root = directory.and_then(|path| {
                            this.project
                                .read(cx)
                                .find_worktree(&path, cx)
                                .map(|(worktree, _)| worktree.read(cx).id())
                        });
                        this.request_reconcile(window, cx);
                    });
                }
                GitStoreEvent::RepositoryUpdated(id, RepositoryEvent::HeadChanged, _) => {
                    if let Some(repository) = store.read(cx).repositories().get(id) {
                        let directory = repository.read(cx).work_directory_abs_path.clone();
                        this.project.update(cx, |project, cx| {
                            project.invalidate_android_context_for_repository(&directory, cx)
                        });
                    }
                    this.request_reconcile(window, cx);
                }
                _ => {}
            },
        ));
        if let Some(trust) = TrustedWorktrees::try_get_global(cx) {
            subscriptions.push(
                cx.subscribe_in(&trust, window, |this, _, event, window, cx| {
                    let (TrustedWorktreesEvent::Trusted(store, _)
                    | TrustedWorktreesEvent::Restricted(store, _)) = event;
                    if *store == this.project.read(cx).worktree_store().downgrade() {
                        if let TrustedWorktreesEvent::Restricted(_, paths) = event {
                            if this.owns_restricted_worktree(paths) {
                                this.cancel_import(cx);
                                cx.notify();
                            }
                        }
                        this.request_reconcile(window, cx);
                    }
                }),
            );
        }
        subscriptions.push(cx.subscribe_in(&build_panel, window, |this, _, event, window, cx| {
            match event {
                BuildEvent::Stop(BuildTab::Sync) if this.import_owner.is_some() => this.cancel_import(cx),
                BuildEvent::Rerun(BuildTab::Sync) if this.owns_sync_session(cx) => {
                    if let Err(error) = this.reconcile(cx) {
                        this.notify_import_error(error, cx);
                    } else if this.last_import_session.is_some_and(|(root, _)| this.active.root() == Some(root)) {
                        this.import(window, cx);
                    } else {
                        this.notify_import_error(anyhow::anyhow!("Select the original trusted Gradle root before rerunning its import"), cx);
                    }
                }
                _ => {}
            }
        }));
        let mut this = Self {
            workspace,
            project,
            build_panel,
            active: ActiveContext::default(),
            selected_root: None,
            source_worktree: None,
            import_owner: None,
            last_import_session: None,
            cancel: None,
            task: None,
            reconcile_scheduled: false,
            _subscriptions: subscriptions,
        };
        // The Workspace is still borrowed by observe_new. Read it only after that
        // callback returns, and coalesce root notifications without polling.
        this.request_reconcile(window, cx);
        this
    }

    fn request_reconcile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reconcile_scheduled {
            return;
        }
        self.reconcile_scheduled = true;
        let controller = cx.entity().downgrade();
        window.defer(cx, move |_, cx| {
            controller
                .update(cx, |controller, cx| {
                    controller.reconcile_scheduled = false;
                    controller.reconcile(cx).log_err();
                })
                .log_err();
        });
    }

    fn clear_active(&mut self, cx: &mut Context<Self>) {
        self.cancel_import(cx);
        self.active.select(None, None).log_err();
        self.source_worktree = None;
        cx.notify();
    }

    fn cancel_import(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.cancel.take() {
            if cancel.send(()).is_err() {
                log::debug!("Gradle context operation already stopped");
            }
        }
        if let Some(owner) = self.import_owner.take() {
            self.project
                .update(cx, |project, cx| {
                    project.finish_failed_android_context_import(&owner.discovery, cx)
                })
                .log_err();
            self.build_panel.update(cx, |panel, cx| {
                panel.finish(
                    BuildTab::Sync,
                    owner.session,
                    BuildStatus::Cancelled,
                    "Gradle import cancelled because its project context changed.".into(),
                    cx,
                )
            });
        }
        self.task = None;
    }

    fn reconcile(&mut self, cx: &mut Context<Self>) -> Result<()> {
        let project = self.project.clone();
        let roots = project
            .read(cx)
            .visible_worktrees(cx)
            .filter(|worktree| !worktree.read(cx).is_single_file())
            .map(|worktree| (worktree.read(cx).id(), worktree.read(cx).abs_path()))
            .collect::<Vec<_>>();
        for (id, _) in &roots {
            let trusted = TrustedWorktrees::try_get_global(cx).is_some_and(|trust| {
                trust.update(cx, |trust, cx| {
                    trust.can_trust(&project.read(cx).worktree_store(), *id, cx)
                })
            });
            project.update(cx, |project, cx| {
                project.ensure_android_context(*id, trusted, cx)
            })?;
        }
        let workspace = self.workspace.upgrade().context("Project window closed")?;
        let item = workspace.read(cx).active_item(cx);
        let path = item.as_ref().and_then(|item| item.project_path(cx));
        let source_worktree = path.as_ref().map(|path| path.worktree_id);
        let (worktree, owner) = if let Some(path) = path {
            let worktree = project
                .read(cx)
                .worktree_for_id(path.worktree_id, cx)
                .context("Active editor root was removed")?;
            if let Some(trust) = TrustedWorktrees::try_get_global(cx) {
                trust.update(cx, |trust, cx| {
                    trust.can_trust(&project.read(cx).worktree_store(), path.worktree_id, cx);
                });
            }
            let owner = worktree.read(cx).abs_path().join(path.path.as_std_path());
            let own_handle = project
                .read(cx)
                .android_context()
                .handle(path.worktree_id.to_proto());
            // An independently opened/evaluated Gradle root keeps its context.
            // Settings/wrapper entries establish only a neutral import boundary,
            // never Android capabilities or an automatic evaluation.
            let independent = own_handle.is_some_and(|handle| {
                project
                    .read(cx)
                    .android_context()
                    .snapshot(handle)
                    .is_some()
            }) || [
                "settings.gradle",
                "settings.gradle.kts",
                "gradlew",
                "gradlew.bat",
            ]
            .iter()
            .any(|name| {
                RelPath::from_unix_str(name)
                    .is_ok_and(|path| worktree.read(cx).entry_for_path(path).is_some())
            });
            let owning_root = if independent {
                Some(path.worktree_id)
            } else {
                let store = project.read(cx).android_context();
                let owners = roots
                    .iter()
                    .filter_map(|(id, _)| {
                        let handle = store.handle(id.to_proto())?;
                        (store.snapshot(handle).is_some_and(|snapshot| {
                            matches!(snapshot.module_owner(&owner), ModuleOwner::Module(_))
                        }) || self.active.retained_external_owner(handle, &owner, store))
                        .then_some(*id)
                    })
                    .collect::<Vec<_>>();
                match owners.as_slice() {
                    [owner] => Some(*owner),
                    [] => Some(path.worktree_id),
                    _ => None,
                }
            };
            (owning_root, owning_root.map(|_| owner))
        } else {
            let selected = self
                .selected_root
                .filter(|id| roots.iter().any(|(root, _)| root == id))
                .or_else(|| {
                    (roots.len() == 1)
                        .then(|| roots.first().map(|(id, _)| *id))
                        .flatten()
                });
            (selected, None)
        };
        let handle =
            worktree.and_then(|id| project.read(cx).android_context().handle(id.to_proto()));
        self.active
            .select_evaluated_owner(handle, owner, project.read(cx).android_context())?;
        self.source_worktree = source_worktree;
        if worktree.is_some() {
            self.selected_root = worktree;
        }
        if self
            .import_owner
            .as_ref()
            .is_some_and(|owner| !self.action_is_current(&owner.active, cx))
        {
            self.cancel_import(cx);
        }
        workspace.update(cx, |_, cx| cx.notify());
        cx.notify();
        Ok(())
    }

    pub(crate) fn root(&self, cx: &App) -> Option<PathBuf> {
        if self.source_is_restricted(cx) {
            return None;
        }
        let store = self.project.read(cx).android_context();
        let handle = self.active.root()?;
        self.active.discovery_token(store)?;
        store.root_path(handle).map(PathBuf::from)
    }

    pub(crate) fn capabilities(
        &self,
        readiness: OperationalReadiness<'_>,
        cx: &App,
    ) -> ContextCapabilities {
        if self.source_is_restricted(cx) {
            return ContextCapabilities::default();
        }
        self.active
            .capabilities(self.project.read(cx).android_context(), readiness)
    }

    pub(crate) fn action_token(&self, cx: &App) -> Option<ActiveContextToken> {
        if self.source_is_restricted(cx) {
            return None;
        }
        self.active.token(self.project.read(cx).android_context())
    }

    pub(crate) fn discovery_token(&self, cx: &App) -> Option<ActiveContextToken> {
        if self.source_is_restricted(cx) {
            return None;
        }
        self.active
            .discovery_token(self.project.read(cx).android_context())
    }

    pub(crate) fn action_is_current(&self, token: &ActiveContextToken, cx: &App) -> bool {
        !self.source_is_restricted(cx)
            && self
                .active
                .is_current(token, self.project.read(cx).android_context())
    }

    fn source_is_restricted(&self, cx: &App) -> bool {
        self.source_worktree.is_some_and(|id| {
            let project = self.project.read(cx);
            let store = project.worktree_store();
            project.worktree_for_id(id, cx).is_none()
                || TrustedWorktrees::try_get_global(cx).is_none_or(|trust| {
                    let trust = trust.read(cx);
                    trust.is_worktree_restricted(&store, id)
                })
        })
    }

    pub(crate) fn owns_restricted_worktree(
        &self,
        paths: &collections::HashSet<project::trusted_worktrees::PathTrust>,
    ) -> bool {
        paths.iter().any(|path| match path {
            project::trusted_worktrees::PathTrust::Worktree(id) => {
                self.source_worktree == Some(*id)
                    || self
                        .active
                        .root()
                        .is_some_and(|root| root.worktree() == id.to_proto())
            }
            project::trusted_worktrees::PathTrust::AbsPath(_) => false,
        })
    }

    pub(crate) fn owns_sync_session(&self, cx: &App) -> bool {
        !self.source_is_restricted(cx)
            && self.last_import_session.is_some_and(|(root, session)| {
                self.active.root() == Some(root)
                    && self
                        .project
                        .read(cx)
                        .android_context()
                        .token(root)
                        .is_some()
                    && self.build_panel.read(cx).session_id(BuildTab::Sync) == Some(session)
            })
    }

    pub(crate) fn import_in_progress(&self, cx: &App) -> bool {
        self.import_owner
            .as_ref()
            .is_some_and(|owner| self.action_is_current(&owner.active, cx))
    }

    pub(crate) fn import_candidate(&self, cx: &App) -> Option<PathBuf> {
        let root = self.root(cx)?;
        (self.project.read(cx).is_local() && android_tools::is_gradle_project(&root))
            .then_some(root)
    }

    fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.import_owner.is_some() {
            return;
        }
        if let Err(error) = self.begin_import(window, cx) {
            self.notify_import_error(error, cx);
        }
    }

    fn notify_import_error(&self, error: anyhow::Error, cx: &mut Context<Self>) {
        log::error!("Gradle import did not start: {error:#}");
        let workspace = self.workspace.clone();
        let message = format!("Gradle import did not start: {error:#}");
        cx.defer(move |cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.show_toast(
                        Toast::new(NotificationId::unique::<ImportGradleProject>(), message)
                            .autohide(),
                        cx,
                    )
                })
                .log_err();
        });
    }

    fn begin_import(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<()> {
        self.reconcile(cx)?;
        let root = self
            .import_candidate(cx)
            .context("Select a trusted Gradle root before importing")?;
        let handle = self.active.root().context("No active project root")?;
        let discovery = self.project.update(cx, |project, cx| {
            project.begin_android_context_import(handle, cx)
        })?;
        let active = self
            .active
            .discovery_token(self.project.read(cx).android_context())
            .context("Gradle import has no current trusted owner")?;
        let (session, output, logs) = self.build_panel.update(cx, |panel, cx| {
            panel.begin(
                BuildTab::Sync,
                format!("Import Gradle project {}", root.display()),
                false,
                window,
                cx,
            )
        });
        self.import_owner = Some(ImportOwner {
            root: handle,
            active: active.clone(),
            discovery: discovery.clone(),
            session,
        });
        self.last_import_session = Some((handle, session));
        let (cancel, cancelled) = oneshot::channel();
        self.cancel = Some(cancel);
        let cancelled = cancelled.map(|_| ()).boxed().shared();
        let executor = cx.background_executor().clone();
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let expected_root = root.clone();
            let result = async {
                let project = this.update(cx, |this, _| this.project.clone())?;
                // Observe the selected root before evaluation. Missing buildSrc
                // and custom output directories can then be tracked without a
                // second evaluation or relying on ignored Worktree entries.
                project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery.clone(), vec![root.clone()], None, cx)).await?;
                this.update(cx, |this, cx| {
                    ensure!(this.action_is_current(&active, cx), "Project context changed before evaluation");
                    Ok::<_, anyhow::Error>(())
                })??;
                let first = cx.background_spawn(evaluate(root.clone(), executor.clone(), output.clone(), cancelled.clone())).await?;
                let mut result = first;
                if let CapturedProcessOutput::Completed { stdout, status } = &result {
                    if status.success() {
                        let snapshots = decode_context_output(stdout, &root)?;
                        let snapshot = snapshots.last().context("Gradle context has no final observation")?;
                        ensure!(snapshot.phase() == ObservationPhase::Complete, "Gradle context evaluation did not complete");
                        let project = this.update(cx, |this, cx| {
                            ensure!(this.action_is_current(&active, cx), "Project context changed before observer installation");
                            this.project.update(cx, |project, _| project.verify_android_context_inputs(&discovery, snapshot))?;
                            Ok::<_, anyhow::Error>(this.project.clone())
                        })??;
                        let directories = observer_directories(snapshot)?;
                        let filesystem = project.read_with(cx, |project, _| project.fs().clone());
                        let directories = cx.background_spawn(async move {
                            let mut existing = Vec::new();
                            for directory in directories {
                                if filesystem.metadata(&directory).await?.is_some_and(|metadata| metadata.is_dir) { existing.push(directory); }
                            }
                            Ok::<_, anyhow::Error>(existing)
                        }).await?;
                        let observer_task = project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery.clone(), directories, Some(snapshot.clone()), cx));
                        let observers_added = observer_task.await?;
                        this.update(cx, |this, cx| {
                            ensure!(this.action_is_current(&active, cx), "Project context changed while establishing observers");
                            Ok::<_, anyhow::Error>(())
                        })??;
                        // First observations only establish provenance. Newly owned
                        // watchers require one bounded verification evaluation; no
                        // capabilities are published between the two evaluations.
                        if observers_added {
                            result = cx.background_spawn(evaluate(root.clone(), executor, output.clone(), cancelled)).await?;
                        }
                    }
                }
                if let CapturedProcessOutput::Completed { stdout, status } = &result {
                    if status.success() {
                        let snapshots = decode_context_output(stdout, &root)?;
                        let snapshot = snapshots.last().context("Gradle verification emitted no context")?;
                        let mut directories = observer_directories(snapshot)?;
                        directories.push(root.clone());
                        directories.sort();
                        directories.dedup();
                        project.update(cx, |project, cx| project.verify_android_context_observers(handle, discovery.clone(), directories, cx)).await?;
                    }
                }
                Ok::<_, anyhow::Error>(result)
            }.await;
            drop(output);
            logs.await;
            this.update_in(cx, |this, _, cx| {
                let Some(owner) = &this.import_owner else { return; };
                if owner.session != session || owner.root != handle { return; }
                let result = result.and_then(|result| {
                    ensure!(this.action_is_current(&active, cx), "Discarded stale Gradle import result");
                    match result {
                        CapturedProcessOutput::Cancelled => Ok(None),
                        CapturedProcessOutput::Completed { stdout, status } => {
                            let snapshots = decode_context_output(&stdout, &expected_root)?;
                            let complete = snapshots.last().is_some_and(|snapshot| snapshot.phase() == ObservationPhase::Complete);
                            // A failing command cannot publish operational capabilities,
                            // even if a plugin printed a complete record before failing.
                            ensure!(!complete || status.success(), "Gradle failed after context evaluation ({status})");
                            for snapshot in snapshots {
                                this.project.update(cx, |project, cx| project.publish_android_context(&this.active, &active, &discovery, snapshot, cx))?;
                            }
                            if !status.success() {
                                anyhow::bail!("Gradle import failed ({status}); evaluated plugin observations were retained for an explicit retry.");
                            }
                            ensure!(complete, "Gradle did not complete project context evaluation");
                            Ok(Some(()))
                        }
                    }
                });
                if !matches!(result, Ok(Some(()))) {
                    this.project.update(cx, |project, cx| project.finish_failed_android_context_import(&discovery, cx)).log_err();
                }
                let (status, message) = match result {
                    Ok(Some(())) => (BuildStatus::Succeeded, "Gradle project imported.".to_owned()),
                    Ok(None) => (BuildStatus::Cancelled, "Gradle import cancelled.".to_owned()),
                    Err(error) => (BuildStatus::Failed, format!("{error:#}")),
                };
                this.build_panel.update(cx, |panel, cx| panel.finish(BuildTab::Sync, session, status, message, cx));
                this.import_owner = None;
                this.cancel = None;
                this.task = None;
                cx.notify();
            }).log_err();
        }));
        cx.notify();
        Ok(())
    }
}
