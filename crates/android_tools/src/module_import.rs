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
        CaptureContext, CaptureLimits, CaptureMode, CaptureObject, CaptureValue, ContainerOrder,
        GetterArgument, GetterMethod, GetterOutcome, GetterPurpose, GetterRequest,
        KotlinFactsSnapshot, MethodSelection, ObjectKind, RequestParameter, RuntimeClass,
        ValueKind,
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
const KOTLIN_JVM_FACTORY: &str = "org.jetbrains.kotlin.gradle.plugin.KotlinJvmFactory";
pub const PLUGIN_IDS: &str =
    "org.gradle.api.plugins.PluginContainer.findPlugin(reference Kotlin plugin IDs)";
pub const PLUGIN_INTERFACES: &str = "org.gradle.api.Project.getPlugins() runtime interfaces";
pub const ANDROID_BASE_PLUGIN: &str =
    "org.gradle.api.plugins.PluginContainer.hasPlugin(com.android.base)";
pub const KOTLIN_EXTENSION: &str = "org.gradle.api.plugins.ExtensionContainer.findByName(kotlin)";
pub const COMPILER_VERSION: &str = "org.jetbrains.kotlin.gradle.plugin.KotlinPluginWrapperKt.getKotlinPluginVersion(org.gradle.api.Project)";
pub const COMPILE_TASKS: &str = "org.gradle.api.Project.getAllTasks(false)";
pub const SOURCE_SET_NAME: &str = "org.jetbrains.kotlin.gradle.tasks.getSourceSetName*()";
pub const COMPILER_ARGUMENTS: &str = "org.jetbrains.kotlin.gradle.plugin.ide.IdeCompilerArgumentsResolver.instance(Project).resolveCompilerArguments(task)";

#[path = "jdk_source_set_case.rs"]
mod jdk_source_set_case;

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
    /// Strict imports retain the immediate-superclass KotlinJvmFactory predicate
    /// as an empty or singleton list; reduced transport preserves its raw list.
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
    android_base_plugins: BTreeMap<String, GetterObservation<bool>>,
    requested_source_sets: BTreeMap<String, Vec<ImportResult<bool>>>,
    capture_context: Option<CaptureContext>,
    capture_revision: Option<ImportRevision>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct StrictKotlinProjectPlan {
    pub project: String,
    pub plugin_lookups: BTreeMap<String, String>,
    pub plugin_iteration: String,
    /// Independent exact predicate used by the reference builtin Kotlin fallback.
    /// Older capture plans omit it and retain typed unavailable evidence.
    #[serde(default)]
    pub android_base_plugin: Option<String>,
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
    container_relations: ContainerRelations<'a>,
}

struct RequestedSourceSets<'a> {
    locale: icu_locale_core::LanguageIdentifier,
    casing: jdk_source_set_case::JdkCaseMap,
    // A model call may issue many task getters with the same parameter; retain
    // one folded token set rather than expanding it for every task.
    parameters: BTreeMap<&'a str, BTreeSet<String>>,
    parameter_errors: BTreeMap<&'a str, FactsUnavailable>,
}

impl<'a> RequestedSourceSets<'a> {
    #[cfg(test)]
    fn new(identifier: &str) -> ImportResult<Self> {
        Self::for_runtime(identifier, "17")
    }

    fn for_runtime(identifier: &str, java_version: &str) -> ImportResult<Self> {
        let locale = identifier
            .parse::<icu_locale_core::Locale>()
            .map_err(|error| {
                unavailable(
                    FactsUnavailableReason::UnsupportedShape,
                    format!("Cannot interpret captured source-set selection locale: {error}"),
                )
            })?;
        Ok(Self {
            locale: locale.id,
            casing: jdk_source_set_case::JdkCaseMap::for_version(java_version),
            parameters: BTreeMap::new(),
            parameter_errors: BTreeMap::new(),
        })
    }

    #[cfg(test)]
    fn allows(&mut self, request: &'a GetterRequest, source_set: &str) -> bool {
        self.try_allows(request, source_set)
            .expect("Supported test casing provenance")
    }

    fn try_allows(&mut self, request: &'a GetterRequest, source_set: &str) -> ImportResult<bool> {
        let RequestParameter::Explicit(Some(parameter)) = &request.parameter else {
            return Ok(true);
        };
        // The reference treats '*' as a literal token. No Unicode lowercase
        // mapping can create it, so this comparison needs no JDK data.
        if parameter == "*" {
            return Ok(source_set == "*");
        }
        if let Some(error) = self.parameter_errors.get(parameter.as_str()) {
            return Err(error.clone());
        }
        let fold = |name: &str| {
            self.casing
                .lowercase(name, self.locale.language.as_str())
                .map_err(|detail| unavailable(FactsUnavailableReason::UnsupportedShape, detail))
        };
        if !self.parameters.contains_key(parameter.as_str()) {
            match parameter
                .split(',')
                .map(fold)
                .collect::<ImportResult<BTreeSet<_>>>()
            {
                Ok(folded) => {
                    self.parameters.insert(parameter, folded);
                }
                Err(error) => {
                    self.parameter_errors.insert(parameter, error.clone());
                    return Err(error);
                }
            }
        }
        let source = fold(source_set)?;
        Ok(self
            .parameters
            .get(parameter.as_str())
            .is_some_and(|requested| requested.contains(&source)))
    }
}

struct ContainerRelations<'a> {
    answers: BTreeMap<(&'a str, &'static str), bool>,
    direct_answers: BTreeMap<(&'a str, &'static str), bool>,
    steps: usize,
    limit: usize,
    exhausted: bool,
}

impl<'a> ContainerRelations<'a> {
    fn new(limit: usize) -> Self {
        Self {
            answers: BTreeMap::new(),
            direct_answers: BTreeMap::new(),
            steps: 0,
            limit,
            exhausted: false,
        }
    }

    fn ensure_available(&self) -> ImportResult<()> {
        if self.exhausted {
            Err(unavailable(
                FactsUnavailableReason::UnsupportedShape,
                "Kotlin projection class ancestry traversal limit exceeded",
            ))
        } else {
            Ok(())
        }
    }

    fn charge(&mut self) -> ImportResult<()> {
        self.ensure_available()?;
        match self.steps.checked_add(1) {
            Some(steps) if steps <= self.limit => {
                self.steps = steps;
                Ok(())
            }
            _ => {
                self.exhausted = true;
                self.ensure_available()
            }
        }
    }

    fn contains(
        &mut self,
        classes: &BTreeMap<&'a str, &'a RuntimeClass>,
        class: &'a str,
        interface: &'static str,
    ) -> ImportResult<bool> {
        self.ensure_available()?;
        if let Some(answer) = self.answers.get(&(class, interface)) {
            return Ok(*answer);
        }
        let mut pending = vec![class];
        let mut visited = BTreeSet::new();
        while let Some(id) = pending.pop() {
            self.charge()?;
            if !visited.insert(id) {
                continue;
            }
            let runtime_class = classes.get(id).copied().ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::MissingMetadata,
                    "Unknown captured runtime class",
                )
            })?;
            if runtime_class.name == interface {
                self.answers.insert((class, interface), true);
                return Ok(true);
            }
            for ancestor in runtime_class
                .superclass
                .as_deref()
                .into_iter()
                .chain(runtime_class.interfaces.iter().map(String::as_str))
            {
                self.charge()?;
                pending.push(ancestor);
            }
        }
        self.answers.insert((class, interface), false);
        Ok(false)
    }

    fn superclass_contains(
        &mut self,
        classes: &BTreeMap<&'a str, &'a RuntimeClass>,
        class: &'a str,
        interface: &'static str,
    ) -> ImportResult<bool> {
        self.ensure_available()?;
        let superclass = classes
            .get(class)
            .copied()
            .ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::MissingMetadata,
                    "Missing plugin runtime class",
                )
            })?
            .superclass
            .as_deref()
            .and_then(|id| classes.get(id).copied())
            .ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::Capability,
                    "Plugin runtime class has no captured immediate superclass",
                )
            })?;
        let key = (superclass.id.as_str(), interface);
        if let Some(answer) = self.direct_answers.get(&key) {
            return Ok(*answer);
        }
        if superclass.interfaces.is_empty() {
            // Known-empty metadata requires no traversal, preserving the budget
            // for queries that actually examine class/interface edges.
            self.direct_answers.insert(key, false);
            return Ok(false);
        }
        self.charge()?;
        for id in &superclass.interfaces {
            // Following an interface examines both its edge and target vertex.
            self.charge()?;
            self.charge()?;
            let direct = classes.get(id.as_str()).copied().ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::MissingMetadata,
                    "Missing direct superclass interface",
                )
            })?;
            if direct.name == interface {
                self.direct_answers.insert(key, true);
                return Ok(true);
            }
        }
        self.direct_answers.insert(key, false);
        Ok(false)
    }
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

    fn class_has_interface(&mut self, object: &str, interface: &'static str) -> ImportResult<bool> {
        let object = self.objects.get(object).copied().ok_or_else(|| {
            unavailable(
                FactsUnavailableReason::MissingMetadata,
                "Unknown captured runtime object",
            )
        })?;
        self.container_relations
            .contains(&self.classes, object.class_id.as_str(), interface)
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

    fn superclass_has_interface(
        &mut self,
        object: &str,
        interface: &'static str,
    ) -> ImportResult<bool> {
        let class = self
            .objects
            .get(object)
            .copied()
            .map(|object| object.class_id.as_str())
            .ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::MissingMetadata,
                    "Missing plugin runtime class",
                )
            })?;
        self.container_relations
            .superclass_contains(&self.classes, class, interface)
    }

    fn plugin_factory_interfaces(&mut self, plugins: &[String]) -> ImportResult<Vec<String>> {
        // The reference consumes only this predicate. Retaining every
        // shared interface name per project would multiply capture bytes.
        let mut factory = false;
        for plugin in plugins {
            if self.superclass_has_interface(plugin, KOTLIN_JVM_FACTORY)? {
                factory = true;
            }
        }
        Ok(if factory {
            vec![KOTLIN_JVM_FACTORY.to_owned()]
        } else {
            Vec::new()
        })
    }

    fn android_base_plugin(&mut self, plan: &StrictKotlinProjectPlan) -> ImportResult<bool> {
        let request = self.request(
            plan.android_base_plugin.as_deref().ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::Capability,
                    "Exact Android base plugin predicate was not independently planned",
                )
            })?,
            &plan.project,
        )?;
        if request.purpose != GetterPurpose::Raw
            || self.method_name(request)? != "hasPlugin"
            || request.arguments != [GetterArgument::String("com.android.base".into())]
            || request.return_shape.kind != ValueKind::Boolean
        {
            return Err(unavailable(
                FactsUnavailableReason::Malformed,
                "Android base plugin predicate does not match its official request",
            ));
        }
        self.check_container(request, "org.gradle.api.plugins.PluginContainer")?;
        match self.outcome(request)? {
            Some(CaptureValue::Boolean(value)) => Ok(*value),
            _ => Err(unavailable(
                FactsUnavailableReason::Malformed,
                "Android base plugin predicate returned a different shape",
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

    fn check_container(
        &mut self,
        request: &GetterRequest,
        interface: &'static str,
    ) -> ImportResult<()> {
        let owner = request.owner.as_deref().ok_or_else(|| {
            unavailable(
                FactsUnavailableReason::MissingMetadata,
                "Official getter lacks owning container",
            )
        })?;
        if !self.class_has_interface(owner, interface)? {
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

    fn project(
        &mut self,
        plan: &StrictKotlinProjectPlan,
        requested: &mut RequestedSourceSets<'a>,
    ) -> ImportResult<(RawKotlinProject, Vec<ImportResult<bool>>)> {
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
            self.plugin_factory_interfaces(self.objects(request)?)
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
        let mut requested_source_sets = Vec::new();
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
            let returned_tasks = self.objects(iteration)?;
            // The official Project task-map value is a Set<Task>. Check the
            // whole collection before a repeated ID can multiply one payload.
            let mut seen_task_ids = BTreeSet::new();
            for id in returned_tasks {
                if !seen_task_ids.insert(id.as_str()) {
                    return Err(unavailable(
                        FactsUnavailableReason::Malformed,
                        "Official Project task set repeats a runtime task object",
                    ));
                }
            }
            let mut tasks = Vec::new();
            for id in returned_tasks {
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
                let mut recorded_source_request = None;
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
                    recorded_source_request = Some(request);
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
                    if let Some(source_request) = plan.source_set_names.get(id) {
                        let source_request = self.request(source_request, &plan.project)?;
                        if request.model_call != source_request.model_call
                            || request.parameter != source_request.parameter
                        {
                            return Err(unavailable(
                                FactsUnavailableReason::Malformed,
                                "Task source-set and compiler arguments belong to different model calls",
                            ));
                        }
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
                requested_source_sets.push(match (&source, recorded_source_request) {
                    (Ok(name), Some(request)) => {
                        requested.try_allows(request, name.as_deref().unwrap_or("main"))
                    }
                    _ => Ok(false),
                });
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
        Ok((
            RawKotlinProject {
                project_path: plan.project.clone(),
                plugin_ids: self.observed(PLUGIN_IDS, ids),
                plugin_interfaces: self.observed(PLUGIN_INTERFACES, interfaces),
                kotlin_extension: self.observed(KOTLIN_EXTENSION, extension),
                compiler_version: self.observed(COMPILER_VERSION, version),
                compile_tasks: self.observed(COMPILE_TASKS, tasks),
            },
            requested_source_sets,
        ))
    }
}

/// `expected` is the host's independently retained discovery/request context,
/// never copied from the untrusted invocation packet as its own proof. `issued`
/// is retained by the host when starting that capture in the current workspace;
/// it must not be manufactured later to rebind a retained invocation packet.
pub fn import_kotlin_from_strict_capture(
    model: &ProjectModel,
    identities: &ImportFactsSnapshot,
    snapshot: &KotlinFactsSnapshot,
    expected: &CaptureContext,
    issued: &ImportRevision,
    plans: &[StrictKotlinProjectPlan],
) -> ImportResult<KotlinImportFacts> {
    import_kotlin_from_strict_capture_with_limits(
        model,
        identities,
        snapshot,
        expected,
        issued,
        plans,
        CaptureLimits::default(),
    )
}

/// The decoder's traversal budget does not cover this subsequent projection.
/// `limits.ancestry_steps` bounds cumulative container ancestry and nonempty
/// immediate-superclass direct-interface traversal across every project plan.
/// Callers can raise it for larger independently captured graphs. Strict plugin
/// interface observations retain only the reference's KotlinJvmFactory predicate;
/// they do not duplicate complete runtime interface-name lists for each project.
/// Exhaustion leaves Basic/import facts intact and supplies no Kotlin publication.
/// Other capture limits apply when parsing the snapshot.
pub fn import_kotlin_from_strict_capture_with_limits(
    model: &ProjectModel,
    identities: &ImportFactsSnapshot,
    snapshot: &KotlinFactsSnapshot,
    expected: &CaptureContext,
    issued: &ImportRevision,
    plans: &[StrictKotlinProjectPlan],
    limits: CaptureLimits,
) -> ImportResult<KotlinImportFacts> {
    snapshot.ensure_current(model, identities, expected)?;
    if expected.mode != CaptureMode::Invocation {
        return Err(unavailable(
            FactsUnavailableReason::Capability,
            "Discovery metadata does not prove getter invocation",
        ));
    }
    if issued.root != model.root
        || issued.model_revision != expected.binding.model_revision
        || issued.selection_revision != expected.binding.selection_revision
    {
        return Err(unavailable(
            FactsUnavailableReason::Stale,
            "Capture was issued for another model, selection or workspace",
        ));
    }
    let mut projection = StrictProjection {
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
        container_relations: ContainerRelations::new(limits.ancestry_steps),
    };
    let mut projects = BTreeMap::new();
    let mut android_base_plugins = BTreeMap::new();
    let mut requested = RequestedSourceSets::for_runtime(
        &expected.runtime.locale.identifier,
        &expected.runtime.java.version,
    )?;
    let mut requested_source_sets = BTreeMap::new();
    for plan in plans {
        identities.project(&plan.project)?;
        let (project, selected_source_sets) = projection.project(plan, &mut requested)?;
        projection.container_relations.ensure_available()?;
        if projects.insert(plan.project.clone(), project).is_some() {
            return Err(unavailable(
                FactsUnavailableReason::Malformed,
                "Duplicate strict Kotlin project plan",
            ));
        }
        requested_source_sets.insert(plan.project.clone(), selected_source_sets);
        let base = projection.android_base_plugin(plan);
        projection.container_relations.ensure_available()?;
        if let Err(error) = &base {
            if matches!(
                error.reason,
                FactsUnavailableReason::Malformed | FactsUnavailableReason::Stale
            ) {
                return Err(error.clone());
            }
        }
        android_base_plugins.insert(
            plan.project.clone(),
            projection.observed(ANDROID_BASE_PLUGIN, base),
        );
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
        android_base_plugins,
        requested_source_sets,
        capture_context: Some(expected.clone()),
        capture_revision: Some(issued.clone()),
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
        android_base_plugins: BTreeMap::new(),
        requested_source_sets: BTreeMap::new(),
        capture_context: None,
        capture_revision: None,
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
    let group = settings.build_src_group.as_deref();
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
            if let Some(group) = group.filter(|group| !group.is_empty()) {
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
            if let Some(group) = group.filter(|group| !group.is_empty()) {
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
    AmbiguousKotlinPluginIds,
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
            || kotlin
                .capture_revision
                .as_ref()
                .is_some_and(|issued| issued != &revision)
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
            let project_id = if identity.is_empty() || identity == ":" {
                project_name.as_str()
            } else {
                identity
            };
            // getModuleId(context, externalProject) groups external IDs separately
            // from escaped internal names and holder/sort identities.
            let external_id = naming
                .build_src_group
                .as_deref()
                .filter(|group| !group.is_empty())
                .map_or_else(
                    || project_id.to_owned(),
                    |group| format!("{group}:{project_id}"),
                );
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
                    external_project_id: Some(&external_id),
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
                    external_id,
                    source_set_name: None,
                    // A holder is a Rust-created grouping member, not automatic
                    // Kotlin membership. Component facts control the linked group;
                    // no components is explicitly Unknown below.
                    kotlin: KotlinMemberState::Absent,
                }],
            };
            // Explicit holder settings are an independent user operation. The
            // source-set importer does not remove or recreate that holder facet.
            if let Some(old) = previous_module(self.committed.as_ref(), &revision, &imported)
                .and_then(|old| {
                    old.members.iter().find(|member| {
                        member.internal_name == holder && member.source_set_name.is_none()
                    })
                })
                .filter(|member| matches!(member.kotlin, KotlinMemberState::Present(_)))
            {
                imported.members[0].kotlin = old.kotlin.clone();
            }
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
                android_base_plugin: kotlin.android_base_plugins.get(&module.path),
                requested_source_sets: kotlin
                    .requested_source_sets
                    .get(&module.path)
                    .map(Vec::as_slice),
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
    android_base_plugin: Option<&'a GetterObservation<bool>>,
    requested_source_sets: Option<&'a [ImportResult<bool>]>,
    previous: Option<&'a CommittedImport>,
    revision: &'a ImportRevision,
}

fn previous_module<'a>(
    previous: Option<&'a CommittedImport>,
    revision: &ImportRevision,
    imported: &ImportedModule,
) -> Option<&'a ImportedModule> {
    previous
        .filter(|previous| {
            previous.revision.context_generation == revision.context_generation
                && previous.revision.root == revision.root
                && previous.revision.import_revision < revision.import_revision
        })
        .and_then(|previous| previous.modules.get(&imported.module))
        .filter(|old| old.variant == imported.variant && old.directory == imported.directory)
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
        android_base_plugin,
        requested_source_sets,
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
    if variant.components.is_empty()
        && !matches!(imported.members[0].kotlin, KotlinMemberState::Present(_))
    {
        imported.members[0].kotlin =
            KotlinMemberState::Unknown(KotlinUnknownReason::MissingSourceSet);
    }
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
            propose_kotlin_member(
                project,
                &component.name,
                &internal_name,
                Some(android_base_plugin),
                requested_source_sets,
            )
        } else {
            KotlinMemberState::Unknown(KotlinUnknownReason::UnverifiedTransport)
        };
        // Unresolved getters, unmatched source sets and unknown versions are reference
        // no-op branches. MPP is a separate importer and must never reuse legacy state.
        if let Some(previous_module) = previous_module(previous, revision, imported) {
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
/// Its legacy Android plugin proxy remains a supplemental compatibility constraint;
/// strict publication requires the independently observed `hasPlugin(com.android.base)`.
pub fn propose_legacy_kotlin_member(
    project: &RawKotlinProject,
    source_set: &str,
    internal_name: &str,
) -> KotlinMemberState {
    propose_kotlin_member(project, source_set, internal_name, None, None)
}

fn propose_kotlin_member(
    project: &RawKotlinProject,
    source_set: &str,
    internal_name: &str,
    strict_android_base: Option<Option<&GetterObservation<bool>>>,
    requested_source_sets: Option<&[ImportResult<bool>]>,
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
    let legacy_count = plugins
        .iter()
        .filter(|plugin| matches!(plugin.as_str(), "kotlin" | "kotlin2js" | "kotlin-android"))
        .count();
    let platform_count = plugins
        .iter()
        .filter(|plugin| {
            matches!(
                plugin.as_str(),
                "kotlin-platform-jvm" | "kotlin-platform-js" | "kotlin-platform-common"
            )
        })
        .count();
    let factory = interfaces.iter().any(|name| name == KOTLIN_JVM_FACTORY);
    let singleton = legacy_count == 1 || platform_count == 1;
    let builtin = if singleton || !factory {
        false
    } else if let Some(base) = strict_android_base {
        match base.map(GetterObservation::available) {
            Some(Ok(value)) => *value,
            Some(Err(error)) => {
                return KotlinMemberState::Unknown(KotlinUnknownReason::GetterUnavailable(
                    error.to_string(),
                ));
            }
            None => {
                return KotlinMemberState::Unknown(KotlinUnknownReason::GetterUnavailable(
                    "Missing exact Android base plugin predicate".into(),
                ));
            }
        }
    } else {
        plugins.iter().any(|plugin| {
            matches!(
                plugin.as_str(),
                "com.android.application"
                    | "com.android.library"
                    | "com.android.dynamic-feature"
                    | "com.android.test"
            )
        })
    };
    let has_plugin = singleton || builtin;
    if !has_plugin {
        if legacy_count > 1 || platform_count > 1 {
            return KotlinMemberState::Unknown(KotlinUnknownReason::AmbiguousKotlinPluginIds);
        }
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
    for (index, task) in tasks.iter().enumerate() {
        let name = match task.source_set_name.available() {
            Ok(Some(name)) => name.as_str(),
            // The reference derives main from a known null; unavailable
            // getter evidence remains distinct and cannot supply this fallback.
            Ok(None) => "main",
            Err(error) => {
                return KotlinMemberState::Unknown(KotlinUnknownReason::GetterUnavailable(
                    error.to_string(),
                ));
            }
        };
        if let Some(requested) = requested_source_sets {
            // An unavailable getter must remain unknown even when the request
            // could exclude a known name. Only successful observations filter.
            match requested.get(index) {
                Some(Ok(false)) => continue,
                Some(Ok(true)) => {}
                Some(Err(error)) => {
                    return KotlinMemberState::Unknown(KotlinUnknownReason::GetterUnavailable(
                        error.to_string(),
                    ));
                }
                None => {
                    return KotlinMemberState::Unknown(KotlinUnknownReason::GetterUnavailable(
                        "Missing recorded source-set selection".into(),
                    ));
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kotlin_import_facts::ClassOrigin;

    fn runtime_class(id: &str, name: &str, interfaces: Vec<String>) -> RuntimeClass {
        RuntimeClass {
            id: id.into(),
            name: name.into(),
            loader: "gradle".into(),
            origin: ClassOrigin::Artifact("gradle-jar".into()),
            superclass: None,
            interfaces,
        }
    }

    #[test]
    fn shared_dense_container_graph_reuses_positive_and_negative_queries() -> ImportResult<()> {
        let mut captured = vec![
            runtime_class(
                "plugins",
                "org.gradle.api.plugins.PluginContainer",
                Vec::new(),
            ),
            runtime_class(
                "extensions",
                "org.gradle.api.plugins.ExtensionContainer",
                Vec::new(),
            ),
        ];
        for index in 0..128 {
            let interfaces = if index == 0 {
                vec!["plugins".into(), "extensions".into()]
            } else {
                (0..index)
                    .rev()
                    .take(4)
                    .map(|ancestor| format!("layer-{ancestor}"))
                    .collect()
            };
            captured.push(runtime_class(
                &format!("layer-{index}"),
                &format!("org.example.Layer{index}"),
                interfaces,
            ));
        }
        let classes = captured
            .iter()
            .map(|class| (class.id.as_str(), class))
            .collect();
        let mut relations = ContainerRelations::new(8192);
        assert!(relations.contains(
            &classes,
            "layer-127",
            "org.gradle.api.plugins.PluginContainer"
        )?);
        assert!(relations.contains(
            &classes,
            "layer-127",
            "org.gradle.api.plugins.ExtensionContainer"
        )?);
        assert!(!relations.contains(&classes, "layer-127", "org.example.AbsentContainer")?);
        let initial_steps = relations.steps;
        relations.limit = initial_steps;
        for _ in 0..1000 {
            assert!(relations.contains(
                &classes,
                "layer-127",
                "org.gradle.api.plugins.PluginContainer"
            )?);
            assert!(relations.contains(
                &classes,
                "layer-127",
                "org.gradle.api.plugins.ExtensionContainer"
            )?);
            assert!(!relations.contains(&classes, "layer-127", "org.example.AbsentContainer")?);
        }
        assert_eq!(relations.steps, initial_steps);
        assert!(!relations.exhausted);
        Ok(())
    }

    #[test]
    fn container_answers_distinguish_runtime_class_ids_and_interface_names() -> ImportResult<()> {
        let captured = [
            runtime_class(
                "plugins",
                "org.gradle.api.plugins.PluginContainer",
                Vec::new(),
            ),
            runtime_class(
                "extensions",
                "org.gradle.api.plugins.ExtensionContainer",
                Vec::new(),
            ),
            runtime_class("first", "org.example.SharedName", vec!["plugins".into()]),
            runtime_class(
                "second",
                "org.example.SharedName",
                vec!["extensions".into()],
            ),
        ];
        let classes = captured
            .iter()
            .map(|class| (class.id.as_str(), class))
            .collect();
        let mut relations = ContainerRelations::new(32);
        for _ in 0..2 {
            assert!(relations.contains(
                &classes,
                "first",
                "org.gradle.api.plugins.PluginContainer"
            )?);
            assert!(!relations.contains(
                &classes,
                "first",
                "org.gradle.api.plugins.ExtensionContainer"
            )?);
            assert!(!relations.contains(
                &classes,
                "second",
                "org.gradle.api.plugins.PluginContainer"
            )?);
            assert!(relations.contains(
                &classes,
                "second",
                "org.gradle.api.plugins.ExtensionContainer"
            )?);
        }
        Ok(())
    }

    #[test]
    fn container_budget_counts_edges_and_is_cumulative_between_queries() -> ImportResult<()> {
        let captured = [
            runtime_class(
                "plugins",
                "org.gradle.api.plugins.PluginContainer",
                Vec::new(),
            ),
            runtime_class(
                "extensions",
                "org.gradle.api.plugins.ExtensionContainer",
                Vec::new(),
            ),
            runtime_class("derived", "org.example.Derived", vec!["plugins".into()]),
        ];
        let classes = captured
            .iter()
            .map(|class| (class.id.as_str(), class))
            .collect();
        let mut relations = ContainerRelations::new(3);
        assert!(relations.contains(
            &classes,
            "derived",
            "org.gradle.api.plugins.PluginContainer"
        )?);
        assert_eq!(relations.steps, 3);
        assert_eq!(
            relations
                .contains(
                    &classes,
                    "extensions",
                    "org.gradle.api.plugins.ExtensionContainer"
                )
                .expect_err("All queries share the vertex and edge budget")
                .reason,
            FactsUnavailableReason::UnsupportedShape
        );
        assert_eq!(
            relations
                .contains(
                    &classes,
                    "derived",
                    "org.gradle.api.plugins.PluginContainer"
                )
                .expect_err("An exhausted projection cannot return cached evidence")
                .reason,
            FactsUnavailableReason::UnsupportedShape
        );
        assert!(relations.exhausted);
        Ok(())
    }

    #[test]
    fn container_work_counter_overflow_remains_unavailable() {
        let mut relations = ContainerRelations::new(usize::MAX);
        relations.steps = usize::MAX;
        assert_eq!(
            relations
                .charge()
                .expect_err("Never wrap the work counter")
                .reason,
            FactsUnavailableReason::UnsupportedShape
        );
        assert_eq!(relations.steps, usize::MAX);
        assert!(relations.exhausted);
    }

    #[test]
    fn direct_superclass_predicate_reuses_wide_shared_metadata_and_negative_answers()
    -> ImportResult<()> {
        let mut captured = vec![runtime_class("factory", KOTLIN_JVM_FACTORY, Vec::new())];
        let mut interfaces = Vec::new();
        for index in 0..32 {
            let id = format!("noise-{index}");
            captured.push(runtime_class(
                &id,
                &format!("org.example.Interface{index}.{}", "x".repeat(4096)),
                Vec::new(),
            ));
            interfaces.push(id);
        }
        interfaces.push("factory".into());
        captured.push(runtime_class("parent", "org.example.Parent", interfaces));
        for index in 0..128 {
            let mut plugin = runtime_class(
                &format!("plugin-{index}"),
                "org.example.SharedPluginName",
                Vec::new(),
            );
            plugin.superclass = Some("parent".into());
            captured.push(plugin);
        }
        let classes = captured
            .iter()
            .map(|class| (class.id.as_str(), class))
            .collect();
        let mut relations = ContainerRelations::new(134);
        assert!(relations.superclass_contains(&classes, "plugin-0", KOTLIN_JVM_FACTORY)?);
        assert!(!relations.superclass_contains(&classes, "plugin-0", "org.example.Absent")?);
        assert_eq!(relations.steps, 134);
        for _ in 0..8 {
            for plugin in captured
                .iter()
                .filter(|class| class.id.starts_with("plugin-"))
            {
                let id = plugin.id.as_str();
                assert!(relations.superclass_contains(&classes, id, KOTLIN_JVM_FACTORY)?);
                assert!(!relations.superclass_contains(&classes, id, "org.example.Absent")?);
            }
        }
        assert_eq!(relations.steps, 134);
        assert!(!relations.exhausted);
        Ok(())
    }

    #[test]
    fn direct_superclass_predicate_separates_class_ids_targets_and_transitive_queries()
    -> ImportResult<()> {
        let mut first = runtime_class("first", "org.example.SamePlugin", Vec::new());
        first.superclass = Some("first-parent".into());
        let mut second = runtime_class("second", "org.example.SamePlugin", vec!["factory".into()]);
        second.loader = "other-loader".into();
        second.superclass = Some("second-parent".into());
        let first_parent = runtime_class(
            "first-parent",
            "org.example.SameParent",
            vec!["factory".into()],
        );
        let mut second_parent = runtime_class(
            "second-parent",
            "org.example.SameParent",
            vec!["bridge".into()],
        );
        second_parent.loader = "other-loader".into();
        second_parent.superclass = Some("first-parent".into());
        let captured = [
            runtime_class("factory", KOTLIN_JVM_FACTORY, Vec::new()),
            runtime_class("bridge", "org.example.Bridge", vec!["factory".into()]),
            first_parent,
            second_parent,
            first,
            second,
        ];
        let classes = captured
            .iter()
            .map(|class| (class.id.as_str(), class))
            .collect();
        let mut relations = ContainerRelations::new(128);
        assert!(relations.contains(&classes, "second", KOTLIN_JVM_FACTORY)?);
        assert!(!relations.superclass_contains(&classes, "second", KOTLIN_JVM_FACTORY)?);
        assert!(relations.superclass_contains(&classes, "first", KOTLIN_JVM_FACTORY)?);
        assert!(!relations.superclass_contains(&classes, "first", "org.example.Bridge")?);
        assert!(relations.superclass_contains(&classes, "second", "org.example.Bridge")?);
        let initial_steps = relations.steps;
        relations.limit = initial_steps;
        for _ in 0..1000 {
            assert!(relations.superclass_contains(&classes, "first", KOTLIN_JVM_FACTORY)?);
            assert!(!relations.superclass_contains(&classes, "second", KOTLIN_JVM_FACTORY)?);
            assert!(!relations.superclass_contains(&classes, "first", "org.example.Bridge")?);
            assert!(relations.superclass_contains(&classes, "second", "org.example.Bridge")?);
        }
        assert_eq!(relations.steps, initial_steps);
        Ok(())
    }

    #[test]
    fn empty_direct_superclass_interfaces_require_no_traversal_budget() -> ImportResult<()> {
        let mut plugin = runtime_class("plugin", "org.example.Plugin", Vec::new());
        plugin.superclass = Some("object".into());
        let captured = [
            runtime_class("object", "java.lang.Object", Vec::new()),
            runtime_class("factory", KOTLIN_JVM_FACTORY, Vec::new()),
            plugin,
        ];
        let classes = captured
            .iter()
            .map(|class| (class.id.as_str(), class))
            .collect();
        let mut relations = ContainerRelations::new(0);
        for _ in 0..1000 {
            assert!(!relations.superclass_contains(&classes, "plugin", KOTLIN_JVM_FACTORY)?);
        }
        assert_eq!(relations.steps, 0);
        assert_eq!(
            relations
                .contains(&classes, "factory", KOTLIN_JVM_FACTORY)
                .expect_err("An uncached vertex needs traversal budget")
                .reason,
            FactsUnavailableReason::UnsupportedShape
        );
        assert_eq!(
            relations
                .superclass_contains(&classes, "plugin", KOTLIN_JVM_FACTORY)
                .expect_err("Known-empty cached evidence cannot escape exhaustion")
                .reason,
            FactsUnavailableReason::UnsupportedShape
        );
        Ok(())
    }

    #[test]
    fn direct_superclass_budget_is_cumulative_with_container_queries() -> ImportResult<()> {
        let mut plugin = runtime_class("plugin", "org.example.Plugin", Vec::new());
        plugin.superclass = Some("parent".into());
        let captured = [
            runtime_class("factory", KOTLIN_JVM_FACTORY, Vec::new()),
            runtime_class("parent", "org.example.Parent", vec!["factory".into()]),
            runtime_class(
                "plugins",
                "org.gradle.api.plugins.PluginContainer",
                Vec::new(),
            ),
            runtime_class(
                "extensions",
                "org.gradle.api.plugins.ExtensionContainer",
                Vec::new(),
            ),
            plugin,
        ];
        let classes = captured
            .iter()
            .map(|class| (class.id.as_str(), class))
            .collect();
        let mut relations = ContainerRelations::new(4);
        assert!(relations.contains(
            &classes,
            "plugins",
            "org.gradle.api.plugins.PluginContainer"
        )?);
        assert!(relations.superclass_contains(&classes, "plugin", KOTLIN_JVM_FACTORY)?);
        assert_eq!(relations.steps, 4);
        assert!(relations.superclass_contains(&classes, "plugin", KOTLIN_JVM_FACTORY)?);
        assert_eq!(
            relations
                .contains(
                    &classes,
                    "extensions",
                    "org.gradle.api.plugins.ExtensionContainer",
                )
                .expect_err("Direct and transitive queries share the work budget")
                .reason,
            FactsUnavailableReason::UnsupportedShape
        );
        assert_eq!(
            relations
                .superclass_contains(&classes, "plugin", KOTLIN_JVM_FACTORY)
                .expect_err("An exhausted projection cannot return cached direct evidence")
                .reason,
            FactsUnavailableReason::UnsupportedShape
        );
        Ok(())
    }

    #[test]
    fn shared_project_plugin_metadata_retains_only_the_factory_marker() -> ImportResult<()> {
        for factory_present in [false, true] {
            let mut captured = vec![runtime_class("factory", KOTLIN_JVM_FACTORY, Vec::new())];
            let mut interfaces = Vec::new();
            for index in 0..32 {
                let id = format!("noise-{index}");
                captured.push(runtime_class(
                    &id,
                    &format!("org.example.Interface{index}.{}", "x".repeat(4096)),
                    Vec::new(),
                ));
                interfaces.push(id);
            }
            if factory_present {
                interfaces.push("factory".into());
            }
            captured.push(runtime_class("parent", "org.example.Parent", interfaces));
            let mut plugin = runtime_class("plugin", "org.example.Plugin", Vec::new());
            plugin.superclass = Some("parent".into());
            captured.push(plugin);
            let objects = (0..128)
                .map(|index| CaptureObject {
                    id: format!("plugin-object-{index}"),
                    kind: ObjectKind::Plugin,
                    project: format!(":project-{index}"),
                    class_id: "plugin".into(),
                    task: None,
                })
                .collect::<Vec<_>>();
            let mut projection = StrictProjection {
                events: BTreeMap::new(),
                requests: BTreeMap::new(),
                objects: objects
                    .iter()
                    .map(|object| (object.id.as_str(), object))
                    .collect(),
                classes: captured
                    .iter()
                    .map(|class| (class.id.as_str(), class))
                    .collect(),
                methods: BTreeMap::new(),
                container_relations: ContainerRelations::new(67),
            };
            let retained = objects
                .iter()
                .map(|object| {
                    projection.plugin_factory_interfaces(std::slice::from_ref(&object.id))
                })
                .collect::<ImportResult<Vec<_>>>()?;
            let expected = if factory_present {
                vec![KOTLIN_JVM_FACTORY.to_owned()]
            } else {
                Vec::new()
            };
            assert_eq!(retained.len(), objects.len());
            assert!(retained.iter().all(|interfaces| interfaces == &expected));
            assert_eq!(
                retained.iter().flatten().map(String::len).sum::<usize>(),
                if factory_present {
                    objects.len() * KOTLIN_JVM_FACTORY.len()
                } else {
                    0
                }
            );
            assert_eq!(
                projection.container_relations.steps,
                if factory_present { 67 } else { 65 }
            );
            assert_eq!(projection.container_relations.direct_answers.len(), 1);
            assert!(!projection.container_relations.exhausted);
        }
        Ok(())
    }
}

#[cfg(test)]
mod requested_source_set_tests {
    use super::*;
    use anyhow::{Context as _, Result};

    fn source_request() -> Result<GetterRequest> {
        let template: serde_json::Value = serde_json::from_str(include_str!(
            "../test_data/module_import/strict-projection-template.json"
        ))?;
        let request = template["packet"]["context"]["requests"]
            .as_array()
            .context("Synthetic request catalogue")?
            .iter()
            .find(|request| request["id"] == "source-first")
            .context("Synthetic source-set request")?;
        Ok(serde_json::from_value(request.clone())?)
    }

    #[test]
    fn repeated_task_selection_retains_one_folded_parameter_set() -> Result<()> {
        let mut request = source_request()?;
        let names = (0..1024)
            .map(|index| format!("SOURCE{index}"))
            .collect::<Vec<_>>();
        request.parameter = RequestParameter::Explicit(Some(names.join(",")));
        let mut requested = RequestedSourceSets::new("tr-TR")?;
        for name in &names {
            assert!(requested.allows(&request, name));
            assert!(!requested.allows(&request, "excluded"));
        }
        assert_eq!(requested.parameters.len(), 1);
        let (raw, folded) = requested
            .parameters
            .first_key_value()
            .context("One folded set")?;
        assert_eq!(*raw, names.join(","));
        assert_eq!(folded.len(), names.len());
        assert!(folded.contains("source0"));
        assert!(folded.contains("source1023"));
        assert_eq!(
            request.parameter,
            RequestParameter::Explicit(Some(names.join(",")))
        );
        Ok(())
    }

    #[test]
    fn source_set_selection_preserves_contextual_unicode_lowercase() -> Result<()> {
        for (locale, parameter, source_set) in [
            ("tr-TR", "MAİN", "main"),
            ("lt-LT", "I\u{301}", "i\u{307}\u{301}"),
            ("el-GR", "ΟΣ", "ος"),
        ] {
            let mut request = source_request()?;
            request.parameter = RequestParameter::Explicit(Some(parameter.into()));
            let mut requested = RequestedSourceSets::new(locale)?;
            assert!(requested.allows(&request, source_set));
            assert!(!requested.allows(&request, "excluded"));
            assert_eq!(
                request.parameter,
                RequestParameter::Explicit(Some(parameter.into()))
            );
        }
        Ok(())
    }
}
