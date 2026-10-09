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
