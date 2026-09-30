use crate::{Command, humanize_action_name, normalize_action_query};
use command_palette_hooks::CommandPaletteFilter;
use editor::{Editor, SelectionEffects, scroll::Autoscroll};
use fuzzy_nucleo::{Case, LengthPenalty, StringMatchCandidate};
use gpui::{
    Action, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Render,
    Subscription, Task, TaskExt, WeakEntity,
};
use language::SymbolKind;
use picker::{Picker, PickerDelegate, PreviewUpdate};
use project::{Candidates, PathMatchCandidateSet, Project, ProjectPath, Symbol, WorktreeId};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use ui::{ListItem, ListItemSpacing, prelude::*};
use util::ResultExt;
use workspace::{ModalView, Workspace};

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Category {
    #[default]
    All,
    Classes,
    Files,
    Symbols,
    Actions,
}

impl Category {
    const ALL: [Self; 5] = [
        Self::All,
        Self::Classes,
        Self::Files,
        Self::Symbols,
        Self::Actions,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Classes => "Classes",
            Self::Files => "Files",
            Self::Symbols => "Symbols",
            Self::Actions => "Actions",
        }
    }
}

/// Searches project files, classes, symbols, and available IDE actions.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = search_everywhere)]
#[serde(default, deny_unknown_fields)]
struct Toggle {
    category: Category,
}

gpui::actions!(search_everywhere, [NextCategory, PreviousCategory]);

pub(super) fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _: &mut Context<Workspace>| {
        workspace.register_action(|workspace, action: &Toggle, window, cx| {
            if let Some(picker) = workspace
                .active_modal::<SearchEverywhere>(cx)
                .map(|modal| modal.read(cx).picker.clone())
            {
                picker.update(cx, |picker, cx| {
                    picker.delegate.category = action.category;
                    picker.refresh(window, cx);
                });
                return;
            }
            let Some(previous_focus) = window.focused(cx) else {
                return;
            };
            let filter = CommandPaletteFilter::try_global(cx);
            let commands = window
                .available_actions(cx)
                .into_iter()
                .filter(|action| !filter.is_some_and(|filter| filter.is_hidden(action.as_ref())))
                .map(|action| Command {
                    name: humanize_action_name(action.name()),
                    action,
                })
                .collect();
            let project = workspace.project().clone();
            let workspace_handle = cx.weak_entity();
            let category = action.category;
            workspace.toggle_modal(window, cx, move |window, cx| {
                let preview = picker_preview::editor_preview(project.clone(), window, cx);
                let picker = cx.new(|cx| {
                    Picker::uniform_list_with_preview(
                        EverywhereDelegate {
                            workspace: workspace_handle,
                            project,
                            previous_focus,
                            commands,
                            category,
                            matches: Vec::new(),
                            selected: 0,
                            loading_symbols: false,
                            symbol_error: None,
                            cancel: Arc::new(AtomicBool::new(false)),
                        },
                        preview,
                        window,
                        cx,
                    )
                });
                let subscription =
                    cx.subscribe(&picker, |_, _, _: &DismissEvent, cx| cx.emit(DismissEvent));
                SearchEverywhere {
                    picker,
                    _subscription: subscription,
                }
            });
        });
    })
    .detach();
}

struct SearchEverywhere {
    picker: Entity<Picker<EverywhereDelegate>>,
    _subscription: Subscription,
}
impl ModalView for SearchEverywhere {}
impl EventEmitter<DismissEvent> for SearchEverywhere {}
impl Focusable for SearchEverywhere {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}
impl Render for SearchEverywhere {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("SearchEverywhere")
            .on_action(cx.listener(|view, _: &NextCategory, window, cx| {
                view.picker
                    .update(cx, |picker, cx| change_category(picker, false, window, cx))
            }))
            .on_action(cx.listener(|view, _: &PreviousCategory, window, cx| {
                view.picker
                    .update(cx, |picker, cx| change_category(picker, true, window, cx))
            }))
            .child(self.picker.clone())
    }
}

#[derive(Clone)]
enum Target {
    Action(usize),
    File(ProjectPath),
    Symbol(Symbol),
}

struct SearchMatch {
    target: Target,
    label: String,
    detail: String,
    score: f64,
}

struct EverywhereDelegate {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    previous_focus: FocusHandle,
    commands: Vec<Command>,
    category: Category,
    matches: Vec<SearchMatch>,
    selected: usize,
    loading_symbols: bool,
    symbol_error: Option<String>,
    cancel: Arc<AtomicBool>,
}

fn is_class(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class
            | SymbolKind::Interface
            | SymbolKind::Enum
            | SymbolKind::Object
            | SymbolKind::Struct
    )
}

fn change_category(
    picker: &mut Picker<EverywhereDelegate>,
    backwards: bool,
    window: &mut Window,
    cx: &mut Context<Picker<EverywhereDelegate>>,
) {
    let index = Category::ALL
        .iter()
        .position(|category| *category == picker.delegate.category)
        .unwrap_or_default();
    let step = if backwards {
        Category::ALL.len() - 1
    } else {
        1
    };
    if let Some(category) = Category::ALL.get((index + step) % Category::ALL.len()) {
        picker.delegate.category = *category;
        picker.refresh(window, cx);
    }
}

impl PickerDelegate for EverywhereDelegate {
    type ListItem = ListItem;
    fn name() -> &'static str {
        "search everywhere"
    }
    fn placeholder_text(&self, _: &mut Window, _: &mut App) -> Arc<str> {
        "Search everywhere…".into()
    }
    fn match_count(&self) -> usize {
        self.matches.len()
    }
    fn selected_index(&self) -> usize {
        self.selected
    }
    fn set_selected_index(&mut self, index: usize, _: &mut Window, _: &mut Context<Picker<Self>>) {
        self.selected = index;
    }
    fn dismissed(&mut self, _: &mut Window, _: &mut Context<Picker<Self>>) {
        self.cancel.store(true, Ordering::Release);
    }
    fn render_header(&self, _: &mut Window, cx: &mut Context<Picker<Self>>) -> Option<AnyElement> {
        Some(
            h_flex()
                .key_context("SearchEverywhere")
                .gap_1()
                .p_2()
                .children(Category::ALL.into_iter().map(|category| {
                    Button::new(category.label(), category.label())
                        .tab_index(0isize)
                        .style(if category == self.category {
                            ButtonStyle::Filled
                        } else {
                            ButtonStyle::Subtle
                        })
                        .on_click(cx.listener(move |picker, _, window, cx| {
                            picker.delegate.category = category;
                            picker.refresh(window, cx);
                            window.focus(&picker.focus_handle(cx), cx);
                        }))
                }))
                .into_any_element(),
        )
    }
    fn render_footer(&self, _: &mut Window, _: &mut Context<Picker<Self>>) -> Option<AnyElement> {
        Some(
            h_flex()
                .p_2()
                .child(
                    Label::new(self.symbol_error.clone().unwrap_or_else(|| {
                        if self.loading_symbols {
                            "Searching symbols…".into()
                        } else {
                            "Enter to open · Esc to close".into()
                        }
                    }))
                    .size(LabelSize::Small)
                    .color(Color::Muted),
                )
                .into_any_element(),
        )
    }
    fn render_match(
        &self,
        index: usize,
        selected: bool,
        _: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) -> Option<ListItem> {
        let result = self.matches.get(index)?;
        Some(
            ListItem::new(index)
                .toggle_state(selected)
                .spacing(ListItemSpacing::Sparse)
                .child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .gap_4()
                        .child(Label::new(result.label.clone()).truncate())
                        .child(
                            Label::new(result.detail.clone())
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                                .truncate(),
                        ),
                ),
        )
    }
    fn try_get_preview_data_for_match(&self, cx: &App) -> Option<PreviewUpdate> {
        match &self.matches.get(self.selected)?.target {
            Target::File(path) => Some(PreviewUpdate::from_path(
                self.project.read(cx).absolute_path(path, cx)?,
            )),
            Target::Symbol(symbol) => Some(PreviewUpdate::from_symbol(symbol.clone())),
            Target::Action(_) => None,
        }
    }
    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        self.cancel.store(true, Ordering::Release);
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let category = self.category;
        let file_sets = if matches!(category, Category::All | Category::Files) {
            self.project
                .read(cx)
                .visible_worktrees(cx)
                .map(|worktree| PathMatchCandidateSet {
                    snapshot: worktree.read(cx).snapshot(),
                    include_ignored: false,
                    include_root_name: false,
                    candidates: Candidates::Files,
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let commands = if matches!(category, Category::All | Category::Actions) {
            self.commands
                .iter()
                .enumerate()
                .map(|(index, command)| StringMatchCandidate::new(index, command.name.clone()))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        self.symbol_error = None;
        self.loading_symbols = !query.is_empty()
            && matches!(
                category,
                Category::All | Category::Classes | Category::Symbols
            );
        let symbols = if self.loading_symbols {
            self.project
                .update(cx, |project, cx| project.symbols(&query, cx))
        } else {
            Task::ready(Ok(Vec::new()))
        };
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |picker, cx| {
            let files = fuzzy_nucleo::match_path_sets(
                &file_sets,
                &query,
                &None,
                Case::Smart,
                100,
                &cancel,
                executor.clone(),
            );
            let action_query = normalize_action_query(&query);
            let actions = fuzzy_nucleo::match_strings_async(
                &commands,
                &action_query,
                Case::Smart,
                LengthPenalty::On,
                100,
                &cancel,
                executor.clone(),
            );
            let (files, actions) = futures::join!(files, actions);
            let mut matches = files
                .into_iter()
                .map(|result| SearchMatch {
                    label: result
                        .path
                        .file_name()
                        .unwrap_or(result.path.as_unix_str())
                        .to_owned(),
                    detail: result.path.as_unix_str().to_owned(),
                    score: result.score,
                    target: Target::File(ProjectPath {
                        worktree_id: WorktreeId::from_usize(result.worktree_id),
                        path: result.path,
                    }),
                })
                .chain(actions.into_iter().map(|result| SearchMatch {
                    label: result.string.to_string(),
                    detail: "Action".into(),
                    score: result.score,
                    target: Target::Action(result.candidate_id),
                }))
                .collect::<Vec<_>>();
            matches.sort_by(|left, right| right.score.total_cmp(&left.score));
            picker
                .update_in(cx, |picker, _, cx| {
                    picker.delegate.matches = matches;
                    picker.delegate.selected = 0;
                    cx.notify();
                })
                .log_err();
            let symbols = symbols.await;
            let symbol_error = symbols
                .as_ref()
                .err()
                .map(|error| format!("Symbol search unavailable: {error}"));
            if let Err(error) = &symbols {
                log::error!("Search Everywhere symbols: {error:#}");
            }
            let symbols = symbols
                .unwrap_or_default()
                .into_iter()
                .filter(|symbol| category != Category::Classes || is_class(symbol.kind))
                .collect::<Vec<_>>();
            let candidates = symbols
                .iter()
                .enumerate()
                .map(|(index, symbol)| StringMatchCandidate::new(index, symbol.name.clone()))
                .collect::<Vec<_>>();
            let symbol_matches = fuzzy_nucleo::match_strings_async(
                &candidates,
                &query,
                Case::Smart,
                LengthPenalty::On,
                100,
                &cancel,
                executor,
            )
            .await;
            picker
                .update_in(cx, |picker, _, cx| {
                    for result in symbol_matches {
                        if let Some(symbol) = symbols.get(result.candidate_id) {
                            picker.delegate.matches.push(SearchMatch {
                                label: symbol.name.clone(),
                                detail: format!(
                                    "{:?} · {}",
                                    symbol.kind,
                                    symbol.container_name.as_deref().unwrap_or("project")
                                ),
                                score: result.score,
                                target: Target::Symbol(symbol.clone()),
                            });
                        }
                    }
                    picker
                        .delegate
                        .matches
                        .sort_by(|left, right| right.score.total_cmp(&left.score));
                    picker.delegate.matches.truncate(100);
                    picker.delegate.loading_symbols = false;
                    picker.delegate.symbol_error = symbol_error;
                    cx.notify();
                })
                .log_err();
        })
    }
    fn confirm(&mut self, secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(target) = self
            .matches
            .get(self.selected)
            .map(|result| result.target.clone())
        else {
            return;
        };
        cx.emit(DismissEvent);
        match target {
            Target::Action(index) => {
                if let Some(command) = self.commands.get(index) {
                    window.focus(&self.previous_focus, cx);
                    window.dispatch_action(command.action.boxed_clone(), cx);
                }
            }
            Target::File(path) => {
                self.workspace
                    .update(cx, |workspace, cx| {
                        let pane =
                            secondary.then(|| workspace.adjacent_pane(window, cx).downgrade());
                        workspace
                            .open_path(path, pane, true, window, cx)
                            .detach_and_log_err(cx);
                    })
                    .log_err();
            }
            Target::Symbol(symbol) => {
                let buffer = self.project.update(cx, |project, cx| {
                    project.open_buffer_for_symbol(&symbol, cx)
                });
                let workspace = self.workspace.clone();
                cx.spawn_in(window, async move |_, cx| {
                    let buffer = buffer.await?;
                    workspace.update_in(cx, |workspace, window, cx| {
                        let position = buffer
                            .read(cx)
                            .clip_point_utf16(symbol.range.start, editor::Bias::Left);
                        let pane = secondary.then(|| workspace.adjacent_pane(window, cx));
                        let editor = workspace.open_project_item::<Editor>(
                            pane, buffer, true, true, true, true, window, cx,
                        );
                        editor.update(cx, |editor, cx| {
                            editor.change_selections(
                                SelectionEffects::scroll(Autoscroll::center()),
                                window,
                                cx,
                                |selections| selections.select_ranges([position..position]),
                            )
                        });
                    })?;
                    anyhow::Ok(())
                })
                .detach_and_log_err(cx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor::test::editor_lsp_test_context::EditorLspTestContext;
    use gpui::{Modifiers, TestAppContext};
    use settings::KeymapFile;

    #[gpui::test]
    async fn double_shift_searches_files_actions_and_filters_classes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings = settings::SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            menu::init();
            crate::init(cx);
            for asset in ["keymaps/default-macos.json", "keymaps/macos/jetbrains.json"] {
                cx.bind_keys(
                    KeymapFile::load_asset_allow_partial_failure(asset, cx).expect("Keymap asset"),
                );
            }
        });
        let mut cx = EditorLspTestContext::new_rust(
            lsp::ServerCapabilities {
                workspace_symbol_provider: Some(lsp::OneOf::Left(true)),
                ..Default::default()
            },
            cx,
        )
        .await;
        cx.set_state("struct FileType;\nfn file_function() {}ˇ\n");
        let uri = cx.buffer_lsp_url.clone();
        cx.lsp
            .set_request_handler::<lsp::WorkspaceSymbolRequest, _, _>(move |_, _| {
                let uri = uri.clone();
                async move {
                    #[expect(deprecated)]
                    Ok(Some(lsp::WorkspaceSymbolResponse::Flat(vec![
                        lsp::SymbolInformation {
                            name: "FileType".into(),
                            kind: lsp::SymbolKind::STRUCT,
                            tags: None,
                            deprecated: None,
                            container_name: None,
                            location: lsp::Location {
                                uri: uri.clone(),
                                range: lsp::Range::new(
                                    lsp::Position::new(0, 7),
                                    lsp::Position::new(0, 15),
                                ),
                            },
                        },
                        lsp::SymbolInformation {
                            name: "file_function".into(),
                            kind: lsp::SymbolKind::FUNCTION,
                            tags: None,
                            deprecated: None,
                            container_name: None,
                            location: lsp::Location {
                                uri,
                                range: lsp::Range::new(
                                    lsp::Position::new(1, 3),
                                    lsp::Position::new(1, 16),
                                ),
                            },
                        },
                    ])))
                }
            });
        for _ in 0..2 {
            cx.simulate_modifiers_change(Modifiers::shift());
            cx.simulate_modifiers_change(Modifiers::none());
        }
        cx.run_until_parked();
        let workspace = cx.workspace.clone();
        let picker = workspace.read_with(&cx.cx.cx, |workspace, cx| {
            workspace
                .active_modal::<SearchEverywhere>(cx)
                .expect("Double Shift opens Search Everywhere")
                .read(cx)
                .picker
                .clone()
        });
        picker.update_in(&mut cx.cx.cx, |picker, window, cx| {
            picker.set_query("file", window, cx)
        });
        cx.run_until_parked();
        picker.read_with(&cx.cx.cx, |picker, _| {
            assert!(
                picker
                    .delegate
                    .matches
                    .iter()
                    .any(|result| matches!(result.target, Target::File(_)))
            );
            assert!(
                picker
                    .delegate
                    .matches
                    .iter()
                    .any(|result| matches!(result.target, Target::Symbol(_)))
            );
            assert!(
                picker
                    .delegate
                    .matches
                    .iter()
                    .any(|result| matches!(result.target, Target::Action(_)))
            );
        });
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        picker.read_with(&cx.cx.cx, |picker, _| {
            assert_eq!(picker.delegate.category, Category::Classes);
            assert_eq!(picker.delegate.matches.len(), 1);
            assert_eq!(
                picker.delegate.matches.first().expect("Class result").label,
                "FileType"
            );
        });
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(workspace.read_with(&cx.cx.cx, |workspace, cx| {
            workspace.active_modal::<SearchEverywhere>(cx).is_none()
        }));
        cx.assert_editor_state("struct ˇFileType;\nfn file_function() {}\n");
        cx.simulate_keystrokes("cmd-o");
        cx.run_until_parked();
        workspace.read_with(&cx.cx.cx, |workspace, cx| {
            let search = workspace
                .active_modal::<SearchEverywhere>(cx)
                .expect("Go to Class overrides the global Open shortcut");
            assert_eq!(
                search.read(cx).picker.read(cx).delegate.category,
                Category::Classes
            );
        });
    }
}
