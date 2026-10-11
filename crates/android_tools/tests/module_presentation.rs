/*
 * Copyright (C) 2017 The Android Open Source Project
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

//! Supplemental source-derived policy cases, without original-test parity credit.

use android_tools::module_presentation::{
    CapturedExternalSystemIdentity, CapturedGradleIdentity, CapturedModuleIdentity,
    ImportedGradleModuleType, ModulePresentationUnavailable, ResolvedModulePresentation,
    resolve_module_presentation,
};

fn gradle<'a>(
    module_type: ImportedGradleModuleType,
    project_id: Option<&'a str>,
    project_path: Option<&'a str>,
    root_path: Option<&'a str>,
) -> CapturedModuleIdentity<'a> {
    CapturedModuleIdentity {
        internal_name: "Imported.Project.app.main~1",
        holder_internal_name: "Imported.Project.app",
        external_system: CapturedExternalSystemIdentity::Gradle(CapturedGradleIdentity {
            module_type,
            external_project_id: project_id,
            external_project_path: project_path,
            external_root_project_path: root_path,
        }),
    }
}

fn presentation<'a>(identity: &CapturedModuleIdentity<'a>) -> ResolvedModulePresentation<'a> {
    resolve_module_presentation(identity).expect("Complete captured identity")
}

#[test]
fn non_gradle_uses_imported_name_and_separate_holder_and_sort_identities() {
    let identity = CapturedModuleIdentity {
        internal_name: "Zebra_internal~1",
        holder_internal_name: "Holder.Name",
        external_system: CapturedExternalSystemIdentity::NotGradle,
    };
    assert_eq!(
        presentation(&identity),
        ResolvedModulePresentation {
            internal_name: "Zebra_internal~1",
            display_name: "Zebra_internal~1",
            group_display_name: "Holder.Name",
            sort_name: "Zebra_internal~1",
        }
    );
}

#[test]
fn uncaptured_external_identity_cannot_be_treated_as_non_gradle() {
    let identity = CapturedModuleIdentity {
        internal_name: "Imported.app",
        holder_internal_name: "Imported.app",
        external_system: CapturedExternalSystemIdentity::Unavailable,
    };
    assert_eq!(
        resolve_module_presentation(&identity),
        Err(ModulePresentationUnavailable::MissingExternalSystemIdentity)
    );
}

#[test]
fn root_project_retains_entire_reported_external_id() {
    let identity = gradle(
        ImportedGradleModuleType::Project,
        Some("Renamed Root:Project"),
        Some("/build/root"),
        Some("/build/root"),
    );
    assert_eq!(presentation(&identity).display_name, "Renamed Root:Project");
}

#[test]
fn nonroot_project_uses_last_colon_component() {
    let identity = gradle(
        ImportedGradleModuleType::Project,
        Some(":nested:feature:application"),
        Some("/build/root/nested/app"),
        Some("/build/root"),
    );
    let resolved = presentation(&identity);
    assert_eq!(resolved.display_name, "application");
    assert_eq!(resolved.internal_name, "Imported.Project.app.main~1");
    assert_eq!(resolved.group_display_name, "Imported.Project.app");
    assert_eq!(resolved.sort_name, "Imported.Project.app.main~1");
}

#[test]
fn nonroot_id_without_colon_is_already_short() {
    let identity = gradle(
        ImportedGradleModuleType::Project,
        Some("IndependentName"),
        Some("/build/child"),
        Some("/build/root"),
    );
    assert_eq!(presentation(&identity).display_name, "IndependentName");
}

#[test]
fn two_null_reported_paths_compare_equal_like_reference() {
    let identity = gradle(
        ImportedGradleModuleType::Project,
        Some(":nested:app"),
        None,
        None,
    );
    assert_eq!(presentation(&identity).display_name, ":nested:app");
}

#[test]
fn one_null_reported_path_does_not_make_a_root() {
    for paths in [(None, Some("/root")), (Some("/root"), None)] {
        let identity = gradle(
            ImportedGradleModuleType::Project,
            Some(":nested:app"),
            paths.0,
            paths.1,
        );
        assert_eq!(presentation(&identity).display_name, "app");
    }
}

#[test]
fn reported_paths_are_not_filesystem_normalized() {
    let identity = gradle(
        ImportedGradleModuleType::Project,
        Some(":nested:app"),
        Some("/root/./"),
        Some("/root"),
    );
    assert_eq!(presentation(&identity).display_name, "app");
}

#[test]
fn reported_path_comparison_is_case_sensitive() {
    let identity = gradle(
        ImportedGradleModuleType::Project,
        Some(":nested:app"),
        Some("/Build/Root"),
        Some("/build/root"),
    );
    assert_eq!(presentation(&identity).display_name, "app");
}

#[test]
fn missing_project_id_uses_internal_name_for_root_and_nonroot() {
    for project_path in [Some("/root"), Some("/root/app"), None] {
        let identity = gradle(
            ImportedGradleModuleType::Project,
            None,
            project_path,
            Some("/root"),
        );
        assert_eq!(
            presentation(&identity).display_name,
            "Imported.Project.app.main~1"
        );
    }
}

#[test]
fn empty_project_id_remains_empty_for_root_and_nonroot() {
    for project_path in [Some("/root"), Some("/root/app")] {
        let identity = gradle(
            ImportedGradleModuleType::Project,
            Some(""),
            project_path,
            Some("/root"),
        );
        assert_eq!(presentation(&identity).display_name, "");
    }
}

#[test]
fn source_set_id_uses_final_suffix_even_when_paths_are_equal() {
    let identity = gradle(
        ImportedGradleModuleType::SourceSet,
        Some(":root:app:debugUnitTest"),
        Some("/root"),
        Some("/root"),
    );
    let resolved = presentation(&identity);
    assert_eq!(resolved.display_name, "debugUnitTest");
    assert_eq!(resolved.group_display_name, "Imported.Project.app");
    assert_eq!(resolved.sort_name, "Imported.Project.app.main~1");
}

#[test]
fn source_set_id_without_colon_uses_imported_name() {
    let identity = gradle(
        ImportedGradleModuleType::SourceSet,
        Some("main"),
        None,
        None,
    );
    assert_eq!(
        presentation(&identity).display_name,
        "Imported.Project.app.main~1"
    );
}

#[test]
fn missing_source_set_id_uses_imported_name() {
    let identity = gradle(
        ImportedGradleModuleType::SourceSet,
        None,
        Some("/root/app"),
        Some("/root"),
    );
    assert_eq!(
        presentation(&identity).display_name,
        "Imported.Project.app.main~1"
    );
}

#[test]
fn source_set_trailing_colon_is_empty_not_fallback() {
    for project_id in [":app:", ":", "::"] {
        let identity = gradle(
            ImportedGradleModuleType::SourceSet,
            Some(project_id),
            None,
            None,
        );
        assert_eq!(presentation(&identity).display_name, "");
    }
}

#[test]
fn nonroot_project_trailing_colon_is_empty_not_fallback() {
    let identity = gradle(
        ImportedGradleModuleType::Project,
        Some(":app:"),
        Some("/root/app"),
        Some("/root"),
    );
    assert_eq!(presentation(&identity).display_name, "");
}

#[test]
fn repeated_delimiters_preserve_the_last_component() {
    let identity = gradle(
        ImportedGradleModuleType::SourceSet,
        Some("::nested::main"),
        None,
        None,
    );
    assert_eq!(presentation(&identity).display_name, "main");
}

#[test]
fn reported_unicode_and_whitespace_are_not_reformatted() {
    let identity = gradle(
        ImportedGradleModuleType::Project,
        Some(":feature: Kotlin_模块 ~1 "),
        Some("/root/app"),
        Some("/root"),
    );
    assert_eq!(presentation(&identity).display_name, " Kotlin_模块 ~1 ");
}

#[test]
fn holder_lookup_fallback_can_capture_the_module_itself() {
    let identity = CapturedModuleIdentity {
        internal_name: "Imported.app",
        holder_internal_name: "Imported.app",
        external_system: CapturedExternalSystemIdentity::NotGradle,
    };
    assert_eq!(presentation(&identity).group_display_name, "Imported.app");
}

#[test]
fn long_captured_names_are_borrowed_without_reformatting() {
    let imported_name = "Qualified_".repeat(32_768);
    let external_id = format!("{imported_name}:application");
    let mut identity = gradle(
        ImportedGradleModuleType::Project,
        Some(&external_id),
        Some("/root/app"),
        Some("/root"),
    );
    identity.internal_name = &imported_name;
    identity.holder_internal_name = &imported_name;
    let resolved = presentation(&identity);
    assert_eq!(resolved.display_name, "application");
    assert_eq!(resolved.internal_name.len(), imported_name.len());
    assert_eq!(resolved.internal_name.as_ptr(), imported_name.as_ptr());
    assert_eq!(resolved.sort_name.as_ptr(), imported_name.as_ptr());
    assert_eq!(resolved.group_display_name.as_ptr(), imported_name.as_ptr());
}
