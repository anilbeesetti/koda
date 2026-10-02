/// Detects Android source modules using scanned project files, without executing Gradle.
pub(crate) fn is_android_gradle_project<'a>(
    mut files: impl Iterator<Item = &'a str>,
    has_file: impl Fn(&str) -> bool,
) -> bool {
    if ![
        "settings.gradle",
        "settings.gradle.kts",
        "build.gradle",
        "build.gradle.kts",
    ]
    .into_iter()
    .any(&has_file)
    {
        return false;
    }
    // Gradle wrappers also occur in JVM projects. Require a source manifest
    // belonging to a Gradle module, excluding generated and cached output.
    files.any(|path| {
        if !path.ends_with("/AndroidManifest.xml")
            || path
                .split('/')
                .any(|part| matches!(part, "build" | ".gradle" | ".zed" | ".git"))
        {
            return false;
        }
        let module_and_source = path
            .split_once("/src/")
            .or_else(|| path.strip_prefix("src/").map(|source| ("", source)));
        module_and_source.is_some_and(|(module, source)| {
            // Manifests in resources or test fixtures don't identify a source set.
            if source.split('/').count() != 2 {
                return false;
            }
            ["build.gradle", "build.gradle.kts"]
                .into_iter()
                .any(|name| {
                    has_file(&if module.is_empty() {
                        name.to_owned()
                    } else {
                        format!("{module}/{name}")
                    })
                })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(files: &[&str]) -> bool {
        is_android_gradle_project(files.iter().copied(), |path| files.contains(&path))
    }

    #[test]
    fn recognizes_android_app_library_and_multiplatform_modules() {
        for files in [
            vec![
                "gradlew",
                "settings.gradle.kts",
                "mobile/build.gradle.kts",
                "mobile/src/main/AndroidManifest.xml",
            ],
            vec![
                "settings.gradle",
                "library/build.gradle",
                "library/src/main/AndroidManifest.xml",
            ],
            vec!["build.gradle.kts", "src/androidMain/AndroidManifest.xml"],
            vec!["build.gradle", "src/demo/AndroidManifest.xml"],
        ] {
            assert!(detect(&files), "{files:?}");
        }
    }

    #[test]
    fn rejects_empty_non_android_and_generated_projects() {
        for files in [
            vec![],
            vec![
                "Cargo.toml",
                "examples/android/build.gradle.kts",
                "examples/android/src/main/AndroidManifest.xml",
            ],
            vec![
                "gradlew",
                "settings.gradle.kts",
                "app/build.gradle.kts",
                "app/src/main/Main.kt",
            ],
            vec![
                "build.gradle",
                "build/generated/src/main/AndroidManifest.xml",
            ],
            vec!["build.gradle", ".gradle/cache/src/main/AndroidManifest.xml"],
            vec!["settings.gradle", "fixtures/src/main/AndroidManifest.xml"],
            vec!["build.gradle", "AndroidManifest.xml"],
            vec!["build.gradle", "src/test/resources/AndroidManifest.xml"],
        ] {
            assert!(!detect(&files), "{files:?}");
        }
    }
}
