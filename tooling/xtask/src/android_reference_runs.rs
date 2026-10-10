#![allow(clippy::disallowed_methods, reason = "tooling is exempt")]

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

#[cfg(target_os = "linux")]
use std::{
    fs::OpenOptions,
    os::{fd::AsFd as _, unix::process::CommandExt as _},
    process::{Child, ExitStatus, Stdio},
    time::Instant,
};

#[cfg(target_os = "linux")]
use nix::{
    errno::Errno,
    fcntl::{FcntlArg, OFlag, fcntl},
    poll::{PollFd, PollFlags, poll},
    sys::{
        signal::{Signal, killpg},
        wait::{Id, WaitPidFlag, WaitStatus, waitid},
    },
    unistd::Pid,
};

use anyhow::{Context as _, Result, bail, ensure};
use clap::Parser;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const MAX_MANIFEST_BYTES: u64 = 512 * 1024;
const MAX_PROCESS_LOG_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ARCHIVE_LOG_BYTES: u64 = 128 * 1024 * 1024;
const MAX_RECEIPT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Parser)]
pub struct AndroidReferenceRunsArgs {
    #[arg(long)]
    manifest: PathBuf,
    #[arg(long)]
    source: String,
    #[arg(long)]
    checkout: PathBuf,
    #[arg(long)]
    target_dir: PathBuf,
    #[arg(long)]
    output_dir: PathBuf,
}

#[derive(Deserialize)]
struct Manifest {
    schema_version: u32,
    sources: Vec<Source>,
    historical_run_evidence_included: bool,
    no_reference_credit_added: bool,
}

#[derive(Deserialize)]
struct Source {
    label: String,
    commit: String,
    tree: String,
    case_count: usize,
    cases: Vec<Case>,
    source_check_bindings: Vec<Binding>,
    runtime_result: Option<Value>,
}

#[derive(Clone, Deserialize)]
struct Binding {
    commit: String,
    path: String,
    git_blob_oid: String,
    sha256: String,
    bytes: u64,
}

#[derive(Deserialize)]
struct Declaration {
    start_line: usize,
    end_line: usize,
    sha256: String,
}

#[derive(Deserialize)]
struct Case {
    reference_id: String,
    package: String,
    integration_target: Option<String>,
    fully_qualified_name: String,
    manifest: Binding,
    source: Binding,
    named_declaration: Declaration,
    exact_planned_argv: Vec<String>,
    expected_selected: usize,
    expected_passed: usize,
    expected_failed: usize,
    expected_ignored: usize,
    runtime_result: Option<Value>,
}

#[derive(Serialize)]
struct CheckoutProof {
    commit: String,
    tree: String,
    index_sha256: String,
    tracked_files: usize,
    bound_files: usize,
    full_materialization: bool,
}

#[derive(Clone, Serialize)]
struct LogProof {
    path: String,
    bytes: u64,
    sha256: String,
}

#[derive(Clone, Serialize)]
struct ArtifactProof {
    executable: PathBuf,
    executable_sha256: String,
    executable_bytes: u64,
    manifest_path: PathBuf,
    src_path: PathBuf,
    profile: Value,
    features: Value,
}

#[derive(Serialize)]
struct ProcessProof {
    argv: Vec<String>,
    success: bool,
    exit_code: Option<i32>,
    elapsed_ms: u128,
    stopped_reason: Option<String>,
    stdout_eof: bool,
    stderr_eof: bool,
    leader_reaped: bool,
    owned_group_signal_sent: bool,
    stdout: LogProof,
    stderr: LogProof,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct TestSummary {
    selected: usize,
    passed: usize,
    failed: usize,
    ignored: usize,
}

#[derive(Serialize)]
struct CaseProof {
    reference_id: String,
    name: String,
    status: String,
    error: Option<String>,
    summary: Option<TestSummary>,
    process: Option<ProcessProof>,
    artifact: Option<ArtifactProof>,
}

#[derive(Serialize)]
struct GroupProof {
    package: String,
    integration_target: Option<String>,
    error: Option<String>,
    compilation: Option<ProcessProof>,
    artifact: Option<ArtifactProof>,
}

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    status: String,
    label: String,
    tested_commit: String,
    tested_tree: String,
    runner_commit: String,
    manifest_sha256: String,
    environment: BTreeMap<String, Option<String>>,
    source_before: Option<CheckoutProof>,
    source_after: Option<CheckoutProof>,
    source_error: Option<String>,
    groups: Vec<GroupProof>,
    cases: Vec<CaseProof>,
    no_reference_credit_added: bool,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn file_digest(path: &Path) -> Result<(u64, String)> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hash = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        bytes = bytes
            .checked_add(count as u64)
            .context("file size overflow")?;
    }
    Ok((bytes, format!("{:x}", hash.finalize())))
}

fn bounded_read(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= maximum,
        "{} exceeds read limit",
        path.display()
    );
    Ok(bytes)
}

fn relative_path(path: &str) -> Result<&Path> {
    let path = Path::new(path);
    ensure!(
        !path.as_os_str().is_empty()
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "source path must be a normal relative path"
    );
    Ok(path)
}

fn git(checkout: &Path, arguments: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(arguments)
        .output()?;
    ensure!(
        output.status.success(),
        "git {:?}: {}",
        arguments,
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

fn git_text(checkout: &Path, arguments: &[&str]) -> Result<String> {
    Ok(String::from_utf8(git(checkout, arguments)?)?
        .trim()
        .to_owned())
}

fn exact_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn planned_argv(case: &Case) -> Vec<String> {
    let mut arguments = vec![
        "cargo".to_owned(),
        "test".to_owned(),
        "--locked".to_owned(),
        "--manifest-path".to_owned(),
        case.manifest.path.clone(),
        "-p".to_owned(),
        case.package.clone(),
    ];
    match &case.integration_target {
        Some(target) => arguments.extend(["--test".to_owned(), target.clone()]),
        None => arguments.push("--lib".to_owned()),
    }
    arguments.extend([
        case.fully_qualified_name.clone(),
        "--".to_owned(),
        "--exact".to_owned(),
        "--show-output".to_owned(),
    ]);
    arguments
}

fn validate_manifest(manifest: &Manifest) -> Result<()> {
    ensure!(
        manifest.schema_version == 1
            && !manifest.historical_run_evidence_included
            && manifest.no_reference_credit_added,
        "execution-only schema required"
    );
    ensure!(
        manifest.sources.len() == 2,
        "both finite source groups required"
    );
    let mut labels = BTreeSet::new();
    for source in &manifest.sources {
        ensure!(
            labels.insert(source.label.as_str()),
            "duplicate source label"
        );
        let (commit, tree, count) = match source.label.as_str() {
            "primary36" => (
                "594bf18526b3d7197315c82f5c6c3860cfb7bf5a",
                "dd2bb2ae16a43fd0a0dbee35bae3615a5fbe26f1",
                36,
            ),
            "table4" => (
                "0d155a5fb869df3f10837db92e252e946687f7ae",
                "c5306d1dc5714e629eb0dd658fd230f77af84f52",
                4,
            ),
            _ => bail!("unknown source group"),
        };
        ensure!(
            source.commit == commit
                && source.tree == tree
                && source.case_count == count
                && source.cases.len() == count,
            "pinned source/count mismatch"
        );
        ensure!(
            source.runtime_result.is_none(),
            "inherited source runtime proof is forbidden"
        );
        let mut names = BTreeSet::new();
        let mut references = BTreeSet::new();
        for case in &source.cases {
            ensure!(
                names.insert((
                    &case.package,
                    &case.integration_target,
                    &case.fully_qualified_name
                )) && references.insert(&case.reference_id),
                "duplicate case/reference binding"
            );
            ensure!(
                case.runtime_result.is_none(),
                "inherited case runtime proof is forbidden"
            );
            ensure!(
                case.expected_selected == 1
                    && case.expected_passed == 1
                    && case.expected_failed == 0
                    && case.expected_ignored == 0,
                "one-pass result contract required"
            );
            ensure!(
                case.exact_planned_argv == planned_argv(case),
                "Cargo argv differs from exact normal test command"
            );
            ensure!(
                matches!(case.package.as_str(), "android_tools" | "android_ui"),
                "unexpected package"
            );
            ensure!(
                case.fully_qualified_name
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric()
                        || character == '_'
                        || character == ':'),
                "invalid named case"
            );
            relative_path(&case.manifest.path)?;
            relative_path(&case.source.path)?;
            for binding in [&case.manifest, &case.source] {
                ensure!(
                    source
                        .source_check_bindings
                        .iter()
                        .any(|check| check.path == binding.path
                            && check.sha256 == binding.sha256
                            && check.git_blob_oid == binding.git_blob_oid
                            && check.bytes == binding.bytes
                            && check.commit == binding.commit),
                    "case binding absent from checked source set"
                );
            }
        }
    }
    Ok(())
}

fn verify_binding(checkout: &Path, source: &Source, binding: &Binding) -> Result<()> {
    ensure!(
        binding.commit == source.commit
            && exact_hex(&binding.sha256, 64)
            && exact_hex(&binding.git_blob_oid, 40),
        "invalid current source binding"
    );
    let path = relative_path(&binding.path)?;
    let object = format!("{}:{}", source.commit, binding.path);
    ensure!(
        git_text(checkout, &["rev-parse", &object])? == binding.git_blob_oid,
        "Git blob mismatch: {}",
        binding.path
    );
    verify_materialized_binding(&checkout.join(path), binding)?;
    Ok(())
}

fn verify_materialized_binding(path: &Path, binding: &Binding) -> Result<()> {
    let actual = file_digest(path)?;
    ensure!(
        actual == (binding.bytes, binding.sha256.clone()),
        "materialized source hash mismatch: {}",
        binding.path
    );
    Ok(())
}

fn verify_checkout(checkout: &Path, source: &Source) -> Result<CheckoutProof> {
    let commit = git_text(checkout, &["rev-parse", "HEAD"])?;
    let tree = git_text(checkout, &["rev-parse", "HEAD^{tree}"])?;
    ensure!(
        commit == source.commit && tree == source.tree,
        "wrong tested HEAD/tree"
    );
    ensure!(
        git(checkout, &["status", "--porcelain", "--untracked-files=no"])?.is_empty(),
        "tested tracked source is dirty"
    );
    let index = git(checkout, &["ls-files", "--stage", "-z"])?;
    let flags = git(checkout, &["ls-files", "-v", "-z"])?;
    let mut tracked_files = 0;
    for entry in flags
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        ensure!(
            entry.first() == Some(&b'H'),
            "sparse/assume-unchanged/unmerged index entry forbidden"
        );
        let name = std::str::from_utf8(entry.get(2..).context("invalid index entry")?)?;
        ensure!(
            checkout
                .join(relative_path(name)?)
                .symlink_metadata()
                .is_ok(),
            "tracked source not materialized: {name}"
        );
        tracked_files += 1;
    }
    ensure!(tracked_files > 0, "empty index");
    let mut paths = BTreeSet::new();
    for binding in &source.source_check_bindings {
        ensure!(paths.insert(&binding.path), "duplicate checked source path");
        verify_binding(checkout, source, binding)?;
    }
    for required in ["Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml"] {
        ensure!(
            paths.contains(&required.to_owned()),
            "missing build-source binding: {required}"
        );
    }
    for case in &source.cases {
        let bytes = bounded_read(&checkout.join(&case.source.path), 4 * 1024 * 1024)?;
        let text = std::str::from_utf8(&bytes)?;
        let declaration = &case.named_declaration;
        ensure!(
            declaration.start_line > 0 && declaration.end_line >= declaration.start_line,
            "invalid declaration span"
        );
        let lines: Vec<_> = text.lines().collect();
        let span = lines
            .get(declaration.start_line - 1..declaration.end_line)
            .context("declaration span exceeds source")?
            .join("\n");
        ensure!(
            digest(span.as_bytes()) == declaration.sha256,
            "named declaration hash mismatch: {}",
            case.fully_qualified_name
        );
    }
    Ok(CheckoutProof {
        commit,
        tree,
        index_sha256: digest(&index),
        tracked_files,
        bound_files: paths.len(),
        full_materialization: true,
    })
}

fn log_proof(path: &Path) -> Result<LogProof> {
    let (bytes, sha256) = file_digest(path)?;
    Ok(LogProof {
        path: path
            .file_name()
            .context("log filename missing")?
            .to_string_lossy()
            .into_owned(),
        bytes,
        sha256,
    })
}

fn verify_log(directory: &Path, proof: &LogProof) -> Result<Vec<u8>> {
    let path = directory.join(relative_path(&proof.path)?);
    ensure!(
        file_digest(&path)? == (proof.bytes, proof.sha256.clone()),
        "raw log hash/size mismatch"
    );
    bounded_read(&path, MAX_PROCESS_LOG_BYTES + 8 * 1024 * 1024)
}

#[cfg(target_os = "linux")]
struct OwnedChild {
    child: Child,
    group: Pid,
    group_cleaned: bool,
    signal_sent: bool,
    reaped: bool,
}

#[cfg(target_os = "linux")]
impl OwnedChild {
    fn spawn(mut command: Command) -> Result<Self> {
        command
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn().context("spawn owned command group")?;
        let group =
            Pid::from_raw(i32::try_from(child.id()).context("child PID exceeds platform range")?);
        Ok(Self {
            child,
            group,
            group_cleaned: false,
            signal_sent: false,
            reaped: false,
        })
    }

    fn exited_without_reaping(&self) -> Result<bool> {
        match waitid(
            Id::Pid(self.group),
            WaitPidFlag::WEXITED | WaitPidFlag::WNOWAIT | WaitPidFlag::WNOHANG,
        ) {
            Ok(WaitStatus::StillAlive) => Ok(false),
            Ok(WaitStatus::Exited(_, _) | WaitStatus::Signaled(_, _, _)) => Ok(true),
            Ok(status) => bail!("unexpected owned child status: {status:?}"),
            Err(Errno::EINTR) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    fn stop_group(&mut self) -> Result<()> {
        if self.group_cleaned {
            return Ok(());
        }
        // WNOWAIT keeps this group's leader PID reserved until the scoped signal is sent.
        match killpg(self.group, Signal::SIGKILL) {
            Ok(()) => {
                self.signal_sent = true;
                self.group_cleaned = true;
            }
            Err(Errno::ESRCH) => self.group_cleaned = true,
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    fn reap_after_cleanup(&mut self) -> Result<ExitStatus> {
        ensure!(
            self.group_cleaned,
            "owned group cleanup must precede reaping"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while !self.exited_without_reaping()? {
            ensure!(
                Instant::now() < deadline,
                "owned leader did not exit after scoped cleanup; cleanup incomplete"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }
}

#[cfg(target_os = "linux")]
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        match self.stop_group().and_then(|()| self.reap_after_cleanup()) {
            Ok(_) => {}
            Err(error) => eprintln!(
                "owned process cleanup incomplete for reserved leader {}: {error:#}",
                self.group
            ),
        }
    }
}

#[cfg(target_os = "linux")]
fn nonblocking(pipe: &impl std::os::fd::AsFd) -> Result<()> {
    let flags = OFlag::from_bits_truncate(fcntl(pipe, FcntlArg::F_GETFL)?);
    fcntl(pipe, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn capture_available(
    input: &mut impl Read,
    file: &mut File,
    total_bytes: &AtomicU64,
    process_bytes: &mut u64,
) -> Result<bool> {
    let mut buffer = [0_u8; 64 * 1024];
    match input.read(&mut buffer) {
        Ok(0) => Ok(true),
        Ok(count) => {
            file.write_all(&buffer[..count])?;
            *process_bytes = process_bytes
                .checked_add(count as u64)
                .context("process output byte overflow")?;
            total_bytes.fetch_add(count as u64, Ordering::Relaxed);
            Ok(false)
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(target_os = "linux")]
struct Supervision {
    status: ExitStatus,
    stopped_reason: Option<String>,
    stdout_eof: bool,
    stderr_eof: bool,
    owned_group_signal_sent: bool,
}

#[cfg(target_os = "linux")]
fn monitor(
    child: &mut OwnedChild,
    stdout_file: &mut File,
    stderr_file: &mut File,
    timeout: Duration,
    total_bytes: &AtomicU64,
) -> Result<Supervision> {
    let mut stdout = child.child.stdout.take().context("stdout pipe missing")?;
    let mut stderr = child.child.stderr.take().context("stderr pipe missing")?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let started = Instant::now();
    let mut stdout_eof = false;
    let mut stderr_eof = false;
    let mut stopped_reason = None;
    let mut cleanup_started = None;
    let mut process_bytes = 0_u64;
    loop {
        let exited = child.exited_without_reaping()?;
        if exited && stdout_eof && stderr_eof {
            break;
        }
        if stopped_reason.is_none()
            && (started.elapsed() >= timeout
                || process_bytes > MAX_PROCESS_LOG_BYTES
                || total_bytes.load(Ordering::Relaxed) > MAX_ARCHIVE_LOG_BYTES)
        {
            stopped_reason = Some(
                if started.elapsed() >= timeout {
                    "process/stream deadline exceeded"
                } else {
                    "raw log budget exceeded"
                }
                .to_owned(),
            );
            child.stop_group()?;
            cleanup_started = Some(Instant::now());
        }
        if cleanup_started.is_some_and(|started| started.elapsed() >= Duration::from_secs(1))
            || process_bytes > MAX_PROCESS_LOG_BYTES + 8 * 1024 * 1024
            || total_bytes.load(Ordering::Relaxed) > MAX_ARCHIVE_LOG_BYTES + 8 * 1024 * 1024
        {
            stopped_reason = Some(
                "owned group signalled; remaining pipe output incomplete after bounded drain"
                    .to_owned(),
            );
            break;
        }
        let mut descriptors = Vec::new();
        if !stdout_eof {
            descriptors.push(PollFd::new(
                stdout.as_fd(),
                PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR,
            ));
        }
        if !stderr_eof {
            descriptors.push(PollFd::new(
                stderr.as_fd(),
                PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR,
            ));
        }
        match poll(&mut descriptors, 20_u16) {
            Ok(_) => {}
            Err(Errno::EINTR) => continue,
            Err(error) => return Err(error.into()),
        }
        drop(descriptors);
        if !stdout_eof {
            stdout_eof =
                capture_available(&mut stdout, stdout_file, total_bytes, &mut process_bytes)?;
        }
        if !stderr_eof {
            stderr_eof =
                capture_available(&mut stderr, stderr_file, total_bytes, &mut process_bytes)?;
        }
    }
    // Readers are nonblocking and owned here: no worker threads or unbounded joins survive.
    drop(stdout);
    drop(stderr);
    child.stop_group()?;
    let status = child.reap_after_cleanup()?;
    stdout_file.sync_all()?;
    stderr_file.sync_all()?;
    Ok(Supervision {
        status,
        stopped_reason,
        stdout_eof,
        stderr_eof,
        owned_group_signal_sent: child.signal_sent,
    })
}

#[cfg(target_os = "linux")]
fn supervise(
    command: Command,
    output: &Path,
    stem: &str,
    argv: &[String],
    timeout: Duration,
    total_bytes: &AtomicU64,
) -> Result<ProcessProof> {
    ensure!(
        total_bytes.load(Ordering::Relaxed) < MAX_ARCHIVE_LOG_BYTES,
        "archive log budget exhausted; execution incomplete"
    );
    let stdout_path = output.join(format!("{stem}.stdout.log"));
    let stderr_path = output.join(format!("{stem}.stderr.log"));
    let mut stdout_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stdout_path)?;
    let mut stderr_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stderr_path)?;
    let started = Instant::now();
    let mut child = OwnedChild::spawn(command)?;
    let supervision = monitor(
        &mut child,
        &mut stdout_file,
        &mut stderr_file,
        timeout,
        total_bytes,
    )?;
    let proof = ProcessProof {
        argv: argv.to_vec(),
        success: supervision.status.success()
            && supervision.stopped_reason.is_none()
            && supervision.stdout_eof
            && supervision.stderr_eof,
        exit_code: supervision.status.code(),
        elapsed_ms: started.elapsed().as_millis(),
        stopped_reason: supervision.stopped_reason,
        stdout_eof: supervision.stdout_eof,
        stderr_eof: supervision.stderr_eof,
        leader_reaped: child.reaped,
        owned_group_signal_sent: supervision.owned_group_signal_sent,
        stdout: log_proof(&stdout_path)?,
        stderr: log_proof(&stderr_path)?,
    };
    println!(
        "{stem}: success={} stdout={} bytes stderr={} bytes",
        proof.success, proof.stdout.bytes, proof.stderr.bytes
    );
    Ok(proof)
}

fn execute(
    checkout: &Path,
    target: &Path,
    output: &Path,
    stem: &str,
    argv: &[String],
    timeout: Duration,
    total_bytes: &AtomicU64,
) -> Result<ProcessProof> {
    #[cfg(target_os = "linux")]
    {
        let mut command = Command::new(argv.first().context("missing program")?);
        command
            .args(argv.get(1..).context("missing argv")?)
            .current_dir(checkout)
            .env("CARGO_TARGET_DIR", target)
            .env("CARGO_TERM_COLOR", "never");
        supervise(command, output, stem, argv, timeout, total_bytes)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _inputs = (checkout, target, output, stem, argv, timeout, total_bytes);
        bail!("named process supervision requires the Linux isolated worker")
    }
}

fn compile_argv(case: &Case) -> Vec<String> {
    let mut argv = planned_argv(case);
    argv.truncate(argv.len() - 4);
    argv.extend(["--no-run".to_owned(), "--message-format=json".to_owned()]);
    argv
}

fn expected_target_source(checkout: &Path, case: &Case) -> Result<PathBuf> {
    let manifest_path = checkout.join(&case.manifest.path);
    let directory = manifest_path
        .parent()
        .context("manifest directory missing")?;
    let path = match &case.integration_target {
        Some(target) => directory.join("tests").join(format!("{target}.rs")),
        None => {
            let manifest: toml::Value = toml::from_str(&fs::read_to_string(&manifest_path)?)?;
            directory.join(
                manifest
                    .get("lib")
                    .and_then(|value| value.get("path"))
                    .and_then(toml::Value::as_str)
                    .unwrap_or("src/lib.rs"),
            )
        }
    };
    Ok(path.canonicalize()?)
}

fn compiler_artifact(
    bytes: &[u8],
    checkout: &Path,
    target_directory: &Path,
    case: &Case,
) -> Result<ArtifactProof> {
    let expected_manifest = checkout.join(&case.manifest.path).canonicalize()?;
    let expected_source = expected_target_source(checkout, case)?;
    let expected_name = case.integration_target.as_deref().unwrap_or(&case.package);
    let expected_kind = if case.integration_target.is_some() {
        "test"
    } else {
        "lib"
    };
    let mut matched = Vec::new();
    for line in std::str::from_utf8(bytes)?
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let value: Value = serde_json::from_str(line).context("invalid compiler JSON line")?;
        if value.get("reason").and_then(Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let target = &value["target"];
        if target["name"].as_str() != Some(expected_name)
            || target["src_path"].as_str().map(Path::new) != Some(expected_source.as_path())
            || value["profile"]["test"].as_bool() != Some(true)
        {
            continue;
        }
        ensure!(
            target["kind"].as_array().is_some_and(|kinds| kinds
                .iter()
                .any(|kind| kind.as_str() == Some(expected_kind))),
            "wrong compiler target kind"
        );
        ensure!(
            value["manifest_path"].as_str().map(Path::new) == Some(expected_manifest.as_path()),
            "wrong compiler manifest"
        );
        ensure!(
            value["fresh"].as_bool() == Some(false),
            "selected test artifact falsely Fresh"
        );
        ensure!(
            value["profile"]["opt_level"].as_str() == Some("0")
                && value["profile"]["debug_assertions"].as_bool() == Some(true),
            "selected artifact is not normal dev test profile"
        );
        let executable = PathBuf::from(
            value["executable"]
                .as_str()
                .context("test executable missing")?,
        )
        .canonicalize()?;
        ensure!(
            executable.starts_with(target_directory.join("debug/deps")),
            "test executable outside fresh dev target"
        );
        let mut header = [0_u8; 4];
        File::open(&executable)?.read_exact(&mut header)?;
        ensure!(&header == b"\x7fELF", "test executable is not Linux ELF");
        let (executable_bytes, executable_sha256) = file_digest(&executable)?;
        matched.push(ArtifactProof {
            executable,
            executable_sha256,
            executable_bytes,
            manifest_path: expected_manifest.clone(),
            src_path: expected_source.clone(),
            profile: value["profile"].clone(),
            features: value["features"].clone(),
        });
    }
    ensure!(
        matched.len() == 1,
        "expected one matching compiler test artifact, got {}",
        matched.len()
    );
    matched.pop().context("matching artifact missing")
}

fn parse_result(text: &str, name: &str) -> Result<TestSummary> {
    let named = format!("test {name} ... ok");
    ensure!(
        text.lines().filter(|line| line.trim() == named).count() == 1,
        "named passing result absent or duplicated"
    );
    let selected = Regex::new(r"^running ([0-9]+) tests?$")?;
    let summary = Regex::new(
        r"^test result: ok\. ([0-9]+) passed; ([0-9]+) failed; ([0-9]+) ignored; ([0-9]+) measured; [0-9]+ filtered out;.*$",
    )?;
    let selected: Vec<_> = text
        .lines()
        .filter_map(|line| selected.captures(line.trim()))
        .collect();
    let summaries: Vec<_> = text
        .lines()
        .filter_map(|line| summary.captures(line.trim()))
        .collect();
    ensure!(
        selected.len() == 1 && summaries.len() == 1,
        "exactly one libtest run/summary required"
    );
    let count = |captures: &regex::Captures<'_>, index| -> Result<usize> {
        Ok(captures
            .get(index)
            .context("result count absent")?
            .as_str()
            .parse()?)
    };
    let selected = count(selected.first().context("selected count missing")?, 1)?;
    let summary = summaries.first().context("summary missing")?;
    let result = TestSummary {
        selected,
        passed: count(summary, 1)?,
        failed: count(summary, 2)?,
        ignored: count(summary, 3)?,
    };
    ensure!(
        result
            == (TestSummary {
                selected: 1,
                passed: 1,
                failed: 0,
                ignored: 0
            })
            && count(summary, 4)? == 0,
        "wrong selected/passed/failed/ignored/measured counts"
    );
    Ok(result)
}

fn verify_runtime(
    output: &Path,
    process: &ProcessProof,
    artifact: &ArtifactProof,
    case: &Case,
) -> Result<TestSummary> {
    ensure!(
        process.success && process.stdout_eof && process.stderr_eof && process.leader_reaped,
        "exact Cargo command failed or was interrupted"
    );
    let stdout = verify_log(output, &process.stdout)?;
    let stderr = verify_log(output, &process.stderr)?;
    let text = std::str::from_utf8(&stdout)?;
    let stderr = std::str::from_utf8(&stderr)?;
    let running = Regex::new(r"^\s*Running .*\((.+)\)\s*$")?;
    let paths: Vec<_> = stderr
        .lines()
        .filter_map(|line| running.captures(line))
        .filter_map(|captures| captures.get(1).map(|path| PathBuf::from(path.as_str())))
        .collect();
    ensure!(
        paths.len() == 1 && paths.first() == Some(&artifact.executable),
        "runtime Cargo executable differs from compiler artifact"
    );
    ensure!(
        file_digest(&artifact.executable)?
            == (
                artifact.executable_bytes,
                artifact.executable_sha256.clone()
            ),
        "test executable changed during named run"
    );
    parse_result(text, &case.fully_qualified_name)
}

fn write_report(output: &Path, report: &Report) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(report)?;
    bytes.push(b'\n');
    ensure!(
        bytes.len() <= MAX_RECEIPT_BYTES,
        "receipt byte budget exceeded"
    );
    let temporary = output.join("receipt.next.json");
    fs::write(&temporary, &bytes)?;
    fs::rename(temporary, output.join("receipt.json"))?;
    Ok(())
}

fn prepare_directories(
    checkout: &Path,
    target: &Path,
    output: &Path,
) -> Result<(PathBuf, PathBuf)> {
    let fresh_path = |path: &Path| -> Result<PathBuf> {
        ensure!(
            !path.exists(),
            "target/output directory must be fresh and absent"
        );
        let parent = path
            .parent()
            .context("capture parent missing")?
            .canonicalize()?;
        let name = path.file_name().context("capture directory name missing")?;
        Ok(parent.join(name))
    };
    let target = fresh_path(target)?;
    let output = fresh_path(output)?;
    ensure!(
        !target.starts_with(checkout)
            && !output.starts_with(checkout)
            && !target.starts_with(&output)
            && !output.starts_with(&target),
        "target/log output must be separate from tested source and each other"
    );
    fs::create_dir(&target)?;
    fs::create_dir(&output)?;
    Ok((target, output))
}

pub fn run(args: AndroidReferenceRunsArgs) -> Result<()> {
    ensure!(
        cfg!(target_os = "linux"),
        "named-reference worker currently requires Linux ELF/process groups"
    );
    let manifest_bytes = bounded_read(&args.manifest, MAX_MANIFEST_BYTES)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    validate_manifest(&manifest)?;
    let source = manifest
        .sources
        .iter()
        .find(|source| source.label == args.source)
        .context("unknown selected source label")?;
    let checkout = args.checkout.canonicalize()?;
    let (target, output) = prepare_directories(&checkout, &args.target_dir, &args.output_dir)?;
    let runner_checkout = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let environment = [
        "CC",
        "CXX",
        "CARGO_BUILD_JOBS",
        "CARGO_INCREMENTAL",
        "CARGO_PROFILE_DEV_DEBUG",
        "CARGO_PROFILE_DEV_BUILD_OVERRIDE_DEBUG",
        "RUSTFLAGS",
        "RUSTC_WRAPPER",
        "WGPU_BACKEND",
        "VK_DRIVER_FILES",
    ]
    .into_iter()
    .map(|key| (key.to_owned(), std::env::var(key).ok()))
    .collect();
    let mut report = Report {
        schema_version: 1,
        status: "NOT_RUN".to_owned(),
        label: source.label.clone(),
        tested_commit: source.commit.clone(),
        tested_tree: source.tree.clone(),
        runner_commit: git_text(&runner_checkout, &["rev-parse", "HEAD"])?,
        manifest_sha256: digest(&manifest_bytes),
        environment,
        source_before: None,
        source_after: None,
        source_error: None,
        groups: Vec::new(),
        cases: source
            .cases
            .iter()
            .map(|case| CaseProof {
                reference_id: case.reference_id.clone(),
                name: case.fully_qualified_name.clone(),
                status: "NOT_RUN".to_owned(),
                error: None,
                summary: None,
                process: None,
                artifact: None,
            })
            .collect(),
        no_reference_credit_added: true,
    };
    write_report(&output, &report)?;
    match verify_checkout(&checkout, source) {
        Ok(proof) => report.source_before = Some(proof),
        Err(error) => {
            report.status = "FAIL".to_owned();
            report.source_error = Some(format!("{error:#}"));
            write_report(&output, &report)?;
            return Err(error);
        }
    }
    let mut groups: BTreeMap<(&str, Option<&str>), Vec<usize>> = BTreeMap::new();
    for (index, case) in source.cases.iter().enumerate() {
        groups
            .entry((&case.package, case.integration_target.as_deref()))
            .or_default()
            .push(index);
    }
    let total_bytes = AtomicU64::new(0);
    for (group_index, ((package, integration_target), indexes)) in groups.into_iter().enumerate() {
        let first = *indexes.first().context("empty compile group")?;
        let case = source.cases.get(first).context("group case missing")?;
        let mut group = GroupProof {
            package: package.to_owned(),
            integration_target: integration_target.map(str::to_owned),
            error: None,
            compilation: None,
            artifact: None,
        };
        let compiled = execute(
            &checkout,
            &target,
            &output,
            &format!("group-{group_index:02}"),
            &compile_argv(case),
            Duration::from_secs(3600),
            &total_bytes,
        )
        .and_then(|process| {
            group.compilation = Some(process);
            let process = group
                .compilation
                .as_ref()
                .context("compile proof missing")?;
            ensure!(process.success, "group compilation failed/interrupted");
            let bytes = verify_log(&output, &process.stdout)?;
            compiler_artifact(&bytes, &checkout, &target, case)
        });
        match compiled {
            Ok(artifact) => {
                group.artifact = Some(artifact.clone());
                for index in indexes {
                    let case = source.cases.get(index).context("case missing")?;
                    let proof = report
                        .cases
                        .get_mut(index)
                        .context("case receipt missing")?;
                    proof.artifact = Some(artifact.clone());
                    let result = execute(
                        &checkout,
                        &target,
                        &output,
                        &format!("case-{index:02}"),
                        &case.exact_planned_argv,
                        Duration::from_secs(600),
                        &total_bytes,
                    )
                    .and_then(|process| {
                        proof.process = Some(process);
                        verify_runtime(
                            &output,
                            proof.process.as_ref().context("process receipt missing")?,
                            &artifact,
                            case,
                        )
                    });
                    match result {
                        Ok(summary) => {
                            proof.status = "PASS".to_owned();
                            proof.summary = Some(summary);
                        }
                        Err(error) => {
                            proof.status = "FAIL".to_owned();
                            proof.error = Some(format!("{error:#}"));
                        }
                    }
                    println!("{} {}", proof.status, proof.name);
                    write_report(&output, &report)?;
                }
            }
            Err(error) => {
                let error = format!("group prerequisite failed: {error:#}");
                group.error = Some(error.clone());
                for index in indexes {
                    let proof = report
                        .cases
                        .get_mut(index)
                        .context("case receipt missing")?;
                    proof.status = "BLOCKED".to_owned();
                    proof.error = Some(error.clone());
                    println!("BLOCKED {}: {}", proof.name, error);
                }
            }
        }
        report.groups.push(group);
        write_report(&output, &report)?;
    }
    match verify_checkout(&checkout, source) {
        Ok(proof) => report.source_after = Some(proof),
        Err(error) => report.source_error = Some(format!("{error:#}")),
    }
    let passed = report
        .cases
        .iter()
        .filter(|case| case.status == "PASS")
        .count();
    let success = passed == source.case_count
        && report.source_error.is_none()
        && total_bytes.load(Ordering::Relaxed) <= MAX_ARCHIVE_LOG_BYTES;
    report.status = if success { "PASS" } else { "FAIL" }.to_owned();
    write_report(&output, &report)?;
    println!(
        "{}: {passed}/{} named cases PASS; no parity credit changed",
        report.status, source.case_count
    );
    ensure!(
        success,
        "named reference proof incomplete or failing; inspect receipt.json and retained raw logs"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "running 1 test\ntest module::case ... ok\n\nsuccesses:\n\nsuccesses:\n    module::case\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.01s\n";

    #[test]
    fn accepts_exact_named_libtest_output() -> Result<()> {
        assert_eq!(
            parse_result(VALID, "module::case")?,
            TestSummary {
                selected: 1,
                passed: 1,
                failed: 0,
                ignored: 0
            }
        );
        Ok(())
    }

    #[test]
    fn rejects_absent_or_substring_name() {
        assert!(parse_result(VALID, "case").is_err());
        assert!(parse_result(VALID, "module::missing").is_err());
    }

    #[test]
    fn rejects_duplicate_results_or_summaries() {
        assert!(parse_result(&format!("{VALID}{VALID}"), "module::case").is_err());
        assert!(
            parse_result(
                &VALID.replace(
                    "test module::case ... ok",
                    "test module::case ... ok\ntest module::case ... ok"
                ),
                "module::case"
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_zero_selected_and_ignored_or_failed_counts() {
        for changed in [
            VALID.replace("running 1 test", "running 0 tests"),
            VALID.replace("1 passed", "0 passed"),
            VALID.replace("0 ignored", "1 ignored"),
            VALID.replace("0 failed", "1 failed"),
            VALID.replace("0 measured", "1 measured"),
            VALID.replace("... ok", "... ignored"),
            VALID.replace("test result: ok", "test result: FAILED"),
        ] {
            assert!(parse_result(&changed, "module::case").is_err());
        }
    }

    #[test]
    fn rejects_missing_or_modified_raw_log() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("raw.log");
        fs::write(&path, VALID)?;
        let proof = log_proof(&path)?;
        assert_eq!(verify_log(directory.path(), &proof)?, VALID.as_bytes());
        fs::write(&path, VALID.replace("... ok", "... FAILED"))?;
        assert!(verify_log(directory.path(), &proof).is_err());
        fs::remove_file(&path)?;
        assert!(verify_log(directory.path(), &proof).is_err());
        Ok(())
    }

    #[test]
    fn rejects_traversal_and_absolute_binding_paths() {
        for path in ["../source", "/source", "source/../other", "", "./source"] {
            assert!(relative_path(path).is_err());
        }
    }

    #[test]
    fn rejects_capture_inside_tested_source_before_creating_directories() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let checkout = directory.path().join("tested");
        fs::create_dir(&checkout)?;
        let target = checkout.join("target");
        let output = directory.path().join("output");
        assert!(prepare_directories(&checkout, &target, &output).is_err());
        assert!(!target.exists() && !output.exists());
        assert!(prepare_directories(&checkout, &output, &output).is_err());
        assert!(!output.exists());
        Ok(())
    }

    #[test]
    fn rejects_changed_or_missing_materialized_source() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("source.rs");
        let bytes = b"fn original_case() {}\n";
        fs::write(&path, bytes)?;
        let binding = Binding {
            commit: "0".repeat(40),
            path: "source.rs".to_owned(),
            git_blob_oid: "0".repeat(40),
            sha256: digest(bytes),
            bytes: bytes.len() as u64,
        };
        verify_materialized_binding(&path, &binding)?;
        fs::write(&path, b"fn modified_case() {}\n")?;
        assert!(verify_materialized_binding(&path, &binding).is_err());
        fs::remove_file(&path)?;
        assert!(verify_materialized_binding(&path, &binding).is_err());
        Ok(())
    }

    #[test]
    fn frozen_manifest_has_forty_current_cases_and_normal_exact_commands() -> Result<()> {
        let bytes = include_bytes!("../../../docs/android-studio/reference-named-tests.json");
        let manifest: Manifest = serde_json::from_slice(bytes)?;
        validate_manifest(&manifest)?;
        assert_eq!(
            manifest
                .sources
                .iter()
                .map(|source| source.cases.len())
                .sum::<usize>(),
            40
        );
        for source in &manifest.sources {
            for case in &source.cases {
                let argv = compile_argv(case);
                assert!(
                    argv.ends_with(&["--no-run".to_owned(), "--message-format=json".to_owned()])
                );
                assert!(!argv.contains(&case.fully_qualified_name));
            }
        }
        Ok(())
    }

    #[test]
    fn rejects_wrong_commit_hash_inherited_proof_and_duplicate_cases() -> Result<()> {
        let bytes = include_bytes!("../../../docs/android-studio/reference-named-tests.json");
        let value: Value = serde_json::from_slice(bytes)?;
        for (pointer, changed) in [
            (
                "/sources/0/commit",
                Value::String("0000000000000000000000000000000000000000".to_owned()),
            ),
            (
                "/sources/0/cases/0/source/sha256",
                Value::String("0".repeat(64)),
            ),
            (
                "/sources/0/cases/0/runtime_result",
                Value::String("historical PASS".to_owned()),
            ),
            ("/historical_run_evidence_included", Value::Bool(true)),
        ] {
            let mut changed_manifest = value.clone();
            *changed_manifest
                .pointer_mut(pointer)
                .context("test manifest pointer missing")? = changed;
            assert!(validate_manifest(&serde_json::from_value(changed_manifest)?).is_err());
        }
        let mut duplicate = value;
        let first = duplicate
            .pointer("/sources/0/cases/0")
            .context("case missing")?
            .clone();
        *duplicate
            .pointer_mut("/sources/0/cases/1")
            .context("case missing")? = first;
        assert!(validate_manifest(&serde_json::from_value(duplicate)?).is_err());
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_lifetime_fixture() -> Result<()> {
        match std::env::var("KODA_REFERENCE_PROCESS_FIXTURE")
            .ok()
            .as_deref()
        {
            Some("hold") => {
                println!("writer ready");
                std::io::stdout().flush()?;
                std::thread::sleep(Duration::from_secs(30));
            }
            Some("descendant") => {
                let descendant = fixture_command("hold")?
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .spawn()?;
                println!("descendant spawned: {}", descendant.id());
                std::io::stdout().flush()?;
                // Deliberately leave inherited pipes open to reproduce the supervisor regression.
                drop(descendant);
            }
            _ => {}
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn fixture_command(mode: &str) -> Result<Command> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args([
                "--exact",
                "android_reference_runs::tests::process_lifetime_fixture",
                "--nocapture",
            ])
            .env("KODA_REFERENCE_PROCESS_FIXTURE", mode);
        Ok(command)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn descendant_pipe_deadline_survives_parent_exit() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let started = Instant::now();
        let proof = supervise(
            fixture_command("descendant")?,
            directory.path(),
            "descendant",
            &["fixture".to_owned()],
            Duration::from_secs(1),
            &AtomicU64::new(0),
        )?;
        assert!(!proof.success);
        assert!(
            proof
                .stopped_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("deadline"))
        );
        assert!(proof.stdout_eof && proof.stderr_eof && proof.leader_reaped);
        assert!(proof.owned_group_signal_sent);
        assert!(started.elapsed() < Duration::from_secs(4));
        let stdout = verify_log(directory.path(), &proof.stdout)?;
        assert!(std::str::from_utf8(&stdout)?.contains("descendant spawned:"));
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn deadline_kills_and_reaps_owned_leader() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let proof = supervise(
            fixture_command("hold")?,
            directory.path(),
            "timeout",
            &["fixture".to_owned()],
            Duration::from_secs(1),
            &AtomicU64::new(0),
        )?;
        assert!(!proof.success && proof.leader_reaped && proof.owned_group_signal_sent);
        assert!(proof.stdout_eof && proof.stderr_eof);
        assert!(proof.stopped_reason.is_some());
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn output_error_guard_kills_and_reaps_reserved_leader() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut output = OpenOptions::new().write(true).open("/dev/full")?;
        let mut stderr = File::create(directory.path().join("stderr.log"))?;
        let mut child = OwnedChild::spawn(fixture_command("hold")?)?;
        let leader = child.group;
        let started = Instant::now();
        let result = monitor(
            &mut child,
            &mut output,
            &mut stderr,
            Duration::from_secs(2),
            &AtomicU64::new(0),
        );
        assert!(result.is_err());
        drop(child);
        assert!(matches!(
            waitid(Id::Pid(leader), WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG),
            Err(Errno::ECHILD)
        ));
        assert!(started.elapsed() < Duration::from_secs(4));
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn artifact_fixture() -> Result<(tempfile::TempDir, Case, PathBuf, Value)> {
        let directory = tempfile::tempdir()?;
        let manifest: Manifest = serde_json::from_slice(include_bytes!(
            "../../../docs/android-studio/reference-named-tests.json"
        ))?;
        let case = manifest
            .sources
            .into_iter()
            .next()
            .context("source fixture missing")?
            .cases
            .into_iter()
            .next()
            .context("case fixture missing")?;
        let manifest_path = directory.path().join(&case.manifest.path);
        let crate_directory = manifest_path.parent().context("fixture crate missing")?;
        fs::create_dir_all(crate_directory.join("src"))?;
        fs::write(&manifest_path, "[lib]\npath = \"src/android_tools.rs\"\n")?;
        let src_path = crate_directory.join("src/android_tools.rs");
        fs::write(&src_path, "pub fn fixture() {}\n")?;
        let target = directory.path().join("target");
        fs::create_dir_all(target.join("debug/deps"))?;
        let executable = target.join("debug/deps/android_tools-fixture");
        // This protocol fixture is never executed or recorded as real compiler evidence.
        fs::write(&executable, b"\x7fELFprotocol fixture")?;
        let value = serde_json::json!({
            "reason": "compiler-artifact", "manifest_path": manifest_path,
            "target": {"name": "android_tools", "kind": ["lib"], "src_path": src_path},
            "profile": {"test": true, "opt_level": "0", "debug_assertions": true},
            "features": [], "fresh": false, "executable": executable,
        });
        Ok((directory, case, target, value))
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rejects_wrong_compiler_source_profile_freshness_and_duplicates() -> Result<()> {
        let (directory, case, target, value) = artifact_fixture()?;
        let bytes = serde_json::to_vec(&value)?;
        compiler_artifact(&bytes, directory.path(), &target, &case)?;
        for (pointer, replacement) in [
            (
                "/manifest_path",
                Value::String("/different/Cargo.toml".to_owned()),
            ),
            (
                "/target/src_path",
                Value::String("/different/src.rs".to_owned()),
            ),
            ("/target/name", Value::String("different_target".to_owned())),
            ("/target/kind", serde_json::json!(["bin"])),
            ("/profile/test", Value::Bool(false)),
            ("/profile/opt_level", Value::String("3".to_owned())),
            ("/profile/debug_assertions", Value::Bool(false)),
            ("/fresh", Value::Bool(true)),
            ("/executable", Value::Null),
        ] {
            let mut changed = value.clone();
            *changed
                .pointer_mut(pointer)
                .context("artifact fixture pointer missing")? = replacement;
            assert!(
                compiler_artifact(
                    &serde_json::to_vec(&changed)?,
                    directory.path(),
                    &target,
                    &case
                )
                .is_err(),
                "accepted altered {pointer}"
            );
        }
        let text = String::from_utf8(bytes)?;
        assert!(
            compiler_artifact(
                format!("{text}\n{text}\n").as_bytes(),
                directory.path(),
                &target,
                &case
            )
            .is_err()
        );
        let executable = Path::new(
            value["executable"]
                .as_str()
                .context("fixture executable missing")?,
        );
        fs::write(executable, b"BAD!invalid executable")?;
        assert!(compiler_artifact(text.as_bytes(), directory.path(), &target, &case).is_err());
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rejects_compiler_executable_outside_fresh_target() -> Result<()> {
        let (directory, case, target, mut value) = artifact_fixture()?;
        let outside = directory.path().join("outside");
        fs::write(&outside, b"\x7fELFprotocol fixture")?;
        value["executable"] = serde_json::to_value(outside)?;
        assert!(
            compiler_artifact(
                &serde_json::to_vec(&value)?,
                directory.path(),
                &target,
                &case
            )
            .is_err()
        );
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rejects_wrong_runtime_executable_or_changed_bytes_and_incomplete_capture() -> Result<()> {
        let (directory, case, target, value) = artifact_fixture()?;
        let artifact = compiler_artifact(
            &serde_json::to_vec(&value)?,
            directory.path(),
            &target,
            &case,
        )?;
        let stdout = directory.path().join("stdout.log");
        let stderr = directory.path().join("stderr.log");
        fs::write(
            &stdout,
            VALID.replace("module::case", &case.fully_qualified_name),
        )?;
        fs::write(
            &stderr,
            format!(
                "     Running unittests {} ({})\n",
                artifact.src_path.display(),
                artifact.executable.display()
            ),
        )?;
        let mut proof = ProcessProof {
            argv: planned_argv(&case),
            success: true,
            exit_code: Some(0),
            elapsed_ms: 1,
            stopped_reason: None,
            stdout_eof: true,
            stderr_eof: true,
            leader_reaped: true,
            owned_group_signal_sent: false,
            stdout: log_proof(&stdout)?,
            stderr: log_proof(&stderr)?,
        };
        verify_runtime(directory.path(), &proof, &artifact, &case)?;
        proof.stdout_eof = false;
        assert!(verify_runtime(directory.path(), &proof, &artifact, &case).is_err());
        proof.stdout_eof = true;
        fs::write(
            &stderr,
            "     Running unittests source.rs (/different/executable)\n",
        )?;
        proof.stderr = log_proof(&stderr)?;
        assert!(verify_runtime(directory.path(), &proof, &artifact, &case).is_err());
        fs::write(
            &stderr,
            format!(
                "     Running unittests source.rs ({})\n",
                artifact.executable.display()
            ),
        )?;
        proof.stderr = log_proof(&stderr)?;
        fs::write(&artifact.executable, b"\x7fELFchanged fixture")?;
        assert!(verify_runtime(directory.path(), &proof, &artifact, &case).is_err());
        Ok(())
    }
}
