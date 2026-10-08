use gpui::{Animation, AnimationExt, Empty};
use ui::{Tooltip, prelude::*};
use util::ResultExt as _;
use workspace::{HideStatusItem, ItemHandle, StatusItemView};

use crate::{AndroidPanel, BuildPanel, BuildTab};

pub(crate) fn register(panel: &gpui::Entity<AndroidPanel>, window: &mut Window, cx: &mut App) {
    let activity = cx.new(|cx| AndroidActivity::new(panel, cx));
    let workspace = panel.read(cx).workspace.clone();
    // Right-side items render in reverse registration order. Append after the
    // workspace's regular items so progress grows leftward without moving them.
    window.defer(cx, move |window, cx| {
        workspace
            .update(cx, |workspace, cx| {
                workspace
                    .status_bar()
                    .update(cx, |bar, cx| bar.add_right_item(activity, window, cx));
            })
            .log_err();
    });
}

#[derive(Clone, PartialEq, Eq)]
enum ActivityToken {
    Build(BuildTab, u64),
    Emulator(std::path::PathBuf, String),
}

pub(crate) struct AndroidActivity {
    panel: gpui::WeakEntity<AndroidPanel>,
    _subscription: gpui::Subscription,
}

impl AndroidActivity {
    fn activity(panel: &AndroidPanel, cx: &App) -> Option<(ActivityToken, SharedString)> {
        if let Some((tab, id)) = panel.active_build_session {
            let label = match tab {
                BuildTab::Sync => panel.status.clone(),
                BuildTab::Output if panel.build_panel.read(cx).is_waiting_for_emulator(tab, id) => {
                    panel.status.clone()
                }
                BuildTab::Output if panel.active_operation_id.is_some() => panel.status.clone(),
                BuildTab::Output => format!("Gradle build running · {}", panel.status).into(),
            };
            Some((ActivityToken::Build(tab, id), label))
        } else if panel.running
            && let Some(startup) = &panel.emulator_startup
        {
            Some((
                ActivityToken::Emulator(startup.root.clone(), startup.name.clone()),
                panel.status.clone(),
            ))
        } else {
            None
        }
    }

    pub(crate) fn new(panel: &gpui::Entity<AndroidPanel>, cx: &mut Context<Self>) -> Self {
        Self {
            panel: panel.downgrade(),
            _subscription: cx.observe(panel, |_, _, cx| cx.notify()),
        }
    }
}

impl StatusItemView for AndroidActivity {
    fn set_active_pane_item(
        &mut self,
        _: Option<&dyn ItemHandle>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
    }

    fn hide_setting(&self, _: &App) -> Option<HideStatusItem> {
        // Like the workspace activity indicator, this disappears when idle.
        None
    }
}

impl Render for AndroidActivity {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let activity = self.panel.read_with(cx, Self::activity).ok().flatten();
        let Some((token, label)) = activity else {
            return Empty.into_any_element();
        };
        let details = match token {
            ActivityToken::Build(..) => "Show Build output",
            ActivityToken::Emulator(..) => "Show Android tools",
        };
        h_flex()
            .debug_selector(|| "android-operation-status".into())
            .gap_1()
            .child(
                div()
                    .debug_selector(|| "android-operation-details".into())
                    .max_w(px(280.))
                    .overflow_hidden()
                    .child(
                        Button::new("android-operation-details", label.clone())
                            .label_size(LabelSize::Small)
                            .truncate(true)
                            .tab_index(0isize)
                            .tooltip(Tooltip::text(format!("{label}\n{details}")))
                            .on_click({
                                let panel = self.panel.clone();
                                let token = token.clone();
                                move |_, window, cx| {
                                    let Some((build_panel, workspace)) = panel
                                        .read_with(cx, |panel, _| {
                                            (panel.build_panel.clone(), panel.workspace.clone())
                                        })
                                        .log_err()
                                    else {
                                        return;
                                    };
                                    let tab = match token {
                                        ActivityToken::Build(tab, _) => tab,
                                        ActivityToken::Emulator(..) => BuildTab::Output,
                                    };
                                    build_panel.update(cx, |pane, cx| pane.select(tab, cx));
                                    workspace
                                        .update(cx, |workspace, cx| {
                                            workspace.reveal_panel::<BuildPanel>(window, cx)
                                        })
                                        .log_err();
                                }
                            }),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "android-operation-progress".into())
                    .w(px(80.))
                    .h(px(3.))
                    .rounded_full()
                    .overflow_hidden()
                    .bg(cx.theme().colors().border_variant)
                    .child(
                        div()
                            .w(px(24.))
                            .h_full()
                            .rounded_full()
                            .bg(cx.theme().status().info)
                            .with_animation(
                                "android-operation-progress",
                                Animation::new(std::time::Duration::from_millis(1200))
                                    .repeat_synced()
                                    .with_max_fps(30.),
                                |bar, delta| bar.ml(px(delta * 56.)),
                            ),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "cancel-android-operation".into())
                    .child(
                        IconButton::new("cancel-android-operation", IconName::Close)
                            .icon_size(IconSize::Small)
                            .tab_index(0isize)
                            .aria_label("Cancel Android operation")
                            .tooltip(Tooltip::text("Cancel Android operation"))
                            .on_click({
                                let panel = self.panel.clone();
                                move |_, _, cx| {
                                    panel
                                        .update(cx, |panel, cx| {
                                            if Self::activity(panel, cx)
                                                .is_some_and(|(current, _)| current == token)
                                            {
                                                match token {
                                                    ActivityToken::Build(tab, _) => {
                                                        panel.cancel_build(tab, cx)
                                                    }
                                                    ActivityToken::Emulator(..) => {
                                                        panel.emulator_task = None;
                                                        panel.emulator_startup = None;
                                                        panel.running = false;
                                                        panel.status = "Operation cancelled".into();
                                                        cx.notify();
                                                    }
                                                }
                                            }
                                        })
                                        .log_err();
                                }
                            }),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::android_build::BuildStatus;
    use gpui::TestAppContext;
    use project::{FakeFs, Project};
    use std::path::Path;
    use workspace::{AppState, Workspace};

    struct FixedStatus(&'static str);

    impl StatusItemView for FixedStatus {
        fn set_active_pane_item(
            &mut self,
            _: Option<&dyn ItemHandle>,
            _: &mut Window,
            _: &mut Context<Self>,
        ) {
        }
        fn hide_setting(&self, _: &App) -> Option<HideStatusItem> {
            None
        }
    }

    impl Render for FixedStatus {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let label = self.0;
            div()
                .debug_selector(move || label.into())
                .w(px(70.))
                .child(Button::new(label, label).label_size(LabelSize::Small))
        }
    }

    #[gpui::test]
    async fn status_stays_visible_with_build_hidden_and_cancels_sync_and_build(
        cx: &mut TestAppContext,
    ) {
        let _app_state = cx.update(AppState::test);
        cx.update(|cx| cx.set_reduce_motion(true));
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/android", serde_json::json!({"settings.gradle.kts": ""}))
            .await;
        let project = Project::test(fs, [Path::new("/android")], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.new(|cx| AndroidPanel::new(workspace.downgrade(), project, cx));
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_panel(panel.read(cx).build_panel.clone(), window, cx);
            crate::register_controller(workspace, &panel, cx);
            register(&panel, window, cx);
            let status_items =
                ["completions", "language", "cursor"].map(|label| cx.new(|_| FixedStatus(label)));
            workspace.status_bar().update(cx, |bar, cx| {
                for item in status_items {
                    bar.add_right_item(item, window, cx);
                }
            });
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("android-operation-status").is_none());
        let idle_bounds = ["completions", "language", "cursor"]
            .map(|label| (label, cx.debug_bounds(label).expect("Regular status item")));
        for tab in [BuildTab::Sync, BuildTab::Output] {
            panel.update_in(cx, |panel, window, cx| {
                let id = panel.build_panel.update(cx, |pane, cx| {
                    let (id, output, logs) =
                        pane.begin(tab, "Android operation".into(), false, window, cx);
                    drop(output);
                    logs.detach();
                    id
                });
                panel.active_build_session = Some((tab, id));
                panel.syncing = tab == BuildTab::Sync;
                panel.running = tab == BuildTab::Output;
                panel.status = match tab {
                    BuildTab::Sync => "Syncing Android project…",
                    BuildTab::Output => "Build android",
                }
                .into();
                cx.notify();
            });
            cx.run_until_parked();
            let details = cx
                .debug_bounds("android-operation-details")
                .expect("Activity details");
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.close_panel::<BuildPanel>(window, cx)
            });
            cx.run_until_parked();
            assert!(cx.debug_bounds("build-console").is_none());
            let status = cx
                .debug_bounds("android-operation-status")
                .expect("Activity survives hiding Build");
            for (label, bounds) in idle_bounds {
                assert_eq!(
                    cx.debug_bounds(label),
                    Some(bounds),
                    "Progress must not move {label}"
                );
                assert!(
                    status.origin.x + status.size.width <= bounds.origin.x,
                    "Progress must be left of {label}"
                );
            }
            let progress = cx
                .debug_bounds("android-operation-progress")
                .expect("Progress bar");
            assert_eq!(progress.size.width, px(80.));
            let cancel = cx
                .debug_bounds("cancel-android-operation")
                .expect("Cancel control");
            assert!(cancel.origin.x > progress.origin.x);
            assert!(cancel.origin.x >= status.origin.x);
            cx.simulate_click(details.center(), Default::default());
            cx.run_until_parked();
            assert!(
                cx.debug_bounds("build-console").is_some(),
                "Activity reopens output"
            );
            if tab == BuildTab::Output {
                panel.update(cx, |panel, cx| {
                    let (tab, id) = panel.active_build_session.expect("Build");
                    panel.build_panel.update(cx, |pane, cx| {
                        pane.finish(
                            tab,
                            id,
                            BuildStatus::Succeeded,
                            "Build succeeded".into(),
                            cx,
                        );
                        pane.set_waiting_for_emulator(tab, id, true, cx);
                    });
                    panel.status = "Waiting for Selected to finish booting…".into();
                    cx.notify();
                });
                workspace.update_in(cx, |workspace, window, cx| {
                    workspace.close_panel::<BuildPanel>(window, cx)
                });
                cx.run_until_parked();
                assert!(
                    cx.debug_bounds("android-operation-status").is_some(),
                    "Boot wait survives hiding Build"
                );
                for (label, bounds) in idle_bounds {
                    assert_eq!(
                        cx.debug_bounds(label),
                        Some(bounds),
                        "Boot wait must not move {label}"
                    );
                }
                let details = cx
                    .debug_bounds("android-operation-details")
                    .expect("Boot wait details");
                cx.simulate_click(details.center(), Default::default());
                cx.run_until_parked();
                assert!(
                    cx.debug_bounds("build-console").is_some(),
                    "Boot wait details reveal Build"
                );
                panel.read_with(cx, |panel, _| {
                    assert!(panel.running);
                    assert!(panel.active_build_session.is_some());
                });
                workspace.update_in(cx, |workspace, window, cx| {
                    workspace.close_panel::<BuildPanel>(window, cx)
                });
                cx.run_until_parked();
            }
            let cancel = cx
                .debug_bounds("cancel-android-operation")
                .expect("Cancel control");
            cx.simulate_click(cancel.center(), Default::default());
            cx.run_until_parked();
            panel.read_with(cx, |panel, _| {
                assert!(panel.active_build_session.is_none());
                assert!(!panel.running && !panel.syncing);
            });
            assert!(
                cx.debug_bounds("android-operation-status").is_none(),
                "Cancelled activity disappears"
            );
        }
        use futures::FutureExt as _;
        let (started, startup) = futures::channel::oneshot::channel::<()>();
        panel.update_in(cx, |panel, window, cx| {
            let root = std::path::PathBuf::from("/android");
            panel.root = Some(root.clone());
            panel.emulator_startup = Some(crate::EmulatorStartup {
                root: root.clone(),
                name: "Selected".into(),
                error_reported: false,
                ready: cx
                    .spawn(async move |_, _| startup.await.map_err(|error| error.to_string()))
                    .shared(),
            });
            panel.wait_for_emulator("Selected".into(), root, None, window, cx);
        });
        cx.run_until_parked();
        let details = cx
            .debug_bounds("android-operation-details")
            .expect("Standalone boot wait details");
        cx.simulate_click(details.center(), Default::default());
        cx.run_until_parked();
        assert!(
            workspace.read_with(cx, |workspace, cx| {
                workspace
                    .bottom_dock()
                    .read(cx)
                    .visible_panel()
                    .is_some_and(|visible| {
                        visible.panel_id() == panel.read(cx).build_panel.entity_id()
                    })
            }),
            "Standalone boot wait details reveal Build Output"
        );
        assert!(panel.read_with(cx, |panel, _| panel.running
            && panel.emulator_startup.is_some()));
        let cancel = cx
            .debug_bounds("cancel-android-operation")
            .expect("Standalone boot wait is cancellable");
        cx.simulate_click(cancel.center(), Default::default());
        cx.run_until_parked();
        assert!(started.send(()).is_err(), "Cancel drops standalone startup");
        panel.read_with(cx, |panel, _| {
            assert!(!panel.running);
            assert!(panel.emulator_startup.is_none());
            assert!(panel.emulator_task.is_none());
            assert!(panel.error.is_none());
        });
        assert!(cx.debug_bounds("android-operation-status").is_none());
    }
}
