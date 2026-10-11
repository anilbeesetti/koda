use super::tests::{draw_view, viewer};
use super::*;
use gpui::{ListOffset, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, TestAppContext};

fn repaint(cx: &mut gpui::VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

fn populate(view: &Entity<LogcatView>, cx: &mut gpui::VisualTestContext, message: &str) {
    view.update_in(cx, |view, _, cx| {
        view.query = Query::default();
        view.preferences.compact = true;
        view.preferences.wrap = false;
        let mut entry = logcat::import("2026-10-01 12:00:00.000 42 43 I Tag: message")
            .expect("Logcat fixture")
            .remove(0);
        entry.message = message.to_owned();
        view.receive(vec![entry; 1000], cx);
        view.list_state.pause_following_tail();
    });
    draw_view(view, cx);
    view.update_in(cx, |view, _, cx| {
        view.list_state.scroll_to(ListOffset {
            item_ix: 500,
            offset_in_item: px(0.),
        });
        cx.notify();
    });
    repaint(cx);
}

fn assert_published_layouts_are_painted(
    view: &Entity<LogcatView>,
    cx: &mut gpui::VisualTestContext,
) {
    let viewport = cx
        .debug_bounds("logcat-scrollbar-frame")
        .expect("Logcat viewport");
    view.read_with(cx, |view, _| {
        let layouts = view.text_layouts.borrow();
        assert!(!layouts.is_empty(), "Visible text must support hit testing");
        assert!(
            layouts.len() < view.visible.len() / 10,
            "Virtual-list overscan measurements must not be published"
        );
        for painted in layouts.values() {
            // bounds() used to panic for the layouts of measured overscan rows.
            assert_eq!(painted.layout.bounds(), painted.bounds);
            assert!(painted.bounds.bottom() > viewport.top());
            assert!(painted.bounds.top() < viewport.bottom());
            assert!(
                painted.layout.position_for_index(0).is_some(),
                "Every published layout completed text prepainting"
            );
        }
        assert!(
            view.visible.iter().enumerate().any(|(index, entry)| {
                (500..520).contains(&index) && layouts.contains_key(&entry.id)
            }),
            "Fixture must exercise virtualized rows in the middle of the log"
        );
    });
}

fn unicode_positions(
    view: &Entity<LogcatView>,
    cx: &mut gpui::VisualTestContext,
) -> (Point<Pixels>, Point<Pixels>) {
    let viewport = cx
        .debug_bounds("logcat-scrollbar-frame")
        .expect("Logcat viewport");
    view.read_with(cx, |view, _| {
        let layouts = view.text_layouts.borrow();
        let entry = view
            .visible
            .iter()
            .find(|entry| {
                layouts.get(&entry.id).is_some_and(|painted| {
                    painted.bounds.top() >= viewport.top()
                        && painted.bounds.bottom() <= viewport.bottom()
                })
            })
            .expect("Fully visible fixture row");
        let text = display_line(entry, true, false).text;
        let offset = text.find("日本語").expect("Unicode fixture");
        let layout = &layouts[&entry.id].layout;
        let start = layout.position_for_index(offset).expect("Selection start");
        let end = layout
            .position_for_index(offset + "日本語".len())
            .expect("Selection end");
        let inset = point(px(0.), layout.line_height() / 2.);
        (start + inset, end + inset)
    })
}

#[gpui::test]
async fn dragging_repaints_virtualized_logcat_without_hit_testing_overscan(
    cx: &mut TestAppContext,
) {
    let (_, view, cx) = viewer(cx, true).await;
    populate(&view, cx, "before 日本語 after");
    assert_published_layouts_are_painted(&view, cx);
    let (start, end) = unicode_positions(&view, cx);
    cx.simulate_event(MouseDownEvent {
        button: MouseButton::Left,
        position: start,
        ..Default::default()
    });
    repaint(cx);
    assert_published_layouts_are_painted(&view, cx);
    cx.simulate_event(MouseMoveEvent {
        position: end,
        pressed_button: Some(MouseButton::Left),
        ..Default::default()
    });
    repaint(cx);
    assert_published_layouts_are_painted(&view, cx);
    cx.simulate_event(MouseUpEvent {
        button: MouseButton::Left,
        position: end,
        ..Default::default()
    });
    repaint(cx);
    view.read_with(cx, |view, _| {
        assert_eq!(view.selected_text(), "日本語");
        assert!(!view.selecting_text);
    });
    cx.simulate_keystrokes("cmd-c");
    assert_eq!(
        cx.read_from_clipboard().expect("Copied selection").text(),
        Some("日本語".into())
    );
}

#[gpui::test]
async fn dragging_unicode_uses_fresh_layouts_after_horizontal_scroll_and_resize(
    cx: &mut TestAppContext,
) {
    let (_, view, cx) = viewer(cx, true).await;
    let message = format!("{}日本語{}", "prefix ".repeat(80), " suffix".repeat(40));
    populate(&view, cx, &message);
    let (start, _) = unicode_positions(&view, cx);
    let viewport = cx
        .debug_bounds("logcat-scrollbar-frame")
        .expect("Logcat viewport");
    let target_x = viewport.right() - px(160.);
    view.update_in(cx, |view, _, cx| {
        view.horizontal_scroll
            .set_offset(point(target_x - start.x, px(0.)));
        cx.notify();
    });
    repaint(cx);
    assert_published_layouts_are_painted(&view, cx);
    let (start, end) = unicode_positions(&view, cx);
    assert!(viewport.contains(&start));
    assert!(viewport.contains(&end));
    view.read_with(cx, |view, _| {
        assert!(view.horizontal_scroll.offset().x < px(-100.));
    });
    cx.simulate_event(MouseDownEvent {
        button: MouseButton::Left,
        position: start,
        ..Default::default()
    });
    repaint(cx);
    cx.simulate_resize(gpui::size(px(800.), px(380.)));
    repaint(cx);
    assert_published_layouts_are_painted(&view, cx);
    let (_, end) = unicode_positions(&view, cx);
    cx.simulate_event(MouseMoveEvent {
        position: end,
        pressed_button: Some(MouseButton::Left),
        ..Default::default()
    });
    repaint(cx);
    cx.simulate_event(MouseUpEvent {
        button: MouseButton::Left,
        position: end,
        ..Default::default()
    });
    repaint(cx);
    view.read_with(cx, |view, _| {
        assert_eq!(view.selected_text(), "日本語");
        assert!(!view.selecting_text);
    });
    cx.simulate_keystrokes("cmd-c");
    assert_eq!(
        cx.read_from_clipboard().expect("Copied selection").text(),
        Some("日本語".into())
    );
}
