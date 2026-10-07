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

//! Raw official AndroidProject artifact facts, independently bound to a Basic model.
//! These folders have not undergone Studio's light-class or KAPT visibility policy.

use crate::{
    project_model::{ModuleKind, ProjectModel},
    project_tree_facts::{FactsUnavailable, FactsUnavailableReason},
};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
};

const MAX_MODEL_RECORD_BYTES: usize = 16 * 1024 * 1024;

type FactsResult<T> = Result<T, FactsUnavailable>;

// Serde otherwise accepts positional arrays for structs and adjacent-tagged enums.
// Restrict only the root of each wire object; ordinary field decoding stays native.
struct ObjectOnly<D>(D);

impl<'de, D: serde::Deserializer<'de>> serde::Deserializer<'de> for ObjectOnly<D> {
    type Error = D::Error;

    fn deserialize_any<V: serde::de::Visitor<'de>>(
        self,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.deserialize_map(visitor)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct
        map struct enum identifier ignored_any
    }

    fn is_human_readable(&self) -> bool {
        self.0.is_human_readable()
    }
}

macro_rules! object_serde {
    ($($name:ident),+ $(,)?) => {$(
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                $name::deserialize(ObjectOnly(deserializer))
            }
        }
        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                $name::serialize(self, serializer)
            }
        }
    )+};
}

fn unavailable(reason: FactsUnavailableReason, detail: impl Into<String>) -> FactsUnavailable {
    FactsUnavailable {
        reason,
        detail: detail.into(),
        path: None,
    }
}

// BasicModules.kt compares only numeric components. Keeping an explicit comparison
// avoids giving Rust Ord a result inconsistent with description-bearing equality.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self")]
#[serde(deny_unknown_fields)]
pub struct ModelVersion {
    pub major: i32,
    pub minor: i32,
    pub description: Option<String>,
}

impl ModelVersion {
    pub fn compare_version(&self, other: &Self) -> Ordering {
        (self.major, self.minor).cmp(&(other.major, other.minor))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self")]
#[serde(deny_unknown_fields)]
pub struct ModelConsumerVersion {
    pub major: i32,
    pub minor: i32,
    pub description: Option<String>,
}

impl ModelConsumerVersion {
    pub fn compare_version(&self, other: &Self) -> Ordering {
        (self.major, self.minor).cmp(&(other.major, other.minor))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self")]
#[serde(
    tag = "status",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum CapturedField<T> {
    Available(T),
    Unavailable(GetterUnavailable),
    NotApplicable(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self")]
#[serde(deny_unknown_fields)]
pub struct GetterUnavailable {
    pub capability: String,
    pub detail: String,
}

impl<T> CapturedField<T> {
    pub fn available(&self) -> FactsResult<&T> {
        match self {
            Self::Available(value) => Ok(value),
            Self::Unavailable(value) => Err(unavailable(
                FactsUnavailableReason::Capability,
                format!("{}: {}", value.capability, value.detail),
            )),
            Self::NotApplicable(detail) => Err(unavailable(
                FactsUnavailableReason::UnsupportedShape,
                detail,
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactModelVersions {
    pub agp: String,
    pub producer: ModelVersion,
    pub minimum_consumer: CapturedField<ModelConsumerVersion>,
    pub models: Vec<NamedModelVersion>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self")]
#[serde(deny_unknown_fields)]
pub struct NamedModelVersion {
    pub name: String,
    pub version: ModelVersion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeneratedArtifactFeature {
    Assets,
    ClassPaths,
}

impl ArtifactModelVersions {
    /// Adapted from pinned BasicModules.kt ModelFeature's model/legacy AGP ranges.
    pub fn supports(&self, feature: GeneratedArtifactFeature) -> FactsResult<bool> {
        match feature {
            GeneratedArtifactFeature::Assets => {
                Ok((self.producer.major, self.producer.minor) >= (11, 0))
            }
            GeneratedArtifactFeature::ClassPaths => {
                // Validate the retained version even when the producer is newer.
                let agp = parse_agp(&self.agp)?;
                Ok((self.producer.major, self.producer.minor) >= (8, 9)
                    || agp >= ((8, 2, 0), (1, 7)))
            }
        }
    }
}

fn parse_agp(version: &str) -> FactsResult<((u32, u32, u32), (u8, u32))> {
    let malformed = || {
        unavailable(
            FactsUnavailableReason::Malformed,
            format!("Malformed AGP version {version:?}"),
        )
    };
    let mut parts = version.split('-');
    let numeric = parts.next().ok_or_else(malformed)?;
    let qualifier = parts.next();
    if parts.next().is_some() {
        return Err(malformed());
    }
    let mut numbers = numeric.split('.');
    let mut number = || {
        let value = numbers.next().ok_or_else(malformed)?;
        if value.is_empty()
            || (value.len() > 1 && value.starts_with('0'))
            || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(malformed());
        }
        value.parse::<i32>().map_err(|_| malformed())
    };
    let major = number()?;
    let minor = number()?;
    let micro = number()?;
    if numbers.next().is_some() {
        return Err(malformed());
    }
    let preview = match qualifier {
        None => (5, 0),
        Some("dev") => (4, 0),
        Some(value) => {
            let (rank, suffix) = if let Some(value) = value.strip_prefix("alpha") {
                (1, value)
            } else if let Some(value) = value.strip_prefix("beta") {
                (2, value)
            } else if let Some(value) = value.strip_prefix("rc") {
                (3, value)
            } else {
                return Err(unavailable(
                    FactsUnavailableReason::UnsupportedShape,
                    format!("Unproved AGP version qualifier in {version:?}"),
                ));
            };
            // Pinned AgpVersion preserves the historical 3.1.0 beta numbering.
            let two_digit = major > 3
                || (major == 3 && minor > 1)
                || (major == 3 && minor == 1 && micro == 0 && rank != 2);
            if suffix.is_empty()
                || suffix.len() > 2
                || !suffix.bytes().all(|byte| byte.is_ascii_digit())
                || (two_digit && suffix.len() != 2)
                || (!two_digit && suffix.starts_with('0'))
            {
                return Err(malformed());
            }
            (rank, suffix.parse().map_err(|_| malformed())?)
        }
    };
    Ok(((major as u32, minor as u32, micro as u32), preview))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ArtifactKind {
    Android,
    Java,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GeneratedClassPath {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GeneratedArtifact {
    pub kind: ArtifactKind,
    pub generated_source_folders: CapturedField<Vec<PathBuf>>,
    pub generated_resource_folders: CapturedField<Vec<PathBuf>>,
    pub generated_assets_folders: CapturedField<Vec<PathBuf>>,
    pub generated_class_paths: CapturedField<Vec<GeneratedClassPath>>,
    pub classes_folders: CapturedField<Vec<PathBuf>>,
    pub ide_setup_task_names: CapturedField<Vec<String>>,
    pub source_gen_task_name: CapturedField<Option<String>>,
    pub res_gen_task_name: CapturedField<Option<String>>,
}

impl GeneratedArtifact {
    /// Older assets need Studio's unported build-folder fallback, not an empty list.
    pub fn generated_assets(&self, versions: &ArtifactModelVersions) -> FactsResult<&[PathBuf]> {
        if !versions.supports(GeneratedArtifactFeature::Assets)? {
            return Err(unavailable(
                FactsUnavailableReason::Capability,
                "agpV2GeneratedAssets: model producer is below 11.0; legacy fallback is unproved",
            ));
        }
        Ok(self.generated_assets_folders.available()?.as_slice())
    }

    pub fn generated_class_paths(
        &self,
        versions: &ArtifactModelVersions,
    ) -> FactsResult<&[GeneratedClassPath]> {
        if !versions.supports(GeneratedArtifactFeature::ClassPaths)? {
            return Err(unavailable(
                FactsUnavailableReason::Capability,
                "agpV2GeneratedClassPaths: model producer below 8.9 and AGP below 8.2.0-alpha07",
            ));
        }
        Ok(self.generated_class_paths.available()?.as_slice())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self")]
#[serde(
    tag = "status",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum ArtifactSlot {
    Present(GeneratedArtifact),
    Absent,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self")]
#[serde(deny_unknown_fields)]
pub struct NamedArtifact {
    /// Exact official SDK artifact-map key, independent of source scope/name guesses.
    pub artifact: String,
    pub value: ArtifactSlot,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GeneratedVariant {
    pub name: String,
    pub main: CapturedField<ArtifactSlot>,
    pub host_tests: CapturedField<Vec<NamedArtifact>>,
    pub device_tests: CapturedField<Vec<NamedArtifact>>,
    pub fixtures: CapturedField<ArtifactSlot>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GeneratedModule {
    pub module: String,
    pub directory: PathBuf,
    pub versions: CapturedField<ArtifactModelVersions>,
    pub build_folder: CapturedField<PathBuf>,
    pub variants: CapturedField<Vec<GeneratedVariant>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self")]
#[serde(deny_unknown_fields)]
struct GeneratedExport {
    schema: u32,
    root: PathBuf,
    modules: Vec<GeneratedModule>,
}

#[derive(Clone, Debug)]
pub struct GeneratedArtifactSnapshot {
    revision: u64,
    root: PathBuf,
    modules: Vec<GeneratedModule>,
    identity: Vec<(String, PathBuf, Vec<String>)>,
}

impl GeneratedArtifactSnapshot {
    pub fn modules(&self) -> &[GeneratedModule] {
        &self.modules
    }

    pub fn ensure_current(&self, model: &ProjectModel, revision: u64) -> FactsResult<()> {
        if revision != self.revision
            || model.root != self.root
            || model_identity(model) != self.identity
        {
            return Err(unavailable(
                FactsUnavailableReason::Stale,
                "Generated artifact snapshot does not match the current Basic model revision/identity",
            ));
        }
        Ok(())
    }

    pub fn variant(&self, module: &str, variant: &str) -> FactsResult<&GeneratedVariant> {
        let module = self
            .modules
            .iter()
            .find(|value| value.module == module)
            .ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::MissingMetadata,
                    format!("No generated artifact model for module {module:?}"),
                )
            })?;
        module
            .variants
            .available()?
            .iter()
            .find(|value| value.name == variant)
            .ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::MissingVariant,
                    format!(
                        "No generated artifact variant {variant:?} in {}",
                        module.module
                    ),
                )
            })
    }
}

fn model_identity(model: &ProjectModel) -> Vec<(String, PathBuf, Vec<String>)> {
    model
        .modules
        .iter()
        .filter(|module| module.kind != ModuleKind::Jvm)
        .map(|module| {
            (
                module.path.clone(),
                module.directory.clone(),
                module
                    .variants
                    .iter()
                    .map(|variant| variant.name.clone())
                    .collect(),
            )
        })
        .collect()
}

#[derive(Deserialize, Serialize)]
#[serde(remote = "Self")]
struct GeneratedRecord {
    #[serde(
        rename = "generatedArtifacts",
        default,
        deserialize_with = "present_sidecar"
    )]
    generated_artifacts: Option<serde_json::Value>,
}

fn present_sidecar<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<serde_json::Value>, D::Error> {
    serde_json::Value::deserialize(deserializer).map(Some)
}

/// Decode the sidecar independently; failure does not erase the caller's Basic model.
/// Root presence and Studio's visible generated-root conversion remain separate facts.
pub fn parse_generated_artifacts(
    output: &str,
    model: &ProjectModel,
    revision: u64,
    consumer: &ModelConsumerVersion,
) -> FactsResult<GeneratedArtifactSnapshot> {
    let mut records = output
        .lines()
        .filter_map(|line| line.strip_prefix("KODA_ANDROID_PROJECT_MODEL="));
    let record = records.next().ok_or_else(|| {
        unavailable(
            FactsUnavailableReason::MissingMetadata,
            "Gradle returned no Android project model record",
        )
    })?;
    if records.next().is_some() {
        return Err(unavailable(
            FactsUnavailableReason::Malformed,
            "Gradle returned multiple Android project model records",
        ));
    }
    if record.len() > MAX_MODEL_RECORD_BYTES {
        return Err(unavailable(
            FactsUnavailableReason::UnsupportedShape,
            "Generated artifact model record exceeds the 16 MiB decoding limit",
        ));
    }
    let record: GeneratedRecord = serde_json::from_str(record).map_err(|error| {
        unavailable(
            FactsUnavailableReason::Malformed,
            format!("Malformed generated artifact record: {error}"),
        )
    })?;
    let sidecar = record.generated_artifacts.ok_or_else(|| {
        unavailable(
            FactsUnavailableReason::MissingMetadata,
            "generatedArtifacts is absent from the legacy model",
        )
    })?;
    let schema = sidecar
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            unavailable(
                FactsUnavailableReason::Malformed,
                "generatedArtifacts schema is missing or malformed",
            )
        })?;
    if schema != 1 {
        return Err(unavailable(
            FactsUnavailableReason::UnsupportedSchema,
            format!("Unsupported generatedArtifacts schema {schema}"),
        ));
    }
    let export: GeneratedExport = serde_json::from_value(sidecar).map_err(|error| {
        unavailable(
            FactsUnavailableReason::Malformed,
            format!("Malformed generatedArtifacts: {error}"),
        )
    })?;
    if export.root != model.root {
        return Err(unavailable(
            FactsUnavailableReason::Stale,
            "Generated artifacts and Basic model have different project roots",
        ));
    }
    let identity = model_identity(model);
    let actual = export
        .modules
        .iter()
        .map(|module| (&module.module, &module.directory))
        .collect::<Vec<_>>();
    let expected = identity
        .iter()
        .map(|(module, directory, _)| (module, directory))
        .collect::<Vec<_>>();
    if actual != expected {
        return Err(unavailable(
            FactsUnavailableReason::Malformed,
            "Generated artifact module identities/order do not match the Basic model",
        ));
    }
    for (module, expected) in export.modules.iter().zip(&identity) {
        validate_field(&module.versions, "Versions", false)?;
        if let CapturedField::Available(versions) = &module.versions {
            parse_agp(&versions.agp)?;
            if let Some(description) = &versions.producer.description {
                validate_text(description, true, "model producer description")?;
            }
            let mut names = BTreeSet::new();
            for model in &versions.models {
                validate_text(&model.name, false, "Versions model key")?;
                if let Some(description) = &model.version.description {
                    validate_text(description, true, "Versions description")?;
                }
                if !names.insert(&model.name) {
                    return Err(unavailable(
                        FactsUnavailableReason::Malformed,
                        format!("Duplicate Versions model key {:?}", model.name),
                    ));
                }
            }
            if versions
                .models
                .iter()
                .find(|value| value.name == "model_producer")
                .is_none_or(|value| value.version != versions.producer)
            {
                return Err(unavailable(
                    FactsUnavailableReason::Malformed,
                    "Versions.model_producer contradicts the retained producer identity",
                ));
            }
            if !versions
                .models
                .iter()
                .any(|value| value.name == "android_project")
            {
                return Err(unavailable(
                    FactsUnavailableReason::MissingMetadata,
                    "Versions.android_project model schema identity is missing",
                ));
            }

            validate_field(&versions.minimum_consumer, "minimum_model_consumer", false)?;
            let required = versions.minimum_consumer.available()?;
            if let Some(description) = &required.description {
                validate_text(description, true, "minimum consumer description")?;
            }
            if versions
                .models
                .iter()
                .find(|value| value.name == "minimum_model_consumer")
                .is_none_or(|value| {
                    value.version.major != required.major
                        || value.version.minor != required.minor
                        || value.version.description != required.description
                })
            {
                return Err(unavailable(
                    FactsUnavailableReason::Malformed,
                    "Versions.minimum_model_consumer contradicts the retained consumer identity",
                ));
            }
            if required.compare_version(consumer) == Ordering::Greater {
                return Err(unavailable(
                    FactsUnavailableReason::UnsupportedSchema,
                    format!(
                        "{} requires model consumer {}.{}; caller supports {}.{}",
                        module.module,
                        required.major,
                        required.minor,
                        consumer.major,
                        consumer.minor
                    ),
                ));
            }
        }
        validate_field(&module.build_folder, "buildFolder", false)?;
        if let CapturedField::Available(folder) = &module.build_folder {
            validate_path(folder)?;
        }
        validate_field(&module.variants, "AndroidProject.variants", false)?;
        if let CapturedField::Available(variants) = &module.variants {
            let names = variants
                .iter()
                .map(|variant| &variant.name)
                .collect::<BTreeSet<_>>();
            if names.len() != variants.len() || names != expected.2.iter().collect::<BTreeSet<_>>()
            {
                return Err(unavailable(
                    FactsUnavailableReason::MissingVariant,
                    format!(
                        "Generated artifact variant identities differ from Basic model for {}",
                        module.module
                    ),
                ));
            }
            for variant in variants {
                validate_slot_field(&variant.main, "mainArtifact", ArtifactKind::Android)?;
                validate_named(&variant.host_tests, "hostTestArtifacts", ArtifactKind::Java)?;
                validate_named(
                    &variant.device_tests,
                    "deviceTestArtifacts",
                    ArtifactKind::Android,
                )?;
                validate_slot_field(
                    &variant.fixtures,
                    "testFixturesArtifact",
                    ArtifactKind::Android,
                )?;
            }
        }
    }
    Ok(GeneratedArtifactSnapshot {
        revision,
        root: export.root,
        modules: export.modules,
        identity,
    })
}

fn validate_text(value: &str, empty_allowed: bool, field: &str) -> FactsResult<()> {
    if (!empty_allowed && value.is_empty()) || value.chars().any(char::is_control) {
        return Err(unavailable(
            FactsUnavailableReason::Malformed,
            format!("Malformed {field}: {value:?}"),
        ));
    }
    Ok(())
}

fn validate_path(path: &Path) -> FactsResult<()> {
    // Raw official SDK paths may legitimately be outside the module or build folder.
    // Disk existence, trust and VFS ownership are not established by this transport.
    let normalized = path.components().collect::<PathBuf>();
    if normalized.as_os_str() != path.as_os_str()
        || !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        || path
            .to_str()
            .is_none_or(|value| value.chars().any(char::is_control))
    {
        return Err(FactsUnavailable {
            reason: FactsUnavailableReason::Malformed,
            detail: format!(
                "Generated artifact path is not an absolute normalized path: {}",
                path.display()
            ),
            path: Some(path.into()),
        });
    }
    Ok(())
}

fn validate_field<T>(
    field: &CapturedField<T>,
    name: &str,
    allow_not_applicable: bool,
) -> FactsResult<()> {
    match field {
        CapturedField::Available(_) => Ok(()),
        CapturedField::Unavailable(value) => {
            validate_text(&value.capability, false, name)?;
            validate_text(&value.detail, false, name)
        }
        CapturedField::NotApplicable(detail) if allow_not_applicable => {
            validate_text(detail, false, name)
        }
        CapturedField::NotApplicable(_) => Err(unavailable(
            FactsUnavailableReason::Malformed,
            format!("{name} cannot be notApplicable for this artifact/model"),
        )),
    }
}

fn validate_slot_field(
    field: &CapturedField<ArtifactSlot>,
    name: &str,
    kind: ArtifactKind,
) -> FactsResult<()> {
    validate_field(field, name, false)?;
    if let CapturedField::Available(ArtifactSlot::Present(artifact)) = field {
        validate_artifact(artifact, kind)?;
    }
    Ok(())
}

fn validate_named(
    field: &CapturedField<Vec<NamedArtifact>>,
    name: &str,
    kind: ArtifactKind,
) -> FactsResult<()> {
    validate_field(field, name, false)?;
    if let CapturedField::Available(artifacts) = field {
        let mut names = BTreeSet::new();
        for artifact in artifacts {
            validate_text(&artifact.artifact, false, name)?;
            if !names.insert(&artifact.artifact) {
                return Err(unavailable(
                    FactsUnavailableReason::Malformed,
                    format!("Duplicate {name} artifact-map key {:?}", artifact.artifact),
                ));
            }
            if let ArtifactSlot::Present(value) = &artifact.value {
                validate_artifact(value, kind)?;
            }
        }
    }
    Ok(())
}

fn validate_artifact(artifact: &GeneratedArtifact, kind: ArtifactKind) -> FactsResult<()> {
    if artifact.kind != kind {
        return Err(unavailable(
            FactsUnavailableReason::Malformed,
            "Artifact kind contradicts the SDK container type",
        ));
    }
    for (name, field, android_only) in [
        (
            "generatedSourceFolders",
            &artifact.generated_source_folders,
            false,
        ),
        (
            "generatedResourceFolders",
            &artifact.generated_resource_folders,
            true,
        ),
        (
            "generatedAssetsFolders",
            &artifact.generated_assets_folders,
            true,
        ),
        ("classesFolders", &artifact.classes_folders, false),
    ] {
        validate_field(field, name, android_only && kind == ArtifactKind::Java)?;
        if android_only
            && kind == ArtifactKind::Java
            && !matches!(field, CapturedField::NotApplicable(_))
        {
            return Err(unavailable(
                FactsUnavailableReason::Malformed,
                format!("JavaArtifact does not declare {name}"),
            ));
        }
        if let CapturedField::Available(paths) = field {
            for path in paths {
                validate_path(path)?;
            }
        }
    }
    validate_field(
        &artifact.generated_class_paths,
        "generatedClassPaths",
        false,
    )?;
    if let CapturedField::Available(paths) = &artifact.generated_class_paths {
        let mut names = BTreeSet::new();
        for value in paths {
            validate_text(&value.name, false, "generatedClassPaths key")?;
            if !names.insert(&value.name) {
                return Err(unavailable(
                    FactsUnavailableReason::Malformed,
                    format!("Duplicate generatedClassPaths map key {:?}", value.name),
                ));
            }
            validate_path(&value.path)?;
        }
    }
    validate_field(&artifact.ide_setup_task_names, "ideSetupTaskNames", false)?;
    if let CapturedField::Available(tasks) = &artifact.ide_setup_task_names {
        for task in tasks {
            validate_text(task, false, "ideSetupTaskNames")?;
        }
    }
    for (name, field) in [
        ("sourceGenTaskName", &artifact.source_gen_task_name),
        ("resGenTaskName", &artifact.res_gen_task_name),
    ] {
        validate_field(field, name, kind == ArtifactKind::Java)?;
        if kind == ArtifactKind::Java && !matches!(field, CapturedField::NotApplicable(_)) {
            return Err(unavailable(
                FactsUnavailableReason::Malformed,
                format!("JavaArtifact does not declare {name}"),
            ));
        }
        if let CapturedField::Available(Some(task)) = field {
            validate_text(task, false, name)?;
        }
    }
    Ok(())
}

object_serde!(
    ModelVersion,
    ModelConsumerVersion,
    GetterUnavailable,
    ArtifactModelVersions,
    NamedModelVersion,
    GeneratedClassPath,
    GeneratedArtifact,
    ArtifactSlot,
    NamedArtifact,
    GeneratedVariant,
    GeneratedModule,
    GeneratedExport,
    GeneratedRecord
);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for CapturedField<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        CapturedField::<T>::deserialize(ObjectOnly(deserializer))
    }
}

impl<T: Serialize> Serialize for CapturedField<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        CapturedField::<T>::serialize(self, serializer)
    }
}
