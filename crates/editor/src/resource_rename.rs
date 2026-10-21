use crate::{Editor, actions};
use futures::{FutureExt as _, channel::oneshot};
use gpui::{DismissEvent, Entity, EventEmitter, FocusHandle, Focusable};
use ui::{AlertModal, prelude::*};
use workspace::ModalView;

pub(crate) async fn await_preparation<T>(
    preparation: impl std::future::Future<Output = T>,
    cancelled: oneshot::Receiver<()>,
) -> Option<T> {
    let preparation = preparation.fuse();
    let cancelled = cancelled.fuse();
    futures::pin_mut!(preparation, cancelled);
    futures::select_biased! {
        _ = cancelled => None,
        result = preparation => Some(result),
    }
}

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
    async fn test_resource_rename_preparation_escape_and_repeat(cx: &mut TestAppContext) {
        let (_, review, _, mut cx) = open_review(cx);
        let editor = review.read_with(&cx, |review, _| review.preview.clone());
        for repeat in [false, true] {
            let (cancel, cancelled) = oneshot::channel();
            let (complete, pending) = oneshot::channel::<()>();
            editor.update(&mut cx, |editor, _| {
                editor.resource_rename_preparation = Some(cancel);
            });
            let mut preparation = Box::pin(await_preparation(pending, cancelled));
            assert!(preparation.as_mut().now_or_never().is_none());
            editor.update_in(&mut cx, |editor, window, cx| {
                if repeat {
                    drop(editor.rename(&actions::Rename, window, cx));
                } else {
                    editor.cancel(&actions::Cancel, window, cx);
                }
                assert!(editor.resource_rename_preparation.is_none());
            });
            assert!(preparation.await.is_none());
            assert!(
                complete.is_canceled(),
                "Cancelled preparation must be dropped"
            );
        }
    }

    #[gpui::test]
    async fn test_resource_rename_preparation_editor_release(cx: &mut TestAppContext) {
        let (_, _, _, mut cx) = open_review(cx);
        let (cancel, cancelled) = oneshot::channel();
        let editor = cx.update(|window, cx| {
            cx.new(|cx| {
                let mut editor = Editor::multi_line(window, cx);
                editor.resource_rename_preparation = Some(cancel);
                editor
            })
        });
        let (complete, pending) = oneshot::channel::<()>();
        let mut preparation = Box::pin(await_preparation(pending, cancelled));
        assert!(preparation.as_mut().now_or_never().is_none());
        drop(editor);
        cx.run_until_parked();
        assert!(preparation.await.is_none());
        assert!(complete.is_canceled());
    }

    #[gpui::test]
    async fn test_resource_rename_preparation_cancellation_wins_ready_result(
        cx: &mut TestAppContext,
    ) {
        let (_, _, _, _cx) = open_review(cx);
        let (cancel, cancelled) = oneshot::channel();
        drop(cancel);
        assert!(
            await_preparation(std::future::ready("stale preview"), cancelled)
                .await
                .is_none()
        );
        let (cancel, cancelled) = oneshot::channel();
        assert_eq!(
            await_preparation(std::future::ready("current preview"), cancelled).await,
            Some("current preview")
        );
        drop(cancel);
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

    #[gpui::test]
    async fn test_android_source_normal_symbol_confirm_rename_uses_lsp_without_resource_review(
        cx: &mut TestAppContext,
    ) {
        use crate::{
            Backspace, ConfirmRename, Rename, test::editor_lsp_test_context::EditorLspTestContext,
        };
        use futures::StreamExt as _;
        use language::{Language, LanguageConfig, LanguageMatcher};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        init_test(cx, |_| {});
        for (name, suffix, kind, text) in [
            ("AndroidRenameKotlin", "kt", "kotlin", "val loˇcal = 1;"),
            ("AndroidRenameJava", "java", "java", "int loˇcal = 1;"),
        ] {
            let language = Language::new(
                LanguageConfig {
                    name: name.into(),
                    matcher: Arc::new(LanguageMatcher {
                        path_suffixes: vec![suffix.into()],
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                None,
            );
            let mut test = EditorLspTestContext::new(
                language,
                lsp::ServerCapabilities {
                    rename_provider: Some(lsp::OneOf::Right(lsp::RenameOptions {
                        prepare_provider: Some(true),
                        work_done_progress_options: Default::default(),
                    })),
                    ..Default::default()
                },
                cx,
            )
            .await;
            test.set_state(text);
            let project = test
                .workspace
                .read_with(&test.cx.cx, |workspace, _| workspace.project().clone());
            let root = EditorLspTestContext::root_path().to_path_buf();
            let model = serde_json::from_value(serde_json::json!({
                "version": 1, "root": root, "diagnostics": [], "resourceModels": {},
                "modules": [{"path": ":app", "directory": root.join("dir"),
                    "namespace": "example.app", "kind": "application", "variants": [{
                        "name": "debug", "outputListing": null,
                        "components": [{"name": "debug", "scope": "main", "dependencies": [],
                            "sources": [{"path": root.join("dir"), "kind": kind, "generated": false}]}]
                    }]}]
            })).expect("Selected Android source model");
            project.update(&mut test.cx.cx, |project, cx| {
                let token = project.invalidate_android_model(Some(root), cx);
                project
                    .publish_android_model(&token, model, cx)
                    .expect("Publish Android model");
                project
                    .select_android_variant(
                        Some(
                            serde_json::from_value(serde_json::json!({
                                "module": ":app", "variant": "debug"
                            }))
                            .expect("Variant identity"),
                        ),
                        cx,
                    )
                    .expect("Select variant");
            });
            let mut prepare_handler = test
                .set_request_handler::<lsp::request::PrepareRenameRequest, _, _>(
                    |_, _, _| async move {
                        Ok(Some(lsp::PrepareRenameResponse::Range(lsp::Range::new(
                            lsp::Position::new(0, 4),
                            lsp::Position::new(0, 9),
                        ))))
                    },
                );
            let request_count = Arc::new(AtomicUsize::new(0));
            let mut rename_handler = test.set_request_handler::<lsp::request::Rename, _, _>({
                let count = request_count.clone();
                move |url, params, _| {
                    count.fetch_add(1, Ordering::SeqCst);
                    async move {
                        Ok(Some(lsp::WorkspaceEdit::new(
                            std::collections::HashMap::from_iter([(
                                url,
                                vec![lsp::TextEdit {
                                    range: lsp::Range::new(
                                        lsp::Position::new(0, 4),
                                        lsp::Position::new(0, 9),
                                    ),
                                    new_text: params.new_name,
                                }],
                            )]),
                        )))
                    }
                }
            });
            let prepare = test
                .update_editor(|editor, window, cx| editor.rename(&Rename, window, cx))
                .expect("Prepare ordinary rename");
            prepare_handler
                .next()
                .await
                .expect("Ordinary LSP prepare request");
            prepare.await.expect("Prepared editor rename");
            let rename_editor = test.editor(|editor, _, _| {
                editor
                    .pending_rename()
                    .expect("Pending ordinary rename")
                    .editor
                    .clone()
            });
            rename_editor.update_in(&mut test.cx.cx, |editor, window, cx| {
                editor.backspace(&Backspace, window, cx);
                editor.insert("renamed", window, cx);
            });
            let confirm = test
                .update_editor(|editor, window, cx| {
                    editor.confirm_rename(&ConfirmRename, window, cx)
                })
                .expect("Confirm ordinary Android-source symbol rename");
            rename_handler
                .next()
                .await
                .expect("Nonresource confirmation must reach LSP rather than a review modal");
            confirm.await.expect("Ordinary symbol rename completes");
            test.run_until_parked();
            assert_eq!(request_count.load(Ordering::SeqCst), 1, "{name}");
            assert_eq!(
                test.buffer_text(),
                text.replace('ˇ', "").replace("local", "renamed")
            );
            assert!(test.editor(|editor, _, _| editor.pending_rename().is_none()));
        }
    }
}
