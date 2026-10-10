use android_tools::project_context::{
    CONTEXT_OUTPUT_PREFIX, ObservationPhase, OperationalReadiness, PluginId, decode_context_output,
};
use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    path::Path,
};

const MAX_LOG_BYTES: u64 = 16 * 1024 * 1024;
const CASES: [(&str, &str, &str, &str); 6] = [
    ("stable", "none", "none", "9.4.0"),
    ("preview", "none", "none", "9.5.0-alpha03"),
    (
        "components-exception",
        "getPluginVersion",
        "exception",
        "9.4.0",
    ),
    ("components-linkage", "getPluginVersion", "linkage", "9.4.0"),
    ("version-exception", "getVersion", "exception", "9.4.0"),
    ("version-linkage", "getVersion", "linkage", "9.4.0"),
];
const FIXTURES: [(&str, &str); 5] = [
    (
        "settings.gradle",
        include_str!("../test_data/context_sdk_getters/settings.gradle"),
    ),
    (
        "buildSrc/build.gradle",
        include_str!("../test_data/context_sdk_getters/buildSrc/build.gradle"),
    ),
    (
        "buildSrc/src/main/resources/META-INF/gradle-plugins/com.android.library.properties",
        include_str!(
            "../test_data/context_sdk_getters/buildSrc/src/main/resources/META-INF/gradle-plugins/com.android.library.properties"
        ),
    ),
    (
        "buildSrc/src/main/groovy/ContextSdkGetterPlugin.groovy",
        include_str!(
            "../test_data/context_sdk_getters/buildSrc/src/main/groovy/ContextSdkGetterPlugin.groovy"
        ),
    ),
    (
        "library/build.gradle",
        include_str!("../test_data/context_sdk_getters/library/build.gradle"),
    ),
];

fn write_new(path: &Path, contents: &[u8]) -> Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(contents)?;
    Ok(())
}

fn prepare(output: &Path) -> Result<()> {
    fs::create_dir(output)
        .context("Use a new output directory; retained attempts must not be overwritten")?;
    let project = output.join("project");
    fs::create_dir(&project)?;
    for (relative, contents) in FIXTURES {
        let destination = project.join(relative);
        fs::create_dir_all(destination.parent().context("Fixture parent")?)?;
        write_new(&destination, contents.as_bytes())?;
    }
    write_new(
        &output.join("production-project_context.gradle"),
        include_bytes!("../src/project_context.gradle"),
    )?;
    let cases = CASES.iter().map(|(name, getter, failure, version)| json!({
        "case": name, "getter": getter, "failure": failure, "version": version,
        "task": "kodaProjectContext",
        "properties": [format!("-PfixtureGetter={getter}"), format!("-PfixtureFailure={failure}"), format!("-PfixtureVersion={version}")],
        "log": format!("{name}.log"), "exitCode": format!("{name}.exit"), "status": "NOT_RUN"
    })).collect::<Vec<_>>();
    write_new(
        &output.join("cases-PREPARED.json"),
        &serde_json::to_vec_pretty(&json!({
            "schema": 1, "cases": cases, "caseDeadlineSeconds": 420,
            "caseLogLimitBytes": MAX_LOG_BYTES, "maxWorkers": 1,
            "runtimeExecuted": false, "referenceParityCredit": false
        }))?,
    )?;
    println!(
        "Prepared six context getter cases in {}; none has run",
        output.display()
    );
    Ok(())
}

fn read_bounded(path: &Path) -> Result<String> {
    let file = File::open(path)?;
    ensure!(
        file.metadata()?.len() <= MAX_LOG_BYTES,
        "Retain the oversized log and report the 16 MiB limit failure"
    );
    let mut contents = Vec::new();
    file.take(MAX_LOG_BYTES + 1).read_to_end(&mut contents)?;
    ensure!(
        contents.len() as u64 <= MAX_LOG_BYTES,
        "Log grew beyond 16 MiB during verification"
    );
    Ok(String::from_utf8(contents)?)
}

fn verify(output: &Path) -> Result<()> {
    let root = output.join("project").canonicalize()?;
    let mut reports = Vec::new();
    for (name, getter, failure, version) in CASES {
        ensure!(
            read_bounded(&output.join(format!("{name}.exit")))? == "0\n",
            "{name}: actual Gradle exit must be zero"
        );
        let log = read_bounded(&output.join(format!("{name}.log")))?;
        ensure!(
            log.contains("BUILD SUCCESSFUL"),
            "{name}: missing successful Gradle result"
        );
        let contexts = decode_context_output(&log, &root)?;
        ensure!(
            contexts.len() == 1,
            "{name}: expected one production context record"
        );
        let snapshot = contexts.first().context("Context missing")?;
        ensure!(
            snapshot.phase() == ObservationPhase::Complete,
            "{name}: evaluation did not complete"
        );
        ensure!(
            snapshot.modules().count() == 2,
            "{name}: expected root and library owners"
        );
        let records = log
            .lines()
            .filter_map(|line| line.strip_prefix(CONTEXT_OUTPUT_PREFIX))
            .collect::<Vec<_>>();
        ensure!(records.len() == 1, "{name}: unexpected raw context count");
        let wire: Value = serde_json::from_str(records.first().context("Context record")?)?;
        let modules = wire["modules"].as_array().context("Context modules")?;
        let root_module = modules
            .iter()
            .find(|module| module["path"] == ":")
            .context("Root owner")?;
        ensure!(
            root_module["directory"] == json!(root),
            "{name}: root directory differs"
        );
        let root_plugins = root_module["plugins"].as_array().context("Root plugins")?;
        ensure!(
            root_plugins.len() == PluginId::ALL.len()
                && root_plugins.iter().all(|plugin| plugin["applied"] == false),
            "{name}: unexpected root plugin catalogue"
        );
        ensure!(
            root_module.get("android").is_none(),
            "{name}: root Android API invented"
        );
        let module = modules
            .iter()
            .find(|module| module["path"] == ":library")
            .context("Library owner")?;
        ensure!(
            module["directory"] == json!(root.join("library")),
            "{name}: library directory differs"
        );
        let plugins = module["plugins"].as_array().context("Library plugins")?;
        ensure!(
            plugins.len() == PluginId::ALL.len()
                && plugins
                    .iter()
                    .all(|plugin| plugin["applied"] == (plugin["plugin"] == "com.android.library")),
            "{name}: wrong applied library plugins"
        );
        ensure!(
            module["targets"]["status"] == "unavailable"
                && module["targets"]["value"]["detail"] == "No evaluated Kotlin target extension",
            "{name}: Kotlin target invented"
        );
        let android = &module["android"];
        if failure == "none" {
            ensure!(
                android["status"] == "available" && android["value"]["pluginVersion"] == version,
                "{name}: exact public version getter missing: {android}"
            );
            ensure!(
                android["value"]["pluginVersion"]
                    != format!("Android Gradle Plugin version {version}"),
                "{name}: descriptive toString leaked into version"
            );
        } else {
            ensure!(
                android["status"] == "unavailable",
                "{name}: failed getter became available: {android}"
            );
            let detail = android["value"]["detail"]
                .as_str()
                .context("Failure detail")?;
            let failure_class = if failure == "linkage" {
                "java.lang.NoClassDefFoundError"
            } else {
                "java.lang.IllegalStateException"
            };
            ensure!(
                detail.contains(failure_class)
                    && detail.contains(&format!("Fixture {getter} {failure} failure")),
                "{name}: original failure class/message lost: {detail}"
            );
            ensure!(
                android["value"].get("pluginVersion").is_none(),
                "{name}: failed version invented"
            );
        }
        let capabilities = snapshot.capabilities(None, OperationalReadiness::default());
        ensure!(
            capabilities.ecosystems.android && capabilities.android_sync,
            "{name}: applied Android plugin must retain explicit Sync"
        );
        ensure!(
            capabilities.android_devices == (failure == "none")
                && capabilities.automatic_android_sync == (failure == "none"),
            "{name}: Android API availability differs from device/automatic Sync readiness"
        );
        ensure!(
            !capabilities.ecosystems.kotlin_multiplatform
                && !capabilities.ecosystems.compose_multiplatform
                && !capabilities.android_run
                && !capabilities.android_compose_preview,
            "{name}: unobserved ecosystem or runtime readiness invented"
        );
        reports.push(json!({"case": name, "status": "PASS", "actualGradleExit": 0, "android": android, "referenceParityCredit": false}));
    }
    write_new(
        &output.join("verification-ACTUAL.json"),
        &serde_json::to_vec_pretty(
            &json!({"schema": 1, "cases": reports, "fullProductionAdapter": true, "referenceParityCredit": false}),
        )?,
    )?;
    println!("All six actual full-adapter context getter cases passed");
    Ok(())
}

fn main() -> Result<()> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    ensure!(
        arguments.len() == 2,
        "Usage: gradle_context_sdk_getters prepare|verify /absolute/new-attempt-directory"
    );
    let output = Path::new(arguments.get(1).context("Output directory")?);
    ensure!(output.is_absolute(), "Output directory must be absolute");
    match arguments.first().and_then(|argument| argument.to_str()) {
        Some("prepare") => prepare(output),
        Some("verify") => verify(output),
        _ => anyhow::bail!("Expected prepare or verify"),
    }
}
