use std::mem;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use android_tools::vector_drawable::{self, Resources};
use anyhow::{Context as _, Result};
use futures::StreamExt as _;

use file_icons::FileIcons;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    RenderImage, Styled, Subscription, Task, WeakEntity, Window, div, img,
};
use language::{Buffer, BufferEvent};
use multi_buffer::MultiBuffer;
use ui::prelude::*;
use workspace::item::Item;
use workspace::{Pane, Workspace};

use crate::{
    OpenFollowingPreview, OpenPreview, OpenPreviewToTheSide, OpenVectorDrawablePreview,
    OpenVectorDrawablePreviewToTheSide,
};

#[derive(Clone, Copy, PartialEq)]
enum PreviewKind {
    Svg,
    VectorDrawable,
}

impl PreviewKind {
    fn label(self) -> &'static str {
        match self {
            Self::Svg => "SVG",
            Self::VectorDrawable => "Vector Drawable",
        }
    }
}

pub struct SvgPreviewView {
    focus_handle: FocusHandle,
    kind: PreviewKind,
    dark_background: bool,
    project: WeakEntity<project::Project>,
    buffer: Option<Entity<Buffer>>,
    current_svg: Option<Result<Arc<RenderImage>, SharedString>>,
    _refresh: Task<()>,
    _buffer_subscription: Option<Subscription>,
    _workspace_subscription: Option<Subscription>,
    _project_subscriptions: Vec<Subscription>,
    _resource_subscriptions: Vec<Subscription>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SvgPreviewMode {
    /// The preview will always show the contents of the provided editor.
    Default,
    /// The preview will "follow" the last active editor of an SVG file.
    Follow,
}

impl SvgPreviewView {
    pub fn new(
        mode: SvgPreviewMode,
        active_buffer: Entity<MultiBuffer>,
        workspace_handle: WeakEntity<Workspace>,
        project: WeakEntity<project::Project>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let workspace_subscription = if mode == SvgPreviewMode::Follow
                && let Some(workspace) = workspace_handle.upgrade()
            {
                Some(Self::subscribe_to_workspace(workspace, window, cx))
            } else {
                None
            };

            let buffer = active_buffer.read_with(cx, |buffer, _cx| buffer.as_singleton());

            let subscription = buffer
                .as_ref()
                .map(|buffer| Self::create_buffer_subscription(buffer, window, cx));

            let kind = if Self::is_svg_file(&active_buffer, cx) {
                PreviewKind::Svg
            } else {
                PreviewKind::VectorDrawable
            };
            let mut project_subscriptions = Vec::new();
            if kind == PreviewKind::VectorDrawable
                && let Some(project) = project.upgrade()
            {
                let buffer_store = project.read(cx).buffer_store().clone();
                project_subscriptions.push(cx.subscribe_in(
                    &buffer_store,
                    window,
                    |_, _, event, window, cx| {
                        use project::buffer_store::BufferStoreEvent;
                        let changed_values =
                            match event {
                                BufferStoreEvent::BufferAdded(buffer) => buffer
                                    .read(cx)
                                    .file()
                                    .is_some_and(|file| is_values_xml(file.path().as_std_path())),
                                BufferStoreEvent::BufferChangedFilePath { buffer, old_file } => {
                                    buffer.read(cx).file().is_some_and(|file| {
                                        is_values_xml(file.path().as_std_path())
                                    }) || old_file.as_ref().is_some_and(|file| {
                                        is_values_xml(file.path().as_std_path())
                                    })
                                }
                                BufferStoreEvent::BufferDropped(_) => true,
                                _ => false,
                            };
                        if changed_values {
                            cx.defer_in(window, |this: &mut Self, window, cx| {
                                this.render_image(window, cx)
                            });
                        }
                    },
                ));
                project_subscriptions.push(cx.subscribe_in(
                    &project,
                    window,
                    |_, _, event, window, cx| {
                        if let project::Event::WorktreeUpdatedEntries(_, changes) = event
                            && changes
                                .iter()
                                .any(|(path, _, _)| is_values_xml(path.as_std_path()))
                        {
                            cx.defer_in(window, |this: &mut Self, window, cx| {
                                this.render_image(window, cx)
                            });
                        }
                    },
                ));
            }
            let mut this = Self {
                kind,
                dark_background: false,
                project,
                focus_handle: cx.focus_handle(),
                buffer,
                current_svg: None,
                _buffer_subscription: subscription,
                _workspace_subscription: workspace_subscription,
                _project_subscriptions: project_subscriptions,
                _resource_subscriptions: Vec::new(),
                _refresh: Task::ready(()),
            };
            this.render_image(window, cx);

            this
        })
    }

    fn subscribe_to_workspace(
        workspace: Entity<Workspace>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(
            &workspace,
            window,
            move |this: &mut SvgPreviewView, workspace, event: &workspace::Event, window, cx| {
                if let workspace::Event::ActiveItemChanged = event {
                    let workspace = workspace.read(cx);
                    if let Some(active_item) = workspace.active_item(cx)
                        && let Some(buffer) = active_item.downcast::<MultiBuffer>()
                        && Self::is_svg_file(&buffer, cx)
                    {
                        let Some(buffer) = buffer.read(cx).as_singleton() else {
                            return;
                        };
                        if this.buffer.as_ref() != Some(&buffer) {
                            this._buffer_subscription =
                                Some(Self::create_buffer_subscription(&buffer, window, cx));
                            this.buffer = Some(buffer);
                            this.render_image(window, cx);
                            cx.notify();
                        }
                    } else {
                        this.set_current(None, window, cx);
                    }
                }
            },
        )
    }

    fn render_image(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(buffer) = self.buffer.as_ref() else {
            return;
        };
        let renderer = cx.svg_renderer();
        let content = buffer.read(cx).snapshot();
        let kind = self.kind;
        let project = (kind == PreviewKind::VectorDrawable)
            .then(|| self.project.upgrade())
            .flatten();
        let fs = project
            .as_ref()
            .map(|project| project.read(cx).fs().clone());
        let resource_directory = content.file().and_then(|file| {
            let path = file.as_local()?.abs_path(cx);
            vector_resource_directory(&path)
        });
        let mut resource_overrides = Vec::new();
        self._resource_subscriptions.clear();
        if let Some(project) = project.as_ref() {
            for buffer in project.read(cx).opened_buffers(cx) {
                let path = buffer
                    .read(cx)
                    .file()
                    .and_then(|file| file.as_local().map(|file| file.abs_path(cx)));
                if let Some(path) = path
                    && path.parent() == resource_directory.as_deref()
                    && is_values_xml(&path)
                {
                    resource_overrides.push((path, buffer.read(cx).snapshot().text()));
                    self._resource_subscriptions
                        .push(Self::create_buffer_subscription(&buffer, window, cx));
                }
            }
        }
        let executor = cx.background_executor().clone();
        let background_task = cx.background_spawn(async move {
            let svg = if kind == PreviewKind::VectorDrawable {
                executor.timer(Duration::from_millis(150)).await;
                let mut resources = Resources::default();
                if let (Some(fs), Some(directory)) = (fs, resource_directory) {
                    if fs.is_dir(&directory).await {
                        let mut entries = fs.read_dir(&directory).await?;
                        let mut paths = Vec::new();
                        while let Some(path) = entries.next().await {
                            let path = path?;
                            if path
                                .extension()
                                .is_some_and(|ext| ext.eq_ignore_ascii_case("xml"))
                            {
                                paths.push(path);
                            }
                        }
                        paths.sort();
                        for path in paths {
                            if !resource_overrides.iter().any(|(source, _)| source == &path) {
                                resources
                                    .add_values_xml(&fs.load(&path).await?)
                                    .with_context(|| {
                                        format!("Invalid resources in {}", path.display())
                                    })?;
                            }
                        }
                    }
                }
                for (path, xml) in resource_overrides {
                    resources
                        .add_values_xml(&xml)
                        .with_context(|| format!("Invalid resources in {}", path.display()))?;
                }
                vector_drawable::to_svg(&content.text(), &resources)?
            } else {
                content.text()
            };
            renderer
                .render_single_frame(svg.as_bytes(), 1.0)
                .map_err(anyhow::Error::from)
        });

        self._refresh = cx.spawn_in(window, async move |this, cx| {
            let result = background_task.await;

            this.update_in(cx, |view, window, cx| {
                let current = result.map_err(|e| format!("{e:#}").into());
                view.set_current(Some(current), window, cx);
            })
            .ok();
        });
    }

    fn set_current(
        &mut self,
        image: Option<Result<Arc<RenderImage>, SharedString>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(Ok(image)) = mem::replace(&mut self.current_svg, image) {
            window.drop_image(image).ok();
        }
        cx.notify();
    }

    fn find_existing_preview_item_idx(
        pane: &Pane,
        buffer: &Entity<MultiBuffer>,
        cx: &App,
    ) -> Option<usize> {
        let buffer_id = buffer.read(cx).as_singleton()?.entity_id();
        pane.items_of_type::<SvgPreviewView>()
            .find(|view| {
                view.read(cx)
                    .buffer
                    .as_ref()
                    .is_some_and(|buffer| buffer.entity_id() == buffer_id)
            })
            .and_then(|view| pane.index_for_item(&view))
    }

    pub fn resolve_active_item_as_svg_buffer(
        workspace: &Workspace,
        cx: &mut Context<Workspace>,
    ) -> Option<Entity<MultiBuffer>> {
        workspace
            .active_item(cx)?
            .act_as::<MultiBuffer>(cx)
            .filter(|buffer| Self::is_svg_file(&buffer, cx))
    }

    fn create_svg_view(
        mode: SvgPreviewMode,
        workspace: &mut Workspace,
        buffer: Entity<MultiBuffer>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<SvgPreviewView> {
        let workspace_handle = workspace.weak_handle();
        let project = workspace.project().downgrade();
        SvgPreviewView::new(mode, buffer, workspace_handle, project, window, cx)
    }

    fn create_buffer_subscription(
        buffer: &Entity<Buffer>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(
            buffer,
            window,
            move |this, _buffer, event: &BufferEvent, window, cx| match event {
                BufferEvent::Edited { .. }
                | BufferEvent::Saved
                | BufferEvent::Reloaded
                | BufferEvent::FileHandleChanged => {
                    this.render_image(window, cx);
                }
                _ => {}
            },
        )
    }

    pub fn is_svg_file(buffer: &Entity<MultiBuffer>, cx: &App) -> bool {
        buffer
            .read(cx)
            .as_singleton()
            .and_then(|buffer| buffer.read(cx).file())
            .is_some_and(|file| {
                std::path::Path::new(file.file_name(cx))
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
            })
    }

    pub fn is_vector_drawable_file(buffer: &Entity<MultiBuffer>, cx: &App) -> bool {
        buffer.read(cx).as_singleton().is_some_and(|buffer| {
            let buffer = buffer.read(cx);
            buffer.file().is_some_and(|file| {
                file.path()
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("xml"))
            }) && vector_drawable::is_vector_drawable(&buffer.snapshot().text())
        })
    }

    pub fn open_preview_in_pane(
        workspace: &mut Workspace,
        buffer: Entity<MultiBuffer>,
        pane: Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        Self::activate_or_add_preview(workspace, buffer, pane, true, window, cx);
    }

    pub fn open_preview_to_the_side_of_pane(
        workspace: &mut Workspace,
        buffer: Entity<MultiBuffer>,
        origin_pane: Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let target_pane = workspace.adjacent_pane_of(&origin_pane, window, cx);
        Self::activate_or_add_preview(workspace, buffer, target_pane, false, window, cx);
    }

    fn activate_or_add_preview(
        workspace: &mut Workspace,
        buffer: Entity<MultiBuffer>,
        pane: Entity<Pane>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let existing_view_idx = Self::find_existing_preview_item_idx(pane.read(cx), &buffer, cx);
        if let Some(existing_view_idx) = existing_view_idx {
            pane.update(cx, |pane, cx| {
                pane.activate_item(existing_view_idx, focus, focus, window, cx);
            });
        } else {
            let view =
                Self::create_svg_view(SvgPreviewMode::Default, workspace, buffer, window, cx);
            pane.update(cx, |pane, cx| {
                pane.add_item(Box::new(view), focus, focus, None, window, cx)
            });
        }
        cx.notify();
    }

    pub fn register(workspace: &mut Workspace, _window: &mut Window, _cx: &mut Context<Workspace>) {
        workspace.register_action(
            move |workspace, _: &OpenVectorDrawablePreview, window, cx| {
                if let Some(buffer) = workspace
                    .active_item(cx)
                    .and_then(|item| item.act_as::<MultiBuffer>(cx))
                    && Self::is_vector_drawable_file(&buffer, cx)
                {
                    let pane = workspace.active_pane().clone();
                    Self::open_preview_in_pane(workspace, buffer, pane, window, cx);
                }
            },
        );
        workspace.register_action(
            move |workspace, _: &OpenVectorDrawablePreviewToTheSide, window, cx| {
                if let Some(buffer) = workspace
                    .active_item(cx)
                    .and_then(|item| item.act_as::<MultiBuffer>(cx))
                    && Self::is_vector_drawable_file(&buffer, cx)
                {
                    let pane = workspace.active_pane().clone();
                    Self::open_preview_to_the_side_of_pane(workspace, buffer, pane, window, cx);
                }
            },
        );
        workspace.register_action(move |workspace, _: &OpenPreview, window, cx| {
            if let Some(buffer) = Self::resolve_active_item_as_svg_buffer(workspace, cx) {
                let pane = workspace.active_pane().clone();
                Self::open_preview_in_pane(workspace, buffer, pane, window, cx);
            }
        });

        workspace.register_action(move |workspace, _: &OpenPreviewToTheSide, window, cx| {
            if let Some(buffer) = Self::resolve_active_item_as_svg_buffer(workspace, cx) {
                let pane = workspace.active_pane().clone();
                Self::open_preview_to_the_side_of_pane(workspace, buffer, pane, window, cx);
            }
        });

        workspace.register_action(move |workspace, _: &OpenFollowingPreview, window, cx| {
            if let Some(editor) = Self::resolve_active_item_as_svg_buffer(workspace, cx)
                && Self::is_svg_file(&editor, cx)
            {
                let view =
                    Self::create_svg_view(SvgPreviewMode::Follow, workspace, editor, window, cx);
                workspace.active_pane().update(cx, |pane, cx| {
                    pane.add_item(Box::new(view), true, true, None, window, cx)
                });
                cx.notify();
            }
        });
    }
}

impl Render for SvgPreviewView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let kind = self.kind;
        let background =
            if kind == PreviewKind::VectorDrawable && matches!(self.current_svg, Some(Ok(_))) {
                gpui::rgb(if self.dark_background {
                    0x202020
                } else {
                    0xffffff
                })
                .into()
            } else {
                cx.theme().colors().editor_background
            };
        v_flex()
            .id(if kind == PreviewKind::Svg {
                "SvgPreview"
            } else {
                "VectorDrawablePreview"
            })
            .key_context(if kind == PreviewKind::Svg {
                "SvgPreview"
            } else {
                "VectorDrawablePreview"
            })
            .track_focus(&self.focus_handle(cx))
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .when(kind == PreviewKind::VectorDrawable, |this| {
                this.child(
                    h_flex()
                        .w_full()
                        .p_2()
                        .gap_1()
                        .border_b_1()
                        .border_color(cx.theme().colors().border)
                        .child(Label::new("Background").size(LabelSize::Small))
                        .child(
                            Button::new("vector-light-background", "Light")
                                .toggle_state(!self.dark_background)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.dark_background = false;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("vector-dark-background", "Dark")
                                .toggle_state(self.dark_background)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.dark_background = true;
                                    cx.notify();
                                })),
                        )
                        .child(div().flex_1())
                        .child(
                            IconButton::new("refresh-vector-preview", IconName::RotateCw)
                                .tooltip(ui::Tooltip::text("Refresh Vector Drawable Preview"))
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.render_image(window, cx)
                                    }),
                                ),
                        ),
                )
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_hidden()
                    .bg(background)
                    .justify_center()
                    .items_center()
                    .map(|this| match self.current_svg.clone() {
                        Some(Ok(image)) => this.child(
                            img(image).max_w_full().max_h_full().with_fallback(move || {
                                h_flex()
                                    .p_4()
                                    .gap_2()
                                    .child(Icon::new(IconName::Warning))
                                    .child(format!("Failed to load {} image", kind.label()))
                                    .into_any_element()
                            }),
                        ),
                        Some(Err(error)) => this.child(div().p_4().child(error).into_any_element()),
                        None => this.child(div().p_4().child(if kind == PreviewKind::Svg {
                            "No SVG file selected"
                        } else {
                            "Loading vector drawable preview…"
                        })),
                    }),
            )
    }
}

impl Focusable for SvgPreviewView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<()> for SvgPreviewView {}

impl Item for SvgPreviewView {
    type Event = ();

    fn tab_icon(&self, _window: &Window, cx: &App) -> Option<Icon> {
        self.buffer
            .as_ref()
            .and_then(|buffer| buffer.read(cx).file())
            .and_then(|file| FileIcons::get_icon(file.path().as_std_path(), cx))
            .map(Icon::from_path)
            .or_else(|| Some(Icon::new(IconName::Image)))
    }

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        self.buffer
            .as_ref()
            .and_then(|svg_path| svg_path.read(cx).file())
            .map(|name| format!("Preview {}", name.file_name(cx)).into())
            .unwrap_or_else(|| format!("{} Preview", self.kind.label()).into())
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some(match self.kind {
            PreviewKind::Svg => "svg preview: open",
            PreviewKind::VectorDrawable => "android vector preview: open",
        })
    }

    fn to_item_events(_event: &Self::Event, _f: &mut dyn FnMut(workspace::item::ItemEvent)) {}
}

fn vector_resource_directory(path: &Path) -> Option<PathBuf> {
    let directory = path.parent()?;
    let name = directory.file_name()?.to_str()?;
    if !matches!(name.split('-').next()?, "drawable" | "mipmap") {
        return None;
    }
    let resources = directory.parent()?;
    (resources.file_name()? == "res").then(|| resources.join("values"))
}

fn is_values_xml(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("xml"))
        && path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "values")
}
