//! Supplemental handoff/filter regressions. Original Gradle/tree tests remain unported.

use android_tools::{
    evaluated_tree_inputs::{
        EvaluatedTreeInputs, SelectedMainGeneratedRoots, prepare_live_module_plan,
        prepare_selected_main_generated_roots,
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

// These fault-injection records test supported decoder/plan boundaries, not
// observations of a Gradle runtime or original navigator-method parity.
fn live_plan_fixture() -> Result<Fixture> {
    let mut fixture = Fixture::new()?;
    let root = fixture.root().to_path_buf();
    let observation =
        |getter: &str, value: Value| json!({"getter":getter,"result":available(value)});
    let project = |path: &str, name: &str, parent: Value| {
        json!({
            "projectName":observation("org.gradle.api.Project.getName()",json!(name)),
            "projectPath":observation("org.gradle.api.Project.getPath()",json!(path)),
            "projectDirectory":observation("org.gradle.api.Project.getProjectDir().getCanonicalPath()",json!(root)),
            "rootName":observation("org.gradle.api.Project.getRootProject().getName()",json!("rust-root")),
            "rootDirectory":observation("org.gradle.api.Project.getRootDir().getCanonicalPath()",json!(root)),
            "parentProjectPath":observation("org.gradle.api.Project.getParent()?.getPath()",parent),
            "buildTreePath":observation("org.gradle.api.Project.getBuildTreePath()",json!(path))
        })
    };
    let root_project = project(":", "rust-root", Value::Null);
    fixture.value["importFacts"] = json!({"schema":1,"root":root,"gradleVersion":"9.6.1",
        "modules":[{"module":":app","directory":root,"kind":"application","variants":["debug","release"]}],
        "buildIdentity":{"rootName":root_project["rootName"],"rootDirectory":root_project["rootDirectory"],
            "projectPath":root_project["projectPath"],"buildTreePath":root_project["buildTreePath"]},
        "projectCatalogue":observation("org.gradle.api.Project.getAllprojects()",json!([
            root_project, project(":app","arbitrary-directory-name",json!(":"))]))});
    fixture.value["kotlinCapabilities"] = json!({"schema":1,"root":root,"modules":[{
        "module":":app","directory":root,"agpVersion":"9.4.0",
        "sdkPluginVersion":observation("com.android.build.api.AndroidPluginVersion.getMajor/getMinor/getMicro/getPreview/getPreviewType/getVersion",
            json!({"major":9,"minor":4,"micro":0,"preview":0,"previewType":null,"version":"9.4.0"})),
        "kotlinAndroid":observation("org.gradle.api.plugins.PluginManager.hasPlugin(org.jetbrains.kotlin.android)",json!(false)),
        "kotlinMultiplatform":observation("org.gradle.api.plugins.PluginManager.hasPlugin(org.jetbrains.kotlin.multiplatform)",json!(false)),
        "kotlinMultiplatformAndroidTarget":observation("KotlinMultiplatformExtension.getTargets().getPlatformType(androidJvm)",json!(false)),
        "builtInKotlin":observation("AGP9.4.0:BuiltInKotlinServicesKt.builtInKotlinEnabledForProject(ProjectServices,CommonExtension)",json!(true)),
        "builtInKotlinDefault":observation("AndroidProject.flags.getFlagValue(BUILT_IN_KOTLIN_DEFAULT_ENABLED)",json!(true))
    }]});
    let shared = root.join("src/main/shared");
    fixture.value["modules"][0]["evaluatedProviders"]["value"]["defaultSourceSet"]["main"]["roots"] = json!([
        {"path":shared,"kind":"java"},{"path":shared,"kind":"kotlin"}]);
    Ok(fixture)
}

fn failed_observation(value: &mut Value, field: &str, detail: &str) {
    let observation = &mut value["kotlinCapabilities"]["modules"][0][field];
    observation["result"] = json!({"status":"unavailable","value":{
        "capability":observation["getter"],"detail":detail}});
}

#[test]
fn actual_legacy_exporter_model_keeps_physical_fallback_without_authoritative_live_facts()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    fs::create_dir_all(root.join("app"))?;
    let original =
        include_str!("../test_data/evaluated_tree_inputs/source-providers-smoke-model.json");
    let value: Value = serde_json::from_str(&original.replace(
        "/workspace/android-studio-artifacts/source-providers-smoke/project",
        root.to_str().context("Temporary root must be UTF-8")?,
    ))?;
    let mut state = ModelState::default();
    let token = state.invalidate(Some(root.clone()));
    let capture = EvaluatedTreeInputs::decode_sync(&record(&value), &root, None, &token)?;
    assert_eq!(capture.model().modules.len(), 1);
    assert!(capture.model().modules[0].source_providers.is_some());
    assert_eq!(
        capture
            .kotlin_capability(":app")
            .expect_err("Legacy Kotlin unknown")
            .reason,
        FactsUnavailableReason::MissingMetadata
    );
    state.publish_evaluated(&token, capture)?;
    let variant = state
        .model
        .as_ref()
        .context("Basic model retained")?
        .modules[0]
        .default_variant
        .clone()
        .context("Actual exported default variant")?;
    state.select(Some(id(&variant)))?;
    let error = prepare_live_module_plan(&state, &state.token(), id(&variant), false)
        .expect_err("Legacy export cannot produce authoritative live groups");
    assert_eq!(
        error.reason,
        AdapterUnavailableReason::Provider(FactsUnavailableReason::MissingMetadata)
    );
    assert!(state.model.is_some());
    Ok(())
}

#[test]
fn live_plan_uses_effective_builtin_kotlin_and_explicit_rust_importer_identity() -> Result<()> {
    let fixture = live_plan_fixture()?;
    let mut state = ModelState::default();
    fixture.publish(&mut state)?;
    state.select(Some(id("debug")))?;
    let plan = prepare_live_module_plan(&state, &state.token(), id("debug"), true)?;
    assert_eq!(plan.imported_identity().internal_name, "rust-root:app");
    assert_eq!(
        plan.imported_identity().holder_internal_name,
        "rust-root:app"
    );
    assert_eq!(plan.imported_identity().external_project_id, ":app");
    assert_eq!(plan.plan().binding().variant, "debug");
    assert_eq!(plan.selected(), &id("debug"));
    assert_eq!(plan.model_token(), &state.token());
    assert!(
        plan.plan()
            .source_roots()
            .iter()
            .any(|root| root.group == SourceGroup::KotlinAndJava)
    );
    assert!(
        plan.plan()
            .source_roots()
            .iter()
            .filter(|root| root.group == SourceGroup::GeneratedJava)
            .all(|root| root.path.ends_with("debug"))
    );
    plan.ensure_current(&state)?;
    Ok(())
}

#[test]
fn disabled_per_module_kotlin_moves_shared_roots_even_when_builtin_default_is_enabled() -> Result<()>
{
    let mut fixture = live_plan_fixture()?;
    fixture.value["kotlinCapabilities"]["modules"][0]["builtInKotlin"]["result"] =
        available(json!(false));
    let mut state = ModelState::default();
    fixture.publish(&mut state)?;
    state.select(Some(id("debug")))?;
    let plan = prepare_live_module_plan(&state, &state.token(), id("debug"), false)?;
    assert!(
        plan.plan()
            .source_roots()
            .iter()
            .any(|root| root.group == SourceGroup::Java)
    );
    assert!(
        !plan
            .plan()
            .source_roots()
            .iter()
            .any(|root| root.group == SourceGroup::KotlinAndJava)
    );
    assert_eq!(
        state
            .evaluated_inputs()
            .context("Capture")?
            .kotlin_capability(":app")?,
        KotlinCapability::Disabled
    );
    Ok(())
}

#[test]
fn effective_getter_failure_is_not_replaced_by_default_flag_or_configured_kotlin_roots()
-> Result<()> {
    let mut fixture = live_plan_fixture()?;
    failed_observation(
        &mut fixture.value,
        "builtInKotlin",
        "CommonExtension getter is absent",
    );
    let mut state = ModelState::default();
    fixture.publish(&mut state)?;
    state.select(Some(id("debug")))?;
    let error = prepare_live_module_plan(&state, &state.token(), id("debug"), false)
        .expect_err("Default true is not effective support");
    assert_eq!(
        error.reason,
        AdapterUnavailableReason::Provider(FactsUnavailableReason::Capability)
    );
    assert!(error.detail.contains("CommonExtension getter is absent"));
    assert!(state.model.is_some());
    Ok(())
}

#[test]
fn observed_android_kgp_plugin_survives_unsupported_builtin_adapter() -> Result<()> {
    for field in ["kotlinAndroid", "kotlinMultiplatform"] {
        let mut fixture = live_plan_fixture()?;
        fixture.value["kotlinCapabilities"]["modules"][0][field]["result"] = available(json!(true));
        fixture.value["kotlinCapabilities"]["modules"][0]["kotlinMultiplatformAndroidTarget"]["result"] =
            available(json!(field == "kotlinMultiplatform"));
        failed_observation(
            &mut fixture.value,
            "builtInKotlin",
            "Android KMP has no CommonExtension",
        );
        let mut state = ModelState::default();
        fixture.publish(&mut state)?;
        state.select(Some(id("debug")))?;
        let plan = prepare_live_module_plan(&state, &state.token(), id("debug"), false)?;
        assert!(
            plan.plan()
                .source_roots()
                .iter()
                .any(|root| root.group == SourceGroup::KotlinAndJava)
        );
    }
    Ok(())
}

#[test]
fn live_plan_rejects_unproven_v2_schema_and_producer_minimum_never_expands_support() -> Result<()> {
    let mut fixture = live_plan_fixture()?;
    fixture.value["generatedArtifacts"]["modules"][0]["versions"]["value"]["models"][1]["version"]
        ["minor"] = json!(2);
    let mut state = ModelState::default();
    fixture.publish(&mut state)?;
    state.select(Some(id("debug")))?;
    let error = prepare_live_module_plan(&state, &state.token(), id("debug"), false)
        .expect_err("A minimum consumer below ours does not prove this schema");
    assert_eq!(
        error.reason,
        AdapterUnavailableReason::Provider(FactsUnavailableReason::UnsupportedSchema)
    );
    Ok(())
}

#[test]
fn live_plan_preserves_unsupported_selected_suite_instead_of_omitting_it() -> Result<()> {
    let mut fixture = live_plan_fixture()?;
    fixture.value["modules"][0]["evaluatedProviders"]["value"]["variants"][0]["testSuites"] =
        json!(["integration"]);
    fixture.value["modules"][0]["evaluatedProviders"]["value"]["testSuites"] = json!([
        {"name":"integration","providers":null}]);
    let mut state = ModelState::default();
    fixture.publish(&mut state)?;
    state.select(Some(id("debug")))?;
    let error = prepare_live_module_plan(&state, &state.token(), id("debug"), false)
        .expect_err("Selected integration suite cannot be treated as an empty collection");
    assert_eq!(
        error.reason,
        AdapterUnavailableReason::Provider(FactsUnavailableReason::UnsupportedShape)
    );
    assert!(error.detail.contains("integration"));
    Ok(())
}

#[test]
fn live_plan_keeps_selection_aba_and_failed_sync_stale() -> Result<()> {
    let fixture = live_plan_fixture()?;
    let mut state = ModelState::default();
    fixture.publish(&mut state)?;
    state.select(Some(id("debug")))?;
    let old = prepare_live_module_plan(&state, &state.token(), id("debug"), false)?;
    state.select(Some(id("release")))?;
    assert!(old.ensure_current(&state).is_err());
    let release = prepare_live_module_plan(&state, &state.token(), id("release"), false)?;
    assert!(
        release
            .plan()
            .source_roots()
            .iter()
            .filter(|root| root.group == SourceGroup::GeneratedJava)
            .all(|root| root.path.ends_with("release"))
    );
    assert!(prepare_live_module_plan(&state, &state.token(), id("debug"), false).is_err());
    state.select(Some(id("debug")))?;
    assert!(old.ensure_current(&state).is_err());
    state.invalidate(Some(fixture.root().to_path_buf()));
    assert!(release.ensure_current(&state).is_err());
    assert!(prepare_live_module_plan(&state, &state.token(), id("debug"), false).is_err());
    Ok(())
}

#[test]
fn kotlin_sidecar_rejects_wrong_root_directory_getter_and_positional_modules() -> Result<()> {
    let fixture = live_plan_fixture()?;
    let mut mutations = Vec::new();
    let mut value = fixture.value.clone();
    value["kotlinCapabilities"]["root"] = json!(fixture.root().join("other"));
    mutations.push((value, FactsUnavailableReason::Stale));
    let mut value = fixture.value.clone();
    value["kotlinCapabilities"]["modules"][0]["directory"] = json!(fixture.root().join("other"));
    mutations.push((value, FactsUnavailableReason::Stale));
    let mut value = fixture.value.clone();
    value["kotlinCapabilities"]["modules"][0]["builtInKotlin"]["getter"] =
        json!("guessed from .kt extension");
    mutations.push((value, FactsUnavailableReason::Malformed));
    let mut value = fixture.value.clone();
    value["kotlinCapabilities"]["modules"][0] = json!([":app", fixture.root(), "9.4.0"]);
    mutations.push((value, FactsUnavailableReason::Malformed));
    let mut value = fixture.value.clone();
    value["kotlinCapabilities"]["modules"][0]["unexpected"] = json!(true);
    mutations.push((value, FactsUnavailableReason::Malformed));
    for (value, expected) in mutations {
        let mut state = ModelState::default();
        let token = state.invalidate(Some(fixture.root().to_path_buf()));
        let capture =
            EvaluatedTreeInputs::decode_sync(&record(&value), fixture.root(), None, &token)?;
        assert_eq!(
            capture
                .kotlin_capability(":app")
                .expect_err("Unavailable capability")
                .reason,
            expected
        );
        state.publish_evaluated(&token, capture)?;
        assert!(
            state.model.is_some(),
            "Capability failure cannot erase the physical model"
        );
    }
    Ok(())
}

#[test]
fn kotlin_plugin_on_jvm_only_module_never_creates_android_groups_in_mixed_project() -> Result<()> {
    let mut fixture = live_plan_fixture()?;
    // The Android importer requires an Android module. Keep that admission
    // anchor separate from the JVM module whose Android facts must be rejected.
    let anchor_directory = fixture.root().join("android-anchor");
    fs::create_dir_all(&anchor_directory)?;
    let mut anchor = fixture.value["modules"][0].clone();
    anchor["path"] = json!(":androidAnchor");
    anchor["directory"] = json!(anchor_directory);
    let mut anchor_generated = fixture.value["generatedArtifacts"]["modules"][0].clone();
    anchor_generated["module"] = json!(":androidAnchor");
    anchor_generated["directory"] = json!(anchor_directory);
    let mut anchor_kotlin = fixture.value["kotlinCapabilities"]["modules"][0].clone();
    anchor_kotlin["module"] = json!(":androidAnchor");
    anchor_kotlin["directory"] = json!(anchor_directory);
    fixture.value["modules"]
        .as_array_mut()
        .context("Basic module catalogue")?
        .push(anchor);
    let mut anchor_import = fixture.value["importFacts"]["modules"][0].clone();
    anchor_import["module"] = json!(":androidAnchor");
    anchor_import["directory"] = json!(anchor_directory);
    fixture.value["importFacts"]["modules"]
        .as_array_mut()
        .context("Import module catalogue")?
        .push(anchor_import);
    let mut anchor_project =
        fixture.value["importFacts"]["projectCatalogue"]["result"]["value"][1].clone();
    for field in ["projectPath", "buildTreePath"] {
        anchor_project[field]["result"] = available(json!(":androidAnchor"));
    }
    anchor_project["projectName"]["result"] = available(json!("android-anchor"));
    anchor_project["projectDirectory"]["result"] = available(json!(anchor_directory));
    fixture.value["importFacts"]["projectCatalogue"]["result"]["value"]
        .as_array_mut()
        .context("Observed project catalogue")?
        .push(anchor_project);
    fixture.value["modules"][0]["kind"] = json!("jvm");
    fixture.value["importFacts"]["modules"][0]["kind"] = json!("jvm");
    fixture.value["generatedArtifacts"]["modules"] = json!([]);
    fixture.value["generatedArtifacts"]["modules"]
        .as_array_mut()
        .context("Generated Android module catalogue")?
        .push(anchor_generated);
    fixture.value["kotlinCapabilities"]["modules"]
        .as_array_mut()
        .context("Kotlin module catalogue")?
        .push(anchor_kotlin);
    fixture.value["kotlinCapabilities"]["modules"][0]["kotlinMultiplatform"]["result"] =
        available(json!(true));
    let mut state = ModelState::default();
    fixture.publish(&mut state)?;
    assert_eq!(
        state
            .evaluated_inputs()
            .context("Capture")?
            .kotlin_capability(":app")
            .expect_err("JVM module is not Android")
            .reason,
        FactsUnavailableReason::Stale
    );
    state.select(Some(id("debug")))?;
    assert!(prepare_live_module_plan(&state, &state.token(), id("debug"), false).is_err());
    assert!(state.model.is_some());
    let current = state
        .model
        .as_ref()
        .context("Supported mixed physical model")?;
    assert_eq!(current.modules.len(), 2);
    assert_eq!(
        current
            .variant(&id("debug"))
            .context("JVM variant retained")?
            .0
            .path,
        ":app"
    );
    // Removing the forged JVM capability row restores authoritative facts for
    // the Android anchor, while the JVM module still cannot get Android groups.
    let removed_capability = fixture.value["kotlinCapabilities"]["modules"]
        .as_array_mut()
        .context("Kotlin module catalogue")?
        .remove(0);
    assert_eq!(removed_capability["module"], ":app");
    fixture.publish(&mut state)?;
    state.select(Some(id("debug")))?;
    assert!(prepare_live_module_plan(&state, &state.token(), id("debug"), false).is_err());
    let anchor = VariantId {
        module: ":androidAnchor".into(),
        variant: "debug".into(),
    };
    state.select(Some(anchor.clone()))?;
    let plan = prepare_live_module_plan(&state, &state.token(), anchor, false)?;
    assert_eq!(plan.plan().binding().module, ":androidAnchor");
    assert_eq!(
        plan.imported_identity().external_project_path,
        anchor_directory
    );
    plan.ensure_current(&state)?;
    Ok(())
}

#[test]
fn all_jvm_kmp_sync_is_rejected_without_publishing_android_groups_or_losing_physical_files()
-> Result<()> {
    let mut fixture = live_plan_fixture()?;
    fixture.value["modules"][0]["kind"] = json!("jvm");
    fixture.value["importFacts"]["modules"][0]["kind"] = json!("jvm");
    fixture.value["generatedArtifacts"]["modules"] = json!([]);
    fixture.value["kotlinCapabilities"]["modules"][0]["kotlinMultiplatform"]["result"] =
        available(json!(true));
    let physical_file = fixture.root().join("GenericEdit.py");
    let contents = "print('physical project remains available')\n";
    fs::write(&physical_file, contents)?;
    let mut state = ModelState::default();
    let failure = fixture
        .publish(&mut state)
        .expect_err("All-JVM records are outside Android importer admission");
    assert_eq!(
        failure.to_string(),
        "No supported Android modules were found"
    );
    assert!(state.model.is_none());
    assert!(state.selected.is_none());
    assert!(state.evaluated_inputs().is_none());
    assert!(prepare_live_module_plan(&state, &state.token(), id("debug"), false).is_err());
    assert_eq!(fs::read_to_string(&physical_file)?, contents);
    assert_eq!(physical_file.parent(), Some(fixture.root()));
    Ok(())
}

#[test]
fn applied_kmp_without_observed_android_target_cannot_supply_kotlin_enablement() -> Result<()> {
    for target in [
        available(json!(false)),
        unsupported("Android target getter failed"),
    ] {
        let mut fixture = live_plan_fixture()?;
        fixture.value["kotlinCapabilities"]["modules"][0]["kotlinMultiplatform"]["result"] =
            available(json!(true));
        let target_getter = fixture.value["kotlinCapabilities"]["modules"][0]["kotlinMultiplatformAndroidTarget"]["getter"].clone();
        let mut target = target;
        if target["status"] == "unavailable" {
            target["value"]["capability"] = target_getter;
        }
        fixture.value["kotlinCapabilities"]["modules"][0]["kotlinMultiplatformAndroidTarget"]["result"] =
            target;
        failed_observation(
            &mut fixture.value,
            "builtInKotlin",
            "Built-in Kotlin is not applicable to this KMP module",
        );
        let mut state = ModelState::default();
        fixture.publish(&mut state)?;
        state.select(Some(id("debug")))?;
        let failure = prepare_live_module_plan(&state, &state.token(), id("debug"), false)
            .expect_err("KMP ecosystem observation cannot create Android Kotlin groups");
        assert_eq!(
            failure.reason,
            AdapterUnavailableReason::Provider(FactsUnavailableReason::Capability)
        );
    }
    Ok(())
}

#[test]
fn actual_unavailable_exporter_attempt_retains_effective_getter_and_consumer_failures() -> Result<()>
{
    for (raw, expected_default) in [
        (
            include_str!(
                "../test_data/evaluated_tree_inputs/attempt1-default-full-stdout-stderr.log"
            ),
            true,
        ),
        (
            include_str!(
                "../test_data/evaluated_tree_inputs/attempt1-built-in-disabled-full-stdout-stderr.log"
            ),
            false,
        ),
    ] {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        fs::create_dir_all(root.join("app"))?;
        let raw = raw.replace(
            "/workspace/android-studio-artifacts/source-providers-smoke/project",
            root.to_str().context("Temporary root must be UTF-8")?,
        );
        let mut state = ModelState::default();
        let token = state.invalidate(Some(root.clone()));
        let capture = EvaluatedTreeInputs::decode_sync(&raw, &root, None, &token)?;
        let wire: Value = serde_json::from_str(
            capture
                .raw_record()
                .strip_prefix("KODA_ANDROID_PROJECT_MODEL=")
                .context("Actual model prefix")?,
        )?;
        let kotlin = &wire["kotlinCapabilities"]["modules"][0];
        assert_eq!(kotlin["agpVersion"], "Android Gradle Plugin version 9.4.0");
        assert_eq!(kotlin["builtInKotlin"]["result"]["status"], "unavailable");
        assert!(
            kotlin["builtInKotlin"]["result"]["value"]["detail"]
                .as_str()
                .context("Retained actual getter failure")?
                .contains("adapter supports AGP9.4.0 only")
        );
        assert_eq!(
            kotlin["builtInKotlinDefault"]["result"]["value"],
            expected_default
        );
        assert_eq!(
            capture
                .kotlin_capability(":app")
                .expect_err("No effective getter observation")
                .reason,
            FactsUnavailableReason::Capability
        );
        assert_eq!(
            capture
                .generated_artifacts(Some(&consumer()))
                .expect_err("Historical explicit1.0 consumer remains incompatible")
                .reason,
            FactsUnavailableReason::UnsupportedSchema
        );
        let generated = capture.generated_artifacts(Some(
            &android_tools::evaluated_tree_inputs::rust_v2_tree_consumer(),
        ))?;
        let module = generated
            .modules()
            .first()
            .context("Actual generated module")?;
        assert_eq!(
            module
                .versions
                .available()?
                .minimum_consumer
                .available()?
                .major,
            66
        );
        assert_eq!(
            module
                .versions
                .available()?
                .minimum_consumer
                .available()?
                .minor,
            1
        );
        let variant = generated.variant(":app", "demoDebug")?;
        let android_tools::generated_artifacts::ArtifactSlot::Present(main) =
            variant.main.available()?
        else {
            anyhow::bail!("Actual MAIN artifact is absent");
        };
        assert_eq!(
            main.generated_source_folders.available()?,
            &[root.join("app/build/generated/ap_generated_sources/demoDebug/out")]
        );
        assert!(
            main.generated_resource_folders.available()?.is_empty(),
            "The actual smoke model reports an available empty generated-resource collection"
        );
        assert_eq!(
            main.generated_assets(module.versions.available()?)?,
            &[root.join("app/build/generated/assets/createAssets")]
        );
        let imports = capture.import_facts()?;
        imports.ensure_current(capture.model(), imports.binding())?;
        assert_eq!(
            imports
                .project(":app")?
                .project_directory
                .as_ref()
                .context("Actual project directory observation")?
                .available()?,
            &root.join("app")
        );
        state.publish_evaluated(&token, capture)?;
        state.select(Some(VariantId {
            module: ":app".into(),
            variant: "demoDebug".into(),
        }))?;
        assert!(
            prepare_live_module_plan(
                &state,
                &state.token(),
                VariantId {
                    module: ":app".into(),
                    variant: "demoDebug".into()
                },
                false
            )
            .is_err(),
            "This actual failed attempt cannot become a positive live-plan fixture"
        );
    }
    Ok(())
}

#[test]
fn pinned_live_subset_rejects_higher_minimum_and_other_agp_or_producer_tuples() -> Result<()> {
    for alteration in ["minimum", "agp", "producer"] {
        let mut fixture = live_plan_fixture()?;
        let versions = &mut fixture.value["generatedArtifacts"]["modules"][0]["versions"]["value"];
        match alteration {
            "minimum" => {
                versions["minimumConsumer"]["value"]["major"] = json!(67);
                versions["minimumConsumer"]["value"]["minor"] = json!(0);
                versions["models"][2]["version"]["major"] = json!(67);
                versions["models"][2]["version"]["minor"] = json!(0);
            }
            "agp" => versions["agp"] = json!("9.5.0"),
            "producer" => {
                versions["producer"]["minor"] = json!(1);
                versions["models"][0]["version"]["minor"] = json!(1);
            }
            _ => anyhow::bail!("Unexpected alteration"),
        }
        let mut state = ModelState::default();
        fixture.publish(&mut state)?;
        state.select(Some(id("debug")))?;
        let failure = prepare_live_module_plan(&state, &state.token(), id("debug"), false)
            .expect_err("A producer's declared minimum cannot expand this supported Rust subset");
        assert_eq!(
            failure.reason,
            AdapterUnavailableReason::Provider(FactsUnavailableReason::UnsupportedSchema)
        );
    }
    Ok(())
}

#[test]
fn structured_sdk_version_rejects_contradictory_or_positional_values_and_unproven_previews()
-> Result<()> {
    for (value, expected) in [
        (
            json!({"major":9,"minor":4,"micro":0,"preview":0,"previewType":null,"version":"9.5.0"}),
            FactsUnavailableReason::Malformed,
        ),
        (
            json!([9, 4, 0, 0, null, "9.4.0"]),
            FactsUnavailableReason::Malformed,
        ),
        (
            json!({"major":9,"minor":4,"micro":0,"preview":1,"previewType":"alpha","version":"9.4.0"}),
            FactsUnavailableReason::Capability,
        ),
    ] {
        let mut fixture = live_plan_fixture()?;
        fixture.value["kotlinCapabilities"]["modules"][0]["sdkPluginVersion"]["result"] =
            available(value);
        let mut state = ModelState::default();
        fixture.publish(&mut state)?;
        assert_eq!(
            state
                .evaluated_inputs()
                .context("Capture")?
                .kotlin_capability(":app")
                .expect_err("Only the observed supported stable SDK contract may be used")
                .reason,
            expected
        );
    }
    Ok(())
}

fn actual_live_exporter_fixtures()
-> [(&'static str, &'static str, KotlinCapability, SourceGroup); 2] {
    [
        (
            include_str!(
                "../test_data/evaluated_tree_inputs/attempt2-default-full-stdout-stderr.log"
            ),
            include_str!(
                "../test_data/evaluated_tree_inputs/attempt2-default-complete-wire-model.json"
            ),
            KotlinCapability::Enabled,
            SourceGroup::KotlinAndJava,
        ),
        (
            include_str!(
                "../test_data/evaluated_tree_inputs/attempt2-built-in-disabled-full-stdout-stderr.log"
            ),
            include_str!(
                "../test_data/evaluated_tree_inputs/attempt2-built-in-disabled-complete-wire-model.json"
            ),
            KotlinCapability::Disabled,
            SourceGroup::Java,
        ),
    ]
}

#[test]
fn actual_provider_unsigned_version_boundaries_remain_incompatible_with_supported_generated_models()
-> Result<()> {
    for (raw, _, kotlin, _) in actual_live_exporter_fixtures() {
        for component in ["major", "minor"] {
            let directory = tempfile::tempdir()?;
            let root = directory.path().canonicalize()?;
            fs::create_dir_all(root.join("app"))?;
            let raw = raw.replace(
                "/workspace/android-studio-artifacts/source-providers-smoke/project",
                root.to_str().context("Temporary root must be UTF-8")?,
            );
            let mut state = ModelState::default();
            let token = state.invalidate(Some(root.clone()));
            let original = EvaluatedTreeInputs::decode_sync(&raw, &root, None, &token)?;
            let mut value: Value = serde_json::from_str(
                original
                    .raw_record()
                    .strip_prefix("KODA_ANDROID_PROJECT_MODEL=")
                    .context("Actual model prefix")?,
            )?;
            let original_generated = value["generatedArtifacts"].clone();
            let original_kotlin = value["kotlinCapabilities"].clone();
            state.publish_evaluated(&token, original)?;
            let selected = VariantId {
                module: ":app".into(),
                variant: "demoDebug".into(),
            };
            state.select(Some(selected.clone()))?;
            let proven = prepare_live_module_plan(&state, &state.token(), selected.clone(), false)?;
            proven.ensure_current(&state)?;
            // Only the provider sidecar is fault-injected; immutable captured
            // Gradle output and its generated/Kotlin observations stay intact.
            value["modules"][0]["evaluatedProviders"]["value"]["modelProducer"][component] =
                json!(u32::MAX);
            assert_eq!(value["generatedArtifacts"], original_generated);
            assert_eq!(value["kotlinCapabilities"], original_kotlin);
            let replacement = state.invalidate(Some(root.clone()));
            let capture =
                EvaluatedTreeInputs::decode_sync(&record(&value), &root, None, &replacement)?;
            assert_eq!(capture.kotlin_capability(":app")?, kotlin);
            state.publish_evaluated(&replacement, capture)?;
            state.select(Some(selected.clone()))?;
            let module = state
                .model
                .as_ref()
                .context("Unsigned provider metadata retains the physical model")?
                .modules
                .iter()
                .find(|module| module.path == ":app")
                .context("Captured app module")?;
            let providers = match module.evaluated_providers.as_ref() {
                Some(android_tools::project_model::EvaluatedProviderMetadata::Available(
                    providers,
                )) => providers,
                _ => anyhow::bail!("Unsigned boundary must decode as available provider metadata"),
            };
            let observed = match component {
                "major" => providers.model_producer.major,
                "minor" => providers.model_producer.minor,
                _ => anyhow::bail!("Unexpected producer version component"),
            };
            assert_eq!(observed, u32::MAX);
            let failure = prepare_live_module_plan(&state, &state.token(), selected, false)
                .expect_err("A representable unsigned provider version cannot borrow the supported generated profile");
            assert_eq!(
                failure.reason,
                AdapterUnavailableReason::Provider(FactsUnavailableReason::Stale)
            );
            assert!(state.model.is_some());
            assert!(proven.ensure_current(&state).is_err());
        }
    }
    Ok(())
}

#[test]
fn actual_supported_export_with_independently_mismatched_provider_versions_is_unavailable()
-> Result<()> {
    for (raw, _, kotlin, _) in actual_live_exporter_fixtures() {
        for alteration in ["agp", "producer_major", "producer_minor", "schema"] {
            let directory = tempfile::tempdir()?;
            let root = directory.path().canonicalize()?;
            fs::create_dir_all(root.join("app"))?;
            let raw = raw.replace(
                "/workspace/android-studio-artifacts/source-providers-smoke/project",
                root.to_str().context("Temporary root must be UTF-8")?,
            );
            let mut state = ModelState::default();
            let token = state.invalidate(Some(root.clone()));
            let original = EvaluatedTreeInputs::decode_sync(&raw, &root, None, &token)?;
            let mut value: Value = serde_json::from_str(
                original
                    .raw_record()
                    .strip_prefix("KODA_ANDROID_PROJECT_MODEL=")
                    .context("Actual model prefix")?,
            )?;
            let original_generated = value["generatedArtifacts"].clone();
            let original_kotlin = value["kotlinCapabilities"].clone();
            state.publish_evaluated(&token, original)?;
            let selected = VariantId {
                module: ":app".into(),
                variant: "demoDebug".into(),
            };
            state.select(Some(selected.clone()))?;
            let proven = prepare_live_module_plan(&state, &state.token(), selected.clone(), false)?;
            proven.ensure_current(&state)?;
            // Alter only the independent provider sidecar of the actual export;
            // no fixture or claimed Gradle observation is rewritten.
            let providers = &mut value["modules"][0]["evaluatedProviders"]["value"];
            let expected = match alteration {
                "agp" => {
                    providers["agpVersion"] = json!("9.5.0");
                    FactsUnavailableReason::Stale
                }
                "producer_major" => {
                    providers["modelProducer"]["major"] = json!(24);
                    FactsUnavailableReason::Stale
                }
                "producer_minor" => {
                    providers["modelProducer"]["minor"] = json!(1);
                    FactsUnavailableReason::Stale
                }
                "schema" => {
                    providers["version"] = json!(2);
                    FactsUnavailableReason::UnsupportedSchema
                }
                _ => anyhow::bail!("Unexpected provider alteration"),
            };
            assert_eq!(value["generatedArtifacts"], original_generated);
            assert_eq!(value["kotlinCapabilities"], original_kotlin);
            let replacement = state.invalidate(Some(root.clone()));
            let capture =
                EvaluatedTreeInputs::decode_sync(&record(&value), &root, None, &replacement)?;
            assert_eq!(capture.kotlin_capability(":app")?, kotlin);
            state.publish_evaluated(&replacement, capture)?;
            state.select(Some(selected.clone()))?;
            let failure = prepare_live_module_plan(&state, &state.token(), selected, false)
                .expect_err("Unproven active-provider versions cannot borrow supported generated/Kotlin versions");
            assert_eq!(failure.reason, AdapterUnavailableReason::Provider(expected));
            assert!(
                state.model.is_some(),
                "Provider incompatibility retains the physical model"
            );
            assert!(proven.ensure_current(&state).is_err());
        }
    }
    Ok(())
}

#[test]
fn actual_exporter_live_plans_bind_effective_kotlin_imported_identity_and_exact_generated_roots()
-> Result<()> {
    for (raw, model, kotlin, shared_group) in actual_live_exporter_fixtures() {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        fs::create_dir_all(root.join("app"))?;
        let actual_prefix = "/workspace/android-studio-artifacts/source-providers-smoke/project";
        let root_text = root.to_str().context("Temporary root must be UTF-8")?;
        let raw = raw.replace(actual_prefix, root_text);
        let model: Value = serde_json::from_str(&model.replace(actual_prefix, root_text))?;
        let mut state = ModelState::default();
        let token = state.invalidate(Some(root.clone()));
        let capture = EvaluatedTreeInputs::decode_sync(&raw, &root, None, &token)?;
        let decoded: Value = serde_json::from_str(
            capture
                .raw_record()
                .strip_prefix("KODA_ANDROID_PROJECT_MODEL=")
                .context("Actual model prefix")?,
        )?;
        assert_eq!(
            decoded, model,
            "Full wire and copied complete model preserve the same actual observations"
        );
        assert_eq!(capture.kotlin_capability(":app")?, kotlin);
        assert_eq!(
            capture
                .import_facts()?
                .project(":app")?
                .build_tree_path
                .as_ref()
                .context("Actual build-tree identity observation")?
                .available()?,
            ":app"
        );
        let original_import_binding = capture.import_facts()?.binding().clone();
        state.publish_evaluated(&token, capture)?;
        state.select(Some(VariantId {
            module: ":app".into(),
            variant: "demoDebug".into(),
        }))?;
        let plan = prepare_live_module_plan(
            &state,
            &state.token(),
            VariantId {
                module: ":app".into(),
                variant: "demoDebug".into(),
            },
            false,
        )?;
        assert_eq!(
            plan.imported_identity().internal_name,
            "SourceProvidersSmoke:app"
        );
        assert_eq!(plan.imported_identity().external_project_id, ":app");
        assert_eq!(
            plan.imported_identity().external_project_path,
            root.join("app")
        );
        assert_eq!(plan.imported_identity().external_root_project_path, root);
        assert_eq!(plan.plan().binding().module, ":app");
        assert_eq!(plan.plan().binding().variant, "demoDebug");
        let shared = root.join("app/src/main/java");
        assert!(
            plan.plan()
                .source_roots()
                .iter()
                .any(|source| source.path == shared && source.group == shared_group)
        );
        assert!(
            !plan
                .plan()
                .source_roots()
                .iter()
                .any(|source| source.path == shared && source.group != shared_group),
            "Actual effective capability must choose the source group for the surviving shared root"
        );
        let generated_sources = plan
            .plan()
            .source_roots()
            .iter()
            .filter(|source| source.group == SourceGroup::GeneratedJava)
            .map(|source| source.path.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            generated_sources,
            [root.join("app/build/generated/ap_generated_sources/demoDebug/out")]
        );
        let generated_assets = plan
            .plan()
            .source_roots()
            .iter()
            .filter(|source| source.group == SourceGroup::GeneratedAssets)
            .map(|source| source.path.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            generated_assets,
            [root.join("app/build/generated/assets/createAssets")]
        );
        assert!(
            !plan
                .plan()
                .source_roots()
                .iter()
                .any(|source| source.group == SourceGroup::GeneratedResources),
            "Actual available empty generated-resource getter remains empty"
        );
        assert!(
            plan.plan()
                .required_presence_paths()
                .contains(&root.join("app/build/generated/assets/createAssets"))
        );
        assert_eq!(
            state
                .evaluated_inputs()
                .context("Current capture")?
                .import_facts()?
                .binding(),
            &original_import_binding,
            "Plan preparation cannot restamp the original import snapshot as a new selection"
        );
        assert_eq!(plan.model_token(), &state.token());
        plan.ensure_current(&state)?;
    }
    Ok(())
}

#[test]
fn actual_live_plan_variant_changes_and_sync_replacement_reject_original_owner() -> Result<()> {
    for (raw, _, _, _) in actual_live_exporter_fixtures() {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        fs::create_dir_all(root.join("app"))?;
        let raw = raw.replace(
            "/workspace/android-studio-artifacts/source-providers-smoke/project",
            root.to_str().context("Temporary root must be UTF-8")?,
        );
        let mut state = ModelState::default();
        let token = state.invalidate(Some(root.clone()));
        state.publish_evaluated(
            &token,
            EvaluatedTreeInputs::decode_sync(&raw, &root, None, &token)?,
        )?;
        let variant = |name: &str| VariantId {
            module: ":app".into(),
            variant: name.into(),
        };
        state.select(Some(variant("demoDebug")))?;
        let old = prepare_live_module_plan(&state, &state.token(), variant("demoDebug"), true)?;
        state.select(Some(variant("demoRelease")))?;
        assert!(old.ensure_current(&state).is_err());
        let release =
            prepare_live_module_plan(&state, &state.token(), variant("demoRelease"), true)?;
        assert_eq!(release.plan().binding().variant, "demoRelease");
        assert_eq!(
            release
                .plan()
                .source_roots()
                .iter()
                .filter(|source| source.group == SourceGroup::GeneratedJava)
                .map(|source| source.path.clone())
                .collect::<Vec<_>>(),
            [root.join("app/build/generated/ap_generated_sources/demoRelease/out")]
        );
        state.select(Some(variant("demoDebug")))?;
        assert!(
            old.ensure_current(&state).is_err(),
            "Actual variant A-B-A retains original-token staleness"
        );
        let replacement = state.invalidate(Some(root.clone()));
        state.publish_evaluated(
            &replacement,
            EvaluatedTreeInputs::decode_sync(&raw, &root, None, &replacement)?,
        )?;
        assert!(old.ensure_current(&state).is_err());
        assert!(release.ensure_current(&state).is_err());
    }
    Ok(())
}

#[test]
fn actual_supported_wire_with_inaccessible_effective_getter_retains_capability_failure()
-> Result<()> {
    let (raw, _, _, _) = actual_live_exporter_fixtures()
        .into_iter()
        .next()
        .context("Actual default exporter fixture")?;
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    fs::create_dir_all(root.join("app"))?;
    let raw = raw.replace(
        "/workspace/android-studio-artifacts/source-providers-smoke/project",
        root.to_str().context("Temporary root must be UTF-8")?,
    );
    let mut state = ModelState::default();
    let token = state.invalidate(Some(root.clone()));
    let original = EvaluatedTreeInputs::decode_sync(&raw, &root, None, &token)?;
    let mut value: Value = serde_json::from_str(
        original
            .raw_record()
            .strip_prefix("KODA_ANDROID_PROJECT_MODEL=")
            .context("Actual model prefix")?,
    )?;
    failed_observation(
        &mut value,
        "builtInKotlin",
        "AGP ProjectServices getter is inaccessible",
    );
    state.publish_evaluated(
        &token,
        EvaluatedTreeInputs::decode_sync(&record(&value), &root, None, &token)?,
    )?;
    state.select(Some(VariantId {
        module: ":app".into(),
        variant: "demoDebug".into(),
    }))?;
    let failure = prepare_live_module_plan(
        &state,
        &state.token(),
        VariantId {
            module: ":app".into(),
            variant: "demoDebug".into(),
        },
        false,
    )
    .expect_err("Fault injected into actual successful wire cannot borrow the default flag");
    assert_eq!(
        failure.reason,
        AdapterUnavailableReason::Provider(FactsUnavailableReason::Capability)
    );
    assert!(failure.detail.contains("getter is inaccessible"));
    assert!(state.model.is_some());
    Ok(())
}

#[test]
fn nullable_sdk_version_failures_retain_core_model_and_original_capability_detail() -> Result<()> {
    for getter in ["getPluginVersion", "getVersion"] {
        for failure_class in [
            "java.lang.IllegalStateException",
            "java.lang.NoClassDefFoundError",
        ] {
            let mut fixture = live_plan_fixture()?;
            let detail = format!("{failure_class}: Fixture {getter} failure");
            fixture.value["kotlinCapabilities"]["modules"][0]["agpVersion"] = Value::Null;
            failed_observation(&mut fixture.value, "sdkPluginVersion", &detail);
            if getter == "getPluginVersion" {
                let sdk =
                    &mut fixture.value["kotlinCapabilities"]["modules"][0]["sdkPluginVersion"];
                sdk["getter"] = json!(
                    "com.android.build.api.variant.AndroidComponentsExtension.getPluginVersion()"
                );
                sdk["result"]["value"]["capability"] = sdk["getter"].clone();
            }
            failed_observation(&mut fixture.value, "builtInKotlin", &detail);
            let physical_model = fixture.value["modules"].clone();
            let mut state = ModelState::default();
            let token = state.invalidate(Some(fixture.root().to_path_buf()));
            let capture = EvaluatedTreeInputs::decode_sync(
                &record(&fixture.value),
                fixture.root(),
                None,
                &token,
            )?;
            let original_model = capture.model().clone();
            let failure = capture
                .kotlin_capability(":app")
                .expect_err("Failed SDK observation");
            assert_eq!(failure.reason, FactsUnavailableReason::Capability);
            assert!(failure.detail.contains(&detail));
            assert_eq!(capture.token(), &token);
            assert_eq!(capture.raw_record(), record(&fixture.value));
            assert_eq!(fixture.value["modules"], physical_model);
            state.publish_evaluated(&token, capture)?;
            assert!(Arc::ptr_eq(
                state
                    .model
                    .as_ref()
                    .context("Core physical model retained")?,
                &original_model,
            ));
            state.select(Some(id("debug")))?;
            let failure = prepare_live_module_plan(&state, &state.token(), id("debug"), false)
                .expect_err("Unavailable version cannot supply an Android logical plan");
            assert_eq!(
                failure.reason,
                AdapterUnavailableReason::Provider(FactsUnavailableReason::Capability)
            );
            assert!(failure.detail.contains(&detail));
            assert!(state.model.is_some());
        }
    }
    Ok(())
}

#[test]
fn null_or_missing_agp_version_requires_an_explicit_valid_failed_sdk_getter() -> Result<()> {
    for alteration in [
        "available",
        "missing_sdk",
        "missing_agp",
        "numeric_agp",
        "wrong_getter",
        "wrong_capability",
        "available_acquisition",
    ] {
        let mut fixture = live_plan_fixture()?;
        fixture.value["kotlinCapabilities"]["modules"][0]["agpVersion"] = Value::Null;
        failed_observation(
            &mut fixture.value,
            "sdkPluginVersion",
            "Original SDK failure",
        );
        let module = fixture.value["kotlinCapabilities"]["modules"][0]
            .as_object_mut()
            .context("Kotlin observation")?;
        match alteration {
            "available" => {
                module
                    .get_mut("sdkPluginVersion")
                    .context("SDK observation")?["result"] = available(json!({
                    "major":9,"minor":4,"micro":0,"preview":0,"previewType":null,"version":"9.4.0"
                }));
            }
            "missing_sdk" => {
                module.remove("sdkPluginVersion");
            }
            "missing_agp" => {
                module.remove("agpVersion");
            }
            "numeric_agp" => {
                module.insert("agpVersion".into(), json!(94));
            }
            "wrong_getter" => {
                module
                    .get_mut("sdkPluginVersion")
                    .context("SDK observation")?["getter"] = json!("guessed version");
            }
            "wrong_capability" => {
                module
                    .get_mut("sdkPluginVersion")
                    .context("SDK observation")?["result"]["value"]["capability"] =
                    json!("unrelated getter");
            }
            "available_acquisition" => {
                module.insert("agpVersion".into(), json!("9.4.0"));
                let sdk = module
                    .get_mut("sdkPluginVersion")
                    .context("SDK observation")?;
                sdk["getter"] = json!(
                    "com.android.build.api.variant.AndroidComponentsExtension.getPluginVersion()"
                );
                sdk["result"] = available(json!({
                    "major":9,"minor":4,"micro":0,"preview":0,"previewType":null,"version":"9.4.0"
                }));
            }
            _ => anyhow::bail!("Unknown SDK version alteration"),
        }
        let mut state = ModelState::default();
        fixture.publish(&mut state)?;
        let failure = state
            .evaluated_inputs()
            .context("Core capture retained")?
            .kotlin_capability(":app")
            .expect_err("Invalid missing version evidence");
        assert_eq!(
            failure.reason,
            FactsUnavailableReason::Malformed,
            "{alteration}"
        );
        assert!(state.model.is_some());
    }
    Ok(())
}
