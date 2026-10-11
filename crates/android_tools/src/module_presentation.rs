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

//! Source-derived display policy over authoritative captured import identities.
//! The host supplies imported names and exact reported external-system values;
//! this module performs no model import, path normalization, facet detection,
//! holder resolution, file scanning, or publication.

use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportedGradleModuleType {
    Project,
    SourceSet,
}

/// `None` means a getter reported null, not an unobserved getter. Paths remain
/// strings because the reference compares reported values without normalizing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapturedGradleIdentity<'a> {
    pub module_type: ImportedGradleModuleType,
    pub external_project_id: Option<&'a str>,
    pub external_project_path: Option<&'a str>,
    pub external_root_project_path: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapturedExternalSystemIdentity<'a> {
    Unavailable,
    NotGradle,
    Gradle(CapturedGradleIdentity<'a>),
}

/// Names include any qualified-name escaping or collision suffix applied by the
/// importer. A holder lookup falling back to this module must capture its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapturedModuleIdentity<'a> {
    pub internal_name: &'a str,
    pub holder_internal_name: &'a str,
    pub external_system: CapturedExternalSystemIdentity<'a>,
}

/// Empty labels are preserved here. Consumers with a narrower supported-input
/// boundary must report that limitation instead of silently inventing a label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedModulePresentation<'a> {
    pub internal_name: &'a str,
    pub display_name: &'a str,
    pub group_display_name: &'a str,
    pub sort_name: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModulePresentationUnavailable {
    MissingExternalSystemIdentity,
}

impl fmt::Display for ModulePresentationUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Captured external-system module identity is unavailable")
    }
}

impl Error for ModulePresentationUnavailable {}

/// Applies `GradleModuleSystem.getDisplayNameForModule`, with its imported-name
/// fallback, and keeps the distinct holder-group and internal-sort identities.
/// Kotlin enablement and view preferences require separate authoritative facts.
pub fn resolve_module_presentation<'a>(
    identity: &CapturedModuleIdentity<'a>,
) -> Result<ResolvedModulePresentation<'a>, ModulePresentationUnavailable> {
    let gradle_name = match identity.external_system {
        CapturedExternalSystemIdentity::Unavailable => {
            return Err(ModulePresentationUnavailable::MissingExternalSystemIdentity);
        }
        CapturedExternalSystemIdentity::NotGradle => None,
        CapturedExternalSystemIdentity::Gradle(gradle) => display_name_from_gradle(gradle),
    };
    Ok(ResolvedModulePresentation {
        internal_name: identity.internal_name,
        display_name: gradle_name.unwrap_or(identity.internal_name),
        group_display_name: identity.holder_internal_name,
        sort_name: identity.internal_name,
    })
}

fn display_name_from_gradle(identity: CapturedGradleIdentity<'_>) -> Option<&str> {
    let project_id = identity.external_project_id?;
    if identity.module_type == ImportedGradleModuleType::SourceSet {
        return project_id.rsplit_once(':').map(|(_, suffix)| suffix);
    }
    if identity.external_project_path == identity.external_root_project_path {
        return Some(project_id);
    }
    Some(
        project_id
            .rsplit_once(':')
            .map_or(project_id, |(_, suffix)| suffix),
    )
}
