/*
 * Copyright (C) 2019 The Android Open Source Project
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

//! Supplemental projection tests. These do not replace the five upstream
//! Gradle-backed cases or their model/generated-folder assertions.

use android_tools::project_tree::{
    FileFact, FileKind, JavaClassFact, NodeKey, SourceGroup, SourceProvider, TreeFiles, TreeModel,
    TreeModule, TreeSnapshot, TreeSourceRoot, project_tree,
};
use anyhow::{Context as _, Result};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

fn platform_path(path: impl Into<PathBuf>) -> PathBuf {
    path.into().components().collect()
}

fn directory(path: impl Into<PathBuf>) -> FileFact {
    FileFact {
        path: platform_path(path),
        kind: FileKind::Directory,
    }
}

fn raw_file(path: impl Into<PathBuf>) -> FileFact {
    FileFact {
        path: path.into(),
        kind: FileKind::File {
            java_classes: Vec::new(),
        },
    }
}

fn canonical_file(path: impl Into<PathBuf>) -> FileFact {
    raw_file(platform_path(path))
}

fn root(path: impl Into<PathBuf>, group: SourceGroup, provider: &str) -> TreeSourceRoot {
    TreeSourceRoot {
        path: platform_path(path),
        group,
        provider: SourceProvider::Named(provider.into()),
    }
}

fn input(roots: Vec<TreeSourceRoot>, entries: Vec<FileFact>) -> (TreeModel, TreeFiles) {
    let directory = platform_path(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/project_tree/simpleApplication/app"),
    );
    (
        TreeModel {
            revision: 7,
            modules: vec![TreeModule {
                id: ":app".into(),
                display_name: "app".into(),
                directory,
                source_roots: roots,
                compact_packages: true,
            }],
        },
        TreeFiles {
            model_revision: 7,
            revision: 11,
            entries,
        },
    )
}

fn leaf_paths(tree: &TreeSnapshot) -> Vec<Vec<String>> {
    let mut pending = tree
        .roots
        .iter()
        .map(|id| (*id, Vec::new()))
        .collect::<Vec<_>>();
    let mut paths = Vec::new();
    while let Some((id, mut path)) = pending.pop() {
        let node = tree
            .node(id)
            .expect("Projection contains only valid node IDs");
        path.push(node.reference_label.clone());
        if node.children.is_empty() {
            paths.push(path);
        } else {
            pending.extend(node.children.iter().map(|id| (*id, path.clone())));
        }
    }
    paths.sort();
    paths
}

fn fixture() -> Result<(TreeModel, TreeFiles)> {
    let base = platform_path(Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/project_tree"));
    let provenance: serde_json::Value =
        serde_json::from_slice(&fs::read(base.join("provenance.json"))?)?;
    let mut entries = BTreeMap::new();
    for record in provenance["fixtures"]
        .as_array()
        .context("Fixture inventory is absent")?
    {
        let path = platform_path(
            base.join(
                record["local_path"]
                    .as_str()
                    .context("Fixture path is absent")?,
            ),
        );
        let bytes = fs::read(&path)?;
        assert_eq!(bytes.len() as u64, record["bytes"].as_u64().unwrap());
        let mut entry = canonical_file(&path);
        // Explicit presentation fact from this fixture, not a filename-based Java parser.
        if path
            .file_name()
            .is_some_and(|name| name == "MyActivity.java")
        {
            let source = std::str::from_utf8(&bytes)?;
            entry.kind = FileKind::File {
                java_classes: vec![JavaClassFact {
                    name: "MyActivity".into(),
                    byte_offset: source.find("public class MyActivity"),
                }],
            };
        }
        entries.insert(path.clone(), entry);
        for ancestor in path
            .parent()
            .context("Fixture parent is absent")?
            .ancestors()
        {
            if !ancestor.starts_with(base.join("simpleApplication")) {
                break;
            }
            entries
                .entry(ancestor.to_owned())
                .or_insert_with(|| directory(ancestor));
        }
    }
    let app = base.join("simpleApplication/app");
    Ok(input(
        vec![
            root(
                app.join("src/main/AndroidManifest.xml"),
                SourceGroup::Manifests,
                "main",
            ),
            root(app.join("src/main/java"), SourceGroup::Java, "main"),
            root(
                app.join("src/androidTest/java"),
                SourceGroup::Java,
                "androidTest",
            ),
            root(app.join("src/test/java"), SourceGroup::Java, "test"),
            root(app.join("src/main/res"), SourceGroup::Resources, "main"),
        ],
        entries.into_values().collect(),
    ))
}

#[test]
fn component_fixture_package_manifest_and_class_navigation() -> Result<()> {
    let (model, files) = fixture()?;
    let tree = project_tree(&model, &files)?;
    let paths = leaf_paths(&tree);
    assert!(paths.contains(&vec![
        "app (Android)".into(),
        "manifests".into(),
        "AndroidManifest.xml (main)".into()
    ]));
    assert!(paths.contains(&vec![
        "app (Android)".into(),
        "java".into(),
        "simpleapplication (main)".into(),
        "MyActivity".into()
    ]));
    let package = tree
        .nodes()
        .find(|node| node.label == "google.simpleapplication" && node.annotation.is_none())
        .unwrap();
    assert_eq!(package.reference_label, "simpleapplication (main)");
    let class = tree
        .nodes()
        .find(|node| node.label == "MyActivity")
        .unwrap();
    let target = class.navigation.as_ref().unwrap();
    let source = fs::read_to_string(&target.path)?;
    assert_eq!(target.byte_offset, source.find("public class MyActivity"));
    assert!(source[target.byte_offset.unwrap()..].starts_with("public class MyActivity"));
    let module = tree.node(tree.roots[0]).unwrap();
    assert_eq!(module.label, "app");
    assert_eq!(module.annotation, None);
    Ok(())
}

#[test]
fn component_fixture_resource_variants_preserve_every_file_target() -> Result<()> {
    let (model, files) = fixture()?;
    let tree = project_tree(&model, &files)?;
    let strings = tree
        .nodes()
        .find(|node| matches!(&node.key, NodeKey::ResourceGroup { name, .. } if name == "strings"))
        .unwrap();
    assert_eq!(strings.reference_label, "strings (5)");
    assert_eq!(strings.navigation, None);
    let variants = strings
        .children
        .iter()
        .map(|id| tree.node(*id).unwrap().reference_label.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        variants,
        BTreeSet::from([
            "strings.xml",
            "strings.xml (en)",
            "strings.xml (en-rGB)",
            "strings.xml (ta)",
            "strings.xml (zh-rCN)"
        ])
    );
    // There are five original strings files; count must reflect physical files.
    assert_eq!(strings.children.len(), 5);
    let expected = files
        .entries
        .iter()
        .filter(|entry| {
            entry
                .path
                .starts_with(model.modules[0].directory.join("src/main/res"))
                && matches!(entry.kind, FileKind::File { .. })
        })
        .map(|entry| entry.path.clone())
        .collect::<BTreeSet<_>>();
    let actual = tree
        .nodes()
        .filter_map(|node| node.navigation.as_ref())
        .filter(|target| {
            target
                .path
                .starts_with(model.modules[0].directory.join("src/main/res"))
        })
        .map(|target| target.path.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn component_generated_groups_keep_explicit_facts_and_literal_assets() -> Result<()> {
    let (mut model, mut files) = fixture()?;
    let generated = platform_path(model.modules[0].directory.join("build/generated"));
    let java = generated.join("buildConfig");
    let assets = generated.join("createAssets");
    let res = generated.join("my_generated_resources");
    model.modules[0].source_roots.extend([
        TreeSourceRoot {
            path: java.clone(),
            group: SourceGroup::GeneratedJava,
            provider: SourceProvider::Unattributed,
        },
        TreeSourceRoot {
            path: assets.clone(),
            group: SourceGroup::GeneratedAssets,
            provider: SourceProvider::Unattributed,
        },
        TreeSourceRoot {
            path: res.clone(),
            group: SourceGroup::GeneratedResources,
            provider: SourceProvider::Unattributed,
        },
    ]);
    files.entries.extend([
        directory(&java),
        directory(java.join("com/application")),
        FileFact {
            path: platform_path(java.join("com/application/BuildConfig.java")),
            kind: FileKind::File {
                java_classes: vec![JavaClassFact {
                    name: "BuildConfig".into(),
                    byte_offset: Some(25),
                }],
            },
        },
        directory(&assets),
        canonical_file(assets.join("raw/createAssets")),
        directory(&res),
        canonical_file(res.join("raw/sample_raw_resource")),
    ]);
    let tree = project_tree(&model, &files)?;
    let paths = leaf_paths(&tree);
    for expected in [
        [
            "app (Android)",
            "java (generated)",
            "application",
            "BuildConfig",
        ],
        ["app (Android)", "assets (generated)", "raw", "createAssets"],
        [
            "app (Android)",
            "res (generated)",
            "raw",
            "sample_raw_resource",
        ],
    ] {
        assert!(paths.contains(&expected.into_iter().map(str::to_owned).collect()));
    }
    Ok(())
}

#[test]
fn component_properties_main_annotation_is_separate_from_visual_text() -> Result<()> {
    let (model, mut files) = fixture()?;
    let path = model.modules[0]
        .directory
        .join("src/main/res/resources.properties");
    files.entries.push(canonical_file(&path));
    let tree = project_tree(&model, &files)?;
    let node = tree
        .nodes()
        .find(|node| {
            node.navigation
                .as_ref()
                .is_some_and(|target| target.path == path)
        })
        .unwrap();
    assert_eq!(node.label, "resources.properties");
    assert_eq!(node.annotation, None);
    assert_eq!(node.reference_label, "resources.properties (main)");
    Ok(())
}

#[test]
fn component_properties_use_first_physical_main_root_without_fallback() -> Result<()> {
    let (mut model, mut files) = fixture()?;
    let app = model.modules[0].directory.clone();
    let first = app.join("a/main/res");
    let second = app.join("z/main/res");
    model.modules[0].source_roots = vec![
        root(&first, SourceGroup::Resources, "main"),
        root(&second, SourceGroup::Resources, "main"),
    ];
    files.entries.extend([
        directory(&first),
        directory(&second),
        canonical_file(second.join("resources.properties")),
    ]);
    let tree = project_tree(&model, &files)?;
    assert!(
        !tree
            .nodes()
            .any(|node| node.label == "resources.properties")
    );
    files
        .entries
        .push(canonical_file(first.join("resources.properties")));
    let tree = project_tree(&model, &files)?;
    let properties = tree
        .nodes()
        .filter(|node| node.label == "resources.properties")
        .collect::<Vec<_>>();
    assert_eq!(properties.len(), 1);
    assert_eq!(
        properties[0].navigation.as_ref().unwrap().path,
        first.join("resources.properties")
    );
    Ok(())
}

#[test]
fn component_properties_do_not_infer_physical_main_from_provider_name() -> Result<()> {
    let (mut model, mut files) = fixture()?;
    let custom = model.modules[0].directory.join("customResources");
    model.modules[0].source_roots = vec![root(&custom, SourceGroup::Resources, "main")];
    files.entries.extend([
        directory(&custom),
        canonical_file(custom.join("resources.properties")),
    ]);
    assert!(
        !project_tree(&model, &files)?
            .nodes()
            .any(|node| node.label == "resources.properties")
    );
    model.modules[0].source_roots[0] = root(
        model.modules[0].directory.join("custom/main/res"),
        SourceGroup::Resources,
        "debug",
    );
    let custom = model.modules[0].source_roots[0].path.clone();
    files.entries.extend([
        directory(&custom),
        canonical_file(custom.join("resources.properties")),
    ]);
    let tree = project_tree(&model, &files)?;
    assert_eq!(
        tree.nodes()
            .find(|node| node.label == "resources.properties")
            .unwrap()
            .reference_label,
        "resources.properties (debug)"
    );
    Ok(())
}

#[test]
fn component_google_services_requires_a_direct_module_file() -> Result<()> {
    let (model, mut files) = fixture()?;
    let app = &model.modules[0].directory;
    files.entries.extend([
        directory(app.join("google-services.json")),
        canonical_file(app.join("nested/google-services.json")),
    ]);
    assert!(
        !project_tree(&model, &files)?
            .nodes()
            .any(|node| node.label == "google-services.json")
    );
    files
        .entries
        .retain(|entry| entry.path != app.join("google-services.json"));
    files
        .entries
        .push(canonical_file(app.join("google-services.json")));
    let tree = project_tree(&model, &files)?;
    let google = tree
        .nodes()
        .find(|node| node.label == "google-services.json")
        .unwrap();
    assert_eq!(
        google.navigation.as_ref().unwrap().path,
        app.join("google-services.json")
    );
    assert!(
        tree.node(tree.roots[0])
            .unwrap()
            .children
            .iter()
            .any(|id| tree.node(*id) == Some(google))
    );
    Ok(())
}

#[test]
fn component_input_reordering_preserves_snapshot_and_stable_keys() -> Result<()> {
    let (mut model, mut files) = fixture()?;
    let expected = project_tree(&model, &files)?;
    model.modules[0].source_roots.reverse();
    files.entries.reverse();
    assert_eq!(project_tree(&model, &files)?, expected);
    let keys = expected
        .nodes()
        .map(|node| &node.key)
        .collect::<BTreeSet<_>>();
    assert_eq!(keys.len(), expected.nodes().count());
    Ok(())
}

#[test]
fn component_overlapping_roots_have_distinct_occurrence_keys() -> Result<()> {
    let (mut model, mut files) = fixture()?;
    let outer = model.modules[0].directory.join("overlap");
    let inner = outer.join("nested");
    model.modules[0].source_roots = vec![
        root(&outer, SourceGroup::Assets, "main"),
        root(&inner, SourceGroup::Assets, "main"),
    ];
    files.entries.extend([
        directory(&outer),
        directory(&inner),
        canonical_file(inner.join("one.txt")),
    ]);
    let tree = project_tree(&model, &files)?;
    let occurrences = tree
        .nodes()
        .filter(|node| node.label == "one.txt")
        .collect::<Vec<_>>();
    assert_eq!(occurrences.len(), 2);
    assert_ne!(occurrences[0].key, occurrences[1].key);
    assert_eq!(occurrences[0].navigation, occurrences[1].navigation);
    assert_eq!(
        tree.nodes()
            .map(|node| &node.key)
            .collect::<BTreeSet<_>>()
            .len(),
        tree.nodes().count()
    );
    Ok(())
}

#[test]
fn component_identical_roots_follow_builtin_type_priority() -> Result<()> {
    let (mut model, mut files) = fixture()?;
    let shared = model.modules[0].directory.join("shared");
    model.modules[0].source_roots = vec![
        root(&shared, SourceGroup::GeneratedJava, "main"),
        root(&shared, SourceGroup::Java, "main"),
        root(&shared, SourceGroup::Java, "main"),
    ];
    files.entries.extend([
        directory(&shared),
        canonical_file(shared.join("Readme.txt")),
    ]);
    let tree = project_tree(&model, &files)?;
    assert_eq!(
        tree.nodes()
            .filter(|node| matches!(node.key, NodeKey::Source { .. }))
            .map(|node| node.reference_label.as_str())
            .collect::<Vec<_>>(),
        vec!["java"]
    );
    Ok(())
}

#[test]
fn component_empty_and_missing_roots_are_distinct() -> Result<()> {
    let (mut model, mut files) = fixture()?;
    let existing = model.modules[0].directory.join("empty");
    let missing = model.modules[0].directory.join("missing");
    model.modules[0].source_roots = vec![
        root(&existing, SourceGroup::Assets, "main"),
        root(&missing, SourceGroup::Kotlin, "main"),
    ];
    files.entries.push(directory(&existing));
    let tree = project_tree(&model, &files)?;
    assert_eq!(leaf_paths(&tree), vec![vec!["app (Android)", "assets"]]);
    Ok(())
}

#[test]
fn component_empty_resource_types_survive_and_unknown_folders_are_excluded() -> Result<()> {
    let (mut model, mut files) = fixture()?;
    let res = model.modules[0].directory.join("res");
    model.modules[0].source_roots = vec![root(&res, SourceGroup::Resources, "main")];
    files.entries.extend([
        directory(&res),
        directory(res.join("raw")),
        canonical_file(res.join("unknown/file.xml")),
        canonical_file(res.join("layout/nested/file.xml")),
    ]);
    let tree = project_tree(&model, &files)?;
    assert!(tree.nodes().any(|node| node.reference_label == "raw"));
    assert!(!tree.nodes().any(|node| node.label == "file.xml"));
    Ok(())
}

#[test]
fn component_missing_class_metadata_keeps_filename_and_given_offsets() -> Result<()> {
    let (mut model, mut files) = fixture()?;
    let java = model.modules[0].directory.join("java");
    model.modules[0].source_roots = vec![root(&java, SourceGroup::KotlinAndJava, "main")];
    files.entries.extend([
        directory(&java),
        canonical_file(java.join("NoClass.java")),
        FileFact {
            path: java.join("MisleadingFilename.java"),
            kind: FileKind::File {
                java_classes: vec![JavaClassFact {
                    name: "ActualClass".into(),
                    byte_offset: Some(42),
                }],
            },
        },
    ]);
    let tree = project_tree(&model, &files)?;
    assert!(
        tree.nodes()
            .any(|node| node.reference_label == "NoClass.java (main)")
    );
    assert!(!tree.nodes().any(|node| node.label == "MisleadingFilename"));
    assert_eq!(
        tree.nodes()
            .find(|node| node.label == "ActualClass")
            .unwrap()
            .navigation
            .as_ref()
            .unwrap()
            .byte_offset,
        Some(42)
    );
    assert!(
        tree.nodes()
            .any(|node| node.reference_label == "kotlin+java")
    );
    Ok(())
}

#[test]
fn component_disabling_package_compaction_and_asset_hierarchy() -> Result<()> {
    let (mut model, mut files) = fixture()?;
    let java = model.modules[0].directory.join("java");
    let assets = model.modules[0].directory.join("assets");
    model.modules[0].source_roots = vec![
        root(&java, SourceGroup::Java, "main"),
        root(&assets, SourceGroup::Assets, "main"),
    ];
    files.entries.extend([
        directory(&java),
        canonical_file(java.join("com/example/one.txt")),
        directory(&assets),
        canonical_file(assets.join("com/example/one.txt")),
    ]);
    let tree = project_tree(&model, &files)?;
    assert!(tree.nodes().any(|node| node.label == "com.example"));
    assert!(leaf_paths(&tree).contains(&vec![
        "app (Android)".into(),
        "assets".into(),
        "com (main)".into(),
        "example (main)".into(),
        "one.txt (main)".into()
    ]));
    model.modules[0].compact_packages = false;
    let tree = project_tree(&model, &files)?;
    assert!(!tree.nodes().any(|node| node.label == "com.example"));
    Ok(())
}

#[test]
fn component_obsolete_file_facts_and_unknown_provider_are_rejected() -> Result<()> {
    let (mut model, mut files) = fixture()?;
    files.model_revision -= 1;
    assert!(
        project_tree(&model, &files)
            .unwrap_err()
            .to_string()
            .contains("obsolete")
    );
    files.model_revision = model.revision;
    model.modules[0].source_roots[0].provider = SourceProvider::Unknown;
    assert!(
        format!("{:#}", project_tree(&model, &files).unwrap_err())
            .contains("metadata is unavailable")
    );
    Ok(())
}

#[test]
fn component_conflicting_file_provider_and_module_facts_are_rejected() -> Result<()> {
    let (mut model, mut files) = fixture()?;
    files.entries.push(directory(
        &files
            .entries
            .iter()
            .find(|entry| matches!(entry.kind, FileKind::File { .. }))
            .unwrap()
            .path,
    ));
    assert!(
        project_tree(&model, &files)
            .unwrap_err()
            .to_string()
            .contains("Conflicting file facts")
    );
    let (new_model, new_files) = fixture()?;
    model = new_model;
    files = new_files;
    let mut root = model.modules[0].source_roots[0].clone();
    root.provider = SourceProvider::Named("debug".into());
    model.modules[0].source_roots.push(root);
    assert!(
        project_tree(&model, &files)
            .unwrap_err()
            .to_string()
            .contains("Conflicting source providers")
    );
    model.modules[0].source_roots.last_mut().unwrap().group = SourceGroup::Java;
    assert!(
        project_tree(&model, &files)
            .unwrap_err()
            .to_string()
            .contains("Conflicting source providers")
    );
    let (mut model, files) = fixture()?;
    model.modules.push(model.modules[0].clone());
    assert!(
        project_tree(&model, &files)
            .unwrap_err()
            .to_string()
            .contains("Duplicate Android module")
    );
    model.modules.pop();
    let mut files = files;
    files.entries = vec![canonical_file(&model.modules[0].directory)];
    assert!(
        project_tree(&model, &files)
            .unwrap_err()
            .to_string()
            .contains("module directory is a file")
    );
    Ok(())
}

#[test]
fn component_file_ancestor_contradictions_are_rejected() -> Result<()> {
    let (mut model, mut files) = fixture()?;
    let assets = model.modules[0].directory.join("ancestorConflict");
    model.modules[0].source_roots = vec![root(&assets, SourceGroup::Assets, "main")];
    files.entries = vec![directory(&assets), canonical_file(assets.join("parent"))];
    for descendant in [
        canonical_file(assets.join("parent/child.txt")),
        directory(assets.join("parent/child")),
    ] {
        files.entries.push(descendant);
        assert!(
            project_tree(&model, &files)
                .unwrap_err()
                .to_string()
                .contains("File has a descendant")
        );
        files.entries.pop();
    }
    Ok(())
}

#[test]
fn component_invalid_source_kind_and_unsafe_paths_are_rejected() -> Result<()> {
    let (mut model, mut files) = fixture()?;
    let path = model.modules[0].directory.join("wrongKind");
    model.modules[0].source_roots = vec![root(&path, SourceGroup::Java, "main")];
    files.entries.push(canonical_file(&path));
    assert!(
        project_tree(&model, &files)
            .unwrap_err()
            .to_string()
            .contains("wrong file kind")
    );
    model.modules[0].source_roots[0].group = SourceGroup::Manifests;
    files.entries.retain(|entry| entry.path != path);
    files.entries.push(directory(&path));
    assert!(
        project_tree(&model, &files)
            .unwrap_err()
            .to_string()
            .contains("wrong file kind")
    );
    let (model, mut files) = fixture()?;
    files.entries.push(raw_file("relative/file.txt"));
    assert!(
        project_tree(&model, &files)
            .unwrap_err()
            .to_string()
            .contains("not absolute")
    );
    files.entries.pop();
    files
        .entries
        .push(raw_file(model.modules[0].directory.join("../outside")));
    assert!(
        project_tree(&model, &files)
            .unwrap_err()
            .to_string()
            .contains("not normalized")
    );
    for suffix in ["folder/./file.txt", "folder//file.txt"] {
        files.entries.pop();
        files
            .entries
            .push(raw_file(model.modules[0].directory.join(suffix)));
        assert!(
            project_tree(&model, &files)
                .unwrap_err()
                .to_string()
                .contains("not normalized")
        );
    }
    Ok(())
}

#[test]
fn component_deep_hierarchy_builds_and_drops_without_recursive_nodes() -> Result<()> {
    let (mut model, mut files) = fixture()?;
    let assets = model.modules[0].directory.join("deep");
    let mut nested = assets.clone();
    for _ in 0..512 {
        nested.push("a");
    }
    model.modules[0].source_roots = vec![root(&assets, SourceGroup::Assets, "main")];
    files
        .entries
        .extend([directory(&assets), canonical_file(nested.join("last.txt"))]);
    let tree = project_tree(&model, &files)?;
    assert_eq!(tree.nodes().count(), 515);
    assert_eq!(leaf_paths(&tree)[0].len(), 515);
    drop(tree);
    Ok(())
}

#[test]
fn component_large_flat_project_preserves_targets_with_unique_keys() -> Result<()> {
    let (mut model, mut files) = fixture()?;
    let assets = model.modules[0].directory.join("large");
    model.modules[0].source_roots = vec![root(&assets, SourceGroup::Assets, "main")];
    files.entries = vec![directory(&assets)];
    files.entries.extend(
        (0..20_000).map(|index| canonical_file(assets.join(format!("shared/file{index:05}.txt")))),
    );
    let tree = project_tree(&model, &files)?;
    assert_eq!(tree.nodes().count(), 20_003);
    assert_eq!(
        tree.nodes()
            .filter(|node| node.navigation.is_some())
            .count(),
        20_000
    );
    assert_eq!(
        tree.nodes()
            .map(|node| &node.key)
            .collect::<BTreeSet<_>>()
            .len(),
        20_003
    );
    Ok(())
}

#[test]
fn component_original_sample_intentional_failure_is_preserved() -> Result<()> {
    let source = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/project_tree/simpleApplication/app/src/test/java/google/simpleapplication/UnitTest.java"))?;
    assert!(source.contains("public void failingTest()"));
    assert!(source.contains("Assert.assertEquals(5, 2 + 2);"));
    assert!(source.contains("Copyright (C) 2015 The Android Open Source Project"));
    assert_eq!(source.len(), 1003);
    Ok(())
}
