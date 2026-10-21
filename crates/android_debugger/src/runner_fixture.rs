//! Acceptance checks for the actual Android test runner Gradle script.
//!
//! Ported from `script/test-android-test-runner` without changing its fixture
//! selectors, predicates, or success messages. This CLI explicitly runs trusted
//! fixture Gradle code; it is not on the IDE startup or editor path.

// These synchronous CLI probes supervise OS processes outside GPUI.
#![allow(clippy::disallowed_methods)]

use anyhow::{Context, Result, bail, ensure};
use quick_xml::{events::Event, name::ResolveResult, reader::NsReader};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    time::Duration,
};
use tempfile::TempDir;

const TIMEOUT: Duration = Duration::from_secs(600);
const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;
const MAX_REPORT_BYTES: usize = 128 * 1024 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 100_000;
const MAX_CASES: usize = 65_536;
const MAX_XML_DEPTH: usize = 128;
const MODEL_MARKER: &str = "KODA_ANDROID_PROJECT_MODEL=";
const UNIT_PASS: &str = "PASS: selected variant unit/instrumentation discovery, method/class/suite, pass/fail/skipped, failure source, fresh rerun, parameterized class";
const DEVICE_PASS: &str = "PASS: Compose instrumentation method on selected device";
const DEVICE_NOT_RUN: &str = "NOT RUN: device/instrumentation validation (--device required)";

#[derive(Debug, PartialEq, Eq)]
struct Arguments {
    root: PathBuf,
    device: Option<String>,
}

#[derive(Clone, Copy, Debug)]
struct Probe {
    selector: &'static str,
    failure: bool,
    count: usize,
    failures: usize,
    skipped: usize,
}

const PROBES: [Probe; 5] = [
    Probe {
        selector: "dev.zed.androidsample.RunnerTest.passes",
        failure: false,
        count: 1,
        failures: 0,
        skipped: 0,
    },
    Probe {
        selector: "dev.zed.androidsample.RunnerSuite",
        failure: false,
        count: 5,
        failures: 0,
        skipped: 1,
    },
    Probe {
        selector: "dev.zed.androidsample.RunnerTest",
        failure: true,
        count: 3,
        failures: 1,
        skipped: 1,
    },
    Probe {
        selector: "dev.zed.androidsample.RunnerTest.optionalFailure",
        failure: false,
        count: 1,
        failures: 0,
        skipped: 0,
    },
    Probe {
        selector: "dev.zed.androidsample.ParameterizedRunnerTest",
        failure: false,
        count: 2,
        failures: 0,
        skipped: 0,
    },
];

#[derive(Debug, Default)]
struct Case {
    attributes: BTreeMap<String, String>,
    failure: bool,
    error: bool,
    skipped: bool,
}

impl Case {
    fn failed(&self) -> bool {
        self.failure || self.error
    }
}

/// `--root` is supplied by the transport script. `--device` has the same
/// meaning as the historical probe and never silently grants device coverage.
pub fn run(args: &[OsString]) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!(
            "Exercise the test runner's actual Gradle script against the Android IDE fixture.\n\nRequires a full JDK, Android SDK, and normal Gradle repository access.\nUsage: android-debugger probe-test-runner --root REPOSITORY [--device SERIAL]\n\n--device SERIAL  Connected device serial for instrumentation validation"
        );
        return Ok(());
    }
    parse_arguments(args)?;
    crate::runner_process::exec_worker(args)
}

/// Internal CLI entry point; supervision is confined to this isolated worker.
pub fn run_worker(args: &[OsString]) -> Result<()> {
    let arguments = parse_arguments(args)?;
    crate::runner_process::in_worker(|| run_fixture(&arguments))
}

fn parse_arguments(args: &[OsString]) -> Result<Arguments> {
    let mut root = None;
    let mut device = None;
    let mut index = 0;
    while index < args.len() {
        let option = &args[index];
        index += 1;
        let value = args
            .get(index)
            .context("Expected a value after --root or --device")?;
        match option.to_str() {
            Some("--root") => {
                ensure!(root.is_none(), "--root specified more than once");
                root = Some(PathBuf::from(value));
            }
            Some("--device") => {
                // argparse accepts a repeated --device and uses the last value.
                let value = value.to_str().context("Device serial is not Unicode")?;
                device = (!value.is_empty()).then(|| value.to_owned());
            }
            _ => bail!(
                "Unknown test runner probe argument: {}",
                option.to_string_lossy()
            ),
        }
        index += 1;
    }
    Ok(Arguments {
        root: root.context("--root REPOSITORY is required")?,
        device,
    })
}

fn run_fixture(arguments: &Arguments) -> Result<()> {
    let fixture = arguments.root.join("examples/android-ide");
    let runner_source = read_text(
        &arguments.root.join("crates/android_tools/src/testing.rs"),
        MAX_FILE_BYTES,
    )?;
    let runner_script = extract_init_script(&runner_source)?;
    let model_script = read_text(
        &arguments
            .root
            .join("crates/android_tools/src/project_model.gradle"),
        MAX_FILE_BYTES,
    )?;

    {
        let directory = tempfile::Builder::new()
            .prefix("koda-runner-model-")
            .tempdir()?;
        let init = directory.path().join("model.gradle");
        fs::write(&init, model_script)?;
        let log_path = directory.path().join("model-output.log");
        let log = File::create(&log_path)?;
        let mut command = gradle_command(&fixture);
        command.args(model_arguments(&init));
        command
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        let status = wait_for_command(command, TIMEOUT, Some(&log_path))?;
        ensure!(status.success(), "Gradle project model failed: {status}");
        let output = read_text(&log_path, MAX_FILE_BYTES)?;
        // Python's print adds one newline, including when stdout already ends in one.
        println!("{output}");
        validate_model_output(&output)?;
    }

    for probe in &PROBES {
        let (directory, cases) = run_probe(
            &fixture,
            runner_script,
            "testDemoDebugUnitTest",
            &[probe.selector],
            probe.failure,
            None,
        )?;
        let events = read_events(&directory.path().join("events.jsonl"))?;
        validate_unit_probe(probe, &cases, &events)?;
        directory.close()?;
    }
    println!("{UNIT_PASS}");

    if let Some(device) = arguments.device.as_deref() {
        let (directory, cases) = run_probe(
            &fixture,
            runner_script,
            "connectedDemoDebugAndroidTest",
            &["dev.zed.androidsample.RunnerComposeTest#showsContent"],
            false,
            Some(device),
        )?;
        validate_device_probe(&cases)?;
        directory.close()?;
        println!("{DEVICE_PASS}");
    } else {
        println!("{DEVICE_NOT_RUN}");
    }
    Ok(())
}

fn extract_init_script(source: &str) -> Result<&str> {
    let (_, tail) = source
        .split_once("pub const INIT_SCRIPT: &str = r#\"")
        .context("Could not locate the test runner init script")?;
    let (script, _) = tail
        .split_once("\"#;")
        .context("Could not locate the test runner init script")?;
    Ok(script)
}

fn model_arguments(init: &Path) -> Vec<OsString> {
    vec![
        "--init-script".into(),
        init.as_os_str().into(),
        "kodaAndroidProjectModel".into(),
        "--no-configuration-cache".into(),
        "--no-daemon".into(),
        "--console=plain".into(),
    ]
}

fn probe_arguments(
    task: &str,
    init: &Path,
    output: &Path,
    selectors: &[&str],
    failure: bool,
    device: bool,
) -> Vec<OsString> {
    let mut arguments = vec![
        format!(":mobile:{task}").into(),
        "--init-script".into(),
        init.as_os_str().into(),
        "-Dkoda.test.module=:mobile".into(),
        "-Dkoda.test.variant=demoDebug".into(),
    ];
    let mut output_argument = OsString::from("-Dkoda.test.output=");
    output_argument.push(output);
    arguments.extend([
        output_argument,
        format!(
            "-Dkoda.test.kind={}",
            if device { "device" } else { "unit" }
        )
        .into(),
        "--console=plain".into(),
        "--no-configuration-cache".into(),
        "--no-daemon".into(),
    ]);
    if device {
        arguments.push("--rerun-tasks".into());
        if !selectors.is_empty() {
            arguments.push(
                format!(
                    "-Pandroid.testInstrumentationRunnerArguments.class={}",
                    selectors.join(",")
                )
                .into(),
            );
        }
    } else {
        for selector in selectors {
            arguments.extend([OsString::from("--tests"), OsString::from(*selector)]);
        }
    }
    if failure {
        arguments.push("-Pkoda.fixture.fail=true".into());
    }
    arguments
}

fn gradle_command(fixture: &Path) -> Command {
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("cmd.exe");
        command.args(["/D", "/C"]).arg(fixture.join("gradlew.bat"));
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new("/bin/sh");
        command.arg("./gradlew");
        command
    };
    command.current_dir(fixture);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // The wrapper and its JVM belong to one bounded probe, including timeout cleanup.
        command.process_group(0);
    }
    command
}

fn wait_for_command(
    command: Command,
    timeout: Duration,
    log_path: Option<&Path>,
) -> Result<ExitStatus> {
    crate::runner_process::wait_for_command(command, timeout, log_path, MAX_FILE_BYTES as u64)
}

fn run_probe(
    fixture: &Path,
    script: &str,
    task: &str,
    selectors: &[&str],
    failure: bool,
    device: Option<&str>,
) -> Result<(TempDir, Vec<Case>)> {
    let directory = tempfile::Builder::new()
        .prefix("koda-runner-probe-")
        .tempdir()?;
    let init = directory.path().join("runner.gradle");
    fs::write(&init, script)?;
    let command = prepare_probe_command(
        fixture,
        task,
        &init,
        directory.path(),
        selectors,
        failure,
        device,
    );
    let status = wait_for_command(command, TIMEOUT, None)?;
    validate_exit(
        !status.success(),
        failure,
        task,
        &exit_code(&status).to_string(),
    )?;
    let cases = read_reports(directory.path())?;
    Ok((directory, cases))
}

fn prepare_probe_command(
    fixture: &Path,
    task: &str,
    init: &Path,
    output: &Path,
    selectors: &[&str],
    failure: bool,
    device: Option<&str>,
) -> Command {
    let mut command = gradle_command(fixture);
    command.args(probe_arguments(
        task,
        init,
        output,
        selectors,
        failure,
        device.is_some(),
    ));
    if let Some(device) = device {
        command.env("ANDROID_SERIAL", device);
    }
    command
}

fn validate_exit(nonzero: bool, failure: bool, task: &str, status: &str) -> Result<()> {
    ensure!(
        nonzero == failure,
        "Unexpected Gradle result for {task}: {status}"
    );
    Ok(())
}

fn exit_code(status: &ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        // subprocess reports termination by signal as the negative signal number.
        -status.signal().unwrap_or(1)
    }
    #[cfg(not(unix))]
    {
        -1
    }
}

fn array<'a>(value: &'a Value, name: &str) -> Result<&'a [Value]> {
    value
        .get(name)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .with_context(|| format!("Project model has no {name} array"))
}

fn required<'a>(value: &'a Value, name: &str) -> Result<&'a Value> {
    value
        .get(name)
        .with_context(|| format!("Project model record is missing {name}"))
}

fn find_record<'a>(records: &'a [Value], key: &str, value: &str) -> Result<&'a Value> {
    for record in records {
        if required(record, key)? == value {
            return Ok(record);
        }
    }
    bail!("Missing project model {key}={value}")
}

fn validate_model_output(output: &str) -> Result<()> {
    let records: Vec<_> = output
        .lines()
        .filter_map(|line| line.strip_prefix(MODEL_MARKER))
        .collect();
    ensure!(
        records.len() == 1,
        "Expected one project model record, got {}",
        records.len()
    );
    let model: Value = serde_json::from_str(records[0])?;
    let module = find_record(array(&model, "modules")?, "path", ":mobile")?;
    let variant = find_record(array(module, "variants")?, "name", "demoDebug")?;
    for (scope, name, path) in [
        ("unitTest", "demoDebugUnitTest", "src/testDemo/java"),
        (
            "androidTest",
            "demoDebugAndroidTest",
            "src/androidTest/java",
        ),
    ] {
        let mut selected = None;
        for component in array(variant, "components")? {
            if required(component, "name")? == name && required(component, "scope")? == scope {
                selected = Some(component);
                break;
            }
        }
        let component = selected.with_context(|| format!("Missing {scope} component {name}"))?;
        let mut directories = Vec::new();
        for source in array(component, "sources")? {
            let kind = required(source, "kind")?;
            if kind == "java" || kind == "kotlin" {
                directories.push(
                    required(source, "path")?
                        .as_str()
                        .context("Component source path is not a string")?,
                );
            }
        }
        ensure!(
            directories.iter().any(|directory| directory.contains(path)),
            "Missing {path} in {directories:?}"
        );
        ensure!(
            !directories
                .iter()
                .any(|directory| directory.contains("testFull")),
            "Unexpected testFull source in {directories:?}"
        );
    }
    Ok(())
}

fn validate_unit_probe(probe: &Probe, cases: &[Case], events: &[Value]) -> Result<()> {
    ensure!(
        cases.len() == probe.count,
        "{} expected {} cases, got {}: {:?}",
        probe.selector,
        probe.count,
        cases.len(),
        cases
            .iter()
            .map(|case| &case.attributes)
            .collect::<Vec<_>>()
    );
    ensure!(
        cases.iter().filter(|case| case.failed()).count() == probe.failures,
        "{} failure/error count differs",
        probe.selector
    );
    ensure!(
        cases.iter().filter(|case| case.skipped).count() == probe.skipped,
        "{} skipped count differs",
        probe.selector
    );
    ensure!(
        events.len() == probe.count,
        "{} event count differs: {}",
        probe.selector,
        events.len()
    );
    if probe.failure {
        let mut source_found = false;
        for event in events {
            let status = event.get("status").context("Runner event has no status")?;
            if status == "Failed" {
                let detail = event
                    .get("detail")
                    .and_then(Value::as_str)
                    .context("Failed runner event has no detail string")?;
                if detail.contains("RunnerTest.java:") {
                    source_found = true;
                    break;
                }
            }
        }
        ensure!(
            source_found,
            "Failed event has no RunnerTest.java: source location"
        );
    }
    Ok(())
}

fn validate_device_probe(cases: &[Case]) -> Result<()> {
    ensure!(
        cases.len() == 1,
        "Expected one Compose instrumentation case, got {}",
        cases.len()
    );
    ensure!(
        !cases[0].failure,
        "Compose instrumentation testcase contains failure"
    );
    ensure!(
        !cases[0].error,
        "Compose instrumentation testcase contains error"
    );
    Ok(())
}

fn read_text(path: &Path, limit: usize) -> Result<String> {
    let file = File::open(path).with_context(|| format!("Could not read {}", path.display()))?;
    ensure!(
        file.metadata()?.len() <= limit as u64,
        "{} exceeds {limit} bytes",
        path.display()
    );
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit,
        "{} exceeds {limit} bytes",
        path.display()
    );
    String::from_utf8(bytes).with_context(|| format!("{} is not UTF-8", path.display()))
}

fn read_events(path: &Path) -> Result<Vec<Value>> {
    let text = read_text(path, MAX_FILE_BYTES)?;
    let mut events = Vec::new();
    for line in text.lines() {
        ensure!(events.len() < MAX_CASES, "Too many runner events");
        events.push(serde_json::from_str(line)?);
    }
    Ok(events)
}

fn read_reports(root: &Path) -> Result<Vec<Case>> {
    let mut pending = vec![root.to_path_buf()];
    let mut entries = 0_usize;
    let mut bytes = 0_usize;
    let mut cases = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            entries = entries
                .checked_add(1)
                .context("Report entry count overflow")?;
            ensure!(
                entries <= MAX_DIRECTORY_ENTRIES,
                "Too many report directory entries"
            );
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("TEST-") && name.ends_with(".xml"))
            {
                ensure!(
                    kind.is_file(),
                    "JUnit report is not a regular file: {}",
                    entry.path().display()
                );
                let text = read_text(&entry.path(), MAX_FILE_BYTES.min(MAX_REPORT_BYTES - bytes))?;
                bytes = bytes
                    .checked_add(text.len())
                    .context("Report byte count overflow")?;
                let parsed = parse_report(&text, MAX_CASES - cases.len())?;
                cases.extend(parsed);
            }
        }
    }
    Ok(cases)
}

fn parse_report(text: &str, limit: usize) -> Result<Vec<Case>> {
    let mut reader = NsReader::from_reader(text.as_bytes());
    reader.config_mut().check_end_names = true;
    reader.config_mut().check_comments = true;
    let mut buffer = Vec::new();
    let mut stack: Vec<Option<usize>> = Vec::new();
    let mut cases = Vec::<Case>::new();
    let mut roots = 0;
    loop {
        let (namespace, event) = reader.read_resolved_event_into(&mut buffer)?;
        let plain = match namespace {
            ResolveResult::Unbound => true,
            ResolveResult::Bound(_) => false,
            ResolveResult::Unknown(_) => bail!("Unbound XML namespace prefix in JUnit report"),
        };
        match event {
            event @ (Event::Start(_) | Event::Empty(_)) => {
                let empty = matches!(&event, Event::Empty(_));
                let tag = match event {
                    Event::Start(tag) | Event::Empty(tag) => tag,
                    _ => unreachable!(),
                };
                if stack.is_empty() {
                    roots += 1;
                    ensure!(roots == 1, "JUnit report has multiple root elements");
                }
                if plain {
                    if let Some(Some(index)) = stack.last() {
                        let case = &mut cases[*index];
                        match tag.local_name().as_ref() {
                            b"failure" => case.failure = true,
                            b"error" => case.error = true,
                            b"skipped" => case.skipped = true,
                            _ => (),
                        }
                    }
                }
                let index = if plain && tag.local_name().as_ref() == b"testcase" {
                    ensure!(cases.len() < limit, "Too many JUnit testcases");
                    let mut case = Case::default();
                    for attribute in tag.attributes() {
                        let attribute = attribute?;
                        let name = std::str::from_utf8(attribute.key.as_ref())?;
                        if name != "xmlns" && !name.starts_with("xmlns:") {
                            case.attributes.insert(
                                name.to_owned(),
                                attribute
                                    .decoded_and_normalized_value(
                                        quick_xml::XmlVersion::Implicit1_0,
                                        reader.decoder(),
                                    )?
                                    .into_owned(),
                            );
                        }
                    }
                    let index = cases.len();
                    cases.push(case);
                    Some(index)
                } else {
                    None
                };
                if !empty {
                    ensure!(
                        stack.len() < MAX_XML_DEPTH,
                        "JUnit nesting exceeds {MAX_XML_DEPTH}"
                    );
                    stack.push(index);
                }
            }
            Event::End(_) => {
                ensure!(stack.pop().is_some(), "Unmatched JUnit closing element");
            }
            Event::Text(text) if stack.is_empty() => {
                ensure!(
                    text.as_ref().iter().all(u8::is_ascii_whitespace),
                    "Text outside JUnit root"
                );
            }
            Event::CData(_) if stack.is_empty() => bail!("CDATA outside JUnit root"),
            Event::GeneralRef(reference) => {
                ensure!(!stack.is_empty(), "Entity reference outside JUnit root");
                let reference = std::str::from_utf8(reference.as_ref())?;
                let entity = format!("&{reference};");
                quick_xml::escape::unescape(&entity)?;
            }
            Event::DocType(_) => bail!("JUnit report must not contain a DTD"),
            Event::Eof => {
                ensure!(
                    roots == 1 && stack.is_empty(),
                    "Incomplete JUnit XML document"
                );
                return Ok(cases);
            }
            _ => (),
        }
        buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture_model() -> Value {
        json!({"modules": [{"path": ":mobile", "variants": [{"name": "demoDebug", "components": [
            {"name": "demoDebugUnitTest", "scope": "unitTest", "sources": [{"kind": "java", "path": "/fixture/mobile/src/testDemo/java"}]},
            {"name": "demoDebugAndroidTest", "scope": "androidTest", "sources": [{"kind": "kotlin", "path": "/fixture/mobile/src/androidTest/java"}]}
        ]}]}]})
    }

    fn model_output(model: &Value) -> String {
        format!("Gradle chatter\n{MODEL_MARKER}{model}\nBUILD SUCCESSFUL\n")
    }

    #[test]
    fn selected_variant_source_predicates_are_preserved() {
        assert!(validate_model_output(&model_output(&fixture_model())).is_ok());
        for component in 0..2 {
            let mut missing = fixture_model();
            missing["modules"][0]["variants"][0]["components"][component]["sources"][0]["path"] =
                json!("src/main/java");
            assert!(validate_model_output(&model_output(&missing)).is_err());
            let mut excluded = fixture_model();
            excluded["modules"][0]["variants"][0]["components"][component]["sources"]
                .as_array_mut()
                .unwrap()
                .push(json!({"kind":"java","path":"src/testFull/java"}));
            assert!(validate_model_output(&model_output(&excluded)).is_err());
        }
    }

    #[test]
    fn model_record_and_component_selection_are_exact() {
        assert!(validate_model_output("no marker").is_err());
        let output = model_output(&fixture_model());
        assert!(validate_model_output(&(output.clone() + &output)).is_err());
        for (key, value) in [("name", "fullDebugUnitTest"), ("scope", "main")] {
            let mut model = fixture_model();
            model["modules"][0]["variants"][0]["components"][0][key] = json!(value);
            assert!(validate_model_output(&model_output(&model)).is_err());
        }
        let mut model = fixture_model();
        model["modules"][0]["variants"][0]["components"][0]["sources"][0]["kind"] = json!("res");
        assert!(validate_model_output(&model_output(&model)).is_err());
    }

    fn probe_cases(probe: &Probe) -> Vec<Case> {
        (0..probe.count)
            .map(|index| Case {
                failure: index < probe.failures,
                skipped: index >= probe.count - probe.skipped,
                ..Case::default()
            })
            .collect()
    }

    fn probe_events(probe: &Probe) -> Vec<Value> {
        (0..probe.count).map(|index| if probe.failure && index == 0 { json!({"status":"Failed", "detail":"at dev.zed.androidsample.RunnerTest.optionalFailure(RunnerTest.java:42)"}) } else { json!({"status":"Passed", "detail":""}) }).collect()
    }

    #[test]
    fn every_original_unit_case_checks_all_four_counts() {
        for probe in &PROBES {
            let mut cases = probe_cases(probe);
            let mut events = probe_events(probe);
            assert!(
                validate_unit_probe(probe, &cases, &events).is_ok(),
                "{}",
                probe.selector
            );
            cases.push(Case::default());
            assert!(
                validate_unit_probe(probe, &cases, &events).is_err(),
                "case count: {}",
                probe.selector
            );
            cases.pop();
            cases[0].failure = !cases[0].failure;
            assert!(
                validate_unit_probe(probe, &cases, &events).is_err(),
                "failure count: {}",
                probe.selector
            );
            cases[0].failure = !cases[0].failure;
            cases[0].skipped = !cases[0].skipped;
            assert!(
                validate_unit_probe(probe, &cases, &events).is_err(),
                "skipped count: {}",
                probe.selector
            );
            cases[0].skipped = !cases[0].skipped;
            events.pop();
            assert!(
                validate_unit_probe(probe, &cases, &events).is_err(),
                "event count: {}",
                probe.selector
            );
        }
    }

    #[test]
    fn failed_event_must_include_original_java_source_location() {
        let probe = &PROBES[2];
        let cases = probe_cases(probe);
        let mut events = probe_events(probe);
        events[0]["detail"] = json!("generic failure");
        assert!(validate_unit_probe(probe, &cases, &events).is_err());
        events[0]["detail"] = json!("RunnerTest.java:42");
        events[0]["status"] = json!("Passed");
        assert!(validate_unit_probe(probe, &cases, &events).is_err());
        events[0]["status"] = json!("Failed");
        assert!(validate_unit_probe(probe, &cases, &events).is_ok());
    }

    #[test]
    fn device_keeps_all_three_original_predicates() {
        assert!(validate_device_probe(&[Case::default()]).is_ok());
        assert!(validate_device_probe(&[]).is_err());
        assert!(validate_device_probe(&[Case::default(), Case::default()]).is_err());
        assert!(
            validate_device_probe(&[Case {
                failure: true,
                ..Case::default()
            }])
            .is_err()
        );
        assert!(
            validate_device_probe(&[Case {
                error: true,
                ..Case::default()
            }])
            .is_err()
        );
    }

    #[test]
    fn gradle_exit_must_match_expected_failure() {
        assert!(validate_exit(false, false, "unit", "0").is_ok());
        assert!(validate_exit(true, true, "unit", "1").is_ok());
        assert!(validate_exit(false, true, "unit", "0").is_err());
        assert!(validate_exit(true, false, "device", "1").is_err());
        assert_eq!(TIMEOUT, Duration::from_secs(600));
    }

    #[test]
    fn gradle_command_flags_and_selectors_match_original() {
        let arguments = probe_arguments(
            "testDemoDebugUnitTest",
            Path::new("/out/runner.gradle"),
            Path::new("/out"),
            &["Class.method", "Suite"],
            true,
            false,
        );
        assert_eq!(
            arguments,
            [
                ":mobile:testDemoDebugUnitTest",
                "--init-script",
                "/out/runner.gradle",
                "-Dkoda.test.module=:mobile",
                "-Dkoda.test.variant=demoDebug",
                "-Dkoda.test.output=/out",
                "-Dkoda.test.kind=unit",
                "--console=plain",
                "--no-configuration-cache",
                "--no-daemon",
                "--tests",
                "Class.method",
                "--tests",
                "Suite",
                "-Pkoda.fixture.fail=true"
            ]
            .map(OsString::from)
        );
        let device = probe_arguments(
            "connectedDemoDebugAndroidTest",
            Path::new("/out/runner.gradle"),
            Path::new("/out"),
            &["Class#method", "Other"],
            false,
            true,
        );
        assert_eq!(
            device,
            [
                ":mobile:connectedDemoDebugAndroidTest",
                "--init-script",
                "/out/runner.gradle",
                "-Dkoda.test.module=:mobile",
                "-Dkoda.test.variant=demoDebug",
                "-Dkoda.test.output=/out",
                "-Dkoda.test.kind=device",
                "--console=plain",
                "--no-configuration-cache",
                "--no-daemon",
                "--rerun-tasks",
                "-Pandroid.testInstrumentationRunnerArguments.class=Class#method,Other"
            ]
            .map(OsString::from)
        );
        assert_eq!(
            model_arguments(Path::new("/out/model.gradle")),
            [
                "--init-script",
                "/out/model.gradle",
                "kodaAndroidProjectModel",
                "--no-configuration-cache",
                "--no-daemon",
                "--console=plain"
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn script_extraction_preserves_bytes_and_rejects_missing_delimiters() {
        assert_eq!(
            extract_init_script(
                "prefix\npub const INIT_SCRIPT: &str = r#\"\r\nfixture # \" quoted\n\"#;\nsuffix"
            )
            .unwrap(),
            "\r\nfixture # \" quoted\n"
        );
        assert!(extract_init_script("no initializer").is_err());
        assert!(extract_init_script("pub const INIT_SCRIPT: &str = r#\"missing end").is_err());
    }

    #[test]
    fn junit_descendants_and_direct_child_status_match_element_tree() {
        let cases = parse_report("<testsuites><testsuite><testcase name='pass'/><testcase name='both'><failure/><error/></testcase><testcase name='skip'><skipped/></testcase><testcase name='nested'><detail><failure/></detail></testcase></testsuite></testsuites>", 10).unwrap();
        assert_eq!(cases.len(), 4);
        assert_eq!(cases.iter().filter(|case| case.failed()).count(), 1);
        assert_eq!(cases.iter().filter(|case| case.skipped).count(), 1);
        assert_eq!(cases[0].attributes["name"], "pass");
        assert!(!cases[3].failed());
    }

    #[test]
    fn junit_namespace_names_are_not_unqualified_element_tree_matches() {
        let cases = parse_report("<testsuite xmlns:n='urn:test'><n:testcase/><testcase><n:failure/></testcase><group xmlns='urn:test'><testcase/></group></testsuite>", 10).unwrap();
        assert_eq!(cases.len(), 1);
        assert!(!cases[0].failed());
    }

    #[test]
    fn junit_truncation_multiple_roots_and_limits_are_errors() {
        assert!(parse_report("<testsuite><testcase>", 10).is_err());
        assert!(parse_report("<testsuite/><testsuite/>", 10).is_err());
        assert!(parse_report("<testsuite><testcase></testsuite></testcase>", 10).is_err());
        assert!(parse_report("<testsuite><unknown:testcase/></testsuite>", 10).is_err());
        assert!(parse_report("<testsuite><testcase/><testcase/></testsuite>", 1).is_err());
        assert!(
            parse_report(
                "<!DOCTYPE testsuite SYSTEM 'file:///untrusted'><testsuite/>",
                10
            )
            .is_err()
        );
        assert!(
            parse_report(
                &format!(
                    "{}{}",
                    "<x>".repeat(MAX_XML_DEPTH + 1),
                    "</x>".repeat(MAX_XML_DEPTH + 1)
                ),
                10
            )
            .is_err()
        );
    }

    #[test]
    fn nested_reports_events_and_fresh_temp_directories_are_used() {
        let first = tempfile::Builder::new()
            .prefix("koda-runner-probe-")
            .tempdir()
            .unwrap();
        let second = tempfile::Builder::new()
            .prefix("koda-runner-probe-")
            .tempdir()
            .unwrap();
        assert_ne!(first.path(), second.path());
        fs::create_dir(first.path().join("nested")).unwrap();
        fs::write(
            first.path().join("TEST-a.xml"),
            "<testsuite><testcase/></testsuite>",
        )
        .unwrap();
        fs::write(
            first.path().join("nested/TEST-b.xml"),
            "<testsuite><testcase><error/></testcase></testsuite>",
        )
        .unwrap();
        fs::write(first.path().join("ignored.xml"), "not XML").unwrap();
        assert_eq!(read_reports(first.path()).unwrap().len(), 2);
        let events = first.path().join("events.jsonl");
        fs::write(
            &events,
            "{\"status\":\"Passed\"}\n{\"status\":\"Failed\"}\n",
        )
        .unwrap();
        assert_eq!(read_events(&events).unwrap().len(), 2);
        fs::write(&events, "{\"status\":\"Passed\"}\n\n").unwrap();
        assert!(read_events(&events).is_err());
    }

    #[test]
    fn arguments_preserve_selected_device_and_no_device_contract() {
        let args = |values: &[&str]| {
            values
                .iter()
                .map(|value| OsString::from(*value))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            parse_arguments(&args(&["--root", "/repo"])).unwrap().device,
            None
        );
        assert_eq!(
            parse_arguments(&args(&[
                "--root", "/repo", "--device", "serial1", "--device", "serial2"
            ]))
            .unwrap()
            .device
            .as_deref(),
            Some("serial2")
        );
        assert_eq!(
            parse_arguments(&args(&["--root", "/repo", "--device", ""]))
                .unwrap()
                .device,
            None
        );
        assert!(parse_arguments(&args(&["--root"])).is_err());
        assert!(parse_arguments(&args(&["--root", "/repo", "--unknown", "x"])).is_err());
        assert_eq!(
            DEVICE_NOT_RUN,
            "NOT RUN: device/instrumentation validation (--device required)"
        );
    }

    #[test]
    fn input_size_gate_refuses_before_reading_oversized_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("oversize");
        File::create(&path).unwrap().set_len(9).unwrap();
        assert!(read_text(&path, 8).is_err());
    }

    #[test]
    fn missing_model_fields_do_not_silently_bypass_python_key_errors() {
        let mut model = fixture_model();
        model["modules"][0]["variants"][0]["components"][0]["sources"]
            .as_array_mut()
            .unwrap()
            .insert(0, json!({"path":"ignored"}));
        assert!(validate_model_output(&model_output(&model)).is_err());
        let mut model = fixture_model();
        model["modules"][0]["variants"][0]["components"]
            .as_array_mut()
            .unwrap()
            .insert(0, json!({"scope":"main"}));
        assert!(validate_model_output(&model_output(&model)).is_err());
    }

    #[test]
    fn failed_event_missing_keys_are_errors_until_first_source_match() {
        let probe = &PROBES[2];
        let cases = probe_cases(probe);
        let mut events = probe_events(probe);
        events[0] = json!({"status":"Failed"});
        events[1] = json!({"status":"Failed", "detail":"RunnerTest.java:42"});
        assert!(validate_unit_probe(probe, &cases, &events).is_err());
        events[0] = json!({});
        assert!(validate_unit_probe(probe, &cases, &events).is_err());
        events[0] = json!({"status":"Failed", "detail":"RunnerTest.java:42"});
        events[1] = json!({});
        assert!(validate_unit_probe(probe, &cases, &events).is_ok());
    }

    #[test]
    fn device_serial_is_only_added_to_the_selected_probe_environment() {
        let command = prepare_probe_command(
            Path::new("/fixture"),
            "connectedDemoDebugAndroidTest",
            Path::new("/out/runner.gradle"),
            Path::new("/out"),
            &["Class#method"],
            false,
            Some("physical-device"),
        );
        assert_eq!(command.get_current_dir(), Some(Path::new("/fixture")));
        assert_eq!(
            command
                .get_envs()
                .find(|(name, _)| *name == "ANDROID_SERIAL")
                .and_then(|(_, value)| value),
            Some(std::ffi::OsStr::new("physical-device"))
        );
        let unit = prepare_probe_command(
            Path::new("/fixture"),
            "testDemoDebugUnitTest",
            Path::new("/out/runner.gradle"),
            Path::new("/out"),
            &["Class.method"],
            false,
            None,
        );
        assert!(!unit.get_envs().any(|(name, _)| name == "ANDROID_SERIAL"));
    }

    #[test]
    fn malformed_junit_entities_and_outside_cdata_are_errors() {
        assert!(
            parse_report(
                "<testsuite><testcase><failure>&undefined;</failure></testcase></testsuite>",
                10
            )
            .is_err()
        );
        assert!(parse_report("<testsuite/><![CDATA[outside]]>", 10).is_err());
        assert!(
            parse_report(
                "<testsuite><testcase><failure>&amp;&#65;</failure></testcase></testsuite>",
                10
            )
            .is_ok()
        );
    }

    #[test]
    fn unexpected_gradle_result_keeps_the_original_numeric_message() {
        assert_eq!(
            validate_exit(true, false, "testDemoDebugUnitTest", "1")
                .unwrap_err()
                .to_string(),
            "Unexpected Gradle result for testDemoDebugUnitTest: 1"
        );
    }

    #[cfg(unix)]
    #[test]
    fn repository_argument_preserves_non_unicode_paths() {
        use std::os::unix::ffi::OsStringExt;
        let root = OsString::from_vec(b"/repo-\xff".to_vec());
        let args = vec![OsString::from("--root"), root.clone()];
        assert_eq!(parse_arguments(&args).unwrap().root, PathBuf::from(root));
    }
}
