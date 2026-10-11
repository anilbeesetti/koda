//! The first provenance tool is compiled remotely and exported as a small artifact.
//! Root authenticates CI/run/source/ZIP/ELF evidence with existing host tools before
//! executing it. This Rust code cannot authorize its own local execution.

use super::{
    Authentication, FileDigest, MAX_METADATA_BYTES, SourceArgs, bounded_line, command, digest_file,
    filesystem_space, observe, open_noatime, physical, write_json,
};
use anyhow::{Context as _, Result, ensure};
use clap::Args;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const MAX_TOOL_BYTES: u64 = 24 * 1024 * 1024;
const MAX_BOOTSTRAP_LOG_BYTES: u64 = 4 * 1024 * 1024;
const MAX_FAILURE_EXPORT_BYTES: u64 = 29 * 1024 * 1024;
const FAILURE_MEMBERS: &[(&str, u64)] = &[
    (
        "source-before-build-observation.json",
        super::MAX_MANIFEST_BYTES,
    ),
    (
        "source-after-build-observation.json",
        super::MAX_MANIFEST_BYTES,
    ),
    ("cargo-build.jsonl", 64 * 1024 * 1024),
    ("cargo-process-closure.json", MAX_METADATA_BYTES),
    ("build-failure.json", MAX_METADATA_BYTES),
    ("build-command-failure.json", MAX_METADATA_BYTES),
    ("producer-admission.json", MAX_METADATA_BYTES),
    ("package-command-failure.json", MAX_METADATA_BYTES),
];

/// The first tool only constructs a new source directory. Its build is not
/// exported or accepted; the workflow recompiles the export candidate here.
#[derive(Args)]
pub struct PrepareCheckoutArgs {
    #[command(flatten)]
    source: SourceArgs,
    #[arg(long)]
    output_dir: PathBuf,
    #[arg(long)]
    receipt: PathBuf,
}

pub(super) fn prepare_checkout(args: PrepareCheckoutArgs) -> Result<()> {
    ensure!(
        args.receipt.is_absolute()
            && args.receipt.parent() == args.output_dir.parent()
            && args.receipt != args.output_dir,
        "checkout receipt must be a new sibling outside the constructed checkout"
    );
    let receipt = super::package::prepare_owned_checkout(&args.source, &args.output_dir)?;
    receipt.write_new_receipt(&args.receipt)
}

#[derive(Args)]
pub struct ExportArgs {
    #[command(flatten)]
    source: SourceArgs,
    #[arg(long)]
    cargo_build_log: PathBuf,
    #[arg(long)]
    run_id: String,
    #[arg(long)]
    run_attempt: String,
    #[arg(long)]
    output_dir: PathBuf,
}

#[derive(Args)]
pub struct FailureArgs {
    #[command(flatten)]
    source: SourceArgs,
    #[arg(long)]
    request: PathBuf,
    #[arg(long)]
    request_sha256: String,
    #[arg(long)]
    build_dir: PathBuf,
    #[arg(long)]
    package_dir: PathBuf,
    #[arg(long)]
    run_id: String,
    #[arg(long)]
    run_attempt: String,
    #[arg(long)]
    observed_build_step_conclusion: String,
    #[arg(long)]
    observed_package_step_conclusion: String,
    #[arg(long)]
    output_dir: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompileEvidence {
    command: Vec<String>,
    cargo_log: FileDigest,
    actual_unique_executable: PathBuf,
    actual_build_finished_success: bool,
    artifact_features: Vec<String>,
    artifact_debug: u64,
    // Internal observation; keep the existing exported compile schema unchanged.
    #[serde(skip)]
    artifact_size_optimized: Option<bool>,
    actual_cargo_exit_requires_independent_terminal_ci_step_binding: bool,
    actual_local_package_directories: Vec<PathBuf>,
}

fn compile_evidence(log: &Path, executable: &Path) -> Result<CompileEvidence> {
    let (digest, before) = digest_file(log, MAX_BOOTSTRAP_LOG_BYTES)?;
    let mut input = std::io::BufReader::new(open_noatime(log, false)?);
    let mut artifact = None;
    let mut finished = None;
    let mut local_packages = std::collections::BTreeSet::new();
    while let Some(line) = bounded_line(&mut input, 1024 * 1024)? {
        let message: serde_json::Value = serde_json::from_slice(&line)?;
        if message.get("reason").and_then(serde_json::Value::as_str) == Some("compiler-artifact") {
            if let Some(id) = message
                .get("package_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.strip_prefix("path+"))
            {
                ensure!(
                    id.len() <= 4096,
                    "bootstrap package-ID byte budget exhausted"
                );
                let url = url::Url::parse(id)?;
                ensure!(
                    url.scheme() == "file" && url.query().is_none(),
                    "unrecognized local Cargo package identity"
                );
                let path = url
                    .to_file_path()
                    .map_err(|()| anyhow::anyhow!("Cargo package file URL is not local"))?;
                ensure!(
                    local_packages.len() < 512 || local_packages.contains(&path),
                    "bootstrap local package count exhausted"
                );
                local_packages.insert(path);
            }
        }
        match message.get("reason").and_then(serde_json::Value::as_str) {
            Some("compiler-artifact")
                if message
                    .pointer("/target/name")
                    .and_then(serde_json::Value::as_str)
                    == Some("xtask") =>
            {
                ensure!(
                    artifact.is_none()
                        && message
                            .pointer("/profile/test")
                            .and_then(serde_json::Value::as_bool)
                            == Some(false),
                    "bootstrap must have exactly one non-test xtask compiler artifact"
                );
                let path = PathBuf::from(
                    message
                        .get("executable")
                        .and_then(serde_json::Value::as_str)
                        .context("bootstrap Cargo executable missing")?,
                );
                ensure!(
                    path == executable,
                    "bootstrap exporter is not the actual Cargo-emitted executable"
                );
                let features: Vec<String> = serde_json::from_value(
                    message
                        .get("features")
                        .context("bootstrap features absent")?
                        .clone(),
                )?;
                ensure!(
                    features.len() <= 512 && features.iter().all(|value| value.len() <= 1024),
                    "bootstrap features exceed bound"
                );
                let debug = message
                    .pointer("/profile/debuginfo")
                    .and_then(serde_json::Value::as_u64)
                    .context("bootstrap debug profile absent")?;
                ensure!(
                    debug == 0,
                    "bootstrap export requires the observed debug-zero profile"
                );
                let size_optimized = message
                    .pointer("/profile/opt_level")
                    .and_then(serde_json::Value::as_str)
                    .map(|value| value == "s");
                artifact = Some((path, features, debug, size_optimized));
            }
            Some("build-finished") => {
                ensure!(
                    finished.is_none(),
                    "duplicate bootstrap build-finished message"
                );
                finished = Some(
                    message
                        .get("success")
                        .and_then(serde_json::Value::as_bool)
                        .context("bootstrap build-finished success must be boolean")?,
                );
            }
            _ => {}
        }
    }
    drop(input);
    ensure!(
        finished == Some(true)
            && digest_file(log, MAX_BOOTSTRAP_LOG_BYTES)? == (digest.clone(), before),
        "bootstrap log completion or complete physical/digest inverse differs"
    );
    let (path, features, debug, size_optimized) =
        artifact.context("bootstrap xtask compiler artifact absent")?;
    Ok(CompileEvidence {
        command: [
            "cargo",
            "build",
            "--locked",
            "-p",
            "xtask",
            "--bin",
            "xtask",
            "--message-format=json-render-diagnostics",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        cargo_log: digest,
        actual_unique_executable: path,
        actual_build_finished_success: true,
        artifact_features: features,
        artifact_debug: debug,
        artifact_size_optimized: size_optimized,
        actual_cargo_exit_requires_independent_terminal_ci_step_binding: true,
        actual_local_package_directories: local_packages.into_iter().collect(),
    })
}

// Parser compatibility and export admission are separate: a legacy log may
// omit opt_level, but an exported controller executable must observe it as s.
fn require_export_profile(compile: &CompileEvidence) -> Result<()> {
    ensure!(
        compile.artifact_size_optimized == Some(true),
        "bootstrap export requires the actual Cargo-observed size-optimized profile opt_level=s"
    );
    Ok(())
}

// Cargo reports every actually compiled workspace/path package. Export the full
// tracked subtree of each package (including build.rs/assets), root Cargo/lock/
// toolchain and repository Cargo configuration, rather than only xtask sources.
// Root independently binds these entries to immutable Git modes/blobs/byte SHAs;
// a full controller canonical digest is not required to admit the first tool.
fn compile_inputs(
    manifest: &super::SourceManifest,
    directories: &[PathBuf],
    controller: &Path,
) -> Result<(Vec<String>, Vec<super::SourceEntry>)> {
    use std::path::Component;
    ensure!(
        !directories.is_empty() && directories.len() <= 512,
        "actual local compile package closure absent/oversized"
    );
    let mut roots = std::collections::BTreeSet::new();
    for directory in directories {
        let relative = directory
            .strip_prefix(controller)
            .context("compiled local package escaped immutable controller")?;
        ensure!(
            relative
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
            "local compile package path is not confined"
        );
        let relative = relative
            .to_str()
            .context("local compile package path is not UTF-8")?;
        // A root package would require the entire tree, which exceeds the narrow
        // bootstrap closure contract; reject explicitly rather than omit inputs.
        super::relative(relative)?;
        ensure!(
            manifest
                .entries
                .iter()
                .any(|entry| entry.path == format!("{relative}/Cargo.toml")),
            "actual local compile package manifest is not tracked in the pinned controller"
        );
        roots.insert(relative.to_owned());
    }
    ensure!(
        roots.contains("tooling/xtask"),
        "actual xtask local package missing"
    );
    for required in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"] {
        ensure!(
            manifest.entries.iter().any(|entry| entry.path == required),
            "mandatory bootstrap input {required} absent"
        );
    }
    let selected = |entry: &&super::SourceEntry| {
        matches!(
            entry.path.as_str(),
            "Cargo.toml" | "Cargo.lock" | "rust-toolchain.toml"
        ) || entry.path.starts_with(".cargo/")
            || roots.iter().any(|root| {
                entry
                    .path
                    .strip_prefix(root.as_str())
                    .is_some_and(|suffix| suffix.starts_with('/'))
            })
    };
    let count = manifest.entries.iter().filter(selected).count();
    ensure!(
        count > 0 && count <= 512,
        "bootstrap complete compile-input closure exceeds bound BEFORE cloning"
    );
    let mut paths: std::collections::BTreeSet<&str> = manifest
        .entries
        .iter()
        .filter(selected)
        .map(|entry| entry.path.as_str())
        .collect();
    let mut links: std::collections::VecDeque<_> = manifest
        .entries
        .iter()
        .filter(|entry| paths.contains(entry.path.as_str()) && entry.link_target.is_some())
        .collect();
    while let Some(link) = links.pop_front() {
        let target = super::normalized_link(
            &link.path,
            link.link_target
                .as_deref()
                .context("compile-input link target absent")?,
        )?;
        let mut found = false;
        for entry in &manifest.entries {
            if entry.path == target
                || entry
                    .path
                    .strip_prefix(target.as_str())
                    .is_some_and(|suffix| suffix.starts_with('/'))
            {
                found = true;
                if !paths.contains(entry.path.as_str()) {
                    ensure!(
                        paths.len() < 512,
                        "bootstrap compile-input symlink closure exceeds bound BEFORE growth"
                    );
                    paths.insert(entry.path.as_str());
                    if entry.link_target.is_some() {
                        links.push_back(entry);
                    }
                }
            }
        }
        ensure!(
            found,
            "bootstrap compile-input symlink target is not tracked"
        );
    }
    let sources = manifest
        .entries
        .iter()
        .filter(|entry| paths.contains(entry.path.as_str()))
        .cloned()
        .collect();
    Ok((roots.into_iter().collect(), sources))
}

pub(super) fn export(args: ExportArgs) -> Result<()> {
    for (name, expected) in [
        ("CARGO_INCREMENTAL", "0"),
        ("CARGO_PROFILE_DEV_DEBUG", "0"),
        ("CARGO_PROFILE_DEV_BUILD_OVERRIDE_DEBUG", "0"),
        ("CARGO_PROFILE_DEV_OPT_LEVEL", "s"),
        ("CARGO_PROFILE_DEV_STRIP", "symbols"),
    ] {
        ensure!(
            std::env::var(name).as_deref() == Ok(expected),
            "bootstrap requires {name}={expected}"
        );
    }
    super::require_hex(&args.source.sha, 40)?;
    super::require_hex(&args.source.tree, 40)?;
    ensure!(
        !args.run_id.is_empty()
            && args.run_id.bytes().all(|byte| byte.is_ascii_digit())
            && !args.run_attempt.is_empty()
            && args.run_attempt.bytes().all(|byte| byte.is_ascii_digit()),
        "invalid bootstrap run identity"
    );
    let executable = std::env::current_exe()?;
    let compile = compile_evidence(&args.cargo_build_log, &executable)?;
    require_export_profile(&compile)?;
    let (binary, binary_identity) = digest_file(&executable, MAX_TOOL_BYTES).context(
        "authenticate actual Cargo-emitted bootstrap executable under the unchanged tool cap",
    )?;
    let (manifest, observation) = observe(&args.source, Authentication::Materialized)?;
    let controller = std::path::absolute(&args.source.source)?;
    let (compile_package_roots, sources) = compile_inputs(
        &manifest,
        &compile.actual_local_package_directories,
        &controller,
    )?;
    let parent = args
        .output_dir
        .parent()
        .context("bootstrap output parent absent")?;
    let (block, available, inodes) = filesystem_space(parent)?;
    let required = MAX_TOOL_BYTES
        + MAX_BOOTSTRAP_LOG_BYTES
        + MAX_METADATA_BYTES
        + 32 * block
        + 64 * 1024 * 1024
        + super::ORIGINAL_FLOOR;
    ensure!(
        available >= required && inodes >= 32,
        "bootstrap producer disk/inode reserve does not fit"
    );
    fs::create_dir(&args.output_dir).context("bootstrap artifact directory must be new")?;
    let copied_path = args.output_dir.join("xtask");
    super::package::copy_verified(&executable, &copied_path, &binary, 0o555)?;
    let elf = super::package::inspect_bootstrap(&copied_path)?;
    ensure!(
        digest_file(&copied_path, MAX_TOOL_BYTES)?.0 == binary
            && digest_file(&executable, MAX_TOOL_BYTES)? == (binary.clone(), binary_identity),
        "actual bootstrap copied/original ELF byte inverse differs"
    );
    super::package::copy_verified(
        &args.cargo_build_log,
        &args.output_dir.join("cargo-build.jsonl"),
        &compile.cargo_log,
        0o644,
    )?;
    let rustc = String::from_utf8(command(
        Some(&args.source.source),
        "rustc",
        &["-vV"],
        64 * 1024,
    )?)?;
    let cargo = String::from_utf8(command(
        Some(&args.source.source),
        "cargo",
        &["--version"],
        64 * 1024,
    )?)?;
    #[derive(Serialize)]
    struct Metadata {
        schema_version: u32,
        state: &'static str,
        repository: &'static str,
        controller_sha: String,
        controller_tree: String,
        controller_manifest: FileDigest,
        run_id: String,
        run_attempt: String,
        binary: FileDigest,
        elf: serde_json::Value,
        compile: CompileEvidence,
        exact_controller_compile_inputs: Vec<super::SourceEntry>,
        exact_controller_compile_input_package_roots: Vec<String>,
        rustc: String,
        cargo: String,
        download_zip_and_terminal_job_binding_requires_root_authentication: bool,
        root_disk_memory_elf_host_abi_and_actor_admission_pending: bool,
        application_test_runtime_reference_credit: u32,
    }
    let metadata = Metadata {
        schema_version: 1,
        state: "REMOTE_CONTROLLER_TOOL_PROVENANCE_NOT_ROOT_EXECUTION_ADMISSION",
        repository: super::REPOSITORY,
        controller_sha: args.source.sha,
        controller_tree: args.source.tree,
        controller_manifest: observation.canonical_manifest,
        run_id: args.run_id,
        run_attempt: args.run_attempt,
        binary,
        elf,
        compile,
        exact_controller_compile_inputs: sources,
        exact_controller_compile_input_package_roots: compile_package_roots,
        rustc,
        cargo,
        download_zip_and_terminal_job_binding_requires_root_authentication: true,
        root_disk_memory_elf_host_abi_and_actor_admission_pending: true,
        application_test_runtime_reference_credit: 0,
    };
    write_json(
        &args.output_dir.join("bootstrap.json"),
        &metadata,
        MAX_METADATA_BYTES,
    )?;
    Ok(())
}

#[derive(Debug, Serialize)]
struct FailureMember {
    original_path: PathBuf,
    observed_absent: bool,
    actual_read_error: Option<String>,
    original_digest: Option<FileDigest>,
    original_physical13: Option<super::Physical13>,
    exported_ranges: Vec<(String, u64, u64, FileDigest)>,
    complete_original_bytes_exported: bool,
}

fn copy_range(input: &mut fs::File, output: &Path, offset: u64, length: u64) -> Result<FileDigest> {
    input.seek(SeekFrom::Start(offset))?;
    let mut writer = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)?;
    let mut remaining = length;
    let mut buffer = vec![0; 64 * 1024];
    while remaining > 0 {
        let count = usize::try_from(remaining.min(buffer.len() as u64))?;
        input.read_exact(&mut buffer[..count])?;
        writer.write_all(&buffer[..count])?;
        remaining -= count as u64;
    }
    writer.sync_all()?;
    drop(writer);
    Ok(digest_file(output, length)?.0)
}

fn failure_member(path: &Path, output: &Path, index: usize, maximum: u64) -> Result<FailureMember> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FailureMember {
                original_path: path.to_owned(),
                observed_absent: true,
                actual_read_error: None,
                original_digest: None,
                original_physical13: None,
                exported_ranges: vec![],
                complete_original_bytes_exported: false,
            });
        }
        Err(error) => return Err(error.into()),
        Ok(metadata) => ensure!(
            metadata.is_file(),
            "failure evidence must be a literal regular file"
        ),
    }
    let (digest, before) = digest_file(path, maximum)?;
    let mut input = open_noatime(path, false)?;
    let mut ranges = Vec::new();
    let complete = digest.bytes <= MAX_METADATA_BYTES;
    let spans = if complete {
        vec![(0, digest.bytes)]
    } else {
        let tail = if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            4 * 1024 * 1024
        } else {
            256 * 1024
        };
        let tail = tail.min(digest.bytes - 256 * 1024);
        vec![(0, 256 * 1024), (digest.bytes - tail, tail)]
    };
    for (part, (offset, length)) in spans.into_iter().enumerate() {
        let name = format!("evidence-{index:02}-{part:02}.bin");
        let actual = copy_range(&mut input, &output.join(&name), offset, length)?;
        ranges.push((name, offset, length, actual));
    }
    ensure!(
        before == physical(&input.metadata()?)?
            && digest_file(path, maximum)? == (digest.clone(), before.clone()),
        "failure evidence complete original inverse differs"
    );
    Ok(FailureMember {
        original_path: path.to_owned(),
        observed_absent: false,
        actual_read_error: None,
        original_digest: Some(digest),
        original_physical13: Some(before),
        exported_ranges: ranges,
        complete_original_bytes_exported: complete,
    })
}

pub(super) fn export_failure(args: FailureArgs) -> Result<()> {
    for conclusion in [
        &args.observed_build_step_conclusion,
        &args.observed_package_step_conclusion,
    ] {
        ensure!(
            matches!(
                conclusion.as_str(),
                "success" | "failure" | "cancelled" | "skipped"
            ),
            "unsupported actual workflow step conclusion"
        );
    }
    let request: super::Request = super::authenticated_json(
        &args.request,
        &args.request_sha256,
        super::MAX_REQUEST_BYTES,
    )?;
    ensure!(
        request.source_sha == args.source.sha && request.source_tree == args.source.tree,
        "failure export source/request identity differs"
    );
    let (block, available, inodes) = filesystem_space(
        args.output_dir
            .parent()
            .context("failure output parent absent")?,
    )?;
    let export_upper = MAX_METADATA_BYTES
        + (FAILURE_MEMBERS.len() as u64 - 1) * MAX_METADATA_BYTES
        + 4 * 1024 * 1024
        + 256 * 1024;
    ensure!(
        export_upper <= MAX_FAILURE_EXPORT_BYTES,
        "whitelisted failure export upper bound exceeds transport budget before writes"
    );
    ensure!(
        available
            >= MAX_FAILURE_EXPORT_BYTES + 64 * block + 64 * 1024 * 1024 + super::ORIGINAL_FLOOR
            && inodes >= 64,
        "failure-export original floor/inode reserve does not fit"
    );
    fs::create_dir(&args.output_dir)
        .context("failure export directory must be new; retry uses a fresh directory")?;
    let mut members = Vec::new();
    let mut actual_errors = Vec::new();
    for (index, (name, maximum)) in FAILURE_MEMBERS.iter().enumerate() {
        let parent = if name.starts_with("producer-") || name.starts_with("package-") {
            &args.package_dir
        } else {
            &args.build_dir
        };
        let path = parent.join(name);
        match failure_member(&path, &args.output_dir, index, *maximum) {
            Ok(member) => members.push(member),
            Err(error) => {
                let actual = format!("{error:#}");
                actual_errors.push(actual.clone());
                members.push(FailureMember {
                    original_path: path,
                    observed_absent: false,
                    actual_read_error: Some(actual),
                    original_digest: None,
                    original_physical13: None,
                    exported_ranges: vec![],
                    complete_original_bytes_exported: false,
                });
            }
        }
    }
    let exported = members
        .iter()
        .flat_map(|member| &member.exported_ranges)
        .try_fold(MAX_METADATA_BYTES, |bytes, (_, _, count, _)| {
            bytes
                .checked_add(*count)
                .context("failure export size overflow")
        })?;
    ensure!(
        exported <= MAX_FAILURE_EXPORT_BYTES,
        "failure export aggregate byte budget exhausted"
    );
    #[derive(Serialize)]
    struct Index {
        state: &'static str,
        request: super::Request,
        run_id: String,
        run_attempt: String,
        observed_build_step_conclusion: String,
        observed_package_step_conclusion: String,
        members: Vec<FailureMember>,
        actual_export_errors: Vec<String>,
        original_files_deleted: u32,
        complete_original_files_are_retained_on_runner: bool,
        actual_terminal_job_logs_must_be_pinned_separately: bool,
        build_test_runtime_reference_credit: u32,
    }
    let has_errors = !actual_errors.is_empty();
    write_json(
        &args.output_dir.join("failure-index.json"),
        &Index {
            state: "BOUNDED_ACTUAL_EVIDENCE_EXPORT_NOT_BUILD_TEST_OR_RUNTIME_ACCEPTANCE",
            request,
            run_id: args.run_id,
            run_attempt: args.run_attempt,
            observed_build_step_conclusion: args.observed_build_step_conclusion,
            observed_package_step_conclusion: args.observed_package_step_conclusion,
            members,
            actual_export_errors: actual_errors,
            original_files_deleted: 0,
            complete_original_files_are_retained_on_runner: true,
            actual_terminal_job_logs_must_be_pinned_separately: true,
            build_test_runtime_reference_credit: 0,
        },
        MAX_METADATA_BYTES,
    )?;
    ensure!(
        !has_errors,
        "actual failure evidence read/export errors retained in failure-index.json"
    );
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn failure_export_retains_full_original_and_exact_head_tail_ranges() {
        let temporary = tempfile::tempdir().unwrap();
        let input = temporary.path().join("cargo-build.jsonl");
        let output = temporary.path().join("export");
        fs::create_dir(&output).unwrap();
        let bytes: Vec<_> = (0..2 * 1024 * 1024).map(|n| (n % 251) as u8).collect();
        fs::write(&input, &bytes).unwrap();
        let original = digest_file(&input, 4 * 1024 * 1024).unwrap();
        let member = failure_member(&input, &output, 3, 4 * 1024 * 1024).unwrap();
        assert!(!member.complete_original_bytes_exported);
        assert_eq!(member.original_digest, Some(original.0.clone()));
        for (name, offset, count, digest) in member.exported_ranges {
            assert_eq!(
                fs::read(output.join(name)).unwrap(),
                bytes[offset as usize..(offset + count) as usize]
            );
            assert_eq!(digest.bytes, count);
        }
        assert_eq!(digest_file(&input, 4 * 1024 * 1024).unwrap(), original);
    }

    #[test]
    fn absent_and_over_budget_failure_inputs_never_gain_an_acceptance_receipt() {
        let temporary = tempfile::tempdir().unwrap();
        let output = temporary.path().join("export");
        fs::create_dir(&output).unwrap();
        let absent = failure_member(&temporary.path().join("missing"), &output, 0, 16).unwrap();
        assert!(absent.observed_absent);
        assert!(absent.original_digest.is_none());
        let input = temporary.path().join("oversized");
        fs::write(&input, [0; 64]).unwrap();
        assert!(failure_member(&input, &output, 1, 16).is_err());
        assert!(input.exists());
        assert_eq!(fs::read_dir(&output).unwrap().count(), 0);
    }

    #[test]
    fn bootstrap_cargo_parser_requires_actual_unique_executable_profile_and_success_messages() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("fixture-cargo.jsonl");
        let executable = Path::new("/fixture-only/xtask");
        let artifact = serde_json::json!({ "reason": "compiler-artifact", "target": { "name": "xtask" },
            "profile": { "test": false, "debuginfo": 0 }, "features": [], "executable": executable });
        let finish = serde_json::json!({ "reason": "build-finished", "success": true });
        let write = |messages: &[serde_json::Value]| {
            let bytes = messages
                .iter()
                .map(|value| format!("{value}\n"))
                .collect::<String>();
            fs::write(&path, bytes).unwrap();
        };
        write(&[artifact.clone(), finish.clone()]);
        let actual = compile_evidence(&path, executable).unwrap();
        assert!(actual.actual_cargo_exit_requires_independent_terminal_ci_step_binding);
        write(&[artifact.clone(), artifact.clone(), finish.clone()]);
        assert!(compile_evidence(&path, executable).is_err());
        write(&[artifact.clone()]);
        assert!(compile_evidence(&path, executable).is_err());
        write(&[
            artifact.clone(),
            serde_json::json!({ "reason": "build-finished", "success": false }),
        ]);
        assert!(compile_evidence(&path, executable).is_err());
        let mut debug = artifact.clone();
        debug["profile"]["debuginfo"] = serde_json::json!(2);
        write(&[debug, finish.clone()]);
        assert!(compile_evidence(&path, executable).is_err());
        let mut wrong = artifact;
        wrong["executable"] = serde_json::json!("/different/xtask");
        write(&[wrong, finish]);
        assert!(compile_evidence(&path, executable).is_err());
    }
}

#[cfg(test)]
mod compile_closure_regressions {
    use super::*;

    #[test]
    fn actual_local_dependency_sources_configs_and_build_assets_are_required_before_bootstrap_export()
     {
        let entry = |path: &str| super::super::SourceEntry {
            path: path.to_owned(),
            mode: "100644".to_owned(),
            git_blob: super::super::git_digest("blob", b"fixture"),
            bytes: 7,
            sha256: super::super::sha256(b"fixture"),
            link_target: None,
        };
        let mut manifest = super::super::SourceManifest {
            schema_version: 1,
            repository: super::super::REPOSITORY.to_owned(),
            commit: "1".repeat(40),
            tree: "2".repeat(40),
            entries: [
                "Cargo.lock",
                "Cargo.toml",
                "rust-toolchain.toml",
                ".cargo/config.toml",
                "tooling/xtask/Cargo.toml",
                "tooling/xtask/src/main.rs",
                "tooling/compliance/Cargo.toml",
                "tooling/compliance/build.rs",
                "tooling/compliance/src/lib.rs",
                "tooling/compliance/assets/license-data",
                "uncompiled-package/Cargo.toml",
                "uncompiled-package/src/lib.rs",
            ]
            .into_iter()
            .map(entry)
            .collect(),
        };
        manifest.entries.push(entry("LICENSE"));
        let mut link = entry("tooling/compliance/LICENSE");
        link.mode = "120000".to_owned();
        link.link_target = Some("../../LICENSE".to_owned());
        manifest.entries.push(link);
        let root = Path::new("/controller");
        let directories = [root.join("tooling/xtask"), root.join("tooling/compliance")];
        let (roots, selected) = compile_inputs(&manifest, &directories, root).unwrap();
        assert_eq!(
            roots,
            vec!["tooling/compliance".to_owned(), "tooling/xtask".to_owned()]
        );
        assert!(
            selected
                .iter()
                .any(|entry| entry.path == "tooling/compliance/build.rs")
        );
        assert!(
            selected
                .iter()
                .any(|entry| entry.path == "tooling/compliance/assets/license-data")
        );
        assert!(
            selected
                .iter()
                .any(|entry| entry.path == ".cargo/config.toml")
        );
        assert!(selected.iter().any(|entry| entry.path == "LICENSE"));
        assert!(
            !selected
                .iter()
                .any(|entry| entry.path.starts_with("uncompiled-package/"))
        );
        assert!(compile_inputs(&manifest, &[], root).is_err());
        assert!(
            compile_inputs(
                &manifest,
                &[PathBuf::from("/outside/compiled-package")],
                root
            )
            .is_err()
        );
        assert!(compile_inputs(&manifest, &[root.join("tooling/missing")], root).is_err());
        manifest.entries.retain(|entry| entry.path != "Cargo.lock");
        assert!(compile_inputs(&manifest, &directories, root).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn bootstrap_tool_cap_kind_and_symlink_failures_preserve_full_physical_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let executable = temporary.path().join("bootstrap-executable");
        let writer = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&executable)
            .unwrap();
        writer.set_len(MAX_TOOL_BYTES + 1).unwrap();
        writer.sync_all().unwrap();
        drop(writer);
        let before = physical(&fs::symlink_metadata(&executable).unwrap()).unwrap();
        let error = digest_file(&executable, MAX_TOOL_BYTES).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("expected a bounded regular file"));
        assert!(message.contains("is_regular=true"));
        assert!(message.contains(&format!("observed_bytes={}", MAX_TOOL_BYTES + 1)));
        assert!(message.contains(&format!("maximum_bytes={MAX_TOOL_BYTES}")));
        assert!(message.contains(&executable.display().to_string()));
        assert_eq!(
            physical(&fs::symlink_metadata(&executable).unwrap()).unwrap(),
            before
        );

        // This fixture mutation precedes the next physical-identity capture.
        let writer = OpenOptions::new().write(true).open(&executable).unwrap();
        writer.set_len(MAX_TOOL_BYTES).unwrap();
        writer.sync_all().unwrap();
        drop(writer);
        let boundary = physical(&fs::symlink_metadata(&executable).unwrap()).unwrap();
        let (digest, observed) = digest_file(&executable, MAX_TOOL_BYTES).unwrap();
        assert_eq!(digest.bytes, MAX_TOOL_BYTES);
        assert_eq!(
            digest.sha256,
            "95aeaae03b56c171cf88753c821630a3c24f1fcf406cec3e17d56781aa3f8369"
        );
        assert_eq!(observed, boundary);
        assert_eq!(
            physical(&fs::symlink_metadata(&executable).unwrap()).unwrap(),
            boundary
        );

        let directory = temporary.path().join("directory");
        fs::create_dir(&directory).unwrap();
        let directory_before = physical(&fs::symlink_metadata(&directory).unwrap()).unwrap();
        let error = digest_file(&directory, MAX_TOOL_BYTES).unwrap_err();
        assert!(format!("{error:#}").contains("is_regular=false"));
        assert_eq!(
            physical(&fs::symlink_metadata(&directory).unwrap()).unwrap(),
            directory_before
        );

        let link = temporary.path().join("link");
        std::os::unix::fs::symlink(&executable, &link).unwrap();
        let link_before = physical(&fs::symlink_metadata(&link).unwrap()).unwrap();
        let target_before = physical(&fs::symlink_metadata(&executable).unwrap()).unwrap();
        assert!(digest_file(&link, MAX_TOOL_BYTES).is_err());
        assert_eq!(
            physical(&fs::symlink_metadata(&link).unwrap()).unwrap(),
            link_before
        );
        assert_eq!(
            physical(&fs::symlink_metadata(&executable).unwrap()).unwrap(),
            target_before
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn bootstrap_compile_evidence_binds_actual_size_profile_and_rejects_profile_tampering() {
        let temporary = tempfile::tempdir().unwrap();
        let log = temporary.path().join("cargo-build.jsonl");
        let executable = PathBuf::from("/controller-target/debug/xtask");
        let artifact = serde_json::json!({
            "reason": "compiler-artifact",
            "package_id": "path+file:///controller/tooling/xtask#0.1.0",
            "target": { "name": "xtask" },
            "executable": executable,
            "features": [],
            "profile": { "test": false, "debuginfo": 0, "opt_level": "s" },
        });
        let write_log = |artifact: &serde_json::Value| {
            let finished = serde_json::json!({ "reason": "build-finished", "success": true });
            fs::write(&log, format!("{artifact}\n{finished}\n")).unwrap();
        };
        write_log(&artifact);
        let before = physical(&fs::symlink_metadata(&log).unwrap()).unwrap();
        let evidence = compile_evidence(&log, &executable)
            .and_then(|compile| {
                require_export_profile(&compile)?;
                Ok(compile)
            })
            .unwrap();
        assert_eq!(evidence.actual_unique_executable, executable);
        assert_eq!(evidence.artifact_debug, 0);
        assert!(evidence.actual_build_finished_success);
        assert_eq!(
            evidence.actual_local_package_directories,
            vec![PathBuf::from("/controller/tooling/xtask")]
        );
        assert_eq!(
            physical(&fs::symlink_metadata(&log).unwrap()).unwrap(),
            before
        );

        let mut unoptimized = artifact.clone();
        unoptimized["profile"]["opt_level"] = serde_json::json!("0");
        write_log(&unoptimized);
        let before = physical(&fs::symlink_metadata(&log).unwrap()).unwrap();
        let error = compile_evidence(&log, &executable)
            .and_then(|compile| require_export_profile(&compile))
            .unwrap_err();
        assert!(
            format!("{error:#}")
                .contains("actual Cargo-observed size-optimized profile opt_level=s")
        );
        assert_eq!(
            physical(&fs::symlink_metadata(&log).unwrap()).unwrap(),
            before
        );

        let mut missing = artifact.clone();
        assert!(
            missing["profile"]
                .as_object_mut()
                .unwrap()
                .remove("opt_level")
                .is_some()
        );
        write_log(&missing);
        let before = physical(&fs::symlink_metadata(&log).unwrap()).unwrap();
        assert!(
            compile_evidence(&log, &executable)
                .and_then(|compile| require_export_profile(&compile))
                .is_err()
        );
        assert_eq!(
            physical(&fs::symlink_metadata(&log).unwrap()).unwrap(),
            before
        );
        let mut test_artifact = artifact.clone();
        test_artifact["profile"]["test"] = serde_json::json!(true);
        write_log(&test_artifact);
        let before = physical(&fs::symlink_metadata(&log).unwrap()).unwrap();
        assert!(
            compile_evidence(&log, &executable)
                .and_then(|compile| require_export_profile(&compile))
                .is_err()
        );
        assert_eq!(
            physical(&fs::symlink_metadata(&log).unwrap()).unwrap(),
            before
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod constructed_checkout_compile_binding_regression {
    use super::*;

    #[test]
    fn exported_compile_inputs_require_actual_constructed_roots_and_reject_initial_constructor_roots()
     {
        let (_borrowed, mut source) = super::super::tests::fixture();
        for (path, bytes) in [
            ("Cargo.toml", b"[workspace]\n".as_slice()),
            ("Cargo.lock", b"version = 4\n".as_slice()),
            (
                "rust-toolchain.toml",
                b"[toolchain]\nchannel = \"stable\"\n".as_slice(),
            ),
            (
                "tooling/xtask/Cargo.toml",
                b"[package]\nname = \"xtask\"\n".as_slice(),
            ),
            ("tooling/xtask/src/main.rs", b"fn main() {}\n".as_slice()),
        ] {
            let path = source.source.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            super::super::create_file(&path, bytes).unwrap();
        }
        command(Some(&source.source), "git", &["add", "--all"], 4096).unwrap();
        command(
            Some(&source.source),
            "git",
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-q",
                "--no-gpg-sign",
                "-m",
                "bootstrap fixture",
            ],
            4096,
        )
        .unwrap();
        source.sha = String::from_utf8(
            command(Some(&source.source), "git", &["rev-parse", "HEAD"], 1024).unwrap(),
        )
        .unwrap()
        .trim()
        .to_owned();
        source.tree = String::from_utf8(
            command(
                Some(&source.source),
                "git",
                &["rev-parse", "HEAD^{tree}"],
                1024,
            )
            .unwrap(),
        )
        .unwrap()
        .trim()
        .to_owned();
        let staging = tempfile::tempdir().unwrap();
        let constructed = staging.path().join("constructed-controller");
        super::super::package::prepare_owned_checkout(&source, &constructed).unwrap();
        let owned_source = SourceArgs {
            source: constructed.clone(),
            sha: source.sha.clone(),
            tree: source.tree.clone(),
        };
        let (manifest, _) = observe(&owned_source, Authentication::Materialized).unwrap();
        let executable = staging.path().join("target/debug/xtask");
        for (label, compiler_root, expected_admission) in [
            ("initial-constructor", source.source.as_path(), false),
            ("constructed-controller", constructed.as_path(), true),
        ] {
            let directory = compiler_root.join("tooling/xtask");
            let package_url = url::Url::from_directory_path(&directory).unwrap();
            let artifact = serde_json::json!({
                "reason": "compiler-artifact", "package_id": format!("path+{package_url}#xtask@0.1.0"),
                "target": {"name": "xtask"}, "profile": {"test": false, "debuginfo": 0, "opt_level": "s"},
                "executable": executable, "features": [],
            });
            let finish = serde_json::json!({"reason": "build-finished", "success": true});
            let log = staging.path().join(format!("{label}.jsonl"));
            super::super::create_file(&log, format!("{artifact}\n{finish}\n").as_bytes()).unwrap();
            let evidence = compile_evidence(&log, &executable).unwrap();
            require_export_profile(&evidence).unwrap();
            let result = compile_inputs(
                &manifest,
                &evidence.actual_local_package_directories,
                &constructed,
            );
            assert_eq!(result.is_ok(), expected_admission, "{label}");
            if let Ok((roots, inputs)) = result {
                assert_eq!(roots, vec!["tooling/xtask".to_owned()]);
                assert!(
                    inputs
                        .iter()
                        .any(|input| input.path == "tooling/xtask/src/main.rs")
                );
                assert!(inputs.iter().any(|input| input.path == "Cargo.lock"));
                assert_eq!(
                    inputs
                        .iter()
                        .find(|input| input.path == "tooling/xtask/src/main.rs")
                        .unwrap()
                        .sha256,
                    super::super::sha256(b"fn main() {}\n")
                );
            }
        }
    }
}
