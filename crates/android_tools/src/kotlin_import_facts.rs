//! Kotlin getter observations remain raw so missing capabilities cannot manufacture
//! a compiler map or facet. Apache-2.0 references are retained in the adjacent test data.

use crate::{
    generated_artifacts::CapturedField,
    import_facts::{
        GetterObservation, ImportFactsBinding, ImportFactsSnapshot, RawBuildIdentity,
        RawImportProject,
    },
    project_model::{ModuleKind, ProjectModel, VariantId},
    project_tree_facts::{FactsUnavailable, FactsUnavailableReason},
};
use serde::{
    Deserialize, Serialize,
    de::{DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::{Component, Path, PathBuf},
};

const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;
type FactsResult<T> = Result<T, FactsUnavailable>;

struct ObjectOnly<D>(D);
impl<'de, D: serde::Deserializer<'de>> serde::Deserializer<'de> for ObjectOnly<D> {
    type Error = D::Error;
    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
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
macro_rules! tagged_serde {
    ($($name:ident),+ $(,)?) => {$(
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = serde_json::Value::deserialize(deserializer)?;
                if !value.is_object() || value.get("value").is_none() {
                    return Err(serde::de::Error::custom("Tagged capture requires an object and explicit value"));
                }
                $name::deserialize(ObjectOnly(value)).map_err(serde::de::Error::custom)
            }
        }
        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                $name::serialize(self, serializer)
            }
        }
    )+};
}

fn failure(reason: FactsUnavailableReason, detail: impl Into<String>) -> FactsUnavailable {
    FactsUnavailable {
        reason,
        detail: detail.into(),
        path: None,
    }
}
fn malformed(detail: impl Into<String>) -> FactsUnavailable {
    failure(FactsUnavailableReason::Malformed, detail)
}

/// These are protocol resource limits, not Android Studio task-count semantics.
/// The defaults leave room for thousands of catalogue methods/classes and getter
/// events; callers can raise node/traversal budgets while the 16 MiB record cap
/// remains fixed. Exhausting a budget makes this capability unavailable and
/// preserves independently decoded Basic/import facts.
#[derive(Clone, Copy, Debug)]
pub struct CaptureLimits {
    /// Maximum UTF-8 record bytes; values above 16 MiB are clamped.
    pub record_bytes: usize,
    /// Maximum JSON value nodes in the entire kotlinFacts sidecar (keys excluded).
    pub entries: usize,
    /// Maximum bytes per decoded string or object key, not their aggregate size.
    pub string_bytes: usize,
    /// Maximum retained exception causes in one unsuccessful getter event.
    pub exception_causes: usize,
    /// Maximum examined class vertices and edges per validation pass, including repeats.
    pub ancestry_steps: usize,
}
impl Default for CaptureLimits {
    fn default() -> Self {
        Self {
            record_bytes: MAX_RECORD_BYTES,
            entries: 131_072,
            string_bytes: MAX_RECORD_BYTES,
            exception_causes: 64,
            ancestry_steps: 524_288,
        }
    }
}

struct Bounds<'a> {
    limits: CaptureLimits,
    entries: &'a mut usize,
    limit_hit: &'a mut bool,
}
impl<'de> DeserializeSeed<'de> for Bounds<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        *self.entries = self
            .entries
            .checked_add(1)
            .ok_or_else(|| serde::de::Error::custom("Kotlin capture entry counter overflow"))?;
        if *self.entries > self.limits.entries {
            *self.limit_hit = true;
            return Err(serde::de::Error::custom(
                "Kotlin capture entry limit exceeded",
            ));
        }
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Bounds<'_> {
    type Value = ();
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("bounded Kotlin capture JSON")
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<(), E> {
        if value.len() > self.limits.string_bytes {
            *self.limit_hit = true;
            return Err(E::custom("Kotlin capture string byte limit exceeded"));
        }
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut values: A) -> Result<(), A::Error> {
        while values
            .next_element_seed(Bounds {
                limits: self.limits,
                entries: &mut *self.entries,
                limit_hit: &mut *self.limit_hit,
            })?
            .is_some()
        {}
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut values: A) -> Result<(), A::Error> {
        let mut keys = BTreeSet::new();
        while let Some(key) = values.next_key::<String>()? {
            if key.len() > self.limits.string_bytes {
                *self.limit_hit = true;
                return Err(serde::de::Error::custom(
                    "Kotlin capture key byte limit exceeded",
                ));
            }
            if !keys.insert(key) {
                return Err(serde::de::Error::custom(
                    "Duplicate Kotlin capture object key",
                ));
            }
            values.next_value_seed(Bounds {
                limits: self.limits,
                entries: &mut *self.entries,
                limit_hit: &mut *self.limit_hit,
            })?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureBinding {
    pub capture_id: String,
    pub source_epoch: String,
    pub fixture_before_sha256: String,
    pub fixture_after_sha256: String,
    pub model_revision: u64,
    pub selection_revision: u64,
    pub selected_variants: Vec<SelectedVariant>,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct SelectedVariant {
    pub module: String,
    pub variant: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct JavaRuntime {
    pub vendor: String,
    pub version: String,
    pub build: String,
    pub artifact: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct RuntimeLocale {
    pub identifier: String,
    pub implementation: String,
    pub version: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct RuntimeArtifact {
    pub id: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    #[serde(deserialize_with = "required_nullable")]
    pub coordinate: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub version: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct RuntimeLoader {
    pub id: String,
    #[serde(deserialize_with = "required_nullable")]
    pub parent: Option<String>,
    pub artifacts: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum ClassOrigin {
    Jdk(String),
    Artifact(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct RuntimeClass {
    pub id: String,
    pub name: String,
    pub loader: String,
    pub origin: ClassOrigin,
    #[serde(deserialize_with = "required_nullable")]
    pub superclass: Option<String>,
    pub interfaces: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeIdentity {
    pub gradle_version: String,
    pub java: JavaRuntime,
    pub locale: RuntimeLocale,
    pub artifacts: Vec<RuntimeArtifact>,
    pub loaders: Vec<RuntimeLoader>,
    pub classes: Vec<RuntimeClass>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ObjectKind {
    Project,
    Plugin,
    Task,
    Extension,
    Target,
    Compilation,
    Property,
    Resolver,
    Container,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct TaskIdentity {
    pub name: String,
    pub path: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureObject {
    pub id: String,
    pub kind: ObjectKind,
    pub project: String,
    pub class_id: String,
    #[serde(deserialize_with = "required_nullable")]
    pub task: Option<TaskIdentity>,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", rename_all = "camelCase", deny_unknown_fields)]
pub struct GetterMethod {
    pub id: String,
    pub name: String,
    pub descriptor: String,
    pub declaring_class: String,
    pub is_static: bool,
    pub parameter_classes: Vec<Option<String>>,
    #[serde(deserialize_with = "required_nullable")]
    pub return_class: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", rename_all = "camelCase", deny_unknown_fields)]
pub struct MethodCatalogue {
    pub id: String,
    pub class_id: String,
    pub methods: Vec<GetterMethod>,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum MethodSelection {
    Selected(String),
    Missing(MissingMethod),
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", rename_all = "camelCase", deny_unknown_fields)]
pub struct MissingMethod {
    pub name: String,
    pub descriptor: String,
    pub is_static: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum GetterArgument {
    String(String),
    Boolean(bool),
    Object(String),
    Null(()),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ValueKind {
    String,
    Boolean,
    Strings,
    Object,
    Objects,
    File,
    Unsupported,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ContainerOrder {
    List,
    Iterable,
    NamedDomainObjectAsMapValues,
    ProjectTaskMapValues,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", rename_all = "camelCase", deny_unknown_fields)]
pub struct ReturnShape {
    pub kind: ValueKind,
    pub nullable: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub object_kind: Option<ObjectKind>,
    #[serde(deserialize_with = "required_nullable")]
    pub order: Option<ContainerOrder>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GetterPurpose {
    Raw,
    SourceSet,
    PropertyGet,
    ResolverInstance,
    CompilerArguments,
    TaskLookup,
    ContainerGet,
    ContainerIterate,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CaptureMode {
    Discovery,
    Invocation,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Consumer {
    Kotlin,
    Kapt,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum RequestParameter {
    Absent(()),
    Explicit(Option<String>),
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", rename_all = "camelCase", deny_unknown_fields)]
pub struct GetterRequest {
    pub id: String,
    pub project: String,
    pub consumer: Consumer,
    pub model_call: String,
    pub parameter: RequestParameter,
    #[serde(deserialize_with = "required_nullable")]
    pub variant: Option<SelectedVariant>,
    #[serde(deserialize_with = "required_nullable")]
    pub owner: Option<String>,
    pub catalogue: String,
    pub method: MethodSelection,
    pub arguments: Vec<GetterArgument>,
    pub return_shape: ReturnShape,
    pub purpose: GetterPurpose,
    #[serde(deserialize_with = "required_nullable")]
    pub after: Option<String>,
}

/// Obtain this contract from the owned runtime/discovery and Rust request planner,
/// not by copying an untrusted invocation packet into its own expected context.
/// This catalogue covers already discovered objects. Initial class-lookup misses
/// still require an explicit discovery protocol before official getter transport.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct CaptureContext {
    pub binding: CaptureBinding,
    pub imports: ImportIdentity,
    pub mode: CaptureMode,
    pub runtime: RuntimeIdentity,
    pub objects: Vec<CaptureObject>,
    pub catalogues: Vec<MethodCatalogue>,
    pub requests: Vec<GetterRequest>,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportIdentity {
    pub build_identity: RawBuildIdentity,
    pub project_catalogue: GetterObservation<Vec<RawImportProject>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum CaptureValue {
    String(String),
    Boolean(bool),
    Strings(Vec<String>),
    Object(String),
    Objects(Vec<String>),
    File(PathBuf),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GetterFailureKind {
    MissingClass,
    MissingMethod,
    Linkage,
    Access,
    Invocation,
    UnsupportedReturnShape,
    CatalogueChanged,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GetterFailureStage {
    ClassLoad,
    MethodDiscovery,
    Invoke,
    PropertyGet,
    TaskLookup,
    ReturnDecode,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct ExceptionCause {
    pub class: String,
    #[serde(deserialize_with = "required_nullable")]
    pub message: Option<String>,
}
fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", rename_all = "camelCase", deny_unknown_fields)]
pub struct GetterFailure {
    pub kind: GetterFailureKind,
    pub stage: GetterFailureStage,
    pub capability: String,
    pub detail: String,
    /// Runtime class ID of an actual returned object when decoding fails.
    #[serde(deserialize_with = "required_nullable")]
    pub actual_class: Option<String>,
    pub exceptions: Vec<ExceptionCause>,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    remote = "Self",
    tag = "status",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum GetterOutcome {
    Available(Option<CaptureValue>),
    Unavailable(GetterFailure),
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", rename_all = "camelCase", deny_unknown_fields)]
pub struct GetterEvent {
    pub id: String,
    pub request: String,
    pub outcome: GetterOutcome,
    /// Actual returned iterable identity, present only for a nonnull collection.
    #[serde(deserialize_with = "required_nullable")]
    pub container: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", deny_unknown_fields)]
struct BasicIdentity {
    module: String,
    directory: PathBuf,
    kind: ModuleKind,
    variants: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(remote = "Self", deny_unknown_fields)]
struct KotlinExport {
    schema: u32,
    root: PathBuf,
    modules: Vec<BasicIdentity>,
    context: CaptureContext,
    events: Vec<GetterEvent>,
}

object_serde!(
    CaptureBinding,
    SelectedVariant,
    JavaRuntime,
    RuntimeLocale,
    RuntimeArtifact,
    RuntimeLoader,
    RuntimeClass,
    RuntimeIdentity,
    TaskIdentity,
    CaptureObject,
    GetterMethod,
    MethodCatalogue,
    MissingMethod,
    ReturnShape,
    GetterRequest,
    CaptureContext,
    ImportIdentity,
    ExceptionCause,
    GetterFailure,
    GetterEvent,
    BasicIdentity,
    KotlinExport
);
tagged_serde!(
    ClassOrigin,
    MethodSelection,
    GetterArgument,
    RequestParameter,
    CaptureValue,
    GetterOutcome
);

#[derive(Deserialize)]
struct Record<'a> {
    #[serde(
        rename = "kotlinFacts",
        borrow,
        default,
        deserialize_with = "present_raw"
    )]
    kotlin_facts: Option<&'a serde_json::value::RawValue>,
}
fn present_raw<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<&'de serde_json::value::RawValue>, D::Error> {
    <&serde_json::value::RawValue>::deserialize(deserializer).map(Some)
}

#[derive(Debug)]
pub struct KotlinFactsSnapshot {
    export: KotlinExport,
}
impl KotlinFactsSnapshot {
    /// Historical raw facts; this accessor does not establish current provenance.
    pub fn raw_context(&self) -> &CaptureContext {
        &self.export.context
    }
    /// Historical events, including failure and null. Recheck before current use.
    pub fn raw_events(&self) -> &[GetterEvent] {
        &self.export.events
    }
    /// Verify current Basic/import identity and the independently retained
    /// runtime/discovery and Rust request plan. The packet's own context is not proof.
    pub fn ensure_current(
        &self,
        model: &ProjectModel,
        imports: &ImportFactsSnapshot,
        expected: &CaptureContext,
    ) -> FactsResult<()> {
        ensure_binding(&self.export, model, imports, expected)
    }
}

/// Decode a separately requested raw capability. A failure does not erase Basic/importFacts.
/// Omitted requests mean not captured; requested events must be complete. A
/// requested available null is distinct from an omitted, empty or unsuccessful result.
pub fn parse_kotlin_facts(
    output: &str,
    model: &ProjectModel,
    imports: &ImportFactsSnapshot,
    expected: &CaptureContext,
    limits: CaptureLimits,
) -> FactsResult<KotlinFactsSnapshot> {
    let mut records = output
        .lines()
        .filter_map(|line| line.strip_prefix("KODA_ANDROID_PROJECT_MODEL="));
    let record = records.next().ok_or_else(|| {
        failure(
            FactsUnavailableReason::MissingMetadata,
            "No Android project model record",
        )
    })?;
    if records.next().is_some() {
        return Err(malformed("Multiple Android project model records"));
    }
    if record.len() > limits.record_bytes.min(MAX_RECORD_BYTES) {
        return Err(failure(
            FactsUnavailableReason::UnsupportedShape,
            "Kotlin capture record byte limit exceeded",
        ));
    }
    let mut decoder = serde_json::Deserializer::from_str(record);
    let record = Record::deserialize(ObjectOnly(&mut decoder))
        .map_err(|error| malformed(error.to_string()))?;
    decoder
        .end()
        .map_err(|error| malformed(error.to_string()))?;
    let raw = record.kotlin_facts.ok_or_else(|| {
        failure(
            FactsUnavailableReason::MissingMetadata,
            "kotlinFacts was not captured",
        )
    })?;
    let mut entries = 0;
    let mut limit_hit = false;
    let mut bounds = serde_json::Deserializer::from_str(raw.get());
    Bounds {
        limits,
        entries: &mut entries,
        limit_hit: &mut limit_hit,
    }
    .deserialize(&mut bounds)
    .map_err(|error| {
        failure(
            if limit_hit {
                FactsUnavailableReason::UnsupportedShape
            } else {
                FactsUnavailableReason::Malformed
            },
            error.to_string(),
        )
    })?;
    bounds.end().map_err(|error| malformed(error.to_string()))?;
    #[derive(Deserialize)]
    struct Schema {
        schema: u32,
    }
    let schema = Schema::deserialize(ObjectOnly(&mut serde_json::Deserializer::from_str(
        raw.get(),
    )))
    .map_err(|error| malformed(error.to_string()))?
    .schema;
    if schema != 1 {
        return Err(failure(
            FactsUnavailableReason::UnsupportedSchema,
            format!("Unsupported kotlinFacts schema {schema}"),
        ));
    }
    let export: KotlinExport =
        serde_json::from_str(raw.get()).map_err(|error| malformed(error.to_string()))?;
    ensure_binding(&export, model, imports, expected)?;
    let metadata = validate_context(&export.context, imports, limits)?;
    validate_events(&export, &metadata, limits)?;
    drop(metadata);
    Ok(KotlinFactsSnapshot { export })
}

fn identities(model: &ProjectModel) -> Vec<BasicIdentity> {
    model
        .modules
        .iter()
        .map(|module| BasicIdentity {
            module: module.path.clone(),
            directory: module.directory.clone(),
            kind: module.kind,
            variants: module
                .variants
                .iter()
                .map(|variant| variant.name.clone())
                .collect(),
        })
        .collect()
}
fn import_binding(binding: &CaptureBinding) -> ImportFactsBinding {
    ImportFactsBinding {
        model_revision: binding.model_revision,
        selection_revision: binding.selection_revision,
        selected_variants: binding
            .selected_variants
            .iter()
            .map(|variant| VariantId {
                module: variant.module.clone(),
                variant: variant.variant.clone(),
            })
            .collect(),
    }
}
fn ensure_binding(
    export: &KotlinExport,
    model: &ProjectModel,
    imports: &ImportFactsSnapshot,
    expected: &CaptureContext,
) -> FactsResult<()> {
    imports.ensure_current(model, &import_binding(&expected.binding))?;
    if export.root.as_os_str() != model.root.as_os_str()
        || export.modules != identities(model)
        || !export
            .modules
            .iter()
            .map(|module| module.directory.as_os_str())
            .eq(model
                .modules
                .iter()
                .map(|module| module.directory.as_os_str()))
        || &export.context != expected
        || !export
            .context
            .runtime
            .artifacts
            .iter()
            .map(|artifact| artifact.path.as_os_str())
            .eq(expected
                .runtime
                .artifacts
                .iter()
                .map(|artifact| artifact.path.as_os_str()))
        || !import_paths(
            &export.context.imports.build_identity,
            &export.context.imports.project_catalogue,
        )
        .eq(import_paths(
            &expected.imports.build_identity,
            &expected.imports.project_catalogue,
        ))
        || export.context.runtime.gradle_version != imports.gradle_version()
        || &export.context.imports.build_identity != imports.build_identity()
        || &export.context.imports.project_catalogue != imports.catalogue_observation()
        || !import_paths(
            &export.context.imports.build_identity,
            &export.context.imports.project_catalogue,
        )
        .eq(import_paths(
            imports.build_identity(),
            imports.catalogue_observation(),
        ))
    {
        return Err(failure(
            FactsUnavailableReason::Stale,
            "Kotlin capture differs from current Basic/import/runtime/request binding",
        ));
    }
    Ok(())
}
// Path equality ignores spelling changes such as interior dots or trailing
// separators. These comparisons supplement typed identity equality without
// normalizing the paths reported by independently retained getter observations.
fn import_paths<'a>(
    build: &'a RawBuildIdentity,
    catalogue: &'a GetterObservation<Vec<RawImportProject>>,
) -> impl Iterator<Item = &'a std::ffi::OsStr> {
    let projects = match &catalogue.result {
        CapturedField::Available(projects) => projects.as_slice(),
        CapturedField::Unavailable(_) | CapturedField::NotApplicable(_) => &[],
    };
    observed_path(build.root_directory.as_ref())
        .into_iter()
        .chain(projects.iter().flat_map(|project| {
            [
                observed_path(project.project_directory.as_ref()),
                observed_path(project.root_directory.as_ref()),
            ]
            .into_iter()
            .flatten()
        }))
}
fn observed_path(observation: Option<&GetterObservation<PathBuf>>) -> Option<&std::ffi::OsStr> {
    match &observation?.result {
        CapturedField::Available(path) => Some(path.as_os_str()),
        CapturedField::Unavailable(_) | CapturedField::NotApplicable(_) => None,
    }
}
fn text(value: &str, description: &str) -> FactsResult<()> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(malformed(format!("Invalid {description}")));
    }
    Ok(())
}
fn digest(value: &str) -> FactsResult<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|value| value.is_ascii_hexdigit() && !value.is_ascii_uppercase())
    {
        return Err(malformed("Expected lower-case SHA-256 digest"));
    }
    Ok(())
}
fn absolute_path(path: &Path) -> FactsResult<()> {
    if !path.is_absolute()
        || path.components().collect::<PathBuf>().as_os_str() != path.as_os_str()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        || path.to_str().is_none_or(|value| value.contains('\0'))
    {
        return Err(malformed(
            "Expected absolute unambiguous runtime artifact path",
        ));
    }
    Ok(())
}
fn index<'a, T>(
    values: &'a [T],
    id: impl Fn(&'a T) -> &'a str,
) -> FactsResult<BTreeMap<&'a str, &'a T>> {
    let mut result = BTreeMap::new();
    for value in values {
        let id = id(value);
        text(id, "capture ID")?;
        if result.insert(id, value).is_some() {
            return Err(malformed(format!("Duplicate capture ID {id}")));
        }
    }
    Ok(result)
}
fn get<'a, T>(values: &BTreeMap<&str, &'a T>, id: &str) -> FactsResult<&'a T> {
    values
        .get(id)
        .copied()
        .ok_or_else(|| malformed(format!("Unknown capture reference {id}")))
}

fn acyclic<'a>(edges: BTreeMap<&'a str, Vec<&'a str>>) -> FactsResult<()> {
    let mut remaining = BTreeMap::new();
    let mut incoming: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (&id, targets) in &edges {
        remaining.insert(id, targets.len());
        for &target in targets {
            if !edges.contains_key(target) {
                return Err(malformed(format!("Dangling graph reference {target}")));
            }
            incoming.entry(target).or_default().push(id);
        }
    }
    let mut ready: Vec<&str> = remaining
        .iter()
        .filter_map(|(&id, &count)| (count == 0).then_some(id))
        .collect();
    let mut visited = 0;
    while let Some(id) = ready.pop() {
        visited += 1;
        if let Some(parents) = incoming.get(id) {
            for parent in parents {
                let count = remaining
                    .get_mut(parent)
                    .ok_or_else(|| malformed("Missing graph count"))?;
                *count = count
                    .checked_sub(1)
                    .ok_or_else(|| malformed("Invalid graph count"))?;
                if *count == 0 {
                    ready.push(parent);
                }
            }
        }
    }
    if visited != edges.len() {
        return Err(malformed("Cyclic capture identity graph"));
    }
    Ok(())
}
struct ClassRelations<'a> {
    classes: &'a BTreeMap<&'a str, &'a RuntimeClass>,
    answers: BTreeMap<(&'a str, &'a str), bool>,
    named_answers: BTreeMap<(&'a str, &'a str), bool>,
    steps: usize,
    limit: usize,
}
impl<'a> ClassRelations<'a> {
    fn charge(&mut self) -> FactsResult<()> {
        self.steps = self
            .steps
            .checked_add(1)
            .ok_or_else(|| malformed("Class traversal counter overflow"))?;
        if self.steps > self.limit {
            return Err(failure(
                FactsUnavailableReason::UnsupportedShape,
                "Class ancestry traversal limit exceeded",
            ));
        }
        Ok(())
    }
    fn assignable(&mut self, class: &'a str, ancestor: &'a str) -> FactsResult<bool> {
        let ancestor_class = get(self.classes, ancestor)?;
        // JVM interfaces have no superclass, but their instances are still
        // assignable to the JDK's Object. Keep the observed superclass graph intact.
        if ancestor_class.name == "java.lang.Object"
            && matches!(&ancestor_class.origin, ClassOrigin::Jdk(module) if module == "java.base")
        {
            get(self.classes, class)?;
            return Ok(true);
        }
        self.contains(class, ancestor)
    }
    fn search(
        &mut self,
        class: &'a str,
        matches: impl Fn(&RuntimeClass) -> bool,
    ) -> FactsResult<bool> {
        let mut ready = vec![class];
        let mut seen = BTreeSet::new();
        while let Some(id) = ready.pop() {
            self.charge()?;
            if !seen.insert(id) {
                continue;
            }
            let value = get(self.classes, id)?;
            if matches(value) {
                return Ok(true);
            }
            for parent in value.superclass.iter().chain(&value.interfaces) {
                self.charge()?;
                ready.push(parent.as_str());
            }
        }
        Ok(false)
    }
    fn contains(&mut self, class: &'a str, ancestor: &'a str) -> FactsResult<bool> {
        if let Some(value) = self.answers.get(&(class, ancestor)) {
            return Ok(*value);
        }
        let found = self.search(class, |value| value.id == ancestor)?;
        self.answers.insert((class, ancestor), found);
        Ok(found)
    }
    fn contains_artifact_name(&mut self, class: &'a str, name: &'a str) -> FactsResult<bool> {
        if let Some(value) = self.named_answers.get(&(class, name)) {
            return Ok(*value);
        }
        let found = self.search(class, |value| {
            value.name == name && matches!(&value.origin, ClassOrigin::Artifact(_))
        })?;
        self.named_answers.insert((class, name), found);
        Ok(found)
    }
}

fn class_descriptor(class: &RuntimeClass) -> String {
    if class.name.starts_with('[') {
        class.name.replace('.', "/")
    } else {
        format!("L{};", class.name.replace('.', "/"))
    }
}
fn validate_descriptor_class(
    value: &str,
    class: &Option<String>,
    classes: &BTreeMap<&str, &RuntimeClass>,
) -> FactsResult<()> {
    match (value.starts_with('L') || value.starts_with('['), class) {
        (true, Some(id)) if class_descriptor(get(classes, id)?) == value => Ok(()),
        (false, None) => Ok(()),
        _ => Err(malformed(
            "Reflection parameter/return class differs from JVM descriptor",
        )),
    }
}

fn descriptor(value: &str) -> FactsResult<Vec<&str>> {
    fn field(value: &str, mut offset: usize, allow_void: bool) -> Option<usize> {
        let bytes = value.as_bytes();
        let mut arrays = 0;
        while bytes.get(offset) == Some(&b'[') {
            arrays += 1;
            offset += 1;
            if arrays > 255 {
                return None;
            }
        }
        match bytes.get(offset)? {
            b'V' if allow_void && arrays == 0 => Some(offset + 1),
            b'Z' | b'B' | b'C' | b'S' | b'I' | b'J' | b'F' | b'D' => Some(offset + 1),
            b'L' => {
                let end = value.get(offset + 1..)?.find(';')? + offset + 1;
                let name = value.get(offset + 1..end)?;
                (!name.is_empty()
                    && !name
                        .chars()
                        .any(|value| value.is_control() || matches!(value, '.' | '[' | '(' | ')')))
                .then_some(end + 1)
            }
            _ => None,
        }
    }
    if !value.starts_with('(') {
        return Err(malformed("Invalid JVM method descriptor"));
    }
    let mut offset = 1;
    let mut arguments = Vec::new();
    while value.as_bytes().get(offset) != Some(&b')') {
        let end = field(value, offset, false)
            .ok_or_else(|| malformed("Invalid JVM argument descriptor"))?;
        arguments.push(
            value
                .get(offset..end)
                .ok_or_else(|| malformed("Invalid JVM descriptor boundary"))?,
        );
        offset = end;
    }
    let end =
        field(value, offset + 1, true).ok_or_else(|| malformed("Invalid JVM return descriptor"))?;
    if end != value.len() {
        return Err(malformed("Trailing JVM descriptor content"));
    }
    Ok(arguments)
}

struct MethodInfo<'a> {
    catalogue: &'a str,
    method: &'a GetterMethod,
    parameters: Vec<&'a str>,
    returned: &'a str,
}
struct MethodIndex<'a> {
    methods: BTreeMap<&'a str, MethodInfo<'a>>,
    jdk_classes: BTreeMap<&'a str, &'a RuntimeClass>,
}
fn validate_context<'a>(
    context: &'a CaptureContext,
    imports: &ImportFactsSnapshot,
    limits: CaptureLimits,
) -> FactsResult<MethodIndex<'a>> {
    text(&context.binding.capture_id, "capture session")?;
    text(&context.binding.source_epoch, "source epoch")?;
    digest(&context.binding.fixture_before_sha256)?;
    digest(&context.binding.fixture_after_sha256)?;
    let runtime = &context.runtime;
    for value in [
        &runtime.gradle_version,
        &runtime.java.vendor,
        &runtime.java.version,
        &runtime.java.build,
        &runtime.locale.identifier,
        &runtime.locale.implementation,
        &runtime.locale.version,
    ] {
        text(value, "runtime provenance")?;
    }
    let artifacts = index(&runtime.artifacts, |value| &value.id)?;
    let loaders = index(&runtime.loaders, |value| &value.id)?;
    let classes = index(&runtime.classes, |value| &value.id)?;
    let objects = index(&context.objects, |value| &value.id)?;
    let catalogues = index(&context.catalogues, |value| &value.id)?;
    let requests = index(&context.requests, |value| &value.id)?;
    get(&artifacts, &runtime.java.artifact)?;
    let mut relations = ClassRelations {
        classes: &classes,
        answers: BTreeMap::new(),
        named_answers: BTreeMap::new(),
        steps: 0,
        limit: limits.ancestry_steps,
    };
    for artifact in &runtime.artifacts {
        absolute_path(&artifact.path)?;
        digest(&artifact.sha256)?;
        if artifact.bytes == 0 {
            return Err(malformed("Empty runtime artifact"));
        }
        for value in [&artifact.coordinate, &artifact.version]
            .into_iter()
            .flatten()
        {
            text(value, "artifact metadata")?;
        }
    }
    let mut loader_artifacts = BTreeMap::new();
    for loader in &runtime.loaders {
        let mut seen = BTreeSet::new();
        for artifact in &loader.artifacts {
            get(&artifacts, artifact)?;
            if !seen.insert(artifact.as_str()) {
                return Err(malformed("Duplicate artifact in loader catalogue"));
            }
        }
        loader_artifacts.insert(loader.id.as_str(), seen);
    }
    acyclic(
        runtime
            .loaders
            .iter()
            .map(|value| {
                (
                    value.id.as_str(),
                    value.parent.iter().map(String::as_str).collect(),
                )
            })
            .collect(),
    )?;
    let mut class_identities = BTreeSet::new();
    let mut jdk_classes = BTreeMap::new();
    for class in &runtime.classes {
        text(&class.name, "runtime class")?;
        if !class_identities.insert((&class.name, &class.loader)) {
            return Err(malformed("Duplicate loaded class identity"));
        }
        let loader = get(&loaders, &class.loader)?;
        match &class.origin {
            ClassOrigin::Jdk(module) => {
                text(module, "JDK class module")?;
                if module == "java.base" && jdk_classes.insert(class.name.as_str(), class).is_some()
                {
                    return Err(malformed("Duplicate JDK base class identity"));
                }
            }
            ClassOrigin::Artifact(artifact) => {
                get(&artifacts, artifact)?;
                if !loader_artifacts
                    .get(loader.id.as_str())
                    .ok_or_else(|| malformed("Unknown loader artifact index"))?
                    .contains(artifact.as_str())
                {
                    return Err(malformed("Class artifact absent from its declaring loader"));
                }
            }
        }
        let mut seen = BTreeSet::new();
        for parent in class.superclass.iter().chain(&class.interfaces) {
            if !seen.insert(parent) {
                return Err(malformed("Duplicate class ancestor"));
            }
        }
    }
    acyclic(
        runtime
            .classes
            .iter()
            .map(|value| {
                (
                    value.id.as_str(),
                    value
                        .superclass
                        .iter()
                        .chain(&value.interfaces)
                        .map(String::as_str)
                        .collect(),
                )
            })
            .collect(),
    )?;
    for object in &context.objects {
        imports.project(&object.project)?;
        get(&classes, &object.class_id)?;
        match (&object.kind, &object.task) {
            (ObjectKind::Task, Some(task)) => {
                text(&task.name, "task name")?;
                let prefix = if object.project == ":" {
                    ":".to_owned()
                } else {
                    format!("{}:", object.project)
                };
                if task.name.contains(':') || task.path != format!("{prefix}{}", task.name) {
                    return Err(malformed(
                        "Task path/name differs from observed owning project",
                    ));
                }
            }
            (ObjectKind::Task, None) | (_, Some(_)) => {
                return Err(malformed("Task identity does not match object kind"));
            }
            (_, None) => {}
        }
    }
    let mut methods = BTreeMap::new();
    let mut signatures = BTreeMap::new();
    for catalogue in &context.catalogues {
        get(&classes, &catalogue.class_id)?;
        let mut catalogue_signatures = BTreeSet::new();
        for method in &catalogue.methods {
            text(&method.id, "method ID")?;
            text(&method.name, "method name")?;
            let arguments = descriptor(&method.descriptor)?;
            if arguments.len() != method.parameter_classes.len() {
                return Err(malformed("Incomplete reflected parameter class identities"));
            }
            for (argument, class) in arguments.iter().zip(&method.parameter_classes) {
                validate_descriptor_class(argument, class, &classes)?;
            }
            let returned = method
                .descriptor
                .rsplit(')')
                .next()
                .ok_or_else(|| malformed("Missing return descriptor"))?;
            validate_descriptor_class(returned, &method.return_class, &classes)?;
            if methods
                .insert(
                    method.id.as_str(),
                    MethodInfo {
                        catalogue: catalogue.id.as_str(),
                        method,
                        parameters: arguments,
                        returned,
                    },
                )
                .is_some()
                || !relations.contains(&catalogue.class_id, &method.declaring_class)?
            {
                return Err(malformed(
                    "Duplicate method ID or unrelated declaring class",
                ));
            }
            catalogue_signatures.insert((
                method.name.as_str(),
                method.descriptor.as_str(),
                method.is_static,
            ));
        }
        signatures.insert(catalogue.id.as_str(), catalogue_signatures);
    }
    if context.mode == CaptureMode::Discovery && !context.requests.is_empty() {
        return Err(malformed(
            "Discovery capture cannot assert getter invocations",
        ));
    }
    let selected_variants: BTreeSet<_> = context
        .binding
        .selected_variants
        .iter()
        .map(|variant| (variant.module.as_str(), variant.variant.as_str()))
        .collect();
    let mut model_calls = BTreeMap::new();
    let mut earlier = BTreeSet::new();
    for request in &context.requests {
        imports.project(&request.project)?;
        text(&request.model_call, "model call")?;
        let call_identity = (
            request.project.as_str(),
            request.consumer,
            &request.parameter,
        );
        if model_calls
            .insert(request.model_call.as_str(), call_identity)
            .is_some_and(|prior| prior != call_identity)
        {
            return Err(malformed(
                "Model call project/consumer/original parameter changed",
            ));
        }
        if let Some(variant) = &request.variant {
            if variant.module != request.project
                || !selected_variants.contains(&(variant.module.as_str(), variant.variant.as_str()))
            {
                return Err(malformed(
                    "Request variant differs from resolved current selection",
                ));
            }
        }
        let catalogue = get(&catalogues, &request.catalogue)?;
        let (name, is_static, reflected, parameters, returned) = match &request.method {
            MethodSelection::Selected(id) => {
                let info = methods
                    .get(id.as_str())
                    .ok_or_else(|| malformed("Requested method absent from its exact catalogue"))?;
                if info.catalogue != request.catalogue {
                    return Err(malformed("Method belongs to another catalogue"));
                }
                (
                    info.method.name.as_str(),
                    info.method.is_static,
                    Some(info.method),
                    Cow::Borrowed(info.parameters.as_slice()),
                    info.returned,
                )
            }
            MethodSelection::Missing(method) => {
                text(&method.name, "requested missing method")?;
                if signatures.get(catalogue.id.as_str()).is_some_and(|values| {
                    values.contains(&(
                        method.name.as_str(),
                        method.descriptor.as_str(),
                        method.is_static,
                    ))
                }) {
                    return Err(malformed("Requested missing method is actually present"));
                }
                let parameters = descriptor(&method.descriptor)?;
                let returned = method
                    .descriptor
                    .rsplit(')')
                    .next()
                    .ok_or_else(|| malformed("Missing return descriptor"))?;
                (
                    method.name.as_str(),
                    method.is_static,
                    None,
                    Cow::Owned(parameters),
                    returned,
                )
            }
        };
        let owner = request
            .owner
            .as_deref()
            .map(|id| get(&objects, id))
            .transpose()?;
        if is_static != owner.is_none()
            || owner.is_some_and(|owner| {
                owner.class_id != catalogue.class_id || owner.project != request.project
            })
        {
            return Err(malformed(
                "Getter static/instance/class/project owner mismatch",
            ));
        }
        if parameters.len() != request.arguments.len() {
            return Err(malformed(
                "Getter argument count differs from exact descriptor",
            ));
        }
        for (ordinal, (parameter, argument)) in
            parameters.iter().zip(&request.arguments).enumerate()
        {
            let matches = match argument {
                GetterArgument::String(_) => {
                    if let Some(method) = reflected {
                        let expected_class = method
                            .parameter_classes
                            .get(ordinal)
                            .and_then(Option::as_deref);
                        let string_class = jdk_classes.get("java.lang.String").copied();
                        match (string_class, expected_class) {
                            (Some(string_class), Some(expected_class)) => {
                                relations.assignable(&string_class.id, expected_class)?
                            }
                            _ => false,
                        }
                    } else {
                        matches!(*parameter, "Ljava/lang/String;" | "Ljava/lang/Object;")
                    }
                }
                GetterArgument::Boolean(_) => *parameter == "Z",
                GetterArgument::Null(()) => {
                    parameter.starts_with('L') || parameter.starts_with('[')
                }
                GetterArgument::Object(id) => {
                    let object = get(&objects, id)?;
                    if object.project != request.project {
                        return Err(malformed("Getter argument crosses project identity"));
                    }
                    if let Some(method) = reflected {
                        let expected_class = method
                            .parameter_classes
                            .get(ordinal)
                            .and_then(Option::as_deref)
                            .ok_or_else(|| {
                                malformed("Object argument lacks reflected parameter class")
                            })?;
                        relations.assignable(&object.class_id, expected_class)?
                    } else {
                        parameter.starts_with('L') || parameter.starts_with('[')
                    }
                }
            };
            if !matches {
                return Err(malformed(
                    "Getter argument shape differs from exact descriptor",
                ));
            }
        }
        if matches!(
            request.return_shape.kind,
            ValueKind::Object | ValueKind::Objects
        ) != request.return_shape.object_kind.is_some()
            || matches!(
                request.return_shape.kind,
                ValueKind::Strings | ValueKind::Objects
            ) != request.return_shape.order.is_some()
        {
            return Err(malformed("Invalid nullable/object return shape contract"));
        }
        if !returned.starts_with('L') && !returned.starts_with('[') {
            let expected_kind = if returned == "Z" {
                ValueKind::Boolean
            } else {
                ValueKind::Unsupported
            };
            // Method.invoke boxes non-void primitives and returns null for void.
            if request.return_shape.kind != expected_kind
                || request.return_shape.nullable != (returned == "V")
            {
                return Err(malformed(
                    "Primitive/void return differs from requested shape/nullability",
                ));
            }
        }
        if let Some(method) = reflected
            && let Some(returned_class) = method.return_class.as_deref()
        {
            if matches!(
                request.return_shape.kind,
                ValueKind::Strings | ValueKind::Objects
            ) {
                let declared = get(&classes, returned_class)?;
                if matches!(&declared.origin, ClassOrigin::Jdk(module) if module == "java.base")
                    && matches!(
                        declared.name.as_str(),
                        "java.lang.String" | "java.lang.Boolean"
                    )
                {
                    return Err(malformed("Final scalar return cannot supply a collection"));
                }
            }
            let scalar_class = match request.return_shape.kind {
                ValueKind::String => Some("java.lang.String"),
                ValueKind::Boolean => Some("java.lang.Boolean"),
                ValueKind::File => Some("java.io.File"),
                _ => None,
            };
            if let Some(scalar_class) = scalar_class {
                let actual = jdk_classes
                    .get(scalar_class)
                    .ok_or_else(|| malformed("Typed scalar lacks JDK class provenance"))?;
                if !relations.assignable(&actual.id, returned_class)? {
                    return Err(malformed(
                        "Requested scalar cannot inhabit the reflected return class",
                    ));
                }
            }
        }
        if let Some(parent) = &request.after {
            let parent_request = get(&requests, parent)?;
            if !earlier.contains(parent.as_str())
                || parent_request.project != request.project
                || parent_request.model_call != request.model_call
            {
                return Err(malformed(
                    "Getter dependency is later, cyclic or from another model call",
                ));
            }
        }
        if matches!(
            request.return_shape.order,
            Some(
                ContainerOrder::NamedDomainObjectAsMapValues | ContainerOrder::ProjectTaskMapValues
            )
        ) && request.purpose != GetterPurpose::ContainerIterate
        {
            return Err(malformed(
                "Derived container order lacks explicit getter chain",
            ));
        }
        match request.purpose {
            GetterPurpose::SourceSet
                if !owner.is_some_and(|value| value.kind == ObjectKind::Task)
                    || !name.starts_with("getSourceSetName")
                    || !parameters.is_empty() =>
            {
                return Err(malformed(
                    "Source-set request lacks exact task getter provenance",
                ));
            }
            GetterPurpose::PropertyGet
                if !owner.is_some_and(|value| value.kind == ObjectKind::Property)
                    || name != "get"
                    || !parameters.is_empty()
                    || request.after.is_none() =>
            {
                return Err(malformed(
                    "Property.get request lacks parent/object provenance",
                ));
            }
            GetterPurpose::ResolverInstance
                if !is_static
                    || name != "instance"
                    || request.return_shape.object_kind != Some(ObjectKind::Resolver) =>
            {
                return Err(malformed(
                    "Resolver instance request lacks static method provenance",
                ));
            }
            GetterPurpose::CompilerArguments
                if !owner.is_some_and(|value| value.kind == ObjectKind::Resolver)
                    || name != "resolveCompilerArguments"
                    || request.return_shape.kind != ValueKind::Strings
                    || request.after.is_none()
                    || !matches!(request.arguments.as_slice(), [GetterArgument::Object(id)] if get(&objects, id).is_ok_and(|value| value.kind == ObjectKind::Task)) =>
            {
                return Err(malformed(
                    "Compiler resolver request lacks task/service provenance",
                ));
            }
            GetterPurpose::TaskLookup
                if request.return_shape.object_kind != Some(ObjectKind::Task) =>
            {
                return Err(malformed("Task lookup return is not a typed task"));
            }
            GetterPurpose::ContainerGet
                if !owner.is_some_and(|value| value.kind == ObjectKind::Container)
                    || name != "getAsMap"
                    || !parameters.is_empty()
                    || request.return_shape.kind != ValueKind::Object
                    || request.return_shape.object_kind != Some(ObjectKind::Container)
                    || request.after.is_none() =>
            {
                return Err(malformed(
                    "Container getter lacks returned-object provenance",
                ));
            }
            GetterPurpose::ContainerIterate => {
                let owner = owner
                    .filter(|value| value.kind == ObjectKind::Container)
                    .ok_or_else(|| malformed("Container iteration lacks container owner"))?;
                let parent = request
                    .after
                    .as_deref()
                    .map(|id| get(&requests, id))
                    .transpose()?
                    .ok_or_else(|| malformed("Container iteration lacks prior getter"))?;
                let MethodSelection::Selected(parent_method) = &parent.method else {
                    return Err(malformed(
                        "Container iteration parent has no selected method",
                    ));
                };
                let parent_method = methods
                    .get(parent_method.as_str())
                    .ok_or_else(|| malformed("Missing container parent method"))?;
                let map = jdk_classes
                    .get("java.util.Map")
                    .ok_or_else(|| malformed("Container iteration lacks JDK Map provenance"))?;
                if !relations.contains(&owner.class_id, &map.id)? {
                    return Err(malformed(
                        "Container iteration receiver is not the captured Map",
                    ));
                }
                match request.return_shape.order {
                    Some(ContainerOrder::NamedDomainObjectAsMapValues) => {
                        let parent_owner = parent
                            .owner
                            .as_deref()
                            .map(|id| get(&objects, id))
                            .transpose()?
                            .ok_or_else(|| malformed("asMap getter lacks named container owner"))?;
                        if name != "values"
                            || !parameters.is_empty()
                            || parent.purpose != GetterPurpose::ContainerGet
                            || parent_method.method.name != "getAsMap"
                            || (!relations.contains_artifact_name(
                                &parent_owner.class_id,
                                "org.gradle.api.NamedDomainObjectContainer",
                            )? && !relations.contains_artifact_name(
                                &parent_owner.class_id,
                                "org.gradle.api.NamedDomainObjectCollection",
                            )?)
                        {
                            return Err(malformed(
                                "asMap.values iteration lacks exact named-container getter provenance",
                            ));
                        }
                    }
                    Some(ContainerOrder::ProjectTaskMapValues) => {
                        let parent_owner = parent
                            .owner
                            .as_deref()
                            .map(|id| get(&objects, id))
                            .transpose()?
                            .ok_or_else(|| malformed("Project task map lacks owning Project"))?;
                        if name != "get"
                            || !matches!(request.arguments.as_slice(), [GetterArgument::Object(id)] if id == &parent_owner.id)
                            || parent_owner.kind != ObjectKind::Project
                            || parent_method.method.name != "getAllTasks"
                            || !matches!(
                                parent.arguments.as_slice(),
                                [GetterArgument::Boolean(false)]
                            )
                        {
                            return Err(malformed(
                                "Task-map iteration lacks exact getAllTasks(false)[project] provenance",
                            ));
                        }
                    }
                    _ => {
                        return Err(malformed(
                            "Derived container iteration lacks an exact source order",
                        ));
                    }
                }
            }
            _ => {}
        }
        earlier.insert(request.id.as_str());
    }
    Ok(MethodIndex {
        methods,
        jdk_classes,
    })
}

fn validate_events<'a>(
    export: &'a KotlinExport,
    metadata: &MethodIndex<'a>,
    limits: CaptureLimits,
) -> FactsResult<()> {
    let context = &export.context;
    let objects = index(&context.objects, |value| &value.id)?;
    let classes = index(&context.runtime.classes, |value| &value.id)?;
    let mut relations = ClassRelations {
        classes: &classes,
        answers: BTreeMap::new(),
        named_answers: BTreeMap::new(),
        steps: 0,
        limit: limits.ancestry_steps,
    };
    let mut event_ids = BTreeSet::new();
    if export.events.len() != context.requests.len() {
        return Err(malformed(
            "Getter responses do not match requested invocation count",
        ));
    }
    let mut previous: BTreeMap<&str, &GetterEvent> = BTreeMap::new();
    for (request, event) in context.requests.iter().zip(&export.events) {
        text(&event.id, "event ID")?;
        if !event_ids.insert(&event.id) {
            return Err(malformed("Duplicate event identity"));
        }
        if event.request != request.id {
            return Err(malformed(
                "Getter response order/identity differs from request plan",
            ));
        }
        if let Some(parent) = &request.after {
            let parent = previous
                .get(parent.as_str())
                .ok_or_else(|| malformed("Missing prior getter response"))?;
            if matches!(
                request.purpose,
                GetterPurpose::PropertyGet
                    | GetterPurpose::CompilerArguments
                    | GetterPurpose::ContainerGet
                    | GetterPurpose::ContainerIterate
            ) && !matches!(&parent.outcome, GetterOutcome::Available(Some(CaptureValue::Object(id))) if request.owner.as_ref() == Some(id))
            {
                return Err(malformed(
                    "Getter parent did not return the requested receiver object",
                ));
            }
        }
        let collection = matches!(
            &event.outcome,
            GetterOutcome::Available(Some(CaptureValue::Strings(_) | CaptureValue::Objects(_)))
        );
        if collection != event.container.is_some() {
            return Err(malformed(
                "Collection outcome lacks or contradicts returned-container evidence",
            ));
        }
        if collection {
            validate_collection(event, request, &objects, metadata, &mut relations)?;
        }
        match &event.outcome {
            GetterOutcome::Available(value) => {
                if matches!(request.method, MethodSelection::Missing(_)) {
                    return Err(malformed(
                        "A missing method cannot return an available value",
                    ));
                }
                let kind = match value {
                    None if request.return_shape.nullable => {
                        previous.insert(&request.id, event);
                        continue;
                    }
                    None => return Err(malformed("Nonnullable getter returned null")),
                    Some(CaptureValue::String(_)) => ValueKind::String,
                    Some(CaptureValue::Boolean(_)) => ValueKind::Boolean,
                    Some(CaptureValue::Strings(_)) => ValueKind::Strings,
                    Some(CaptureValue::Object(id)) => {
                        validate_return_object(
                            id,
                            request,
                            &objects,
                            &metadata.methods,
                            &mut relations,
                        )?;
                        ValueKind::Object
                    }
                    Some(CaptureValue::Objects(ids)) => {
                        for id in ids {
                            validate_return_object(
                                id,
                                request,
                                &objects,
                                &metadata.methods,
                                &mut relations,
                            )?;
                        }
                        ValueKind::Objects
                    }
                    Some(CaptureValue::File(path)) => {
                        if path.to_str().is_none_or(|value| value.contains('\0')) {
                            return Err(malformed("Getter returned invalid File path"));
                        }
                        ValueKind::File
                    }
                };
                if kind != request.return_shape.kind {
                    return Err(malformed("Getter returned another typed shape"));
                }
            }
            GetterOutcome::Unavailable(value) => {
                text(&value.capability, "failure capability")?;
                if let Some(actual_class) = &value.actual_class {
                    if value.stage != GetterFailureStage::ReturnDecode {
                        return Err(malformed(
                            "Actual return class was reported before return decoding",
                        ));
                    }
                    validate_actual_class(actual_class, request, metadata, &mut relations)?;
                }
                if (value.kind == GetterFailureKind::MissingClass
                    && value.stage != GetterFailureStage::ClassLoad)
                    || (value.kind == GetterFailureKind::CatalogueChanged
                        && value.stage != GetterFailureStage::MethodDiscovery)
                {
                    return Err(malformed("Getter failure kind contradicts its stage"));
                }
                if value.exceptions.len() > limits.exception_causes {
                    return Err(failure(
                        FactsUnavailableReason::UnsupportedShape,
                        "Getter exception cause limit exceeded",
                    ));
                }
                for exception in &value.exceptions {
                    text(&exception.class, "exception class")?;
                }
                if value.kind == GetterFailureKind::UnsupportedReturnShape
                    && (value.stage != GetterFailureStage::ReturnDecode
                        || value.actual_class.as_deref().is_none_or(str::is_empty))
                {
                    return Err(malformed(
                        "Unsupported return shape requires actual return class",
                    ));
                }
                if matches!(request.method, MethodSelection::Missing(_))
                    && value.kind != GetterFailureKind::MissingMethod
                {
                    return Err(malformed(
                        "Missing method response lacks matching discovery failure",
                    ));
                }
                if value.kind == GetterFailureKind::MissingMethod
                    && (value.stage != GetterFailureStage::MethodDiscovery
                        || !matches!(request.method, MethodSelection::Missing(_)))
                {
                    return Err(malformed(
                        "Missing method failure contradicts requested catalogue",
                    ));
                }
                if matches!(
                    value.kind,
                    GetterFailureKind::Invocation
                        | GetterFailureKind::Linkage
                        | GetterFailureKind::Access
                        | GetterFailureKind::MissingClass
                ) && value.exceptions.is_empty()
                {
                    return Err(malformed(
                        "Getter exception failure omitted original exception class",
                    ));
                }
            }
        }
        previous.insert(&request.id, event);
    }
    Ok(())
}
fn validate_actual_class<'a>(
    actual_class: &str,
    request: &'a GetterRequest,
    metadata: &MethodIndex<'a>,
    relations: &mut ClassRelations<'a>,
) -> FactsResult<()> {
    let actual = get(relations.classes, actual_class)?;
    let MethodSelection::Selected(method) = &request.method else {
        return Err(malformed(
            "Missing method cannot report an actual return class",
        ));
    };
    let info = metadata
        .methods
        .get(method.as_str())
        .ok_or_else(|| malformed("Missing actual-return method"))?;
    if let Some(returned) = info.method.return_class.as_deref() {
        if !relations.assignable(&actual.id, returned)? {
            return Err(malformed(
                "Actual return class contradicts reflected return class",
            ));
        }
    } else {
        let boxed_name = match info.returned {
            "Z" => "java.lang.Boolean",
            "B" => "java.lang.Byte",
            "C" => "java.lang.Character",
            "S" => "java.lang.Short",
            "I" => "java.lang.Integer",
            "J" => "java.lang.Long",
            "F" => "java.lang.Float",
            "D" => "java.lang.Double",
            _ => {
                return Err(malformed(
                    "Void method cannot report a returned object class",
                ));
            }
        };
        let boxed = metadata
            .jdk_classes
            .get(boxed_name)
            .ok_or_else(|| malformed("Primitive return lacks captured JDK boxing class"))?;
        if actual.id != boxed.id {
            return Err(malformed(
                "Actual return class contradicts JDK reflection boxing",
            ));
        }
    }
    Ok(())
}

fn validate_collection<'a>(
    event: &GetterEvent,
    request: &'a GetterRequest,
    objects: &BTreeMap<&str, &'a CaptureObject>,
    metadata: &MethodIndex<'a>,
    relations: &mut ClassRelations<'a>,
) -> FactsResult<()> {
    let container = event
        .container
        .as_deref()
        .map(|id| get(objects, id))
        .transpose()?
        .ok_or_else(|| malformed("Missing collection identity"))?;
    if container.kind != ObjectKind::Container || container.project != request.project {
        return Err(malformed(
            "Returned collection crosses project or object kind",
        ));
    }
    let MethodSelection::Selected(method) = &request.method else {
        return Err(malformed("Missing getter cannot return a collection"));
    };
    let method = metadata
        .methods
        .get(method.as_str())
        .ok_or_else(|| malformed("Missing collection return method"))?
        .method;
    let returned = method
        .return_class
        .as_deref()
        .ok_or_else(|| malformed("Collection return lacks reflected class"))?;
    if !relations.assignable(&container.class_id, returned)? {
        return Err(malformed(
            "Returned collection is not assignable to reflected return class",
        ));
    }
    let expected_name = match request.return_shape.order {
        Some(ContainerOrder::List) => "java.util.List",
        Some(
            ContainerOrder::Iterable
            | ContainerOrder::NamedDomainObjectAsMapValues
            | ContainerOrder::ProjectTaskMapValues,
        ) => "java.lang.Iterable",
        None => return Err(malformed("Collection lacks iteration source")),
    };
    let expected = metadata
        .jdk_classes
        .get(expected_name)
        .ok_or_else(|| malformed("Collection lacks JDK iteration class provenance"))?;
    if !relations.contains(&container.class_id, &expected.id)? {
        return Err(malformed(
            "Observed collection does not support requested iteration source",
        ));
    }
    Ok(())
}

fn validate_return_object<'a>(
    id: &str,
    request: &'a GetterRequest,
    objects: &BTreeMap<&str, &'a CaptureObject>,
    methods: &BTreeMap<&str, MethodInfo<'a>>,
    relations: &mut ClassRelations<'a>,
) -> FactsResult<()> {
    let object = get(objects, id)?;
    if object.project != request.project || request.return_shape.object_kind != Some(object.kind) {
        return Err(malformed("Getter return object crosses project or kind"));
    }
    if request.return_shape.kind == ValueKind::Object
        && let MethodSelection::Selected(method) = &request.method
    {
        let method = methods
            .get(method.as_str())
            .ok_or_else(|| malformed("Missing return method"))?
            .method;
        let returned = method
            .return_class
            .as_deref()
            .ok_or_else(|| malformed("Object return lacks reflected class"))?;
        if !relations.assignable(&object.class_id, returned)? {
            return Err(malformed(
                "Returned object is not assignable to reflected return class",
            ));
        }
    }
    Ok(())
}
