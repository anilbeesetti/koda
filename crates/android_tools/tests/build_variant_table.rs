// Copyright (C) 2022 The Android Open Source Project
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

// Adapted from the four non-NDK methods in BuildVariantTableModelTest.kt.
// Original sources, notices and method mappings: test_data/build_variant_table.
use android_tools::build_variant_table::{
    AndroidProjectType, BuildVariantItem, BuildVariantModule, BuildVariantTableModel,
    BuildVariantTableRow, ModuleIdentity,
};
use anyhow::Result;

fn module(
    path: &str,
    project_type: AndroidProjectType,
    default: Option<&str>,
) -> BuildVariantModule {
    BuildVariantModule {
        module: ModuleIdentity {
            path: path.into(),
            name: format!("testProject.{}", path.trim_start_matches(':')),
        },
        project_type,
        selected_variant: Some("debug".into()),
        default_variant: default.map(str::to_owned),
        // The test builders expose debug/release; reversed input also exercises sorting.
        variants: vec!["release".into(), "debug".into()],
        dynamic_features: Vec::new(),
    }
}

fn expected_row(path: &str, default: Option<&str>) -> BuildVariantTableRow {
    BuildVariantTableRow {
        module: ModuleIdentity {
            path: path.into(),
            name: format!("testProject.{}", path.trim_start_matches(':')),
        },
        variant: "debug".into(),
        abi: None,
        build_variants: vec![
            BuildVariantItem {
                build_variant_name: "debug".into(),
                is_default: default == Some("debug"),
            },
            BuildVariantItem {
                build_variant_name: "release".into(),
                is_default: default == Some("release"),
            },
        ],
        abis: Vec::new(),
        is_dynamic_feature: false,
    }
}

#[test]
fn without_abi() -> Result<()> {
    let model = BuildVariantTableModel::create(
        &[
            module(":app", AndroidProjectType::Application, None),
            module(":lib", AndroidProjectType::Library, None),
        ],
        "en",
    )?;
    assert_eq!(model.rows.len(), 2);
    assert_eq!(model.rows.first(), Some(&expected_row(":app", None)));
    assert_eq!(model.rows.get(1), Some(&expected_row(":lib", None)));
    Ok(())
}

#[test]
fn test_default_variant_indicator() -> Result<()> {
    let model = BuildVariantTableModel::create(
        &[
            module(":app", AndroidProjectType::Application, Some("release")),
            module(":lib", AndroidProjectType::Library, Some("debug")),
        ],
        "en",
    )?;
    assert_eq!(model.rows.len(), 2);
    assert_eq!(
        model.rows.first(),
        Some(&expected_row(":app", Some("release")))
    );
    assert_eq!(
        model.rows.get(1),
        Some(&expected_row(":lib", Some("debug")))
    );
    Ok(())
}

#[test]
fn same_type_rows_are_sorted() -> Result<()> {
    let model = BuildVariantTableModel::create(
        &[":appB", ":appA", ":appD", ":appC"]
            .map(|path| module(path, AndroidProjectType::Application, None)),
        "en",
    )?;
    assert_eq!(model.rows.len(), 4);
    for (index, expected) in ["appA", "appB", "appC", "appD"].into_iter().enumerate() {
        assert_eq!(
            model.rows.get(index).map(|row| row.module.name.as_str()),
            Some(format!("testProject.{expected}").as_str())
        );
    }
    Ok(())
}

#[test]
fn different_type_rows_are_sorted_by_android_project_type() -> Result<()> {
    let model = BuildVariantTableModel::create(
        &[
            module(":xappC", AndroidProjectType::Application, None),
            module(":appB", AndroidProjectType::Application, None),
            module(":appA", AndroidProjectType::Application, None),
            module(":feature", AndroidProjectType::Feature, None),
            module(":libB", AndroidProjectType::Library, None),
            module(":alibA", AndroidProjectType::Library, None),
            module(":test", AndroidProjectType::Test, None),
        ],
        "en",
    )?;
    assert_eq!(model.rows.len(), 7);
    for (index, expected) in ["appA", "appB", "xappC", "alibA", "libB", "test", "feature"]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            model.rows.get(index).map(|row| row.module.name.as_str()),
            Some(format!("testProject.{expected}").as_str())
        );
    }
    Ok(())
}

#[test]
fn selected_default_and_display_are_independent() -> Result<()> {
    let model = BuildVariantTableModel::create(
        &[module(
            ":app",
            AndroidProjectType::Application,
            Some("release"),
        )],
        "en",
    )?;
    let row = model
        .rows
        .first()
        .ok_or_else(|| anyhow::anyhow!("Missing app row"))?;
    assert_eq!(row.variant_display_name()?, "debug");
    assert_eq!(
        row.build_variants
            .get(1)
            .map(BuildVariantItem::display_name),
        Some("release (default)".into())
    );
    assert!(row.build_variants_as_array().is_some());
    assert!(row.abis_as_array().is_none());
    assert_eq!(
        BuildVariantTableModel::COLUMN_NAMES,
        ["Module", "Active Build Variant"]
    );
    Ok(())
}

#[test]
fn dynamic_features_inherit_the_owning_application_row() -> Result<()> {
    let mut app = module(":app", AndroidProjectType::Application, Some("debug"));
    app.dynamic_features = vec![":zFeature".into(), ":aFeature".into()];
    let mut feature = module(
        ":zFeature",
        AndroidProjectType::DynamicFeature,
        Some("release"),
    );
    feature.selected_variant = Some("release".into());
    let model = BuildVariantTableModel::create(
        &[
            feature,
            module(":lib", AndroidProjectType::Library, None),
            app,
            module(":aFeature", AndroidProjectType::DynamicFeature, None),
            module(":orphan", AndroidProjectType::DynamicFeature, None),
        ],
        "en",
    )?;
    assert_eq!(model.rows.len(), 4);
    assert_eq!(
        model
            .rows
            .iter()
            .map(|row| row.module.path.as_str())
            .collect::<Vec<_>>(),
        [":app", ":zFeature", ":aFeature", ":lib"]
    );
    let app = model
        .rows
        .first()
        .ok_or_else(|| anyhow::anyhow!("Missing app row"))?;
    for row in model.rows.iter().skip(1).take(2) {
        assert_eq!(row.variant, app.variant);
        assert_eq!(row.build_variants, app.build_variants);
        assert_eq!(row.abi, None);
        assert!(row.abis.is_empty());
        assert!(row.is_dynamic_feature);
    }
    assert!(!app.is_dynamic_feature);
    assert!(!model.rows.last().is_some_and(|row| row.is_dynamic_feature));
    Ok(())
}

#[test]
fn missing_selection_is_unavailable_instead_of_a_first_variant() {
    let mut app = module(":app", AndroidProjectType::Application, None);
    app.selected_variant = None;
    assert!(BuildVariantTableModel::create(&[app], "en").is_err());
}

#[test]
fn unavailable_variant_default_or_duplicate_module_is_rejected() {
    let app = module(":app", AndroidProjectType::Application, None);
    let mut missing = app.clone();
    missing.selected_variant = Some("missing".into());
    assert!(BuildVariantTableModel::create(&[missing], "en").is_err());
    let mut default = app.clone();
    default.default_variant = Some("missing".into());
    assert!(BuildVariantTableModel::create(&[default], "en").is_err());
    let mut duplicate = app.clone();
    duplicate.variants.push("debug".into());
    assert!(BuildVariantTableModel::create(&[duplicate], "en").is_err());
    assert!(BuildVariantTableModel::create(&[app.clone(), app], "en").is_err());
}

#[test]
fn empty_variant_modules_are_omitted_and_empty_project_has_no_rows() -> Result<()> {
    let mut app = module(":app", AndroidProjectType::Application, None);
    app.variants.clear();
    app.selected_variant = None;
    assert!(
        BuildVariantTableModel::create(&[app.clone()], "en")?
            .rows
            .is_empty()
    );
    assert_eq!(
        BuildVariantTableModel::create(
            &[app, module(":lib", AndroidProjectType::Library, None)],
            "en"
        )?
        .rows,
        vec![expected_row(":lib", None)]
    );
    assert!(BuildVariantTableModel::create(&[], "en")?.rows.is_empty());
    Ok(())
}

#[test]
fn variant_names_use_java_utf16_order() -> Result<()> {
    let mut app = module(":app", AndroidProjectType::Application, None);
    app.selected_variant = Some("\u{10000}".into());
    app.variants = vec!["\u{e000}".into(), "\u{10000}".into(), "a".into()];
    let model = BuildVariantTableModel::create(&[app], "en")?;
    assert_eq!(
        model.rows.first().map(|row| row
            .build_variants
            .iter()
            .map(|item| item.build_variant_name.as_str())
            .collect::<Vec<_>>()),
        Some(vec!["a", "\u{10000}", "\u{e000}"])
    );
    Ok(())
}

#[test]
fn locale_collation_does_not_use_utf8_byte_order() -> Result<()> {
    let mut z = module(":z", AndroidProjectType::Application, None);
    z.module.name = "z".into();
    let mut accented = module(":accented", AndroidProjectType::Application, None);
    accented.module.name = "ä".into();
    let inputs = [z, accented];
    let german = BuildVariantTableModel::create(&inputs, "de")?;
    let swedish = BuildVariantTableModel::create(&inputs, "sv")?;
    assert_eq!(
        german.rows.first().map(|row| row.module.name.as_str()),
        Some("ä")
    );
    assert_eq!(
        swedish.rows.first().map(|row| row.module.name.as_str()),
        Some("z")
    );
    Ok(())
}

#[test]
fn invalid_or_duplicate_dynamic_ownership_is_rejected() {
    let mut app = module(":app", AndroidProjectType::Application, None);
    app.dynamic_features.push(":lib".into());
    assert!(
        BuildVariantTableModel::create(
            &[
                app.clone(),
                module(":lib", AndroidProjectType::Library, None)
            ],
            "en"
        )
        .is_err()
    );
    app.dynamic_features = vec![":feature".into(), ":feature".into()];
    assert!(
        BuildVariantTableModel::create(
            &[
                app.clone(),
                module(":feature", AndroidProjectType::DynamicFeature, None)
            ],
            "en"
        )
        .is_err()
    );
    app.dynamic_features.pop();
    let mut other_app = module(":otherApp", AndroidProjectType::Application, None);
    other_app.dynamic_features = app.dynamic_features.clone();
    assert!(
        BuildVariantTableModel::create(
            &[
                app,
                other_app,
                module(":feature", AndroidProjectType::DynamicFeature, None)
            ],
            "en"
        )
        .is_err()
    );
    let mut lib = module(":lib", AndroidProjectType::Library, None);
    lib.dynamic_features = vec![":feature".into()];
    assert!(
        BuildVariantTableModel::create(
            &[
                lib,
                module(":feature", AndroidProjectType::DynamicFeature, None)
            ],
            "en"
        )
        .is_err()
    );
}

#[test]
fn dynamic_feature_expansion_is_bounded_before_cloning_choices() {
    let mut app = module(":app", AndroidProjectType::Application, None);
    app.variants = (0..4096).map(|index| format!("variant{index}")).collect();
    app.selected_variant = Some("variant0".into());
    let mut modules = (0..64)
        .map(|index| {
            module(
                &format!(":feature{index}"),
                AndroidProjectType::DynamicFeature,
                None,
            )
        })
        .collect::<Vec<_>>();
    app.dynamic_features = modules
        .iter()
        .map(|module| module.module.path.clone())
        .collect();
    modules.push(app);
    let error = BuildVariantTableModel::create(&modules, "en");
    assert!(
        error.is_err(),
        "4096 choices times 65 inherited rows exceed the item budget"
    );
}

fn selected_project(
    include_library_selection: bool,
) -> Result<android_tools::project_model::SelectedProject> {
    use android_tools::project_model::{ProjectModel, SelectedProject, VariantId};
    use serde_json::json;
    use std::{collections::BTreeMap, sync::Arc};
    let model = Arc::new(serde_json::from_value::<ProjectModel>(json!({
        "version":1,"root":"/testProject","diagnostics":[],"modules":[
            {"path":":", "directory":"/testProject", "namespace":null,"kind":"jvm",
             "variants":[{"name":"jvm","components":[],"outputListing":null}]},
            {"path":":app", "directory":"/testProject/app", "namespace":null,"kind":"application",
             "defaultVariant":"release","variants":[
                {"name":"release","components":[],"outputListing":null},
                {"name":"debug","components":[],"outputListing":null}]},
            {"path":":lib", "directory":"/testProject/lib", "namespace":null,"kind":"library",
             "defaultVariant":"debug","variants":[
                {"name":"release","components":[],"outputListing":null},
                {"name":"debug","components":[],"outputListing":null}]}
        ]
    }))?);
    let mut variants = BTreeMap::from([(":app".into(), "debug".into())]);
    if include_library_selection {
        variants.insert(":lib".into(), "release".into());
    }
    Ok(SelectedProject {
        model,
        selected: VariantId {
            module: ":app".into(),
            variant: "debug".into(),
        },
        variants,
    })
}

#[test]
fn adapter_uses_independent_selected_variants_and_excludes_jvm_root() -> Result<()> {
    let selected = selected_project(true)?;
    let model = BuildVariantTableModel::from_selected_project(&selected, "en")?;
    assert_eq!(model.rows.len(), 2);
    assert_eq!(
        model
            .rows
            .first()
            .map(|row| (row.module.path.as_str(), row.variant.as_str())),
        Some((":app", "debug"))
    );
    assert_eq!(
        model
            .rows
            .get(1)
            .map(|row| (row.module.path.as_str(), row.variant.as_str())),
        Some((":lib", "release"))
    );
    assert_eq!(
        model.rows.first().map(|row| row
            .build_variants
            .iter()
            .filter(|item| item.is_default)
            .map(|item| item.build_variant_name.as_str())
            .collect::<Vec<_>>()),
        Some(vec!["release"])
    );
    assert_eq!(
        model.rows.get(1).map(|row| row
            .build_variants
            .iter()
            .filter(|item| item.is_default)
            .map(|item| item.build_variant_name.as_str())
            .collect::<Vec<_>>()),
        Some(vec!["debug"])
    );
    Ok(())
}

#[test]
fn adapter_does_not_omit_unselected_modules_or_guess_from_defaults() -> Result<()> {
    let selected = selected_project(false)?;
    assert!(BuildVariantTableModel::from_selected_project(&selected, "en").is_err());
    Ok(())
}

#[test]
fn dynamic_feature_reference_limit_is_checked_before_reference_contents() -> Result<()> {
    use anyhow::Context as _;
    let mut app = module(":app", AndroidProjectType::Application, None);
    app.dynamic_features = vec![String::new(); 16_384];
    let error = BuildVariantTableModel::create(&[app.clone()], "en")
        .err()
        .context("Empty ownership references must be rejected")?;
    assert_eq!(
        error.to_string(),
        "Dynamic feature ownership is missing, invalid, or duplicated"
    );
    app.dynamic_features.push(String::new());
    let error = BuildVariantTableModel::create(&[app], "en")
        .err()
        .context("Oversized reference list must be rejected before its contents")?;
    assert_eq!(error.to_string(), "Build variant table is too large");
    Ok(())
}

#[test]
fn dynamic_feature_reference_limit_is_shared_across_modules() -> Result<()> {
    use anyhow::Context as _;
    let mut first = module(":app", AndroidProjectType::Application, None);
    let mut second = module(":otherApp", AndroidProjectType::Application, None);
    first.dynamic_features = vec![String::new(); 8193];
    second.dynamic_features = vec![String::new(); 8193];
    let error = BuildVariantTableModel::create(&[first, second], "en")
        .err()
        .context("Aggregate reference count must be rejected before ownership scanning")?;
    assert_eq!(error.to_string(), "Build variant table is too large");
    Ok(())
}

#[test]
fn adapter_rejects_oversized_selected_and_default_names_before_cloning() -> Result<()> {
    use anyhow::Context as _;
    for selected_name in [true, false] {
        let mut selected = selected_project(true)?;
        let oversized = "v".repeat(64 * 1024 * 1024 + 1);
        if selected_name {
            selected.variants.insert(":app".into(), oversized);
        } else {
            std::sync::Arc::make_mut(&mut selected.model)
                .modules
                .iter_mut()
                .find(|module| module.path == ":app")
                .context("Application module")?
                .default_variant = Some(oversized);
        }
        let error = BuildVariantTableModel::from_selected_project(&selected, "en")
            .err()
            .context("Oversized typed name must be rejected before cloning or membership checks")?;
        assert_eq!(error.to_string(), "Build variant table is too large");
    }
    Ok(())
}

#[test]
fn adapter_budgets_repeated_selected_default_and_variant_names_together() -> Result<()> {
    use anyhow::Context as _;
    let mut selected = selected_project(true)?;
    let app = std::sync::Arc::make_mut(&mut selected.model)
        .modules
        .iter_mut()
        .find(|module| module.path == ":app")
        .context("Application module")?;
    let variant = app
        .variants
        .iter_mut()
        .find(|variant| variant.name == "debug")
        .context("Selected debug variant")?;
    variant.name = "v".repeat(22 * 1024 * 1024);
    app.default_variant = Some(variant.name.clone());
    selected
        .variants
        .insert(":app".into(), variant.name.clone());
    selected.selected.variant = variant.name.clone();
    let error = BuildVariantTableModel::from_selected_project(&selected, "en")
        .err()
        .context("Three individually valid 22 MiB names exceed the aggregate clone budget")?;
    assert_eq!(error.to_string(), "Build variant table is too large");
    Ok(())
}
