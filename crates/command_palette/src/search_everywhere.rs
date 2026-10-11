use crate::{Command, humanize_action_name, normalize_action_query};
use command_palette_hooks::CommandPaletteFilter;
use editor::{Editor, SelectionEffects, scroll::Autoscroll};
use file_icons::FileIcons;
use fuzzy_nucleo::{Case, LengthPenalty, StringMatchCandidate};
use gpui::{
    Action, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Render,
    Subscription, Task, TaskExt, WeakEntity,
};
use language::SymbolKind;
use picker::{ErasedEditor, Picker, PickerDelegate, PreviewUpdate};
use project::{
    Candidates, PathMatchCandidateSet, Project, ProjectPath, Symbol, WorktreeId,
    lsp_store::SymbolLocation,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use ui::{
    ButtonLike, Divider, HighlightedLabel, KeyBinding, ListItem, ListItemSpacing, TintColor,
    prelude::*,
};
use util::ResultExt;
use workspace::{ModalView, Workspace};

const MAX_RESULTS: usize = 100;

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
                    picker.refresh_placeholder(window, cx);
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
                    name: humanize_action_name(action.name()).into(),
                    action,
                    usage: None,
                })
                .collect();
            let project = workspace.project().clone();
            let selected_files = workspace
                .panes()
                .iter()
                .filter_map(|pane| pane.read(cx).active_item()?.project_path(cx))
                .collect();
            let recent_files = workspace
                .active_item(cx)
                .and_then(|item| item.project_path(cx))
                .into_iter()
                .chain(
                    workspace
                        .recent_navigation_history(Some(MAX_RESULTS), cx)
                        .into_iter()
                        .map(|(path, _)| path),
                )
                .filter(|path| {
                    project
                        .read(cx)
                        .entry_for_path(path, cx)
                        .is_some_and(|entry| entry.is_file() && !entry.is_ignored)
                })
                .fold(Vec::new(), |mut paths, path| {
                    if paths.len() < MAX_RESULTS && !paths.contains(&path) {
                        paths.push(path);
                    }
                    paths
                });
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
                            recent_files,
                            selected_files,
                            category,
                            matches: Vec::new(),
                            selected: 0,
                            selection_changed: false,
                            query: String::new(),
                            loading_symbols: false,
                            symbol_error: None,
                            cancel: Arc::new(AtomicBool::new(false)),
                        },
                        preview,
                        window,
                        cx,
                    )
                    .initial_width(rems(48.))
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

impl Target {
    fn same_target(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Action(left), Self::Action(right)) => left == right,
            (Self::File(left), Self::File(right)) => left == right,
            (Self::Symbol(left), Self::Symbol(right)) => {
                left.source_language_server_id == right.source_language_server_id
                    && left.path == right.path
                    && left.range == right.range
            }
            _ => false,
        }
    }
}

struct SearchMatch {
    category: Category,
    target: Target,
    label: String,
    detail: String,
    score: f64,
    name_match: bool,
    exact_match: bool,
    positions: Vec<usize>,
    detail_positions: Vec<usize>,
    recent_index: Option<usize>,
}

impl SearchMatch {
    fn contributor_weight(&self) -> usize {
        match &self.target {
            Target::File(_) if self.recent_index.is_some() => 70,
            Target::Symbol(symbol)
                if self.category != Category::Symbols && is_class(symbol.kind) =>
            {
                100
            }
            Target::File(_) => 200,
            Target::Symbol(_) => 300,
            Target::Action(_) => 400,
        }
    }

    fn priority(&self) -> f64 {
        let bonus = match &self.target {
            Target::File(_) if self.recent_index.is_some() => 7.,
            Target::Symbol(symbol)
                if self.category != Category::Symbols && is_class(symbol.kind) =>
            {
                5.
            }
            Target::File(_) => 2.,
            _ => 0.,
        };
        self.score + bonus
    }

    fn compare(&self, other: &Self) -> std::cmp::Ordering {
        other
            .exact_match
            .cmp(&self.exact_match)
            .then_with(|| other.name_match.cmp(&self.name_match))
            .then_with(|| other.priority().total_cmp(&self.priority()))
            .then_with(|| self.contributor_weight().cmp(&other.contributor_weight()))
            .then_with(|| self.recent_index.cmp(&other.recent_index))
            .then_with(|| self.label.cmp(&other.label))
            .then_with(|| self.detail.cmp(&other.detail))
            .then_with(|| match (&self.target, &other.target) {
                (Target::File(left), Target::File(right)) => {
                    left.worktree_id.cmp(&right.worktree_id)
                }
                (Target::Symbol(left), Target::Symbol(right)) => left
                    .source_worktree_id
                    .cmp(&right.source_worktree_id)
                    .then_with(|| left.range.start.0.cmp(&right.range.start.0))
                    .then_with(|| left.range.end.0.cmp(&right.range.end.0)),
                _ => std::cmp::Ordering::Equal,
            })
    }
}

fn exact_name_match(label: &str, query: &str, file: bool) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return false;
    }
    label.to_lowercase() == query
        || (file
            && std::path::Path::new(label)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| stem.to_lowercase() == query))
}

fn symbol_icon(kind: SymbolKind) -> (IconName, Color) {
    match kind {
        SymbolKind::Class | SymbolKind::Struct | SymbolKind::Object => {
            (IconName::Box, Color::Accent)
        }
        SymbolKind::Interface | SymbolKind::TypeParameter => (IconName::Blocks, Color::Accent),
        SymbolKind::Method | SymbolKind::Function | SymbolKind::Constructor => {
            (IconName::Code, Color::Info)
        }
        SymbolKind::Enum | SymbolKind::EnumMember => (IconName::ListTree, Color::Warning),
        SymbolKind::Module | SymbolKind::Namespace | SymbolKind::Package => {
            (IconName::Folder, Color::Muted)
        }
        SymbolKind::Field | SymbolKind::Property | SymbolKind::Variable | SymbolKind::Constant => {
            (IconName::SquareDot, Color::Info)
        }
        SymbolKind::Event => (IconName::BoltOutlined, Color::Warning),
        SymbolKind::File => (IconName::File, Color::Muted),
        _ => (IconName::Hash, Color::Muted),
    }
}

fn file_detail(path: &ProjectPath, worktree_names: &[(WorktreeId, String)]) -> String {
    let relative = path.path.as_unix_str();
    if (worktree_names.len() > 1 || relative.is_empty())
        && let Some((_, root)) = worktree_names
            .iter()
            .find(|(id, _)| *id == path.worktree_id)
    {
        if relative.is_empty() {
            root.clone()
        } else {
            format!("{root}/{relative}")
        }
    } else {
        relative.to_owned()
    }
}

fn symbol_detail(symbol: &Symbol, worktree_names: &[(WorktreeId, String)]) -> String {
    let path = match &symbol.path {
        SymbolLocation::InProject(path) => file_detail(path, worktree_names),
        SymbolLocation::OutsideProject { abs_path, .. } => abs_path.to_string_lossy().into_owned(),
    };
    match &symbol.container_name {
        Some(container) => format!("{container} · {path}"),
        None => path,
    }
}

fn rank_matches(
    mut matches: Vec<SearchMatch>,
    selected_target: Option<Target>,
) -> (Vec<SearchMatch>, usize) {
    matches.sort_by(SearchMatch::compare);
    let mut ranked = Vec::<SearchMatch>::with_capacity(MAX_RESULTS);
    let mut selected_match = None;
    let mut selected_retained = false;
    for result in matches.drain(..) {
        if ranked.len() < MAX_RESULTS {
            if !ranked
                .iter()
                .any(|existing| existing.target.same_target(&result.target))
            {
                selected_retained |= selected_target
                    .as_ref()
                    .is_some_and(|target| target.same_target(&result.target));
                ranked.push(result);
            }
        } else if selected_retained {
            break;
        } else if let Some(target) = &selected_target {
            if target.same_target(&result.target) {
                selected_match = Some(result);
                break;
            }
        } else {
            break;
        }
    }
    // A navigated target must survive late results even when
    // those results displace it past the visible result limit.
    if let Some(result) = selected_match {
        ranked.pop();
        ranked.push(result);
    }
    let selected = selected_target
        .and_then(|target| {
            ranked
                .iter()
                .position(|result| target.same_target(&result.target))
        })
        .unwrap_or(0);
    (ranked, selected)
}

struct EverywhereDelegate {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    previous_focus: FocusHandle,
    commands: Vec<Command>,
    recent_files: Vec<ProjectPath>,
    selected_files: Vec<ProjectPath>,
    category: Category,
    matches: Vec<SearchMatch>,
    selected: usize,
    selection_changed: bool,
    query: String,
    loading_symbols: bool,
    symbol_error: Option<String>,
    cancel: Arc<AtomicBool>,
}

impl EverywhereDelegate {
    fn append_matches(&mut self, matches: impl IntoIterator<Item = SearchMatch>) {
        let selected_target = self
            .selection_changed
            .then(|| {
                self.matches
                    .get(self.selected)
                    .map(|result| result.target.clone())
            })
            .flatten();
        let mut combined = std::mem::take(&mut self.matches);
        combined.extend(matches);
        (self.matches, self.selected) = rank_matches(combined, selected_target);
    }
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
        picker.refresh_placeholder(window, cx);
        picker.refresh(window, cx);
    }
}

impl PickerDelegate for EverywhereDelegate {
    type ListItem = ListItem;
    fn name() -> &'static str {
        "search everywhere"
    }
    fn placeholder_text(&self, _: &mut Window, _: &mut App) -> Arc<str> {
        match self.category {
            Category::All => "Search everywhere…",
            Category::Classes => "Search classes…",
            Category::Files => "Search files…",
            Category::Symbols => "Search symbols…",
            Category::Actions => "Search actions…",
        }
        .into()
    }
    fn match_count(&self) -> usize {
        self.matches.len()
    }
    fn selected_index(&self) -> usize {
        self.selected
    }
    fn set_selected_index(&mut self, index: usize, _: &mut Window, _: &mut Context<Picker<Self>>) {
        if index < self.matches.len() {
            self.selected = index;
            self.selection_changed = true;
        }
    }
    fn dismissed(&mut self, _: &mut Window, _: &mut Context<Picker<Self>>) {
        self.cancel.store(true, Ordering::Release);
    }
    fn retain_pending_confirmation(&self) -> bool {
        // A queued Enter belongs to this query and category.
        false
    }
    fn finalize_update_matches(
        &mut self,
        _: String,
        _: std::time::Duration,
        _: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) -> bool {
        // Visible results belong to the current query and can open immediately,
        // even while another contributor is still searching.
        self.matches.get(self.selected).is_some()
    }
    fn render_editor(
        &self,
        editor: &Arc<dyn ErasedEditor>,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<gpui::Div> {
        Some(
            v_flex()
                .flex_none()
                .child(
                    h_flex()
                        .px_2()
                        .py_1()
                        .gap_1()
                        .children(Category::ALL.into_iter().map(|category| {
                            let selected = category == self.category;
                            div()
                                .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
                                    window.prevent_default();
                                })
                                .child(
                                    ButtonLike::new_rounded_all(category.label())
                                        .style(ButtonStyle::OutlinedCustom(
                                            gpui::transparent_black(),
                                        ))
                                        .selected_style(ButtonStyle::Tinted(TintColor::Accent))
                                        .size(ButtonSize::Medium)
                                        .aria_label(category.label())
                                        .toggle_state(selected)
                                        .child(Label::new(category.label()).color(if selected {
                                            Color::Default
                                        } else {
                                            Color::Muted
                                        }))
                                        .on_click(cx.listener(move |picker, _, window, cx| {
                                            picker.delegate.category = category;
                                            picker.refresh_placeholder(window, cx);
                                            picker.refresh(window, cx);
                                            window.focus(&picker.focus_handle(cx), cx);
                                        })),
                                )
                        })),
                )
                .child(Divider::horizontal())
                .child(
                    h_flex()
                        .h_10()
                        .px_3()
                        .gap_2()
                        .child(Icon::new(IconName::MagnifyingGlass).color(Color::Muted))
                        .child(div().flex_1().min_w_0().child(editor.render(window, cx))),
                )
                .child(Divider::horizontal()),
        )
    }
    fn no_matches_text(&self, _: &mut Window, _: &mut App) -> Option<SharedString> {
        Some(if self.loading_symbols {
            "Searching symbols…".into()
        } else if self.query.is_empty() {
            match self.category {
                Category::All => "Start typing to search files, classes, symbols, and actions",
                Category::Files => "No recent files. Type a file name to search the project",
                Category::Classes => "Type a class name to search the project",
                Category::Symbols => "Type a symbol name to search the project",
                Category::Actions => "No available actions",
            }
            .into()
        } else {
            format!("No results for ‘{}’", self.query).into()
        })
    }
    fn render_footer(&self, _: &mut Window, _: &mut Context<Picker<Self>>) -> Option<AnyElement> {
        Some(
            h_flex()
                .justify_between()
                .gap_2()
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
                    .color(if self.symbol_error.is_some() {
                        Color::Error
                    } else {
                        Color::Muted
                    })
                    .truncate(),
                )
                .child(
                    Label::new("Tab to switch category")
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
        cx: &mut Context<Picker<Self>>,
    ) -> Option<ListItem> {
        let result = self.matches.get(index)?;
        let icon = match &result.target {
            Target::File(_) => FileIcons::get_icon(std::path::Path::new(&result.label), cx)
                .map(|path| Icon::from_path(path).color(Color::Muted))
                .unwrap_or_else(|| Icon::new(IconName::File).color(Color::Muted)),
            Target::Symbol(symbol) => {
                let (icon, color) = symbol_icon(symbol.kind);
                Icon::new(icon).color(color)
            }
            Target::Action(_) => Icon::new(IconName::Command).color(Color::Muted),
        };
        Some(
            ListItem::new(index)
                .toggle_state(selected)
                .aria_role(gpui::Role::ListBoxOption)
                .aria_label(format!("{} {}", result.label, result.detail))
                .when(selected, |item| item.aria_active_descendant())
                .spacing(ListItemSpacing::Dense)
                .start_slot(icon)
                .child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .min_w_0()
                        .gap_3()
                        .child(
                            HighlightedLabel::new(result.label.clone(), result.positions.clone())
                                .truncate(),
                        )
                        .child(
                            div().flex_1().min_w_0().flex().justify_end().child(
                                HighlightedLabel::new(
                                    result.detail.clone(),
                                    result.detail_positions.clone(),
                                )
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                                .truncate_start(),
                            ),
                        )
                        .when_some(
                            match &result.target {
                                Target::Action(index) => self.commands.get(*index),
                                _ => None,
                            },
                            |row, command| {
                                row.child(KeyBinding::for_action_in(
                                    command.action.as_ref(),
                                    &self.previous_focus,
                                    cx,
                                ))
                            },
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
        let query = query.trim().to_owned();
        self.query = query.clone();
        self.matches.clear();
        self.selected = 0;
        self.selection_changed = false;
        self.symbol_error = None;
        self.loading_symbols = !query.is_empty()
            && matches!(
                category,
                Category::All | Category::Classes | Category::Symbols
            );
        let mut recent_files = self.recent_files.clone();
        let worktree_names = self
            .project
            .read(cx)
            .visible_worktrees(cx)
            .map(|worktree| {
                let worktree = worktree.read(cx);
                (worktree.id(), worktree.root_name().as_unix_str().to_owned())
            })
            .collect::<Vec<_>>();
        if query.is_empty() && matches!(category, Category::All | Category::Files) {
            self.matches = recent_files
                .iter()
                .enumerate()
                .filter(|(_, path)| self.project.read(cx).entry_for_path(path, cx).is_some())
                .map(|(index, path)| SearchMatch {
                    category,
                    target: Target::File(path.clone()),
                    label: path
                        .path
                        .file_name()
                        .or_else(|| {
                            worktree_names
                                .iter()
                                .find(|(id, _)| *id == path.worktree_id)
                                .map(|(_, name)| name.as_str())
                        })
                        .unwrap_or(path.path.as_unix_str())
                        .to_owned(),
                    detail: file_detail(path, &worktree_names),
                    score: -(index as f64),
                    name_match: true,
                    exact_match: false,
                    positions: Vec::new(),
                    detail_positions: Vec::new(),
                    recent_index: Some(index),
                })
                .collect();
            return Task::ready(());
        }
        // Studio promotes history entries only when they are not selected
        // in an editor pane; selected files still match as ordinary files.
        recent_files.retain(|path| !self.selected_files.contains(path));
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
        let symbols = if self.loading_symbols {
            self.project
                .update(cx, |project, cx| project.symbols(&query, cx))
        } else {
            Task::ready(Ok(Vec::new()))
        };
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |picker, cx| {
            let name_sets = file_sets
                .iter()
                .map(|set| PathMatchCandidateSet {
                    snapshot: set.snapshot.clone(),
                    include_ignored: false,
                    include_root_name: false,
                    candidates: Candidates::Files,
                })
                .collect::<Vec<_>>();
            let names = executor.spawn({
                let executor = executor.clone();
                let query = query.clone();
                let cancel = cancel.clone();
                let recent_files = recent_files.clone();
                let worktree_names = worktree_names.clone();
                async move {
                    let mut paths = Vec::new();
                    let mut candidates = Vec::new();
                    let mut exact_paths = Vec::new();
                    let path_query = query.contains('/').then(|| query.to_lowercase());
                    for set in &name_sets {
                        for entry in set.snapshot.files(false, 0) {
                            if cancel.load(Ordering::Acquire) {
                                return Vec::new();
                            }
                            let label = entry
                                .path
                                .file_name()
                                .unwrap_or(set.snapshot.root_name().as_unix_str());
                            candidates
                                .push(StringMatchCandidate::new(paths.len(), label.to_owned()));
                            if path_query.as_ref().is_some_and(|query| {
                                entry.path.as_unix_str().to_lowercase() == *query
                            }) {
                                let path = ProjectPath {
                                    worktree_id: set.snapshot.id(),
                                    path: entry.path.clone(),
                                };
                                let detail = file_detail(&path, &worktree_names);
                                let prefix_len =
                                    detail.len().saturating_sub(entry.path.as_unix_str().len());
                                exact_paths.push(SearchMatch {
                                    category,
                                    target: Target::File(path.clone()),
                                    label: label.to_owned(),
                                    detail,
                                    score: 0.,
                                    name_match: false,
                                    exact_match: true,
                                    positions: label
                                        .char_indices()
                                        .map(|(index, _)| index)
                                        .collect(),
                                    detail_positions: entry
                                        .path
                                        .as_unix_str()
                                        .char_indices()
                                        .map(|(index, _)| index + prefix_len)
                                        .collect(),
                                    recent_index: recent_files
                                        .iter()
                                        .position(|recent| *recent == path),
                                });
                            }
                            paths.push(ProjectPath {
                                worktree_id: set.snapshot.id(),
                                path: entry.path.clone(),
                            });
                        }
                    }
                    // Name matches are ranked before the result limit. A deep
                    // exact filename must not be lost to shallow path matches.
                    let names = fuzzy_nucleo::match_strings_async(
                        &candidates,
                        &query,
                        Case::Ignore,
                        LengthPenalty::On,
                        candidates.len(),
                        &cancel,
                        executor,
                    )
                    .await;
                    let mut matches = names
                        .into_iter()
                        .filter_map(|result| {
                            let path = paths.get(result.candidate_id)?.clone();
                            Some(SearchMatch {
                                category,
                                exact_match: exact_name_match(&result.string, &query, true),
                                label: result.string.to_string(),
                                detail: file_detail(&path, &worktree_names),
                                score: result.score,
                                name_match: true,
                                positions: result.positions,
                                detail_positions: Vec::new(),
                                recent_index: recent_files
                                    .iter()
                                    .position(|recent| *recent == path),
                                target: Target::File(path),
                            })
                        })
                        .collect::<Vec<_>>();
                    matches.extend(exact_paths);
                    rank_matches(matches, None).0
                }
            });
            let files = fuzzy_nucleo::match_path_sets(
                &file_sets,
                &query,
                &None,
                Case::Ignore,
                MAX_RESULTS,
                &cancel,
                executor.clone(),
            );
            let action_query = normalize_action_query(&query);
            let actions = fuzzy_nucleo::match_strings_async(
                &commands,
                &action_query,
                Case::Ignore,
                LengthPenalty::On,
                commands.len(),
                &cancel,
                executor.clone(),
            );
            let (names, files, actions) = futures::join!(names, files, actions);
            if cancel.load(Ordering::Acquire) {
                return;
            }
            let mut matches = names;
            matches.extend(files.into_iter().map(|result| {
                let label = result
                    .path
                    .file_name()
                    .unwrap_or(result.path.as_unix_str())
                    .to_owned();
                let path = ProjectPath {
                    worktree_id: WorktreeId::from_usize(result.worktree_id),
                    path: file_sets
                        .iter()
                        .find(|set| set.snapshot.id().to_usize() == result.worktree_id)
                        .and_then(|set| set.snapshot.root_entry().filter(|entry| entry.is_file()))
                        .map(|entry| entry.path.clone())
                        .unwrap_or(result.path),
                };
                let detail = file_detail(&path, &worktree_names);
                let prefix_len = if path.path.is_empty() {
                    0
                } else {
                    detail.len().saturating_sub(path.path.as_unix_str().len())
                };
                SearchMatch {
                    category,
                    exact_match: exact_name_match(path.path.as_unix_str(), &query, false),
                    label,
                    detail,
                    score: result.score,
                    name_match: false,
                    positions: Vec::new(),
                    detail_positions: result
                        .positions
                        .into_iter()
                        .map(|position| position + prefix_len)
                        .collect(),
                    recent_index: recent_files.iter().position(|recent| *recent == path),
                    target: Target::File(path),
                }
            }));
            matches.extend(actions.into_iter().map(|result| SearchMatch {
                category,
                exact_match: exact_name_match(&result.string, &action_query, false),
                label: result.string.to_string(),
                detail: "Action".into(),
                score: result.score,
                name_match: true,
                positions: result.positions,
                detail_positions: Vec::new(),
                recent_index: None,
                target: Target::Action(result.candidate_id),
            }));
            picker
                .update_in(cx, |picker, _, cx| {
                    picker.delegate.append_matches(matches);
                    cx.notify();
                })
                .log_err();
            let symbols = symbols.await;
            if cancel.load(Ordering::Acquire) {
                return;
            }
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
                Case::Ignore,
                LengthPenalty::On,
                candidates.len(),
                &cancel,
                executor.clone(),
            )
            .await;
            if cancel.load(Ordering::Acquire) {
                return;
            }
            let symbol_matches = executor
                .spawn({
                    let cancel = cancel.clone();
                    async move {
                        let mut matches = Vec::new();
                        for result in symbol_matches {
                            if cancel.load(Ordering::Acquire) {
                                return Vec::new();
                            }
                            if let Some(symbol) = symbols.get(result.candidate_id) {
                                matches.push(SearchMatch {
                                    category,
                                    exact_match: exact_name_match(&symbol.name, &query, false),
                                    label: symbol.name.clone(),
                                    detail: symbol_detail(symbol, &worktree_names),
                                    score: result.score,
                                    name_match: true,
                                    positions: result.positions,
                                    detail_positions: Vec::new(),
                                    recent_index: None,
                                    target: Target::Symbol(symbol.clone()),
                                });
                            }
                        }
                        rank_matches(matches, None).0
                    }
                })
                .await;
            if cancel.load(Ordering::Acquire) {
                return;
            }
            picker
                .update_in(cx, |picker, _, cx| {
                    picker.delegate.append_matches(symbol_matches);
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
        self.cancel.store(true, Ordering::Release);
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

    fn initialize_test(cx: &mut TestAppContext) {
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
    }

    #[test]
    fn exact_names_include_file_stems_and_unicode() {
        assert!(exact_name_match("MainActivity.kt", "MainActivity", true));
        assert!(exact_name_match("MainActivity.kt", "mainactivity.kt", true));
        assert!(exact_name_match("Écran.kt", "écran", true));
        assert!(!exact_name_match(
            "MainActivityTest.kt",
            "MainActivity",
            true
        ));
        assert!(!exact_name_match("MainActivity.kt", "MainActivity", false));
        assert!(!exact_name_match("MainActivity.kt", " ", true));
    }

    #[test]
    fn ranking_prefers_exact_names_then_name_matches_and_breaks_ties() {
        let result = |index, label: &str, score, name_match, exact_match| SearchMatch {
            category: Category::Actions,
            target: Target::Action(index),
            label: label.into(),
            detail: String::new(),
            score,
            name_match,
            exact_match,
            positions: Vec::new(),
            detail_positions: Vec::new(),
            recent_index: None,
        };
        let mut results = [
            result(0, "Directory match", 1000., false, false),
            result(1, "Name match", 100., true, false),
            result(2, "Exact name", 50., true, true),
            result(3, "Another name", 100., true, false),
        ];
        results.sort_by(SearchMatch::compare);
        assert_eq!(
            results
                .iter()
                .map(|result| result.label.as_str())
                .collect::<Vec<_>>(),
            [
                "Exact name",
                "Another name",
                "Name match",
                "Directory match"
            ]
        );
    }

    #[test]
    fn result_limit_preserves_navigation_without_duplicate_rows() {
        let result = |index, score| SearchMatch {
            category: Category::Actions,
            target: Target::Action(index),
            label: format!("Action {index}"),
            detail: String::new(),
            score,
            name_match: true,
            exact_match: false,
            positions: Vec::new(),
            detail_positions: Vec::new(),
            recent_index: None,
        };
        let results = || {
            (0..MAX_RESULTS + 10)
                .map(|index| result(index, -(index as f64)))
                .collect::<Vec<_>>()
        };
        let (ranked, selected) = rank_matches(results(), Some(Target::Action(MAX_RESULTS + 9)));
        assert_eq!(ranked.len(), MAX_RESULTS);
        assert_eq!(selected, MAX_RESULTS - 1);
        assert!(
            ranked[selected]
                .target
                .same_target(&Target::Action(MAX_RESULTS + 9))
        );

        let mut duplicates = results();
        duplicates.push(result(0, -1000.));
        let (ranked, selected) = rank_matches(duplicates, Some(Target::Action(0)));
        assert_eq!(ranked.len(), MAX_RESULTS);
        assert_eq!(selected, 0);
        assert_eq!(
            ranked
                .iter()
                .filter(|result| result.target.same_target(&Target::Action(0)))
                .count(),
            1
        );
    }

    #[gpui::test]
    async fn delayed_symbols_preserve_keyboard_selection(cx: &mut TestAppContext) {
        check_delayed_symbols(cx, DelayedSymbolsScenario::Navigate).await;
    }

    #[gpui::test]
    async fn delayed_symbols_select_the_best_result_without_navigation(cx: &mut TestAppContext) {
        check_delayed_symbols(cx, DelayedSymbolsScenario::SelectBest).await;
    }

    #[gpui::test]
    async fn visible_default_result_opens_before_delayed_symbols(cx: &mut TestAppContext) {
        check_delayed_symbols(cx, DelayedSymbolsScenario::ConfirmDefault).await;
    }

    #[gpui::test]
    async fn visible_navigated_result_opens_before_delayed_symbols(cx: &mut TestAppContext) {
        check_delayed_symbols(cx, DelayedSymbolsScenario::ConfirmNavigated).await;
    }

    #[gpui::test]
    async fn visible_result_opens_in_another_pane_before_delayed_symbols(cx: &mut TestAppContext) {
        check_delayed_symbols(cx, DelayedSymbolsScenario::ConfirmSecondary).await;
    }

    #[gpui::test]
    async fn changing_query_cancels_queued_confirmation(cx: &mut TestAppContext) {
        check_delayed_symbols(cx, DelayedSymbolsScenario::ReplaceQuery).await;
    }

    #[gpui::test]
    async fn changing_category_cancels_queued_confirmation(cx: &mut TestAppContext) {
        check_delayed_symbols(cx, DelayedSymbolsScenario::ReplaceCategory).await;
    }

    #[gpui::test]
    async fn classes_rank_before_symbols_before_the_result_limit(cx: &mut TestAppContext) {
        check_delayed_symbols(cx, DelayedSymbolsScenario::MixedContributors).await;
    }

    #[gpui::test]
    async fn symbols_do_not_receive_class_contributor_priority(cx: &mut TestAppContext) {
        check_delayed_symbols(cx, DelayedSymbolsScenario::SymbolContributor).await;
    }

    #[derive(Clone, Copy, PartialEq)]
    enum DelayedSymbolsScenario {
        Navigate,
        SelectBest,
        ConfirmDefault,
        ConfirmNavigated,
        ConfirmSecondary,
        ReplaceQuery,
        ReplaceCategory,
        MixedContributors,
        SymbolContributor,
    }

    async fn check_delayed_symbols(cx: &mut TestAppContext, scenario: DelayedSymbolsScenario) {
        let navigate = matches!(
            scenario,
            DelayedSymbolsScenario::Navigate | DelayedSymbolsScenario::ConfirmNavigated
        );
        let immediate_confirm = matches!(
            scenario,
            DelayedSymbolsScenario::ConfirmDefault
                | DelayedSymbolsScenario::ConfirmNavigated
                | DelayedSymbolsScenario::ConfirmSecondary
        );
        let confirm = matches!(
            scenario,
            DelayedSymbolsScenario::ConfirmNavigated
                | DelayedSymbolsScenario::ConfirmDefault
                | DelayedSymbolsScenario::ConfirmSecondary
                | DelayedSymbolsScenario::ReplaceQuery
                | DelayedSymbolsScenario::ReplaceCategory
        );
        let symbol_scope = scenario == DelayedSymbolsScenario::SymbolContributor;
        let mixed = scenario == DelayedSymbolsScenario::MixedContributors || symbol_scope;
        let class_index = if symbol_scope { MAX_RESULTS } else { 0 };
        let replace_query = scenario == DelayedSymbolsScenario::ReplaceQuery;
        let replace_category = scenario == DelayedSymbolsScenario::ReplaceCategory;
        initialize_test(cx);
        let mut cx = EditorLspTestContext::new_rust(
            lsp::ServerCapabilities {
                workspace_symbol_provider: Some(lsp::OneOf::Left(true)),
                ..Default::default()
            },
            cx,
        )
        .await;
        cx.set_state("struct FileType;\nfn file_function() {}ˇ\n");
        if navigate || mixed {
            let filesystem = cx.workspace.read_with(&cx.cx.cx, |workspace, cx| {
                workspace.project().read(cx).fs().clone()
            });
            let directory = EditorLspTestContext::root_path().join("symbol_fixture");
            filesystem
                .create_dir(&directory)
                .await
                .expect("Create searchable directory");
            filesystem
                .as_fake()
                .insert_file(directory.join("file.rs"), Vec::new())
                .await;
            cx.workspace
                .read_with(&cx.cx.cx, |workspace, cx| {
                    workspace.worktree_scans_complete(cx)
                })
                .await;
        }
        let uri = cx.buffer_lsp_url.clone();
        let (sender, receiver) = futures::channel::oneshot::channel();
        let mut receiver = Some(receiver);
        cx.lsp
            .set_request_handler::<lsp::WorkspaceSymbolRequest, _, _>(move |_, _| {
                let receiver = receiver.take();
                async move {
                    Ok(match receiver {
                        Some(receiver) => receiver.await.expect("Release delayed symbols"),
                        None => Some(lsp::WorkspaceSymbolResponse::Flat(Vec::new())),
                    })
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
                .expect("Search Everywhere")
                .read(cx)
                .picker
                .clone()
        });
        picker.read_with(&cx.cx.cx, |picker, _| {
            assert!(!picker.delegate.matches.is_empty());
            assert!(
                picker
                    .delegate
                    .matches
                    .iter()
                    .all(|result| matches!(result.target, Target::File(_)))
            );
            assert!(picker.delegate.matches.len() <= MAX_RESULTS);
        });
        if symbol_scope {
            cx.simulate_keystrokes("tab tab tab");
            cx.run_until_parked();
        }
        picker.update_in(&mut cx.cx.cx, |picker, window, cx| {
            picker.set_query(
                if replace_query || replace_category {
                    "missing"
                } else if navigate || mixed {
                    "symbol_fixture"
                } else {
                    "file"
                },
                window,
                cx,
            )
        });
        cx.run_until_parked();
        picker.update_in(&mut cx.cx.cx, |picker, window, cx| {
            assert!(picker.delegate.loading_symbols);
            if replace_query || replace_category || symbol_scope {
                assert!(picker.delegate.matches.is_empty());
                return;
            }
            let index = picker
                .delegate
                .matches
                .iter()
                .position(|result| matches!(result.target, Target::File(_)))
                .expect("File results are available before symbols");
            if navigate {
                picker.set_selected_index(index, None, false, window, cx);
            } else if !mixed {
                assert_eq!(picker.delegate.selected, index);
            }
        });
        if confirm {
            cx.simulate_keystrokes(if scenario == DelayedSymbolsScenario::ConfirmSecondary {
                "cmd-enter"
            } else {
                "enter"
            });
        }
        cx.run_until_parked();
        assert_eq!(
            workspace.read_with(&cx.cx.cx, |workspace, cx| {
                workspace.active_modal::<SearchEverywhere>(cx).is_none()
            }),
            immediate_confirm
        );
        if replace_query {
            picker.update_in(&mut cx.cx.cx, |picker, window, cx| {
                picker.set_query("fi", window, cx)
            });
            cx.run_until_parked();
        }
        if replace_category {
            cx.simulate_keystrokes("tab");
            cx.run_until_parked();
        }
        #[expect(deprecated)]
        let symbols = (0..if mixed { MAX_RESULTS + 1 } else { MAX_RESULTS })
            .map(|index| lsp::SymbolInformation {
                // Exact matches rank ahead of symbol_fixture/file.rs and would push the
                // selected file outside the result limit.
                name: if navigate || mixed {
                    "symbol_fixture"
                } else {
                    "file"
                }
                .into(),
                kind: if mixed && index != class_index {
                    lsp::SymbolKind::FUNCTION
                } else {
                    lsp::SymbolKind::STRUCT
                },
                tags: None,
                deprecated: None,
                container_name: None,
                location: lsp::Location {
                    uri: uri.clone(),
                    range: lsp::Range::new(
                        lsp::Position::new(index as u32, 7),
                        lsp::Position::new(index as u32, 15),
                    ),
                },
            })
            .collect();
        let response = sender.send(Some(lsp::WorkspaceSymbolResponse::Flat(symbols)));
        assert!(
            response.is_ok() || replace_query || replace_category || immediate_confirm,
            "Symbol search is pending"
        );
        cx.run_until_parked();
        picker.read_with(&cx.cx.cx, |picker, _| {
            if replace_category {
                assert_eq!(picker.delegate.category, Category::Classes);
                assert_eq!(picker.delegate.query, "missing");
                assert!(!picker.delegate.loading_symbols);
                assert!(picker.delegate.matches.is_empty());
                return;
            }
            if immediate_confirm {
                return;
            }
            if replace_query {
                assert_eq!(picker.delegate.query, "fi");
                assert!(!picker.delegate.loading_symbols);
                assert!(!picker.delegate.matches.is_empty());
                assert!(
                    !picker
                        .delegate
                        .matches
                        .iter()
                        .any(|result| matches!(result.target, Target::Symbol(_)))
                );
                return;
            }
            assert_eq!(picker.delegate.matches.len(), MAX_RESULTS);
            if symbol_scope {
                assert_eq!(picker.delegate.category, Category::Symbols);
                assert!(picker.delegate.matches.iter().all(|result| {
                    matches!(&result.target, Target::Symbol(symbol) if symbol.kind == SymbolKind::Function)
                }));
                return;
            }
            if navigate {
                assert_eq!(picker.delegate.selected, 99);
                assert!(matches!(
                    picker.delegate.matches[picker.delegate.selected].target,
                    Target::File(_)
                ));
            } else {
                assert_eq!(picker.delegate.selected, 0);
                assert!(matches!(
                    picker.delegate.matches.first().expect("Best match").target,
                    Target::Symbol(_)
                ));
                if mixed {
                    let Target::Symbol(symbol) =
                        &picker.delegate.matches.first().expect("Class match").target
                    else {
                        panic!("Expected a class");
                    };
                    assert!(is_class(symbol.kind));
                }
            }
        });
        assert_eq!(
            workspace.read_with(&cx.cx.cx, |workspace, cx| {
                workspace.active_modal::<SearchEverywhere>(cx).is_none()
            }),
            immediate_confirm
        );
        cx.assert_editor_state("struct FileType;\nfn file_function() {}ˇ\n");
    }

    #[gpui::test]
    async fn selected_file_does_not_receive_recent_priority(cx: &mut TestAppContext) {
        initialize_test(cx);
        let mut cx = EditorLspTestContext::new_rust(
            lsp::ServerCapabilities {
                workspace_symbol_provider: Some(lsp::OneOf::Left(true)),
                ..Default::default()
            },
            cx,
        )
        .await;
        let uri = cx.buffer_lsp_url.clone();
        let query = uri
            .to_file_path()
            .expect("Local fixture")
            .file_stem()
            .expect("Fixture filename")
            .to_string_lossy()
            .into_owned();
        let name = query.clone();
        cx.lsp
            .set_request_handler::<lsp::WorkspaceSymbolRequest, _, _>(move |_, _| {
                let uri = uri.clone();
                let name = name.clone();
                async move {
                    #[expect(deprecated)]
                    Ok(Some(lsp::WorkspaceSymbolResponse::Flat(vec![
                        lsp::SymbolInformation {
                            name,
                            kind: lsp::SymbolKind::STRUCT,
                            tags: None,
                            deprecated: None,
                            container_name: None,
                            location: lsp::Location {
                                uri,
                                range: lsp::Range::default(),
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
        let picker = cx.workspace.read_with(&cx.cx.cx, |workspace, cx| {
            workspace
                .active_modal::<SearchEverywhere>(cx)
                .expect("Search Everywhere")
                .read(cx)
                .picker
                .clone()
        });
        picker.update_in(&mut cx.cx.cx, |picker, window, cx| {
            picker.set_query(&query, window, cx)
        });
        cx.run_until_parked();
        picker.read_with(&cx.cx.cx, |picker, _| {
            assert!(matches!(
                picker.delegate.matches.first().expect("Class result").target,
                Target::Symbol(_)
            ));
            let file = picker
                .delegate
                .matches
                .iter()
                .find(|result| matches!(&result.target, Target::File(path) if picker.delegate.selected_files.contains(path)))
                .expect("Selected file remains searchable");
            assert_eq!(file.recent_index, None);
        });
    }

    #[gpui::test]
    async fn deep_exact_file_names_survive_the_result_limit(cx: &mut TestAppContext) {
        initialize_test(cx);
        let mut cx = EditorLspTestContext::new_rust(lsp::ServerCapabilities::default(), cx).await;
        let workspace = cx.workspace.clone();
        let filesystem = workspace.read_with(&cx.cx.cx, |workspace, cx| {
            workspace.project().read(cx).fs().clone()
        });
        let root = EditorLspTestContext::root_path();
        let deep = root.join("a_very_long_directory/a_nested_module/another_directory");
        filesystem
            .create_dir(&deep)
            .await
            .expect("Create nested directories");
        filesystem
            .as_fake()
            .insert_file(deep.join("mainactivity.rs"), Vec::new())
            .await;
        for index in 0..MAX_RESULTS + 10 {
            filesystem
                .as_fake()
                .insert_file(root.join(format!("MAINACTIVITY{index}.rs")), Vec::new())
                .await;
        }
        workspace
            .read_with(&cx.cx.cx, |workspace, cx| {
                workspace.worktree_scans_complete(cx)
            })
            .await;
        cx.update(|window, cx| {
            window.dispatch_action(
                Box::new(Toggle {
                    category: Category::Files,
                }),
                cx,
            )
        });
        cx.run_until_parked();
        let picker = workspace.read_with(&cx.cx.cx, |workspace, cx| {
            workspace
                .active_modal::<SearchEverywhere>(cx)
                .expect("Search Everywhere")
                .read(cx)
                .picker
                .clone()
        });
        picker.update_in(&mut cx.cx.cx, |picker, window, cx| {
            picker.set_query("MAINACTIVITY", window, cx)
        });
        cx.run_until_parked();
        picker.read_with(&cx.cx.cx, |picker, _| {
            let first = picker
                .delegate
                .matches
                .first()
                .expect("Exact filename result");
            assert_eq!(first.label, "mainactivity.rs");
            assert!(first.exact_match);
            assert_eq!(picker.delegate.matches.len(), MAX_RESULTS);
        });
    }

    #[gpui::test]
    async fn double_shift_searches_files_actions_and_filters_classes(cx: &mut TestAppContext) {
        initialize_test(cx);
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
