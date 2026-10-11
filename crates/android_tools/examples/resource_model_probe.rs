//! Resource-exporter probe migrated from the authored Python probe.
//!
//! The two JSON assets preserve its original Gradle fixture bytes. Gradle/JDK,
//! the Android SDK and Cargo remain external processes; all fixture, command,
//! metadata and predicate logic here is Rust. This is not an AOSP test port.

use anyhow::{Context as _, Result, bail, ensure};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

const MODEL_MARKER: &str = "KODA_ANDROID_PROJECT_MODEL=";
const MAX_LOG_LINE_BYTES: usize = 64 * 1024 * 1024;
const USAGE: &str =
    "Resource model probe: --gradle PATH --sdk PATH --agp 8.9.1|9.4.0 [--keep PATH]";

struct Options {
    gradle: OsString,
    sdk: PathBuf,
    agp: String,
    keep: Option<PathBuf>,
}

impl Options {
    fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Option<Self>> {
        let mut arguments = arguments.into_iter();
        let (mut gradle, mut sdk, mut agp, mut keep) = (None, None, None, None);
        while let Some(flag) = arguments.next() {
            if flag == "--help" || flag == "-h" {
                println!("{USAGE}");
                return Ok(None);
            }
            let flag = flag.to_str().context("Option must be UTF-8")?;
            ensure!(
                matches!(flag, "--gradle" | "--sdk" | "--agp" | "--keep"),
                "Unknown option {flag}; {USAGE}"
            );
            let value = arguments
                .next()
                .with_context(|| format!("Missing value for {flag}"))?;
            match flag {
                "--gradle" => gradle = Some(value),
                "--sdk" => sdk = Some(PathBuf::from(value)),
                "--agp" => {
                    agp = Some(
                        value
                            .into_string()
                            .map_err(|_| anyhow::anyhow!("AGP version must be UTF-8"))?,
                    )
                }
                "--keep" => keep = Some(PathBuf::from(value)),
                _ => unreachable!(),
            }
        }
        let agp = agp.context("--agp is required")?;
        ensure!(
            matches!(agp.as_str(), "8.9.1" | "9.4.0"),
            "Unsupported AGP {agp}; {USAGE}"
        );
        Ok(Some(Self {
            gradle: gradle.context("--gradle is required")?,
            sdk: sdk.context("--sdk is required")?,
            agp,
            keep,
        }))
    }
}

#[derive(Deserialize)]
struct Fixture {
    agp: String,
    files: BTreeMap<String, String>,
}

fn fixture(agp: &str) -> Result<Fixture> {
    let bytes = match agp {
        "8.9.1" => include_str!("../test_data/resource_model_probe/agp8.json"),
        "9.4.0" => include_str!("../test_data/resource_model_probe/agp9.json"),
        _ => bail!("Unsupported fixture AGP {agp}"),
    };
    let fixture: Fixture = serde_json::from_str(bytes)?;
    ensure!(fixture.agp == agp, "Fixture AGP identity mismatch");
    Ok(fixture)
}

fn write_fixture(root: &Path, agp: &str) -> Result<()> {
    for (relative, bytes) in fixture(agp)?.files {
        let relative = Path::new(&relative);
        ensure!(
            !relative.is_absolute()
                && relative
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))),
            "Fixture path must stay inside its root"
        );
        let path = root.join(relative);
        fs::create_dir_all(path.parent().context("Fixture parent")?)?;
        fs::write(path, bytes)?;
    }
    Ok(())
}

fn gradle_arguments(repository: &Path) -> Vec<OsString> {
    vec![
        "--console=plain".into(),
        "--no-configuration-cache".into(),
        "--parallel".into(),
        "--max-workers=2".into(),
        "--init-script".into(),
        repository
            .join("crates/android_tools/src/project_model.gradle")
            .into_os_string(),
        "kodaAndroidProjectModel".into(),
    ]
}

fn successful(command: &mut Command, description: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("Start {description}"))?;
    ensure!(status.success(), "{description} failed: {status}");
    Ok(())
}

fn model_from_log(reader: impl BufRead) -> Result<Value> {
    model_from_log_with_limit(reader, MAX_LOG_LINE_BYTES)
}

fn model_from_log_with_limit(mut reader: impl BufRead, limit: usize) -> Result<Value> {
    let mut line = Vec::new();
    let mut record = None;
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() && line.is_empty() {
            break;
        }
        let end = chunk
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(chunk.len(), |index| index + 1);
        let terminated = chunk.get(end.wrapping_sub(1)) == Some(&b'\n');
        let eof = chunk.is_empty();
        let length = line
            .len()
            .checked_add(end)
            .context("Gradle log line size overflow")?;
        ensure!(
            length <= limit,
            "Gradle log line exceeds the {limit}-byte probe limit"
        );
        line.try_reserve_exact(end)?;
        line.extend_from_slice(&chunk[..end]);
        reader.consume(end);
        if terminated || eof {
            let text = std::str::from_utf8(&line)
                .context("Gradle log must be UTF-8")?
                .trim_end_matches(['\r', '\n']);
            if let Some(json) = text.strip_prefix(MODEL_MARKER) {
                ensure!(
                    record.is_none(),
                    "Expected exactly one Android project model record"
                );
                record = Some(
                    serde_json::from_str(json).context("Decode Android project model record")?,
                );
            }
            line.clear();
        }
    }
    record.context("Expected exactly one Android project model record")
}

fn verify_resources(root: &Path, model: &Value) -> Result<()> {
    let app = model["modules"]
        .as_array()
        .context("Model modules")?
        .iter()
        .find(|module| module["path"] == ":app")
        .context("App module")?;
    let main = app["variants"]
        .as_array()
        .context("App variants")?
        .iter()
        .find(|variant| variant["name"] == "demoDebug")
        .context("demoDebug variant")?["components"]
        .as_array()
        .context("demoDebug components")?
        .iter()
        .find(|component| component["scope"] == "main")
        .context("Main component")?;
    let resources = &model["resourceModels"][":app/demoDebug"];
    let layers = resources["layers"].as_array().context("Resource layers")?;
    let mut ranks = BTreeMap::new();
    for (index, layer) in layers.iter().enumerate() {
        for path in layer.as_array().context("Resource layer")? {
            ranks
                .entry(path.as_str().context("Resource root path")?)
                .or_insert(index);
        }
    }
    let rank = |relative: &str| -> Result<usize> {
        let path = root.join("app").join(relative);
        ranks
            .get(path.to_str().context("Fixture root must be UTF-8")?)
            .copied()
            .with_context(|| format!("Missing resource layer {relative}"))
    };
    ensure!(
        rank("src/demoDebug/res")? < rank("src/debug/res")?
            && rank("src/debug/res")? < rank("custom/demo-res")?
            && rank("custom/demo-res")? < rank("custom/main-res")?,
        "P01: resource overlay layer ordering"
    );
    ensure!(
        rank("custom/main-res")? == rank("custom/equal-res")?,
        "P02: equal-priority custom roots"
    );
    let roots = main["sources"]
        .as_array()
        .context("Main sources")?
        .iter()
        .filter(|source| source["kind"] == "resources")
        .map(|source| Ok((source["path"].as_str().context("Source root path")?, source)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    for path in ranks.keys() {
        ensure!(
            roots.contains_key(path),
            "P03: every resource layer must be a main resource source"
        );
    }
    let variant_root = root.join("app/src/demoDebug/res");
    let variant = roots
        .get(
            variant_root
                .to_str()
                .context("Fixture root must be UTF-8")?,
        )
        .context("Variant resource root")?;
    ensure!(
        variant["generated"] == false,
        "P04: variant resource source is not generated"
    );
    ensure!(
        roots.values().any(|source| source["generated"] == true
            && source["path"]
                .as_str()
                .is_some_and(|path| path.contains("/generated/res/"))),
        "P05: generated resource source exists"
    );
    ensure!(
        resources["nonTransitiveR"] == true,
        "P06: non-transitive R metadata"
    );
    ensure!(
        Path::new(resources["framework"].as_str().context("Framework path")?).is_dir(),
        "P07: framework resource directory exists"
    );
    ensure!(
        resources["mergedManifest"]
            .as_str()
            .is_some_and(|path| path.ends_with("AndroidManifest.xml")),
        "P08: merged manifest path"
    );
    let core = resources["dependencies"]
        .as_array()
        .context("Resource dependencies")?
        .iter()
        .find(|dependency| dependency["namespace"] == "androidx.core")
        .context("Core AAR resource dependency")?;
    ensure!(
        Path::new(core["path"].as_str().context("Core resource path")?).is_dir()
            && Path::new(core["manifest"].as_str().context("Core manifest path")?).is_file(),
        "P09: AAR resources and manifest exist"
    );
    ensure!(
        core["publicResources"]
            .as_str()
            .is_some_and(|path| !path.is_empty() && Path::new(path).is_file()),
        "P10: AAR public metadata exists"
    );
    let test_root = root.join("app/src/androidTest/res");
    let test_root = test_root.to_str().context("Fixture root must be UTF-8")?;
    let test = &model["resourceModels"][":app/demoDebugAndroidTest"];
    ensure!(
        test["layers"]
            .as_array()
            .context("Test resource layers")?
            .iter()
            .any(|layer| layer
                .as_array()
                .is_some_and(|roots| roots.iter().any(|path| path == test_root))),
        "P11: Android test resource root"
    );
    Ok(())
}

fn run(options: Options) -> Result<()> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("Repository root")?;
    let temporary = if options.keep.is_none() {
        Some(
            tempfile::Builder::new()
                .prefix("koda-resources-")
                .tempdir()?,
        )
    } else {
        None
    };
    let root = options
        .keep
        .as_deref()
        .or_else(|| temporary.as_ref().map(|directory| directory.path()))
        .context("Fixture root")?;
    fs::create_dir_all(root)?;
    let root = root.canonicalize()?;
    write_fixture(&root, &options.agp)?;
    fs::write(
        root.join("local.properties"),
        format!("sdk.dir={}\n", options.sdk.canonicalize()?.display()),
    )?;
    let log = root.join("resources.log");
    let output = File::create(&log)?;
    successful(
        Command::new(&options.gradle)
            .args(gradle_arguments(repository))
            .current_dir(&root)
            .stdout(output.try_clone()?)
            .stderr(output),
        "Gradle resource export",
    )?;
    let model = model_from_log(BufReader::new(File::open(&log)?))?;
    verify_resources(&root, &model)?;
    let metadata = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(repository)
        .output()
        .context("Cargo metadata")?;
    ensure!(
        metadata.status.success(),
        "Cargo metadata failed: {}",
        metadata.status
    );
    let metadata: Value = serde_json::from_slice(&metadata.stdout)?;
    successful(
        Command::new("cargo")
            .args([
                "build",
                "--locked",
                "-p",
                "android_tools",
                "--example",
                "project_model_probe",
            ])
            .current_dir(repository),
        "Rust project model probe build",
    )?;
    let executable = Path::new(
        metadata["target_directory"]
            .as_str()
            .context("Cargo target directory")?,
    )
    .join("debug/examples")
    .join(format!("project_model_probe{}", env::consts::EXE_SUFFIX));
    successful(
        Command::new(executable)
            .arg(&root)
            .arg(&log)
            .args([":app", "demoDebug"])
            .stdout(Stdio::null()),
        "Rust selected project model probe",
    )?;
    println!(
        "AGP {}: resource layers, equal/custom roots, qualifiers, generated roots, AAR public metadata, framework and test roots passed",
        options.agp
    );
    Ok(())
}

fn main() -> Result<()> {
    if let Some(options) = Options::parse(env::args_os().skip(1))? {
        run(options)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn original_agp8_fixture_preserves_kotlin_custom_overlay_qualifiers_and_public_data()
    -> Result<()> {
        let root = tempfile::tempdir()?;
        write_fixture(root.path(), "8.9.1")?;
        assert!(
            fs::read_to_string(root.path().join("build.gradle"))?
                .contains("id 'org.jetbrains.kotlin.android' version '2.1.20' apply false")
        );
        let app = fs::read_to_string(root.path().join("app/build.gradle"))?;
        assert!(app.contains("apply plugin: 'org.jetbrains.kotlin.android'"));
        assert!(
            app.contains("sourceSets.main.res.srcDirs = ['custom/main-res', 'custom/equal-res']")
        );
        assert!(app.contains("resValue 'string', 'generated_probe', 'Generated'"));
        assert!(app.contains("org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_1_8"));
        assert_eq!(
            fs::read_to_string(root.path().join("app/custom/main-res/values-fr/probe.xml"))?,
            "<resources><string name=\"overlay_probe\">French</string></resources>"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("library/src/main/res/values/public.xml"))?,
            "<resources><public type=\"string\" name=\"library_probe\"/></resources>"
        );
        assert!(
            root.path()
                .join("app/src/androidTest/res/values/probe.xml")
                .is_file()
        );
        Ok(())
    }

    #[test]
    fn original_agp9_fixture_preserves_builtin_kotlin_and_parallel_project_dependencies()
    -> Result<()> {
        let root = tempfile::tempdir()?;
        write_fixture(root.path(), "9.4.0")?;
        assert!(
            !fs::read_to_string(root.path().join("build.gradle"))?
                .contains("org.jetbrains.kotlin.android")
        );
        let app = fs::read_to_string(root.path().join("app/build.gradle"))?;
        assert!(!app.contains("apply plugin: 'org.jetbrains.kotlin.android'"));
        assert!(app.contains("testImplementation testFixtures(project(':library'))"));
        assert!(app.contains("implementation 'androidx.core:core:1.16.0'"));
        assert!(
            fs::read_to_string(root.path().join("library/build.gradle"))?
                .contains("withBuildType('debug')")
        );
        assert!(
            fs::read_to_string(root.path().join("testing/build.gradle"))?
                .contains("sourceSets.main.java.srcDir(generator.flatMap { it.outputDirectory })")
        );
        assert_eq!(
            fs::read_to_string(root.path().join("gradle.properties"))?,
            "android.useAndroidX=true\nandroid.nonTransitiveRClass=true\n"
        );
        Ok(())
    }

    fn valid_model(root: &Path) -> Result<Value> {
        for path in ["framework", "core/res"] {
            fs::create_dir_all(root.join(path))?;
        }
        fs::write(root.join("core/AndroidManifest.xml"), "<manifest/>")?;
        fs::write(root.join("core/public.txt"), "string core_title\n")?;
        let dirs = [
            "src/demoDebug/res",
            "src/debug/res",
            "custom/demo-res",
            "custom/main-res",
            "custom/equal-res",
        ];
        let paths = dirs.map(|path| root.join("app").join(path));
        let mut sources: Vec<Value> = paths
            .iter()
            .map(|path| json!({"path":path,"kind":"resources","generated":false}))
            .collect();
        sources.push(json!({"path":root.join("app/build/generated/res/resValues"),"kind":"resources","generated":true}));
        Ok(
            json!({"modules":[{"path":":app","variants":[{"name":"demoDebug","components":[{"scope":"main","sources":sources}]}]}],
            "resourceModels":{":app/demoDebug":{"layers":[[paths[0]],[paths[1]],[paths[2]],[paths[3],paths[4]]],"nonTransitiveR":true,
                "framework":root.join("framework"),"mergedManifest":root.join("AndroidManifest.xml"),
                "dependencies":[{"namespace":"androidx.core","path":root.join("core/res"),"manifest":root.join("core/AndroidManifest.xml"),"publicResources":root.join("core/public.txt")}]},
                ":app/demoDebugAndroidTest":{"layers":[[root.join("app/src/androidTest/res")]]}}}),
        )
    }

    #[test]
    fn all_original_resource_predicates_accept_the_exporter_shape() -> Result<()> {
        let root = tempfile::tempdir()?;
        verify_resources(root.path(), &valid_model(root.path())?)
    }

    #[test]
    fn each_original_resource_predicate_rejects_its_invalid_export() -> Result<()> {
        let root = tempfile::tempdir()?;
        let original = valid_model(root.path())?;
        for case in 1..=11 {
            let mut invalid = original.clone();
            match case {
                1 => invalid["resourceModels"][":app/demoDebug"]["layers"]
                    .as_array_mut()
                    .unwrap()
                    .swap(0, 1),
                2 => {
                    let equal = invalid["resourceModels"][":app/demoDebug"]["layers"][3]
                        .as_array_mut()
                        .unwrap()
                        .pop()
                        .unwrap();
                    invalid["resourceModels"][":app/demoDebug"]["layers"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!([equal]));
                }
                3 => invalid["resourceModels"][":app/demoDebug"]["layers"][3]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(root.path().join("missing-root"))),
                4 => {
                    invalid["modules"][0]["variants"][0]["components"][0]["sources"][0]["generated"] =
                        json!(true)
                }
                5 => {
                    invalid["modules"][0]["variants"][0]["components"][0]["sources"]
                        .as_array_mut()
                        .unwrap()
                        .last_mut()
                        .unwrap()["generated"] = json!(false)
                }
                6 => invalid["resourceModels"][":app/demoDebug"]["nonTransitiveR"] = json!(false),
                7 => {
                    invalid["resourceModels"][":app/demoDebug"]["framework"] =
                        json!(root.path().join("missing-framework"))
                }
                8 => {
                    invalid["resourceModels"][":app/demoDebug"]["mergedManifest"] =
                        json!("Other.xml")
                }
                9 => {
                    invalid["resourceModels"][":app/demoDebug"]["dependencies"][0]["manifest"] =
                        json!(root.path().join("missing-manifest"))
                }
                10 => {
                    invalid["resourceModels"][":app/demoDebug"]["dependencies"][0]["publicResources"] =
                        json!("")
                }
                11 => invalid["resourceModels"][":app/demoDebugAndroidTest"]["layers"] = json!([]),
                _ => unreachable!(),
            }
            let error = verify_resources(root.path(), &invalid)
                .expect_err("Every original predicate remains enforced");
            assert!(
                error.to_string().contains(&format!("P{case:02}:")),
                "{error}"
            );
        }
        Ok(())
    }

    #[test]
    fn log_requires_one_valid_record_and_checks_size_before_growth() -> Result<()> {
        assert!(model_from_log(&b"ordinary Gradle output\n"[..]).is_err());
        assert!(
            model_from_log(&b"KODA_ANDROID_PROJECT_MODEL={}\nKODA_ANDROID_PROJECT_MODEL={}\n"[..])
                .is_err()
        );
        assert!(model_from_log(&b"KODA_ANDROID_PROJECT_MODEL=bad\n"[..]).is_err());
        assert!(model_from_log_with_limit(&b"KODA_ANDROID_PROJECT_MODEL={}\n"[..], 8).is_err());
        assert_eq!(
            model_from_log(&b"ordinary\nKODA_ANDROID_PROJECT_MODEL={\"modules\":[]}\r\n"[..])?,
            json!({"modules":[]})
        );
        Ok(())
    }

    #[test]
    fn cli_preserves_required_versions_paths_and_original_gradle_flags() -> Result<()> {
        assert!(Options::parse(Vec::<OsString>::new()).is_err());
        for agp in ["8.9.1", "9.4.0"] {
            let args = [
                "--gradle",
                "gradle path",
                "--sdk",
                "sdk path",
                "--agp",
                agp,
                "--keep",
                "fixture path",
            ]
            .map(OsString::from);
            let options = Options::parse(args)?.context("CLI options")?;
            assert_eq!(options.gradle, "gradle path");
            assert_eq!(options.sdk, PathBuf::from("sdk path"));
            assert_eq!(options.agp, agp);
            assert_eq!(options.keep, Some(PathBuf::from("fixture path")));
        }
        assert!(
            Options::parse(
                ["--gradle", "gradle", "--sdk", "sdk", "--agp", "9.0.0"].map(OsString::from)
            )
            .is_err()
        );
        assert_eq!(
            gradle_arguments(Path::new("repo")),
            vec![
                OsString::from("--console=plain"),
                "--no-configuration-cache".into(),
                "--parallel".into(),
                "--max-workers=2".into(),
                "--init-script".into(),
                Path::new("repo/crates/android_tools/src/project_model.gradle").into(),
                "kodaAndroidProjectModel".into()
            ]
        );
        Ok(())
    }
}
