/*
 * Copyright (C) 2014 The Android Open Source Project
 * Copyright (C) 2017 The Android Open Source Project
 * Copyright (C) 2019 The Android Open Source Project
 * Copyright (C) 2020 The Android Open Source Project
 * Copyright (C) 2021 The Android Open Source Project
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

//! Evaluated provider selection and per-entry lookup for background tree producers.
//!
//! Selection mirrors the pinned Studio collector, including its forward-flavor
//! TODO. Native AGP membership remains a distinct fact. Presence is explicit:
//! configured paths alone never establish that a VFS root exists.

use crate::project_model::{
    EvaluatedProviderMetadata, Module, NativeProviderMembership, ProviderArtifact,
    ProviderContainer, ProviderDimension, ProviderToolingModel, ProviderVariant, SourceProvider,
    SourceProviderRootKind,
};
use crate::project_tree::SourceGroup;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::{Component, Path, PathBuf},
};

const UNIT_TEST: &str = "_unit_test_";
const SCREENSHOT_TEST: &str = "_screenshot_test_";
const ANDROID_TEST: &str = "_android_test_";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FactsUnavailableReason {
    MissingMetadata,
    Capability,
    UnsupportedSchema,
    UnsupportedShape,
    MissingVariant,
    Malformed,
    Stale,
    UnknownPresence,
    MissingEntry,
    UnknownEncounter,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactsUnavailable {
    pub reason: FactsUnavailableReason,
    pub detail: String,
    pub path: Option<PathBuf>,
}

impl fmt::Display for FactsUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for FactsUnavailable {}

fn unavailable(reason: FactsUnavailableReason, detail: impl Into<String>) -> FactsUnavailable {
    FactsUnavailable {
        reason,
        detail: detail.into(),
        path: None,
    }
}

type FactsResult<T> = Result<T, FactsUnavailable>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderBinding {
    pub model_revision: u64,
    pub module: String,
    pub variant: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderRole {
    Main,
    UnitTest,
    ScreenshotTest,
    AndroidTest,
    TestSuite(String),
    TestFixtures,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveProvider {
    pub role: ProviderRole,
    pub provider: SourceProvider,
}

#[derive(Clone, Debug)]
pub struct ActiveProviderIndex {
    binding: ProviderBinding,
    providers: Vec<ActiveProvider>,
    native_membership: Option<Vec<NativeProviderMembership>>,
    lookup: ProviderLookupIndex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum RootCategory {
    Manifest,
    Java,
    Kotlin,
    Resources,
    Aidl,
    Renderscript,
    Assets,
    JniLibs,
}

impl From<SourceProviderRootKind> for RootCategory {
    fn from(kind: SourceProviderRootKind) -> Self {
        match kind {
            SourceProviderRootKind::Manifest => Self::Manifest,
            SourceProviderRootKind::Java => Self::Java,
            SourceProviderRootKind::Kotlin => Self::Kotlin,
            SourceProviderRootKind::Resources => Self::Resources,
            SourceProviderRootKind::Aidl => Self::Aidl,
            SourceProviderRootKind::Renderscript => Self::Renderscript,
            SourceProviderRootKind::Assets => Self::Assets,
            SourceProviderRootKind::JniLibs => Self::JniLibs,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum MatchRequirement {
    Directory,
    ManifestFile,
    ManifestDirectory,
}

#[derive(Clone, Copy, Debug)]
struct IndexedRoot {
    provider_encounter: usize,
    category: RootCategory,
    root_encounter: usize,
    requirement: MatchRequirement,
}

#[derive(Clone, Debug, Default)]
struct ProviderLookupIndex {
    directories: BTreeMap<PathBuf, Vec<IndexedRoot>>,
    manifests: BTreeMap<PathBuf, Vec<IndexedRoot>>,
    resources: BTreeMap<PathBuf, Vec<usize>>,
}

impl ProviderLookupIndex {
    fn new(providers: &[ActiveProvider]) -> Self {
        let mut index = Self::default();
        for (provider_encounter, active) in providers.iter().enumerate() {
            for (root_encounter, root) in active.provider.roots.iter().enumerate() {
                let rule = IndexedRoot {
                    provider_encounter,
                    category: root.kind.into(),
                    root_encounter,
                    requirement: MatchRequirement::Directory,
                };
                if root.kind == SourceProviderRootKind::Manifest {
                    index
                        .manifests
                        .entry(root.path.clone())
                        .or_default()
                        .push(IndexedRoot {
                            requirement: MatchRequirement::ManifestFile,
                            ..rule
                        });
                    if let Some(parent) = root.path.parent() {
                        index
                            .manifests
                            .entry(parent.into())
                            .or_default()
                            .push(IndexedRoot {
                                requirement: MatchRequirement::ManifestDirectory,
                                ..rule
                            });
                    }
                } else {
                    index
                        .directories
                        .entry(root.path.clone())
                        .or_default()
                        .push(rule);
                    if root.kind == SourceProviderRootKind::Resources {
                        let providers = index.resources.entry(root.path.clone()).or_default();
                        if providers.last() != Some(&provider_encounter) {
                            providers.push(provider_encounter);
                        }
                    }
                }
            }
        }
        index
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootPresence {
    Directory,
    File,
    Missing,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct ProviderPresence {
    pub model_revision: u64,
    pub file_revision: u64,
    paths: BTreeMap<PathBuf, RootPresence>,
}

impl ProviderPresence {
    pub fn new(
        model_revision: u64,
        file_revision: u64,
        paths: impl IntoIterator<Item = (PathBuf, RootPresence)>,
    ) -> FactsResult<Self> {
        let mut checked = BTreeMap::new();
        for (path, presence) in paths {
            validate_path(&path)?;
            if let Some(previous) = checked.insert(path.clone(), presence) {
                if previous != presence {
                    return Err(unavailable(
                        FactsUnavailableReason::Malformed,
                        format!("Conflicting presence for {}", path.display()),
                    ));
                }
            }
        }
        validate_presence_hierarchy(&checked)?;
        Ok(Self {
            model_revision,
            file_revision,
            paths: checked,
        })
    }

    pub fn get(&self, path: &Path) -> RootPresence {
        self.paths
            .get(path)
            .copied()
            .unwrap_or(RootPresence::Unknown)
    }

    pub(crate) fn validate(&self) -> FactsResult<()> {
        validate_presence_hierarchy(&self.paths)
    }

    pub(crate) fn add_entry(&mut self, path: &Path, presence: RootPresence) -> FactsResult<()> {
        validate_path(path)?;
        let previous = self.get(path);
        if previous != RootPresence::Unknown && previous != presence {
            return Err(unavailable(
                FactsUnavailableReason::Malformed,
                format!("Presence disagrees with file facts for {}", path.display()),
            ));
        }
        self.paths.insert(path.into(), presence);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootEncounterProvenance {
    /// Captured iteration of the producer's source-root collection. This alone
    /// does not establish the full Studio AndroidSourceType/model contract.
    ProducerIterator,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RootOccurrence {
    pub group: SourceGroup,
    pub path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct RootEncounterFacts {
    pub model_revision: u64,
    pub module: String,
    pub variant: String,
    pub provenance: RootEncounterProvenance,
    pub roots: Vec<RootOccurrence>,
}

#[derive(Clone, Debug)]
pub struct ModuleProjectionFacts {
    pub providers: ActiveProviderIndex,
    pub roots: RootEncounterFacts,
}

#[derive(Clone, Debug)]
pub struct TreeProjectionFacts {
    pub presence: ProviderPresence,
    pub modules: Vec<ModuleProjectionFacts>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderCandidate {
    pub encounter: usize,
    pub role: ProviderRole,
    pub name: String,
    pub root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderResolution {
    /// Ordered candidates are retained even though the pinned lookup uses the first.
    pub candidates: Vec<ProviderCandidate>,
}

impl ProviderResolution {
    pub fn winner(&self) -> Option<&ProviderCandidate> {
        self.candidates.first()
    }
}

impl ActiveProviderIndex {
    pub fn from_module(module: &Module, variant: &str, model_revision: u64) -> FactsResult<Self> {
        let model = match &module.evaluated_providers {
            Some(EvaluatedProviderMetadata::Available(model)) => model,
            Some(EvaluatedProviderMetadata::Unavailable(failure)) => {
                return Err(unavailable(
                    FactsUnavailableReason::Capability,
                    format!("{}: {}", failure.capability, failure.detail),
                ));
            }
            None => {
                return Err(unavailable(
                    FactsUnavailableReason::MissingMetadata,
                    "Evaluated active provider metadata is unavailable",
                ));
            }
        };
        validate_model(model)?;
        let selected = model
            .variants
            .iter()
            .find(|candidate| candidate.name == variant)
            .ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::MissingVariant,
                    format!("Variant {variant} is absent from evaluated provider metadata"),
                )
            })?;
        if !module
            .variants
            .iter()
            .any(|candidate| candidate.name == variant)
        {
            return Err(unavailable(
                FactsUnavailableReason::MissingVariant,
                "Evaluated provider variant is absent from the module model",
            ));
        }
        let mut providers = Vec::new();
        let main = collect(model, selected, &ProviderRole::Main, Some(&selected.main))?;
        append(&mut providers, ProviderRole::Main, main);
        // The pinned host collector retains all-variant container providers even
        // when the selected variant does not contain a unit/screenshot artifact.
        let unit = selected
            .host_tests
            .iter()
            .find(|artifact| artifact.artifact == UNIT_TEST);
        append(
            &mut providers,
            ProviderRole::UnitTest,
            collect(
                model,
                selected,
                &ProviderRole::UnitTest,
                unit.map(|artifact| &artifact.sources),
            )?,
        );
        let screenshot = selected
            .host_tests
            .iter()
            .find(|artifact| artifact.artifact == SCREENSHOT_TEST);
        append(
            &mut providers,
            ProviderRole::ScreenshotTest,
            collect(
                model,
                selected,
                &ProviderRole::ScreenshotTest,
                screenshot.map(|artifact| &artifact.sources),
            )?,
        );
        if let Some(device) = selected
            .device_tests
            .iter()
            .find(|artifact| artifact.artifact == ANDROID_TEST)
        {
            append(
                &mut providers,
                ProviderRole::AndroidTest,
                collect(
                    model,
                    selected,
                    &ProviderRole::AndroidTest,
                    Some(&device.sources),
                )?,
            );
        }
        let suites = match &selected.test_suites {
            Some(suites) => suites.as_slice(),
            None if model.model_producer.major < 16 => &[],
            None => {
                return Err(unavailable(
                    FactsUnavailableReason::Capability,
                    "Selected test-suite membership is unavailable",
                ));
            }
        };
        if !suites.is_empty() {
            let definitions = model.test_suites.as_ref().ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::Capability,
                    "Test-suite definitions are unavailable",
                )
            })?;
            for name in suites {
                let definition = definitions
                    .iter()
                    .find(|definition| &definition.name == name)
                    .ok_or_else(|| {
                        unavailable(
                            FactsUnavailableReason::Malformed,
                            format!("Selected test suite {name} has no definition"),
                        )
                    })?;
                let suite = definition.providers.as_ref().ok_or_else(|| {
                    unavailable(
                        FactsUnavailableReason::UnsupportedShape,
                        format!("Test-suite source conversion is unavailable for {name}"),
                    )
                })?;
                append(
                    &mut providers,
                    ProviderRole::TestSuite(name.clone()),
                    suite.iter().collect(),
                );
            }
        }
        if let Some(fixtures) = &selected.fixtures {
            append(
                &mut providers,
                ProviderRole::TestFixtures,
                collect(model, selected, &ProviderRole::TestFixtures, Some(fixtures))?,
            );
        }
        let lookup = ProviderLookupIndex::new(&providers);
        Ok(Self {
            lookup,
            binding: ProviderBinding {
                model_revision,
                module: module.path.clone(),
                variant: variant.into(),
            },
            providers,
            native_membership: model.native_membership.as_ref().map(|membership| {
                membership
                    .iter()
                    .filter(|membership| membership.variant == variant)
                    .cloned()
                    .collect()
            }),
        })
    }

    pub fn binding(&self) -> &ProviderBinding {
        &self.binding
    }
    pub fn providers(&self) -> &[ActiveProvider] {
        &self.providers
    }
    pub fn native_membership(&self) -> Option<&[NativeProviderMembership]> {
        self.native_membership.as_deref()
    }

    pub fn resolve(
        &self,
        path: &Path,
        presence: &ProviderPresence,
    ) -> FactsResult<ProviderResolution> {
        validate_path(path)?;
        if self.binding.model_revision != presence.model_revision {
            return Err(unavailable(
                FactsUnavailableReason::Stale,
                "Provider presence belongs to an obsolete model",
            ));
        }
        match presence.get(path) {
            RootPresence::Unknown => return Err(unknown_presence(path)),
            RootPresence::Missing => {
                return Err(unavailable(
                    FactsUnavailableReason::MissingEntry,
                    format!("The candidate entry is absent: {}", path.display()),
                ));
            }
            RootPresence::Directory | RootPresence::File => {}
        }
        let mut matching_rules = Vec::new();
        if let Some((root, rules)) = self.lookup.manifests.get_key_value(path) {
            matching_rules.extend(rules.iter().map(|rule| (root.as_path(), rule)));
        }
        for ancestor in path.ancestors() {
            if let Some((root, rules)) = self.lookup.directories.get_key_value(ancestor) {
                matching_rules.extend(rules.iter().map(|rule| (root.as_path(), rule)));
            }
        }
        matching_rules
            .sort_by_key(|(_, rule)| (rule.provider_encounter, rule.category, rule.root_encounter));
        let mut candidates = Vec::new();
        let mut completed_provider = None;
        for (root, rule) in matching_rules {
            // Once a provider matches, its later rules never participate in
            // findSourceRoot, including later paths with unknown presence.
            if completed_provider == Some(rule.provider_encounter) {
                continue;
            }
            let matched = match rule.requirement {
                MatchRequirement::ManifestFile => presence.get(path) == RootPresence::File,
                MatchRequirement::ManifestDirectory => {
                    presence.get(path) == RootPresence::Directory
                }
                MatchRequirement::Directory => match presence.get(root) {
                    RootPresence::Directory => true,
                    RootPresence::Missing => false,
                    RootPresence::Unknown => return Err(unknown_presence(root)),
                    RootPresence::File => {
                        return Err(unavailable(
                            FactsUnavailableReason::Malformed,
                            format!("A configured directory root is a file: {}", root.display()),
                        ));
                    }
                },
            };
            if matched {
                let active = self.providers.get(rule.provider_encounter).ok_or_else(|| {
                    unavailable(
                        FactsUnavailableReason::Malformed,
                        "Indexed provider encounter is unavailable",
                    )
                })?;
                candidates.push(ProviderCandidate {
                    encounter: rule.provider_encounter,
                    role: active.role.clone(),
                    name: active.provider.name.clone(),
                    root: root.into(),
                });
                completed_provider = Some(rule.provider_encounter);
            }
        }
        Ok(ProviderResolution { candidates })
    }

    /// AndroidResFileNode uses exact `resDirectories` membership, distinct from
    /// the generic findByFile ancestry policy used by ordinary files.
    pub fn resolve_resource_root(
        &self,
        root: &Path,
        presence: &ProviderPresence,
    ) -> FactsResult<ProviderResolution> {
        validate_path(root)?;
        if self.binding.model_revision != presence.model_revision {
            return Err(unavailable(
                FactsUnavailableReason::Stale,
                "Resource-root presence belongs to an obsolete model",
            ));
        }
        match presence.get(root) {
            RootPresence::Directory => {}
            RootPresence::Unknown => return Err(unknown_presence(root)),
            RootPresence::Missing => {
                return Err(unavailable(
                    FactsUnavailableReason::MissingEntry,
                    format!("The resource root is absent: {}", root.display()),
                ));
            }
            RootPresence::File => {
                return Err(unavailable(
                    FactsUnavailableReason::Malformed,
                    format!("The resource root is a file: {}", root.display()),
                ));
            }
        }
        let mut candidates = Vec::new();
        if let Some(encounters) = self.lookup.resources.get(root) {
            for encounter in encounters {
                let active = self.providers.get(*encounter).ok_or_else(|| {
                    unavailable(
                        FactsUnavailableReason::Malformed,
                        "Indexed resource provider encounter is unavailable",
                    )
                })?;
                candidates.push(ProviderCandidate {
                    encounter: *encounter,
                    role: active.role.clone(),
                    name: active.provider.name.clone(),
                    root: root.into(),
                });
            }
        }
        Ok(ProviderResolution { candidates })
    }
}

fn append(output: &mut Vec<ActiveProvider>, role: ProviderRole, providers: Vec<&SourceProvider>) {
    output.extend(providers.into_iter().map(|provider| ActiveProvider {
        role: role.clone(),
        provider: provider.clone(),
    }));
}

fn collect<'a>(
    model: &'a ProviderToolingModel,
    variant: &ProviderVariant,
    role: &ProviderRole,
    artifact: Option<&'a ProviderArtifact>,
) -> FactsResult<Vec<&'a SourceProvider>> {
    let mut providers = Vec::new();
    if let Some(container) = &model.default_source_set {
        if let Some(provider) = select(container, role) {
            providers.push(provider);
        }
    }
    // Preserve the upstream forward-flavor TODO; native AGP reverses these.
    for flavor in &variant.product_flavors {
        if let Some(container) = dimension(&model.product_flavors, flavor) {
            if let Some(provider) = select(container, role) {
                providers.push(provider);
            }
        }
    }
    if let Some(provider) = artifact.and_then(|artifact| artifact.multi_flavor.as_ref()) {
        providers.push(provider);
    }
    if let Some(container) = variant
        .build_type
        .as_deref()
        .and_then(|name| dimension(&model.build_types, name))
    {
        if let Some(provider) = select(container, role) {
            providers.push(provider);
        }
    }
    if let Some(provider) = artifact.and_then(|artifact| artifact.variant.as_ref()) {
        providers.push(provider);
    }
    Ok(providers)
}

fn dimension<'a>(dimensions: &'a [ProviderDimension], name: &str) -> Option<&'a ProviderContainer> {
    dimensions
        .iter()
        .find(|dimension| dimension.name.as_deref() == Some(name))
        .map(|dimension| &dimension.container)
}

fn select<'a>(container: &'a ProviderContainer, role: &ProviderRole) -> Option<&'a SourceProvider> {
    match role {
        ProviderRole::Main => container.main.as_ref(),
        ProviderRole::UnitTest => container
            .host_tests
            .iter()
            .find(|provider| provider.artifact == UNIT_TEST)
            .map(|provider| &provider.provider),
        ProviderRole::ScreenshotTest => container
            .host_tests
            .iter()
            .find(|provider| provider.artifact == SCREENSHOT_TEST)
            .map(|provider| &provider.provider),
        ProviderRole::AndroidTest => container
            .device_tests
            .iter()
            .find(|provider| provider.artifact == ANDROID_TEST)
            .map(|provider| &provider.provider),
        ProviderRole::TestFixtures => container.fixtures.as_ref(),
        ProviderRole::TestSuite(_) => None,
    }
}

fn validate_presence_hierarchy(checked: &BTreeMap<PathBuf, RootPresence>) -> FactsResult<()> {
    let mut checked_ancestors = BTreeSet::new();
    for (path, presence) in checked {
        if !matches!(presence, RootPresence::Directory | RootPresence::File) {
            continue;
        }
        for ancestor in path.ancestors().skip(1) {
            if checked_ancestors.contains(ancestor) {
                break;
            }
            if checked.get(ancestor).is_some_and(|presence| {
                matches!(presence, RootPresence::Missing | RootPresence::File)
            }) {
                return Err(unavailable(
                    FactsUnavailableReason::Malformed,
                    format!(
                        "A present path has a missing/file ancestor: {}",
                        ancestor.display()
                    ),
                ));
            }
            checked_ancestors.insert(ancestor.to_owned());
        }
    }
    Ok(())
}

pub(crate) fn unknown_presence(path: &Path) -> FactsUnavailable {
    FactsUnavailable {
        reason: FactsUnavailableReason::UnknownPresence,
        detail: format!("Filesystem presence is unknown for {}", path.display()),
        path: Some(path.into()),
    }
}

fn validate_model(model: &ProviderToolingModel) -> FactsResult<()> {
    if model.version != 1 {
        return Err(unavailable(
            FactsUnavailableReason::UnsupportedSchema,
            format!("Unsupported evaluated provider schema {}", model.version),
        ));
    }
    if model.model_producer.major < 11 {
        return Err(unavailable(
            FactsUnavailableReason::UnsupportedShape,
            "Model producers before 11 require the unimplemented legacy asset filter",
        ));
    }
    valid_name(&model.agp_version)?;
    if let Some(container) = &model.default_source_set {
        validate_container(container)?;
    }
    for dimensions in [&model.build_types, &model.product_flavors] {
        let mut names = BTreeSet::new();
        for dimension in dimensions {
            let name = dimension.name.as_deref().ok_or_else(|| {
                unavailable(
                    FactsUnavailableReason::UnsupportedShape,
                    "A model dimension has no authoritative identity",
                )
            })?;
            valid_name(name)?;
            if !names.insert(name) {
                return Err(unavailable(
                    FactsUnavailableReason::Malformed,
                    "Duplicate model dimension",
                ));
            }
            validate_container(&dimension.container)?;
        }
    }
    let mut variants = BTreeSet::new();
    for variant in &model.variants {
        valid_name(&variant.name)?;
        if !variants.insert(&variant.name) {
            return Err(unavailable(
                FactsUnavailableReason::Malformed,
                "Duplicate provider variant",
            ));
        }
        if let Some(build_type) = &variant.build_type {
            valid_name(build_type)?;
        }
        let mut flavors = BTreeSet::new();
        for flavor in &variant.product_flavors {
            valid_name(flavor)?;
            if !flavors.insert(flavor) {
                return Err(unavailable(
                    FactsUnavailableReason::Malformed,
                    "Duplicate selected flavor",
                ));
            }
        }
        validate_artifact(&variant.main)?;
        for artifacts in [&variant.host_tests, &variant.device_tests] {
            let mut names = BTreeSet::new();
            for artifact in artifacts {
                valid_name(&artifact.artifact)?;
                if !names.insert(&artifact.artifact) {
                    return Err(unavailable(
                        FactsUnavailableReason::Malformed,
                        "Duplicate variant artifact",
                    ));
                }
                validate_artifact(&artifact.sources)?;
            }
        }
        if variant
            .host_tests
            .iter()
            .any(|artifact| !matches!(artifact.artifact.as_str(), UNIT_TEST | SCREENSHOT_TEST))
            || variant
                .device_tests
                .iter()
                .any(|artifact| artifact.artifact != ANDROID_TEST)
        {
            return Err(unavailable(
                FactsUnavailableReason::UnsupportedShape,
                "Unknown test artifact membership",
            ));
        }
        if let Some(fixtures) = &variant.fixtures {
            validate_artifact(fixtures)?;
        }
        if let Some(suites) = &variant.test_suites {
            unique_names(suites.iter().map(String::as_str))?;
        }
    }
    if let Some(suites) = &model.test_suites {
        unique_names(suites.iter().map(|suite| suite.name.as_str()))?;
        for suite in suites {
            if let Some(providers) = &suite.providers {
                for provider in providers {
                    validate_provider(provider)?;
                }
            }
        }
    }
    let mut native = BTreeSet::new();
    for membership in model.native_membership.iter().flatten() {
        valid_name(&membership.variant)?;
        valid_name(&membership.component)?;
        valid_name(&membership.artifact)?;
        if !native.insert((
            &membership.variant,
            &membership.component,
            &membership.artifact,
        )) {
            return Err(unavailable(
                FactsUnavailableReason::Malformed,
                "Duplicate native component membership",
            ));
        }
        if let Some(providers) = &membership.providers {
            for name in providers {
                valid_name(name)?;
            }
        }
    }
    Ok(())
}

fn validate_container(container: &ProviderContainer) -> FactsResult<()> {
    if let Some(provider) = &container.main {
        validate_provider(provider)?;
    }
    for entries in [&container.host_tests, &container.device_tests] {
        unique_names(entries.iter().map(|entry| entry.artifact.as_str()))?;
        for entry in entries {
            validate_provider(&entry.provider)?;
            // The pinned converter classifies extra providers by these prefixes.
            // Check agreement with the authoritative artifact key, never assign
            // an unproved role from a source-set name.
            let classified = if entry.provider.name.starts_with("androidTest") {
                ANDROID_TEST
            } else if entry.provider.name.starts_with("testFixtures") {
                "_test_fixtures_"
            } else if entry.provider.name.starts_with("screenshotTest") {
                SCREENSHOT_TEST
            } else {
                UNIT_TEST
            };
            if classified != entry.artifact {
                return Err(unavailable(
                    FactsUnavailableReason::UnsupportedShape,
                    "Explicit artifact membership disagrees with the pinned Studio converter",
                ));
            }
        }
    }
    if container
        .host_tests
        .iter()
        .any(|entry| !matches!(entry.artifact.as_str(), UNIT_TEST | SCREENSHOT_TEST))
        || container
            .device_tests
            .iter()
            .any(|entry| entry.artifact != ANDROID_TEST)
    {
        return Err(unavailable(
            FactsUnavailableReason::UnsupportedShape,
            "Unknown container test artifact",
        ));
    }
    if let Some(provider) = &container.fixtures {
        validate_provider(provider)?;
        if !provider.name.starts_with("testFixtures") {
            return Err(unavailable(
                FactsUnavailableReason::UnsupportedShape,
                "Fixture provider disagrees with the pinned Studio converter",
            ));
        }
    }
    Ok(())
}

fn validate_artifact(artifact: &ProviderArtifact) -> FactsResult<()> {
    for provider in [&artifact.multi_flavor, &artifact.variant]
        .into_iter()
        .flatten()
    {
        validate_provider(provider)?;
    }
    Ok(())
}

fn validate_provider(provider: &SourceProvider) -> FactsResult<()> {
    valid_name(&provider.name)?;
    for root in &provider.roots {
        validate_path(&root.path)?;
    }
    Ok(())
}

fn unique_names<'a>(names: impl IntoIterator<Item = &'a str>) -> FactsResult<()> {
    let mut unique = BTreeSet::new();
    for name in names {
        valid_name(name)?;
        if !unique.insert(name) {
            return Err(unavailable(
                FactsUnavailableReason::Malformed,
                "Duplicate evaluated identity",
            ));
        }
    }
    Ok(())
}

fn valid_name(name: &str) -> FactsResult<()> {
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        return Err(unavailable(
            FactsUnavailableReason::Malformed,
            "Invalid evaluated identity",
        ));
    }
    Ok(())
}

fn validate_path(path: &Path) -> FactsResult<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
        || path.components().collect::<PathBuf>().as_os_str() != path.as_os_str()
    {
        return Err(unavailable(
            FactsUnavailableReason::Malformed,
            format!(
                "Provider path is not absolute and normalized: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}
