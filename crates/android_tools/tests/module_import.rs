// Copyright 2000-2026 JetBrains s.r.o. and contributors.
// Licensed under the Apache License, Version 2.0.
// Seven KotlinFacetBridgeTest state/settings cases are adapted below; the
// IntelliJ Swing facet editor test has no corresponding module import API.
// See test_data/module_import/reference-cases.json for per-assertion scope.

use android_tools::{
    import_facts::{ImportFactsBinding, ImportFactsSnapshot, parse_import_facts},
    kotlin_import_facts::{CaptureContext, CaptureLimits, parse_kotlin_facts},
    module_import::{
        COMPILE_TASKS, COMPILER_ARGUMENTS, COMPILER_VERSION, ExternalSystemRunTask, ImportNameMode,
        ImportNaming, ImportRevision, ImportTransaction, ImportedNameInput, KOTLIN_EXTENSION,
        KotlinCompilerSettings, KotlinImportFacts, KotlinMemberState, KotlinSettings,
        KotlinUnknownReason, ModuleImportPublisher, PLUGIN_IDS, PLUGIN_INTERFACES, SOURCE_SET_NAME,
        StrictKotlinProjectPlan, import_kotlin_from_strict_capture, imported_internal_name,
        parse_kotlin_import_facts, propose_legacy_kotlin_member,
    },
    project_model::{ProjectModel, VariantId, parse_model},
    project_tree_adapter::KotlinCapability,
    project_tree_facts::FactsUnavailableReason,
};
use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::Path};

struct Fixture {
    _directory: tempfile::TempDir,
    value: Value,
    model: ProjectModel,
    identity: ImportFactsSnapshot,
    kotlin: KotlinImportFacts,
    naming: ImportNaming,
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

fn observation(getter: &str, value: Value) -> Value {
    json!({"getter": getter, "result": {"status": "available", "value": value}})
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
        selected_variants: vec![VariantId {
            module: ":android".into(),
            variant: "debug".into(),
        }],
    }
}

fn fixture() -> Result<Fixture> {
    let directory = tempfile::Builder::new()
        .prefix("not-declared-name-")
        .tempdir()?;
    let root = directory.path().canonicalize()?;
    for name in ["android", "strange-parent", "shared-directory"] {
        fs::create_dir(root.join(name))?;
    }
    let mut value: Value =
        serde_json::from_str(include_str!("../test_data/import_facts/wire-template.json"))?;
    replace_root(&mut value, root.to_str().context("Fixture root UTF-8")?);
    let modules = value["importFacts"]["modules"].clone();
    let projects = [":android", ":nested:library"]
        .into_iter()
        .map(|path| {
            json!({
                "projectPath": path,
                "pluginIds": observation(PLUGIN_IDS, json!(["kotlin-android"])),
                "pluginInterfaces": observation(PLUGIN_INTERFACES, json!([])),
                "kotlinExtension": observation(KOTLIN_EXTENSION, json!(true)),
                "compilerVersion": observation(COMPILER_VERSION, json!("2.2.10")),
                "compileTasks": observation(COMPILE_TASKS, json!([{
                    "path": format!("{path}:compileDebugKotlin"),
                    "className": "org.jetbrains.kotlin.gradle.tasks.KotlinCompile_Decorated",
                    "sourceSetName": observation(SOURCE_SET_NAME, json!("debug")),
                    "compilerArguments": observation(COMPILER_ARGUMENTS, json!([]))
                }]))
            })
        })
        .collect::<Vec<_>>();
    value["moduleImportFacts"] =
        json!({"schema": 1, "root": root, "modules": modules, "projects": projects});
    let model = parse_model(&wire(&value)?, &root)?;
    let identity = parse_import_facts(&wire(&value)?, &model, binding())?;
    let kotlin = parse_kotlin_import_facts(&wire(&value)?, &model, &identity, binding())?;
    let naming = ImportNaming {
        mode: ImportNameMode::Phased,
        root_build_directory: root,
        root_build_name: "declared-root".into(),
        build_src_group: None,
        existing_names: BTreeSet::new(),
    };
    Ok(Fixture {
        _directory: directory,
        value,
        model,
        identity,
        kotlin,
        naming,
    })
}

fn revision(fixture: &Fixture, import_revision: u64) -> ImportRevision {
    ImportRevision {
        context_generation: 2,
        model_revision: 7,
        selection_revision: 3,
        import_revision,
        root: fixture.model.root.clone(),
    }
}

fn stage(
    fixture: &Fixture,
    publisher: &ModuleImportPublisher,
    import_revision: u64,
) -> Result<ImportTransaction> {
    Ok(publisher.stage(
        &fixture.model,
        &fixture.identity,
        &fixture.kotlin,
        revision(fixture, import_revision),
        &fixture.naming,
    )?)
}

fn settings(transaction: &mut ImportTransaction) -> Result<&mut KotlinSettings> {
    transaction
        .settings_mut(":android", "declared-root.android.debug")
        .context("Imported Kotlin member")
}

fn create_default_settings(transaction: &mut ImportTransaction) -> Result<()> {
    *transaction
        .create_settings(":android", "declared-root.android.debug")
        .context("Explicit settings creation")? =
        KotlinSettings::new("declared-root.android.debug");
    Ok(())
}

fn committed_settings(publisher: &ModuleImportPublisher) -> Result<&KotlinSettings> {
    let state = publisher.committed().context("Committed import")?;
    let module = state.modules.get(":android").context("Imported module")?;
    let present = module
        .members
        .iter()
        .filter_map(|member| match &member.kotlin {
            KotlinMemberState::Present(settings) => Some(settings.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(present.len(), 1);
    present
        .first()
        .copied()
        .context("One Kotlin settings record")
}

fn check_storage(publisher: &ModuleImportPublisher, expected: &KotlinSettings) -> Result<()> {
    let committed = committed_settings(publisher)?;
    assert_eq!(committed.name, expected.name);
    assert_eq!(committed.module_name, expected.module_name);
    assert_eq!(
        committed.use_project_settings,
        expected.use_project_settings
    );
    assert_eq!(committed.hmpp_enabled, expected.hmpp_enabled);
    assert_eq!(
        committed.external_system_run_tasks,
        expected.external_system_run_tasks
    );
    assert_eq!(committed.compiler_arguments, expected.compiler_arguments);
    assert_eq!(committed.compiler_settings, expected.compiler_settings);
    assert_eq!(committed.target_platform, expected.target_platform);
    assert!(committed.compiler_arguments.is_none());
    assert!(committed.compiler_settings.is_none());
    assert!(committed.target_platform.is_none());
    assert_eq!(
        committed.production_output_path,
        expected.production_output_path
    );
    assert_eq!(committed.test_output_path, expected.test_output_path);
    assert_eq!(
        serde_json::from_str::<KotlinSettings>(&serde_json::to_string(committed)?)?,
        *expected
    );
    Ok(())
}

fn create_committed_default(
    fixture: &Fixture,
    publisher: &mut ModuleImportPublisher,
) -> Result<()> {
    let mut creation = stage(fixture, publisher, 1)?;
    create_default_settings(&mut creation)?;
    let expected = settings(&mut creation)?.clone();
    publisher.commit(creation, &revision(fixture, 1))?;
    check_storage(publisher, &expected)
}

#[test]
fn kotlin_facet_bridge_test_simple_kotlin_facet_create() -> Result<()> {
    let fixture = fixture()?;
    let mut publisher = ModuleImportPublisher::default();
    let mut transaction = stage(&fixture, &publisher, 1)?;
    create_default_settings(&mut transaction)?;
    let expected = settings(&mut transaction)?.clone();
    assert!(publisher.committed().is_none());
    publisher.commit(transaction, &revision(&fixture, 1))?;
    check_storage(&publisher, &expected)
}

#[test]
fn kotlin_facet_bridge_test_facet_rename_and_remove() -> Result<()> {
    let fixture = fixture()?;
    let mut publisher = ModuleImportPublisher::default();
    let mut creation = stage(&fixture, &publisher, 1)?;
    create_default_settings(&mut creation)?;
    publisher.commit(creation, &revision(&fixture, 1))?;
    let old = committed_settings(&publisher)?.clone();
    assert_eq!(old.name, "Kotlin");
    let mut transaction = publisher.edit(revision(&fixture, 2))?;
    settings(&mut transaction)?.name = "NewFacetName".into();
    assert_eq!(committed_settings(&publisher)?.name, "Kotlin");
    assert_eq!(settings(&mut transaction)?.name, "NewFacetName");
    let renamed = settings(&mut transaction)?.clone();
    check_storage(&publisher, &old)?;
    publisher.commit(transaction, &revision(&fixture, 2))?;
    check_storage(&publisher, &renamed)?;
    let mut transaction = publisher.edit(revision(&fixture, 3))?;
    assert!(transaction.remove_kotlin(":android", "declared-root.android.debug"));
    publisher.commit(transaction, &revision(&fixture, 3))?;
    let module = publisher
        .committed()
        .context("Committed removal")?
        .modules
        .get(":android")
        .context("Module")?;
    assert_eq!(module.kotlin_capability(), KotlinCapability::Disabled);
    assert!(
        module
            .members
            .iter()
            .all(|member| !matches!(member.kotlin, KotlinMemberState::Present(_)))
    );
    Ok(())
}

#[test]
fn kotlin_facet_bridge_test_create_facet_with_disabled_use_project_settings_flag() -> Result<()> {
    let fixture = fixture()?;
    let mut publisher = ModuleImportPublisher::default();
    create_committed_default(&fixture, &mut publisher)?;
    let mut transaction = publisher.edit(revision(&fixture, 2))?;
    settings(&mut transaction)?.use_project_settings = false;
    let expected = settings(&mut transaction)?.clone();
    publisher.commit(transaction, &revision(&fixture, 2))?;
    check_storage(&publisher, &expected)
}

#[test]
fn kotlin_facet_bridge_test_create_facet_enable_hmpp() -> Result<()> {
    let fixture = fixture()?;
    let mut publisher = ModuleImportPublisher::default();
    create_committed_default(&fixture, &mut publisher)?;
    let mut transaction = publisher.edit(revision(&fixture, 2))?;
    settings(&mut transaction)?.hmpp_enabled = true;
    let expected = settings(&mut transaction)?.clone();
    publisher.commit(transaction, &revision(&fixture, 2))?;
    check_storage(&publisher, &expected)
}

#[test]
fn kotlin_facet_bridge_test_create_facet_external_system_project_id() -> Result<()> {
    let fixture = fixture()?;
    let mut publisher = ModuleImportPublisher::default();
    create_committed_default(&fixture, &mut publisher)?;
    let mut transaction = publisher.edit(revision(&fixture, 2))?;
    settings(&mut transaction)?.external_system_run_tasks = vec![ExternalSystemRunTask {
        task_name: "taskName".into(),
        external_system_project_id: "externalSystemProjectId".into(),
        target_name: "targetName".into(),
        kotlin_platform_id: "kotlinPlatformId".into(),
    }];
    let expected = settings(&mut transaction)?.clone();
    publisher.commit(transaction, &revision(&fixture, 2))?;
    check_storage(&publisher, &expected)
}

#[test]
fn kotlin_facet_bridge_test_create_facet_and_check_nullable_types() -> Result<()> {
    let fixture = fixture()?;
    let mut publisher = ModuleImportPublisher::default();
    create_committed_default(&fixture, &mut publisher)?;
    let mut transaction = publisher.edit(revision(&fixture, 2))?;
    let value = settings(&mut transaction)?;
    value.compiler_arguments = None;
    value.compiler_settings = None;
    value.target_platform = None;
    value.production_output_path = None;
    value.test_output_path = None;
    let expected = value.clone();
    publisher.commit(transaction, &revision(&fixture, 2))?;
    check_storage(&publisher, &expected)?;
    let json = serde_json::to_value(committed_settings(&publisher)?)?;
    for field in [
        "compiler_arguments",
        "compiler_settings",
        "target_platform",
        "production_output_path",
        "test_output_path",
    ] {
        assert_eq!(json[field], Value::Null);
    }
    Ok(())
}

#[test]
fn kotlin_facet_bridge_test_change_compiler_settings_after_creation() -> Result<()> {
    let fixture = fixture()?;
    let mut publisher = ModuleImportPublisher::default();
    create_committed_default(&fixture, &mut publisher)?;
    let mut transaction = publisher.edit(revision(&fixture, 2))?;
    settings(&mut transaction)?.compiler_settings = Some(KotlinCompilerSettings {
        additional_arguments: "foo".into(),
        ..Default::default()
    });
    publisher.commit(transaction, &revision(&fixture, 2))?;
    let mut transaction = publisher.edit(revision(&fixture, 3))?;
    settings(&mut transaction)?
        .compiler_settings
        .as_mut()
        .context("Compiler settings")?
        .script_templates = "bar".into();
    let expected = settings(&mut transaction)?.clone();
    publisher.commit(transaction, &revision(&fixture, 3))?;
    let actual = committed_settings(&publisher)?
        .compiler_settings
        .as_ref()
        .context("Compiler settings persisted")?;
    assert_eq!(actual.additional_arguments, "foo");
    assert_eq!(actual.script_templates, "bar");
    assert_eq!(committed_settings(&publisher)?, &expected);
    Ok(())
}

#[test]
fn publication_rejects_stale_active_context_aba_and_out_of_order_commit() -> Result<()> {
    let fixture = fixture()?;
    let mut publisher = ModuleImportPublisher::default();
    let first = stage(&fixture, &publisher, 1)?;
    let delayed = stage(&fixture, &publisher, 2)?;
    let mut returned_a = revision(&fixture, 1);
    returned_a.context_generation += 2;
    assert_eq!(
        publisher
            .commit(first.clone(), &returned_a)
            .expect_err("Old A token cannot revive")
            .reason,
        FactsUnavailableReason::Stale
    );
    assert!(publisher.committed().is_none());
    publisher.commit(first, &revision(&fixture, 1))?;
    assert_eq!(
        publisher
            .commit(delayed, &revision(&fixture, 2))
            .expect_err("Previous committed token differs")
            .reason,
        FactsUnavailableReason::Stale
    );
    assert_eq!(
        publisher
            .committed()
            .context("First import retained")?
            .revision
            .import_revision,
        1
    );
    Ok(())
}

fn recapture(fixture: &Fixture, value: &Value) -> Result<KotlinImportFacts> {
    Ok(parse_kotlin_import_facts(
        &wire(value)?,
        &fixture.model,
        &fixture.identity,
        binding(),
    )?)
}

fn strict_fixture(fixture: &Fixture) -> Result<(Value, CaptureContext, StrictKotlinProjectPlan)> {
    let mut value: Value = serde_json::from_str(include_str!(
        "../test_data/module_import/strict-projection-template.json"
    ))?;
    assert_eq!(value["synthetic_fixture"], json!(true));
    assert_eq!(value["runtime_credit"], json!(false));
    replace_root(
        &mut value,
        fixture.model.root.to_str().context("UTF-8 root")?,
    );
    value["packet"]["modules"] = fixture.value["importFacts"]["modules"].clone();
    value["packet"]["context"]["imports"] = json!({
        "buildIdentity": fixture.value["importFacts"]["buildIdentity"],
        "projectCatalogue": fixture.value["importFacts"]["projectCatalogue"],
    });
    let expected = serde_json::from_value(value["packet"]["context"].clone())?;
    let plan = serde_json::from_value(value["plan"].clone())?;
    let mut output = fixture.value.clone();
    output["kotlinFacts"] = value["packet"].clone();
    Ok((output, expected, plan))
}

#[test]
fn strict_snapshot_projection_publishes_membership_and_rejects_incomplete_or_wrong_plan()
-> Result<()> {
    let mut fixture = fixture()?;
    let (value, expected, plan) = strict_fixture(&fixture)?;
    let snapshot = parse_kotlin_facts(
        &wire(&value)?,
        &fixture.model,
        &fixture.identity,
        &expected,
        CaptureLimits::default(),
    )?;
    fixture.kotlin = import_kotlin_from_strict_capture(
        &fixture.model,
        &fixture.identity,
        &snapshot,
        &expected,
        &revision(&fixture, 1),
        std::slice::from_ref(&plan),
    )?;
    let mut publisher = ModuleImportPublisher::default();
    publisher.commit(stage(&fixture, &publisher, 1)?, &revision(&fixture, 1))?;
    assert_eq!(
        publisher.committed().context("Committed import")?.modules[":android"].kotlin_capability(),
        KotlinCapability::Enabled
    );
    assert_eq!(
        committed_settings(&publisher)?
            .compiler_arguments
            .as_deref(),
        Some(
            [
                "-module-name".to_string(),
                "declared-root.android.debug".to_string()
            ]
            .as_slice()
        )
    );
    let mut incomplete = plan.clone();
    incomplete.plugin_lookups.remove("kotlin-android");
    assert_eq!(
        import_kotlin_from_strict_capture(
            &fixture.model,
            &fixture.identity,
            &snapshot,
            &expected,
            &revision(&fixture, 1),
            &[incomplete]
        )
        .expect_err("Absence needs every official lookup")
        .reason,
        FactsUnavailableReason::MissingMetadata
    );
    let mut wrong = plan;
    wrong.compiler_version = "source-first".into();
    assert_eq!(
        import_kotlin_from_strict_capture(
            &fixture.model,
            &fixture.identity,
            &snapshot,
            &expected,
            &revision(&fixture, 1),
            &[wrong]
        )
        .expect_err("Source-set string is not a compiler version")
        .reason,
        FactsUnavailableReason::Malformed
    );
    Ok(())
}

#[test]
fn unverified_reimport_preserves_explicit_settings_but_cannot_enable_a_new_owner() -> Result<()> {
    let mut fixture = fixture()?;
    let mut publisher = ModuleImportPublisher::default();
    create_committed_default(&fixture, &mut publisher)?;
    fixture.value["moduleImportFacts"]["projects"][0]["compilerVersion"] =
        observation(COMPILER_VERSION, Value::Null);
    fixture.kotlin = recapture(&fixture, &fixture.value)?;
    let transaction = stage(&fixture, &publisher, 2)?;
    assert_eq!(
        transaction
            .module(":android")
            .context("Module")?
            .kotlin_capability(),
        KotlinCapability::Enabled
    );
    let mut new_owner = revision(&fixture, 3);
    new_owner.context_generation += 1;
    let transaction = publisher.stage(
        &fixture.model,
        &fixture.identity,
        &fixture.kotlin,
        new_owner,
        &fixture.naming,
    )?;
    assert_eq!(
        transaction
            .module(":android")
            .context("Module")?
            .kotlin_capability(),
        KotlinCapability::Unknown
    );
    fixture.value["moduleImportFacts"]["projects"][0]["pluginIds"] =
        observation(PLUGIN_IDS, json!(["org.jetbrains.kotlin.multiplatform"]));
    fixture.kotlin = recapture(&fixture, &fixture.value)?;
    let project =
        serde_json::from_value(fixture.value["moduleImportFacts"]["projects"][0].clone())?;
    assert_eq!(
        propose_legacy_kotlin_member(&project, "debug", "declared-root.android.debug"),
        KotlinMemberState::Unknown(KotlinUnknownReason::MultiplatformImport)
    );
    Ok(())
}

#[test]
fn absence_proposal_ignores_files_and_unverified_transport_cannot_remove_settings() -> Result<()> {
    let mut fixture = fixture()?;
    let mut publisher = ModuleImportPublisher::default();
    create_committed_default(&fixture, &mut publisher)?;
    fs::write(
        fixture.model.root.join("android/LooksLikeKotlin.kt"),
        "val irrelevant = 1",
    )?;
    fixture.value["moduleImportFacts"]["projects"][0]["pluginIds"] =
        observation(PLUGIN_IDS, json!([]));
    fixture.value["moduleImportFacts"]["projects"][0]["kotlinExtension"] =
        observation(KOTLIN_EXTENSION, json!(false));
    fixture.kotlin = recapture(&fixture, &fixture.value)?;
    let project =
        serde_json::from_value(fixture.value["moduleImportFacts"]["projects"][0].clone())?;
    assert_eq!(
        propose_legacy_kotlin_member(&project, "debug", "declared-root.android.debug"),
        KotlinMemberState::Absent
    );
    publisher.commit(stage(&fixture, &publisher, 2)?, &revision(&fixture, 2))?;
    assert_eq!(
        publisher.committed().context("Current")?.modules[":android"].kotlin_capability(),
        KotlinCapability::Enabled
    );
    Ok(())
}

#[test]
fn built_in_kotlin_needs_source_set_version_arguments_and_complete_membership() -> Result<()> {
    let mut fixture = fixture()?;
    fixture.value["moduleImportFacts"]["projects"][0]["pluginIds"] =
        observation(PLUGIN_IDS, json!(["com.android.library"]));
    fixture.value["moduleImportFacts"]["projects"][0]["pluginInterfaces"] = observation(
        PLUGIN_INTERFACES,
        json!(["org.jetbrains.kotlin.gradle.plugin.KotlinJvmFactory"]),
    );
    fixture.kotlin = recapture(&fixture, &fixture.value)?;
    let publisher = ModuleImportPublisher::default();
    let project =
        serde_json::from_value(fixture.value["moduleImportFacts"]["projects"][0].clone())?;
    assert!(matches!(
        propose_legacy_kotlin_member(&project, "debug", "declared-root.android.debug"),
        KotlinMemberState::Present(_)
    ));
    assert_eq!(
        stage(&fixture, &publisher, 1)?
            .module(":android")
            .context("Module")?
            .kotlin_capability(),
        KotlinCapability::Unknown
    );
    fixture.value["moduleImportFacts"]["projects"][0]["compileTasks"] =
        observation(COMPILE_TASKS, json!([]));
    fixture.kotlin = recapture(&fixture, &fixture.value)?;
    let project =
        serde_json::from_value(fixture.value["moduleImportFacts"]["projects"][0].clone())?;
    assert_eq!(
        propose_legacy_kotlin_member(&project, "debug", "declared-root.android.debug"),
        KotlinMemberState::Unknown(KotlinUnknownReason::MissingSourceSet)
    );
    assert_eq!(
        stage(&fixture, &publisher, 2)?
            .module(":android")
            .context("Module")?
            .kotlin_capability(),
        KotlinCapability::Unknown
    );
    Ok(())
}

#[test]
fn sidecar_rejects_wrong_getter_duplicate_project_schema_missing_and_sequence_shapes() -> Result<()>
{
    let fixture = fixture()?;
    let mut cases = Vec::new();
    let mut value = fixture.value.clone();
    value["moduleImportFacts"]["schema"] = json!(2);
    cases.push((value, FactsUnavailableReason::UnsupportedSchema));
    let mut value = fixture.value.clone();
    value["moduleImportFacts"]["projects"][0]["compilerVersion"]["getter"] =
        json!("guessed-version");
    cases.push((value, FactsUnavailableReason::Malformed));
    let mut value = fixture.value.clone();
    value["moduleImportFacts"]["projects"][1] = value["moduleImportFacts"]["projects"][0].clone();
    cases.push((value, FactsUnavailableReason::Malformed));
    let mut value = fixture.value.clone();
    value["moduleImportFacts"] = json!([1]);
    cases.push((value, FactsUnavailableReason::Malformed));
    let mut value = fixture.value.clone();
    value["moduleImportFacts"]["projects"][0]["compileTasks"]["result"] =
        json!({"status": "available"});
    cases.push((value, FactsUnavailableReason::Malformed));
    for (value, reason) in cases {
        assert_eq!(
            recapture(&fixture, &value)
                .expect_err("Reject invalid capability")
                .downcast::<android_tools::project_tree_facts::FactsUnavailable>()?
                .reason,
            reason
        );
    }
    Ok(())
}

#[test]
fn phased_names_keep_declared_root_build_src_escape_and_one_collision_suffix() -> Result<()> {
    let settings = ImportNaming {
        mode: ImportNameMode::Phased,
        root_build_directory: "/build/real-folder".into(),
        root_build_name: "Declared Project".into(),
        build_src_group: None,
        existing_names: BTreeSet::new(),
    };
    let input = ImportedNameInput {
        build_directory: Path::new("/build/real-folder"),
        build_name: "Declared Project",
        identity_path: ":nested:app.dot",
        qualified_path: None,
        idea_module_name: None,
        source_set_name: None,
    };
    assert_eq!(
        imported_internal_name(&input, &settings)?,
        "Declared_Project.nested.app_dot"
    );
    let source = ImportedNameInput {
        source_set_name: Some("debug"),
        ..input.clone()
    };
    let mut collision = settings.clone();
    collision
        .existing_names
        .insert("Declared_Project.nested.app_dot.debug".into());
    assert_eq!(
        imported_internal_name(&source, &collision)?,
        "Declared_Project.nested.app_dot.debug~1"
    );
    collision
        .existing_names
        .insert("Declared_Project.nested.app_dot.debug~1".into());
    assert_eq!(
        imported_internal_name(&source, &collision)
            .expect_err("No invented suffix")
            .reason,
        FactsUnavailableReason::Capability
    );
    let build_src = ImportedNameInput {
        build_directory: Path::new("/build/real-folder/buildSrc"),
        build_name: "buildSrc",
        identity_path: ":buildSrc",
        ..input.clone()
    };
    assert_eq!(
        imported_internal_name(&build_src, &settings)?,
        "Declared_Project.buildSrc"
    );
    let included = ImportedNameInput {
        build_directory: Path::new("/elsewhere/included"),
        build_name: "Included",
        identity_path: ":included:app",
        ..input
    };
    assert_eq!(
        imported_internal_name(&included, &settings)?,
        "included.app"
    );
    Ok(())
}

#[test]
fn unqualified_requires_nullable_idea_name_qualified_requires_captured_qname() -> Result<()> {
    let fixture = fixture()?;
    let input = ImportedNameInput {
        build_directory: &fixture.model.root,
        build_name: "declared-root",
        identity_path: ":android",
        qualified_path: None,
        idea_module_name: None,
        source_set_name: Some("main"),
    };
    let mut settings = fixture.naming.clone();
    settings.mode = ImportNameMode::Unqualified;
    assert_eq!(
        imported_internal_name(&input, &settings)
            .expect_err("No basename fallback")
            .reason,
        FactsUnavailableReason::MissingMetadata
    );
    let named = ImportedNameInput {
        idea_module_name: Some("Configured Name"),
        ..input.clone()
    };
    assert_eq!(
        imported_internal_name(&named, &settings)?,
        "Configured_Name_main"
    );
    settings.mode = ImportNameMode::Qualified;
    assert_eq!(
        imported_internal_name(&input, &settings)
            .expect_err("No guessed qname")
            .reason,
        FactsUnavailableReason::MissingMetadata
    );
    let qualified = ImportedNameInput {
        qualified_path: Some(":nested:android"),
        ..input
    };
    assert_eq!(
        imported_internal_name(&qualified, &settings)?,
        "declared-root.nested.android.main"
    );
    Ok(())
}

#[test]
fn holder_only_capture_stays_unknown_with_unverified_or_failed_getters() -> Result<()> {
    let mut fixture = fixture()?;
    fixture.value["modules"][0]["variants"][0]["components"] = json!([]);
    fixture.model = parse_model(&wire(&fixture.value)?, &fixture.model.root)?;
    fixture.identity = parse_import_facts(&wire(&fixture.value)?, &fixture.model, binding())?;
    fixture.kotlin = recapture(&fixture, &fixture.value)?;
    let publisher = ModuleImportPublisher::default();
    let transaction = stage(&fixture, &publisher, 1)?;
    let module = transaction
        .module(":android")
        .context("Holder-only module")?;
    assert_eq!(module.members.len(), 1);
    assert_eq!(module.kotlin_capability(), KotlinCapability::Unknown);
    assert_eq!(
        module.members[0].kotlin,
        KotlinMemberState::Unknown(KotlinUnknownReason::MissingSourceSet)
    );

    let (mut value, expected, plan) = strict_fixture(&fixture)?;
    let event = value["kotlinFacts"]["events"]
        .as_array_mut()
        .context("Events")?
        .iter_mut()
        .find(|event| event["request"] == "version")
        .context("Version event")?;
    event["outcome"] = json!({"status":"unavailable", "value": {
        "kind":"invocation", "stage":"invoke", "capability":COMPILER_VERSION,
        "detail":"Synthetic version getter failure", "actualClass":null,
        "exceptions":[{"class":"java.lang.IllegalStateException","message":"synthetic failure"}]
    }});
    let snapshot = parse_kotlin_facts(
        &wire(&value)?,
        &fixture.model,
        &fixture.identity,
        &expected,
        CaptureLimits::default(),
    )?;
    fixture.kotlin = import_kotlin_from_strict_capture(
        &fixture.model,
        &fixture.identity,
        &snapshot,
        &expected,
        &revision(&fixture, 1),
        &[plan],
    )?;
    assert_eq!(
        stage(&fixture, &publisher, 1)?
            .module(":android")
            .context("Failed strict holder")?
            .kotlin_capability(),
        KotlinCapability::Unknown
    );
    Ok(())
}

#[test]
fn explicit_holder_settings_survive_same_owner_reimport_and_do_not_cross_contexts() -> Result<()> {
    let mut fixture = fixture()?;
    let mut publisher = ModuleImportPublisher::default();
    let mut transaction = stage(&fixture, &publisher, 1)?;
    let settings = transaction
        .create_settings(":android", "declared-root.android")
        .context("Holder settings")?;
    settings.name = "User holder facet".into();
    settings.compiler_settings = Some(KotlinCompilerSettings {
        additional_arguments: "-Xexplicit-holder".into(),
        ..KotlinCompilerSettings::default()
    });
    let expected = settings.clone();
    publisher.commit(transaction, &revision(&fixture, 1))?;
    fixture.value["modules"][0]["variants"][0]["components"] = json!([]);
    fixture.model = parse_model(&wire(&fixture.value)?, &fixture.model.root)?;
    fixture.identity = parse_import_facts(&wire(&fixture.value)?, &fixture.model, binding())?;
    fixture.kotlin = recapture(&fixture, &fixture.value)?;
    publisher.commit(stage(&fixture, &publisher, 2)?, &revision(&fixture, 2))?;
    let module = &publisher.committed().context("Reimport")?.modules[":android"];
    assert_eq!(module.members.len(), 1);
    assert_eq!(
        module.members[0].kotlin,
        KotlinMemberState::Present(Box::new(expected))
    );
    assert_eq!(module.kotlin_capability(), KotlinCapability::Enabled);
    let mut next_owner = revision(&fixture, 3);
    next_owner.context_generation += 1;
    let transaction = publisher.stage(
        &fixture.model,
        &fixture.identity,
        &fixture.kotlin,
        next_owner,
        &fixture.naming,
    )?;
    assert_eq!(
        transaction
            .module(":android")
            .context("New owner")?
            .kotlin_capability(),
        KotlinCapability::Unknown
    );
    Ok(())
}

#[test]
fn strict_old_facts_cannot_be_rebound_to_a_returned_workspace_generation() -> Result<()> {
    let mut fixture = fixture()?;
    let (mut value, expected, plan) = strict_fixture(&fixture)?;
    let snapshot = parse_kotlin_facts(
        &wire(&value)?,
        &fixture.model,
        &fixture.identity,
        &expected,
        CaptureLimits::default(),
    )?;
    fixture.kotlin = import_kotlin_from_strict_capture(
        &fixture.model,
        &fixture.identity,
        &snapshot,
        &expected,
        &revision(&fixture, 1),
        std::slice::from_ref(&plan),
    )?;
    let mut publisher = ModuleImportPublisher::default();
    publisher.commit(stage(&fixture, &publisher, 1)?, &revision(&fixture, 1))?;
    let mut returned_owner = revision(&fixture, 2);
    returned_owner.context_generation += 2;
    assert_eq!(
        publisher
            .stage(
                &fixture.model,
                &fixture.identity,
                &fixture.kotlin,
                returned_owner.clone(),
                &fixture.naming
            )
            .err()
            .context("Old invocation must not receive new owner")?
            .reason,
        FactsUnavailableReason::Stale
    );
    assert_eq!(
        publisher
            .committed()
            .context("Old state retained")?
            .revision,
        revision(&fixture, 1)
    );
    value["kotlinFacts"]["context"]["binding"]["captureId"] =
        json!("fresh-returned-owner-invocation");
    value["kotlinFacts"]["context"]["binding"]["sourceEpoch"] = json!("fresh-returned-owner-epoch");
    let fresh_expected = serde_json::from_value(value["kotlinFacts"]["context"].clone())?;
    let fresh_snapshot = parse_kotlin_facts(
        &wire(&value)?,
        &fixture.model,
        &fixture.identity,
        &fresh_expected,
        CaptureLimits::default(),
    )?;
    let fresh = import_kotlin_from_strict_capture(
        &fixture.model,
        &fixture.identity,
        &fresh_snapshot,
        &fresh_expected,
        &returned_owner,
        &[plan],
    )?;
    let transaction = publisher.stage(
        &fixture.model,
        &fixture.identity,
        &fresh,
        returned_owner.clone(),
        &fixture.naming,
    )?;
    publisher.commit(transaction, &returned_owner)?;
    assert_eq!(
        publisher.committed().context("Fresh invocation")?.modules[":android"].kotlin_capability(),
        KotlinCapability::Enabled
    );
    Ok(())
}

#[test]
fn grouped_import_preserves_root_nonroot_and_source_set_external_ids() -> Result<()> {
    let mut fixture = fixture()?;
    fixture.naming.build_src_group = Some("host".into());
    let publisher = ModuleImportPublisher::default();
    let transaction = stage(&fixture, &publisher, 1)?;
    let nonroot = transaction.module(":android").context("Grouped nonroot")?;
    assert_eq!(nonroot.holder_internal_name, "host.declared-root.android");
    assert_eq!(nonroot.members[0].external_id, "host::android");
    assert_eq!(nonroot.members[1].external_id, "host::android:debug");
    assert_eq!(
        nonroot.members[1].internal_name,
        "host.declared-root.android.debug"
    );
    assert_eq!(nonroot.display_name, "android");
    assert_eq!(nonroot.sort_name, nonroot.holder_internal_name);

    fixture.value["modules"][0]["path"] = json!(":");
    fixture.value["modules"][0]["directory"] = json!(fixture.model.root);
    for field in ["importFacts", "moduleImportFacts"] {
        fixture.value[field]["modules"][0]["module"] = json!(":");
        fixture.value[field]["modules"][0]["directory"] = json!(fixture.model.root);
    }
    fixture.value["moduleImportFacts"]["projects"][0]["projectPath"] = json!(":");
    fixture.value["moduleImportFacts"]["projects"][0]["compileTasks"]["result"]["value"][0]["path"] =
        json!(":compileDebugKotlin");
    let root_binding = ImportFactsBinding {
        selected_variants: vec![VariantId {
            module: ":".into(),
            variant: "debug".into(),
        }],
        ..binding()
    };
    fixture.model = parse_model(&wire(&fixture.value)?, &fixture.model.root)?;
    fixture.identity =
        parse_import_facts(&wire(&fixture.value)?, &fixture.model, root_binding.clone())?;
    fixture.kotlin = parse_kotlin_import_facts(
        &wire(&fixture.value)?,
        &fixture.model,
        &fixture.identity,
        root_binding,
    )?;
    let transaction = stage(&fixture, &publisher, 1)?;
    let root = transaction.module(":").context("Grouped root")?;
    assert_eq!(root.holder_internal_name, "host.declared-root");
    assert_eq!(root.members[0].external_id, "host:declared-root");
    assert_eq!(root.members[1].external_id, "host:declared-root:debug");
    assert_eq!(root.members[1].internal_name, "host.declared-root.debug");
    assert_eq!(root.display_name, "host:declared-root");
    assert_eq!(root.sort_name, root.holder_internal_name);
    Ok(())
}

// These mutations define independently retained synthetic request contexts.
// They are adapter regressions, never original fixture/runtime parity evidence.
fn set_strict_plugin_ids(
    value: &mut Value,
    plan: &StrictKotlinProjectPlan,
    ids: &[&str],
) -> Result<()> {
    for (plugin, request) in &plan.plugin_lookups {
        let event = value["kotlinFacts"]["events"]
            .as_array_mut()
            .context("Strict events")?
            .iter_mut()
            .find(|event| event["request"] == request.as_str())
            .context("Planned plugin event")?;
        event["outcome"]["value"] = if ids.contains(&plugin.as_str()) {
            json!({"kind": "object", "value": "kotlin-plugin"})
        } else {
            Value::Null
        };
    }
    Ok(())
}

fn set_strict_factory_location(value: &mut Value, location: &str) -> Result<()> {
    let classes = value["kotlinFacts"]["context"]["runtime"]["classes"]
        .as_array_mut()
        .context("Runtime classes")?;
    let mut factory = classes
        .iter()
        .find(|class| class["id"] == "plugin-wrapper")
        .context("Plugin class")?
        .clone();
    factory["id"] = json!("jvm-factory");
    factory["name"] = json!("org.jetbrains.kotlin.gradle.plugin.KotlinJvmFactory");
    factory["superclass"] = Value::Null;
    factory["interfaces"] = json!([]);
    classes.push(factory.clone());
    let mut parent = factory.clone();
    parent["id"] = json!("plugin-parent");
    parent["name"] = json!("synthetic.PluginParent");
    parent["superclass"] = json!("object");
    parent["interfaces"] = if location == "immediate" {
        json!(["jvm-factory"])
    } else {
        json!([])
    };
    if location == "grandparent" {
        let mut grandparent = parent.clone();
        grandparent["id"] = json!("plugin-grandparent");
        grandparent["name"] = json!("synthetic.PluginGrandparent");
        grandparent["interfaces"] = json!(["jvm-factory"]);
        classes.push(grandparent);
        parent["superclass"] = json!("plugin-grandparent");
    } else if location == "transitive" {
        let mut bridge = factory;
        bridge["id"] = json!("factory-bridge");
        bridge["name"] = json!("synthetic.FactoryBridge");
        bridge["interfaces"] = json!(["jvm-factory"]);
        classes.push(bridge);
        parent["interfaces"] = json!(["factory-bridge"]);
    }
    classes.push(parent);
    let plugin = classes
        .iter_mut()
        .find(|class| class["id"] == "plugin-wrapper")
        .context("Plugin runtime class")?;
    plugin["superclass"] = json!("plugin-parent");
    if location == "self" {
        plugin["interfaces"] = json!(["plugin-base", "jvm-factory"]);
    }
    Ok(())
}

fn add_strict_android_base_predicate(
    value: &mut Value,
    plan: &mut StrictKotlinProjectPlan,
    applied: bool,
) -> Result<()> {
    let catalogue = value["kotlinFacts"]["context"]["catalogues"]
        .as_array_mut()
        .context("Runtime catalogues")?
        .iter_mut()
        .find(|catalogue| catalogue["id"] == "plugin-catalogue")
        .context("Plugin catalogue")?;
    catalogue["methods"]
        .as_array_mut()
        .context("Plugin methods")?
        .push(json!({
            "id": "has-plugin", "name": "hasPlugin", "descriptor": "(Ljava/lang/String;)Z",
            "declaringClass": "plugins", "isStatic": false,
            "parameterClasses": ["string"], "returnClass": null
        }));
    let requests = value["kotlinFacts"]["context"]["requests"]
        .as_array_mut()
        .context("Planned requests")?;
    let mut request = requests
        .iter()
        .find(|request| request["id"] == "lookup-0")
        .context("Plugin request template")?
        .clone();
    request["id"] = json!("android-base-plugin");
    request["method"]["value"] = json!("has-plugin");
    request["arguments"] = json!([{"kind": "string", "value": "com.android.base"}]);
    request["returnShape"] = json!({
        "kind": "boolean", "nullable": false, "objectKind": null, "order": null
    });
    requests.push(request);
    value["kotlinFacts"]["events"]
        .as_array_mut()
        .context("Captured events")?
        .push(json!({
            "id": "event-android-base-plugin", "request": "android-base-plugin",
            "outcome": {"status": "available", "value": {"kind": "boolean", "value": applied}},
            "container": null
        }));
    plan.android_base_plugin = Some("android-base-plugin".into());
    Ok(())
}

fn import_strict_case(
    fixture: &Fixture,
    value: &Value,
    plan: &StrictKotlinProjectPlan,
) -> Result<KotlinImportFacts> {
    let expected = serde_json::from_value(value["kotlinFacts"]["context"].clone())?;
    let snapshot = parse_kotlin_facts(
        &wire(value)?,
        &fixture.model,
        &fixture.identity,
        &expected,
        CaptureLimits::default(),
    )?;
    Ok(import_kotlin_from_strict_capture(
        &fixture.model,
        &fixture.identity,
        &snapshot,
        &expected,
        &revision(fixture, 1),
        std::slice::from_ref(plan),
    )?)
}

#[test]
fn strict_ambiguous_plugin_families_do_not_enable_membership() -> Result<()> {
    for ids in [
        vec!["kotlin", "kotlin-android"],
        vec!["kotlin-platform-jvm", "kotlin-platform-js"],
    ] {
        let mut fixture = fixture()?;
        let (mut value, _, plan) = strict_fixture(&fixture)?;
        set_strict_plugin_ids(&mut value, &plan, &ids)?;
        fixture.kotlin = import_strict_case(&fixture, &value, &plan)?;
        let mut publisher = ModuleImportPublisher::default();
        publisher.commit(stage(&fixture, &publisher, 1)?, &revision(&fixture, 1))?;
        let module = &publisher
            .committed()
            .context("Published ambiguity")?
            .modules[":android"];
        assert_eq!(module.kotlin_capability(), KotlinCapability::Unknown);
        assert_eq!(
            module.members[1].kotlin,
            KotlinMemberState::Unknown(KotlinUnknownReason::AmbiguousKotlinPluginIds)
        );
    }
    Ok(())
}

#[test]
fn strict_other_singleton_family_still_resolves_membership() -> Result<()> {
    for ids in [
        vec!["kotlin", "kotlin-android", "kotlin-platform-jvm"],
        vec![
            "kotlin-android",
            "kotlin-platform-jvm",
            "kotlin-platform-js",
        ],
    ] {
        let mut fixture = fixture()?;
        let (mut value, _, plan) = strict_fixture(&fixture)?;
        set_strict_plugin_ids(&mut value, &plan, &ids)?;
        fixture.kotlin = import_strict_case(&fixture, &value, &plan)?;
        let mut publisher = ModuleImportPublisher::default();
        publisher.commit(stage(&fixture, &publisher, 1)?, &revision(&fixture, 1))?;
        assert_eq!(
            publisher.committed().context("Resolved singleton")?.modules[":android"]
                .kotlin_capability(),
            KotlinCapability::Enabled
        );
        assert_eq!(
            committed_settings(&publisher)?.target_platform.as_deref(),
            Some("JVM")
        );
    }
    Ok(())
}

#[test]
fn strict_builtin_requires_exact_base_predicate_and_immediate_superclass_interface() -> Result<()> {
    for location in ["immediate", "self", "grandparent", "transitive"] {
        let mut fixture = fixture()?;
        let (mut value, _, mut plan) = strict_fixture(&fixture)?;
        set_strict_plugin_ids(&mut value, &plan, &["com.android.library"])?;
        set_strict_factory_location(&mut value, location)?;
        add_strict_android_base_predicate(&mut value, &mut plan, true)?;
        // A known absent Kotlin extension distinguishes proven absence from Unknown.
        value["kotlinFacts"]["events"]
            .as_array_mut()
            .context("Events")?
            .iter_mut()
            .find(|event| event["request"] == "extension-lookup")
            .context("Extension event")?["outcome"]["value"] = Value::Null;
        fixture.kotlin = import_strict_case(&fixture, &value, &plan)?;
        let mut publisher = ModuleImportPublisher::default();
        publisher.commit(stage(&fixture, &publisher, 1)?, &revision(&fixture, 1))?;
        let module = &publisher.committed().context("Builtin decision")?.modules[":android"];
        if location == "immediate" {
            assert_eq!(module.kotlin_capability(), KotlinCapability::Enabled);
            assert!(matches!(
                module.members[1].kotlin,
                KotlinMemberState::Present(_)
            ));
        } else {
            assert_eq!(module.kotlin_capability(), KotlinCapability::Disabled);
            assert_eq!(module.members[1].kotlin, KotlinMemberState::Absent);
        }
    }
    Ok(())
}

#[test]
fn strict_builtin_missing_or_false_base_predicate_never_uses_android_plugin_proxy() -> Result<()> {
    for base in [None, Some(false)] {
        let mut fixture = fixture()?;
        let (mut value, _, mut plan) = strict_fixture(&fixture)?;
        set_strict_plugin_ids(&mut value, &plan, &["com.android.library"])?;
        set_strict_factory_location(&mut value, "immediate")?;
        if let Some(applied) = base {
            add_strict_android_base_predicate(&mut value, &mut plan, applied)?;
        }
        fixture.kotlin = import_strict_case(&fixture, &value, &plan)?;
        let mut publisher = ModuleImportPublisher::default();
        publisher.commit(stage(&fixture, &publisher, 1)?, &revision(&fixture, 1))?;
        let module = &publisher.committed().context("Missing base guard")?.modules[":android"];
        assert_eq!(module.kotlin_capability(), KotlinCapability::Unknown);
        if base.is_none() {
            assert!(matches!(
                module.members[1].kotlin,
                KotlinMemberState::Unknown(KotlinUnknownReason::GetterUnavailable(_))
            ));
        } else {
            assert_eq!(
                module.members[1].kotlin,
                KotlinMemberState::Unknown(KotlinUnknownReason::UnrecognizedKotlinExtension)
            );
        }
    }
    Ok(())
}

#[test]
fn strict_builtin_fallback_resolves_ambiguous_legacy_family() -> Result<()> {
    let mut fixture = fixture()?;
    let (mut value, _, mut plan) = strict_fixture(&fixture)?;
    set_strict_plugin_ids(&mut value, &plan, &["kotlin", "kotlin-android"])?;
    set_strict_factory_location(&mut value, "immediate")?;
    add_strict_android_base_predicate(&mut value, &mut plan, true)?;
    fixture.kotlin = import_strict_case(&fixture, &value, &plan)?;
    let mut publisher = ModuleImportPublisher::default();
    publisher.commit(stage(&fixture, &publisher, 1)?, &revision(&fixture, 1))?;
    assert_eq!(
        publisher.committed().context("Builtin fallback")?.modules[":android"].kotlin_capability(),
        KotlinCapability::Enabled
    );
    assert_eq!(
        committed_settings(&publisher)?.target_platform.as_deref(),
        Some("JVM")
    );
    Ok(())
}

#[test]
fn strict_android_base_predicate_rejects_wrong_plugin_argument() -> Result<()> {
    let fixture = fixture()?;
    let (mut value, _, mut plan) = strict_fixture(&fixture)?;
    add_strict_android_base_predicate(&mut value, &mut plan, true)?;
    value["kotlinFacts"]["context"]["requests"]
        .as_array_mut()
        .context("Requests")?
        .iter_mut()
        .find(|request| request["id"] == "android-base-plugin")
        .context("Base plugin request")?["arguments"][0]["value"] = json!("com.android.library");
    assert_eq!(
        import_strict_case(&fixture, &value, &plan)
            .expect_err("An Android plugin proxy cannot prove com.android.base")
            .downcast::<android_tools::project_tree_facts::FactsUnavailable>()?
            .reason,
        FactsUnavailableReason::Malformed
    );
    Ok(())
}

#[test]
fn phased_names_preserve_absent_empty_and_nonempty_groups_and_collisions() -> Result<()> {
    let input = ImportedNameInput {
        build_directory: Path::new("/build/real-folder"),
        build_name: "Declared Project",
        identity_path: ":nested:app.dot",
        qualified_path: None,
        idea_module_name: None,
        source_set_name: None,
    };
    let source = ImportedNameInput {
        source_set_name: Some("debug"),
        ..input.clone()
    };
    for (group, holder, member, collision) in [
        (
            None,
            "Declared_Project.nested.app_dot",
            "Declared_Project.nested.app_dot.debug",
            "Declared_Project.nested.app_dot.debug~1",
        ),
        (
            Some(""),
            ".Declared_Project.nested.app_dot",
            ".Declared_Project.nested.app_dot.debug",
            ".Declared_Project.nested.app_dot.debug~1",
        ),
        (
            Some("host"),
            "host.Declared_Project.nested.app_dot",
            "host.Declared_Project.nested.app_dot.debug",
            "host.Declared_Project.nested.app_dot.debug~1",
        ),
    ] {
        let mut naming = ImportNaming {
            mode: ImportNameMode::Phased,
            root_build_directory: "/build/real-folder".into(),
            root_build_name: "Declared Project".into(),
            build_src_group: group.map(str::to_owned),
            existing_names: BTreeSet::new(),
        };
        assert_eq!(imported_internal_name(&input, &naming)?, holder);
        assert_eq!(imported_internal_name(&source, &naming)?, member);
        naming.existing_names.insert(member.into());
        assert_eq!(imported_internal_name(&source, &naming)?, collision);
        naming.existing_names.insert(collision.into());
        assert_eq!(
            imported_internal_name(&source, &naming)
                .expect_err("Grouped identities retain the single collision suffix")
                .reason,
            FactsUnavailableReason::Capability
        );
    }
    Ok(())
}

#[test]
fn legacy_names_ignore_only_empty_groups_and_preserve_nonempty_groups() -> Result<()> {
    let input = ImportedNameInput {
        build_directory: Path::new("/build/real-folder"),
        build_name: "declared-root",
        identity_path: ":nested:android",
        qualified_path: Some(":nested:android"),
        idea_module_name: Some("Configured Name"),
        source_set_name: None,
    };
    let source = ImportedNameInput {
        source_set_name: Some("main"),
        ..input.clone()
    };
    for (mode, group, holder, member) in [
        (
            ImportNameMode::Qualified,
            None,
            "declared-root.nested.android",
            "declared-root.nested.android.main",
        ),
        (
            ImportNameMode::Qualified,
            Some(""),
            "declared-root.nested.android",
            "declared-root.nested.android.main",
        ),
        (
            ImportNameMode::Qualified,
            Some("host"),
            "host.declared-root.nested.android",
            "host.declared-root.nested.android.main",
        ),
        (
            ImportNameMode::Unqualified,
            None,
            "Configured_Name",
            "Configured_Name_main",
        ),
        (
            ImportNameMode::Unqualified,
            Some(""),
            "Configured_Name",
            "Configured_Name_main",
        ),
        (
            ImportNameMode::Unqualified,
            Some("host"),
            "host_Configured_Name",
            "host_Configured_Name_main",
        ),
    ] {
        let naming = ImportNaming {
            mode,
            root_build_directory: "/build/real-folder".into(),
            root_build_name: "declared-root".into(),
            build_src_group: group.map(str::to_owned),
            existing_names: BTreeSet::new(),
        };
        assert_eq!(imported_internal_name(&input, &naming)?, holder);
        assert_eq!(imported_internal_name(&source, &naming)?, member);
    }
    Ok(())
}

#[test]
fn staged_empty_group_keeps_phased_names_and_ungrouped_external_ids() -> Result<()> {
    for (group, holder, member, external_holder, external_member) in [
        (
            None,
            "declared-root.android",
            "declared-root.android.debug",
            ":android",
            ":android:debug",
        ),
        (
            Some(""),
            ".declared-root.android",
            ".declared-root.android.debug",
            ":android",
            ":android:debug",
        ),
        (
            Some("host"),
            "host.declared-root.android",
            "host.declared-root.android.debug",
            "host::android",
            "host::android:debug",
        ),
    ] {
        let mut fixture = fixture()?;
        fixture.naming.build_src_group = group.map(str::to_owned);
        let publisher = ModuleImportPublisher::default();
        let transaction = stage(&fixture, &publisher, 1)?;
        let module = transaction
            .module(":android")
            .context("Staged grouped identities")?;
        assert_eq!(module.holder_internal_name, holder);
        assert_eq!(module.sort_name, holder);
        assert_eq!(module.members[0].internal_name, holder);
        assert_eq!(module.members[1].internal_name, member);
        assert_eq!(module.members[0].external_id, external_holder);
        assert_eq!(module.members[1].external_id, external_member);
        assert_eq!(module.display_name, "android");
    }
    Ok(())
}

#[test]
fn strict_projection_reuses_shared_containers_with_a_two_vertex_budget() -> Result<()> {
    let mut fixture = fixture()?;
    let (value, _, plan) = strict_fixture(&fixture)?;
    let expected: CaptureContext = serde_json::from_value(value["kotlinFacts"]["context"].clone())?;
    let snapshot = parse_kotlin_facts(
        &wire(&value)?,
        &fixture.model,
        &fixture.identity,
        &expected,
        CaptureLimits::default(),
    )?;
    fixture.kotlin = android_tools::module_import::import_kotlin_from_strict_capture_with_limits(
        &fixture.model,
        &fixture.identity,
        &snapshot,
        &expected,
        &revision(&fixture, 1),
        &[plan],
        CaptureLimits {
            ancestry_steps: 2,
            ..CaptureLimits::default()
        },
    )?;
    let mut publisher = ModuleImportPublisher::default();
    publisher.commit(stage(&fixture, &publisher, 1)?, &revision(&fixture, 1))?;
    assert_eq!(
        publisher
            .committed()
            .context("Budgeted strict import")?
            .modules[":android"]
            .kotlin_capability(),
        KotlinCapability::Enabled
    );
    Ok(())
}

#[test]
fn strict_projection_budget_failure_preserves_basic_and_import_identity() -> Result<()> {
    let fixture = fixture()?;
    let (value, _, plan) = strict_fixture(&fixture)?;
    let expected: CaptureContext = serde_json::from_value(value["kotlinFacts"]["context"].clone())?;
    let snapshot = parse_kotlin_facts(
        &wire(&value)?,
        &fixture.model,
        &fixture.identity,
        &expected,
        CaptureLimits::default(),
    )?;
    let publisher = ModuleImportPublisher::default();
    let before_root = fixture.model.root.clone();
    let before_binding = fixture.identity.binding().clone();
    assert_eq!(
        android_tools::module_import::import_kotlin_from_strict_capture_with_limits(
            &fixture.model,
            &fixture.identity,
            &snapshot,
            &expected,
            &revision(&fixture, 1),
            &[plan],
            CaptureLimits {
                ancestry_steps: 1,
                ..CaptureLimits::default()
            },
        )
        .expect_err("Projection cannot publish membership beyond its cumulative budget")
        .reason,
        FactsUnavailableReason::UnsupportedShape
    );
    assert_eq!(fixture.model.root, before_root);
    assert_eq!(fixture.identity.binding(), &before_binding);
    assert!(
        fixture
            .model
            .modules
            .iter()
            .any(|module| module.path == ":android")
    );
    assert!(fixture.identity.project(":android").is_ok());
    assert!(publisher.committed().is_none());
    Ok(())
}

fn strict_large_task_arguments(
    fixture: &Fixture,
) -> Result<(Value, CaptureContext, StrictKotlinProjectPlan, Vec<String>)> {
    let (mut value, expected, plan) = strict_fixture(fixture)?;
    let arguments = vec![
        "-module-name".into(),
        "Name with space".into(),
        "x".repeat(64 * 1024),
        "-Xsame".into(),
        "-Xsame".into(),
    ];
    let request = plan
        .compiler_arguments
        .get("first-task")
        .context("Independently planned task arguments")?;
    let event = value["kotlinFacts"]["events"]
        .as_array_mut()
        .context("Strict getter events")?
        .iter_mut()
        .find(|event| event["request"] == request.as_str())
        .context("Captured task arguments event")?;
    event["outcome"]["value"]["value"] = json!(&arguments);
    Ok((value, expected, plan, arguments))
}

#[test]
fn strict_unique_task_keeps_large_ordered_and_duplicate_compiler_arguments() -> Result<()> {
    let mut fixture = fixture()?;
    let (value, expected, plan, arguments) = strict_large_task_arguments(&fixture)?;
    let snapshot = parse_kotlin_facts(
        &wire(&value)?,
        &fixture.model,
        &fixture.identity,
        &expected,
        CaptureLimits::default(),
    )?;
    fixture.kotlin = import_kotlin_from_strict_capture(
        &fixture.model,
        &fixture.identity,
        &snapshot,
        &expected,
        &revision(&fixture, 1),
        &[plan],
    )?;
    let mut publisher = ModuleImportPublisher::default();
    publisher.commit(stage(&fixture, &publisher, 1)?, &revision(&fixture, 1))?;
    assert_eq!(
        committed_settings(&publisher)?.compiler_arguments.as_ref(),
        Some(&arguments)
    );
    assert_eq!(
        publisher
            .committed()
            .context("Valid task publication")?
            .modules[":android"]
            .kotlin_capability(),
        KotlinCapability::Enabled
    );
    Ok(())
}

#[test]
fn strict_repeated_task_ids_are_rejected_before_payload_projection() -> Result<()> {
    let fixture = fixture()?;
    let (mut value, expected, plan, _) = strict_large_task_arguments(&fixture)?;
    let event = value["kotlinFacts"]["events"]
        .as_array_mut()
        .context("Strict getter events")?
        .iter_mut()
        .find(|event| event["request"] == plan.task_iteration.as_str())
        .context("Captured official task set")?;
    event["outcome"]["value"]["value"] = json!(vec!["first-task"; 256]);
    let snapshot = parse_kotlin_facts(
        &wire(&value)?,
        &fixture.model,
        &fixture.identity,
        &expected,
        CaptureLimits::default(),
    )?;
    assert_eq!(snapshot.raw_context(), &expected);
    let before_root = fixture.model.root.clone();
    let before_binding = fixture.identity.binding().clone();
    let publisher = ModuleImportPublisher::default();
    let failure = import_kotlin_from_strict_capture(
        &fixture.model,
        &fixture.identity,
        &snapshot,
        &expected,
        &revision(&fixture, 1),
        &[plan],
    )
    .expect_err("The strict decoder accepted the record, but official task sets cannot repeat IDs");
    assert_eq!(failure.reason, FactsUnavailableReason::Malformed);
    assert_eq!(
        failure.detail,
        "Official Project task set repeats a runtime task object"
    );
    assert_eq!(fixture.model.root, before_root);
    assert_eq!(fixture.identity.binding(), &before_binding);
    assert!(fixture.identity.project(":android").is_ok());
    assert!(publisher.committed().is_none());
    Ok(())
}

fn strict_wide_builtin(
    value: &mut Value,
    plan: &mut StrictKotlinProjectPlan,
    distinct_plugins: bool,
    factory_present: bool,
) -> Result<()> {
    set_strict_plugin_ids(value, plan, &["com.android.library"])?;
    set_strict_factory_location(value, "immediate")?;
    add_strict_android_base_predicate(value, plan, true)?;
    let classes = value["kotlinFacts"]["context"]["runtime"]["classes"]
        .as_array_mut()
        .context("Runtime classes")?;
    let template = classes
        .iter()
        .find(|class| class["id"] == "jvm-factory")
        .context("Factory interface")?
        .clone();
    let mut interfaces = Vec::new();
    for index in 0..32 {
        let id = format!("wide-interface-{index}");
        let mut interface = template.clone();
        interface["id"] = json!(id);
        interface["name"] = json!(format!("synthetic.Interface{index}.{}", "x".repeat(8192)));
        classes.push(interface);
        interfaces.push(id);
    }
    if factory_present {
        interfaces.push("jvm-factory".into());
    }
    classes
        .iter_mut()
        .find(|class| class["id"] == "plugin-parent")
        .context("Immediate plugin superclass")?["interfaces"] = json!(interfaces);
    let objects = value["kotlinFacts"]["context"]["objects"]
        .as_array_mut()
        .context("Runtime objects")?;
    let template = objects
        .iter()
        .find(|object| object["id"] == "kotlin-plugin")
        .context("Plugin runtime object")?
        .clone();
    let mut plugins = Vec::new();
    for index in 0..512 {
        if distinct_plugins {
            let id = format!("shared-plugin-{index}");
            let mut plugin = template.clone();
            plugin["id"] = json!(id);
            objects.push(plugin);
            plugins.push(id);
        } else {
            plugins.push("kotlin-plugin".into());
        }
    }
    let events = value["kotlinFacts"]["events"]
        .as_array_mut()
        .context("Strict getter events")?;
    events
        .iter_mut()
        .find(|event| event["request"] == plan.plugin_iteration.as_str())
        .context("Plugin enumeration event")?["outcome"]["value"]["value"] = json!(plugins);
    if !factory_present {
        events
            .iter_mut()
            .find(|event| event["request"] == plan.extension_lookup.as_str())
            .context("Kotlin extension event")?["outcome"]["value"] = Value::Null;
    }
    Ok(())
}

#[test]
fn strict_builtin_shared_metadata_caches_positive_and_negative_predicates() -> Result<()> {
    for distinct_plugins in [false, true] {
        for factory_present in [false, true] {
            let mut fixture = fixture()?;
            let (mut value, _, mut plan) = strict_fixture(&fixture)?;
            strict_wide_builtin(&mut value, &mut plan, distinct_plugins, factory_present)?;
            let expected: CaptureContext =
                serde_json::from_value(value["kotlinFacts"]["context"].clone())?;
            let snapshot = parse_kotlin_facts(
                &wire(&value)?,
                &fixture.model,
                &fixture.identity,
                &expected,
                CaptureLimits::default(),
            )?;
            assert_eq!(snapshot.raw_context(), &expected);
            fixture.kotlin =
                android_tools::module_import::import_kotlin_from_strict_capture_with_limits(
                    &fixture.model,
                    &fixture.identity,
                    &snapshot,
                    &expected,
                    &revision(&fixture, 1),
                    &[plan],
                    CaptureLimits {
                        ancestry_steps: 69,
                        ..CaptureLimits::default()
                    },
                )?;
            let mut publisher = ModuleImportPublisher::default();
            publisher.commit(stage(&fixture, &publisher, 1)?, &revision(&fixture, 1))?;
            let module = &publisher
                .committed()
                .context("Shared plugin metadata publication")?
                .modules[":android"];
            if factory_present {
                assert_eq!(module.kotlin_capability(), KotlinCapability::Enabled);
                assert!(matches!(
                    module.members[1].kotlin,
                    KotlinMemberState::Present(_)
                ));
                assert_eq!(
                    committed_settings(&publisher)?.target_platform.as_deref(),
                    Some("JVM")
                );
            } else {
                assert_eq!(module.kotlin_capability(), KotlinCapability::Disabled);
                assert_eq!(module.members[1].kotlin, KotlinMemberState::Absent);
            }
        }
    }
    Ok(())
}

#[test]
fn strict_builtin_direct_interface_budget_failure_preserves_existing_publication() -> Result<()> {
    let mut fixture = fixture()?;
    let (initial_value, _, initial_plan) = strict_fixture(&fixture)?;
    fixture.kotlin = import_strict_case(&fixture, &initial_value, &initial_plan)?;
    let mut publisher = ModuleImportPublisher::default();
    publisher.commit(stage(&fixture, &publisher, 1)?, &revision(&fixture, 1))?;
    let before = publisher.committed().context("Prior publication")?.clone();
    let before_root = fixture.model.root.clone();
    let before_binding = fixture.identity.binding().clone();
    let issued = revision(&fixture, 2);
    let (mut value, _, mut plan) = strict_fixture(&fixture)?;
    strict_wide_builtin(&mut value, &mut plan, true, true)?;
    let expected: CaptureContext = serde_json::from_value(value["kotlinFacts"]["context"].clone())?;
    let snapshot = parse_kotlin_facts(
        &wire(&value)?,
        &fixture.model,
        &fixture.identity,
        &expected,
        CaptureLimits::default(),
    )?;
    assert_eq!(
        android_tools::module_import::import_kotlin_from_strict_capture_with_limits(
            &fixture.model,
            &fixture.identity,
            &snapshot,
            &expected,
            &issued,
            &[plan],
            CaptureLimits {
                ancestry_steps: 2,
                ..CaptureLimits::default()
            },
        )
        .expect_err("Nonempty direct interfaces cannot exceed the shared work budget")
        .reason,
        FactsUnavailableReason::UnsupportedShape
    );
    assert_eq!(fixture.model.root, before_root);
    assert_eq!(fixture.identity.binding(), &before_binding);
    assert!(fixture.identity.project(":android").is_ok());
    let retained = publisher
        .committed()
        .context("Prior publication retained")?;
    assert_eq!(retained.revision, before.revision);
    assert_eq!(retained.modules, before.modules);
    assert_eq!(
        retained.modules[":android"].kotlin_capability(),
        KotlinCapability::Enabled
    );
    Ok(())
}
