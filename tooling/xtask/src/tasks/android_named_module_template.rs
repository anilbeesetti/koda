#![allow(clippy::disallowed_methods, reason = "tooling is exempt")]

use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead as _, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(test)]
use std::sync::atomic::AtomicBool;

use anyhow::{Context as _, Result, bail, ensure};
use cargo_metadata::Message;
use clap::Parser;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::android_parity::{module_template_arguments, verify_test_output};

const ARTIFACT_BUDGET: u64 = 24 * 1024 * 1024;
const RECEIPT_RESERVE: u64 = 1024 * 1024;
const LOG_BUDGET: u64 = ARTIFACT_BUDGET - RECEIPT_RESERVE;
const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_RESULT_BYTES: u64 = 2 * 1024 * 1024;
const TARGET_SOURCE: &str = "crates/android_tools/tests/module_template.rs";
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const ORIGINAL_CASES: [(&str, &str); 2] = [
    (
        "default_source_set_at_current_dir",
        "testDefaultSourceSetAtCurrentDir",
    ),
    (
        "default_source_set_at_invalid_dir",
        "testDefaultSourceSetAtInvalidDir",
    ),
];

#[derive(Parser)]
pub struct AndroidNamedModuleTemplateArgs {
    #[arg(long, default_value = ".")]
    repository: PathBuf,
    /// Exact PR head, or dispatch/push commit. Its tree must equal the checkout tree.
    #[arg(long)]
    expected_head: String,
    /// Actual github.sha, including the synthetic PR merge commit.
    #[arg(long)]
    expected_checkout: String,
    /// New directory outside the checkout; existing output is never replaced.
    #[arg(long)]
    output: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Artifact {
    path: PathBuf,
    bytes: u64,
    sha256: String,
}

#[derive(Debug, Serialize)]
struct Capture {
    command: Vec<String>,
    exit_code: Option<i32>,
    signal: Option<i32>,
    success: bool,
    timed_out: bool,
    /// Reaped leader, signalled owned group, and EOF on both captured pipes.
    /// Escaped actors are never signalled by an unverified PID.
    closure_complete: bool,
    cleanup_timed_out: bool,
    stdout_eof: bool,
    stderr_eof: bool,
    unreaped_child_pid: Option<u32>,
    log_limit_exceeded: bool,
    errors: Vec<String>,
    stdout: Artifact,
    stderr: Artifact,
}

#[derive(Default, Serialize)]
struct Checkout {
    actual_commit: Option<String>,
    actual_tree: Option<String>,
    candidate_tree: Option<String>,
    trees_equal: bool,
}

#[derive(Serialize)]
struct Compiler {
    rustc: Artifact,
    version_capture: usize,
    cargo: Artifact,
    /// These values override both environment and Cargo configuration wrappers.
    wrappers_disabled: bool,
}

#[derive(Clone, Debug, Serialize)]
struct TestExecutable {
    executable: Artifact,
    package_id: String,
    target_source: PathBuf,
    fresh: bool,
    profile: cargo_metadata::ArtifactProfile,
    features: Vec<String>,
}

#[derive(Serialize)]
struct Case {
    rust_name: &'static str,
    reference_method: &'static str,
    command_capture: Option<usize>,
    executable_before: Option<Artifact>,
    executable_after: Option<Artifact>,
    binding_before: Option<CaseBinding>,
    binding_after: Option<CaseBinding>,
    running_executable: Option<Artifact>,
    exact_result_verified: bool,
    passed: Option<u32>,
    failed: Option<u32>,
    ignored: Option<u32>,
    error: Option<String>,
}

#[derive(PartialEq, Eq, Serialize)]
struct CaseBinding {
    cargo: Artifact,
    rustc: Artifact,
    sources: Vec<Artifact>,
}

#[derive(Serialize)]
struct Receipt {
    schema_version: u32,
    purpose: &'static str,
    expected_head: String,
    expected_checkout: String,
    checkout: Checkout,
    recorder: Artifact,
    compiler: Option<Compiler>,
    sources_before: Vec<Artifact>,
    sources_after: Vec<Artifact>,
    sources_equal: bool,
    executable: Option<TestExecutable>,
    captures: Vec<Capture>,
    /// Capture failed before a complete record could be retained; spawning stops.
    capture_aborted: bool,
    cases: Vec<Case>,
    success: bool,
    error: Option<String>,
    /// A successful receipt is evidence for a separate reviewed ledger admission.
    canonical_parity_credit: u32,
    log_budget_bytes: u64,
    artifact_budget_bytes: u64,
}

struct Recorder {
    repository: PathBuf,
    output: PathBuf,
    remaining_log_bytes: Arc<AtomicU64>,
    receipt: Receipt,
}

pub fn run(args: AndroidNamedModuleTemplateArgs) -> Result<()> {
    let repository = args
        .repository
        .canonicalize()
        .context("checkout directory")?;
    let parent = args
        .output
        .parent()
        .context("output parent")?
        .canonicalize()?;
    ensure!(
        !parent.starts_with(&repository),
        "output must be outside the checkout"
    );
    let output = parent.join(args.output.file_name().context("output directory name")?);
    fs::create_dir(&output).context("creating fresh evidence directory")?;
    let mut recorder = Recorder {
        repository,
        output,
        remaining_log_bytes: Arc::new(AtomicU64::new(LOG_BUDGET)),
        receipt: Receipt {
            schema_version: 1,
            purpose: "module_template_original_exact_named_runs",
            expected_head: args.expected_head,
            expected_checkout: args.expected_checkout,
            checkout: Checkout::default(),
            recorder: hash_file(&std::env::current_exe()?)?,
            compiler: None,
            sources_before: Vec::new(),
            sources_after: Vec::new(),
            sources_equal: false,
            executable: None,
            captures: Vec::new(),
            capture_aborted: false,
            cases: ORIGINAL_CASES
                .iter()
                .map(|&(rust_name, reference_method)| Case {
                    rust_name,
                    reference_method,
                    command_capture: None,
                    executable_before: None,
                    executable_after: None,
                    binding_before: None,
                    binding_after: None,
                    running_executable: None,
                    exact_result_verified: false,
                    passed: None,
                    failed: None,
                    ignored: None,
                    error: Some("not executed".into()),
                })
                .collect(),
            success: false,
            error: None,
            canonical_parity_credit: 0,
            log_budget_bytes: LOG_BUDGET,
            artifact_budget_bytes: ARTIFACT_BUDGET,
        },
    };
    let result = recorder.execute();
    recorder.receipt.success = result.is_ok();
    recorder.receipt.error = result.as_ref().err().map(|error| format!("{error:#}"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(recorder.output.join("receipt.json"))?;
    let mut bytes = serde_json::to_vec_pretty(&recorder.receipt)?;
    bytes.push(b'\n');
    ensure!(
        bytes.len() as u64 <= RECEIPT_RESERVE,
        "receipt exceeds reserved artifact budget"
    );
    file.write_all(&bytes)?;
    file.sync_all()?;
    result
}

impl Recorder {
    fn execute(&mut self) -> Result<()> {
        ensure!(
            std::env::consts::OS == "linux",
            "named evidence currently supports Linux CI"
        );
        validate_sha(&self.receipt.expected_head)?;
        validate_sha(&self.receipt.expected_checkout)?;
        let actual = self.git_value(&["rev-parse", "--verify", "HEAD"])?;
        self.receipt.checkout.actual_commit = Some(actual.clone());
        ensure!(
            actual == self.receipt.expected_checkout,
            "checkout differs from github.sha"
        );
        let checkout_tree = self.git_value(&["rev-parse", "--verify", "HEAD^{tree}"])?;
        self.receipt.checkout.actual_tree = Some(checkout_tree.clone());
        let candidate = format!("{}^{{tree}}", self.receipt.expected_head);
        let candidate_tree = self.git_value(&["rev-parse", "--verify", &candidate])?;
        self.receipt.checkout.candidate_tree = Some(candidate_tree.clone());
        self.receipt.checkout.trees_equal = checkout_tree == candidate_tree;
        ensure!(
            self.receipt.checkout.trees_equal,
            "PR merge checkout tree differs from candidate head tree; no head execution credit"
        );
        self.ensure_clean()?;
        self.receipt.sources_before = self.source_hashes()?;

        let which = self.capture(Command::new("rustup").args(["which", "rustc"]))?;
        let rustc_path = PathBuf::from(self.stdout_value(which)?).canonicalize()?;
        let rustc = hash_file(&rustc_path)?;
        let version_capture = self.capture(Command::new(&rustc_path).arg("-vV"))?;
        self.ensure_success(version_capture)?;
        let cargo_path =
            PathBuf::from(std::env::var_os("CARGO").context("Cargo must launch xtask")?)
                .canonicalize()?;
        self.receipt.compiler = Some(Compiler {
            rustc,
            version_capture,
            cargo: hash_file(&cargo_path)?,
            wrappers_disabled: true,
        });

        let mut compile = cargo_command(&cargo_path, &rustc_path, &self.repository)?;
        // JSON is confined to this separate compilation, never a named run.
        compile.args([
            "test",
            "--locked",
            "--manifest-path",
            "crates/android_tools/Cargo.toml",
            "-p",
            "android_tools",
            "--test",
            "module_template",
            "--message-format=json-render-diagnostics",
            "--no-run",
        ]);
        let compile_capture = self.capture(&mut compile)?;
        self.ensure_success(compile_capture)?;
        let executable = executable_from_log(
            &self.receipt.captures[compile_capture].stdout.path,
            &self.repository,
        )?;
        self.receipt.executable = Some(executable.clone());

        // Attempt both original cases even when the first fails. A compiler or
        // provenance failure above blocks execution and leaves explicit not-run rows.
        for (index, &(name, _)) in ORIGINAL_CASES.iter().enumerate() {
            let result = self.execute_case(index, name, &executable, &cargo_path, &rustc_path);
            self.receipt.cases[index].error = result.err().map(|error| format!("{error:#}"));
        }
        self.receipt.sources_after = self.source_hashes()?;
        self.receipt.sources_equal = self.receipt.sources_before == self.receipt.sources_after;
        self.ensure_clean()?;
        ensure!(
            self.git_value(&["rev-parse", "--verify", "HEAD"])? == actual,
            "checkout moved during test execution"
        );
        ensure!(
            hash_file(&rustc_path)?
                == self
                    .receipt
                    .compiler
                    .as_ref()
                    .context("compiler record")?
                    .rustc,
            "compiler bytes changed during execution"
        );
        ensure!(
            hash_file(&cargo_path)?
                == self
                    .receipt
                    .compiler
                    .as_ref()
                    .context("compiler record")?
                    .cargo,
            "Cargo bytes changed during execution"
        );
        ensure!(
            self.receipt.sources_equal,
            "source bytes changed during execution"
        );
        ensure!(
            self.receipt.cases.iter().all(|case| case.error.is_none()),
            "one or more original named cases failed; see receipt and raw logs"
        );
        Ok(())
    }

    fn execute_case(
        &mut self,
        index: usize,
        name: &'static str,
        executable: &TestExecutable,
        cargo_path: &Path,
        rustc_path: &Path,
    ) -> Result<()> {
        let binding_before = self.case_binding(cargo_path, rustc_path)?;
        let compiler = self.receipt.compiler.as_ref().context("compiler record")?;
        ensure!(
            binding_before.cargo == compiler.cargo
                && binding_before.rustc == compiler.rustc
                && binding_before.sources == self.receipt.sources_before,
            "Cargo, compiler or source changed before named run"
        );
        let manifest = binding_before
            .sources
            .iter()
            .find(|artifact| {
                artifact.path == self.repository.join("crates/android_tools/Cargo.toml")
            })
            .context("manifest source hash")?;
        let source = binding_before
            .sources
            .iter()
            .find(|artifact| artifact.path == self.repository.join(TARGET_SOURCE))
            .context("test source hash")?;
        let arguments = module_template_arguments(name, &manifest.sha256, &source.sha256);
        self.receipt.cases[index].binding_before = Some(binding_before);
        let before = hash_file(&executable.executable.path)?;
        self.receipt.cases[index].executable_before = Some(before.clone());
        ensure!(
            before == executable.executable,
            "compiled executable changed before named run"
        );
        let mut command = cargo_command(cargo_path, rustc_path, &self.repository)?;
        command.args(arguments);
        let capture = self.capture(&mut command)?;
        self.receipt.cases[index].command_capture = Some(capture);
        let after = hash_file(&executable.executable.path)?;
        self.receipt.cases[index].executable_after = Some(after.clone());
        self.receipt.cases[index].binding_after = Some(self.case_binding(cargo_path, rustc_path)?);
        ensure!(
            self.receipt.cases[index].binding_before == self.receipt.cases[index].binding_after,
            "Cargo, compiler or source bytes changed during named run"
        );
        ensure!(
            before == after,
            "compiled executable changed during named run"
        );
        self.ensure_success(capture)?;
        // Normal Cargo output identifies the executable actually launched.
        // A second JSON compilation cannot substitute for this run's path.
        let running = running_executable_from_log(
            &self.receipt.captures[capture].stderr.path,
            &self.repository,
        )?;
        self.receipt.cases[index].running_executable = Some(running.clone());
        ensure!(
            running == before,
            "Cargo launched a different executable for the named run"
        );
        let stdout = read_result(&self.receipt.captures[capture].stdout.path)?;
        verify_test_output(&stdout, name, true)?;
        let case = &mut self.receipt.cases[index];
        case.exact_result_verified = true;
        case.passed = Some(1);
        case.failed = Some(0);
        case.ignored = Some(0);
        Ok(())
    }

    fn case_binding(&self, cargo: &Path, rustc: &Path) -> Result<CaseBinding> {
        Ok(CaseBinding {
            cargo: hash_file(cargo)?,
            rustc: hash_file(rustc)?,
            sources: self.source_hashes()?,
        })
    }

    fn source_hashes(&self) -> Result<Vec<Artifact>> {
        [
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            ".github/workflows/fork_ci.yml",
            "tooling/xtask/Cargo.toml",
            "tooling/xtask/src/main.rs",
            "tooling/xtask/src/tasks.rs",
            "tooling/xtask/src/tasks/android_parity.rs",
            "tooling/xtask/src/tasks/android_named_module_template.rs",
            "crates/android_tools/Cargo.toml",
            "crates/android_tools/src/android_tools.rs",
            "crates/android_tools/src/module_template.rs",
            TARGET_SOURCE,
            "crates/android_tools/test_data/module_template/PROVENANCE.json",
            "crates/android_tools/test_data/module_template/LICENSE-APACHE",
            "crates/android_tools/test_data/module_template/reference/idea/android-npw/testSrc/com/android/tools/idea/npw/GradleModuleTemplateTest.java",
            "crates/android_tools/test_data/module_template/reference/idea/android-npw/src/com/android/tools/idea/npw/project/GradleAndroidModuleTemplate.java",
            "crates/android_tools/test_data/module_template/reference/idea/project-system/src/com/android/tools/idea/projectsystem/AndroidModulePaths.kt",
            "crates/android_tools/test_data/module_template/reference/idea/project-system/src/com/android/tools/idea/projectsystem/NamedModuleTemplate.kt",
            "crates/android_tools/test_data/module_template/reference/base/common/src/main/java/com/android/SdkConstants.java",
        ]
        .iter()
        .map(|path| hash_file(&self.repository.join(path)))
        .collect()
    }

    fn capture(&mut self, command: &mut Command) -> Result<usize> {
        ensure!(
            !self.receipt.capture_aborted
                && self
                    .receipt
                    .captures
                    .iter()
                    .all(|capture| capture.closure_complete),
            "an earlier child or pipe did not close; no further commands may start"
        );
        command.current_dir(&self.repository);
        let index = self.receipt.captures.len();
        self.receipt.capture_aborted = true;
        let capture = capture(
            command,
            &self.output.join(format!("{index:02}-stdout.log")),
            &self.output.join(format!("{index:02}-stderr.log")),
            self.remaining_log_bytes.clone(),
            Duration::from_secs(900),
        )?;
        self.receipt.capture_aborted = false;
        self.receipt.captures.push(capture);
        Ok(index)
    }

    fn ensure_success(&self, index: usize) -> Result<()> {
        let capture = &self.receipt.captures[index];
        ensure!(
            capture.success
                && capture.closure_complete
                && !capture.timed_out
                && !capture.log_limit_exceeded
                && capture.errors.is_empty(),
            "command capture {index} failed (exit {:?}, signal {:?}): {:?}",
            capture.exit_code,
            capture.signal,
            capture.errors
        );
        Ok(())
    }

    fn stdout_value(&self, index: usize) -> Result<String> {
        self.ensure_success(index)?;
        let text = read_result(&self.receipt.captures[index].stdout.path)?;
        let value = text.trim();
        ensure!(
            !value.is_empty() && value.lines().count() == 1,
            "command capture {index} must contain one value"
        );
        Ok(value.to_owned())
    }

    fn git_value(&mut self, args: &[&str]) -> Result<String> {
        let index = self.capture(Command::new("git").args(args))?;
        let value = self.stdout_value(index)?;
        validate_sha(&value)?;
        Ok(value)
    }

    fn ensure_clean(&mut self) -> Result<()> {
        let index = self.capture(Command::new("git").args([
            "status",
            "--porcelain=v1",
            "--untracked-files=normal",
        ]))?;
        self.ensure_success(index)?;
        ensure!(
            read_result(&self.receipt.captures[index].stdout.path)?.is_empty(),
            "source checkout has tracked or untracked changes"
        );
        Ok(())
    }
}

fn cargo_command(cargo: &Path, rustc: &Path, repository: &Path) -> Result<Command> {
    ensure!(
        cargo.file_name() == Some(std::ffi::OsStr::new("cargo")),
        "resolved Cargo executable must be named cargo"
    );
    let directory = cargo.parent().context("Cargo executable parent")?;
    let mut paths = vec![directory.to_path_buf()];
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }
    // The actual program is the ledger's literal cargo. The child-only PATH
    // resolves it to the separately hashed executable, without rewriting argv.
    let mut command = Command::new("cargo");
    command
        .env("PATH", std::env::join_paths(paths)?)
        .env("RUSTC", rustc)
        .env("RUSTC_WRAPPER", "")
        .env("RUSTC_WORKSPACE_WRAPPER", "")
        .env("CARGO_TARGET_DIR", repository.join("target"))
        .env("CARGO_TERM_COLOR", "never")
        .env("CARGO_TERM_VERBOSE", "false")
        .env("CARGO_TERM_QUIET", "false")
        .env("RUST_TEST_THREADS", "1")
        .env_remove("RUST_TEST_NOCAPTURE");
    Ok(command)
}

fn validate_sha(value: &str) -> Result<()> {
    ensure!(
        value.len() == 40
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "expected a full lowercase Git SHA-1"
    );
    Ok(())
}

fn read_result(path: &Path) -> Result<String> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_RESULT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_RESULT_BYTES,
        "result exceeds bounded validation buffer"
    );
    String::from_utf8(bytes).context("test/command output must be UTF-8")
}

fn hash_file(path: &Path) -> Result<Artifact> {
    ensure!(
        fs::symlink_metadata(path)?.is_file(),
        "artifact must be a regular file: {}",
        path.display()
    );
    let mut file = File::open(path)?;
    let before = file.metadata()?;
    let mut hasher = Sha256::new();
    let mut bytes = 0;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        bytes += count as u64;
    }
    let after = file.metadata()?;
    ensure!(
        bytes == before.len()
            && before.len() == after.len()
            && before.modified()? == after.modified()?,
        "artifact changed while hashing: {}",
        path.display()
    );
    Ok(Artifact {
        path: path.to_owned(),
        bytes,
        sha256: format!("{:x}", hasher.finalize()),
    })
}

fn executable_from_log(log: &Path, repository: &Path) -> Result<TestExecutable> {
    let mut reader = BufReader::new(File::open(log)?);
    let source = repository.join(TARGET_SOURCE).canonicalize()?;
    let target_root = repository.join("target").canonicalize()?;
    let mut result = None;
    loop {
        let mut line = Vec::new();
        let count = reader
            .by_ref()
            .take(MAX_LINE_BYTES as u64 + 1)
            .read_until(b'\n', &mut line)?;
        if count == 0 {
            break;
        }
        ensure!(count <= MAX_LINE_BYTES, "Cargo JSON line exceeds bound");
        let message = serde_json::from_slice::<Message>(&line);
        let Ok(Message::CompilerArtifact(artifact)) = message else {
            continue;
        };
        if artifact.target.name != "module_template"
            || !artifact.profile.test
            || !artifact.target.is_test()
        {
            continue;
        }
        let path = artifact
            .executable
            .context("test artifact has no executable")?
            .into_std_path_buf();
        let actual_source = artifact
            .target
            .src_path
            .into_std_path_buf()
            .canonicalize()?;
        ensure!(
            actual_source == source,
            "Cargo test artifact source differs from requested target"
        );
        let path = path.canonicalize()?;
        ensure!(
            path.starts_with(&target_root),
            "test executable is outside the controlled Cargo target directory"
        );
        ensure!(
            result.is_none(),
            "Cargo reported multiple module_template executables"
        );
        result = Some(TestExecutable {
            executable: hash_file(&path)?,
            package_id: artifact.package_id.to_string(),
            target_source: actual_source,
            fresh: artifact.fresh,
            profile: artifact.profile,
            features: artifact.features,
        });
    }
    result.context("Cargo did not report the module_template compiler artifact")
}

fn running_executable_from_log(log: &Path, repository: &Path) -> Result<Artifact> {
    let target_root = repository.join("target").canonicalize()?;
    let mut reader = BufReader::new(File::open(log)?);
    let mut result = None;
    loop {
        let mut line = String::new();
        let count = reader
            .by_ref()
            .take(MAX_LINE_BYTES as u64 + 1)
            .read_line(&mut line)?;
        if count == 0 {
            break;
        }
        ensure!(count <= MAX_LINE_BYTES, "Cargo launch line exceeds bound");
        let Some(path) = line
            .trim()
            .strip_prefix("Running tests/module_template.rs (")
            .and_then(|line| line.strip_suffix(')'))
        else {
            continue;
        };
        ensure!(
            result.is_none(),
            "Cargo reported multiple module_template launches"
        );
        let path = repository.join(path).canonicalize()?;
        ensure!(
            path.starts_with(&target_root),
            "launched executable is outside target directory"
        );
        result = Some(hash_file(&path)?);
    }
    result
        .context("Cargo did not identify the module_template executable launched by this named run")
}

fn reserve_log_bytes(remaining: &AtomicU64, count: u64) -> bool {
    remaining
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |left| {
            left.checked_sub(count)
        })
        .is_ok()
}

#[cfg(test)]
fn copy_log(
    mut source: impl Read,
    mut output: File,
    remaining: Arc<AtomicU64>,
    failed: Arc<AtomicBool>,
) -> Result<()> {
    let result = (|| {
        let mut buffer = [0_u8; 8192];
        loop {
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            if !reserve_log_bytes(&remaining, count as u64) {
                failed.store(true, Ordering::Release);
                bail!("23 MiB aggregate raw-log limit exceeded; output is incomplete");
            }
            output.write_all(&buffer[..count])?;
        }
        output.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        failed.store(true, Ordering::Release);
    }
    result
}

// Both pipes are nonblocking; no reader thread or blocking wait can outlive
// the deadline. Cleanup signals only our unreaped leader's process group.
fn capture(
    command: &mut Command,
    stdout_path: &Path,
    stderr_path: &Path,
    remaining: Arc<AtomicU64>,
    timeout: Duration,
) -> Result<Capture> {
    ensure!(
        cfg!(target_os = "linux"),
        "process capture only supports Linux"
    );
    let mut stdout = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(stdout_path)?;
    let mut stderr = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(stderr_path)?;
    let command_text = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    let mut errors = Vec::new();
    let mut status = None;
    let mut timed_out = false;
    let mut cleanup_timed_out = false;
    let mut closure_complete = true; // No child exists until spawn succeeds.
    let mut stdout_eof = false;
    let mut stderr_eof = false;
    let mut unreaped_child_pid = None;
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    if remaining.load(Ordering::Acquire) == 0 {
        errors.push("raw-log budget exhausted; command was not spawned".into());
    } else {
        match command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Err(error) => errors.push(format!("spawn: {error}")),
            Ok(mut child) => {
                closure_complete = false;
                let pid = child.id();
                let mut output = child.stdout.take();
                let mut error = child.stderr.take();
                if output.is_none() || error.is_none() {
                    errors.push("spawned child has no output pipes".into());
                }
                if let Some(pipe) = &output {
                    if let Err(error) = make_nonblocking(pipe) {
                        errors.push(format!("nonblocking stdout: {error:#}"));
                        output = None; // Never attempt a potentially blocking read.
                    }
                }
                if let Some(pipe) = &error {
                    if let Err(failure) = make_nonblocking(pipe) {
                        errors.push(format!("nonblocking stderr: {failure:#}"));
                        error = None;
                    }
                }
                let deadline = Instant::now() + timeout;
                let mut cleanup_deadline = None;
                let mut group_signalled = false;
                let mut wait_failed = false;
                loop {
                    if let Some(pipe) = &mut output {
                        match drain_pipe(pipe, &mut stdout, &remaining) {
                            Ok(true) => {
                                stdout_eof = true;
                                output = None;
                            }
                            Ok(false) => {}
                            Err(error) => {
                                errors.push(format!("stdout capture: {error:#}"));
                                output = None;
                            }
                        }
                    }
                    if let Some(pipe) = &mut error {
                        match drain_pipe(pipe, &mut stderr, &remaining) {
                            Ok(true) => {
                                stderr_eof = true;
                                error = None;
                            }
                            Ok(false) => {}
                            Err(failure) => {
                                errors.push(format!("stderr capture: {failure:#}"));
                                error = None;
                            }
                        }
                    }
                    if cleanup_deadline.is_none() {
                        // This observation cannot reap/reuse our leader PID.
                        let exited = match child_exited_unreaped(pid) {
                            Ok(exited) => exited,
                            Err(error) => {
                                errors.push(format!("observing child: {error:#}"));
                                false
                            }
                        };
                        timed_out = Instant::now() >= deadline;
                        if exited || timed_out || !errors.is_empty() {
                            // Signal before any try_wait can release the leader.
                            match close_process_group(pid) {
                                Ok(()) => group_signalled = true,
                                Err(error) => {
                                    errors.push(format!("process-group cleanup: {error:#}"))
                                }
                            }
                            // Never extend cleanup beyond timeout + this allowance.
                            cleanup_deadline = Some(
                                (Instant::now() + CLEANUP_TIMEOUT).min(deadline + CLEANUP_TIMEOUT),
                            );
                        }
                    }
                    if let Some(cleanup_deadline) = cleanup_deadline {
                        if status.is_none() && !wait_failed {
                            match child.try_wait() {
                                Ok(exit) => status = exit,
                                Err(error) => {
                                    errors.push(format!("nonblocking wait: {error}"));
                                    wait_failed = true;
                                }
                            }
                        }
                        if status.is_some() && stdout_eof && stderr_eof {
                            closure_complete = group_signalled;
                            break;
                        }
                        if Instant::now() >= cleanup_deadline {
                            cleanup_timed_out = true;
                            errors.push(format!(
                                "cleanup deadline: leader reaped={}, stdout EOF={}, stderr EOF={}; \
                                 unverified escaped pipe holders are not signalled",
                                status.is_some(),
                                stdout_eof,
                                stderr_eof
                            ));
                            break;
                        }
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                // Dropping our nonblocking readers closes our descriptors even
                // when an escaped descendant retains its own pipe endpoints.
                drop(output);
                drop(error);
                if status.is_none() {
                    unreaped_child_pid = Some(pid);
                    errors.push(format!(
                        "owned child {pid} remains unconfirmed; closure is incomplete"
                    ));
                }
                // Dropping Child does not wait. Preserve the incomplete record;
                // Recorder::capture refuses to start any further child.
            }
        }
    }
    stdout.sync_all()?;
    stderr.sync_all()?;
    let log_limit_exceeded = errors
        .iter()
        .any(|error| error.contains("raw-log limit exceeded"));
    Ok(Capture {
        command: command_text,
        exit_code: status.as_ref().and_then(ExitStatus::code),
        signal: exit_signal(status.as_ref()),
        success: status.as_ref().is_some_and(ExitStatus::success)
            && closure_complete
            && !timed_out
            && errors.is_empty(),
        timed_out,
        closure_complete,
        cleanup_timed_out,
        stdout_eof,
        stderr_eof,
        unreaped_child_pid,
        log_limit_exceeded,
        errors,
        stdout: hash_file(stdout_path)?,
        stderr: hash_file(stderr_path)?,
    })
}

fn drain_pipe(source: &mut impl Read, output: &mut File, remaining: &AtomicU64) -> Result<bool> {
    let mut buffer = [0_u8; 8192];
    // Bound each drain so a continuously writing child cannot starve deadline
    // checks or the other pipe. At most 128 KiB is handled per polling round.
    for _ in 0..8 {
        match source.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) => {
                ensure!(
                    reserve_log_bytes(remaining, count as u64),
                    "23 MiB aggregate raw-log limit exceeded; output is incomplete"
                );
                output.write_all(&buffer[..count])?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

#[cfg(target_os = "linux")]
fn make_nonblocking(pipe: &impl std::os::fd::AsRawFd) -> Result<()> {
    unsafe extern "C" {
        fn fcntl(fd: i32, operation: i32, ...) -> i32;
    }
    let fd = pipe.as_raw_fd();
    // SAFETY: fd is our live child-pipe descriptor. Linux F_GETFL (3) takes no
    // extra argument; F_SETFL (4) takes these existing flags plus O_NONBLOCK.
    let flags = unsafe { fcntl(fd, 3) };
    ensure!(flags >= 0, "F_GETFL: {}", std::io::Error::last_os_error());
    let result = unsafe { fcntl(fd, 4, flags | 0x800) };
    ensure!(result == 0, "F_SETFL: {}", std::io::Error::last_os_error());
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn make_nonblocking<T>(pipe: &T) -> Result<()> {
    let _ = pipe;
    bail!("nonblocking pipe capture only supports Linux")
}

fn child_exited_unreaped(pid: u32) -> Result<bool> {
    // Linux CI supplies procfs. Reading stat leaves the child unreaped; unlike
    // try_wait, it cannot release/reuse our group-leader PID before cleanup.
    let mut stat = String::new();
    File::open(format!("/proc/{pid}/stat"))?
        .take(4097)
        .read_to_string(&mut stat)?;
    ensure!(stat.len() <= 4096, "child stat exceeds bound");
    let (_, state) = stat.rsplit_once(')').context("child stat framing")?;
    match state.as_bytes() {
        [b' ', state, b' ', ..] => Ok(*state == b'Z' || *state == b'X'),
        _ => bail!("child stat has no process state"),
    }
}

#[cfg(target_os = "linux")]
fn close_process_group(process_group: u32) -> Result<()> {
    // std can create a process group but cannot signal one. This is the
    // Linux libc ABI, limited to the group created for this capture.
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    let group = i32::try_from(process_group).context("process group exceeds pid_t")?;
    ensure!(group > 1, "refusing invalid process group");
    // SAFETY: group is the positive PID returned by our own child spawn,
    // which has not yet been reaped. Negation addresses only that child's
    // independently created process group, before its PID can be reused.
    let result = unsafe { kill(-group, 9) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        // ESRCH means the group has already exited normally.
        ensure!(
            error.raw_os_error() == Some(3),
            "closing process group {group}: {error}"
        );
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn close_process_group(process_group: u32) -> Result<()> {
    let _ = process_group;
    bail!("process-group capture only supports Linux")
}

fn exit_signal(status: Option<&ExitStatus>) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        status.and_then(ExitStatus::signal)
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn actual_named_cargo_argv_matches_the_unchanged_ledger_selector() -> Result<()> {
        let directory = TempDir::new()?;
        let cargo = directory.path().join("toolchain/bin/cargo");
        let rustc = directory.path().join("toolchain/bin/rustc");
        for &(name, _) in &ORIGINAL_CASES {
            let mut command = cargo_command(&cargo, &rustc, directory.path())?;
            command.args(module_template_arguments(
                name,
                "fixture-manifest-hash",
                "fixture-source-hash",
            ));
            let actual = std::iter::once(command.get_program())
                .chain(command.get_args())
                .map(|value| value.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            assert_eq!(
                actual,
                vec![
                    "cargo",
                    "test",
                    "--locked",
                    "--manifest-path",
                    "crates/android_tools/Cargo.toml",
                    "-p",
                    "android_tools",
                    "--test",
                    "module_template",
                    name,
                    "--",
                    "--exact",
                    "--show-output",
                ]
            );
            let path = command
                .get_envs()
                .find(|(key, _)| *key == "PATH")
                .and_then(|(_, value)| value)
                .context("child PATH")?;
            assert_eq!(
                std::env::split_paths(path).next(),
                cargo.parent().map(Path::to_path_buf)
            );
        }
        // Path binding uses this actual run's ordinary Cargo launch record,
        // independent of the separate JSON compilation's artifact metadata.
        let executable = directory
            .path()
            .join("target/debug/deps/module_template-fixture");
        fs::create_dir_all(executable.parent().context("executable parent")?)?;
        fs::write(&executable, "fixture executable")?;
        let log = directory.path().join("launch.log");
        let launch =
            "     Running tests/module_template.rs (target/debug/deps/module_template-fixture)\n";
        fs::write(&log, launch)?;
        assert_eq!(
            running_executable_from_log(&log, directory.path())?,
            hash_file(&executable.canonicalize()?)?
        );
        fs::write(&log, format!("{launch}{launch}"))?;
        assert!(running_executable_from_log(&log, directory.path()).is_err());
        fs::write(&log, "     Running tests/module_template.rs (launch.log)\n")?;
        assert!(running_executable_from_log(&log, directory.path()).is_err());
        fs::write(&log, "test result: ok. 1 passed; 0 failed;\n")?;
        assert!(running_executable_from_log(&log, directory.path()).is_err());
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn escaped_pipe_holder_yields_bounded_failed_capture() -> Result<()> {
        const ROLE: &str = "KODA_NAMED_CAPTURE_REGRESSION_ROLE";
        const ROOT: &str = "KODA_NAMED_CAPTURE_REGRESSION_ROOT";
        const SELECTOR: &str = "KODA_NAMED_CAPTURE_REGRESSION_SELECTOR";
        // Reinvoke this Rust test executable as a disposable fixture. All role
        // variables belong to child commands; the test process's env is unchanged.
        if let Some(role) = std::env::var_os(ROLE) {
            let root = PathBuf::from(std::env::var_os(ROOT).context("fixture root")?);
            let selector = std::env::var_os(SELECTOR).context("fixture selector")?;
            match role.to_str() {
                Some("sleep") => thread::sleep(Duration::from_secs(5)),
                Some("leader") => {
                    let _leaf = Command::new(std::env::current_exe()?)
                        .args([
                            std::ffi::OsStr::new("--exact"),
                            &selector,
                            std::ffi::OsStr::new("--nocapture"),
                        ])
                        .env(ROLE, "escape")
                        .env(ROOT, &root)
                        .env(SELECTOR, &selector)
                        .stdin(Stdio::null())
                        .stdout(Stdio::inherit())
                        .stderr(Stdio::inherit())
                        .spawn()?;
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while !root.join("ready").exists() && Instant::now() < deadline {
                        thread::sleep(Duration::from_millis(5));
                    }
                    ensure!(
                        root.join("ready").exists(),
                        "escaped fixture did not become ready"
                    );
                    // Return normally so the group leader exits 0 while the
                    // independently created session still owns both pipe writers.
                }
                Some("escape") => {
                    unsafe extern "C" {
                        fn setsid() -> i32;
                    }
                    // SAFETY: this disposable child is not our group leader;
                    // setsid affects only its own process, never the test runner.
                    ensure!(unsafe { setsid() } > 0, "fixture setsid failed");
                    let pid = std::process::id();
                    let (_, start) =
                        fixture_process_identity(pid)?.context("fixture procfs identity")?;
                    fs::write(root.join("ready"), format!("{pid} {start}\n"))?;
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while root.join("release").exists() && Instant::now() < deadline {
                        thread::sleep(Duration::from_millis(5));
                    }
                    fs::write(root.join("exited"), "finite escaped fixture finished\n")?;
                }
                _ => bail!("unknown private fixture role"),
            }
            return Ok(());
        }

        let directory = TempDir::new()?;
        let selector = thread::current()
            .name()
            .context("libtest selector")?
            .to_owned();
        let mut release = FixtureRelease {
            root: directory.path().to_owned(),
            complete: false,
        };
        fs::write(
            directory.path().join("release"),
            "owned fixture release gate\n",
        )?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["--exact", &selector, "--nocapture"])
            .env(ROLE, "leader")
            .env(ROOT, directory.path())
            .env(SELECTOR, &selector);
        let began = Instant::now();
        let captured = capture(
            &mut command,
            &directory.path().join("escape-stdout.log"),
            &directory.path().join("escape-stderr.log"),
            Arc::new(AtomicU64::new(LOG_BUDGET)),
            Duration::from_secs(5),
        )?;
        let elapsed = began.elapsed();
        // Release only our private file gate. Never kill an escaped or reused PID.
        release.release_and_wait()?;
        assert!(elapsed < Duration::from_secs(8));
        assert_eq!(captured.exit_code, Some(0));
        assert!(!captured.success);
        assert!(!captured.closure_complete);
        assert!(captured.cleanup_timed_out);
        assert!(!captured.stdout_eof && !captured.stderr_eof);
        assert_eq!(captured.unreaped_child_pid, None);
        assert!(
            captured
                .errors
                .iter()
                .any(|error| error.contains("cleanup deadline"))
        );

        let mut sleeping = Command::new(std::env::current_exe()?);
        sleeping
            .args(["--exact", &selector, "--nocapture"])
            .env(ROLE, "sleep")
            .env(ROOT, directory.path())
            .env(SELECTOR, &selector);
        let timeout = capture(
            &mut sleeping,
            &directory.path().join("timeout-stdout.log"),
            &directory.path().join("timeout-stderr.log"),
            Arc::new(AtomicU64::new(LOG_BUDGET)),
            Duration::from_millis(25),
        )?;
        assert!(timeout.timed_out && !timeout.success);
        assert!(timeout.closure_complete && timeout.stdout_eof && timeout.stderr_eof);
        assert_eq!(timeout.signal, Some(9));
        assert_eq!(timeout.unreaped_child_pid, None);
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn fixture_process_identity(pid: u32) -> Result<Option<(u8, u64)>> {
        let mut stat = String::new();
        let file = match File::open(format!("/proc/{pid}/stat")) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        file.take(4097).read_to_string(&mut stat)?;
        ensure!(stat.len() <= 4096, "fixture stat exceeds bound");
        let (_, tail) = stat.rsplit_once(')').context("fixture stat framing")?;
        let fields = tail.split_whitespace().collect::<Vec<_>>();
        let state = fields.first().context("fixture state")?.as_bytes()[0];
        let start = fields.get(19).context("fixture start time")?.parse()?;
        Ok(Some((state, start)))
    }

    #[cfg(target_os = "linux")]
    struct FixtureRelease {
        root: PathBuf,
        complete: bool,
    }

    #[cfg(target_os = "linux")]
    impl FixtureRelease {
        fn release_and_wait(&mut self) -> Result<()> {
            match fs::remove_file(self.root.join("release")) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("remove private fixture release gate"),
            }
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                if let Ok(ready) = fs::read_to_string(self.root.join("ready")) {
                    let (pid, start) = ready.trim().split_once(' ').context("fixture witness")?;
                    let pid = pid.parse()?;
                    let start: u64 = start.parse()?;
                    let closed = match fixture_process_identity(pid)? {
                        None => true,
                        Some((state, actual_start)) => {
                            actual_start != start || state == b'Z' || state == b'X'
                        }
                    };
                    if closed && self.root.join("exited").exists() {
                        self.complete = true;
                        return Ok(());
                    }
                }
                ensure!(
                    Instant::now() < deadline,
                    "private escaped fixture closure unconfirmed"
                );
                thread::sleep(Duration::from_millis(5));
            }
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for FixtureRelease {
        fn drop(&mut self) {
            if !self.complete {
                if let Err(error) = self.release_and_wait() {
                    eprintln!("fixture cleanup failed: {error:#}");
                }
            }
        }
    }

    #[test]
    fn original_names_require_one_pass_and_reject_filtered_or_ignored_runs() {
        for &(name, _) in &ORIGINAL_CASES {
            let pass = format!(
                "test {name} ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.00s\n"
            );
            assert!(verify_test_output(&pass, name, true).is_ok());
            assert!(
                verify_test_output(
                    "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out;",
                    name,
                    true
                )
                .is_err()
            );
            assert!(
                verify_test_output(
                    &pass.replace(
                        "1 passed; 0 failed; 0 ignored",
                        "0 passed; 0 failed; 1 ignored"
                    ),
                    name,
                    true
                )
                .is_err()
            );
            assert!(verify_test_output(&pass.replace(name, "unrelated_case"), name, true).is_err());
            assert!(verify_test_output(&format!("{pass}{pass}"), name, true).is_err());
        }
    }

    #[test]
    fn full_git_shas_reject_refs_options_uppercase_and_abbreviations() {
        assert!(validate_sha("b7f6bf220fbdb5f66d662c71029760dc186a0474").is_ok());
        for value in [
            "HEAD",
            "--help",
            "b7f6",
            "B7F6BF220FBDB5F66D662C71029760DC186A0474",
            "b7f6bf220fbdb5f66d662c71029760dc186a0474^{tree}",
        ] {
            assert!(validate_sha(value).is_err());
        }
    }

    #[test]
    fn log_budget_never_wraps_or_admits_more_bytes_than_reserved() {
        let remaining = AtomicU64::new(9);
        assert!(reserve_log_bytes(&remaining, 7));
        assert!(!reserve_log_bytes(&remaining, 3));
        assert_eq!(remaining.load(Ordering::Acquire), 2);
        assert!(reserve_log_bytes(&remaining, 2));
        assert!(!reserve_log_bytes(&remaining, 1));
        assert_eq!(remaining.load(Ordering::Acquire), 0);
    }

    #[test]
    fn over_budget_log_is_explicitly_incomplete_and_never_written() -> Result<()> {
        let directory = TempDir::new()?;
        let path = directory.path().join("stdout.log");
        let remaining = Arc::new(AtomicU64::new(2));
        let failed = Arc::new(AtomicBool::new(false));
        assert!(
            copy_log(
                std::io::Cursor::new(b"three"),
                File::create(&path)?,
                remaining.clone(),
                failed.clone()
            )
            .is_err()
        );
        assert!(failed.load(Ordering::Acquire));
        assert_eq!(hash_file(&path)?.bytes, 0);
        assert_eq!(remaining.load(Ordering::Acquire), 2);
        Ok(())
    }

    #[test]
    fn cargo_artifacts_require_exact_source_single_target_and_controlled_executable() -> Result<()>
    {
        let directory = TempDir::new()?;
        let repository = directory.path();
        let source = repository.join(TARGET_SOURCE);
        fs::create_dir_all(source.parent().context("source parent")?)?;
        fs::write(&source, "#[test] fn example() {}\n")?;
        let executable = repository.join("target/debug/deps/module_template-fixture");
        fs::create_dir_all(executable.parent().context("executable parent")?)?;
        fs::write(&executable, "test executable identity fixture")?;
        let artifact = serde_json::json!({
            "reason": "compiler-artifact",
            "package_id": "path+file:///fixture#android_tools@0.1.0",
            "manifest_path": repository.join("crates/android_tools/Cargo.toml"),
            "target": { "kind": ["test"], "crate_types": ["bin"], "name": "module_template", "src_path": source, "edition": "2024", "doc": false, "doctest": false, "test": true },
            "profile": { "opt_level": "0", "debuginfo": 0, "debug_assertions": true, "overflow_checks": true, "test": true },
            "features": [], "filenames": [executable], "executable": executable, "fresh": true
        });
        let log = repository.join("cargo.log");
        fs::write(&log, format!("{artifact}\nrunning 1 test\n"))?;
        assert_eq!(
            executable_from_log(&log, repository)?.executable,
            hash_file(&executable)?
        );
        fs::write(&log, format!("{artifact}\n{artifact}\n"))?;
        assert!(executable_from_log(&log, repository).is_err());
        let mut wrong_source = artifact.clone();
        wrong_source["target"]["src_path"] = serde_json::json!(log);
        fs::write(&log, wrong_source.to_string())?;
        assert!(executable_from_log(&log, repository).is_err());
        let mut escaped = artifact.clone();
        escaped["executable"] = serde_json::json!(log);
        fs::write(&log, escaped.to_string())?;
        assert!(executable_from_log(&log, repository).is_err());
        fs::write(&log, "test result: ok. 0 passed; 0 failed;\n")?;
        assert!(executable_from_log(&log, repository).is_err());
        Ok(())
    }
}
