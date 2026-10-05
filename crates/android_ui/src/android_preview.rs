use super::*;
use android_tools::preview;
use editor::Editor;
use gpui::{
    Bounds, Image, ImageFormat, ListState, MouseButton, Pixels, canvas, img, list, point, size,
};
use language::Buffer;
use std::{cell::Cell, rc::Rc};
use ui::WithScrollbar;
use workspace::{
    Pane, SaveIntent, SplitDirection,
    item::{Item, ItemEvent},
};

const REFRESH_DELAY: Duration = Duration::from_millis(700);

pub(super) fn toggle_preview(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let view = workspace
        .panel::<AndroidPanel>(cx)
        .and_then(|panel| panel.read(cx).preview_view.as_ref()?.upgrade());
    if let Some(view) = view
        && let Some(pane) = workspace.pane_for(&view)
    {
        let id = view.entity_id();
        pane.update(cx, |pane, cx| {
            pane.close_items(window, cx, SaveIntent::Skip, &move |item| item == id)
                .detach_and_log_err(cx);
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
                        let Some(view) = panel.preview_view.as_ref().and_then(WeakEntity::upgrade)
                        else {
                            return;
                        };
                        let source = panel.active_preview_source(cx);
                        view.update(cx, |view, cx| {
                            if let Some((buffer, pane)) = source {
                                view.bind_source(buffer, pane, window, cx);
                            }
                            if view.pending && view.visible(cx) {
                                view.queue_refresh(false, window, cx);
                            }
                        });
                    });
                }
            },
        ));
    }

    fn active_preview_source(&self, cx: &App) -> Option<(Entity<Buffer>, WeakEntity<Pane>)> {
        let workspace = self.workspace.upgrade()?;
        let workspace = workspace.read(cx);
        let editor = workspace.active_item(cx)?.downcast::<Editor>()?;
        let buffer = editor.read(cx).buffer().read(cx).as_singleton()?;
        let file = buffer.read(cx).file()?;
        if file.path().extension() != Some("kt") {
            return None;
        }
        Some((buffer, workspace.pane_for(&editor)?.downgrade()))
    }

    pub(super) fn show_compose_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = (|| {
            let root = self.trusted_root(cx)?;
            let target = self
                .selected_target
                .clone()
                .context("Sync the Android project and select a build variant first.")?;
            let source = self.active_preview_source(cx);
            if let Some(view) = self.preview_view.as_ref().and_then(WeakEntity::upgrade)
                && let Some(pane) = self
                    .workspace
                    .read_with(cx, |workspace, _| workspace.pane_for(&view))?
            {
                pane.update(cx, |pane, cx| {
                    if let Some(index) = pane.index_for_item(&view) {
                        pane.activate_item(index, false, false, window, cx);
                    }
                });
                view.update(cx, |view, cx| {
                    view.configure(root, target, window, cx);
                    if let Some((buffer, pane)) = source {
                        view.bind_source(buffer, pane, window, cx);
                    }
                    view.queue_refresh(true, window, cx);
                });
                return Ok(());
            }
            let (buffer, source_pane) =
                source.context("Open a Kotlin source file to preview its composables.")?;
            let panel = cx.weak_entity();
            let view = cx.new(|cx| {
                ComposePreviewView::new(
                    panel,
                    self.workspace.clone(),
                    self.project.clone(),
                    buffer,
                    source_pane.clone(),
                    root,
                    target,
                    window,
                    cx,
                )
            });
            self.preview_view = Some(view.downgrade());
            self.workspace.update(cx, |workspace, cx| {
                let source_pane = source_pane
                    .upgrade()
                    .unwrap_or_else(|| workspace.active_pane().clone());
                let pane = workspace.split_pane(source_pane, SplitDirection::Right, window, cx);
                pane.update(cx, |pane, cx| {
                    pane.add_item(Box::new(view.clone()), false, false, None, window, cx)
                });
            })?;
            view.update(cx, |view, cx| view.queue_refresh(true, window, cx));
            Ok::<_, anyhow::Error>(())
        })();
        if let Err(error) = result {
            self.fail(error, window, cx);
        }
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

struct PreviewCard {
    label: String,
    result: preview::RenderedPreview,
    image: Option<Arc<Image>>,
    rendered_image: Option<Arc<gpui::RenderImage>>,
    nodes: Arc<Vec<preview::ComposeNode>>,
    outlines: Arc<Vec<[i32; 4]>>,
}

struct Gallery {
    cards: Vec<PreviewCard>,
    _directory: tempfile::TempDir,
}

pub(super) struct ComposePreviewView {
    panel: WeakEntity<AndroidPanel>,
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    source: Entity<Buffer>,
    source_pane: WeakEntity<Pane>,
    source_path: PathBuf,
    root: PathBuf,
    target: AndroidTarget,
    focus_handle: FocusHandle,
    gallery: Option<Gallery>,
    list_state: ListState,
    revision: u64,
    pending: bool,
    pending_manual: bool,
    stale: bool,
    building: bool,
    configuration_suspended: bool,
    auto_refresh: bool,
    inspect: bool,
    hovered: Option<(usize, usize)>,
    zoom: f32,
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
        source_pane: WeakEntity<Pane>,
        root: PathBuf,
        target: AndroidTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let source_path = buffer_path(&source, cx).unwrap_or_default();
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
                                    *change != project::PathChange::Loaded && preview_input(path)
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
            source_pane,
            source_path,
            root,
            target,
            focus_handle: cx.focus_handle(),
            gallery: None,
            list_state: ListState::new(0, gpui::ListAlignment::Top, px(300.)),
            revision: 0,
            pending: false,
            pending_manual: false,
            stale: true,
            building: false,
            configuration_suspended: false,
            auto_refresh: true,
            inspect: false,
            hovered: None,
            zoom: 0.5,
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
        if !path.starts_with(&self.root)
            || !path
                .extension()
                .is_some_and(|extension| matches!(extension.to_str(), Some("kt" | "java" | "xml")))
        {
            self.buffer_subscriptions.remove(&id);
            return;
        }
        let Some(relative) = path
            .strip_prefix(&self.root)
            .ok()
            .and_then(|path| RelPath::new(path, util::paths::PathStyle::local()).ok())
        else {
            return;
        };
        if !preview_input(&relative) {
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
                        if path.starts_with(&view.root) {
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
                    panel.trusted_root(cx)?,
                    panel
                        .selected_target
                        .clone()
                        .context("Select an Android build variant")?,
                    panel.active_preview_source(cx),
                ))
            })
            .and_then(|result| result);
        match configuration {
            Ok((root, target, source)) => {
                let recovering = self.configuration_suspended;
                self.configuration_suspended = false;
                let inspected_source = (root == self.root && !self.navigation_pending)
                    .then(|| self.navigation_source.clone())
                    .flatten();
                self.configure(root, target, window, cx);
                self.navigation_source = inspected_source;
                if let Some((buffer, pane)) = source {
                    self.bind_source(buffer, pane, window, cx);
                }
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
        if root != self.root || target != self.target {
            self.root = root;
            self.target = target;
            self.observe_buffers(window, cx);
            self.invalidate(window, cx);
        }
    }

    fn bind_source(
        &mut self,
        source: Entity<Buffer>,
        pane: WeakEntity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = buffer_path(&source, cx) else {
            return;
        };
        if !path.starts_with(&self.root) {
            return;
        }
        self.observe_buffer(source.clone(), window, cx);
        self.source_pane = pane;
        if self.navigation_source.as_ref() == Some(&path) {
            return;
        }
        self.navigation_source = None;
        if source != self.source || path != self.source_path {
            self.source = source;
            self.source_path = path;
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
        self.list_state.reset(0);
        self.queue_refresh(false, window, cx);
        cx.emit(ItemEvent::UpdateTab);
    }

    fn visible(&self, cx: &Context<Self>) -> bool {
        let id = cx.entity_id();
        self.workspace
            .read_with(cx, |workspace, cx| {
                workspace.pane_for_item_id(id).is_some_and(|pane| {
                    pane.read(cx)
                        .active_item()
                        .is_some_and(|item| item.item_id() == id)
                })
            })
            .unwrap_or(false)
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
        if !manual && (!self.auto_refresh || !self.visible(cx)) {
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
                if view.pending
                    && !view.building
                    && (view.pending_manual || view.auto_refresh && view.visible(cx))
                {
                    view.refresh(window, cx);
                }
            })
            .log_err();
        }));
        cx.notify();
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = (|| {
            let (root, target) = self.panel.read_with(cx, |panel, cx| {
                Ok::<_, anyhow::Error>((
                    panel.trusted_root(cx)?,
                    panel
                        .selected_target
                        .clone()
                        .context("Select an Android build variant")?,
                ))
            })??;
            self.configure(root.clone(), target.clone(), window, cx);
            ensure!(
                self.source_path.starts_with(&root),
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
                    (path.starts_with(&root)
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
                    // Await blocking extraction separately so cancelling setup cannot start a project build.
                    let (installation, java) = cx.background_spawn(async {
                        let installation = preview::installation()?;
                        let java = preview::java_binary(&installation)?;
                        Ok::<_, anyhow::Error>((installation, java))
                    }).await?;
                    cx.background_spawn(async move {
                        let cache = preview::prepare(&root)?;
                        let temporary = tempfile::Builder::new()
                            .prefix("render-")
                            .tempdir_in(&cache)?;
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
                            cache.join("export.gradle").to_string_lossy().into_owned(),
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
                        let model = preview::parse_model(&output, &root, &target)?;
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
                            let label = definition.label();
                            let label = if result.parameter_index > 0 || !definition.parameters.get("methodParams").and_then(serde_json::Value::as_array).is_none_or(Vec::is_empty) {
                                format!("{label} · value {}", result.parameter_index + 1)
                            } else { label };
                                let image = result
                                    .image
                                    .as_ref()
                                    .map(|path| {
                                        std::fs::read(path).map(|bytes| {
                                            Arc::new(Image::from_bytes(ImageFormat::Png, bytes))
                                        })
                                    })
                                    .transpose()?;
                                let outlines = Arc::new(result.nodes.iter().map(|node| node.bounds)
                                    .filter(|[left, top, right, bottom]| right > left && bottom > top)
                                    .collect::<std::collections::BTreeSet<_>>().into_iter().collect());
                                let nodes = Arc::new(std::mem::take(&mut result.nodes));
                                cards.push(PreviewCard {
                                    label,
                                    result,
                                    image,
                                    rendered_image: None,
                                    nodes,
                                    outlines,
                                });
                            }
                        }
                        Ok::<_, anyhow::Error>(Gallery {
                            cards,
                            _directory: temporary,
                        })
                    }).await
                }.await;
                view.update_in(cx, |view, window, cx| {
                    view.building = false;
                    view.render_task = None;
                    let valid = view
                        .panel
                        .read_with(cx, |panel, cx| {
                            panel
                                .trusted_root(cx)
                                .is_ok_and(|root| root == request_root)
                                && panel.selected_target.as_ref() == Some(&request_target)
                        })
                        .unwrap_or(false);
                    if view.revision == revision && valid {
                        match result {
                            Ok(gallery) => {
                                view.release_gallery(window, cx);
                                view.list_state.reset(gallery.cards.len());
                                view.status = if gallery.cards.is_empty() {
                                    "No @Preview composables in this file".into()
                                } else {
                                    format!("{} previews · up to date", gallery.cards.len()).into()
                                };
                                view.gallery = Some(gallery);
                                view.stale = false;
                            }
                            Err(error) => {
                                view.error = Some(format!("{error:#}"));
                                view.status = "Preview refresh failed".into();
                            }
                        }
                    }
                    if view.pending
                        && (view.pending_manual || view.auto_refresh && view.visible(cx))
                    {
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

    fn release_gallery(&mut self, window: &mut Window, cx: &mut App) {
        if let Some(gallery) = self.gallery.take() {
            for card in gallery.cards {
                if let Some(image) = card.rendered_image {
                    window.drop_image(image).log_err();
                }
                if let Some(image) = card.image {
                    image.remove_asset(cx);
                }
            }
        }
    }

    fn navigate(
        &mut self,
        card_index: usize,
        node_index: usize,
        revision: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.stale || self.revision != revision {
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
        self.source_task = Some(cx.spawn_in(window, async move |view, cx| {
            let opened = workspace.update_in(cx, |workspace, window, cx| {
                workspace.open_path(project_path, Some(pane), true, window, cx)
            });
            let result = async {
                let item = opened?.await?;
                if !view.read_with(cx, |view, _| view.revision == revision && !view.stale)? {
                    return Ok(());
                }
                if let Some(editor) = item.downcast::<Editor>() {
                    editor.update_in(cx, |editor, window, cx| {
                        editor.go_to_singleton_buffer_point(
                            language::Point::new(node.line_number.saturating_sub(1) as u32, 0),
                            window,
                            cx,
                        )
                    })?;
                }
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
        let mut content = v_flex()
            .gap_2()
            .p_3()
            .w_full()
            .child(Label::new(card.label.clone()).truncate());
        if let Some(error) = &card.result.error {
            content = content.child(Label::new(error.clone()).color(Color::Error));
        }
        if let Some(error) = &card.result.inspection_error {
            content = content.child(Label::new(error.clone()).color(Color::Muted));
        }
        if let Some(image) = &card.image {
            card.rendered_image = image.clone().get_render_image(window, cx);
            let bounds = Rc::new(Cell::new(Bounds::<Pixels>::default()));
            let image_width = card.result.width as f32;
            let image_height = card.result.height as f32;
            let inspected = self.inspect;
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
                    .cursor_pointer()
                    .child(img(image.clone()).size_full())
                    .child(
                        canvas(
                            move |bounds, _, _| {
                                paint_bounds.set(bounds);
                                bounds
                            },
                            move |bounds, _, window, _| {
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
                            let bounds = bounds.get();
                            let x = f32::from(event.position.x - bounds.origin.x) * image_width
                                / f32::from(bounds.size.width);
                            let y = f32::from(event.position.y - bounds.origin.y) * image_height
                                / f32::from(bounds.size.height);
                            let hovered = (!view.stale && view.revision == revision)
                                .then(|| preview::hit_test(&nodes, x, y))
                                .flatten()
                                .map(|node| (index, node));
                            if hovered != view.hovered {
                                view.hovered = hovered;
                                cx.notify();
                            }
                        })
                    })
                    .on_mouse_down(MouseButton::Left, {
                        let nodes = nodes.clone();
                        cx.listener(move |view, event: &gpui::MouseDownEvent, window, cx| {
                            if view.stale || view.revision != revision {
                                return;
                            }
                            let bounds = bounds.get();
                            let x = f32::from(event.position.x - bounds.origin.x) * image_width
                                / f32::from(bounds.size.width);
                            let y = f32::from(event.position.y - bounds.origin.y) * image_height
                                / f32::from(bounds.size.height);
                            let hit = preview::hit_test(&nodes, x, y);
                            if view.inspect {
                                if let Some(node) = hit {
                                    view.navigate(index, node, revision, window, cx);
                                }
                            } else {
                                view.inspect = true;
                                view.list_state.remeasure();
                            }
                            view.hovered = hit.map(|node| (index, node));
                            cx.notify();
                        })
                    }),
            );
        }
        if self.inspect {
            let selected =
                self.hovered
                    .filter(|(card, _)| *card == index)
                    .and_then(|(_, node_index)| {
                        card.nodes.get(node_index).map(|node| (node_index, node))
                    });
            let footer = h_flex().h_8().gap_2().child(
                PopoverMenu::new(("compose-components", index))
                    .trigger(
                        Button::new(("compose-components-trigger", index), "Components")
                            .disabled(
                                self.stale || !nodes.iter().any(|node| node.source_path.is_some()),
                            )
                            .tab_index(0isize),
                    )
                    .menu({
                        let view = cx.weak_entity();
                        move |window, cx| {
                            Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                                let mut seen = std::collections::BTreeSet::new();
                                for (node_index, node) in nodes.iter().enumerate() {
                                    if let Some(path) = &node.source_path
                                        && node.line_number > 0
                                        && seen.insert((path.clone(), node.line_number))
                                    {
                                        let label =
                                            format!("{}:{}", node.file_name, node.line_number);
                                        let view = view.clone();
                                        menu = menu.entry(label, None, move |window, cx| {
                                            view.update(cx, |view, cx| {
                                                view.navigate(
                                                    index, node_index, revision, window, cx,
                                                )
                                            })
                                            .log_err();
                                        });
                                    }
                                }
                                menu
                            }))
                        }
                    }),
            );
            content = content.child(if let Some((node_index, node)) = selected {
                footer.child(
                    Button::new(
                        ("compose-source", index),
                        format!("{}:{}", node.file_name, node.line_number),
                    )
                    .disabled(self.stale)
                    .tab_index(0isize)
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.navigate(index, node_index, revision, window, cx)
                    })),
                )
            } else {
                footer.child(
                    Label::new("Select a component to open its source")
                        .color(Color::Muted)
                        .truncate(),
                )
            });
        }
        content
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .into_any_element()
    }
}

fn buffer_path(buffer: &Entity<Buffer>, cx: &App) -> Option<PathBuf> {
    Some(buffer.read(cx).file()?.as_local()?.abs_path(cx))
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
impl EventEmitter<ItemEvent> for ComposePreviewView {}
impl Item for ComposePreviewView {
    type Event = ItemEvent;
    fn tab_content_text(&self, _: usize, _: &App) -> SharedString {
        format!(
            "{} · Compose",
            self.source_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        )
        .into()
    }
    fn show_toolbar(&self) -> bool {
        false
    }
    fn to_item_events(event: &Self::Event, callback: &mut dyn FnMut(ItemEvent)) {
        callback(*event);
    }
}
impl Render for ComposePreviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let maximum_width = self
            .gallery
            .as_ref()
            .and_then(|gallery| gallery.cards.iter().map(|card| card.result.width).max())
            .unwrap_or(0) as f32;
        let viewport_width = self.viewport_width.clone();
        v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .child(
                h_flex()
                    .flex_wrap()
                    .flex_none()
                    .gap_1()
                    .p_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        Button::new("compose-refresh", "Build & Refresh")
                            .tab_index(0isize)
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.queue_refresh(true, window, cx)
                            })),
                    )
                    .when(self.building, |toolbar| {
                        toolbar.child(
                            Button::new("compose-stop", "Stop")
                                .tab_index(0isize)
                                .on_click(cx.listener(|view, _, _, cx| view.stop(cx))),
                        )
                    })
                    .child(
                        Button::new("compose-auto", "Auto")
                            .toggle_state(self.auto_refresh)
                            .tab_index(0isize)
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.auto_refresh = !view.auto_refresh;
                                if view.auto_refresh && view.pending {
                                    view.queue_refresh(false, window, cx);
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("compose-inspect", "Inspect")
                            .toggle_state(self.inspect)
                            .tab_index(0isize)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.inspect = !view.inspect;
                                view.list_state.remeasure();
                                cx.notify();
                            })),
                    )
                    .child(
                        PopoverMenu::new("compose-preview-list")
                            .trigger(
                                Button::new("compose-preview-list-trigger", "Previews")
                                    .disabled(
                                        self.gallery
                                            .as_ref()
                                            .is_none_or(|gallery| gallery.cards.is_empty()),
                                    )
                                    .tab_index(0isize),
                            )
                            .menu({
                                let view = cx.weak_entity();
                                let revision = self.revision;
                                let labels = self
                                    .gallery
                                    .as_ref()
                                    .map(|gallery| {
                                        gallery
                                            .cards
                                            .iter()
                                            .map(|card| card.label.clone())
                                            .collect::<Vec<_>>()
                                    })
                                    .unwrap_or_default();
                                move |window, cx| {
                                    Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                                        for (index, label) in labels.iter().enumerate() {
                                            let view = view.clone();
                                            menu = menu.entry(label.clone(), None, move |_, cx| {
                                                view.update(cx, |view, cx| {
                                                    if view.revision == revision {
                                                        view.list_state.scroll_to(
                                                            gpui::ListOffset {
                                                                item_ix: index,
                                                                offset_in_item: px(0.),
                                                            },
                                                        );
                                                        cx.notify();
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
                    .child(
                        Button::new("compose-fit", "Fit")
                            .tab_index(0isize)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                if maximum_width > 0. {
                                    view.zoom = ((view.viewport_width.get() - 40.) / maximum_width)
                                        .clamp(0.1, 2.);
                                    view.list_state.remeasure();
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        Button::new("compose-zoom-out", "−")
                            .tab_index(0isize)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.zoom = (view.zoom / 1.25).max(0.1);
                                view.list_state.remeasure();
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("compose-zoom-in", "+")
                            .tab_index(0isize)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.zoom = (view.zoom * 1.25).min(2.);
                                view.list_state.remeasure();
                                cx.notify();
                            })),
                    ),
            )
            .child(Label::new(self.status.clone()).color(if self.stale {
                Color::Warning
            } else {
                Color::Muted
            }))
            .when_some(self.error.clone(), |view, error| {
                view.child(div().p_3().child(Label::new(error).color(Color::Error)))
            })
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        canvas(
                            move |bounds, _, _| viewport_width.set(f32::from(bounds.size.width)),
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(
                        div()
                            .id("compose-horizontal-scroll")
                            .size_full()
                            .overflow_x_scroll()
                            .restrict_scroll_to_axis()
                            .track_scroll(&self.horizontal_scroll)
                            .child(
                                div()
                                    .h_full()
                                    .w_full()
                                    .min_w(px(maximum_width * self.zoom + 32.))
                                    .child(
                                        list(
                                            self.list_state.clone(),
                                            cx.processor(|view, index, window, cx| {
                                                view.render_card(index, window, cx)
                                            }),
                                        )
                                        .size_full(),
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

    async fn test_project(cx: &mut TestAppContext) -> (Entity<Project>, Entity<Buffer>) {
        cx.update(|cx| {
            AppState::test(cx);
            editor::init(cx);
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
        panel.update(cx, |panel, _| {
            panel.root = Some(PathBuf::from("/android"));
            panel.selected_target = Some(target.clone());
        });
        workspace.add_panel(panel.clone(), window, cx);
        let source_pane = workspace.active_pane().clone();
        let view = cx.new(|cx| {
            ComposePreviewView::new(
                panel.downgrade(),
                workspace.weak_handle(),
                project,
                buffer,
                source_pane.downgrade(),
                PathBuf::from("/android"),
                target,
                window,
                cx,
            )
        });
        view.update(cx, |view, _| view.auto_refresh = false);
        panel.update(cx, |panel, _| panel.preview_view = Some(view.downgrade()));
        let pane = workspace.split_pane(source_pane, SplitDirection::Right, window, cx);
        pane.update(cx, |pane, cx| {
            pane.add_item(Box::new(view.clone()), false, false, None, window, cx)
        });
        (view, panel, pane)
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
                pane.add_item(Box::new(other.clone()), false, false, None, window, cx)
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
            assert_eq!(pane.read(cx).items_len(), 1);
        });
    }

    #[gpui::test]
    async fn showing_existing_preview_activates_its_tab(cx: &mut TestAppContext) {
        let (project, buffer) = test_project(cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let (view, panel, pane) = workspace.update_in(cx, |workspace, window, cx| {
            add_preview(workspace, project, buffer, window, cx)
        });
        pane.update_in(cx, |pane, window, cx| {
            let other = cx.new(TestItem::new);
            pane.add_item(Box::new(other), true, false, None, window, cx);
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
            view.entity_id()
        );
        assert_eq!(pane.read_with(cx, |pane, _| pane.items_len()), 2);
    }

    #[gpui::test]
    async fn refresh_requests_coalesce_and_manual_refresh_survives_auto_off(
        cx: &mut TestAppContext,
    ) {
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
            view.auto_refresh = true;
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
    async fn changing_project_rebinds_an_already_active_kotlin_editor(cx: &mut TestAppContext) {
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
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |view, _| view.source_path.clone()),
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
            cx.notify();
            target
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.configuration_suspended && !view.pending && view.error.is_some())
        });
        panel.update(cx, |panel, cx| {
            panel.selected_target = target;
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
            view.gallery = Some(Gallery {
                cards: vec![PreviewCard {
                    label: "Greeting".into(),
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
                }],
                _directory: tempfile::tempdir().expect("Render directory"),
            });
            view.list_state.reset(1);
            view.stale = false;
            cx.notify();
        });
        cx.run_until_parked();
        let bounds = cx.debug_bounds("compose-image-0").expect("Rendered image");
        assert_eq!(bounds.size, size(px(50.), px(40.)));
        let position = bounds.origin + point(px(12.5), px(12.5));
        cx.simulate_click(position, Default::default());
        assert!(view.read_with(cx, |view, _| view.inspect));
        assert_eq!(view.read_with(cx, |view, _| view.hovered), Some((0, 0)));
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
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.target.variant, "release");
            assert_eq!(view.source_path, Path::new("/android/Main.kt"));
        });
    }
}
