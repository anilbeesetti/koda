//! Root-run actual JVM fixture; it is supplemental and adds no reference parity.
use android_tools::kotlin_getter_lifetime::OwnedGradleRuntime;
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
            "{}\n{}",
            &bridge[..boundary],
            include_str!("../test_data/kotlin_import_facts/bridge_shared_collections.gradle")
        ),
    )?;
    let output_path = directory.path().join("stdout.log");
    let error_path = directory.path().join("stderr.log");
    let mut command = Command::new(&arguments[0]);
    command
        .current_dir(&arguments[1])
        .env("JAVA_HOME", &arguments[2])
        .args(["--no-daemon", "--console=plain", "--init-script"])
        .arg(&script)
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
