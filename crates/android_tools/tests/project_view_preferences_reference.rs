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

use android_tools::project_view_preferences::{
    DefaultPaneDecision, DefaultView, IdeIdentity, LegacyProjectViewConfiguration,
    MigrationNotification, NotificationKind, PROJECT_VIEW_DEFAULT_KEY, ProjectViewCapabilities,
    ProjectViewEvent, ProjectViewPreferences,
};
use anyhow::{Context as _, Result};
use std::fs;

const ANDROID: ProjectViewCapabilities = ProjectViewCapabilities {
    is_android_project: true,
    supports_android_view: true,
};

fn default_pane(
    preferences: &mut ProjectViewPreferences,
    identity: IdeIdentity,
    legacy: &mut LegacyProjectViewConfiguration<'_>,
) -> DefaultPaneDecision {
    preferences.is_default_android_pane(ANDROID, identity, true, legacy)
}

fn notification(note: &str) -> MigrationNotification {
    MigrationNotification {
        title: "Default Project View Setting Updated".into(),
        message: format!(
            "'Set Project view as the default' advanced setting was enabled due to the custom property `studio.projectview=true`. {note}"
        ),
        kind: NotificationKind::Information,
    }
}

// Each named test below preserves the assertions from AndroidProjectViewTest.
// Explicit capabilities replace IntelliJ project services, mutable configuration
// replaces JVM system properties, and returned events replace UsageTracker.
#[test]
fn test_show_visibility_icons_when_option_is_selected() {
    let mut preferences = ProjectViewPreferences::default();
    preferences.set_show_visibility_icons(true);
    assert!(preferences.show_visibility_icons());
}

#[test]
fn test_show_visibility_icons_when_option_is_unselected() {
    let mut preferences = ProjectViewPreferences::default();
    preferences.set_show_visibility_icons(false);
    assert!(!preferences.show_visibility_icons());
}

#[test]
fn test_android_view_is_default() {
    let mut preferences = ProjectViewPreferences::default();
    let mut legacy = LegacyProjectViewConfiguration::default();

    assert!(ANDROID.is_android_view_visible());
    assert!(!ProjectViewPreferences::is_default_to_project_view_visible(
        false
    ));
    assert!(ProjectViewPreferences::is_default_to_project_view_visible(
        true
    ));
    assert!(!default_pane(&mut preferences, IdeIdentity::Other, &mut legacy).is_default);
    assert!(default_pane(&mut preferences, IdeIdentity::AndroidStudio, &mut legacy).is_default);
    assert!(default_pane(&mut preferences, IdeIdentity::GameTools, &mut legacy).is_default);

    legacy.project_view_property = Some("true".into());
    assert!(ProjectViewPreferences::is_default_to_project_view_enabled(
        true
    ));
    assert!(!default_pane(&mut preferences, IdeIdentity::Other, &mut legacy).is_default);
    assert!(!default_pane(&mut preferences, IdeIdentity::AndroidStudio, &mut legacy).is_default);
    assert!(!default_pane(&mut preferences, IdeIdentity::GameTools, &mut legacy).is_default);

    preferences.set_default_to_project_view(true);
    legacy.project_view_property = Some("false".into());
    assert!(ProjectViewPreferences::is_default_to_project_view_enabled(
        true
    ));
    assert!(!default_pane(&mut preferences, IdeIdentity::Other, &mut legacy).is_default);
    assert!(!default_pane(&mut preferences, IdeIdentity::AndroidStudio, &mut legacy).is_default);
    assert!(!default_pane(&mut preferences, IdeIdentity::GameTools, &mut legacy).is_default);
}

#[test]
fn test_android_view_not_visible_in_unsupported_project_system() {
    let capabilities = ProjectViewCapabilities {
        is_android_project: true,
        supports_android_view: false,
    };
    let mut preferences = ProjectViewPreferences::default();
    let mut legacy = LegacyProjectViewConfiguration::default();
    assert!(!capabilities.is_android_view_visible());
    assert!(
        !preferences
            .is_default_android_pane(capabilities, IdeIdentity::AndroidStudio, true, &mut legacy)
            .is_default
    );
}

#[test]
fn test_android_view_not_visible_in_non_android_project() {
    let capabilities = ProjectViewCapabilities {
        is_android_project: false,
        supports_android_view: true,
    };
    let mut preferences = ProjectViewPreferences::default();
    let mut legacy = LegacyProjectViewConfiguration::default();
    assert!(!capabilities.is_android_view_visible());
    assert!(
        !preferences
            .is_default_android_pane(capabilities, IdeIdentity::AndroidStudio, true, &mut legacy)
            .is_default
    );
}

#[test]
fn test_android_view_is_default_custom_property_handling() {
    let mut preferences = ProjectViewPreferences::default();
    let mut legacy = LegacyProjectViewConfiguration {
        project_view_property: Some("true".into()),
        ..Default::default()
    };
    let first = default_pane(&mut preferences, IdeIdentity::AndroidStudio, &mut legacy);
    assert!(!first.is_default);
    assert!(ProjectViewPreferences::is_default_to_project_view_enabled(
        true
    ));
    assert!(preferences.is_project_view_default(true, &legacy));
    assert!(!legacy.is_project_view_property_true());

    let second = default_pane(&mut preferences, IdeIdentity::AndroidStudio, &mut legacy);
    let notifications = [first.notification, second.notification]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(
        notifications,
        vec![notification(
            "We recommend removing this property and using 'Advanced Settings -> Project View -> Set Project view as the default` to configure the default project view."
        )]
    );
}

#[test]
fn test_android_view_is_default_custom_property_handling_with_custom_properties_file() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("idea.properties");
    fs::write(&path, format!("{PROJECT_VIEW_DEFAULT_KEY}=true"))?;
    let mut preferences = ProjectViewPreferences::default();
    let mut legacy = LegacyProjectViewConfiguration {
        project_view_property: Some("true".into()),
        custom_properties_file: Some(&path),
        ..Default::default()
    };
    let decision = default_pane(&mut preferences, IdeIdentity::AndroidStudio, &mut legacy);
    assert!(!decision.is_default);
    assert!(ProjectViewPreferences::is_default_to_project_view_enabled(
        true
    ));
    assert!(preferences.is_project_view_default(true, &legacy));
    assert!(!legacy.is_project_view_property_true());
    assert_eq!(
        decision.notification,
        Some(notification(&format!(
            "This property has been removed from {}",
            path.display()
        )))
    );
    assert!(!fs::read_to_string(&path)?.contains(&format!("{PROJECT_VIEW_DEFAULT_KEY}=true")));
    Ok(())
}

#[test]
fn test_android_view_is_default_custom_property_handling_with_custom_vm_properties_file()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("studio64.vmoptions");
    fs::write(&path, format!("-D{PROJECT_VIEW_DEFAULT_KEY}=true"))?;
    let mut preferences = ProjectViewPreferences::default();
    let mut legacy = LegacyProjectViewConfiguration {
        project_view_property: Some("true".into()),
        custom_vm_options_file: Some(&path),
        ..Default::default()
    };
    let decision = default_pane(&mut preferences, IdeIdentity::AndroidStudio, &mut legacy);
    assert!(!decision.is_default);
    assert!(ProjectViewPreferences::is_default_to_project_view_enabled(
        true
    ));
    assert!(preferences.is_project_view_default(true, &legacy));
    assert!(!legacy.is_project_view_property_true());
    assert_eq!(
        decision.notification,
        Some(notification(
            "This property has been removed from custom VM options."
        ))
    );
    assert!(!fs::read_to_string(&path)?.contains(&format!("{PROJECT_VIEW_DEFAULT_KEY}=true")));
    Ok(())
}

#[test]
fn test_android_view_is_default_metrics() -> Result<()> {
    let mut preferences = ProjectViewPreferences::default();
    let mut legacy = LegacyProjectViewConfiguration {
        project_view_property: Some("false".into()),
        ..Default::default()
    };
    preferences.set_default_to_project_view(true);
    let mut events = Vec::new();
    events.extend(preferences.set_default_to_project_view(false));
    assert!(ProjectViewPreferences::is_default_to_project_view_enabled(
        true
    ));
    assert!(default_pane(&mut preferences, IdeIdentity::AndroidStudio, &mut legacy).is_default);

    events.extend(preferences.set_default_to_project_view(true));
    assert!(ProjectViewPreferences::is_default_to_project_view_enabled(
        true
    ));
    assert!(!default_pane(&mut preferences, IdeIdentity::AndroidStudio, &mut legacy).is_default);
    events.extend(preferences.set_default_to_project_view(true));
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events.first().context("Missing first event")?,
        ProjectViewEvent::DefaultViewChanged(_)
    ));
    assert_eq!(
        events.first().context("Missing first event")?,
        &ProjectViewEvent::DefaultViewChanged(DefaultView::Android)
    );
    assert!(matches!(
        events.get(1).context("Missing second event")?,
        ProjectViewEvent::DefaultViewChanged(_)
    ));
    assert_eq!(
        events.get(1).context("Missing second event")?,
        &ProjectViewEvent::DefaultViewChanged(DefaultView::Project)
    );
    Ok(())
}

#[test]
fn legacy_flag_preserves_property_fallback_without_migration() {
    let mut preferences = ProjectViewPreferences::default();
    preferences.set_default_to_project_view(true);
    let mut legacy = LegacyProjectViewConfiguration {
        project_view_property: Some("false".into()),
        ..Default::default()
    };
    assert!(!ProjectViewPreferences::is_default_to_project_view_enabled(
        false
    ));
    assert!(
        preferences
            .is_default_android_pane(ANDROID, IdeIdentity::AndroidStudio, false, &mut legacy)
            .is_default
    );
    legacy.project_view_property = Some("TRUE".into());
    let decision =
        preferences.is_default_android_pane(ANDROID, IdeIdentity::GameTools, false, &mut legacy);
    assert!(!decision.is_default);
    assert!(decision.notification.is_none());
    assert!(legacy.is_project_view_property_true());
}

#[test]
fn unsupported_and_other_identities_do_not_migrate() {
    for capabilities in [
        ANDROID,
        ProjectViewCapabilities {
            is_android_project: false,
            ..ANDROID
        },
        ProjectViewCapabilities {
            supports_android_view: false,
            ..ANDROID
        },
    ] {
        let mut preferences = ProjectViewPreferences::default();
        let mut legacy = LegacyProjectViewConfiguration {
            project_view_property: Some("true".into()),
            ..Default::default()
        };
        let identity = if capabilities == ANDROID {
            IdeIdentity::Other
        } else {
            IdeIdentity::AndroidStudio
        };
        let decision =
            preferences.is_default_android_pane(capabilities, identity, true, &mut legacy);
        assert_eq!(decision, DefaultPaneDecision::default());
        assert!(!preferences.default_to_project_view());
        assert!(legacy.is_project_view_property_true());
    }
}

#[test]
fn properties_migration_preserves_unrelated_bytes_and_removes_duplicate_escaped_keys() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("idea.properties");
    let retained = b"# comment ending in slash\\\r\n!studio.projectview=true\r\nother=caf\xe9\r\nother=second\r\nfoo=continued\\\r\n studio.projectview=true\r\nstudio.projectview\\:other=true\r\n\r\n";
    let mut contents = retained.to_vec();
    contents.extend_from_slice(b"studio.projectview=true\r\nstudio.project\\u0076iew:false\r\nstudio.proj\\\r\n ectview = true\r\n");
    fs::write(&path, contents)?;
    let mut preferences = ProjectViewPreferences::default();
    let mut legacy = LegacyProjectViewConfiguration {
        project_view_property: Some("true".into()),
        custom_properties_file: Some(&path),
        ..Default::default()
    };
    let decision = default_pane(&mut preferences, IdeIdentity::AndroidStudio, &mut legacy);
    assert!(decision.warnings.is_empty());
    assert_eq!(fs::read(&path)?, retained);
    assert!(
        default_pane(&mut preferences, IdeIdentity::AndroidStudio, &mut legacy)
            .notification
            .is_none()
    );
    Ok(())
}

#[test]
fn properties_migration_precedes_vm_options_and_preserves_file_permissions() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let properties = directory.path().join("idea.properties");
    let options = directory.path().join("studio64.vmoptions");
    fs::write(&properties, b"studio.projectview=false\nother=unchanged\n")?;
    fs::write(&options, b"-Dstudio.projectview=true\n-Xmx2g\n")?;
    let permissions = fs::metadata(&properties)?.permissions();
    let mut preferences = ProjectViewPreferences::default();
    let mut legacy = LegacyProjectViewConfiguration {
        project_view_property: Some("true".into()),
        custom_properties_file: Some(&properties),
        custom_vm_options_file: Some(&options),
        ..Default::default()
    };
    let decision = default_pane(&mut preferences, IdeIdentity::AndroidStudio, &mut legacy);
    assert_eq!(
        decision.notification,
        Some(notification(&format!(
            "This property has been removed from {}",
            properties.display()
        )))
    );
    assert_eq!(fs::read(&properties)?, b"other=unchanged\n");
    assert_eq!(fs::read(&options)?, b"-Dstudio.projectview=true\n-Xmx2g\n");
    assert_eq!(fs::metadata(&properties)?.permissions(), permissions);
    Ok(())
}

#[test]
fn vm_options_use_last_value_and_remove_all_entries_only_when_exactly_true() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("studio64.vmoptions");
    for last in ["false", "TRUE", "true"] {
        let contents = format!(
            "# -Dstudio.projectview=true\r\n-Xmx2g\r\n -Dstudio.projectview=true\r\n-Dstudio.projectview={last}\r\n-Dstudio.projectview.other=true\r\n"
        );
        fs::write(&path, &contents)?;
        let mut preferences = ProjectViewPreferences::default();
        let mut legacy = LegacyProjectViewConfiguration {
            project_view_property: Some("TRUE".into()),
            custom_vm_options_file: Some(&path),
            ..Default::default()
        };
        let decision = default_pane(&mut preferences, IdeIdentity::AndroidStudio, &mut legacy);
        assert!(!decision.is_default);
        if last == "true" {
            assert_eq!(
                fs::read(&path)?,
                b"# -Dstudio.projectview=true\r\n-Xmx2g\r\n-Dstudio.projectview.other=true\r\n"
            );
            assert_eq!(
                decision.notification,
                Some(notification(
                    "This property has been removed from custom VM options."
                ))
            );
        } else {
            assert_eq!(fs::read_to_string(&path)?, contents);
            assert!(
                decision
                    .notification
                    .context("Missing migration notification")?
                    .message
                    .contains("We recommend removing this property")
            );
        }
    }
    Ok(())
}

#[test]
fn unavailable_configuration_migrates_once_with_manual_removal_guidance() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut preferences = ProjectViewPreferences::default();
    let mut legacy = LegacyProjectViewConfiguration {
        project_view_property: Some("true".into()),
        custom_properties_file: Some(directory.path()),
        ..Default::default()
    };
    let first = default_pane(&mut preferences, IdeIdentity::AndroidStudio, &mut legacy);
    assert!(!first.is_default);
    assert_eq!(first.warnings.len(), 1);
    assert!(
        first
            .notification
            .context("Missing migration notification")?
            .message
            .contains("We recommend removing this property")
    );
    assert_eq!(
        first.event,
        Some(ProjectViewEvent::DefaultViewChanged(DefaultView::Project))
    );
    let second = default_pane(&mut preferences, IdeIdentity::AndroidStudio, &mut legacy);
    assert!(second.notification.is_none());
    assert!(second.event.is_none());
    Ok(())
}

#[test]
fn preferences_round_trip_without_emitting_load_events() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("android-project-view.json");
    let mut preferences = ProjectViewPreferences::load(&path)?;
    assert_eq!(preferences, ProjectViewPreferences::default());
    preferences.set_default_to_project_view(true);
    preferences.set_show_visibility_icons(true);
    preferences.save(&path)?;
    let mut restored = ProjectViewPreferences::load(&path)?;
    assert_eq!(restored, preferences);
    assert_eq!(restored.set_default_to_project_view(true), None);
    fs::write(&path, b"{broken")?;
    assert!(ProjectViewPreferences::load(&path).is_err());
    fs::write(&path, b"{\"future_setting\":true}")?;
    assert!(ProjectViewPreferences::load(&path).is_err());
    Ok(())
}

#[test]
fn unreadable_platform_options_do_not_block_custom_options_migration() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let custom = directory.path().join("studio64.vmoptions");
    fs::write(&custom, b"-Dstudio.projectview=true\n-Xmx2g\n")?;
    let mut preferences = ProjectViewPreferences::default();
    let mut legacy = LegacyProjectViewConfiguration {
        project_view_property: Some("true".into()),
        platform_vm_options_file: Some(directory.path()),
        custom_vm_options_file: Some(&custom),
        ..Default::default()
    };
    let decision = default_pane(&mut preferences, IdeIdentity::AndroidStudio, &mut legacy);
    assert_eq!(fs::read(&custom)?, b"-Xmx2g\n");
    assert_eq!(decision.warnings.len(), 1);
    assert_eq!(
        decision.warnings.first().context("Missing warning")?.path,
        directory.path()
    );
    assert_eq!(
        decision.notification,
        Some(notification(
            "This property has been removed from custom VM options."
        ))
    );
    Ok(())
}

#[test]
fn long_continued_property_migrates_without_changing_other_entries() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("idea.properties");
    let retained = b"# retained\nother=value\n";
    let mut contents = retained.to_vec();
    contents.extend_from_slice(b"studio.projectview=\\\n");
    for _ in 0..10_000 {
        contents.extend_from_slice(b"\\\\\\\n");
    }
    contents.extend_from_slice(b"true\n");
    fs::write(&path, contents)?;
    let mut preferences = ProjectViewPreferences::default();
    let mut legacy = LegacyProjectViewConfiguration {
        project_view_property: Some("true".into()),
        custom_properties_file: Some(&path),
        ..Default::default()
    };
    let decision = default_pane(&mut preferences, IdeIdentity::AndroidStudio, &mut legacy);
    assert!(decision.warnings.is_empty());
    assert_eq!(fs::read(&path)?, retained);
    Ok(())
}

#[cfg(unix)]
#[test]
fn migration_follows_symlinks_and_retains_read_only_configuration() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let target = directory.path().join("actual.properties");
    let link = directory.path().join("idea.properties");
    fs::write(&target, b"studio.projectview=true\nother=value\n")?;
    std::os::unix::fs::symlink(&target, &link)?;
    let mut preferences = ProjectViewPreferences::default();
    let mut legacy = LegacyProjectViewConfiguration {
        project_view_property: Some("true".into()),
        custom_properties_file: Some(&link),
        ..Default::default()
    };
    assert!(
        default_pane(&mut preferences, IdeIdentity::AndroidStudio, &mut legacy)
            .warnings
            .is_empty()
    );
    assert!(link.is_symlink());
    assert_eq!(fs::read(&target)?, b"other=value\n");
    fs::write(&target, b"studio.projectview=true\n")?;
    let mut permissions = fs::metadata(&target)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&target, permissions)?;
    legacy.project_view_property = Some("true".into());
    let decision = default_pane(&mut preferences, IdeIdentity::AndroidStudio, &mut legacy);
    assert_eq!(decision.warnings.len(), 1);
    assert_eq!(fs::read(&target)?, b"studio.projectview=true\n");
    assert!(
        decision
            .notification
            .context("Missing migration notification")?
            .message
            .contains("We recommend removing this property")
    );
    Ok(())
}
