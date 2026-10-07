/*
 * Copyright (C) 2019 The Android Open Source Project
 * Copyright (C) 2025 The Android Open Source Project
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

//! Android project-view policy adapted from the pinned Android Studio settings
//! and pane implementations in `test_data/project_view_preferences`.
//!
//! This backend does not install an Android tree or UI controls. Its caller
//! supplies project capabilities, stores preferences, and displays returned
//! notifications. Events are local values; this module sends no telemetry.

use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write as _, ops::Range, path::Path};

pub const PROJECT_VIEW_DEFAULT_KEY: &str = "studio.projectview";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectViewCapabilities {
    pub is_android_project: bool,
    pub supports_android_view: bool,
}

impl ProjectViewCapabilities {
    pub fn is_android_view_visible(self) -> bool {
        self.is_android_project && self.supports_android_view
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdeIdentity {
    AndroidStudio,
    GameTools,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultView {
    Android,
    Project,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectViewEvent {
    DefaultViewChanged(DefaultView),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectViewPreferences {
    default_to_project_view: bool,
    show_visibility_icons: bool,
}

impl ProjectViewPreferences {
    pub fn default_to_project_view(&self) -> bool {
        self.default_to_project_view
    }

    /// Returns the one local event to publish when the value actually changes.
    pub fn set_default_to_project_view(&mut self, value: bool) -> Option<ProjectViewEvent> {
        if self.default_to_project_view == value {
            return None;
        }
        self.default_to_project_view = value;
        Some(ProjectViewEvent::DefaultViewChanged(if value {
            DefaultView::Project
        } else {
            DefaultView::Android
        }))
    }

    pub fn show_visibility_icons(&self) -> bool {
        self.show_visibility_icons
    }

    pub fn set_show_visibility_icons(&mut self, value: bool) {
        self.show_visibility_icons = value;
    }

    pub fn is_default_to_project_view_visible(show_default_project_view_settings: bool) -> bool {
        show_default_project_view_settings
    }

    pub fn is_default_to_project_view_enabled(show_default_project_view_settings: bool) -> bool {
        show_default_project_view_settings
    }

    pub fn is_project_view_default(
        &self,
        show_default_project_view_settings: bool,
        legacy: &LegacyProjectViewConfiguration<'_>,
    ) -> bool {
        if show_default_project_view_settings {
            self.default_to_project_view
        } else {
            legacy.is_project_view_property_true()
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let contents = match fs::read(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).with_context(|| format!("Read {}", path.display())),
        };
        serde_json::from_slice(&contents).with_context(|| {
            format!(
                "Parse Android project-view preferences in {}",
                path.display()
            )
        })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let contents = serde_json::to_vec_pretty(self)?;
        write_configuration(path, &contents)
    }

    /// The caller must persist changed preferences before treating the result
    /// as durable. IntelliJ normally supplies that application settings service.
    pub fn is_default_android_pane(
        &mut self,
        capabilities: ProjectViewCapabilities,
        identity: IdeIdentity,
        show_default_project_view_settings: bool,
        legacy: &mut LegacyProjectViewConfiguration<'_>,
    ) -> DefaultPaneDecision {
        if !capabilities.is_android_view_visible() || identity == IdeIdentity::Other {
            return DefaultPaneDecision::default();
        }
        let mut decision = DefaultPaneDecision::default();
        if show_default_project_view_settings && legacy.is_project_view_property_true() {
            decision.event = self.set_default_to_project_view(true);
            legacy.project_view_property = None;
            let (note, warnings) = remove_legacy_property(legacy);
            decision.notification = Some(MigrationNotification {
                title: "Default Project View Setting Updated".into(),
                message: format!(
                    "'Set Project view as the default' advanced setting was enabled due to the custom property `studio.projectview=true`. {note}"
                ),
                kind: NotificationKind::Information,
            });
            decision.warnings = warnings;
        }
        decision.is_default =
            !self.is_project_view_default(show_default_project_view_settings, legacy);
        decision
    }
}

#[derive(Clone, Debug, Default)]
pub struct LegacyProjectViewConfiguration<'a> {
    /// The launcher-resolved JVM property, including environment/command-line
    /// overrides. Migration clears this value rather than changing process env.
    pub project_view_property: Option<String>,
    pub custom_properties_file: Option<&'a Path>,
    pub platform_vm_options_file: Option<&'a Path>,
    pub custom_vm_options_file: Option<&'a Path>,
}

impl LegacyProjectViewConfiguration<'_> {
    pub fn is_project_view_property_true(&self) -> bool {
        self.project_view_property
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationKind {
    Information,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationNotification {
    pub title: String,
    pub message: String,
    pub kind: NotificationKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationWarning {
    pub path: std::path::PathBuf,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DefaultPaneDecision {
    pub is_default: bool,
    pub event: Option<ProjectViewEvent>,
    pub notification: Option<MigrationNotification>,
    pub warnings: Vec<MigrationWarning>,
}

fn remove_legacy_property(
    legacy: &LegacyProjectViewConfiguration<'_>,
) -> (String, Vec<MigrationWarning>) {
    let mut warnings = Vec::new();
    if let Some(path) = legacy.custom_properties_file {
        match remove_properties_entry(path) {
            Ok(true) => {
                return (
                    format!("This property has been removed from {}", path.display()),
                    warnings,
                );
            }
            Ok(false) => {}
            Err(error) => warnings.push(MigrationWarning {
                path: path.into(),
                message: error.to_string(),
            }),
        }
    }
    match remove_vm_options_entry(legacy, &mut warnings) {
        Ok(true) => {
            return (
                "This property has been removed from custom VM options.".into(),
                warnings,
            );
        }
        Ok(false) => {}
        Err(error) => {
            if let Some(path) = legacy.custom_vm_options_file {
                warnings.push(MigrationWarning {
                    path: path.into(),
                    message: error.to_string(),
                });
            }
        }
    }
    (
        "We recommend removing this property and using 'Advanced Settings -> Project View -> Set Project view as the default` to configure the default project view.".into(),
        warnings,
    )
}

fn remove_properties_entry(path: &Path) -> Result<bool> {
    let contents = match fs::read(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("Read {}", path.display())),
    };
    let mut removed = Vec::new();
    let mut lines = natural_lines(&contents);
    while let Some((range, content_end)) = lines.next() {
        let first = &contents[range.start..content_end];
        let first = trim_properties_whitespace(first);
        if first.is_empty() || matches!(first.first(), Some(b'#' | b'!')) {
            continue;
        }
        let start = range.start;
        let mut end = range.end;
        let mut logical = first.to_vec();
        let mut continued = trailing_backslashes(first) % 2 == 1;
        // java.util.Properties joins continued physical lines before decoding
        // escaped keys. Remove the whole entry without rewriting other bytes.
        while continued {
            logical.pop();
            let Some((range, content_end)) = lines.next() else {
                break;
            };
            let continuation = trim_properties_whitespace(&contents[range.start..content_end]);
            // Count only this physical line. Rescanning the accumulated entry
            // makes a long series of continued backslashes quadratic.
            continued = trailing_backslashes(continuation) % 2 == 1;
            logical.extend_from_slice(continuation);
            end = range.end;
        }
        if properties_key(&logical)?
            .into_iter()
            .eq(PROJECT_VIEW_DEFAULT_KEY.encode_utf16())
        {
            removed.push(start..end);
        }
    }
    if removed.is_empty() {
        return Ok(false);
    }
    write_configuration(path, &without_ranges(&contents, &removed))?;
    Ok(true)
}

fn trim_properties_whitespace(bytes: &[u8]) -> &[u8] {
    let first = bytes
        .iter()
        .position(|byte| !matches!(byte, b' ' | b'\t' | b'\x0c'))
        .unwrap_or(bytes.len());
    &bytes[first..]
}

fn trailing_backslashes(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count()
}

fn properties_key(logical: &[u8]) -> Result<Vec<u16>> {
    let mut key = Vec::new();
    let mut bytes = logical.iter().copied();
    while let Some(byte) = bytes.next() {
        if matches!(byte, b'=' | b':' | b' ' | b'\t' | b'\x0c') {
            break;
        }
        if byte != b'\\' {
            key.push(u16::from(byte));
            continue;
        }
        let Some(escaped) = bytes.next() else {
            break;
        };
        key.push(match escaped {
            b'u' => {
                let mut character = 0;
                for _ in 0..4 {
                    let digit = bytes
                        .next()
                        .context("Incomplete Unicode escape in custom properties")?;
                    let value = char::from(digit)
                        .to_digit(16)
                        .context("Invalid Unicode escape in custom properties")?;
                    character = character * 16 + value as u16;
                }
                character
            }
            b't' => u16::from(b'\t'),
            b'r' => u16::from(b'\r'),
            b'n' => u16::from(b'\n'),
            b'f' => u16::from(b'\x0c'),
            other => u16::from(other),
        });
    }
    Ok(key)
}

fn remove_vm_options_entry(
    legacy: &LegacyProjectViewConfiguration<'_>,
    warnings: &mut Vec<MigrationWarning>,
) -> Result<bool> {
    let mut last_value = None;
    let mut custom_contents = None;
    let prefix = format!("-D{PROJECT_VIEW_DEFAULT_KEY}=");
    for (path, is_custom) in [
        (legacy.platform_vm_options_file, false),
        (legacy.custom_vm_options_file, true),
    ] {
        let Some(path) = path else { continue };
        let contents = match fs::read(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                // VMOptions continues with the other options file if one read
                // fails; expose that failure locally without blocking migration.
                warnings.push(MigrationWarning {
                    path: path.into(),
                    message: format!("Read {}: {error}", path.display()),
                });
                continue;
            }
        };
        for (range, content_end) in natural_lines(&contents) {
            let line = java_trim(&contents[range.start..content_end]);
            if let Some(value) = line.strip_prefix(prefix.as_bytes()) {
                last_value = Some(value.to_vec());
            }
        }
        if is_custom {
            custom_contents = Some(contents);
        }
    }
    // VMOptions.readOption uses the last entry, with a case-sensitive "true"
    // comparison, even though Boolean.getBoolean is case-insensitive.
    if last_value.as_deref() != Some(b"true".as_slice()) {
        return Ok(false);
    }
    let path = legacy
        .custom_vm_options_file
        .context("No custom VM options file is configured")?;
    let Some(contents) = custom_contents else {
        return Ok(false);
    };
    let removed = natural_lines(&contents)
        .filter_map(|(range, content_end)| {
            java_trim(&contents[range.start..content_end])
                .starts_with(prefix.as_bytes())
                .then_some(range)
        })
        .collect::<Vec<_>>();
    if removed.is_empty() {
        return Ok(false);
    }
    write_configuration(path, &without_ranges(&contents, &removed))?;
    Ok(true)
}

fn java_trim(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| *byte > b' ')
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| *byte > b' ')
        .map_or(start, |offset| offset + 1);
    &bytes[start..end]
}

fn natural_lines(contents: &[u8]) -> impl Iterator<Item = (Range<usize>, usize)> + '_ {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start >= contents.len() {
            return None;
        }
        let content_end = contents[start..]
            .iter()
            .position(|byte| matches!(byte, b'\r' | b'\n'))
            .map_or(contents.len(), |offset| start + offset);
        let mut end = content_end;
        if contents.get(end) == Some(&b'\r') {
            end += 1;
        }
        if contents.get(end) == Some(&b'\n') {
            end += 1;
        }
        let range = start..end;
        start = end;
        Some((range, content_end))
    })
}

fn without_ranges(contents: &[u8], removed: &[Range<usize>]) -> Vec<u8> {
    let mut result = Vec::with_capacity(contents.len());
    let mut copied = 0;
    for range in removed {
        result.extend_from_slice(&contents[copied..range.start]);
        copied = range.end;
    }
    result.extend_from_slice(&contents[copied..]);
    result
}

fn write_configuration(path: &Path, contents: &[u8]) -> Result<()> {
    // Replacing a symlink would leave its actual configuration unchanged.
    let target = if path.is_symlink() {
        fs::canonicalize(path).with_context(|| format!("Resolve {}", path.display()))?
    } else {
        path.into()
    };
    let parent = target
        .parent()
        .context("Configuration file has no parent")?;
    ensure!(
        !parent.as_os_str().is_empty(),
        "Configuration path must include its parent"
    );
    let metadata = match fs::metadata(&target) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file(),
                "Configuration is not a regular file: {}",
                target.display()
            );
            Some(metadata)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).with_context(|| format!("Inspect {}", target.display())),
    };
    if let Some(metadata) = &metadata
        && metadata.permissions().readonly()
    {
        bail!("Configuration is read-only: {}", target.display());
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(contents)?;
    if let Some(metadata) = metadata {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    temporary.as_file().sync_all()?;
    temporary
        .persist(&target)
        .with_context(|| format!("Write {}", target.display()))?;
    Ok(())
}
