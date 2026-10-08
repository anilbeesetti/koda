use android_tools::project_context::{
    ActiveContext, CONTEXT_OUTPUT_PREFIX, ContextCapabilities, ContextSnapshot, ContextStore,
    MAX_CONTEXT_RECORD_BYTES, ModuleOwner, ObservationPhase, OperationalReadiness, PluginId,
    decode_context_output, decode_context_record,
};
use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn project_root(name: &str) -> PathBuf {
    std::env::temp_dir()
        .join("koda-project-context-policy")
        .join(name)
}

#[test]
fn java_only_android_uses_public_agp_facts_without_inventing_kotlin_targets() -> Result<()> {
    let root = project_root("java-only-android");
    let mut value = catalogue(
        &root,
        vec![
            module(&root, ":", "", &[], &[]),
            module(&root, ":app", "app", &[PluginId::AndroidApplication], &[]),
        ],
    );
    value["modules"][1]["targets"] = json!({"status": "unavailable", "value": {"detail": "Kotlin extension has no public getTargets() API"}});
    let without_agp = decode(&root, &value)?;
    assert!(
        !without_agp
            .capabilities(None, readiness(&root))
            .android_devices
    );
    value["modules"][1]["android"] =
        json!({"status": "available", "value": {"pluginVersion": "9.2.0"}});
    let snapshot = decode(&root, &value)?;
    let capabilities =
        snapshot.capabilities(Some(&root.join("app/src/Main.java")), readiness(&root));
    assert!(capabilities.android_devices);
    assert!(capabilities.android_run);
    assert!(!capabilities.android_compose_preview);
    assert!(matches!(
        snapshot
            .modules()
            .find(|module| module.path() == ":app")
            .context("App module")?
            .targets(),
        android_tools::project_context::TargetCatalogue::Unavailable(_)
    ));
    value["modules"][1]["android"] = json!({"status": "unavailable", "value": {"detail": "Unsupported AndroidComponents getter"}});
    assert!(
        !decode(&root, &value)?
            .capabilities(None, readiness(&root))
            .android_devices
    );
    value["modules"][0]["android"] =
        json!({"status": "available", "value": {"pluginVersion": "9.2.0"}});
    assert!(decode(&root, &value).is_err());
    Ok(())
}

#[test]
fn context_transport_accepts_only_exact_bounded_consistent_records() -> Result<()> {
    let root = project_root("context-transport");
    let complete = android_catalogue(&root);
    let payload = serde_json::to_string(&complete)?;
    let output = format!(
        "ordinary log\n {CONTEXT_OUTPUT_PREFIX}{payload}\n{CONTEXT_OUTPUT_PREFIX}{payload}\r\n"
    );
    assert_eq!(decode_context_output(&output, &root)?.len(), 1);
    assert!(decode_context_output(&format!(" {CONTEXT_OUTPUT_PREFIX}{payload}\n"), &root).is_err());
    assert!(
        decode_context_output(
            &format!("{CONTEXT_OUTPUT_PREFIX}{payload}\n{CONTEXT_OUTPUT_PREFIX}{payload}\n"),
            &root
        )
        .is_err()
    );
    assert!(
        decode_context_output(
            &format!("{CONTEXT_OUTPUT_PREFIX}{payload}\n"),
            &project_root("wrong-root")
        )
        .is_err()
    );
    let mut partial = complete.clone();
    partial["phase"] = json!("partial");
    for module in partial["modules"].as_array_mut().context("Modules")? {
        module["plugins"]
            .as_array_mut()
            .context("Plugins")?
            .retain(|plugin| plugin["applied"] == true);
        module["targets"] =
            json!({"status":"unavailable", "value":{"detail":"Configuration failed"}});
    }
    let partial = serde_json::to_string(&partial)?;
    assert_eq!(
        decode_context_output(
            &format!("{CONTEXT_OUTPUT_PREFIX}{partial}\n{CONTEXT_OUTPUT_PREFIX}{payload}\n"),
            &root
        )?
        .len(),
        2
    );
    assert!(decode_context_output(&format!("{CONTEXT_OUTPUT_PREFIX}{partial}\n{CONTEXT_OUTPUT_PREFIX}{partial}\n{CONTEXT_OUTPUT_PREFIX}{payload}\n"), &root).is_err());
    Ok(())
}

fn convention_catalogue(root: &Path) -> Value {
    let mut value = android_catalogue(root);
    value["buildLogicDirectories"] = json!([
        root.join("buildSrc"),
        root.parent().map(|parent| parent.join("external-logic"))
    ]);
    value["buildLayouts"] = json!([
        {"directory": root, "buildDirectory": root.join("out"), "sourceDirectories": []},
        {"directory": root.join("app"), "buildDirectory": root.join("app/custom-output"), "sourceDirectories": [root.join("app/src")]},
        {"directory": root.join("buildSrc"), "buildDirectory": root.join("buildSrc/convention-output"), "sourceDirectories": [root.join("buildSrc/src"), root.join("buildSrc/convention-output/generated")]}
    ]);
    value
}

#[test]
fn configured_outputs_and_caches_are_excluded_from_build_logic_input_changes() -> Result<()> {
    let root = project_root("convention-inputs");
    let snapshot = decode(&root, &convention_catalogue(&root))?;
    for path in [
        "build.gradle.kts",
        "local.properties",
        "gradle.lockfile",
        "gradle/libs.versions.toml",
        "gradle/verification-metadata.xml",
        "buildSrc/src/main/kotlin/Convention.kt",
        "buildSrc/src/main/resources/META-INF/plugin.properties",
    ] {
        assert!(snapshot.is_input(&root.join(path)), "{path}");
    }
    for path in [
        "buildSrc/convention-output/classes/Convention.class",
        "buildSrc/convention-output/resources/META-INF/plugin.properties",
        "buildSrc/convention-output/generated/Generated.kt",
        "buildSrc/.gradle/cache.properties",
        "buildSrc/.kotlin/session",
        "buildSrc/.git/objects/abc",
        "app/src/main/Main.kt",
        "app/custom-output/gradle.properties",
        "main.py",
        "index.html",
    ] {
        assert!(!snapshot.is_input(&root.join(path)), "{path}");
    }
    assert!(
        snapshot.is_input(
            &root
                .parent()
                .context("Parent")?
                .join("external-logic/src/Convention.kt")
        )
    );
    Ok(())
}

#[test]
fn generated_pending_writes_do_not_stale_import_but_actual_inputs_do() -> Result<()> {
    let root = project_root("pending-convention");
    let mut store = ContextStore::default();
    let handle = store.add_root(1, root.clone(), true)?;
    let token = store.begin_import(handle)?;
    assert!(
        store
            .observe_input_change(
                &root.join("buildSrc/convention-output/resources/plugin.properties")
            )?
            .is_empty()
    );
    assert!(
        store
            .observe_input_change(&root.join("app/src/main/Main.kt"))?
            .is_empty()
    );
    let snapshot = decode(&root, &convention_catalogue(&root))?;
    store.verify_import_inputs(&token, &snapshot)?;
    assert!(
        store.snapshot(handle).is_none(),
        "First-pass provenance must not publish capabilities"
    );
    store.publish(&token, snapshot)?;
    let before = store.token(handle).context("Current token")?;
    assert!(
        store
            .observe_input_change(
                &root.join("buildSrc/convention-output/classes/Convention.class")
            )?
            .is_empty()
    );
    assert!(store.is_current(&before));
    assert_eq!(
        store.observe_input_change(&root.join("buildSrc/src/main/kotlin/Convention.kt"))?,
        vec![handle]
    );
    assert!(!store.is_current(&before));
    let token = store.begin_import(handle)?;
    store.observe_input_change(&root.join("buildSrc/src/main/kotlin/Convention.kt"))?;
    assert!(
        store
            .publish(&token, decode(&root, &convention_catalogue(&root))?)
            .is_err()
    );
    assert!(store.snapshot(handle).is_none());
    assert!(!store.import_is_current(&token));
    Ok(())
}

#[test]
fn invalid_layouts_and_excessive_pending_change_sets_are_rejected() -> Result<()> {
    let root = project_root("invalid-provenance");
    let mut value = convention_catalogue(&root);
    value["buildLayouts"][0]["buildDirectory"] = json!(&root);
    assert!(decode(&root, &value).is_err());
    let mut value = convention_catalogue(&root);
    value["buildLayouts"][0]["sourceDirectories"] = json!([root.join("src"), root.join("src")]);
    assert!(decode(&root, &value).is_err());
    let mut store = ContextStore::default();
    let handle = store.add_root(1, root.clone(), true)?;
    let token = store.begin_import(handle)?;
    for index in 0..16385 {
        store.observe_input_change(&root.join(format!("unresolved/{index}")))?;
    }
    assert!(!store.import_is_current(&token));
    assert!(
        store
            .publish(&token, decode(&root, &convention_catalogue(&root))?)
            .is_err()
    );
    Ok(())
}

#[test]
fn unrelated_project_events_do_not_exhaust_another_pending_import() -> Result<()> {
    let root = project_root("isolated-pending");
    let other = project_root("busy-unrelated-root");
    let external = project_root("owned-external-logic");
    let mut store = ContextStore::default();
    let handle = store.add_root(1, root.clone(), true)?;
    let other_handle = store.add_root(2, other.clone(), true)?;
    let discovery = store.begin_import(handle)?;
    let other_discovery = store.begin_import(other_handle)?;
    for index in 0..66 {
        store.observe_input_change(&other.join(format!("{index}-{}", "x".repeat(16384))))?;
    }
    assert!(store.import_is_current(&discovery));
    assert!(!store.import_is_current(&other_discovery));
    let mut value = convention_catalogue(&root);
    value["buildLogicDirectories"] = json!([external]);
    store.observe_root_input_change(handle, &external.join("src/Convention.kt"))?;
    assert!(store.publish(&discovery, decode(&root, &value)?).is_err());
    assert!(!store.import_is_current(&discovery));
    Ok(())
}

#[test]
fn evaluated_build_logic_inside_parent_output_keeps_its_own_inputs() -> Result<()> {
    let root = project_root("logic-inside-parent-output");
    let logic = root.join("out/logic");
    let mut value = convention_catalogue(&root);
    value["buildLogicDirectories"] = json!([&logic]);
    value["buildLayouts"].as_array_mut().context("Layouts")?.push(json!({
        "directory":&logic,"buildDirectory":logic.join("generated"),"sourceDirectories":[logic.join("src")]
    }));
    let snapshot = decode(&root, &value)?;
    assert!(snapshot.is_input(&logic.join("src/Convention.kt")));
    assert!(!snapshot.is_generated_output(&logic.join("src/Convention.kt")));
    assert!(snapshot.is_generated_output(&logic.join("generated/classes/Convention.class")));
    assert!(!snapshot.is_input(&logic.join("generated/classes/Convention.class")));
    assert!(snapshot.is_generated_output(&root.join("out/unrelated-output")));
    value["buildLogicDirectories"] = json!([&root, &logic]);
    let nested = decode(&root, &value)?.observer_directories()?;
    assert!(nested.contains(&root));
    assert!(nested.contains(&logic));
    assert!(nested.contains(&logic.join("src")));
    Ok(())
}

fn module(
    root: &Path,
    path: &str,
    directory: &str,
    applied: &[PluginId],
    platforms: &[(&str, &str)],
) -> Value {
    json!({"path": path, "directory": root.join(directory),
        "plugins": PluginId::ALL.map(|plugin| json!({"plugin": plugin, "applied": applied.contains(&plugin)})),
        "targets": {"status": "available", "value": platforms.iter().map(|(name, platform)| json!({"name": name, "platform": platform})).collect::<Vec<_>>()}})
}

fn catalogue(root: &Path, modules: Vec<Value>) -> Value {
    json!({"schema": 1, "root": root, "gradleVersion": "9.4", "phase": "complete", "modules": modules})
}

fn decode(root: &Path, value: &Value) -> Result<ContextSnapshot> {
    decode_context_record(&serde_json::to_vec(value)?, root)
}

fn android_catalogue(root: &Path) -> Value {
    catalogue(
        root,
        vec![
            module(root, ":", "", &[], &[]),
            module(
                root,
                ":app",
                "app",
                &[PluginId::AndroidApplication, PluginId::ComposeCompiler],
                &[("android", "androidJvm")],
            ),
        ],
    )
}

fn readiness(root: &Path) -> OperationalReadiness<'_> {
    OperationalReadiness {
        application_module: Some(":app"),
        model_current: true,
        model_root: Some(root),
        android_renderer_supported: true,
    }
}

#[test]
fn ordinary_projects_and_unknown_contexts_have_no_android_capabilities() -> Result<()> {
    for name in [
        "python",
        "html",
        "standalone",
        "plain-jvm-gradle",
        "kotlin-jvm",
    ] {
        let root = project_root(name);
        let snapshot = decode(
            &root,
            &catalogue(&root, vec![module(&root, ":", "", &[], &[("jvm", "jvm")])]),
        )?;
        assert_eq!(
            snapshot.capabilities(Some(&root.join("src/Main.kt")), readiness(&root)),
            ContextCapabilities::default()
        );
        assert!(
            !snapshot
                .project_view_capabilities(true)
                .is_android_view_visible()
        );
    }
    let mut store = ContextStore::default();
    let root = project_root("unknown");
    let handle = store.add_root(1, root.clone(), true)?;
    let mut active = ActiveContext::default();
    active.select(Some(handle), Some(root.join("main.py")))?;
    assert_eq!(
        active.capabilities(&store, readiness(&root)),
        ContextCapabilities::default()
    );
    assert!(active.token(&store).is_none());
    Ok(())
}

#[test]
fn application_devices_run_and_preview_require_their_own_facts() -> Result<()> {
    let root = project_root("android");
    let snapshot = decode(&root, &android_catalogue(&root))?;
    let file = root.join("app/src/Main.kt");
    let capabilities = snapshot.capabilities(Some(&file), readiness(&root));
    assert!(capabilities.ecosystems.android);
    assert!(capabilities.android_sync);
    assert!(capabilities.automatic_android_sync);
    assert!(capabilities.android_devices);
    assert!(capabilities.android_run);
    assert!(capabilities.android_compose_preview);
    assert!(
        snapshot
            .project_view_capabilities(true)
            .is_android_view_visible()
    );
    assert!(
        !snapshot
            .project_view_capabilities(false)
            .is_android_view_visible()
    );
    let unavailable = snapshot.capabilities(Some(&file), OperationalReadiness::default());
    assert!(unavailable.android_devices);
    assert!(!unavailable.android_run);
    assert!(!unavailable.android_compose_preview);
    assert!(
        !snapshot
            .capabilities(
                Some(&file),
                OperationalReadiness {
                    android_renderer_supported: false,
                    ..readiness(&root)
                }
            )
            .android_compose_preview
    );
    let other = project_root("different-model");
    let foreign_model = snapshot.capabilities(Some(&file), readiness(&other));
    assert!(!foreign_model.android_run);
    assert!(!foreign_model.android_compose_preview);
    Ok(())
}

#[test]
fn library_and_dynamic_feature_do_not_synthesize_an_application() -> Result<()> {
    for plugin in [
        PluginId::AndroidLibrary,
        PluginId::AndroidDynamicFeature,
        PluginId::AndroidTest,
        PluginId::AndroidMultiplatformLibrary,
    ] {
        let root = project_root("library");
        let snapshot = decode(
            &root,
            &catalogue(
                &root,
                vec![
                    module(&root, ":", "", &[], &[]),
                    module(
                        &root,
                        ":app",
                        "app",
                        &[plugin],
                        &[("android", "androidJvm")],
                    ),
                ],
            ),
        )?;
        let capabilities = snapshot.capabilities(None, readiness(&root));
        assert!(capabilities.ecosystems.android);
        assert!(capabilities.android_devices);
        assert!(!capabilities.android_run);
        assert!(!capabilities.android_compose_preview);
    }
    Ok(())
}

#[test]
fn kmp_and_desktop_cmp_qualify_without_android_device_or_preview_capabilities() -> Result<()> {
    for plugins in [
        vec![PluginId::KotlinMultiplatform],
        vec![
            PluginId::KotlinMultiplatform,
            PluginId::ComposeMultiplatform,
            PluginId::ComposeCompiler,
        ],
    ] {
        let root = project_root("multiplatform-desktop");
        let snapshot = decode(
            &root,
            &catalogue(
                &root,
                vec![module(
                    &root,
                    ":",
                    "",
                    &plugins,
                    &[
                        ("desktop", "jvm"),
                        ("web", "js"),
                        ("linux", "native"),
                        ("wasmJs", "wasm"),
                        ("metadata", "common"),
                    ],
                )],
            ),
        )?;
        let capabilities = snapshot.capabilities(Some(&root.join("src/Main.kt")), readiness(&root));
        assert!(capabilities.ecosystems.qualifies());
        assert!(capabilities.ecosystems.kotlin_multiplatform);
        assert_eq!(
            capabilities.ecosystems.compose_multiplatform,
            plugins.contains(&PluginId::ComposeMultiplatform)
        );
        assert!(!capabilities.android_sync);
        assert!(!capabilities.android_devices);
        assert!(!capabilities.android_run);
        assert!(!capabilities.android_compose_preview);
        assert!(
            !snapshot
                .project_view_capabilities(true)
                .is_android_view_visible()
        );
    }
    Ok(())
}

#[test]
fn evaluated_kmp_android_target_enables_only_proven_android_capabilities() -> Result<()> {
    let root = project_root("kmp-android");
    let snapshot = decode(
        &root,
        &catalogue(
            &root,
            vec![module(
                &root,
                ":",
                "",
                &[
                    PluginId::KotlinMultiplatform,
                    PluginId::ComposeMultiplatform,
                ],
                &[("android", "androidJvm"), ("desktop", "jvm")],
            )],
        ),
    )?;
    let capabilities = snapshot.capabilities(
        Some(&root.join("src/Main.kt")),
        OperationalReadiness::default(),
    );
    assert!(capabilities.ecosystems.kotlin_multiplatform);
    assert!(capabilities.ecosystems.compose_multiplatform);
    assert!(capabilities.android_devices);
    assert!(capabilities.android_sync);
    assert!(!capabilities.android_run);
    assert!(!capabilities.android_compose_preview);
    assert!(
        snapshot
            .project_view_capabilities(true)
            .is_android_view_visible()
    );
    Ok(())
}

#[test]
fn unsupported_target_getters_do_not_invent_operational_capabilities() -> Result<()> {
    let root = project_root("unavailable-targets");
    let mut value = android_catalogue(&root);
    value["modules"][1]["targets"] = json!({"status": "unavailable", "value": {"detail": "Target getter is not supported by this plugin version"}});
    let capabilities =
        decode(&root, &value)?.capabilities(Some(&root.join("app/Main.kt")), readiness(&root));
    assert!(capabilities.ecosystems.android);
    assert!(capabilities.android_sync);
    assert!(!capabilities.android_devices);
    assert!(!capabilities.android_run);
    assert!(!capabilities.android_compose_preview);
    Ok(())
}

#[test]
fn kotlin_and_compose_compiler_plugins_alone_do_not_classify_an_android_project() -> Result<()> {
    let root = project_root("compiler-only");
    let snapshot = decode(
        &root,
        &catalogue(
            &root,
            vec![module(
                &root,
                ":",
                "",
                &[PluginId::KotlinAndroid, PluginId::ComposeCompiler],
                &[],
            )],
        ),
    )?;
    assert_eq!(
        snapshot.capabilities(Some(&root.join("Main.kt")), readiness(&root)),
        ContextCapabilities::default()
    );
    Ok(())
}

#[test]
fn longest_evaluated_module_owns_a_file_and_ambiguous_owners_cannot_preview() -> Result<()> {
    let root = project_root("module-ownership");
    let mut value = android_catalogue(&root);
    value["modules"]
        .as_array_mut()
        .context("Modules array")?
        .push(module(
            &root,
            ":app:jvm",
            "app/nested",
            &[],
            &[("jvm", "jvm")],
        ));
    let snapshot = decode(&root, &value)?;
    let nested = root.join("app/nested/Main.kt");
    assert_eq!(
        snapshot.module_owner(&nested),
        ModuleOwner::Module(":app:jvm")
    );
    assert!(
        !snapshot
            .capabilities(Some(&nested), readiness(&root))
            .android_compose_preview
    );
    assert_eq!(
        snapshot.module_owner(&root.join("application/Main.kt")),
        ModuleOwner::Module(":")
    );
    assert_eq!(
        snapshot.module_owner(
            &root
                .parent()
                .context("Root parent")?
                .join("unrelated/Main.kt")
        ),
        ModuleOwner::OutsideRoot
    );
    assert_eq!(
        snapshot.module_owner(&root.join("app/../../unrelated/Main.kt")),
        ModuleOwner::Unresolved
    );
    value["modules"]
        .as_array_mut()
        .context("Modules array")?
        .push(module(
            &root,
            ":duplicate",
            "app/nested",
            &[PluginId::AndroidLibrary, PluginId::ComposeCompiler],
            &[("android", "androidJvm")],
        ));
    let ambiguous = decode(&root, &value)?;
    assert_eq!(ambiguous.module_owner(&nested), ModuleOwner::Ambiguous);
    assert!(
        !ambiguous
            .capabilities(Some(&nested), readiness(&root))
            .android_compose_preview
    );
    Ok(())
}

#[test]
fn pane_preview_does_not_borrow_a_different_root_or_module() -> Result<()> {
    let root = project_root("owner-android");
    let snapshot = decode(&root, &android_catalogue(&root))?;
    assert!(
        snapshot
            .capabilities(Some(&root.join("app/Main.kt")), readiness(&root))
            .android_compose_preview
    );
    assert!(
        !snapshot
            .capabilities(Some(&root.join("tools/Plain.kt")), readiness(&root))
            .android_compose_preview
    );
    assert!(
        !snapshot
            .capabilities(
                Some(&project_root("owner-python").join("Plain.kt")),
                readiness(&root)
            )
            .android_compose_preview
    );
    assert_eq!(
        snapshot.capabilities(
            Some(&project_root("owner-python").join("Plain.kt")),
            readiness(&root)
        ),
        ContextCapabilities::default()
    );
    assert!(
        !snapshot
            .capabilities(None, readiness(&root))
            .android_compose_preview
    );
    Ok(())
}

#[test]
fn protocol_rejects_wrong_root_schema_unknown_fields_and_non_object_records() -> Result<()> {
    let root = project_root("protocol");
    let value = android_catalogue(&root);
    assert!(decode(&project_root("wrong-root"), &value).is_err());
    for (key, replacement) in [
        ("schema", json!(2)),
        ("unknown", json!(true)),
        ("gradleVersion", json!("")),
    ] {
        let mut invalid = value.clone();
        invalid[key] = replacement;
        assert!(decode(&root, &invalid).is_err(), "{key}");
    }
    assert!(decode_context_record(b"[1]", &root).is_err());
    let mut invalid = value.clone();
    invalid["modules"][0] = json!([":", root, [], []]);
    assert!(decode(&root, &invalid).is_err());
    let mut invalid = value.clone();
    invalid["modules"][1]["targets"]["value"][0] = json!(["android", "androidJvm"]);
    assert!(decode(&root, &invalid).is_err());
    let duplicate_schema =
        serde_json::to_string(&value)?.replacen("\"schema\":1", "\"schema\":1,\"schema\":1", 1);
    assert!(decode_context_record(duplicate_schema.as_bytes(), &root).is_err());
    assert!(decode_context_record(&vec![b' '; MAX_CONTEXT_RECORD_BYTES + 1], &root).is_err());
    let bytes = serde_json::to_vec(&value)?;
    assert!(
        decode_context_record(
            bytes.get(..bytes.len() - 1).context("Truncated bytes")?,
            &root
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn protocol_rejects_duplicate_missing_and_contradictory_evaluated_facts() -> Result<()> {
    let root = project_root("identities");
    let value = android_catalogue(&root);
    let mut duplicate = value.clone();
    let module = duplicate["modules"][1].clone();
    duplicate["modules"]
        .as_array_mut()
        .context("Modules")?
        .push(module);
    assert!(decode(&root, &duplicate).is_err());
    let mut duplicate = value.clone();
    let plugin = duplicate["modules"][0]["plugins"][0].clone();
    duplicate["modules"][0]["plugins"]
        .as_array_mut()
        .context("Plugins")?
        .push(plugin);
    assert!(decode(&root, &duplicate).is_err());
    let mut duplicate = value.clone();
    let target = duplicate["modules"][1]["targets"]["value"][0].clone();
    duplicate["modules"][1]["targets"]["value"]
        .as_array_mut()
        .context("Targets")?
        .push(target);
    assert!(decode(&root, &duplicate).is_err());
    let mut missing = value.clone();
    missing["modules"][0]["plugins"]
        .as_array_mut()
        .context("Plugins")?
        .pop();
    assert!(decode(&root, &missing).is_err());
    let mut missing_root = value.clone();
    missing_root["modules"]
        .as_array_mut()
        .context("Modules")?
        .remove(0);
    assert!(decode(&root, &missing_root).is_err());
    let mut conflicting = value.clone();
    conflicting["modules"][1]["plugins"][1]["applied"] = json!(true);
    assert!(decode(&root, &conflicting).is_err());
    let mut no_plugin = value.clone();
    no_plugin["modules"][1]["plugins"][0]["applied"] = json!(false);
    assert!(decode(&root, &no_plugin).is_err());
    let mut invalid_platform = value.clone();
    invalid_platform["modules"][1]["targets"]["value"][0]["platform"] =
        json!("futureAndroidPlatform");
    assert!(decode(&root, &invalid_platform).is_err());
    Ok(())
}

#[test]
fn protocol_rejects_invalid_paths_module_names_and_collection_limits() -> Result<()> {
    let root = project_root("bounds");
    let value = android_catalogue(&root);
    for path in [
        "app",
        ":app::nested",
        ":app/nested",
        ":app\\nested",
        ":bad\0name",
    ] {
        let mut invalid = value.clone();
        invalid["modules"][1]["path"] = json!(path);
        assert!(decode(&root, &invalid).is_err());
    }
    for directory in [PathBuf::from("relative"), root.join("app/../other")] {
        let mut invalid = value.clone();
        invalid["modules"][1]["directory"] = json!(directory);
        assert!(decode(&root, &invalid).is_err());
    }
    let mut invalid_root = value.clone();
    invalid_root["modules"][0]["directory"] = json!(root.join("wrong"));
    assert!(decode(&root, &invalid_root).is_err());
    let mut too_many_targets = value.clone();
    too_many_targets["modules"][1]["targets"]["value"] = json!(
        (0..257)
            .map(|number| json!({"name": number.to_string(), "platform": "androidJvm"}))
            .collect::<Vec<_>>()
    );
    assert!(decode(&root, &too_many_targets).is_err());
    let too_many_modules = json!({"schema": 1, "root": root, "gradleVersion": "9.4", "phase": "partial",
        "modules": (0..4097).map(|number| json!({"path": format!(":module{number}"), "directory": root.join(number.to_string()), "plugins": [], "targets": {"status": "unavailable", "value": {"detail": "not observed"}}})).collect::<Vec<_>>()});
    let bytes = serde_json::to_vec(&too_many_modules)?;
    assert!(bytes.len() <= MAX_CONTEXT_RECORD_BYTES);
    assert!(decode_context_record(&bytes, &root).is_err());
    Ok(())
}

fn partial(root: &Path) -> Value {
    json!({"schema": 1, "root": root, "gradleVersion": "9.4", "phase": "partial", "modules": [{
        "path": ":app", "directory": root.join("app"),
        "plugins": [{"plugin": "com.android.application", "applied": true}],
        "targets": {"status": "unavailable", "value": {"detail": "configuration is still in progress"}}}]})
}

#[test]
fn partial_applied_plugin_facts_allow_repair_sync_after_sdk_failure_without_device_or_run_credit()
-> Result<()> {
    let root = project_root("sdk-failure");
    let mut store = ContextStore::default();
    let handle = store.add_root(1, root.clone(), true)?;
    let token = store.begin_import(handle)?;
    store.publish(&token, decode(&root, &partial(&root))?)?;
    store.finish_failed_import(&token)?;
    let snapshot = store.snapshot(handle).context("Partial snapshot")?;
    assert_eq!(snapshot.phase(), ObservationPhase::Partial);
    assert!(snapshot.ecosystems().android);
    let capabilities = snapshot.capabilities(Some(&root.join("app/Main.kt")), readiness(&root));
    assert!(capabilities.android_sync);
    assert!(!capabilities.automatic_android_sync);
    assert!(!capabilities.android_devices);
    assert!(!capabilities.android_run);
    assert!(!capabilities.android_compose_preview);
    assert!(
        store
            .publish(&token, decode(&root, &android_catalogue(&root))?)
            .is_err()
    );
    let mut false_absence = partial(&root);
    false_absence["modules"][0]["plugins"][0]["applied"] = json!(false);
    assert!(decode(&root, &false_absence).is_err());
    Ok(())
}

#[test]
fn partial_observations_can_refine_but_cannot_contradict_within_an_import() -> Result<()> {
    let root = project_root("refinement");
    let mut store = ContextStore::default();
    let handle = store.add_root(1, root.clone(), true)?;
    let token = store.begin_import(handle)?;
    store.publish(&token, decode(&root, &partial(&root))?)?;
    let wrong = catalogue(
        &root,
        vec![
            module(&root, ":", "", &[], &[]),
            module(&root, ":app", "app", &[], &[]),
        ],
    );
    assert!(store.publish(&token, decode(&root, &wrong)?).is_err());
    assert!(
        store
            .snapshot(handle)
            .context("Previous facts")?
            .ecosystems()
            .android
    );
    store.publish(&token, decode(&root, &android_catalogue(&root))?)?;
    assert_eq!(
        store.snapshot(handle).context("Complete snapshot")?.phase(),
        ObservationPhase::Complete
    );
    assert!(store.finish_failed_import(&token).is_err());
    Ok(())
}

#[test]
fn out_of_order_imports_and_input_invalidation_reject_stale_publication() -> Result<()> {
    let root = project_root("import-order");
    let mut store = ContextStore::default();
    let handle = store.add_root(1, root.clone(), true)?;
    let first = store.begin_import(handle)?;
    let second = store.begin_import(handle)?;
    assert!(
        store
            .publish(&first, decode(&root, &android_catalogue(&root))?)
            .is_err()
    );
    store.publish(&second, decode(&root, &android_catalogue(&root))?)?;
    let model_token = store.token(handle).context("Context token")?;
    store.invalidate(handle)?;
    assert!(!store.is_current(&model_token));
    assert!(store.snapshot(handle).is_none());
    assert!(
        store
            .publish(&second, decode(&root, &android_catalogue(&root))?)
            .is_err()
    );
    Ok(())
}

#[test]
fn removed_and_readded_same_path_roots_have_different_incarnations() -> Result<()> {
    let root = project_root("readded");
    let mut store = ContextStore::default();
    let old = store.add_root(7, root.clone(), true)?;
    let old_import = store.begin_import(old)?;
    let old_token = store.token(old).context("Old token")?;
    assert!(store.add_root(7, root.clone(), true).is_err());
    assert!(store.remove_root(old));
    let new = store.add_root(7, root.clone(), true)?;
    assert_ne!(old, new);
    assert!(!store.remove_root(old));
    assert!(!store.is_current(&old_token));
    assert!(
        store
            .publish(&old_import, decode(&root, &android_catalogue(&root))?)
            .is_err()
    );
    assert!(store.snapshot(new).is_none());
    Ok(())
}

#[test]
fn trust_revocation_clears_facts_and_does_not_restore_them_when_retrusted() -> Result<()> {
    let root = project_root("trust");
    let mut store = ContextStore::default();
    let handle = store.add_root(1, root.clone(), false)?;
    assert!(store.begin_import(handle).is_err());
    assert!(store.token(handle).is_none());
    store.set_trusted(handle, true)?;
    let token = store.begin_import(handle)?;
    store.publish(&token, decode(&root, &android_catalogue(&root))?)?;
    let mut active = ActiveContext::default();
    active.select(Some(handle), Some(root.join("app/Main.kt")))?;
    let active_token = active.token(&store).context("Active token")?;
    assert!(active.capabilities(&store, readiness(&root)).android_run);
    store.set_trusted(handle, false)?;
    assert!(!active.is_current(&active_token, &store));
    assert_eq!(
        active.capabilities(&store, readiness(&root)),
        ContextCapabilities::default()
    );
    store.set_trusted(handle, true)?;
    assert!(store.snapshot(handle).is_none());
    assert!(active.token(&store).is_none());
    assert!(
        store
            .publish(&token, decode(&root, &android_catalogue(&root))?)
            .is_err()
    );
    Ok(())
}

#[test]
fn mixed_roots_and_independent_windows_do_not_borrow_context() -> Result<()> {
    let android = project_root("mixed-android");
    let python = project_root("mixed-python");
    let mut store = ContextStore::default();
    let android_handle = store.add_root(1, android.clone(), true)?;
    let python_handle = store.add_root(2, python.clone(), true)?;
    let token = store.begin_import(android_handle)?;
    store.publish(&token, decode(&android, &android_catalogue(&android))?)?;
    let token = store.begin_import(python_handle)?;
    store.publish(
        &token,
        decode(
            &python,
            &catalogue(&python, vec![module(&python, ":", "", &[], &[])]),
        )?,
    )?;
    let mut first_window = ActiveContext::default();
    let mut second_window = ActiveContext::default();
    first_window.select(Some(android_handle), Some(android.join("app/Main.kt")))?;
    second_window.select(Some(python_handle), Some(python.join("Main.kt")))?;
    assert!(
        first_window
            .capabilities(&store, readiness(&android))
            .android_run
    );
    assert_eq!(
        second_window.capabilities(&store, readiness(&android)),
        ContextCapabilities::default()
    );
    let first_token = first_window.token(&store).context("First window token")?;
    second_window.select(Some(android_handle), Some(android.join("tools/Plain.kt")))?;
    assert!(
        !second_window
            .capabilities(&store, readiness(&android))
            .android_compose_preview
    );
    assert!(first_window.is_current(&first_token, &store));
    second_window.select(None, None)?;
    assert_eq!(
        second_window.capabilities(&store, readiness(&android)),
        ContextCapabilities::default()
    );
    Ok(())
}

#[test]
fn switching_a_b_a_and_changing_pane_owner_invalidates_deferred_actions() -> Result<()> {
    let android = project_root("aba-android");
    let other = project_root("aba-other");
    let mut store = ContextStore::default();
    let first = store.add_root(1, android.clone(), true)?;
    let second = store.add_root(2, other, true)?;
    let import = store.begin_import(first)?;
    store.publish(&import, decode(&android, &android_catalogue(&android))?)?;
    let mut active = ActiveContext::default();
    active.select(Some(first), Some(android.join("app/First.kt")))?;
    let before_switch = active.token(&store).context("Before switch")?;
    active.select(Some(second), None)?;
    active.select(Some(first), Some(android.join("app/First.kt")))?;
    assert!(!active.is_current(&before_switch, &store));
    let before_file = active.token(&store).context("Before file")?;
    active.select(Some(first), Some(android.join("app/Second.kt")))?;
    assert!(!active.is_current(&before_file, &store));
    let unchanged = active.token(&store).context("Unchanged token")?;
    active.select(Some(first), Some(android.join("app/Second.kt")))?;
    assert!(active.is_current(&unchanged, &store));
    assert!(
        active
            .select(None, Some(android.join("app/Second.kt")))
            .is_err()
    );
    Ok(())
}

#[test]
fn active_import_publication_rejects_a_b_a_even_before_initial_facts_exist() -> Result<()> {
    let android = project_root("import-aba-android");
    let mut store = ContextStore::default();
    let first = store.add_root(1, android.clone(), true)?;
    let second = store.add_root(2, project_root("import-aba-python"), true)?;
    let mut active = ActiveContext::default();
    active.select(Some(first), Some(android.join("app/Main.kt")))?;
    let import = store.begin_import(first)?;
    let captured = active
        .discovery_token(&store)
        .context("Discovery selection")?;
    assert!(active.token(&store).is_none());
    active.select(Some(second), None)?;
    active.select(Some(first), Some(android.join("app/Main.kt")))?;
    assert!(
        active
            .publish(
                &mut store,
                &captured,
                &import,
                decode(&android, &android_catalogue(&android))?
            )
            .is_err()
    );
    assert!(store.snapshot(first).is_none());
    let current_import = store.begin_import(first)?;
    let current = active
        .discovery_token(&store)
        .context("Current selection")?;
    active.publish(
        &mut store,
        &current,
        &current_import,
        decode(&android, &android_catalogue(&android))?,
    )?;
    assert!(active.capabilities(&store, readiness(&android)).android_run);
    Ok(())
}

#[test]
fn inconsistent_active_root_and_file_owner_cannot_grant_tools_or_action_tokens() -> Result<()> {
    let android = project_root("inconsistent-android");
    let mut store = ContextStore::default();
    let handle = store.add_root(1, android.clone(), true)?;
    let import = store.begin_import(handle)?;
    store.publish(&import, decode(&android, &android_catalogue(&android))?)?;
    let mut active = ActiveContext::default();
    active.select(
        Some(handle),
        Some(project_root("inconsistent-python").join("Main.kt")),
    )?;
    assert!(active.token(&store).is_none());
    assert!(active.discovery_token(&store).is_none());
    assert_eq!(
        active.capabilities(&store, readiness(&android)),
        ContextCapabilities::default()
    );
    Ok(())
}
