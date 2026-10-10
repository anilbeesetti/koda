//! Supplemental handoff/filter regressions. Original Gradle/tree tests remain unported.

use android_tools::{
    evaluated_tree_inputs::{
        EvaluatedTreeInputs, SelectedMainGeneratedRoots, prepare_selected_main_generated_roots,
    },
    generated_artifacts::ModelConsumerVersion,
    project_model::{ModelState, ModelToken, VariantId},
    project_tree::SourceGroup,
    project_tree_adapter::{
        AdapterUnavailableReason, CapturedModulePresentation, KotlinCapability,
        prepare_module_roots_with_generated,
    },
    project_tree_facts::FactsUnavailableReason,
};
use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

fn available(value: Value) -> Value {
    json!({"status":"available", "value":value})
}
fn unsupported(detail: &str) -> Value {
    json!({"status":"unavailable", "value":{"capability":"official getter", "detail":detail}})
}
fn consumer() -> ModelConsumerVersion {
    ModelConsumerVersion {
        major: 1,
        minor: 0,
        description: Some("explicit supplemental consumer".into()),
    }
}
fn id(variant: &str) -> VariantId {
    VariantId {
        module: ":app".into(),
        variant: variant.into(),
    }
}
fn record(value: &Value) -> String {
    format!("KODA_ANDROID_PROJECT_MODEL={value}")
}

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    value: Value,
}
impl Fixture {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        let provider = |name: &str| json!({"name":name, "roots":[]});
        let container = |name: &str| json!({"main":provider(name), "hostTests":[], "deviceTests":[], "fixtures":null});
        let artifact = |name: &str| {
            json!({"status":"present", "value":{"kind":"android",
            "generatedSourceFolders":available(json!([root.join(format!("custom-build/generated/source/buildConfig/{name}"))])),
            "generatedResourceFolders":available(json!([root.join(format!("custom-build/generated/res/{name}"))])),
            "generatedAssetsFolders":available(json!([root.join(format!("custom-build/generated/assets/{name}"))])),
            "generatedClassPaths":available(json!([])), "classesFolders":available(json!([])),
            "ideSetupTaskNames":available(json!([])), "sourceGenTaskName":available(Value::Null), "resGenTaskName":available(Value::Null)}})
        };
        let value = json!({"version":1,"root":root,"diagnostics":[],"modules":[{
            "path":":app","directory":root,"kind":"application","namespace":"example",
            "variants":[{"name":"debug","components":[{"name":"debug","scope":"main","sources":[
                {"path":root.join("native-main-only"),"kind":"java","generated":true}],"dependencies":[]}],"outputListing":null},
                {"name":"release","components":[{"name":"release","scope":"main","sources":[],"dependencies":[]}],"outputListing":null}],
            "evaluatedProviders":{"status":"available","value":{"version":1,"agpVersion":"9.4.0","modelProducer":{"major":23,"minor":0},
                "defaultSourceSet":container("main"),"buildTypes":[{"name":"debug","container":container("debug")},{"name":"release","container":container("release")}],
                "productFlavors":[],"variants":[{"name":"debug","buildType":"debug","productFlavors":[],"main":{"multiFlavor":null,"variant":null},"hostTests":[],"deviceTests":[],"fixtures":null,"testSuites":[]},
                {"name":"release","buildType":"release","productFlavors":[],"main":{"multiFlavor":null,"variant":null},"hostTests":[],"deviceTests":[],"fixtures":null,"testSuites":[]}],"testSuites":[],"nativeMembership":null}}}],
            "generatedArtifacts":{"schema":1,"root":root,"modules":[{"module":":app","directory":root,
                "versions":available(json!({"agp":"9.4.0","producer":{"major":23,"minor":0,"description":null},
                    "minimumConsumer":available(json!({"major":1,"minor":0,"description":null})),
                    "models":[{"name":"model_producer","version":{"major":23,"minor":0,"description":null}},
                        {"name":"android_project","version":{"major":0,"minor":1,"description":null}},
                        {"name":"minimum_model_consumer","version":{"major":1,"minor":0,"description":null}}]})),
                "buildFolder":available(json!(root.join("custom-build"))),"variants":available(json!([
                    {"name":"debug","main":available(artifact("debug")),"hostTests":available(json!([])),"deviceTests":available(json!([])),"fixtures":available(json!({"status":"absent"}))},
                    {"name":"release","main":available(artifact("release")),"hostTests":available(json!([])),"deviceTests":available(json!([])),"fixtures":available(json!({"status":"absent"}))}]))}]}});
        Ok(Self {
            _directory: directory,
            root,
            value,
        })
    }
    fn root(&self) -> &Path {
        &self.root
    }
    fn publish(&self, state: &mut ModelState) -> Result<ModelToken> {
        let token = state.invalidate(Some(self.root().to_path_buf()));
        let capture =
            EvaluatedTreeInputs::decode_sync(&record(&self.value), self.root(), None, &token)?;
        state.publish_evaluated(&token, capture)?;
        Ok(token)
    }
    fn selected(&self) -> Result<(ModelState, SelectedMainGeneratedRoots)> {
        let mut state = ModelState::default();
        self.publish(&mut state)?;
        state.select(Some(id("debug")))?;
        let roots = prepare_selected_main_generated_roots(
            &state,
            &state.token(),
            id("debug"),
            &consumer(),
        )?;
        Ok((state, roots))
    }
    fn main(&mut self) -> &mut Value {
        &mut self.value["generatedArtifacts"]["modules"][0]["variants"]["value"][0]["main"]["value"]
            ["value"]
    }
}

#[test]
fn sync_publication_retains_raw_generated_facts_and_missing_import_without_guessing_consumer()
-> Result<()> {
    let fixture = Fixture::new()?;
    let mut state = ModelState::default();
    fixture.publish(&mut state)?;
    let capture = state
        .evaluated_inputs()
        .context("Published evaluated capture")?;
    assert_eq!(capture.raw_record(), record(&fixture.value));
    assert_eq!(
        capture.raw_generated_artifacts()?,
        &fixture.value["generatedArtifacts"]
    );
    assert_eq!(
        capture
            .import_facts()
            .expect_err("Missing import sidecar")
            .reason,
        FactsUnavailableReason::MissingMetadata
    );
    assert_eq!(
        capture
            .generated_artifacts(None)
            .expect_err("No guessed consumer")
            .reason,
        FactsUnavailableReason::Capability
    );
    capture.generated_artifacts(Some(&consumer()))?;
    assert!(Arc::ptr_eq(
        capture.model(),
        state.model.as_ref().context("Published Basic model")?
    ));
    Ok(())
}

#[test]
fn legacy_missing_and_explicit_null_sidecars_remain_distinct_and_keep_basic_model() -> Result<()> {
    for (sidecar, expected) in [
        (None, FactsUnavailableReason::MissingMetadata),
        (Some(Value::Null), FactsUnavailableReason::Malformed),
    ] {
        let mut fixture = Fixture::new()?;
        fixture
            .value
            .as_object_mut()
            .context("Object")?
            .remove("generatedArtifacts");
        if let Some(value) = sidecar {
            fixture.value["generatedArtifacts"] = value;
        }
        let mut state = ModelState::default();
        fixture.publish(&mut state)?;
        assert!(state.model.is_some());
        assert_eq!(
            state
                .evaluated_inputs()
                .context("Capture")?
                .generated_artifacts(None)
                .expect_err("Explicit unavailable")
                .reason,
            expected
        );
    }
    Ok(())
}

#[test]
fn original_import_binding_survives_selection_changes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    for name in ["android", "strange-parent", "shared-directory"] {
        fs::create_dir(root.join(name))?;
    }
    let value: Value = serde_json::from_str(
        &include_str!("../test_data/import_facts/wire-template.json")
            .replace("$ROOT", root.to_str().context("UTF8")?),
    )?;
    let mut state = ModelState::default();
    let token = state.invalidate(Some(root.clone()));
    let capture = EvaluatedTreeInputs::decode_sync(&record(&value), &root, None, &token)?;
    let binding = capture.import_facts()?.binding().clone();
    state.publish_evaluated(&token, capture)?;
    state.select(Some(VariantId {
        module: ":android".into(),
        variant: "debug".into(),
    }))?;
    let capture = state.evaluated_inputs().context("Capture")?;
    assert_eq!(capture.import_facts()?.binding(), &binding);
    assert_eq!(state.model_revision(), Some(binding.model_revision));
    assert!(!state.is_current(&token));
    capture
        .import_facts()?
        .ensure_current(capture.model(), &binding)?;
    Ok(())
}

#[test]
fn light_class_filters_are_inclusive_component_wise_and_preserve_reported_order() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let build = fixture.root().join("custom-build");
    let excluded = [
        "generated/source/r",
        "generated/not_namespaced_r_class_sources",
        "generated/data_binding_base_class_source_out",
        "generated/source/navigation-args",
    ];
    let survivors = vec![
        build.join("generated/source/buildConfig/debug"),
        build.join("generated/source/r-sibling"),
        build.join("generated/not_namespaced_r_class_sources-extra"),
        build.join("generated/data_binding_base_class_source_outside"),
        build.join("generated/source/navigation-args-other"),
        fixture.root().join("build/generated/source/r"),
        fixture.root().join("outside-build/generated"),
    ];
    let mut reported = survivors.clone();
    for relative in excluded {
        reported.insert(1, build.join(relative));
        reported.insert(2, build.join(relative).join("debug"));
    }
    fixture.main()["generatedSourceFolders"] = available(json!(reported));
    let (_, roots) = fixture.selected()?;
    assert_eq!(roots.java()?, survivors);
    Ok(())
}

#[test]
fn unavailable_source_filter_retains_independent_resource_and_asset_outcomes() -> Result<()> {
    for build_missing in [true, false] {
        let mut fixture = Fixture::new()?;
        if build_missing {
            fixture.value["generatedArtifacts"]["modules"][0]["buildFolder"] =
                unsupported("missing buildFolder");
        } else {
            fixture.main()["generatedSourceFolders"] =
                unsupported("missing generatedSourceFolders");
        }
        let (_, roots) = fixture.selected()?;
        let error = roots.java().expect_err("Do not claim filtering ran");
        assert_eq!(error.reason, FactsUnavailableReason::Capability);
        assert!(error.detail.contains(if build_missing {
            "missing buildFolder"
        } else {
            "missing generatedSourceFolders"
        }));
        assert_eq!(roots.resources()?.len(), 1);
        assert_eq!(roots.assets()?.len(), 1);
    }
    Ok(())
}

#[test]
fn classes_only_buildconfig_never_creates_a_java_root_and_valid_empty_stays_available() -> Result<()>
{
    let mut fixture = Fixture::new()?;
    let classes = fixture
        .root()
        .join("custom-build/generated/classes/BuildConfig.class");
    fixture.main()["generatedSourceFolders"] = available(json!([]));
    fixture.main()["classesFolders"] = available(json!([classes]));
    fixture.main()["generatedResourceFolders"] = available(json!([]));
    fixture.main()["generatedAssetsFolders"] = available(json!([]));
    let (_, roots) = fixture.selected()?;
    assert!(roots.java()?.is_empty());
    assert!(roots.resources()?.is_empty());
    assert!(roots.assets()?.is_empty());
    Ok(())
}

#[test]
fn selected_variant_changes_invalidate_old_roots_without_restamping_model_snapshot() -> Result<()> {
    let fixture = Fixture::new()?;
    let (mut state, roots) = fixture.selected()?;
    let revision = roots.model_revision();
    let snapshot = state
        .evaluated_inputs()
        .context("Capture")?
        .generated_artifacts(Some(&consumer()))?;
    state.select(Some(id("release")))?;
    assert_eq!(
        roots
            .ensure_current(&state)
            .expect_err("Old selection")
            .reason,
        FactsUnavailableReason::Stale
    );
    let release =
        prepare_selected_main_generated_roots(&state, &state.token(), id("release"), &consumer())?;
    assert_eq!(release.model_revision(), revision);
    assert_ne!(release.selection_revision(), roots.selection_revision());
    assert!(release.java()?.iter().all(|path| path.ends_with("release")));
    snapshot.ensure_current(release.model(), revision)?;
    assert_eq!(
        snapshot
            .ensure_current(release.model(), release.selection_revision())
            .expect_err("No restamp")
            .reason,
        FactsUnavailableReason::Stale
    );
    state.select(Some(id("debug")))?;
    assert!(
        roots.ensure_current(&state).is_err(),
        "Selection ABA must remain stale"
    );
    Ok(())
}

#[test]
fn root_aba_cancellation_and_out_of_order_publication_clear_capture() -> Result<()> {
    let fixture = Fixture::new()?;
    let other = Fixture::new()?;
    let (mut state, roots) = fixture.selected()?;
    let obsolete = state.invalidate(Some(fixture.root().to_path_buf()));
    let capture =
        EvaluatedTreeInputs::decode_sync(&record(&fixture.value), fixture.root(), None, &obsolete)?;
    state.invalidate(Some(other.root().to_path_buf()));
    let current = state.invalidate(Some(fixture.root().to_path_buf()));
    assert!(state.publish_evaluated(&obsolete, capture).is_err());
    assert!(
        state.evaluated_inputs().is_none() && state.model.is_none() && state.selected.is_none()
    );
    assert!(roots.ensure_current(&state).is_err());
    let recovery =
        EvaluatedTreeInputs::decode_sync(&record(&fixture.value), fixture.root(), None, &current)?;
    state.publish_evaluated(&current, recovery)?;
    state.invalidate(None);
    assert!(
        state.evaluated_inputs().is_none(),
        "Cancelled/removed root cannot retain current facts"
    );
    Ok(())
}

#[test]
fn stale_sidecar_and_invalid_variant_are_explicit_and_do_not_reuse_prior_facts() -> Result<()> {
    let mut fixture = Fixture::new()?;
    fixture.value["generatedArtifacts"]["root"] = json!(fixture.root().join("different"));
    let mut state = ModelState::default();
    fixture.publish(&mut state)?;
    assert_eq!(
        state
            .evaluated_inputs()
            .context("Capture")?
            .generated_artifacts(None)
            .expect_err("Live unknown consumer cannot hide stale sidecar")
            .reason,
        FactsUnavailableReason::Stale
    );
    assert_eq!(
        state
            .evaluated_inputs()
            .context("Capture")?
            .generated_artifacts(Some(&consumer()))
            .expect_err("Different sidecar root")
            .reason,
        FactsUnavailableReason::Stale
    );
    state.select(Some(id("debug")))?;
    assert_eq!(
        prepare_selected_main_generated_roots(&state, &state.token(), id("release"), &consumer())
            .expect_err("Unselected variant")
            .reason,
        FactsUnavailableReason::Stale
    );
    assert_eq!(
        prepare_selected_main_generated_roots(
            &state,
            &state.token(),
            VariantId {
                module: ":other".into(),
                variant: "debug".into()
            },
            &consumer()
        )
        .expect_err("Different module")
        .reason,
        FactsUnavailableReason::Stale
    );
    Ok(())
}

#[test]
fn unsupported_minimum_consumer_and_old_assets_capability_remain_unavailable() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut state = ModelState::default();
    fixture.publish(&mut state)?;
    let unsupported_consumer = ModelConsumerVersion {
        major: 0,
        minor: 9,
        description: None,
    };
    assert_eq!(
        state
            .evaluated_inputs()
            .context("Capture")?
            .generated_artifacts(Some(&unsupported_consumer))
            .expect_err("Minimum retained")
            .reason,
        FactsUnavailableReason::UnsupportedSchema
    );
    let mut fixture = fixture;
    let versions = &mut fixture.value["generatedArtifacts"]["modules"][0]["versions"]["value"];
    versions["producer"]["major"] = json!(10);
    versions["models"][0]["version"]["major"] = json!(10);
    let (_, roots) = fixture.selected()?;
    assert_eq!(
        roots
            .assets()
            .expect_err("Old assets need real fallback")
            .reason,
        FactsUnavailableReason::Capability
    );
    assert_eq!(roots.java()?.len(), 1);
    assert_eq!(roots.resources()?.len(), 1);
    Ok(())
}

#[test]
fn authoritative_v2_main_replaces_native_flags_and_missing_presentation_keeps_fallback()
-> Result<()> {
    let fixture = Fixture::new()?;
    let (state, roots) = fixture.selected()?;
    assert_eq!(
        prepare_module_roots_with_generated(&roots, &state, None)
            .expect_err("Presentation prerequisite")
            .reason,
        AdapterUnavailableReason::MissingPresentation
    );
    let presentation = CapturedModulePresentation {
        display_name: Some("app".into()),
        kotlin: KotlinCapability::Unknown,
        compact_packages: false,
    };
    let plan = prepare_module_roots_with_generated(&roots, &state, Some(&presentation))?;
    let generated_java = plan
        .source_roots()
        .iter()
        .filter(|root| root.group == SourceGroup::GeneratedJava)
        .map(|root| root.path.clone())
        .collect::<Vec<PathBuf>>();
    assert_eq!(generated_java, roots.java()?);
    assert!(
        !plan
            .required_presence_paths()
            .contains(&fixture.root().join("native-main-only"))
    );
    assert!(
        plan.source_roots()
            .iter()
            .any(|root| root.group == SourceGroup::GeneratedResources)
    );
    assert!(
        plan.source_roots()
            .iter()
            .any(|root| root.group == SourceGroup::GeneratedAssets)
    );
    Ok(())
}

#[test]
fn malformed_multiple_model_records_cannot_publish_an_evaluated_capture() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut state = ModelState::default();
    let token = state.invalidate(Some(fixture.root().to_path_buf()));
    let output = format!("{}\n{}", record(&fixture.value), record(&fixture.value));
    assert!(EvaluatedTreeInputs::decode_sync(&output, fixture.root(), None, &token).is_err());
    assert!(state.evaluated_inputs().is_none() && state.model.is_none());
    Ok(())
}

#[test]
fn absent_main_and_failed_main_getter_are_distinct_unavailable_outcomes() -> Result<()> {
    for (main, reason) in [
        (
            available(json!({"status":"absent"})),
            FactsUnavailableReason::UnsupportedShape,
        ),
        (
            unsupported("main getter failed"),
            FactsUnavailableReason::Capability,
        ),
    ] {
        let mut fixture = Fixture::new()?;
        fixture.value["generatedArtifacts"]["modules"][0]["variants"]["value"][0]["main"] = main;
        let mut state = ModelState::default();
        fixture.publish(&mut state)?;
        state.select(Some(id("debug")))?;
        assert_eq!(
            prepare_selected_main_generated_roots(&state, &state.token(), id("debug"), &consumer())
                .expect_err("No guessed main artifact")
                .reason,
            reason
        );
    }
    Ok(())
}

#[test]
fn legacy_publication_clears_evaluated_facts_and_retained_capture_cannot_override_new_sync()
-> Result<()> {
    let fixture = Fixture::new()?;
    let (mut state, roots) = fixture.selected()?;
    let current = state.token();
    let model = roots.model().clone();
    state.publish(&current, model)?;
    assert!(state.evaluated_inputs().is_none());
    assert!(roots.ensure_current(&state).is_err());
    let obsolete = state.invalidate(Some(fixture.root().to_path_buf()));
    let capture =
        EvaluatedTreeInputs::decode_sync(&record(&fixture.value), fixture.root(), None, &obsolete)?;
    fixture.publish(&mut state)?;
    assert!(state.publish_evaluated(&obsolete, capture).is_err());
    assert!(state.evaluated_inputs().is_some() && state.model.is_some());
    Ok(())
}

#[test]
fn other_artifact_generation_and_unsupported_manifest_keep_existing_adapter_policy() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let test_root = fixture.root().join("native-test-generated");
    let manifest = fixture
        .root()
        .join("native-main-manifest/AndroidManifest.xml");
    let components = fixture.value["modules"][0]["variants"][0]["components"]
        .as_array_mut()
        .context("Components")?;
    components[0]["sources"]
        .as_array_mut()
        .context("Sources")?
        .push(json!({"path":manifest,"kind":"manifest","generated":true}));
    components.push(
        json!({"name":"debugAndroidTest","scope":"androidTest","sources":[
        {"path":test_root,"kind":"java","generated":true}],"dependencies":[]}),
    );
    let (state, roots) = fixture.selected()?;
    let presentation = CapturedModulePresentation {
        display_name: Some("app".into()),
        kotlin: KotlinCapability::Unknown,
        compact_packages: false,
    };
    let plan = prepare_module_roots_with_generated(&roots, &state, Some(&presentation))?;
    assert!(
        plan.source_roots()
            .iter()
            .any(|root| root.group == SourceGroup::GeneratedJava && root.path == test_root)
    );
    assert!(
        plan.unsupported_roots()
            .iter()
            .any(|root| root.path == manifest)
    );
    assert!(
        !plan
            .source_roots()
            .iter()
            .any(|root| root.path == fixture.root().join("native-main-only"))
    );
    Ok(())
}

#[test]
fn unknown_linked_kotlin_keeps_shared_root_projection_unavailable() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let shared = fixture.root().join("src/main/shared");
    fixture.value["modules"][0]["evaluatedProviders"]["value"]["defaultSourceSet"]["main"]["roots"] = json!([
        {"path":shared,"kind":"java"},{"path":shared,"kind":"kotlin"}]);
    let (state, roots) = fixture.selected()?;
    let presentation = CapturedModulePresentation {
        display_name: Some("app".into()),
        kotlin: KotlinCapability::Unknown,
        compact_packages: false,
    };
    assert_eq!(
        prepare_module_roots_with_generated(&roots, &state, Some(&presentation))
            .expect_err("No Disabled guess for linked Kotlin")
            .reason,
        AdapterUnavailableReason::MissingKotlinCapability
    );
    Ok(())
}
