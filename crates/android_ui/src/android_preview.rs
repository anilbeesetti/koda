use super::*;
use android_tools::preview;
use editor::{Addon, Editor};
use gpui::{
    Bounds, Image, ImageFormat, ListState, MouseButton, Pixels, canvas, img, list, point, size,
};
use language::Buffer;
use std::{cell::Cell, collections::HashSet, rc::Rc};
use ui::{ButtonLike, CommonAnimationExt, ContextMenuEntry, WithScrollbar};
use workspace::Pane;

const REFRESH_DELAY: Duration = Duration::from_millis(700);
const GALLERY_INSET: f32 = 40.;
const CARD_GAP: f32 = 12.;
const MINIMUM_CARD_WIDTH: f32 = 80.;
const MINIMUM_ZOOM: f32 = 0.05;
const MAXIMUM_ZOOM: f32 = 2.;

pub(super) fn suspend_previews(workspace: &WeakEntity<Workspace>, cx: &mut App) {
    let Some(workspace) = workspace.upgrade() else {
        return;
    };
    let editors = workspace.read(cx).items_of_type::<Editor>(cx).collect::<Vec<_>>();
    for editor in editors {
        if let Some(view) = editor.read(cx).addon::<ComposePreviewAddon>().map(|addon| addon.view.clone()) {
            view.update(cx, |view, cx| view.stop(cx));
            editor.update(cx, |_, cx| cx.notify());
        }
    }
}

pub(super) fn toggle_preview(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some(panel) = workspace.panel::<AndroidPanel>(cx)
        && panel.read(cx).compose_preview_enabled
    {
        for editor in workspace.items_of_type::<Editor>(cx).collect::<Vec<_>>() {
            if let Some(view) = editor
                .read(cx)
                .addon::<ComposePreviewAddon>()
                .map(|addon| addon.view.clone())
            {
                view.update(cx, |view, cx| view.stop(cx));
            }
            editor.update(cx, |editor, cx| {
                editor.unregister_addon::<ComposePreviewAddon>();
                cx.notify();
            });
        }
        panel.update(cx, |panel, _| {
            panel.compose_preview_enabled = false;
            panel.preview_view = None;
        });
    } else {
        with_panel(workspace, window, cx, AndroidPanel::show_compose_preview);
    }
}

impl AndroidPanel {
    pub(super) fn observe_compose_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        self._startup_subscriptions.push(cx.subscribe_in(
            &workspace,
            window,
            |_, _, event, window, cx| {
                if matches!(event, workspace::Event::ActiveItemChanged) {
                    // Workspace events can arrive while a pane or the workspace itself is updating.
                    cx.defer_in(window, |panel, window, cx| {
                        let view = panel.active_preview_editor(cx).and_then(|editor| {
                            editor
                                .read(cx)
                                .addon::<ComposePreviewAddon>()
                                .map(|addon| addon.view.clone())
                        });
                        panel.preview_view = view.as_ref().map(Entity::downgrade);
                        if view.is_none()
                            && panel.compose_preview_enabled
                            && panel.active_preview_source(cx).is_some()
                        {
                            panel.attach_compose_preview(false, window, cx).log_err();
                        }
                    });
                }
            },
        ));
        self._startup_subscriptions.push(cx.observe_in(
            &cx.entity(),
            window,
            |_, _, window, cx| {
                cx.defer_in(window, |panel, window, cx| {
                    if !panel.compose_preview_enabled || panel.selected_target.is_none() {
                        return;
                    }
                    if let Some(editor) = panel.active_preview_editor(cx)
                        && editor.read(cx).addon::<ComposePreviewAddon>().is_none()
                        && let Some((buffer, _)) = panel.active_preview_source(cx)
                        && panel.accepts_compose_preview(&buffer, cx)
                    {
                        panel.attach_compose_preview(false, window, cx).log_err();
                    }
                });
            },
        ));
    }

    fn active_preview_editor(&self, cx: &App) -> Option<Entity<Editor>> {
        self.workspace
            .upgrade()?
            .read(cx)
            .active_item(cx)?
            .downcast::<Editor>()
    }

    fn active_preview_source(&self, cx: &App) -> Option<(Entity<Buffer>, WeakEntity<Pane>)> {
        let workspace = self.workspace.upgrade()?;
        let workspace = workspace.read(cx);
        let editor = self.active_preview_editor(cx)?;
        let buffer = editor.read(cx).buffer().read(cx).as_singleton()?;
        let file = buffer.read(cx).file()?;
        if file.path().extension() != Some("kt") || !self.accepts_compose_preview(&buffer, cx) {
            return None;
        }
        Some((buffer, workspace.pane_for(&editor)?.downgrade()))
    }

    pub(super) fn accepts_compose_preview(&self, buffer: &Entity<Buffer>, cx: &App) -> bool {
        buffer_path(buffer, cx).is_some_and(|path| path.extension().is_some_and(|extension| extension == "kt")
            && self.preview_path_owned(&path, cx))
    }

    fn preview_path_owned(&self, path: &Path, cx: &App) -> bool {
        let Ok(root) = self.trusted_root(cx) else { return false; };
        let project = self.project.read(cx);
        let model = project.android_model();
        let Some(snapshot) = evaluated_snapshot(&project, &root) else { return false; };
        snapshot.capabilities(Some(path), android_tools::project_context::OperationalReadiness {
            application_module: self.selected_target.as_ref().map(|target| target.module.as_str()),
            model_current: model.model.is_some(), model_root: model.root(),
            android_renderer_supported: cfg!(feature = "bundled-preview"),
        }).android_compose_preview
    }

    pub(super) fn show_compose_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = self.operation_owner(AndroidOperation::Preview, cx) {
            self.fail(error, window, cx);
            return;
        }
        self.compose_preview_enabled = true;
        if let Err(error) = self.attach_compose_preview(true, window, cx) {
            self.fail(error, window, cx);
        }
    }

    fn attach_compose_preview(
        &mut self,
        manual: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let owner = self.operation_owner(AndroidOperation::Preview, cx)?;
        self.backend_owner = Some(owner.context.clone());
        let root = owner.root.clone();
        let target = self
            .selected_target
            .clone()
            .context("Sync the Android project and select a build variant first.")?;
        let (buffer, source_pane) = self
            .active_preview_source(cx)
            .context("Open a Kotlin source file to preview its composables.")?;
        ensure!(
            self.accepts_compose_preview(&buffer, cx),
            "The active file is not eligible for Android Compose previews"
        );
        let editor = self
            .active_preview_editor(cx)
            .context("Open a source editor")?;
        if let Some(view) = editor
            .read(cx)
            .addon::<ComposePreviewAddon>()
            .map(|addon| addon.view.clone())
        {
            self.preview_view = Some(view.downgrade());
            view.update(cx, |view, cx| {
                view.configure(root, target, window, cx);
                view.queue_refresh(manual, window, cx);
            });
            return Ok(());
        }
        let panel = cx.weak_entity();
        let view = cx.new(|cx| {
            ComposePreviewView::new(
                panel,
                self.workspace.clone(),
                self.project.clone(),
                buffer,
                editor.downgrade(),
                source_pane,
                root,
                target,
                window,
                cx,
            )
        });
        self.preview_view = Some(view.downgrade());
        editor.update(cx, |editor, cx| {
            editor.register_addon(ComposePreviewAddon { view: view.clone() });
            cx.notify();
        });
        view.update(cx, |view, cx| view.queue_refresh(manual, window, cx));
        Ok::<_, anyhow::Error>(())
    }

    pub(super) fn generate_preview(
        &mut self,
        target: AndroidTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.running = false;
        if self.selected_target.as_ref() == Some(&target) {
            self.show_compose_preview(window, cx);
        }
    }
}

struct ComposePreviewAddon {
    view: Entity<ComposePreviewView>,
}

impl Addon for ComposePreviewAddon {
    fn wrap_editor_content(
        &self,
        content: gpui::AnyElement,
        _: &mut Window,
        cx: &mut App,
    ) -> gpui::AnyElement {
        let view = self.view.read(cx);
        if !view.panel.read_with(cx, |panel, cx| {
            panel.operation_permitted(AndroidOperation::Preview, cx)
                && panel.trusted_root(cx).is_ok_and(|root| root == view.root)
        }).unwrap_or(false) {
            return content;
        }
        let editor_bounds = view.editor_bounds.clone();
        let preview_fraction = view.preview_fraction;
        let view = self.view.downgrade();
        let resize_view = view.clone();
        h_flex()
            .relative()
            .size_full()
            .overflow_hidden()
            .debug_selector(|| "compose-editor".into())
            .child(div().flex_1().min_w_0().h_full().child(content))
            .child(
                div()
                    .relative()
                    .flex_none()
                    .w(gpui::relative(preview_fraction))
                    .h_full()
                    .border_l_1()
                    .border_color(cx.theme().colors().border)
                    .child(self.view.clone())
                    .child(
                        div()
                            .id("compose-preview-divider")
                            .debug_selector(|| "compose-preview-divider".into())
                            .absolute()
                            .left(px(-3.))
                            .top_0()
                            .w(px(6.))
                            .h_full()
                            .cursor_col_resize()
                            .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                                resize_view
                                    .update(cx, |view, cx| {
                                        view.resizing = event.click_count != 2;
                                        if event.click_count == 2 {
                                            view.preview_fraction = 0.5;
                                            view.editor.update(cx, |_, cx| cx.notify()).log_err();
                                        }
                                    })
                                    .log_err();
                                cx.stop_propagation();
                            }),
                    ),
            )
            .child(
                canvas(
                    move |bounds, _, _| editor_bounds.set(bounds),
                    move |_, _, window, _| {
                        let resize_view = view.clone();
                        window.on_mouse_event(move |event: &gpui::MouseMoveEvent, phase, _, cx| {
                            if phase != gpui::DispatchPhase::Capture {
                                return;
                            }
                            resize_view
                                .update(cx, |view, cx| {
                                    if !view.resizing {
                                        return;
                                    }
                                    if event.pressed_button == Some(MouseButton::Left) {
                                        let bounds = view.editor_bounds.get();
                                        if bounds.size.width > px(0.) {
                                            view.preview_fraction =
                                                (f32::from(bounds.right() - event.position.x)
                                                    / f32::from(bounds.size.width))
                                                .clamp(0.2, 0.8);
                                            view.editor.update(cx, |_, cx| cx.notify()).log_err();
                                        }
                                        cx.stop_propagation();
                                    } else {
                                        view.resizing = false;
                                    }
                                })
                                .log_err();
                        });
                        window.on_mouse_event(move |event: &gpui::MouseUpEvent, phase, _, cx| {
                            if phase == gpui::DispatchPhase::Capture
                                && event.button == MouseButton::Left
                            {
                                view.update(cx, |view, cx| {
                                    if view.resizing {
                                        view.resizing = false;
                                        cx.stop_propagation();
                                    }
                                })
                                .log_err();
                            }
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
            .into_any_element()
    }

    fn to_any(&self) -> &dyn std::any::Any {
        self
    }
}

struct PreviewCard {
    label: String,
    method: String,
    variant: String,
    result: preview::RenderedPreview,
    image: Option<Arc<Image>>,
    rendered_image: Option<Arc<gpui::RenderImage>>,
    nodes: Arc<Vec<preview::ComposeNode>>,
    outlines: Arc<Vec<[i32; 4]>>,
}

impl PreviewCard {
    fn new(definition: &preview::Preview, mut result: preview::RenderedPreview) -> Result<Self> {
        let parameterized = result.parameter_index > 0
            || !definition
                .parameters
                .get("methodParams")
                .and_then(serde_json::Value::as_array)
                .is_none_or(Vec::is_empty);
        let label = definition.label();
        let label = if parameterized {
            format!("{label} · value {}", result.parameter_index + 1)
        } else {
            label
        };
        let variant = definition
            .parameters
            .get("previewParams")
            .and_then(|parameters| parameters.get("name"))
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.is_empty())
            .unwrap_or("Default");
        let variant = if parameterized {
            format!("{variant} · value {}", result.parameter_index + 1)
        } else {
            variant.to_string()
        };
        let image = result
            .image
            .as_ref()
            .map(|path| {
                std::fs::read(path)
                    .map(|bytes| Arc::new(Image::from_bytes(ImageFormat::Png, bytes)))
            })
            .transpose()?;
        let outlines = Arc::new(
            result
                .nodes
                .iter()
                .map(|node| node.bounds)
                .filter(|[left, top, right, bottom]| right > left && bottom > top)
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect(),
        );
        let nodes = Arc::new(std::mem::take(&mut result.nodes));
        Ok(Self {
            label,
            method: definition.method.clone(),
            variant,
            result,
            image,
            rendered_image: None,
            nodes,
            outlines,
        })
    }

    fn width(&self, zoom: f32) -> f32 {
        if self.image.is_some() {
            (self.result.width as f32 * zoom).max(MINIMUM_CARD_WIDTH)
        } else {
            220.
        }
    }
}

struct PreviewGroup {
    method: String,
    label: String,
    cards: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
enum GalleryRow {
    Header(usize),
    Cards { group: usize, cards: Vec<usize> },
}

impl GalleryRow {
    fn group(&self) -> usize {
        match self {
            Self::Header(group) | Self::Cards { group, .. } => *group,
        }
    }
}

struct Gallery {
    cards: Vec<PreviewCard>,
    groups: Vec<PreviewGroup>,
    labels: Arc<Vec<String>>,
    largest_widths: Vec<u32>,
    failures: usize,
    has_missing_images: bool,
    _directory: tempfile::TempDir,
}

impl Gallery {
    fn new(cards: Vec<PreviewCard>, directory: tempfile::TempDir) -> Self {
        let mut groups: Vec<PreviewGroup> = Vec::new();
        let mut methods = HashMap::new();
        for (index, card) in cards.iter().enumerate() {
            let group = *methods.entry(card.method.clone()).or_insert_with(|| {
                groups.push(PreviewGroup {
                    method: card.method.clone(),
                    label: card
                        .method
                        .rsplit('.')
                        .next()
                        .unwrap_or(&card.method)
                        .into(),
                    cards: Vec::new(),
                });
                groups.len() - 1
            });
            groups[group].cards.push(index);
        }
        let labels = Arc::new(cards.iter().map(|card| card.label.clone()).collect());
        let failures = cards
            .iter()
            .filter(|card| card.result.error.is_some())
            .count();
        let has_missing_images = cards.iter().any(|card| card.image.is_none());
        let mut largest_widths = cards
            .iter()
            .filter(|card| card.image.is_some())
            .map(|card| card.result.width)
            .collect::<Vec<_>>();
        largest_widths.sort_unstable_by(|left, right| right.cmp(left));
        largest_widths.truncate(2);
        Self {
            cards,
            groups,
            labels,
            largest_widths,
            failures,
            has_missing_images,
            _directory: directory,
        }
    }

    fn rows(&self, zoom: f32, width: f32, collapsed: &HashSet<String>) -> Vec<GalleryRow> {
        let available = (width - GALLERY_INSET).max(MINIMUM_CARD_WIDTH);
        let mut rows = Vec::new();
        for (group_index, group) in self.groups.iter().enumerate() {
            rows.push(GalleryRow::Header(group_index));
            if collapsed.contains(&group.method) {
                continue;
            }
            let mut cards = Vec::new();
            let mut row_width = 0.;
            for &index in &group.cards {
                let card_width = self.cards[index].width(zoom);
                if !cards.is_empty() && row_width + CARD_GAP + card_width > available {
                    rows.push(GalleryRow::Cards {
                        group: group_index,
                        cards: std::mem::take(&mut cards),
                    });
                    row_width = 0.;
                }
                if !cards.is_empty() {
                    row_width += CARD_GAP;
                }
                row_width += card_width;
                cards.push(index);
            }
            if !cards.is_empty() {
                rows.push(GalleryRow::Cards {
                    group: group_index,
                    cards,
                });
            }
        }
        rows
    }

    fn fit_zoom(&self, width: f32) -> f32 {
        let widths = &self.largest_widths;
        let gap = if widths.len() > 1 { CARD_GAP } else { 0. };
        if widths.is_empty() {
            return 0.5;
        }
        let available = width - GALLERY_INSET - gap - 1.;
        let mut minimum = MINIMUM_ZOOM;
        let mut maximum = 1.;
        // Small previews still need room for their title and options button.
        for _ in 0..20 {
            let zoom = (minimum + maximum) / 2.;
            let row_width = widths
                .iter()
                .take(2)
                .map(|width| (*width as f32 * zoom).max(MINIMUM_CARD_WIDTH))
                .sum::<f32>();
            if row_width <= available {
                minimum = zoom;
            } else {
                maximum = zoom;
            }
        }
        minimum
    }

    fn maximum_width(&self, zoom: f32) -> f32 {
        let image_width = self
            .largest_widths
            .first()
            .map(|width| (*width as f32 * zoom).max(MINIMUM_CARD_WIDTH))
            .unwrap_or(0.);
        image_width.max(if self.has_missing_images { 220. } else { 0. })
    }
}

pub(super) struct ComposePreviewView {
    panel: WeakEntity<AndroidPanel>,
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    source: Entity<Buffer>,
    editor: WeakEntity<Editor>,
    editor_bounds: Rc<Cell<Bounds<Pixels>>>,
    preview_fraction: f32,
    resizing: bool,
    source_pane: WeakEntity<Pane>,
    source_path: PathBuf,
    root: PathBuf,
    target: AndroidTarget,
    model_token: android_tools::project_model::ModelToken,
    focus_handle: FocusHandle,
    gallery: Option<Gallery>,
    rows: Vec<GalleryRow>,
    collapsed_groups: HashSet<String>,
    list_state: ListState,
    revision: u64,
    gallery_generation: u64,
    pending: bool,
    pending_manual: bool,
    stale: bool,
    building: bool,
    configuration_suspended: bool,
    inspect: bool,
    hovered: Option<(usize, usize)>,
    selected_card: Option<usize>,
    zoom: f32,
    fit_to_window: bool,
    pan_mode: bool,
    pan_position: Option<gpui::Point<Pixels>>,
    status: SharedString,
    error: Option<String>,
    debounce_task: Option<Task<()>>,
    render_task: Option<Task<()>>,
    source_task: Option<Task<()>>,
    navigation_source: Option<PathBuf>,
    navigation_pending: bool,
    horizontal_scroll: gpui::ScrollHandle,
    viewport_width: Rc<Cell<f32>>,
    buffer_subscriptions: HashMap<gpui::EntityId, Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl ComposePreviewView {
    fn new(
        panel: WeakEntity<AndroidPanel>,
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        source: Entity<Buffer>,
        editor: WeakEntity<Editor>,
        source_pane: WeakEntity<Pane>,
        root: PathBuf,
        target: AndroidTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let source_path = buffer_path(&source, cx).unwrap_or_default();
        let model_token = project.read(cx).android_model().token();
        let mut subscriptions =
            vec![
                cx.subscribe_in(&project, window, |view, _, event, window, cx| {
                    let relevant = match event {
                        project::Event::WorktreeUpdatedEntries(worktree, changes) => {
                            view.project
                                .read(cx)
                                .worktree_for_id(*worktree, cx)
                                .is_some_and(|worktree| {
                                    worktree.read(cx).abs_path().as_ref() == view.root
                                })
                                && changes.iter().any(|(path, _, change)| {
                                    *change != project::PathChange::Loaded
                                        && view.preview_input(path, cx)
                                })
                        }
                        _ => false,
                    };
                    if relevant {
                        cx.defer_in(window, |view, window, cx| {
                            view.queue_refresh(false, window, cx)
                        });
                    }
                }),
            ];
        if let Some(workspace) = workspace.upgrade() {
            subscriptions.push(cx.subscribe_in(
                &workspace,
                window,
                |view, _, event, window, cx| {
                    if matches!(
                        event,
                        workspace::Event::ActiveItemChanged | workspace::Event::ZoomChanged
                    ) {
                        cx.defer_in(window, |view, window, cx| {
                            if view.visible(cx) {
                                view.resume_if_visible(window, cx);
                            } else {
                                if view.building || view.debounce_task.is_some() {
                                    let manual = view.pending_manual;
                                    view.stop(cx);
                                    view.pending = true;
                                    view.pending_manual = manual;
                                    view.status = "Previews are out of date".into();
                                }
                                view.release_images(window, cx);
                            }
                        });
                    } else if let workspace::Event::ItemRemoved { item_id } = event
                        && *item_id == view.editor.entity_id()
                    {
                        // Moving a tab emits removal before adding it to its destination pane.
                        cx.defer_in(window, |view, window, cx| {
                            let pane = view
                                .workspace
                                .read_with(cx, |workspace, _| {
                                    workspace.pane_for_item_id(view.editor.entity_id())
                                })
                                .log_err()
                                .flatten();
                            if let Some(pane) = pane {
                                view.set_source_pane(&pane);
                            } else {
                                view.stop(cx);
                                view.release_gallery(window, cx);
                                view.editor
                                    .update(cx, |editor, cx| {
                                        editor.unregister_addon::<ComposePreviewAddon>();
                                        cx.notify();
                                    })
                                    .log_err();
                            }
                        });
                    }
                },
            ));
        }
        let store = project.read(cx).buffer_store().clone();
        subscriptions.push(cx.subscribe_in(&store, window, |_, _, event, window, cx| {
            if let project::buffer_store::BufferStoreEvent::BufferAdded(buffer)
            | project::buffer_store::BufferStoreEvent::BufferChangedFilePath {
                buffer, ..
            } = event
            {
                let buffer = buffer.clone();
                cx.defer_in(window, move |view, window, cx| {
                    view.observe_buffer(buffer, window, cx)
                });
            }
        }));
        subscriptions.push(cx.observe_in(&project, window, |_, _, window, cx| {
            cx.defer_in(window, |view, window, cx| {
                view.sync_configuration(window, cx)
            });
        }));
        if let Some(panel) = panel.upgrade() {
            subscriptions.push(cx.observe_in(&panel, window, |_, _, window, cx| {
                cx.defer_in(window, |view, window, cx| {
                    view.sync_configuration(window, cx)
                });
            }));
        }
        if let Some(trusted) = TrustedWorktrees::try_get_global(cx) {
            subscriptions.push(cx.subscribe_in(&trusted, window, |_, _, _, window, cx| {
                cx.defer_in(window, |view, window, cx| {
                    view.sync_configuration(window, cx)
                });
            }));
        }
        cx.on_release_in(window, |view, window, cx| view.release_gallery(window, cx))
            .detach();
        let mut view = Self {
            panel,
            workspace,
            project,
            source,
            editor,
            editor_bounds: Rc::new(Cell::new(Bounds::default())),
            preview_fraction: 0.5,
            resizing: false,
            source_pane,
            source_path,
            root,
            target,
            model_token,
            focus_handle: cx.focus_handle(),
            gallery: None,
            rows: Vec::new(),
            collapsed_groups: HashSet::default(),
            list_state: ListState::new(0, gpui::ListAlignment::Top, px(300.)),
            revision: 0,
            gallery_generation: 0,
            pending: false,
            pending_manual: false,
            stale: true,
            building: false,
            configuration_suspended: false,
            inspect: false,
            hovered: None,
            selected_card: None,
            zoom: 0.5,
            fit_to_window: true,
            pan_mode: false,
            pan_position: None,
            status: "Preparing Compose previews…".into(),
            error: None,
            debounce_task: None,
            render_task: None,
            source_task: None,
            navigation_source: None,
            navigation_pending: false,
            horizontal_scroll: gpui::ScrollHandle::new(),
            viewport_width: Rc::new(Cell::new(0.)),
            buffer_subscriptions: HashMap::default(),
            _subscriptions: subscriptions,
        };
        view.observe_buffers(window, cx);
        view
    }

    fn observe_buffers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.buffer_subscriptions.clear();
        for buffer in self.project.read(cx).opened_buffers(cx) {
            self.observe_buffer(buffer, window, cx);
        }
    }

    fn observe_buffer(
        &mut self,
        buffer: Entity<Buffer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = buffer.entity_id();
        let Some(path) = buffer_path(&buffer, cx) else {
            self.buffer_subscriptions.remove(&id);
            return;
        };
        if evaluated_module(&self.project.read(cx), &self.root, &path).is_none()
            || !path
                .extension()
                .is_some_and(|extension| matches!(extension.to_str(), Some("kt" | "java" | "xml")))
        {
            self.buffer_subscriptions.remove(&id);
            return;
        }
        if !self.preview_absolute_input(&path, cx) {
            self.buffer_subscriptions.remove(&id);
            return;
        }
        if self.buffer_subscriptions.contains_key(&id) {
            return;
        }
        let subscription = cx.subscribe_in(&buffer, window, |view, buffer, event, window, cx| {
            if matches!(event, language::BufferEvent::FileHandleChanged) && *buffer == view.source {
                cx.defer_in(window, |view, window, cx| {
                    if let Some(path) = buffer_path(&view.source, cx) {
                        if evaluated_module(&view.project.read(cx), &view.root, &path).is_some() {
                            view.source_path = path;
                            view.invalidate(window, cx);
                        } else {
                            view.stop(cx);
                            view.error =
                                Some("The preview source moved outside the Android project".into());
                        }
                    }
                });
                return;
            }
            if matches!(
                event,
                language::BufferEvent::Edited { .. }
                    | language::BufferEvent::Saved
                    | language::BufferEvent::Reloaded
                    | language::BufferEvent::FileHandleChanged
            ) {
                view.queue_refresh(false, window, cx);
            }
        });
        self.buffer_subscriptions.insert(id, subscription);
    }

    fn sync_configuration(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let configuration = self
            .panel
            .read_with(cx, |panel, cx| {
                Ok::<_, anyhow::Error>((
                    {
                        let root = panel.operation_owner(AndroidOperation::Preview, cx)?.root;
                        ensure!(
                            panel.preview_path_owned(&self.source_path, cx),
                            "The preview file belongs to a different Android project"
                        );
                        root
                    },
                    panel
                        .selected_target
                        .clone()
                        .context("Select an Android build variant")?,
                ))
            })
            .and_then(|result| result);
        let configuration = configuration.and_then(|(root, target)| {
            self.selected_model(&root, &target, cx)?;
            Ok((root, target))
        });
        match configuration {
            Ok((root, target)) => {
                let recovering = self.configuration_suspended;
                self.configuration_suspended = false;
                let inspected_source = (root == self.root && !self.navigation_pending)
                    .then(|| self.navigation_source.clone())
                    .flatten();
                self.configure(root, target, window, cx);
                self.navigation_source = inspected_source;
                if recovering {
                    self.error = None;
                    self.invalidate(window, cx);
                }
            }
            Err(error) => {
                self.stop(cx);
                self.configuration_suspended = true;
                self.release_gallery(window, cx);
                self.list_state.reset(0);
                self.error = Some(format!("{error:#}"));
                cx.notify();
            }
        }
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        self.render_task = None;
        self.debounce_task = None;
        self.source_task = None;
        self.navigation_source = None;
        self.navigation_pending = false;
        self.revision = self.revision.wrapping_add(1);
        self.building = false;
        self.pending = false;
        self.pending_manual = false;
        self.stale = true;
        self.status = "Preview refresh stopped".into();
        cx.notify();
    }

    fn configure(
        &mut self,
        root: PathBuf,
        target: AndroidTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let model_token = self.project.read(cx).android_model().token();
        if root != self.root || target != self.target || model_token != self.model_token {
            self.model_token = model_token;
            self.root = root;
            self.target = target;
            self.observe_buffers(window, cx);
            self.invalidate(window, cx);
        }
    }

    fn invalidate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.render_task = None;
        self.debounce_task = None;
        self.building = false;
        self.release_gallery(window, cx);
        self.hovered = None;
        self.navigation_source = None;
        self.inspect = false;
        self.collapsed_groups.clear();
        self.pan_position = None;
        self.list_state.reset(0);
        self.queue_refresh(false, window, cx);
    }

    fn visible(&self, cx: &Context<Self>) -> bool {
        let id = self.editor.entity_id();
        self.workspace
            .read_with(cx, |workspace, cx| {
                workspace.pane_for_item_id(id).is_some_and(|pane| {
                    workspace
                        .zoomed_item()
                        .is_none_or(|zoomed| zoomed == &pane.downgrade().into())
                        && (!workspace.is_pane_maximized() || *workspace.active_pane() == pane)
                        && pane
                            .read(cx)
                            .active_item()
                            .is_some_and(|item| item.item_id() == id)
                })
            })
            .unwrap_or(false)
    }

    fn resume_if_visible(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.visible(cx) {
            return;
        }
        if let Some(pane) = self
            .workspace
            .read_with(cx, |workspace, _| {
                workspace.pane_for_item_id(self.editor.entity_id())
            })
            .log_err()
            .flatten()
        {
            self.set_source_pane(&pane);
        }
        if self.pending && !self.building && self.debounce_task.is_none() {
            self.queue_refresh(self.pending_manual, window, cx);
        }
    }

    fn set_source_pane(&mut self, pane: &Entity<Pane>) {
        if self.source_pane.entity_id() != pane.entity_id() && self.navigation_pending {
            self.source_task = None;
            self.navigation_pending = false;
            self.navigation_source = None;
        }
        self.source_pane = pane.downgrade();
    }

    fn queue_refresh(&mut self, manual: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.revision = self.revision.wrapping_add(1);
        self.pending = true;
        self.pending_manual |= manual;
        let manual = self.pending_manual;
        self.stale = true;
        self.hovered = None;
        self.debounce_task = None;
        self.source_task = None;
        if self.navigation_pending {
            self.navigation_source = None;
        }
        self.navigation_pending = false;
        if !self.visible(cx) {
            self.status = "Previews are out of date".into();
            cx.notify();
            return;
        }
        self.status = if self.building {
            "Code changed · refresh queued"
        } else {
            "Waiting to refresh…"
        }
        .into();
        self.debounce_task = Some(cx.spawn_in(window, async move |view, cx| {
            if !manual {
                cx.background_executor().timer(REFRESH_DELAY).await;
            }
            view.update_in(cx, |view, window, cx| {
                view.debounce_task = None;
                if view.pending && !view.building && view.visible(cx) {
                    view.refresh(window, cx);
                }
            })
            .log_err();
        }));
        cx.notify();
    }

    fn preview_input(&self, path: &RelPath, cx: &App) -> bool {
        self.preview_absolute_input(&self.root.join(path.as_std_path()), cx)
    }

    fn preview_absolute_input(&self, path: &Path, cx: &App) -> bool {
        self.project
            .read(cx)
            .android_model()
            .selected
            .as_ref()
            .and_then(|selected| {
                let absolute = model_source_path(&self.project.read(cx), &self.root, path, &selected.model).ok()?;
                let visible = selected.visible_modules(
                    &selected.selected.module,
                    android_tools::project_model::SourceScope::Main,
                );
                selected
                    .modules()
                    .flat_map(|(module, variant)| {
                        variant
                            .components
                            .iter()
                            .map(move |component| (module, component))
                    })
                    .flat_map(|(module, component)| {
                        component
                            .sources
                            .iter()
                            .map(move |source| (module, component, source))
                    })
                    .filter(|(_, _, source)| absolute.starts_with(&source.path))
                    .max_by_key(|(_, _, source)| source.path.components().count())
                    .map(|(module, component, source)| {
                        visible.contains(&module.path)
                            && !source.generated
                            && component.scope == android_tools::project_model::SourceScope::Main
                    })
            })
            .unwrap_or_else(|| path.strip_prefix(&self.root).ok()
                .and_then(|path| RelPath::new(path, util::paths::PathStyle::local()).ok())
                .is_some_and(|path| preview_input(&path)))
    }

    fn configuration_current(&self, cx: &App) -> bool {
        self.project
            .read(cx)
            .android_model()
            .is_current(&self.model_token)
            && self
                .panel
                .read_with(cx, |panel, cx| {
                    panel.operation_permitted(AndroidOperation::Preview, cx)
                        && panel.trusted_root(cx).is_ok_and(|root| root == self.root)
                        && panel.selected_target.as_ref() == Some(&self.target)
                })
                .unwrap_or(false)
    }

    fn selected_model(
        &self,
        root: &Path,
        target: &AndroidTarget,
        cx: &App,
    ) -> Result<Arc<android_tools::project_model::SelectedProject>> {
        let selected = self
            .project
            .read(cx)
            .android_model()
            .selected
            .clone()
            .context("Sync the Android project and select a build variant first")?;
        selected.validate_target(target)?;
        ensure!(
            root == selected.model.root
                || root
                    .canonicalize()
                    .is_ok_and(|root| root == selected.model.root),
            "The preview belongs to a different Android project model"
        );
        Ok(selected)
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = (|| {
            let (owner, target) = self.panel.read_with(cx, |panel, cx| {
                Ok::<_, anyhow::Error>((
                    panel.operation_owner(AndroidOperation::Preview, cx)?,
                    panel
                        .selected_target
                        .clone()
                        .context("Select an Android build variant")?,
                ))
            })??;
            let root = owner.root.clone();
            let selected = self.selected_model(&root, &target, cx)?;
            let model_source_path = model_source_path(&self.project.read(cx), &root, &self.source_path, &selected.model)?;
            let visible = selected.visible_modules(
                &selected.selected.module,
                android_tools::project_model::SourceScope::Main,
            );
            ensure!(
                selected
                    .modules()
                    .filter(|(module, _)| visible.contains(&module.path))
                    .any(|(_, variant)| variant.components.iter().any(|component| {
                        component.scope == android_tools::project_model::SourceScope::Main
                            && component.sources.iter().any(|source| {
                                matches!(
                                    source.kind,
                                    android_tools::project_model::SourceKind::Kotlin
                                        | android_tools::project_model::SourceKind::Java
                                ) && model_source_path.starts_with(&source.path)
                            })
                    })),
                "The preview file does not belong to the selected Android variant's main sources"
            );
            let model_token = self.project.read(cx).android_model().token();
            self.configure(root.clone(), target.clone(), window, cx);
            ensure!(
                self.panel.read_with(cx, |panel, cx| panel.preview_path_owned(&self.source_path, cx))?,
                "The preview file belongs to a different Android project"
            );
            let source_text = self.source.read(cx).snapshot().text();
            let source_path = self.source_path.clone();
            let package = preview::kotlin_package(&source_text);
            let sources = self
                .project
                .read(cx)
                .opened_buffers(cx)
                .into_iter()
                .filter_map(|buffer| {
                    let path = buffer_path(&buffer, cx)?;
                    (evaluated_module(&self.project.read(cx), &root, &path).is_some()
                        && path.extension().is_some_and(|extension| extension == "kt")
                        && buffer.read(cx).is_dirty())
                    .then(|| (path, buffer.read(cx).snapshot().text()))
                })
                .collect::<Vec<_>>();
            let environment =
                self.project
                    .read(cx)
                    .environment()
                    .clone()
                    .update(cx, |environment, cx| {
                        environment.local_directory_environment(
                            &task::Shell::Program(util::get_system_shell()),
                            Arc::from(root.as_path()),
                            cx,
                        )
                    });
            let terminal_environment = self
                .project
                .read(cx)
                .terminal_settings(&Some(root.clone()), cx)
                .env
                .clone();
            let revision = self.revision;
            let executor = cx.background_executor().clone();
            self.pending = false;
            self.pending_manual = false;
            self.building = true;
            self.error = None;
            self.status = "Building and rendering Compose previews…".into();
            self.render_task = Some(cx.spawn_in(window, async move |view, cx| {
                let request_root = root.clone();
                let request_target = target.clone();
                let mut environment = environment.await.unwrap_or_default();
                environment.extend(terminal_environment);
                let result = async {
                    view.read_with(cx, |view, cx| view.panel.read_with(cx, |panel, cx| {
                        panel.verify_operation_owner(&owner, AndroidOperation::Preview, cx)
                    }))???;
                    // Await blocking extraction separately so cancelling setup cannot start a project build.
                    let (installation, java) = cx.background_spawn(async {
                        let installation = preview::installation()?;
                        let java = preview::java_binary(&installation)?;
                        Ok::<_, anyhow::Error>((installation, java))
                    }).await?;
                    ensure!(view.update_in(cx, |view, _, cx| {
                        view.project.read(cx).android_model().is_current(&model_token)
                            && view.configuration_current(cx)
                            && view.revision == revision
                            && view.panel.read_with(cx, |panel, cx| panel.verify_operation_owner(&owner, AndroidOperation::Preview, cx).is_ok()).unwrap_or(false)
                    })?, "Discarded an outdated Compose preview request");
                    let worker_owner = owner.clone();
                    cx.background_spawn(async move {
                        worker_owner.ensure_active()?;
                        let temporary = preview::prepare()?;
                        let directory = temporary.path();
                        let overlay = preview::write_source_overlay(directory, &sources)?;
                        let program = if cfg!(windows) {
                            root.join("gradlew.bat")
                        } else {
                            PathBuf::from("/bin/sh")
                        };
                        let mut arguments = if cfg!(windows) {
                            Vec::new()
                        } else {
                            vec!["./gradlew".into()]
                        };
                        arguments.extend([
                            "--init-script".into(),
                            directory.join("export.gradle").to_string_lossy().into_owned(),
                            format!("-Dzed.android.module={}", target.module),
                            format!("-Dzed.android.variant={}", target.variant),
                            target.gradle_task("assemble", ""),
                            format!(
                                "{}:{}",
                                target.module.trim_end_matches(':'),
                                preview::MODEL_TASK
                            ),
                            "--no-configuration-cache".into(),
                            "--console=plain".into(),
                        ]);
                        if let Some(overlay) = overlay {
                            arguments.push(format!(
                                "-Dzed.android.preview.overlay={}",
                                overlay.display()
                            ));
                        }
                        let output = preview_command(
                            program,
                            arguments,
                            &root,
                            &environment,
                            &executor,
                            Duration::from_secs(300),
                        )
                        .await?;
                        worker_owner.ensure_active()?;
                        let model = preview::parse_model(&output, &root, &target)?;
                        preview::validate_selection(&model, &selected)?;
                        ensure!(model.source_files.iter().any(|path| path == &model_source_path
                            || path.canonicalize().is_ok_and(|path| path == model_source_path)),
                            "The preview file was not compiled for the selected Android variant");
                        let model_path = directory.join("model.json");
                        std::fs::write(&model_path, serde_json::to_vec(&model)?)?;
                        let previews_path = directory.join("previews.json");
                        preview_command(
                            java.clone(),
                            preview::bridge_arguments(
                                &installation,
                                "discover",
                                &model_path,
                                Some(&previews_path),
                            )?,
                            &root,
                            &environment,
                            &executor,
                            Duration::from_secs(60),
                        )
                        .await?;
                        worker_owner.ensure_active()?;
                        let previews = preview::read_previews(&previews_path)?
                            .into_iter()
                            .filter(|preview| preview.belongs_to(&source_path, &package))
                            .collect::<Vec<_>>();
                        let mut cards = Vec::new();
                        if !previews.is_empty() {
                            let apk = target
                                .apk_paths()?
                                .into_iter()
                                .next()
                                .context("The build produced no resource APK")?;
                            let settings = preview::render_settings(
                                &model,
                                &previews,
                                &apk,
                                &installation,
                                directory,
                            );
                            let settings_path = directory.join("render.json");
                            std::fs::write(&settings_path, serde_json::to_vec(&settings)?)?;
                            preview_command(
                                java,
                                preview::bridge_arguments(
                                    &installation,
                                    "render",
                                    &settings_path,
                                    None,
                                )?,
                                &root,
                                &environment,
                                &executor,
                                Duration::from_secs(180),
                            )
                            .await?;
                            worker_owner.ensure_active()?;
                            let results = preview::rendered_previews(directory, &previews)?;
                            let names = results
                                .iter()
                                .flat_map(|result| {
                                    result.nodes.iter().map(|node| node.file_name.clone())
                                })
                                .collect();
                            let source_index = preview::SourceIndex::new(
                                &root,
                                &model.source_files,
                                &sources,
                                &names,
                            )?;
                            for mut result in results {
                            for node in &mut result.nodes {
                                node.source_path = source_index.resolve(node, &source_path);
                            }
                            if result.inspection_error.is_none() && result.nodes.iter().all(|node| node.source_path.is_none()) {
                                result.inspection_error = Some("No project source locations are available. Enable source information in the Compose compiler to navigate layouts.".into());
                            }
                                let definition = previews
                                    .iter()
                                    .find(|preview| preview.id == result.id)
                                    .context("Unknown preview result")?;
                                cards.push(PreviewCard::new(definition, result)?);
                            }
                        }
                        Ok::<_, anyhow::Error>(Gallery::new(cards, temporary))
                    }).await
                }.await;
                view.update_in(cx, |view, window, cx| {
                    view.building = false;
                    view.render_task = None;
                    let valid = view.project.read(cx).android_model().is_current(&model_token)
                        && view
                        .panel
                        .read_with(cx, |panel, cx| {
                            panel.verify_operation_owner(&owner, AndroidOperation::Preview, cx).is_ok()
                                && panel
                                .trusted_root(cx)
                                .is_ok_and(|root| root == request_root)
                                && panel.selected_target.as_ref() == Some(&request_target)
                        })
                        .unwrap_or(false);
                    if view.revision == revision && valid {
                        match result {
                            Ok(gallery) => {
                                view.status = if gallery.cards.is_empty() {
                                    "No @Preview composables in this file".into()
                                } else {
                                    format!("{} previews · up to date", gallery.cards.len()).into()
                                };
                                view.replace_gallery(gallery, window, cx);
                                view.stale = false;
                            }
                            Err(error) => {
                                view.error = Some(format!("{error:#}"));
                                view.status = "Preview refresh failed".into();
                            }
                        }
                    }
                    if view.pending && view.visible(cx) {
                        cx.defer_in(window, |view, window, cx| {
                            view.queue_refresh(view.pending_manual, window, cx)
                        });
                    }
                    cx.notify();
                })
                .log_err();
            }));
            Ok::<_, anyhow::Error>(())
        })();
        if let Err(error) = result {
            self.pending = false;
            self.pending_manual = false;
            self.error = Some(format!("{error:#}"));
            self.status = "Preview refresh failed".into();
        }
        cx.notify();
    }

    fn release_images(&mut self, window: &mut Window, cx: &mut App) {
        if let Some(gallery) = &mut self.gallery {
            for card in &mut gallery.cards {
                if let Some(image) = card.rendered_image.take() {
                    window.drop_image(image).log_err();
                }
                if let Some(image) = &card.image {
                    image.clone().remove_asset(cx);
                }
            }
        }
    }

    fn release_gallery(&mut self, window: &mut Window, cx: &mut App) {
        self.gallery_generation = self.gallery_generation.wrapping_add(1);
        self.release_images(window, cx);
        self.rows.clear();
        self.selected_card = None;
        self.hovered = None;
        self.gallery = None;
    }

    fn replace_gallery(&mut self, gallery: Gallery, window: &mut Window, cx: &mut App) {
        let selected = self
            .selected_card
            .and_then(|index| self.gallery.as_ref()?.cards.get(index))
            .map(|card| (card.result.id.clone(), card.result.parameter_index));
        let top = self.list_state.logical_scroll_top();
        let row = self.rows.get(top.item_ix);
        let method = row
            .and_then(|row| self.gallery.as_ref()?.groups.get(row.group()))
            .map(|group| group.method.clone());
        let anchor = row
            .and_then(|row| match row {
                GalleryRow::Cards { cards, .. } => {
                    self.gallery.as_ref()?.cards.get(*cards.first()?)
                }
                GalleryRow::Header(_) => None,
            })
            .map(|card| (card.result.id.clone(), card.result.parameter_index));
        self.release_gallery(window, cx);
        self.selected_card = selected.and_then(|(id, parameter)| {
            gallery
                .cards
                .iter()
                .position(|card| card.result.id == id && card.result.parameter_index == parameter)
        });
        let methods = gallery
            .groups
            .iter()
            .map(|group| group.method.as_str())
            .collect::<HashSet<_>>();
        self.collapsed_groups
            .retain(|method| methods.contains(method.as_str()));
        self.gallery = Some(gallery);
        self.reflow();
        let gallery = self.gallery.as_ref();
        let card_index = anchor.and_then(|(id, parameter)| {
            gallery?
                .cards
                .iter()
                .position(|card| card.result.id == id && card.result.parameter_index == parameter)
        });
        let item_ix = card_index
            .and_then(|index| {
                self.rows.iter().position(
                    |row| matches!(row, GalleryRow::Cards { cards, .. } if cards.contains(&index)),
                )
            })
            .or_else(|| {
                method.and_then(|method| {
                    self.rows.iter().position(|row| match row {
                        GalleryRow::Header(group) => gallery
                            .and_then(|gallery| gallery.groups.get(*group))
                            .is_some_and(|group| group.method == method),
                        GalleryRow::Cards { .. } => false,
                    })
                })
            });
        if let Some(item_ix) = item_ix {
            self.list_state.scroll_to(gpui::ListOffset {
                item_ix,
                offset_in_item: px(0.),
            });
        }
    }

    fn navigate(
        &mut self,
        card_index: usize,
        node_index: usize,
        revision: u64,
        gallery_generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.stale
            || self.revision != revision
            || self.gallery_generation != gallery_generation
            || !self.configuration_current(cx)
        {
            return;
        }
        let Some(node) = self
            .gallery
            .as_ref()
            .and_then(|gallery| gallery.cards.get(card_index))
            .and_then(|card| card.nodes.get(node_index))
            .cloned()
        else {
            return;
        };
        let Some(path) = node.source_path else { return };
        let Some(project_path) = self.project.read(cx).find_project_path(&path, cx) else {
            return;
        };
        self.navigation_source = Some(path);
        self.navigation_pending = true;
        let workspace = self.workspace.clone();
        let pane = self.source_pane.clone();
        let project = self.project.clone();
        self.source_task = Some(cx.spawn_in(window, async move |view, cx| {
            if !view
                .read_with(cx, |view, cx| {
                    view.configuration_current(cx)
                        && view.revision == revision
                        && view.gallery_generation == gallery_generation
                        && !view.stale
                })
                .unwrap_or(false)
            {
                return;
            }
            // Load without activating a tab, then gate activation on the current model.
            let opened = project.update(cx, |project, cx| project.open_buffer(project_path, cx));
            let result = async {
                let buffer = opened.await?;
                if !view.read_with(cx, |view, cx| {
                    view.configuration_current(cx)
                        && view.revision == revision
                        && view.gallery_generation == gallery_generation
                        && !view.stale
                })? {
                    return Ok(());
                }
                let pane = pane
                    .upgrade()
                    .context("The preview source pane was closed")?;
                let editor = workspace.update_in(cx, |workspace, window, cx| {
                    workspace.open_project_item::<Editor>(
                        Some(pane),
                        buffer,
                        true,
                        true,
                        false,
                        true,
                        window,
                        cx,
                    )
                })?;
                editor.update_in(cx, |editor, window, cx| {
                    editor.go_to_singleton_buffer_point(
                        language::Point::new(node.line_number.saturating_sub(1) as u32, 0),
                        window,
                        cx,
                    )
                })?;
                Ok::<_, anyhow::Error>(())
            }
            .await;
            view.update(cx, |view, cx| {
                view.source_task = None;
                view.navigation_pending = false;
                if let Err(error) = result {
                    view.navigation_source = None;
                    view.error = Some(format!("Could not navigate to source: {error:#}"));
                    cx.notify();
                }
            })
            .log_err();
        }));
    }

    fn reflow(&mut self) {
        let Some(gallery) = &self.gallery else {
            self.rows.clear();
            self.list_state.reset(0);
            return;
        };
        let width = self.viewport_width.get();
        if self.fit_to_window && width > GALLERY_INSET {
            self.zoom = gallery.fit_zoom(width);
            self.horizontal_scroll.set_offset(point(px(0.), px(0.)));
        }
        let rows = gallery.rows(self.zoom, width, &self.collapsed_groups);
        if rows == self.rows {
            self.list_state.remeasure();
            return;
        }
        let top = self.list_state.logical_scroll_top();
        let anchor = self.rows.get(top.item_ix).cloned();
        self.list_state.reset(rows.len());
        if let Some(anchor) = anchor {
            let index = rows
                .iter()
                .position(|row| match (&anchor, row) {
                    (GalleryRow::Cards { cards: old, .. }, GalleryRow::Cards { cards, .. }) => {
                        old.first().is_some_and(|index| cards.contains(index))
                    }
                    (GalleryRow::Header(old), GalleryRow::Header(group)) => old == group,
                    _ => false,
                })
                .or_else(|| {
                    rows.iter()
                        .position(|row| *row == GalleryRow::Header(anchor.group()))
                });
            if let Some(item_ix) = index {
                self.list_state.scroll_to(gpui::ListOffset {
                    item_ix,
                    offset_in_item: px(0.),
                });
            }
        }
        self.rows = rows;
    }

    fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        self.fit_to_window = false;
        self.zoom = zoom.clamp(MINIMUM_ZOOM, MAXIMUM_ZOOM);
        self.reflow();
        cx.notify();
    }

    fn reveal_card(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(card) = self
            .gallery
            .as_ref()
            .and_then(|gallery| gallery.cards.get(index))
        {
            self.collapsed_groups.remove(&card.method);
            self.selected_card = Some(index);
            self.reflow();
            if let Some(item_ix) = self.rows.iter().position(
                |row| matches!(row, GalleryRow::Cards { cards, .. } if cards.contains(&index)),
            ) {
                self.list_state.scroll_to(gpui::ListOffset {
                    item_ix,
                    offset_in_item: px(0.),
                });
            }
            self.horizontal_scroll.set_offset(point(px(0.), px(0.)));
            cx.notify();
        }
    }

    fn render_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match self.rows.get(index).cloned() {
            Some(GalleryRow::Header(group_index)) => {
                let Some(group) = self
                    .gallery
                    .as_ref()
                    .and_then(|gallery| gallery.groups.get(group_index))
                else {
                    return div().into_any_element();
                };
                let method = group.method.clone();
                let collapsed = self.collapsed_groups.contains(&method);
                div()
                    .debug_selector(move || format!("compose-group-{group_index}"))
                    .px_2()
                    .pt_2()
                    .child(
                        div().bg(cx.theme().colors().editor_background).child(
                            ButtonLike::new(("compose-group", group_index))
                                .full_width()
                                .height(px(28.).into())
                                .tab_index(0isize)
                                .aria_label(format!("{} previews", group.label))
                                .tooltip(Tooltip::text(group.label.clone()))
                                .aria_expanded(!collapsed)
                                .child(
                                    h_flex()
                                        .w_full()
                                        .gap_1()
                                        .child(
                                            Icon::new(if collapsed {
                                                IconName::ChevronRight
                                            } else {
                                                IconName::ChevronDown
                                            })
                                            .size(IconSize::Small)
                                            .color(Color::Muted),
                                        )
                                        .child(
                                            Label::new(group.label.clone())
                                                .size(LabelSize::Small)
                                                .weight(gpui::FontWeight::SEMIBOLD)
                                                .truncate(),
                                        ),
                                )
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    if !view.collapsed_groups.remove(&method) {
                                        view.collapsed_groups.insert(method.clone());
                                    }
                                    view.reflow();
                                    cx.notify();
                                })),
                        ),
                    )
                    .into_any_element()
            }
            Some(GalleryRow::Cards { cards, .. }) => {
                let mut row = h_flex()
                    .items_start()
                    .gap(px(CARD_GAP))
                    .w_full()
                    .pl_3()
                    .border_l_1()
                    .border_color(cx.theme().colors().border);
                for index in cards {
                    row = row.child(self.render_card(index, window, cx));
                }
                div().pl_3().pr_2().pb_3().child(row).into_any_element()
            }
            None => div().into_any_element(),
        }
    }

    fn render_card(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(card) = self
            .gallery
            .as_mut()
            .and_then(|gallery| gallery.cards.get_mut(index))
        else {
            return div().into_any_element();
        };
        let nodes = card.nodes.clone();
        let revision = self.revision;
        let gallery_generation = self.gallery_generation;
        let width = card.width(self.zoom);
        let inspection_error = card.result.inspection_error.clone();
        let source_diagnostic = inspection_error.clone();
        let diagnostic = card.result.error.clone();
        let can_inspect = !self.stale && card.image.is_some();
        let mut content = v_flex().flex_none().w(px(width)).gap_1().child(
            h_flex()
                .h_6()
                .gap_1()
                .w_full()
                .child(
                    div()
                        .id(("compose-variant-title", index))
                        .flex_1()
                        .min_w_0()
                        .tooltip(Tooltip::text(card.variant.clone()))
                        .child(
                            Label::new(card.variant.clone())
                                .size(LabelSize::Small)
                                .truncate(),
                        ),
                )
                .when_some(inspection_error, |header, error| {
                    header.child(
                        IconButton::new(("compose-inspection-warning", index), IconName::Warning)
                            .icon_size(IconSize::Small)
                            .icon_color(Color::Warning)
                            .aria_label(format!("Source navigation unavailable: {error}"))
                            .tooltip(Tooltip::text(error))
                            .tab_index(0isize),
                    )
                })
                .child(
                    PopoverMenu::new(("compose-components", index))
                        .trigger(
                            IconButton::new(
                                ("compose-components-trigger", index),
                                IconName::EllipsisVertical,
                            )
                            .icon_size(IconSize::Small)
                            .aria_label(format!("{} preview options", card.variant))
                            .tooltip(Tooltip::text("Preview options and component sources"))
                            .tab_index(0isize),
                        )
                        .menu({
                            let view = cx.weak_entity();
                            let nodes = nodes.clone();
                            move |window, cx| {
                                Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                                    let inspect_view = view.clone();
                                    menu = menu.item(
                                        ContextMenuEntry::new("Inspect components")
                                            .disabled(!can_inspect)
                                            .handler(move |_, cx| {
                                                inspect_view
                                                    .update(cx, |view, cx| {
                                                        if view.revision == revision
                                                            && view.gallery_generation
                                                                == gallery_generation
                                                        {
                                                            view.selected_card = Some(index);
                                                            view.inspect = true;
                                                            view.pan_mode = false;
                                                            cx.notify();
                                                        }
                                                    })
                                                    .log_err();
                                            }),
                                    );
                                    if let Some(diagnostic) = &diagnostic {
                                        let diagnostic = diagnostic.clone();
                                        menu =
                                            menu.entry("Copy render error", None, move |_, cx| {
                                                cx.write_to_clipboard(
                                                    gpui::ClipboardItem::new_string(
                                                        diagnostic.clone(),
                                                    ),
                                                );
                                            });
                                    }
                                    if let Some(diagnostic) = &source_diagnostic {
                                        let diagnostic = diagnostic.clone();
                                        menu = menu.entry(
                                            "Copy source navigation error",
                                            None,
                                            move |_, cx| {
                                                cx.write_to_clipboard(
                                                    gpui::ClipboardItem::new_string(
                                                        diagnostic.clone(),
                                                    ),
                                                );
                                            },
                                        );
                                    }
                                    let mut seen = std::collections::BTreeSet::new();
                                    let mut has_sources = false;
                                    for (node_index, node) in nodes.iter().enumerate() {
                                        if let Some(path) = &node.source_path
                                            && node.line_number > 0
                                            && seen.insert((path.clone(), node.line_number))
                                        {
                                            if !has_sources {
                                                menu = menu.separator().header("Component sources");
                                                has_sources = true;
                                            }
                                            let label =
                                                format!("{}:{}", node.file_name, node.line_number);
                                            let view = view.clone();
                                            menu = menu.item(
                                                ContextMenuEntry::new(label)
                                                    .disabled(!can_inspect)
                                                    .handler(move |window, cx| {
                                                        view.update(cx, |view, cx| {
                                                            view.navigate(
                                                                index,
                                                                node_index,
                                                                revision,
                                                                gallery_generation,
                                                                window,
                                                                cx,
                                                            )
                                                        })
                                                        .log_err();
                                                    }),
                                            );
                                        }
                                    }
                                    menu
                                }))
                            }
                        }),
                ),
        );
        if let Some(error) = &card.result.error {
            content = content.child(
                div()
                    .id(("compose-card-error", index))
                    .debug_selector(move || format!("compose-card-error-{index}"))
                    .p_2()
                    .max_h(px(180.))
                    .overflow_y_scroll()
                    .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        Label::new(error.clone())
                            .size(LabelSize::Small)
                            .color(Color::Error),
                    ),
            );
        }
        if let Some(image) = &card.image {
            card.rendered_image = image.clone().get_render_image(window, cx);
            let bounds = Rc::new(Cell::new(Bounds::<Pixels>::default()));
            let image_width = card.result.width as f32;
            let image_height = card.result.height as f32;
            let inspected =
                self.inspect && self.selected_card.is_none_or(|selected| selected == index);
            let selected = self.selected_card == Some(index);
            let hovered = self.hovered;
            let color = cx.theme().colors().text_accent;
            let normal_color = cx.theme().colors().border;
            let outlines = card.outlines.clone();
            let selected_bounds = hovered
                .filter(|(card, _)| *card == index)
                .and_then(|(_, node)| nodes.get(node))
                .map(|node| node.bounds);
            let paint_bounds = bounds.clone();
            content = content.child(
                div()
                    .id(("compose-image", index))
                    .debug_selector(move || format!("compose-image-{index}"))
                    .relative()
                    .w(px(image_width * self.zoom))
                    .h(px(image_height * self.zoom))
                    .flex_none()
                    .cursor(if self.pan_mode {
                        gpui::CursorStyle::OpenHand
                    } else {
                        gpui::CursorStyle::PointingHand
                    })
                    .child(img(image.clone()).size_full())
                    .child(
                        canvas(
                            move |bounds, _, _| {
                                paint_bounds.set(bounds);
                                bounds
                            },
                            move |bounds, _, window, _| {
                                window.paint_quad(gpui::outline(
                                    bounds,
                                    if selected { color } else { normal_color },
                                    gpui::BorderStyle::default(),
                                ));
                                if !inspected {
                                    return;
                                }
                                let scale_x = f32::from(bounds.size.width) / image_width;
                                let scale_y = f32::from(bounds.size.height) / image_height;
                                window.with_content_mask(
                                    Some(gpui::ContentMask { bounds }),
                                    |window| {
                                        for rectangle in
                                            outlines.iter().copied().chain(selected_bounds)
                                        {
                                            let [left, top, right, bottom] = rectangle;
                                            if right <= left || bottom <= top {
                                                continue;
                                            }
                                            let rectangle = Bounds::new(
                                                bounds.origin
                                                    + point(
                                                        px(left as f32 * scale_x),
                                                        px(top as f32 * scale_y),
                                                    ),
                                                size(
                                                    px((right as f32 - left as f32) * scale_x),
                                                    px((bottom as f32 - top as f32) * scale_y),
                                                ),
                                            );
                                            window.paint_quad(gpui::outline(
                                                rectangle,
                                                if selected_bounds
                                                    == Some([left, top, right, bottom])
                                                {
                                                    color
                                                } else {
                                                    normal_color
                                                },
                                                gpui::BorderStyle::default(),
                                            ));
                                        }
                                    },
                                );
                            },
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    )
                    .on_mouse_move({
                        let bounds = bounds.clone();
                        let nodes = nodes.clone();
                        cx.listener(move |view, event: &gpui::MouseMoveEvent, _, cx| {
                            if view.pan_mode {
                                return;
                            }
                            let bounds = bounds.get();
                            let x = f32::from(event.position.x - bounds.origin.x) * image_width
                                / f32::from(bounds.size.width);
                            let y = f32::from(event.position.y - bounds.origin.y) * image_height
                                / f32::from(bounds.size.height);
                            let hovered = (!view.stale
                                && view.revision == revision
                                && view.gallery_generation == gallery_generation)
                                .then(|| preview::hit_test(&nodes, x, y))
                                .flatten()
                                .map(|node| (index, node));
                            if hovered != view.hovered {
                                view.hovered = hovered;
                                cx.notify();
                            }
                        })
                    })
                    .on_hover(cx.listener(move |view, hovered: &bool, _, cx| {
                        if !hovered
                            && view.gallery_generation == gallery_generation
                            && view.hovered.is_some_and(|(card, _)| card == index)
                        {
                            view.hovered = None;
                            cx.notify();
                        }
                    }))
                    .on_mouse_down(MouseButton::Left, {
                        cx.listener(move |view, event: &gpui::MouseDownEvent, window, cx| {
                            if view.pan_mode
                                || view.stale
                                || view.revision != revision
                                || view.gallery_generation != gallery_generation
                            {
                                return;
                            }
                            let bounds = bounds.get();
                            let x = f32::from(event.position.x - bounds.origin.x) * image_width
                                / f32::from(bounds.size.width);
                            let y = f32::from(event.position.y - bounds.origin.y) * image_height
                                / f32::from(bounds.size.height);
                            let hit = preview::hit_test(&nodes, x, y);
                            if view.inspect && view.selected_card == Some(index) {
                                if let Some(node) = hit {
                                    view.navigate(
                                        index,
                                        node,
                                        revision,
                                        gallery_generation,
                                        window,
                                        cx,
                                    );
                                }
                            } else {
                                view.inspect = true;
                            }
                            view.selected_card = Some(index);
                            view.hovered = hit.map(|node| (index, node));
                            cx.notify();
                        })
                    }),
            );
        }
        content.into_any_element()
    }
}

fn buffer_path(buffer: &Entity<Buffer>, cx: &App) -> Option<PathBuf> {
    Some(buffer.read(cx).file()?.as_local()?.abs_path(cx))
}

fn evaluated_snapshot<'a>(project: &'a Project, root: &Path) -> Option<&'a android_tools::project_context::ContextSnapshot> {
    let store = project.android_context();
    store.handles().find_map(|handle| (store.root_path(handle) == Some(root)).then(|| store.snapshot(handle)).flatten())
}

fn evaluated_module<'a>(project: &'a Project, root: &Path, path: &Path) -> Option<&'a android_tools::project_context::ModuleContext> {
    let snapshot = evaluated_snapshot(project, root)?;
    let android_tools::project_context::ModuleOwner::Module(owner) = snapshot.module_owner(path) else { return None; };
    snapshot.modules().find(|module| module.path() == owner)
}

fn model_source_path(project: &Project, root: &Path, path: &Path, model: &android_tools::project_model::ProjectModel) -> Result<PathBuf> {
    let module = evaluated_module(project, root, path).context("The preview file has no current evaluated Android project owner")?;
    if let Ok(relative) = path.strip_prefix(root) {
        return Ok(model.root.join(relative));
    }
    let model_module = model.modules.iter().find(|candidate| candidate.path == module.path())
        .context("The evaluated sibling module is absent from the current Android model")?;
    Ok(model_module.directory.join(path.strip_prefix(module.directory())?))
}

fn preview_input(path: &RelPath) -> bool {
    if path
        .components()
        .any(|part| matches!(part, ".koda" | ".gradle" | "build" | ".git"))
    {
        return false;
    }
    matches!(
        path.extension(),
        Some("kt" | "java" | "xml" | "png" | "webp" | "jpg" | "properties")
    ) || path.file_name().is_some_and(|name| {
        matches!(
            name,
            "build.gradle" | "build.gradle.kts" | "settings.gradle" | "settings.gradle.kts"
        )
    })
}

async fn preview_command(
    program: PathBuf,
    arguments: Vec<String>,
    root: &Path,
    environment: &collections::HashMap<String, String>,
    executor: &BackgroundExecutor,
    timeout: Duration,
) -> Result<String> {
    let mut command = util::command::new_std_command(program);
    command.args(arguments).current_dir(root).envs(environment);
    let (sender, mut receiver) = futures::channel::mpsc::channel::<android_build::OutputLine>(128);
    let (cancel, cancelled) = oneshot::channel();
    let drain = async move {
        let mut tail = String::new();
        while let Some(line) = receiver.next().await {
            tail.push_str(&line.text);
            tail.push('\n');
            if tail.len() > 4000 {
                let mut start = tail.len() - 4000;
                while !tail.is_char_boundary(start) {
                    start += 1;
                }
                tail.drain(..start);
            }
        }
        tail
    };
    let (result, tail) = futures::join!(
        android_build::command_output(command, executor, timeout, sender, cancelled, true),
        drain
    );
    drop(cancel);
    match result {
        Ok(output) => output.stdout(),
        Err(error) if tail.is_empty() => Err(error),
        Err(error) => Err(error).with_context(|| tail),
    }
}

impl Focusable for ComposePreviewView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl Render for ComposePreviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let maximum_width = self
            .gallery
            .as_ref()
            .map(|gallery| gallery.maximum_width(self.zoom))
            .unwrap_or(0.);
        let empty = self
            .gallery
            .as_ref()
            .is_none_or(|gallery| gallery.cards.is_empty());
        let failures = self
            .gallery
            .as_ref()
            .map(|gallery| gallery.failures)
            .unwrap_or(0);
        let (status_icon, status_label, status_color) = if self.building {
            (
                IconName::LoadCircle,
                "Refreshing…".to_string(),
                Color::Muted,
            )
        } else if self.error.is_some() {
            (
                IconName::Warning,
                "Refresh failed".to_string(),
                Color::Error,
            )
        } else if self.stale {
            (IconName::Clock, "Out of date".to_string(), Color::Warning)
        } else if empty {
            (IconName::Info, "No previews".to_string(), Color::Muted)
        } else if failures > 0 {
            (
                IconName::Warning,
                format!("{failures} failed"),
                Color::Error,
            )
        } else {
            (IconName::Check, "Up-to-date".to_string(), Color::Success)
        };
        let viewport_width = self.viewport_width.clone();
        let view = cx.weak_entity();
        v_flex()
            .size_full()
            .bg(cx.theme().colors().panel_background)
            .track_focus(&self.focus_handle)
            .child(
                h_flex()
                    .flex_none()
                    .h_9()
                    .px_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        IconButton::new("compose-refresh", IconName::Rerun)
                            .aria_label("Build and refresh previews")
                            .tooltip(Tooltip::text("Build and refresh previews"))
                            .tab_index(0isize)
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.queue_refresh(true, window, cx)
                            })),
                    )
                    .when(self.building, |toolbar| {
                        toolbar.child(
                            IconButton::new("compose-stop", IconName::Stop)
                                .aria_label("Stop preview refresh")
                                .tooltip(Tooltip::text("Stop preview refresh"))
                                .tab_index(0isize)
                                .on_click(cx.listener(|view, _, _, cx| view.stop(cx))),
                        )
                    })
                    .child(
                        IconButton::new("compose-inspect", IconName::Eye)
                            .aria_label("Show component outlines")
                            .tooltip(Tooltip::text(
                                "Show component outlines; click a component to open its source",
                            ))
                            .toggle_state(self.inspect)
                            .tab_index(0isize)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.inspect = !view.inspect;
                                view.pan_mode = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        PopoverMenu::new("compose-preview-list")
                            .trigger(
                                IconButton::new("compose-preview-list-trigger", IconName::ListTree)
                                    .aria_label("Choose a preview")
                                    .tooltip(Tooltip::text("Choose a preview"))
                                    .disabled(empty)
                                    .tab_index(0isize),
                            )
                            .menu({
                                let view = cx.weak_entity();
                                let revision = self.revision;
                                let gallery_generation = self.gallery_generation;
                                let labels = self
                                    .gallery
                                    .as_ref()
                                    .map(|gallery| gallery.labels.clone())
                                    .unwrap_or_default();
                                move |window, cx| {
                                    Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                                        for (index, label) in labels.iter().enumerate() {
                                            let view = view.clone();
                                            menu = menu.entry(label.clone(), None, move |_, cx| {
                                                view.update(cx, |view, cx| {
                                                    if view.revision == revision
                                                        && view.gallery_generation
                                                            == gallery_generation
                                                    {
                                                        view.reveal_card(index, cx);
                                                    }
                                                })
                                                .log_err();
                                            });
                                        }
                                        menu
                                    }))
                                }
                            }),
                    )
                    .child(div().flex_1().min_w_0())
                    .child(
                        div()
                            .id("compose-status")
                            .min_w_0()
                            .tooltip(Tooltip::text(self.status.clone()))
                            .child(
                                h_flex()
                                    .gap_1()
                                    .min_w_0()
                                    .child(if self.building {
                                        Icon::new(status_icon)
                                            .size(IconSize::Small)
                                            .color(status_color)
                                            .with_keyed_rotate_animation(
                                                "compose-refresh-spinner",
                                                2,
                                            )
                                            .into_any_element()
                                    } else {
                                        Icon::new(status_icon)
                                            .size(IconSize::Small)
                                            .color(status_color)
                                            .into_any_element()
                                    })
                                    .child(
                                        Label::new(status_label)
                                            .size(LabelSize::Small)
                                            .color(status_color)
                                            .truncate(),
                                    ),
                            ),
                    ),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .id("compose-error")
                        .p_3()
                        .max_h_32()
                        .overflow_y_scroll()
                        .child(Label::new(error).size(LabelSize::Small).color(Color::Error)),
                )
            })
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .debug_selector(|| "compose-viewport".into())
                    .child(
                        canvas(
                            move |bounds, _, cx| {
                                let width = f32::from(bounds.size.width);
                                if (viewport_width.replace(width) - width).abs() > 0.5 {
                                    cx.defer(move |cx| {
                                        view.update(cx, |view, cx| {
                                            view.reflow();
                                            cx.notify();
                                        })
                                        .log_err();
                                    });
                                }
                            },
                            {
                                let view = cx.weak_entity();
                                move |_, _, window, _| {
                                    window.on_mouse_event(
                                        move |event: &gpui::MouseMoveEvent, phase, _, cx| {
                                            if phase != gpui::DispatchPhase::Capture {
                                                return;
                                            }
                                            view.update(cx, |view, cx| {
                                                if view.pan_mode && view.pan_position.is_some() {
                                                    if event.pressed_button
                                                        == Some(MouseButton::Left)
                                                    {
                                                        if let Some(previous) = view
                                                            .pan_position
                                                            .replace(event.position)
                                                        {
                                                            let delta = event.position - previous;
                                                            view.horizontal_scroll.set_offset(
                                                                view.horizontal_scroll.offset()
                                                                    + point(delta.x, px(0.)),
                                                            );
                                                            view.list_state.scroll_by(-delta.y);
                                                            cx.stop_propagation();
                                                        }
                                                    } else {
                                                        view.pan_position = None;
                                                    }
                                                    cx.notify();
                                                }
                                            })
                                            .log_err();
                                        },
                                    );
                                }
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                    .when(empty && !self.building && self.error.is_none(), |surface| {
                        surface.child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .p_4()
                                .child(
                                    Label::new(if self.gallery.is_some() {
                                        "No @Preview composables in this file"
                                    } else {
                                        "Build and refresh to load Compose previews"
                                    })
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                                ),
                        )
                    })
                    .child(
                        div()
                            .id("compose-horizontal-scroll")
                            .size_full()
                            .overflow_x_scroll()
                            .restrict_scroll_to_axis()
                            .track_scroll(&self.horizontal_scroll)
                            .when(self.pan_mode, |surface| {
                                surface.cursor(if self.pan_position.is_some() {
                                    gpui::CursorStyle::ClosedHand
                                } else {
                                    gpui::CursorStyle::OpenHand
                                })
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|view, event: &gpui::MouseDownEvent, _, cx| {
                                    if view.pan_mode {
                                        view.pan_position = Some(event.position);
                                        cx.notify();
                                    }
                                }),
                            )
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|view, _, _, cx| {
                                    view.pan_position = None;
                                    cx.notify();
                                }),
                            )
                            .on_mouse_up_out(
                                MouseButton::Left,
                                cx.listener(|view, _, _, cx| {
                                    view.pan_position = None;
                                    cx.notify();
                                }),
                            )
                            .child(
                                div()
                                    .h_full()
                                    .w_full()
                                    .min_w(px(maximum_width + GALLERY_INSET))
                                    .child(
                                        list(
                                            self.list_state.clone(),
                                            cx.processor(|view, index, window, cx| {
                                                view.render_row(index, window, cx)
                                            }),
                                        )
                                        .size_full()
                                        .pb(px(160.)),
                                    ),
                            )
                            .custom_scrollbars(
                                ui::Scrollbars::new(ui::ScrollAxes::Horizontal)
                                    .tracked_scroll_handle(&self.horizontal_scroll)
                                    .tracked_entity(cx.entity_id()),
                                window,
                                cx,
                            ),
                    )
                    .custom_scrollbars(
                        ui::Scrollbars::always_visible(ui::ScrollAxes::Vertical)
                            .tracked_scroll_handle(&self.list_state)
                            .tracked_entity(cx.entity_id()),
                        window,
                        cx,
                    )
                    .child(
                        v_flex()
                            .absolute()
                            .bottom_4()
                            .right_4()
                            .w(px(40.))
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .justify_center()
                                    .p_0p5()
                                    .border_1()
                                    .rounded_md()
                                    .border_color(cx.theme().colors().border)
                                    .bg(cx.theme().colors().panel_background)
                                    .child(
                                        IconButton::new("compose-pan", IconName::Hand)
                                            .full_width()
                                            .aria_label("Pan previews")
                                            .tooltip(Tooltip::text("Pan previews by dragging"))
                                            .toggle_state(self.pan_mode)
                                            .tab_index(0isize)
                                            .on_click(cx.listener(|view, _, _, cx| {
                                                view.pan_mode = !view.pan_mode;
                                                view.pan_position = None;
                                                view.hovered = None;
                                                cx.notify();
                                            })),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .w_full()
                                    .p_0p5()
                                    .items_center()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(cx.theme().colors().border)
                                    .bg(cx.theme().colors().panel_background)
                                    .child(
                                        IconButton::new("compose-zoom-in", IconName::Plus)
                                            .full_width()
                                            .aria_label("Zoom in")
                                            .tooltip(Tooltip::text("Zoom in"))
                                            .disabled(empty || self.zoom >= MAXIMUM_ZOOM)
                                            .tab_index(0isize)
                                            .on_click(cx.listener(|view, _, _, cx| {
                                                view.set_zoom(view.zoom * 1.25, cx)
                                            })),
                                    )
                                    .child(
                                        IconButton::new("compose-zoom-out", IconName::Dash)
                                            .full_width()
                                            .aria_label("Zoom out")
                                            .tooltip(Tooltip::text("Zoom out"))
                                            .disabled(empty || self.zoom <= MINIMUM_ZOOM)
                                            .tab_index(0isize)
                                            .on_click(cx.listener(|view, _, _, cx| {
                                                view.set_zoom(view.zoom / 1.25, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("compose-actual-size", "1:1")
                                            .label_size(LabelSize::Small)
                                            .full_width()
                                            .tooltip(Tooltip::text("Actual size"))
                                            .disabled(empty)
                                            .tab_index(0isize)
                                            .on_click(
                                                cx.listener(|view, _, _, cx| view.set_zoom(1., cx)),
                                            ),
                                    )
                                    .child(
                                        IconButton::new("compose-fit", IconName::MaximizeAlt)
                                            .full_width()
                                            .aria_label("Fit previews to the pane")
                                            .tooltip(Tooltip::text("Fit previews to the pane"))
                                            .toggle_state(self.fit_to_window)
                                            .disabled(empty)
                                            .tab_index(0isize)
                                            .on_click(cx.listener(|view, _, _, cx| {
                                                view.fit_to_window = true;
                                                view.reflow();
                                                cx.notify();
                                            })),
                                    ),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use project::FakeFs;
    use workspace::{AppState, item::test::TestItem};

    fn card(method: &str, variant: &str, width: u32, height: u32) -> PreviewCard {
        let mut bytes = format!("P6\n{width} {height}\n255\n").into_bytes();
        bytes.extend(std::iter::repeat_n(
            230,
            width as usize * height as usize * 3,
        ));
        PreviewCard {
            label: format!("{variant} · {method}"),
            method: method.into(),
            variant: variant.into(),
            result: preview::RenderedPreview {
                id: format!("{method}:{variant}"),
                parameter_index: 0,
                image: None,
                width,
                height,
                nodes: Vec::new(),
                error: None,
                inspection_error: None,
            },
            image: Some(Arc::new(Image::from_bytes(ImageFormat::Pnm, bytes))),
            rendered_image: None,
            nodes: Arc::new(Vec::new()),
            outlines: Arc::new(Vec::new()),
        }
    }

    fn gallery(cards: Vec<PreviewCard>) -> Gallery {
        Gallery::new(cards, tempfile::tempdir().expect("Gallery directory"))
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "Requires a headless Vulkan adapter; writes native GPUI screenshots"]
    fn capture_compose_preview_surface() {
        let mut cx = gpui::HeadlessAppContext::with_platform(
            Arc::new(gpui_wgpu::CosmicTextSystem::new("DejaVu Sans")),
            Arc::new(assets::Assets),
            || Ok(Some(Box::new(gpui_wgpu::WgpuHeadlessRenderer::new()?))),
        );
        let (project, buffer, app_state) = cx.update(|cx| {
            assets::Assets.load_fonts(cx).expect("UI fonts");
            let app_state = AppState::test(cx);
            editor::init(cx);
            project::trusted_worktrees::init(Default::default(), cx);
            let project = Project::local(
                app_state.client.clone(), app_state.node_runtime.clone(), app_state.user_store.clone(),
                app_state.languages.clone(), app_state.fs.clone(), None, Default::default(), cx,
            );
            let buffer = cx.new(|cx| Buffer::local(
                "@Composable\nfun Content() {\n    Column {\n        Text(\"Compose previews\")\n        Button(onClick = {}) {\n            Text(\"Open source\")\n        }\n    }\n}\n\n@Preview(name = \"Day\")\n@Preview(name = \"Night\")\n@Composable\nfun ContentPreview() {\n    Content()\n}\n", cx));
            (project, buffer, app_state)
        });
        let window = cx
            .open_window(size(px(1200.), px(900.)), |window, cx| {
                cx.new(|cx| {
                    Workspace::new(Default::default(), project.clone(), app_state, window, cx)
                })
            })
            .expect("Native screenshot window");
        let view = window
            .update(&mut cx, |workspace, window, cx| {
                let editor = cx.new(|cx| {
                    Editor::for_buffer(buffer.clone(), Some(project.clone()), window, cx)
                });
                workspace.active_pane().update(cx, |pane, cx| {
                    pane.add_item(Box::new(editor), true, true, None, window, cx)
                });
                add_preview(workspace, project, buffer, window, cx).0
            })
            .expect("Preview pane");
        view.update(&mut cx, |view, cx| {
            let cards = if let Some(directory) = std::env::var_os("COMPOSE_PREVIEW_VISUAL_FIXTURE")
            {
                let directory = PathBuf::from(directory);
                let previews = preview::read_previews(&directory.join("previews.json"))
                    .expect("Discovered previews");
                preview::rendered_previews(&directory, &previews)
                    .expect("Rendered previews")
                    .into_iter()
                    .map(|result| {
                        let definition = previews
                            .iter()
                            .find(|definition| definition.id == result.id)
                            .expect("Preview definition");
                        PreviewCard::new(definition, result).expect("Preview card")
                    })
                    .collect()
            } else {
                vec![
                    card("sample.ContentPreview", "Day", 400, 800),
                    card("sample.ContentPreview", "Night", 400, 800),
                    card("sample.LoadingPreview", "Desktop", 700, 400),
                    card("sample.LoadingPreview", "Phone", 300, 700),
                    card("sample.LoadingPreview", "Tablet", 500, 600),
                ]
            };
            view.gallery = Some(gallery(cards));
            view.source_path = PathBuf::from("/android/Content.kt");
            view.fit_to_window = true;
            view.stale = false;
            view.status = "Compose previews are up to date".into();
            view.reflow();
            cx.notify();
        });
        cx.run_until_parked();
        let output = PathBuf::from(
            std::env::var_os("COMPOSE_PREVIEW_VISUAL_OUTPUT")
                .unwrap_or_else(|| "target/compose-preview-visuals".into()),
        );
        std::fs::create_dir_all(&output).expect("Screenshot output");
        for (name, width, inspect) in [
            ("gallery", 1200., false),
            ("narrow", 800., false),
            ("inspection", 1200., true),
        ] {
            view.update(&mut cx, |view, cx| {
                view.inspect = inspect;
                view.selected_card = inspect.then_some(0);
                cx.notify();
            });
            cx.update_window(window.into(), |_, window, cx| {
                window.resize(size(px(width), px(900.)));
                window.bounds_changed(cx);
                window.draw(cx).clear(cx);
            })
            .expect("Draw preview surface");
            cx.run_until_parked();
            cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
                .expect("Draw reflowed surface");
            cx.capture_screenshot(window.into())
                .expect("Capture native GPUI surface")
                .save(output.join(format!("{name}.png")))
                .expect("Save screenshot");
        }
        drop(view);
        cx.update_window(window.into(), |_, window, _| window.remove_window())
            .expect("Close screenshot window");
        cx.run_until_parked();
    }

    #[test]
    fn grouping_retains_variants_parameter_values_and_individual_failures() {
        let mut failed = card("sample.Content", "Day · value 2", 100, 100);
        failed.result.error = Some("Preview failed".into());
        failed.result.parameter_index = 1;
        failed.image = None;
        let gallery = gallery(vec![
            card("sample.Content", "Day · value 1", 100, 100),
            card("other.Content", "Night", 100, 100),
            failed,
        ]);
        assert_eq!(gallery.groups.len(), 2);
        assert_eq!(gallery.groups[0].label, "Content");
        assert_eq!(gallery.groups[0].cards, [0, 2]);
        assert_eq!(gallery.groups[1].cards, [1]);
        assert_eq!(gallery.cards[2].variant, "Day · value 2");
        assert!(gallery.cards[2].result.error.is_some());
    }

    #[test]
    fn variable_device_sizes_wrap_at_a_shared_scale_and_collapse_by_function() {
        let gallery = gallery(vec![
            card("sample.Content", "Desktop", 400, 200),
            card("sample.Content", "Phone", 200, 400),
            card("sample.Content", "Tablet", 300, 200),
            card("sample.Content", "Button", 2, 2),
            card("sample.Loading", "Default", 300, 500),
        ]);
        let mut collapsed = HashSet::default();
        assert_eq!(
            gallery.rows(0.5, 400., &collapsed),
            [
                GalleryRow::Header(0),
                GalleryRow::Cards {
                    group: 0,
                    cards: vec![0, 1]
                },
                GalleryRow::Cards {
                    group: 0,
                    cards: vec![2, 3]
                },
                GalleryRow::Header(1),
                GalleryRow::Cards {
                    group: 1,
                    cards: vec![4]
                },
            ]
        );
        collapsed.insert("sample.Content".into());
        assert_eq!(
            gallery.rows(0.5, 400., &collapsed),
            [
                GalleryRow::Header(0),
                GalleryRow::Header(1),
                GalleryRow::Cards {
                    group: 1,
                    cards: vec![4]
                },
            ]
        );
        assert_eq!(gallery.cards[3].width(0.5), MINIMUM_CARD_WIDTH);
        let zoom = gallery.fit_zoom(400.);
        assert!(700. * zoom + CARD_GAP <= 400. - GALLERY_INSET + 0.001);
        assert!(zoom > MINIMUM_ZOOM);
    }

    /// Adapted from AOSP NonComposeProjectTest.`compose preview not available`
    /// at tools/adt/idea a84efec3ba9542d9bfa1255103f0dc94833a3796. The complete
    /// original and Apache notice are in test_data/project_context/NonComposeProjectTest.kt.
    #[gpui::test]
    async fn compose_preview_not_available_in_non_compose_project(cx: &mut TestAppContext) -> Result<()> {
        use android_tools::project_context::PluginId;
        cx.update(|cx| {
            let state = AppState::test(cx);
            editor::init(cx);
            workspace::init(state, cx);
            project::trusted_worktrees::init(Default::default(), cx);
            crate::init(cx);
        });
        let filesystem = FakeFs::new(cx.executor());
        filesystem.insert_tree("/non-compose", serde_json::json!({"Main.kt":"fun testMethod() {\n}"})).await;
        let project = Project::test_with_worktree_trust(filesystem, [Path::new("/non-compose")], cx).await;
        cx.update(|cx| {
            project_surfaces::tests::trust(&project, cx)?;
            project_surfaces::tests::publish_catalogue(&project, Path::new("/non-compose"), &[PluginId::AndroidApplication], &[("android", "androidJvm")], true, cx)
        })?;
        let (workspace, visual) = cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        workspace.update_in(visual, |workspace, window, cx| {
            workspace.open_abs_path(Path::new("/non-compose/Main.kt"), Default::default(), window, cx)
        }).await?;
        visual.run_until_parked();
        let (panel, buffer) = workspace.read_with(visual, |workspace, cx| {
            let panel = workspace.panel::<AndroidPanel>(cx).context("Android project panel")?;
            let editor = workspace.active_item(cx).and_then(|item| item.downcast::<Editor>()).context("Main.kt editor")?;
            let buffer = editor.read(cx).buffer().read(cx).as_singleton().context("Main.kt buffer")?;
            Ok::<_, anyhow::Error>((panel, buffer))
        })?;
        panel.update(visual, |panel, cx| {
            let target = AndroidTarget { module: ":".into(), variant: "debug".into(), output_listing: PathBuf::from("/non-compose/output.json") };
            panel.targets = vec![target.clone()];
            panel.selected_target = Some(target.clone());
            super::super::tests::publish_test_android_model(panel, &target, cx);
            assert!(panel.operation_permitted(AndroidOperation::Run, cx), "The non-Compose Android model is operational");
            assert_eq!(buffer.read(cx).snapshot().text(), "fun testMethod() {\n}");
            assert!(!panel.accepts_compose_preview(&buffer, cx));
        });
        panel.update_in(visual, |panel, window, cx| {
            panel.show_compose_preview(window, cx);
            assert!(panel.preview_view.is_none());
            assert!(!panel.compose_preview_enabled);
        });
        visual.run_until_parked();
        workspace.read_with(visual, |workspace, cx| {
            let editor = workspace.active_item(cx).and_then(|item| item.downcast::<Editor>()).expect("Main.kt editor");
            assert!(editor.read(cx).addon::<ComposePreviewAddon>().is_none());
        });
        Ok(())
    }

    #[gpui::test]
    async fn compose_preview_available_in_compose_project_uses_production_provider(cx: &mut TestAppContext) -> Result<()> {
        use android_tools::project_context::PluginId;
        assert!(cfg!(feature = "bundled-preview"), "This production provider test requires the normal zed bundled-preview dependency graph");
        cx.update(|cx| {
            let state = AppState::test(cx);
            editor::init(cx);
            workspace::init(state, cx);
            project::trusted_worktrees::init(Default::default(), cx);
            crate::init(cx);
        });
        let filesystem = FakeFs::new(cx.executor());
        filesystem.insert_tree("/compose-project", serde_json::json!({"Main.kt":"@Composable\nfun Content() {}"})).await;
        let project = Project::test_with_worktree_trust(filesystem, [Path::new("/compose-project")], cx).await;
        cx.update(|cx| {
            project_surfaces::tests::trust(&project, cx)?;
            project_surfaces::tests::publish_catalogue(&project, Path::new("/compose-project"), &[PluginId::AndroidApplication, PluginId::ComposeCompiler], &[("android", "androidJvm")], true, cx)
        })?;
        let (workspace, visual) = cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        workspace.update_in(visual, |workspace, window, cx| workspace.open_abs_path(Path::new("/compose-project/Main.kt"), Default::default(), window, cx)).await?;
        visual.run_until_parked();
        let (panel, buffer) = workspace.read_with(visual, |workspace, cx| {
            let panel = workspace.panel::<AndroidPanel>(cx).context("Android panel")?;
            let editor = workspace.active_item(cx).and_then(|item| item.downcast::<Editor>()).context("Compose source editor")?;
            let buffer = editor.read(cx).buffer().read(cx).as_singleton().context("Compose buffer")?;
            Ok::<_, anyhow::Error>((panel, buffer))
        })?;
        panel.update(visual, |panel, cx| {
            let target = AndroidTarget { module: ":".into(), variant: "debug".into(), output_listing: PathBuf::from("/compose-project/output.json") };
            panel.targets = vec![target.clone()];
            panel.selected_target = Some(target.clone());
            publish_preview_test_model(panel, &target, cx);
            assert!(panel.operation_permitted(AndroidOperation::Run, cx));
            assert!(panel.accepts_compose_preview(&buffer, cx), "The real provider must accept an operational Compose source");
        });
        // Provider acceptance precedes rendering; this test never schedules an
        // SDK installation, Gradle build, or renderer process.
        Ok(())
    }

    #[gpui::test]
    async fn evaluated_sibling_editor_and_preview_keep_parent_ownership_and_independent_root_priority(cx: &mut TestAppContext) -> Result<()> {
        use android_tools::project_context::{ActiveContext, PluginId, decode_context_record};
        assert!(cfg!(feature = "bundled-preview"), "Use the normal zed bundled-preview graph");
        cx.update(|cx| {
            let state = AppState::test(cx);
            editor::init(cx);
            workspace::init(state, cx);
            project::trusted_worktrees::init(Default::default(), cx);
            crate::init(cx);
        });
        let filesystem = FakeFs::new(cx.executor());
        filesystem.insert_tree("/sibling-parent", serde_json::json!({"settings.gradle.kts":"include(\":app\"); project(\":app\").projectDir = file(\"../sibling-app\")", "gradlew":""})).await;
        filesystem.insert_tree("/sibling-app", serde_json::json!({"build.gradle.kts":"", "src":{"Main.kt":"@Composable\nfun Content() {}"}})).await;
        filesystem.insert_tree("/independent-jvm", serde_json::json!({"settings.gradle.kts":"", "gradlew":"", "Main.kt":"fun main() {}"})).await;
        let project = Project::test_with_worktree_trust(filesystem, [Path::new("/sibling-parent"), Path::new("/sibling-app"), Path::new("/independent-jvm")], cx).await;
        let publish = |cx: &mut App| -> Result<()> {
            let record = serde_json::json!({"schema":1,"root":"/sibling-parent","gradleVersion":"9.6.1","phase":"complete", "modules":[
                (":", "/sibling-parent", false), (":app", "/sibling-app", true), (":independent", "/independent-jvm", true)
            ].map(|(module,directory,android)|serde_json::json!({"path":module,"directory":directory,
                "plugins":PluginId::ALL.map(|plugin|serde_json::json!({"plugin":plugin,"applied":android && matches!(plugin, PluginId::AndroidApplication | PluginId::ComposeCompiler)})),
                "targets":{"status":"available","value":if android {vec![serde_json::json!({"name":"android","platform":"androidJvm"})]} else {vec![]}}}))});
            let snapshot = decode_context_record(&serde_json::to_vec(&record)?, Path::new("/sibling-parent"))?;
            project.update(cx, |project, cx| {
                let worktree = project.visible_worktrees(cx).find(|worktree|worktree.read(cx).abs_path().as_ref() == Path::new("/sibling-parent")).context("Parent worktree")?.read(cx).id();
                let handle = project.ensure_android_context(worktree, true, cx)?;
                let import = project.begin_android_context_import(handle, cx)?;
                let mut active = ActiveContext::default();
                active.select(Some(handle), None)?;
                let owner = active.discovery_token(project.android_context()).context("Fixture import owner")?;
                project.publish_android_context(&active, &owner, &import, snapshot, cx)
            })
        };
        cx.update(|cx| {
            project_surfaces::tests::trust(&project, cx)?;
            publish(cx)?;
            project_surfaces::tests::publish_catalogue(&project, Path::new("/independent-jvm"), &[], &[("jvm","jvm")], true, cx)
        })?;
        let (workspace, visual) = cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        workspace.update_in(visual, |workspace, window, cx|workspace.open_abs_path(Path::new("/sibling-app/src/Main.kt"), Default::default(), window, cx)).await?;
        visual.run_until_parked();
        let (panel, buffer, controller) = workspace.read_with(visual, |workspace, cx| {
            let editor = workspace.active_item(cx).and_then(|item|item.downcast::<Editor>()).context("Sibling editor")?;
            Ok::<_, anyhow::Error>((workspace.panel::<AndroidPanel>(cx).context("Android panel")?,
                editor.read(cx).buffer().read(cx).as_singleton().context("Sibling buffer")?,
                project_context::for_workspace(&workspace.weak_handle(), cx).context("Controller")?))
        })?;
        let select_model = |panel: &mut AndroidPanel, cx: &mut Context<AndroidPanel>| -> Result<()> {
            let target = AndroidTarget {module:":app".into(),variant:"debug".into(),output_listing:PathBuf::from("/sibling-app/output.json")};
            let model = serde_json::from_value(serde_json::json!({"version":1,"root":"/sibling-parent","diagnostics":[],"modules":[{
                "path":":app","directory":"/sibling-app","kind":"application","namespace":"sample", "variants":[{
                    "name":"debug","outputListing":target.output_listing,"components":[{"name":"debug","scope":"main","dependencies":[],"sources":[{"path":"/sibling-app/src","kind":"kotlin","generated":false}]}]}]}]}))?;
            panel.targets = vec![target.clone()]; panel.selected_target = Some(target.clone());
            panel.project.update(cx, |project, cx| {
                let token = project.invalidate_android_model(Some(PathBuf::from("/sibling-parent")), cx);
                project.publish_android_model(&token, model, cx)?;
                project.select_android_variant(Some((&target).into()), cx)
            })
        };
        panel.update(visual, select_model)?;
        let operation_owners = panel.read_with(visual, |panel, cx| {
            [AndroidOperation::Devices, AndroidOperation::Run, AndroidOperation::Preview].map(|operation| panel.operation_owner(operation, cx)).into_iter().collect::<Result<Vec<_>>>()
        })?;
        let first = controller.read_with(visual, |controller, cx| {
            assert_eq!(controller.root(cx), Some(PathBuf::from("/sibling-parent")));
            controller.action_token(cx).context("Sibling action token")
        })?;
        panel.read_with(visual, |panel, cx| assert!(panel.accepts_compose_preview(&buffer, cx)));
        workspace.update_in(visual, |workspace, window, cx|workspace.open_abs_path(Path::new("/independent-jvm/Main.kt"), Default::default(), window, cx)).await?;
        visual.run_until_parked();
        controller.read_with(visual, |controller, cx| {
            assert_eq!(controller.root(cx), Some(PathBuf::from("/independent-jvm")));
            assert!(!controller.capabilities(Default::default(), cx).ecosystems.qualifies());
            assert!(!controller.action_is_current(&first, cx));
        });
        assert!(operation_owners.iter().all(|owner| owner.ensure_active().is_err()), "Pending backend owners are cancelled on the actual editor switch");
        panel.read_with(visual, |panel, cx| assert!(!panel.accepts_compose_preview(&buffer, cx)));
        workspace.update_in(visual, |workspace, window, cx|workspace.open_abs_path(Path::new("/sibling-app/src/Main.kt"), Default::default(), window, cx)).await?;
        visual.run_until_parked();
        panel.update(visual, select_model)?;
        controller.read_with(visual, |controller, cx| assert!(!controller.action_is_current(&first, cx)));
        panel.read_with(visual, |panel, cx| assert!(panel.accepts_compose_preview(&buffer, cx)));
        project.update(visual, |project, cx|project.invalidate_android_context_for_repository(Path::new("/sibling-app"), cx));
        visual.run_until_parked();
        controller.read_with(visual, |controller, cx| {
            assert_eq!(controller.root(cx), Some(PathBuf::from("/sibling-parent")));
            assert!(controller.discovery_token(cx).is_some());
            assert!(controller.action_token(cx).is_none());
            assert_eq!(controller.capabilities(Default::default(), cx), Default::default());
        });
        panel.read_with(visual, |panel, cx| assert!(!panel.accepts_compose_preview(&buffer, cx)));
        visual.update(|_, cx| publish(cx))?;
        visual.run_until_parked();
        panel.update(visual, select_model)?;
        panel.read_with(visual, |panel, cx| assert!(panel.accepts_compose_preview(&buffer, cx)));
        let trusted_owner = panel.read_with(visual, |panel, cx| panel.operation_owner(AndroidOperation::Preview, cx))?;
        let source_worktree = project.read_with(visual, |project, cx|project.visible_worktrees(cx).find(|worktree|worktree.read(cx).abs_path().as_ref() == Path::new("/sibling-app")).expect("Source worktree").read(cx).id());
        let store = project.read_with(visual, |project, _|project.worktree_store());
        visual.update(|_, cx| TrustedWorktrees::try_get_global(cx).expect("Trust store").update(cx, |trust, cx|trust.restrict(store.downgrade(), [project::trusted_worktrees::PathTrust::Worktree(source_worktree)].into_iter().collect(), cx)));
        assert!(trusted_owner.ensure_active().is_err(), "Trust restriction cancels owned workers before a deferred reconciliation");
        panel.read_with(visual, |panel, cx| assert!(!panel.accepts_compose_preview(&buffer, cx), "Source trust invalidation denies previews immediately"));
        Ok(())
    }

    async fn test_project(cx: &mut TestAppContext) -> (Entity<Project>, Entity<Buffer>) {
        cx.update(|cx| {
            AppState::test(cx);
            editor::init(cx);
            cx.bind_keys([
                gpui::KeyBinding::new("down", menu::SelectNext, Some("menu")),
                gpui::KeyBinding::new("enter", menu::Confirm, Some("menu")),
                gpui::KeyBinding::new("end", menu::SelectLast, Some("menu")),
            ]);
            project::trusted_worktrees::init(Default::default(), cx);
        });
        let filesystem = FakeFs::new(cx.executor());
        filesystem
            .insert_tree(
                "/android",
                serde_json::json!({
                    "settings.gradle.kts": "", "gradlew": "",
                    "Main.kt": "package sample\n\nfun greeting() {}\n",
                    "Other.kt": "package sample\nfun other() {}\n"
                }),
            )
            .await;
        filesystem.insert_tree("/other-android", serde_json::json!({
            "settings.gradle.kts":"", "gradlew":"", "Main.kt":"package other\nfun other() {}\n"
        })).await;
        let project = Project::test(
            filesystem,
            [Path::new("/android"), Path::new("/other-android")],
            cx,
        )
        .await;
        cx.update(|cx| {
            let store = project.read(cx).worktree_store();
            TrustedWorktrees::try_get_global(cx)
                .expect("Trust store")
                .update(cx, |trusted, cx| {
                    trusted.trust(
                        &store,
                        ["/android", "/other-android"]
                            .map(|path| {
                                project::trusted_worktrees::PathTrust::AbsPath(PathBuf::from(path))
                            })
                            .into_iter()
                            .collect(),
                        cx,
                    );
                });
            for root in [Path::new("/android"), Path::new("/other-android")] {
                super::super::tests::publish_test_android_catalogue(&project, root, cx).expect("Explicit inherited preview context");
            }
        });
        let buffer = project
            .update(cx, |project, cx| {
                project.open_buffer(
                    project
                        .find_project_path("/android/Main.kt", cx)
                        .expect("Source path"),
                    cx,
                )
            })
            .await
            .expect("Buffer");
        (project, buffer)
    }

    fn publish_preview_test_model(
        panel: &mut AndroidPanel,
        target: &AndroidTarget,
        cx: &mut Context<AndroidPanel>,
    ) {
        use android_tools::project_model::{SourceKind, SourceRoot, SourceScope, VariantId};
        super::super::tests::publish_test_android_model(panel, target, cx);
        let root = panel.root.clone().expect("Preview root");
        panel.project.update(cx, |project, cx| {
            let mut model = project
                .android_model()
                .model
                .as_deref()
                .expect("Published model")
                .clone();
            let component = model
                .modules
                .iter_mut()
                .flat_map(|module| &mut module.variants)
                .flat_map(|variant| &mut variant.components)
                .find(|component| component.scope == SourceScope::Main)
                .expect("Main component");
            component
                .sources
                .extend(["Main.kt", "Other.kt", "New.kt"].map(|path| SourceRoot {
                    path: root.join(path),
                    kind: SourceKind::Kotlin,
                    generated: false,
                }));
            component.sources.push(SourceRoot {
                path: root.join("app/build/generated"),
                kind: SourceKind::Kotlin,
                generated: true,
            });
            let token = project.invalidate_android_model(Some(root), cx);
            project
                .publish_android_model(&token, model, cx)
                .expect("Publish preview model");
            project
                .select_android_variant(Some(VariantId::from(target)), cx)
                .expect("Select preview variant");
        });
    }

    fn add_preview(
        workspace: &mut Workspace,
        project: Entity<Project>,
        buffer: Entity<Buffer>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> (
        Entity<ComposePreviewView>,
        Entity<AndroidPanel>,
        Entity<Pane>,
    ) {
        let target = AndroidTarget {
            module: ":app".into(),
            variant: "debug".into(),
            output_listing: PathBuf::from("/android/metadata.json"),
        };
        let panel = cx.new(|cx| AndroidPanel::new(workspace.weak_handle(), project.clone(), cx));
        let build_panel = cx.new(|cx| BuildPanel::new(workspace.weak_handle(), cx));
        project_context::register(workspace, build_panel, window, cx);
        panel.update(cx, |panel, cx| {
            panel.root = Some(PathBuf::from("/android"));
            panel.targets = vec![target.clone()];
            panel.selected_target = Some(target.clone());
            publish_preview_test_model(panel, &target, cx);
        });
        workspace.add_panel(panel.clone(), window, cx);
        let pane = workspace.active_pane().clone();
        let editor = pane
            .read(cx)
            .active_item()
            .and_then(|item| item.downcast::<Editor>())
            .filter(|editor| {
                editor.read(cx).buffer().read(cx).as_singleton().as_ref() == Some(&buffer)
            })
            .unwrap_or_else(|| {
                let editor = cx.new(|cx| {
                    Editor::for_buffer(buffer.clone(), Some(project.clone()), window, cx)
                });
                pane.update(cx, |pane, cx| {
                    pane.add_item(Box::new(editor.clone()), true, true, None, window, cx)
                });
                editor
            });
        let view = cx.new(|cx| {
            ComposePreviewView::new(
                panel.downgrade(),
                workspace.weak_handle(),
                project,
                buffer,
                editor.downgrade(),
                pane.downgrade(),
                PathBuf::from("/android"),
                target,
                window,
                cx,
            )
        });
        view.update(cx, |view, _| view.fit_to_window = false);
        editor.update(cx, |editor, cx| {
            editor.register_addon(ComposePreviewAddon { view: view.clone() });
            cx.notify();
        });
        panel.update(cx, |panel, _| {
            panel.preview_view = Some(view.downgrade());
            panel.compose_preview_enabled = true;
        });
        (view, panel, pane)
    }

    #[gpui::test]
    async fn previews_belong_to_source_tabs_and_survive_tab_switches(cx: &mut TestAppContext) {
        let (project, buffer) = test_project(cx).await;
        let other_buffer = project
            .update(cx, |project, cx| {
                project.open_buffer(
                    project
                        .find_project_path("/android/Other.kt", cx)
                        .expect("Other path"),
                    cx,
                )
            })
            .await
            .expect("Other buffer");
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, panel, pane) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project.clone(), buffer, window, cx)
        });
        panel.update_in(cx, |panel, window, cx| {
            panel.observe_compose_preview(window, cx)
        });
        let editor = view.read_with(cx, |view, _| view.editor.upgrade().expect("Owner"));
        view.update(cx, |view, cx| {
            view.gallery = Some(gallery(vec![
                card("sample.Content", "Day", 100, 100),
                card("sample.Content", "Night", 100, 100),
            ]));
            view.selected_card = Some(1);
            view.stale = false;
            view.reflow();
            cx.notify();
        });
        let other = workspace.update_in(cx, |workspace, window, cx| {
            let other = cx.new(|cx| Editor::for_buffer(other_buffer, Some(project), window, cx));
            workspace.active_pane().update(cx, |pane, cx| {
                pane.add_item(Box::new(other.clone()), true, true, None, window, cx)
            });
            other
        });
        cx.run_until_parked();
        let other_view = other.read_with(cx, |editor, _| {
            editor
                .addon::<ComposePreviewAddon>()
                .expect("New tab preview")
                .view
                .clone()
        });
        assert_ne!(view.entity_id(), other_view.entity_id());
        assert_eq!(
            other_view.read_with(cx, |view, _| view.source_path.clone()),
            Path::new("/android/Other.kt")
        );
        assert_eq!(
            view.read_with(cx, |view, _| view.source_path.clone()),
            Path::new("/android/Main.kt")
        );
        pane.update_in(cx, |pane, window, cx| {
            pane.activate_item(
                pane.index_for_item(&editor).expect("Original tab"),
                true,
                true,
                window,
                cx,
            )
        });
        cx.run_until_parked();
        assert_eq!(
            panel.read_with(cx, |panel, _| panel
                .preview_view
                .as_ref()
                .expect("Active preview")
                .entity_id()),
            view.entity_id()
        );
        assert_eq!(view.read_with(cx, |view, _| view.selected_card), Some(1));
        assert!(cx.debug_bounds("compose-image-1").is_some());
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.panes().len(), 1);
            assert_eq!(pane.read(cx).items_len(), 2);
        });
        let weak = view.downgrade();
        pane.update_in(cx, |pane, window, cx| {
            pane.remove_item(editor.entity_id(), false, false, window, cx)
        });
        drop(editor);
        drop(view);
        cx.run_until_parked();
        assert!(
            weak.upgrade().is_none(),
            "Closing the source tab releases its preview"
        );
        assert!(other.read_with(cx, |editor, _| {
            editor.addon::<ComposePreviewAddon>().is_some()
        }));
    }

    #[gpui::test]
    async fn moving_a_source_tab_keeps_its_embedded_preview(cx: &mut TestAppContext) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, panel, pane) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project, buffer, window, cx)
        });
        panel.update_in(cx, |panel, window, cx| {
            panel.observe_compose_preview(window, cx)
        });
        let editor = view.read_with(cx, |view, _| view.editor.upgrade().expect("Owner"));
        view.update(cx, |view, cx| {
            view.navigation_pending = true;
            view.navigation_source = Some(PathBuf::from("/android/Other.kt"));
            view.source_task = Some(cx.spawn(async |_, _| futures::future::pending().await));
        });
        let destination = workspace.update_in(cx, |workspace, window, cx| {
            let destination =
                workspace.split_pane(pane.clone(), workspace::SplitDirection::Right, window, cx);
            workspace::move_item(&pane, &destination, editor.entity_id(), 0, true, window, cx);
            destination
        });
        cx.run_until_parked();
        assert_eq!(
            editor.read_with(cx, |editor, _| editor
                .addon::<ComposePreviewAddon>()
                .expect("Moved tab preview")
                .view
                .entity_id()),
            view.entity_id()
        );
        assert_eq!(
            view.read_with(cx, |view, _| view.source_pane.entity_id()),
            destination.entity_id()
        );
        assert!(view.update(cx, |view, cx| view.visible(cx)));
        view.read_with(cx, |view, _| {
            assert!(view.source_task.is_none());
            assert!(!view.navigation_pending);
            assert!(view.navigation_source.is_none());
        });
    }

    #[gpui::test]
    async fn hiding_a_source_tab_cancels_work_and_releases_decoded_images(cx: &mut TestAppContext) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, _, pane) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project, buffer, window, cx)
        });
        view.update(cx, |view, cx| {
            view.gallery = Some(gallery(vec![card("sample.Content", "Day", 100, 100)]));
            view.selected_card = Some(0);
            view.stale = false;
            view.reflow();
            cx.notify();
        });
        cx.run_until_parked();
        let generation = view.read_with(cx, |view, _| view.gallery_generation);
        view.update(cx, |view, cx| {
            view.building = true;
            view.render_task = Some(cx.spawn(async |_, _| futures::future::pending().await));
        });
        pane.update_in(cx, |pane, window, cx| {
            let item = cx.new(TestItem::new);
            pane.add_item(Box::new(item), true, false, None, window, cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.building && view.render_task.is_none());
            assert!(view.pending && view.stale);
            assert_eq!(view.selected_card, Some(0));
            assert_eq!(view.gallery_generation, generation);
            let gallery = view.gallery.as_ref().expect("Retained gallery");
            assert!(gallery.cards[0].image.is_some());
            assert!(gallery.cards[0].rendered_image.is_none());
        });
        view.update_in(cx, |view, window, cx| {
            view.queue_refresh(true, window, cx);
            assert!(view.pending && view.pending_manual);
            assert!(view.debounce_task.is_none());
        });
    }

    #[gpui::test]
    async fn zooming_another_pane_pauses_hidden_previews(cx: &mut TestAppContext) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, _, pane) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project, buffer, window, cx)
        });
        let other_pane = workspace.update_in(cx, |workspace, window, cx| {
            let pane = workspace.split_pane(pane, workspace::SplitDirection::Right, window, cx);
            pane.update(cx, |pane, cx| {
                let item = cx.new(TestItem::new);
                pane.add_item(Box::new(item), true, true, None, window, cx);
            });
            pane
        });
        cx.run_until_parked();
        assert!(view.update(cx, |view, cx| view.visible(cx)));
        view.update(cx, |view, cx| {
            view.building = true;
            view.render_task = Some(cx.spawn(async |_, _| futures::future::pending().await));
        });
        other_pane.update_in(cx, |pane, window, cx| {
            pane.zoom_in(&workspace::ZoomIn, window, cx)
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            assert!(!view.visible(cx));
            assert!(!view.building && view.render_task.is_none());
            assert!(view.pending);
        });
        other_pane.update_in(cx, |pane, window, cx| {
            pane.zoom_out(&workspace::ZoomOut, window, cx)
        });
        cx.run_until_parked();
        assert!(view.update(cx, |view, cx| view.visible(cx)));
        view.update(cx, |view, cx| {
            view.building = true;
            view.render_task = Some(cx.spawn(async |_, _| futures::future::pending().await));
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_editor_zoom(&workspace::ToggleEditorZoom, window, cx)
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            assert!(!view.visible(cx));
            assert!(!view.building && view.render_task.is_none());
            assert!(view.pending);
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_editor_zoom(&workspace::ToggleEditorZoom, window, cx)
        });
        cx.run_until_parked();
        assert!(view.update(cx, |view, cx| view.visible(cx)));
    }

    #[gpui::test]
    async fn embedded_divider_resizes_and_bottom_controls_share_the_same_center(
        cx: &mut TestAppContext,
    ) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, _, _) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project, buffer, window, cx)
        });
        view.update(cx, |view, cx| {
            view.gallery = Some(gallery(vec![
                card("sample.Content", "Day", 400, 800),
                card("sample.Content", "Night", 400, 800),
            ]));
            view.stale = false;
            view.fit_to_window = true;
            view.reflow();
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("ICON-BoltOutlined").is_none());
        let before = view.read_with(cx, |view, _| view.viewport_width.get());
        let bounds = cx.debug_bounds("compose-editor").expect("Embedded editor");
        let divider = cx.debug_bounds("compose-preview-divider").expect("Divider");
        cx.simulate_mouse_down(divider.center(), MouseButton::Left, Default::default());
        let position = point(bounds.left() + bounds.size.width * 0.3, divider.center().y);
        cx.simulate_mouse_move(position, Some(MouseButton::Left), Default::default());
        cx.run_until_parked();
        cx.simulate_mouse_up(position, MouseButton::Left, Default::default());
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.resizing);
            assert!((view.preview_fraction - 0.7).abs() < 0.01);
            assert!(view.viewport_width.get() > before);
        });
        let hand = cx.debug_bounds("ICON-Hand").expect("Pan");
        for selector in ["ICON-Plus", "ICON-Dash", "ICON-MaximizeAlt"] {
            let control = cx.debug_bounds(selector).expect("Zoom control");
            assert!(
                (f32::from(control.center().x - hand.center().x)).abs() < 0.1,
                "{selector} must share the pan control's center"
            );
            assert_eq!(control.size.width, hand.size.width);
        }
    }

    #[gpui::test]
    async fn resizing_reflows_groups_and_fit_preserves_device_proportions(cx: &mut TestAppContext) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, _, _) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project, buffer, window, cx)
        });
        view.update(cx, |view, cx| {
            view.gallery = Some(gallery(vec![
                card("sample.Content", "Landscape", 400, 200),
                card("sample.Content", "Portrait", 200, 400),
            ]));
            view.stale = false;
            view.reflow();
            cx.notify();
        });
        cx.run_until_parked();
        let landscape = cx
            .debug_bounds("compose-image-0")
            .expect("Landscape preview");
        let portrait = cx
            .debug_bounds("compose-image-1")
            .expect("Portrait preview");
        assert_eq!(landscape.top(), portrait.top());
        assert!(portrait.left() >= landscape.right() + px(CARD_GAP));
        assert_eq!(landscape.size, size(px(200.), px(100.)));
        assert_eq!(portrait.size, size(px(100.), px(200.)));
        cx.simulate_resize(size(px(650.), px(700.)));
        cx.run_until_parked();
        let landscape = cx
            .debug_bounds("compose-image-0")
            .expect("Landscape after resize");
        let portrait = cx
            .debug_bounds("compose-image-1")
            .expect("Portrait after resize");
        assert!(portrait.top() > landscape.bottom());
        view.update(cx, |view, cx| {
            view.fit_to_window = true;
            view.reflow();
            cx.notify();
        });
        cx.run_until_parked();
        let landscape = cx
            .debug_bounds("compose-image-0")
            .expect("Fitted landscape");
        let portrait = cx.debug_bounds("compose-image-1").expect("Fitted portrait");
        assert_eq!(
            landscape.top(),
            portrait.top(),
            "{}",
            view.read_with(cx, |view, _| format!(
                "width={}, zoom={}, rows={:?}",
                view.viewport_width.get(),
                view.zoom,
                view.rows
            ))
        );
        assert!(
            (f32::from(landscape.size.width) / f32::from(portrait.size.width) - 2.).abs() < 0.01
        );
        cx.simulate_resize(size(px(1000.), px(700.)));
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("compose-image-0")
                .expect("Refitted landscape")
                .size
                .width
                > landscape.size.width
        );
    }

    #[gpui::test]
    async fn collapse_and_keyboard_preview_choice_reveal_the_selected_variant(
        cx: &mut TestAppContext,
    ) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, _, _) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project, buffer, window, cx)
        });
        view.update(cx, |view, cx| {
            view.gallery = Some(gallery(vec![
                card("sample.Content", "Day", 100, 100),
                card("sample.Content", "Night", 100, 100),
            ]));
            view.stale = false;
            view.reflow();
            cx.notify();
        });
        cx.run_until_parked();
        let header = cx.debug_bounds("compose-group-0").expect("Group header");
        cx.simulate_click(header.center(), Default::default());
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |view, _| view.rows.clone()),
            [GalleryRow::Header(0)]
        );
        assert!(cx.debug_bounds("compose-image-0").is_none());
        let chooser = cx.debug_bounds("ICON-ListTree").expect("Preview chooser");
        cx.simulate_click(chooser.center(), Default::default());
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
        }
        cx.simulate_keystrokes("end enter");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.collapsed_groups.is_empty());
            assert_eq!(view.selected_card, Some(1));
        });
        assert!(cx.debug_bounds("compose-image-1").is_some());
    }

    #[gpui::test]
    async fn preview_menu_opened_during_refresh_cannot_select_a_replaced_card(
        cx: &mut TestAppContext,
    ) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, _, _) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project, buffer, window, cx)
        });
        view.update(cx, |view, cx| {
            view.gallery = Some(gallery(vec![
                card("sample.Content", "Day", 100, 100),
                card("sample.Content", "Night", 100, 100),
            ]));
            view.building = true;
            view.stale = true;
            view.revision = 10;
            view.reflow();
            cx.notify();
        });
        cx.run_until_parked();
        let chooser = cx.debug_bounds("ICON-ListTree").expect("Preview chooser");
        cx.simulate_click(chooser.center(), Default::default());
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
        }
        view.update_in(cx, |view, window, cx| {
            view.replace_gallery(
                gallery(vec![
                    card("sample.NewContent", "Default", 100, 100),
                    card("sample.Content", "Day", 100, 100),
                    card("sample.Content", "Night", 100, 100),
                ]),
                window,
                cx,
            );
            view.building = false;
            view.stale = false;
            cx.notify();
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("end enter");
        view.read_with(cx, |view, _| {
            assert_eq!(view.revision, 10);
            assert!(
                view.selected_card.is_none(),
                "The old menu must not select a new card by index"
            );
        });
    }

    #[gpui::test]
    async fn card_diagnostics_scroll_without_moving_the_gallery(cx: &mut TestAppContext) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, _, _) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project, buffer, window, cx)
        });
        view.update(cx, |view, cx| {
            let mut failed = card("sample.Failed", "Default", 100, 100);
            failed.image = None;
            failed.result.error = Some("Rendering failed with a long diagnostic. ".repeat(100));
            view.gallery = Some(gallery(vec![
                failed,
                card("sample.Content", "Day", 100, 2000),
            ]));
            view.reflow();
            cx.notify();
        });
        cx.run_until_parked();
        let diagnostic = cx.debug_bounds("compose-card-error-0").expect("Diagnostic");
        let before = view.read_with(cx, |view, _| view.list_state.logical_scroll_top());
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: diagnostic.center(),
            delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-80.))),
            ..Default::default()
        });
        cx.run_until_parked();
        let after = view.read_with(cx, |view, _| view.list_state.logical_scroll_top());
        assert_eq!(before.item_ix, after.item_ix);
        assert_eq!(before.offset_in_item, after.offset_in_item);
    }

    #[gpui::test]
    async fn source_navigation_diagnostic_is_available_from_the_keyboard_menu(
        cx: &mut TestAppContext,
    ) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, _, _) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project, buffer, window, cx)
        });
        let diagnostic = "Compose source information is unavailable for this dependency";
        view.update(cx, |view, cx| {
            let mut preview = card("sample.Content", "Default", 100, 100);
            preview.result.inspection_error = Some(diagnostic.into());
            view.gallery = Some(gallery(vec![preview]));
            view.reflow();
            view.stale = false;
            cx.notify();
        });
        cx.run_until_parked();
        let options = cx
            .debug_bounds("ICON-EllipsisVertical")
            .expect("Preview options");
        cx.simulate_click(options.center(), Default::default());
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
        }
        cx.simulate_keystrokes("end enter");
        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some(diagnostic)
        );
    }

    #[gpui::test]
    async fn pan_drag_scrolls_without_inspecting_or_navigating(cx: &mut TestAppContext) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, _, _) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project, buffer, window, cx)
        });
        view.update(cx, |view, cx| {
            view.gallery = Some(gallery(
                (0..10)
                    .map(|index| card("sample.Content", &format!("Phone {index}"), 1600, 600))
                    .collect(),
            ));
            view.pan_mode = true;
            view.set_zoom(1., cx);
            view.stale = false;
        });
        cx.run_until_parked();
        let start =
            cx.debug_bounds("compose-image-0").expect("Preview").origin + point(px(150.), px(150.));
        cx.simulate_event(gpui::MouseDownEvent {
            button: MouseButton::Left,
            position: start,
            ..Default::default()
        });
        cx.simulate_event(gpui::MouseMoveEvent {
            position: start - point(px(60.), px(70.)),
            pressed_button: Some(MouseButton::Left),
            ..Default::default()
        });
        cx.run_until_parked();
        let before = view.read_with(cx, |view, _| view.horizontal_scroll.offset());
        let outside = point(px(20.), start.y - px(100.));
        cx.simulate_event(gpui::MouseMoveEvent {
            position: outside,
            pressed_button: Some(MouseButton::Left),
            ..Default::default()
        });
        cx.run_until_parked();
        assert!(
            view.read_with(cx, |view, _| view.horizontal_scroll.offset().x) < before.x,
            "{}",
            view.read_with(cx, |view, _| format!(
                "before={before:?}, after={:?}, pan={:?}, start={start:?}, outside={outside:?}",
                view.horizontal_scroll.offset(),
                view.pan_position
            ))
        );
        assert_eq!(
            view.read_with(cx, |view, _| view.pan_position),
            Some(outside)
        );
        cx.simulate_event(gpui::MouseUpEvent {
            button: MouseButton::Left,
            position: outside,
            ..Default::default()
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.horizontal_scroll.offset().x < px(0.));
            assert!(view.list_state.scroll_px_offset_for_scrollbar().y < px(0.));
            assert!(!view.inspect);
            assert!(view.selected_card.is_none());
            assert!(view.pan_position.is_none());
        });
    }

    #[gpui::test]
    async fn gallery_virtualizes_wrapped_rows_and_preserves_the_group_when_zooming(
        cx: &mut TestAppContext,
    ) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, _, _) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project, buffer, window, cx)
        });
        view.update(cx, |view, cx| {
            view.gallery = Some(gallery(
                (0..300)
                    .map(|index| {
                        card(
                            &format!("sample.Content{}", index / 3),
                            &format!("Phone {index}"),
                            100,
                            200,
                        )
                    })
                    .collect(),
            ));
            view.stale = false;
            view.reflow();
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let count = view
                .gallery
                .as_ref()
                .expect("Gallery")
                .cards
                .iter()
                .filter(|card| card.rendered_image.is_some())
                .count();
            assert!(
                count < 50,
                "Only visible rows should decode images, got {count}"
            );
        });
        view.update(cx, |view, cx| view.reveal_card(150, cx));
        cx.run_until_parked();
        view.update(cx, |view, cx| view.set_zoom(2., cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let top = view
                .rows
                .get(view.list_state.logical_scroll_top().item_ix)
                .expect("Visible row");
            assert!(
                (49..=50).contains(&top.group()),
                "Zoom should keep the selected function in view: {top:?}"
            );
            assert_eq!(view.selected_card, Some(150));
        });
        assert!(cx.debug_bounds("compose-image-150").is_some());
    }

    #[gpui::test]
    async fn rebuilding_preserves_selection_collapsed_groups_and_scroll_by_preview_identity(
        cx: &mut TestAppContext,
    ) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, _, _) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project, buffer, window, cx)
        });
        let cards = || {
            (0..90)
                .map(|index| {
                    card(
                        &format!("sample.Content{}", index / 3),
                        &format!("Phone {index}"),
                        100,
                        200,
                    )
                })
                .collect::<Vec<_>>()
        };
        view.update(cx, |view, cx| {
            view.gallery = Some(gallery(cards()));
            view.collapsed_groups.insert("sample.Content0".into());
            view.reflow();
            view.reveal_card(45, cx);
            view.stale = false;
        });
        cx.run_until_parked();
        view.update_in(cx, |view, window, cx| {
            let mut updated = vec![card("sample.NewPreview", "Default", 200, 200)];
            updated.extend(cards());
            view.replace_gallery(gallery(updated), window, cx);
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.selected_card, Some(46));
            assert!(view.collapsed_groups.contains("sample.Content0"));
            assert_eq!(
                view.rows
                    .get(view.list_state.logical_scroll_top().item_ix)
                    .expect("Visible row")
                    .group(),
                16
            );
        });
        assert!(cx.debug_bounds("compose-image-46").is_some());
    }

    #[gpui::test]
    async fn hiding_preview_preserves_other_tabs_and_unsaved_edits(cx: &mut TestAppContext) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (source, other, pane) = workspace.update_in(cx, |workspace, window, cx| {
            let source = cx.new(|cx| TestItem::new(cx).with_label("Main.kt").with_dirty(true));
            workspace.active_pane().update(cx, |pane, cx| {
                pane.add_item(Box::new(source.clone()), true, true, None, window, cx)
            });
            let (_, _, pane) = add_preview(workspace, project, buffer, window, cx);
            let other = cx.new(|cx| TestItem::new(cx).with_label("Other.kt"));
            pane.update(cx, |pane, cx| {
                pane.add_item_inner(
                    Box::new(other.clone()),
                    false,
                    false,
                    false,
                    None,
                    window,
                    cx,
                )
            });
            (source, other, pane)
        });
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            toggle_preview(workspace, window, cx)
        });
        cx.run_until_parked();
        workspace.read_with(cx, |workspace, cx| {
            assert!(workspace.pane_for(&source).is_some());
            assert!(workspace.pane_for(&other).is_some());
            assert!(source.read(cx).is_dirty);
            assert_eq!(source.read(cx).save_count, 0);
            assert_eq!(pane.read(cx).items_len(), 3);
            assert_eq!(workspace.panes().len(), 1);
            let editor = pane
                .read(cx)
                .active_item()
                .expect("Source tab")
                .downcast::<Editor>()
                .expect("Source editor");
            assert!(editor.read(cx).addon::<ComposePreviewAddon>().is_none());
        });
    }

    #[gpui::test]
    async fn showing_existing_preview_keeps_the_source_tab_and_pane(cx: &mut TestAppContext) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, panel, pane) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project, buffer, window, cx)
        });
        pane.update_in(cx, |pane, window, cx| {
            let other = cx.new(TestItem::new);
            pane.add_item_inner(Box::new(other), false, false, false, None, window, cx);
        });
        cx.run_until_parked();
        panel.update_in(cx, |panel, window, cx| {
            panel.show_compose_preview(window, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            pane.read_with(cx, |pane, _| pane
                .active_item()
                .expect("Active preview")
                .item_id()),
            view.read_with(cx, |view, _| view.editor.entity_id())
        );
        assert_eq!(pane.read_with(cx, |pane, _| pane.items_len()), 2);
        workspace.read_with(cx, |workspace, _| assert_eq!(workspace.panes().len(), 1));
    }

    #[gpui::test]
    async fn refresh_requests_coalesce_and_preserve_manual_refresh(cx: &mut TestAppContext) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let view = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project, buffer, window, cx).0
        });
        view.update_in(cx, |view, window, cx| {
            view.building = true;
            for _ in 0..10 {
                view.queue_refresh(false, window, cx);
            }
            view.queue_refresh(true, window, cx);
            view.queue_refresh(false, window, cx);
            assert!(view.debounce_task.is_some());
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.building && view.pending && view.pending_manual && view.stale);
            assert!(view.render_task.is_none());
            assert!(view.debounce_task.is_none());
            assert_eq!(view.revision, 12);
        });
        view.update(cx, |view, cx| view.stop(cx));
        view.read_with(cx, |view, _| {
            assert!(!view.building && !view.pending && !view.pending_manual)
        });
    }

    #[gpui::test]
    async fn changing_variant_cancels_old_render_and_invalidates_source_navigation(
        cx: &mut TestAppContext,
    ) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, panel, _) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project, buffer, window, cx)
        });
        view.update(cx, |view, cx| {
            view.building = true;
            view.stale = false;
            view.render_task = Some(cx.spawn(async |_, _| futures::future::pending().await));
            view.source_task = Some(cx.spawn(async |_, _| futures::future::pending().await));
            view.navigation_source = Some(PathBuf::from("/android/Other.kt"));
            view.navigation_pending = true;
        });
        panel.update(cx, |panel, cx| {
            panel.selected_target.as_mut().expect("Target").variant = "release".into();
            let target = panel.selected_target.clone().expect("Target");
            panel.targets = vec![target.clone()];
            publish_preview_test_model(panel, &target, cx);
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.target.variant, "release");
            assert!(view.render_task.is_none() && !view.building);
            assert!(
                view.source_task.is_none()
                    && view.navigation_source.is_none()
                    && !view.navigation_pending
            );
            assert!(view.stale && view.pending);
        });
    }

    #[gpui::test]
    async fn returning_to_the_same_variant_invalidates_the_previous_model_gallery(
        cx: &mut TestAppContext,
    ) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, panel, _) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project.clone(), buffer, window, cx)
        });
        cx.run_until_parked();
        let original_target = panel.read_with(cx, |panel, _| {
            panel.selected_target.clone().expect("Original target")
        });
        let original_token = project.read_with(cx, |project, _| project.android_model().token());
        let original_revision = view.update(cx, |view, cx| {
            view.gallery = Some(gallery(vec![card("sample.Content", "Day", 100, 100)]));
            view.selected_card = Some(0);
            view.stale = false;
            view.building = true;
            view.render_task = Some(cx.spawn(async |_, _| futures::future::pending().await));
            view.source_task = Some(cx.spawn(async |_, _| futures::future::pending().await));
            view.navigation_source = Some(PathBuf::from("/android/Other.kt"));
            view.navigation_pending = true;
            view.revision
        });
        // Coalesce both selections so the view only observes the restored identity.
        panel.update(cx, |panel, cx| {
            let mut intermediate = original_target.clone();
            intermediate.variant = "release".into();
            panel.targets = vec![intermediate.clone()];
            panel.selected_target = Some(intermediate.clone());
            publish_preview_test_model(panel, &intermediate, cx);
            panel.targets = vec![original_target.clone()];
            panel.selected_target = Some(original_target.clone());
            publish_preview_test_model(panel, &original_target, cx);
            cx.notify();
        });
        cx.run_until_parked();
        project.read_with(cx, |project, _| {
            assert!(!project.android_model().is_current(&original_token));
        });
        view.read_with(cx, |view, cx| {
            assert_eq!(view.root, Path::new("/android"));
            assert_eq!(view.target, original_target);
            assert!(
                view.project
                    .read(cx)
                    .android_model()
                    .is_current(&view.model_token)
            );
            assert!(view.revision > original_revision);
            assert!(view.gallery.is_none() && view.selected_card.is_none());
            assert!(view.render_task.is_none() && !view.building);
            assert!(view.source_task.is_none() && view.navigation_source.is_none());
            assert!(!view.navigation_pending);
            assert!(view.stale && view.pending);
        });
    }

    #[test]
    fn generated_outputs_do_not_schedule_preview_refreshes() {
        for path in [
            "app/build/tmp/kotlin/Generated.kt",
            ".koda/android-preview/render-1/image.png",
            ".gradle/cache.kt",
        ] {
            assert!(!preview_input(RelPath::from_unix_str(path).expect("Path")));
        }
        for path in [
            "app/src/main/java/Main.kt",
            "app/src/main/res/values/strings.xml",
            "app/build.gradle.kts",
        ] {
            assert!(preview_input(RelPath::from_unix_str(path).expect("Path")));
        }
    }

    #[gpui::test]
    async fn selected_source_scopes_control_automatic_preview_refresh(cx: &mut TestAppContext) {
        use android_tools::project_model::{
            Component, SourceKind, SourceRoot, SourceScope, VariantId,
        };
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, panel, _) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project.clone(), buffer, window, cx)
        });
        let target = panel.read_with(cx, |panel, _| {
            panel.selected_target.clone().expect("Target")
        });
        project.update(cx, |project, cx| {
            let mut model = project
                .android_model()
                .model
                .as_deref()
                .expect("Model")
                .clone();
            // A selected build may be canonicalized while its worktree retains a symlink path.
            let canonical_root = PathBuf::from("/canonical-android");
            let original_root = model.root.clone();
            model.root = canonical_root.clone();
            for module in &mut model.modules {
                module.directory = canonical_root.join(
                    module
                        .directory
                        .strip_prefix(&original_root)
                        .expect("Module path"),
                );
                for variant in &mut module.variants {
                    for component in &mut variant.components {
                        for source in &mut component.sources {
                            source.path = canonical_root.join(
                                source
                                    .path
                                    .strip_prefix(&original_root)
                                    .expect("Source path"),
                            );
                        }
                    }
                    let main = variant
                        .components
                        .iter_mut()
                        .find(|component| component.scope == SourceScope::Main)
                        .expect("Main component");
                    main.sources.extend([
                        SourceRoot {
                            path: canonical_root.join("code"),
                            kind: SourceKind::Kotlin,
                            generated: false,
                        },
                        SourceRoot {
                            path: canonical_root.join("code/custom-output"),
                            kind: SourceKind::Kotlin,
                            generated: true,
                        },
                        SourceRoot {
                            path: canonical_root.join("custom-generated"),
                            kind: SourceKind::Kotlin,
                            generated: true,
                        },
                    ]);
                    variant.components.push(Component {
                        name: "debugUnitTest".into(),
                        namespace: None,
                        scope: SourceScope::UnitTest,
                        dependencies: Vec::new(),
                        sources: vec![SourceRoot {
                            path: canonical_root.join("code/tests"),
                            kind: SourceKind::Kotlin,
                            generated: false,
                        }],
                    });
                }
            }
            let token = project.invalidate_android_model(Some(canonical_root), cx);
            project
                .publish_android_model(&token, model, cx)
                .expect("Publish canonical model");
            project
                .select_android_variant(Some(VariantId::from(&target)), cx)
                .expect("Select variant");
        });
        view.read_with(cx, |view, cx| {
            for path in ["Main.kt", "code/Content.kt", "app/build.gradle.kts"] {
                assert!(
                    view.preview_input(RelPath::from_unix_str(path).expect("Path"), cx),
                    "{path}"
                );
            }
            for path in [
                "code/custom-output/Generated.kt",
                "custom-generated/Generated.kt",
                "code/tests/Test.kt",
            ] {
                assert!(
                    !view.preview_input(RelPath::from_unix_str(path).expect("Path"), cx),
                    "{path}"
                );
            }
        });
    }

    #[gpui::test]
    async fn inactive_variant_source_is_rejected_before_starting_preview_tools(
        cx: &mut TestAppContext,
    ) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, _, _) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project, buffer, window, cx)
        });
        view.update_in(cx, |view, window, cx| {
            view.source_path = PathBuf::from("/android/src/inactive/kotlin/Content.kt");
            view.refresh(window, cx);
            assert!(!view.building && view.render_task.is_none());
            assert!(!view.pending && !view.pending_manual);
            assert!(
                view.error
                    .as_ref()
                    .is_some_and(|error| error.contains("selected Android variant's main sources"))
            );
        });
    }

    #[gpui::test]
    async fn invalidation_while_loading_navigation_keeps_the_original_source_tab(
        cx: &mut TestAppContext,
    ) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, _, pane) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project.clone(), buffer, window, cx)
        });
        cx.run_until_parked();
        let editor = view.read_with(cx, |view, _| view.editor.entity_id());
        let loaded = Rc::new(Cell::new(false));
        let _subscription = project.update(cx, |project, cx| {
            let store = project.buffer_store().clone();
            let loaded = loaded.clone();
            cx.subscribe(&store, move |project, _, event, cx| {
                if let project::buffer_store::BufferStoreEvent::BufferAdded(buffer) = event
                    && buffer_path(buffer, cx).as_deref() == Some(Path::new("/android/Other.kt"))
                {
                    loaded.set(true);
                    project.invalidate_android_model(Some(PathBuf::from("/android")), cx);
                }
            })
        });
        view.update_in(cx, |view, window, cx| {
            let mut preview = card("sample.Content", "Day", 100, 100);
            preview.nodes = Arc::new(vec![preview::ComposeNode {
                name: "other".into(),
                file_name: "Other.kt".into(),
                line_number: 2,
                package_hash: preview::package_hash("sample"),
                bounds: [0, 0, 50, 50],
                depth: 0,
                source_path: Some(PathBuf::from("/android/Other.kt")),
            }]);
            view.gallery = Some(gallery(vec![preview]));
            view.stale = false;
            view.navigate(0, 0, view.revision, view.gallery_generation, window, cx);
            assert!(view.navigation_pending && view.source_task.is_some());
        });
        cx.run_until_parked();
        assert!(
            loaded.get(),
            "Invalidate after loading the destination buffer"
        );
        pane.read_with(cx, |pane, _| {
            assert_eq!(pane.items_len(), 1);
            assert_eq!(
                pane.active_item().expect("Original source").item_id(),
                editor
            );
        });
        view.read_with(cx, |view, _| {
            assert!(!view.navigation_pending);
            assert!(view.configuration_suspended && view.gallery.is_none());
        });
    }

    #[gpui::test]
    async fn automatic_refresh_tracks_preview_tab_visibility(cx: &mut TestAppContext) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, _, pane) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project, buffer, window, cx)
        });
        cx.run_until_parked();
        view.update_in(cx, |view, window, cx| {
            assert!(view.visible(cx));
            view.queue_refresh(false, window, cx);
            assert!(view.debounce_task.is_some());
        });
        pane.update_in(cx, |pane, window, cx| {
            let item = cx.new(TestItem::new);
            pane.add_item(Box::new(item), true, false, None, window, cx);
        });
        view.update_in(cx, |view, window, cx| {
            assert!(!view.visible(cx));
            view.queue_refresh(false, window, cx);
            assert!(view.pending && view.debounce_task.is_none());
        });
    }

    #[gpui::test]
    async fn changing_project_creates_an_editor_owned_preview_without_rebinding_other_tabs(
        cx: &mut TestAppContext,
    ) {
        let (project, buffer) = test_project(cx).await;
        let other = project
            .update(cx, |project, cx| {
                project.open_buffer(
                    project
                        .find_project_path("/other-android/Main.kt", cx)
                        .expect("Other root source"),
                    cx,
                )
            })
            .await
            .expect("Other buffer");
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, panel, _) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project.clone(), buffer, window, cx)
        });
        panel.update_in(cx, |panel, window, cx| {
            panel.observe_compose_preview(window, cx)
        });
        workspace.update_in(cx, |workspace, window, cx| {
            let editor = cx.new(|cx| Editor::for_buffer(other, Some(project), window, cx));
            workspace.active_pane().update(cx, |pane, cx| {
                pane.add_item(Box::new(editor), true, true, None, window, cx)
            });
        });
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |view, _| view.source_path.clone()),
            Path::new("/android/Main.kt")
        );
        panel.update(cx, |panel, cx| {
            panel.root = Some(PathBuf::from("/other-android"));
            panel
                .selected_target
                .as_mut()
                .expect("Target")
                .output_listing = PathBuf::from("/other-android/metadata.json");
            let target = panel.selected_target.clone().expect("Other root target");
            panel.targets = vec![target.clone()];
            publish_preview_test_model(panel, &target, cx);
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.source_path, Path::new("/android/Main.kt"));
            assert!(view.configuration_suspended);
        });
        let other_view = panel.read_with(cx, |panel, _| {
            panel
                .preview_view
                .as_ref()
                .expect("Other preview")
                .upgrade()
                .expect("Live preview")
        });
        assert_ne!(view.entity_id(), other_view.entity_id());
        assert_eq!(
            other_view.read_with(cx, |view, _| view.source_path.clone()),
            Path::new("/other-android/Main.kt")
        );
    }

    #[gpui::test]
    async fn configuration_recovery_resumes_without_restarting_explicit_stop(
        cx: &mut TestAppContext,
    ) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, panel, _) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project, buffer, window, cx)
        });
        let target = panel.update(cx, |panel, cx| {
            let target = panel.selected_target.take();
            panel.invalidate_model(panel.root.clone(), cx);
            cx.notify();
            target
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.configuration_suspended && !view.pending && view.error.is_some())
        });
        panel.update(cx, |panel, cx| {
            panel.selected_target = target;
            let target = panel.selected_target.clone().expect("Recovered target");
            publish_preview_test_model(panel, &target, cx);
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.configuration_suspended && view.pending && view.error.is_none())
        });
        view.update(cx, |view, cx| view.stop(cx));
        panel.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert!(!view.read_with(cx, |view, _| view.pending));
    }

    #[gpui::test]
    async fn saving_an_untitled_kotlin_buffer_subscribes_to_future_edits(cx: &mut TestAppContext) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, _, _) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project.clone(), buffer, window, cx)
        });
        let untitled = project
            .update(cx, |project, cx| project.create_buffer(None, true, cx))
            .await
            .expect("Untitled buffer");
        cx.run_until_parked();
        project
            .update(cx, |project, cx| {
                project.save_buffer_as(
                    untitled.clone(),
                    project
                        .find_project_path("/android/New.kt", cx)
                        .expect("New path"),
                    cx,
                )
            })
            .await
            .expect("Save as Kotlin");
        cx.run_until_parked();
        let revision = view.read_with(cx, |view, _| view.revision);
        untitled.update(cx, |buffer, cx| {
            buffer.edit([(0..0, "package sample\n")], None, cx)
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.revision) > revision);
    }

    #[gpui::test]
    async fn generated_buffers_do_not_trigger_automatic_builds(cx: &mut TestAppContext) {
        let (project, buffer) = test_project(cx).await;
        project
            .read_with(cx, |project, _| project.fs().clone())
            .create_dir(Path::new("/android/app/build/generated"))
            .await
            .expect("Directory");
        let filesystem = project.read_with(cx, |project, _| project.fs().clone());
        filesystem
            .atomic_write(
                PathBuf::from("/android/app/build/generated/Generated.kt"),
                "package sample\n".into(),
            )
            .await
            .expect("Generated source");
        let generated = project
            .update(cx, |project, cx| {
                project.open_buffer(
                    project
                        .find_project_path("/android/app/build/generated/Generated.kt", cx)
                        .expect("Path"),
                    cx,
                )
            })
            .await
            .expect("Generated buffer");
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, _, _) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project, buffer, window, cx)
        });
        cx.run_until_parked();
        let revision = view.read_with(cx, |view, _| view.revision);
        generated.update(cx, |buffer, cx| {
            buffer.edit([(0..0, "// generated\n")], None, cx)
        });
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |view, _| view.revision), revision);
    }

    #[gpui::test]
    async fn inspecting_scaled_preview_navigates_to_the_composable(cx: &mut TestAppContext) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, panel, _) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project.clone(), buffer, window, cx)
        });
        panel.update_in(cx, |panel, window, cx| {
            panel.observe_compose_preview(window, cx)
        });
        view.update(cx, |view, cx| {
            let mut bytes = b"P6\n100 80\n255\n".to_vec();
            bytes.extend(std::iter::repeat_n(255, 100 * 80 * 3));
            let node = preview::ComposeNode {
                name: "greeting".into(),
                file_name: "Other.kt".into(),
                line_number: 2,
                package_hash: preview::package_hash("sample"),
                bounds: [10, 10, 40, 40],
                depth: 2,
                source_path: Some(PathBuf::from("/android/Other.kt")),
            };
            view.gallery = Some(Gallery::new(
                vec![
                    card("sample.Greeting", "Night", 100, 80),
                    PreviewCard {
                        label: "Greeting".into(),
                        method: "sample.Greeting".into(),
                        variant: "Default".into(),
                        result: preview::RenderedPreview {
                            id: "greeting".into(),
                            parameter_index: 0,
                            image: None,
                            width: 100,
                            height: 80,
                            nodes: Vec::new(),
                            error: None,
                            inspection_error: None,
                        },
                        image: Some(Arc::new(Image::from_bytes(ImageFormat::Pnm, bytes))),
                        rendered_image: None,
                        outlines: Arc::new(vec![node.bounds]),
                        nodes: Arc::new(vec![node]),
                    },
                ],
                tempfile::tempdir().expect("Render directory"),
            ));
            view.reflow();
            view.stale = false;
            cx.notify();
        });
        cx.run_until_parked();
        let bounds = cx.debug_bounds("compose-image-1").expect("Rendered image");
        assert_eq!(bounds.size, size(px(50.), px(40.)));
        let position = bounds.origin + point(px(12.5), px(12.5));
        cx.simulate_click(position, Default::default());
        assert!(view.read_with(cx, |view, _| view.inspect));
        assert_eq!(view.read_with(cx, |view, _| view.hovered), Some((1, 0)));
        let header = cx.debug_bounds("compose-group-0").expect("Group header");
        cx.simulate_mouse_move(header.center(), None, Default::default());
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.hovered.is_none()));
        cx.simulate_click(position, Default::default());
        cx.run_until_parked();
        let editor = workspace
            .read_with(cx, |workspace, cx| workspace.active_item_as::<Editor>(cx))
            .expect("Source editor");
        editor.update_in(cx, |editor, window, cx| {
            let buffer = editor.snapshot(window, cx).display_snapshot;
            assert_eq!(
                editor.selections.newest::<language::Point>(&buffer).head(),
                language::Point::new(1, 0)
            );
        });
        assert!(view.read_with(cx, |view, _| view.error.is_none()));
        let helper = project.read_with(cx, |project, cx| {
            project
                .opened_buffers(cx)
                .into_iter()
                .find(|buffer| {
                    buffer_path(buffer, cx).as_deref() == Some(Path::new("/android/Other.kt"))
                })
                .expect("Helper buffer")
        });
        helper.update(cx, |buffer, cx| {
            buffer.edit([(0..0, "// helper edit\n")], None, cx)
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.source_path, Path::new("/android/Main.kt"));
            assert!(view.stale && view.pending);
        });
        panel.update(cx, |panel, cx| {
            panel.selected_target.as_mut().expect("Target").variant = "release".into();
            let target = panel.selected_target.clone().expect("Target");
            publish_preview_test_model(panel, &target, cx);
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.target.variant, "release");
            assert_eq!(view.source_path, Path::new("/android/Main.kt"));
        });
    }
}
