/*
 * Copyright (C) 2017 The Android Open Source Project
 * Copyright (C) 2021 The Android Open Source Project
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

//! Evaluated sync facts retained under their original immutable model revision.
//! Light-class filtering adapts AOSP idea's GradleProjectSystemUtil.java131–181
//! and FilenameConstants.kt at a84efec3ba9542d9bfa1255103f0dc94833a3796.

use crate::{
    generated_artifacts::{
        ArtifactSlot, CapturedField, GeneratedArtifactSnapshot, ModelConsumerVersion,
        parse_generated_artifacts, parse_generated_artifacts_with_consumer,
    },
    import_facts::{
        GetterObservation, ImportFactsBinding, ImportFactsSnapshot, parse_import_facts,
    },
    module_presentation::{
        CapturedExternalSystemIdentity, CapturedGradleIdentity, CapturedModuleIdentity,
        ImportedGradleModuleType, resolve_module_presentation,
    },
    project_model::{
        EvaluatedModelPaths, MODEL_OUTPUT_PREFIX, ModelState, ModelToken, ModuleKind, ProjectModel,
        VariantId, parse_model, parse_model_with_context,
    },
    project_tree_adapter::{
        AdapterUnavailable, CapturedModulePresentation, KotlinCapability, ModuleRootPlan,
        prepare_module_roots_with_generated,
    },
    project_tree_facts::{FactsUnavailable, FactsUnavailableReason},
};
use anyhow::{Context as _, Result, ensure};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

type FactsResult<T> = std::result::Result<T, FactsUnavailable>;
const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;

fn unavailable(reason: FactsUnavailableReason, detail: impl Into<String>) -> FactsUnavailable {
    FactsUnavailable {
        reason,
        detail: detail.into(),
        path: None,
    }
}

#[derive(Clone, Debug)]
pub struct EvaluatedTreeInputs {
    token: ModelToken,
    model: Arc<ProjectModel>,
    record: Arc<str>,
    import_facts: FactsResult<ImportFactsSnapshot>,
    generated_artifacts: FactsResult<GeneratedArtifactSnapshot>,
    raw_generated_artifacts: FactsResult<serde_json::Value>,
    kotlin_capabilities: FactsResult<BTreeMap<String, KotlinModuleObservation>>,
}

impl EvaluatedTreeInputs {
    /// This is the production sync decoder. All sidecars refer to this one record.
    pub fn decode_sync(
        output: &str,
        root: &Path,
        paths: Option<&EvaluatedModelPaths>,
        token: &ModelToken,
    ) -> Result<Self> {
        let mut records = output
            .lines()
            .filter(|line| line.starts_with(MODEL_OUTPUT_PREFIX));
        let record = records
            .next()
            .context("Gradle returned no Android project model")?;
        ensure!(
            records.next().is_none(),
            "Gradle returned multiple Android project models"
        );
        ensure!(
            record.len() <= MAX_RECORD_BYTES,
            "Android project model exceeds the 16 MiB decoding limit"
        );
        let model = Arc::new(match paths {
            Some(paths) => parse_model_with_context(record, root, paths)?,
            None => parse_model(record, root)?,
        });
        let import_facts = parse_import_facts(
            record,
            &model,
            ImportFactsBinding {
                model_revision: token.revision(),
                selection_revision: token.revision(),
                selected_variants: Vec::new(),
            },
        );
        let generated_artifacts =
            parse_generated_artifacts_with_consumer(record, &model, token.revision(), None);
        let wire: serde_json::Value = serde_json::from_str(
            record
                .strip_prefix(MODEL_OUTPUT_PREFIX)
                .context("Android model record prefix is missing")?,
        )?;
        let raw_generated_artifacts = match wire.get("generatedArtifacts") {
            Some(value) if value.is_object() => Ok(value.clone()),
            Some(_) => Err(unavailable(
                FactsUnavailableReason::Malformed,
                "generatedArtifacts must be an object",
            )),
            None => Err(unavailable(
                FactsUnavailableReason::MissingMetadata,
                "generatedArtifacts is absent from the legacy model",
            )),
        };
        let kotlin_capabilities = decode_kotlin_capabilities(&wire, &model);
        Ok(Self {
            token: token.clone(),
            model,
            record: Arc::from(record),
            import_facts,
            generated_artifacts,
            raw_generated_artifacts,
            kotlin_capabilities,
        })
    }

    pub fn token(&self) -> &ModelToken {
        &self.token
    }
    pub fn model(&self) -> &Arc<ProjectModel> {
        &self.model
    }
    pub fn raw_record(&self) -> &str {
        &self.record
    }
    pub fn import_facts(&self) -> FactsResult<&ImportFactsSnapshot> {
        self.import_facts.as_ref().map_err(Clone::clone)
    }
    pub fn raw_generated_artifacts(&self) -> FactsResult<&serde_json::Value> {
        self.raw_generated_artifacts.as_ref().map_err(Clone::clone)
    }

    pub fn kotlin_capability(&self, module: &str) -> FactsResult<KotlinCapability> {
        let modules = self.kotlin_capabilities.as_ref().map_err(Clone::clone)?;
        let observation = modules.get(module).ok_or_else(|| {
            unavailable(
                FactsUnavailableReason::MissingMetadata,
                "Selected Kotlin capability is unavailable",
            )
        })?;
        observation.effective()
    }

    /// This general decoder requires an explicit consumer. The live tree opts
    /// into its pinned schema through prepare_live_module_plan; other callers
    /// retain raw facts rather than inferring support from the producer minimum.
    pub fn generated_artifacts(
        &self,
        consumer: Option<&ModelConsumerVersion>,
    ) -> FactsResult<GeneratedArtifactSnapshot> {
        match consumer {
            Some(consumer) => parse_generated_artifacts(
                &self.record,
                &self.model,
                self.token.revision(),
                consumer,
            ),
            None => self.generated_artifacts.clone(),
        }
    }
}

/// Independent kind outcomes retain resources/assets when source filtering is
/// unavailable. A complete tree plan must require every relevant kind to succeed.
#[derive(Clone, Debug)]
pub struct SelectedMainGeneratedRoots {
    capture: Arc<EvaluatedTreeInputs>,
    selection: ModelToken,
    selected: VariantId,
    java: FactsResult<Vec<PathBuf>>,
    resources: FactsResult<Vec<PathBuf>>,
    assets: FactsResult<Vec<PathBuf>>,
}

impl SelectedMainGeneratedRoots {
    pub fn selected(&self) -> &VariantId {
        &self.selected
    }
    pub fn model_revision(&self) -> u64 {
        self.capture.token.revision()
    }
    pub fn selection_revision(&self) -> u64 {
        self.selection.revision()
    }
    pub fn model(&self) -> &ProjectModel {
        &self.capture.model
    }
    pub fn java(&self) -> FactsResult<&[PathBuf]> {
        self.java.as_deref().map_err(Clone::clone)
    }
    pub fn resources(&self) -> FactsResult<&[PathBuf]> {
        self.resources.as_deref().map_err(Clone::clone)
    }
    pub fn assets(&self) -> FactsResult<&[PathBuf]> {
        self.assets.as_deref().map_err(Clone::clone)
    }

    pub fn ensure_current(&self, state: &ModelState) -> FactsResult<()> {
        let current = state
            .evaluated_inputs()
            .is_some_and(|capture| Arc::ptr_eq(capture, &self.capture));
        let model = state
            .model
            .as_ref()
            .is_some_and(|model| Arc::ptr_eq(model, &self.capture.model));
        let selected = state.selected.as_ref().is_some_and(|selected| {
            Arc::ptr_eq(&selected.model, &self.capture.model)
                && selected.variants.get(&self.selected.module) == Some(&self.selected.variant)
        });
        if !current
            || !model
            || !selected
            || !state.is_current(&self.selection)
            || state.model_revision() != Some(self.model_revision())
        {
            return Err(unavailable(
                FactsUnavailableReason::Stale,
                "Selected generated roots belong to an outdated model/root/selection",
            ));
        }
        Ok(())
    }
}

pub fn prepare_selected_main_generated_roots(
    state: &ModelState,
    selection: &ModelToken,
    selected: VariantId,
    consumer: &ModelConsumerVersion,
) -> FactsResult<SelectedMainGeneratedRoots> {
    let capture = state
        .evaluated_inputs()
        .ok_or_else(|| {
            unavailable(
                FactsUnavailableReason::MissingMetadata,
                "Evaluated Android tree inputs are unavailable",
            )
        })?
        .clone();
    let mut roots = SelectedMainGeneratedRoots {
        capture,
        selection: selection.clone(),
        selected,
        java: Ok(Vec::new()),
        resources: Ok(Vec::new()),
        assets: Ok(Vec::new()),
    };
    roots.ensure_current(state)?;
    let snapshot = roots.capture.generated_artifacts(Some(consumer))?;
    snapshot.ensure_current(&roots.capture.model, roots.model_revision())?;
    let module = snapshot
        .modules()
        .iter()
        .find(|module| module.module == roots.selected.module)
        .ok_or_else(|| {
            unavailable(
                FactsUnavailableReason::MissingMetadata,
                "Selected generated module is unavailable",
            )
        })?;
    let versions = module.versions.available()?;
    let variant = snapshot.variant(&roots.selected.module, &roots.selected.variant)?;
    let ArtifactSlot::Present(artifact) = variant.main.available()? else {
        return Err(unavailable(
            FactsUnavailableReason::UnsupportedShape,
            "Selected V2 main artifact is absent",
        ));
    };
    roots.java = module.build_folder.available().and_then(|build_folder| {
        artifact
            .generated_source_folders
            .available()
            .map(|folders| filter_generated_java(folders, build_folder))
    });
    roots.resources = artifact.generated_resource_folders.available().cloned();
    roots.assets = artifact.generated_assets(versions).map(<[PathBuf]>::to_vec);
    Ok(roots)
}

fn filter_generated_java(folders: &[PathBuf], build_folder: &Path) -> Vec<PathBuf> {
    let generated = build_folder.join("generated");
    let excluded = [
        generated.join("source/r"),
        generated.join("not_namespaced_r_class_sources"),
        generated.join("data_binding_base_class_source_out"),
        generated.join("source/navigation-args"),
    ];
    folders
        .iter()
        .filter(|folder| !excluded.iter().any(|excluded| folder.starts_with(excluded)))
        .cloned()
        .collect()
}

const SDK_PLUGIN_VERSION_GETTER: &str = "com.android.build.api.AndroidPluginVersion.getMajor/getMinor/getMicro/getPreview/getPreviewType/getVersion";
const KOTLIN_ANDROID_GETTER: &str =
    "org.gradle.api.plugins.PluginManager.hasPlugin(org.jetbrains.kotlin.android)";
const KOTLIN_MULTIPLATFORM_GETTER: &str =
    "org.gradle.api.plugins.PluginManager.hasPlugin(org.jetbrains.kotlin.multiplatform)";
const KOTLIN_MULTIPLATFORM_TARGET_GETTER: &str =
    "KotlinMultiplatformExtension.getTargets().getPlatformType(androidJvm)";
const BUILT_IN_KOTLIN_GETTER: &str = "AGP9.4.0:BuiltInKotlinServicesKt.builtInKotlinEnabledForProject(ProjectServices,CommonExtension)";
const BUILT_IN_KOTLIN_DEFAULT_GETTER: &str =
    "AndroidProject.flags.getFlagValue(BUILT_IN_KOTLIN_DEFAULT_ENABLED)";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SdkPluginVersionObservation {
    major: i32,
    minor: i32,
    micro: i32,
    preview: i32,
    preview_type: Option<String>,
    version: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KotlinModuleObservation {
    module: String,
    directory: PathBuf,
    agp_version: String,
    #[serde(default)]
    sdk_plugin_version: Option<GetterObservation<SdkPluginVersionObservation>>,
    kotlin_android: GetterObservation<bool>,
    kotlin_multiplatform: GetterObservation<bool>,
    kotlin_multiplatform_android_target: GetterObservation<bool>,
    built_in_kotlin: GetterObservation<bool>,
    built_in_kotlin_default: GetterObservation<bool>,
}

impl KotlinModuleObservation {
    fn effective(&self) -> FactsResult<KotlinCapability> {
        // A positive applied KGP observation applies only to the real Android
        // modules bound below, never to an ecosystem-only KMP project.
        if matches!(self.kotlin_android.result, CapturedField::Available(true)) {
            return Ok(KotlinCapability::Enabled);
        }
        self.kotlin_android.available()?;
        if *self.kotlin_multiplatform.available()?
            && *self.kotlin_multiplatform_android_target.available()?
        {
            return Ok(KotlinCapability::Enabled);
        }
        // This version-pinned adapter captures the effective per-module getter,
        // including DSL enableKotlin. The V2 default flag is retained separately.
        let sdk = self
            .sdk_plugin_version
            .as_ref()
            .ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::Capability,
                    "Structured AndroidPluginVersion was not observed",
                )
            })?
            .available()?;
        if (sdk.major, sdk.minor, sdk.micro, sdk.preview) != (9, 4, 0, 0)
            || sdk.version != "9.4.0"
            || self.agp_version != sdk.version
        {
            return Err(unavailable(
                FactsUnavailableReason::Capability,
                "Effective built-in Kotlin has no proven adapter for this AGP version",
            ));
        }
        Ok(if *self.built_in_kotlin.available()? {
            KotlinCapability::Enabled
        } else {
            KotlinCapability::Disabled
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KotlinCapabilitiesExport {
    schema: u32,
    root: PathBuf,
    modules: Vec<KotlinModuleObservation>,
}

fn validate_observation<T>(observation: &GetterObservation<T>, getter: &str) -> FactsResult<()> {
    if observation.getter != getter {
        return Err(unavailable(
            FactsUnavailableReason::Malformed,
            format!("Kotlin capability getter provenance differs from {getter}"),
        ));
    }
    if let CapturedField::Unavailable(failure) = &observation.result {
        if failure.capability != getter || failure.detail.is_empty() || failure.detail.len() > 4096
        {
            return Err(unavailable(
                FactsUnavailableReason::Malformed,
                "Kotlin capability failure has invalid getter provenance",
            ));
        }
    }
    Ok(())
}

fn decode_kotlin_capabilities(
    wire: &serde_json::Value,
    model: &ProjectModel,
) -> FactsResult<BTreeMap<String, KotlinModuleObservation>> {
    let value = wire.get("kotlinCapabilities").ok_or_else(|| {
        unavailable(
            FactsUnavailableReason::MissingMetadata,
            "Legacy sync has no evaluated Kotlin capabilities",
        )
    })?;
    if !value.is_object()
        || value
            .get("modules")
            .and_then(|value| value.as_array())
            .is_none_or(|modules| modules.iter().any(|module| !module.is_object()))
    {
        return Err(unavailable(
            FactsUnavailableReason::Malformed,
            "Kotlin capability sidecar and its modules must be objects",
        ));
    }
    if value["modules"].as_array().is_some_and(|modules| {
        modules.iter().any(|module| {
            module
                .get("sdkPluginVersion")
                .and_then(|observation| observation.get("result"))
                .is_some_and(|result| {
                    result["status"] == "available" && !result["value"].is_object()
                })
        })
    }) {
        return Err(unavailable(
            FactsUnavailableReason::Malformed,
            "Structured SDK version payload must be an object",
        ));
    }
    let export: KotlinCapabilitiesExport = serde_json::from_value(value.clone())
        .map_err(|failure| unavailable(FactsUnavailableReason::Malformed, failure.to_string()))?;
    if export.schema != 1 {
        return Err(unavailable(
            FactsUnavailableReason::UnsupportedSchema,
            "Unsupported Kotlin capability sidecar schema",
        ));
    }
    if export.root != model.root {
        return Err(unavailable(
            FactsUnavailableReason::Stale,
            "Kotlin capability sidecar belongs to a different build root",
        ));
    }
    let android_modules = model
        .modules
        .iter()
        .filter(|module| module.kind != ModuleKind::Jvm)
        .map(|module| (module.path.as_str(), module))
        .collect::<BTreeMap<_, _>>();
    let mut modules = BTreeMap::new();
    for observation in export.modules {
        let module = android_modules
            .get(observation.module.as_str())
            .ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::Stale,
                    "Kotlin capability module is absent from the Android model",
                )
            })?;
        if module.kind == ModuleKind::Jvm || module.directory != observation.directory {
            return Err(unavailable(
                FactsUnavailableReason::Stale,
                "Kotlin capability is not bound to the observed Android module directory",
            ));
        }
        if observation.agp_version.is_empty()
            || observation.agp_version.len() > 4096
            || observation.agp_version.trim() != observation.agp_version
        {
            return Err(unavailable(
                FactsUnavailableReason::Malformed,
                "Invalid Kotlin capability AGP version",
            ));
        }
        if let Some(sdk) = &observation.sdk_plugin_version {
            validate_observation(sdk, SDK_PLUGIN_VERSION_GETTER)?;
            if let CapturedField::Available(sdk) = &sdk.result {
                if [sdk.major, sdk.minor, sdk.micro, sdk.preview]
                    .iter()
                    .any(|value| *value < 0)
                    || sdk.version != observation.agp_version
                    || sdk
                        .preview_type
                        .as_ref()
                        .is_some_and(|value| value.len() > 4096)
                {
                    return Err(unavailable(
                        FactsUnavailableReason::Malformed,
                        "Structured AndroidPluginVersion contradicts the raw SDK version",
                    ));
                }
            }
        }
        validate_observation(&observation.kotlin_android, KOTLIN_ANDROID_GETTER)?;
        validate_observation(
            &observation.kotlin_multiplatform,
            KOTLIN_MULTIPLATFORM_GETTER,
        )?;
        validate_observation(
            &observation.kotlin_multiplatform_android_target,
            KOTLIN_MULTIPLATFORM_TARGET_GETTER,
        )?;
        validate_observation(&observation.built_in_kotlin, BUILT_IN_KOTLIN_GETTER)?;
        validate_observation(
            &observation.built_in_kotlin_default,
            BUILT_IN_KOTLIN_DEFAULT_GETTER,
        )?;
        if modules
            .insert(observation.module.clone(), observation)
            .is_some()
        {
            return Err(unavailable(
                FactsUnavailableReason::Malformed,
                "Duplicate Kotlin capability module",
            ));
        }
    }
    if model
        .modules
        .iter()
        .filter(|module| module.kind != ModuleKind::Jvm)
        .any(|module| !modules.contains_key(&module.path))
    {
        return Err(unavailable(
            FactsUnavailableReason::MissingMetadata,
            "Kotlin capability catalogue omits an Android module",
        ));
    }
    Ok(modules)
}

/// This importer creates one holder per evaluated Gradle project. Its stable
/// internal name is rootName + buildTreePath (rootName for ':'), without
/// IntelliJ source-set modules, name escaping or Idea-plugin overrides. This is
/// an explicit Rust importer adaptation; original importer/facet tests remain unported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RustImportedModuleIdentity {
    pub internal_name: String,
    pub holder_internal_name: String,
    pub external_project_id: String,
    pub external_project_path: PathBuf,
    pub external_root_project_path: PathBuf,
}

/// The pinned AGP9.4.0/producer23.0/AndroidProject0.1 subset uses SDK consumer
/// ordinal66.1. MINIMUM_MODEL_CONSUMER is a Studio release compatibility ordinal
/// (ModelBuilder.kt270–283), not our wire schema. This static adapter profile
/// covers tree getters only; it does not promise full Studio compatibility or
/// derive its support from the producer's current minimum. The live plan checks
/// the exact supported tuple and rejects any higher minimum or unknown schema.
pub fn rust_v2_tree_consumer() -> ModelConsumerVersion {
    ModelConsumerVersion {
        major: 66,
        minor: 1,
        description: Some("Koda Rust tree subset: AGP9.4.0/producer23.0/AndroidProject0.1".into()),
    }
}

#[derive(Clone, Debug)]
pub struct PreparedLiveModulePlan {
    generated: SelectedMainGeneratedRoots,
    plan: Arc<ModuleRootPlan>,
    imported_identity: RustImportedModuleIdentity,
}

impl PreparedLiveModulePlan {
    pub fn plan(&self) -> &Arc<ModuleRootPlan> {
        &self.plan
    }
    pub fn model_token(&self) -> &ModelToken {
        &self.generated.selection
    }
    pub fn selected(&self) -> &VariantId {
        self.generated.selected()
    }
    pub fn imported_identity(&self) -> &RustImportedModuleIdentity {
        &self.imported_identity
    }
    pub fn ensure_current(&self, state: &ModelState) -> FactsResult<()> {
        self.generated.ensure_current(state)
    }
}

pub fn prepare_live_module_plan(
    state: &ModelState,
    selection: &ModelToken,
    selected: VariantId,
    compact_packages: bool,
) -> std::result::Result<PreparedLiveModulePlan, AdapterUnavailable> {
    let generated = prepare_selected_main_generated_roots(
        state,
        selection,
        selected,
        &rust_v2_tree_consumer(),
    )?;
    let capture = &generated.capture;
    let snapshot = capture.generated_artifacts(Some(&rust_v2_tree_consumer()))?;
    let generated_module = snapshot
        .modules()
        .iter()
        .find(|module| module.module == generated.selected.module)
        .ok_or_else(|| {
            unavailable(
                FactsUnavailableReason::MissingMetadata,
                "Selected generated module is absent",
            )
        })?;
    let versions = generated_module.versions.available()?;
    if versions.agp != "9.4.0"
        || (versions.producer.major, versions.producer.minor) != (23, 0)
        || versions
            .models
            .iter()
            .find(|model| model.name == "android_project")
            .is_none_or(|model| (model.version.major, model.version.minor) != (0, 1))
    {
        return Err(unavailable(
            FactsUnavailableReason::UnsupportedSchema,
            "Rust live tree supports AGP9.4.0/producer23.0/AndroidProject0.1 only",
        )
        .into());
    }
    let kotlin_modules = capture.kotlin_capabilities.as_ref().map_err(Clone::clone)?;
    let kotlin_module = kotlin_modules
        .get(&generated.selected.module)
        .ok_or_else(|| {
            unavailable(
                FactsUnavailableReason::MissingMetadata,
                "Selected Kotlin capability is absent",
            )
        })?;
    if kotlin_module.agp_version != versions.agp {
        return Err(unavailable(
            FactsUnavailableReason::Stale,
            "Kotlin capability and generated models report different AGP versions",
        )
        .into());
    }
    let kotlin = kotlin_module.effective()?;
    let imports = capture.import_facts()?;
    imports.ensure_current(&capture.model, imports.binding())?;
    let project = imports.project(&generated.selected.module)?;
    let required = |value: &Option<GetterObservation<String>>| -> FactsResult<String> {
        value
            .as_ref()
            .ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::MissingMetadata,
                    "Importer identity getter was not observed",
                )
            })?
            .available()
            .cloned()
    };
    let root_name = required(&project.root_name)?;
    let path = required(&project.project_path)?;
    let build_tree_path = required(&project.build_tree_path)?;
    let internal_name = if path == ":" {
        root_name.clone()
    } else {
        format!("{root_name}{build_tree_path}")
    };
    let directory = project
        .project_directory
        .as_ref()
        .ok_or_else(|| {
            unavailable(
                FactsUnavailableReason::MissingMetadata,
                "Importer project directory getter is missing",
            )
        })?
        .available()?
        .clone();
    let root = project
        .root_directory
        .as_ref()
        .ok_or_else(|| {
            unavailable(
                FactsUnavailableReason::MissingMetadata,
                "Importer root directory getter is missing",
            )
        })?
        .available()?
        .clone();
    let imported_identity = RustImportedModuleIdentity {
        holder_internal_name: internal_name.clone(),
        internal_name,
        external_project_id: if path == ":" {
            root_name
        } else {
            build_tree_path
        },
        external_project_path: directory,
        external_root_project_path: root,
    };
    let directory_text = imported_identity
        .external_project_path
        .to_str()
        .ok_or_else(|| {
            unavailable(
                FactsUnavailableReason::UnsupportedShape,
                "Importer directory is not UTF-8",
            )
        })?;
    let root_text = imported_identity
        .external_root_project_path
        .to_str()
        .ok_or_else(|| {
            unavailable(
                FactsUnavailableReason::UnsupportedShape,
                "Importer root directory is not UTF-8",
            )
        })?;
    let identity = CapturedModuleIdentity {
        internal_name: &imported_identity.internal_name,
        holder_internal_name: &imported_identity.holder_internal_name,
        external_system: CapturedExternalSystemIdentity::Gradle(CapturedGradleIdentity {
            module_type: ImportedGradleModuleType::Project,
            external_project_id: Some(&imported_identity.external_project_id),
            external_project_path: Some(directory_text),
            external_root_project_path: Some(root_text),
        }),
    };
    let resolved = resolve_module_presentation(&identity).map_err(|failure| {
        unavailable(FactsUnavailableReason::MissingMetadata, failure.to_string())
    })?;
    let presentation = CapturedModulePresentation {
        display_name: Some(resolved.display_name.into()),
        kotlin,
        compact_packages,
    };
    let plan = Arc::new(prepare_module_roots_with_generated(
        &generated,
        state,
        Some(&presentation),
    )?);
    generated.ensure_current(state)?;
    Ok(PreparedLiveModulePlan {
        generated,
        plan,
        imported_identity,
    })
}
