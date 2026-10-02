use super::tests::viewer;
use super::*;
use gpui::TestAppContext;

fn entry(tag: &str, message: &str) -> Entry {
    let mut entry = logcat::import("2026-10-01 12:00:00.000 42 43 I Fixture: message")
        .expect("Valid fixture")
        .remove(0);
    entry.tag = tag.into();
    entry.message = message.into();
    entry
}

#[gpui::test]
async fn pending_find_matches_are_recorded_during_bounded_capture(cx: &mut TestAppContext) {
    let (_, view, cx) = viewer(cx, false).await;
    view.update_in(cx, |view, _, cx| {
        view.query = Query::default();
        view.receive(vec![entry("Tag", "needle initial")], cx);
        let initial = view.visible[0].id;
        view.pending_search = logcat::Search::new("needle", false, false).expect("Find");
        view.receive(vec![entry("Tag", "needle tail"), entry("Tag", "other")], cx);
        assert_eq!(view.search_tail, vec![view.visible[1].id]);
        view.apply_search(
            logcat::Search::new("needle", false, false),
            Some(vec![initial]),
            cx,
        );
        assert_eq!(view.search_matches, vec![0, 1]);
        assert!(view.pending_search.is_none());
        assert!(view.search_tail.is_empty());
    });
}

#[gpui::test]
async fn source_links_resolve_in_background_and_are_cached_until_selection_changes(
    cx: &mut TestAppContext,
) {
    let (_, view, cx) = viewer(cx, false).await;
    let filesystem = view.read_with(cx, |view, cx| view.project.read(cx).fs().as_fake());
    let files = (0..30)
        .map(|index| {
            (
                format!("File{index}.kt"),
                serde_json::Value::String(String::new()),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    filesystem
        .insert_tree("/logcat/src", serde_json::Value::Object(files))
        .await;
    cx.run_until_parked();
    let message = (0..30)
        .map(|index| format!("at pkg.Class.method(File{index}.kt:42)\n"))
        .collect::<String>();
    view.update_in(cx, |view, _, cx| {
        view.query = Query::default();
        view.receive(
            vec![entry("Trace", &message), entry("Tag", "ordinary message")],
            cx,
        );
        let entry = view.visible[0].clone();
        view.update_source_locations(&entry, cx);
        assert!(view.source_task.is_some());
        assert!(view.source_locations.is_empty());
    });
    cx.run_until_parked();
    view.update_in(cx, |view, _, cx| {
        assert_eq!(view.source_locations.len(), 20);
        assert!(view.source_locations.iter().all(|(_, line)| *line == 42));
        assert!(view.source_task.is_none());
        let entry = view.visible[0].clone();
        view.update_source_locations(&entry, cx);
        assert!(
            view.source_task.is_none(),
            "Redraws reuse resolved locations"
        );
        let next = view.visible[1].clone();
        view.update_source_locations(&next, cx);
        assert!(view.source_locations.is_empty());
        assert_eq!(view.source_entry, Some(next.id));
        view.clear(cx);
        assert!(view.source_entry.is_none());
        assert!(view.source_task.is_none());
    });
    cx.run_until_parked();
    view.read_with(cx, |view, _| assert!(view.source_locations.is_empty()));
}

fn backlog() -> Vec<Entry> {
    (0..1024)
        .map(|index| {
            entry(
                if index % 2 == 0 { "First" } else { "Second" },
                &format!("retained message {index}"),
            )
        })
        .collect()
}

#[gpui::test]
async fn background_filter_preserves_matching_capture_suffix(cx: &mut TestAppContext) {
    let (_, view, cx) = viewer(cx, false).await;
    view.update_in(cx, |view, _, cx| {
        view.query = Query::default();
        view.receive(backlog(), cx);
        view.query = Query::parse("tag=:First", false).expect("Filter");
        view.rebuild(cx);
        assert!(view.rebuild_task.is_some(), "Large snapshots use a worker");
        view.receive(
            vec![
                entry("First", "matching suffix"),
                entry("Second", "hidden suffix"),
            ],
            cx,
        );
    });
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert_eq!(view.visible.len(), 513);
        assert!(view.visible.iter().all(|entry| entry.tag == "First"));
        assert_eq!(
            view.visible.last().expect("Suffix").message,
            "matching suffix"
        );
        assert!(view.visible.windows(2).all(|pair| pair[0].id < pair[1].id));
    });
}

#[gpui::test]
async fn clearing_cancels_pending_filter_without_restoring_old_rows(cx: &mut TestAppContext) {
    let (_, view, cx) = viewer(cx, false).await;
    view.update_in(cx, |view, _, cx| {
        view.query = Query::default();
        view.receive(backlog(), cx);
        view.rebuild(cx);
        assert!(view.rebuild_task.is_some());
        view.clear(cx);
        view.receive(vec![entry("Fresh", "after clear")], cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert_eq!(view.visible.len(), 1);
        assert_eq!(view.buffer.entries.len(), 1);
        assert_eq!(view.visible[0].message, "after clear");
        assert!(view.selected.is_empty());
        assert!(view.text_selection.is_none());
    });
}

#[gpui::test]
async fn pause_freezes_latest_rows_when_a_filter_is_pending(cx: &mut TestAppContext) {
    let (_, view, cx) = viewer(cx, false).await;
    view.update_in(cx, |view, _, cx| {
        view.query = Query::default();
        view.receive(backlog(), cx);
        view.rebuild(cx);
        view.receive(vec![entry("First", "before pause")], cx);
        view.pause(cx);
        view.receive(vec![entry("First", "after pause")], cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert_eq!(view.visible.len(), 1025);
        assert_eq!(
            view.visible.last().expect("Frozen suffix").message,
            "before pause"
        );
        assert_eq!(view.paused.as_ref().expect("Frozen snapshot").len(), 1025);
        assert_eq!(view.buffer.entries.len(), 1026);
    });
    view.update_in(cx, |view, _, cx| view.pause(cx));
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert!(view.paused.is_none());
        assert_eq!(view.visible.len(), 1026);
        assert_eq!(
            view.visible.last().expect("Live suffix").message,
            "after pause"
        );
    });
}

#[gpui::test]
async fn newest_typed_filter_cancels_debounced_results(cx: &mut TestAppContext) {
    let (_, view, cx) = viewer(cx, false).await;
    view.update_in(cx, |view, window, cx| {
        view.query = Query::default();
        view.receive(backlog(), cx);
        set_input_text(&view.filter_input, "tag=:First", window, cx);
        view.update_filter(cx);
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(25));
    view.update_in(cx, |view, window, cx| {
        set_input_text(&view.filter_input, "tag=:Second", window, cx);
        view.update_filter(cx);
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(25));
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert_eq!(
            view.visible.len(),
            1024,
            "Canceled first query never applies"
        );
    });
    cx.executor().advance_clock(Duration::from_millis(25));
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert_eq!(view.visible.len(), 512);
        assert!(view.visible.iter().all(|entry| entry.tag == "Second"));
        assert!(view.filter_error.is_none());
    });
    view.update_in(cx, |view, window, cx| {
        set_input_text(&view.filter_input, "tag:", window, cx);
        view.update_filter(cx);
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(50));
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert!(view.filter_error.is_some());
        assert_eq!(
            view.visible.len(),
            512,
            "Invalid text preserves last valid query"
        );
        assert!(view.visible.iter().all(|entry| entry.tag == "Second"));
    });
}

#[gpui::test]
async fn find_position_remains_valid_as_matches_are_evicted(cx: &mut TestAppContext) {
    let (_, view, cx) = viewer(cx, false).await;
    view.update_in(cx, |view, window, cx| {
        view.query = Query::default();
        let matched = entry("Tag", &format!("needle{}", "x".repeat(394)));
        let footprint = std::mem::size_of::<Entry>() + matched.tag.len() + matched.message.len();
        view.buffer = Buffer::new(3 * footprint);
        view.receive(vec![matched.clone(), matched.clone(), matched], cx);
        view.search_visible = true;
        set_input_text(&view.search_input, "needle", window, cx);
        view.update_search(cx);
        view.find(true, cx);
        assert_eq!(view.search_position, Some(2));
        let unmatched = entry("Tag", &format!("other {}", "x".repeat(394)));
        view.receive(vec![unmatched.clone(), unmatched.clone()], cx);
        assert_eq!(view.visible.len(), 3);
        assert_eq!(view.search_matches, vec![0]);
        assert!(
            view.search_position
                .is_none_or(|position| position < view.search_matches.len())
        );
        view.receive(vec![unmatched], cx);
        assert!(view.search_matches.is_empty());
        assert!(view.search_position.is_none());
    });
}

#[gpui::test]
async fn importing_saved_logs_exits_pause_and_cancels_live_snapshot(cx: &mut TestAppContext) {
    let (_, view, cx) = viewer(cx, false).await;
    let path = PathBuf::from("/logcat/saved.jsonl");
    view.update_in(cx, |view, _, cx| {
        view.query = Query::default();
        view.receive(backlog(), cx);
        view.pause(cx);
        let mut imported = Buffer::new(view.preferences.capacity);
        for index in 0..1024 {
            imported.push(entry("Saved", &format!("saved message {index}")));
        }
        view.apply_import(path.clone(), imported, cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert!(view.paused.is_none());
        assert_eq!(view.file.as_ref(), Some(&path));
        assert!(!view.capturing);
        assert_eq!(view.visible.len(), 1024);
        assert!(view.visible.iter().all(|entry| entry.tag == "Saved"));
        assert!(view.selected.is_empty());
        assert!(view.text_selection.is_none());
    });
}

#[gpui::test]
async fn background_find_merges_live_matches_and_cancels_when_closed(cx: &mut TestAppContext) {
    let (_, view, cx) = viewer(cx, false).await;
    view.update_in(cx, |view, window, cx| {
        view.query = Query::default();
        let mut entries = backlog();
        entries[0].message = "needle oldest".into();
        view.receive(entries, cx);
        view.search_visible = true;
        set_input_text(&view.search_input, "needle", window, cx);
        view.update_search(cx);
        assert!(view.search_task.is_some());
        view.receive(
            vec![
                entry("First", "needle newest"),
                entry("First", "unmatched suffix"),
            ],
            cx,
        );
    });
    cx.run_until_parked();
    view.read_with(cx, |view, _| assert_eq!(view.search_matches, vec![0, 1024]));
    view.update_in(cx, |view, window, cx| {
        set_input_text(&view.search_input, "message", window, cx);
        view.update_search(cx);
        view.close_find(window, cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert!(!view.search_visible);
        assert!(view.search_matches.is_empty());
        assert!(view.search.is_none());
        assert!(view.search_position.is_none());
    });
}

#[gpui::test]
async fn large_messages_use_workers_even_with_few_rows(cx: &mut TestAppContext) {
    let (_, view, cx) = viewer(cx, false).await;
    view.update_in(cx, |view, window, cx| {
        view.query = Query::default();
        view.receive(vec![entry("Big", &"x".repeat(64 * 1024)); 4], cx);
        assert!(!small_snapshot(view.buffer.entries.iter()));
        set_input_text(&view.filter_input, "tag:Big", window, cx);
        view.update_filter(cx);
        assert!(view.filter_task.is_some());
        view.search_visible = true;
        set_input_text(&view.search_input, "xxx", window, cx);
        view.update_search(cx);
        assert!(view.search_task.is_some());
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(50));
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert_eq!(view.visible.len(), 4);
        assert_eq!(view.search_matches, vec![0, 1, 2, 3]);
    });
}

#[gpui::test]
async fn fifty_thousand_rows_keep_painting_and_typing_virtualized(cx: &mut TestAppContext) {
    let (_, view, cx) = viewer(cx, true).await;
    view.update_in(cx, |view, _, cx| {
        view.query = Query::default();
        let mut buffer = Buffer::new(64 * 1024 * 1024);
        for index in 0..50_000 {
            buffer.push(entry(
                if index % 2 == 0 { "First" } else { "Second" },
                "Unicode 日本語 payload",
            ));
        }
        view.buffer = buffer;
        view.rebuild(cx);
        assert!(
            view.visible.is_empty(),
            "Loading publishes results from a worker"
        );
    });
    cx.run_until_parked();
    let started = Instant::now();
    super::tests::draw_view(&view, cx);
    let paint_elapsed = started.elapsed();
    view.read_with(cx, |view, _| {
        assert_eq!(view.visible.len(), 50_000);
        assert!(
            view.text_layouts.borrow().len() < 100,
            "Rendering does not shape the backlog"
        );
        assert!(
            view.list_state.max_offset_for_scrollbar().y > px(500_000.),
            "Off-screen rows contribute estimated height"
        );
    });
    view.update_in(cx, |view, window, cx| {
        window.focus(&view.filter_input.focus_handle(cx), cx);
        set_input_text(&view.filter_input, "", window, cx);
    });
    let started = Instant::now();
    let mut per_key = Vec::new();
    for character in "tag:First".chars() {
        let key_started = Instant::now();
        cx.simulate_input(&character.to_string());
        per_key.push(key_started.elapsed());
    }
    let typing_elapsed = started.elapsed();
    let handler_elapsed = view.update_in(cx, |view, window, cx| {
        let started = Instant::now();
        view.update_filter(cx);
        view.update_completions(window, cx);
        started.elapsed()
    });
    view.read_with(cx, |view, cx| {
        assert_eq!(view.filter_input.read(cx).text(cx), "tag:First");
        assert_eq!(
            view.visible.len(),
            50_000,
            "The input edit never waits for a backlog scan"
        );
        assert!(view.filter_task.is_some());
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(50));
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert_eq!(view.visible.len(), 25_000);
        assert!(view.visible.iter().all(|entry| entry.tag == "First"));
        assert!(view.rebuild_tail.is_empty());
        assert!(view.rebuild_task.is_none());
    });
    view.update_in(cx, |view, window, cx| {
        let mut buffer = Buffer::new(logcat::DEFAULT_CAPACITY);
        for _ in 0..100 {
            buffer.push(entry("First", "Unicode 日本語 payload"));
        }
        view.buffer = buffer;
        view.query = Query::default();
        view.rebuild(cx);
        set_input_text(&view.filter_input, "", window, cx);
    });
    cx.run_until_parked();
    super::tests::draw_view(&view, cx);
    let started = Instant::now();
    for character in "tag:First".chars() {
        cx.simulate_input(&character.to_string());
    }
    let small_typing_elapsed = started.elapsed();
    eprintln!(
        "Logcat 50,000 rows: initial paint {paint_elapsed:?}, filter/completion handler {handler_elapsed:?}, nine-key dispatch {typing_elapsed:?}, per key {per_key:?}; 100-row nine-key baseline {small_typing_elapsed:?}"
    );
}

#[test]
fn column_estimates_cover_formatted_ascii_and_unicode_metadata_without_allocating_lines() {
    for compact in [false, true] {
        let mut entry = entry("日本語", "first line 日本語\nlonger second line 🦀🦀🦀");
        entry.pid = u32::MAX;
        entry.tid = u32::MAX;
        entry.uid = Some(u32::MAX);
        entry.package = "package".into();
        entry.process = "package:service".into();
        let exact = display_line(&entry, compact, false)
            .text
            .lines()
            .map(str::width)
            .max()
            .expect("Line");
        assert_eq!(line_columns(&entry, compact), exact);
    }
}

#[test]
fn replaced_worker_observes_cancellation_before_scanning_retained_rows() {
    let cancellation = WorkCancellation::default();
    let token = cancellation.0.clone();
    drop(cancellation);
    let entries = backlog().into_iter().map(Arc::new).collect();
    assert!(filter_entries(entries, &Query::default(), &[], false, 0, Some(&token)).is_none());
}

#[gpui::test]
async fn filtering_drops_old_width_even_when_capture_continues(cx: &mut TestAppContext) {
    let (_, view, cx) = viewer(cx, false).await;
    view.update_in(cx, |view, _, cx| {
        view.query = Query::default();
        view.receive(backlog(), cx);
        view.receive(vec![entry("Hidden", &"x".repeat(10_000))], cx);
        assert!(view.unwrapped_columns >= 10_000);
        view.query = Query::parse("tag=:First", false).expect("Filter");
        view.rebuild(cx);
        view.receive(vec![entry("First", "short suffix")], cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert_eq!(view.visible.len(), 513);
        assert!(
            view.unwrapped_columns < 100,
            "The old query's widest line must not leak into the new view"
        );
    });
}
