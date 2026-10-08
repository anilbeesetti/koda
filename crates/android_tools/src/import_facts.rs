//! Official Gradle identity inputs, without imported-name or Kotlin facet policy.
//! Getter boundaries follow the attributed Apache-2.0 sources retained in
//! `test_data/import_facts`; original naming/facet tests are not credited here.

use crate::{
    generated_artifacts::CapturedField,
    project_model::{ModuleKind, ProjectModel, VariantId},
    project_tree_facts::{FactsUnavailable, FactsUnavailableReason},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

const MAX_MODEL_RECORD_BYTES: usize = 16 * 1024 * 1024;
const PROJECT_NAME: &str = "org.gradle.api.Project.getName()";
const PROJECT_PATH: &str = "org.gradle.api.Project.getPath()";
const PROJECT_DIRECTORY: &str = "org.gradle.api.Project.getProjectDir().getCanonicalPath()";
const ROOT_NAME: &str = "org.gradle.api.Project.getRootProject().getName()";
const ROOT_DIRECTORY: &str = "org.gradle.api.Project.getRootDir().getCanonicalPath()";
const PARENT_PATH: &str = "org.gradle.api.Project.getParent()?.getPath()";
const BUILD_TREE_PATH: &str = "org.gradle.api.Project.getBuildTreePath()";
const REFERENCE_IDENTITY_PATH: &str =
    "org.gradle.api.internal.project.ProjectInternal.getIdentityPath().getPath()";
const IDEA_PLUGIN: &str =
    "org.gradle.api.plugins.PluginContainer.findPlugin(org.gradle.plugins.ide.idea.IdeaPlugin.class)";
const IDEA_NAME: &str = "org.gradle.plugins.ide.idea.IdeaPlugin.getModel()?.getModule()?.getName()";
const PROJECT_CATALOGUE: &str = "org.gradle.api.Project.getAllprojects()";

type FactsResult<T> = Result<T, FactsUnavailable>;

struct ObjectOnly<D>(D);

impl<'de, D: serde::Deserializer<'de>> serde::Deserializer<'de> for ObjectOnly<D> {
    type Error = D::Error;

    fn deserialize_any<V: serde::de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
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
    FactsUnavailable { reason, detail: detail.into(), path: None }
}

/// A getter invocation under the snapshot's observed Gradle version.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", deny_unknown_fields, bound(deserialize = "T: Deserialize<'de>"))]
pub struct GetterObservation<T> {
    pub getter: String,
    #[serde(deserialize_with = "captured_result")]
    pub result: CapturedField<T>,
}

impl<T> GetterObservation<T> {
    pub fn available(&self) -> FactsResult<&T> {
        self.result.available()
    }
}

fn captured_result<'de, D, T>(deserializer: D) -> Result<CapturedField<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    if !value.is_object() || value.get("value").is_none() {
        return Err(serde::de::Error::custom("Getter result must be an object with an explicit value"));
    }
    CapturedField::deserialize(value).map_err(serde::de::Error::custom)
}

fn present_observation<'de, D, T>(deserializer: D) -> Result<Option<GetterObservation<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    GetterObservation::deserialize(deserializer).map(Some)
}

/// Missing fields are absent observations, rather than successful nullable values.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", rename_all = "camelCase", deny_unknown_fields)]
pub struct RawImportProject {
    #[serde(default, deserialize_with = "present_observation", skip_serializing_if = "Option::is_none")]
    pub project_name: Option<GetterObservation<String>>,
    #[serde(default, deserialize_with = "present_observation", skip_serializing_if = "Option::is_none")]
    pub project_path: Option<GetterObservation<String>>,
    #[serde(default, deserialize_with = "present_observation", skip_serializing_if = "Option::is_none")]
    pub project_directory: Option<GetterObservation<PathBuf>>,
    #[serde(default, deserialize_with = "present_observation", skip_serializing_if = "Option::is_none")]
    pub root_name: Option<GetterObservation<String>>,
    #[serde(default, deserialize_with = "present_observation", skip_serializing_if = "Option::is_none")]
    pub root_directory: Option<GetterObservation<PathBuf>>,
    #[serde(default, deserialize_with = "present_observation", skip_serializing_if = "Option::is_none")]
    pub parent_project_path: Option<GetterObservation<Option<String>>>,
    #[serde(default, deserialize_with = "present_observation", skip_serializing_if = "Option::is_none")]
    pub build_tree_path: Option<GetterObservation<String>>,
    #[serde(default, deserialize_with = "present_observation", skip_serializing_if = "Option::is_none")]
    pub reference_identity_path: Option<GetterObservation<Option<String>>>,
    #[serde(default, deserialize_with = "present_observation", skip_serializing_if = "Option::is_none")]
    pub idea_plugin_present: Option<GetterObservation<bool>>,
    #[serde(default, deserialize_with = "present_observation", skip_serializing_if = "Option::is_none")]
    pub idea_module_name: Option<GetterObservation<Option<String>>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", rename_all = "camelCase", deny_unknown_fields)]
pub struct RawBuildIdentity {
    #[serde(default, deserialize_with = "present_observation", skip_serializing_if = "Option::is_none")]
    pub root_name: Option<GetterObservation<String>>,
    #[serde(default, deserialize_with = "present_observation", skip_serializing_if = "Option::is_none")]
    pub root_directory: Option<GetterObservation<PathBuf>>,
    #[serde(default, deserialize_with = "present_observation", skip_serializing_if = "Option::is_none")]
    pub project_path: Option<GetterObservation<String>>,
    #[serde(default, deserialize_with = "present_observation", skip_serializing_if = "Option::is_none")]
    pub build_tree_path: Option<GetterObservation<String>>,
    #[serde(default, deserialize_with = "present_observation", skip_serializing_if = "Option::is_none")]
    pub reference_identity_path: Option<GetterObservation<Option<String>>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", deny_unknown_fields)]
struct BasicModuleIdentity {
    module: String,
    directory: PathBuf,
    kind: ModuleKind,
    variants: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(remote = "Self", rename_all = "camelCase", deny_unknown_fields)]
struct ImportExport {
    schema: u32,
    root: PathBuf,
    gradle_version: String,
    modules: Vec<BasicModuleIdentity>,
    build_identity: RawBuildIdentity,
    project_catalogue: GetterObservation<Vec<RawImportProject>>,
}

#[derive(Deserialize, Serialize)]
#[serde(remote = "Self")]
struct ImportRecord {
    #[serde(rename = "importFacts", default, deserialize_with = "present_sidecar")]
    import_facts: Option<serde_json::Value>,
}

fn present_sidecar<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<serde_json::Value>, D::Error> {
    serde_json::Value::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportFactsBinding {
    pub model_revision: u64,
    pub selection_revision: u64,
    pub selected_variants: Vec<VariantId>,
}

#[derive(Clone, Debug)]
pub struct ImportFactsSnapshot {
    binding: ImportFactsBinding,
    root: PathBuf,
    identity: Vec<BasicModuleIdentity>,
    gradle_version: String,
    build_identity: RawBuildIdentity,
    project_catalogue: GetterObservation<Vec<RawImportProject>>,
    project_index: BTreeMap<String, usize>,
}

impl ImportFactsSnapshot {
    pub fn gradle_version(&self) -> &str { &self.gradle_version }
    pub fn binding(&self) -> &ImportFactsBinding { &self.binding }
    pub fn build_identity(&self) -> &RawBuildIdentity { &self.build_identity }
    pub fn catalogue_observation(&self) -> &GetterObservation<Vec<RawImportProject>> { &self.project_catalogue }
    pub fn projects(&self) -> FactsResult<&[RawImportProject]> {
        Ok(self.project_catalogue.available()?.as_slice())
    }

    pub fn project(&self, path: &str) -> FactsResult<&RawImportProject> {
        self.project_index.get(path).and_then(|index| match &self.project_catalogue.result { CapturedField::Available(projects) => projects.get(*index), _ => None })
            .ok_or_else(|| unavailable(FactsUnavailableReason::MissingMetadata, format!("No captured Gradle project {path:?}")))
    }

    pub fn ensure_current(&self, model: &ProjectModel, binding: &ImportFactsBinding) -> FactsResult<()> {
        if binding != &self.binding || model.version != 1 || model.root != self.root || model_identity(model) != self.identity {
            return Err(unavailable(FactsUnavailableReason::Stale, "Import facts do not match the current Basic model and selection revision/identity"));
        }
        Ok(())
    }
}

fn model_identity(model: &ProjectModel) -> Vec<BasicModuleIdentity> {
    model.modules.iter().map(|module| BasicModuleIdentity {
        module: module.path.clone(), directory: module.directory.clone(), kind: module.kind,
        variants: module.variants.iter().map(|variant| variant.name.clone()).collect(),
    }).collect()
}

/// Decode separately so a missing or failed import capability cannot erase the Basic model.
pub fn parse_import_facts(output: &str, model: &ProjectModel, binding: ImportFactsBinding) -> FactsResult<ImportFactsSnapshot> {
    let mut records = output.lines().filter_map(|line| line.strip_prefix("KODA_ANDROID_PROJECT_MODEL="));
    let record = records.next().ok_or_else(|| unavailable(FactsUnavailableReason::MissingMetadata, "Gradle returned no Android project model record"))?;
    if records.next().is_some() {
        return Err(unavailable(FactsUnavailableReason::Malformed, "Gradle returned multiple Android project model records"));
    }
    if record.len() > MAX_MODEL_RECORD_BYTES {
        return Err(unavailable(FactsUnavailableReason::UnsupportedShape, "Import facts record exceeds the 16 MiB decoding limit"));
    }
    let record: ImportRecord = serde_json::from_str(record).map_err(|error| unavailable(FactsUnavailableReason::Malformed, format!("Malformed import facts record: {error}")))?;
    let sidecar = record.import_facts.ok_or_else(|| unavailable(FactsUnavailableReason::MissingMetadata, "importFacts is absent from the legacy model"))?;
    let schema = sidecar.get("schema").and_then(serde_json::Value::as_u64).ok_or_else(|| unavailable(FactsUnavailableReason::Malformed, "importFacts schema is missing or malformed"))?;
    if schema != 1 {
        return Err(unavailable(FactsUnavailableReason::UnsupportedSchema, format!("Unsupported importFacts schema {schema}")));
    }
    let export: ImportExport = serde_json::from_value(sidecar).map_err(|error| unavailable(FactsUnavailableReason::Malformed, format!("Malformed importFacts: {error}")))?;
    let identity = model_identity(model);
    if model.version != 1 || export.root != model.root || export.modules != identity {
        return Err(unavailable(FactsUnavailableReason::Stale, "Import facts and Basic model have different root/module/directory/variant identities"));
    }
    validate_text(&export.gradle_version, false, "Gradle version")?;
    validate_path(&export.root)?;
    validate_selection(model, &binding)?;
    validate_build(&export.build_identity, &export.root)?;
    validate_observation(&export.project_catalogue, PROJECT_CATALOGUE)?;
    let projects = export.project_catalogue.available()?;
    let mut project_index = BTreeMap::new();
    for (index, project) in projects.iter().enumerate() {
        validate_project(project, &export.build_identity, &export.root)?;
        let path = required(&project.project_path, PROJECT_PATH)?;
        if project_index.insert(path.clone(), index).is_some() {
            return Err(unavailable(FactsUnavailableReason::Malformed, format!("Duplicate raw Gradle project identity {path:?}")));
        }
    }
    for project in projects {
        if let Some(parent) = &project.parent_project_path {
            if let CapturedField::Available(Some(parent)) = &parent.result {
                if !project_index.contains_key(parent) {
                    return Err(unavailable(FactsUnavailableReason::MissingMetadata, format!("Raw parent project {parent:?} is absent from the successful Gradle catalogue")));
                }
            }
        }
    }
    let root_project = project_index.get(":").and_then(|index| projects.get(*index))
        .ok_or_else(|| unavailable(FactsUnavailableReason::MissingMetadata, "Raw Gradle catalogue has no root holder"))?;
    if required(&root_project.project_directory, PROJECT_DIRECTORY)? != &export.root {
        return Err(unavailable(FactsUnavailableReason::Stale, "Raw Gradle root holder has a different project directory"));
    }
    if let (Some(name), Some(root_name)) = (&root_project.project_name, &export.build_identity.root_name) {
        if let (CapturedField::Available(name), CapturedField::Available(root_name)) = (&name.result, &root_name.result) {
            if name != root_name { return Err(unavailable(FactsUnavailableReason::Malformed, "Raw root holder and build names disagree")); }
        }
    }
    consistent_observations(&root_project.build_tree_path, &export.build_identity.build_tree_path, BUILD_TREE_PATH)?;
    consistent_observations(&root_project.reference_identity_path, &export.build_identity.reference_identity_path, REFERENCE_IDENTITY_PATH)?;
    for module in &identity {
        let project = project_index.get(&module.module).and_then(|index| projects.get(*index))
            .ok_or_else(|| unavailable(FactsUnavailableReason::MissingMetadata, format!("No raw Gradle identity for Basic module {}", module.module)))?;
        if required(&project.project_directory, PROJECT_DIRECTORY)? != &module.directory {
            return Err(unavailable(FactsUnavailableReason::Stale, format!("Raw Gradle project directory differs from Basic module {}", module.module)));
        }
    }
    Ok(ImportFactsSnapshot { binding, root: export.root, identity, gradle_version: export.gradle_version, build_identity: export.build_identity, project_catalogue: export.project_catalogue, project_index })
}

fn required<'a, T>(value: &'a Option<GetterObservation<T>>, getter: &str) -> FactsResult<&'a T> {
    value.as_ref().ok_or_else(|| unavailable(FactsUnavailableReason::MissingMetadata, format!("No observation for {getter}")))?.available()
}

fn validate_selection(model: &ProjectModel, binding: &ImportFactsBinding) -> FactsResult<()> {
    let mut selected = BTreeSet::new();
    let modules = model.modules.iter().map(|module| (module.path.as_str(), module)).collect::<BTreeMap<_, _>>();
    for selection in &binding.selected_variants {
        if !selected.insert(selection) {
            return Err(unavailable(FactsUnavailableReason::Malformed, "Duplicate selected module/variant identity"));
        }
        let module = modules.get(selection.module.as_str()).ok_or_else(|| unavailable(FactsUnavailableReason::MissingMetadata, format!("Selected module {} is absent from the Basic model", selection.module)))?;
        if !module.variants.iter().any(|variant| variant.name == selection.variant) {
            return Err(unavailable(FactsUnavailableReason::MissingVariant, format!("Selected variant {} is absent from {}", selection.variant, selection.module)));
        }
    }
    Ok(())
}

fn validate_observation<T>(value: &GetterObservation<T>, getter: &str) -> FactsResult<()> {
    if value.getter != getter {
        return Err(unavailable(FactsUnavailableReason::Malformed, format!("Unexpected getter provenance for {getter}: {:?}", value.getter)));
    }
    match &value.result {
        CapturedField::Available(_) => Ok(()),
        CapturedField::Unavailable(failure) if failure.capability == getter && !failure.detail.is_empty() => Ok(()),
        CapturedField::Unavailable(_) => Err(unavailable(FactsUnavailableReason::Malformed, format!("Malformed unavailable observation for {getter}"))),
        CapturedField::NotApplicable(_) => Err(unavailable(FactsUnavailableReason::Malformed, format!("No notApplicable identity getter branch exists for {getter}"))),
    }
}

fn optional_observation<T>(value: &Option<GetterObservation<T>>, getter: &str, validate: impl FnOnce(&T) -> FactsResult<()>) -> FactsResult<()> {
    if let Some(value) = value {
        validate_observation(value, getter)?;
        if let CapturedField::Available(value) = &value.result { validate(value)?; }
    }
    Ok(())
}

fn consistent_observations<T: PartialEq>(left: &Option<GetterObservation<T>>, right: &Option<GetterObservation<T>>, getter: &str) -> FactsResult<()> {
    if let (Some(left), Some(right)) = (left, right) {
        if let (CapturedField::Available(left), CapturedField::Available(right)) = (&left.result, &right.result) {
            if left != right { return Err(unavailable(FactsUnavailableReason::Malformed, format!("Raw root/build observations disagree for {getter}"))); }
        }
    }
    Ok(())
}

fn validate_build(build: &RawBuildIdentity, root: &Path) -> FactsResult<()> {
    optional_observation(&build.root_name, ROOT_NAME, |value| validate_text(value, true, ROOT_NAME))?;
    optional_observation(&build.root_directory, ROOT_DIRECTORY, |value| validate_path(value))?;
    optional_observation(&build.project_path, PROJECT_PATH, |value| validate_project_path(value))?;
    optional_observation(&build.build_tree_path, BUILD_TREE_PATH, |value| validate_project_path(value))?;
    optional_observation(&build.reference_identity_path, REFERENCE_IDENTITY_PATH, |value| match value { Some(value) => validate_project_path(value), None => Ok(()) })?;
    if required(&build.root_directory, ROOT_DIRECTORY)? != root || required(&build.project_path, PROJECT_PATH)? != ":" {
        return Err(unavailable(FactsUnavailableReason::Stale, "Raw build identity does not match the Basic model root"));
    }
    Ok(())
}

fn validate_project(project: &RawImportProject, build: &RawBuildIdentity, root: &Path) -> FactsResult<()> {
    optional_observation(&project.project_name, PROJECT_NAME, |value| validate_text(value, true, PROJECT_NAME))?;
    optional_observation(&project.project_path, PROJECT_PATH, |value| validate_project_path(value))?;
    optional_observation(&project.project_directory, PROJECT_DIRECTORY, |value| validate_path(value))?;
    optional_observation(&project.root_name, ROOT_NAME, |value| validate_text(value, true, ROOT_NAME))?;
    optional_observation(&project.root_directory, ROOT_DIRECTORY, |value| validate_path(value))?;
    optional_observation(&project.parent_project_path, PARENT_PATH, |value| match value { Some(value) => validate_project_path(value), None => Ok(()) })?;
    optional_observation(&project.build_tree_path, BUILD_TREE_PATH, |value| validate_project_path(value))?;
    optional_observation(&project.reference_identity_path, REFERENCE_IDENTITY_PATH, |value| match value { Some(value) => validate_project_path(value), None => Ok(()) })?;
    optional_observation(&project.idea_plugin_present, IDEA_PLUGIN, |_| Ok(()))?;
    optional_observation(&project.idea_module_name, IDEA_NAME, |value| match value { Some(value) => validate_text(value, true, IDEA_NAME), None => Ok(()) })?;
    let path = required(&project.project_path, PROJECT_PATH)?;
    required(&project.project_directory, PROJECT_DIRECTORY)?;
    if required(&project.root_directory, ROOT_DIRECTORY)? != root {
        return Err(unavailable(FactsUnavailableReason::Stale, format!("Raw project {path} belongs to a different build root")));
    }
    if let (Some(left), Some(right)) = (&project.root_name, &build.root_name) {
        if let (CapturedField::Available(left), CapturedField::Available(right)) = (&left.result, &right.result) {
            if left != right { return Err(unavailable(FactsUnavailableReason::Malformed, "Raw root name observations disagree")); }
        }
    }
    if let Some(parent) = &project.parent_project_path {
        if let CapturedField::Available(parent) = &parent.result {
            let expected = if path == ":" { None } else { Some(path.rsplit_once(':').map(|(parent, _)| if parent.is_empty() { ":" } else { parent }).ok_or_else(|| unavailable(FactsUnavailableReason::Malformed, "Malformed project parent path"))?) };
            if parent.as_deref() != expected { return Err(unavailable(FactsUnavailableReason::Malformed, format!("Raw parent project path contradicts {path}"))); }
        }
    }
    if let (Some(plugin), Some(name)) = (&project.idea_plugin_present, &project.idea_module_name) {
        if matches!(plugin.result, CapturedField::Available(false)) && matches!(name.result, CapturedField::Available(Some(_))) {
            return Err(unavailable(FactsUnavailableReason::Malformed, "Idea module name is present while the official Idea plugin is absent"));
        }
    }
    Ok(())
}

fn validate_text(value: &str, empty_allowed: bool, field: &str) -> FactsResult<()> {
    if (!empty_allowed && value.is_empty()) || value.chars().any(char::is_control) {
        return Err(unavailable(FactsUnavailableReason::Malformed, format!("Malformed {field}: {value:?}")));
    }
    Ok(())
}

fn validate_project_path(value: &str) -> FactsResult<()> {
    validate_text(value, false, "Gradle project identity path")?;
    if value.strip_prefix(':').is_none_or(|suffix| !suffix.is_empty() && suffix.split(':').any(str::is_empty)) {
        return Err(unavailable(FactsUnavailableReason::Malformed, format!("Malformed Gradle project identity path {value:?}")));
    }
    Ok(())
}

fn validate_path(path: &Path) -> FactsResult<()> {
    if !path.is_absolute() || path.components().collect::<PathBuf>().as_os_str() != path.as_os_str()
        || path.components().any(|component| matches!(component, Component::ParentDir | Component::CurDir))
        || path.to_str().is_none_or(|value| value.chars().any(char::is_control)) {
        return Err(FactsUnavailable { reason: FactsUnavailableReason::Malformed, detail: format!("Raw Gradle directory is not absolute and normalized: {}", path.display()), path: Some(path.into()) });
    }
    Ok(())
}

object_serde!(RawImportProject, RawBuildIdentity, BasicModuleIdentity, ImportExport, ImportRecord);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for GetterObservation<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        GetterObservation::<T>::deserialize(ObjectOnly(deserializer))
    }
}
impl<T: Serialize> Serialize for GetterObservation<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        GetterObservation::<T>::serialize(self, serializer)
    }
}
