use android_tools::{
    generated_artifacts::{CapturedField, GetterUnavailable},
    import_facts::{
        GetterObservation, ImportFactsBinding, ImportFactsSnapshot, RawImportProject,
        parse_import_facts,
    },
    project_model::{ModuleKind, ProjectModel, VariantId, parse_model},
    project_tree_facts::FactsUnavailableReason,
};
use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use std::{fs, path::Path};

struct Fixture {
    _directory: tempfile::TempDir,
    value: Value,
    model: ProjectModel,
}

fn replace_root(value: &mut Value, root: &str) {
    match value {
        Value::String(value) => *value = value.replace("$ROOT", root),
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| replace_root(value, root)),
        Value::Object(values) => values
            .values_mut()
            .for_each(|value| replace_root(value, root)),
        _ => {}
    }
}

fn wire(value: &Value) -> Result<String> {
    Ok(format!(
        "KODA_ANDROID_PROJECT_MODEL={}",
        serde_json::to_string(value)?
    ))
}

fn binding() -> ImportFactsBinding {
    ImportFactsBinding {
        model_revision: 7,
        selection_revision: 3,
        selected_variants: vec![
            VariantId {
                module: ":android".into(),
                variant: "debug".into(),
            },
            VariantId {
                module: ":nested:library".into(),
                variant: "jvm".into(),
            },
        ],
    }
}

fn fixture() -> Result<Fixture> {
    let directory = tempfile::Builder::new()
        .prefix("different-folder-")
        .tempdir()?;
    let root = directory.path().canonicalize()?;
    for name in ["android", "strange-parent", "shared-directory"] {
        fs::create_dir(root.join(name))?;
    }
    let mut value: Value =
        serde_json::from_str(include_str!("../test_data/import_facts/wire-template.json"))?;
    replace_root(
        &mut value,
        root.to_str().context("Fixture root is not UTF-8")?,
    );
    let model = parse_model(&wire(&value)?, &root)?;
    Ok(Fixture {
        _directory: directory,
        value,
        model,
    })
}

fn decode(fixture: &Fixture, value: &Value) -> Result<ImportFactsSnapshot> {
    Ok(parse_import_facts(
        &wire(value)?,
        &fixture.model,
        binding(),
    )?)
}

fn assert_failure(fixture: &Fixture, value: &Value, reason: FactsUnavailableReason) -> Result<()> {
    let error = parse_import_facts(&wire(value)?, &fixture.model, binding())
        .expect_err("Malformed/unavailable capture must fail");
    assert_eq!(error.reason, reason, "{error}");
    Ok(())
}

fn catalogue(value: &mut Value) -> Result<&mut Vec<Value>> {
    value["importFacts"]["projectCatalogue"]["result"]["value"]
        .as_array_mut()
        .context("Expected fixture project catalogue")
}

#[test]
fn root_holders_and_nested_directories_are_independent_of_compiled_modules() -> Result<()> {
    let fixture = fixture()?;
    let snapshot = decode(&fixture, &fixture.value)?;
    assert_eq!(snapshot.projects()?.len(), 4);
    assert_eq!(fixture.model.modules.len(), 2);
    let root = snapshot.project(":")?;
    assert_eq!(
        root.project_name
            .as_ref()
            .context("Root name")?
            .available()?,
        "declared-root"
    );
    assert_ne!(
        fixture
            .model
            .root
            .file_name()
            .and_then(|name| name.to_str()),
        Some("declared-root")
    );
    let nested = snapshot.project(":nested:library")?;
    assert_eq!(
        nested
            .project_directory
            .as_ref()
            .context("Project directory")?
            .available()?,
        &fixture.model.root.join("shared-directory")
    );
    assert_eq!(
        nested
            .parent_project_path
            .as_ref()
            .context("Parent")?
            .available()?
            .as_deref(),
        Some(":nested")
    );
    assert_eq!(snapshot.gradle_version(), "9.6.1");
    snapshot.ensure_current(&fixture.model, &binding())?;
    Ok(())
}

#[test]
fn missing_failed_null_and_empty_idea_names_remain_distinct() -> Result<()> {
    let fixture = fixture()?;
    let null = decode(&fixture, &fixture.value)?;
    assert_eq!(
        null.project(":android")?
            .idea_module_name
            .as_ref()
            .context("Nullable name")?
            .available()?,
        &None
    );
    let mut missing = fixture.value.clone();
    catalogue(&mut missing)?
        .get_mut(3)
        .context("Android project")?
        .as_object_mut()
        .context("Project object")?
        .remove("ideaModuleName");
    assert!(
        decode(&fixture, &missing)?
            .project(":android")?
            .idea_module_name
            .is_none()
    );
    let mut unavailable = fixture.value.clone();
    let getter = unavailable["importFacts"]["projectCatalogue"]["result"]["value"][3]["ideaModuleName"]["getter"].clone();
    unavailable["importFacts"]["projectCatalogue"]["result"]["value"][3]["ideaModuleName"]["result"] = json!({"status":"unavailable","value":{"capability":getter,"detail":"Getter threw\nwith original details"}});
    let unavailable = decode(&fixture, &unavailable)?;
    let observation = unavailable
        .project(":android")?
        .idea_module_name
        .as_ref()
        .context("Unavailable name")?;
    assert_eq!(
        observation
            .available()
            .expect_err("Getter unavailable")
            .reason,
        FactsUnavailableReason::Capability
    );
    assert!(
        matches!(&observation.result, CapturedField::Unavailable(failure) if failure.detail == "Getter threw\nwith original details")
    );
    let mut empty = fixture.value.clone();
    empty["importFacts"]["projectCatalogue"]["result"]["value"][3]["ideaPluginPresent"]["result"]
        ["value"] = json!(true);
    empty["importFacts"]["projectCatalogue"]["result"]["value"][3]["ideaModuleName"]["result"]["value"] =
        json!("");
    assert_eq!(
        decode(&fixture, &empty)?
            .project(":android")?
            .idea_module_name
            .as_ref()
            .context("Empty name")?
            .available()?
            .as_deref(),
        Some("")
    );
    Ok(())
}

#[test]
fn successful_nullable_getter_requires_an_explicit_value_member() -> Result<()> {
    let fixture = fixture()?;
    for field in [
        "parentProjectPath",
        "referenceIdentityPath",
        "ideaModuleName",
    ] {
        let mut value = fixture.value.clone();
        value["importFacts"]["projectCatalogue"]["result"]["value"][0][field]["result"]
            .as_object_mut()
            .context("Getter result")?
            .remove("value");
        assert_failure(&fixture, &value, FactsUnavailableReason::Malformed)?;
    }
    Ok(())
}

#[test]
fn optional_getter_failure_preserves_other_observed_identity() -> Result<()> {
    let fixture = fixture()?;
    let mut value = fixture.value.clone();
    let getter =
        value["importFacts"]["projectCatalogue"]["result"]["value"][3]["buildTreePath"]["getter"]
            .clone();
    value["importFacts"]["projectCatalogue"]["result"]["value"][3]["buildTreePath"]["result"] = json!({"status":"unavailable","value":{"capability":getter,"detail":"MissingMethodException: getBuildTreePath"}});
    let snapshot = decode(&fixture, &value)?;
    let project = snapshot.project(":android")?;
    assert_eq!(
        project
            .build_tree_path
            .as_ref()
            .context("Public path")?
            .available()
            .expect_err("Public getter unavailable")
            .reason,
        FactsUnavailableReason::Capability
    );
    assert_eq!(
        project
            .reference_identity_path
            .as_ref()
            .context("Reference path")?
            .available()?
            .as_deref(),
        Some(":android")
    );
    assert_eq!(
        project.project_name.as_ref().context("Name")?.available()?,
        "android"
    );
    assert_eq!(
        parse_model(&wire(&value)?, &fixture.model.root)?
            .modules
            .len(),
        2
    );
    Ok(())
}

#[test]
fn public_and_reference_identity_paths_are_never_substituted() -> Result<()> {
    let fixture = fixture()?;
    let mut value = fixture.value.clone();
    value["importFacts"]["projectCatalogue"]["result"]["value"][3]["buildTreePath"]["result"]["value"] =
        json!(":public:android");
    value["importFacts"]["projectCatalogue"]["result"]["value"][3]["referenceIdentityPath"]["result"]
        ["value"] = json!(":reference:android");
    let snapshot = decode(&fixture, &value)?;
    let project = snapshot.project(":android")?;
    assert_eq!(
        project
            .build_tree_path
            .as_ref()
            .context("Public path")?
            .available()?,
        ":public:android"
    );
    assert_eq!(
        project
            .reference_identity_path
            .as_ref()
            .context("Reference path")?
            .available()?
            .as_deref(),
        Some(":reference:android")
    );
    Ok(())
}

#[test]
fn colliding_project_and_idea_names_do_not_trigger_transport_renaming() -> Result<()> {
    let fixture = fixture()?;
    let mut value = fixture.value.clone();
    for index in [2, 3] {
        let project = catalogue(&mut value)?
            .get_mut(index)
            .context("Compiled project")?;
        project["projectName"]["result"]["value"] = json!("same-name");
        project["ideaPluginPresent"]["result"]["value"] = json!(true);
        project["ideaModuleName"]["result"]["value"] = json!("same-idea-name");
    }
    let snapshot = decode(&fixture, &value)?;
    for path in [":android", ":nested:library"] {
        let project = snapshot.project(path)?;
        assert_eq!(
            project.project_name.as_ref().context("Name")?.available()?,
            "same-name"
        );
        assert_eq!(
            project
                .idea_module_name
                .as_ref()
                .context("Idea name")?
                .available()?
                .as_deref(),
            Some("same-idea-name")
        );
    }
    Ok(())
}

#[test]
fn catalogue_order_is_preserved_and_duplicate_identities_are_rejected() -> Result<()> {
    let fixture = fixture()?;
    let mut value = fixture.value.clone();
    catalogue(&mut value)?.reverse();
    let snapshot = decode(&fixture, &value)?;
    let paths = snapshot
        .projects()?
        .iter()
        .map(|project| {
            project
                .project_path
                .as_ref()
                .context("Path")?
                .available()
                .map(String::as_str)
                .map_err(Into::into)
        })
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(paths, [":android", ":nested:library", ":nested", ":"]);
    let duplicate = catalogue(&mut value)?
        .first()
        .context("First project")?
        .clone();
    catalogue(&mut value)?.push(duplicate);
    assert_failure(&fixture, &value, FactsUnavailableReason::Malformed)?;
    Ok(())
}

#[test]
fn missing_required_identity_and_failed_getter_have_typed_reasons() -> Result<()> {
    let fixture = fixture()?;
    let mut missing = fixture.value.clone();
    catalogue(&mut missing)?
        .get_mut(3)
        .context("Android project")?
        .as_object_mut()
        .context("Project object")?
        .remove("projectPath");
    assert_failure(&fixture, &missing, FactsUnavailableReason::MissingMetadata)?;
    let mut failed = fixture.value.clone();
    let getter = failed["importFacts"]["projectCatalogue"]["result"]["value"][3]["projectDirectory"]["getter"].clone();
    failed["importFacts"]["projectCatalogue"]["result"]["value"][3]["projectDirectory"]["result"] = json!({"status":"unavailable","value":{"capability":getter,"detail":"IOException: original failure"}});
    assert_failure(&fixture, &failed, FactsUnavailableReason::Capability)?;
    assert_eq!(
        parse_model(&wire(&failed)?, &fixture.model.root)?
            .modules
            .len(),
        2
    );
    Ok(())
}

#[test]
fn missing_sidecar_null_sidecar_and_unsupported_schema_are_distinct() -> Result<()> {
    let fixture = fixture()?;
    let mut missing = fixture.value.clone();
    missing
        .as_object_mut()
        .context("Record")?
        .remove("importFacts");
    assert_failure(&fixture, &missing, FactsUnavailableReason::MissingMetadata)?;
    let mut null = fixture.value.clone();
    null["importFacts"] = Value::Null;
    assert_failure(&fixture, &null, FactsUnavailableReason::Malformed)?;
    let mut unsupported = fixture.value.clone();
    unsupported["importFacts"]["schema"] = json!(2);
    assert_failure(
        &fixture,
        &unsupported,
        FactsUnavailableReason::UnsupportedSchema,
    )?;
    for schema in [json!(null), json!("1"), json!(-1)] {
        let mut value = fixture.value.clone();
        value["importFacts"]["schema"] = schema;
        assert_failure(&fixture, &value, FactsUnavailableReason::Malformed)?;
    }
    Ok(())
}

#[test]
fn sidecar_binding_rejects_changed_root_module_directory_kind_and_variants() -> Result<()> {
    let fixture = fixture()?;
    for (pointer, replacement) in [
        ("/importFacts/root", json!("/different-root")),
        ("/importFacts/modules/0/module", json!(":different")),
        (
            "/importFacts/modules/0/directory",
            json!("/different-directory"),
        ),
        ("/importFacts/modules/0/kind", json!("application")),
        (
            "/importFacts/modules/0/variants",
            json!(["release", "debug"]),
        ),
    ] {
        let mut value = fixture.value.clone();
        *value.pointer_mut(pointer).context("Binding field")? = replacement;
        assert_failure(&fixture, &value, FactsUnavailableReason::Stale)?;
    }
    let mut reordered = fixture.value.clone();
    reordered["importFacts"]["modules"]
        .as_array_mut()
        .context("Modules")?
        .reverse();
    assert_failure(&fixture, &reordered, FactsUnavailableReason::Stale)?;
    Ok(())
}

#[test]
fn snapshot_rejects_changed_model_selection_revision_and_selected_variant() -> Result<()> {
    let fixture = fixture()?;
    let snapshot = decode(&fixture, &fixture.value)?;
    let mut model = fixture.model.clone();
    model.modules.first_mut().context("Module")?.kind = ModuleKind::Application;
    assert_eq!(
        snapshot
            .ensure_current(&model, &binding())
            .expect_err("Changed model")
            .reason,
        FactsUnavailableReason::Stale
    );
    for changed in [
        ImportFactsBinding {
            model_revision: 8,
            ..binding()
        },
        ImportFactsBinding {
            selection_revision: 4,
            ..binding()
        },
        ImportFactsBinding {
            selected_variants: vec![VariantId {
                module: ":android".into(),
                variant: "release".into(),
            }],
            ..binding()
        },
    ] {
        assert_eq!(
            snapshot
                .ensure_current(&fixture.model, &changed)
                .expect_err("Changed binding")
                .reason,
            FactsUnavailableReason::Stale
        );
    }
    Ok(())
}

#[test]
fn unknown_or_duplicate_selected_variants_are_explicit_errors() -> Result<()> {
    let fixture = fixture()?;
    for (selected_variants, expected) in [
        (
            vec![VariantId {
                module: ":missing".into(),
                variant: "debug".into(),
            }],
            FactsUnavailableReason::MissingMetadata,
        ),
        (
            vec![VariantId {
                module: ":android".into(),
                variant: "missing".into(),
            }],
            FactsUnavailableReason::MissingVariant,
        ),
        (
            vec![
                VariantId {
                    module: ":android".into(),
                    variant: "debug".into()
                };
                2
            ],
            FactsUnavailableReason::Malformed,
        ),
    ] {
        let error = parse_import_facts(
            &wire(&fixture.value)?,
            &fixture.model,
            ImportFactsBinding {
                selected_variants,
                ..binding()
            },
        )
        .expect_err("Invalid selection");
        assert_eq!(error.reason, expected);
    }
    parse_import_facts(
        &wire(&fixture.value)?,
        &fixture.model,
        ImportFactsBinding {
            selected_variants: Vec::new(),
            ..binding()
        },
    )?;
    Ok(())
}

#[test]
fn raw_catalogue_binding_requires_root_and_each_basic_module() -> Result<()> {
    let fixture = fixture()?;
    for index in [0, 3] {
        let mut value = fixture.value.clone();
        catalogue(&mut value)?.remove(index);
        assert_failure(&fixture, &value, FactsUnavailableReason::MissingMetadata)?;
    }
    for field in ["projectDirectory", "rootDirectory"] {
        let mut value = fixture.value.clone();
        value["importFacts"]["projectCatalogue"]["result"]["value"][3][field]["result"]["value"] =
            json!("/different-directory");
        assert_failure(&fixture, &value, FactsUnavailableReason::Stale)?;
    }
    Ok(())
}

#[test]
fn raw_available_parent_root_and_plugin_observations_cannot_contradict() -> Result<()> {
    let fixture = fixture()?;
    for (field, replacement) in [
        ("parentProjectPath", json!(":wrong-parent")),
        ("rootName", json!("wrong-root-name")),
        ("ideaModuleName", json!("name-with-no-plugin")),
    ] {
        let mut value = fixture.value.clone();
        value["importFacts"]["projectCatalogue"]["result"]["value"][3][field]["result"]["value"] =
            replacement;
        assert_failure(&fixture, &value, FactsUnavailableReason::Malformed)?;
    }
    Ok(())
}

#[test]
fn provenance_and_unavailable_failure_metadata_are_validated() -> Result<()> {
    let fixture = fixture()?;
    let mut value = fixture.value.clone();
    value["importFacts"]["projectCatalogue"]["result"]["value"][3]["projectName"]["getter"] =
        json!("guessedFolderName");
    assert_failure(&fixture, &value, FactsUnavailableReason::Malformed)?;
    for failure in [
        json!({"capability":"wrong-getter","detail":"original"}),
        json!({"capability":"org.gradle.api.Project.getName()","detail":""}),
    ] {
        let mut value = fixture.value.clone();
        value["importFacts"]["projectCatalogue"]["result"]["value"][3]["projectName"]["result"] =
            json!({"status":"unavailable","value":failure});
        assert_failure(&fixture, &value, FactsUnavailableReason::Malformed)?;
    }
    let mut value = fixture.value.clone();
    value["importFacts"]["projectCatalogue"]["result"]["value"][3]["projectName"]["result"] =
        json!({"status":"notApplicable","value":"not implemented"});
    assert_failure(&fixture, &value, FactsUnavailableReason::Malformed)?;
    Ok(())
}

#[test]
fn positional_arrays_are_rejected_at_every_object_boundary() -> Result<()> {
    let fixture = fixture()?;
    for pointer in [
        "/importFacts",
        "/importFacts/buildIdentity",
        "/importFacts/modules/0",
        "/importFacts/projectCatalogue",
        "/importFacts/projectCatalogue/result",
        "/importFacts/projectCatalogue/result/value/0",
        "/importFacts/projectCatalogue/result/value/0/projectName",
        "/importFacts/projectCatalogue/result/value/0/projectName/result",
    ] {
        let mut value = fixture.value.clone();
        let fields = value
            .pointer(pointer)
            .context("Object boundary")?
            .as_object()
            .context("Object")?
            .values()
            .cloned()
            .collect::<Vec<_>>();
        *value.pointer_mut(pointer).context("Boundary")? = Value::Array(fields);
        assert_failure(&fixture, &value, FactsUnavailableReason::Malformed)?;
    }
    let encoded = wire(&fixture.value)?;
    let array_record = format!(
        "KODA_ANDROID_PROJECT_MODEL=[{}]",
        encoded
            .strip_prefix("KODA_ANDROID_PROJECT_MODEL=")
            .context("Prefix")?
    );
    assert_eq!(
        parse_import_facts(&array_record, &fixture.model, binding())
            .expect_err("Array root")
            .reason,
        FactsUnavailableReason::Malformed
    );
    Ok(())
}

#[test]
fn bare_null_observations_and_nonnullable_null_payloads_are_malformed() -> Result<()> {
    let fixture = fixture()?;
    for pointer in [
        "/importFacts/projectCatalogue",
        "/importFacts/buildIdentity/rootName",
        "/importFacts/projectCatalogue/result/value/0/ideaModuleName",
        "/importFacts/projectCatalogue/result/value/0/ideaPluginPresent/result/value",
        "/importFacts/projectCatalogue/result/value/0/projectName/result/value",
    ] {
        let mut value = fixture.value.clone();
        *value.pointer_mut(pointer).context("Boundary")? = Value::Null;
        assert_failure(&fixture, &value, FactsUnavailableReason::Malformed)?;
    }
    Ok(())
}

#[test]
fn unknown_wire_fields_and_statuses_are_rejected() -> Result<()> {
    let fixture = fixture()?;
    for pointer in [
        "/importFacts",
        "/importFacts/buildIdentity",
        "/importFacts/modules/0",
        "/importFacts/projectCatalogue",
        "/importFacts/projectCatalogue/result/value/0",
        "/importFacts/projectCatalogue/result/value/0/projectName",
    ] {
        let mut value = fixture.value.clone();
        value
            .pointer_mut(pointer)
            .context("Object")?
            .as_object_mut()
            .context("Object")?
            .insert("unexpected".into(), json!(true));
        assert_failure(&fixture, &value, FactsUnavailableReason::Malformed)?;
    }
    let mut value = fixture.value.clone();
    value["importFacts"]["projectCatalogue"]["result"]["value"][0]["projectName"]["result"]["status"] =
        json!("missing");
    assert_failure(&fixture, &value, FactsUnavailableReason::Malformed)?;
    Ok(())
}

#[test]
fn catalogue_failure_is_capability_without_losing_basic_model() -> Result<()> {
    let fixture = fixture()?;
    let mut value = fixture.value.clone();
    value["importFacts"]["projectCatalogue"]["result"] = json!({"status":"unavailable","value":{"capability":"org.gradle.api.Project.getAllprojects()","detail":"Original collection failure"}});
    assert_failure(&fixture, &value, FactsUnavailableReason::Capability)?;
    assert_eq!(
        parse_model(&wire(&value)?, &fixture.model.root)?
            .modules
            .len(),
        2
    );
    Ok(())
}

#[test]
fn raw_holder_directory_can_be_outside_the_build_without_claiming_trust() -> Result<()> {
    let fixture = fixture()?;
    let mut value = fixture.value.clone();
    value["importFacts"]["projectCatalogue"]["result"]["value"][1]["projectDirectory"]["result"]
        ["value"] = json!("/external-holder");
    let snapshot = decode(&fixture, &value)?;
    assert_eq!(
        snapshot
            .project(":nested")?
            .project_directory
            .as_ref()
            .context("Directory")?
            .available()?,
        Path::new("/external-holder")
    );
    Ok(())
}

#[test]
fn malformed_directories_and_project_paths_are_typed_errors() -> Result<()> {
    let fixture = fixture()?;
    for directory in [
        "relative",
        "/absolute/../parent",
        "/absolute//duplicate",
        "/absolute/./current",
        "/control\npath",
    ] {
        let mut value = fixture.value.clone();
        value["importFacts"]["projectCatalogue"]["result"]["value"][1]["projectDirectory"]["result"]
            ["value"] = json!(directory);
        assert_failure(&fixture, &value, FactsUnavailableReason::Malformed)?;
    }
    for path in [
        "",
        "relative",
        ":nested:",
        ":nested::child",
        ":control\nname",
    ] {
        let mut value = fixture.value.clone();
        value["importFacts"]["projectCatalogue"]["result"]["value"][1]["projectPath"]["result"]["value"] =
            json!(path);
        assert_failure(&fixture, &value, FactsUnavailableReason::Malformed)?;
    }
    Ok(())
}

#[test]
fn absent_observation_round_trip_does_not_become_bare_null() -> Result<()> {
    let fixture = fixture()?;
    let mut value = fixture.value.clone();
    let project = catalogue(&mut value)?.get_mut(3).context("Project")?;
    project
        .as_object_mut()
        .context("Project")?
        .remove("ideaModuleName");
    let decoded: RawImportProject = serde_json::from_value(project.clone())?;
    let encoded = serde_json::to_value(&decoded)?;
    assert!(encoded.get("ideaModuleName").is_none());
    assert_eq!(
        serde_json::from_value::<RawImportProject>(encoded)?,
        decoded
    );
    let observed: GetterObservation<Option<String>> = GetterObservation {
        getter: "org.gradle.plugins.ide.idea.IdeaPlugin.getModel()?.getModule()?.getName()".into(),
        result: CapturedField::Available(None),
    };
    let encoded = serde_json::to_value(&observed)?;
    assert_eq!(encoded["result"]["value"], Value::Null);
    assert_eq!(
        serde_json::from_value::<GetterObservation<Option<String>>>(encoded)?,
        observed
    );
    Ok(())
}

#[test]
fn unavailable_getter_preserves_full_failure_in_round_trip() -> Result<()> {
    let observed: GetterObservation<String> = GetterObservation {
        getter: "org.gradle.api.Project.getName()".into(),
        result: CapturedField::Unavailable(GetterUnavailable {
            capability: "org.gradle.api.Project.getName()".into(),
            detail: "Exception: first line\nsecond line".into(),
        }),
    };
    assert_eq!(
        serde_json::from_value::<GetterObservation<String>>(serde_json::to_value(&observed)?)?,
        observed
    );
    Ok(())
}

#[test]
fn empty_catalogue_is_observed_then_rejected_for_missing_root_identity() -> Result<()> {
    let fixture = fixture()?;
    let mut value = fixture.value.clone();
    catalogue(&mut value)?.clear();
    assert_failure(&fixture, &value, FactsUnavailableReason::MissingMetadata)?;
    Ok(())
}

#[test]
fn single_record_json_and_existing_size_boundary_are_enforced() -> Result<()> {
    let fixture = fixture()?;
    let encoded = wire(&fixture.value)?;
    assert_eq!(
        parse_import_facts("ordinary output", &fixture.model, binding())
            .expect_err("No model")
            .reason,
        FactsUnavailableReason::MissingMetadata
    );
    assert_eq!(
        parse_import_facts(
            "KODA_ANDROID_PROJECT_MODEL={invalid}",
            &fixture.model,
            binding()
        )
        .expect_err("Malformed JSON")
        .reason,
        FactsUnavailableReason::Malformed
    );
    assert_eq!(
        parse_import_facts(&format!("{encoded}\n{encoded}"), &fixture.model, binding())
            .expect_err("Multiple models")
            .reason,
        FactsUnavailableReason::Malformed
    );
    let mut at_limit = encoded;
    let record_bytes = at_limit
        .strip_prefix("KODA_ANDROID_PROJECT_MODEL=")
        .context("Prefix")?
        .len();
    at_limit.extend(std::iter::repeat_n(' ', 16 * 1024 * 1024 - record_bytes));
    parse_import_facts(&at_limit, &fixture.model, binding())?;
    at_limit.push(' ');
    assert_eq!(
        parse_import_facts(&at_limit, &fixture.model, binding())
            .expect_err("Oversized model")
            .reason,
        FactsUnavailableReason::UnsupportedShape
    );
    Ok(())
}

#[test]
fn basic_projection_is_unchanged_by_unrelated_or_missing_sidecars() -> Result<()> {
    let fixture = fixture()?;
    let before = serde_json::to_value(parse_model(&wire(&fixture.value)?, &fixture.model.root)?)?;
    let mut value = fixture.value.clone();
    value
        .as_object_mut()
        .context("Record")?
        .remove("importFacts");
    value["unrelated"] = json!({"empty":[],"nullable":null});
    let after = serde_json::to_value(parse_model(&wire(&value)?, &fixture.model.root)?)?;
    assert_eq!(before, after);
    Ok(())
}

#[test]
fn repeated_root_build_getters_cannot_disagree() -> Result<()> {
    let fixture = fixture()?;
    for (field, replacement) in [
        ("projectName", json!("different-root")),
        ("buildTreePath", json!(":different-root")),
        ("referenceIdentityPath", json!(":different-root")),
    ] {
        let mut value = fixture.value.clone();
        value["importFacts"]["projectCatalogue"]["result"]["value"][0][field]["result"]["value"] =
            replacement;
        assert_failure(&fixture, &value, FactsUnavailableReason::Malformed)?;
    }
    Ok(())
}

#[test]
fn successful_all_projects_catalogue_requires_observed_parent_holders() -> Result<()> {
    let fixture = fixture()?;
    let mut value = fixture.value.clone();
    catalogue(&mut value)?.remove(1);
    assert_failure(&fixture, &value, FactsUnavailableReason::MissingMetadata)?;
    Ok(())
}
