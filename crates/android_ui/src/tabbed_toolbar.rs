// Adapted from AOSP TabbedToolbar.java and HorizontalScrollView.kt, Apache-2.0
// (Copyright 2018/2019/2021 The Android Open Source Project). Unchanged originals,
// tests, artwork and license are retained in ../test_data/tabbed_toolbar.

use anyhow::{Context as _, Result};
use gpui::{AnyView, App, Context, FocusHandle, MouseButton, ScrollHandle, canvas, point};
use std::rc::Rc;
use ui::{ButtonLike, Tooltip, prelude::*};
use util::ResultExt as _;

pub type ToolbarListener = Rc<dyn Fn(&mut Window, &mut App)>;

struct ToolbarTab {
    id: usize,
    label: SharedString,
    icon: Option<Icon>,
    selected: ToolbarListener,
    closed: Option<ToolbarListener>,
    focus: FocusHandle,
    close_focus: Option<FocusHandle>,
}

struct ToolbarAction {
    icon: Icon,
    label: SharedString,
    clicked: ToolbarListener,
    focus: FocusHandle,
}

/// An embedded tool-window header with a custom title, tabs and icon actions.
pub struct TabbedToolbar {
    title: AnyView,
    tabs: Vec<ToolbarTab>,
    actions: Vec<ToolbarAction>,
    active_tab: Option<usize>,
    next_tab_id: usize,
    scroll: ScrollHandle,
    scroll_left_visible: bool,
    scroll_right_visible: bool,
    scroll_left_focus: FocusHandle,
    scroll_right_focus: FocusHandle,
}

impl TabbedToolbar {
    pub fn new(title: impl Into<AnyView>, cx: &mut Context<Self>) -> Self {
        Self {
            title: title.into(),
            tabs: Vec::new(),
            actions: Vec::new(),
            active_tab: None,
            next_tab_id: 0,
            scroll: ScrollHandle::new(),
            scroll_left_visible: false,
            scroll_right_visible: false,
            scroll_left_focus: cx.focus_handle(),
            scroll_right_focus: cx.focus_handle(),
        }
    }

    pub fn add_tab(
        &mut self,
        label: impl Into<SharedString>,
        cx: &mut Context<Self>,
        selected: impl Fn(&mut Window, &mut App) + 'static,
        closed: Option<ToolbarListener>,
    ) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(ToolbarTab {
            id,
            label: label.into(),
            icon: None,
            selected: Rc::new(selected),
            close_focus: closed.as_ref().map(|_| cx.focus_handle()),
            closed,
            focus: cx.focus_handle(),
        });
        self.scroll.scroll_to_item(self.tabs.len() - 1);
        cx.notify();
    }

    pub fn add_action(
        &mut self,
        icon: Icon,
        label: impl Into<SharedString>,
        cx: &mut Context<Self>,
        clicked: impl Fn(&mut Window, &mut App) + 'static,
    ) {
        self.actions.push(ToolbarAction {
            icon,
            label: label.into(),
            clicked: Rc::new(clicked),
            focus: cx.focus_handle(),
        });
        cx.notify();
    }

    pub fn count_tabs(&self) -> usize {
        self.tabs.len()
    }

    pub fn clear_tabs(&mut self, cx: &mut Context<Self>) {
        self.tabs.clear();
        self.active_tab = None;
        self.scroll = ScrollHandle::new();
        self.scroll_left_visible = false;
        self.scroll_right_visible = false;
        cx.notify();
    }

    /// Rejects an invalid index, matching the reference's IndexOutOfBoundsException
    /// through Rust's fallible API. It never calls a listener for an invalid index.
    /// The listener runs after this entity update, so it can rebuild the toolbar.
    pub fn select_tab(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.set_active_tab(index, cx)?;
        let listener = self
            .tabs
            .get(index)
            .context("Toolbar tab disappeared")?
            .selected
            .clone();
        window.defer(cx, move |window, cx| listener(window, cx));
        Ok(())
    }

    /// Reflects an owner's existing selection without invoking its listener again.
    pub fn set_active_tab(&mut self, index: usize, cx: &mut Context<Self>) -> Result<()> {
        let tab = self
            .tabs
            .get(index)
            .with_context(|| format!("Toolbar tab index {index} is out of range"))?;
        let id = Some(tab.id);
        if self.active_tab != id {
            self.active_tab = id;
            self.scroll.scroll_to_item(index);
            cx.notify();
        }
        Ok(())
    }

    pub fn set_tab_icon(
        &mut self,
        index: usize,
        icon: Option<Icon>,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let tab = self
            .tabs
            .get_mut(index)
            .with_context(|| format!("Toolbar tab index {index} is out of range"))?;
        tab.icon = icon;
        cx.notify();
        Ok(())
    }

    fn select_id(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
            // The rendered ID can disappear before a pending event is dispatched.
            // Resolve it against current tabs instead of selecting a new occupant.
            if let Some(tab) = self.tabs.get(index) {
                tab.focus.focus(window, cx);
            }
            self.select_tab(index, window, cx).log_err();
        }
    }

    fn scroll_by(&mut self, amount: Pixels, cx: &mut Context<Self>) {
        let maximum = self.scroll.max_offset().x;
        let offset = self.scroll.offset().x;
        let offset = (offset - amount).clamp(-maximum, px(0.));
        self.scroll.set_offset(point(offset, px(0.)));
        self.scroll_left_visible = offset < px(0.);
        self.scroll_right_visible = -offset < maximum;
        cx.notify();
    }
}

impl Render for TabbedToolbar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hover = cx.theme().colors().ghost_element_hover;
        let accent = cx.theme().colors().text_accent;
        let scroll = self.scroll.clone();
        let toolbar = cx.entity().downgrade();
        let previous_visibility = (self.scroll_left_visible, self.scroll_right_visible);
        h_flex()
            .id("tabbed-toolbar")
            .debug_selector(|| "tabbed-toolbar".into())
            .h(px(36.))
            .w_full()
            .flex_none()
            .child(div().flex_none().mr(px(5.)).child(self.title.clone()))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .when(self.scroll_left_visible, |this| {
                        this.child(
                            div()
                                .flex_none()
                                .debug_selector(|| "tabbed-toolbar-scroll-left".into())
                                .on_mouse_down(MouseButton::Left, {
                                    let focus = self.scroll_left_focus.clone();
                                    move |_, window, cx| focus.focus(window, cx)
                                })
                                .child(
                                    ButtonLike::new("tabbed-toolbar-scroll-left")
                                        .height(px(24.).into())
                                        .aria_label("Scroll tabs left")
                                        .tooltip(Tooltip::text("Scroll tabs left"))
                                        .tab_index(0isize)
                                        .track_focus(&self.scroll_left_focus)
                                        .child(Label::new("←"))
                                        .on_click(cx.listener(|toolbar, _, _, cx| {
                                            toolbar.scroll_by(px(-30.), cx)
                                        })),
                                ),
                        )
                    })
                    .child(
                        h_flex()
                            .id("tabbed-toolbar-tabs")
                            .debug_selector(|| "tabbed-toolbar-tabs".into())
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_x_scroll()
                            .track_scroll(&self.scroll)
                            .role(gpui::Role::TabList)
                            .children(self.tabs.iter().map(|tab| {
                                let id = tab.id;
                                h_flex()
                                    .id(("tabbed-toolbar-tab", id))
                                    .debug_selector(move || format!("tabbed-toolbar-tab-{id}"))
                                    .role(gpui::Role::Tab)
                                    .aria_label(tab.label.clone())
                                    .aria_selected(self.active_tab == Some(id))
                                    .track_focus(&tab.focus)
                                    .tab_index(0isize)
                                    .tab_stop(true)
                                    .relative()
                                    .flex_none()
                                    .h_full()
                                    .px(px(10.))
                                    .py(px(5.))
                                    .cursor_pointer()
                                    .hover(move |style| style.bg(hover))
                                    .focus(move |style| style.bg(hover))
                                    .when_some(tab.icon.clone(), |this, icon| {
                                        this.child(
                                            div()
                                                .mr(px(4.))
                                                .debug_selector(move || {
                                                    format!("tabbed-toolbar-tab-icon-{id}")
                                                })
                                                .child(icon),
                                        )
                                    })
                                    .child(
                                        div()
                                            .debug_selector({
                                                let label = tab.label.clone();
                                                move || format!("tabbed-toolbar-label-{label}")
                                            })
                                            .child(Label::new(tab.label.clone())),
                                    )
                                    .when_some(tab.closed.clone(), |this, closed| {
                                        this.child(
                                            div()
                                                .debug_selector(move || {
                                                    format!("tabbed-toolbar-close-{id}")
                                                })
                                                .on_mouse_down(MouseButton::Left, {
                                                    let focus = tab.close_focus.clone();
                                                    move |_, window, cx| {
                                                        if let Some(focus) = &focus {
                                                            focus.focus(window, cx);
                                                        }
                                                        cx.stop_propagation()
                                                    }
                                                })
                                                .on_mouse_down(MouseButton::Middle, |_, _, cx| {
                                                    cx.stop_propagation()
                                                })
                                                .on_mouse_up(MouseButton::Middle, |_, _, cx| {
                                                    cx.stop_propagation()
                                                })
                                                .on_mouse_down(MouseButton::Right, |_, _, cx| {
                                                    cx.stop_propagation()
                                                })
                                                .child(
                                                    ButtonLike::new(("tabbed-toolbar-close", id))
                                                        .width(px(24.))
                                                        .height(px(24.).into())
                                                        .aria_label(format!("Close {}", tab.label))
                                                        .tooltip(Tooltip::text(format!(
                                                            "Close {}",
                                                            tab.label
                                                        )))
                                                        .tab_index(0isize)
                                                        .when_some(
                                                            tab.close_focus.as_ref(),
                                                            |this, focus| this.track_focus(focus),
                                                        )
                                                        .child(Icon::from_path(
                                                            "icons/android-studio-close.svg",
                                                        ))
                                                        .on_click(move |_, window, cx| {
                                                            cx.stop_propagation();
                                                            closed(window, cx);
                                                        }),
                                                ),
                                        )
                                    })
                                    .when(self.active_tab == Some(id), |this| {
                                        this.child(
                                            div()
                                                .debug_selector(move || {
                                                    format!("tabbed-toolbar-active-{id}")
                                                })
                                                .absolute()
                                                .bottom_0()
                                                .left_0()
                                                .w_full()
                                                .h(px(2.))
                                                .bg(accent),
                                        )
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |toolbar, _, window, cx| {
                                            toolbar.select_id(id, window, cx);
                                        }),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Middle,
                                        cx.listener(move |toolbar, _, window, cx| {
                                            toolbar.select_id(id, window, cx);
                                        }),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Right,
                                        cx.listener(move |toolbar, _, window, cx| {
                                            toolbar.select_id(id, window, cx);
                                        }),
                                    )
                                    .when(tab.closed.is_some(), |this| {
                                        this.on_aux_click(cx.listener(
                                            move |toolbar, event: &gpui::ClickEvent, window, cx| {
                                                if !event.is_middle_click() {
                                                    return;
                                                }
                                                if let Some(closed) = toolbar
                                                    .tabs
                                                    .iter()
                                                    .find(|tab| tab.id == id)
                                                    .and_then(|tab| tab.closed.clone())
                                                {
                                                    window.defer(cx, move |window, cx| {
                                                        closed(window, cx)
                                                    });
                                                }
                                            },
                                        ))
                                    })
                                    .on_key_down(cx.listener(
                                        move |toolbar, event: &gpui::KeyDownEvent, window, cx| {
                                            if event.keystroke.key == "space"
                                                && toolbar.tabs.iter().any(|tab| {
                                                    tab.id == id && tab.focus.is_focused(window)
                                                })
                                            {
                                                cx.stop_propagation();
                                                toolbar.select_id(id, window, cx);
                                            }
                                        },
                                    ))
                            }))
                            .child(
                                canvas(
                                    move |_, window, _| {
                                        let offset = scroll.offset().x;
                                        let maximum = scroll.max_offset().x;
                                        let left = offset < px(0.);
                                        let right = -offset < maximum;
                                        if (left, right) == previous_visibility {
                                            return;
                                        }
                                        let toolbar = toolbar.clone();
                                        let scroll = scroll.clone();
                                        // Layout can clamp the offset or change overflow. Notify
                                        // after the frame, when invalidation cannot be consumed
                                        // by the frame that measured these bounds.
                                        window.on_next_frame(move |_, cx| {
                                            let offset = scroll.offset().x;
                                            let left = offset < px(0.);
                                            let right = -offset < scroll.max_offset().x;
                                            if let Some(toolbar) = toolbar.upgrade() {
                                                toolbar.update(cx, |toolbar, cx| {
                                                    if toolbar.scroll_left_visible != left
                                                        || toolbar.scroll_right_visible != right
                                                    {
                                                        toolbar.scroll_left_visible = left;
                                                        toolbar.scroll_right_visible = right;
                                                        cx.notify();
                                                    }
                                                });
                                            }
                                        });
                                    },
                                    |_, _, _, _| {},
                                )
                                .absolute()
                                .size_full(),
                            ),
                    )
                    .when(self.scroll_right_visible, |this| {
                        this.child(
                            div()
                                .flex_none()
                                .debug_selector(|| "tabbed-toolbar-scroll-right".into())
                                .on_mouse_down(MouseButton::Left, {
                                    let focus = self.scroll_right_focus.clone();
                                    move |_, window, cx| focus.focus(window, cx)
                                })
                                .child(
                                    ButtonLike::new("tabbed-toolbar-scroll-right")
                                        .height(px(24.).into())
                                        .aria_label("Scroll tabs right")
                                        .tooltip(Tooltip::text("Scroll tabs right"))
                                        .tab_index(0isize)
                                        .track_focus(&self.scroll_right_focus)
                                        .child(Label::new("→"))
                                        .on_click(cx.listener(|toolbar, _, _, cx| {
                                            toolbar.scroll_by(px(30.), cx)
                                        })),
                                ),
                        )
                    }),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(5.))
                    .px(px(5.))
                    .py(px(5.))
                    .children(self.actions.iter().enumerate().map(|(index, action)| {
                        let clicked = action.clicked.clone();
                        div()
                            .debug_selector(move || format!("tabbed-toolbar-action-{index}"))
                            .on_mouse_down(MouseButton::Left, {
                                let focus = action.focus.clone();
                                move |_, window, cx| focus.focus(window, cx)
                            })
                            .child(
                                ButtonLike::new(("tabbed-toolbar-action", index))
                                    .height(px(24.).into())
                                    .aria_label(action.label.clone())
                                    .tooltip(Tooltip::text(action.label.clone()))
                                    .tab_index(0isize)
                                    .track_focus(&action.focus)
                                    .child(action.icon.clone())
                                    .on_click(move |_, window, cx| clicked(window, cx)),
                            )
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, TestAppContext, VisualTestContext, size};
    use std::cell::{Cell, RefCell};

    struct TestTitle(&'static str);
    impl Render for TestTitle {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .debug_selector(|| "toolbar-test-title".into())
                .child(Label::new(self.0))
        }
    }

    fn toolbar(cx: &mut TestAppContext) -> (Entity<TabbedToolbar>, &mut VisualTestContext) {
        toolbar_with_title("Test", cx)
    }

    fn toolbar_with_title<'a>(
        title: &'static str,
        cx: &'a mut TestAppContext,
    ) -> (Entity<TabbedToolbar>, &'a mut VisualTestContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
        });
        let title = cx.new(|_| TestTitle(title));
        cx.add_window_view(|_, cx| TabbedToolbar::new(title, cx))
    }

    // Test windows have no platform frame loop. Deliver the same post-frame
    // callbacks that the native app runs, and fail if geometry never settles.
    fn settle_frames(cx: &mut VisualTestContext) {
        let mut idle_frames = 0;
        for _ in 0..8 {
            let callbacks = cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
            if callbacks == 0 {
                // The App can draw after the update closure returns and queue
                // new callbacks. Require a second idle delivery to observe them.
                idle_frames += 1;
                if idle_frames == 2 {
                    return;
                }
            } else {
                idle_frames = 0;
            }
        }
        panic!("Toolbar geometry did not settle within eight native frame deliveries");
    }

    // Ported from the seven methods of pinned AOSP TabbedToolbarTest.kt.
    // Each window renders the production toolbar; callbacks run through actual
    // GPUI input dispatch or the production select_tab API, not a state replica.
    #[gpui::test]
    fn component_is_added_element(cx: &mut TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
        });
        let component = cx.new(|_| TestTitle("Test"));
        let component_id = component.entity_id();
        let (toolbar, cx) = cx.add_window_view(|_, cx| TabbedToolbar::new(component, cx));
        cx.run_until_parked();
        assert_eq!(
            toolbar.read_with(cx, |toolbar, _| toolbar.title.entity_id()),
            component_id
        );
        assert!(cx.debug_bounds("toolbar-test-title").is_some());
    }

    #[gpui::test]
    fn tab_is_added(cx: &mut TestAppContext) {
        let (toolbar, cx) = toolbar(cx);
        toolbar.update(cx, |toolbar, cx| {
            toolbar.add_tab("First", cx, |_, _| {}, None)
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("tabbed-toolbar-label-First").is_some());
        assert_eq!(
            toolbar.read_with(cx, |toolbar, _| toolbar
                .tabs
                .first()
                .map(|tab| tab.label.clone())),
            Some("First".into())
        );
    }

    #[gpui::test]
    fn closed_is_called_when_clicked(cx: &mut TestAppContext) {
        let (toolbar, cx) = toolbar(cx);
        let closed = Rc::new(Cell::new(false));
        toolbar.update(cx, |toolbar, cx| {
            toolbar.add_tab(
                "First",
                cx,
                |_, _| {},
                Some({
                    let closed = closed.clone();
                    Rc::new(move |_, _| closed.set(true))
                }),
            )
        });
        cx.run_until_parked();
        let close = cx
            .debug_bounds("tabbed-toolbar-close-0")
            .expect("Rendered close button");
        cx.simulate_click(close.center(), Default::default());
        assert!(closed.get());
    }

    #[gpui::test]
    fn no_close_button_when_no_listener(cx: &mut TestAppContext) {
        let (toolbar, cx) = toolbar(cx);
        toolbar.update(cx, |toolbar, cx| {
            toolbar.add_tab("First", cx, |_, _| {}, None)
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("tabbed-toolbar-close-0").is_none());
        assert!(toolbar.read_with(cx, |toolbar, _| {
            toolbar.tabs.iter().all(|tab| tab.closed.is_none())
        }));
    }

    #[gpui::test]
    fn icon_buttons_callback_when_clicked(cx: &mut TestAppContext) {
        let (toolbar, cx) = toolbar(cx);
        let clicked = Rc::new(Cell::new(false));
        toolbar.update(cx, |toolbar, cx| {
            toolbar.add_action(
                Icon::from_path("icons/android-studio-add.svg"),
                "Add",
                cx,
                {
                    let clicked = clicked.clone();
                    move |_, _| clicked.set(true)
                },
            )
        });
        cx.run_until_parked();
        let action = cx
            .debug_bounds("tabbed-toolbar-action-0")
            .expect("Rendered action button");
        cx.simulate_click(action.center(), Default::default());
        assert!(clicked.get());
    }

    #[gpui::test]
    fn can_select_tab_by_index(cx: &mut TestAppContext) {
        let (toolbar, cx) = toolbar_with_title("Title", cx);
        let selected = Rc::new(RefCell::new("nope"));
        toolbar.update(cx, |toolbar, cx| {
            for name in ["First", "Second", "Third"] {
                let selected = selected.clone();
                toolbar.add_tab(name, cx, move |_, _| *selected.borrow_mut() = name, None);
            }
        });
        cx.run_until_parked();
        assert_eq!(toolbar.read_with(cx, |toolbar, _| toolbar.count_tabs()), 3);
        toolbar.update_in(cx, |toolbar, window, cx| {
            toolbar.select_tab(1, window, cx).expect("Second tab")
        });
        cx.run_until_parked();
        assert_eq!(*selected.borrow(), "Second");
        toolbar.update(cx, |toolbar, cx| {
            toolbar.clear_tabs(cx);
            for name in ["Fourth", "Fifth"] {
                let selected = selected.clone();
                toolbar.add_tab(name, cx, move |_, _| *selected.borrow_mut() = name, None);
            }
        });
        cx.run_until_parked();
        assert_eq!(toolbar.read_with(cx, |toolbar, _| toolbar.count_tabs()), 2);
        assert!(cx.debug_bounds("tabbed-toolbar-label-First").is_none());
        assert!(cx.debug_bounds("tabbed-toolbar-label-Fifth").is_some());
        toolbar.update_in(cx, |toolbar, window, cx| {
            toolbar.select_tab(1, window, cx).expect("Fifth tab")
        });
        cx.run_until_parked();
        assert_eq!(*selected.borrow(), "Fifth");
    }

    #[gpui::test]
    fn adding_tab_should_not_trigger_select_listener(cx: &mut TestAppContext) {
        let (toolbar, cx) = toolbar_with_title("Title", cx);
        let selected = Rc::new(RefCell::new("nope"));
        toolbar.update(cx, |toolbar, cx| {
            toolbar.add_tab(
                "First",
                cx,
                {
                    let selected = selected.clone();
                    move |_, _| *selected.borrow_mut() = "First"
                },
                None,
            )
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("tabbed-toolbar-label-First").is_some());
        assert_eq!(*selected.borrow(), "nope");
    }

    #[gpui::test]
    fn pointer_space_middle_close_and_invalid_selection_use_production_callbacks(
        cx: &mut TestAppContext,
    ) {
        let (toolbar, cx) = toolbar(cx);
        let selected = Rc::new(Cell::new(0));
        let closed = Rc::new(Cell::new(0));
        toolbar.update(cx, |toolbar, cx| {
            toolbar.add_tab(
                "First",
                cx,
                {
                    let selected = selected.clone();
                    move |_, _| selected.set(selected.get() + 1)
                },
                Some({
                    let closed = closed.clone();
                    Rc::new(move |_, _| closed.set(closed.get() + 1))
                }),
            )
        });
        cx.run_until_parked();
        let tab = cx
            .debug_bounds("tabbed-toolbar-label-First")
            .expect("Tab text");
        cx.simulate_click(tab.center(), Default::default());
        cx.run_until_parked();
        assert_eq!(selected.get(), 1);
        cx.simulate_keystrokes("space");
        cx.run_until_parked();
        assert_eq!(selected.get(), 2);
        cx.run_until_parked();
        let active = cx
            .debug_bounds("tabbed-toolbar-active-0")
            .expect("Selected underline");
        assert_eq!(active.size.height, px(2.));
        let close = cx
            .debug_bounds("tabbed-toolbar-close-0")
            .expect("Close button");
        cx.simulate_click(close.center(), Default::default());
        assert_eq!(closed.get(), 1);
        assert_eq!(selected.get(), 2, "Close button must not select its tab");
        cx.simulate_keystrokes("space");
        // simulate_keystrokes emits only KeyDown; native buttons activate on
        // release, so exercise the complete physical key press here.
        cx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("space").expect("Space key"),
        });
        assert_eq!(closed.get(), 2, "Focused close supports Space activation");
        assert_eq!(selected.get(), 2, "Close Space must not select its tab");
        let title = cx.debug_bounds("toolbar-test-title").expect("Title");
        cx.simulate_mouse_down(title.center(), MouseButton::Middle, Default::default());
        cx.simulate_mouse_up(tab.center(), MouseButton::Middle, Default::default());
        assert_eq!(closed.get(), 2, "An outside press is not a tab click");
        assert_eq!(selected.get(), 2);
        cx.simulate_mouse_down(tab.center(), MouseButton::Middle, Default::default());
        cx.simulate_mouse_up(tab.center(), MouseButton::Middle, Default::default());
        cx.run_until_parked();
        assert_eq!(closed.get(), 3);
        assert_eq!(selected.get(), 3);
        toolbar.update_in(cx, |toolbar, window, cx| {
            assert!(toolbar.select_tab(1, window, cx).is_err());
            assert!(toolbar.select_tab(usize::MAX, window, cx).is_err());
            toolbar.clear_tabs(cx);
            assert_eq!(toolbar.count_tabs(), 0);
            assert!(toolbar.select_tab(0, window, cx).is_err());
        });
        cx.run_until_parked();
        assert_eq!(selected.get(), 3);
        assert!(cx.debug_bounds("tabbed-toolbar-active-0").is_none());
        assert!(cx.debug_bounds("tabbed-toolbar-label-First").is_none());
    }

    #[gpui::test]
    fn callbacks_can_rebuild_toolbar_after_selection_and_middle_close(cx: &mut TestAppContext) {
        let (toolbar, cx) = toolbar(cx);
        let selected = Rc::new(Cell::new(0));
        let closed = Rc::new(Cell::new(0));
        toolbar.update(cx, |toolbar, cx| {
            let owner = cx.entity().downgrade();
            toolbar.add_tab(
                "Select and rebuild",
                cx,
                {
                    let selected = selected.clone();
                    let closed = closed.clone();
                    move |_, cx| {
                        selected.set(selected.get() + 1);
                        let owner = owner.upgrade().expect("Toolbar alive");
                        owner.update(cx, |toolbar, cx| {
                            toolbar.clear_tabs(cx);
                            toolbar.add_tab(
                                "Middle and rebuild",
                                cx,
                                {
                                    let owner = owner.downgrade();
                                    move |_, cx| {
                                        owner.upgrade().expect("Toolbar alive").update(
                                            cx,
                                            |toolbar, cx| {
                                                toolbar.set_tab_icon(0, None, cx).expect("Tab")
                                            },
                                        );
                                    }
                                },
                                Some({
                                    let owner = owner.downgrade();
                                    let closed = closed.clone();
                                    Rc::new(move |_, cx| {
                                        closed.set(closed.get() + 1);
                                        owner.upgrade().expect("Toolbar alive").update(
                                            cx,
                                            |toolbar, cx| {
                                                toolbar.clear_tabs(cx);
                                                toolbar.add_tab("Rebuilt", cx, |_, _| {}, None);
                                            },
                                        );
                                    })
                                }),
                            );
                        });
                    }
                },
                None,
            );
        });
        cx.run_until_parked();
        let select = cx
            .debug_bounds("tabbed-toolbar-label-Select and rebuild")
            .expect("Original tab");
        cx.simulate_click(select.center(), Default::default());
        cx.run_until_parked();
        assert_eq!(selected.get(), 1);
        let middle = cx
            .debug_bounds("tabbed-toolbar-label-Middle and rebuild")
            .expect("Replacement tab");
        cx.simulate_mouse_down(middle.center(), MouseButton::Middle, Default::default());
        cx.simulate_mouse_up(middle.center(), MouseButton::Middle, Default::default());
        cx.run_until_parked();
        assert_eq!(closed.get(), 1);
        assert_eq!(toolbar.read_with(cx, |toolbar, _| toolbar.count_tabs()), 1);
        assert!(cx.debug_bounds("tabbed-toolbar-label-Rebuilt").is_some());
    }

    #[gpui::test]
    fn title_tabs_and_multiple_actions_retain_reference_geometry(cx: &mut TestAppContext) {
        let (toolbar, cx) = toolbar(cx);
        toolbar.update(cx, |toolbar, cx| {
            toolbar.add_tab("First", cx, |_, _| {}, Some(Rc::new(|_, _| {})));
            for label in ["Add", "Another action"] {
                toolbar.add_action(
                    Icon::from_path("icons/android-studio-add.svg"),
                    label,
                    cx,
                    |_, _| {},
                );
            }
        });
        cx.run_until_parked();
        let label = cx.debug_bounds("tabbed-toolbar-label-First").expect("Text");
        let close = cx.debug_bounds("tabbed-toolbar-close-0").expect("Close");
        assert_eq!(close.size, size(px(24.), px(24.)));
        assert_eq!(close.left(), label.right(), "BorderLayout has no gap");
        let first = cx.debug_bounds("tabbed-toolbar-action-0").expect("Action");
        let second = cx.debug_bounds("tabbed-toolbar-action-1").expect("Action");
        assert_eq!(first.size.height, px(24.));
        assert_eq!(second.left() - first.right(), px(5.));
        let bounds = cx.debug_bounds("tabbed-toolbar").expect("Toolbar");
        assert_eq!(bounds.right() - second.right(), px(5.));
        let title = cx.debug_bounds("toolbar-test-title").expect("Title");
        let viewport = cx.debug_bounds("tabbed-toolbar-tabs").expect("Tabs");
        assert_eq!(viewport.left() - title.right(), px(5.));
    }

    #[gpui::test]
    fn overflow_scrolls_and_resizing_retains_title_actions_and_tabs(cx: &mut TestAppContext) {
        let (toolbar, cx) = toolbar(cx);
        toolbar.update(cx, |toolbar, cx| {
            for name in [
                "First", "Second", "Third", "Fourth", "Fifth", "Sixth", "Seventh",
            ] {
                toolbar.add_tab(name, cx, |_, _| {}, None);
            }
            toolbar.add_action(
                Icon::from_path("icons/android-studio-add.svg"),
                "Add",
                cx,
                |_, _| {},
            );
        });
        cx.simulate_resize(size(px(250.), px(100.)));
        settle_frames(cx);
        assert!(cx.debug_bounds("toolbar-test-title").is_some());
        assert!(cx.debug_bounds("tabbed-toolbar-action-0").is_some());
        toolbar.update(cx, |toolbar, cx| toolbar.scroll_by(px(-10_000.), cx));
        settle_frames(cx);
        let first_offset = toolbar.read_with(cx, |toolbar, _| toolbar.scroll.offset().x);
        assert_eq!(first_offset, px(0.));
        assert!(
            cx.debug_bounds("tabbed-toolbar-scroll-left").is_none(),
            "Left arrow at start; state {:?}",
            toolbar.read_with(cx, |toolbar, _| (
                toolbar.scroll.offset(),
                toolbar.scroll.max_offset(),
                toolbar.scroll_left_visible,
                toolbar.scroll_right_visible,
            ))
        );
        let right = cx
            .debug_bounds("tabbed-toolbar-scroll-right")
            .expect("Right arrow when tabs overflow");
        cx.simulate_click(right.center(), Default::default());
        settle_frames(cx);
        assert_eq!(
            toolbar.read_with(cx, |toolbar, _| toolbar.scroll.offset().x),
            px(-30.)
        );
        let left = cx
            .debug_bounds("tabbed-toolbar-scroll-left")
            .expect("Left arrow after scrolling");
        cx.simulate_click(left.center(), Default::default());
        settle_frames(cx);
        assert_eq!(
            toolbar.read_with(cx, |toolbar, _| toolbar.scroll.offset().x),
            px(0.)
        );
        let viewport = cx
            .debug_bounds("tabbed-toolbar-tabs")
            .expect("Scrollable tabs");
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: viewport.center(),
            delta: gpui::ScrollDelta::Pixels(point(px(-60.), px(0.))),
            ..Default::default()
        });
        settle_frames(cx);
        assert!(toolbar.read_with(cx, |toolbar, _| toolbar.scroll.offset().x < px(0.)));
        toolbar.update(cx, |toolbar, cx| toolbar.scroll_by(px(10_000.), cx));
        settle_frames(cx);
        let last = cx
            .debug_bounds("tabbed-toolbar-label-Seventh")
            .expect("Last tab");
        let viewport = cx.debug_bounds("tabbed-toolbar-tabs").expect("Viewport");
        assert!(last.center().x <= viewport.right());
        assert!(cx.debug_bounds("tabbed-toolbar-scroll-right").is_none());
        cx.simulate_resize(size(px(1000.), px(100.)));
        settle_frames(cx);
        assert!(cx.debug_bounds("tabbed-toolbar-action-0").is_some());
        assert!(toolbar.read_with(cx, |toolbar, _| toolbar.scroll.max_offset().x <= px(0.)));
        assert!(
            cx.debug_bounds("tabbed-toolbar-scroll-left").is_none(),
            "Left arrow after resize; state {:?}",
            toolbar.read_with(cx, |toolbar, _| (
                toolbar.scroll.offset(),
                toolbar.scroll.max_offset(),
                toolbar.scroll_left_visible,
                toolbar.scroll_right_visible,
            ))
        );
        assert!(cx.debug_bounds("tabbed-toolbar-scroll-right").is_none());
    }
}
