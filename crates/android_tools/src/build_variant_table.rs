// Copyright (C) 2014, 2022 The Android Open Source Project
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Non-NDK row projection adapted from AOSP `BuildVariantTableModel.kt` and
//! `ModuleTypeComparator.java` at a84efec3ba9542d9bfa1255103f0dc94833a3796.
//! Exact originals and test provenance are retained in `test_data/build_variant_table`.

use crate::project_model::{ModuleKind, SelectedProject};
use anyhow::{Context as _, Result, ensure};
use icu_collator::{Collator, options::CollatorOptions};
use icu_locale_core::Locale;
use std::collections::{BTreeMap, BTreeSet};

const MAXIMUM_MODULES: usize = 16_384;
const MAXIMUM_VARIANT_ITEMS: usize = 262_144;
const MAXIMUM_TEXT_BYTES: usize = 64 * 1024 * 1024;

/// The UI locale is explicit input to projection, rather than a byte-order fallback.
pub fn system_collation_locale() -> String {
    sys_locale::get_locale().unwrap_or_else(|| "und".into())
}

/// Ordinal order from `IdeAndroidProjectType`, including the distinct legacy feature type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AndroidProjectType {
    Application,
    Library,
    Test,
    Atom,
    InstantApplication,
    Feature,
    DynamicFeature,
    KotlinMultiplatform,
    FusedLibrary,
}

/// Stable model identity and the independently imported, project-qualified display name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleIdentity {
    pub path: String,
    pub name: String,
}

/// Evaluated/explicitly selected input; absent selection never means the first variant.
#[derive(Clone, Debug)]
pub struct BuildVariantModule {
    pub module: ModuleIdentity,
    pub project_type: AndroidProjectType,
    pub selected_variant: Option<String>,
    pub default_variant: Option<String>,
    pub variants: Vec<String>,
    /// Ordered ownership from the project system, not arbitrary dependency edges.
    pub dynamic_features: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildVariantItem {
    pub build_variant_name: String,
    pub is_default: bool,
}

impl BuildVariantItem {
    pub fn display_name(&self) -> String {
        if self.is_default {
            format!("{} (default)", self.build_variant_name)
        } else {
            self.build_variant_name.clone()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildVariantTableRow {
    pub module: ModuleIdentity,
    pub variant: String,
    pub abi: Option<String>,
    pub build_variants: Vec<BuildVariantItem>,
    pub abis: Vec<String>,
    pub is_dynamic_feature: bool,
}

impl BuildVariantTableRow {
    pub fn variant_item(&self) -> Result<&BuildVariantItem> {
        self.build_variants
            .iter()
            .find(|item| item.build_variant_name == self.variant)
            .context("The selected variant is absent from the module's variant choices")
    }

    pub fn variant_display_name(&self) -> Result<String> {
        Ok(self.variant_item()?.display_name())
    }

    pub fn build_variants_as_array(&self) -> Option<&[BuildVariantItem]> {
        (!self.build_variants.is_empty()).then_some(self.build_variants.as_slice())
    }

    pub fn abis_as_array(&self) -> Option<&[String]> {
        (!self.abis.is_empty()).then_some(self.abis.as_slice())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct BuildVariantTableModel {
    pub rows: Vec<BuildVariantTableRow>,
}

impl BuildVariantTableModel {
    pub const COLUMN_NAMES: [&'static str; 2] = ["Module", "Active Build Variant"];

    /// ICU4X supplies Rust locale collation; Java/ICU differential parity is tracked
    /// separately from the four original table tests, which use ASCII module names.
    pub fn create(modules: &[BuildVariantModule], locale: &str) -> Result<Self> {
        ensure!(modules.len() <= MAXIMUM_MODULES, "Too many Android modules");
        let locale: Locale = locale
            .parse()
            .context("Invalid module-name collation locale")?;
        let collator = Collator::try_new(locale.into(), CollatorOptions::default())
            .context("Module-name collation is unavailable")?;
        let mut identities = BTreeMap::new();
        let mut input_items = 0usize;
        let mut feature_references = 0usize;
        let mut text_bytes = 0usize;
        for module in modules {
            ensure!(
                !module.module.path.is_empty()
                    && !module.module.name.is_empty()
                    && identities
                        .insert(module.module.path.as_str(), module)
                        .is_none(),
                "Invalid or duplicate Android module identity"
            );
            add_budget(
                &mut input_items,
                module.variants.len(),
                MAXIMUM_VARIANT_ITEMS,
            )?;
            // Each feature has one owning app, so valid references cannot
            // exceed the module limit even when the strings are empty.
            add_budget(
                &mut feature_references,
                module.dynamic_features.len(),
                MAXIMUM_MODULES,
            )?;
            add_budget(
                &mut text_bytes,
                module.module.path.len(),
                MAXIMUM_TEXT_BYTES,
            )?;
            add_budget(
                &mut text_bytes,
                module.module.name.len(),
                MAXIMUM_TEXT_BYTES,
            )?;
            for name in &module.variants {
                add_budget(&mut text_bytes, name.len(), MAXIMUM_TEXT_BYTES)?;
            }
            for feature in &module.dynamic_features {
                add_budget(&mut text_bytes, feature.len(), MAXIMUM_TEXT_BYTES)?;
            }
        }
        let mut owners = BTreeSet::new();
        for module in modules {
            ensure!(
                module.dynamic_features.is_empty()
                    || module.project_type == AndroidProjectType::Application,
                "Only an application can own dynamic feature modules"
            );
            for feature in &module.dynamic_features {
                ensure!(
                    identities.get(feature.as_str()).is_some_and(|feature| {
                        feature.project_type == AndroidProjectType::DynamicFeature
                    }) && owners.insert(feature.as_str()),
                    "Dynamic feature ownership is missing, invalid, or duplicated"
                );
            }
        }
        let mut modules = modules
            .iter()
            .filter(|module| module.project_type != AndroidProjectType::DynamicFeature)
            .collect::<Vec<_>>();
        modules.sort_by(|left, right| {
            left.project_type
                .cmp(&right.project_type)
                .then_with(|| collator.compare(&left.module.name, &right.module.name))
        });
        let mut rows = Vec::new();
        let mut output_items = 0usize;
        let mut output_bytes = 0usize;
        for module in modules {
            if module.variants.is_empty() {
                continue;
            }
            let selected = module.selected_variant.as_ref().with_context(|| {
                format!("Select a build variant for module {}", module.module.name)
            })?;
            let names = module.variants.iter().collect::<BTreeSet<_>>();
            ensure!(
                names.len() == module.variants.len()
                    && !names.iter().any(|name| name.is_empty())
                    && names.contains(selected),
                "The selected variant or variant choices are invalid for {}",
                module.module.name
            );
            ensure!(
                module
                    .default_variant
                    .as_ref()
                    .is_none_or(|default| names.contains(default)),
                "The default variant is absent from module {}",
                module.module.name
            );
            let copies = module
                .dynamic_features
                .len()
                .checked_add(1)
                .context("Too many dynamic features")?;
            add_budget(
                &mut output_items,
                module
                    .variants
                    .len()
                    .checked_mul(copies)
                    .context("Too many build variant items")?,
                MAXIMUM_VARIANT_ITEMS,
            )?;
            let name_bytes = module.variants.iter().try_fold(0usize, |total, name| {
                total
                    .checked_add(name.len())
                    .context("Build variant names are too large")
            })?;
            add_budget(
                &mut output_bytes,
                name_bytes
                    .checked_mul(copies)
                    .context("Build variant names are too large")?,
                MAXIMUM_TEXT_BYTES,
            )?;
            let mut build_variants = module
                .variants
                .iter()
                .map(|name| BuildVariantItem {
                    build_variant_name: name.clone(),
                    is_default: module.default_variant.as_ref() == Some(name),
                })
                .collect::<Vec<_>>();
            // Kotlin String.compareTo compares UTF-16 code units, unlike Rust str::cmp.
            build_variants.sort_by(|left, right| {
                left.build_variant_name
                    .encode_utf16()
                    .cmp(right.build_variant_name.encode_utf16())
            });
            rows.push(BuildVariantTableRow {
                module: module.module.clone(),
                variant: selected.clone(),
                abi: None,
                build_variants,
                abis: Vec::new(),
                is_dynamic_feature: false,
            });
            for feature in &module.dynamic_features {
                let feature = identities
                    .get(feature.as_str())
                    .context("Dynamic feature ownership changed during projection")?;
                let app_row = rows
                    .last()
                    .context("The owning application row is unavailable")?;
                rows.push(BuildVariantTableRow {
                    module: feature.module.clone(),
                    variant: app_row.variant.clone(),
                    abi: None,
                    build_variants: app_row.build_variants.clone(),
                    abis: Vec::new(),
                    is_dynamic_feature: true,
                });
            }
        }
        Ok(Self { rows })
    }

    /// Current Gradle model selections cover only the dependency-reachable graph.
    /// Refuse missing per-module state or ownership instead of constructing guessed rows.
    pub fn from_selected_project(selected: &SelectedProject, locale: &str) -> Result<Self> {
        let mut items = 0usize;
        let mut text_bytes = 0usize;
        let mut count = 0usize;
        for module in &selected.model.modules {
            if module.kind == ModuleKind::Jvm {
                continue;
            }
            add_budget(&mut count, 1, MAXIMUM_MODULES)?;
            add_budget(&mut items, module.variants.len(), MAXIMUM_VARIANT_ITEMS)?;
            add_budget(
                &mut text_bytes,
                module
                    .path
                    .len()
                    .checked_mul(2)
                    .context("Module names are too large")?,
                MAXIMUM_TEXT_BYTES,
            )?;
            for variant in &module.variants {
                add_budget(&mut text_bytes, variant.name.len(), MAXIMUM_TEXT_BYTES)?;
            }
            for name in [
                selected.variants.get(&module.path),
                module.default_variant.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                add_budget(&mut text_bytes, name.len(), MAXIMUM_TEXT_BYTES)?;
            }
        }
        let modules = selected.model.modules.iter().filter(|module| module.kind != ModuleKind::Jvm)
            .map(|module| {
                let project_type = match module.kind {
                    ModuleKind::Application => AndroidProjectType::Application,
                    ModuleKind::Library => AndroidProjectType::Library,
                    ModuleKind::Test => AndroidProjectType::Test,
                    ModuleKind::DynamicFeature => anyhow::bail!(
                        "Dynamic feature build variants are unavailable until their ownership is synced"
                    ),
                    ModuleKind::Jvm => anyhow::bail!("JVM modules have no Android build variants"),
                };
                Ok(BuildVariantModule {
                    module: ModuleIdentity { path: module.path.clone(), name: module.path.clone() },
                    project_type,
                    selected_variant: selected.variants.get(&module.path).cloned(),
                    default_variant: module.default_variant.clone(),
                    variants: module.variants.iter().map(|variant| variant.name.clone()).collect(),
                    dynamic_features: Vec::new(),
                })
            }).collect::<Result<Vec<_>>>()?;
        Self::create(&modules, locale)
    }
}

fn add_budget(total: &mut usize, added: usize, maximum: usize) -> Result<()> {
    *total = total
        .checked_add(added)
        .context("Build variant table is too large")?;
    ensure!(*total <= maximum, "Build variant table is too large");
    Ok(())
}
