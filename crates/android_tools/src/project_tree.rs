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

//! Pure projection of immutable source and file facts into Android virtual nodes.
//!
//! Adapted node contracts and unchanged reference files are retained under
//! `test_data/project_tree`. This does not import Gradle, parse Java, walk disk,
//! or install GPUI controls. Producers must supply actual model/provider/class
//! facts and reject obsolete results before publishing a snapshot.

use crate::project_tree_facts::{
    RootEncounterProvenance, RootOccurrence, RootPresence, TreeProjectionFacts,
};
use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

/// Declaration order follows the supported subset of AndroidSourceType's
/// BUILT_IN_TYPES. That order also resolves identical roots shared by types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SourceGroup {
    Manifests,
    Java,
    Kotlin,
    KotlinAndJava,
    GeneratedJava,
    Assets,
    GeneratedAssets,
    Resources,
    GeneratedResources,
}

impl SourceGroup {
    pub fn label(self) -> &'static str {
        match self {
            Self::Manifests => "manifests",
            Self::Java | Self::GeneratedJava => "java",
            Self::Kotlin => "kotlin",
            Self::KotlinAndJava => "kotlin+java",
            Self::Assets | Self::GeneratedAssets => "assets",
            Self::Resources | Self::GeneratedResources => "res",
        }
    }

    pub fn is_generated(self) -> bool {
        matches!(
            self,
            Self::GeneratedJava | Self::GeneratedAssets | Self::GeneratedResources
        )
    }

    fn is_package_group(self) -> bool {
        matches!(
            self,
            Self::Java | Self::Kotlin | Self::KotlinAndJava | Self::GeneratedJava
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceProvider {
    Named(String),
    Unattributed,
    Unknown,
}

impl SourceProvider {
    fn name(&self) -> Result<Option<&str>> {
        match self {
            Self::Named(name) => {
                ensure!(!name.is_empty(), "Source-provider name is empty");
                Ok(Some(name))
            }
            Self::Unattributed => Ok(None),
            Self::Unknown => bail!("Source-provider metadata is unavailable"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeSourceRoot {
    pub path: PathBuf,
    pub group: SourceGroup,
    pub provider: SourceProvider,
}

#[derive(Clone, Debug)]
pub struct TreeModule {
    pub id: String,
    pub display_name: String,
    pub directory: PathBuf,
    pub source_roots: Vec<TreeSourceRoot>,
    pub compact_packages: bool,
}

#[derive(Clone, Debug)]
pub struct TreeModel {
    pub revision: u64,
    pub modules: Vec<TreeModule>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JavaClassFact {
    pub name: String,
    pub byte_offset: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileKind {
    Directory,
    File { java_classes: Vec<JavaClassFact> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileFact {
    pub path: PathBuf,
    pub kind: FileKind,
}

#[derive(Clone, Debug)]
pub struct TreeFiles {
    /// The model revision captured when the producer assembled these facts.
    pub model_revision: u64,
    pub revision: u64,
    pub entries: Vec<FileFact>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum NodeKey {
    Module {
        module: String,
    },
    Source {
        module: String,
        group: SourceGroup,
    },
    Directory {
        module: String,
        group: SourceGroup,
        source_root: PathBuf,
        path: PathBuf,
    },
    ResourceType {
        module: String,
        group: SourceGroup,
        folder_type: String,
    },
    ResourceGroup {
        module: String,
        group: SourceGroup,
        folder_type: String,
        name: String,
    },
    File {
        module: String,
        group: Option<SourceGroup>,
        source_root: Option<PathBuf>,
        path: PathBuf,
    },
    JavaClass {
        module: String,
        group: SourceGroup,
        source_root: PathBuf,
        path: PathBuf,
        name: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeId(usize);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavigationTarget {
    pub path: PathBuf,
    pub byte_offset: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeNode {
    pub key: NodeKey,
    pub label: String,
    pub annotation: Option<String>,
    /// Android Studio's toTestString may differ from rendered presentation.
    pub reference_label: String,
    pub navigation: Option<NavigationTarget>,
    pub children: Vec<NodeId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeSnapshot {
    pub model_revision: u64,
    pub file_revision: u64,
    pub roots: Vec<NodeId>,
    nodes: Vec<TreeNode>,
}

impl TreeSnapshot {
    pub fn node(&self, id: NodeId) -> Option<&TreeNode> {
        self.nodes.get(id.0)
    }

    pub fn nodes(&self) -> impl Iterator<Item = &TreeNode> {
        self.nodes.iter()
    }

    fn add(&mut self, parent: Option<NodeId>, node: TreeNode) -> NodeId {
        let id = NodeId(self.nodes.len());
        self.nodes.push(node);
        if let Some(parent) = parent {
            self.nodes[parent.0].children.push(id);
        } else {
            self.roots.push(id);
        }
        id
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModuleLabelPolicy {
    NonEmpty,
    Imported,
}

pub fn project_tree(model: &TreeModel, files: &TreeFiles) -> Result<TreeSnapshot> {
    project_tree_internal(model, files, None, ModuleLabelPolicy::NonEmpty)
}

/// Preserves explicitly captured root encounters and resolves annotations for
/// each entry. This installs no model-to-group adapter or GPUI workflow.
pub fn project_tree_with_facts(
    model: &TreeModel,
    files: &TreeFiles,
    facts: &TreeProjectionFacts,
) -> Result<TreeSnapshot> {
    project_tree_internal(model, files, Some(facts), ModuleLabelPolicy::NonEmpty)
}

pub(crate) fn project_tree_with_label_policy(
    model: &TreeModel,
    files: &TreeFiles,
    facts: &TreeProjectionFacts,
    label_policy: ModuleLabelPolicy,
) -> Result<TreeSnapshot> {
    project_tree_internal(model, files, Some(facts), label_policy)
}

fn project_tree_internal(
    model: &TreeModel,
    files: &TreeFiles,
    facts: Option<&TreeProjectionFacts>,
    label_policy: ModuleLabelPolicy,
) -> Result<TreeSnapshot> {
    ensure!(
        model.revision == files.model_revision,
        "File facts belong to an obsolete Android model"
    );
    let mut entries = BTreeMap::new();
    for entry in &files.entries {
        validate_path(&entry.path)?;
        if let Some(previous) = entries.insert(entry.path.clone(), entry) {
            ensure!(
                previous == entry,
                "Conflicting file facts for {}",
                entry.path.display()
            );
        }
    }
    let mut checked_directories = BTreeSet::new();
    for path in entries.keys() {
        for ancestor in path.ancestors().skip(1) {
            if checked_directories.contains(ancestor) {
                break;
            }
            ensure!(
                !entries
                    .get(ancestor)
                    .is_some_and(|entry| matches!(entry.kind, FileKind::File { .. })),
                "File has a descendant: {}",
                ancestor.display()
            );
            checked_directories.insert(ancestor.to_owned());
        }
    }
    let mut provider_names = BTreeMap::new();
    let mut resource_provider_names = BTreeMap::new();
    let mut root_encounters = BTreeMap::new();
    if let Some(facts) = facts {
        ensure!(
            facts.presence.model_revision == model.revision
                && facts.presence.file_revision == files.revision,
            "Provider presence belongs to obsolete model or file facts"
        );
        let mut presence = facts.presence.clone();
        for entry in entries.values() {
            presence.add_entry(
                &entry.path,
                match entry.kind {
                    FileKind::Directory => RootPresence::Directory,
                    FileKind::File { .. } => RootPresence::File,
                },
            )?;
            // A real observed child also proves its ancestor directories exist.
            for ancestor in entry.path.ancestors().skip(1) {
                if presence.get(ancestor) == RootPresence::Directory {
                    break;
                }
                presence.add_entry(ancestor, RootPresence::Directory)?;
            }
        }
        presence.validate()?;
        for module_facts in &facts.modules {
            let binding = module_facts.providers.binding();
            let encounters = &module_facts.roots;
            ensure!(
                binding.model_revision == model.revision
                    && encounters.model_revision == model.revision,
                "Provider/root encounters belong to an obsolete model"
            );
            ensure!(
                binding.module == encounters.module,
                "Provider and root modules disagree"
            );
            ensure!(
                binding.variant == encounters.variant,
                "Provider and root variants disagree"
            );
            ensure!(
                encounters.provenance == RootEncounterProvenance::ProducerIterator,
                "Source-root encounter order is unavailable"
            );
            let module = model
                .modules
                .iter()
                .find(|module| module.id == binding.module)
                .context("Provider module is absent from the tree model")?;
            for root in &module.source_roots {
                match presence.get(&root.path) {
                    RootPresence::Unknown => {
                        return Err(crate::project_tree_facts::unknown_presence(&root.path).into());
                    }
                    RootPresence::Missing => {}
                    RootPresence::Directory | RootPresence::File => {
                        ensure!(
                            entries.contains_key(&root.path),
                            "A present source root is absent from the file inventory: {}",
                            root.path.display()
                        );
                    }
                }
                if matches!(
                    root.group,
                    SourceGroup::Resources | SourceGroup::GeneratedResources
                ) && presence.get(&root.path) != RootPresence::Missing
                {
                    let resolution = module_facts
                        .providers
                        .resolve_resource_root(&root.path, &presence)?;
                    resource_provider_names.insert(
                        (module.id.as_str(), root.path.clone()),
                        resolution.winner().map(|candidate| candidate.name.clone()),
                    );
                }
            }
            ensure!(
                root_encounters
                    .insert(module.id.as_str(), &encounters.roots)
                    .is_none(),
                "Duplicate provider projection module"
            );
            let mut expected = module
                .source_roots
                .iter()
                .map(|root| RootOccurrence {
                    group: root.group,
                    path: root.path.clone(),
                })
                .collect::<Vec<_>>();
            let mut actual = encounters.roots.clone();
            expected.sort();
            actual.sort();
            ensure!(
                expected == actual,
                "Source-root encounters do not cover the evaluated roots"
            );
            let mut annotated_paths = BTreeSet::new();
            for root in &module.source_roots {
                let mut completed_ancestors = BTreeSet::new();
                for (path, _) in entries
                    .range(root.path.clone()..)
                    .take_while(|(path, _)| path.starts_with(&root.path))
                {
                    annotated_paths.insert(path.clone());
                    for ancestor in path
                        .ancestors()
                        .skip(1)
                        .take_while(|ancestor| ancestor.starts_with(&root.path))
                    {
                        if completed_ancestors.contains(ancestor) {
                            break;
                        }
                        annotated_paths.insert(ancestor.into());
                        completed_ancestors.insert(ancestor.to_owned());
                    }
                }
            }
            for path in annotated_paths {
                let resolution = module_facts.providers.resolve(&path, &presence)?;
                provider_names.insert(
                    (module.id.as_str(), path),
                    resolution.winner().map(|candidate| candidate.name.clone()),
                );
            }
        }
        ensure!(
            root_encounters.len() == model.modules.len(),
            "A tree module has no evaluated provider facts"
        );
    }
    let provider_names = facts.map(|_| &provider_names);
    let resource_provider_names = facts.map(|_| &resource_provider_names);
    let mut modules = model.modules.iter().collect::<Vec<_>>();
    modules.sort_by(|left, right| left.id.encode_utf16().cmp(right.id.encode_utf16()));
    let mut module_ids = BTreeSet::new();
    let mut tree = TreeSnapshot {
        model_revision: model.revision,
        file_revision: files.revision,
        roots: Vec::new(),
        nodes: Vec::new(),
    };
    for module in modules {
        ensure!(
            !module.id.is_empty()
                && (label_policy == ModuleLabelPolicy::Imported || !module.display_name.is_empty()),
            "Android module identity is empty"
        );
        ensure!(
            module_ids.insert(&module.id),
            "Duplicate Android module {}",
            module.id
        );
        validate_path(&module.directory)?;
        ensure!(
            entries
                .get(&module.directory)
                .is_none_or(|entry| matches!(entry.kind, FileKind::Directory)),
            "Android module directory is a file: {}",
            module.directory.display()
        );
        let module_node = tree.add(
            None,
            TreeNode {
                key: NodeKey::Module {
                    module: module.id.clone(),
                },
                label: module.display_name.clone(),
                annotation: None,
                reference_label: format!("{} (Android)", module.display_name),
                navigation: None,
                children: Vec::new(),
            },
        );
        let mut roots = module.source_roots.iter().collect::<Vec<_>>();
        if let Some(encounters) = root_encounters.get(module.id.as_str()) {
            let mut by_occurrence = BTreeMap::new();
            for root in &roots {
                by_occurrence
                    .entry(RootOccurrence {
                        group: root.group,
                        path: root.path.clone(),
                    })
                    .or_insert(*root);
            }
            let mut ordered = Vec::new();
            for occurrence in *encounters {
                let root = by_occurrence
                    .get(occurrence)
                    .context("Source-root encounter is absent from evaluated roots")?;
                ordered.push(*root);
            }
            roots = ordered;
            roots.sort_by_key(|root| root.group);
        } else {
            roots.sort_by(|left, right| {
                left.group
                    .cmp(&right.group)
                    .then_with(|| {
                        provider_sort_key(&left.provider)
                            .encode_utf16()
                            .cmp(provider_sort_key(&right.provider).encode_utf16())
                    })
                    .then_with(|| left.path.cmp(&right.path))
            });
        }
        let mut root_paths: BTreeMap<&Path, &TreeSourceRoot> = BTreeMap::new();
        let mut grouped = BTreeMap::<SourceGroup, Vec<&TreeSourceRoot>>::new();
        for root in roots {
            validate_path(&root.path)?;
            if provider_names.is_none() {
                root.provider
                    .name()
                    .with_context(|| format!("Source root {}", root.path.display()))?;
            }
            if let Some(previous) = root_paths.get(root.path.as_path()) {
                ensure!(
                    provider_names.is_some() || previous.provider == root.provider,
                    "Conflicting source providers for {}",
                    root.path.display()
                );
                continue;
            }
            root_paths.insert(&root.path, root);
            if let Some(entry) = entries.get(&root.path) {
                ensure!(
                    matches!(entry.kind, FileKind::File { .. })
                        == (root.group == SourceGroup::Manifests),
                    "Source root has the wrong file kind: {}",
                    root.path.display()
                );
                grouped.entry(root.group).or_default().push(root);
            }
        }
        for (group, roots) in grouped {
            let generated = group.is_generated().then(|| " (generated)".to_owned());
            let group_node = tree.add(
                Some(module_node),
                TreeNode {
                    key: NodeKey::Source {
                        module: module.id.clone(),
                        group,
                    },
                    label: group.label().into(),
                    reference_label: format!(
                        "{}{}",
                        group.label(),
                        generated.as_deref().unwrap_or_default()
                    ),
                    annotation: generated,
                    navigation: None,
                    children: Vec::new(),
                },
            );
            match group {
                SourceGroup::Resources | SourceGroup::GeneratedResources => resource_nodes(
                    &mut tree,
                    group_node,
                    module,
                    group,
                    &roots,
                    &entries,
                    provider_names,
                    resource_provider_names,
                )?,
                SourceGroup::Manifests => {
                    for root in roots {
                        if let Some(entry) = entries.get(&root.path) {
                            file_nodes(
                                &mut tree,
                                group_node,
                                module,
                                root,
                                entry,
                                false,
                                provider_names,
                            )?;
                        }
                    }
                }
                _ => {
                    for root in roots {
                        directory_nodes(
                            &mut tree,
                            group_node,
                            module,
                            root,
                            &entries,
                            provider_names,
                        )?;
                    }
                }
            }
        }
        let google_services = module.directory.join("google-services.json");
        if let Some(entry) = entries.get(&google_services)
            && matches!(entry.kind, FileKind::File { .. })
        {
            let name = file_name(&google_services)?;
            tree.add(
                Some(module_node),
                TreeNode {
                    key: NodeKey::File {
                        module: module.id.clone(),
                        group: None,
                        source_root: None,
                        path: google_services.clone(),
                    },
                    label: name.clone(),
                    annotation: None,
                    reference_label: name,
                    navigation: Some(NavigationTarget {
                        path: google_services,
                        byte_offset: None,
                    }),
                    children: Vec::new(),
                },
            );
        }
    }
    sort_children(&mut tree);
    Ok(tree)
}

fn provider_sort_key(provider: &SourceProvider) -> &str {
    match provider {
        SourceProvider::Named(name) if name != "main" => name,
        _ => "",
    }
}

fn validate_path(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute(),
        "Tree path is not absolute: {}",
        path.display()
    );
    ensure!(
        !path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
            && path.components().collect::<PathBuf>().as_os_str() == path.as_os_str(),
        "Tree path is not normalized: {}",
        path.display()
    );
    Ok(())
}

fn file_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .with_context(|| format!("Tree name is not UTF-8: {}", path.display()))
}

fn annotated(name: &str, provider: Option<&str>, include_main: bool) -> String {
    match provider {
        Some(provider) if include_main || provider != "main" => format!("{name} ({provider})"),
        _ => name.into(),
    }
}

type EntryProviders<'a> = BTreeMap<(&'a str, PathBuf), Option<String>>;

fn entry_provider<'a>(
    root: &'a TreeSourceRoot,
    module: &'a TreeModule,
    path: &Path,
    names: Option<&'a EntryProviders<'_>>,
) -> Result<Option<&'a str>> {
    match names {
        Some(names) => names
            .get(&(module.id.as_str(), path.into()))
            .map(|provider| provider.as_deref())
            .context("Entry provider fact is unavailable"),
        None => root.provider.name(),
    }
}

fn file_nodes(
    tree: &mut TreeSnapshot,
    parent: NodeId,
    module: &TreeModule,
    root: &TreeSourceRoot,
    entry: &FileFact,
    classes: bool,
    provider_names: Option<&EntryProviders<'_>>,
) -> Result<()> {
    let group = root.group;
    let provider = entry_provider(root, module, &entry.path, provider_names)?;
    let FileKind::File { java_classes } = &entry.kind else {
        return Ok(());
    };
    if classes && !java_classes.is_empty() {
        let mut names = BTreeSet::new();
        let mut java_classes = java_classes.iter().collect::<Vec<_>>();
        java_classes.sort_by(|left, right| left.name.encode_utf16().cmp(right.name.encode_utf16()));
        for class in java_classes {
            ensure!(
                !class.name.is_empty() && !class.name.contains(['/', '\\', '\n', '\r']),
                "Invalid Java class presentation fact"
            );
            ensure!(
                names.insert(&class.name),
                "Duplicate Java class fact in {}",
                entry.path.display()
            );
            tree.add(
                Some(parent),
                TreeNode {
                    key: NodeKey::JavaClass {
                        module: module.id.clone(),
                        group,
                        source_root: root.path.clone(),
                        path: entry.path.clone(),
                        name: class.name.clone(),
                    },
                    label: class.name.clone(),
                    annotation: None,
                    reference_label: class.name.clone(),
                    navigation: Some(NavigationTarget {
                        path: entry.path.clone(),
                        byte_offset: class.byte_offset,
                    }),
                    children: Vec::new(),
                },
            );
        }
    } else {
        let name = file_name(&entry.path)?;
        tree.add(
            Some(parent),
            TreeNode {
                key: NodeKey::File {
                    module: module.id.clone(),
                    group: Some(group),
                    source_root: Some(root.path.clone()),
                    path: entry.path.clone(),
                },
                label: name.clone(),
                annotation: provider
                    .filter(|name| *name != "main")
                    .map(|name| format!(" ({name})")),
                reference_label: annotated(&name, provider, true),
                navigation: Some(NavigationTarget {
                    path: entry.path.clone(),
                    byte_offset: None,
                }),
                children: Vec::new(),
            },
        );
    }
    Ok(())
}

#[derive(Default)]
struct DirectoryContents<'a> {
    directories: BTreeSet<PathBuf>,
    files: Vec<&'a FileFact>,
}

fn directory_nodes(
    tree: &mut TreeSnapshot,
    parent: NodeId,
    module: &TreeModule,
    root: &TreeSourceRoot,
    entries: &BTreeMap<PathBuf, &FileFact>,
    provider_names: Option<&EntryProviders<'_>>,
) -> Result<()> {
    let mut directories = BTreeMap::<PathBuf, DirectoryContents<'_>>::new();
    directories.insert(root.path.clone(), DirectoryContents::default());
    for (path, entry) in entries
        .range(root.path.clone()..)
        .take_while(|(path, _)| path.starts_with(&root.path))
    {
        if path == &root.path {
            continue;
        }
        let directory = match entry.kind {
            FileKind::Directory => path.as_path(),
            FileKind::File { .. } => path.parent().context("File has no parent")?,
        };
        let mut ancestors = Vec::new();
        let mut cursor = directory;
        while !directories.contains_key(cursor) {
            ancestors.push(cursor.to_owned());
            cursor = cursor.parent().context("Source file is outside its root")?;
        }
        for path in ancestors.into_iter().rev() {
            let parent = path.parent().context("Directory has no parent")?.to_owned();
            directories
                .entry(parent)
                .or_default()
                .directories
                .insert(path.clone());
            directories.entry(path).or_default();
        }
        if matches!(entry.kind, FileKind::File { .. }) {
            directories
                .entry(directory.to_owned())
                .or_default()
                .files
                .push(entry);
        }
    }
    let compact = module.compact_packages && root.group.is_package_group();
    let emitted = directories
        .iter()
        .filter_map(|(path, contents)| {
            (path == &root.path
                || !compact
                || !contents.files.is_empty()
                || contents.directories.len() != 1)
                .then_some(path.clone())
        })
        .collect::<BTreeSet<_>>();
    let mut parents = BTreeMap::from([(root.path.clone(), parent)]);
    for path in &emitted {
        if path == &root.path {
            continue;
        }
        let mut ancestor = path.parent().context("Directory has no parent")?;
        while !emitted.contains(ancestor) {
            ancestor = ancestor
                .parent()
                .context("Compacted directory has no parent")?;
        }
        let parent = *parents
            .get(ancestor)
            .context("Compacted parent has not been projected")?;
        let label = path
            .strip_prefix(ancestor)?
            .components()
            .map(|part| {
                part.as_os_str()
                    .to_str()
                    .context("Package name is not UTF-8")
            })
            .collect::<Result<Vec<_>>>()?
            .join(if root.group.is_package_group() {
                "."
            } else {
                "/"
            });
        let provider = entry_provider(root, module, path, provider_names)?;
        let id = tree.add(
            Some(parent),
            TreeNode {
                key: NodeKey::Directory {
                    module: module.id.clone(),
                    group: root.group,
                    source_root: root.path.clone(),
                    path: path.clone(),
                },
                label,
                annotation: provider
                    .filter(|name| *name != "main")
                    .map(|name| format!(" ({name})")),
                reference_label: annotated(&file_name(path)?, provider, true),
                navigation: None,
                children: Vec::new(),
            },
        );
        parents.insert(path.clone(), id);
    }
    for (path, contents) in directories {
        if let Some(parent) = parents.get(&path) {
            for entry in contents.files {
                file_nodes(
                    tree,
                    *parent,
                    module,
                    root,
                    entry,
                    root.group.is_package_group(),
                    provider_names,
                )?;
            }
        }
    }
    Ok(())
}

type ResourceFiles<'a> = BTreeMap<PathBuf, (Option<&'a str>, String)>;
type ResourceGroups<'a> = BTreeMap<String, BTreeMap<String, ResourceFiles<'a>>>;

fn resource_nodes(
    tree: &mut TreeSnapshot,
    parent: NodeId,
    module: &TreeModule,
    group: SourceGroup,
    roots: &[&TreeSourceRoot],
    entries: &BTreeMap<PathBuf, &FileFact>,
    provider_names: Option<&EntryProviders<'_>>,
    resource_provider_names: Option<&EntryProviders<'_>>,
) -> Result<()> {
    let mut resources = ResourceGroups::new();
    for root in roots {
        for (path, entry) in entries
            .range(root.path.clone()..)
            .take_while(|(path, _)| path.starts_with(&root.path))
        {
            let relative = path.strip_prefix(&root.path)?;
            let mut parts = relative.components();
            let Some(folder) = parts.next().and_then(|part| part.as_os_str().to_str()) else {
                continue;
            };
            let (folder_type, qualifier) = folder.split_once('-').unwrap_or((folder, ""));
            if !is_resource_type(folder_type) {
                continue;
            }
            let Some(name) = parts.next().and_then(|part| part.as_os_str().to_str()) else {
                if matches!(entry.kind, FileKind::Directory) {
                    resources.entry(folder_type.into()).or_default();
                }
                continue;
            };
            if parts.next().is_some() || !matches!(entry.kind, FileKind::File { .. }) {
                continue;
            }
            let stem = Path::new(name)
                .file_stem()
                .and_then(|name| name.to_str())
                .context("Resource name is not UTF-8")?;
            resources
                .entry(folder_type.into())
                .or_default()
                .entry(stem.into())
                .or_default()
                .insert(
                    path.clone(),
                    (
                        entry_provider(root, module, &root.path, resource_provider_names)?,
                        qualifier.into(),
                    ),
                );
        }
    }
    for (folder_type, names) in resources {
        let folder_node = tree.add(
            Some(parent),
            TreeNode {
                key: NodeKey::ResourceType {
                    module: module.id.clone(),
                    group,
                    folder_type: folder_type.clone(),
                },
                label: folder_type.clone(),
                annotation: None,
                reference_label: folder_type.clone(),
                navigation: None,
                children: Vec::new(),
            },
        );
        for (name, files) in names {
            let parent = if files.len() > 1 {
                tree.add(
                    Some(folder_node),
                    TreeNode {
                        key: NodeKey::ResourceGroup {
                            module: module.id.clone(),
                            group,
                            folder_type: folder_type.clone(),
                            name: name.clone(),
                        },
                        label: name.clone(),
                        annotation: Some(format!(" ({})", files.len())),
                        reference_label: format!("{name} ({})", files.len()),
                        // Selecting a best qualified resource needs FolderConfiguration
                        // semantics from later resource tooling; retain individual targets.
                        navigation: None,
                        children: Vec::new(),
                    },
                )
            } else {
                folder_node
            };
            for (path, (provider, qualifier)) in files {
                let name = file_name(&path)?;
                let qualifiers = [
                    (!qualifier.is_empty()).then_some(qualifier.as_str()),
                    provider.filter(|provider| *provider != "main"),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
                let annotation =
                    (!qualifiers.is_empty()).then(|| format!(" ({})", qualifiers.join(", ")));
                tree.add(
                    Some(parent),
                    TreeNode {
                        key: NodeKey::File {
                            module: module.id.clone(),
                            group: Some(group),
                            source_root: None,
                            path: path.clone(),
                        },
                        label: name.clone(),
                        reference_label: format!(
                            "{name}{}",
                            annotation.as_deref().unwrap_or_default()
                        ),
                        annotation,
                        navigation: Some(NavigationTarget {
                            path,
                            byte_offset: None,
                        }),
                        children: Vec::new(),
                    },
                );
            }
        }
    }
    // AndroidResFolderNode checks the first physical root whose parent is
    // named main, then annotates through the actual source-provider metadata.
    if let Some(root) = roots.iter().find(|root| {
        root.path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "main")
    }) {
        let properties = root.path.join("resources.properties");
        if let Some(entry) = entries.get(&properties) {
            file_nodes(tree, parent, module, root, entry, false, provider_names)?;
        }
    }
    Ok(())
}

fn is_resource_type(name: &str) -> bool {
    matches!(
        name,
        "anim"
            | "animator"
            | "color"
            | "drawable"
            | "font"
            | "interpolator"
            | "layout"
            | "menu"
            | "mipmap"
            | "navigation"
            | "raw"
            | "transition"
            | "values"
            | "xml"
    )
}

fn sort_children(tree: &mut TreeSnapshot) {
    for index in 0..tree.nodes.len() {
        let mut children = std::mem::take(&mut tree.nodes[index].children);
        children.sort_by(|left, right| {
            let left = &tree.nodes[left.0];
            let right = &tree.nodes[right.0];
            let weight = |node: &TreeNode| match &node.key {
                NodeKey::Source { group, .. } => (0, Some(*group)),
                NodeKey::Directory { .. } | NodeKey::ResourceType { .. } => (1, None),
                _ => (2, None),
            };
            weight(left)
                .cmp(&weight(right))
                .then_with(|| left.label.encode_utf16().cmp(right.label.encode_utf16()))
                .then_with(|| {
                    left.reference_label
                        .encode_utf16()
                        .cmp(right.reference_label.encode_utf16())
                })
                .then_with(|| left.key.cmp(&right.key))
        });
        tree.nodes[index].children = children;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_label_permission_never_allows_empty_stable_module_ids() -> Result<()> {
        let mut model = TreeModel {
            revision: 7,
            modules: vec![TreeModule {
                id: ":app".into(),
                display_name: String::new(),
                directory: std::env::temp_dir().components().collect(),
                source_roots: Vec::new(),
                compact_packages: true,
            }],
        };
        let files = TreeFiles {
            model_revision: 7,
            revision: 11,
            entries: Vec::new(),
        };
        let imported = project_tree_internal(&model, &files, None, ModuleLabelPolicy::Imported)?;
        let row = imported.nodes().next().context("Imported module row")?;
        assert_eq!(row.label, "");
        assert_eq!(
            row.key,
            NodeKey::Module {
                module: ":app".into()
            }
        );
        model
            .modules
            .first_mut()
            .context("Imported module")?
            .id
            .clear();
        for label_policy in [ModuleLabelPolicy::NonEmpty, ModuleLabelPolicy::Imported] {
            for display_name in ["", "app"] {
                model
                    .modules
                    .first_mut()
                    .context("Imported module")?
                    .display_name = display_name.into();
                assert_eq!(
                    project_tree_internal(&model, &files, None, label_policy)
                        .expect_err("An imported label permission never replaces stable identity")
                        .to_string(),
                    "Android module identity is empty"
                );
            }
        }
        Ok(())
    }
}
