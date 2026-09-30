use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use collections::HashMap;
use fs::{FakeFs, Fs};
use futures::{FutureExt, StreamExt};
use gpui::{Entity, TestAppContext};
use language::{
    Buffer, CodeLabel, DiagnosticSourceKind, FakeLspAdapter, HighlightId, LocalFile,
    OffsetRangeExt, rust_lang,
};
use lsp::{LanguageServerId, LanguageServerName, Uri};
use parking_lot::Mutex;
use project::{
    DiagnosticSummary, Event, Project,
    lsp_store::{
        log_store::{TestRpcLogHeaderState, TestRpcRequestTracker},
        *,
    },
};
use serde_json::json;
use unindent::Unindent;
use util::{path, rel_path::rel_path};

use crate::init_test;

#[gpui::test]
async fn test_explicit_server_can_share_an_extension_adapter_across_languages(
    cx: &mut TestAppContext,
) {
    use language::{Language, LanguageConfig, LanguageMatcher};

    init_test(cx);
    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        path!("/project"),
        json!({
            "main.rs": "fn main() {}",
            "Greeting.java": "class Greeting {}",
            ".zed": {"settings.json": json!({"languages": {
                "Rust": {"language_servers": ["kotlin-lsp"]},
                "Java": {"language_servers": ["kotlin-lsp", "jdtls"]}
            }}).to_string()}
        }),
    )
    .await;
    let project = Project::test(fs.clone(), [Path::new(path!("/project"))], cx).await;
    let languages = project.read_with(cx, |project, _| project.languages().clone());
    languages.add(rust_lang());
    languages.add(Arc::new(Language::new(
        LanguageConfig {
            name: "Java".into(),
            matcher: LanguageMatcher {
                path_suffixes: vec!["java".into()],
                ..Default::default()
            }
            .into(),
            ..Default::default()
        },
        None,
    )));
    let mut java_servers = languages.register_fake_lsp(
        "Java",
        FakeLspAdapter {
            name: "jdtls",
            ..Default::default()
        },
    );
    let mut servers = languages.register_fake_lsp(
        "Rust",
        FakeLspAdapter {
            name: "kotlin-lsp",
            ..Default::default()
        },
    );
    let (_rust, _rust_handle) = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/project/main.rs"), cx)
        })
        .await
        .expect("Rust opens");
    let mut server = servers.next().await.expect("Shared server starts");
    server
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;
    let (java, _java_handle) = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/project/Greeting.java"), cx)
        })
        .await
        .expect("Java opens");
    cx.run_until_parked();
    assert_eq!(
        project.read_with(cx, |project, cx| project
            .lsp_store()
            .read(cx)
            .language_server_statuses()
            .count()),
        2
    );
    let opened = server
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;
    assert_eq!(opened.text_document.language_id, "java");
    assert!(opened.text_document.uri.as_str().ends_with("Greeting.java"));
    assert_eq!(
        languages
            .lsp_adapters(&"Java".into())
            .iter()
            .map(|adapter| adapter.name().to_string())
            .collect::<Vec<_>>(),
        vec!["jdtls"],
        "Explicit attachment must not add a default server for other Java projects"
    );
    let jdt = java_servers.next().await.expect("JDT starts for Java");
    let jdt_requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    server.set_request_handler::<lsp::request::GotoDefinition, _, _>(|_, _| async move {
        Ok(Some(lsp::GotoDefinitionResponse::Scalar(
            lsp::Location::new(
                Uri::from_file_path(path!("/project/main.rs")).expect("Source URI"),
                lsp::Range::default(),
            ),
        )))
    });
    jdt.set_request_handler::<lsp::request::GotoDefinition, _, _>({
        let jdt_requests = jdt_requests.clone();
        move |_, _| {
            jdt_requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move {
                Ok(Some(lsp::GotoDefinitionResponse::Scalar(
                    lsp::Location::new(
                        Uri::from_file_path(path!("/project/Greeting.java")).expect("JDT URI"),
                        lsp::Range::default(),
                    ),
                )))
            }
        }
    });
    let definitions = project
        .update(cx, |project, cx| project.definitions(&java, 6, cx))
        .await
        .expect("Java definitions")
        .expect("Java definition response");
    assert_eq!(definitions.len(), 1);
    assert_eq!(jdt_requests.load(std::sync::atomic::Ordering::SeqCst), 0);

    fs.save(
        Path::new(path!("/project/.zed/settings.json")),
        &json!({"languages": {
            "Rust": {"language_servers": ["kotlin-lsp"]},
            "Java": {"language_servers": ["jdtls", "kotlin-lsp"]}
        }})
        .to_string()
        .into(),
        language::LineEnding::Unix,
    )
    .await
    .expect("Change Java priority");
    cx.run_until_parked();
    let definitions = project
        .update(cx, |project, cx| project.definitions(&java, 6, cx))
        .await
        .expect("Custom Java definitions")
        .expect("Custom Java definition response");
    assert_eq!(
        definitions.len(),
        2,
        "Custom JDT priority retains normal aggregation"
    );
    assert_eq!(jdt_requests.load(std::sync::atomic::Ordering::SeqCst), 1);

    project.update(cx, |project, cx| {
        project.lsp_store().update(cx, |store, cx| {
            store.restart_all_language_servers(cx);
        });
    });
    let mut restarted = servers.next().await.expect("Shared server restarts");
    let mut reopened = Vec::new();
    for _ in 0..2 {
        reopened.push(
            restarted
                .receive_notification::<lsp::notification::DidOpenTextDocument>()
                .await
                .text_document
                .language_id,
        );
    }
    reopened.sort();
    assert_eq!(reopened, vec!["java", "rust"]);
}

#[gpui::test]
async fn test_kotlin_completion_sessions_expire_on_import_restart_and_other_documents(
    cx: &mut TestAppContext,
) {
    init_test(cx);
    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        path!("/project"),
        json!({"main.rs": "But", "other.rs": "But"}),
    )
    .await;
    let project = Project::test(fs, [Path::new(path!("/project"))], cx).await;
    let languages = project.read_with(cx, |project, _| project.languages().clone());
    languages.add(rust_lang());
    let mut servers = languages.register_fake_lsp(
        "Rust",
        FakeLspAdapter {
            name: "kotlin-lsp",
            capabilities: lsp::ServerCapabilities {
                completion_provider: Some(lsp::CompletionOptions::default()),
                execute_command_provider: Some(lsp::ExecuteCommandOptions {
                    commands: vec!["jetbrains.kotlin.completion.apply".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let (buffer, _handle) = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/project/main.rs"), cx)
        })
        .await
        .expect("source opens");
    let server = servers.next().await.expect("Kotlin server starts");
    cx.run_until_parked();
    let install_completions = |server: &lsp::FakeLanguageServer| {
        server.set_request_handler::<lsp::request::Completion, _, _>(|params, _| async move {
            let position = params.text_document_position.position;
            Ok(Some(lsp::CompletionResponse::Array(vec![
                lsp::CompletionItem {
                    label: "Button".into(),
                    text_edit: Some(lsp::CompletionTextEdit::Edit(lsp::TextEdit::new(
                        lsp::Range::new(position, position),
                        String::new(),
                    ))),
                    command: Some(lsp::Command {
                        title: "Apply Completion".into(),
                        command: "jetbrains.kotlin.completion.apply".into(),
                        arguments: Some(vec![json!(1)]),
                    }),
                    ..Default::default()
                },
            ])))
        });
    };
    install_completions(&server);
    let lsp_store = project.read_with(cx, |project, _| project.lsp_store());
    let request = |buffer: &Entity<Buffer>, cx: &mut TestAppContext| {
        project.update(cx, |project, cx| {
            project.completions(
                buffer,
                3,
                lsp::CompletionContext {
                    trigger_kind: lsp::CompletionTriggerKind::INVOKED,
                    trigger_character: None,
                },
                cx,
            )
        })
    };
    let first = request(&buffer, cx)
        .await
        .expect("completion response")
        .into_iter()
        .flat_map(|response| response.completions)
        .next()
        .expect("completion item");
    assert!(lsp_store.read_with(cx, |store, cx| {
        store.completion_is_current(&buffer, &first, cx)
    }));
    enum ImportLog {}
    impl lsp::notification::Notification for ImportLog {
        type Params = serde_json::Value;
        const METHOD: &'static str = "intellij/importLog";
    }
    server.notify::<ImportLog>(json!({"started": true}));
    cx.run_until_parked();
    assert!(!lsp_store.read_with(cx, |store, cx| {
        store.completion_is_current(&buffer, &first, cx)
    }));
    let project::CompletionSource::Lsp {
        server_id,
        lsp_completion,
        completion_session,
        ..
    } = first.source
    else {
        panic!("LSP completion expected")
    };
    let result = lsp_store
        .update(cx, |store, cx| {
            store.apply_completion_command(
                buffer.clone(),
                project::CodeAction {
                    server_id,
                    range: language::Anchor::min_min_range_for_buffer(buffer.read(cx).remote_id()),
                    lsp_action: project::LspAction::Command(
                        lsp_completion.command.expect("completion command"),
                    ),
                    resolved: false,
                },
                completion_session,
                cx,
            )
        })
        .await;
    assert!(
        result
            .err()
            .expect("old model command is rejected")
            .to_string()
            .contains("Completion expired")
    );
    let after_import = request(&buffer, cx)
        .await
        .expect("new model completion")
        .into_iter()
        .flat_map(|response| response.completions)
        .next()
        .expect("completion item");
    let (other_buffer, _other_handle) = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/project/other.rs"), cx)
        })
        .await
        .expect("second source opens");
    request(&other_buffer, cx)
        .await
        .expect("other document completion");
    assert!(
        !lsp_store.read_with(cx, |store, cx| store.completion_is_current(
            &buffer,
            &after_import,
            cx
        )),
        "the server session belongs to the latest document request"
    );
    let before_restart = request(&buffer, cx)
        .await
        .expect("completion before restart")
        .into_iter()
        .flat_map(|response| response.completions)
        .next()
        .expect("completion item");
    lsp_store.update(cx, |store, cx| store.restart_all_language_servers(cx));
    let restarted = servers.next().await.expect("replacement server starts");
    cx.run_until_parked();
    assert!(
        !lsp_store.read_with(cx, |store, cx| store.completion_is_current(
            &buffer,
            &before_restart,
            cx
        ))
    );
    install_completions(&restarted);
    let after_restart = request(&buffer, cx)
        .await
        .expect("new runtime completion")
        .into_iter()
        .flat_map(|response| response.completions)
        .next()
        .expect("completion item");
    assert!(
        lsp_store.read_with(cx, |store, cx| store.completion_is_current(
            &buffer,
            &after_restart,
            cx
        ))
    );

    restarted.set_request_handler::<lsp::request::Completion, _, _>(|_, _| async {
        Err(lsp::ResponseError::new(
            lsp::ResponseErrorCode::Other(-32603),
            "fixture Kotlin internal error",
        )
        .into())
    });
    let error = request(&buffer, cx)
        .await
        .err()
        .expect("internal completion error must reach the caller");
    assert!(format!("{error:#}").contains("fixture Kotlin internal error"));
    restarted.set_request_handler::<lsp::request::Completion, _, _>(|_, _| async {
        Err(lsp::ResponseError::server_cancelled().into())
    });
    assert!(
        request(&buffer, cx)
            .await
            .expect("cancellation is quiet")
            .is_empty()
    );

    cx.update(|cx| {
        use gpui::UpdateGlobal as _;
        settings::SettingsStore::update_global(cx, |store, cx| {
            store.update_user_settings(cx, &|settings: &mut settings::SettingsContent| {
                settings
                    .global_lsp_settings
                    .get_or_insert_default()
                    .request_timeout = Some(1);
            });
        });
    });
    let mut server = restarted;
    for command_timeout in [false, true] {
        install_completions(&server);
        if command_timeout {
            let completion = request(&buffer, cx)
                .await
                .expect("completion before timeout")
                .into_iter()
                .flat_map(|response| response.completions)
                .next()
                .expect("completion");
            let project::CompletionSource::Lsp {
                server_id,
                lsp_completion,
                completion_session,
                ..
            } = completion.source
            else {
                panic!("LSP completion expected");
            };
            server.set_request_handler::<lsp::request::ExecuteCommand, _, _>(|_, _| {
                futures::future::pending()
            });
            let error = lsp_store
                .update(cx, |store, cx| {
                    store.apply_completion_command(
                        buffer.clone(),
                        project::CodeAction {
                            server_id,
                            range: language::Anchor::min_min_range_for_buffer(
                                buffer.read(cx).remote_id(),
                            ),
                            lsp_action: project::LspAction::Command(
                                lsp_completion.command.expect("command"),
                            ),
                            resolved: false,
                        },
                        completion_session,
                        cx,
                    )
                })
                .await
                .err()
                .expect("command times out");
            assert!(format!("{error:#}").contains("timed out"));
        } else {
            server.set_request_handler::<lsp::request::Completion, _, _>(|_, _| {
                futures::future::pending()
            });
            let error = request(&buffer, cx)
                .await
                .err()
                .expect("completion times out");
            assert!(format!("{error:#}").contains("timed out"));
        }
        cx.run_until_parked();
        // A server can keep working after the request timeout and send unversioned
        // edits. The missing response leaves its global completion state unknown.
        let response = server
            .server
            .request::<lsp::request::ApplyWorkspaceEdit>(
                lsp::ApplyWorkspaceEditParams {
                    label: None,
                    edit: lsp::WorkspaceEdit::new(
                        [(
                            Uri::from_file_path(path!("/project/main.rs")).expect("source URI"),
                            vec![lsp::TextEdit::new(
                                lsp::Range::new(lsp::Position::default(), lsp::Position::new(0, 3)),
                                "Button()".into(),
                            )],
                        )]
                        .into_iter()
                        .collect(),
                    ),
                },
                lsp::DEFAULT_LSP_REQUEST_TIMEOUT,
            )
            .await
            .into_response()
            .expect("late edit response");
        assert!(!response.applied);
        assert!(
            response
                .failure_reason
                .is_some_and(|reason| reason.contains("Restart the language server"))
        );
        assert_eq!(buffer.read_with(cx, |buffer, _| buffer.text()), "But");
        let shown = server
            .server
            .request::<lsp::request::ShowDocument>(
                lsp::ShowDocumentParams {
                    uri: Uri::from_file_path(path!("/project/main.rs")).expect("source URI"),
                    external: None,
                    take_focus: Some(true),
                    selection: Some(lsp::Range::default()),
                },
                lsp::DEFAULT_LSP_REQUEST_TIMEOUT,
            )
            .await
            .into_response()
            .expect("late navigation response");
        assert!(!shown.success);
        install_completions(&server);
        let error = request(&buffer, cx)
            .await
            .err()
            .expect("an unknown session requires restart");
        assert!(format!("{error:#}").contains("Restart the language server"));
        lsp_store.update(cx, |store, cx| store.restart_all_language_servers(cx));
        server = servers.next().await.expect("server restarts after timeout");
        cx.run_until_parked();
        install_completions(&server);
        assert!(
            !request(&buffer, cx)
                .await
                .expect("completion recovers after restart")
                .is_empty()
        );
    }
}

#[gpui::test]
async fn test_lsp_workspace_edit_reports_failure(cx: &mut TestAppContext) {
    init_test(cx);
    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(path!("/dir"), json!({ "a.rs": "one" }))
        .await;
    let project = Project::test(fs, [Path::new(path!("/dir"))], cx).await;
    let language_registry = project.read_with(cx, |project, _| project.languages().clone());
    language_registry.add(rust_lang());
    let mut fake_servers = language_registry.register_fake_lsp("Rust", FakeLspAdapter::default());
    let (buffer, _handle) = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/dir/a.rs"), cx)
        })
        .await
        .expect("test buffer should open");
    let server = fake_servers.next().await.expect("server should start");
    cx.run_until_parked();

    for (version, capability, failure) in [
        (
            Some(i32::MAX),
            language::Capability::ReadWrite,
            Some("snapshot not found"),
        ),
        (None, language::Capability::Read, Some("read-only")),
        (None, language::Capability::ReadOnly, Some("read-only")),
        (None, language::Capability::ReadWrite, None),
    ] {
        buffer.update(cx, |buffer, cx| buffer.set_capability(capability, cx));
        let response = server
            .request::<lsp::request::ApplyWorkspaceEdit>(
                lsp::ApplyWorkspaceEditParams {
                    label: Some("Apply Completion".into()),
                    edit: lsp::WorkspaceEdit {
                        document_changes: Some(lsp::DocumentChanges::Edits(vec![
                            lsp::TextDocumentEdit {
                                text_document: lsp::OptionalVersionedTextDocumentIdentifier {
                                    uri: Uri::from_file_path(path!("/dir/a.rs"))
                                        .expect("valid URI"),
                                    version,
                                },
                                edits: vec![lsp::Edit::Plain(lsp::TextEdit::new(
                                    lsp::Range::new(
                                        lsp::Position::new(0, 0),
                                        lsp::Position::new(0, 3),
                                    ),
                                    "two".into(),
                                ))],
                            },
                        ])),
                        ..Default::default()
                    },
                },
                lsp::DEFAULT_LSP_REQUEST_TIMEOUT,
            )
            .await
            .into_response()
            .expect("edit rejection should be a protocol response");
        assert_eq!(response.applied, failure.is_none());
        if let Some(failure) = failure {
            assert!(
                response
                    .failure_reason
                    .expect("failure reason")
                    .contains(failure)
            );
            assert_eq!(buffer.read_with(cx, |buffer, _| buffer.text()), "one");
        } else {
            assert_eq!(response.failure_reason, None);
            assert_eq!(buffer.read_with(cx, |buffer, _| buffer.text()), "two");
        }
    }
}

#[gpui::test]
async fn test_diagnostic_batches_skip_paths_without_worktrees(cx: &mut TestAppContext) {
    init_test(cx);

    for skipped_index in 0..=2 {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/dir"), json!({ "a.rs": "one", "b.rs": "two" }))
            .await;
        let project = Project::test(fs, [Path::new(path!("/dir"))], cx).await;
        let lsp_store = project.read_with(cx, |project, _| project.lsp_store());
        let buffer_a = project
            .update(cx, |project, cx| {
                project.open_local_buffer(path!("/dir/a.rs"), cx)
            })
            .await
            .unwrap();
        let worktree_id =
            buffer_a.read_with(cx, |buffer, cx| buffer.file().unwrap().worktree_id(cx));
        let server_id = LanguageServerId(0);

        for message in [Some("error"), None] {
            cx.run_until_parked();
            project.read_with(cx, |project, cx| {
                assert_eq!(
                    project.get_open_buffer(&(worktree_id, rel_path("b.rs")).into(), cx),
                    None
                );
            });
            let mut events = cx.events(&project);
            let mut paths = vec![path!("/dir/a.rs"), path!("/dir/b.rs")];
            paths.insert(skipped_index, path!("/outside.rs"));
            let source_uri = Uri::from_file_path(path!("/dir/a.rs")).unwrap();
            let uris = paths
                .into_iter()
                .map(|path| Uri::from_file_path(path).unwrap())
                .chain(["jar", "jrt"].into_iter().map(|scheme| {
                    source_uri
                        .as_str()
                        .replacen("file:", &format!("{scheme}:"), 1)
                        .parse::<Uri>()
                        .unwrap()
                }));
            let updates = uris
                .map(|uri| {
                    let message = if uri.scheme() == "file" {
                        message
                    } else {
                        Some("nonfile error must not attach to a.rs")
                    };
                    DocumentDiagnosticsUpdate {
                        diagnostics: lsp::PublishDiagnosticsParams {
                            uri,
                            version: None,
                            diagnostics: message
                                .into_iter()
                                .map(|message| lsp::Diagnostic {
                                    range: lsp::Range::new(
                                        lsp::Position::new(0, 0),
                                        lsp::Position::new(0, 3),
                                    ),
                                    severity: Some(lsp::DiagnosticSeverity::ERROR),
                                    message: lsp::DiagnosticMessage::from(message),
                                    ..lsp::Diagnostic::default()
                                })
                                .collect(),
                        },
                        result_id: None,
                        registration_id: None,
                        server_id,
                        disk_based_sources: Cow::Borrowed(&[]),
                    }
                })
                .collect();
            lsp_store.update(cx, |lsp_store, cx| {
                lsp_store
                    .merge_lsp_diagnostics(
                        DiagnosticSourceKind::Pushed,
                        updates,
                        |_, _, _| false,
                        cx,
                    )
                    .unwrap();
            });
            cx.run_until_parked();

            project.read_with(cx, |project, cx| {
                assert_eq!(
                    project.diagnostic_summary(false, cx),
                    DiagnosticSummary {
                        error_count: if message.is_some() { 2 } else { 0 },
                        warning_count: 0,
                    },
                    "skipped update at index {skipped_index}, message {message:?}"
                );
            });
            let diagnostic_events = std::iter::from_fn(|| events.next().now_or_never().flatten())
                .filter_map(|event| match event {
                    Event::DiagnosticsUpdated {
                        language_server_id,
                        paths,
                    } => Some((language_server_id, paths)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                diagnostic_events,
                vec![(
                    server_id,
                    vec![
                        (worktree_id, rel_path("a.rs")).into(),
                        (worktree_id, rel_path("b.rs")).into(),
                    ],
                )],
                "skipped update at index {skipped_index}, message {message:?}"
            );

            let buffer_b = project
                .update(cx, |project, cx| {
                    project.open_local_buffer(path!("/dir/b.rs"), cx)
                })
                .await
                .unwrap();
            for buffer in [&buffer_a, &buffer_b] {
                buffer.read_with(cx, |buffer, _| {
                    assert_eq!(
                        buffer
                            .buffer_diagnostics(Some(server_id))
                            .iter()
                            .map(|entry| entry.diagnostic.message.to_string())
                            .collect::<Vec<_>>(),
                        message.into_iter().collect::<Vec<_>>()
                    );
                });
            }
        }
    }
}

#[gpui::test]
async fn test_invisible_worktree_reuses_project_lsp_and_cleans_bookkeeping(
    cx: &mut TestAppContext,
) {
    init_test(cx);
    cx.executor().allow_parking();

    cx.update_global::<settings::SettingsStore, _>(|store, cx| {
        store
            .set_user_settings(
                &json!({"languages": {"Rust": {"language_servers": ["default-rust"]}}}).to_string(),
                cx,
            )
            .unwrap();
    });
    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        path!("/the-root"),
        json!({
            "main.rs": "fn main() {}",
            ".zed": {"settings.json": json!({
                "languages": {"Rust": {"language_servers": ["the-fake-language-server"]}}
            }).to_string()}
        }),
    )
    .await;
    fs.insert_tree(
        path!("/the-registry"),
        json!({ "dep": { "src": { "dep.rs": "pub fn dep() {}" } } }),
    )
    .await;

    let project = Project::test(fs, [path!("/the-root").as_ref()], cx).await;
    let language_registry = project.read_with(cx, |project, _| project.languages().clone());
    language_registry.add(rust_lang());
    let mut fake_servers = language_registry.register_fake_lsp("Rust", FakeLspAdapter::default());

    let (_visible_buffer, _visible_handle) = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/the-root/main.rs"), cx)
        })
        .await
        .unwrap();
    let fake_server = fake_servers.next().await.unwrap();
    cx.run_until_parked();

    let server_id = project.read_with(cx, |project, cx| {
        project
            .lsp_store()
            .read(cx)
            .language_server_statuses()
            .next()
            .unwrap()
            .0
    });
    let external_buffer = project
        .update(cx, |project, cx| {
            project.open_local_buffer_via_lsp(
                Uri::from_file_path(path!("/the-registry/dep/src/dep.rs")).unwrap(),
                server_id,
                cx,
            )
        })
        .await
        .unwrap();
    cx.run_until_parked();

    let _external_handle = project.update(cx, |project, cx| {
        project.register_buffer_with_language_servers(&external_buffer, cx)
    });
    cx.run_until_parked();

    let invisible_worktree_id =
        external_buffer.read_with(cx, |buffer, cx| buffer.file().unwrap().worktree_id(cx));
    project.read_with(cx, |project, cx| {
        let worktree = project.worktree_for_id(invisible_worktree_id, cx).unwrap();
        assert!(!worktree.read(cx).is_visible());
        assert!(
            project
                .lsp_store()
                .read(cx)
                .has_language_server_seed_for_worktree(invisible_worktree_id)
        );
    });

    fake_server.set_request_handler::<lsp::request::GotoDefinition, _, _>(|params, _| async move {
        let uri = params.text_document_position_params.text_document.uri;
        assert_eq!(
            uri,
            Uri::from_file_path(path!("/the-registry/dep/src/dep.rs")).unwrap()
        );
        Ok(Some(lsp::GotoDefinitionResponse::Scalar(
            lsp::Location::new(
                uri,
                lsp::Range::new(lsp::Position::new(0, 7), lsp::Position::new(0, 10)),
            ),
        )))
    });
    let definitions = project
        .update(cx, |project, cx| {
            project.definitions(&external_buffer, 8, cx)
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(definitions.len(), 1);
    assert!(fake_servers.try_recv().is_err());

    project.update(cx, |project, cx| {
        project.remove_worktree(invisible_worktree_id, cx);
    });
    cx.run_until_parked();

    project.read_with(cx, |project, cx| {
        let lsp_store = project.lsp_store();
        let lsp_store = lsp_store.read(cx);
        assert!(
            lsp_store
                .language_server_statuses()
                .any(|(status_server_id, _)| status_server_id == server_id)
        );
        assert!(!lsp_store.has_language_server_seed_for_worktree(invisible_worktree_id));
    });
}

#[gpui::test]
async fn test_first_archive_language_server_starts_in_project(cx: &mut TestAppContext) {
    init_test(cx);
    cx.executor().allow_parking();
    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        path!("/the-project"),
        json!({
            "main.rs": "fn main() {}",
            ".zed": {"settings.json": json!({
                "languages": {"Rust": {"language_servers": ["project-rust"]}}
            }).to_string()}
        }),
    )
    .await;
    fs.insert_tree(
        path!("/registry"),
        json!({
            "library.jar!": {"dependency.rs": "pub fn dependency() {}"}
        }),
    )
    .await;
    let project = Project::test(fs, [path!("/the-project").as_ref()], cx).await;
    let project_worktree = project.read_with(cx, |project, cx| {
        project.worktrees(cx).next().unwrap().read(cx).id()
    });
    let languages = project.read_with(cx, |project, _| project.languages().clone());
    languages.add(rust_lang());
    let mut servers = languages.register_fake_lsp(
        "Rust",
        FakeLspAdapter {
            name: "project-rust",
            ..Default::default()
        },
    );
    let _archive_worktree = project
        .update(cx, |project, cx| {
            project.create_worktree(path!("/registry/library.jar!/dependency.rs"), false, cx)
        })
        .await
        .unwrap();
    let (buffer, _handle) = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/registry/library.jar!/dependency.rs"), cx)
        })
        .await
        .unwrap();
    let server = servers.next().await.unwrap();
    cx.run_until_parked();
    project.read_with(cx, |project, cx| {
        let lsp_store = project.lsp_store().read(cx);
        assert_eq!(lsp_store.language_server_statuses().count(), 1);
        assert_eq!(
            lsp_store
                .language_server_statuses()
                .next()
                .unwrap()
                .1
                .worktree,
            Some(project_worktree)
        );
    });
    server.set_request_handler::<lsp::request::GotoDefinition, _, _>(|params, _| async move {
        let uri = params.text_document_position_params.text_document.uri;
        assert_eq!(
            uri,
            Uri::from_file_path(path!("/registry/library.jar!/dependency.rs")).unwrap()
        );
        Ok(Some(lsp::GotoDefinitionResponse::Scalar(
            lsp::Location::new(
                uri,
                lsp::Range::new(lsp::Position::new(0, 7), lsp::Position::new(0, 17)),
            ),
        )))
    });
    let definitions = project
        .update(cx, |project, cx| project.definitions(&buffer, 9, cx))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(definitions.len(), 1);

    cx.update_global::<settings::SettingsStore, _>(|store, cx| {
        store
            .set_user_settings(
                &json!({"lsp": {"project-rust": {
                    "initialization_options": {"refresh": true}
                }}})
                .to_string(),
                cx,
            )
            .unwrap();
    });
    let refreshed_server = servers.next().await.unwrap();
    assert_ne!(
        refreshed_server.server.server_id(),
        server.server.server_id()
    );
    cx.run_until_parked();
    project.read_with(cx, |project, cx| {
        let lsp_store = project.lsp_store().read(cx);
        assert_eq!(lsp_store.language_server_statuses().count(), 1);
        assert_eq!(
            lsp_store
                .language_server_statuses()
                .next()
                .unwrap()
                .1
                .worktree,
            Some(project_worktree)
        );
    });
}

#[gpui::test]
async fn test_kotlin_virtual_documents_preserve_uri_ownership_and_model(cx: &mut TestAppContext) {
    init_test(cx);
    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(path!("/project"), json!({"main.rs": "fn main() {}"}))
        .await;
    let project = Project::test(fs, [Path::new(path!("/project"))], cx).await;
    let languages = project.read_with(cx, |project, _| project.languages().clone());
    languages.add(rust_lang());
    let mut capabilities = lsp::LanguageServer::full_capabilities();
    capabilities.execute_command_provider = Some(lsp::ExecuteCommandOptions {
        commands: vec!["decompile".into()],
        ..Default::default()
    });
    capabilities.hover_provider = None;
    let mut servers = languages.register_fake_lsp(
        "Rust",
        FakeLspAdapter {
            name: "kotlin-lsp",
            capabilities,
            ..Default::default()
        },
    );
    let (source, _source_handle) = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/project/main.rs"), cx)
        })
        .await
        .expect("source should open");
    let mut server = servers.next().await.expect("official server should start");
    let server_id = server.server.server_id();
    cx.run_until_parked();
    server
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .now_or_never()
        .expect("source didOpen after startup");

    let binary: Uri = "jar:///cache%20directory/ui.jar!/sample/Dependency.class"
        .parse()
        .expect("valid jar URI");
    let original: Uri = "jar:///cache%20directory/ui-sources.jar!/commonMain/Nested.kt"
        .parse()
        .expect("valid source URI");
    let runtime: Uri = "jrt:///jdk%2025!/java.base/java/lang/String.class"
        .parse()
        .expect("valid JDK URI");
    let fetched = Arc::new(Mutex::new(Vec::new()));
    server.set_request_handler::<lsp::request::ExecuteCommand, _, _>({
        let fetched = fetched.clone();
        move |params, _| {
            let fetched = fetched.clone();
            async move {
                assert_eq!(params.command, "decompile");
                let uri = params
                    .arguments
                    .first()
                    .and_then(|value| value.as_str())
                    .expect("original URI argument");
                fetched.lock().push(uri.to_owned());
                if uri.ends_with("missing.class") {
                    anyhow::bail!("missing library entry");
                }
                if uri.ends_with("malformed.class") {
                    return Ok(Some(json!({"code": 42})));
                }
                Ok(Some(
                    json!({"code": "pub fn dependency() {}", "language": "rust"}),
                ))
            }
        }
    });
    server.set_request_handler::<lsp::request::GotoDefinition, _, _>({
        let binary = binary.clone();
        let original = original.clone();
        let runtime = runtime.clone();
        move |params, _| {
            let uri = params.text_document_position_params.text_document.uri;
            let target = if uri.scheme() == "file" {
                binary.clone()
            } else if uri == binary {
                original.clone()
            } else {
                assert_eq!(uri, original);
                runtime.clone()
            };
            async move {
                Ok(Some(lsp::GotoDefinitionResponse::Scalar(
                    lsp::Location::new(
                        target,
                        lsp::Range::new(lsp::Position::new(0, 7), lsp::Position::new(0, 17)),
                    ),
                )))
            }
        }
    });
    let definitions = project
        .update(cx, |project, cx| project.definitions(&source, 3, cx))
        .await
        .expect("definition request")
        .expect("definition response");
    let library = definitions
        .first()
        .expect("library target")
        .target
        .buffer
        .clone();
    library.read_with(cx, |buffer, cx| {
        assert_eq!(buffer.text(), "pub fn dependency() {}");
        assert_eq!(
            buffer
                .language()
                .expect("server language")
                .name()
                .0
                .as_ref(),
            "Rust"
        );
        assert!(buffer.read_only());
        assert!(buffer.file().expect("display metadata").can_open());
        assert!(matches!(
            buffer.file().expect("display metadata").disk_state(),
            language::DiskState::Historic { .. }
        ));
        assert!(
            buffer
                .file()
                .expect("display metadata")
                .as_local()
                .is_none()
        );
        assert_eq!(
            buffer.file().expect("display metadata").full_path(cx),
            PathBuf::from(binary.as_str())
        );
        assert_eq!(
            definitions
                .first()
                .expect("library target")
                .target
                .range
                .to_point_utf16(buffer),
            language::PointUtf16::new(0, 7)..language::PointUtf16::new(0, 17)
        );
    });
    let old_handle = project.update(cx, |project, cx| {
        project.register_buffer_with_language_servers(&library, cx)
    });
    cx.run_until_parked();
    assert_eq!(
        server
            .receive_notification::<lsp::notification::DidOpenTextDocument>()
            .now_or_never()
            .expect("binary didOpen")
            .text_document
            .uri,
        binary
    );
    let duplicate = project
        .update(cx, |project, cx| {
            project.open_local_buffer_via_lsp(binary.clone(), server_id, cx)
        })
        .await
        .expect("duplicate should reuse buffer");
    assert_eq!(library, duplicate);
    assert_eq!(*fetched.lock(), vec![binary.to_string()]);

    server.set_request_handler::<lsp::request::DocumentDiagnosticRequest, _, _>(
        |params, _| async move {
            assert_eq!(params.text_document.uri.scheme(), "jar");
            Ok(lsp::DocumentDiagnosticReportResult::Report(
                lsp::DocumentDiagnosticReport::Full(
                    lsp::RelatedFullDocumentDiagnosticReport::default(),
                ),
            ))
        },
    );
    server
        .request::<lsp::request::RegisterCapability>(
            lsp::RegistrationParams {
                registrations: vec![
                    lsp::Registration {
                        id: "jar-hover".into(),
                        method: "textDocument/hover".into(),
                        register_options: Some(
                            json!({"documentSelector": [{"scheme": "jar", "language": "rust"}]}),
                        ),
                    },
                    lsp::Registration {
                        id: "jar-diagnostics".into(),
                        method: "textDocument/diagnostic".into(),
                        register_options: Some(json!({
                            "documentSelector": [{"scheme": "jar", "language": "rust"}],
                            "interFileDependencies": false,
                            "workspaceDiagnostics": false
                        })),
                    },
                ],
            },
            lsp::DEFAULT_LSP_REQUEST_TIMEOUT,
        )
        .await
        .into_response()
        .expect("register jar hover");
    let lsp_store = project.read_with(cx, |project, _| project.lsp_store());
    let diagnostics = lsp_store
        .update(cx, |store, cx| store.pull_diagnostics(library.clone(), cx))
        .await
        .expect("library diagnostics keep the server URI")
        .expect("library diagnostic response");
    assert_eq!(diagnostics.len(), 1);
    assert!(matches!(
        diagnostics.first(),
        Some(project::LspPullDiagnostics::Response { server_id: owner, uri, .. })
            if *owner == server_id && uri == &binary
    ));
    lsp_store
        .update(cx, |store, cx| {
            store.pull_diagnostics_for_buffer(library.clone(), cx)
        })
        .await
        .expect("background library diagnostics should not require a filesystem path");
    server.set_request_handler::<lsp::request::HoverRequest, _, _>({
        let binary = binary.clone();
        move |params, _| {
            assert_eq!(
                params.text_document_position_params.text_document.uri,
                binary
            );
            async {
                Ok(Some(lsp::Hover {
                    contents: lsp::HoverContents::Scalar(lsp::MarkedString::String(
                        "library documentation".into(),
                    )),
                    range: None,
                }))
            }
        }
    });
    assert!(
        project
            .update(cx, |project, cx| project.hover(&library, 8, cx))
            .await
            .is_some_and(|hovers| !hovers.is_empty())
    );
    let nested = project
        .update(cx, |project, cx| project.definitions(&library, 8, cx))
        .await
        .expect("nested definition request")
        .expect("nested definition response");
    let nested = nested
        .first()
        .expect("original source target")
        .target
        .buffer
        .clone();
    let nested_handle = project.update(cx, |project, cx| {
        project.register_buffer_with_language_servers(&nested, cx)
    });
    cx.run_until_parked();
    assert_eq!(
        server
            .receive_notification::<lsp::notification::DidOpenTextDocument>()
            .now_or_never()
            .expect("source attachment didOpen")
            .text_document
            .uri,
        original
    );
    let jdk = project
        .update(cx, |project, cx| project.definitions(&nested, 8, cx))
        .await
        .expect("JDK definition request")
        .expect("JDK definition response");
    assert!(
        jdk.first()
            .expect("JDK target")
            .target
            .buffer
            .read_with(cx, |buffer, _| buffer.read_only())
    );
    assert_eq!(
        *fetched.lock(),
        vec![
            binary.to_string(),
            original.to_string(),
            runtime.to_string()
        ]
    );
    assert_eq!(
        worktree_roots(&project, cx),
        vec![PathBuf::from(path!("/project"))]
    );
    assert!(servers.try_recv().is_err());
    for entry in ["missing.class", "malformed.class"] {
        let uri = format!("jar:///cache/ui.jar!/{entry}")
            .parse()
            .expect("valid error URI");
        assert!(
            project
                .update(cx, |project, cx| project
                    .open_local_buffer_via_lsp(uri, server_id, cx))
                .await
                .is_err()
        );
    }

    enum ImportLog {}
    impl lsp::notification::Notification for ImportLog {
        type Params = serde_json::Value;
        const METHOD: &'static str = "intellij/importLog";
    }
    server.notify::<ImportLog>(json!({"started": true}));
    cx.run_until_parked();
    let mut closed = vec![
        server
            .receive_notification::<lsp::notification::DidCloseTextDocument>()
            .now_or_never()
            .expect("first old document didClose")
            .text_document
            .uri,
        server
            .receive_notification::<lsp::notification::DidCloseTextDocument>()
            .now_or_never()
            .expect("second old document didClose")
            .text_document
            .uri,
    ];
    closed.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    let mut expected = vec![binary.clone(), original.clone()];
    expected.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    assert_eq!(closed, expected);
    let fresh = project
        .update(cx, |project, cx| {
            project.open_local_buffer_via_lsp(binary.clone(), server_id, cx)
        })
        .await
        .expect("reimported document should load");
    assert_ne!(library, fresh);
    let fresh_handle = project.update(cx, |project, cx| {
        project.register_buffer_with_language_servers(&fresh, cx)
    });
    cx.run_until_parked();
    assert_eq!(
        server
            .receive_notification::<lsp::notification::DidOpenTextDocument>()
            .now_or_never()
            .expect("fresh binary didOpen")
            .text_document
            .uri,
        binary
    );
    cx.update(|_| {
        drop(old_handle);
        drop(nested_handle);
    });
    cx.run_until_parked();
    assert!(
        server
            .try_receive_notification::<lsp::notification::DidCloseTextDocument>()
            .now_or_never()
            .is_none()
    );
    assert!(
        project
            .update(cx, |project, cx| project.hover(&fresh, 8, cx))
            .await
            .is_some_and(|hovers| !hovers.is_empty())
    );
    cx.update(|_| drop(fresh_handle));
    cx.run_until_parked();
    assert_eq!(
        server
            .receive_notification::<lsp::notification::DidCloseTextDocument>()
            .now_or_never()
            .expect("fresh binary didClose")
            .text_document
            .uri,
        binary
    );
    project.update(cx, |project, cx| {
        project
            .lsp_store()
            .update(cx, |store, cx| store.stop_all_language_servers(cx))
    });
    cx.run_until_parked();
    assert!(
        project
            .update(cx, |project, cx| project
                .open_local_buffer_via_lsp(binary, server_id, cx))
            .await
            .is_err()
    );
}

#[gpui::test]
async fn test_kotlin_virtual_documents_deduplicate_per_server(cx: &mut TestAppContext) {
    init_test(cx);
    let fs = FakeFs::new(cx.executor());
    for root in [path!("/first"), path!("/second")] {
        fs.insert_tree(root, json!({"main.rs": "fn main() {}"}))
            .await;
    }
    let project = Project::test(
        fs,
        [Path::new(path!("/first")), Path::new(path!("/second"))],
        cx,
    )
    .await;
    let languages = project.read_with(cx, |project, _| project.languages().clone());
    languages.add(rust_lang());
    let mut capabilities = lsp::LanguageServer::full_capabilities();
    capabilities.execute_command_provider = Some(lsp::ExecuteCommandOptions {
        commands: vec!["decompile".into()],
        ..Default::default()
    });
    let mut servers = languages.register_fake_lsp(
        "Rust",
        FakeLspAdapter {
            name: "kotlin-lsp",
            capabilities,
            ..Default::default()
        },
    );
    let mut handles = Vec::new();
    let mut libraries = Vec::new();
    let mut running_servers = Vec::new();
    let uri: Uri = "jar:///shared.jar!/Dependency.class"
        .parse()
        .expect("valid shared URI");
    for (index, path) in [path!("/first/main.rs"), path!("/second/main.rs")]
        .into_iter()
        .enumerate()
    {
        let (_, handle) = project
            .update(cx, |project, cx| {
                project.open_local_buffer_with_lsp(path, cx)
            })
            .await
            .expect("source should open");
        handles.push(handle);
        let server = servers.next().await.expect("workspace server should start");
        let server_id = server.server.server_id();
        cx.run_until_parked();
        let requests = Arc::new(Mutex::new(0));
        server.set_request_handler::<lsp::request::ExecuteCommand, _, _>({
            let requests = requests.clone();
            let uri = uri.clone();
            move |params, _| {
                assert_eq!(params.arguments, vec![json!(uri)]);
                *requests.lock() += 1;
                async move { Ok(Some(json!({"code": format!("pub const WORKSPACE: usize = {index};"), "language": "rust"}))) }
            }
        });
        let (first, concurrent) = project.update(cx, |project, cx| {
            (
                project.open_local_buffer_via_lsp(uri.clone(), server_id, cx),
                project.open_local_buffer_via_lsp(uri.clone(), server_id, cx),
            )
        });
        let (first, concurrent) = futures::join!(first, concurrent);
        let first = first.expect("first open");
        assert_eq!(first, concurrent.expect("concurrent open"));
        assert_eq!(*requests.lock(), 1);
        assert!(
            first
                .read_with(cx, |buffer, _| buffer.text())
                .contains(&format!("= {index};"))
        );
        libraries.push(first);
        running_servers.push(server);
    }
    assert_ne!(libraries.first(), libraries.last());
    assert_eq!(worktree_roots(&project, cx).len(), 2);
    assert!(servers.try_recv().is_err());
}

#[gpui::test]
async fn test_kotlin_virtual_requests_reject_responses_after_model_invalidation(
    cx: &mut TestAppContext,
) {
    init_test(cx);
    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(path!("/project"), json!({"main.rs": "fn main() {}"}))
        .await;
    let project = Project::test(fs, [Path::new(path!("/project"))], cx).await;
    let languages = project.read_with(cx, |project, _| project.languages().clone());
    languages.add(rust_lang());
    let mut capabilities = lsp::LanguageServer::full_capabilities();
    capabilities.hover_provider = Some(lsp::HoverProviderCapability::Simple(true));
    capabilities.execute_command_provider = Some(lsp::ExecuteCommandOptions {
        commands: vec!["decompile".into()],
        ..Default::default()
    });
    let mut servers = languages.register_fake_lsp(
        "Rust",
        FakeLspAdapter {
            name: "kotlin-lsp",
            capabilities,
            ..Default::default()
        },
    );
    let (_source, _source_handle) = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/project/main.rs"), cx)
        })
        .await
        .expect("source should open");
    let server = servers.next().await.expect("official server should start");
    let server_id = server.server.server_id();
    cx.run_until_parked();

    let source_uri: Uri = "jar:///dependency.jar!/Source.class"
        .parse()
        .expect("valid source URI");
    let target_uri: Uri = "jar:///dependency.jar!/Target.class"
        .parse()
        .expect("valid target URI");
    let fetched = Arc::new(Mutex::new(Vec::new()));
    server.set_request_handler::<lsp::request::ExecuteCommand, _, _>({
        let fetched = fetched.clone();
        move |params, _| {
            assert_eq!(params.command, "decompile");
            fetched.lock().push(params.arguments);
            async move {
                Ok(Some(json!({
                    "code": "pub fn dependency() {}",
                    "language": "rust"
                })))
            }
        }
    });
    let library = project
        .update(cx, |project, cx| {
            project.open_local_buffer_via_lsp(source_uri.clone(), server_id, cx)
        })
        .await
        .expect("library should open");
    let _library_handle = project.update(cx, |project, cx| {
        project.register_buffer_with_language_servers(&library, cx)
    });
    cx.run_until_parked();

    let (request_started, request_received) = futures::channel::oneshot::channel();
    let (release_response, response_released) = futures::channel::oneshot::channel();
    server.set_request_handler::<lsp::request::GotoDefinition, _, _>({
        let source_uri = source_uri.clone();
        let mut request_started = Some(request_started);
        let mut response_released = Some(response_released);
        move |params, _| {
            assert_eq!(
                params.text_document_position_params.text_document.uri,
                source_uri
            );
            request_started
                .take()
                .expect("one definition request")
                .send(())
                .expect("definition request is observed");
            let response_released = response_released.take().expect("one response");
            let target_uri = target_uri.clone();
            async move {
                response_released
                    .await
                    .expect("response should be released");
                Ok(Some(lsp::GotoDefinitionResponse::Scalar(
                    lsp::Location::new(
                        target_uri,
                        lsp::Range::new(lsp::Position::new(0, 7), lsp::Position::new(0, 17)),
                    ),
                )))
            }
        }
    });
    let (hover_started, hover_received) = futures::channel::oneshot::channel();
    let (release_hover, hover_released) = futures::channel::oneshot::channel();
    server.set_request_handler::<lsp::request::HoverRequest, _, _>({
        let source_uri = source_uri.clone();
        let mut hover_started = Some(hover_started);
        let mut hover_released = Some(hover_released);
        move |params, _| {
            assert_eq!(
                params.text_document_position_params.text_document.uri,
                source_uri
            );
            hover_started
                .take()
                .expect("one hover request")
                .send(())
                .expect("hover request is observed");
            let hover_released = hover_released.take().expect("one hover response");
            async move {
                hover_released.await.expect("hover should be released");
                Ok(Some(lsp::Hover {
                    contents: lsp::HoverContents::Scalar(lsp::MarkedString::String(
                        "obsolete documentation".into(),
                    )),
                    range: None,
                }))
            }
        }
    });
    let pending_definition = project.update(cx, |project, cx| project.definitions(&library, 7, cx));
    let pending_hover = project.update(cx, |project, cx| project.hover(&library, 7, cx));
    cx.run_until_parked();
    request_received
        .now_or_never()
        .expect("definition is in flight")
        .expect("definition handler started");
    hover_received
        .now_or_never()
        .expect("hover is in flight")
        .expect("hover handler started");

    enum ImportLog {}
    impl lsp::notification::Notification for ImportLog {
        type Params = serde_json::Value;
        const METHOD: &'static str = "intellij/importLog";
    }
    server.notify::<ImportLog>(json!({"started": true}));
    cx.run_until_parked();
    release_response.send(()).expect("request is still pending");
    release_hover.send(()).expect("hover is still pending");

    let definitions = pending_definition
        .await
        .expect("definition request should complete")
        .expect("definition response");
    assert!(
        definitions.is_empty(),
        "an old model's definition response must be rejected"
    );
    assert_eq!(
        *fetched.lock(),
        vec![vec![json!(source_uri)]],
        "an obsolete response must not decompile its target in the new model"
    );
    assert!(
        pending_hover.await.is_none_or(|hovers| hovers.is_empty()),
        "an old model's hover response must be rejected"
    );
}

#[derive(Default)]
struct VirtualDocumentRpcClient {
    outbound: Mutex<Option<futures::channel::mpsc::UnboundedSender<rpc::proto::Envelope>>>,
    pending: Mutex<
        HashMap<u32, futures::channel::oneshot::Sender<anyhow::Result<rpc::proto::Envelope>>>,
    >,
    next_id: std::sync::atomic::AtomicU32,
    handlers: Mutex<rpc::ProtoMessageHandlerSet>,
}

impl rpc::ProtoClient for VirtualDocumentRpcClient {
    fn request(
        &self,
        mut envelope: rpc::proto::Envelope,
        _: &'static str,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<rpc::proto::Envelope>> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        envelope.id = id;
        let (sender, receiver) = futures::channel::oneshot::channel();
        self.pending.lock().insert(id, sender);
        let sent = self.send(envelope, "request");
        async move {
            sent?;
            receiver.await?
        }
        .boxed()
    }

    fn send(&self, envelope: rpc::proto::Envelope, _: &'static str) -> anyhow::Result<()> {
        self.outbound
            .lock()
            .as_ref()
            .expect("connected endpoint")
            .unbounded_send(envelope)?;
        Ok(())
    }

    fn send_response(
        &self,
        envelope: rpc::proto::Envelope,
        name: &'static str,
    ) -> anyhow::Result<()> {
        self.send(envelope, name)
    }

    fn message_handler_set(&self) -> &Mutex<rpc::ProtoMessageHandlerSet> {
        &self.handlers
    }
    fn is_via_collab(&self) -> bool {
        true
    }
    fn has_wsl_interop(&self) -> bool {
        false
    }
}

fn virtual_document_rpc_pair(
    cx: &mut TestAppContext,
) -> (
    rpc::AnyProtoClient,
    rpc::AnyProtoClient,
    Vec<gpui::Task<()>>,
) {
    let (host_sender, host_receiver) = futures::channel::mpsc::unbounded();
    let (guest_sender, guest_receiver) = futures::channel::mpsc::unbounded();
    let host = Arc::new(VirtualDocumentRpcClient::default());
    let guest = Arc::new(VirtualDocumentRpcClient::default());
    *host.outbound.lock() = Some(guest_sender);
    *guest.outbound.lock() = Some(host_sender);
    let host_client = rpc::AnyProtoClient::new(host.clone());
    let guest_client = rpc::AnyProtoClient::new(guest.clone());
    let tasks = [
        (host, host_client.clone(), host_receiver),
        (guest, guest_client.clone(), guest_receiver),
    ]
    .into_iter()
    .map(|(endpoint, client, mut receiver)| {
        cx.spawn(async move |cx| {
            while let Some(envelope) = receiver.next().await {
                if let Some(id) = envelope.responding_to {
                    endpoint
                        .pending
                        .lock()
                        .remove(&id)
                        .expect("pending response")
                        .send(Ok(envelope))
                        .expect("request receiver");
                    continue;
                }
                let message = rpc::proto::build_typed_envelope(
                    rpc::proto::PeerId::default(),
                    Instant::now(),
                    envelope,
                )
                .expect("typed RPC message");
                if let Some(handler) = rpc::ProtoMessageHandlerSet::handle_message(
                    &endpoint.handlers,
                    message,
                    client.clone(),
                    cx.clone(),
                ) {
                    handler.await.expect("RPC handler");
                }
            }
        })
    })
    .collect();
    (host_client, guest_client, tasks)
}

#[gpui::test]
async fn test_shared_kotlin_virtual_documents_preserve_language_and_owner(cx: &mut TestAppContext) {
    use gpui::AppContext as _;
    use language::{Capability, Language, LanguageConfig, LanguageRegistry};
    use project::buffer_store::BufferStore;

    init_test(cx);
    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(path!("/project"), json!({"main.rs": "fn main() {}"}))
        .await;
    let project = Project::test(fs, [Path::new(path!("/project"))], cx).await;
    let languages = project.read_with(cx, |project, _| project.languages().clone());
    languages.add(rust_lang());
    let java = Arc::new(Language::new(
        LanguageConfig {
            name: "Java".into(),
            ..Default::default()
        },
        None,
    ));
    languages.add(java.clone());
    let mut capabilities = lsp::LanguageServer::full_capabilities();
    capabilities.execute_command_provider = Some(lsp::ExecuteCommandOptions {
        commands: vec!["decompile".into()],
        ..Default::default()
    });
    capabilities.definition_provider = None;
    capabilities.hover_provider = None;
    let mut servers = languages.register_fake_lsp(
        "Rust",
        FakeLspAdapter {
            name: "kotlin-lsp",
            capabilities,
            ..Default::default()
        },
    );
    let (_, _source_handle) = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/project/main.rs"), cx)
        })
        .await
        .expect("source buffer");
    let server = servers.next().await.expect("Kotlin server");
    cx.run_until_parked();
    let server_id = server.server.server_id();
    let jar: Uri = "jar:///cache%20directory/sdk.jar!/Dependency.class"
        .parse()
        .expect("jar URI");
    let jrt: Uri = "jrt:///jdk%2025!/java.base/java/lang/String.class"
        .parse()
        .expect("JRT URI");
    server.set_request_handler::<lsp::request::ExecuteCommand, _, _>(|_, _| async {
        Ok(Some(
            json!({"code":"class Dependency {}", "language":"java"}),
        ))
    });
    let received = Arc::new(Mutex::new(Vec::new()));
    server.set_request_handler::<lsp::request::GotoDefinition, _, _>({
        let received = received.clone();
        let jrt = jrt.clone();
        move |params, _| {
            received
                .lock()
                .push(params.text_document_position_params.text_document.uri);
            let jrt = jrt.clone();
            async move {
                Ok(Some(lsp::GotoDefinitionResponse::Scalar(
                    lsp::Location::new(jrt, lsp::Range::default()),
                )))
            }
        }
    });
    server.set_request_handler::<lsp::request::HoverRequest, _, _>({
        let received = received.clone();
        move |params, _| {
            received
                .lock()
                .push(params.text_document_position_params.text_document.uri);
            async {
                Ok(Some(lsp::Hover {
                    contents: lsp::HoverContents::Scalar(lsp::MarkedString::String(
                        "JDK documentation".into(),
                    )),
                    range: None,
                }))
            }
        }
    });
    server
        .request::<lsp::request::RegisterCapability>(
            lsp::RegistrationParams {
                registrations: vec![
                    lsp::Registration {
                        id: "library-definition".into(),
                        method: "textDocument/definition".into(),
                        register_options: Some(
                            json!({"documentSelector":[{"scheme":"jar","language":"java"}]}),
                        ),
                    },
                    lsp::Registration {
                        id: "library-hover".into(),
                        method: "textDocument/hover".into(),
                        register_options: Some(
                            json!({"documentSelector":[{"scheme":"jrt","language":"java"}]}),
                        ),
                    },
                ],
            },
            lsp::DEFAULT_LSP_REQUEST_TIMEOUT,
        )
        .await
        .into_response()
        .expect("dynamic library capabilities");
    let library = project
        .update(cx, |project, cx| {
            project.open_local_buffer_via_lsp(jar.clone(), server_id, cx)
        })
        .await
        .expect("host library");
    let _library_handle = project.update(cx, |project, cx| {
        project.register_buffer_with_language_servers(&library, cx)
    });
    cx.run_until_parked();

    let (host_client, guest_client, _rpc_tasks) = virtual_document_rpc_pair(cx);
    LspStore::init(&host_client);
    LspStore::init(&guest_client);
    let (host_buffers, host_lsp, worktree_metadata) = project.read_with(cx, |project, cx| {
        (
            project.buffer_store().clone(),
            project.lsp_store(),
            project
                .worktrees(cx)
                .next()
                .expect("host worktree")
                .read(cx)
                .metadata_proto(),
        )
    });
    host_client.subscribe_to_entity(1, &host_lsp);
    let guest_languages = Arc::new(LanguageRegistry::new(cx.background_executor.clone()));
    guest_languages.add(java);
    assert!(guest_languages.lsp_adapters(&"Java".into()).is_empty());
    let worktrees = cx.new(|cx| {
        project::worktree_store::WorktreeStore::remote(
            true,
            guest_client.clone(),
            1,
            util::paths::PathStyle::Unix,
            project::worktree_store::WorktreeIdCounter::get(cx),
        )
    });
    let worktree = cx.update(|cx| {
        worktree::Worktree::remote(
            1,
            language::ReplicaId::new(1),
            worktree_metadata,
            guest_client.clone(),
            util::paths::PathStyle::Unix,
            cx,
        )
    });
    worktrees.update(cx, |worktrees, cx| worktrees.add(&worktree, cx));
    let guest_buffers =
        cx.new(|cx| BufferStore::remote(worktrees.clone(), guest_client.clone(), 1, cx));
    let guest_lsp = cx.new(|cx| {
        LspStore::new_remote(
            guest_buffers.clone(),
            worktrees,
            guest_languages,
            guest_client.clone(),
            1,
            cx,
        )
    });
    guest_client.subscribe_to_entity(1, &guest_lsp);
    guest_client.subscribe_to_entity(1, &guest_buffers);
    guest_client.add_entity_message_handler(
        |buffers: Entity<BufferStore>,
         message: rpc::TypedEnvelope<rpc::proto::CreateBufferForPeer>,
         mut cx: gpui::AsyncApp| async move {
            buffers.update(&mut cx, |buffers, cx| {
                buffers.handle_create_buffer_for_peer(
                    message,
                    language::ReplicaId::new(1),
                    Capability::ReadWrite,
                    cx,
                )
            })
        },
    );
    host_buffers.update(cx, |buffers, cx| buffers.shared(1, host_client.clone(), cx));
    host_lsp.update(cx, |store, cx| store.shared(1, host_client.clone(), cx));
    host_buffers
        .update(cx, |buffers, cx| {
            buffers.create_buffer_for_peer(&library, rpc::proto::PeerId::default(), cx)
        })
        .await
        .expect("share library state and chunks");
    cx.run_until_parked();
    let library_id = library.read_with(cx, |buffer, _| buffer.remote_id());
    let guest_library = guest_buffers
        .read_with(cx, |buffers, _| buffers.get_existing(library_id))
        .expect("shared class buffer");
    guest_library.read_with(cx, |buffer, _| {
        assert_eq!(
            buffer
                .language()
                .expect("reported language")
                .name()
                .as_ref(),
            "Java"
        );
        assert_eq!(
            buffer
                .language_server_document()
                .expect("shared metadata")
                .uri,
            jar
        );
        assert!(buffer.read_only());
    });
    assert_eq!(
        guest_lsp.read_with(cx, |store, cx| store
            .relevant_server_ids_for_capability_check(&guest_library, cx)),
        collections::HashSet::from_iter([server_id])
    );
    let definitions = guest_lsp
        .update(cx, |store, cx| {
            store.definitions(&guest_library, language::PointUtf16::new(0, 7), cx)
        })
        .await
        .expect("guest RPC")
        .expect("guest definition");
    let runtime = definitions
        .first()
        .expect("nested JRT target")
        .target
        .buffer
        .clone();
    cx.run_until_parked();
    assert_eq!(
        runtime.read_with(cx, |buffer, _| buffer
            .language_server_document()
            .expect("nested metadata")
            .uri
            .clone()),
        jrt
    );
    let runtime_id = runtime.read_with(cx, |buffer, _| buffer.remote_id());
    let host_runtime = host_buffers
        .read_with(cx, |buffers, _| buffers.get_existing(runtime_id))
        .expect("authoritative runtime buffer");
    let _runtime_handle = project.update(cx, |project, cx| {
        project.register_buffer_with_language_servers(&host_runtime, cx)
    });
    assert!(
        guest_lsp
            .update(cx, |store, cx| store.hover(
                &runtime,
                language::PointUtf16::new(0, 7),
                cx
            ))
            .await
            .is_some_and(|hovers| !hovers.is_empty())
    );
    assert_eq!(*received.lock(), vec![jar, jrt]);
    assert_eq!(worktree_roots(&project, cx).len(), 1);
    assert!(servers.try_recv().is_err());

    let malformed_id = text::BufferId::new(99_000).expect("unused remote buffer id");
    let failed_load = guest_buffers.update(cx, |buffers, cx| {
        buffers.wait_for_remote_buffer(malformed_id, cx)
    });
    let mut malformed_state = library.read_with(cx, |buffer, cx| buffer.to_proto(cx));
    malformed_state.id = malformed_id.into();
    malformed_state
        .language_server_document
        .as_mut()
        .expect("server metadata")
        .uri = "file:///unexpected.class".into();
    guest_buffers
        .update(cx, |buffers, cx| {
            buffers.handle_create_buffer_for_peer(
                rpc::TypedEnvelope {
                    sender_id: rpc::proto::PeerId::default(),
                    original_sender_id: None,
                    message_id: 0,
                    received_at: Instant::now(),
                    payload: rpc::proto::CreateBufferForPeer {
                        project_id: 1,
                        peer_id: None,
                        variant: Some(rpc::proto::create_buffer_for_peer::Variant::State(
                            malformed_state,
                        )),
                    },
                },
                language::ReplicaId::new(1),
                Capability::ReadWrite,
                cx,
            )
        })
        .expect("malformed state should notify its waiting load");
    assert!(
        failed_load
            .await
            .expect_err("malformed metadata load must fail")
            .to_string()
            .contains("Unsupported language server document URI scheme")
    );

    let mut legacy_state = library.read_with(cx, |buffer, cx| buffer.to_proto(cx));
    legacy_state.language_server_document = None;
    let legacy_replica = cx.new(|cx| {
        Buffer::from_proto(
            language::ReplicaId::new(2),
            Capability::ReadWrite,
            legacy_state,
            None,
            cx,
        )
        .expect("legacy guest replica")
    });
    legacy_replica.update(cx, |buffer, cx| {
        buffer.edit([(0..0, "changed ")], None, cx);
        buffer.undo(cx).expect("legacy edit transaction");
    });
    let operations = legacy_replica
        .update(cx, |buffer, cx| buffer.serialize_ops(None, cx))
        .await;
    let mut rejected = 0;
    for operation in operations {
        if !matches!(
            operation.variant,
            Some(rpc::proto::operation::Variant::Edit(_) | rpc::proto::operation::Variant::Undo(_))
        ) {
            continue;
        }
        let response = BufferStore::handle_update_buffer(
            host_buffers.clone(),
            rpc::TypedEnvelope {
                sender_id: rpc::proto::PeerId::default(),
                original_sender_id: None,
                message_id: 0,
                received_at: Instant::now(),
                payload: rpc::proto::UpdateBuffer {
                    project_id: 1,
                    buffer_id: library_id.into(),
                    operations: vec![operation],
                },
            },
            cx.to_async(),
        )
        .await;
        assert!(
            response
                .expect_err("legacy text/undo must be rejected")
                .to_string()
                .contains("read-only language server document")
        );
        rejected += 1;
    }
    assert_eq!(rejected, 2);
    BufferStore::handle_update_buffer(
        host_buffers.clone(),
        rpc::TypedEnvelope {
            sender_id: rpc::proto::PeerId::default(),
            original_sender_id: None,
            message_id: 0,
            received_at: Instant::now(),
            payload: rpc::proto::UpdateBuffer {
                project_id: 1,
                buffer_id: library_id.into(),
                operations: vec![rpc::proto::Operation {
                    variant: Some(rpc::proto::operation::Variant::UpdateSelections(
                        rpc::proto::operation::UpdateSelections {
                            replica_id: 2,
                            lamport_timestamp: 10,
                            selections: vec![],
                            line_mode: false,
                            cursor_shape: 0,
                        },
                    )),
                }],
            },
        },
        cx.to_async(),
    )
    .await
    .expect("selection updates remain allowed");
    assert_eq!(
        library.read_with(cx, |buffer, _| buffer.text()),
        "class Dependency {}"
    );

    project.update(cx, |project, cx| {
        project.mark_as_collab_for_testing();
        for role in [
            rpc::proto::ChannelRole::Guest,
            rpc::proto::ChannelRole::Member,
            rpc::proto::ChannelRole::Guest,
            rpc::proto::ChannelRole::Admin,
        ] {
            project.set_role(role, cx);
            assert!(library.read(cx).read_only());
            assert!(host_runtime.read(cx).read_only());
        }
    });
    guest_library.update(cx, |buffer, cx| {
        buffer.set_capability(Capability::ReadWrite, cx)
    });
    assert!(guest_library.read_with(cx, |buffer, _| buffer.read_only()));
}

#[gpui::test]
async fn test_open_buffer_via_lsp_case_variant_no_duplicate(cx: &mut TestAppContext) {
    init_test(cx);
    cx.executor().allow_parking();

    let fs = FakeFs::new(cx.executor());
    fs.set_case_sensitive(false);
    fs.insert_tree(
        path!("/root"),
        json!({ "src": { "main.rs": "fn main() {}" } }),
    )
    .await;

    let project = Project::test(fs.clone(), [path!("/root").as_ref()], cx).await;
    let language_registry = project.read_with(cx, |project, _| project.languages().clone());
    language_registry.add(rust_lang());
    let mut fake_servers = language_registry.register_fake_lsp("Rust", FakeLspAdapter::default());

    project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/root/src/main.rs"), cx)
        })
        .await
        .unwrap();
    fake_servers.next().await.unwrap();
    cx.run_until_parked();

    let server_id = project.read_with(cx, |project, cx| {
        project
            .lsp_store()
            .read(cx)
            .language_server_statuses()
            .next()
            .unwrap()
            .0
    });

    project
        .update(cx, |project, cx| {
            project.open_local_buffer_via_lsp(
                Uri::from_file_path(path!("/root/SRC/main.rs")).unwrap(),
                server_id,
                cx,
            )
        })
        .await
        .unwrap();
    cx.run_until_parked();

    project.read_with(cx, |project, cx| {
        let worktree = project.worktrees(cx).next().unwrap();
        let entries: Vec<_> = worktree
            .read(cx)
            .snapshot()
            .entries(true, 0)
            .map(|entry| entry.path.as_unix_str().to_string())
            .collect();
        assert_eq!(entries, vec!["", "src", "src/main.rs"]);
    });
}

#[gpui::test]
async fn test_open_buffer_via_lsp_preserves_external_symlink_path(cx: &mut TestAppContext) {
    init_test(cx);
    cx.executor().allow_parking();

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        path!("/shared"),
        json!({ "pkg": { "def.rs": "pub fn def() {}" } }),
    )
    .await;
    fs.insert_tree(
        path!("/project"),
        json!({ "src": { "main.rs": "fn main() {}" } }),
    )
    .await;
    fs.create_symlink(
        path!("/project/pkg").as_ref(),
        PathBuf::from(path!("/shared/pkg")),
    )
    .await
    .unwrap();

    let (project, server_id) =
        project_with_rust_server(fs, path!("/project"), path!("/project/src/main.rs"), cx).await;

    let buffer = project
        .update(cx, |project, cx| {
            project.open_local_buffer_via_lsp(
                Uri::from_file_path(path!("/project/pkg/def.rs")).unwrap(),
                server_id,
                cx,
            )
        })
        .await
        .unwrap();
    cx.run_until_parked();

    assert_eq!(
        buffer_paths(&buffer, cx),
        (
            "pkg/def.rs".to_string(),
            PathBuf::from(path!("/project/pkg/def.rs"))
        )
    );
    assert_eq!(
        worktree_roots(&project, cx),
        vec![PathBuf::from(path!("/project"))]
    );
}

#[gpui::test]
async fn test_open_buffer_via_lsp_case_variant_in_unscanned_dir(cx: &mut TestAppContext) {
    init_test(cx);
    cx.executor().allow_parking();

    let fs = FakeFs::new(cx.executor());
    fs.set_case_sensitive(false);
    fs.insert_tree(
        path!("/root"),
        json!({
            ".gitignore": "ignored\n",
            "src": { "main.rs": "fn main() {}" },
            "ignored": { "lib.rs": "pub fn lib() {}" },
        }),
    )
    .await;

    let (project, server_id) =
        project_with_rust_server(fs, path!("/root"), path!("/root/src/main.rs"), cx).await;
    assert_eq!(
        worktree_entries(&project, cx),
        vec!["", ".gitignore", "ignored", "src", "src/main.rs"]
    );

    let buffer = project
        .update(cx, |project, cx| {
            project.open_local_buffer_via_lsp(
                Uri::from_file_path(path!("/root/IGNORED/LIB.rs")).unwrap(),
                server_id,
                cx,
            )
        })
        .await
        .unwrap();
    cx.run_until_parked();

    assert_eq!(
        buffer_paths(&buffer, cx),
        (
            "ignored/lib.rs".to_string(),
            PathBuf::from(path!("/root/ignored/lib.rs"))
        )
    );
    assert_eq!(
        worktree_roots(&project, cx),
        vec![PathBuf::from(path!("/root"))]
    );
    assert_eq!(
        worktree_entries(&project, cx),
        vec![
            "",
            ".gitignore",
            "ignored",
            "ignored/lib.rs",
            "src",
            "src/main.rs"
        ]
    );
}

#[test]
fn test_rpc_log_grouping_separates_timed_messages() {
    for (received, direction) in [(false, "Send"), (true, "Receive")] {
        let mut header_state = TestRpcLogHeaderState::new();

        assert_eq!(
            header_state.header_for_message(received, None),
            Some(format!("\n// {direction}:"))
        );
        assert_eq!(header_state.header_for_message(received, None), None);
        assert_eq!(
            header_state.header_for_message(received, Some(Duration::from_millis(53))),
            Some(format!("\n// {direction} (took 53.0ms):"))
        );
        assert_eq!(
            header_state.header_for_message(received, None),
            Some(format!("\n// {direction}:"))
        );
        assert_eq!(header_state.header_for_message(received, None), None);
    }
}

#[test]
fn test_rpc_request_tracker_distinguishes_request_directions() {
    let mut tracker = TestRpcRequestTracker::new();
    let started_at = Instant::now();

    assert_eq!(
        tracker.observe(
            false,
            r#"{"jsonrpc":"2.0","id":1,"method":"textDocument/hover"}"#,
            started_at,
        ),
        None
    );
    assert_eq!(
        tracker.observe(
            true,
            r#"{"jsonrpc":"2.0","id":1,"method":"workspace/configuration"}"#,
            started_at + Duration::from_millis(10),
        ),
        None
    );
    assert_eq!(
        tracker.observe(
            false,
            r#"{"jsonrpc":"2.0","id":1,"result":[]}"#,
            started_at + Duration::from_millis(30),
        ),
        Some(Duration::from_millis(20))
    );
    assert_eq!(
        tracker.observe(
            true,
            r#"{"jsonrpc":"2.0","id":1,"result":null}"#,
            started_at + Duration::from_millis(50),
        ),
        Some(Duration::from_millis(50))
    );
}

#[test]
fn test_rpc_request_tracker_decodes_ids_and_times_cancelled_requests() {
    let mut tracker = TestRpcRequestTracker::new();
    let started_at = Instant::now();

    tracker.observe(
        true,
        r#"{"jsonrpc":"2.0","id":"foo\u002fbar","method":"workspace/configuration"}"#,
        started_at,
    );
    assert_eq!(
        tracker.observe(
            false,
            r#"{"jsonrpc":"2.0","id":"foo/bar","result":[]}"#,
            started_at + Duration::from_millis(25),
        ),
        Some(Duration::from_millis(25))
    );

    tracker.observe(
        false,
        r#"{"jsonrpc":"2.0","id":7,"method":"textDocument/hover"}"#,
        started_at,
    );
    tracker.observe(
        false,
        r#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":7}}"#,
        started_at + Duration::from_millis(1),
    );
    assert_eq!(tracker.pending_request_count(), 1);
    assert_eq!(
        tracker.observe(
            true,
            r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32800,"message":"Request was cancelled"}}"#,
            started_at + Duration::from_millis(10),
        ),
        Some(Duration::from_millis(10))
    );
    assert_eq!(tracker.pending_request_count(), 0);
}

#[test]
fn test_rpc_request_tracker_bounds_unanswered_requests() {
    let mut tracker = TestRpcRequestTracker::new();
    let started_at = Instant::now();
    let max_pending_requests = TestRpcRequestTracker::max_pending_requests();

    for id in 0..=max_pending_requests {
        tracker.observe(
            false,
            &format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"textDocument/hover"}}"#),
            started_at + Duration::from_nanos(id as u64),
        );
    }

    assert_eq!(tracker.pending_request_count(), max_pending_requests);
    assert_eq!(
        tracker.observe(
            true,
            r#"{"jsonrpc":"2.0","id":0,"result":null}"#,
            started_at + Duration::from_secs(1),
        ),
        None
    );
    assert!(
        tracker
            .observe(
                true,
                r#"{"jsonrpc":"2.0","id":1,"result":null}"#,
                started_at + Duration::from_secs(1),
            )
            .is_some()
    );
}

#[test]
fn test_rpc_log_duration_proto_roundtrip() {
    let log_type = LanguageServerLogType::Rpc {
        received: true,
        elapsed: Some(Duration::from_micros(1234)),
    };

    assert_eq!(
        LanguageServerLogType::from_proto(log_type.to_proto()),
        log_type
    );
}

#[test]
fn test_glob_literal_prefix() {
    assert_eq!(glob_literal_prefix(Path::new("**/*.js")), Path::new(""));
    assert_eq!(
        glob_literal_prefix(Path::new("node_modules/**/*.js")),
        Path::new("node_modules")
    );
    assert_eq!(
        glob_literal_prefix(Path::new("foo/{bar,baz}.js")),
        Path::new("foo")
    );
    assert_eq!(
        glob_literal_prefix(Path::new("foo/bar/baz.js")),
        Path::new("foo/bar/baz.js")
    );

    #[cfg(target_os = "windows")]
    {
        assert_eq!(glob_literal_prefix(Path::new("**\\*.js")), Path::new(""));
        assert_eq!(
            glob_literal_prefix(Path::new("node_modules\\**/*.js")),
            Path::new("node_modules")
        );
        assert_eq!(
            glob_literal_prefix(Path::new("foo/{bar,baz}.js")),
            Path::new("foo")
        );
        assert_eq!(
            glob_literal_prefix(Path::new("foo\\bar\\baz.js")),
            Path::new("foo/bar/baz.js")
        );
    }
}

#[test]
fn test_multi_len_chars_normalization() {
    let mut label = CodeLabel::new(
        "myElˇ (parameter) myElˇ: {\n    foo: string;\n}".to_string(),
        0..6,
        vec![(0..6, HighlightId::new(1))],
    );
    ensure_uniform_list_compatible_label(&mut label);
    assert_eq!(
        label,
        CodeLabel::new(
            "myElˇ (parameter) myElˇ: { foo: string; }".to_string(),
            0..6,
            vec![(0..6, HighlightId::new(1))],
        )
    );
}

#[test]
fn test_completion_label_snippet_normalization() {
    for line_ending in ["\n", "\r\n", "\r"] {
        let text = "
            #[cfg(test)]
            mod tests {
                use super::*;

                #[test]
                fn test_name() {

                }
            }"
        .unindent()
        .replace('\n', line_ending);
        let name_start = text.find("test_name").expect("snippet has a test name");
        let text_len = text.len();
        let mut label = CodeLabel::new(
            text,
            0..text_len,
            vec![
                (0..12, HighlightId::new(1)),
                (name_start..name_start + 9, HighlightId::TABSTOP_REPLACE_ID),
            ],
        );

        ensure_uniform_list_compatible_label(&mut label);

        assert_eq!(
            label,
            CodeLabel::new(
                "#[cfg(test)] mod tests { use super::*; #[test] fn test_name() { } }".to_string(),
                0..67,
                vec![
                    (0..12, HighlightId::new(1)),
                    (50..59, HighlightId::TABSTOP_REPLACE_ID),
                ],
            ),
            "line ending: {line_ending:?}",
        );
    }
}

#[test]
fn test_completion_label_unicode_normalization() {
    for line_ending in ["\n", "\r\n", "\r"] {
        let text = "
            héllo {
                🦀value: 世界,
            }"
        .unindent()
        .replace('\n', line_ending);
        let value_start = text.find("🦀value").expect("label has a value");
        let type_start = text.find("世界").expect("label has a type");
        let text_len = text.len();
        let mut label = CodeLabel::new(
            text,
            value_start..value_start + 9,
            vec![
                (0..text_len, HighlightId::new(0)),
                (0..6, HighlightId::new(1)),
                (
                    value_start..value_start + 9,
                    HighlightId::TABSTOP_REPLACE_ID,
                ),
                (type_start..type_start + 6, HighlightId::new(2)),
            ],
        );

        ensure_uniform_list_compatible_label(&mut label);

        assert_eq!(
            label,
            CodeLabel::new(
                "héllo { 🦀value: 世界, }".to_string(),
                9..18,
                vec![
                    (0..29, HighlightId::new(0)),
                    (0..6, HighlightId::new(1)),
                    (9..18, HighlightId::TABSTOP_REPLACE_ID),
                    (20..26, HighlightId::new(2)),
                ],
            ),
            "line ending: {line_ending:?}",
        );
    }
}

#[test]
fn test_completion_label_whitespace_normalization() {
    for line_ending in ["\n", "\r\n", "\r", "\n\r", "\r\r\n"] {
        for before in ["", " ", " \t "] {
            for after in ["", " ", " \t "] {
                let mut label = CodeLabel::plain(format!("{before}{line_ending}{after}"), None);
                ensure_uniform_list_compatible_label(&mut label);
                assert_eq!(label, CodeLabel::plain(" ".to_string(), None));
            }
        }
    }

    for text in ["", " ", " \t ", "héllo  世界", "héllo\t世界"] {
        let mut label = CodeLabel::plain(text.to_string(), None);
        let expected = label.clone();
        ensure_uniform_list_compatible_label(&mut label);
        assert_eq!(label, expected);
    }
}

#[test]
fn test_trailing_newline_in_completion_documentation() {
    let doc =
        lsp::Documentation::String("Inappropriate argument value (of correct type).\n".to_string());
    let completion_doc: CompletionDocumentation = doc.into();
    assert!(
        matches!(completion_doc, CompletionDocumentation::SingleLine(s) if s == "Inappropriate argument value (of correct type).")
    );

    let doc = lsp::Documentation::String("  some value  \n".to_string());
    let completion_doc: CompletionDocumentation = doc.into();
    assert!(matches!(
        completion_doc,
        CompletionDocumentation::SingleLine(s) if s == "some value"
    ));
}

#[gpui::test]
async fn test_user_initialization_options_override_adapter_arrays(cx: &mut TestAppContext) {
    init_test(cx);

    let user_settings = serde_json::json!({
        "lsp": {
            "the-fake-language-server": {
                "initialization_options": {
                    "preview": {
                        "background": {
                            "enabled": true,
                            "args": ["--data-plane-host=127.0.0.1:23635", "--invert-colors=never"],
                        },
                    },
                    "plugins": ["user-plugin"],
                    "userOnly": ["user"],
                },
            },
        },
    });

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        path!("/the-root"),
        json!({
            ".zed": {
                "settings.json": user_settings.to_string(),
            },
            "main.rs": "fn main() {}",
        }),
    )
    .await;

    let project = Project::test(fs, [path!("/the-root").as_ref()], cx).await;
    let language_registry = project.read_with(cx, |project, _| project.languages().clone());
    language_registry.add(rust_lang());

    let sent_initialization_options = Arc::new(Mutex::new(None));
    let mut fake_servers = language_registry.register_fake_lsp(
        "Rust",
        FakeLspAdapter {
            name: "the-fake-language-server",
            initialization_options: Some(json!({
                "preview": {
                    "background": {
                        "args": ["--data-plane-host=127.0.0.1:23635", "--invert-colors=never"],
                        "partialRendering": true,
                    },
                },
                "plugins": ["default-plugin", "user-plugin"],
                "adapterOnly": [1, 2],
            })),
            initializer: Some(Box::new({
                let sent_initialization_options = sent_initialization_options.clone();
                move |fake_server| {
                    let sent_initialization_options = sent_initialization_options.clone();
                    fake_server.set_request_handler::<lsp::request::Initialize, _, _>(
                        move |params, _| {
                            *sent_initialization_options.lock() = params.initialization_options;
                            async move { Ok(lsp::InitializeResult::default()) }
                        },
                    );
                }
            })),
            ..FakeLspAdapter::default()
        },
    );
    cx.run_until_parked();

    project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/the-root/main.rs"), cx)
        })
        .await
        .unwrap();
    fake_servers.next().await.unwrap();
    cx.run_until_parked();

    assert_eq!(
        sent_initialization_options.lock().take(),
        Some(json!({
            "preview": {
                "background": {
                    "enabled": true,
                    "args": ["--data-plane-host=127.0.0.1:23635", "--invert-colors=never"],
                    "partialRendering": true,
                },
            },
            "plugins": ["user-plugin"],
            "adapterOnly": [1, 2],
            "userOnly": ["user"],
        })),
    );
}

#[gpui::test]
async fn test_other_adapters_lsp_configuration_contributions_are_unioned(cx: &mut TestAppContext) {
    init_test(cx);

    let user_settings = serde_json::json!({
        "lsp": {
            "the-fake-language-server": {
                "initialization_options": {
                    "languages": ["user-lang"],
                    "userOnly": true,
                },
            },
        },
    });

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        path!("/the-root"),
        json!({
            ".zed": {
                "settings.json": user_settings.to_string(),
            },
            "main.rs": "fn main() {}",
        }),
    )
    .await;

    let project = Project::test(fs, [path!("/the-root").as_ref()], cx).await;
    let language_registry = project.read_with(cx, |project, _| project.languages().clone());
    language_registry.add(rust_lang());

    let main_server_name = LanguageServerName("the-fake-language-server".into());
    for (language, server_name, plugin, lang, memory) in [
        ("Vue", "vue-language-server", "vue-plugin", "vue", 4096),
        (
            "Astro",
            "astro-language-server",
            "astro-plugin",
            "astro",
            2048,
        ),
    ] {
        let contribution = json!({
            "tsserver": {
                "globalPlugins": ["shared-plugin", plugin],
                "maxMemory": memory,
            },
            "languages": [lang],
        });
        language_registry.register_fake_lsp_adapter(
            language,
            FakeLspAdapter {
                name: server_name,
                additional_initialization_options: HashMap::from_iter([(
                    main_server_name.clone(),
                    contribution.clone(),
                )]),
                additional_workspace_configuration: HashMap::from_iter([(
                    main_server_name.clone(),
                    contribution,
                )]),
                ..FakeLspAdapter::default()
            },
        );
    }

    let sent_initialization_options = Arc::new(Mutex::new(None));
    let mut fake_servers = language_registry.register_fake_lsp(
        "Rust",
        FakeLspAdapter {
            name: "the-fake-language-server",
            initialization_options: Some(json!({
                "tsserver": {
                    "globalPlugins": ["default-plugin"],
                },
                "languages": ["default-lang"],
            })),
            initializer: Some(Box::new({
                let sent_initialization_options = sent_initialization_options.clone();
                move |fake_server| {
                    let sent_initialization_options = sent_initialization_options.clone();
                    fake_server.set_request_handler::<lsp::request::Initialize, _, _>(
                        move |params, _| {
                            *sent_initialization_options.lock() = params.initialization_options;
                            async move { Ok(lsp::InitializeResult::default()) }
                        },
                    );
                }
            })),
            ..FakeLspAdapter::default()
        },
    );
    cx.run_until_parked();

    project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/the-root/main.rs"), cx)
        })
        .await
        .unwrap();
    let mut fake_server = fake_servers.next().await.unwrap();
    let workspace_configuration = fake_server
        .receive_notification::<lsp::notification::DidChangeConfiguration>()
        .await
        .settings;
    cx.run_until_parked();

    assert_eq!(
        sent_initialization_options.lock().take(),
        Some(json!({
            "tsserver": {
                "globalPlugins": ["default-plugin", "shared-plugin", "astro-plugin", "vue-plugin"],
                "maxMemory": 4096,
            },
            "languages": ["user-lang"],
            "userOnly": true,
        })),
    );
    assert_eq!(
        workspace_configuration,
        json!({
            "tsserver": {
                "globalPlugins": ["shared-plugin", "astro-plugin", "vue-plugin"],
                "maxMemory": 4096,
            },
            "languages": ["astro", "vue"],
        }),
    );
}

#[gpui::test]
async fn test_initialization_options_contributions_without_own_options(cx: &mut TestAppContext) {
    init_test(cx);

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(path!("/the-root"), json!({ "main.rs": "fn main() {}" }))
        .await;

    let project = Project::test(fs, [path!("/the-root").as_ref()], cx).await;
    let language_registry = project.read_with(cx, |project, _| project.languages().clone());
    language_registry.add(rust_lang());

    let contribution = json!({
        "tsserver": {
            "globalPlugins": ["vue-plugin"],
        },
    });
    language_registry.register_fake_lsp_adapter(
        "Vue",
        FakeLspAdapter {
            name: "vue-language-server",
            additional_initialization_options: HashMap::from_iter([(
                LanguageServerName("the-fake-language-server".into()),
                contribution.clone(),
            )]),
            ..FakeLspAdapter::default()
        },
    );

    let sent_initialization_options = Arc::new(Mutex::new(None));
    let mut fake_servers = language_registry.register_fake_lsp(
        "Rust",
        FakeLspAdapter {
            name: "the-fake-language-server",
            initialization_options: None,
            initializer: Some(Box::new({
                let sent_initialization_options = sent_initialization_options.clone();
                move |fake_server| {
                    let sent_initialization_options = sent_initialization_options.clone();
                    fake_server.set_request_handler::<lsp::request::Initialize, _, _>(
                        move |params, _| {
                            *sent_initialization_options.lock() =
                                Some(params.initialization_options);
                            async move { Ok(lsp::InitializeResult::default()) }
                        },
                    );
                }
            })),
            ..FakeLspAdapter::default()
        },
    );
    cx.run_until_parked();

    project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/the-root/main.rs"), cx)
        })
        .await
        .unwrap();
    fake_servers.next().await.unwrap();
    cx.run_until_parked();

    assert_eq!(
        sent_initialization_options.lock().take(),
        Some(Some(contribution)),
    );
}

async fn project_with_rust_server(
    fs: Arc<FakeFs>,
    root: &str,
    first_file: &str,
    cx: &mut TestAppContext,
) -> (Entity<Project>, LanguageServerId) {
    let project = Project::test(fs, [root.as_ref()], cx).await;
    let language_registry = project.read_with(cx, |project, _| project.languages().clone());
    language_registry.add(rust_lang());
    let mut fake_servers = language_registry.register_fake_lsp("Rust", FakeLspAdapter::default());

    project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(first_file, cx)
        })
        .await
        .unwrap();
    fake_servers.next().await.unwrap();
    cx.run_until_parked();

    let server_id = project.read_with(cx, |project, cx| {
        project
            .lsp_store()
            .read(cx)
            .language_server_statuses()
            .next()
            .unwrap()
            .0
    });
    (project, server_id)
}

fn buffer_paths(buffer: &Entity<Buffer>, cx: &TestAppContext) -> (String, PathBuf) {
    buffer.read_with(cx, |buffer, cx| {
        let file = File::from_dyn(buffer.file()).unwrap();
        (file.path.as_unix_str().to_string(), file.abs_path(cx))
    })
}

fn worktree_roots(project: &Entity<Project>, cx: &TestAppContext) -> Vec<PathBuf> {
    project.read_with(cx, |project, cx| {
        project
            .worktrees(cx)
            .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
            .collect()
    })
}

fn worktree_entries(project: &Entity<Project>, cx: &TestAppContext) -> Vec<String> {
    project.read_with(cx, |project, cx| {
        let worktree = project.worktrees(cx).next().unwrap();
        worktree
            .read(cx)
            .snapshot()
            .entries(true, 0)
            .map(|entry| entry.path.as_unix_str().to_string())
            .collect()
    })
}
