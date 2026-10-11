// Tests adapted from GradleModuleTemplateTest at
// a84efec3ba9542d9bfa1255103f0dc94833a3796.
// Copyright (C) 2021 The Android Open Source Project.
// Licensed under the Apache License, Version 2.0.

use android_tools::module_template::DefaultModuleTemplate;
use std::path::{MAIN_SEPARATOR_STR, Path, PathBuf};

fn native_path(path: &str) -> PathBuf {
    path.replace('/', MAIN_SEPARATOR_STR).into()
}

#[test]
fn default_source_set_at_current_dir() {
    let template = DefaultModuleTemplate::at(".");

    assert_eq!(template.name(), "main");
    assert_eq!(
        template.module_root().as_os_str(),
        Path::new(".").as_os_str()
    );
    assert_eq!(
        template.source_directory(Some("my.package")).as_os_str(),
        native_path("./src/main/java/my/package").as_os_str()
    );
    assert_eq!(
        template.test_directory(Some("my.package")).as_os_str(),
        native_path("./src/androidTest/java/my/package").as_os_str()
    );
    assert_eq!(
        template.aidl_directory(Some("my.package")).as_os_str(),
        native_path("./src/main/aidl/my/package").as_os_str()
    );
    assert_eq!(
        template.manifest_directory().as_os_str(),
        native_path("./src/main").as_os_str()
    );
    assert_eq!(
        template
            .resource_directories()
            .iter()
            .map(|directory| directory.as_os_str())
            .collect::<Vec<_>>(),
        [native_path("./src/main/res").as_os_str()]
    );
}

#[test]
fn default_source_set_at_invalid_dir() {
    // The wizard needs to construct a layout while a field temporarily contains an invalid path.
    for root in [":", "<", ">", "?", "\0"] {
        assert_eq!(
            DefaultModuleTemplate::at(root).module_root().as_os_str(),
            Path::new(root).as_os_str()
        );
    }
}

#[test]
fn null_and_empty_package_keep_each_source_root() {
    let template = DefaultModuleTemplate::at("module");
    for package in [None, Some("")] {
        assert_eq!(
            template.source_directory(package).as_os_str(),
            native_path("module/src/main/java").as_os_str()
        );
        assert_eq!(
            template.test_directory(package).as_os_str(),
            native_path("module/src/androidTest/java").as_os_str()
        );
        assert_eq!(
            template.unit_test_directory(package).as_os_str(),
            native_path("module/src/test/java").as_os_str()
        );
        assert_eq!(
            template.aidl_directory(package).as_os_str(),
            native_path("module/src/main/aidl").as_os_str()
        );
    }
}

#[test]
fn unit_tests_and_ml_models_use_pinned_default_directories() {
    let template = DefaultModuleTemplate::at("module");
    assert_eq!(
        template.unit_test_directory(Some("my.package")).as_os_str(),
        native_path("module/src/test/java/my/package").as_os_str()
    );
    assert_eq!(
        template
            .ml_model_directories()
            .iter()
            .map(|directory| directory.as_os_str())
            .collect::<Vec<_>>(),
        [native_path("module/src/main/ml").as_os_str()]
    );
}

#[test]
fn empty_root_uses_the_reference_default_parent_for_children() {
    let template = DefaultModuleTemplate::at("");
    assert_eq!(
        template.module_root().as_os_str(),
        Path::new("").as_os_str()
    );
    assert_eq!(
        template.manifest_directory().as_os_str(),
        native_path("/src/main").as_os_str()
    );
    assert_eq!(
        template.source_directory(None).as_os_str(),
        native_path("/src/main/java").as_os_str()
    );
}

#[test]
fn rooted_package_suffix_does_not_replace_the_source_root() {
    let template = DefaultModuleTemplate::at("module");
    for package in [".my..package.", "..my.package", "/my/package/"] {
        assert_eq!(
            template.source_directory(Some(package)).as_os_str(),
            native_path("module/src/main/java/my/package").as_os_str()
        );
    }
}

#[test]
fn dot_segments_and_nul_are_lexical_values() {
    let root = native_path("uncreated/../module/.");
    let template = DefaultModuleTemplate::at(root.clone());
    assert_eq!(template.module_root().as_os_str(), root.as_os_str());
    assert_eq!(
        template.source_directory(Some("my.package")).as_os_str(),
        native_path("uncreated/../module/./src/main/java/my/package").as_os_str()
    );

    let template = DefaultModuleTemplate::at("\0");
    assert_eq!(
        template.source_directory(Some("my.\0")).as_os_str(),
        native_path("\0/src/main/java/my/\0").as_os_str()
    );
}
