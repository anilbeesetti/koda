use android_tools::{
    project_context::{ContextStore, PluginId, RootHandle, decode_context_record},
    project_model::{
        EvaluatedModelPaths, MODEL_OUTPUT_PREFIX, parse_model, parse_model_with_context,
    },
};
use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn evaluated_store(root: &Path, module: &Path, phase: &str) -> Result<(ContextStore, RootHandle)> {
    let mut record = json!({"schema":1,"root":root,"gradleVersion":"9.4","phase":phase,
        "modules":[{"path":":","directory":root,
            "plugins":PluginId::ALL.map(|plugin| json!({"plugin":plugin,"applied":false})),
            "targets":{"status":"available","value":[]}},
            {"path":":lib","directory":module,
             "plugins":PluginId::ALL.map(|plugin| json!({"plugin":plugin,"applied":plugin==PluginId::AndroidLibrary})),
             "targets":{"status":"available","value":[]},
             "android":{"status":"available","value":{"pluginVersion":"9.2.0"}}}]});
    if phase == "partial" {
        for module in record["modules"].as_array_mut().context("Modules")? {
            module["plugins"]
                .as_array_mut()
                .context("Plugin observations")?
                .retain(|plugin| plugin["applied"] == true);
        }
    }
    let mut store = ContextStore::default();
    let handle = store.add_root(1, root.to_path_buf(), true)?;
    let import = store.begin_import(handle)?;
    store.publish(
        &import,
        decode_context_record(&serde_json::to_vec(&record)?, root)?,
    )?;
    Ok((store, handle))
}

fn fixture() -> Result<(tempfile::TempDir, PathBuf, PathBuf, Value)> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("build");
    let module = directory.path().join("lib");
    std::fs::create_dir_all(&root)?;
    std::fs::create_dir_all(module.join("src/main/kotlin"))?;
    let root = root.canonicalize()?;
    let module = module.canonicalize()?;
    let model = json!({"version":1,"root":root,"diagnostics":[],"modules":[{
        "path":":lib","directory":module,"kind":"library","namespace":"example.library",
        "sourceProviders":{"order":"gradleSourceSetIteration","providers":[{
            "name":"main","roots":[{"path":module.join("src/main/kotlin"),"kind":"kotlin"}]}]},
        "variants":[{"name":"debug","outputListing":null,"components":[{
            "name":"debug","namespace":"example.library","scope":"main","dependencies":[],
            "sources":[{"path":module.join("src/main/kotlin"),"kind":"kotlin","generated":false},
                {"path":module.join("build/generated/kotlin"),"kind":"kotlin","generated":true}]}]}]}]});
    Ok((directory, root, module, model))
}

fn output(model: &Value) -> String {
    format!("{MODEL_OUTPUT_PREFIX}{model}")
}

#[test]
fn contextual_parser_accepts_only_fresh_evaluated_sibling_module_directories() -> Result<()> {
    let (_directory, root, module, model) = fixture()?;
    let (store, handle) = evaluated_store(&root, &module, "complete")?;
    let token = store.token(handle).context("Trusted token")?;
    let paths = EvaluatedModelPaths::capture(&store, &token, &root)?;
    assert!(paths.is_current(&store));
    assert!(
        parse_model(&output(&model), &root).is_err(),
        "The default parser still rejects external directories"
    );
    let parsed = parse_model_with_context(&output(&model), &root, &paths)?;
    assert_eq!(parsed.modules[0].directory, module);
    assert_eq!(parsed.modules[0].variants[0].components[0].sources.len(), 2);
    assert!(
        parsed.targets().is_empty(),
        "A library must not synthesize an APK target"
    );
    Ok(())
}

#[test]
fn contextual_parser_rejects_forged_modules_and_source_escape_fields() -> Result<()> {
    let (directory, root, module, model) = fixture()?;
    let (store, handle) = evaluated_store(&root, &module, "complete")?;
    let paths =
        EvaluatedModelPaths::capture(&store, &store.token(handle).context("Token")?, &root)?;
    let unrelated = directory.path().join("unrelated");
    std::fs::create_dir_all(&unrelated)?;
    let mut forged = model.clone();
    forged["modules"][0]["path"] = json!(":forged");
    assert!(parse_model_with_context(&output(&forged), &root, &paths).is_err());
    forged = model.clone();
    forged["modules"][0]["directory"] = json!(unrelated);
    assert!(parse_model_with_context(&output(&forged), &root, &paths).is_err());
    for path in [unrelated.join("src"), module.join("../unrelated/src")] {
        forged = model.clone();
        forged["modules"][0]["variants"][0]["components"][0]["sources"][0]["path"] = json!(path);
        assert!(parse_model_with_context(&output(&forged), &root, &paths).is_err());
        forged = model.clone();
        forged["modules"][0]["sourceProviders"]["providers"][0]["roots"][0]["path"] = json!(path);
        assert!(parse_model_with_context(&output(&forged), &root, &paths).is_err());
        forged = model.clone();
        forged["modules"][0]["variants"][0]["outputListing"] = json!(path);
        assert!(parse_model_with_context(&output(&forged), &root, &paths).is_err());
    }
    Ok(())
}

#[test]
fn contextual_model_path_capture_rejects_stale_untrusted_incomplete_and_wrong_roots() -> Result<()>
{
    let (directory, root, module, model) = fixture()?;
    let (mut store, handle) = evaluated_store(&root, &module, "complete")?;
    let token = store.token(handle).context("Token")?;
    let paths = EvaluatedModelPaths::capture(&store, &token, &root)?;
    let other = directory.path().join("other");
    std::fs::create_dir_all(&other)?;
    assert!(EvaluatedModelPaths::capture(&store, &token, &other).is_err());
    assert!(parse_model_with_context(&output(&model), &other, &paths).is_err());
    store.invalidate(handle)?;
    assert!(
        !paths.is_current(&store),
        "A captured directory map must not authorize stale result application"
    );
    assert!(EvaluatedModelPaths::capture(&store, &token, &root).is_err());
    // An immutable map is a snapshot, not a live revocation service. Production
    // dispatch/application must check is_current, even if parsing alone succeeds.
    assert!(parse_model_with_context(&output(&model), &root, &paths).is_ok());
    store.set_trusted(handle, false)?;
    assert!(store.token(handle).is_none());
    assert!(EvaluatedModelPaths::capture(&store, &token, &root).is_err());
    let (partial, handle) = evaluated_store(&root, &module, "partial")?;
    assert!(
        EvaluatedModelPaths::capture(
            &partial,
            &partial.token(handle).context("Partial token")?,
            &root
        )
        .is_err()
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn contextual_parser_rejects_symlink_escape_in_evaluated_sibling_sources() -> Result<()> {
    let (directory, root, module, mut model) = fixture()?;
    let original = model.clone();
    let (store, handle) = evaluated_store(&root, &module, "complete")?;
    let paths =
        EvaluatedModelPaths::capture(&store, &store.token(handle).context("Token")?, &root)?;
    let unrelated = directory.path().join("outside");
    std::fs::create_dir_all(&unrelated)?;
    let alias = module.join("escape");
    std::os::unix::fs::symlink(&unrelated, &alias)?;
    model["modules"][0]["variants"][0]["components"][0]["sources"][1]["path"] =
        json!(alias.join("not-created/generated"));
    assert!(
        parse_model_with_context(&output(&model), &root, &paths).is_err(),
        "A missing generated root cannot hide a symlink escape"
    );
    let alias = directory.path().join("module-alias");
    std::os::unix::fs::symlink(&module, &alias)?;
    model = original;
    model["modules"][0]["directory"] = json!(alias);
    assert!(
        parse_model_with_context(&output(&model), &root, &paths).is_err(),
        "A directory alias is not the exact evaluated module identity"
    );
    Ok(())
}

#[test]
fn library_variants_without_apk_metadata_have_build_test_and_lint_task_identities() -> Result<()> {
    let (_directory, root, module, model) = fixture()?;
    let (store, handle) = evaluated_store(&root, &module, "complete")?;
    let paths =
        EvaluatedModelPaths::capture(&store, &store.token(handle).context("Token")?, &root)?;
    let parsed = parse_model_with_context(&output(&model), &root, &paths)?;
    let variant = parsed
        .default_library_variant()
        .context("Library variant")?;
    assert_eq!(parsed.library_variants(), std::slice::from_ref(&variant));
    assert_eq!(variant.label(), ":lib · debug");
    assert_eq!(variant.gradle_task("assemble", ""), ":lib:assembleDebug");
    assert_eq!(
        variant.gradle_task("test", "UnitTest"),
        ":lib:testDebugUnitTest"
    );
    assert_eq!(variant.gradle_task("lint", ""), ":lib:lintDebug");
    assert!(parsed.default_target().is_none());
    assert!(parsed.targets().is_empty());
    Ok(())
}
