pub mod editor_lsp_test_context;
pub mod editor_test_context;

use std::{rc::Rc, sync::LazyLock};

pub use crate::emmet_ext::wrap_with_abbreviation;
pub use crate::rust_analyzer_ext::expand_macro_recursively;
use crate::{
    DisplayPoint, Editor, EditorMode, FoldPlaceholder, MultiBuffer, SelectionEffects, Size,
    display_map::{Block, CustomBlockId, DisplayMap, DisplayRow, DisplaySnapshot, ToDisplayPoint},
};
use collections::HashMap;
use gpui::{
    AppContext as _, Context, Entity, EntityId, Font, FontFeatures, FontStyle, FontWeight, Pixels,
    VisualTestContext, Window, font, size,
};
use multi_buffer::MultiBufferOffset;
use pretty_assertions::assert_eq;
use project::{Project, project_settings::DiagnosticSeverity};
use ui::{App, BorrowAppContext, IntoElement, px};
use util::test::{generate_marked_text, marked_text_offsets, marked_text_ranges};

#[cfg(test)]
#[ctor::ctor(unsafe)]
fn init_logger() {
    zlog::init_test();
}

pub fn test_font() -> Font {
    static TEST_FONT: LazyLock<Font> = LazyLock::new(|| {
        #[cfg(not(target_os = "windows"))]
        {
            font("Helvetica")
        }

        #[cfg(target_os = "windows")]
        {
            font("Courier New")
        }
    });

    TEST_FONT.clone()
}

// Returns a snapshot from text containing '|' character markers with the markers removed, and DisplayPoints for each one.
#[track_caller]
pub fn marked_display_snapshot(
    text: &str,
    cx: &mut gpui::App,
) -> (DisplaySnapshot, Vec<DisplayPoint>) {
    let (unmarked_text, markers) = marked_text_offsets(text);

    let font = Font {
        family: ".ZedMono".into(),
        features: FontFeatures::default(),
        fallbacks: None,
        weight: FontWeight::default(),
        style: FontStyle::default(),
    };
    let font_size: Pixels = 14usize.into();

    let buffer = MultiBuffer::build_simple(&unmarked_text, cx);
    let display_map = cx.new(|cx| {
        DisplayMap::new(
            buffer,
            font,
            font_size,
            None,
            1,
            1,
            FoldPlaceholder::test(),
            DiagnosticSeverity::Warning,
            cx,
        )
    });
    let snapshot = display_map.update(cx, |map, cx| map.snapshot(cx));
    let markers = markers
        .into_iter()
        .map(|offset| MultiBufferOffset(offset).to_display_point(&snapshot))
        .collect();

    (snapshot, markers)
}

#[track_caller]
pub fn select_ranges(
    editor: &mut Editor,
    marked_text: &str,
    window: &mut Window,
    cx: &mut Context<Editor>,
) {
    let (unmarked_text, text_ranges) = marked_text_ranges(marked_text, true);
    assert_eq!(editor.text(cx), unmarked_text);
    editor.change_selections(SelectionEffects::no_scroll(), window, cx, |s| {
        s.select_ranges(
            text_ranges
                .into_iter()
                .map(|range| MultiBufferOffset(range.start)..MultiBufferOffset(range.end)),
        )
    });
}

#[track_caller]
pub fn assert_text_with_selections(
    editor: &mut Editor,
    marked_text: &str,
    cx: &mut Context<Editor>,
) {
    let (unmarked_text, _text_ranges) = marked_text_ranges(marked_text, true);
    assert_eq!(editor.text(cx), unmarked_text, "text doesn't match");
    let actual = generate_marked_text(
        &editor.text(cx),
        &editor
            .selections
            .ranges::<MultiBufferOffset>(&editor.display_snapshot(cx))
            .into_iter()
            .map(|range| range.start.0..range.end.0)
            .collect::<Vec<_>>(),
        marked_text.contains("«"),
    );
    assert_eq!(actual, marked_text, "Selections don't match");
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn build_editor(
    buffer: Entity<MultiBuffer>,
    window: &mut Window,
    cx: &mut Context<Editor>,
) -> Editor {
    Editor::new(EditorMode::full(), buffer, None, window, cx)
}

pub(crate) fn build_editor_with_project(
    project: Entity<Project>,
    buffer: Entity<MultiBuffer>,
    window: &mut Window,
    cx: &mut Context<Editor>,
) -> Editor {
    Editor::new(EditorMode::full(), buffer, Some(project), window, cx)
}

#[derive(Default)]
struct TestBlockContent(
    HashMap<(EntityId, CustomBlockId), Rc<dyn Fn(&mut VisualTestContext) -> String>>,
);

impl gpui::Global for TestBlockContent {}

pub fn set_block_content_for_tests(
    editor: &Entity<Editor>,
    id: CustomBlockId,
    cx: &mut App,
    f: impl Fn(&mut VisualTestContext) -> String + 'static,
) {
    cx.update_default_global::<TestBlockContent, _>(|bc, _| {
        bc.0.insert((editor.entity_id(), id), Rc::new(f))
    });
}

pub fn block_content_for_tests(
    editor: &Entity<Editor>,
    id: CustomBlockId,
    cx: &mut VisualTestContext,
) -> Option<String> {
    let f = cx.update(|_, cx| {
        cx.default_global::<TestBlockContent>()
            .0
            .get(&(editor.entity_id(), id))
            .cloned()
    })?;
    Some(f(cx))
}

pub fn editor_content_with_blocks(editor: &Entity<Editor>, cx: &mut VisualTestContext) -> String {
    editor_content_with_blocks_and_width(editor, px(3000.), cx)
}

pub fn editor_content_with_blocks_and_width(
    editor: &Entity<Editor>,
    width: Pixels,
    cx: &mut VisualTestContext,
) -> String {
    editor_content_with_blocks_and_size(editor, size(width, px(3000.0)), cx)
}

pub fn editor_content_with_blocks_and_size(
    editor: &Entity<Editor>,
    draw_size: Size<Pixels>,
    cx: &mut VisualTestContext,
) -> String {
    cx.simulate_resize(draw_size);
    cx.draw(gpui::Point::default(), draw_size, |_, _| {
        editor.clone().into_any_element()
    });
    let (snapshot, mut lines, blocks) = editor.update_in(cx, |editor, window, cx| {
        let snapshot = editor.snapshot(window, cx);
        let text = editor.display_text(cx);
        let lines = text.lines().map(|s| s.to_string()).collect::<Vec<String>>();
        let blocks = snapshot
            .blocks_in_range(DisplayRow(0)..snapshot.max_point().row())
            .map(|(row, block)| (row, block.clone()))
            .collect::<Vec<_>>();
        (snapshot, lines, blocks)
    });
    for (row, block) in blocks {
        match block {
            Block::Custom(custom_block) => {
                let content = block_content_for_tests(editor, custom_block.id, cx)
                    .expect("block content not found");
                // 2: "related info 1 for diagnostic 0"
                if let Some(height) = custom_block.height {
                    if height == 0 {
                        lines[row.0 as usize - 1].push_str(" § ");
                        lines[row.0 as usize - 1].push_str(&content);
                    } else {
                        let block_lines = content.lines().collect::<Vec<_>>();
                        assert_eq!(block_lines.len(), height as usize);
                        lines[row.0 as usize].push_str("§ ");
                        lines[row.0 as usize].push_str(block_lines[0].trim_end());
                        for i in 1..height as usize {
                            if row.0 as usize + i >= lines.len() {
                                lines.push("".to_string());
                            };
                            lines[row.0 as usize + i].push_str("§ ");
                            lines[row.0 as usize + i].push_str(block_lines[i].trim_end());
                        }
                    }
                }
            }
            Block::FoldedBuffer {
                first_excerpt,
                height,
            } => {
                while lines.len() <= row.0 as usize {
                    lines.push(String::new());
                }
                lines[row.0 as usize].push_str(&cx.update(|_, cx| {
                    format!(
                        "§ {}",
                        first_excerpt
                            .buffer(snapshot.buffer_snapshot())
                            .file()
                            .map(|file| file.file_name(cx))
                            .unwrap_or("<no file>")
                    )
                }));
                for row in row.0 + 1..row.0 + height {
                    while lines.len() <= row as usize {
                        lines.push(String::new());
                    }
                    lines[row as usize].push_str("§ -----");
                }
            }
            Block::ExcerptBoundary { height, .. } => {
                for row in row.0..row.0 + height {
                    while lines.len() <= row as usize {
                        lines.push(String::new());
                    }
                    lines[row as usize].push_str("§ -----");
                }
            }
            Block::BufferHeader { excerpt, height } => {
                while lines.len() <= row.0 as usize {
                    lines.push(String::new());
                }
                lines[row.0 as usize].push_str(&cx.update(|_, cx| {
                    format!(
                        "§ {}",
                        excerpt
                            .buffer(snapshot.buffer_snapshot())
                            .file()
                            .map(|file| file.file_name(cx))
                            .unwrap_or("<no file>")
                    )
                }));
                for row in row.0 + 1..row.0 + height {
                    while lines.len() <= row as usize {
                        lines.push(String::new());
                    }
                    lines[row as usize].push_str("§ -----");
                }
            }
            Block::Spacer { height, .. } => {
                for row in row.0..row.0 + height {
                    while lines.len() <= row as usize {
                        lines.push(String::new());
                    }
                    lines[row as usize].push_str("§ spacer");
                }
            }
        }
    }
    lines.join("\n")
}

pub async fn run_real_kotlin_editor_probe(cx: &mut gpui::TestAppContext) {
    use crate::*;
    use collections::HashSet;
    use futures::channel::oneshot;
    use gpui::TestAppContext;
    use language::{FakeLspAdapter, LanguageConfig, LanguageMatcher};
    use serde_json::json;
    use std::io::Write as _;
    use std::time::Instant;
    use workspace::MultiWorkspace;

    let config_path = std::env::var("ZED_KOTLIN_CLIENT_PROBE_CONFIG").expect("probe config path");
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(config_path).expect("read probe config"))
            .expect("parse probe config");
    let root = std::path::PathBuf::from(config["root"].as_str().expect("fixture root"));
    let source_path = root.join(config["source"].as_str().expect("relative source path"));
    let output = std::path::PathBuf::from(config["output"].as_str().expect("output directory"));
    std::fs::create_dir_all(&output).expect("create probe output");
    let original_disk_text = std::fs::read_to_string(&source_path).expect("read fixture source");
    let warm_samples = config["warm_samples"].as_u64().unwrap_or(30);
    let completion_warm_samples = config["completion_warm_samples"]
        .as_u64()
        .unwrap_or(warm_samples);
    let completion_refinement = config["completion_refinement"] == true;
    let automatic_first = config["automatic_first"] == true;
    let started = Instant::now();
    let mut report = json!({
        "harness": "real Kotlin subprocess, RealFs, GPUI test window",
        "debug_assertions": cfg!(debug_assertions), "editor_unit_test_build": cfg!(test),
        "root": root, "source": source_path, "client_pid": std::process::id(),
        "samples": [], "memory": [], "phase": "starting",
        "completion_preparation": if completion_refinement { "delete and retype only the prefix using Editor actions" } else { "replace whole function before typing each prefix" },
        "warm_samples": warm_samples, "completion_warm_samples": completion_warm_samples,
        "request_order": ["definition_after_edit", "definition_unchanged",
            if automatic_first { "string_triggered" } else { "string_explicit" },
            if automatic_first { "string_explicit" } else { "string_triggered" },
            "string_prefix", "modifier", "named_argument", "acceptance"],
    });
    let wire_log = Arc::new(Mutex::new(Vec::new()));
    let write_report = |report: &serde_json::Value| {
        std::fs::write(
            output.join("results.json"),
            serde_json::to_vec_pretty(report).expect("serialize report"),
        )
        .expect("write report");
        let mut wire = std::io::BufWriter::new(
            std::fs::File::create(output.join("wire.jsonl")).expect("wire log"),
        );
        for entry in wire_log.lock().iter() {
            writeln!(wire, "{entry}").expect("write wire log");
        }
        wire.flush().expect("flush wire log");
    };
    write_report(&report);

    // External subprocess IO must park in real time; advancing the deterministic
    // scheduler to its next timer would make LSP requests expire immediately.
    cx.executor().allow_parking();
    cx.update_global::<SettingsStore, _>(|store, cx| {
        store
            .set_user_settings(
                &json!({
                    "show_completions_on_input": false,
                    "languages": {"Kotlin": {"language_servers": ["kotlin-lsp"]}},
                    "lsp": {"kotlin-lsp": {
                        "binary": config["binary"],
                        "initialization_options": config["initialization_options"],
                    }},
                })
                .to_string(),
                cx,
            )
            .expect("explicit real server settings");
    });
    let fs = fs::RealFs::new(None, cx.executor());
    let app_state = cx.update(|cx| {
        let mut state = workspace::AppState::test(cx);
        Arc::get_mut(&mut state)
            .expect("unshared test app state")
            .fs = fs.clone();
        <dyn fs::Fs>::set_global(fs.clone(), cx);
        workspace::init(state.clone(), cx);
        state
    });
    let project = Project::test(fs.clone(), [root.as_path()], cx).await;
    let register_adapter = |project: &Entity<Project>, cx: &mut TestAppContext| {
        let registry = project.read_with(cx, |project, _| project.languages().clone());
        if let Some(extension) = config["kotlin_extension"].as_str() {
            let extension = std::path::PathBuf::from(extension);
            let language_path = extension.join("languages/kotlin");
            let language_config = LanguageConfig::from_toml(
                &std::fs::read_to_string(language_path.join("config.toml")).expect("Kotlin config"),
            )
            .expect("parse Kotlin config");
            let query_files = std::fs::read_dir(&language_path)
                .expect("Kotlin queries")
                .filter_map(|entry| {
                    let path = entry.expect("query entry").path();
                    let query = path
                        .file_name()?
                        .to_str()?
                        .parse::<language::QueryFile>()
                        .ok()?;
                    Some((query, std::fs::read_to_string(path).expect("read query")))
                })
                .collect::<Vec<_>>();
            registry.register_wasm_grammars(vec![(
                "kotlin".into(),
                extension.join("grammars/kotlin.wasm"),
            )]);
            registry.register_language(
                language_config.name.clone(),
                Some("kotlin".into()),
                language_config.matcher.clone(),
                false,
                None,
                Arc::new(move || {
                    let config = language_config.clone();
                    let queries = language::LanguageQueries::from_files(query_files.iter().map(
                        |(query, contents)| {
                            language::QueryFileContents::new(
                                *query,
                                std::borrow::Cow::Owned(contents.clone()),
                            )
                        },
                    ));
                    Box::pin(async move {
                        Ok(language::LoadedLanguage {
                            config,
                            queries,
                            context_provider: None,
                            toolchain_provider: None,
                            manifest_name: None,
                        })
                    })
                }),
            );
        } else {
            registry.add(Arc::new(language::Language::new(
                LanguageConfig {
                    name: "Kotlin".into(),
                    matcher: LanguageMatcher {
                        path_suffixes: vec!["kt".into()],
                        ..Default::default()
                    }
                    .into(),
                    ..Default::default()
                },
                None,
            )));
        }
        registry.register_fake_lsp_adapter(
            "Kotlin",
            FakeLspAdapter {
                name: "kotlin-lsp",
                language_server_binary: lsp::LanguageServerBinary {
                    path: config["binary"]["path"]
                        .as_str()
                        .expect("server path")
                        .into(),
                    arguments: config["binary"]["arguments"]
                        .as_array()
                        .expect("server arguments")
                        .iter()
                        .map(|argument| argument.as_str().expect("argument string").into())
                        .collect(),
                    env: Some(
                        serde_json::from_value(config["binary"]["env"].clone())
                            .expect("server environment"),
                    ),
                },
                initialization_options: Some(config["initialization_options"].clone()),
                ..Default::default()
            },
        );
    };
    register_adapter(&project, cx);
    let buffer = project
        .update(cx, |project, cx| {
            project.open_local_buffer(&source_path, cx)
        })
        .await
        .expect("open real source buffer");
    let _buffer_registration = project.update(cx, |project, cx| {
        project.register_buffer_with_language_servers(&buffer, cx)
    });
    let lsp_store = project.read_with(cx, |project, _| project.lsp_store());
    kotlin_live_wait(&lsp_store, cx, |store, _| {
        store
            .language_server_statuses()
            .any(|(id, _)| store.language_server_for_id(id).is_some())
    })
    .await;
    let server = lsp_store
        .read_with(cx, |store, _| {
            store
                .language_server_statuses()
                .find_map(|(id, _)| store.language_server_for_id(id))
        })
        .expect("running real language server");
    report["server_pid"] = json!(server.process_id());
    report["initialize_milliseconds"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    report["phase"] = json!("importing");
    write_report(&report);
    let (ready, readiness) = oneshot::channel();
    let mut ready = Some(ready);
    let mut import_finished = false;
    let mut saw_indexing = false;
    let mut indexing = HashSet::default();
    let wire_log = wire_log.clone();
    let _io = server.on_io(move |kind, message| {
        wire_log.lock().push(json!({"seconds": started.elapsed().as_secs_f64(), "kind": format!("{kind:?}"), "message": message}));
        if ready.is_none() {
            return;
        }
        let Ok(message) = serde_json::from_str::<serde_json::Value>(message) else { return; };
        if !matches!(kind, lsp::IoKind::StdOut) { return; }
        let params = &message["params"];
        match message["method"].as_str() {
            Some("intellij/importLog") => {
                if params["failed"] == true {
                    if let Some(ready) = ready.take() {
                        ready.send(Err(format!("project import failed: {params}"))).expect("readiness receiver");
                    }
                }
                if params["succeeded"] == true { import_finished = true; }
            }
            Some("$/progress") => {
                if params["value"]["kind"] == "begin" && params["value"]["title"] == "Indexing" {
                    indexing.insert(params["token"].to_string());
                    saw_indexing = true;
                } else if params["value"]["kind"] == "end" {
                    indexing.remove(&params["token"].to_string());
                }
            }
            _ => {}
        }
        if import_finished && saw_indexing && indexing.is_empty() {
            if let Some(ready) = ready.take() { ready.send(Ok(())).expect("readiness receiver"); }
        }
    });
    let (multi_workspace, cx) =
        cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
    let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
    let editor = workspace.update_in(cx, |workspace, window, cx| {
        workspace.open_project_item::<Editor>(
            None,
            buffer.clone(),
            true,
            true,
            true,
            false,
            window,
            cx,
        )
    });
    editor.update_in(cx, |editor, window, cx| {
        editor.word_completions_enabled = false;
        window.activate_window();
        window.focus(&editor.focus_handle(cx), cx);
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let mut readiness = readiness.fuse();
    let readiness_started = Instant::now();
    loop {
        let heartbeat = cx.executor().timer(Duration::from_secs(1)).fuse();
        futures::pin_mut!(heartbeat);
        futures::select_biased! {
            result = readiness => {
                result.expect("readiness notification").expect("import and indexing succeeded");
                break;
            },
            _ = heartbeat => assert!(readiness_started.elapsed() < Duration::from_secs(180), "real Kotlin import timed out"),
        }
    }
    report["ready_milliseconds"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    if config["first_navigation_only"] == true {
        report["request_order"] = json!(["first_after_readiness", "warm"]);
        let symbol = config["navigation_symbol"]
            .as_str()
            .unwrap_or("headlineSmall");
        let position = MultiBufferOffset(
            original_disk_text
                .rfind(symbol)
                .expect("first navigation symbol")
                + 1,
        );
        for sample in 0..=warm_samples {
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.activate_item(&editor, true, true, window, cx);
            });
            editor.update_in(cx, |editor, window, cx| {
                editor.change_selections(SelectionEffects::no_scroll(), window, cx, |selections| {
                    selections.select_ranges([position..position])
                });
            });
            let sample_started = Instant::now();
            let sample_start_seconds = started.elapsed().as_secs_f64();
            let navigated = editor
                .update_in(cx, |editor, window, cx| {
                    editor.go_to_definition(&GoToDefinition::default(), window, cx)
                })
                .await;
            let navigation_milliseconds = sample_started.elapsed().as_secs_f64() * 1000.0;
            if !matches!(navigated, Ok(Navigated::Yes)) {
                report["failure"] = json!(format!("First-navigation probe returned {navigated:?}"));
                report["samples"]
                    .as_array_mut()
                    .expect("samples")
                    .push(json!({
                        "scenario": if sample == 0 { "first_after_readiness" } else { "warm" },
                        "sample": sample, "start_seconds": sample_start_seconds,
                        "navigation_milliseconds": navigation_milliseconds, "failed": true,
                    }));
                write_report(&report);
                panic!("{}", report["failure"]);
            }
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let milliseconds = sample_started.elapsed().as_secs_f64() * 1000.0;
            let target = workspace.read_with(cx, |workspace, cx| {
                workspace
                    .active_item_as::<Editor>(cx)
                    .expect("definition editor")
            });
            let target = target.update(cx, |target, cx| {
                let point = target
                    .selections
                    .newest::<Point>(&target.display_snapshot(cx))
                    .start;
                let snapshot = target.buffer.read(cx).snapshot(cx);
                let selected = snapshot
                    .text_for_range(
                        point..Point::new(point.row, point.column + symbol.len() as u32),
                    )
                    .collect::<String>();
                assert_eq!(
                    selected, symbol,
                    "the editor must land on the original declaration range"
                );
                let buffer = target
                    .buffer
                    .read(cx)
                    .as_singleton()
                    .expect("definition buffer");
                let buffer = buffer.read(cx);
                assert!(buffer.read_only());
                let uri = buffer
                    .language_server_document()
                    .expect("definition owner")
                    .uri
                    .clone();
                assert!(uri.as_str().ends_with(".kt"));
                uri
            });
            report["samples"].as_array_mut().expect("samples").push(json!({
                "scenario": if sample == 0 { "first_after_readiness" } else { "warm" },
                "sample": sample, "start_seconds": sample_start_seconds,
                "navigation_milliseconds": navigation_milliseconds, "milliseconds": milliseconds,
                "target": target,
            }));
        }
        report["phase"] = json!("complete");
        write_report(&report);
        lsp_store
            .update(cx, |store, cx| {
                store.stop_language_servers_for_buffers(vec![buffer], HashSet::default(), cx)
            })
            .await
            .expect("stop real language server");
        assert_eq!(
            std::fs::read_to_string(&source_path).expect("source after probe"),
            original_disk_text
        );
        return;
    }
    report["memory"]
        .as_array_mut()
        .expect("memory samples")
        .push(kotlin_live_memory("ready", server.process_id().expect("server PID")).await);
    report["phase"] = json!("editor");
    write_report(&report);
    eprintln!(
        "real Kotlin import/index ready in {} ms",
        report["ready_milliseconds"]
    );

    let source = "package liveclientprobe\nimport androidx.compose.runtime.Composable\nimport androidx.compose.ui.Modifier\nimport androidx.compose.material3.Text\nimport androidx.compose.material3.MaterialTheme\n\n";
    let mut definition_editor_id = None;
    for (scenario, replace_text) in [
        ("definition_after_edit", true),
        ("definition_unchanged", false),
    ] {
        for sample in 0..=warm_samples {
            let text = format!(
                "{source}@Composable\nprivate fun clientDefinition() {{ MaterialTheme.typography.headlineSmall }}\n"
            );
            let position =
                MultiBufferOffset(text.find("headlineSmall").expect("definition symbol") + 1);
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.activate_item(&editor, true, true, window, cx);
            });
            editor.update_in(cx, |editor, window, cx| {
                editor.hide_context_menu(window, cx);
                if replace_text {
                    editor.set_text(text, window, cx);
                }
                editor.change_selections(SelectionEffects::no_scroll(), window, cx, |selections| {
                    selections.select_ranges([position..position])
                });
            });
            let sample_started = Instant::now();
            let sample_start_seconds = started.elapsed().as_secs_f64();
            let navigated = editor
                .update_in(cx, |editor, window, cx| {
                    editor.go_to_definition(&GoToDefinition::default(), window, cx)
                })
                .await
                .expect("real definition navigation");
            assert_eq!(navigated, Navigated::Yes);
            let navigation_milliseconds = sample_started.elapsed().as_secs_f64() * 1000.0;
            let drawing_started = Instant::now();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let drawing_milliseconds = drawing_started.elapsed().as_secs_f64() * 1000.0;
            let (target, target_editor_id, editor_count) =
                workspace.read_with(cx, |workspace, cx| {
                    let target = workspace
                        .active_item_as::<Editor>(cx)
                        .expect("definition editor");
                    let buffer = target
                        .read(cx)
                        .buffer()
                        .read(cx)
                        .as_singleton()
                        .expect("definition buffer");
                    assert!(buffer.read(cx).read_only());
                    let uri = buffer
                        .read(cx)
                        .language_server_document()
                        .expect("virtual definition metadata")
                        .uri
                        .clone();
                    (
                        uri,
                        target.entity_id().as_u64(),
                        workspace.items_of_type::<Editor>(cx).count(),
                    )
                });
            assert_eq!(
                editor_count, 2,
                "library navigation must reuse the existing tab"
            );
            if let Some(previous_editor_id) = definition_editor_id {
                assert_eq!(target_editor_id, previous_editor_id);
            } else {
                definition_editor_id = Some(target_editor_id);
            }
            report["samples"]
            .as_array_mut()
            .expect("samples")
            .push(json!({
                "scenario": scenario, "sample": sample,
                "start_seconds": sample_start_seconds,
                "navigation_milliseconds": navigation_milliseconds, "drawing_milliseconds": drawing_milliseconds,
                "target_editor_id": target_editor_id, "editor_count": editor_count,
                "milliseconds": sample_started.elapsed().as_secs_f64() * 1000.0, "target": target,
            }));
            write_report(&report);
        }
    }
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.activate_item(&editor, true, true, window, cx);
    });
    let mut completion_scenarios = [
        (
            "string_explicit",
            "val greeting = \"hello\"\n greeting",
            &["."][..],
            "length",
            false,
        ),
        (
            "string_triggered",
            "val greeting = \"hello\"\n greeting",
            &["."][..],
            "length",
            true,
        ),
        (
            "string_prefix",
            "val greeting = \"hello\"\n greeting.",
            &["l", "le", "len"][..],
            "length",
            false,
        ),
        ("modifier", "Modifier.", &["p", "pa"][..], "padding", false),
        ("named_argument", "Text(", &["t", "te"][..], "text =", false),
    ];
    if automatic_first {
        completion_scenarios.swap(0, 1);
    }
    for (scenario, before, inputs, expected, triggered) in completion_scenarios {
        let mut previous_input_length = 0;
        for sample in 0..=completion_warm_samples {
            let input = inputs
                .get(sample as usize % inputs.len())
                .expect("completion prefix");
            let function_name = if completion_refinement {
                "clientProbe".to_string()
            } else {
                format!("clientProbe{sample}")
            };
            let text = format!("{source}@Composable\nprivate fun {function_name}() {{\n {before}");
            editor.update_in(cx, |editor, window, cx| {
                editor.set_show_completions_on_input(Some(false));
                editor.hide_context_menu(window, cx);
                if sample == 0 || !completion_refinement {
                    editor.set_text(text, window, cx);
                    let end = editor.buffer().read(cx).len(cx);
                    editor.change_selections(
                        SelectionEffects::no_scroll(),
                        window,
                        cx,
                        |selections| selections.select_ranges([end..end]),
                    );
                } else {
                    let end = editor.buffer().read(cx).len(cx);
                    let start = MultiBufferOffset(
                        end.0
                            .checked_sub(previous_input_length)
                            .expect("previous completion prefix"),
                    );
                    editor.change_selections(
                        SelectionEffects::no_scroll(),
                        window,
                        cx,
                        |selections| selections.select_ranges([start..end]),
                    );
                    editor.backspace(&Backspace, window, cx);
                }
            });
            let sample_started = Instant::now();
            let sample_start_seconds = started.elapsed().as_secs_f64();
            editor.update_in(cx, |editor, window, cx| {
                editor.set_show_completions_on_input(Some(triggered));
                editor.handle_input(input, window, cx);
                if !triggered {
                    editor.show_completions(&ShowCompletions, window, cx);
                }
            });
            previous_input_length = input.len();
            let input_milliseconds = sample_started.elapsed().as_secs_f64() * 1000.0;
            kotlin_live_wait(&editor, cx, |editor, _| {
                kotlin_live_completion_index(editor, expected).is_some()
            })
            .await;
            let menu_milliseconds = sample_started.elapsed().as_secs_f64() * 1000.0;
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let elapsed = sample_started.elapsed().as_secs_f64() * 1000.0;
            report["samples"].as_array_mut().expect("samples").push(json!({
                "scenario": scenario, "sample": sample, "milliseconds": elapsed,
                "prefix": input, "triggered": triggered, "start_seconds": sample_start_seconds,
                "input_milliseconds": input_milliseconds, "menu_milliseconds": menu_milliseconds,
                "rank": editor.read_with(cx, |editor, _| kotlin_live_completion_index(editor, expected)),
            }));
            write_report(&report);
        }
        eprintln!(
            "real editor completed {scenario}: first + {completion_warm_samples} warm samples"
        );
    }

    editor.update_in(cx, |editor, window, cx| {
        editor.set_show_completions_on_input(Some(false));
        editor.hide_context_menu(window, cx);
        editor.set_text(
            format!("{source}@Composable\nprivate fun clientAccept() {{\n But"),
            window,
            cx,
        );
        let end = editor.buffer().read(cx).len(cx);
        editor.change_selections(SelectionEffects::no_scroll(), window, cx, |selections| {
            selections.select_ranges([end..end])
        });
        editor.show_completions(&ShowCompletions, window, cx);
    });
    kotlin_live_wait(&editor, cx, |editor, _| {
        kotlin_live_completion_index(editor, "Button").is_some()
    })
    .await;
    let accepted = Instant::now();
    editor
        .update_in(cx, |editor, window, cx| {
            let index = kotlin_live_completion_index(editor, "Button").expect("Button completion");
            editor
                .confirm_completion(
                    &ConfirmCompletion {
                        item_ix: Some(index),
                    },
                    window,
                    cx,
                )
                .expect("completion acceptance")
        })
        .await
        .expect("real command completion acceptance");
    let (accepted_text, caret) = editor.read_with(cx, |editor, cx| {
        (
            editor.text(cx),
            editor
                .selections
                .newest_anchor()
                .head()
                .to_offset(&editor.buffer().read(cx).snapshot(cx))
                .0,
        )
    });
    assert_eq!(
        accepted_text
            .matches("import androidx.compose.material3.Button\n")
            .count(),
        1
    );
    assert!(accepted_text.contains("Button("));
    assert!(
        caret
            > accepted_text
                .find("private fun clientAccept")
                .expect("probe function")
    );
    if config["require_compose"] == true {
        assert!(accepted_text.contains("Button() {"), "{accepted_text}");
    }
    report["acceptance"] = json!({"milliseconds": accepted.elapsed().as_secs_f64() * 1000.0, "text": accepted_text, "caret": caret});
    write_report(&report);

    assert_eq!(
        std::fs::read_to_string(&source_path).expect("source after probe"),
        original_disk_text
    );
    report["memory"]
        .as_array_mut()
        .expect("memory samples")
        .push(kotlin_live_memory("after_samples", server.process_id().expect("server PID")).await);
    let saved_library = if config["restore_library_tabs"] == true {
        let uri: lsp::Uri = report["samples"][0]["target"]
            .as_str()
            .expect("library URI")
            .parse()
            .unwrap();
        let library = project
            .update(cx, |project, cx| {
                project.open_local_buffer_via_lsp(uri, server.server_id(), cx)
            })
            .await
            .expect("library before restart");
        let location = lsp_store
            .read_with(cx, |store, cx| {
                store.language_server_document_location(library.read(cx), cx)
            })
            .expect("library location")
            .expect("server-owned library");
        Some((
            serde_json::from_value::<project::lsp_store::LanguageServerDocumentLocation>(
                serde_json::to_value(location).expect("serialize library location"),
            )
            .expect("deserialize library location"),
            library.read_with(cx, |buffer, _| buffer.text()),
        ))
    } else {
        None
    };
    lsp_store
        .update(cx, |store, cx| {
            store.stop_language_servers_for_buffers(vec![buffer], HashSet::default(), cx)
        })
        .await
        .expect("stop real language server");
    if let Some((location, original_library_text)) = saved_library {
        report["phase"] = json!("restoring_libraries");
        report["restorations"] = json!([]);
        write_report(&report);
        project.update(cx, |project, cx| {
            let worktree_id = project
                .worktrees(cx)
                .next()
                .expect("source workspace")
                .read(cx)
                .id();
            project.remove_worktree(worktree_id, cx);
        });
        for restart_during_import in [true, false] {
            let reopened = Project::test(fs.clone(), [root.as_path()], cx).await;
            register_adapter(&reopened, cx);
            let store = reopened.read_with(cx, |project, _| project.lsp_store());
            let (started, starting) = oneshot::channel();
            let mut started = Some(started);
            let _subscription = store.update(cx, |_, cx| {
                cx.subscribe(&cx.entity(), move |_, _, event, _| {
                    if let project::lsp_store::LspStoreEvent::LanguageServerAdded(id, _, _) = event
                        && let Some(started) = started.take()
                    {
                        started.send(*id).expect("initial server listener");
                    }
                })
            });
            let mut restore = store.update(cx, |store, cx| {
                store.restore_language_server_document(location.clone(), cx)
            });
            if restart_during_import {
                let server_id = starting.await.expect("server starts");
                assert!(
                    (&mut restore).now_or_never().is_none(),
                    "Inject managed setup before real import finishes"
                );
                store.update(cx, |store, cx| {
                    let restart = store.restart_language_servers_for_buffers_task(
                        Vec::new(),
                        HashSet::from_iter([lsp::LanguageServerSelector::Id(server_id)]),
                        true,
                        cx,
                    );
                    store.set_kotlin_setup_task(root.clone(), restart, cx);
                });
            }
            let library = restore.await.expect("restore real persisted library");
            let owner = library.read_with(cx, |buffer, _| {
                assert!(buffer.read_only());
                assert_eq!(buffer.text(), original_library_text);
                buffer
                    .language_server_document()
                    .expect("restored library owner")
                    .server_id
            });
            let process_id = store.read_with(cx, |store, _| {
                store
                    .language_server_for_id(owner)
                    .expect("current owner")
                    .process_id()
            });
            report["restorations"].as_array_mut().unwrap().push(json!({
                "restart_during_import": restart_during_import, "server_pid": process_id, "read_only": true,
            }));
            write_report(&report);
            store
                .update(cx, |store, cx| {
                    store.stop_language_servers_for_buffers(
                        Vec::new(),
                        HashSet::from_iter([lsp::LanguageServerSelector::Id(owner)]),
                        cx,
                    )
                })
                .await
                .expect("stop restored library owner");
            reopened.update(cx, |project, cx| {
                let worktree_id = project
                    .worktrees(cx)
                    .next()
                    .expect("reopened workspace")
                    .read(cx)
                    .id();
                project.remove_worktree(worktree_id, cx);
            });
        }
    }
    report["phase"] = json!("complete");
    write_report(&report);
    drop(app_state);
}

async fn kotlin_live_wait<T: 'static>(
    entity: &Entity<T>,
    cx: &mut gpui::TestAppContext,
    mut ready: impl FnMut(&T, &App) -> bool,
) {
    use futures::{FutureExt, StreamExt};
    use std::time::{Duration, Instant};
    let mut notifications = cx.notifications(entity);
    let started = Instant::now();
    loop {
        if entity.read_with(cx, &mut ready) {
            return;
        }
        let heartbeat = cx.executor().timer(Duration::from_secs(1)).fuse();
        futures::pin_mut!(heartbeat);
        futures::select_biased! {
            notification = notifications.next().fuse() => assert!(notification.is_some(), "probe entity dropped"),
            _ = heartbeat => assert!(started.elapsed() < Duration::from_secs(180), "real Kotlin editor probe timed out"),
        }
    }
}

fn kotlin_live_completion_index(editor: &Editor, expected: &str) -> Option<usize> {
    use crate::code_context_menus::CodeContextMenu;
    let menu = editor.context_menu.borrow();
    let CodeContextMenu::Completions(menu) = menu.as_ref()? else {
        return None;
    };
    let completions = menu.completions.borrow();
    menu.entries.borrow().iter().position(|entry| {
        entry
            .as_match()
            .and_then(|entry| completions.get(entry.candidate_id))
            .and_then(|completion| completion.source.lsp_completion(false))
            .is_some_and(|completion| completion.label.trim() == expected)
    })
}

async fn kotlin_live_memory(phase: &str, server_pid: u32) -> serde_json::Value {
    use collections::HashSet;
    use serde_json::json;
    let output = util::command::new_command("ps")
        .args(["-axo", "pid=,ppid=,rss=,command="])
        .output()
        .await
        .expect("read process memory");
    assert!(output.status.success());
    let processes = String::from_utf8(output.stdout)
        .expect("process output")
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((
                fields.next()?.parse::<u32>().ok()?,
                fields.next()?.parse::<u32>().ok()?,
                fields.next()?.parse::<u64>().ok()?,
                fields.collect::<Vec<_>>().join(" "),
            ))
        })
        .collect::<Vec<_>>();
    let client_pid = std::process::id();
    let mut owned = HashSet::from_iter([client_pid, server_pid]);
    loop {
        let previous = owned.len();
        for (pid, parent, _, command) in &processes {
            if owned.contains(parent) && !command.starts_with("ps ") {
                owned.insert(*pid);
            }
        }
        if owned.len() == previous {
            break;
        }
    }
    let selected = processes.into_iter().filter(|(pid, _, _, command)| {
        owned.contains(pid) || command.contains("org.gradle.launcher.daemon.bootstrap.GradleDaemon")
    }).map(|(pid, parent, rss, command)| json!({
        "pid": pid, "parent_pid": parent, "rss_kib": rss,
        "role": if pid == client_pid { "editor_client" } else if pid == server_pid { "kotlin_lsp_jvm" } else if command.contains("GradleDaemon") { "gradle_daemon" } else { "owned_child" },
        "owned_by_probe": owned.contains(&pid), "command": command,
    })).collect::<Vec<_>>();
    let total = selected
        .iter()
        .map(|process| process["rss_kib"].as_u64().expect("RSS"))
        .sum::<u64>();
    let owned_total = selected
        .iter()
        .filter(|process| process["owned_by_probe"] == true)
        .map(|process| process["rss_kib"].as_u64().expect("RSS"))
        .sum::<u64>();
    json!({"phase": phase, "processes": selected, "owned_rss_kib": owned_total,
        "including_shared_gradle_rss_kib": total,
        "gradle_note": "All machine Gradle daemons are listed separately; reused daemons can be shared with other runs.",
        "jdt_active_in_probe": false, "compose_preview_active_in_probe": false})
}
