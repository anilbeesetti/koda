use android_tools::{
    evaluated_tree_inputs::EvaluatedTreeInputs,
    project_model::{MODEL_OUTPUT_PREFIX, ModelState, ModuleKind},
    project_tree_facts::FactsUnavailableReason,
};
use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    path::Path,
    sync::Arc,
};

const MAX_LOG_BYTES: u64 = 16 * 1024 * 1024;
const SDK_GETTER: &str = "com.android.build.api.AndroidPluginVersion.getMajor/getMinor/getMicro/getPreview/getPreviewType/getVersion";
const SDK_COMPONENTS_GETTER: &str =
    "com.android.build.api.variant.AndroidComponentsExtension.getPluginVersion()";
const CASES: [(&str, &str, &str); 4] = [
    (
        "getPluginVersion",
        "exception",
        "java.lang.IllegalStateException",
    ),
    (
        "getPluginVersion",
        "linkage",
        "java.lang.NoClassDefFoundError",
    ),
    ("getVersion", "exception", "java.lang.IllegalStateException"),
    ("getVersion", "linkage", "java.lang.NoClassDefFoundError"),
];
const FIXTURES: [(&str, &str); 7] = [
    (
        "settings.gradle",
        include_str!("../test_data/optional_sdk_getters/settings.gradle"),
    ),
    (
        "buildSrc/build.gradle",
        include_str!("../test_data/optional_sdk_getters/buildSrc/build.gradle"),
    ),
    (
        "buildSrc/src/main/resources/META-INF/gradle-plugins/com.android.library.properties",
        include_str!(
            "../test_data/optional_sdk_getters/buildSrc/src/main/resources/META-INF/gradle-plugins/com.android.library.properties"
        ),
    ),
    (
        "buildSrc/src/main/groovy/OptionalSdkGetterPlugin.groovy",
        include_str!(
            "../test_data/optional_sdk_getters/buildSrc/src/main/groovy/OptionalSdkGetterPlugin.groovy"
        ),
    ),
    (
        "library/build.gradle",
        include_str!("../test_data/optional_sdk_getters/library/build.gradle"),
    ),
    (
        "library/src/main/java/example/Library.java",
        include_str!(
            "../test_data/optional_sdk_getters/library/src/main/java/example/Library.java"
        ),
    ),
    (
        "library/src/main/AndroidManifest.xml",
        include_str!("../test_data/optional_sdk_getters/library/src/main/AndroidManifest.xml"),
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
        &output.join("production-project_model.gradle"),
        include_bytes!("../src/project_model.gradle"),
    )?;
    let cases = CASES.iter().map(|(getter, failure, failure_class)| json!({
        "case":format!("{getter}-{failure}"), "getter":getter, "failure":failure,
        "failureClass":failure_class, "task":"kodaAndroidProjectModel",
        "properties":[format!("-PfixtureGetter={getter}"), format!("-PfixtureFailure={failure}")],
        "log":format!("{getter}-{failure}.log"), "exitCode":format!("{getter}-{failure}.exit"),
        "status":"NOT_RUN"
    })).collect::<Vec<_>>();
    write_new(
        &output.join("cases-PREPARED.json"),
        &serde_json::to_vec_pretty(&json!({
            "schema":1, "cases":cases, "caseDeadlineSeconds":420, "caseLogLimitBytes":MAX_LOG_BYTES,
            "maxWorkers":1, "runtimeExecuted":false, "referenceParityCredit":false
        }))?,
    )?;
    println!(
        "Prepared four full-adapter Gradle cases in {}; none has run",
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
    for (getter, failure, failure_class) in CASES {
        let name = format!("{getter}-{failure}");
        let expected_getter = if getter == "getPluginVersion" {
            SDK_COMPONENTS_GETTER
        } else {
            SDK_GETTER
        };
        ensure!(
            read_bounded(&output.join(format!("{name}.exit")))? == "0\n",
            "{name}: actual Gradle exit must be zero"
        );
        let log = read_bounded(&output.join(format!("{name}.log")))?;
        ensure!(
            log.contains("BUILD SUCCESSFUL"),
            "{name}: missing actual successful Gradle result"
        );
        let mut state = ModelState::default();
        let token = state.invalidate(Some(root.clone()));
        let capture = EvaluatedTreeInputs::decode_sync(&log, &root, None, &token)?;
        let wire: Value = serde_json::from_str(
            capture
                .raw_record()
                .strip_prefix(MODEL_OUTPUT_PREFIX)
                .context("Actual model prefix")?,
        )?;
        ensure!(
            capture.token() == &token,
            "{name}: original token must be retained"
        );
        ensure!(
            wire["modules"]
                .as_array()
                .context("Core module list")?
                .len()
                == 1,
            "{name}: core module must survive"
        );
        let module = &wire["modules"][0];
        ensure!(
            module["path"] == ":library" && module["kind"] == "library",
            "{name}: actual library identity changed"
        );
        ensure!(
            module["namespace"] == "example",
            "{name}: namespace changed"
        );
        ensure!(
            module["variants"][0]["name"] == "debug",
            "{name}: actual variant missing"
        );
        let java = root.join("library/src/main/java");
        let manifest = root.join("library/src/main/AndroidManifest.xml");
        let sources = module["variants"][0]["components"][0]["sources"]
            .as_array()
            .context("Direct SDK source roots")?;
        ensure!(
            sources.len() == 2,
            "{name}: retain both direct roots without fabricating others"
        );
        ensure!(
            sources.iter().any(|source| source["path"] == json!(java)
                && source["kind"] == "java"
                && source["generated"] == false),
            "{name}: direct static Java root missing"
        );
        ensure!(
            sources
                .iter()
                .any(|source| source["path"] == json!(manifest)
                    && source["kind"] == "manifest"
                    && source["generated"] == false),
            "{name}: direct manifest root missing"
        );
        ensure!(
            java.join("example/Library.java").is_file() && manifest.is_file(),
            "{name}: physical fixture files missing"
        );
        let kotlin = &wire["kotlinCapabilities"]["modules"][0];
        ensure!(
            kotlin["module"] == ":library" && kotlin["directory"] == json!(root.join("library")),
            "{name}: sidecar owner changed"
        );
        ensure!(
            kotlin["agpVersion"].is_null(),
            "{name}: failed getter must not invent an AGP version"
        );
        let sdk = &kotlin["sdkPluginVersion"];
        ensure!(
            sdk["getter"] == expected_getter
                && sdk["result"]["value"]["capability"] == expected_getter,
            "{name}: getter provenance changed"
        );
        ensure!(
            sdk["result"]["status"] == "unavailable",
            "{name}: failed getter must remain unavailable"
        );
        let detail = sdk["result"]["value"]["detail"]
            .as_str()
            .context("SDK failure detail")?;
        ensure!(
            detail.contains(failure_class)
                && detail.contains(&format!("Fixture {getter} {failure} failure")),
            "{name}: original failure class/message lost"
        );
        ensure!(
            kotlin["builtInKotlin"]["result"]["status"] == "unavailable",
            "{name}: builtin adapter must not guess support"
        );
        ensure!(
            wire["generatedArtifacts"]["modules"][0]["versions"]["status"] == "unavailable",
            "{name}: absent tooling model must remain unavailable"
        );
        let diagnostics = wire["diagnostics"]
            .as_array()
            .context("Actual diagnostics")?;
        ensure!(
            diagnostics
                .iter()
                .any(|entry| entry.as_str().is_some_and(|entry| entry
                    .contains("Kotlin SDK version is unavailable for :library")
                    && entry.contains(detail))),
            "{name}: unavailable version diagnostic missing"
        );
        let legacy_diagnostic = diagnostics.iter().any(|entry| {
            entry.as_str().is_some_and(|entry| {
                entry.contains("Legacy source-root augmentation is unavailable for :library")
                    && entry.contains("direct source-provider roots were retained")
            })
        });
        ensure!(
            legacy_diagnostic == (getter == "getPluginVersion"),
            "{name}: distinguish unavailable major from unavailable version text"
        );
        let capability_failure = capture
            .kotlin_capability(":library")
            .err()
            .context("SDK getter cannot supply Kotlin capability")?;
        ensure!(
            capability_failure.reason == FactsUnavailableReason::Capability
                && capability_failure.detail.contains(detail),
            "{name}: typed capability reason/detail lost"
        );
        let original_model = capture.model().clone();
        state.publish_evaluated(&token, capture)?;
        let model = state
            .model
            .as_ref()
            .context("Published core physical model")?;
        ensure!(
            Arc::ptr_eq(model, &original_model),
            "{name}: core model must retain original Arc ownership"
        );
        ensure!(
            model.modules.len() == 1
                && model
                    .modules
                    .first()
                    .is_some_and(|module| module.kind == ModuleKind::Library),
            "{name}: published library missing"
        );
        reports.push(json!({"case":name,"status":"PASS","actualGradleExit":0,"sdkGetter":expected_getter,"failureDetail":detail,"physicalModules":1,"referenceParityCredit":false}));
    }
    write_new(
        &output.join("verification-ACTUAL.json"),
        &serde_json::to_vec_pretty(
            &json!({"schema":1,"cases":reports,"fullProductionAdapter":true,"referenceParityCredit":false}),
        )?,
    )?;
    println!("All four actual full-adapter Gradle cases passed");
    Ok(())
}

fn main() -> Result<()> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    ensure!(
        arguments.len() == 2,
        "Usage: gradle_optional_sdk_getters prepare|verify /absolute/new-attempt-directory"
    );
    let output = Path::new(arguments.get(1).context("Output directory")?);
    ensure!(output.is_absolute(), "Output directory must be absolute");
    match arguments.first().and_then(|argument| argument.to_str()) {
        Some("prepare") => prepare(output),
        Some("verify") => verify(output),
        _ => anyhow::bail!("Expected prepare or verify"),
    }
}
