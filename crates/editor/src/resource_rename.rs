use crate::{Editor, actions};
use futures::channel::oneshot;
use gpui::{DismissEvent, Entity, EventEmitter, FocusHandle, Focusable};
use ui::{AlertModal, prelude::*};
use workspace::ModalView;

pub(crate) struct ResourceRenameReview {
    preview: Entity<Editor>,
    decision: Option<oneshot::Sender<bool>>,
    focus: FocusHandle,
    cancel_focus: FocusHandle,
    apply_focus: FocusHandle,
}

impl ResourceRenameReview {
    pub(crate) fn new(
        preview: String,
        decision: oneshot::Sender<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let preview = cx.new(|cx| {
            let mut editor = Editor::multi_line(window, cx);
            editor.set_text(preview, window, cx);
            editor.set_read_only(true);
            editor
        });
        let focus = preview.focus_handle(cx);
        Self {
            preview,
            decision: Some(decision),
            focus,
            cancel_focus: cx.focus_handle(),
            apply_focus: cx.focus_handle(),
        }
    }

    fn decide(&mut self, apply: bool, cx: &mut Context<Self>) {
        if let Some(decision) = self.decision.take() {
            decision.send(apply).ok();
        }
        cx.emit(DismissEvent);
    }
}

impl Focusable for ResourceRenameReview {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<DismissEvent> for ResourceRenameReview {}
impl ModalView for ResourceRenameReview {}

impl Render for ResourceRenameReview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = (window.viewport_size().width - px(32.)).min(window.rem_size() * 48.);
        let height = (window.viewport_size().height - window.rem_size() * 18.)
            .max(px(48.))
            .min(window.rem_size() * 22.);
        div()
            .key_context("Menu")
            .track_focus(&self.focus)
            .capture_action(cx.listener(|this, _: &menu::Cancel, _, cx| this.decide(false, cx)))
            .capture_action(cx.listener(|this, _: &actions::Cancel, _, cx| this.decide(false, cx)))
            .capture_action(cx.listener(|this, _: &menu::Confirm, window, cx| {
                this.decide(!this.cancel_focus.is_focused(window), cx)
            }))
            .capture_action(cx.listener(|this, _: &menu::SelectNext, window, cx| {
                let focus = if this.focus.is_focused(window) {
                    &this.cancel_focus
                } else if this.cancel_focus.is_focused(window) {
                    &this.apply_focus
                } else {
                    &this.focus
                };
                focus.focus(window, cx);
            }))
            .capture_action(cx.listener(|this, _: &menu::SelectPrevious, window, cx| {
                let focus = if this.focus.is_focused(window) {
                    &this.apply_focus
                } else if this.apply_focus.is_focused(window) {
                    &this.cancel_focus
                } else {
                    &this.focus
                };
                focus.focus(window, cx);
            }))
            .child(AlertModal::new("android-resource-rename-review")
                .width(width)
                .title("Review Android resource rename")
                .child(Label::new("Edits cover catalogued variants and qualifiers. Apply keeps them unsaved and grouped for undo."))
                .child(div().debug_selector(|| "resource-rename-preview".into()).tab_index(0).track_focus(&self.focus).h(height).w_full().child(self.preview.clone()))
                .footer(h_flex().debug_selector(|| "resource-rename-footer".into()).p_3().gap_2().justify_end()
                    .child(Button::new("cancel-resource-rename", "Cancel").tab_index(1_isize).track_focus(&self.cancel_focus).on_click(cx.listener(|this, _, _, cx| this.decide(false, cx))))
                    .child(Button::new("apply-resource-rename", "Apply edits").tab_index(2_isize).track_focus(&self.apply_focus).on_click(cx.listener(|this, _, _, cx| this.decide(true, cx))))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor_tests::init_test;
    use gpui::{KeyBinding, TestAppContext, VisualTestContext, point, size};
    use workspace::ModalLayer;

    fn open_review(
        cx: &mut TestAppContext,
    ) -> (
        Entity<ModalLayer>,
        Entity<ResourceRenameReview>,
        oneshot::Receiver<bool>,
        VisualTestContext,
    ) {
        init_test(cx, |_| {});
        cx.update(|cx| {
            cx.bind_keys([
                KeyBinding::new("enter", menu::Confirm, Some("Menu")),
                KeyBinding::new("escape", menu::Cancel, Some("Menu")),
                KeyBinding::new("tab", menu::SelectNext, Some("Menu")),
                KeyBinding::new("shift-tab", menu::SelectPrevious, Some("Menu")),
            ]);
        });
        let window = cx.open_window(size(px(1024.), px(768.)), |_, _| ModalLayer::new());
        let layer = window.root(cx).expect("Modal layer");
        let mut cx = VisualTestContext::from_window(*window, cx);
        let (decision, answer) = oneshot::channel();
        layer.update_in(&mut cx, |layer, window, cx| {
            layer.toggle_modal(window, cx, |window, cx| {
                ResourceRenameReview::new(
                    "strings.xml (1 edit)\n  1: title → renamed_title\n".into(),
                    decision,
                    window,
                    cx,
                )
            });
        });
        cx.run_until_parked();
        let review = layer.read_with(&cx, |layer, _| {
            layer
                .active_modal::<ResourceRenameReview>()
                .expect("Review modal")
        });
        (layer, review, answer, cx)
    }

    #[gpui::test]
    fn test_resource_rename_review_keyboard_cancel(cx: &mut TestAppContext) {
        let (layer, review, mut answer, mut cx) = open_review(cx);
        cx.update(|window, cx| {
            let review = review.read(cx);
            assert!(review.preview.focus_handle(cx).is_focused(window));
            assert!(review.preview.read(cx).read_only(cx));
        });
        cx.simulate_input("changed");
        review.read_with(&cx, |review, cx| {
            assert_eq!(
                review.preview.read(cx).text(cx),
                "strings.xml (1 edit)\n  1: title → renamed_title\n"
            );
        });
        assert_eq!(answer.try_recv().expect("Pending decision"), None);
        cx.simulate_keystrokes("tab");
        cx.update(|window, cx| assert!(review.read(cx).cancel_focus.is_focused(window)));
        cx.simulate_keystrokes("enter");
        assert_eq!(answer.try_recv().expect("Cancel decision"), Some(false));
        layer.read_with(&cx, |layer, _| assert!(!layer.has_active_modal()));
    }

    #[gpui::test]
    fn test_resource_rename_review_keyboard_apply(cx: &mut TestAppContext) {
        let (layer, review, mut answer, mut cx) = open_review(cx);
        cx.simulate_keystrokes("tab tab");
        cx.update(|window, cx| assert!(review.read(cx).apply_focus.is_focused(window)));
        assert_eq!(answer.try_recv().expect("Pending decision"), None);
        cx.simulate_keystrokes("tab");
        cx.update(|window, cx| assert!(review.read(cx).focus.is_focused(window)));
        cx.simulate_keystrokes("shift-tab");
        cx.update(|window, cx| assert!(review.read(cx).apply_focus.is_focused(window)));
        cx.simulate_keystrokes("shift-tab");
        cx.update(|window, cx| assert!(review.read(cx).cancel_focus.is_focused(window)));
        cx.simulate_keystrokes("tab enter");
        assert_eq!(answer.try_recv().expect("Apply decision"), Some(true));
        layer.read_with(&cx, |layer, _| assert!(!layer.has_active_modal()));
    }

    #[gpui::test]
    fn test_resource_rename_review_escape(cx: &mut TestAppContext) {
        let (layer, _, mut answer, mut cx) = open_review(cx);
        cx.simulate_keystrokes("escape");
        assert_eq!(answer.try_recv().expect("Cancel decision"), Some(false));
        layer.read_with(&cx, |layer, _| assert!(!layer.has_active_modal()));
    }

    #[gpui::test]
    fn test_resource_rename_review_outside_dismiss(cx: &mut TestAppContext) {
        let (layer, review, mut answer, mut cx) = open_review(cx);
        drop(review);
        cx.simulate_mouse_down(
            point(px(4.), px(4.)),
            gpui::MouseButton::Left,
            gpui::Modifiers::none(),
        );
        cx.run_until_parked();
        assert!(answer.try_recv().is_err());
        layer.read_with(&cx, |layer, _| assert!(!layer.has_active_modal()));
    }

    #[gpui::test]
    fn test_resource_rename_review_resizes_with_viewport(cx: &mut TestAppContext) {
        let (_, _, _, mut cx) = open_review(cx);
        let initial = cx
            .debug_bounds("resource-rename-preview")
            .expect("Preview bounds");
        for viewport in [size(px(520.), px(480.)), size(px(800.), px(600.))] {
            cx.simulate_resize(viewport);
            cx.run_until_parked();
            let preview = cx
                .debug_bounds("resource-rename-preview")
                .expect("Preview bounds");
            let footer = cx
                .debug_bounds("resource-rename-footer")
                .expect("Footer bounds");
            assert!(preview.size.width <= viewport.width - px(32.));
            assert!(preview.size.height <= initial.size.height);
            if viewport.height == px(480.) {
                assert!(preview.size.height < initial.size.height);
            }
            assert!(
                footer.bottom() <= viewport.height,
                "Footer must remain visible after resizing: {footer:?}, viewport: {viewport:?}"
            );
        }
    }
}
