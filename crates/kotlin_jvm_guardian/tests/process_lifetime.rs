#[cfg(not(target_os = "linux"))]
#[test]
fn unsupported_containment_is_reported_before_starting_a_wrapper() {
    use android_tools::kotlin_getter_lifetime::{CapabilityUnavailable, OwnedGradleRuntime};
    use std::{
        path::Path,
        process::{Command, Stdio},
        time::Duration,
    };

    let outcome = OwnedGradleRuntime::spawn(
        Command::new("this-wrapper-must-never-be-started"),
        Path::new(env!("CARGO_BIN_EXE_kotlin_jvm_guardian")),
        Path::new(env!("CARGO_BIN_EXE_kotlin_jvm_guardian_process_fixture")),
        Stdio::null(),
        Stdio::null(),
        Duration::from_secs(5),
    );
    let error = match outcome {
        Ok(_) => panic!("Unsupported containment started a wrapper"),
        Err(error) => error,
    };
    assert!(error.downcast_ref::<CapabilityUnavailable>().is_some());
}

#[cfg(target_os = "linux")]
mod linux {
    use android_tools::{
        kotlin_getter_executor::{GetterTransport as _, GradleCaptureOptions, GradleTransport},
        kotlin_getter_lifetime::OwnedGradleRuntime,
    };
    use kotlin_jvm_guardian::{CONFIG_ENV, Configuration};
    use std::{
        fs,
        os::unix::fs::MetadataExt as _,
        path::{Path, PathBuf},
        process::{Child, Command, ExitStatus, Stdio},
        sync::{Arc, atomic::AtomicBool},
        thread,
        time::{Duration, Instant},
    };

    const SHUTDOWN: Duration = Duration::from_secs(5);

    fn launcher() -> &'static Path {
        Path::new(env!("CARGO_BIN_EXE_kotlin_jvm_guardian"))
    }

    fn fixture() -> &'static Path {
        Path::new(env!("CARGO_BIN_EXE_kotlin_jvm_guardian_process_fixture"))
    }

    fn library() -> PathBuf {
        let parent = launcher().parent().expect("Cargo binary directory");
        let mut artifacts = std::collections::BTreeMap::new();
        for directory in [parent.to_owned(), parent.join("deps")] {
            for entry in fs::read_dir(directory).expect("Cargo artifact directory") {
                let entry = entry.expect("Read Cargo artifact row");
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if (name == "libkotlin_jvm_guardian.so"
                    || name.starts_with("libkotlin_jvm_guardian-"))
                    && name.ends_with(".so")
                {
                    let path = entry
                        .path()
                        .canonicalize()
                        .expect("Resolve compiled guardian");
                    let metadata = fs::metadata(&path).expect("Compiled guardian metadata");
                    if metadata.is_file() {
                        artifacts
                            .entry((metadata.dev(), metadata.ino()))
                            .or_insert(path);
                    }
                }
            }
        }
        assert_eq!(
            artifacts.len(),
            1,
            "Require the current Cargo-built guardian cdylib, without guessing between stale artifacts"
        );
        artifacts
            .into_values()
            .next()
            .expect("Compiled guardian library")
    }

    fn command(mode: &str, ready: &Path) -> Command {
        let mut command = Command::new(fixture());
        command
            .env("KODA_GUARDIAN_FIXTURE_MODE", mode)
            .env("KODA_GUARDIAN_FIXTURE_READY", ready);
        command
    }

    fn start_runtime(command: Command) -> OwnedGradleRuntime {
        OwnedGradleRuntime::spawn(
            command,
            launcher(),
            &library(),
            Stdio::null(),
            Stdio::null(),
            SHUTDOWN,
        )
        .expect("Start real Rust containment launcher")
    }

    fn ready_pid(path: &Path) -> u32 {
        let deadline = Instant::now() + SHUTDOWN;
        loop {
            if let Ok(contents) = fs::read_to_string(path)
                && let Ok(pid) = contents.parse::<u32>()
            {
                assert!(pid > 1);
                return pid;
            }
            assert!(Instant::now() < deadline, "Fixture did not become ready");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn process_start(pid: u32) -> Option<u64> {
        match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(status) => {
                let (_, fields) = status
                    .rsplit_once(") ")
                    .expect("Linux process status fields");
                Some(
                    fields
                        .split_ascii_whitespace()
                        .nth(19)
                        .expect("Linux start ticks")
                        .parse()
                        .expect("Parse Linux start ticks"),
                )
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("Read owned fixture process status: {error}"),
        }
    }

    fn assert_gone(pid: u32, start: Option<u64>) {
        let deadline = Instant::now() + SHUTDOWN;
        loop {
            let current = process_start(pid);
            if current.is_none() || start.is_some() && current != start {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Owned fixture {pid} remained after closure"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_status(runtime: &mut OwnedGradleRuntime) -> ExitStatus {
        let deadline = Instant::now() + SHUTDOWN;
        loop {
            if let Some(status) = runtime
                .try_wait()
                .expect("Observe verified launcher completion")
            {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "Containment launcher did not complete"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    struct Unrelated(Child);

    impl Drop for Unrelated {
        fn drop(&mut self) {
            match self.0.try_wait() {
                Ok(Some(_)) => {}
                Ok(None) => {
                    if let Err(error) = self.0.kill() {
                        eprintln!("Unable to stop test-owned unrelated fixture: {error}");
                    }
                    if let Err(error) = self.0.wait() {
                        eprintln!("Unable to reap test-owned unrelated fixture: {error}");
                    }
                }
                Err(error) => eprintln!("Unable to inspect unrelated fixture: {error}"),
            }
        }
    }

    #[test]
    fn cancellation_closes_registered_detached_rust_guardian_peer_and_preserves_unrelated_process()
    {
        let directory = tempfile::tempdir().expect("Fixture directory");
        let unrelated_ready = directory.path().join("unrelated");
        let mut unrelated = Unrelated(
            command("unrelated", &unrelated_ready)
                .spawn()
                .expect("Unrelated Rust fixture"),
        );
        ready_pid(&unrelated_ready);
        let ready = directory.path().join("detached");
        let mut launch = command("parent-blocking", &ready);
        launch.env("KODA_GUARDIAN_FIXTURE_CHILD_MODE", "detached-guardian");
        let mut runtime = start_runtime(launch);
        let pid = ready_pid(&ready);
        let start = process_start(pid).expect("Detached fixture was alive");
        runtime
            .close()
            .expect("Verified detached Rust guardian peer closure");
        runtime.close().expect("Repeated closure is idempotent");
        assert_gone(pid, Some(start));
        assert!(
            unrelated
                .0
                .try_wait()
                .expect("Inspect unrelated fixture")
                .is_none()
        );
    }

    #[test]
    fn cancellation_closes_detached_starter_before_native_agent_registration() {
        let directory = tempfile::tempdir().expect("Fixture directory");
        let ready = directory.path().join("detached");
        let mut launch = command("parent-blocking", &ready);
        launch.env("KODA_GUARDIAN_FIXTURE_CHILD_MODE", "detached-unregistered");
        let mut runtime = start_runtime(launch);
        let pid = ready_pid(&ready);
        let start = process_start(pid).expect("Unregistered detached starter was alive");
        runtime
            .close()
            .expect("Subreaper establishes pre-registration closure");
        assert_gone(pid, Some(start));
    }

    #[test]
    fn dropping_runtime_closes_a_detached_pre_registration_starter() {
        let directory = tempfile::tempdir().expect("Fixture directory");
        let ready = directory.path().join("detached");
        let mut launch = command("parent-blocking", &ready);
        launch.env("KODA_GUARDIAN_FIXTURE_CHILD_MODE", "detached-unregistered");
        let runtime = start_runtime(launch);
        let pid = ready_pid(&ready);
        let start = process_start(pid).expect("Detached starter was alive");
        drop(runtime);
        assert_gone(pid, Some(start));
    }

    #[test]
    fn immediate_close_and_drop_before_launcher_registration_are_bounded_and_reap_the_launcher() {
        for close_explicitly in [true, false] {
            let directory = tempfile::tempdir().expect("Fixture directory");
            let mut runtime = start_runtime(command("blocking", &directory.path().join("ready")));
            let pid = runtime.launcher_id();
            let start = process_start(pid).expect("Owned launcher was alive");
            let began = Instant::now();
            if close_explicitly {
                runtime.close().expect(
                    "Cancellation before launcher authentication still reports verified closure",
                );
            }
            drop(runtime);
            assert!(began.elapsed() < SHUTDOWN);
            assert_gone(pid, Some(start));
        }
    }

    #[test]
    fn wrapper_exit_closes_detached_children_before_reporting_success() {
        let directory = tempfile::tempdir().expect("Fixture directory");
        let ready = directory.path().join("detached");
        let mut launch = command("parent-exits", &ready);
        launch.env("KODA_GUARDIAN_FIXTURE_CHILD_MODE", "detached-unregistered");
        let mut runtime = start_runtime(launch);
        let pid = ready_pid(&ready);
        let start = process_start(pid);
        assert!(wait_status(&mut runtime).success());
        runtime.close().expect("Verified completion closure");
        assert_gone(pid, start);
    }

    #[test]
    fn wrapper_exit_status_and_failed_spawn_survive_descendant_cleanup() {
        let directory = tempfile::tempdir().expect("Fixture directory");
        let mut launch = command("exit-status", &directory.path().join("unused"));
        launch.env("KODA_GUARDIAN_FIXTURE_EXIT_CODE", "23");
        let mut runtime = start_runtime(launch);
        assert_eq!(wait_status(&mut runtime).code(), Some(23));
        runtime
            .close()
            .expect("Nonzero wrapper still has verified closure");

        let mut missing = start_runtime(Command::new(directory.path().join("missing-wrapper")));
        assert!(!wait_status(&mut missing).success());
        missing
            .close()
            .expect("Failed wrapper spawn has no descendants");
    }

    #[test]
    fn unowned_authenticated_invalid_and_late_peers_are_rejected_without_signalling_them() {
        let directory = tempfile::tempdir().expect("Fixture directory");
        let ready = directory.path().join("wrapper");
        let configuration_path = directory.path().join("private-config");
        let mut launch = command("blocking", &ready);
        launch.env("KODA_GUARDIAN_FIXTURE_CONFIG_FILE", &configuration_path);
        let mut runtime = start_runtime(launch);
        ready_pid(&ready);
        let configuration = Configuration::parse(
            &fs::read_to_string(configuration_path)
                .expect("Read test-owned invocation configuration"),
        )
        .expect("Decode private invocation configuration");
        let mut foreign = Vec::new();
        let mut invalid = configuration.clone();
        invalid.token[0] ^= 1;
        for (index, actual) in [configuration.clone(), invalid].into_iter().enumerate() {
            let ready = directory.path().join(format!("foreign-{index}"));
            let mut launch = command("rejected-guardian", &ready);
            launch.env(CONFIG_ENV, actual.encode());
            let mut child = Unrelated(
                launch
                    .spawn()
                    .expect("Start unowned authentication fixture"),
            );
            ready_pid(&ready);
            assert!(
                child
                    .0
                    .try_wait()
                    .expect("Authentication must not signal the foreign process")
                    .is_none()
            );
            foreign.push(child);
        }
        runtime.close().expect("Close only the owned invocation");
        let ready = directory.path().join("late");
        let mut launch = command("rejected-guardian", &ready);
        launch.env(CONFIG_ENV, configuration.encode());
        let mut late = Unrelated(launch.spawn().expect("Late foreign startup fixture"));
        ready_pid(&ready);
        assert!(
            late.0
                .try_wait()
                .expect("Late foreign fixture preserved")
                .is_none()
        );
        for child in &mut foreign {
            assert!(
                child
                    .0
                    .try_wait()
                    .expect("Foreign fixture preserved after cancellation")
                    .is_none()
            );
        }
    }

    fn capture(directory: &Path, maximum: u64) -> GradleTransport {
        GradleTransport::start(&GradleCaptureOptions {
            wrapper: fixture().to_owned(),
            project_root: directory.to_owned(),
            java_home: directory.to_owned(),
            guardian_launcher: launcher().to_owned(),
            guardian_library: library(),
            shutdown_timeout: SHUTDOWN,
            timeout: Duration::from_secs(10),
            logs_directory: directory.join("logs"),
            diagnostic_bytes: maximum,
            cancelled: Arc::new(AtomicBool::new(false)),
        })
        .expect("Start real transport with a Rust protocol fixture")
    }

    fn assert_raw(directory: &Path, stream: &str) {
        let stdout = fs::read(directory.join("logs/stdout.log")).expect("Retained raw stdout");
        let stderr = fs::read(directory.join("logs/stderr.log")).expect("Retained raw stderr");
        if stream == "stdout" {
            assert_eq!(stdout, vec![b'O'; 8192]);
            assert!(stderr.is_empty());
        } else {
            assert!(stdout.is_empty());
            assert_eq!(stderr, vec![b'E'; 4096]);
        }
    }

    #[test]
    fn finish_rejects_final_stdout_and_stderr_overflow_and_retains_every_written_byte() {
        for stream in ["stdout", "stderr"] {
            let directory = tempfile::tempdir().expect("Capture fixture directory");
            fs::write(directory.path().join("final-output-stream"), stream)
                .expect("Choose final output");
            let mut transport = capture(directory.path(), 128);
            let pid = ready_pid(&directory.path().join("fixture-pid"));
            let start = process_start(pid).expect("Protocol fixture was alive");
            let error = transport
                .finish()
                .expect_err("Final raw output must reject capture");
            assert!(format!("{error:#}").contains("diagnostic output exceeded capture budget"));
            transport
                .abort()
                .expect("Overflow rejection preserves verified closure");
            assert_raw(directory.path(), stream);
            assert_gone(pid, Some(start));
        }
    }

    #[test]
    fn finish_accepts_bounded_final_output_after_verified_process_closure() {
        for stream in ["stdout", "stderr"] {
            let directory = tempfile::tempdir().expect("Capture fixture directory");
            fs::write(directory.path().join("final-output-stream"), stream)
                .expect("Choose final output");
            let mut transport = capture(directory.path(), 16 * 1024);
            let pid = ready_pid(&directory.path().join("fixture-pid"));
            let start = process_start(pid).expect("Protocol fixture was alive");
            transport
                .finish()
                .expect("Bounded final output and all processes closed");
            assert_raw(directory.path(), stream);
            assert_gone(pid, Some(start));
        }
    }
}
