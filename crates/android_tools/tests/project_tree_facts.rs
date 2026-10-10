/*
 * Copyright (C) 2014 The Android Open Source Project
 * Copyright (C) 2017 The Android Open Source Project
 * Copyright (C) 2019 The Android Open Source Project
 * Copyright (C) 2020 The Android Open Source Project
 * Copyright (C) 2021 The Android Open Source Project
 * Copyright (C) 2022 The Android Open Source Project
 * Copyright (C) 2023 The Android Open Source Project
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

// Supplemental component tests. None credits an original Gradle/GPUI tree case.

use android_tools::{
    project_model::{
        ArtifactSourceProvider, Component, EvaluatedProviderMetadata, Module, ModuleKind,
        NamedProviderArtifact, NativeProviderMembership, NativeProviderOrder, ProviderArtifact,
        ProviderContainer, ProviderDimension, ProviderMetadataUnavailable, ProviderModelVersion,
        ProviderSuiteDefinition, ProviderToolingModel, ProviderVariant, SourceProvider,
        SourceProviderCatalog, SourceProviderOrder, SourceProviderRoot, SourceProviderRootKind,
        SourceScope, Variant,
    },
    project_tree::{
        FileFact, FileKind, NodeKey, SourceGroup, SourceProvider as TreeProvider, TreeFiles,
        TreeModel, TreeModule, TreeSourceRoot, project_tree_with_facts,
    },
    project_tree_facts::{
        ActiveProviderIndex, FactsUnavailableReason, ModuleProjectionFacts, ProviderPresence,
        ProviderRole, RootEncounterFacts, RootEncounterProvenance, RootOccurrence, RootPresence,
        TreeProjectionFacts,
    },
};
use anyhow::{Context as _, Result};
use std::path::{Path, PathBuf};

fn path(root: &Path, relative: &str) -> PathBuf {
    root.join(relative).components().collect()
}

fn provider(name: &str, roots: Vec<(PathBuf, SourceProviderRootKind)>) -> SourceProvider {
    SourceProvider {
        name: name.into(),
        roots: roots
            .into_iter()
            .map(|(path, kind)| SourceProviderRoot { path, kind })
            .collect(),
    }
}

fn container(main: Option<SourceProvider>) -> ProviderContainer {
    ProviderContainer {
        main,
        host_tests: Vec::new(),
        device_tests: Vec::new(),
        fixtures: None,
    }
}

fn artifact(variant: Option<SourceProvider>) -> ProviderArtifact {
    ProviderArtifact {
        multi_flavor: None,
        variant,
    }
}

fn fixture(root: &Path) -> Module {
    Module {
        path: ":app".into(),
        directory: root.into(),
        namespace: Some("dev.example".into()),
        kind: ModuleKind::Application,
        default_variant: None,
        source_providers: None,
        variants: vec![Variant {
            name: "redWideDebug".into(),
            output_listing: None,
            components: vec![Component {
                name: "redWideDebug".into(),
                namespace: Some("dev.example".into()),
                scope: SourceScope::Main,
                sources: Vec::new(),
                dependencies: Vec::new(),
            }],
        }],
        evaluated_providers: Some(EvaluatedProviderMetadata::Available(ProviderToolingModel {
            version: 1,
            agp_version: "9.4.0".into(),
            model_producer: ProviderModelVersion {
                major: 22,
                minor: 0,
            },
            default_source_set: Some(container(Some(provider("main", Vec::new())))),
            build_types: vec![ProviderDimension {
                name: Some("debug".into()),
                container: container(Some(provider("debug", Vec::new()))),
            }],
            product_flavors: vec![
                ProviderDimension {
                    name: Some("wide".into()),
                    container: container(Some(provider("wide", Vec::new()))),
                },
                ProviderDimension {
                    name: Some("red".into()),
                    container: container(Some(provider("red", Vec::new()))),
                },
                ProviderDimension {
                    name: Some("inactive".into()),
                    container: container(Some(provider("inactive", Vec::new()))),
                },
            ],
            variants: vec![ProviderVariant {
                name: "redWideDebug".into(),
                build_type: Some("debug".into()),
                product_flavors: vec!["red".into(), "wide".into()],
                main: ProviderArtifact {
                    multi_flavor: Some(provider("redWide", Vec::new())),
                    variant: Some(provider("redWideDebug", Vec::new())),
                },
                host_tests: Vec::new(),
                device_tests: Vec::new(),
                fixtures: None,
                test_suites: Some(Vec::new()),
            }],
            test_suites: Some(Vec::new()),
            native_membership: Some(vec![NativeProviderMembership {
                variant: "redWideDebug".into(),
                component: "redWideDebug".into(),
                artifact: "_main_".into(),
                order: NativeProviderOrder::AgpSourceProviderNames,
                providers: Some(vec![
                    "main".into(),
                    "wide".into(),
                    "red".into(),
                    "redWide".into(),
                    "debug".into(),
                    "redWideDebug".into(),
                ]),
            }]),
        })),
    }
}

fn metadata(module: &mut Module) -> Result<&mut ProviderToolingModel> {
    match &mut module.evaluated_providers {
        Some(EvaluatedProviderMetadata::Available(model)) => Ok(model),
        _ => anyhow::bail!("Fixture provider model is absent"),
    }
}

fn names(index: &ActiveProviderIndex) -> Vec<&str> {
    index
        .providers()
        .iter()
        .map(|provider| provider.provider.name.as_str())
        .collect()
}

#[test]
fn studio_forward_flavors_and_native_reverse_flavors_remain_distinct() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let module = fixture(temporary.path());
    let index = ActiveProviderIndex::from_module(&module, "redWideDebug", 7)?;
    assert_eq!(
        names(&index),
        ["main", "red", "wide", "redWide", "debug", "redWideDebug"]
    );
    let native = index
        .native_membership()
        .context("Native membership missing")?;
    assert_eq!(
        native[0].providers.as_ref().context("Names missing")?,
        &["main", "wide", "red", "redWide", "debug", "redWideDebug"]
    );
    assert!(!names(&index).contains(&"inactive"));
    Ok(())
}

#[test]
fn all_variant_host_containers_survive_absent_selected_artifacts_but_device_and_fixture_do_not()
-> Result<()> {
    let temporary = tempfile::tempdir()?;
    let mut module = fixture(temporary.path());
    let model = metadata(&mut module)?;
    let default = model
        .default_source_set
        .as_mut()
        .context("Default missing")?;
    default.host_tests = vec![
        ArtifactSourceProvider {
            artifact: "_screenshot_test_".into(),
            provider: provider("screenshotTest", Vec::new()),
        },
        ArtifactSourceProvider {
            artifact: "_unit_test_".into(),
            provider: provider("test", Vec::new()),
        },
    ];
    default.device_tests = vec![ArtifactSourceProvider {
        artifact: "_android_test_".into(),
        provider: provider("androidTest", Vec::new()),
    }];
    default.fixtures = Some(provider("testFixtures", Vec::new()));
    let index = ActiveProviderIndex::from_module(&module, "redWideDebug", 1)?;
    assert_eq!(
        names(&index),
        [
            "main",
            "red",
            "wide",
            "redWide",
            "debug",
            "redWideDebug",
            "test",
            "screenshotTest"
        ]
    );
    assert_eq!(index.providers()[6].role, ProviderRole::UnitTest);
    assert_eq!(index.providers()[7].role, ProviderRole::ScreenshotTest);
    Ok(())
}

#[test]
fn main_host_device_suite_fixture_encounters_match_reference_concatenation() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let mut module = fixture(temporary.path());
    let model = metadata(&mut module)?;
    let default = model
        .default_source_set
        .as_mut()
        .context("Default missing")?;
    default.host_tests = vec![ArtifactSourceProvider {
        artifact: "_unit_test_".into(),
        provider: provider("test", Vec::new()),
    }];
    default.device_tests = vec![ArtifactSourceProvider {
        artifact: "_android_test_".into(),
        provider: provider("androidTest", Vec::new()),
    }];
    default.fixtures = Some(provider("testFixtures", Vec::new()));
    let selected = &mut model.variants[0];
    selected.host_tests = vec![NamedProviderArtifact {
        artifact: "_unit_test_".into(),
        sources: artifact(Some(provider("testRedWideDebug", Vec::new()))),
    }];
    selected.device_tests = vec![NamedProviderArtifact {
        artifact: "_android_test_".into(),
        sources: artifact(Some(provider("androidTestRedWideDebug", Vec::new()))),
    }];
    selected.fixtures = Some(artifact(Some(provider(
        "testFixturesRedWideDebug",
        Vec::new(),
    ))));
    selected.test_suites = Some(vec!["second".into(), "first".into()]);
    model.test_suites = Some(vec![
        ProviderSuiteDefinition {
            name: "first".into(),
            providers: Some(vec![provider("firstSource", Vec::new())]),
        },
        ProviderSuiteDefinition {
            name: "second".into(),
            providers: Some(vec![
                provider("secondA", Vec::new()),
                provider("secondB", Vec::new()),
            ]),
        },
    ]);
    let index = ActiveProviderIndex::from_module(&module, "redWideDebug", 1)?;
    assert_eq!(
        names(&index),
        [
            "main",
            "red",
            "wide",
            "redWide",
            "debug",
            "redWideDebug",
            "test",
            "testRedWideDebug",
            "androidTest",
            "androidTestRedWideDebug",
            "secondA",
            "secondB",
            "firstSource",
            "testFixtures",
            "testFixturesRedWideDebug"
        ]
    );
    Ok(())
}

#[test]
fn first_active_provider_wins_over_deeper_root_and_later_native_precedence() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let mut module = fixture(temporary.path());
    let broad = path(temporary.path(), "sources");
    let nested = broad.join("nested");
    let entry = nested.join("Actual.java");
    let model = metadata(&mut module)?;
    model.default_source_set = Some(container(Some(provider(
        "main",
        vec![(broad.clone(), SourceProviderRootKind::Java)],
    ))));
    model.product_flavors[1].container.main = Some(provider(
        "red",
        vec![(nested.clone(), SourceProviderRootKind::Java)],
    ));
    let index = ActiveProviderIndex::from_module(&module, "redWideDebug", 3)?;
    let presence = ProviderPresence::new(
        3,
        8,
        [
            (broad.clone(), RootPresence::Directory),
            (nested, RootPresence::Directory),
            (entry.clone(), RootPresence::File),
        ],
    )?;
    let resolved = index.resolve(&entry, &presence)?;
    assert_eq!(
        resolved
            .candidates
            .iter()
            .map(|candidate| candidate.name.as_str())
            .collect::<Vec<_>>(),
        ["main", "red"]
    );
    assert_eq!(resolved.winner().context("No provider")?.root, broad);
    Ok(())
}

#[test]
fn provider_winner_changes_per_entry_within_one_projected_root() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let base = path(temporary.path(), "source");
    let nested = base.join("nested");
    let mut module = fixture(temporary.path());
    let model = metadata(&mut module)?;
    model.default_source_set = Some(container(Some(provider(
        "main",
        vec![(nested.clone(), SourceProviderRootKind::Assets)],
    ))));
    model.build_types[0].container.main = Some(provider(
        "debug",
        vec![(base.clone(), SourceProviderRootKind::Assets)],
    ));
    let index = ActiveProviderIndex::from_module(&module, "redWideDebug", 2)?;
    let tree_model = TreeModel {
        revision: 2,
        modules: vec![TreeModule {
            id: ":app".into(),
            display_name: "app".into(),
            directory: temporary.path().into(),
            compact_packages: false,
            source_roots: vec![TreeSourceRoot {
                path: base.clone(),
                group: SourceGroup::Assets,
                provider: TreeProvider::Unknown,
            }],
        }],
    };
    let files = TreeFiles {
        model_revision: 2,
        revision: 5,
        entries: vec![
            FileFact {
                path: base.clone(),
                kind: FileKind::Directory,
            },
            FileFact {
                path: nested.clone(),
                kind: FileKind::Directory,
            },
            FileFact {
                path: base.join("outer.txt"),
                kind: FileKind::File {
                    java_classes: Vec::new(),
                },
            },
            FileFact {
                path: nested.join("inner.txt"),
                kind: FileKind::File {
                    java_classes: Vec::new(),
                },
            },
        ],
    };
    let facts = TreeProjectionFacts {
        presence: ProviderPresence::new(2, 5, [])?,
        modules: vec![ModuleProjectionFacts {
            providers: index,
            roots: RootEncounterFacts {
                model_revision: 2,
                module: ":app".into(),
                variant: "redWideDebug".into(),
                provenance: RootEncounterProvenance::ProducerIterator,
                roots: vec![RootOccurrence {
                    group: SourceGroup::Assets,
                    path: base,
                }],
            },
        }],
    };
    let tree = project_tree_with_facts(&tree_model, &files, &facts)?;
    let outer = tree
        .nodes()
        .find(|node| node.label == "outer.txt")
        .context("Outer missing")?;
    let inner = tree
        .nodes()
        .find(|node| node.label == "inner.txt")
        .context("Inner missing")?;
    let directory = tree
        .nodes()
        .find(|node| node.label == "nested")
        .context("Directory missing")?;
    assert_eq!(outer.reference_label, "outer.txt (debug)");
    assert_eq!(outer.annotation.as_deref(), Some(" (debug)"));
    assert_eq!(inner.reference_label, "inner.txt (main)");
    assert_eq!(inner.annotation, None);
    assert_eq!(directory.reference_label, "nested (main)");
    assert_eq!(
        inner
            .navigation
            .as_ref()
            .context("Navigation missing")?
            .path,
        nested.join("inner.txt")
    );
    Ok(())
}

#[test]
fn shared_roots_retain_all_candidates_and_ignore_inactive_dsl_catalog() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = path(temporary.path(), "shared");
    let entry = root.join("Example.xml");
    let mut module = fixture(temporary.path());
    let model = metadata(&mut module)?;
    model.product_flavors[0].container.main = Some(provider(
        "wide",
        vec![(root.clone(), SourceProviderRootKind::Resources)],
    ));
    model.product_flavors[1].container.main = Some(provider(
        "red",
        vec![(root.clone(), SourceProviderRootKind::Resources)],
    ));
    module.source_providers = Some(SourceProviderCatalog {
        order: SourceProviderOrder::GradleSourceSetIteration,
        providers: vec![provider(
            "inactive",
            vec![(root.clone(), SourceProviderRootKind::Resources)],
        )],
    });
    let index = ActiveProviderIndex::from_module(&module, "redWideDebug", 1)?;
    let presence = ProviderPresence::new(
        1,
        2,
        [
            (root, RootPresence::Directory),
            (entry.clone(), RootPresence::File),
        ],
    )?;
    let resolved = index.resolve(&entry, &presence)?;
    assert_eq!(
        resolved
            .candidates
            .iter()
            .map(|candidate| (&candidate.name, candidate.encounter))
            .collect::<Vec<_>>(),
        [(&"red".to_string(), 1), (&"wide".to_string(), 2)]
    );
    Ok(())
}

#[test]
fn existing_manifest_parent_matches_even_when_the_manifest_is_missing() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let parent = path(temporary.path(), "manifestParent");
    let manifest = parent.join("AndroidManifest.xml");
    let mut module = fixture(temporary.path());
    metadata(&mut module)?.default_source_set = Some(container(Some(provider(
        "main",
        vec![(manifest.clone(), SourceProviderRootKind::Manifest)],
    ))));
    let index = ActiveProviderIndex::from_module(&module, "redWideDebug", 1)?;
    let presence = ProviderPresence::new(
        1,
        2,
        [
            (parent.clone(), RootPresence::Directory),
            (manifest, RootPresence::Missing),
        ],
    )?;
    let resolution = index.resolve(&parent, &presence)?;
    assert_eq!(
        resolution.winner().context("Parent not attributed")?.name,
        "main"
    );
    assert_eq!(
        resolution.winner().context("Parent not attributed")?.root,
        parent
    );
    let child = parent.join("unrelated.txt");
    let child_presence = ProviderPresence::new(1, 2, [(child.clone(), RootPresence::File)])?;
    assert!(index.resolve(&child, &child_presence)?.winner().is_none());
    Ok(())
}

#[test]
fn manifest_exact_file_matches_and_other_files_are_unattributed() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let manifest = path(temporary.path(), "custom.xml");
    let mut module = fixture(temporary.path());
    metadata(&mut module)?.default_source_set = Some(container(Some(provider(
        "main",
        vec![(manifest.clone(), SourceProviderRootKind::Manifest)],
    ))));
    let index = ActiveProviderIndex::from_module(&module, "redWideDebug", 1)?;
    let presence = ProviderPresence::new(1, 2, [(manifest.clone(), RootPresence::File)])?;
    assert_eq!(
        index
            .resolve(&manifest, &presence)?
            .winner()
            .context("Manifest not attributed")?
            .root,
        manifest
    );
    Ok(())
}

#[test]
fn unknown_relevant_root_never_becomes_unattributed_or_a_later_winner() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = path(temporary.path(), "source");
    let entry = root.join("Example.java");
    let mut module = fixture(temporary.path());
    metadata(&mut module)?.default_source_set = Some(container(Some(provider(
        "main",
        vec![(root.clone(), SourceProviderRootKind::Java)],
    ))));
    let index = ActiveProviderIndex::from_module(&module, "redWideDebug", 1)?;
    let presence = ProviderPresence::new(1, 2, [(entry.clone(), RootPresence::File)])?;
    let error = index
        .resolve(&entry, &presence)
        .expect_err("Unknown root cannot choose a provider");
    assert_eq!(error.reason, FactsUnavailableReason::UnknownPresence);
    assert_eq!(error.path.as_ref(), Some(&root));
    Ok(())
}

#[test]
fn known_absent_or_noncontaining_roots_do_not_match() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let configured = path(temporary.path(), "configured");
    let sibling = path(temporary.path(), "configuredOther/Example.java");
    let mut module = fixture(temporary.path());
    metadata(&mut module)?.default_source_set = Some(container(Some(provider(
        "main",
        vec![(configured.clone(), SourceProviderRootKind::Java)],
    ))));
    let index = ActiveProviderIndex::from_module(&module, "redWideDebug", 1)?;
    let presence = ProviderPresence::new(
        1,
        2,
        [
            (configured, RootPresence::Missing),
            (sibling.clone(), RootPresence::File),
        ],
    )?;
    assert!(index.resolve(&sibling, &presence)?.winner().is_none());
    Ok(())
}

#[test]
fn reference_category_order_precedes_raw_root_order_inside_a_provider() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let broad = path(temporary.path(), "source");
    let nested = broad.join("nested");
    let entry = nested.join("Example.java");
    let mut module = fixture(temporary.path());
    metadata(&mut module)?.default_source_set = Some(container(Some(provider(
        "main",
        vec![
            (broad.clone(), SourceProviderRootKind::Assets),
            (nested.clone(), SourceProviderRootKind::Java),
        ],
    ))));
    let index = ActiveProviderIndex::from_module(&module, "redWideDebug", 1)?;
    let presence = ProviderPresence::new(
        1,
        2,
        [
            (broad, RootPresence::Directory),
            (nested.clone(), RootPresence::Directory),
            (entry.clone(), RootPresence::File),
        ],
    )?;
    assert_eq!(
        index
            .resolve(&entry, &presence)?
            .winner()
            .context("Provider missing")?
            .root,
        nested
    );
    Ok(())
}

#[test]
fn stale_presence_missing_entries_and_conflicting_presence_are_explicit() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let entry = path(temporary.path(), "Example.java");
    let module = fixture(temporary.path());
    let index = ActiveProviderIndex::from_module(&module, "redWideDebug", 3)?;
    let stale = ProviderPresence::new(2, 8, [(entry.clone(), RootPresence::File)])?;
    assert_eq!(
        index
            .resolve(&entry, &stale)
            .expect_err("Stale facts")
            .reason,
        FactsUnavailableReason::Stale
    );
    let absent = ProviderPresence::new(3, 8, [(entry.clone(), RootPresence::Missing)])?;
    assert_eq!(
        index
            .resolve(&entry, &absent)
            .expect_err("Missing candidate")
            .reason,
        FactsUnavailableReason::MissingEntry
    );
    assert_eq!(
        ProviderPresence::new(
            3,
            8,
            [
                (entry.clone(), RootPresence::Directory),
                (entry, RootPresence::File)
            ]
        )
        .expect_err("Conflicting presence")
        .reason,
        FactsUnavailableReason::Malformed
    );
    Ok(())
}

#[test]
fn unavailable_legacy_kmp_and_capability_metadata_never_fall_back_to_dsl() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let mut module = fixture(temporary.path());
    module.evaluated_providers = None;
    assert_eq!(
        ActiveProviderIndex::from_module(&module, "redWideDebug", 1)
            .expect_err("Missing metadata")
            .reason,
        FactsUnavailableReason::MissingMetadata
    );
    module.evaluated_providers = Some(EvaluatedProviderMetadata::Unavailable(
        ProviderMetadataUnavailable {
            capability: "KMP".into(),
            detail: "unsupported source model".into(),
        },
    ));
    assert_eq!(
        ActiveProviderIndex::from_module(&module, "redWideDebug", 1)
            .expect_err("Unsupported KMP")
            .reason,
        FactsUnavailableReason::Capability
    );
    Ok(())
}

#[test]
fn unsupported_model_version_and_artifact_converter_disagreement_are_explicit() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let mut module = fixture(temporary.path());
    metadata(&mut module)?.model_producer.major = 10;
    assert_eq!(
        ActiveProviderIndex::from_module(&module, "redWideDebug", 1)
            .expect_err("Legacy assets unproved")
            .reason,
        FactsUnavailableReason::UnsupportedShape
    );
    metadata(&mut module)?.model_producer.major = 22;
    metadata(&mut module)?
        .default_source_set
        .as_mut()
        .context("Default missing")?
        .host_tests = vec![ArtifactSourceProvider {
        artifact: "_unit_test_".into(),
        provider: provider("screenshotTestShared", Vec::new()),
    }];
    assert_eq!(
        ActiveProviderIndex::from_module(&module, "redWideDebug", 1)
            .expect_err("Converter disagreement")
            .reason,
        FactsUnavailableReason::UnsupportedShape
    );
    Ok(())
}

#[test]
fn nonempty_unconverted_suites_and_unknown_membership_remain_unavailable() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let mut module = fixture(temporary.path());
    metadata(&mut module)?.variants[0].test_suites = None;
    assert_eq!(
        ActiveProviderIndex::from_module(&module, "redWideDebug", 1)
            .expect_err("Unknown membership")
            .reason,
        FactsUnavailableReason::Capability
    );
    let model = metadata(&mut module)?;
    model.variants[0].test_suites = Some(vec!["actualSuite".into()]);
    model.test_suites = Some(vec![ProviderSuiteDefinition {
        name: "actualSuite".into(),
        providers: None,
    }]);
    assert_eq!(
        ActiveProviderIndex::from_module(&module, "redWideDebug", 1)
            .expect_err("Unconverted suite")
            .reason,
        FactsUnavailableReason::UnsupportedShape
    );
    Ok(())
}

#[test]
fn malformed_dimensions_variants_and_provider_paths_are_rejected() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let module = fixture(temporary.path());
    let mut malformed = module.clone();
    metadata(&mut malformed)?.product_flavors[0].name = None;
    assert_eq!(
        ActiveProviderIndex::from_module(&malformed, "redWideDebug", 1)
            .expect_err("No dimension identity")
            .reason,
        FactsUnavailableReason::UnsupportedShape
    );
    malformed = module.clone();
    let duplicate = metadata(&mut malformed)?.variants[0].clone();
    metadata(&mut malformed)?.variants.push(duplicate);
    assert_eq!(
        ActiveProviderIndex::from_module(&malformed, "redWideDebug", 1)
            .expect_err("Duplicate variant")
            .reason,
        FactsUnavailableReason::Malformed
    );
    malformed = module;
    metadata(&mut malformed)?.default_source_set = Some(container(Some(provider(
        "main",
        vec![(PathBuf::from("relative/java"), SourceProviderRootKind::Java)],
    ))));
    assert_eq!(
        ActiveProviderIndex::from_module(&malformed, "redWideDebug", 1)
            .expect_err("Unsafe root")
            .reason,
        FactsUnavailableReason::Malformed
    );
    Ok(())
}

#[test]
fn optional_evaluated_metadata_round_trips_without_erasing_unknown_capabilities() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let mut module = fixture(temporary.path());
    metadata(&mut module)?.native_membership = None;
    let serialized = serde_json::to_value(&module)?;
    assert!(serialized["evaluatedProviders"]["value"]["nativeMembership"].is_null());
    let decoded: Module = serde_json::from_value(serialized)?;
    let index = ActiveProviderIndex::from_module(&decoded, "redWideDebug", 1)?;
    assert!(index.native_membership().is_none());
    let mut legacy = serde_json::to_value(&module)?;
    legacy
        .as_object_mut()
        .context("Module not object")?
        .remove("evaluatedProviders");
    let decoded: Module = serde_json::from_value(legacy)?;
    assert!(decoded.evaluated_providers.is_none());
    Ok(())
}

fn projection_fixture(
    root: &Path,
    roots: Vec<(PathBuf, SourceGroup)>,
) -> Result<(TreeModel, TreeFiles, TreeProjectionFacts)> {
    let mut module = fixture(root);
    metadata(&mut module)?.default_source_set = Some(container(Some(provider(
        "main",
        roots
            .iter()
            .map(|(path, _)| (path.clone(), SourceProviderRootKind::Resources))
            .collect(),
    ))));
    let providers = ActiveProviderIndex::from_module(&module, "redWideDebug", 4)?;
    let tree_model = TreeModel {
        revision: 4,
        modules: vec![TreeModule {
            id: ":app".into(),
            display_name: "app".into(),
            directory: root.into(),
            compact_packages: false,
            source_roots: roots
                .iter()
                .map(|(path, group)| TreeSourceRoot {
                    path: path.clone(),
                    group: *group,
                    provider: TreeProvider::Unknown,
                })
                .collect(),
        }],
    };
    let files = TreeFiles {
        model_revision: 4,
        revision: 9,
        entries: roots
            .iter()
            .map(|(path, _)| FileFact {
                path: path.clone(),
                kind: FileKind::Directory,
            })
            .collect(),
    };
    let facts = TreeProjectionFacts {
        presence: ProviderPresence::new(4, 9, [])?,
        modules: vec![ModuleProjectionFacts {
            providers,
            roots: RootEncounterFacts {
                model_revision: 4,
                module: ":app".into(),
                variant: "redWideDebug".into(),
                provenance: RootEncounterProvenance::ProducerIterator,
                roots: roots
                    .into_iter()
                    .map(|(path, group)| RootOccurrence { group, path })
                    .collect(),
            },
        }],
    };
    Ok((tree_model, files, facts))
}

#[test]
fn explicit_resource_root_encounter_preserves_first_main_without_fallback() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let first = path(temporary.path(), "z/main/res");
    let second = path(temporary.path(), "a/main/res");
    let (model, mut files, mut facts) = projection_fixture(
        temporary.path(),
        vec![
            (first, SourceGroup::Resources),
            (second.clone(), SourceGroup::Resources),
        ],
    )?;
    let properties = second.join("resources.properties");
    files.entries.push(FileFact {
        path: properties.clone(),
        kind: FileKind::File {
            java_classes: Vec::new(),
        },
    });
    let tree = project_tree_with_facts(&model, &files, &facts)?;
    assert!(
        !tree
            .nodes()
            .any(|node| node.label == "resources.properties")
    );
    facts.modules[0].roots.roots.reverse();
    let tree = project_tree_with_facts(&model, &files, &facts)?;
    let node = tree
        .nodes()
        .find(|node| node.label == "resources.properties")
        .context("Properties missing")?;
    assert_eq!(node.reference_label, "resources.properties (main)");
    assert_eq!(
        node.navigation.as_ref().context("Navigation missing")?.path,
        properties
    );
    Ok(())
}

#[test]
fn projection_rejects_unknown_incomplete_or_stale_root_encounters() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let (model, files, facts) = projection_fixture(
        temporary.path(),
        vec![(path(temporary.path(), "main/res"), SourceGroup::Resources)],
    )?;
    let mut invalid = facts.clone();
    invalid.modules[0].roots.provenance = RootEncounterProvenance::Unknown;
    assert!(
        project_tree_with_facts(&model, &files, &invalid)
            .expect_err("Unknown order")
            .to_string()
            .contains("encounter order")
    );
    invalid = facts.clone();
    invalid.modules[0].roots.roots.clear();
    assert!(
        project_tree_with_facts(&model, &files, &invalid)
            .expect_err("Incomplete roots")
            .to_string()
            .contains("cover")
    );
    invalid = facts.clone();
    invalid.modules[0].roots.model_revision = 3;
    assert!(
        project_tree_with_facts(&model, &files, &invalid)
            .expect_err("Stale roots")
            .to_string()
            .contains("obsolete")
    );
    invalid = facts.clone();
    invalid.presence.file_revision = 8;
    assert!(
        project_tree_with_facts(&model, &files, &invalid)
            .expect_err("Stale files")
            .to_string()
            .contains("obsolete")
    );
    invalid = facts;
    invalid.modules[0].roots.variant = "anotherVariant".into();
    assert!(
        project_tree_with_facts(&model, &files, &invalid)
            .expect_err("Variant disagreement")
            .to_string()
            .contains("variants disagree")
    );
    Ok(())
}

#[test]
fn twenty_thousand_evaluated_entries_resolve_without_foreground_or_disk_work() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = path(temporary.path(), "main/res");
    let (mut model, mut files, mut facts) =
        projection_fixture(temporary.path(), vec![(root.clone(), SourceGroup::Assets)])?;
    model.modules[0].compact_packages = false;
    for number in 0..20_000 {
        files.entries.push(FileFact {
            path: root.join(format!("entry{number:05}.txt")),
            kind: FileKind::File {
                java_classes: Vec::new(),
            },
        });
    }
    let tree = project_tree_with_facts(&model, &files, &facts)?;
    assert_eq!(
        tree.nodes()
            .filter(|node| matches!(node.key, NodeKey::File { .. }))
            .count(),
        20_000
    );
    assert!(
        tree.nodes()
            .filter(|node| matches!(node.key, NodeKey::File { .. }))
            .all(|node| {
                node.reference_label.ends_with(" (main)")
                    && node
                        .navigation
                        .as_ref()
                        .is_some_and(|target| target.path.starts_with(&root))
            })
    );
    facts.presence = ProviderPresence::new(4, 9, [(root, RootPresence::Missing)])?;
    assert!(
        project_tree_with_facts(&model, &files, &facts)
            .expect_err("Contradictory root presence")
            .to_string()
            .contains("disagrees")
    );
    Ok(())
}

#[test]
fn presence_rejects_present_descendants_below_missing_or_file_ancestors() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let ancestor = path(temporary.path(), "ancestor");
    let intervening = ancestor.join("existing");
    let child = intervening.join("root");
    for contradictory in [RootPresence::Missing, RootPresence::File] {
        let error = ProviderPresence::new(
            4,
            9,
            [
                (ancestor.clone(), contradictory),
                (intervening.clone(), RootPresence::Directory),
                (child.clone(), RootPresence::Directory),
            ],
        )
        .expect_err("Intervening existing directory cannot hide an ancestor contradiction");
        assert_eq!(error.reason, FactsUnavailableReason::Malformed);
        assert!(error.detail.contains("ancestor"));
    }
    Ok(())
}

#[test]
fn projection_distinguishes_unknown_missing_and_uninventoried_present_roots() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = path(temporary.path(), "main/res");
    let (model, mut files, mut facts) = projection_fixture(
        temporary.path(),
        vec![(root.clone(), SourceGroup::Resources)],
    )?;
    files.entries.clear();
    let unknown =
        project_tree_with_facts(&model, &files, &facts).expect_err("Unknown cannot become omitted");
    assert_eq!(
        unknown
            .downcast_ref::<android_tools::project_tree_facts::FactsUnavailable>()
            .context("Untyped unknown")?
            .reason,
        FactsUnavailableReason::UnknownPresence
    );
    facts.presence = ProviderPresence::new(4, 9, [(root.clone(), RootPresence::Missing)])?;
    let tree = project_tree_with_facts(&model, &files, &facts)?;
    assert_eq!(tree.nodes().count(), 1);
    facts.presence = ProviderPresence::new(4, 9, [(root, RootPresence::Directory)])?;
    assert!(
        project_tree_with_facts(&model, &files, &facts)
            .expect_err("Present root without inventory")
            .to_string()
            .contains("absent from the file inventory")
    );
    Ok(())
}

#[test]
fn observed_file_cannot_refine_unknown_parent_above_known_present_descendants() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = path(temporary.path(), "manifest.xml");
    let (model, mut files, mut facts) = projection_fixture(
        temporary.path(),
        vec![(root.clone(), SourceGroup::Manifests)],
    )?;
    facts.presence = ProviderPresence::new(
        4,
        9,
        [
            (root.clone(), RootPresence::Unknown),
            (root.join("known-child"), RootPresence::Directory),
        ],
    )?;
    files.entries = vec![FileFact {
        path: root,
        kind: FileKind::File {
            java_classes: Vec::new(),
        },
    }];
    let error = project_tree_with_facts(&model, &files, &facts)
        .expect_err("Observed File cannot become ancestor of a known Directory");
    assert_eq!(
        error
            .downcast_ref::<android_tools::project_tree_facts::FactsUnavailable>()
            .context("Untyped presence failure")?
            .reason,
        FactsUnavailableReason::Malformed
    );
    assert!(error.to_string().contains("ancestor"));
    Ok(())
}

#[test]
fn exact_resource_provider_is_not_stolen_by_earlier_java_assets_or_resource_ancestors() -> Result<()>
{
    let temporary = tempfile::tempdir()?;
    let broad = path(temporary.path(), "custom");
    let root = broad.join("main/res").components().collect::<PathBuf>();
    let layout = root.join("layout-land");
    let resource = layout.join("sample.xml");
    let properties = root.join("resources.properties");
    for kind in [
        SourceProviderRootKind::Java,
        SourceProviderRootKind::Assets,
        SourceProviderRootKind::Resources,
    ] {
        let mut module = fixture(temporary.path());
        let evaluated = metadata(&mut module)?;
        evaluated.default_source_set = Some(container(Some(provider(
            "main",
            vec![(broad.clone(), kind)],
        ))));
        evaluated.build_types[0].container.main = Some(provider(
            "debug",
            vec![(root.clone(), SourceProviderRootKind::Resources)],
        ));
        let providers = ActiveProviderIndex::from_module(&module, "redWideDebug", 4)?;
        let presence = ProviderPresence::new(
            4,
            9,
            [
                (broad.clone(), RootPresence::Directory),
                (root.clone(), RootPresence::Directory),
                (layout.clone(), RootPresence::Directory),
                (resource.clone(), RootPresence::File),
                (properties.clone(), RootPresence::File),
            ],
        )?;
        assert_eq!(
            providers
                .resolve(&resource, &presence)?
                .winner()
                .context("Generic provider missing")?
                .name,
            "main"
        );
        assert_eq!(
            providers
                .resolve_resource_root(&root, &presence)?
                .winner()
                .context("Exact resource provider missing")?
                .name,
            "debug"
        );
        let (model, mut files, mut facts) = projection_fixture(
            temporary.path(),
            vec![(root.clone(), SourceGroup::Resources)],
        )?;
        files.entries.extend([
            FileFact {
                path: layout.clone(),
                kind: FileKind::Directory,
            },
            FileFact {
                path: resource.clone(),
                kind: FileKind::File {
                    java_classes: Vec::new(),
                },
            },
            FileFact {
                path: properties.clone(),
                kind: FileKind::File {
                    java_classes: Vec::new(),
                },
            },
        ]);
        facts.presence = presence;
        facts.modules[0].providers = providers;
        let tree = project_tree_with_facts(&model, &files, &facts)?;
        let node = tree
            .nodes()
            .find(|node| node.label == "sample.xml")
            .context("Resource missing")?;
        assert_eq!(node.reference_label, "sample.xml (land, debug)");
        assert_eq!(node.annotation.as_deref(), Some(" (land, debug)"));
        let node = tree
            .nodes()
            .find(|node| node.label == "resources.properties")
            .context("Properties missing")?;
        assert_eq!(node.reference_label, "resources.properties (main)");
        assert_eq!(node.annotation, None);
    }
    Ok(())
}

#[test]
fn exact_resource_lookup_keeps_unknown_missing_file_and_stale_presence_distinct() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = path(temporary.path(), "res");
    let mut module = fixture(temporary.path());
    metadata(&mut module)?.default_source_set = Some(container(Some(provider(
        "main",
        vec![(root.clone(), SourceProviderRootKind::Resources)],
    ))));
    let index = ActiveProviderIndex::from_module(&module, "redWideDebug", 4)?;
    for (presence, reason) in [
        (
            RootPresence::Unknown,
            FactsUnavailableReason::UnknownPresence,
        ),
        (RootPresence::Missing, FactsUnavailableReason::MissingEntry),
        (RootPresence::File, FactsUnavailableReason::Malformed),
    ] {
        let facts = ProviderPresence::new(4, 9, [(root.clone(), presence)])?;
        assert_eq!(
            index
                .resolve_resource_root(&root, &facts)
                .expect_err("Unproved resource root")
                .reason,
            reason
        );
    }
    let stale = ProviderPresence::new(3, 9, [(root.clone(), RootPresence::Directory)])?;
    assert_eq!(
        index
            .resolve_resource_root(&root, &stale)
            .expect_err("Stale exact-root facts")
            .reason,
        FactsUnavailableReason::Stale
    );
    Ok(())
}

#[test]
fn successful_earlier_rule_ignores_later_unknown_but_earlier_unknown_blocks_match() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let broad = path(temporary.path(), "source");
    let directory = broad.join("manifest-parent");
    let mut module = fixture(temporary.path());
    metadata(&mut module)?.default_source_set = Some(container(Some(provider(
        "main",
        vec![
            (broad.clone(), SourceProviderRootKind::Java),
            (
                directory.join("AndroidManifest.xml"),
                SourceProviderRootKind::Manifest,
            ),
        ],
    ))));
    let presence = ProviderPresence::new(4, 9, [(directory.clone(), RootPresence::Directory)])?;
    let index = ActiveProviderIndex::from_module(&module, "redWideDebug", 4)?;
    let resolution = index.resolve(&directory, &presence)?;
    assert_eq!(resolution.candidates.len(), 1);
    assert_eq!(
        resolution.winner().context("Manifest parent missing")?.root,
        directory
    );

    metadata(&mut module)?.default_source_set = Some(container(Some(provider(
        "main",
        vec![
            (directory.clone(), SourceProviderRootKind::Assets),
            (broad.clone(), SourceProviderRootKind::Java),
        ],
    ))));
    let index = ActiveProviderIndex::from_module(&module, "redWideDebug", 4)?;
    let error = index
        .resolve(&directory, &presence)
        .expect_err("Unknown Java root precedes known Assets root");
    assert_eq!(error.reason, FactsUnavailableReason::UnknownPresence);
    assert_eq!(error.path, Some(broad));
    Ok(())
}

#[test]
fn thousands_of_disjoint_roots_keep_overlap_category_and_duplicate_provider_order() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let roots = (0..2_048)
        .map(|number| path(temporary.path(), &format!("disjoint{number:04}")))
        .collect::<Vec<_>>();
    let broad = path(temporary.path(), "shared");
    let nested = broad.join("nested");
    let entry = nested.join("Example.java");
    let mut main_roots = roots
        .iter()
        .map(|root| (root.clone(), SourceProviderRootKind::Java))
        .collect::<Vec<_>>();
    main_roots.extend([
        (nested.clone(), SourceProviderRootKind::Assets),
        (broad.clone(), SourceProviderRootKind::Java),
        (nested.clone(), SourceProviderRootKind::Java),
        (nested.clone(), SourceProviderRootKind::Resources),
        (nested.clone(), SourceProviderRootKind::Resources),
    ]);
    let mut module = fixture(temporary.path());
    let evaluated = metadata(&mut module)?;
    evaluated.default_source_set = Some(container(Some(provider("main", main_roots))));
    evaluated.build_types[0].container.main = Some(provider(
        "debug",
        vec![
            (nested.clone(), SourceProviderRootKind::Java),
            (nested.clone(), SourceProviderRootKind::Resources),
            (nested.clone(), SourceProviderRootKind::Resources),
        ],
    ));
    let index = ActiveProviderIndex::from_module(&module, "redWideDebug", 4)?;
    let mut paths = roots
        .iter()
        .flat_map(|root| {
            [
                (root.clone(), RootPresence::Directory),
                (root.join("Entry.java"), RootPresence::File),
            ]
        })
        .collect::<Vec<_>>();
    paths.extend([
        (broad.clone(), RootPresence::Directory),
        (nested.clone(), RootPresence::Directory),
        (entry.clone(), RootPresence::File),
    ]);
    let presence = ProviderPresence::new(4, 9, paths)?;
    for root in roots {
        let resolution = index.resolve(&root.join("Entry.java"), &presence)?;
        assert_eq!(resolution.candidates.len(), 1);
        let winner = resolution.winner().context("Disjoint provider missing")?;
        assert_eq!(winner.name, "main");
        assert_eq!(winner.root, root);
    }
    let overlap = index.resolve(&entry, &presence)?;
    assert_eq!(overlap.candidates.len(), 2);
    assert_eq!(overlap.candidates[0].name, "main");
    assert_eq!(overlap.candidates[0].root, broad);
    assert_eq!(overlap.candidates[1].name, "debug");
    assert_eq!(overlap.candidates[1].root, nested);
    let exact = index.resolve_resource_root(&nested, &presence)?;
    assert_eq!(exact.candidates.len(), 2);
    assert_eq!(exact.candidates[0].name, "main");
    assert_eq!(exact.candidates[1].name, "debug");
    Ok(())
}

#[test]
fn deep_shared_ancestors_keep_all_sibling_navigation_and_annotations() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = path(temporary.path(), "assets");
    let mut directory = root.clone();
    for depth in 0..96 {
        directory.push(format!("level{depth:02}"));
    }
    let (model, mut files, mut facts) =
        projection_fixture(temporary.path(), vec![(root.clone(), SourceGroup::Assets)])?;
    let mut module = fixture(temporary.path());
    metadata(&mut module)?.build_types[0].container.main = Some(provider(
        "debug",
        vec![(root, SourceProviderRootKind::Assets)],
    ));
    facts.modules[0].providers = ActiveProviderIndex::from_module(&module, "redWideDebug", 4)?;
    for number in 0..4_096 {
        files.entries.push(FileFact {
            path: directory.join(format!("entry{number:04}.txt")),
            kind: FileKind::File {
                java_classes: Vec::new(),
            },
        });
    }
    let tree = project_tree_with_facts(&model, &files, &facts)?;
    let directories = tree
        .nodes()
        .filter(|node| matches!(node.key, NodeKey::Directory { .. }))
        .collect::<Vec<_>>();
    assert_eq!(directories.len(), 96);
    assert!(
        directories
            .iter()
            .all(|node| node.annotation.as_deref() == Some(" (debug)"))
    );
    let entries = tree
        .nodes()
        .filter(|node| matches!(node.key, NodeKey::File { .. }))
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 4_096);
    for (number, node) in entries.into_iter().enumerate() {
        assert_eq!(node.label, format!("entry{number:04}.txt"));
        assert_eq!(node.annotation.as_deref(), Some(" (debug)"));
        assert_eq!(
            node.navigation.as_ref().context("Navigation missing")?.path,
            directory.join(&node.label)
        );
    }
    Ok(())
}

#[test]
fn thousands_of_root_encounters_preserve_first_main_and_builtin_group_priority() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let mut roots = (0..2_048)
        .map(|number| {
            (
                path(temporary.path(), &format!("source{number:04}/main/res")),
                SourceGroup::Resources,
            )
        })
        .collect::<Vec<_>>();
    let last = roots.last().context("Roots missing")?.0.clone();
    roots.push((last.clone(), SourceGroup::GeneratedResources));
    roots.push((last.clone(), SourceGroup::Resources));
    let (model, mut files, mut facts) = projection_fixture(temporary.path(), roots)?;
    let properties = last.join("resources.properties");
    files.entries.push(FileFact {
        path: properties.clone(),
        kind: FileKind::File {
            java_classes: Vec::new(),
        },
    });
    let tree = project_tree_with_facts(&model, &files, &facts)?;
    assert!(
        !tree
            .nodes()
            .any(|node| node.label == "resources.properties")
    );
    facts.modules[0].roots.roots.reverse();
    let tree = project_tree_with_facts(&model, &files, &facts)?;
    let entries = tree
        .nodes()
        .filter(|node| node.label == "resources.properties")
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1);
    let node = entries[0];
    assert!(matches!(
        node.key,
        NodeKey::File {
            group: Some(SourceGroup::Resources),
            ..
        }
    ));
    assert_eq!(node.reference_label, "resources.properties (main)");
    assert_eq!(
        node.navigation.as_ref().context("Navigation missing")?.path,
        properties
    );
    assert!(!tree.nodes().any(|node| matches!(
        node.key,
        NodeKey::Source {
            group: SourceGroup::GeneratedResources,
            ..
        }
    )));
    Ok(())
}

// These captured-fact regressions retain both ordering assertions from
// AndroidSourceTypeNodeTest.testNodeFoldersOrder. Its actual Gradle setup is
// still required before the original case can receive port/run parity credit.
fn source_folder_fixture(
    root: &Path,
    ordered_paths: Vec<PathBuf>,
) -> Result<(TreeModel, TreeFiles, TreeProjectionFacts)> {
    let main = path(root, "src/main/java");
    let android_test = path(root, "src/androidTest/java");
    let unit_test = path(root, "src/test/java");
    let mut module = fixture(root);
    let model = metadata(&mut module)?;
    let default = model
        .default_source_set
        .as_mut()
        .context("Default missing")?;
    let source = |name, directory: PathBuf| {
        provider(
            name,
            vec![
                (directory.clone(), SourceProviderRootKind::Java),
                (directory, SourceProviderRootKind::Kotlin),
            ],
        )
    };
    default.main = Some(source("main", main));
    default.host_tests = vec![ArtifactSourceProvider {
        artifact: "_unit_test_".into(),
        provider: source("test", unit_test),
    }];
    default.device_tests = vec![ArtifactSourceProvider {
        artifact: "_android_test_".into(),
        provider: source("androidTest", android_test),
    }];
    model.variants[0].device_tests = vec![NamedProviderArtifact {
        artifact: "_android_test_".into(),
        sources: artifact(None),
    }];
    let providers = ActiveProviderIndex::from_module(&module, "redWideDebug", 4)?;
    let (tree_model, files, mut facts) = projection_fixture(
        root,
        ordered_paths
            .into_iter()
            .map(|directory| (directory, SourceGroup::KotlinAndJava))
            .collect(),
    )?;
    facts.modules[0].providers = providers;
    Ok((tree_model, files, facts))
}

#[test]
fn source_folders_match_reference_order_for_expected_and_shuffled_captured_roots() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let main = path(temporary.path(), "src/main/java");
    let android_test = path(temporary.path(), "src/androidTest/java");
    let unit_test = path(temporary.path(), "src/test/java");
    let expected = vec![main.clone(), android_test.clone(), unit_test.clone()];
    let source_key = NodeKey::Source {
        module: ":app".into(),
        group: SourceGroup::KotlinAndJava,
    };
    let (model, files, facts) = source_folder_fixture(temporary.path(), expected.clone())?;
    let tree = project_tree_with_facts(&model, &files, &facts)?;
    assert_eq!(
        tree.source_folders(&source_key)
            .context("Source folders missing")?,
        expected
    );

    let shuffled = vec![unit_test, main, android_test];
    let (model, files, facts) = source_folder_fixture(temporary.path(), shuffled)?;
    let tree = project_tree_with_facts(&model, &files, &facts)?;
    assert_eq!(
        tree.source_folders(&source_key)
            .context("Source folders missing")?,
        expected
    );
    Ok(())
}

#[test]
fn equal_provider_sort_keys_preserve_actual_root_encounters() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let first = path(temporary.path(), "z/shared");
    let second = path(temporary.path(), "a/shared");
    let source_key = NodeKey::Source {
        module: ":app".into(),
        group: SourceGroup::Assets,
    };
    let (model, files, mut facts) = projection_fixture(
        temporary.path(),
        vec![
            (first.clone(), SourceGroup::Assets),
            (second.clone(), SourceGroup::Assets),
        ],
    )?;
    let tree = project_tree_with_facts(&model, &files, &facts)?;
    assert_eq!(
        tree.source_folders(&source_key)
            .context("Source folders missing")?,
        [first.clone(), second.clone()]
    );
    facts.modules[0].roots.roots.reverse();
    let tree = project_tree_with_facts(&model, &files, &facts)?;
    assert_eq!(
        tree.source_folders(&source_key)
            .context("Source folders missing")?,
        [second, first]
    );
    Ok(())
}

#[test]
fn actual_first_provider_orders_roots_instead_of_directory_names_or_deepest_roots() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let main = path(temporary.path(), "src/main/java");
    let android_test = path(temporary.path(), "src/androidTest/java");
    let unit_test = path(temporary.path(), "src/test/java");
    let nested = main.join("nested");
    let (model, mut files, mut facts) = source_folder_fixture(
        temporary.path(),
        vec![
            unit_test.clone(),
            android_test.clone(),
            main.clone(),
            nested.clone(),
        ],
    )?;
    let mut provider_module = fixture(temporary.path());
    let provider_model = metadata(&mut provider_module)?;
    provider_model.default_source_set = Some(container(Some(provider(
        "main",
        vec![(main.clone(), SourceProviderRootKind::Java)],
    ))));
    provider_model.product_flavors[1].container.main = Some(provider(
        "red",
        vec![(nested.clone(), SourceProviderRootKind::Java)],
    ));
    provider_model.build_types[0].container.main = Some(provider(
        "debug",
        vec![(android_test.clone(), SourceProviderRootKind::Java)],
    ));
    provider_model.product_flavors[0].container.main = Some(provider(
        "wide",
        vec![(unit_test.clone(), SourceProviderRootKind::Java)],
    ));
    facts.modules[0].providers =
        ActiveProviderIndex::from_module(&provider_module, "redWideDebug", 4)?;
    // Both nested and outer roots resolve to main through actual provider lookup.
    // Their equal keys preserve the supplied outer-before-nested encounters.
    let tree = project_tree_with_facts(&model, &files, &facts)?;
    let source_key = NodeKey::Source {
        module: ":app".into(),
        group: SourceGroup::KotlinAndJava,
    };
    assert_eq!(
        tree.source_folders(&source_key)
            .context("Source folders missing")?,
        [main, nested, android_test, unit_test]
    );
    // A deleted selected source root must not survive as a group folder.
    let deleted = model.modules[0].source_roots[0].path.clone();
    files.entries.retain(|entry| entry.path != deleted);
    facts.presence = ProviderPresence::new(4, 9, [(deleted.clone(), RootPresence::Missing)])?;
    let tree = project_tree_with_facts(&model, &files, &facts)?;
    assert!(
        !tree
            .source_folders(&source_key)
            .context("Source folders missing")?
            .contains(&deleted)
    );
    // The original metadata remains immutable across both projections.
    assert_eq!(model.modules[0].source_roots[0].path, deleted);
    Ok(())
}

#[test]
fn source_folder_provider_order_uses_utf16_and_not_unicode_scalar_order() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let supplementary = path(temporary.path(), "supplementary/java");
    let private_use = path(temporary.path(), "private/java");
    let mut module = fixture(temporary.path());
    let model = metadata(&mut module)?;
    let default = model
        .default_source_set
        .as_mut()
        .context("Default missing")?;
    default.main = Some(provider(
        "\u{e000}",
        vec![(private_use.clone(), SourceProviderRootKind::Java)],
    ));
    default.host_tests = vec![ArtifactSourceProvider {
        artifact: "_unit_test_".into(),
        provider: provider(
            "\u{10000}",
            vec![(supplementary.clone(), SourceProviderRootKind::Java)],
        ),
    }];
    let providers = ActiveProviderIndex::from_module(&module, "redWideDebug", 4)?;
    let (tree_model, files, mut facts) = projection_fixture(
        temporary.path(),
        vec![
            (private_use.clone(), SourceGroup::Java),
            (supplementary.clone(), SourceGroup::Java),
        ],
    )?;
    facts.modules[0].providers = providers;
    let tree = project_tree_with_facts(&tree_model, &files, &facts)?;
    let source_key = NodeKey::Source {
        module: ":app".into(),
        group: SourceGroup::Java,
    };
    assert_eq!(
        tree.source_folders(&source_key)
            .context("Source folders missing")?,
        [supplementary, private_use]
    );
    Ok(())
}

#[test]
fn source_folder_accessor_never_relabels_manifest_files_or_absent_groups_as_directories()
-> Result<()> {
    let temporary = tempfile::tempdir()?;
    let manifest = path(temporary.path(), "src/main/AndroidManifest.xml");
    let (model, mut files, mut facts) = projection_fixture(
        temporary.path(),
        vec![(manifest.clone(), SourceGroup::Manifests)],
    )?;
    files.entries[0].kind = FileKind::File {
        java_classes: Vec::new(),
    };
    let mut module = fixture(temporary.path());
    metadata(&mut module)?.default_source_set = Some(container(Some(provider(
        "main",
        vec![(manifest, SourceProviderRootKind::Manifest)],
    ))));
    facts.modules[0].providers = ActiveProviderIndex::from_module(&module, "redWideDebug", 4)?;
    let tree = project_tree_with_facts(&model, &files, &facts)?;
    assert!(
        tree.source_folders(&NodeKey::Source {
            module: ":app".into(),
            group: SourceGroup::Manifests
        })
        .is_none()
    );
    assert!(
        tree.source_folders(&NodeKey::Source {
            module: ":app".into(),
            group: SourceGroup::Java
        })
        .is_none()
    );
    assert!(
        tree.source_folders(&NodeKey::Module {
            module: ":app".into()
        })
        .is_none()
    );
    Ok(())
}
