use crate::android_build::{
    self, BuildEvent, BuildPanel, BuildStatus, BuildTab, CapturedProcessOutput,
};
use android_tools::project_context::{
    ActiveContext, ActiveContextToken, ActiveProjectToken, ContextCapabilities, ContextSnapshot,
    DiscoveryToken, ModuleOwner, ObservationPhase, OperationalReadiness, RootHandle,
    decode_context_output,
};
use anyhow::{Context as _, Result, ensure};
use futures::{
    FutureExt as _,
    channel::{mpsc, oneshot},
    future::{BoxFuture, Shared},
};
use gpui::{
    App, AppContext as _, BackgroundExecutor, Context, Entity, EntityId, Global,
    InteractiveElement as _, Subscription, Task, WeakEntity, Window, actions,
};
use project::{
    Project, WorktreeId,
    git_store::{GitStoreEvent, RepositoryEvent},
    trusted_worktrees::{TrustedWorktrees, TrustedWorktreesEvent},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};
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
    use gpui::TestAppContext;
    use project::Fs as _;
    use project::trusted_worktrees::{self, PathTrust};
    use serde_json::json;
    use workspace::AppState;

    #[gpui::test]
    async fn import_availability_tracks_cached_root_files_without_qualifying_generic_projects(
        cx: &mut TestAppContext,
    ) {
        import_availability_tracks_cached_root_files_without_qualifying_generic_projects_case(cx)
            .await
            .expect("Android project-context fixture must complete successfully");
    }

    async fn import_availability_tracks_cached_root_files_without_qualifying_generic_projects_case(
        cx: &mut TestAppContext,
    ) -> Result<()> {
        cx.update(|cx| {
            let state = AppState::test(cx);
            editor::init(cx);
            workspace::init(state, cx);
            trusted_worktrees::init(Default::default(), cx);
        });
        let filesystem = project::FakeFs::new(cx.executor());
        filesystem
            .insert_tree("/cached-import-owner", json!({"main.py":"print(1)"}))
            .await;
        let project = Project::test_with_worktree_trust(
            filesystem.clone(),
            [Path::new("/cached-import-owner")],
            cx,
        )
        .await;
        cx.update(|cx| crate::project_surfaces::tests::trust(&project, cx))?;
        let (workspace, visual) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let build = visual.new(|cx| BuildPanel::new(workspace.downgrade(), cx));
        workspace.update_in(visual, |workspace, window, cx| {
            register(workspace, build, window, cx);
        });
        workspace
            .update_in(visual, |workspace, window, cx| {
                workspace.open_abs_path(
                    Path::new("/cached-import-owner/main.py").to_path_buf(),
                    Default::default(),
                    window,
                    cx,
                )
            })
            .await?;
        visual.run_until_parked();
        let controller = visual
            .update(|_, cx| for_workspace(&workspace.downgrade(), cx))
            .context("Controller")?;
        assert!(controller.read_with(visual, |controller, cx| {
            controller.import_candidate(cx).is_none()
        }));
        filesystem
            .write(Path::new("/cached-import-owner/gradlew"), b"wrapper")
            .await?;
        visual.run_until_parked();
        assert!(controller.read_with(visual, |controller, cx| {
            controller.import_candidate(cx).is_none()
        }));
        filesystem
            .write(
                Path::new("/cached-import-owner/settings.gradle.kts"),
                b"rootProject.name = \"plain\"",
            )
            .await?;
        visual.run_until_parked();
        controller.read_with(visual, |controller, cx| {
            assert_eq!(controller.import_candidate(cx), Some(PathBuf::from("/cached-import-owner")));
            assert_eq!(controller.capabilities(Default::default(), cx), Default::default(),
                "Wrapper/build entries only enable neutral explicit import; they cannot establish Android or multiplatform facts");
        });
        filesystem
            .remove_file(
                Path::new("/cached-import-owner/gradlew"),
                Default::default(),
            )
            .await?;
        filesystem
            .create_dir(Path::new("/cached-import-owner/gradlew"))
            .await?;
        visual.run_until_parked();
        assert!(
            controller.read_with(visual, |controller, cx| controller
                .import_candidate(cx)
                .is_none()),
            "A cached directory named gradlew is not a wrapper file"
        );
        filesystem
            .write(Path::new("/cached-import-owner/gradlew.bat"), b"wrapper")
            .await?;
        visual.run_until_parked();
        assert!(controller.read_with(visual, |controller, cx| {
            controller.import_candidate(cx).is_some()
        }));
        filesystem
            .remove_file(
                Path::new("/cached-import-owner/settings.gradle.kts"),
                Default::default(),
            )
            .await?;
        visual.run_until_parked();
        assert!(controller.read_with(visual, |controller, cx| {
            controller.import_candidate(cx).is_none()
        }));
        Ok(())
    }

    #[gpui::test]
    async fn nested_repository_selection_preserves_source_and_import_owners_but_rapid_roots_do_not(
        cx: &mut TestAppContext,
    ) {
        nested_repository_selection_preserves_source_and_import_owners_but_rapid_roots_do_not_case(
            cx,
        )
        .await
        .expect("Android project-context fixture must complete successfully");
    }

    async fn nested_repository_selection_preserves_source_and_import_owners_but_rapid_roots_do_not_case(
        cx: &mut TestAppContext,
    ) -> Result<()> {
        cx.update(|cx| {
            let state = AppState::test(cx);
            editor::init(cx);
            workspace::init(state, cx);
            trusted_worktrees::init(Default::default(), cx);
        });
        let filesystem = project::FakeFs::new(cx.executor());
        filesystem
            .insert_tree(
                "/repo-owner",
                json!({".git":{},"Main.kt":"class Main",
            "nested":{".git":{},"Other.kt":"class Other"}}),
            )
            .await;
        filesystem
            .insert_tree("/repo-other", json!({".git":{},"main.py":"print(1)"}))
            .await;
        for git in [
            "/repo-owner/.git",
            "/repo-owner/nested/.git",
            "/repo-other/.git",
        ] {
            filesystem.set_branch_name(Path::new(git), Some("main"));
        }
        let project = Project::test_with_worktree_trust(
            filesystem,
            [Path::new("/repo-owner"), Path::new("/repo-other")],
            cx,
        )
        .await;
        project
            .update(cx, |project, cx| project.git_scans_complete(cx))
            .await;
        cx.update(|cx| {
            crate::project_surfaces::tests::trust(&project, cx)?;
            crate::project_surfaces::tests::publish_catalogue(
                &project,
                Path::new("/repo-owner"),
                &[android_tools::project_context::PluginId::AndroidApplication],
                &[("android", "androidJvm")],
                true,
                cx,
            )
        })?;
        let (workspace, visual) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let build = visual.new(|cx| BuildPanel::new(workspace.downgrade(), cx));
        workspace.update_in(visual, |workspace, window, cx| {
            register(workspace, build, window, cx);
        });
        visual.update(|window, _| window.activate_window());
        workspace
            .update_in(visual, |workspace, window, cx| {
                workspace.open_abs_path(
                    Path::new("/repo-owner/Main.kt").to_path_buf(),
                    Default::default(),
                    window,
                    cx,
                )
            })
            .await?;
        visual.run_until_parked();
        let controller = visual
            .update(|_, cx| for_workspace(&workspace.downgrade(), cx))
            .context("Controller")?;
        let (source, owner) = controller.read_with(visual, |controller, cx| {
            Ok::<_, anyhow::Error>((
                controller.action_token(cx).context("Source token")?,
                controller.project_token(cx).context("Project token")?,
            ))
        })?;
        let nested_path = project.read_with(visual, |project, cx| {
            project
                .find_project_path("/repo-owner/nested/Other.kt", cx)
                .context("Nested path")
        })?;
        let git = project.read_with(visual, |project, _cx| project.git_store().clone());
        let previous_repository = git.read_with(visual, |git, cx| {
            git.active_repository()
                .context("Root repo")
                .map(|repo| repo.read(cx).id)
        })?;
        git.update(visual, |git, cx| {
            git.set_active_repo_for_path(&nested_path, cx)
        });
        visual.run_until_parked();
        git.read_with(visual, |git, cx| {
            let repository = git.active_repository().expect("Nested repository").read(cx);
            assert_ne!(
                repository.id, previous_repository,
                "The real active-repository event must change repositories"
            );
            assert_eq!(
                repository.work_directory_abs_path.as_ref(),
                Path::new("/repo-owner/nested")
            );
        });
        controller.read_with(visual, |controller, cx| {
            assert!(
                controller.action_is_current(&source, cx),
                "A repository-only chooser change must retain the current source"
            );
            assert!(controller.project_is_current(&owner, cx));
        });
        workspace
            .update_in(visual, |workspace, window, cx| {
                workspace.open_abs_path(
                    Path::new("/repo-owner/nested/Other.kt").to_path_buf(),
                    Default::default(),
                    window,
                    cx,
                )
            })
            .await?;
        visual.run_until_parked();
        controller.read_with(visual, |controller, cx| {
            assert!(
                !controller.action_is_current(&source, cx),
                "An actual file switch invalidates source work"
            );
            assert!(controller.project_is_current(&owner, cx));
        });
        let (mut cancelled, importing) = controller.update(visual, |controller, cx| {
            let root = controller.active.root().context("Import root")?;
            let discovery = project.update(cx, |project, cx| {
                project.begin_android_context_import(root, cx)
            })?;
            let active = controller
                .active
                .project_discovery_token(project.read(cx).android_context())
                .context("Import owner")?;
            let (cancel, cancelled) = oneshot::channel();
            controller.import_owner = Some(ImportOwner {
                root,
                active: active.clone(),
                discovery,
                session: 1,
            });
            controller.cancel = Some(cancel);
            controller.task = Some(Task::ready(()));
            Ok::<_, anyhow::Error>((cancelled, active))
        })?;
        let (root_id, other_id) = project.read_with(visual, |project, cx| {
            Ok::<_, anyhow::Error>((
                project
                    .find_worktree(Path::new("/repo-owner"), cx)
                    .context("Owner root")?
                    .0
                    .read(cx)
                    .id(),
                project
                    .find_worktree(Path::new("/repo-other"), cx)
                    .context("Other root")?
                    .0
                    .read(cx)
                    .id(),
            ))
        })?;
        git.update(visual, |git, cx| {
            git.set_active_repo_for_worktree(root_id, cx)
        });
        visual.run_until_parked();
        assert_eq!(cancelled.try_recv()?, None);
        controller.read_with(visual, |controller, cx| {
            assert!(controller.import_in_progress(cx));
            assert!(controller.project_is_current(&importing, cx));
        });
        // Both queued Git events are emitted before pumping; reading only the
        // store's final active repository would silently collapse this B/A pair.
        git.update(visual, |git, cx| {
            git.set_active_repo_for_worktree(other_id, cx);
            git.set_active_repo_for_worktree(root_id, cx);
        });
        visual.run_until_parked();
        assert_eq!(cancelled.try_recv()?, Some(()));
        controller.read_with(visual, |controller, cx| {
            assert_eq!(controller.root(cx), Some(PathBuf::from("/repo-owner")));
            assert!(!controller.project_is_current(&importing, cx));
            assert!(controller.import_owner.is_none() && controller.task.is_none());
        });
        Ok(())
    }

    #[gpui::test]
    async fn queued_import_keeps_its_project_across_files_and_cancels_rapid_root_switches(
        cx: &mut TestAppContext,
    ) {
        queued_import_keeps_its_project_across_files_and_cancels_rapid_root_switches_case(cx)
            .await
            .expect("Android project-context fixture must complete successfully");
    }

    async fn queued_import_keeps_its_project_across_files_and_cancels_rapid_root_switches_case(
        cx: &mut TestAppContext,
    ) -> Result<()> {
        use std::path::Path;
        cx.update(|cx| {
            let state = AppState::test(cx);
            editor::init(cx);
            workspace::init(state, cx);
            trusted_worktrees::init(Default::default(), cx);
        });
        let filesystem = project::FakeFs::new(cx.executor());
        filesystem
            .insert_tree(
                "/import-owner",
                json!({"Main.kt":"fun main() {}", "Other.kt":"fun other() {}"}),
            )
            .await;
        filesystem
            .insert_tree("/import-python", json!({"main.py":"print(1)"}))
            .await;
        let project = Project::test_with_worktree_trust(
            filesystem,
            [Path::new("/import-owner"), Path::new("/import-python")],
            cx,
        )
        .await;
        cx.update(|cx| {
            crate::project_surfaces::tests::trust(&project, cx)?;
            crate::project_surfaces::tests::publish_catalogue(
                &project,
                Path::new("/import-owner"),
                &[android_tools::project_context::PluginId::AndroidApplication],
                &[("android", "androidJvm")],
                true,
                cx,
            )
        })?;
        let (workspace, visual) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let build_panel = visual.new(|cx| BuildPanel::new(workspace.downgrade(), cx));
        workspace.update_in(visual, |workspace, window, cx| {
            register(workspace, build_panel, window, cx)
        });
        workspace
            .update_in(visual, |workspace, window, cx| {
                workspace.open_abs_path(
                    Path::new("/import-python/main.py").to_path_buf(),
                    Default::default(),
                    window,
                    cx,
                )
            })
            .await?;
        visual.run_until_parked();
        let python = workspace.read_with(visual, |workspace, cx| {
            workspace.active_item(cx).context("Python item")
        })?;
        workspace
            .update_in(visual, |workspace, window, cx| {
                workspace.open_abs_path(
                    Path::new("/import-owner/Main.kt").to_path_buf(),
                    Default::default(),
                    window,
                    cx,
                )
            })
            .await?;
        visual.run_until_parked();
        let controller = visual
            .update(|_, cx| for_workspace(&workspace.downgrade(), cx))
            .context("Controller")?;
        let mut cancelled = controller.update(visual, |controller, cx| {
            let root = controller.active.root().context("Import root")?;
            let discovery = controller.project.update(cx, |project, cx| {
                project.begin_android_context_import(root, cx)
            })?;
            let active = controller
                .active
                .project_discovery_token(controller.project.read(cx).android_context())
                .context("Project discovery owner")?;
            let (cancel, cancelled) = oneshot::channel();
            controller.import_owner = Some(ImportOwner {
                root,
                active,
                discovery,
                session: 1,
            });
            controller.cancel = Some(cancel);
            controller.task = Some(Task::ready(()));
            Ok::<_, anyhow::Error>(cancelled)
        })?;
        workspace
            .update_in(visual, |workspace, window, cx| {
                workspace.open_abs_path(
                    Path::new("/import-owner/Other.kt").to_path_buf(),
                    Default::default(),
                    window,
                    cx,
                )
            })
            .await?;
        visual.run_until_parked();
        assert!(
            cancelled.try_recv()?.is_none(),
            "Same-root source changes retain a queued Gradle import"
        );
        controller.read_with(visual, |controller, cx| {
            assert!(controller.import_in_progress(cx));
            assert!(controller.task.is_some());
        });
        let android = workspace.read_with(visual, |workspace, cx| {
            workspace.active_item(cx).context("Android item")
        })?;
        workspace.update_in(visual, |workspace, window, cx| {
            assert!(workspace.activate_item(python.as_ref(), false, false, window, cx));
            assert!(workspace.activate_item(android.as_ref(), false, false, window, cx));
        });
        visual.run_until_parked();
        assert_eq!(
            cancelled.try_recv()?,
            Some(()),
            "Rapid A/B/A cancels the original import"
        );
        controller.read_with(visual, |controller, cx| {
            assert!(
                !controller.import_in_progress(cx)
                    && controller.import_owner.is_none()
                    && controller.task.is_none()
            );
        });
        Ok(())
    }

    #[gpui::test]
    async fn exact_source_restriction_is_visible_with_other_restricted_directory_roots(
        cx: &mut TestAppContext,
    ) {
        exact_source_restriction_is_visible_with_other_restricted_directory_roots_case(cx)
            .await
            .expect("Android project-context fixture must complete successfully");
    }

    async fn exact_source_restriction_is_visible_with_other_restricted_directory_roots_case(
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
    ) {
        untitled_editor_retains_selected_project_without_borrowing_ambiguous_root_case(cx)
            .await
            .expect("Android project-context fixture must complete successfully");
    }

    async fn untitled_editor_retains_selected_project_without_borrowing_ambiguous_root_case(
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
                    std::path::Path::new("/scratch-android/Main.kt").to_path_buf(),
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
    async fn deferred_neutral_import_survives_same_root_files_and_rejects_captured_rapid_roots(
        cx: &mut TestAppContext,
    ) {
        async {
            cx.executor().allow_parking();
            cx.update(|cx| {
                let state = AppState::test(cx);
                editor::init(cx);
                workspace::init(state, cx);
                trusted_worktrees::init(Default::default(), cx);
            });
            let directory = tempfile::TempDir::new()?;
            let root = directory.path().join("owner");
            let other_root = directory.path().join("other");
            std::fs::create_dir(&root)?;
            std::fs::create_dir(&other_root)?;
            let payload = json!({"schema":1,"root":&root,"gradleVersion":"9.6.1","phase":"complete",
                "modules":[{"path":":","directory":&root,
                    "plugins":android_tools::project_context::PluginId::ALL.map(|plugin| json!({"plugin":plugin,"applied":false})),
                    "targets":{"status":"available","value":[]}}]});
            let record = format!("{}{}", android_tools::project_context::CONTEXT_OUTPUT_PREFIX, serde_json::to_string(&payload)?);
            // Exercise the real import command/observer transport without host Gradle.
            std::fs::write(root.join("gradlew"), format!("printf '%s\\n' '{}'\n", record.replace('\'', "'\\''")))?;
            std::fs::write(root.join("build.gradle"), "")?;
            let filesystem = project::FakeFs::new(cx.executor());
            filesystem.insert_tree(&root, json!({"gradlew":"", "build.gradle":"", "Main.kt":"class Main", "Other.kt":"class Other"})).await;
            filesystem.insert_tree(&other_root, json!({"main.py":"print(1)"})).await;
            let project = Project::test_with_worktree_trust(filesystem, [root.as_path(), other_root.as_path()], cx).await;
            cx.update(|cx| crate::project_surfaces::tests::trust(&project, cx))?;
            let (workspace, visual) =
                cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
            let build = visual.new(|cx| BuildPanel::new(workspace.downgrade(), cx));
            workspace.update_in(visual, |workspace, window, cx| {
                workspace.add_panel(build.clone(), window, cx);
                register(workspace, build.clone(), window, cx);
            });
            let mut items = Vec::new();
            for path in [root.join("Other.kt"), other_root.join("main.py"), root.join("Main.kt")] {
                workspace.update_in(visual, |workspace, window, cx| workspace.open_abs_path(path, Default::default(), window, cx)).await?;
                visual.run_until_parked();
                items.push(workspace.read_with(visual, |workspace, cx| workspace.active_item(cx).expect("Fixture item")));
            }
            let controller = visual.update(|_, cx| for_workspace(&workspace.downgrade(), cx)).context("Controller")?;
            let handle = project.read_with(visual, |project, cx| {
                let id = project.find_worktree(&root, cx).context("Owner worktree")?.0.read(cx).id();
                project.android_context().handle(id.to_proto()).context("Owner handle")
            })?;
            assert!(project.read_with(visual, |project, _| project.android_context().snapshot(handle).is_none()));
            assert!(build.read_with(visual, |build, _| build.session_id(BuildTab::Sync).is_none()));
            workspace.update_in(visual, |workspace, window, cx| {
                assert!(workspace.activate_item(items[0].as_ref(), false, false, window, cx));
                cx.emit(workspace::Event::ActiveProjectPathChanged(workspace.active_item(cx).and_then(|item| item.project_path(cx))));
                defer_import(&controller, window, cx);
            });
            visual.run_until_parked();
            assert!(build.read_with(visual, |build, _| build.session_id(BuildTab::Sync).is_some()),
                "Neutral import must begin for a same-root source transition before its deferred callback");
            visual.condition(&project, |project, _| project.android_context().snapshot(handle).is_some()).await;
            let session = build.read_with(visual, |build, _| build.session_id(BuildTab::Sync));
            controller.read_with(visual, |controller, cx| {
                assert_eq!(controller.root(cx), Some(root.clone()));
                assert_eq!(controller.capabilities(Default::default(), cx), ContextCapabilities::default());
                assert!(controller.import_owner.is_none() && controller.task.is_none());
            });
            workspace.update_in(visual, |workspace, window, cx| {
                assert!(workspace.activate_item(items[1].as_ref(), false, false, window, cx));
                cx.emit(workspace::Event::ActiveProjectPathChanged(workspace.active_item(cx).and_then(|item| item.project_path(cx))));
                assert!(workspace.activate_item(items[2].as_ref(), false, false, window, cx));
                cx.emit(workspace::Event::ActiveProjectPathChanged(workspace.active_item(cx).and_then(|item| item.project_path(cx))));
                defer_import(&controller, window, cx);
            });
            visual.run_until_parked();
            assert_eq!(build.read_with(visual, |build, _| build.session_id(BuildTab::Sync)), session,
                "A captured root A/B/A must not start a new import session");
            controller.read_with(visual, |controller, cx| {
                assert_eq!(controller.root(cx), Some(root));
                assert!(controller.import_owner.is_none() && controller.task.is_none());
            });
            Ok::<_, anyhow::Error>(())
        }.await.expect("Deferred UX regression must reach every assertion");
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn deferred_import_action_enters_current_workspace_without_reentry(
        cx: &mut TestAppContext,
    ) {
        deferred_import_action_enters_current_workspace_without_reentry_case(cx)
            .await
            .expect("Android project-context fixture must complete successfully");
    }

    #[cfg(unix)]
    async fn deferred_import_action_enters_current_workspace_without_reentry_case(
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
        std::fs::write(root.join("build.gradle"), "")?;
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
                defer_import(&controller, window, cx);
            }))
        } else {
            element
        }
    });
}

fn defer_import(controller: &Entity<ProjectContextController>, window: &mut Window, cx: &mut App) {
    let owner = controller
        .read(cx)
        .active
        .project_discovery_token(controller.read(cx).project.read(cx).android_context());
    let controller = controller.downgrade();
    // Import reconciliation reads the Workspace; release the action
    // listener's Workspace lease before resolving its selected root.
    window.defer(cx, move |window, cx| {
        controller
            .update(cx, |controller, cx| {
                let result = controller.reconcile(cx).and_then(|()| {
                    ensure!(
                        owner
                            .as_ref()
                            .is_some_and(|owner| controller.project_is_current(owner, cx)),
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
}

struct ImportOwner {
    root: RootHandle,
    active: ActiveProjectToken,
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
    ensure!(
        android_tools::is_gradle_project(&root),
        "The selected project no longer has a Gradle wrapper and build definition"
    );
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
    manual_model_sync: Option<ActiveProjectToken>,
    cancel: Option<oneshot::Sender<()>>,
    task: Option<Task<()>>,
    reconcile_scheduled: bool,
    _subscriptions: Vec<Subscription>,
}

impl ProjectContextController {
    #[cfg(test)]
    pub(crate) fn import_owner_is_finished_for_test(&self) -> bool {
        self.import_owner.is_none() && self.task.is_none()
    }

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
                    if let workspace::Event::ActiveProjectPathChanged(path) = event {
                        this.active.invalidate_source_selection().log_err();
                        this.select_path(path.clone(), cx).log_err();
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
                } else if let project::Event::WorktreeUpdatedEntries(id, entries) = event {
                    let project = this.project.read(cx);
                    let is_root = project.worktree_for_id(*id, cx).is_some_and(|worktree| {
                        this.root(cx).as_deref() == Some(worktree.read(cx).abs_path().as_ref())
                    });
                    if is_root
                        && entries.iter().any(|(path, _, _)| {
                            matches!(
                                path.as_unix_str(),
                                "gradlew"
                                    | "gradlew.bat"
                                    | "settings.gradle"
                                    | "settings.gradle.kts"
                                    | "build.gradle"
                                    | "build.gradle.kts"
                            )
                        })
                    {
                        this.request_reconcile(window, cx);
                    }
                }
            }),
        );
        let git_store = project.read(cx).git_store().clone();
        subscriptions.push(cx.subscribe_in(
            &git_store,
            window,
            |this, store, event, window, cx| match event {
                GitStoreEvent::ActiveRepositoryChanged(id) if window.is_window_active() => {
                    let directory = id
                        .and_then(|id| store.read(cx).repositories().get(&id))
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
                        if let Some(directory) = directory {
                            this.select_repository_directory(&directory, cx).log_err();
                        }
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
            manual_model_sync: None,
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

    fn cancel_import(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.cancel.take() {
            if cancel.send(()).is_err() {
                log::debug!("Gradle context operation already stopped");
            }
        }
        if let Some(owner) = self.import_owner.take() {
            if self.manual_model_sync.as_ref() == Some(&owner.active) {
                self.manual_model_sync = None;
            }
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
        self.select_path(path, cx)?;
        workspace.update(cx, |_, cx| cx.notify());
        cx.notify();
        Ok(())
    }

    fn resolve_path_owner(
        &self,
        path: &project::ProjectPath,
        cx: &App,
    ) -> Result<(Option<WorktreeId>, PathBuf)> {
        let project = self.project.clone();
        let worktree = project
            .read(cx)
            .worktree_for_id(path.worktree_id, cx)
            .context("Active editor root was removed")?;
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
            let owners = project
                .read(cx)
                .visible_worktrees(cx)
                .filter(|worktree| !worktree.read(cx).is_single_file())
                .filter_map(|worktree| {
                    let id = worktree.read(cx).id();
                    let handle = store.handle(id.to_proto())?;
                    (store.snapshot(handle).is_some_and(|snapshot| {
                        matches!(snapshot.module_owner(&owner), ModuleOwner::Module(_))
                    }) || self.active.retained_external_owner(handle, &owner, store))
                    .then_some(id)
                })
                .collect::<Vec<_>>();
            match owners.as_slice() {
                [owner] => Some(*owner),
                [] => Some(path.worktree_id),
                _ => None,
            }
        };
        Ok((owning_root, owner))
    }

    fn select_repository_directory(
        &mut self,
        directory: &Path,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let project = self.project.clone();
        let (worktree, path) = project
            .read(cx)
            .find_worktree(directory, cx)
            .context("Selected repository is outside this project")?;
        let path = project::ProjectPath {
            worktree_id: worktree.read(cx).id(),
            path,
        };
        let (worktree, _) = self.resolve_path_owner(&path, cx)?;
        let handle =
            worktree.and_then(|id| project.read(cx).android_context().handle(id.to_proto()));
        self.selected_root = worktree;
        if handle != self.active.root() {
            // Process each captured repository transition, even when a later
            // chooser event returns to the original root before reconciliation.
            self.active
                .select_evaluated_owner(handle, None, project.read(cx).android_context())?;
            self.source_worktree = None;
            if self
                .import_owner
                .as_ref()
                .is_some_and(|owner| !self.project_is_current(&owner.active, cx))
            {
                self.cancel_import(cx);
            }
            cx.notify();
        }
        Ok(())
    }

    fn select_path(
        &mut self,
        path: Option<project::ProjectPath>,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let project = self.project.clone();
        let roots = project
            .read(cx)
            .visible_worktrees(cx)
            .filter(|worktree| !worktree.read(cx).is_single_file())
            .map(|worktree| (worktree.read(cx).id(), worktree.read(cx).abs_path()))
            .collect::<Vec<_>>();
        let source_worktree = path.as_ref().map(|path| path.worktree_id);
        let (worktree, owner) = if let Some(path) = path {
            let (owning_root, owner) = self.resolve_path_owner(&path, cx)?;
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
            .is_some_and(|owner| !self.project_is_current(&owner.active, cx))
        {
            self.cancel_import(cx);
        }
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

    pub(crate) fn project_token(&self, cx: &App) -> Option<ActiveProjectToken> {
        if self.source_is_restricted(cx) {
            return None;
        }
        self.active
            .project_token(self.project.read(cx).android_context())
    }

    pub(crate) fn project_is_current(&self, token: &ActiveProjectToken, cx: &App) -> bool {
        !self.source_is_restricted(cx)
            && self
                .active
                .project_is_current(token, self.project.read(cx).android_context())
    }

    #[cfg(test)]
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

    #[cfg(test)]
    pub(crate) fn import_in_progress(&self, cx: &App) -> bool {
        self.import_owner
            .as_ref()
            .is_some_and(|owner| self.project_is_current(&owner.active, cx))
    }

    pub(crate) fn import_candidate(&self, cx: &App) -> Option<PathBuf> {
        let root = self.root(cx)?;
        let project = self.project.read(cx);
        if !project.is_local() {
            return None;
        }
        let (worktree, _) = project.find_worktree(&root, cx)?;
        let worktree = worktree.read(cx);
        if worktree.abs_path().as_ref() != root.as_path() {
            return None;
        }
        let is_file = |name| {
            RelPath::from_unix_str(name).is_ok_and(|path| {
                worktree
                    .entry_for_path(path)
                    .is_some_and(|entry| entry.is_file())
            })
        };
        ((is_file("gradlew") || is_file("gradlew.bat"))
            && [
                "settings.gradle.kts",
                "settings.gradle",
                "build.gradle.kts",
                "build.gradle",
            ]
            .into_iter()
            .any(is_file))
        .then_some(root)
    }

    pub(crate) fn retry_partial_android_import(
        &mut self,
        owner: &ActiveProjectToken,
        root: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.reconcile(cx)?;
        ensure!(
            self.project_is_current(owner, cx) && self.root(cx).as_deref() == Some(root),
            "The Android project changed before retrying its partial import"
        );
        ensure!(
            self.active
                .root()
                .and_then(|handle| self.project.read(cx).android_context().snapshot(handle))
                .is_some_and(|snapshot| snapshot.phase() == ObservationPhase::Partial),
            "The Android import no longer has partial facts to retry"
        );
        if self.import_owner.is_none() {
            self.begin_import(window, cx)?;
        }
        // begin_import intentionally expires the pre-import RootToken. Transfer
        // the verified explicit request to this import's fresh discovery owner.
        self.manual_model_sync = Some(
            self.import_owner
                .as_ref()
                .context("Partial retry has no import owner")?
                .active
                .clone(),
        );
        cx.notify();
        Ok(())
    }

    pub(crate) fn manual_model_sync_pending(&self, cx: &App) -> bool {
        self.manual_model_sync
            .as_ref()
            .is_some_and(|owner| self.project_is_current(owner, cx))
    }

    fn take_manual_model_sync(
        &mut self,
        owner: &ActiveProjectToken,
        root: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<WeakEntity<Workspace>> {
        // Never let a queued old callback consume a newer project's request.
        if self.manual_model_sync.as_ref() != Some(owner) {
            return None;
        }
        self.manual_model_sync = None;
        let complete = self
            .active
            .root()
            .and_then(|handle| self.project.read(cx).android_context().snapshot(handle))
            .is_some_and(|snapshot| snapshot.phase() == ObservationPhase::Complete);
        if !complete
            || !self.project_is_current(owner, cx)
            || self.project_token(cx).as_ref() != Some(owner)
            || self.root(cx).as_deref() != Some(root)
        {
            cx.notify();
            return None;
        }
        let current =
            Workspace::for_window(window, cx).or_else(|| window.root::<Workspace>().flatten());
        if current.as_ref().map(|workspace| workspace.entity_id())
            != Some(self.workspace.entity_id())
        {
            cx.notify();
            return None;
        }
        cx.notify();
        Some(self.workspace.clone())
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
            .project_discovery_token(self.project.read(cx).android_context())
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
                    ensure!(this.project_is_current(&active, cx), "Project context changed before evaluation");
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
                            ensure!(this.project_is_current(&active, cx), "Project context changed before observer installation");
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
                            ensure!(this.project_is_current(&active, cx), "Project context changed while establishing observers");
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
            this.update_in(cx, |this, window, cx| {
                let Some(owner) = &this.import_owner else { return; };
                if owner.session != session || owner.root != handle { return; }
                let result = result.and_then(|result| {
                    ensure!(this.project_is_current(&active, cx), "Discarded stale Gradle import result");
                    match result {
                        CapturedProcessOutput::Cancelled => Ok(None),
                        CapturedProcessOutput::Completed { stdout, status } => {
                            let snapshots = decode_context_output(&stdout, &expected_root)?;
                            let complete = snapshots.last().is_some_and(|snapshot| snapshot.phase() == ObservationPhase::Complete);
                            // A failing command cannot publish operational capabilities,
                            // even if a plugin printed a complete record before failing.
                            ensure!(!complete || status.success(), "Gradle failed after context evaluation ({status})");
                            for snapshot in snapshots {
                                this.project.update(cx, |project, cx| project.publish_android_project_context(&this.active, &active, &discovery, snapshot, cx))?;
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
                let succeeded = matches!(result, Ok(Some(())));
                let requested_model_sync = this.manual_model_sync.as_ref() == Some(&active);
                if !succeeded && requested_model_sync {
                    this.manual_model_sync = None;
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
                if succeeded && requested_model_sync {
                    // Context publication remains import-owned; only a complete
                    // successful result may continue the explicit user's Sync.
                    let controller = cx.weak_entity();
                    window.defer(cx, move |window, cx| {
                        let authorized = controller.update(cx, |this, cx| {
                            this.take_manual_model_sync(&active, &expected_root, window, cx)
                        });
                        let Ok(Some(workspace)) = authorized else { return; };
                        // The controller lease must be released before any panel
                        // method reads that same controller to capture its owner.
                        let Some(workspace) = workspace.upgrade() else { return; };
                        let Some(panel) = workspace.read(cx).panel::<crate::AndroidPanel>(cx) else { return; };
                        panel.update(cx, |panel, cx| {
                            panel.context_operations_changed(cx);
                            let Ok(current) = panel.operation_owner(crate::AndroidOperation::Sync, cx) else { return; };
                            if current.context == active && current.root == expected_root {
                                panel.sync_project(window, cx);
                            }
                        });
                    });
                }
                cx.notify();
            }).log_err();
        }));
        cx.notify();
        Ok(())
    }
}
