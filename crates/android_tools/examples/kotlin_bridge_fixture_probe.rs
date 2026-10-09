//! Root-run actual JVM fixture; it is supplemental and adds no reference parity.
use android_tools::kotlin_getter_lifetime::OwnedGradleRuntime;
use android_tools::kotlin_import_facts::CaptureLimits;
use anyhow::{Context as _, Result, bail, ensure};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn main() -> Result<()> {
    let arguments = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    ensure!(
        arguments.len() == 5,
        "Expected wrapper, fixture project root containing :first and :second, JAVA_HOME, guardian launcher, guardian library"
    );
    let directory = tempfile::Builder::new()
        .prefix("koda-shared-list-probe-")
        .tempdir()?;
    let bridge = include_str!("../src/kotlin_getter_capture.gradle");
    let boundary = bridge
        .find("\ngradle.projectsEvaluated {")
        .context("Production bridge class boundary")?;
    let script = directory.path().join("shared-list.gradle");
    fs::write(
        &script,
        format!(
            "{}\n{}\n{}",
            &bridge[..boundary],
            include_str!("../test_data/kotlin_import_facts/bridge_shared_collections.gradle"),
            include_str!("../test_data/kotlin_import_facts/bridge_producer_limits.gradle")
        ),
    )?;
    let output_path = directory.path().join("stdout.log");
    let error_path = directory.path().join("stderr.log");
    let mut command = Command::new(&arguments[0]);
    let limits = CaptureLimits::default();
    command
        .current_dir(&arguments[1])
        .env("JAVA_HOME", &arguments[2])
        .args(["--no-daemon", "--console=plain", "--init-script"])
        .arg(&script)
        .arg("-Dkoda.kotlin.capture.timeoutMillis=180000")
        .arg(format!(
            "-Dkoda.kotlin.capture.maximumFrameBytes={}",
            limits.record_bytes
        ))
        .arg(format!(
            "-Dkoda.kotlin.capture.maximumValueNodes={}",
            limits.entries
        ))
        .arg(format!(
            "-Dkoda.kotlin.capture.maximumScalarBytes={}",
            limits.string_bytes
        ))
        .arg("help");
    let mut runtime = OwnedGradleRuntime::spawn(
        command,
        &arguments[3],
        &arguments[4],
        Stdio::from(fs::File::create(&output_path)?),
        Stdio::from(fs::File::create(&error_path)?),
        Duration::from_secs(30),
    )?;
    let deadline = Instant::now() + Duration::from_secs(180);
    let result = (|| -> Result<()> {
        loop {
            ensure!(
                Instant::now() < deadline,
                "Shared-list fixture exceeded its deadline"
            );
            ensure!(
                fs::metadata(&output_path)?.len() + fs::metadata(&error_path)?.len()
                    <= 16 * 1024 * 1024,
                "Shared-list fixture diagnostics exceeded their budget"
            );
            runtime.health_check()?;
            if let Some(status) = runtime.try_wait()? {
                ensure!(
                    status.success(),
                    "Actual Gradle bridge fixture failed: {status}; stderr: {}",
                    fs::read_to_string(&error_path)?
                );
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        runtime.close()?;
        let output = fs::read_to_string(&output_path)?;
        let lines = output
            .lines()
            .filter_map(|line| line.strip_prefix("KODA_KOTLIN_SHARED_COLLECTIONS_FIXTURE="))
            .collect::<Vec<_>>();
        ensure!(lines.len() == 1, "Expected one actual JVM fixture record");
        let value: serde_json::Value =
            serde_json::from_str(lines.first().context("Fixture record")?)?;
        let cases = value
            .get("cases")
            .and_then(serde_json::Value::as_array)
            .context("Actual fixture cases")?;
        ensure!(
            cases.len() == 2,
            "Expected empty and ordered shared-list cases"
        );
        ensure!(
            value.get("sequence").and_then(serde_json::Value::as_u64) == Some(6)
                && value.get("events").and_then(serde_json::Value::as_u64) == Some(8),
            "Successful deltas and rejected receiver attempts changed"
        );
        for case in cases {
            for flag in [
                "exactPhysicalIdentity",
                "wrongKindRejected",
                "foreignReceiverRejected",
            ] {
                ensure!(
                    case.get(flag).and_then(serde_json::Value::as_bool) == Some(true),
                    "Fixture failed {flag}"
                );
            }
            ensure!(
                case["first"]["id"] != case["second"]["id"]
                    && case["first"]["id"] == case["repeated"],
                "Project handles were not scoped/stable"
            );
            ensure!(
                case["first"]["classId"] == case["second"]["classId"]
                    && case["runtimeClass"]["id"] == case["first"]["classId"]
                    && case["runtimeClass"]["loader"] == case["classLoader"]["id"],
                "Shared object class/loader provenance changed"
            );
            ensure!(
                case["first"]["project"] == ":first" && case["second"]["project"] == ":second",
                "Imported project scopes changed"
            );
        }
        ensure!(
            cases[0]["values"] == serde_json::json!([]),
            "Empty list changed"
        );
        ensure!(
            cases[1]["values"] == serde_json::json!(["-Xfirst", "-Xsecond"]),
            "Ordered values changed"
        );
        validate_producer_fixture(&output)?;
        validate_uncaught_producer_failure(&arguments, &script, directory.path())?;
        println!(
            "KODA_KOTLIN_SHARED_COLLECTIONS_FIXTURE={}",
            serde_json::to_string(&value)?
        );
        Ok(())
    })();
    let closure = runtime.close();
    match (result, closure) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(closure)) => {
            bail!("{error:#}; owned fixture closure also failed: {closure:#}")
        }
    }
}

fn validate_producer_fixture(output: &str) -> Result<()> {
    let records = output
        .lines()
        .filter_map(|line| line.strip_prefix("KODA_KOTLIN_PRODUCER_LIMITS_FIXTURE="))
        .collect::<Vec<_>>();
    ensure!(
        records.len() == 1,
        "Expected one actual producer-limit fixture record"
    );
    let value: serde_json::Value =
        serde_json::from_str(records.first().context("Producer fixture record")?)?;
    let cases = value
        .get("cases")
        .and_then(serde_json::Value::as_array)
        .context("Producer cases")?;
    ensure!(
        cases.len() == 11,
        "Expected all eleven producer-boundary cases"
    );
    let mut indexed = std::collections::BTreeMap::new();
    for case in cases {
        let name = case
            .get("name")
            .and_then(serde_json::Value::as_str)
            .context("Producer case name")?;
        ensure!(
            indexed.insert(name, case).is_none(),
            "Duplicate producer case"
        );
        if name != "exactBoundaryAndUnicode" {
            ensure!(
                case.get("outputBytes").and_then(serde_json::Value::as_u64) == Some(0),
                "Rejected producer published bytes: {name}"
            );
        }
    }
    let limits = CaptureLimits::default();
    for name in ["lazyStrings", "lazyObjects"] {
        let case = indexed.get(name).context("Lazy iterable case")?;
        ensure!(
            case["limit"].as_u64() == Some(limits.entries as u64)
                && case["visited"].as_u64() == Some((limits.entries - 1) as u64),
            "Lazy source was fully expanded or the original node limit changed"
        );
        ensure!(
            case["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("value-node budget")),
            "Wrong lazy rejection cause"
        );
    }
    ensure!(
        indexed.get("lazyStrings").context("Lazy String case")?["newObjects"].as_u64() == Some(0),
        "Rejected lazy List retained a container"
    );
    let giant = indexed.get("giantScalar").context("Giant scalar case")?;
    ensure!(
        giant["inputCharacters"].as_u64() == Some(limits.string_bytes as u64 + 1)
            && giant["limit"].as_u64() == Some(limits.string_bytes as u64),
        "Giant scalar did not exercise the real default boundary"
    );
    ensure!(
        indexed.get("cyclic").context("Cyclic case")?["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("Cyclic")),
        "Cyclic structure was not rejected"
    );
    let boundary = indexed
        .get("exactBoundaryAndUnicode")
        .context("Exact boundary case")?;
    let raw = boundary["raw"]
        .as_str()
        .context("Actual bounded encoded frame")?;
    let frame: serde_json::Value = serde_json::from_str(raw)?;
    ensure!(
        frame["kind"] == "event"
            && frame["value"]["ordered"]
                == serde_json::json!([
                    "quote:\" slash:\\ control:\u{0} euro:€ emoji:😀",
                    "",
                    "last"
                ]),
        "Escaped/non-ASCII values or order changed"
    );
    ensure!(
        boundary["bytes"].as_u64() == Some(raw.len() as u64)
            && boundary["nodes"].as_u64() == Some(7),
        "Exact encoded byte/node accounting changed"
    );
    ensure!(
        boundary["oneByteLowerRejected"] == true && boundary["oneNodeLowerRejected"] == true,
        "One-below resource boundary was accepted"
    );
    let cumulative = indexed
        .get("cumulativeObservations")
        .context("Cumulative producer case")?;
    let before = cumulative["beforeNodes"]
        .as_u64()
        .context("Original observation usage")?;
    let row = cumulative["rowNodes"]
        .as_u64()
        .context("Row observation usage")?;
    ensure!(
        cumulative["newObjects"].as_u64() == Some(3)
            && cumulative["afterNodes"].as_u64() == Some(before + 3 * row)
            && cumulative["limit"] == cumulative["afterNodes"],
        "Discovery exceeded its cumulative admission budget"
    );
    for (name, expected) in [
        ("deadline", "deadline elapsed"),
        ("interruption", "interrupted"),
    ] {
        ensure!(
            indexed.get(name).context("Producer health case")?["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains(expected)),
            "Producer health cause changed"
        );
    }
    let escaped = indexed
        .get("escapedOversize")
        .context("Escape-heavy producer case")?;
    ensure!(
        escaped["inputCharacters"].as_u64() == Some(5000)
            && escaped["encodedBytes"].as_u64() == Some(10000)
            && escaped["allocatedBytes"]
                .as_u64()
                .is_some_and(|bytes| bytes <= 10000),
        "Encoder allocated beyond the bounded byte capacity"
    );
    let cancelling = indexed
        .get("deadlineDuringIteration")
        .context("Mid-iteration deadline case")?;
    ensure!(
        cancelling["visited"].as_u64() == Some(64)
            && cancelling["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("deadline elapsed")),
        "Producer missed cancellation inside expansion"
    );
    ensure!(
        indexed.contains_key("noPartialDeltaEvent"),
        "Producer publication case missing"
    );
    println!(
        "KODA_KOTLIN_PRODUCER_LIMITS_FIXTURE={}",
        serde_json::to_string(&value)?
    );
    Ok(())
}

fn validate_uncaught_producer_failure(
    arguments: &[PathBuf],
    script: &std::path::Path,
    directory: &std::path::Path,
) -> Result<()> {
    let output = directory.join("producer-failure-stdout.log");
    let errors = directory.join("producer-failure-stderr.log");
    let limits = CaptureLimits::default();
    let mut command = Command::new(arguments.first().context("Wrapper path")?);
    command
        .current_dir(arguments.get(1).context("Fixture root")?)
        .env("JAVA_HOME", arguments.get(2).context("Java home")?)
        .args(["--no-daemon", "--console=plain", "--init-script"])
        .arg(script)
        .arg("-Dkoda.kotlin.capture.timeoutMillis=180000")
        .arg(format!(
            "-Dkoda.kotlin.capture.maximumFrameBytes={}",
            limits.record_bytes
        ))
        .arg(format!(
            "-Dkoda.kotlin.capture.maximumValueNodes={}",
            limits.entries
        ))
        .arg(format!(
            "-Dkoda.kotlin.capture.maximumScalarBytes={}",
            limits.string_bytes
        ))
        .arg("-Dkoda.kotlin.fixture.producer.uncaught=true")
        .arg("help");
    let mut runtime = OwnedGradleRuntime::spawn(
        command,
        arguments.get(3).context("Guardian launcher")?,
        arguments.get(4).context("Guardian library")?,
        Stdio::from(fs::File::create(&output)?),
        Stdio::from(fs::File::create(&errors)?),
        Duration::from_secs(30),
    )?;
    let deadline = Instant::now() + Duration::from_secs(180);
    let result = (|| -> Result<()> {
        loop {
            ensure!(
                Instant::now() < deadline,
                "Producer failure probe deadline elapsed"
            );
            ensure!(
                fs::metadata(&output)?.len() + fs::metadata(&errors)?.len() <= 16 * 1024 * 1024,
                "Producer failure diagnostics exceeded budget"
            );
            runtime.health_check()?;
            if let Some(status) = runtime.try_wait()? {
                ensure!(
                    !status.success(),
                    "Oversized producer unexpectedly completed successfully"
                );
                let errors = fs::read_to_string(&errors)?;
                ensure!(
                    errors.contains("Producer iterable value-node budget exceeded"),
                    "Original producer rejection cause missing: {errors}"
                );
                runtime.close()?;
                println!("KODA_KOTLIN_PRODUCER_FAILURE_VERIFIED_CLOSURE=true");
                return Ok(());
            }
            thread::sleep(Duration::from_millis(10));
        }
    })();
    let closure = runtime.close();
    match (result, closure) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(closure)) => {
            bail!("{error:#}; owned producer failure closure also failed: {closure:#}")
        }
    }
}
