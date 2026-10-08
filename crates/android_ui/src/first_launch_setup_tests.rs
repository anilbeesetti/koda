use super::*;
use gpui::{DismissEvent, TestAppContext, WindowHandle};
use project::{
    FakeFs,
    trusted_worktrees::{self, PathTrust},
};
use workspace::{AppState, ModalView};

fn initialize(enabled: bool, cx: &mut TestAppContext) -> Arc<AppState> {
    cx.update(|cx| {
        cx.set_global(db::AppDatabase::test_new());
        let state = AppState::test(cx);
        trusted_worktrees::init(Default::default(), cx);
        init(cx);
        if enabled {
            enable_first_launch_setup(cx);
        }
        state
    })
}

async fn empty_project(cx: &mut TestAppContext) -> Entity<Project> {
    Project::test(FakeFs::new(cx.executor()), [], cx).await
}

async fn android_project(root: &Path, cx: &mut TestAppContext) -> Entity<Project> {
    let filesystem = FakeFs::new(cx.executor());
    filesystem
        .insert_tree(
            root,
            serde_json::json!({"gradlew": "", "settings.gradle.kts": ""}),
        )
        .await;
    let project = Project::test_with_worktree_trust(filesystem, [root], cx).await;
    let worktrees = project.read_with(cx, |project, _| project.worktree_store());
    cx.update(|cx| {
        TrustedWorktrees::try_get_global(cx)
            .expect("Trust store")
            .update(cx, |trusted, cx| {
                trusted.trust(
                    &worktrees,
                    [PathTrust::AbsPath(root.to_path_buf())]
                        .into_iter()
                        .collect(),
                    cx,
                );
            });
        assert!(!TrustedWorktrees::has_restricted_worktrees(&worktrees, cx));
    });
    project
}

fn set_project_readiness(window: WindowHandle<Workspace>, ready: bool, cx: &mut TestAppContext) {
    window
        .update(cx, |workspace, window, cx| {
            let panel = controller(workspace, cx).expect("Android controller");
            panel.update(cx, |panel, cx| {
                let root = panel.trusted_root(cx).expect("Trusted project root");
                panel.startup_settings_ready = true;
                panel.tool_setup.bootstrap_root = Some(root);
                panel.tool_setup.bootstrap_ready = Some(ready);
                panel.auto_sync_project(window, cx);
            });
        })
        .expect("Workspace window is open");
}

fn add_workspace(
    project: &Entity<Project>,
    blocked: bool,
    cx: &mut TestAppContext,
) -> WindowHandle<Workspace> {
    cx.add_window(|window, cx| {
        let mut workspace = Workspace::test_new(project.clone(), window, cx);
        if blocked {
            workspace.toggle_modal(window, cx, |_, cx| BlockingModal(cx.focus_handle()));
        }
        workspace
    })
}

fn has_modal(window: WindowHandle<Workspace>, cx: &mut TestAppContext) -> bool {
    window
        .update(cx, |workspace, window, cx| {
            workspace.has_active_modal(window, cx)
        })
        .expect("Workspace window is open")
}

fn persisted_state(cx: &TestAppContext) -> Option<String> {
    cx.read(|cx| {
        KeyValueStore::global(cx)
            .read_kvp(&first_launch_setup_key())
            .expect("Read onboarding state")
    })
}

#[gpui::test]
async fn startup_kotlin_outcomes_preserve_existing_operation_errors(cx: &mut TestAppContext) {
    let _state = initialize(true, cx);
    cx.update(|cx| startup_kotlin::set_state_for_test(startup_kotlin::State::Checking, cx));
    let project = empty_project(cx).await;
    let window = add_workspace(&project, false, cx);
    cx.run_until_parked();
    window
        .update(cx, |workspace, _, cx| {
            let panel = controller(workspace, cx).expect("Android controller");
            panel.update(cx, |panel, _| {
                panel.error = Some("Existing build failure".into());
                panel.status = "Build failed".into();
            });
        })
        .expect("Open window");
    for outcome in [
        startup_kotlin::State::Ready,
        startup_kotlin::State::Failed("Kotlin setup failed: Python is missing".into()),
    ] {
        cx.update(|cx| startup_kotlin::set_state_for_test(outcome, cx));
        cx.run_until_parked();
        window
            .update(cx, |workspace, _, cx| {
                let panel = controller(workspace, cx).expect("Android controller");
                assert_eq!(
                    panel.read(cx).error.as_deref(),
                    Some("Existing build failure")
                );
                assert_eq!(panel.read(cx).status.as_ref(), "Build failed");
            })
            .expect("Open window");
    }
}

struct BlockingModal(FocusHandle);

impl Focusable for BlockingModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.0.clone()
    }
}

impl EventEmitter<DismissEvent> for BlockingModal {}
impl ModalView for BlockingModal {}
impl Render for BlockingModal {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().track_focus(&self.0).child("Another modal")
    }
}

#[gpui::test]
async fn disabled_coordinator_does_not_open_or_persist_setup(cx: &mut TestAppContext) {
    let _state = initialize(false, cx);
    let project = empty_project(cx).await;
    let window = add_workspace(&project, false, cx);
    cx.run_until_parked();
    assert!(!has_modal(window, cx));
    cx.update(|cx| {
        finish_first_launch_setup(cx);
        defer_first_launch_setup(cx);
    });
    cx.run_until_parked();
    assert_eq!(persisted_state(cx), None);
}

#[gpui::test]
async fn empty_workspace_opens_setup_without_a_project_or_saved_completion(
    cx: &mut TestAppContext,
) {
    let _state = initialize(true, cx);
    let project = empty_project(cx).await;
    let window = add_workspace(&project, false, cx);
    cx.run_until_parked();
    assert!(has_modal(window, cx));
    window
        .read_with(cx, |workspace, cx| {
            let panel = controller(workspace, cx).expect("Android controller");
            assert!(panel.read(cx).root.is_none());
            assert!(!panel.read(cx).syncing);
            let setup = cx.global::<FirstLaunchSetup>();
            assert_eq!(setup.window, Some(window.window_id()));
            assert_eq!(
                setup
                    .workspace
                    .as_ref()
                    .map(|workspace| workspace.entity_id()),
                Some(workspace.weak_handle().entity_id())
            );
            assert!(!setup.resolved);
        })
        .expect("Workspace window is open");
    assert_eq!(persisted_state(cx), None);
}

#[gpui::test]
async fn multiple_windows_claim_only_one_setup_when_enabled_together(cx: &mut TestAppContext) {
    let _state = initialize(false, cx);
    let project = empty_project(cx).await;
    let first = add_workspace(&project, false, cx);
    let second_project = empty_project(cx).await;
    let second = add_workspace(&second_project, false, cx);
    cx.run_until_parked();
    cx.update(enable_first_launch_setup);
    cx.run_until_parked();
    assert_ne!(has_modal(first, cx), has_modal(second, cx));
    let claimed = cx.read(|cx| cx.global::<FirstLaunchSetup>().window);
    for window in [first, second] {
        window
            .update(cx, |_, _, cx| cx.notify())
            .expect("Workspace window is open");
    }
    cx.run_until_parked();
    assert_ne!(has_modal(first, cx), has_modal(second, cx));
    assert_eq!(
        cx.read(|cx| cx.global::<FirstLaunchSetup>().window),
        claimed
    );
}

#[gpui::test]
async fn setup_waits_for_a_modal_to_dismiss_without_workspace_notifications(
    cx: &mut TestAppContext,
) {
    let _state = initialize(true, cx);
    let project = empty_project(cx).await;
    let window = add_workspace(&project, true, cx);
    cx.run_until_parked();
    let blocker = window
        .read_with(cx, |workspace, cx| {
            assert!(cx.global::<FirstLaunchSetup>().workspace.is_none());
            workspace
                .active_modal::<BlockingModal>(cx)
                .expect("Blocking modal")
        })
        .expect("Workspace window is open");
    blocker.update(cx, |_, cx| cx.emit(DismissEvent));
    cx.run_until_parked();
    assert!(has_modal(window, cx));
    window
        .read_with(cx, |workspace, cx| {
            assert!(workspace.active_modal::<BlockingModal>(cx).is_none());
            assert_eq!(
                cx.global::<FirstLaunchSetup>().window,
                Some(window.window_id())
            );
        })
        .expect("Workspace window is open");
}

#[gpui::test]
async fn blocked_window_does_not_prevent_another_window_from_claiming_setup(
    cx: &mut TestAppContext,
) {
    let _state = initialize(true, cx);
    let project = empty_project(cx).await;
    let blocked = add_workspace(&project, true, cx);
    let available_project = empty_project(cx).await;
    let available = add_workspace(&available_project, false, cx);
    cx.run_until_parked();
    assert!(has_modal(available, cx));
    blocked
        .update(cx, |workspace, window, cx| {
            assert!(workspace.active_modal::<BlockingModal>(cx).is_some());
            assert!(workspace.hide_modal(window, cx));
        })
        .expect("Workspace window is open");
    cx.run_until_parked();
    assert!(!has_modal(blocked, cx));
    assert!(has_modal(available, cx));
}

#[gpui::test]
async fn closing_claimed_window_releases_setup_even_if_workspace_is_retained(
    cx: &mut TestAppContext,
) {
    let _state = initialize(true, cx);
    let project = empty_project(cx).await;
    let first = add_workspace(&project, false, cx);
    cx.run_until_parked();
    assert!(has_modal(first, cx));
    let retained_workspace = first.root(cx).expect("First workspace");
    let second_project = empty_project(cx).await;
    let second = add_workspace(&second_project, false, cx);
    cx.run_until_parked();
    assert!(!has_modal(second, cx));
    first
        .update(cx, |_, window, _| window.remove_window())
        .expect("First window closes");
    cx.run_until_parked();
    assert!(retained_workspace.downgrade().upgrade().is_some());
    assert!(has_modal(second, cx));
    assert_eq!(
        cx.read(|cx| cx.global::<FirstLaunchSetup>().window),
        Some(second.window_id())
    );
    assert_eq!(persisted_state(cx), None);
}

#[gpui::test]
async fn deferred_setup_stays_dismissed_after_coordinator_restart(cx: &mut TestAppContext) {
    let _state = initialize(true, cx);
    let project = empty_project(cx).await;
    let first = add_workspace(&project, false, cx);
    cx.run_until_parked();
    first
        .update(cx, |workspace, window, cx| {
            assert!(workspace.hide_modal(window, cx))
        })
        .expect("Dismiss setup");
    cx.run_until_parked();
    assert_eq!(persisted_state(cx).as_deref(), Some("deferred"));
    assert!(!has_modal(first, cx));
    cx.update(|cx| {
        cx.set_global(FirstLaunchSetup {
            enabled: true,
            ..Default::default()
        })
    });
    let second_project = empty_project(cx).await;
    let second = add_workspace(&second_project, false, cx);
    cx.run_until_parked();
    assert!(!has_modal(first, cx));
    assert!(!has_modal(second, cx));
    assert!(cx.read(|cx| cx.global::<FirstLaunchSetup>().resolved));
}

#[gpui::test]
async fn completing_setup_is_not_downgraded_by_modal_dismissal(cx: &mut TestAppContext) {
    let _state = initialize(true, cx);
    let project = empty_project(cx).await;
    let first = add_workspace(&project, false, cx);
    cx.run_until_parked();
    first
        .update(cx, |workspace, window, cx| {
            finish_first_launch_setup(cx);
            assert!(workspace.hide_modal(window, cx));
        })
        .expect("Finish and dismiss setup");
    cx.run_until_parked();
    assert_eq!(persisted_state(cx).as_deref(), Some("completed"));
    cx.update(|cx| {
        cx.set_global(FirstLaunchSetup {
            enabled: true,
            ..Default::default()
        })
    });
    let second_project = empty_project(cx).await;
    let second = add_workspace(&second_project, false, cx);
    cx.run_until_parked();
    assert!(!has_modal(first, cx));
    assert!(!has_modal(second, cx));
    assert!(cx.read(|cx| cx.global::<FirstLaunchSetup>().resolved));
}

#[test]
fn onboarding_keys_are_distinct_for_stable_nightly_and_dev_profiles() {
    let roots = [
        Path::new("/profiles/Koda/android-tools"),
        Path::new("/profiles/Koda Nightly/android-tools"),
        Path::new("/profiles/Koda Dev/android-tools"),
    ];
    let keys = roots.map(first_launch_setup_key_for_root);
    assert_eq!(
        keys.iter().collect::<std::collections::HashSet<_>>().len(),
        3
    );
    for (root, key) in roots.into_iter().zip(keys) {
        assert_eq!(
            key,
            format!("android-setup-onboarding-v1:{}", root.display())
        );
    }
}

#[gpui::test]
async fn first_launch_claim_blocks_missing_project_prompts_and_transfers_on_close(
    cx: &mut TestAppContext,
) {
    let _state = initialize(true, cx);
    let first_project = android_project(Path::new("/first"), cx).await;
    let second_project = android_project(Path::new("/second"), cx).await;
    let first = add_workspace(&first_project, false, cx);
    cx.run_until_parked();
    let second = add_workspace(&second_project, false, cx);
    cx.run_until_parked();
    set_project_readiness(first, false, cx);
    set_project_readiness(second, false, cx);
    cx.run_until_parked();
    assert!(has_modal(first, cx));
    assert!(!has_modal(second, cx));
    second
        .read_with(cx, |workspace, cx| {
            assert!(
                controller(workspace, cx)
                    .expect("Android controller")
                    .read(cx)
                    .onboarding_prompted_root
                    .is_none()
            );
        })
        .expect("Second window is open");
    first
        .update(cx, |_, window, _| window.remove_window())
        .expect("Close first window");
    cx.run_until_parked();
    assert!(has_modal(second, cx));
    second
        .read_with(cx, |workspace, cx| {
            assert_eq!(
                controller(workspace, cx)
                    .expect("Android controller")
                    .read(cx)
                    .onboarding_prompted_root
                    .as_deref(),
                Some(Path::new("/second"))
            );
        })
        .expect("Second window is open");
}

#[gpui::test]
async fn missing_project_prompts_share_a_claim_and_retry_the_unconsumed_root(
    cx: &mut TestAppContext,
) {
    let _state = initialize(false, cx);
    let first_project = android_project(Path::new("/first"), cx).await;
    let second_project = android_project(Path::new("/second"), cx).await;
    let first = add_workspace(&first_project, false, cx);
    let second = add_workspace(&second_project, false, cx);
    cx.run_until_parked();
    set_project_readiness(first, false, cx);
    cx.run_until_parked();
    set_project_readiness(second, false, cx);
    cx.run_until_parked();
    assert!(has_modal(first, cx));
    assert!(!has_modal(second, cx));
    second
        .read_with(cx, |workspace, cx| {
            assert!(
                controller(workspace, cx)
                    .expect("Android controller")
                    .read(cx)
                    .onboarding_prompted_root
                    .is_none()
            );
        })
        .expect("Second window is open");
    first
        .update(cx, |workspace, window, cx| {
            assert!(workspace.hide_modal(window, cx))
        })
        .expect("Dismiss first project setup");
    cx.run_until_parked();
    assert!(!has_modal(first, cx));
    assert!(has_modal(second, cx));
    second
        .update(cx, |workspace, window, cx| {
            assert!(workspace.hide_modal(window, cx))
        })
        .expect("Dismiss second project setup");
    cx.run_until_parked();
    assert!(!has_modal(first, cx));
    assert!(!has_modal(second, cx));
}

#[gpui::test]
async fn first_launch_reserves_startup_sync_until_the_wizard_is_dismissed(cx: &mut TestAppContext) {
    let _state = initialize(true, cx);
    let project = android_project(Path::new("/android"), cx).await;
    let window = add_workspace(&project, true, cx);
    cx.run_until_parked();
    set_project_readiness(window, true, cx);
    cx.run_until_parked();
    window
        .read_with(cx, |workspace, cx| {
            assert!(workspace.active_modal::<BlockingModal>(cx).is_some());
            let panel = controller(workspace, cx).expect("Android controller");
            assert!(panel.read(cx).auto_sync_root.is_none());
            assert!(!panel.read(cx).syncing);
        })
        .expect("Workspace is open");
    window
        .update(cx, |workspace, window, cx| {
            assert!(workspace.hide_modal(window, cx))
        })
        .expect("Dismiss blocking modal");
    cx.run_until_parked();
    assert!(has_modal(window, cx));
    window
        .read_with(cx, |workspace, cx| {
            let panel = controller(workspace, cx).expect("Android controller");
            assert!(panel.read(cx).auto_sync_root.is_none());
            assert!(!panel.read(cx).syncing);
        })
        .expect("Setup wizard is open");
    window
        .update(cx, |workspace, window, cx| {
            assert!(workspace.hide_modal(window, cx))
        })
        .expect("Dismiss setup wizard");
    cx.run_until_parked();
    window
        .read_with(cx, |workspace, cx| {
            assert_eq!(
                controller(workspace, cx)
                    .expect("Android controller")
                    .read(cx)
                    .auto_sync_root
                    .as_deref(),
                Some(Path::new("/android"))
            );
        })
        .expect("Workspace is open");
    assert_eq!(persisted_state(cx).as_deref(), Some("deferred"));
}
