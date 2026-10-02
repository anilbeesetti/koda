use super::android_logcat::{LogcatView, NewViewer, Toggle};
use super::*;
use workspace::{Pane, PaneGroup, PaneRenderContext, SplitDirection, pane};

pub(super) struct LogcatPanel {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    active_pane: Entity<Pane>,
    center: PaneGroup,
    focus_handle: FocusHandle,
}

impl LogcatPanel {
    pub(super) fn new(workspace: &Workspace, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let pane = Self::new_pane(
            workspace.weak_handle(),
            workspace.project().clone(),
            window,
            cx,
        );
        Self {
            workspace: workspace.weak_handle(),
            project: workspace.project().clone(),
            active_pane: pane.clone(),
            center: PaneGroup::new(pane),
            focus_handle: cx.focus_handle(),
        }
    }

    fn new_pane(
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Pane> {
        let panel = cx.weak_entity();
        let pane = cx.new(|cx| {
            let mut pane = Pane::new(
                workspace,
                project,
                Default::default(),
                Some(Arc::new(|_, _, _| false)),
                NewViewer.boxed_clone(),
                false,
                window,
                cx,
            );
            pane.set_can_navigate(false, cx);
            pane.display_nav_history_buttons(None);
            pane.set_should_display_tab_bar(|_, _| true);
            pane.set_zoom_out_on_close(false);
            pane.set_can_split(Some(Arc::new(|_, _, _, _| false)));
            pane.set_render_tab_bar_buttons(cx, move |pane, _, cx| {
                let close = panel.clone();
                let buttons = h_flex()
                    .gap_1()
                    .child(
                        IconButton::new("new-logcat", IconName::Plus)
                            .tooltip(Tooltip::text("New Logcat tab"))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(NewViewer.boxed_clone(), cx)
                            }),
                    )
                    .child(
                        IconButton::new("split-logcat", IconName::Split)
                            .tooltip(Tooltip::text("Split Logcat"))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(
                                    workspace::SplitRight::default().boxed_clone(),
                                    cx,
                                )
                            }),
                    )
                    .child(
                        IconButton::new("zoom-logcat", IconName::Maximize)
                            .toggle_state(pane.is_zoomed())
                            .selected_icon(IconName::Minimize)
                            .tooltip(Tooltip::text("Maximize or restore Logcat"))
                            .on_click(cx.listener(|pane, _, window, cx| {
                                pane.toggle_zoom(&workspace::ToggleZoom, window, cx)
                            })),
                    )
                    .child(
                        IconButton::new("hide-logcat", IconName::Dash)
                            .tooltip(Tooltip::text("Hide Logcat"))
                            .on_click(move |_, _, cx| {
                                close
                                    .update(cx, |_, cx| cx.emit(PanelEvent::Close))
                                    .log_err();
                            }),
                    );
                (
                    Some(Label::new("Logcat").into_any_element()),
                    Some(buttons.into_any_element()),
                )
            });
            pane
        });
        cx.subscribe_in(&pane, window, Self::pane_event).detach();
        cx.observe(&pane, |_, _, cx| cx.notify()).detach();
        pane
    }

    pub(super) fn has_views(&self, cx: &App) -> bool {
        self.center
            .panes()
            .iter()
            .any(|pane| pane.read(cx).items_len() > 0)
    }

    pub(super) fn show_logs(
        &mut self,
        root: PathBuf,
        serial: Option<String>,
        targets: Vec<AndroidTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self.center.panes().into_iter().find_map(|pane| {
            pane.read(cx)
                .items_of_type::<LogcatView>()
                .find(|view| view.read(cx).root() == &root)
                .map(|view| (pane.clone(), view))
        });
        if let Some((pane, view)) = existing {
            view.update(cx, |view, cx| view.set_targets(targets, cx));
            if let Some(index) = pane.read(cx).index_for_item(&view) {
                pane.update(cx, |pane, cx| {
                    pane.activate_item(index, true, true, window, cx)
                });
            }
            self.active_pane = pane;
        } else {
            self.add_view(root, serial, targets, self.active_pane.clone(), window, cx);
        }
        cx.notify();
    }

    fn add_view(
        &mut self,
        root: PathBuf,
        serial: Option<String>,
        targets: Vec<AndroidTarget>,
        pane: Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.new(|cx| {
            LogcatView::new(
                self.workspace.clone(),
                self.project.clone(),
                root,
                serial,
                targets,
                window,
                cx,
            )
        });
        view.update(cx, |view, cx| view.watch_devices(cx));
        pane.update(cx, |pane, cx| {
            pane.add_item(Box::new(view), true, true, None, window, cx)
        });
        self.active_pane = pane;
        cx.notify();
    }

    pub(super) fn new_view(
        &mut self,
        direction: Option<SplitDirection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self
            .active_pane
            .read(cx)
            .active_item()
            .and_then(|item| item.downcast::<LogcatView>())
        else {
            return;
        };
        let (root, serial, targets) = view.read(cx).capture_context();
        let pane = if let Some(direction) = direction {
            let pane = Self::new_pane(self.workspace.clone(), self.project.clone(), window, cx);
            self.center.split(&self.active_pane, &pane, direction, cx);
            pane
        } else {
            self.active_pane.clone()
        };
        self.add_view(root, serial, targets, pane, window, cx);
    }

    fn pane_event(
        &mut self,
        pane: &Entity<Pane>,
        event: &pane::Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            pane::Event::Focus => self.active_pane = pane.clone(),
            pane::Event::Remove { .. } => {
                if self.center.panes().len() == 1 {
                    self.set_zoomed(false, window, cx);
                    cx.emit(PanelEvent::ZoomOut);
                    cx.emit(PanelEvent::Close);
                } else {
                    self.center.remove(pane, cx).log_err();
                    self.active_pane = self.center.first_pane();
                }
            }
            pane::Event::ZoomIn => {
                self.set_zoomed(true, window, cx);
                cx.emit(PanelEvent::ZoomIn);
            }
            pane::Event::ZoomOut => {
                self.set_zoomed(false, window, cx);
                cx.emit(PanelEvent::ZoomOut);
            }
            pane::Event::Split { direction, .. } => {
                self.active_pane = pane.clone();
                self.new_view(Some(*direction), window, cx);
            }
            pane::Event::AddItem { item } => {
                self.workspace
                    .update(cx, |workspace, cx| {
                        item.added_to_pane(workspace, pane.clone(), window, cx)
                    })
                    .log_err();
            }
            _ => {}
        }
        cx.notify();
    }
}

impl Focusable for LogcatPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl EventEmitter<PanelEvent> for LogcatPanel {}
impl Panel for LogcatPanel {
    fn persistent_name() -> &'static str {
        "LogcatPanel"
    }
    fn panel_key() -> &'static str {
        "LogcatPanel"
    }
    fn activation_focus_handle(&self, cx: &App) -> FocusHandle {
        self.active_pane.focus_handle(cx)
    }
    fn position(&self, _: &Window, _: &App) -> DockPosition {
        DockPosition::Bottom
    }
    fn position_is_valid(&self, position: DockPosition) -> bool {
        position == DockPosition::Bottom
    }
    fn set_position(&mut self, _: DockPosition, _: &mut Window, _: &mut Context<Self>) {}
    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        px(320.)
    }
    fn min_size(&self, _: &Window, _: &App) -> Option<Pixels> {
        Some(px(120.))
    }
    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::Logcat)
    }
    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Logcat")
    }
    fn toggle_action(&self) -> Box<dyn Action> {
        Toggle.boxed_clone()
    }
    fn pane(&self) -> Option<Entity<Pane>> {
        Some(self.active_pane.clone())
    }
    fn activation_priority(&self) -> u32 {
        3
    }
    fn is_zoomed(&self, _: &Window, cx: &App) -> bool {
        self.active_pane.read(cx).is_zoomed()
    }
    fn set_zoomed(&mut self, zoomed: bool, _: &mut Window, cx: &mut Context<Self>) {
        for pane in self.center.panes() {
            pane.update(cx, |pane, cx| pane.set_zoomed(zoomed, cx));
        }
        cx.notify();
    }
}
impl Render for LogcatPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = self
            .workspace
            .update(cx, |workspace, cx| {
                self.center
                    .render(
                        workspace.zoomed_item(),
                        None,
                        &PaneRenderContext {
                            follower_states: &Default::default(),
                            active_call: workspace.active_call(),
                            active_pane: &self.active_pane,
                            app_state: workspace.app_state(),
                            project: workspace.project(),
                            workspace: &self.workspace,
                        },
                        window,
                        cx,
                    )
                    .into_any_element()
            })
            .log_err();
        div()
            .key_context("LogcatPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .on_action(
                cx.listener(|panel, _: &NewViewer, window, cx| panel.new_view(None, window, cx)),
            )
            .children(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    async fn logcat_tabs_and_splits_stay_in_bottom_dock_and_preserve_editors(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _, cx) = super::super::android_logcat::tests::viewer(cx, false).await;
        let (panel, editor_pane, editor) = workspace.update_in(cx, |workspace, window, cx| {
            let editor = cx.new(|cx| editor::Editor::single_line(window, cx));
            workspace.add_item_to_active_pane(Box::new(editor.clone()), None, true, window, cx);
            let editor_pane = workspace.active_pane().clone();
            let panel = cx.new(|cx| LogcatPanel::new(workspace, window, cx));
            assert_eq!(panel.read(cx).position(window, cx), DockPosition::Bottom);
            assert!(!panel.read(cx).position_is_valid(DockPosition::Left));
            assert_eq!(panel.read(cx).icon(window, cx), Some(IconName::Logcat));
            workspace.add_panel(panel.clone(), window, cx);
            super::super::android_logcat::open(
                workspace,
                PathBuf::from("/logcat"),
                None,
                Vec::new(),
                window,
                cx,
            );
            (panel, editor_pane, editor)
        });
        cx.run_until_parked();
        let first_view = panel.read_with(cx, |panel, cx| {
            assert_eq!(panel.center.panes().len(), 1);
            assert_eq!(panel.active_pane.read(cx).items_len(), 1);
            panel
                .active_pane
                .read(cx)
                .items_of_type::<LogcatView>()
                .next()
                .expect("Dock Logcat")
        });
        workspace.update_in(cx, |workspace, window, cx| {
            assert!(workspace.bottom_dock().read(cx).is_open());
            assert_eq!(workspace.active_pane(), &editor_pane);
            assert_eq!(
                editor_pane
                    .read(cx)
                    .active_item()
                    .expect("Editor")
                    .item_id(),
                editor.entity_id()
            );
            super::super::android_logcat::open(
                workspace,
                PathBuf::from("/logcat"),
                None,
                Vec::new(),
                window,
                cx,
            );
        });
        panel.update_in(cx, |panel, window, cx| {
            assert_eq!(
                panel.active_pane.read(cx).items_len(),
                1,
                "Opening the same project reuses its viewer"
            );
            assert_eq!(
                panel
                    .active_pane
                    .read(cx)
                    .items_of_type::<LogcatView>()
                    .next()
                    .expect("View"),
                first_view
            );
            panel.new_view(None, window, cx);
            assert_eq!(panel.active_pane.read(cx).items_len(), 2);
            panel.new_view(Some(SplitDirection::Right), window, cx);
            assert_eq!(panel.center.panes().len(), 2);
            let second_pane = panel.active_pane.clone();
            assert_eq!(second_pane.read(cx).items_len(), 1);
            let second_view = second_pane
                .read(cx)
                .items_of_type::<LogcatView>()
                .next()
                .expect("Split view");
            assert_ne!(second_view, first_view);
            assert_eq!(second_view.read(cx).root(), first_view.read(cx).root());
            second_pane.update(cx, |_, cx| {
                cx.emit(pane::Event::Remove {
                    focus_on_pane: None,
                })
            });
        });
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| assert_eq!(panel.center.panes().len(), 1));
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.close_panel::<LogcatPanel>(window, cx);
            assert!(!workspace.bottom_dock().read(cx).is_open());
            super::super::android_logcat::open(
                workspace,
                PathBuf::from("/logcat"),
                None,
                Vec::new(),
                window,
                cx,
            );
            assert!(workspace.bottom_dock().read(cx).is_open());
            assert_eq!(workspace.active_pane(), &editor_pane);
            assert_eq!(editor_pane.read(cx).items_len(), 1);
        });
    }
}
