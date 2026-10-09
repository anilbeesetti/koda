/*
 * Copyright (C) 2014 The Android Open Source Project
 * Copyright (C) 2017 The Android Open Source Project
 * Copyright (C) 2019 The Android Open Source Project
 * Copyright (C) 2022 The Android Open Source Project
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

//! Pure per-module adapter over already captured Android model and file facts.
//!
//! This implements the supported AndroidSourceType subset using the attributed
//! sources in `test_data/project_tree/reference`. It installs no scanner, model
//! publisher, language-capability producer, Gradle task, or project-panel UI.
//! Captures must identify the exact immutable inventory/source-byte revision;
//! the host remains responsible for binding that revision to real file scans.

use crate::{
    java_class_facts::{JavaFactsError, MAX_JAVA_FACT_BYTES, discover_top_level_java_classes},
    project_model::{Module, ModuleKind, SourceKind, SourceProviderRootKind},
    project_tree::{
        FileFact, FileKind, ModuleLabelPolicy, SourceGroup, SourceProvider, TreeFiles, TreeModel,
        TreeModule, TreeSnapshot, TreeSourceRoot, project_tree_with_label_policy,
    },
    project_tree_facts::{
        ActiveProviderIndex, FactsUnavailable, FactsUnavailableReason, ModuleProjectionFacts,
        ProviderBinding, ProviderPresence, RootEncounterFacts, RootEncounterProvenance,
        RootOccurrence, RootPresence, TreeProjectionFacts,
    },
};
use std::{
    collections::BTreeSet,
    error::Error,
    fmt,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

/// Aggregate parsing work per captured module, in addition to the parser's
/// per-file bound. Input byte storage is supplied by the host, without copying.
pub const MAX_PARSED_JAVA_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KotlinCapability {
    Enabled,
    Disabled,
    Unknown,
}

/// Presentation/language facts require an authoritative host producer. A
/// directory name, source extension, or configured Kotlin root proves neither.
#[derive(Clone, Debug)]
pub struct CapturedModulePresentation {
    pub display_name: Option<String>,
    pub kotlin: KotlinCapability,
    pub compact_packages: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterUnavailableReason {
    MissingPresentation,
    MissingKotlinCapability,
    UnsupportedModule,
    StaleCapture,
    MalformedCapture,
    Provider(FactsUnavailableReason),
    Projection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterUnavailable {
    pub reason: AdapterUnavailableReason,
    pub detail: String,
    pub path: Option<PathBuf>,
}

impl fmt::Display for AdapterUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl Error for AdapterUnavailable {}

impl From<FactsUnavailable> for AdapterUnavailable {
    fn from(failure: FactsUnavailable) -> Self {
        Self {
            reason: AdapterUnavailableReason::Provider(failure.reason),
            detail: failure.detail,
            path: failure.path,
        }
    }
}

fn unavailable(
    reason: AdapterUnavailableReason,
    detail: impl Into<String>,
    path: Option<&Path>,
) -> AdapterUnavailable {
    AdapterUnavailable {
        reason,
        detail: detail.into(),
        path: path.map(Path::to_owned),
    }
}

type AdapterResult<T> = Result<T, AdapterUnavailable>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnsupportedRootKind {
    Provider(SourceProviderRootKind),
    GeneratedManifest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsupportedRoot {
    pub path: PathBuf,
    pub kind: UnsupportedRootKind,
}

/// Checked immutable root preparation for one explicitly selected module.
/// Encounter order is this producer's iterator, not upstream HashMultimap order.
#[derive(Clone, Debug)]
pub struct ModuleRootPlan {
    module: TreeModule,
    providers: ActiveProviderIndex,
    unsupported_roots: Vec<UnsupportedRoot>,
    label_policy: ModuleLabelPolicy,
}

impl ModuleRootPlan {
    pub fn binding(&self) -> &ProviderBinding {
        self.providers.binding()
    }

    pub fn source_roots(&self) -> &[TreeSourceRoot] {
        &self.module.source_roots
    }

    pub fn unsupported_roots(&self) -> &[UnsupportedRoot] {
        &self.unsupported_roots
    }

    /// Relevant physical roots for a future capture producer. This is a scan
    /// requirement list, not proof that any path exists or was fully scanned.
    pub fn required_presence_paths(&self) -> BTreeSet<PathBuf> {
        let mut paths = BTreeSet::from([self.module.directory.clone()]);
        for active in self.providers.providers() {
            for root in &active.provider.roots {
                paths.insert(root.path.clone());
                if root.kind == SourceProviderRootKind::Manifest
                    && let Some(parent) = root.path.parent()
                {
                    paths.insert(parent.into());
                }
            }
        }
        paths.extend(
            self.module
                .source_roots
                .iter()
                .map(|root| root.path.clone()),
        );
        paths.extend(self.unsupported_roots.iter().map(|root| root.path.clone()));
        paths
    }
}

/// Prepare roots from checked selected Studio providers and actual generation
/// flags. Static roots must not be reduced to selected component membership:
/// Studio retains some host containers when a selected artifact is absent.
pub fn prepare_module_roots(
    module: &Module,
    variant: &str,
    model_revision: u64,
    presentation: Option<&CapturedModulePresentation>,
) -> AdapterResult<ModuleRootPlan> {
    prepare_roots(module, variant, model_revision, presentation, false)
}

/// Imported labels may legitimately be empty (a source-set ID ending in `:`).
/// The import publisher must retain the separate internal identity for navigation.
pub(crate) fn prepare_imported_module_roots(
    module: &Module,
    variant: &str,
    model_revision: u64,
    presentation: &CapturedModulePresentation,
) -> AdapterResult<ModuleRootPlan> {
    prepare_roots(module, variant, model_revision, Some(presentation), true)
}

fn prepare_roots(
    module: &Module,
    variant: &str,
    model_revision: u64,
    presentation: Option<&CapturedModulePresentation>,
    imported: bool,
) -> AdapterResult<ModuleRootPlan> {
    if module.kind == ModuleKind::Jvm {
        return Err(unavailable(
            AdapterUnavailableReason::UnsupportedModule,
            "The Android tree adapter does not project JVM modules",
            None,
        ));
    }
    let presentation = presentation.ok_or_else(|| {
        unavailable(
            AdapterUnavailableReason::MissingPresentation,
            "Captured Android module presentation is unavailable",
            None,
        )
    })?;
    let display_name = presentation
        .display_name
        .as_deref()
        .filter(|name| (imported || !name.is_empty()) && !name.contains(['\n', '\r']))
        .ok_or_else(|| {
            unavailable(
                AdapterUnavailableReason::MissingPresentation,
                "Captured Android module display identity is unavailable",
                None,
            )
        })?;
    validate_path(&module.directory)?;
    let providers = ActiveProviderIndex::from_module(module, variant, model_revision)?;
    let selected = module
        .variants
        .iter()
        .find(|candidate| candidate.name == variant)
        .ok_or_else(|| {
            unavailable(
                AdapterUnavailableReason::Provider(FactsUnavailableReason::MissingVariant),
                "Selected module variant is unavailable",
                None,
            )
        })?;
    let mut by_group = std::collections::BTreeMap::<SourceGroup, Vec<PathBuf>>::new();
    let mut unsupported_roots = Vec::new();
    for active in providers.providers() {
        let java = active
            .provider
            .roots
            .iter()
            .filter(|root| root.kind == SourceProviderRootKind::Java)
            .map(|root| root.path.as_path())
            .collect::<BTreeSet<_>>();
        let kotlin = active
            .provider
            .roots
            .iter()
            .filter(|root| root.kind == SourceProviderRootKind::Kotlin)
            .map(|root| root.path.as_path())
            .collect::<BTreeSet<_>>();
        for root in &active.provider.roots {
            let group = match root.kind {
                SourceProviderRootKind::Manifest => Some(SourceGroup::Manifests),
                SourceProviderRootKind::Java => {
                    (!kotlin.contains(root.path.as_path())).then_some(SourceGroup::Java)
                }
                SourceProviderRootKind::Kotlin => {
                    if java.contains(root.path.as_path()) {
                        Some(SourceGroup::KotlinAndJava)
                    } else {
                        Some(SourceGroup::Kotlin)
                    }
                }
                SourceProviderRootKind::Resources => Some(SourceGroup::Resources),
                SourceProviderRootKind::Assets => Some(SourceGroup::Assets),
                kind @ (SourceProviderRootKind::Aidl
                | SourceProviderRootKind::Renderscript
                | SourceProviderRootKind::JniLibs) => {
                    unsupported_roots.push(UnsupportedRoot {
                        path: root.path.clone(),
                        kind: UnsupportedRootKind::Provider(kind),
                    });
                    None
                }
            };
            if let Some(group) = group {
                by_group.entry(group).or_default().push(root.path.clone());
            }
        }
    }
    for component in &selected.components {
        for root in component.sources.iter().filter(|root| root.generated) {
            validate_path(&root.path)?;
            let group = match root.kind {
                SourceKind::Java | SourceKind::Kotlin => Some(SourceGroup::GeneratedJava),
                SourceKind::Resources => Some(SourceGroup::GeneratedResources),
                SourceKind::Assets => Some(SourceGroup::GeneratedAssets),
                SourceKind::Manifest => {
                    unsupported_roots.push(UnsupportedRoot {
                        path: root.path.clone(),
                        kind: UnsupportedRootKind::GeneratedManifest,
                    });
                    None
                }
            };
            if let Some(group) = group {
                by_group.entry(group).or_default().push(root.path.clone());
            }
        }
    }
    // Unsupported built-in types still participate in the pinned shared-root
    // priority. For example AIDL precedes assets, JNI libraries precede res.
    let mut unsupported_priority = std::collections::BTreeMap::new();
    for root in &unsupported_roots {
        let priority = match root.kind {
            UnsupportedRootKind::Provider(SourceProviderRootKind::Aidl) => Some(6),
            UnsupportedRootKind::Provider(SourceProviderRootKind::Renderscript) => Some(7),
            UnsupportedRootKind::Provider(SourceProviderRootKind::JniLibs) => Some(11),
            _ => None,
        };
        if let Some(priority) = priority {
            unsupported_priority
                .entry(root.path.as_path())
                .and_modify(|previous: &mut usize| *previous = (*previous).min(priority))
                .or_insert(priority);
        }
    }
    let mut seen = BTreeSet::new();
    let mut source_roots = Vec::new();
    for (group, paths) in by_group {
        let priority = match group {
            SourceGroup::Manifests => 0,
            SourceGroup::Java => 1,
            SourceGroup::Kotlin => 2,
            SourceGroup::KotlinAndJava => 3,
            SourceGroup::GeneratedJava => 4,
            SourceGroup::Assets => 9,
            SourceGroup::GeneratedAssets => 10,
            SourceGroup::Resources => 12,
            SourceGroup::GeneratedResources => 13,
        };
        for path in paths {
            let shadowed = unsupported_priority
                .get(path.as_path())
                .is_some_and(|earlier| *earlier < priority);
            if shadowed || !seen.insert(path.clone()) {
                continue;
            }
            // The pinned collector moves only surviving common roots after
            // built-in deduplication. A shadowed common root needs no capability.
            let group = if group == SourceGroup::KotlinAndJava {
                match presentation.kotlin {
                    KotlinCapability::Enabled => SourceGroup::KotlinAndJava,
                    KotlinCapability::Disabled => SourceGroup::Java,
                    KotlinCapability::Unknown => {
                        return Err(unavailable(
                            AdapterUnavailableReason::MissingKotlinCapability,
                            "Kotlin enablement is unavailable for surviving shared source roots",
                            Some(&path),
                        ));
                    }
                }
            } else {
                group
            };
            source_roots.push(TreeSourceRoot {
                path,
                group,
                // The facts-aware projection resolves the actual provider per entry.
                provider: SourceProvider::Unknown,
            });
        }
    }
    // Capture the final producer grouping after the disabled-Kotlin move;
    // stable sorting preserves first encounters within each resulting group.
    source_roots.sort_by_key(|root| root.group);
    Ok(ModuleRootPlan {
        module: TreeModule {
            id: module.path.clone(),
            display_name: display_name.into(),
            directory: module.directory.clone(),
            source_roots,
            compact_packages: presentation.compact_packages,
        },
        providers,
        unsupported_roots,
        label_policy: if imported {
            ModuleLabelPolicy::Imported
        } else {
            ModuleLabelPolicy::NonEmpty
        },
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureBinding {
    pub module: String,
    pub variant: String,
    pub model_revision: u64,
    /// Identifies the entire immutable file inventory, presence and byte capture.
    pub file_revision: u64,
}

#[derive(Clone, Debug)]
pub struct CapturedJavaSource {
    pub file_revision: u64,
    pub bytes: Arc<[u8]>,
}

#[derive(Clone, Debug)]
pub enum CapturedEntryKind {
    Directory,
    File {
        java_source: Option<CapturedJavaSource>,
    },
}

#[derive(Clone, Debug)]
pub struct CapturedEntry {
    pub path: PathBuf,
    pub kind: CapturedEntryKind,
}

#[derive(Clone, Debug)]
pub struct CapturedModuleFiles {
    pub binding: CaptureBinding,
    pub entries: Vec<CapturedEntry>,
    /// Explicit probes/scan evidence only. An omitted path remains Unknown.
    pub presence: Vec<(PathBuf, RootPresence)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JavaFileDiagnosticKind {
    MissingBytes,
    Parser(JavaFactsError),
    DuplicateDeclarations,
    InvalidOffset,
    AggregateBudgetExceeded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JavaFileDiagnostic {
    pub path: PathBuf,
    pub kind: JavaFileDiagnosticKind,
}

/// A ready snapshot of the supported subset. Unsupported physical roots remain
/// explicit; this does not assert complete Android view coverage.
#[derive(Clone, Debug)]
pub struct AdaptedModuleTree {
    pub binding: CaptureBinding,
    pub tree: TreeSnapshot,
    pub java_diagnostics: Vec<JavaFileDiagnostic>,
    pub unsupported_roots: Vec<UnsupportedRoot>,
}

pub fn adapt_captured_module(
    plan: &ModuleRootPlan,
    capture: &CapturedModuleFiles,
) -> AdapterResult<AdaptedModuleTree> {
    let binding = plan.binding();
    if capture.binding.module != binding.module
        || capture.binding.variant != binding.variant
        || capture.binding.model_revision != binding.model_revision
    {
        return Err(unavailable(
            AdapterUnavailableReason::StaleCapture,
            "Captured files belong to a different Android module, variant or model revision",
            None,
        ));
    }
    let package_roots = plan
        .source_roots()
        .iter()
        .filter(|root| {
            matches!(
                root.group,
                SourceGroup::Java
                    | SourceGroup::Kotlin
                    | SourceGroup::KotlinAndJava
                    | SourceGroup::GeneratedJava
            )
        })
        .map(|root| root.path.as_path())
        .collect::<BTreeSet<_>>();
    let mut paths = BTreeSet::new();
    let mut presence = ProviderPresence::new(
        binding.model_revision,
        capture.binding.file_revision,
        capture.presence.iter().cloned(),
    )?;
    for entry in &capture.entries {
        validate_path(&entry.path)?;
        if !paths.insert(entry.path.as_path()) {
            return Err(unavailable(
                AdapterUnavailableReason::MalformedCapture,
                "Duplicate path in captured module inventory",
                Some(&entry.path),
            ));
        }
        if entry.path == plan.module.directory {
            presence.add_entry(
                &entry.path,
                match entry.kind {
                    CapturedEntryKind::Directory => RootPresence::Directory,
                    CapturedEntryKind::File { .. } => RootPresence::File,
                },
            )?;
        } else if entry.path.starts_with(&plan.module.directory) {
            // An observed descendant proves its module ancestor directory.
            // Entries in external generated roots prove no module presence.
            presence.add_entry(&plan.module.directory, RootPresence::Directory)?;
        }
    }
    let missing = match presence.get(&plan.module.directory) {
        RootPresence::Directory => None,
        RootPresence::Unknown => Some((
            FactsUnavailableReason::UnknownPresence,
            "Module directory presence is unknown",
        )),
        RootPresence::Missing => Some((
            FactsUnavailableReason::MissingEntry,
            "Module directory is absent",
        )),
        RootPresence::File => Some((
            FactsUnavailableReason::Malformed,
            "Module directory is a file",
        )),
    };
    if let Some((reason, detail)) = missing {
        return Err(unavailable(
            AdapterUnavailableReason::Provider(reason),
            detail,
            Some(&plan.module.directory),
        ));
    }
    let mut entries = Vec::with_capacity(capture.entries.len());
    let mut java_diagnostics = Vec::new();
    let mut parsed_bytes = 0;
    for entry in &capture.entries {
        let kind = match &entry.kind {
            CapturedEntryKind::Directory => FileKind::Directory,
            CapturedEntryKind::File { java_source } => {
                if let Some(source) = java_source {
                    if source.file_revision != capture.binding.file_revision {
                        return Err(unavailable(
                            AdapterUnavailableReason::StaleCapture,
                            "Java source bytes belong to a different captured file revision",
                            Some(&entry.path),
                        ));
                    }
                    if entry
                        .path
                        .extension()
                        .is_none_or(|extension| extension != "java")
                    {
                        return Err(unavailable(
                            AdapterUnavailableReason::MalformedCapture,
                            "Java source bytes were supplied for a non-Java file",
                            Some(&entry.path),
                        ));
                    }
                }
                let class_capable = entry
                    .path
                    .extension()
                    .is_some_and(|extension| extension == "java")
                    && entry
                        .path
                        .ancestors()
                        .any(|path| package_roots.contains(path));
                let mut java_classes = Vec::new();
                if class_capable {
                    let result = match java_source {
                        None => Err(JavaFileDiagnosticKind::MissingBytes),
                        Some(source) => {
                            if source.bytes.len() <= MAX_JAVA_FACT_BYTES
                                && source.bytes.len() > MAX_PARSED_JAVA_BYTES - parsed_bytes
                            {
                                Err(JavaFileDiagnosticKind::AggregateBudgetExceeded)
                            } else {
                                if source.bytes.len() <= MAX_JAVA_FACT_BYTES {
                                    parsed_bytes += source.bytes.len();
                                }
                                discover_top_level_java_classes(&source.bytes)
                                    .map_err(JavaFileDiagnosticKind::Parser)
                                    .and_then(|classes| {
                                        let mut names = BTreeSet::new();
                                        if classes.iter().any(|class| !names.insert(&class.name)) {
                                            return Err(
                                                JavaFileDiagnosticKind::DuplicateDeclarations,
                                            );
                                        }
                                        let source_text = std::str::from_utf8(&source.bytes)
                                            .map_err(|_| JavaFileDiagnosticKind::InvalidOffset)?;
                                        for class in &classes {
                                            let start = class
                                                .byte_offset
                                                .ok_or(JavaFileDiagnosticKind::InvalidOffset)?;
                                            let end = start
                                                .checked_add(class.name.len())
                                                .ok_or(JavaFileDiagnosticKind::InvalidOffset)?;
                                            if source_text.get(start..end)
                                                != Some(class.name.as_str())
                                            {
                                                return Err(JavaFileDiagnosticKind::InvalidOffset);
                                            }
                                        }
                                        Ok(classes)
                                    })
                            }
                        }
                    };
                    match result {
                        Ok(classes) => java_classes = classes,
                        Err(kind) => java_diagnostics.push(JavaFileDiagnostic {
                            path: entry.path.clone(),
                            kind,
                        }),
                    }
                }
                FileKind::File { java_classes }
            }
        };
        entries.push(FileFact {
            path: entry.path.clone(),
            kind,
        });
    }
    // Observed children prove ancestor directories, including a source root
    // whose directory row was omitted from the supplied inventory. This never
    // infers Missing or allocates a filesystem entry identifier.
    let directory_roots = plan
        .source_roots()
        .iter()
        .filter(|root| root.group != SourceGroup::Manifests)
        .map(|root| root.path.as_path())
        .chain(std::iter::once(plan.module.directory.as_path()))
        .collect::<BTreeSet<_>>();
    let inferred = {
        let mut completed_ancestors = BTreeSet::new();
        let mut inferred = BTreeSet::new();
        for entry in &entries {
            for ancestor in entry.path.ancestors().skip(1) {
                if !completed_ancestors.insert(ancestor) {
                    break;
                }
                if directory_roots.contains(ancestor) && !paths.contains(ancestor) {
                    inferred.insert(ancestor.to_owned());
                }
            }
        }
        inferred
    };
    entries.extend(inferred.into_iter().map(|path| FileFact {
        path,
        kind: FileKind::Directory,
    }));
    let files = TreeFiles {
        model_revision: binding.model_revision,
        revision: capture.binding.file_revision,
        entries,
    };
    let model = TreeModel {
        revision: binding.model_revision,
        modules: vec![plan.module.clone()],
    };
    let facts = TreeProjectionFacts {
        presence,
        modules: vec![ModuleProjectionFacts {
            providers: plan.providers.clone(),
            roots: RootEncounterFacts {
                model_revision: binding.model_revision,
                module: binding.module.clone(),
                variant: binding.variant.clone(),
                provenance: RootEncounterProvenance::ProducerIterator,
                roots: plan
                    .source_roots()
                    .iter()
                    .map(|root| RootOccurrence {
                        group: root.group,
                        path: root.path.clone(),
                    })
                    .collect(),
            },
        }],
    };
    let tree = project_tree_with_label_policy(&model, &files, &facts, plan.label_policy).map_err(
        |failure| {
            if let Some(provider) = failure.downcast_ref::<FactsUnavailable>() {
                provider.clone().into()
            } else {
                unavailable(
                    AdapterUnavailableReason::Projection,
                    failure.to_string(),
                    None,
                )
            }
        },
    )?;
    Ok(AdaptedModuleTree {
        binding: capture.binding.clone(),
        tree,
        java_diagnostics,
        unsupported_roots: plan.unsupported_roots.clone(),
    })
}

fn validate_path(path: &Path) -> AdapterResult<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        || path.components().collect::<PathBuf>().as_os_str() != path.as_os_str()
    {
        return Err(unavailable(
            AdapterUnavailableReason::MalformedCapture,
            "Captured path is not absolute and normalized",
            Some(path),
        ));
    }
    Ok(())
}
