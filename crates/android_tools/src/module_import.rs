// Copyright 2000-2026 JetBrains s.r.o. and contributors.
// Copyright (C) 2024 The Android Open Source Project
// Licensed under the Apache License, Version 2.0.
// See test_data/module_import/NOTICE.md for adapted source and test provenance.

//! Rust import naming and transactional linked Kotlin membership over evaluated getters.
//! Getter availability is a capability boundary, not a plugin or filename heuristic.

use crate::{
    generated_artifacts::CapturedField,
    import_facts::{
        GetterObservation, ImportFactsBinding, ImportFactsSnapshot, ObjectOnly,
        validate_observation,
    },
    kotlin_import_facts::{
        CaptureContext, CaptureMode, CaptureObject, CaptureValue, ContainerOrder, GetterArgument,
        GetterMethod, GetterOutcome, GetterPurpose, GetterRequest, KotlinFactsSnapshot,
        MethodSelection, ObjectKind, RuntimeClass,
    },
    module_presentation::{
        CapturedExternalSystemIdentity, CapturedGradleIdentity, CapturedModuleIdentity,
        ImportedGradleModuleType, resolve_module_presentation,
    },
    project_model::{Module, ModuleKind, ProjectModel, VariantId},
    project_tree_adapter::{
        AdapterUnavailable, CapturedModulePresentation, KotlinCapability, ModuleRootPlan,
        prepare_imported_module_roots,
    },
    project_tree_facts::{FactsUnavailable, FactsUnavailableReason},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;
pub const PLUGIN_IDS: &str =
    "org.gradle.api.plugins.PluginContainer.findPlugin(reference Kotlin plugin IDs)";
pub const PLUGIN_INTERFACES: &str = "org.gradle.api.Project.getPlugins() runtime interfaces";
pub const KOTLIN_EXTENSION: &str = "org.gradle.api.plugins.ExtensionContainer.findByName(kotlin)";
pub const COMPILER_VERSION: &str = "org.jetbrains.kotlin.gradle.plugin.KotlinPluginWrapperKt.getKotlinPluginVersion(org.gradle.api.Project)";
pub const COMPILE_TASKS: &str = "org.gradle.api.Project.getAllTasks(false)";
pub const SOURCE_SET_NAME: &str = "org.jetbrains.kotlin.gradle.tasks.getSourceSetName*()";
pub const COMPILER_ARGUMENTS: &str = "org.jetbrains.kotlin.gradle.plugin.ide.IdeCompilerArgumentsResolver.instance(Project).resolveCompilerArguments(task)";

type ImportResult<T> = Result<T, FactsUnavailable>;

fn unavailable(reason: FactsUnavailableReason, detail: impl Into<String>) -> FactsUnavailable {
    FactsUnavailable {
        reason,
        detail: detail.into(),
        path: None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(remote = "Self", rename_all = "camelCase", deny_unknown_fields)]
pub struct RawKotlinTask {
    pub path: String,
    pub class_name: String,
    pub source_set_name: GetterObservation<Option<String>>,
    pub compiler_arguments: GetterObservation<Option<Vec<String>>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(remote = "Self", rename_all = "camelCase", deny_unknown_fields)]
pub struct RawKotlinProject {
    pub project_path: String,
    pub plugin_ids: GetterObservation<Vec<String>>,
    pub plugin_interfaces: GetterObservation<Vec<String>>,
    pub kotlin_extension: GetterObservation<bool>,
    pub compiler_version: GetterObservation<Option<String>>,
    pub compile_tasks: GetterObservation<Vec<RawKotlinTask>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
struct ModuleIdentity {
    module: String,
    directory: PathBuf,
    kind: ModuleKind,
    variants: Vec<String>,
}

#[derive(Deserialize)]
#[serde(remote = "Self", rename_all = "camelCase", deny_unknown_fields)]
struct KotlinExport {
    schema: u32,
    root: PathBuf,
    modules: Vec<ModuleIdentity>,
    projects: Vec<RawKotlinProject>,
}

macro_rules! object_decode {
    ($($name:ident),+ $(,)?) => {$(
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                $name::deserialize(ObjectOnly(deserializer))
            }
        }
    )+};
}
object_decode!(
    RawKotlinTask,
    RawKotlinProject,
    ModuleIdentity,
    KotlinExport
);

#[derive(Clone, Debug)]
pub struct KotlinImportFacts {
    root: PathBuf,
    binding: ImportFactsBinding,
    identity: Vec<ModuleIdentity>,
    projects: BTreeMap<String, RawKotlinProject>,
    capture_context: Option<CaptureContext>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct StrictKotlinProjectPlan {
    pub project: String,
    pub plugin_lookups: BTreeMap<String, String>,
    pub plugin_iteration: String,
    pub extension_lookup: String,
    pub compiler_version: String,
    pub task_iteration: String,
    pub source_set_names: BTreeMap<String, String>,
    pub compiler_arguments: BTreeMap<String, String>,
}

const REFERENCE_PLUGIN_IDS: &[&str] = &[
    "kotlin",
    "kotlin2js",
    "kotlin-android",
    "kotlin-platform-jvm",
    "kotlin-platform-js",
    "kotlin-platform-common",
    "org.jetbrains.kotlin.multiplatform",
    "kotlin-multiplatform",
    "com.android.application",
    "com.android.library",
    "com.android.dynamic-feature",
    "com.android.test",
    "com.android.kotlin.multiplatform.library",
];

struct StrictProjection<'a> {
    events: BTreeMap<&'a str, &'a GetterOutcome>,
    requests: BTreeMap<&'a str, &'a GetterRequest>,
    objects: BTreeMap<&'a str, &'a CaptureObject>,
    classes: BTreeMap<&'a str, &'a RuntimeClass>,
    methods: BTreeMap<(&'a str, &'a str), &'a GetterMethod>,
}

impl<'a> StrictProjection<'a> {
    fn request(&self, id: &str, project: &str) -> ImportResult<&'a GetterRequest> {
        self.requests
            .get(id)
            .copied()
            .filter(|request| request.project == project)
            .ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::MissingMetadata,
                    "No independently planned getter request",
                )
            })
    }

    fn method_name(&self, request: &GetterRequest) -> ImportResult<&str> {
        let method_id = match &request.method {
            MethodSelection::Selected(id) => id,
            MethodSelection::Missing(_) => {
                return Err(unavailable(
                    FactsUnavailableReason::Capability,
                    "Requested official getter is unavailable",
                ));
            }
        };
        self.methods
            .get(&(request.catalogue.as_str(), method_id.as_str()))
            .copied()
            .map(|method| method.name.as_str())
            .ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::MissingMetadata,
                    "Requested method is not in captured runtime catalogue",
                )
            })
    }

    fn outcome(&self, request: &GetterRequest) -> ImportResult<Option<&'a CaptureValue>> {
        match self.events.get(request.id.as_str()) {
            Some(GetterOutcome::Available(value)) => Ok(value.as_ref()),
            Some(GetterOutcome::Unavailable(failure)) => Err(unavailable(
                FactsUnavailableReason::Capability,
                failure.detail.clone(),
            )),
            None => Err(unavailable(
                FactsUnavailableReason::MissingMetadata,
                "Requested getter has no event",
            )),
        }
    }

    fn class_names(&self, object: &str) -> ImportResult<BTreeSet<&'a str>> {
        let object = self.objects.get(object).copied().ok_or_else(|| {
            unavailable(
                FactsUnavailableReason::MissingMetadata,
                "Unknown captured runtime object",
            )
        })?;
        let mut pending = vec![object.class_id.as_str()];
        let mut visited = BTreeSet::new();
        let mut names = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id) {
                continue;
            }
            let class = self.classes.get(id).copied().ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::MissingMetadata,
                    "Unknown captured runtime class",
                )
            })?;
            names.insert(class.name.as_str());
            pending.extend(class.superclass.as_deref());
            pending.extend(class.interfaces.iter().map(String::as_str));
        }
        Ok(names)
    }

    fn nullable_object(&self, request: &GetterRequest) -> ImportResult<Option<&'a str>> {
        match self.outcome(request)? {
            Some(CaptureValue::Object(id)) => Ok(Some(id.as_str())),
            None => Ok(None),
            _ => Err(unavailable(
                FactsUnavailableReason::Malformed,
                "Official getter returned a different shape",
            )),
        }
    }

    fn objects(&self, request: &GetterRequest) -> ImportResult<&'a [String]> {
        match self.outcome(request)? {
            Some(CaptureValue::Objects(objects)) => Ok(objects),
            _ => Err(unavailable(
                FactsUnavailableReason::Capability,
                "Official object collection is unavailable or null",
            )),
        }
    }

    fn observed<T>(&self, getter: &str, value: ImportResult<T>) -> GetterObservation<T> {
        GetterObservation {
            getter: getter.into(),
            result: match value {
                Ok(value) => CapturedField::Available(value),
                Err(error) => {
                    CapturedField::Unavailable(crate::generated_artifacts::GetterUnavailable {
                        capability: getter.into(),
                        detail: error.to_string(),
                    })
                }
            },
        }
    }

    fn check_container(&self, request: &GetterRequest, interface: &str) -> ImportResult<()> {
        let owner = request.owner.as_deref().ok_or_else(|| {
            unavailable(
                FactsUnavailableReason::MissingMetadata,
                "Official getter lacks owning container",
            )
        })?;
        if !self.class_names(owner)?.contains(interface) {
            return Err(unavailable(
                FactsUnavailableReason::Malformed,
                "Getter owner is not the official Gradle container",
            ));
        }
        let parent = self.request(
            request.after.as_deref().ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::MissingMetadata,
                    "Container lacks Project getter provenance",
                )
            })?,
            &request.project,
        )?;
        let expected = if interface == "org.gradle.api.plugins.PluginContainer" {
            "getPlugins"
        } else {
            "getExtensions"
        };
        let project_owner = parent
            .owner
            .as_deref()
            .and_then(|id| self.objects.get(id).copied());
        if self.method_name(parent)? != expected
            || !parent.arguments.is_empty()
            || !project_owner.is_some_and(|object| object.kind == ObjectKind::Project)
            || self.nullable_object(parent)? != Some(owner)
        {
            return Err(unavailable(
                FactsUnavailableReason::Malformed,
                "Container is not the owning Project's official getter result",
            ));
        }
        Ok(())
    }

    fn project(&self, plan: &StrictKotlinProjectPlan) -> ImportResult<RawKotlinProject> {
        if plan
            .plugin_lookups
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            != REFERENCE_PLUGIN_IDS.iter().copied().collect()
        {
            return Err(unavailable(
                FactsUnavailableReason::MissingMetadata,
                "Plugin absence requires every reference Kotlin/Android lookup",
            ));
        }
        let ids = (|| {
            let mut present = Vec::new();
            for (plugin, id) in &plan.plugin_lookups {
                let request = self.request(id, &plan.project)?;
                if self.method_name(request)? != "findPlugin"
                    || request.arguments != [GetterArgument::String(plugin.clone())]
                    || request.return_shape.object_kind != Some(ObjectKind::Plugin)
                {
                    return Err(unavailable(
                        FactsUnavailableReason::Malformed,
                        "Plugin lookup does not match its official request",
                    ));
                }
                self.check_container(request, "org.gradle.api.plugins.PluginContainer")?;
                if self.nullable_object(request)?.is_some() {
                    present.push(plugin.clone());
                }
            }
            Ok(present)
        })();
        let interfaces = (|| {
            let request = self.request(&plan.plugin_iteration, &plan.project)?;
            let owner = request
                .owner
                .as_deref()
                .and_then(|id| self.objects.get(id).copied());
            if request.purpose != GetterPurpose::Raw
                || self.method_name(request)? != "getPlugins"
                || !request.arguments.is_empty()
                || !owner.is_some_and(|object| object.kind == ObjectKind::Project)
                || request.return_shape.object_kind != Some(ObjectKind::Plugin)
                || request.return_shape.order != Some(ContainerOrder::Iterable)
            {
                return Err(unavailable(
                    FactsUnavailableReason::Malformed,
                    "Plugin enumeration is not an official typed iteration",
                ));
            }
            let mut interfaces = BTreeSet::new();
            for plugin in self.objects(request)? {
                interfaces.extend(self.class_names(plugin)?.into_iter().map(str::to_owned));
            }
            Ok(interfaces.into_iter().collect())
        })();
        let extension = (|| {
            let request = self.request(&plan.extension_lookup, &plan.project)?;
            if self.method_name(request)? != "findByName"
                || request.arguments != [GetterArgument::String("kotlin".into())]
                || request.return_shape.object_kind != Some(ObjectKind::Extension)
            {
                return Err(unavailable(
                    FactsUnavailableReason::Malformed,
                    "Kotlin extension lookup has wrong official arguments",
                ));
            }
            self.check_container(request, "org.gradle.api.plugins.ExtensionContainer")?;
            Ok(self.nullable_object(request)?.is_some())
        })();
        let version = (|| {
            let request = self.request(&plan.compiler_version, &plan.project)?;
            if self.method_name(request)? != "getKotlinPluginVersion" || request.owner.is_some() {
                return Err(unavailable(
                    FactsUnavailableReason::Malformed,
                    "Compiler version is not the official static getter",
                ));
            }
            let method_id = match &request.method {
                MethodSelection::Selected(id) => id,
                _ => {
                    return Err(unavailable(
                        FactsUnavailableReason::Capability,
                        "No compiler version getter",
                    ));
                }
            };
            let method = self
                .methods
                .get(&(request.catalogue.as_str(), method_id.as_str()))
                .copied()
                .ok_or_else(|| {
                    unavailable(
                        FactsUnavailableReason::MissingMetadata,
                        "No version method catalogue",
                    )
                })?;
            let declaring = self
                .classes
                .get(method.declaring_class.as_str())
                .copied()
                .ok_or_else(|| {
                    unavailable(
                        FactsUnavailableReason::MissingMetadata,
                        "No version getter class",
                    )
                })?;
            if declaring.name != "org.jetbrains.kotlin.gradle.plugin.KotlinPluginWrapperKt"
                || !method.is_static
                || method.descriptor != "(Lorg/gradle/api/Project;)Ljava/lang/String;"
            {
                return Err(unavailable(
                    FactsUnavailableReason::Malformed,
                    "Version getter has the wrong declaring class",
                ));
            }
            match self.outcome(request)? {
                Some(CaptureValue::String(value)) => Ok(Some(value.clone())),
                None => Ok(None),
                _ => Err(unavailable(
                    FactsUnavailableReason::Malformed,
                    "Version getter returned a different shape",
                )),
            }
        })();
        let tasks = (|| {
            let iteration = self.request(&plan.task_iteration, &plan.project)?;
            if iteration.purpose != GetterPurpose::ContainerIterate
                || iteration.return_shape.order != Some(ContainerOrder::ProjectTaskMapValues)
                || iteration.return_shape.object_kind != Some(ObjectKind::Task)
            {
                return Err(unavailable(
                    FactsUnavailableReason::Malformed,
                    "Task collection lacks exact getAllTasks(false)[project] provenance",
                ));
            }
            let mut tasks = Vec::new();
            for id in self.objects(iteration)? {
                let object = self.objects.get(id.as_str()).copied().ok_or_else(|| {
                    unavailable(
                        FactsUnavailableReason::MissingMetadata,
                        "Missing task object",
                    )
                })?;
                let class = self
                    .classes
                    .get(object.class_id.as_str())
                    .copied()
                    .ok_or_else(|| {
                        unavailable(
                            FactsUnavailableReason::MissingMetadata,
                            "Missing task class",
                        )
                    })?;
                if !matches!(
                    class.name.as_str(),
                    "org.jetbrains.kotlin.gradle.tasks.KotlinCompile_Decorated"
                        | "org.jetbrains.kotlin.gradle.tasks.KotlinCompileWithWorkers_Decorated"
                        | "org.jetbrains.kotlin.gradle.tasks.Kotlin2JsCompile_Decorated"
                        | "org.jetbrains.kotlin.gradle.tasks.KotlinCompileCommon_Decorated"
                        | "org.jetbrains.kotlin.gradle.tasks.Kotlin2JsCompileWithWorkers_Decorated"
                        | "org.jetbrains.kotlin.gradle.tasks.KotlinCompileCommonWithWorkers_Decorated"
                ) {
                    continue;
                }
                let identity = object.task.as_ref().ok_or_else(|| {
                    unavailable(
                        FactsUnavailableReason::MissingMetadata,
                        "Missing Kotlin task path",
                    )
                })?;
                let source = (|| {
                    let request_id = plan.source_set_names.get(id).ok_or_else(|| {
                        unavailable(
                            FactsUnavailableReason::MissingMetadata,
                            "Kotlin source-set getter was not planned",
                        )
                    })?;
                    let request = self.request(request_id, &plan.project)?;
                    let source_request = if request.purpose == GetterPurpose::PropertyGet {
                        self.request(
                            request.after.as_deref().ok_or_else(|| {
                                unavailable(
                                    FactsUnavailableReason::MissingMetadata,
                                    "Property result lacks source-set parent",
                                )
                            })?,
                            &plan.project,
                        )?
                    } else {
                        request
                    };
                    if source_request.purpose != GetterPurpose::SourceSet
                        || source_request.owner.as_deref() != Some(id.as_str())
                    {
                        return Err(unavailable(
                            FactsUnavailableReason::Malformed,
                            "Source-set result belongs to another task",
                        ));
                    }
                    match self.outcome(request)? {
                        Some(CaptureValue::String(value)) => Ok(Some(value.clone())),
                        None => Ok(None),
                        _ => Err(unavailable(
                            FactsUnavailableReason::Malformed,
                            "Source-set getter returned a different shape",
                        )),
                    }
                })();
                let arguments = (|| {
                    let request = self.request(
                        plan.compiler_arguments.get(id).ok_or_else(|| {
                            unavailable(
                                FactsUnavailableReason::MissingMetadata,
                                "Compiler arguments were not planned",
                            )
                        })?,
                        &plan.project,
                    )?;
                    if request.purpose != GetterPurpose::CompilerArguments
                        || request.arguments != [GetterArgument::Object(id.clone())]
                    {
                        return Err(unavailable(
                            FactsUnavailableReason::Malformed,
                            "Compiler argument result belongs to another task",
                        ));
                    }
                    match self.outcome(request)? {
                        Some(CaptureValue::Strings(value)) => Ok(Some(value.clone())),
                        None => Ok(None),
                        _ => Err(unavailable(
                            FactsUnavailableReason::Malformed,
                            "Compiler resolver returned a different shape",
                        )),
                    }
                })();
                for failure in [source.as_ref().err(), arguments.as_ref().err()]
                    .into_iter()
                    .flatten()
                {
                    if matches!(
                        failure.reason,
                        FactsUnavailableReason::Malformed | FactsUnavailableReason::Stale
                    ) {
                        return Err(failure.clone());
                    }
                }
                tasks.push(RawKotlinTask {
                    path: identity.path.clone(),
                    class_name: class.name.clone(),
                    source_set_name: self.observed(SOURCE_SET_NAME, source),
                    compiler_arguments: self.observed(COMPILER_ARGUMENTS, arguments),
                });
            }
            Ok(tasks)
        })();
        for failure in [
            ids.as_ref().err(),
            interfaces.as_ref().err(),
            extension.as_ref().err(),
            version.as_ref().err(),
            tasks.as_ref().err(),
        ]
        .into_iter()
        .flatten()
        {
            if matches!(
                failure.reason,
                FactsUnavailableReason::Malformed | FactsUnavailableReason::Stale
            ) {
                return Err(failure.clone());
            }
        }
        Ok(RawKotlinProject {
            project_path: plan.project.clone(),
            plugin_ids: self.observed(PLUGIN_IDS, ids),
            plugin_interfaces: self.observed(PLUGIN_INTERFACES, interfaces),
            kotlin_extension: self.observed(KOTLIN_EXTENSION, extension),
            compiler_version: self.observed(COMPILER_VERSION, version),
            compile_tasks: self.observed(COMPILE_TASKS, tasks),
        })
    }
}

/// `expected` is the host's independently retained discovery/request context,
/// never copied from the untrusted invocation packet as its own proof.
pub fn import_kotlin_from_strict_capture(
    model: &ProjectModel,
    identities: &ImportFactsSnapshot,
    snapshot: &KotlinFactsSnapshot,
    expected: &CaptureContext,
    plans: &[StrictKotlinProjectPlan],
) -> ImportResult<KotlinImportFacts> {
    snapshot.ensure_current(model, identities, expected)?;
    if expected.mode != CaptureMode::Invocation {
        return Err(unavailable(
            FactsUnavailableReason::Capability,
            "Discovery metadata does not prove getter invocation",
        ));
    }
    let projection = StrictProjection {
        events: snapshot
            .raw_events()
            .iter()
            .map(|event| (event.request.as_str(), &event.outcome))
            .collect(),
        requests: expected
            .requests
            .iter()
            .map(|request| (request.id.as_str(), request))
            .collect(),
        objects: expected
            .objects
            .iter()
            .map(|object| (object.id.as_str(), object))
            .collect(),
        classes: expected
            .runtime
            .classes
            .iter()
            .map(|class| (class.id.as_str(), class))
            .collect(),
        methods: expected
            .catalogues
            .iter()
            .flat_map(|catalogue| {
                catalogue
                    .methods
                    .iter()
                    .map(move |method| ((catalogue.id.as_str(), method.id.as_str()), method))
            })
            .collect(),
    };
    let mut projects = BTreeMap::new();
    for plan in plans {
        identities.project(&plan.project)?;
        if projects
            .insert(plan.project.clone(), projection.project(plan)?)
            .is_some()
        {
            return Err(unavailable(
                FactsUnavailableReason::Malformed,
                "Duplicate strict Kotlin project plan",
            ));
        }
    }
    let binding = ImportFactsBinding {
        model_revision: expected.binding.model_revision,
        selection_revision: expected.binding.selection_revision,
        selected_variants: expected
            .binding
            .selected_variants
            .iter()
            .map(|selection| VariantId {
                module: selection.module.clone(),
                variant: selection.variant.clone(),
            })
            .collect(),
    };
    for selected in &binding.selected_variants {
        if !projects.contains_key(&selected.module) {
            return Err(unavailable(
                FactsUnavailableReason::MissingMetadata,
                "Selected module has no complete strict import plan",
            ));
        }
    }
    Ok(KotlinImportFacts {
        root: model.root.clone(),
        binding,
        identity: model_identity(model),
        projects,
        capture_context: Some(expected.clone()),
    })
}

fn model_identity(model: &ProjectModel) -> Vec<ModuleIdentity> {
    model
        .modules
        .iter()
        .map(|module| ModuleIdentity {
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

/// Transport-only decoding. This reduced sidecar cannot establish Kotlin
/// membership; authoritative publication requires the strict snapshot adapter.
pub fn parse_kotlin_import_facts(
    output: &str,
    model: &ProjectModel,
    identities: &ImportFactsSnapshot,
    binding: ImportFactsBinding,
) -> ImportResult<KotlinImportFacts> {
    identities.ensure_current(model, &binding)?;
    let mut records = output
        .lines()
        .filter_map(|line| line.strip_prefix("KODA_ANDROID_PROJECT_MODEL="));
    let record = records.next().ok_or_else(|| {
        unavailable(
            FactsUnavailableReason::MissingMetadata,
            "No project model record",
        )
    })?;
    if records.next().is_some() || record.len() > MAX_RECORD_BYTES {
        return Err(unavailable(
            FactsUnavailableReason::Malformed,
            "Duplicate or oversized Kotlin import record",
        ));
    }
    #[derive(Deserialize)]
    #[serde(remote = "Self")]
    struct Record {
        #[serde(rename = "moduleImportFacts")]
        facts: Option<KotlinExport>,
    }
    impl<'de> Deserialize<'de> for Record {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            Record::deserialize(ObjectOnly(deserializer))
        }
    }
    let record: Record = serde_json::from_str(record).map_err(|error| {
        unavailable(
            FactsUnavailableReason::Malformed,
            format!("Malformed Kotlin import sidecar: {error}"),
        )
    })?;
    let export = record.facts.ok_or_else(|| {
        unavailable(
            FactsUnavailableReason::MissingMetadata,
            "moduleImportFacts is absent or null",
        )
    })?;
    if export.schema != 1 {
        return Err(unavailable(
            FactsUnavailableReason::UnsupportedSchema,
            format!("Unsupported moduleImportFacts schema {}", export.schema),
        ));
    }
    if export.root != model.root || export.modules != model_identity(model) {
        return Err(unavailable(
            FactsUnavailableReason::Stale,
            "Kotlin and Basic model identities differ",
        ));
    }
    let mut projects = BTreeMap::new();
    for project in export.projects {
        identities.project(&project.project_path)?;
        validate_observation(&project.plugin_ids, PLUGIN_IDS)?;
        validate_observation(&project.plugin_interfaces, PLUGIN_INTERFACES)?;
        validate_observation(&project.kotlin_extension, KOTLIN_EXTENSION)?;
        validate_observation(&project.compiler_version, COMPILER_VERSION)?;
        validate_observation(&project.compile_tasks, COMPILE_TASKS)?;
        if let CapturedField::Available(tasks) = &project.compile_tasks.result {
            let mut paths = BTreeSet::new();
            for task in tasks {
                if !paths.insert(&task.path) || task.path.is_empty() || task.class_name.is_empty() {
                    return Err(unavailable(
                        FactsUnavailableReason::Malformed,
                        "Duplicate or missing Kotlin task identity",
                    ));
                }
                validate_observation(&task.source_set_name, SOURCE_SET_NAME)?;
                validate_observation(&task.compiler_arguments, COMPILER_ARGUMENTS)?;
            }
        }
        if projects
            .insert(project.project_path.clone(), project)
            .is_some()
        {
            return Err(unavailable(
                FactsUnavailableReason::Malformed,
                "Duplicate Kotlin project identity",
            ));
        }
    }
    for module in &export.modules {
        if !projects.contains_key(&module.module) {
            return Err(unavailable(
                FactsUnavailableReason::MissingMetadata,
                "Kotlin catalogue omits an imported module",
            ));
        }
    }
    Ok(KotlinImportFacts {
        root: export.root,
        binding,
        identity: export.modules,
        projects,
        capture_context: None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportNameMode {
    Phased,
    Qualified,
    Unqualified,
}

/// Import settings are captured by the host; Gradle has no getter for IDE mode,
/// existing workspace names, or the root buildSrc import group.
#[derive(Clone, Debug)]
pub struct ImportNaming {
    pub mode: ImportNameMode,
    pub root_build_directory: PathBuf,
    pub root_build_name: String,
    pub build_src_group: Option<String>,
    pub existing_names: BTreeSet<String>,
}

#[derive(Clone, Debug)]
pub struct ImportedNameInput<'a> {
    pub build_directory: &'a std::path::Path,
    pub build_name: &'a str,
    pub identity_path: &'a str,
    pub qualified_path: Option<&'a str>,
    pub idea_module_name: Option<&'a str>,
    pub source_set_name: Option<&'a str>,
}

fn escape_element(value: &str) -> String {
    value
        .replace([' ', '/'], "_")
        .replace("\\\\", "_")
        .replace('.', "_")
}

/// Phased source-set collisions use exactly the reference's single `~1` suffix.
/// A second collision is unavailable rather than silently inventing another name.
pub fn imported_internal_name(
    input: &ImportedNameInput<'_>,
    settings: &ImportNaming,
) -> ImportResult<String> {
    let group = settings
        .build_src_group
        .as_deref()
        .filter(|group| !group.is_empty());
    let name = match settings.mode {
        ImportNameMode::Phased => {
            let mut name = input.identity_path.trim_start_matches(':').to_owned();
            if input.build_directory == settings.root_build_directory.as_path()
                || (input.build_directory.parent() == Some(settings.root_build_directory.as_path())
                    && input.build_name == "buildSrc")
            {
                name = format!("{}:{name}", settings.root_build_name);
            }
            if let Some(group) = group {
                name = format!("{group}:{name}");
            }
            let holder = name
                .trim_end_matches(':')
                .split(':')
                .map(escape_element)
                .collect::<Vec<_>>()
                .join(".");
            if let Some(source_set) = input.source_set_name {
                let candidate = format!("{holder}.{}", escape_element(source_set));
                if settings.existing_names.contains(&candidate) {
                    format!("{candidate}~1")
                } else {
                    candidate
                }
            } else {
                holder
            }
        }
        ImportNameMode::Qualified => {
            let path = input.qualified_path.ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::MissingMetadata,
                    "No captured ExternalProject qualified path",
                )
            })?;
            let path_name = path
                .split(':')
                .filter(|piece| !piece.is_empty())
                .collect::<Vec<_>>()
                .join(".");
            let mut name = if path.starts_with(':') {
                format!("{}.{path_name}", input.build_name)
            } else {
                path_name
            };
            if let Some(group) = group {
                name = format!("{group}.{name}");
            }
            if let Some(source_set) = input.source_set_name {
                name.push('.');
                name.push_str(source_set);
            }
            suggest_file_name(&name)
        }
        ImportNameMode::Unqualified => {
            let mut name = input
                .idea_module_name
                .ok_or_else(|| {
                    unavailable(
                        FactsUnavailableReason::MissingMetadata,
                        "No captured Idea module name",
                    )
                })?
                .to_owned();
            if let Some(group) = group {
                name = format!("{group}_{name}");
            }
            if let Some(source_set) = input.source_set_name {
                name.push('_');
                name.push_str(source_set);
            }
            suggest_file_name(&name)
        }
    };
    if name.is_empty()
        || name.chars().any(char::is_control)
        || settings.existing_names.contains(&name)
    {
        return Err(unavailable(
            FactsUnavailableReason::Capability,
            format!("Imported module name is unavailable or collides: {name:?}"),
        ));
    }
    Ok(name)
}

fn suggest_file_name(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character < ' ' || matches!(
                character,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | ';'
            ) || matches!(character, ' ' | '\u{1680}' | '\u{2000}'..='\u{2006}' | '\u{2008}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{205f}' | '\u{3000}') {
                '_'
            } else {
                character
            }
        })
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct KotlinCompilerSettings {
    pub additional_arguments: String,
    pub script_templates: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ExternalSystemRunTask {
    pub task_name: String,
    pub external_system_project_id: String,
    pub target_name: String,
    pub kotlin_platform_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct KotlinSettings {
    pub name: String,
    pub module_name: String,
    pub use_project_settings: bool,
    pub hmpp_enabled: bool,
    pub external_system_run_tasks: Vec<ExternalSystemRunTask>,
    pub compiler_arguments: Option<Vec<String>>,
    pub compiler_settings: Option<KotlinCompilerSettings>,
    pub target_platform: Option<String>,
    pub production_output_path: Option<String>,
    pub test_output_path: Option<String>,
}

impl KotlinSettings {
    pub fn new(module_name: impl Into<String>) -> Self {
        Self {
            name: "Kotlin".into(),
            module_name: module_name.into(),
            use_project_settings: true,
            hmpp_enabled: false,
            external_system_run_tasks: Vec::new(),
            compiler_arguments: None,
            compiler_settings: None,
            target_platform: None,
            production_output_path: None,
            test_output_path: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KotlinUnknownReason {
    UnverifiedTransport,
    GetterUnavailable(String),
    UnrecognizedKotlinExtension,
    MultiplatformImport,
    MissingSourceSet,
    UnknownCompilerVersion,
    UnsupportedCompilerVersion(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KotlinMemberState {
    Present(Box<KotlinSettings>),
    Absent,
    Unknown(KotlinUnknownReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportRevision {
    pub context_generation: u64,
    pub model_revision: u64,
    pub selection_revision: u64,
    pub import_revision: u64,
    pub root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedMember {
    pub internal_name: String,
    pub external_id: String,
    pub source_set_name: Option<String>,
    pub kotlin: KotlinMemberState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedModule {
    pub module: String,
    pub variant: String,
    pub directory: PathBuf,
    pub holder_internal_name: String,
    pub display_name: String,
    pub sort_name: String,
    pub members: Vec<ImportedMember>,
}

impl ImportedModule {
    pub fn kotlin_capability(&self) -> KotlinCapability {
        if self
            .members
            .iter()
            .any(|member| matches!(member.kotlin, KotlinMemberState::Present(_)))
        {
            KotlinCapability::Enabled
        } else if !self.members.is_empty()
            && self
                .members
                .iter()
                .all(|member| member.kotlin == KotlinMemberState::Absent)
        {
            KotlinCapability::Disabled
        } else {
            KotlinCapability::Unknown
        }
    }
}

#[derive(Clone, Debug)]
pub struct CommittedImport {
    pub revision: ImportRevision,
    pub modules: BTreeMap<String, ImportedModule>,
}

impl CommittedImport {
    pub fn prepare_roots(
        &self,
        model: &ProjectModel,
        revision: &ImportRevision,
        module: &str,
        compact_packages: bool,
    ) -> ImportResult<ModuleRootPlan> {
        self.prepare_member_roots(model, revision, module, None, compact_packages)
    }

    pub fn prepare_member_roots(
        &self,
        model: &ProjectModel,
        revision: &ImportRevision,
        module: &str,
        member_name: Option<&str>,
        compact_packages: bool,
    ) -> ImportResult<ModuleRootPlan> {
        if &self.revision != revision || model.root != revision.root {
            return Err(unavailable(
                FactsUnavailableReason::Stale,
                "Imported presentation is no longer current",
            ));
        }
        let imported = self.modules.get(module).ok_or_else(|| {
            unavailable(
                FactsUnavailableReason::MissingMetadata,
                "No committed imported module",
            )
        })?;
        let model_module = model
            .modules
            .iter()
            .find(|candidate| candidate.path == module && candidate.directory == imported.directory)
            .ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::Stale,
                    "Imported module differs from current model",
                )
            })?;
        let display_name = if let Some(member_name) = member_name {
            let member = imported
                .members
                .iter()
                .find(|member| member.internal_name == member_name)
                .ok_or_else(|| {
                    unavailable(
                        FactsUnavailableReason::MissingMetadata,
                        "Imported member is unavailable",
                    )
                })?;
            let directory = imported.directory.to_str().ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::Malformed,
                    "Non-UTF8 imported directory",
                )
            })?;
            let root = revision.root.to_str().ok_or_else(|| {
                unavailable(FactsUnavailableReason::Malformed, "Non-UTF8 imported root")
            })?;
            resolve_module_presentation(&CapturedModuleIdentity {
                internal_name: &member.internal_name,
                holder_internal_name: &imported.holder_internal_name,
                external_system: CapturedExternalSystemIdentity::Gradle(CapturedGradleIdentity {
                    module_type: if member.source_set_name.is_some() {
                        ImportedGradleModuleType::SourceSet
                    } else {
                        ImportedGradleModuleType::Project
                    },
                    external_project_id: Some(&member.external_id),
                    external_project_path: Some(directory),
                    external_root_project_path: Some(root),
                }),
            })
            .map_err(|error| {
                unavailable(FactsUnavailableReason::MissingMetadata, error.to_string())
            })?
            .display_name
            .to_owned()
        } else {
            imported.display_name.clone()
        };
        prepare_imported_module_roots(
            model_module,
            &imported.variant,
            revision.model_revision,
            &CapturedModulePresentation {
                display_name: Some(display_name),
                kotlin: imported.kotlin_capability(),
                compact_packages,
            },
        )
        .map_err(|error: AdapterUnavailable| {
            unavailable(FactsUnavailableReason::Capability, error.to_string())
        })
    }
}

#[derive(Clone, Debug)]
pub struct ImportTransaction {
    previous_revision: Option<ImportRevision>,
    candidate: CommittedImport,
}

impl ImportTransaction {
    pub fn module(&self, module: &str) -> Option<&ImportedModule> {
        self.candidate.modules.get(module)
    }

    pub fn settings_mut(&mut self, module: &str, member_name: &str) -> Option<&mut KotlinSettings> {
        match &mut self
            .candidate
            .modules
            .get_mut(module)?
            .members
            .iter_mut()
            .find(|member| member.internal_name == member_name)?
            .kotlin
        {
            KotlinMemberState::Present(settings) => Some(settings.as_mut()),
            _ => None,
        }
    }

    /// An explicit settings operation, distinct from automatic Gradle import.
    pub fn create_settings(
        &mut self,
        module: &str,
        member_name: &str,
    ) -> Option<&mut KotlinSettings> {
        let member = self
            .candidate
            .modules
            .get_mut(module)?
            .members
            .iter_mut()
            .find(|member| member.internal_name == member_name)?;
        if !matches!(member.kotlin, KotlinMemberState::Present(_)) {
            member.kotlin = KotlinMemberState::Present(Box::new(KotlinSettings::new(member_name)));
        }
        match &mut member.kotlin {
            KotlinMemberState::Present(settings) => Some(settings.as_mut()),
            _ => None,
        }
    }

    pub fn remove_kotlin(&mut self, module: &str, member_name: &str) -> bool {
        let Some(member) = self.candidate.modules.get_mut(module).and_then(|module| {
            module
                .members
                .iter_mut()
                .find(|member| member.internal_name == member_name)
        }) else {
            return false;
        };
        member.kotlin = KotlinMemberState::Absent;
        true
    }
}

#[derive(Default)]
pub struct ModuleImportPublisher {
    committed: Option<CommittedImport>,
}

impl ModuleImportPublisher {
    pub fn committed(&self) -> Option<&CommittedImport> {
        self.committed.as_ref()
    }

    pub fn commit(
        &mut self,
        transaction: ImportTransaction,
        active: &ImportRevision,
    ) -> ImportResult<&CommittedImport> {
        if &transaction.candidate.revision != active
            || self.committed.as_ref().map(|state| &state.revision)
                != transaction.previous_revision.as_ref()
        {
            return Err(unavailable(
                FactsUnavailableReason::Stale,
                "Stale or out-of-order import transaction",
            ));
        }
        if self.committed.as_ref().is_some_and(|old| {
            old.revision.context_generation == active.context_generation
                && old.revision.import_revision >= active.import_revision
        }) {
            return Err(unavailable(
                FactsUnavailableReason::Stale,
                "Import revision did not advance",
            ));
        }
        self.committed = Some(transaction.candidate);
        self.committed.as_ref().ok_or_else(|| {
            unavailable(
                FactsUnavailableReason::MissingMetadata,
                "Import publication failed",
            )
        })
    }

    pub fn edit(&self, revision: ImportRevision) -> ImportResult<ImportTransaction> {
        let committed = self.committed.as_ref().ok_or_else(|| {
            unavailable(
                FactsUnavailableReason::MissingMetadata,
                "No committed settings to edit",
            )
        })?;
        if committed.revision.context_generation != revision.context_generation
            || committed.revision.root != revision.root
            || committed.revision.model_revision != revision.model_revision
            || committed.revision.selection_revision != revision.selection_revision
        {
            return Err(unavailable(
                FactsUnavailableReason::Stale,
                "Settings edit belongs to another model or context",
            ));
        }
        let mut candidate = committed.clone();
        candidate.revision = revision;
        Ok(ImportTransaction {
            previous_revision: Some(committed.revision.clone()),
            candidate,
        })
    }

    pub fn stage(
        &self,
        model: &ProjectModel,
        identities: &ImportFactsSnapshot,
        kotlin: &KotlinImportFacts,
        revision: ImportRevision,
        naming: &ImportNaming,
    ) -> ImportResult<ImportTransaction> {
        identities.ensure_current(model, &kotlin.binding)?;
        if kotlin.root != revision.root
            || model.root != revision.root
            || model_identity(model) != kotlin.identity
            || revision.model_revision != kotlin.binding.model_revision
            || revision.selection_revision != kotlin.binding.selection_revision
        {
            return Err(unavailable(
                FactsUnavailableReason::Stale,
                "Import request differs from captured model or selection",
            ));
        }
        let root_name = identities
            .build_identity()
            .root_name
            .as_ref()
            .ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::MissingMetadata,
                    "No evaluated root build name",
                )
            })?
            .available()?;
        if naming.root_build_directory != model.root || &naming.root_build_name != root_name {
            return Err(unavailable(
                FactsUnavailableReason::Stale,
                "Import naming settings differ from evaluated root build identity",
            ));
        }
        let mut modules = BTreeMap::new();
        let mut naming = naming.clone();
        for selection in &kotlin.binding.selected_variants {
            let module = model
                .modules
                .iter()
                .find(|module| module.path == selection.module)
                .ok_or_else(|| {
                    unavailable(
                        FactsUnavailableReason::MissingMetadata,
                        "Missing selected module",
                    )
                })?;
            let raw = identities.project(&module.path)?;
            let root_name = raw
                .root_name
                .as_ref()
                .ok_or_else(|| {
                    unavailable(
                        FactsUnavailableReason::MissingMetadata,
                        "No captured root name",
                    )
                })?
                .available()?;
            let identity = raw
                .reference_identity_path
                .as_ref()
                .ok_or_else(|| {
                    unavailable(
                        FactsUnavailableReason::MissingMetadata,
                        "No captured reference identity path",
                    )
                })?
                .available()?
                .as_deref()
                .ok_or_else(|| {
                    unavailable(
                        FactsUnavailableReason::MissingMetadata,
                        "Reference identity path is null",
                    )
                })?;
            let build_directory = raw
                .root_directory
                .as_ref()
                .ok_or_else(|| {
                    unavailable(
                        FactsUnavailableReason::MissingMetadata,
                        "No captured build directory",
                    )
                })?
                .available()?;
            let project_name = raw
                .project_name
                .as_ref()
                .ok_or_else(|| {
                    unavailable(
                        FactsUnavailableReason::MissingMetadata,
                        "No captured project name",
                    )
                })?
                .available()?;
            let idea_name = if naming.mode == ImportNameMode::Unqualified {
                Some(
                    raw.idea_module_name
                        .as_ref()
                        .ok_or_else(|| {
                            unavailable(
                                FactsUnavailableReason::MissingMetadata,
                                "No Idea module-name observation",
                            )
                        })?
                        .available()?
                        .as_deref()
                        .unwrap_or(project_name),
                )
            } else {
                None
            };
            let input = ImportedNameInput {
                build_directory,
                build_name: root_name,
                identity_path: identity,
                qualified_path: Some(if module.path == ":" {
                    project_name
                } else {
                    &module.path
                }),
                idea_module_name: idea_name,
                source_set_name: None,
            };
            let holder = imported_internal_name(&input, &naming)?;
            naming.existing_names.insert(holder.clone());
            let external_id = if identity.is_empty() || identity == ":" {
                project_name.as_str()
            } else {
                identity
            };
            let project_path = module.directory.to_str().ok_or_else(|| {
                unavailable(FactsUnavailableReason::Malformed, "Non-UTF8 module path")
            })?;
            let root_path = build_directory.to_str().ok_or_else(|| {
                unavailable(FactsUnavailableReason::Malformed, "Non-UTF8 build path")
            })?;
            let presentation = resolve_module_presentation(&CapturedModuleIdentity {
                internal_name: &holder,
                holder_internal_name: &holder,
                external_system: CapturedExternalSystemIdentity::Gradle(CapturedGradleIdentity {
                    module_type: ImportedGradleModuleType::Project,
                    external_project_id: Some(external_id),
                    external_project_path: Some(project_path),
                    external_root_project_path: Some(root_path),
                }),
            })
            .map_err(|error| {
                unavailable(FactsUnavailableReason::MissingMetadata, error.to_string())
            })?;
            let mut imported = ImportedModule {
                module: module.path.clone(),
                variant: selection.variant.clone(),
                directory: module.directory.clone(),
                holder_internal_name: holder.clone(),
                display_name: presentation.display_name.into(),
                sort_name: holder.clone(),
                members: vec![ImportedMember {
                    internal_name: holder.clone(),
                    external_id: external_id.into(),
                    source_set_name: None,
                    kotlin: KotlinMemberState::Absent,
                }],
            };
            let source_sets = SourceSetImport {
                module,
                selection,
                input: &input,
                project: kotlin.projects.get(&module.path).ok_or_else(|| {
                    unavailable(
                        FactsUnavailableReason::MissingMetadata,
                        "Missing Kotlin project",
                    )
                })?,
                authoritative: kotlin.capture_context.is_some(),
                previous: self.committed.as_ref(),
                revision: &revision,
            };
            add_source_set_members(&mut imported, &mut naming, source_sets)?;
            modules.insert(module.path.clone(), imported);
        }
        Ok(ImportTransaction {
            previous_revision: self.committed.as_ref().map(|state| state.revision.clone()),
            candidate: CommittedImport { revision, modules },
        })
    }
}

struct SourceSetImport<'a> {
    module: &'a Module,
    selection: &'a VariantId,
    input: &'a ImportedNameInput<'a>,
    project: &'a RawKotlinProject,
    authoritative: bool,
    previous: Option<&'a CommittedImport>,
    revision: &'a ImportRevision,
}

fn add_source_set_members(
    imported: &mut ImportedModule,
    naming: &mut ImportNaming,
    source_sets: SourceSetImport<'_>,
) -> ImportResult<()> {
    let SourceSetImport {
        module,
        selection,
        input,
        project,
        authoritative,
        previous,
        revision,
    } = source_sets;
    let variant = module
        .variants
        .iter()
        .find(|variant| variant.name == selection.variant)
        .ok_or_else(|| {
            unavailable(
                FactsUnavailableReason::MissingVariant,
                "Missing selected variant",
            )
        })?;
    let mut source_sets = BTreeSet::new();
    for component in &variant.components {
        if !source_sets.insert(&component.name) {
            return Err(unavailable(
                FactsUnavailableReason::Malformed,
                "Duplicate selected source-set identity",
            ));
        }
        let input = ImportedNameInput {
            source_set_name: Some(&component.name),
            ..input.clone()
        };
        let internal_name = imported_internal_name(&input, naming)?;
        naming.existing_names.insert(internal_name.clone());
        let mut state = if authoritative {
            propose_legacy_kotlin_member(project, &component.name, &internal_name)
        } else {
            KotlinMemberState::Unknown(KotlinUnknownReason::UnverifiedTransport)
        };
        // Unresolved getters, unmatched source sets and unknown versions are reference
        // no-op branches. MPP is a separate importer and must never reuse legacy state.
        if let Some(previous) = previous.filter(|previous| {
            previous.revision.context_generation == revision.context_generation
                && previous.revision.root == revision.root
                && previous.revision.import_revision < revision.import_revision
        }) {
            if let Some(previous_module) = previous.modules.get(&module.path).filter(|old| {
                old.variant == imported.variant && old.directory == imported.directory
            }) {
                if let Some(member) = previous_module.members.iter().find(|member| {
                    member.internal_name == internal_name
                        && member.source_set_name.as_deref() == Some(&component.name)
                }) {
                    match (&mut state, &member.kotlin) {
                        (KotlinMemberState::Present(settings), KotlinMemberState::Present(old)) => {
                            let arguments = settings.compiler_arguments.take();
                            let platform = settings.target_platform.take();
                            *settings = old.clone();
                            settings.compiler_arguments = arguments;
                            settings.target_platform = platform;
                        }
                        (
                            KotlinMemberState::Unknown(reason),
                            old @ (KotlinMemberState::Present(_) | KotlinMemberState::Absent),
                        ) if !matches!(reason, KotlinUnknownReason::MultiplatformImport) => {
                            state = old.clone()
                        }
                        _ => {}
                    }
                }
            }
        }
        imported.members.push(ImportedMember {
            internal_name,
            external_id: format!(
                "{}:{}",
                imported
                    .members
                    .first()
                    .map_or("", |member| member.external_id.as_str()),
                component.name
            ),
            source_set_name: Some(component.name.clone()),
            kotlin: state,
        });
    }
    Ok(())
}

/// A source-derived proposal over raw transport, not committed membership.
/// Only the strict capture adapter can supply its automatic publication authority.
pub fn propose_legacy_kotlin_member(
    project: &RawKotlinProject,
    source_set: &str,
    internal_name: &str,
) -> KotlinMemberState {
    let known = (|| -> ImportResult<_> {
        Ok((
            project.plugin_ids.available()?,
            project.plugin_interfaces.available()?,
            project.kotlin_extension.available()?,
            project.compile_tasks.available()?,
        ))
    })();
    let (plugins, interfaces, extension, tasks) = match known {
        Ok(known) => known,
        Err(error) => {
            return KotlinMemberState::Unknown(KotlinUnknownReason::GetterUnavailable(
                error.to_string(),
            ));
        }
    };
    if plugins.iter().any(|plugin| {
        matches!(
            plugin.as_str(),
            "org.jetbrains.kotlin.multiplatform" | "kotlin-multiplatform"
        )
    }) {
        return KotlinMemberState::Unknown(KotlinUnknownReason::MultiplatformImport);
    }
    let has_plugin = plugins.iter().any(|plugin| {
        matches!(
            plugin.as_str(),
            "kotlin"
                | "kotlin2js"
                | "kotlin-android"
                | "kotlin-platform-jvm"
                | "kotlin-platform-js"
                | "kotlin-platform-common"
        )
    }) || (plugins.iter().any(|plugin| {
        matches!(
            plugin.as_str(),
            "com.android.application"
                | "com.android.library"
                | "com.android.dynamic-feature"
                | "com.android.test"
        )
    }) && interfaces
        .iter()
        .any(|name| name == "org.jetbrains.kotlin.gradle.plugin.KotlinJvmFactory"));
    if !has_plugin {
        return if *extension {
            KotlinMemberState::Unknown(KotlinUnknownReason::UnrecognizedKotlinExtension)
        } else {
            KotlinMemberState::Absent
        };
    }
    let version = match project.compiler_version.available() {
        Ok(Some(version)) => version,
        Ok(None) => return KotlinMemberState::Unknown(KotlinUnknownReason::UnknownCompilerVersion),
        Err(error) => {
            return KotlinMemberState::Unknown(KotlinUnknownReason::GetterUnavailable(
                error.to_string(),
            ));
        }
    };
    let supported = version
        .split('.')
        .take(2)
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>();
    if !matches!(supported.as_deref(), Ok([major, minor]) if *major > 1 || (*major == 1 && *minor >= 9))
    {
        return KotlinMemberState::Unknown(KotlinUnknownReason::UnsupportedCompilerVersion(
            version.clone(),
        ));
    }
    let mut selected = None;
    for task in tasks {
        let name = match task.source_set_name.available() {
            Ok(Some(name)) => name,
            Ok(None) => continue,
            Err(error) => {
                return KotlinMemberState::Unknown(KotlinUnknownReason::GetterUnavailable(
                    error.to_string(),
                ));
            }
        };
        if name != source_set {
            continue;
        }
        selected = Some(task);
    }
    if let Some(task) = selected {
        if !matches!(
            task.class_name.as_str(),
            "org.jetbrains.kotlin.gradle.tasks.KotlinCompile_Decorated"
                | "org.jetbrains.kotlin.gradle.tasks.KotlinCompileWithWorkers_Decorated"
        ) {
            return KotlinMemberState::Unknown(KotlinUnknownReason::GetterUnavailable(format!(
                "Unsupported Kotlin task class {}",
                task.class_name
            )));
        }
        let arguments = match task.compiler_arguments.available() {
            Ok(arguments) => arguments.clone().unwrap_or_default(),
            Err(error) => {
                return KotlinMemberState::Unknown(KotlinUnknownReason::GetterUnavailable(
                    error.to_string(),
                ));
            }
        };
        let mut settings = KotlinSettings::new(internal_name);
        settings.use_project_settings = false;
        settings.compiler_arguments = Some(arguments);
        settings.target_platform = Some("JVM".into());
        return KotlinMemberState::Present(Box::new(settings));
    }
    KotlinMemberState::Unknown(KotlinUnknownReason::MissingSourceSet)
}
