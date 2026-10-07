/*
 * Copyright (C) 2024 The Android Open Source Project
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use android_tools::{
    generated_artifacts::{
        ArtifactModelVersions, ArtifactSlot, CapturedField, GeneratedArtifactFeature,
        GeneratedArtifactSnapshot, ModelConsumerVersion, ModelVersion, parse_generated_artifacts,
    },
    project_model::{ProjectModel, parse_model},
    project_tree_facts::FactsUnavailableReason,
};
use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use std::path::Path;

#[test]
fn check_model_version_ordering() {
    let ordered = [
        (i32::MIN, i32::MIN, "z"),
        (0, 0, "y"),
        (0, 1, "z"),
        (0, 2, "w"),
        (1, 0, "v"),
        (1, 1, "u"),
        (1, 2, "t"),
        (2, 0, "s"),
        (i32::MAX, i32::MAX, "a"),
    ]
    .map(|(major, minor, description)| ModelVersion {
        major,
        minor,
        description: Some(description.into()),
    });
    let mut actual = ordered.iter().rev().cloned().collect::<Vec<_>>();
    actual.sort_by(ModelVersion::compare_version);
    assert_eq!(actual, ordered);
}

#[test]
fn check_model_consumer_version_ordering() {
    let ordered = [
        (i32::MIN, i32::MIN, "z"),
        (0, 0, "y"),
        (0, 1, "z"),
        (0, 2, "w"),
        (1, 0, "v"),
        (1, 1, "u"),
        (1, 2, "t"),
        (2, 0, "s"),
        (i32::MAX, i32::MAX, "a"),
    ]
    .map(|(major, minor, description)| ModelConsumerVersion {
        major,
        minor,
        description: Some(description.into()),
    });
    let mut actual = ordered.iter().rev().cloned().collect::<Vec<_>>();
    actual.sort_by(ModelConsumerVersion::compare_version);
    assert_eq!(actual, ordered);
}

fn versions(major: i32, minor: i32, agp: &str) -> ArtifactModelVersions {
    ArtifactModelVersions {
        agp: agp.into(),
        producer: ModelVersion {
            major,
            minor,
            description: None,
        },
        minimum_consumer: CapturedField::Available(consumer()),
        models: Vec::new(),
    }
}

fn consumer() -> ModelConsumerVersion {
    ModelConsumerVersion {
        major: 66,
        minor: 1,
        description: Some("test consumer".into()),
    }
}

#[test]
fn descriptions_remain_identity_but_do_not_change_numeric_comparison() {
    let left = ModelVersion {
        major: 1,
        minor: 0,
        description: Some("z".into()),
    };
    let right = ModelVersion {
        major: 1,
        minor: 0,
        description: Some("a".into()),
    };
    assert_ne!(left, right);
    assert_eq!(left.compare_version(&right), std::cmp::Ordering::Equal);
    let left = ModelConsumerVersion {
        major: 1,
        minor: 0,
        description: Some("z".into()),
    };
    let right = ModelConsumerVersion {
        major: 1,
        minor: 0,
        description: Some("a".into()),
    };
    assert_ne!(left, right);
    assert_eq!(left.compare_version(&right), std::cmp::Ordering::Equal);
}

#[test]
fn assets_use_producer_eleven_independently_of_agp_major() -> Result<()> {
    for (major, minor, agp, supported) in [
        (10, i32::MAX, "99.0.0", false),
        (11, 0, "1.0.0", true),
        (11, 1, "1.0.0", true),
        (i32::MIN, i32::MAX, "99.0.0", false),
    ] {
        assert_eq!(
            versions(major, minor, agp).supports(GeneratedArtifactFeature::Assets)?,
            supported
        );
    }
    Ok(())
}

#[test]
fn classpaths_use_model_or_exact_legacy_agp_boundary() -> Result<()> {
    for (major, minor, agp, supported) in [
        (8, 8, "8.2.0-alpha06", false),
        (8, 8, "8.2.0-alpha07", true),
        (8, 8, "8.2.0-beta01", true),
        (8, 8, "8.2.0-rc01", true),
        (8, 8, "8.2.0", true),
        (8, 8, "8.2.0-dev", false),
        (8, 9, "1.0.0", true),
        (9, 0, "1.0.0", true),
        (i32::MIN, i32::MIN, "8.2.0-alpha07", true),
        (8, 8, "8.1.99", false),
        (8, 8, "8.3.0-dev", true),
    ] {
        assert_eq!(
            versions(major, minor, agp).supports(GeneratedArtifactFeature::ClassPaths)?,
            supported,
            "producer {major}.{minor}, AGP {agp}"
        );
    }
    Ok(())
}

#[test]
fn malformed_and_unproved_agp_versions_are_typed_even_with_new_model() {
    for agp in [
        "",
        "8.2",
        "8.2.0-alpha",
        "8.2.0-alpha07-extra",
        "8.2.0\n",
        "-8.2.0",
        "8.x.0",
    ] {
        assert_eq!(
            versions(23, 0, agp)
                .supports(GeneratedArtifactFeature::ClassPaths)
                .unwrap_err()
                .reason,
            FactsUnavailableReason::Malformed,
            "{agp}"
        );
    }
    assert_eq!(
        versions(23, 0, "8.2.0-snapshot")
            .supports(GeneratedArtifactFeature::ClassPaths)
            .unwrap_err()
            .reason,
        FactsUnavailableReason::UnsupportedShape
    );
}

fn field(value: Value) -> Value {
    json!({"status":"available", "value":value})
}
fn absent() -> Value {
    json!({"status":"absent"})
}
fn artifact(root: &Path, java: bool) -> Value {
    let not_applicable = |name: &str| json!({"status":"notApplicable","value":format!("JavaArtifact does not declare {name}")});
    json!({"status":"present","value":{
        "kind":if java { "java" } else { "android" },
        "generatedSourceFolders":field(json!([root.join("outside-build/generated-java"), root.join("build/ap")])),
        "generatedResourceFolders":if java {not_applicable("generatedResourceFolders")} else {field(json!([root.join("outside-build/res")]))},
        "generatedAssetsFolders":if java {not_applicable("generatedAssetsFolders")} else {field(json!([root.join("outside-build/assets")]))},
        "generatedClassPaths":field(json!([{"name":"buildConfigGeneratedClasses","path":root.join("build/classes/BuildConfig.jar")}])) ,
        "classesFolders":field(json!([])),"ideSetupTaskNames":field(json!(["produceOutsideBuild"])),
        "sourceGenTaskName":if java {not_applicable("sourceGenTaskName")} else {field(json!("generateOfficialSources"))},
        "resGenTaskName":if java {not_applicable("resGenTaskName")} else {field(Value::Null)}
    }})
}

fn fixture() -> Result<(tempfile::TempDir, ProjectModel, Value)> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let versions = json!({"agp":"9.4.0","producer":{"major":23,"minor":0,"description":null},
        "minimumConsumer":field(json!({"major":66,"minor":1,"description":"Rabbit"})),
        "models":[{"name":"model_producer","version":{"major":23,"minor":0,"description":null}},
          {"name":"android_project","version":{"major":0,"minor":1,"description":null}},
          {"name":"minimum_model_consumer","version":{"major":66,"minor":1,"description":"Rabbit"}}]});
    let variant = |name: &str| {
        json!({"name":name,"main":field(artifact(&root,false)),
        "hostTests":field(json!([{"artifact":"_unit_test_","value":artifact(&root,true)}])),
        "deviceTests":field(json!([{"artifact":"_android_test_","value":artifact(&root,false)}])),
        "fixtures":field(absent())})
    };
    let value = json!({"version":1,"root":root,"diagnostics":[],"modules":[{
        "path":":app","directory":root,"namespace":"dev.sample","kind":"application",
        "variants":[{"name":"debug","components":[{"name":"debug","scope":"main","sources":[],"dependencies":[]}],"outputListing":null},
          {"name":"release","components":[{"name":"release","scope":"main","sources":[],"dependencies":[]}],"outputListing":null}]}],
        "generatedArtifacts":{"schema":1,"root":root,"modules":[{"module":":app","directory":root,
            "versions":field(versions),"buildFolder":field(json!(root.join("build"))),
            "variants":field(json!([variant("debug"),variant("release")]))}]}});
    let model = parse_model(&format!("KODA_ANDROID_PROJECT_MODEL={value}\n"), &root)?;
    Ok((directory, model, value))
}

fn parse(
    value: &Value,
    model: &ProjectModel,
) -> Result<GeneratedArtifactSnapshot, android_tools::project_tree_facts::FactsUnavailable> {
    parse_generated_artifacts(
        &format!("KODA_ANDROID_PROJECT_MODEL={value}\n"),
        model,
        4,
        &consumer(),
    )
}

fn main_artifact(
    snapshot: &GeneratedArtifactSnapshot,
) -> Result<&android_tools::generated_artifacts::GeneratedArtifact> {
    let ArtifactSlot::Present(artifact) = snapshot.variant(":app", "debug")?.main.available()?
    else {
        anyhow::bail!("Expected official main artifact");
    };
    Ok(artifact)
}

#[test]
fn official_raw_artifacts_keep_classpaths_distinct_and_outside_build_roots() -> Result<()> {
    let (_directory, model, value) = fixture()?;
    let snapshot = parse(&value, &model)?;
    snapshot.ensure_current(&model, 4)?;
    let artifact = main_artifact(&snapshot)?;
    assert_eq!(
        artifact.generated_source_folders.available()?,
        &[
            model.root.join("outside-build/generated-java"),
            model.root.join("build/ap")
        ]
    );
    assert_eq!(
        artifact.generated_class_paths.available()?[0].name,
        "buildConfigGeneratedClasses"
    );
    assert!(
        !artifact
            .generated_source_folders
            .available()?
            .contains(&model.root.join("build/classes/BuildConfig.jar"))
    );
    assert_eq!(
        artifact.generated_resource_folders.available()?,
        &[model.root.join("outside-build/res")]
    );
    assert_eq!(
        artifact.generated_assets_folders.available()?,
        &[model.root.join("outside-build/assets")]
    );
    Ok(())
}

#[test]
fn available_empty_absent_artifact_and_unavailable_getter_stay_distinct() -> Result<()> {
    let (_directory, model, mut value) = fixture()?;
    value["generatedArtifacts"]["modules"][0]["variants"]["value"][0]["main"]["value"]["value"]["generatedSourceFolders"] =
        field(json!([]));
    value["generatedArtifacts"]["modules"][0]["variants"]["value"][0]["main"]["value"]["value"]["generatedAssetsFolders"] = json!({"status":"unavailable","value":{"capability":"AndroidProject.generatedAssetsFolders","detail":"Getter unavailable"}});
    let snapshot = parse(&value, &model)?;
    let artifact = main_artifact(&snapshot)?;
    assert!(artifact.generated_source_folders.available()?.is_empty());
    assert!(matches!(
        snapshot.variant(":app", "debug")?.fixtures.available()?,
        ArtifactSlot::Absent
    ));
    assert_eq!(
        artifact
            .generated_assets_folders
            .available()
            .unwrap_err()
            .reason,
        FactsUnavailableReason::Capability
    );
    Ok(())
}

#[test]
fn raw_paths_preserve_duplicates_and_sdk_encounter_order() -> Result<()> {
    let (_directory, model, mut value) = fixture()?;
    let expected = vec![
        model.root.join("z"),
        model.root.join("a"),
        model.root.join("z"),
    ];
    value["generatedArtifacts"]["modules"][0]["variants"]["value"][0]["main"]["value"]["value"]["generatedSourceFolders"] =
        field(json!(expected));
    assert_eq!(
        main_artifact(&parse(&value, &model)?)?
            .generated_source_folders
            .available()?,
        &expected
    );
    Ok(())
}

#[test]
fn variant_encounter_order_is_retained_without_guessing_selected_variant() -> Result<()> {
    let (_directory, model, mut value) = fixture()?;
    value["generatedArtifacts"]["modules"][0]["variants"]["value"]
        .as_array_mut()
        .context("variants")?
        .reverse();
    let snapshot = parse(&value, &model)?;
    assert_eq!(
        snapshot.modules()[0].variants.available()?[0].name,
        "release"
    );
    assert_eq!(snapshot.variant(":app", "debug")?.name, "debug");
    assert_eq!(
        snapshot
            .variant(":app", "debugUnitTest")
            .unwrap_err()
            .reason,
        FactsUnavailableReason::MissingVariant
    );
    Ok(())
}

#[test]
fn legacy_import_is_unchanged_and_missing_sidecar_is_explicit() -> Result<()> {
    let (_directory, model, mut value) = fixture()?;
    value
        .as_object_mut()
        .context("object")?
        .remove("generatedArtifacts");
    let parsed = parse_model(
        &format!("KODA_ANDROID_PROJECT_MODEL={value}\n"),
        &model.root,
    )?;
    assert_eq!(parsed.modules[0].variants.len(), 2);
    assert_eq!(
        parse(&value, &model).unwrap_err().reason,
        FactsUnavailableReason::MissingMetadata
    );
    Ok(())
}

#[test]
fn unavailable_android_project_preserves_versions_and_basic_import() -> Result<()> {
    let (_directory, model, mut value) = fixture()?;
    value["generatedArtifacts"]["modules"][0]["variants"] = json!({"status":"unavailable","value":{"capability":"AndroidProject","detail":"Official builder unavailable"}});
    let snapshot = parse(&value, &model)?;
    assert_eq!(
        snapshot.modules()[0].versions.available()?.producer.major,
        23
    );
    assert_eq!(
        snapshot.variant(":app", "debug").unwrap_err().reason,
        FactsUnavailableReason::Capability
    );
    assert_eq!(
        parse_model(
            &format!("KODA_ANDROID_PROJECT_MODEL={value}\n"),
            &model.root
        )?
        .modules
        .len(),
        1
    );
    Ok(())
}

#[test]
fn snapshot_rejects_revision_or_model_identity_reuse() -> Result<()> {
    let (_directory, mut model, value) = fixture()?;
    let snapshot = parse(&value, &model)?;
    assert_eq!(
        snapshot.ensure_current(&model, 5).unwrap_err().reason,
        FactsUnavailableReason::Stale
    );
    model.modules[0].variants[0].name = "other".into();
    assert_eq!(
        snapshot.ensure_current(&model, 4).unwrap_err().reason,
        FactsUnavailableReason::Stale
    );
    Ok(())
}

#[test]
fn unsupported_schema_and_root_are_explicit() -> Result<()> {
    let (_directory, model, value) = fixture()?;
    let mut invalid = value.clone();
    invalid["generatedArtifacts"]["schema"] = json!(2);
    assert_eq!(
        parse(&invalid, &model).unwrap_err().reason,
        FactsUnavailableReason::UnsupportedSchema
    );
    let mut invalid = value;
    invalid["generatedArtifacts"]["root"] = json!(model.root.join("other"));
    assert_eq!(
        parse(&invalid, &model).unwrap_err().reason,
        FactsUnavailableReason::Stale
    );
    Ok(())
}

#[test]
fn module_and_variant_identity_contradictions_are_rejected() -> Result<()> {
    let (_directory, model, value) = fixture()?;
    let mut invalid = value.clone();
    invalid["generatedArtifacts"]["modules"][0]["module"] = json!(":other");
    assert_eq!(
        parse(&invalid, &model).unwrap_err().reason,
        FactsUnavailableReason::Malformed
    );
    let mut invalid = value.clone();
    invalid["generatedArtifacts"]["modules"][0]["directory"] = json!(model.root.join("other"));
    assert_eq!(
        parse(&invalid, &model).unwrap_err().reason,
        FactsUnavailableReason::Malformed
    );
    let mut invalid = value;
    invalid["generatedArtifacts"]["modules"][0]["variants"]["value"][1]["name"] = json!("debug");
    assert_eq!(
        parse(&invalid, &model).unwrap_err().reason,
        FactsUnavailableReason::MissingVariant
    );
    Ok(())
}

#[test]
fn malformed_paths_and_artifact_shapes_never_become_empty() -> Result<()> {
    let (_directory, model, value) = fixture()?;
    for path in [
        "relative/generated",
        "/tmp/../escape",
        "/tmp/./escape",
        "/tmp//escape",
        "/tmp/invalid\nroot",
    ] {
        let mut invalid = value.clone();
        invalid["generatedArtifacts"]["modules"][0]["variants"]["value"][0]["main"]["value"]["value"]
            ["generatedSourceFolders"] = field(json!([path]));
        assert_eq!(
            parse(&invalid, &model).unwrap_err().reason,
            FactsUnavailableReason::Malformed,
            "{path}"
        );
    }
    let mut invalid = value.clone();
    invalid["generatedArtifacts"]["modules"][0]["variants"]["value"][0]["hostTests"]["value"][0]
        ["value"]["value"]["kind"] = json!("android");
    assert_eq!(
        parse(&invalid, &model).unwrap_err().reason,
        FactsUnavailableReason::Malformed
    );
    let mut invalid = value;
    invalid["generatedArtifacts"]["modules"][0]["variants"]["value"][0]["main"]["value"]["value"]
        .as_object_mut()
        .context("artifact")?
        .remove("generatedSourceFolders");
    assert_eq!(
        parse(&invalid, &model).unwrap_err().reason,
        FactsUnavailableReason::Malformed
    );
    Ok(())
}

#[test]
fn java_artifact_android_fields_are_explicitly_not_applicable() -> Result<()> {
    let (_directory, model, mut value) = fixture()?;
    let snapshot = parse(&value, &model)?;
    let ArtifactSlot::Present(host) =
        &snapshot.variant(":app", "debug")?.host_tests.available()?[0].value
    else {
        anyhow::bail!("host")
    };
    assert_eq!(
        host.generated_resource_folders
            .available()
            .unwrap_err()
            .reason,
        FactsUnavailableReason::UnsupportedShape
    );
    value["generatedArtifacts"]["modules"][0]["variants"]["value"][0]["hostTests"]["value"][0]["value"]
        ["value"]["generatedResourceFolders"] = field(json!([]));
    assert_eq!(
        parse(&value, &model).unwrap_err().reason,
        FactsUnavailableReason::Malformed
    );
    Ok(())
}

#[test]
fn duplicate_sdk_map_keys_are_rejected_without_deduplicating_raw_paths() -> Result<()> {
    let (_directory, model, value) = fixture()?;
    let mut invalid = value.clone();
    let hosts =
        invalid["generatedArtifacts"]["modules"][0]["variants"]["value"][0]["hostTests"]["value"]
            .as_array_mut()
            .context("hosts")?;
    hosts.push(hosts[0].clone());
    assert_eq!(
        parse(&invalid, &model).unwrap_err().reason,
        FactsUnavailableReason::Malformed
    );
    let mut invalid = value;
    let artifact = &mut invalid["generatedArtifacts"]["modules"][0]["variants"]["value"][0]["main"]
        ["value"]["value"];
    let paths = artifact["generatedClassPaths"]["value"]
        .as_array_mut()
        .context("paths")?;
    paths.push(paths[0].clone());
    assert_eq!(
        parse(&invalid, &model).unwrap_err().reason,
        FactsUnavailableReason::Malformed
    );
    Ok(())
}

#[test]
fn model_versions_retain_descriptions_and_enforce_consumer_compatibility() -> Result<()> {
    let (_directory, model, value) = fixture()?;
    let snapshot = parse(&value, &model)?;
    let versions = snapshot.modules()[0].versions.available()?;
    assert_eq!(versions.producer.description, None);
    assert_eq!(
        versions
            .minimum_consumer
            .available()?
            .description
            .as_deref(),
        Some("Rabbit")
    );
    let old = ModelConsumerVersion {
        major: 66,
        minor: 0,
        description: None,
    };
    assert_eq!(
        parse_generated_artifacts(
            &format!("KODA_ANDROID_PROJECT_MODEL={value}\n"),
            &model,
            4,
            &old
        )
        .unwrap_err()
        .reason,
        FactsUnavailableReason::UnsupportedSchema
    );
    Ok(())
}

#[test]
fn contradictory_producer_consumer_or_model_schema_identity_is_rejected() -> Result<()> {
    let (_directory, model, value) = fixture()?;
    let mut invalid = value.clone();
    invalid["generatedArtifacts"]["modules"][0]["versions"]["value"]["producer"]["major"] =
        json!(24);
    assert_eq!(
        parse(&invalid, &model).unwrap_err().reason,
        FactsUnavailableReason::Malformed
    );
    let mut invalid = value.clone();
    invalid["generatedArtifacts"]["modules"][0]["versions"]["value"]["minimumConsumer"]["value"]
        ["minor"] = json!(0);
    assert_eq!(
        parse(&invalid, &model).unwrap_err().reason,
        FactsUnavailableReason::Malformed
    );
    let mut invalid = value;
    invalid["generatedArtifacts"]["modules"][0]["versions"]["value"]["models"][1]["name"] =
        json!("unknown");
    assert_eq!(
        parse(&invalid, &model).unwrap_err().reason,
        FactsUnavailableReason::MissingMetadata
    );
    Ok(())
}

#[test]
fn unavailable_minimum_consumer_never_claims_model_compatibility() -> Result<()> {
    let (_directory, model, mut value) = fixture()?;
    value["generatedArtifacts"]["modules"][0]["versions"]["value"]["minimumConsumer"] = json!({"status":"unavailable","value":{"capability":"minimum_model_consumer","detail":"Metadata missing"}});
    assert_eq!(
        parse(&value, &model).unwrap_err().reason,
        FactsUnavailableReason::Capability
    );
    Ok(())
}

#[test]
fn older_model_retains_raw_assets_but_requires_explicit_legacy_policy() -> Result<()> {
    let (_directory, model, value) = fixture()?;
    let snapshot = parse(&value, &model)?;
    let artifact = main_artifact(&snapshot)?;
    assert!(!artifact.generated_assets_folders.available()?.is_empty());
    assert_eq!(
        artifact
            .generated_assets(&versions(10, 9, "99.0.0"))
            .unwrap_err()
            .reason,
        FactsUnavailableReason::Capability
    );
    assert_eq!(
        artifact
            .generated_class_paths(&versions(8, 8, "8.2.0-alpha06"))
            .unwrap_err()
            .reason,
        FactsUnavailableReason::Capability
    );
    assert_eq!(
        artifact.generated_assets(&versions(11, 0, "1.0.0"))?.len(),
        1
    );
    assert_eq!(
        artifact
            .generated_class_paths(&versions(8, 9, "1.0.0"))?
            .len(),
        1
    );
    Ok(())
}

#[test]
fn malformed_transport_multiple_records_unknown_fields_and_statuses_are_typed() -> Result<()> {
    let (_directory, model, value) = fixture()?;
    for output in [
        "KODA_ANDROID_PROJECT_MODEL={".to_owned(),
        format!("KODA_ANDROID_PROJECT_MODEL={value}\nKODA_ANDROID_PROJECT_MODEL={value}\n"),
    ] {
        assert_eq!(
            parse_generated_artifacts(&output, &model, 4, &consumer())
                .unwrap_err()
                .reason,
            FactsUnavailableReason::Malformed
        );
    }
    let mut invalid = value.clone();
    invalid["generatedArtifacts"]["extra"] = json!(true);
    assert_eq!(
        parse(&invalid, &model).unwrap_err().reason,
        FactsUnavailableReason::Malformed
    );
    let mut invalid = value;
    invalid["generatedArtifacts"]["modules"][0]["buildFolder"]["status"] = json!("empty");
    assert_eq!(
        parse(&invalid, &model).unwrap_err().reason,
        FactsUnavailableReason::Malformed
    );
    Ok(())
}

#[test]
fn oversized_transport_is_rejected_before_json_materialization() -> Result<()> {
    let (_directory, model, _) = fixture()?;
    let output = format!(
        "KODA_ANDROID_PROJECT_MODEL={}\n",
        "x".repeat(16 * 1024 * 1024 + 1)
    );
    assert_eq!(
        parse_generated_artifacts(&output, &model, 4, &consumer())
            .unwrap_err()
            .reason,
        FactsUnavailableReason::UnsupportedShape
    );
    Ok(())
}

#[test]
fn null_collection_and_null_sidecar_are_malformed_while_empty_is_available() -> Result<()> {
    let (_directory, model, value) = fixture()?;
    let mut invalid = value.clone();
    invalid.pointer_mut("/generatedArtifacts/modules/0/variants/value/0/main/value/value/generatedSourceFolders")
        .context("generatedSourceFolders")?.clone_from(&field(Value::Null));
    assert_eq!(
        parse(&invalid, &model).unwrap_err().reason,
        FactsUnavailableReason::Malformed
    );
    invalid.pointer_mut("/generatedArtifacts/modules/0/variants/value/0/main/value/value/generatedSourceFolders")
        .context("generatedSourceFolders")?.clone_from(&field(json!([])));
    assert!(
        main_artifact(&parse(&invalid, &model)?)?
            .generated_source_folders
            .available()?
            .is_empty()
    );
    let mut invalid = value;
    invalid["generatedArtifacts"] = Value::Null;
    assert_eq!(
        parse(&invalid, &model).unwrap_err().reason,
        FactsUnavailableReason::Malformed
    );
    Ok(())
}

#[test]
fn java_artifact_task_getters_require_not_applicable_even_for_null_or_failure() -> Result<()> {
    let (_directory, model, value) = fixture()?;
    for name in ["sourceGenTaskName", "resGenTaskName"] {
        for field in [
            field(Value::Null),
            json!({"status":"unavailable","value":{
            "capability":"JavaArtifact.task","detail":"Getter unavailable"}}),
        ] {
            let mut invalid = value.clone();
            let artifact = invalid
                .pointer_mut(
                    "/generatedArtifacts/modules/0/variants/value/0/hostTests/value/0/value/value",
                )
                .context("host artifact")?;
            artifact[name] = field;
            assert_eq!(
                parse(&invalid, &model).unwrap_err().reason,
                FactsUnavailableReason::Malformed
            );
        }
    }
    Ok(())
}

#[test]
fn unrelated_basic_record_payload_does_not_change_decoded_artifact_facts() -> Result<()> {
    let (_directory, model, mut value) = fixture()?;
    let expected = serde_json::to_value(parse(&value, &model)?.modules())?;
    value["largeUnrelatedBasicPayload"] = json!(["unused".repeat(100_000), {"deep":[1,2,3]}]);
    assert_eq!(
        serde_json::to_value(parse(&value, &model)?.modules())?,
        expected
    );
    Ok(())
}

#[test]
fn positional_envelope_nested_dtos_and_tagged_fields_are_malformed() -> Result<()> {
    let (_directory, model, value) = fixture()?;
    let envelope = format!(
        "KODA_ANDROID_PROJECT_MODEL={}\n",
        json!([value["generatedArtifacts"]])
    );
    assert_eq!(
        parse_generated_artifacts(&envelope, &model, 4, &consumer())
            .unwrap_err()
            .reason,
        FactsUnavailableReason::Malformed
    );
    for (pointer, replacement) in [
        (
            "/generatedArtifacts/modules/0/versions/value/producer",
            json!([23, 0, null]),
        ),
        (
            "/generatedArtifacts/modules/0/versions/value/minimumConsumer/value",
            json!([66, 1, "Rabbit"]),
        ),
        (
            "/generatedArtifacts/modules/0/versions/value/models/1",
            json!(["android_project", {"major":0,"minor":1,"description":null}]),
        ),
        (
            "/generatedArtifacts/modules/0/buildFolder",
            json!(["available", model.root.join("build")]),
        ),
        (
            "/generatedArtifacts/modules/0/variants/value/0/main/value/value/generatedSourceFolders",
            json!(["available", []]),
        ),
        (
            "/generatedArtifacts/modules/0/variants/value/0/main/value",
            json!(["absent"]),
        ),
        (
            "/generatedArtifacts/modules/0/variants/value/0/main/value",
            json!(["absent", null]),
        ),
        (
            "/generatedArtifacts/modules/0/variants/value/0/main/value/value/generatedClassPaths/value/0",
            json!([
                "buildConfigGeneratedClasses",
                model.root.join("BuildConfig.jar")
            ]),
        ),
    ] {
        let mut invalid = value.clone();
        invalid
            .pointer_mut(pointer)
            .context("wire object")?
            .clone_from(&replacement);
        assert_eq!(
            parse(&invalid, &model).unwrap_err().reason,
            FactsUnavailableReason::Malformed,
            "{pointer}"
        );
    }
    Ok(())
}
