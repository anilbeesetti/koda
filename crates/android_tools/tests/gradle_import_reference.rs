// Tests adapted from AOSP GradleModuleImportTest.java (Copyright 2014 AOSP,
// Apache-2.0), pinned at a84efec3ba9542d9bfa1255103f0dc94833a3796.
// Original suite, generated build template, helper implementations and license
// are retained unchanged in ../test_data/gradle_import.

use android_tools::gradle_import::{
    GradleImportPlan, discover_gradle_modules, import_gradle_modules,
};
use anyhow::{Context as _, Result};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

const MODULE_NAME: &str = "guadeloupe";
const SAMPLE_PROJECT_PATH: &str = "samples/sample1";
const BUILD_GRADLE_TEMPLATE: &str =
    include_str!("../test_data/gradle_import/build.gradle.template");

fn module(number: usize) -> String {
    format!("{MODULE_NAME}{number}")
}

fn gradle_name(path: &str) -> String {
    format!(":{}", path.replace('/', ":"))
}

fn create_project(directory: &Path, name: &str, dependencies: &[&str]) -> Result<PathBuf> {
    let module = directory.join(name);
    fs::create_dir_all(&module)?;
    let dependencies = dependencies
        .iter()
        .map(|dependency| format!("\timplementation project('{}')", gradle_name(dependency)))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        module.join("build.gradle"),
        BUILD_GRADLE_TEMPLATE.replace("%s", &dependencies),
    )?;
    Ok(module.canonicalize()?)
}

fn configure_root(directory: &Path, modules: &[&str], locations: &[String]) -> Result<PathBuf> {
    // Exact configureTopLevelProject output, including its second trailing newline.
    fs::write(
        directory.join("settings.gradle"),
        format!(
            "include '{}'\n{}\n",
            modules.join("', '"),
            locations.join("\n")
        ),
    )?;
    Ok(directory.canonicalize()?)
}

fn create_subprojects(
    directory: &Path,
    modules: &[(&str, &str)],
    missing: &[&str],
) -> Result<PathBuf> {
    let mut locations = Vec::new();
    let mut names = Vec::new();
    for (name, location) in modules {
        let default = name.trim_start_matches(':').replace(':', "/");
        let location = if location.is_empty() {
            default.as_str()
        } else {
            locations.push(format!(
                "project('{name}').projectDir = new File('{location}')"
            ));
            location
        };
        create_project(directory, location, &[])?;
        names.push(name.to_string());
    }
    names.extend(missing.iter().map(|name| gradle_name(name)));
    configure_root(
        directory,
        &names.iter().map(String::as_str).collect::<Vec<_>>(),
        &locations,
    )
}

fn assert_required_but_missing(plan: &GradleImportPlan, path: &str) {
    let name = gradle_name(path);
    assert!(plan.modules.contains_key(&name) && plan.modules.get(&name) == Some(&None));
}

fn assert_imported(
    destination: &Path,
    relative_path: &str,
    source: &Path,
    original: &[u8],
) -> Result<()> {
    // Every assertion from assertModuleImported/assertNoFilesAdded is retained.
    assert!(
        destination.join(relative_path).is_dir(),
        "Module sources were not copied"
    );
    let children = fs::read_dir(source)?.collect::<std::io::Result<Vec<_>>>()?;
    assert_eq!(
        children.len(),
        1,
        "Files were altered in the source directory"
    );
    assert_eq!(
        children
            .first()
            .context("Missing module child")?
            .file_name(),
        "build.gradle"
    );
    assert_eq!(fs::read(source.join("build.gradle"))?, original);
    assert!(
        destination.join("settings.gradle").is_file(),
        "Missing settings.gradle"
    );
    let registered = discover_gradle_modules(destination)?;
    assert!(registered.modules.contains_key(&gradle_name(relative_path)));
    Ok(())
}

#[test]
fn import_simple_gradle_project() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = create_project(directory.path(), MODULE_NAME, &[])?;
    let original = fs::read(source.join("build.gradle"))?;
    let plan = GradleImportPlan {
        modules: BTreeMap::from([(MODULE_NAME.into(), Some(source.clone()))]),
    };
    import_gradle_modules(&plan, directory.path())?;
    assert_imported(directory.path(), MODULE_NAME, &source, &original)
}

#[test]
fn import_subprojects() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let first = module(1);
    let second = module(2);
    let paths = [first.as_str(), second.as_str(), SAMPLE_PROJECT_PATH];
    let names = paths
        .iter()
        .map(|path| gradle_name(path))
        .collect::<Vec<_>>();
    let modules = names
        .iter()
        .map(|name| (name.as_str(), ""))
        .collect::<Vec<_>>();
    let root = create_subprojects(directory.path(), &modules, &[])?;
    let plan = discover_gradle_modules(&root)?;
    assert_eq!(plan.modules.len(), paths.len());
    let originals = paths
        .iter()
        .map(|path| fs::read(root.join(path).join("build.gradle")))
        .collect::<std::io::Result<Vec<_>>>()?;
    for path in paths {
        assert_eq!(
            plan.modules.get(&gradle_name(path)),
            Some(&Some(root.join(path)))
        );
    }
    import_gradle_modules(&plan, &root)?;
    for (path, original) in paths.into_iter().zip(originals) {
        let source = root.join(path);
        assert!(
            source.is_dir(),
            "Module was not imported into {}",
            source.display()
        );
        assert_imported(&root, path, &source, &original)?;
    }
    Ok(())
}

#[test]
fn import_subprojects_with_missing_submodule() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let first = module(1);
    let second = module(2);
    let root = create_subprojects(
        directory.path(),
        &[(gradle_name(&first).as_str(), "")],
        &[&second],
    )?;
    let settings = fs::read(root.join("settings.gradle"))?;
    let source = fs::read(root.join(&first).join("build.gradle"))?;
    let plan = discover_gradle_modules(&root)?;
    assert_eq!(plan.modules.len(), 2);
    assert_required_but_missing(&plan, &second);
    assert!(import_gradle_modules(&plan, &root).is_err());
    assert_eq!(fs::read(root.join("settings.gradle"))?, settings);
    assert_eq!(fs::read(root.join(first).join("build.gradle"))?, source);
    Ok(())
}

#[test]
fn import_subproject_with_custom_location() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let name = gradle_name(SAMPLE_PROJECT_PATH);
    let root = create_subprojects(
        directory.path(),
        &[(name.as_str(), SAMPLE_PROJECT_PATH)],
        &[],
    )?;
    let plan = discover_gradle_modules(&root)?;
    assert_eq!(plan.modules.len(), 1);
    let source = root.join(SAMPLE_PROJECT_PATH);
    assert!(source.is_dir());
    assert_eq!(plan.modules.get(&name), Some(&Some(source.clone())));
    let original = fs::read(source.join("build.gradle"))?;
    import_gradle_modules(&plan, &root)?;
    assert_imported(&root, SAMPLE_PROJECT_PATH, &source, &original)
}

#[test]
fn required_projects() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let first = module(1);
    let second = module(2);
    let project1 = create_project(directory.path(), &first, &[])?;
    let project2 = create_project(directory.path(), &second, &[&first])?;
    configure_root(directory.path(), &[&first, &second], &[])?;
    let plan = discover_gradle_modules(&project2)?;
    assert_eq!(plan.modules.len(), 2);
    assert_eq!(
        plan.modules.get(&gradle_name(&first)),
        Some(&Some(project1))
    );
    assert_eq!(
        plan.modules.get(&gradle_name(&second)),
        Some(&Some(project2))
    );
    Ok(())
}

#[test]
fn missing_required_projects() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let first = module(1);
    let second = module(2);
    let project2 = create_project(directory.path(), &second, &[&first])?;
    configure_root(directory.path(), &[&first, &second], &[])?;
    let plan = discover_gradle_modules(&project2)?;
    assert_eq!(plan.modules.len(), 2);
    assert_required_but_missing(&plan, &first);
    assert_eq!(
        plan.modules.get(&gradle_name(&second)),
        Some(&Some(project2))
    );
    Ok(())
}

#[test]
fn missing_enclosing_project() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let first = module(1);
    let second = module(2);
    let source = create_project(directory.path(), &first, &[&second])?;
    let plan = discover_gradle_modules(&source)?;
    assert_eq!(plan.modules.len(), 2);
    assert_required_but_missing(&plan, &second);
    assert_eq!(plan.modules.get(&gradle_name(&first)), Some(&Some(source)));
    Ok(())
}

#[test]
fn transitive_dependencies() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let first = module(1);
    let second = module(2);
    let third = module(3);
    let project1 = create_project(directory.path(), &first, &[])?;
    let project2 = create_project(directory.path(), &second, &[&first])?;
    let project3 = create_project(directory.path(), &third, &[&second])?;
    configure_root(directory.path(), &[&first, &second, &third], &[])?;
    let plan = discover_gradle_modules(&project3)?;
    assert_eq!(plan.modules.len(), 3);
    assert_eq!(
        plan.modules.get(&gradle_name(&first)),
        Some(&Some(project1))
    );
    assert_eq!(
        plan.modules.get(&gradle_name(&second)),
        Some(&Some(project2))
    );
    assert_eq!(
        plan.modules.get(&gradle_name(&third)),
        Some(&Some(project3))
    );
    Ok(())
}

#[test]
fn circular_dependencies() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let first = module(1);
    let second = module(2);
    let third = module(3);
    let project1 = create_project(directory.path(), &first, &[&third])?;
    let project2 = create_project(directory.path(), &second, &[&first])?;
    let project3 = create_project(directory.path(), &third, &[&second])?;
    configure_root(directory.path(), &[&first, &second, &third], &[])?;
    let plan = discover_gradle_modules(&project3)?;
    assert_eq!(plan.modules.len(), 3);
    assert_eq!(
        plan.modules.get(&gradle_name(&first)),
        Some(&Some(project1))
    );
    assert_eq!(
        plan.modules.get(&gradle_name(&second)),
        Some(&Some(project2))
    );
    assert_eq!(
        plan.modules.get(&gradle_name(&third)),
        Some(&Some(project3))
    );
    Ok(())
}

#[test]
fn external_sources_are_copied_and_settings_content_is_preserved() -> Result<()> {
    let source_root = tempfile::tempdir()?;
    let destination = tempfile::tempdir()?;
    let source = create_project(source_root.path(), SAMPLE_PROJECT_PATH, &[])?;
    fs::write(source.join("kept.txt"), b"source bytes\r\n")?;
    let original =
        "// unrelated comments\r\nrootProject.name = 'destination'\r\ninclude ':existing'\r\n";
    fs::write(destination.path().join("settings.gradle"), original)?;
    let plan = GradleImportPlan {
        modules: BTreeMap::from([(gradle_name(SAMPLE_PROJECT_PATH), Some(source.clone()))]),
    };
    let result = import_gradle_modules(&plan, destination.path())?;
    let target = destination
        .path()
        .join(SAMPLE_PROJECT_PATH)
        .canonicalize()?;
    assert_eq!(
        result.modules.get(&gradle_name(SAMPLE_PROJECT_PATH)),
        Some(&target)
    );
    assert_eq!(
        fs::read(source.join("build.gradle"))?,
        fs::read(target.join("build.gradle"))?
    );
    assert_eq!(
        fs::read(source.join("kept.txt"))?,
        fs::read(target.join("kept.txt"))?
    );
    assert_eq!(fs::read_dir(&source)?.count(), 2);
    assert!(fs::read_to_string(result.settings_file)?.starts_with(original));
    Ok(())
}

#[test]
fn true_custom_location_is_registered_without_moving_sources() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = create_project(directory.path(), "unrelated/physical location", &[])?;
    let plan = GradleImportPlan {
        modules: BTreeMap::from([(":logical:name".into(), Some(source.clone()))]),
    };
    import_gradle_modules(&plan, directory.path())?;
    assert!(!directory.path().join("logical/name").exists());
    assert_eq!(
        discover_gradle_modules(directory.path())?
            .modules
            .get(":logical:name"),
        Some(&Some(source))
    );
    Ok(())
}

#[test]
fn kotlin_literal_discovery_and_registration() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let first = create_project(directory.path(), "one", &[])?;
    let second = create_project(directory.path(), "physical-two", &[])?;
    fs::remove_file(second.join("build.gradle"))?;
    fs::write(
        second.join("build.gradle.kts"),
        "dependencies {\n implementation(project(path = \":one\"))\n}\n",
    )?;
    fs::write(
        directory.path().join("settings.gradle.kts"),
        "rootProject.name = \"test\"\ninclude(\n \":one\", \":two\"\n)\nproject(\":two\").projectDir = file(\"physical-two\")\n",
    )?;
    let plan = discover_gradle_modules(&second)?;
    assert_eq!(
        plan.modules,
        BTreeMap::from([(":one".into(), Some(first)), (":two".into(), Some(second))])
    );
    let third = create_project(directory.path(), "custom-three", &[])?;
    let plan = GradleImportPlan {
        modules: BTreeMap::from([(":three".into(), Some(third.clone()))]),
    };
    import_gradle_modules(&plan, directory.path())?;
    assert_eq!(
        discover_gradle_modules(directory.path())?
            .modules
            .get(":three"),
        Some(&Some(third))
    );
    assert!(!directory.path().join("settings.gradle").exists());
    Ok(())
}

#[test]
fn unsupported_settings_and_dependencies_do_not_modify_target() -> Result<()> {
    let source_root = tempfile::tempdir()?;
    let destination = tempfile::tempdir()?;
    let source = create_project(source_root.path(), MODULE_NAME, &[])?;
    let plan = GradleImportPlan {
        modules: BTreeMap::from([(MODULE_NAME.into(), Some(source.clone()))]),
    };
    for settings in [
        "include modules",
        "if (enabled) { include ':a' }",
        "include ':a' + suffix",
        "project(':a').projectDir = file(path)",
        "include(\":${module}\")",
        "apply from: 'settings-extra.gradle'",
    ] {
        fs::write(destination.path().join("settings.gradle"), settings)?;
        assert!(
            import_gradle_modules(&plan, destination.path()).is_err(),
            "{settings}"
        );
        assert_eq!(
            fs::read_to_string(destination.path().join("settings.gradle"))?,
            settings
        );
        assert_eq!(fs::read_dir(destination.path())?.count(), 1);
    }
    fs::remove_file(destination.path().join("settings.gradle"))?;
    for build in [
        "dependencies { implementation project(module) }",
        "dependencies { if (enabled) { implementation project(':a') } }",
        "subprojects { dependencies { implementation project(':a') } }",
        "apply from: 'dependency-script.gradle'",
        "dependencies { implementation libs.generated }",
        "if (false) dependencies { implementation project(':a') }",
        "while (false) dependencies { implementation project(':a') }",
        "project(':other').dependencies { implementation project(':a') }",
        "wrapper(dependencies { implementation project(':a') })",
        "wrapper(\n dependencies { implementation project(':a') }\n)",
        "project(':other').\n dependencies { implementation project(':a') }",
        "apply(\n from = \"dependency-script.gradle\"\n)",
        "apply {\n from 'dependency-script.gradle'\n}",
        "apply {\n from(\"dependency-script.gradle\")\n}",
        "false &&\n dependencies { implementation project(':a') }",
        "true ?\n dependencies { implementation project(':a') } : null",
        "return\n dependencies { implementation project(':a') }",
        "throw new RuntimeException()\n dependencies { implementation project(':a') }",
        "assert false\n dependencies { implementation project(':a') }",
        "System.exit(0)\n dependencies { implementation project(':a') }",
        "configureDependencies()\n dependencies { implementation project(':a') }",
    ] {
        fs::write(source.join("build.gradle"), build)?;
        assert!(
            import_gradle_modules(&plan, destination.path()).is_err(),
            "{build}"
        );
        assert_eq!(fs::read_dir(destination.path())?.count(), 0);
        assert_eq!(fs::read_to_string(source.join("build.gradle"))?, build);
    }
    Ok(())
}

#[test]
fn missing_sources_and_destination_conflicts_leave_target_unchanged() -> Result<()> {
    let source_root = tempfile::tempdir()?;
    let destination = tempfile::tempdir()?;
    let source = create_project(source_root.path(), MODULE_NAME, &[])?;
    let missing = GradleImportPlan {
        modules: BTreeMap::from([(":z".into(), None), (":a".into(), None)]),
    };
    assert_eq!(
        import_gradle_modules(&missing, destination.path())
            .expect_err("Missing sources must fail")
            .to_string(),
        "Sources were not found for modules ':a', ':z'"
    );
    assert_eq!(fs::read_dir(destination.path())?.count(), 0);
    fs::create_dir(destination.path().join(MODULE_NAME))?;
    fs::write(
        destination.path().join(MODULE_NAME).join("original.txt"),
        "keep",
    )?;
    let plan = GradleImportPlan {
        modules: BTreeMap::from([(MODULE_NAME.into(), Some(source))]),
    };
    assert!(import_gradle_modules(&plan, destination.path()).is_err());
    assert_eq!(
        fs::read_to_string(destination.path().join(MODULE_NAME).join("original.txt"))?,
        "keep"
    );
    assert!(!destination.path().join("settings.gradle").exists());
    Ok(())
}

#[test]
fn unsafe_or_overlapping_module_paths_are_rejected_before_copying() -> Result<()> {
    let source_root = tempfile::tempdir()?;
    let destination = tempfile::tempdir()?;
    let source = create_project(source_root.path(), "source", &[])?;
    for name in [":..:escape", ":a::b", ":a/b", ":a\\b", ":", "a\nb"] {
        let plan = GradleImportPlan {
            modules: BTreeMap::from([(name.into(), Some(source.clone()))]),
        };
        assert!(
            import_gradle_modules(&plan, destination.path()).is_err(),
            "{name}"
        );
        assert_eq!(fs::read_dir(destination.path())?.count(), 0);
    }
    let plan = GradleImportPlan {
        modules: BTreeMap::from([
            (":a".into(), Some(source.clone())),
            (":a:b".into(), Some(source)),
        ]),
    };
    assert!(import_gradle_modules(&plan, destination.path()).is_err());
    assert_eq!(fs::read_dir(destination.path())?.count(), 0);
    Ok(())
}

#[test]
fn destination_inside_source_is_rejected_without_recursive_copy() -> Result<()> {
    let source_root = tempfile::tempdir()?;
    let source = create_project(source_root.path(), "source", &[])?;
    let destination = source.join("destination");
    fs::create_dir(&destination)?;
    let plan = GradleImportPlan {
        modules: BTreeMap::from([(MODULE_NAME.into(), Some(source))]),
    };
    assert!(import_gradle_modules(&plan, &destination).is_err());
    assert_eq!(fs::read_dir(destination)?.count(), 0);
    Ok(())
}

#[test]
fn already_registered_module_is_not_remapped_to_unrelated_sources() -> Result<()> {
    let source_root = tempfile::tempdir()?;
    let destination = tempfile::tempdir()?;
    let source = create_project(source_root.path(), "source", &[])?;
    let original = "// keep registration\ninclude ':logical'\nproject(':logical').projectDir = file('unrelated')\n";
    fs::write(destination.path().join("settings.gradle"), original)?;
    let plan = GradleImportPlan {
        modules: BTreeMap::from([(":logical".into(), Some(source))]),
    };
    assert!(import_gradle_modules(&plan, destination.path()).is_err());
    assert_eq!(
        fs::read_to_string(destination.path().join("settings.gradle"))?,
        original
    );
    assert_eq!(fs::read_dir(destination.path())?.count(), 1);
    Ok(())
}

#[cfg(unix)]
#[test]
fn unix_custom_path_backslashes_are_preserved_and_control_characters_rejected() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = create_project(directory.path(), "physical\\name", &[])?;
    let plan = GradleImportPlan {
        modules: BTreeMap::from([(":logical".into(), Some(source.clone()))]),
    };
    import_gradle_modules(&plan, directory.path())?;
    assert_eq!(
        discover_gradle_modules(directory.path())?
            .modules
            .get(":logical"),
        Some(&Some(source))
    );
    let settings = fs::read(directory.path().join("settings.gradle"))?;
    let bad_source = create_project(directory.path(), "physical\nname", &[])?;
    let original = fs::read(bad_source.join("build.gradle"))?;
    let plan = GradleImportPlan {
        modules: BTreeMap::from([(":bad".into(), Some(bad_source.clone()))]),
    };
    assert!(import_gradle_modules(&plan, directory.path()).is_err());
    assert_eq!(
        fs::read(directory.path().join("settings.gradle"))?,
        settings
    );
    assert_eq!(fs::read(bad_source.join("build.gradle"))?, original);
    assert_eq!(fs::read_dir(bad_source)?.count(), 1);
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlink_sources_and_destination_parents_are_rejected_without_mutation() -> Result<()> {
    let source_root = tempfile::tempdir()?;
    let destination = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let source = create_project(source_root.path(), "source", &[])?;
    fs::write(outside.path().join("keep.txt"), "keep")?;
    std::os::unix::fs::symlink(outside.path().join("keep.txt"), source.join("link.txt"))?;
    let plan = GradleImportPlan {
        modules: BTreeMap::from([(MODULE_NAME.into(), Some(source.clone()))]),
    };
    assert!(import_gradle_modules(&plan, destination.path()).is_err());
    assert_eq!(fs::read_dir(destination.path())?.count(), 0);
    fs::remove_file(source.join("link.txt"))?;
    std::os::unix::fs::symlink(outside.path(), destination.path().join("parent"))?;
    let plan = GradleImportPlan {
        modules: BTreeMap::from([(":parent:module".into(), Some(source))]),
    };
    assert!(import_gradle_modules(&plan, destination.path()).is_err());
    assert_eq!(fs::read_dir(outside.path())?.count(), 1);
    assert_eq!(fs::read_to_string(outside.path().join("keep.txt"))?, "keep");
    assert!(!destination.path().join("settings.gradle").exists());
    Ok(())
}

#[test]
fn comments_are_not_dependencies_and_unterminated_input_is_rejected() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = create_project(directory.path(), "source", &[])?;
    fs::write(
        source.join("build.gradle"),
        "// dependencies { implementation project(':missing') }\n/* project(':also-missing') */\ndependencies {\n implementation 'group:artifact:1' // project(':comment')\n}\n",
    )?;
    assert_eq!(discover_gradle_modules(&source)?.modules.len(), 1);
    for build in [
        "/* unterminated",
        "dependencies { implementation project(':a')",
        "dependencies { implementation 'unterminated }",
    ] {
        fs::write(source.join("build.gradle"), build)?;
        assert!(discover_gradle_modules(&source).is_err());
    }
    Ok(())
}
