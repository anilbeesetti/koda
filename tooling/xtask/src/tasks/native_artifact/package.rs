use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail, ensure};
use clap::Args;
use flate2::{
    Compression,
    write::{GzEncoder, ZlibEncoder},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    Authentication, FileDigest, GitBatch, MAX_MANIFEST_BYTES, MAX_METADATA_BYTES, MAX_PIECES,
    MAX_ZIP_BYTES, OwnedChild, PIECE_BYTES, Physical13, PhysicalEntry, REPOSITORY, Request,
    SourceArgs, SourceManifest, SourceObservation, authenticated_json, bounded_line, command,
    create_file, digest_file, link_bytes, normalized_link, observe, open_beneath, open_noatime,
    payload_relative, physical, read_bounded, require_hex, sha256, validate_link_graph, write_json,
};

mod bounded_tar;
pub(super) mod elf;
mod producer;

const MAX_BINARY_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_BUILD_LOG_BYTES: u64 = 64 * 1024 * 1024;
const MAX_BUILD_LINE_BYTES: usize = 2 * 1024 * 1024;
const MAX_BUILD_EVIDENCE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PAYLOAD_ENTRIES: usize = 60_000;
const MAX_PAYLOAD_BYTES: u64 = 12 * 1024 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = PIECE_BYTES * MAX_PIECES as u64;

pub(super) fn inspect_bootstrap(path: &Path) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(elf::inspect(path)?)?)
}

#[derive(Args)]
pub struct BuildArgs {
    #[command(flatten)]
    source: SourceArgs,
    #[arg(long)]
    request: PathBuf,
    #[arg(long)]
    request_sha256: String,
    #[arg(long)]
    controller_sha: String,
    #[arg(long)]
    run_id: String,
    #[arg(long)]
    run_attempt: String,
    /// New directory; partial failures remain here and are never called PASS.
    #[arg(long)]
    output_dir: PathBuf,
    /// Optional CI output file; receives only the completed actual receipt digest.
    #[arg(long)]
    github_output: Option<PathBuf>,
}

#[derive(Args)]
pub struct PackageArgs {
    #[command(flatten)]
    source: SourceArgs,
    #[arg(long)]
    request: PathBuf,
    #[arg(long)]
    request_sha256: String,
    #[arg(long)]
    build_receipt: PathBuf,
    #[arg(long)]
    build_receipt_sha256: String,
    /// New directory containing runtime, manifests, archive and ordered pieces.
    #[arg(long)]
    output_dir: PathBuf,
}

#[derive(Args)]
pub struct ExtractArgs {
    #[arg(long)]
    metadata: PathBuf,
    #[arg(long)]
    metadata_sha256: String,
    #[arg(long)]
    request: PathBuf,
    #[arg(long)]
    request_sha256: String,
    #[arg(long)]
    payload_manifest: PathBuf,
    #[arg(long)]
    pieces: PathBuf,
    /// New directory on the separately admitted staging filesystem.
    #[arg(long)]
    output_dir: PathBuf,
    #[arg(long)]
    receipt: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Piece {
    pub(super) order: usize,
    pub(super) file: String,
    pub(super) bytes: u64,
    pub(super) sha256: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ArtifactMetadata {
    pub(super) schema_version: u32,
    pub(super) repository: String,
    pub(super) source_sha: String,
    pub(super) source_tree: String,
    pub(super) source_manifest_sha256: String,
    pub(super) source_owner_receipt_sha256: String,
    pub(super) controller_sha: String,
    pub(super) run_id: String,
    pub(super) run_attempt: String,
    pub(super) runtime_archive: FileDigest,
    pub(super) payload_manifest: FileDigest,
    pub(super) build_evidence: FileDigest,
    pub(super) piece_manifest: FileDigest,
    pub(super) pieces: Vec<Piece>,
    pub(super) unpacked_regular_bytes: u64,
    pub(super) directory_count: u64,
    pub(super) symlink_count: u64,
    pub(super) regular_file_count: u64,
}

impl ArtifactMetadata {
    pub(super) fn validate(&self, request: &Request) -> Result<()> {
        ensure!(
            self.schema_version == 1
                && self.repository == REPOSITORY
                && self.source_sha == request.source_sha
                && self.source_tree == request.source_tree
                && self.source_manifest_sha256 == request.source_manifest_sha256
                && self.source_owner_receipt_sha256 == request.source_owner_receipt_sha256,
            "artifact/request identity differs"
        );
        require_hex(&self.controller_sha, 40)?;
        numeric_identity(&self.run_id)?;
        numeric_identity(&self.run_attempt)?;
        validate_pieces(&self.pieces, &self.runtime_archive)?;
        for digest in [
            &self.payload_manifest,
            &self.build_evidence,
            &self.piece_manifest,
        ] {
            ensure!(
                digest.bytes > 0 && digest.bytes <= MAX_MANIFEST_BYTES,
                "empty or oversized metadata member"
            );
            require_hex(&digest.sha256, 64)?;
        }
        ensure!(
            self.unpacked_regular_bytes > 0
                && self.unpacked_regular_bytes <= MAX_PAYLOAD_BYTES
                && self.regular_file_count > 0
                && self.directory_count > 0
                && self
                    .regular_file_count
                    .checked_add(self.directory_count)
                    .and_then(|count| count.checked_add(self.symlink_count))
                    .is_some_and(|count| count <= MAX_PAYLOAD_ENTRIES as u64),
            "unpacked count/size budget differs"
        );
        Ok(())
    }
}

pub(super) fn validate_metadata_members(
    metadata: &ArtifactMetadata,
    request: &Request,
    payload_path: &Path,
    source_path: &Path,
    pieces_path: &Path,
    evidence_path: &Path,
) -> Result<()> {
    let payload: PayloadManifest = authenticated_json(
        payload_path,
        &metadata.payload_manifest.sha256,
        MAX_MANIFEST_BYTES,
    )?;
    payload.validate()?;
    ensure!(
        digest_file(payload_path, MAX_MANIFEST_BYTES)?.0 == metadata.payload_manifest
            && payload.unpacked_regular_bytes == metadata.unpacked_regular_bytes
            && payload.directory_count == metadata.directory_count
            && payload.symlink_count == metadata.symlink_count
            && payload.regular_file_count == metadata.regular_file_count,
        "metadata payload totals/digest differ"
    );
    let source: SourceManifest = authenticated_json(
        source_path,
        &request.source_manifest_sha256,
        MAX_MANIFEST_BYTES,
    )?;
    source.validate()?;
    ensure!(
        source.commit == request.source_sha && source.tree == request.source_tree,
        "downloaded source manifest identity differs"
    );
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Pieces {
        schema_version: u32,
        source_sha: String,
        source_tree: String,
        source_owner_receipt_sha256: String,
        concatenated_archive: FileDigest,
        maximum_github_zip_bytes: u64,
        pieces: Vec<Piece>,
    }
    let pieces: Pieces = authenticated_json(
        pieces_path,
        &metadata.piece_manifest.sha256,
        MAX_METADATA_BYTES,
    )?;
    ensure!(
        digest_file(pieces_path, MAX_METADATA_BYTES)?.0 == metadata.piece_manifest
            && pieces.schema_version == 1
            && pieces.source_sha == request.source_sha
            && pieces.source_tree == request.source_tree
            && pieces.source_owner_receipt_sha256 == request.source_owner_receipt_sha256
            && pieces.concatenated_archive == metadata.runtime_archive
            && pieces.maximum_github_zip_bytes == MAX_ZIP_BYTES
            && pieces.pieces == metadata.pieces,
        "downloaded piece manifest differs"
    );
    ensure!(
        digest_file(evidence_path, MAX_BUILD_EVIDENCE_BYTES)?.0 == metadata.build_evidence,
        "downloaded build/ELF evidence digest differs"
    );
    Ok(())
}

fn numeric_identity(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 20
            && value.bytes().all(|byte| byte.is_ascii_digit())
            && value.parse::<u64>().is_ok_and(|value| value > 0),
        "invalid run identity"
    );
    Ok(())
}

pub(super) fn validate_pieces(pieces: &[Piece], archive: &FileDigest) -> Result<()> {
    ensure!(
        !pieces.is_empty()
            && pieces.len() <= MAX_PIECES
            && archive.bytes > 0
            && archive.bytes <= MAX_ARCHIVE_BYTES,
        "piece/archive limits differ"
    );
    require_hex(&archive.sha256, 64)?;
    let mut total = 0_u64;
    for (order, piece) in pieces.iter().enumerate() {
        ensure!(
            piece.order == order
                && piece.file == format!("{order:06}.part")
                && piece.bytes > 0
                && piece.bytes <= PIECE_BYTES
                && (order + 1 == pieces.len() || piece.bytes == PIECE_BYTES),
            "piece order/name/size differs"
        );
        require_hex(&piece.sha256, 64)?;
        total = total
            .checked_add(piece.bytes)
            .context("piece total overflow")?;
    }
    ensure!(
        total == archive.bytes,
        "piece total differs from archive size"
    );
    Ok(())
}

#[derive(Deserialize)]
struct CargoArtifact {
    package_id: String,
    target: CargoTarget,
    profile: CargoProfile,
    features: Vec<String>,
    executable: Option<PathBuf>,
}

#[derive(Deserialize)]
struct CargoTarget {
    name: String,
    kind: Vec<String>,
}

#[derive(Deserialize)]
struct CargoProfile {
    test: bool,
    debuginfo: serde_json::Value,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FeatureReceipt {
    package_id: String,
    target: String,
    features: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BuildReceipt {
    schema_version: u32,
    state: String,
    request: Request,
    controller_sha: String,
    run_id: String,
    run_attempt: String,
    observed_command: Vec<String>,
    actual_child_exit: i32,
    actual_build_finished_success: bool,
    elapsed_millis: u64,
    source_before: BoundFile,
    source_after: BoundFile,
    tracked_atime_only_transitions: Vec<AtimeTransition>,
    build_log: BoundFile,
    process_closure: BoundFile,
    binary: BoundFile,
    feature_receipts: Vec<FeatureReceipt>,
    default_features: bool,
    removed_features: Vec<String>,
    profile: String,
    debug: u32,
    incremental: bool,
    rustc: String,
    cargo: String,
    clang: String,
    runtime_test_and_reference_credit: u32,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BoundFile {
    path: PathBuf,
    digest: FileDigest,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AtimeTransition {
    path: String,
    before_atime_ns: i64,
    after_atime_ns: i64,
}

fn request_input(path: &Path, expected_sha: &str, source: Option<&SourceArgs>) -> Result<Request> {
    require_hex(expected_sha, 64)?;
    let bytes = read_bounded(path, super::MAX_REQUEST_BYTES)?.0;
    ensure!(sha256(&bytes) == expected_sha, "request digest differs");
    let request = Request::parse(&bytes)?;
    if let Some(source) = source {
        ensure!(
            source.sha == request.source_sha && source.tree == request.source_tree,
            "source CLI identity differs from the pinned request"
        );
    }
    Ok(request)
}

fn source_transitions(
    before: &[PhysicalEntry],
    after: &[PhysicalEntry],
) -> Result<Vec<AtimeTransition>> {
    ensure!(
        before.len() == after.len() && !before.is_empty(),
        "source observation set changed"
    );
    let mut transitions = Vec::new();
    for (before, after) in before.iter().zip(after) {
        ensure!(before.path == after.path, "observed source path changed");
        let mut comparison = after.identity.clone();
        comparison.atime_ns = before.identity.atime_ns;
        ensure!(
            before.identity == comparison,
            "build changed a tracked physical source field other than atime"
        );
        if before.identity.atime_ns != after.identity.atime_ns {
            transitions.push(AtimeTransition {
                path: before.path.clone(),
                before_atime_ns: before.identity.atime_ns,
                after_atime_ns: after.identity.atime_ns,
            });
        }
    }
    Ok(transitions)
}

fn fixed_build_command() -> Vec<String> {
    [
        "cargo",
        "build",
        "--locked",
        "--keep-going",
        "-p",
        "zed",
        "--bin",
        "koda",
        "--message-format=json-render-diagnostics",
    ]
    .iter()
    .map(|value| (*value).to_owned())
    .collect()
}

fn validate_features(receipts: &[FeatureReceipt]) -> Result<()> {
    for target in ["koda", "call", "livekit_client", "libwebrtc", "webrtc_sys"] {
        ensure!(
            receipts.iter().any(|receipt| receipt.target == target),
            "default-feature artifact lacks {target}"
        );
    }
    let application = receipts
        .iter()
        .find(|receipt| receipt.target == "koda")
        .context("application feature receipt absent")?;
    ensure!(
        !application
            .features
            .iter()
            .any(|feature| matches!(feature.as_str(), "test-support" | "visual-tests")),
        "test-only application features enabled"
    );
    Ok(())
}

pub(super) fn build(args: BuildArgs) -> Result<()> {
    let output = args.output_dir.clone();
    let mut owned = false;
    let result = build_inner(args, &mut owned);
    preserve_failure(&output, owned, "build-command-failure.json", &result)?;
    result
}

fn preserve_failure(output: &Path, owned: bool, name: &str, result: &Result<()>) -> Result<()> {
    if let Err(error) = result {
        if owned {
            #[derive(Serialize)]
            struct Failure {
                state: &'static str,
                actual_error: String,
                build_test_runtime_reference_credit: u32,
                retained_partial_evidence: bool,
            }
            write_json(
                &output.join(name),
                &Failure {
                    state: "ACTUAL_COMMAND_FAILURE_RETAINED_ZERO_CREDIT",
                    actual_error: format!("{error:#}"),
                    build_test_runtime_reference_credit: 0,
                    retained_partial_evidence: true,
                },
                MAX_METADATA_BYTES,
            )?;
        }
    }
    Ok(())
}

fn build_inner(args: BuildArgs, owned: &mut bool) -> Result<()> {
    let request = request_input(&args.request, &args.request_sha256, Some(&args.source))?;
    require_hex(&args.controller_sha, 40)?;
    numeric_identity(&args.run_id)?;
    numeric_identity(&args.run_attempt)?;
    for (key, expected) in [
        ("CARGO_INCREMENTAL", "0"),
        ("CARGO_PROFILE_DEV_DEBUG", "0"),
        ("CARGO_PROFILE_DEV_BUILD_OVERRIDE_DEBUG", "0"),
    ] {
        ensure!(
            std::env::var(key).as_deref() == Ok(expected),
            "fixed native build requires {key}={expected}"
        );
    }
    for key in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_BUILD_RUSTFLAGS",
    ] {
        ensure!(
            std::env::var_os(key).is_none(),
            "unreviewed build flag environment {key}"
        );
    }
    fs::create_dir(&args.output_dir).context("build output directory must be new")?;
    *owned = true;
    let (_, before) = observe(&args.source, Authentication::Materialized)?;
    ensure!(
        before.canonical_manifest.sha256 == request.source_manifest_sha256,
        "pre-build source identity differs"
    );
    let before_path = args.output_dir.join("source-before-build-observation.json");
    write_json(&before_path, &before, MAX_MANIFEST_BYTES)?;
    let build_log_path = args.output_dir.join("cargo-build.jsonl");
    let mut build_log = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&build_log_path)?;
    let observed_command = fixed_build_command();
    let start = Instant::now();
    let mut cargo_command = Command::new(&observed_command[0]);
    cargo_command
        .args(&observed_command[1..])
        .current_dir(&args.source.source)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = OwnedChild::spawn(&mut cargo_command, Duration::from_secs(150 * 60))
        .context("spawn fixed Cargo native build")?;
    child.set_receipt(args.output_dir.join("cargo-process-closure.json"));
    let mut bytes = 0_u64;
    let mut features = BTreeMap::new();
    let mut executable: Option<PathBuf> = None;
    let mut finished = None;
    let diagnostics: Result<()> = (|| {
        let mut stdout = BufReader::new(child.stdout()?);
        while let Some(line) = bounded_line(&mut stdout, MAX_BUILD_LINE_BYTES)? {
            bytes = bytes
                .checked_add(line.len() as u64)
                .context("build log size overflow")?;
            ensure!(
                bytes <= MAX_BUILD_LOG_BYTES,
                "build log budget exhausted; partial evidence retained"
            );
            build_log.write_all(&line)?;
            let message: serde_json::Value = serde_json::from_slice(&line)
                .context("Cargo emitted malformed JSON diagnostics")?;
            match message.get("reason").and_then(serde_json::Value::as_str) {
                Some("compiler-artifact") => {
                    let artifact: CargoArtifact = serde_json::from_value(message)?;
                    ensure!(
                        !artifact.profile.test
                            && artifact.features.len() <= 512
                            && artifact.package_id.len() <= 4096
                            && artifact.target.name.len() <= 256,
                        "unexpected or oversized compiler artifact"
                    );
                    let mut observed_features = artifact.features;
                    observed_features.sort();
                    observed_features.dedup();
                    if artifact.target.name == "koda"
                        && artifact.target.kind.iter().any(|kind| kind == "bin")
                    {
                        ensure!(
                            artifact.profile.debuginfo.as_u64() == Some(0) && executable.is_none(),
                            "application debug profile or unique executable differs"
                        );
                        executable = Some(
                            artifact
                                .executable
                                .context("application executable receipt absent")?,
                        );
                    }
                    let key = format!("{}:{}", artifact.package_id, artifact.target.name);
                    ensure!(
                        features.len() < 4096 || features.contains_key(&key),
                        "compiler feature receipt budget exhausted"
                    );
                    features.insert(
                        key,
                        FeatureReceipt {
                            package_id: artifact.package_id,
                            target: artifact.target.name,
                            features: observed_features,
                        },
                    );
                }
                Some("build-finished") => {
                    ensure!(finished.is_none(), "duplicate Cargo build-finished receipt");
                    finished = Some(
                        message
                            .get("success")
                            .and_then(serde_json::Value::as_bool)
                            .context("Cargo build-finished success is not boolean")?,
                    );
                }
                _ => {}
            }
        }
        Ok(())
    })();
    build_log.sync_all()?;
    drop(build_log);
    let process_closure = child.finish(diagnostics.is_err())?;
    if diagnostics.is_err() || process_closure.require_success().is_err() || finished != Some(true)
    {
        #[derive(Serialize)]
        struct Failure<'a> {
            state: &'static str,
            observed_command: &'a [String],
            actual_child_exit: Option<i32>,
            actual_build_finished_success: Option<bool>,
            diagnostic_error: Option<String>,
            process_closure: &'a super::process::CloseReceipt,
            build_log: FileDigest,
            build_test_and_runtime_credit: u32,
        }
        write_json(
            &args.output_dir.join("build-failure.json"),
            &Failure {
                state: "ACTUAL_CARGO_BUILD_FAILURE_RETAINED_ZERO_CREDIT",
                observed_command: &observed_command,
                actual_child_exit: process_closure.leader_exit_code,
                actual_build_finished_success: finished,
                diagnostic_error: diagnostics.as_ref().err().map(|error| format!("{error:#}")),
                process_closure: &process_closure,
                build_log: digest_file(&build_log_path, MAX_BUILD_LOG_BYTES)?.0,
                build_test_and_runtime_credit: 0,
            },
            MAX_METADATA_BYTES,
        )?;
        if let Err(error) = diagnostics {
            return Err(
                error.context("actual Cargo diagnostics failed; cleanup/failure receipts retained")
            );
        }
        process_closure.require_success()?;
        bail!(
            "actual Cargo build-finished receipt failed; actual closure/failure receipts retained"
        );
    }
    let executable = executable.context("actual application executable absent")?;
    ensure!(
        executable.is_absolute(),
        "Cargo executable path must be absolute"
    );
    let feature_receipts: Vec<_> = features.into_values().collect();
    validate_features(&feature_receipts)?;
    let (_, after) = observe(&args.source, Authentication::Materialized)?;
    ensure!(
        after.canonical_manifest == before.canonical_manifest,
        "Cargo changed tracked source bytes or modes"
    );
    let transitions = source_transitions(&before.physical, &after.physical)?;
    let after_path = args.output_dir.join("source-after-build-observation.json");
    write_json(&after_path, &after, MAX_MANIFEST_BYTES)?;
    let tool = |program: &str, arguments: &[&str]| -> Result<String> {
        String::from_utf8(command(
            Some(&args.source.source),
            program,
            arguments,
            64 * 1024,
        )?)
        .context("non-UTF8 toolchain facts")
    };
    let receipt = BuildReceipt {
        schema_version: 1,
        state: "ACTUAL_FIXED_CARGO_CHILD_WAIT_ZERO_NOT_TEST_OR_RUNTIME_ACCEPTANCE".to_owned(),
        request,
        controller_sha: args.controller_sha,
        run_id: args.run_id,
        run_attempt: args.run_attempt,
        observed_command,
        actual_child_exit: 0,
        actual_build_finished_success: true,
        elapsed_millis: u64::try_from(start.elapsed().as_millis())?,
        source_before: BoundFile {
            digest: digest_file(&before_path, MAX_MANIFEST_BYTES)?.0,
            path: before_path,
        },
        source_after: BoundFile {
            digest: digest_file(&after_path, MAX_MANIFEST_BYTES)?.0,
            path: after_path,
        },
        tracked_atime_only_transitions: transitions,
        build_log: BoundFile {
            digest: digest_file(&build_log_path, MAX_BUILD_LOG_BYTES)?.0,
            path: build_log_path,
        },
        process_closure: BoundFile {
            digest: digest_file(
                &args.output_dir.join("cargo-process-closure.json"),
                MAX_METADATA_BYTES,
            )?
            .0,
            path: args.output_dir.join("cargo-process-closure.json"),
        },
        binary: BoundFile {
            digest: digest_file(&executable, MAX_BINARY_BYTES)?.0,
            path: executable,
        },
        feature_receipts,
        default_features: true,
        removed_features: Vec::new(),
        profile: "dev".to_owned(),
        debug: 0,
        incremental: false,
        rustc: tool("rustc", &["-vV"])?,
        cargo: tool("cargo", &["-V"])?,
        clang: tool("clang", &["--version"])?,
        runtime_test_and_reference_credit: 0,
    };
    let receipt_path = args.output_dir.join("build-receipt.json");
    write_json(&receipt_path, &receipt, MAX_BUILD_EVIDENCE_BYTES)?;
    if let Some(output_path) = &args.github_output {
        let mut output = OpenOptions::new().append(true).open(output_path)?;
        writeln!(
            output,
            "native_build_receipt_sha256={}",
            digest_file(&receipt_path, MAX_BUILD_EVIDENCE_BYTES)?
                .0
                .sha256
        )?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum PayloadKind {
    Directory,
    File,
    Symlink,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct PayloadEntry {
    pub(super) path: String,
    pub(super) kind: PayloadKind,
    pub(super) mode: u32,
    pub(super) bytes: u64,
    pub(super) sha256: Option<String>,
    pub(super) link_target: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PayloadManifest {
    schema_version: u32,
    entries: Vec<PayloadEntry>,
    unpacked_regular_bytes: u64,
    directory_count: u64,
    symlink_count: u64,
    regular_file_count: u64,
}

impl PayloadManifest {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1
                && !self.entries.is_empty()
                && self.entries.len() <= MAX_PAYLOAD_ENTRIES,
            "payload set/schema budget differs"
        );
        let mut previous: Option<&str> = None;
        let mut links = BTreeMap::new();
        let mut regular = 0_u64;
        let mut directories = 0_u64;
        let mut symlinks = 0_u64;
        let mut bytes = 0_u64;
        let declared: BTreeMap<_, _> = self
            .entries
            .iter()
            .map(|entry| (entry.path.as_str(), &entry.kind))
            .collect();
        for entry in &self.entries {
            payload_relative(&entry.path)?;
            ensure!(
                entry.path == "runtime" || entry.path.starts_with("runtime/"),
                "payload leaves runtime root"
            );
            ensure!(
                previous.is_none_or(|old| old.as_bytes() < entry.path.as_bytes()),
                "payload paths duplicate or unsorted"
            );
            previous = Some(&entry.path);
            let mut parent = Path::new(&entry.path).parent();
            while let Some(path) = parent.filter(|path| !path.as_os_str().is_empty()) {
                ensure!(
                    declared.get(path.to_str().context("non-UTF8 payload parent")?)
                        == Some(&&PayloadKind::Directory),
                    "payload parent is missing or is not a directory"
                );
                parent = path.parent();
            }
            match entry.kind {
                PayloadKind::Directory => {
                    ensure!(
                        entry.mode == 0o755
                            && entry.bytes == 0
                            && entry.sha256.is_none()
                            && entry.link_target.is_none(),
                        "directory metadata differs"
                    );
                    directories += 1;
                }
                PayloadKind::File => {
                    ensure!(
                        matches!(entry.mode, 0o644 | 0o755) && entry.link_target.is_none(),
                        "unsafe file mode or link target"
                    );
                    require_hex(entry.sha256.as_deref().context("file digest absent")?, 64)?;
                    bytes = bytes
                        .checked_add(entry.bytes)
                        .context("payload byte overflow")?;
                    regular += 1;
                }
                PayloadKind::Symlink => {
                    let target = entry
                        .link_target
                        .as_deref()
                        .context("payload link target absent")?;
                    ensure!(
                        entry.mode == 0o777
                            && entry.bytes == target.len() as u64
                            && entry.sha256.as_deref() == Some(sha256(target.as_bytes()).as_str()),
                        "payload link identity differs"
                    );
                    links.insert(entry.path.clone(), normalized_link(&entry.path, target)?);
                    symlinks += 1;
                }
            }
        }
        ensure!(
            self.entries[0].path == "runtime"
                && self.entries[0].kind == PayloadKind::Directory
                && bytes > 0
                && bytes <= MAX_PAYLOAD_BYTES
                && bytes == self.unpacked_regular_bytes
                && regular == self.regular_file_count
                && directories == self.directory_count
                && symlinks == self.symlink_count,
            "payload counts/logical bytes differ"
        );
        validate_link_graph(self.entries.iter().map(|entry| entry.path.as_str()), &links)
    }
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        bail!("native packaging requires Unix")
    }
}

pub(super) fn copy_verified(
    input: &Path,
    output: &Path,
    expected: &FileDigest,
    mode: u32,
) -> Result<Physical13> {
    let before = copy_verified_descriptor(open_noatime(input, false)?, output, expected, mode)?;
    ensure!(
        before == physical(&fs::symlink_metadata(input)?)?,
        "copy input path changed"
    );
    Ok(before)
}

fn copy_verified_descriptor(
    mut source: File,
    output: &Path,
    expected: &FileDigest,
    mode: u32,
) -> Result<Physical13> {
    let before = physical(&source.metadata()?)?;
    ensure!(
        source.metadata()?.is_file() && before.bytes == expected.bytes,
        "copy source size/type differs"
    );
    let mut destination = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)?;
    let mut digest = Sha256::new();
    let mut count = 0_u64;
    let mut buffer = vec![0; 256 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        count = count
            .checked_add(read as u64)
            .context("copy size overflow")?;
        ensure!(count <= expected.bytes, "copy source grew");
        digest.update(&buffer[..read]);
        destination.write_all(&buffer[..read])?;
    }
    ensure!(
        count == expected.bytes
            && format!("{:x}", digest.finalize()) == expected.sha256
            && before == physical(&source.metadata()?)?,
        "copy input inverse differs"
    );
    destination.sync_all()?;
    drop(destination);
    set_mode(output, mode)?;
    Ok(before)
}

fn ensure_parents(path: &Path) -> Result<()> {
    fs::create_dir_all(path.parent().context("output has no parent")?)?;
    Ok(())
}

/// A creation boundary we can enforce with ordinary Linux permissions:
/// the retained parent is owned by this effective user and is not writable by
/// group/other users. Privileged or arbitrary same-user interference is outside
/// this single-job constructor's trust model. Linux mkdirat does not return an
/// inode FD, so we do not claim an atomic mkdir-to-FD guarantee against those
/// actors. Descendant writes are still confined to retained descriptors when
/// public pathnames are replaced; a changed path prevents final admission.
#[cfg(target_os = "linux")]
struct TrustedCreationParent {
    file: File,
    path: PathBuf,
    identity: Physical13,
}

#[cfg(target_os = "linux")]
fn same_created_directory(expected: &Physical13, actual: &Physical13) -> bool {
    // Directories are mutable during creation. These are ownership fields,
    // not an exception to any frozen full13 source observation.
    expected.dev == actual.dev
        && expected.ino == actual.ino
        && expected.mode == actual.mode
        && expected.uid == actual.uid
        && expected.gid == actual.gid
}

#[cfg(target_os = "linux")]
fn require_creation_parent(file: &File) -> Result<Physical13> {
    let metadata = file.metadata()?;
    let identity = physical(&metadata)?;
    // SAFETY: geteuid has no arguments and does not change process credentials.
    let effective_uid = unsafe { libc::geteuid() };
    ensure!(
        metadata.is_dir() && identity.uid == effective_uid && identity.mode & 0o022 == 0,
        "checkout creation parent must be effective-user-owned and not group/other writable"
    );
    Ok(identity)
}

#[cfg(target_os = "linux")]
fn open_directory_at(parent: &File, name: &std::ffi::CStr) -> Result<File> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    // SAFETY: live retained parent FD and terminated single-component name;
    // O_NOFOLLOW refuses symlink replacement and the returned FD is owned.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY
                | libc::O_NOATIME
                | libc::O_NOFOLLOW
                | libc::O_DIRECTORY
                | libc::O_CLOEXEC,
        )
    };
    ensure!(
        fd >= 0,
        "confined directory open failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: successful openat transferred a new descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(target_os = "linux")]
fn open_ancestor_directory_at(parent: std::os::fd::RawFd, name: &std::ffi::CStr) -> Result<File> {
    use std::os::fd::FromRawFd as _;
    // System ancestors can belong to another UID. O_PATH grants a directory
    // capability without reading it, changing atime, or requiring CAP_FOWNER.
    // O_DIRECTORY with O_NOFOLLOW refuses symlink components rather than
    // returning their inode. This helper is separate from readable owned
    // descendants and from the unchanged strict borrowed-source reader.
    // SAFETY: AT_FDCWD or a live retained directory FD and a terminated name.
    let fd = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    ensure!(
        fd >= 0,
        "confined ancestor capability open failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: successful openat transferred a new descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(target_os = "linux")]
fn open_absolute_directory(path: &Path) -> Result<File> {
    use std::{os::fd::AsRawFd as _, os::unix::ffi::OsStrExt as _, path::Component};
    ensure!(path.is_absolute(), "creation parent must be absolute");
    let mut file = open_ancestor_directory_at(libc::AT_FDCWD, &std::ffi::CString::new("/")?)?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                file = open_ancestor_directory_at(
                    file.as_raw_fd(),
                    &std::ffi::CString::new(name.as_bytes())?,
                )?;
            }
            _ => bail!("creation parent has an unconfined component"),
        }
    }
    Ok(file)
}

#[cfg(target_os = "linux")]
impl TrustedCreationParent {
    fn open(path: &Path) -> Result<Self> {
        let file = open_absolute_directory(path)?;
        let identity = require_creation_parent(&file)?;
        Ok(Self {
            file,
            path: path.to_owned(),
            identity,
        })
    }

    fn require_current(&self) -> Result<()> {
        let descriptor = require_creation_parent(&self.file)?;
        let pathname = require_creation_parent(&open_absolute_directory(&self.path)?)?;
        ensure!(
            same_created_directory(&self.identity, &descriptor)
                && same_created_directory(&self.identity, &pathname),
            "trusted checkout creation parent was replaced"
        );
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn create_directory_at(parent: &File, name: &std::ffi::CStr) -> Result<File> {
    use std::os::fd::AsRawFd as _;
    require_creation_parent(parent)?;
    // SAFETY: retained protected directory and terminated single-component name.
    // Existing names are refused; no existing directory becomes a capability.
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
    ensure!(
        result == 0,
        "owned checkout directory must be new: {}",
        std::io::Error::last_os_error()
    );
    let file = open_directory_at(parent, name)?;
    let created = require_creation_parent(&file)?;
    ensure!(
        created.mode & 0o777 == 0o700,
        "new checkout directory is not private"
    );
    Ok(file)
}

#[cfg(target_os = "linux")]
fn create_file_at(parent: &File, name: &std::ffi::CStr) -> Result<File> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    require_creation_parent(parent)?;
    // SAFETY: retained protected directory and terminated component; O_EXCL
    // refuses existing files/links and O_NOFOLLOW prevents target following.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    ensure!(
        fd >= 0,
        "owned file creation failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: successful openat transferred a newly owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(target_os = "linux")]
fn open_link_at(parent: &File, name: &std::ffi::CStr) -> Result<File> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    // SAFETY: retained directory and terminated component; O_PATH/O_NOFOLLOW
    // returns the link inode, without following a target.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    ensure!(
        fd >= 0,
        "owned link open failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: successful openat transferred a newly owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(target_os = "linux")]
fn descriptor_path(file: &File) -> PathBuf {
    use std::os::fd::AsRawFd as _;
    // The parent process retains this descriptor throughout every child wait.
    // A trailing dot permits the unchanged materialized observer's final
    // O_DIRECTORY/O_NOFOLLOW open while resolving beneath this retained root.
    PathBuf::from(format!(
        "/proc/{}/fd/{}/.",
        std::process::id(),
        file.as_raw_fd()
    ))
}

#[cfg(target_os = "linux")]
struct OwnedCheckout {
    source: SourceArgs,
    root: File,
    created_root: Physical13,
    parent: TrustedCreationParent,
    name: std::ffi::CString,
    created_directories: BTreeMap<String, Physical13>,
}

#[cfg(target_os = "linux")]
impl OwnedCheckout {
    fn create(source: &SourceArgs, destination: &Path) -> Result<Self> {
        let parent =
            TrustedCreationParent::open(destination.parent().context("checkout parent absent")?)?;
        Self::create_beneath(source, destination, parent)
    }

    fn create_beneath(
        source: &SourceArgs,
        destination: &Path,
        parent: TrustedCreationParent,
    ) -> Result<Self> {
        use std::os::unix::ffi::OsStrExt as _;
        ensure!(
            destination.parent() == Some(parent.path.as_path()),
            "checkout parent capability differs"
        );
        parent.require_current()?;
        let name = std::ffi::CString::new(
            destination
                .file_name()
                .context("checkout name absent")?
                .as_bytes(),
        )?;
        let root = create_directory_at(&parent.file, &name)?;
        let created_root = physical(&root.metadata()?)?;
        let checkout = Self {
            source: SourceArgs {
                source: destination.to_owned(),
                sha: source.sha.clone(),
                tree: source.tree.clone(),
            },
            root,
            created_root,
            parent,
            name,
            created_directories: BTreeMap::new(),
        };
        checkout.require_owned_path()?;
        Ok(checkout)
    }

    fn require_owned_path(&self) -> Result<()> {
        self.parent.require_current()?;
        let descriptor = physical(&self.root.metadata()?)?;
        let pathname = physical(&open_directory_at(&self.parent.file, &self.name)?.metadata()?)?;
        ensure!(
            same_created_directory(&self.created_root, &descriptor)
                && same_created_directory(&self.created_root, &pathname),
            "newly created checkout root was replaced"
        );
        Ok(())
    }

    fn directory(&mut self, path: &str) -> Result<File> {
        if !path.is_empty() {
            payload_relative(path)?;
        }
        let mut directory = self.root.try_clone()?;
        ensure!(
            same_created_directory(&self.created_root, &physical(&directory.metadata()?)?),
            "retained checkout root changed ownership"
        );
        let mut prefix = String::new();
        for part in path.split('/').filter(|part| !part.is_empty()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            let name = std::ffi::CString::new(part)?;
            let child = match self.created_directories.get(&prefix).cloned() {
                Some(expected) => {
                    let child = open_directory_at(&directory, &name)?;
                    ensure!(
                        same_created_directory(&expected, &physical(&child.metadata()?)?),
                        "owned descendant directory was replaced: {prefix}"
                    );
                    child
                }
                None => {
                    ensure!(
                        self.created_directories.len() < MAX_PAYLOAD_ENTRIES,
                        "owned directory capability budget exhausted before creation"
                    );
                    let child = create_directory_at(&directory, &name)?;
                    self.created_directories
                        .insert(prefix.clone(), physical(&child.metadata()?)?);
                    child
                }
            };
            directory = child;
        }
        Ok(directory)
    }

    fn parent_for(&mut self, path: &str) -> Result<(File, std::ffi::CString)> {
        payload_relative(path)?;
        let (parent, leaf) = path.rsplit_once('/').unwrap_or(("", path));
        Ok((self.directory(parent)?, std::ffi::CString::new(leaf)?))
    }

    fn new_file(&mut self, path: &str) -> Result<File> {
        let (parent, name) = self.parent_for(path)?;
        create_file_at(&parent, &name)
    }

    fn write_file(&mut self, path: &str, bytes: &[u8], mode: u32) -> Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let mut file = self.new_file(path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        Ok(())
    }

    fn create_link(&mut self, path: &str, target: &str) -> Result<()> {
        use std::os::fd::AsRawFd as _;
        let (parent, name) = self.parent_for(path)?;
        require_creation_parent(&parent)?;
        let target_text = std::ffi::CString::new(target)?;
        // SAFETY: terminated target/name and a retained protected parent FD;
        // symlinkat creates a new name and refuses an existing inode.
        let result =
            unsafe { libc::symlinkat(target_text.as_ptr(), parent.as_raw_fd(), name.as_ptr()) };
        ensure!(
            result == 0,
            "owned symlink creation failed: {}",
            std::io::Error::last_os_error()
        );
        let file = open_link_at(&parent, &name)?;
        ensure!(
            file.metadata()?.file_type().is_symlink(),
            "newly created link changed type"
        );
        // Only this freshly created link receives its creation readback. A
        // borrowed inode is never warmed, and the frozen inverse remains all13.
        let initialized_bytes = link_bytes(&file)?;
        ensure!(
            initialized_bytes == target.as_bytes(),
            "created link target differs"
        );
        let initialized = physical(&file.metadata()?)?;
        let again = open_link_at(&parent, &name)?;
        let path_before = physical(&again.metadata()?)?;
        ensure!(
            initialized == path_before,
            "created link was replaced before strict readback"
        );
        let strict_bytes = link_bytes(&again)?;
        let descriptor_after = physical(&file.metadata()?)?;
        let path_after = physical(&again.metadata()?)?;
        let named_after = physical(&open_link_at(&parent, &name)?.metadata()?)?;
        ensure!(
            strict_bytes == initialized_bytes
                && initialized == descriptor_after
                && initialized == path_after
                && initialized == named_after,
            "created link initialization inverse differs: descriptor fields {:?}, path fields {:?}, named fields {:?}",
            initialized.changed_fields(&descriptor_after),
            initialized.changed_fields(&path_after),
            initialized.changed_fields(&named_after)
        );
        Ok(())
    }

    fn descriptor_source(&self) -> SourceArgs {
        SourceArgs {
            source: descriptor_path(&self.root),
            sha: self.source.sha.clone(),
            tree: self.source.tree.clone(),
        }
    }
}

#[cfg(target_os = "linux")]
fn owned_git_output(root: &File, git: &File, args: &[&str], maximum: u64) -> Result<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt as _;
    let root_path = descriptor_path(root);
    let git_path = descriptor_path(git);
    let mut command = Command::new("git");
    command
        .arg("--git-dir")
        .arg(&git_path)
        .arg("--work-tree")
        .arg(&root_path)
        .args(args)
        .current_dir(&root_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    // Index/object/config environment overrides must never redirect a write to
    // a borrowed repository. This changes only this owned child environment.
    for (name, _) in std::env::vars_os() {
        if name.as_bytes().starts_with(b"GIT_") {
            command.env_remove(name);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_REPLACE_OBJECTS", "1");
    let mut child = OwnedChild::spawn(&mut command, Duration::from_secs(300))?;
    let result: Result<Vec<u8>> = (|| {
        let mut output = Vec::new();
        let mut stdout = child.stdout()?;
        (&mut stdout).take(maximum + 1).read_to_end(&mut output)?;
        ensure!(
            output.len() as u64 <= maximum,
            "owned Git output exceeds its budget"
        );
        Ok(output)
    })();
    let receipt = child.finish(result.is_err())?;
    receipt.require_closed()?;
    let output = result?;
    receipt.require_success()?;
    Ok(output)
}

#[cfg(target_os = "linux")]
fn reconstruct_owned_git(
    source: &SourceArgs,
    checkout: &mut OwnedCheckout,
    plan: &producer::GitPlan,
) -> Result<()> {
    checkout.directory(".git/objects")?;
    checkout.directory(".git/refs")?;
    checkout.write_file(".git/HEAD", format!("{}\n", source.sha).as_bytes(), 0o644)?;
    checkout.write_file(
        ".git/shallow",
        format!("{}\n", source.sha).as_bytes(),
        0o644,
    )?;
    checkout.write_file(".git/config", b"[core]\nrepositoryformatversion = 0\nfilemode = true\nbare = false\nlogallrefupdates = false\n", 0o644)?;
    let mut batch = GitBatch::new(&source.source)?;
    let mut total = 0_u64;
    let mut compressed = 0_u64;
    for (oid, kind) in &plan.objects {
        let bytes = batch.object(oid, kind)?;
        total = total
            .checked_add(bytes.len() as u64)
            .context("Git reconstruction size overflow")?;
        ensure!(
            total <= super::MAX_SOURCE_BYTES + super::MAX_TREE_BYTES,
            "Git reconstruction byte budget exhausted"
        );
        let header = format!("{kind} {}\0", bytes.len());
        let cap = producer::compression_bound(bytes.len() as u64 + header.len() as u64)?;
        let output = checkout.new_file(&format!(".git/objects/{}/{}", &oid[..2], &oid[2..]))?;
        let limited = producer::LimitedWriter::new(output, cap);
        let mut encoder = ZlibEncoder::new(limited, Compression::fast());
        encoder.write_all(header.as_bytes())?;
        encoder.write_all(&bytes)?;
        let output = encoder.finish()?.into_inner();
        output.sync_all()?;
        compressed = compressed
            .checked_add(output.metadata()?.len())
            .context("Git compressed total overflow")?;
        ensure!(
            compressed <= plan.compressed_upper_bytes,
            "Git compressed plan exhausted before further writes"
        );
    }
    batch.finish()?;
    let git = checkout.directory(".git")?;
    owned_git_output(&checkout.root, &git, &["read-tree", &source.sha], 1024)?;
    let head = owned_git_output(&checkout.root, &git, &["rev-parse", "HEAD^{tree}"], 1024)?;
    ensure!(
        std::str::from_utf8(&head)?.trim() == source.tree,
        "reconstructed Git tree differs"
    );
    Ok(())
}

#[derive(Debug, Serialize)]
pub(super) struct OwnedCheckoutReceipt {
    schema_version: u32,
    creation_trust_policy: &'static str,
    descendant_mutations_use_retained_descriptors: bool,
    #[cfg(target_os = "linux")]
    #[serde(skip)]
    retained_parent: File,
    #[cfg(target_os = "linux")]
    #[serde(skip)]
    retained_root: File,
    #[cfg(target_os = "linux")]
    #[serde(skip)]
    creation_parent: Physical13,
    state: &'static str,
    source_commit: String,
    source_tree: String,
    constructed_directory: PathBuf,
    constructed_root_after_initialization: Physical13,
    canonical_manifest: FileDigest,
    tracked_entries: usize,
    initialized_new_symlinks: usize,
    strict_materialized_entries: usize,
    complete_source_full13_inverses_equal: bool,
    required_bytes_before_creation: u64,
    required_inodes_before_creation: u64,
    original_filesystem_floor_bytes: u64,
    borrowed_materialized_source_was_not_initialized: bool,
    first_constructor_tool_is_unaccepted: bool,
    runtime_test_or_reference_credit: u32,
    closure: &'static str,
}

#[cfg(target_os = "linux")]
pub(super) fn prepare_owned_checkout(
    source: &SourceArgs,
    destination: &Path,
) -> Result<OwnedCheckoutReceipt> {
    prepare_owned_checkout_with_receipt_reserve(source, destination, None)
}

#[cfg(target_os = "linux")]
fn prepare_owned_checkout_with_receipt_reserve(
    source: &SourceArgs,
    destination: &Path,
    receipts: Option<&BorrowedPhysicalGuard>,
) -> Result<OwnedCheckoutReceipt> {
    use std::path::Component;
    let absolute_confined = |path: &Path| {
        path.is_absolute()
            && path
                .components()
                .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
    };
    ensure!(
        absolute_confined(&source.source)
            && absolute_confined(destination)
            && !destination.starts_with(&source.source)
            && !source.source.starts_with(destination),
        "new checkout must be a separate confined absolute directory"
    );
    // Fail before object scans for an already borrowed path. The later atomic
    // create remains the authoritative gate if another creator races this check.
    match fs::symlink_metadata(destination) {
        Ok(_) => bail!("owned checkout directory must be new"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let parent = destination.parent().context("checkout parent absent")?;
    let (manifest, object_observation) = observe(source, Authentication::GitObjects)?;
    let git = producer::git_plan(source)?;
    let (block, available, available_inodes) = match receipts {
        Some(guard) => {
            ensure!(
                parent == guard.output_parent.path && guard.pending_receipts.get() == 1,
                "checkout receipt reservation parent or pending state differs"
            );
            guard.receipt_filesystem_space()?
        }
        None => super::filesystem_space(parent)?,
    };
    let (receipt_bytes, receipt_inodes) = match receipts {
        Some(guard) => guard.pending_receipt_reserve(block)?,
        None => (0, 0),
    };
    ensure!(
        block > 0 && block <= 1024 * 1024,
        "invalid checkout block size"
    );
    let mut directories = BTreeSet::from([
        String::new(),
        ".git".to_owned(),
        ".git/objects".to_owned(),
        ".git/refs".to_owned(),
    ]);
    let mut bodies = 0_u64;
    let mut index_upper = 64 * 1024_u64;
    for entry in &manifest.entries {
        bodies = bodies
            .checked_add(entry.bytes)
            .context("checkout source size overflow")?;
        index_upper = index_upper
            .checked_add(entry.path.len() as u64 + 256)
            .context("checkout index bound overflow")?;
        let mut ancestor = Path::new(&entry.path).parent();
        while let Some(path) = ancestor.filter(|path| !path.as_os_str().is_empty()) {
            directories.insert(
                path.to_str()
                    .context("checkout parent is not UTF-8")?
                    .to_owned(),
            );
            ancestor = path.parent();
        }
    }
    for oid in git.objects.keys() {
        directories.insert(format!(".git/objects/{}", &oid[..2]));
    }
    let required_inodes = (manifest.entries.len() as u64)
        .checked_add(git.objects.len() as u64)
        .and_then(|n| n.checked_add(directories.len() as u64))
        .and_then(|n| n.checked_add(37))
        .and_then(|n| n.checked_add(receipt_inodes))
        .context("checkout inode bound overflow")?;
    ensure!(
        required_inodes <= MAX_PAYLOAD_ENTRIES as u64,
        "checkout inode bound exceeds the existing payload entry budget"
    );
    let required_bytes = required_inodes
        .checked_mul(block)
        .and_then(|n| n.checked_add(bodies))
        .and_then(|n| n.checked_add(git.compressed_upper_bytes))
        .and_then(|n| n.checked_add(index_upper))
        .and_then(|n| n.checked_add(MAX_METADATA_BYTES))
        .and_then(|n| n.checked_add(64 * 1024 * 1024))
        .and_then(|n| n.checked_add(receipt_bytes))
        .and_then(|n| n.checked_add(super::ORIGINAL_FLOOR))
        .context("checkout disk preflight overflow")?;
    ensure!(
        available >= required_bytes && available_inodes >= required_inodes,
        "owned checkout disk/inode reserve does not fit before creation"
    );
    let mut checkout = OwnedCheckout::create(source, destination)?;
    let mut batch = GitBatch::new(&source.source)?;
    // Every tracked inode is created here. Immutable Git objects supply bytes;
    // no borrowed working-tree symlink is read, warmed, replaced or relabeled.
    for entry in &manifest.entries {
        if entry.mode == "120000" {
            continue;
        }
        let bytes = batch.object(&entry.git_blob, "blob")?;
        ensure!(
            bytes.len() as u64 == entry.bytes && sha256(&bytes) == entry.sha256,
            "constructed regular source differs from its canonical Git object"
        );
        checkout.write_file(
            &entry.path,
            &bytes,
            if entry.mode == "100755" { 0o755 } else { 0o644 },
        )?;
    }
    batch.finish()?;
    let mut initialized_new_symlinks = 0;
    for entry in &manifest.entries {
        if entry.mode != "120000" {
            continue;
        }
        checkout.create_link(
            &entry.path,
            entry
                .link_target
                .as_deref()
                .context("owned link target absent")?,
        )?;
        initialized_new_symlinks += 1;
    }
    checkout.require_owned_path()?;
    reconstruct_owned_git(source, &mut checkout, &git)?;
    checkout.require_owned_path()?;
    let (actual_manifest, observation) =
        observe(&checkout.descriptor_source(), Authentication::Materialized)?;
    checkout.require_owned_path()?;
    ensure!(
        actual_manifest == manifest
            && observation.canonical_manifest == object_observation.canonical_manifest
            && observation.full_physical_noatime_inverses_equal
            && observation.physically_materialized_entries == manifest.entries.len(),
        "owned materialized checkout differs from exact immutable Git source"
    );
    let constructed_root_after_initialization = physical(&checkout.root.metadata()?)?;
    ensure!(
        constructed_root_after_initialization
            == physical(&open_directory_at(&checkout.parent.file, &checkout.name)?.metadata()?)?,
        "owned checkout complete final root inverse differs"
    );
    Ok(OwnedCheckoutReceipt {
        schema_version: 1,
        creation_trust_policy: "effective-user-owned parent without group/other write; same-user and privileged interference excluded",
        descendant_mutations_use_retained_descriptors: true,
        retained_parent: checkout.parent.file.try_clone()?,
        retained_root: checkout.root.try_clone()?,
        creation_parent: checkout.parent.identity.clone(),
        state: "RUST_CONSTRUCTED_NEW_EXACT_CHECKOUT_NOT_BOOTSTRAP_OR_NATIVE_ACCEPTANCE",
        source_commit: source.sha.clone(),
        source_tree: source.tree.clone(),
        constructed_directory: destination.to_owned(),
        constructed_root_after_initialization,
        canonical_manifest: observation.canonical_manifest,
        tracked_entries: manifest.entries.len(),
        initialized_new_symlinks,
        strict_materialized_entries: observation.physically_materialized_entries,
        complete_source_full13_inverses_equal: observation.full_physical_noatime_inverses_equal,
        required_bytes_before_creation: required_bytes,
        required_inodes_before_creation: required_inodes,
        original_filesystem_floor_bytes: super::ORIGINAL_FLOOR,
        borrowed_materialized_source_was_not_initialized: true,
        first_constructor_tool_is_unaccepted: true,
        runtime_test_or_reference_credit: 0,
        closure: "CLOSED0",
    })
}

#[cfg(not(target_os = "linux"))]
pub(super) fn prepare_owned_checkout(_: &SourceArgs, _: &Path) -> Result<OwnedCheckoutReceipt> {
    bail!("owned descriptor-relative checkout creation requires Linux")
}

impl OwnedCheckoutReceipt {
    pub(super) fn write_new_receipt(&self, path: &Path) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::OsStrExt as _;
            ensure!(
                path.parent() == self.constructed_directory.parent(),
                "receipt parent capability differs"
            );
            let verify = || -> Result<()> {
                let named_parent =
                    open_absolute_directory(path.parent().context("receipt parent absent")?)?;
                ensure!(
                    same_created_directory(
                        &self.creation_parent,
                        &require_creation_parent(&self.retained_parent)?
                    ) && same_created_directory(
                        &self.creation_parent,
                        &require_creation_parent(&named_parent)?
                    ),
                    "receipt creation parent was replaced"
                );
                let root_name = std::ffi::CString::new(
                    self.constructed_directory
                        .file_name()
                        .context("checkout root name absent")?
                        .as_bytes(),
                )?;
                ensure!(
                    self.constructed_root_after_initialization
                        == physical(&self.retained_root.metadata()?)?
                        && self.constructed_root_after_initialization
                            == physical(
                                &open_directory_at(&self.retained_parent, &root_name)?
                                    .metadata()?
                            )?,
                    "frozen owned checkout root changed before receipt admission"
                );
                Ok(())
            };
            let bytes = super::canonical(self, MAX_METADATA_BYTES)?;
            verify()?;
            let name = std::ffi::CString::new(
                path.file_name().context("receipt name absent")?.as_bytes(),
            )?;
            let mut output = create_file_at(&self.retained_parent, &name)?;
            output.write_all(&bytes)?;
            output.sync_all()?;
            verify()?;
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = path;
            bail!("owned descriptor-relative receipt creation requires Linux")
        }
    }
}

fn copy_source(source: &SourceArgs, manifest: &SourceManifest, destination: &Path) -> Result<()> {
    fs::create_dir(destination)?;
    let root = open_noatime(&source.source, true)?;
    // All regular files are copied before any links; no archive-controlled parent
    // can redirect a later write outside the private destination.
    for entry in &manifest.entries {
        if entry.mode == "120000" {
            continue;
        }
        let path = destination.join(&entry.path);
        ensure_parents(&path)?;
        let before = copy_verified_descriptor(
            open_beneath(&root, &entry.path, false)?,
            &path,
            &FileDigest {
                bytes: entry.bytes,
                sha256: entry.sha256.clone(),
            },
            if entry.mode == "100755" { 0o755 } else { 0o644 },
        )?;
        ensure!(
            before == physical(&open_beneath(&root, &entry.path, false)?.metadata()?)?,
            "copied source path changed"
        );
    }
    for entry in &manifest.entries {
        if entry.mode != "120000" {
            continue;
        }
        let file = open_beneath(&root, &entry.path, true)?;
        let before = physical(&file.metadata()?)?;
        let bytes = link_bytes(&file)?;
        ensure!(
            sha256(&bytes) == entry.sha256
                && before == physical(&file.metadata()?)?
                && before == physical(&open_beneath(&root, &entry.path, true)?.metadata()?)?,
            "source link changed"
        );
        let path = destination.join(&entry.path);
        ensure_parents(&path)?;
        create_symlink(
            entry.link_target.as_deref().context("link target absent")?,
            &path,
        )?;
    }
    Ok(())
}

fn create_symlink(target: &str, path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, path)?;
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(path)?;
            ensure!(
                file.metadata()?.file_type().is_symlink(),
                "newly created link changed type"
            );
            // This readback initializes only an output we just created. Linux
            // may update link atime even through O_PATH; capture its baseline
            // after initialization, before any frozen observation admits it.
            let readback = link_bytes(&file)?;
            ensure!(readback == target.as_bytes(), "created link target differs");
            let initialized = physical(&file.metadata()?)?;
            let strict_readback = host_link(path)?;
            let descriptor_after = physical(&file.metadata()?)?;
            let path_after = physical(&fs::symlink_metadata(path)?)?;
            ensure!(
                strict_readback == readback
                    && initialized == descriptor_after
                    && initialized == path_after,
                "created link initialization inverse differs for {}: descriptor fields {:?}, path fields {:?}",
                path.display(),
                initialized.changed_fields(&descriptor_after),
                initialized.changed_fields(&path_after)
            );
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (target, path);
        bail!("native symlink creation requires Unix")
    }
}

#[cfg(all(test, target_os = "linux"))]
fn reconstruct_git(source: &SourceArgs, destination: &Path) -> Result<()> {
    let plan = producer::git_plan(source)?;
    reconstruct_git_with_plan(source, destination, &plan)
}

fn reconstruct_git_with_plan(
    source: &SourceArgs,
    destination: &Path,
    plan: &producer::GitPlan,
) -> Result<()> {
    let git = destination.join(".git");
    fs::create_dir(&git)?;
    for directory in ["objects", "refs"] {
        fs::create_dir(git.join(directory))?;
    }
    create_file(&git.join("HEAD"), format!("{}\n", source.sha).as_bytes())?;
    create_file(&git.join("shallow"), format!("{}\n", source.sha).as_bytes())?;
    create_file(&git.join("config"), b"[core]\nrepositoryformatversion = 0\nfilemode = true\nbare = false\nlogallrefupdates = false\n")?;
    let mut batch = GitBatch::new(&source.source)?;
    let mut total = 0_u64;
    let mut compressed = 0_u64;
    for (oid, kind) in &plan.objects {
        let bytes = batch.object(oid, kind)?;
        total = total
            .checked_add(bytes.len() as u64)
            .context("Git reconstruction size overflow")?;
        ensure!(
            total <= super::MAX_SOURCE_BYTES + super::MAX_TREE_BYTES,
            "Git reconstruction byte budget exhausted"
        );
        let header = format!("{kind} {}\0", bytes.len());
        let cap = producer::compression_bound(bytes.len() as u64 + header.len() as u64)?;
        let directory = git.join("objects").join(&oid[..2]);
        fs::create_dir_all(&directory)?;
        let path = directory.join(&oid[2..]);
        let output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        let limited = producer::LimitedWriter::new(output, cap);
        let mut encoder = ZlibEncoder::new(limited, Compression::fast());
        encoder.write_all(header.as_bytes())?;
        encoder.write_all(&bytes)?;
        encoder.finish()?.into_inner().sync_all()?;
        compressed = compressed
            .checked_add(fs::symlink_metadata(&path)?.len())
            .context("Git compressed total overflow")?;
        ensure!(
            compressed <= plan.compressed_upper_bytes,
            "Git compressed plan exhausted before further writes"
        );
    }
    batch.finish()?;
    command(Some(destination), "git", &["read-tree", &source.sha], 1024)?;
    let head = command(
        Some(destination),
        "git",
        &["rev-parse", "HEAD^{tree}"],
        1024,
    )?;
    ensure!(
        std::str::from_utf8(&head)?.trim() == source.tree,
        "reconstructed Git tree differs"
    );
    Ok(())
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
fn resolve_noatime(path: &Path) -> Result<PathBuf> {
    resolve_noatime_with_budget(path, &mut elf::MetadataBudget::new())
}
fn resolve_noatime_with_budget(path: &Path, budget: &mut elf::MetadataBudget) -> Result<PathBuf> {
    ensure!(
        path.is_absolute(),
        "host dependency must have an absolute loader path"
    );
    ensure!(
        path.as_os_str().len() <= super::MAX_PATH_BYTES,
        "host dependency input path budget exhausted"
    );
    budget.charge(
        path.as_os_str().len() as u64 * 128 + 1024,
        "host dependency path component queue",
    )?;
    let mut pending: std::collections::VecDeque<_> = path
        .components()
        .skip(1)
        .map(|part| part.as_os_str().to_owned())
        .collect();
    let mut result = PathBuf::from("/");
    let mut followed = 0;
    while let Some(component) = pending.pop_front() {
        if component == "." {
            continue;
        }
        if component == ".." {
            ensure!(result.pop(), "host path escapes root");
            continue;
        }
        budget.charge(
            (result.as_os_str().len() as u64 + component.len() as u64 + 1) * 2 + 256,
            "host dependency resolution path growth",
        )?;
        result.push(component);
        let metadata = fs::symlink_metadata(&result)?;
        if metadata.file_type().is_symlink() {
            followed += 1;
            ensure!(followed <= 64, "host symlink chain too deep");
            budget.charge(
                super::MAX_PATH_BYTES as u64 * 2 + 1024,
                "host dependency symlink target buffer",
            )?;
            let target = host_link(&result)?;
            budget.charge(
                target.len() as u64 * 128 + 1024,
                "host dependency symlink component queue",
            )?;
            result.pop();
            let target = PathBuf::from(String::from_utf8(target)?);
            if target.is_absolute() {
                result = PathBuf::from("/");
            }
            for part in target.components().rev() {
                if matches!(part, std::path::Component::RootDir) {
                    continue;
                }
                pending.push_front(part.as_os_str().to_owned());
            }
        } else if !pending.is_empty() {
            ensure!(metadata.is_dir(), "host path ancestor is not a directory");
        }
    }
    Ok(result)
}

#[cfg(target_os = "linux")]
fn host_link(path: &Path) -> Result<Vec<u8>> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let before = physical(&file.metadata()?)?;
    let bytes = link_bytes(&file)?;
    let descriptor_after = physical(&file.metadata()?)?;
    let path_after = physical(&fs::symlink_metadata(path)?)?;
    ensure!(
        before == descriptor_after && before == path_after,
        "host symlink inverse differs for {}: descriptor fields {:?}, path fields {:?}",
        path.display(),
        before.changed_fields(&descriptor_after),
        before.changed_fields(&path_after)
    );
    Ok(bytes)
}

#[cfg(not(target_os = "linux"))]
fn host_link(_: &Path) -> Result<Vec<u8>> {
    bail!("native host symlink authentication requires Linux")
}

#[derive(Serialize)]
struct Dependency {
    soname: String,
    loader_path: PathBuf,
    resolved_path: PathBuf,
    digest: FileDigest,
    physical: Physical13,
    delivery: &'static str,
    elf: elf::SharedEvidence,
    copied_identity: Option<Physical13>,
}

fn host_family(soname: &str) -> bool {
    // Existing script/bundle-linux exclusions plus the platform loader and the
    // glibc rt family. libstdc++ is intentionally bundled when dynamically needed.
    [
        "ld-linux",
        "libc.so",
        "libm.so",
        "libpthread.so",
        "libdl.so",
        "librt.so",
        "libgcc_s.so",
        "libresolv.so",
        "libasound.so",
        "libglib-2.0.so",
        "libgobject-2.0.so",
        "libgio-2.0.so",
        "libgmodule-2.0.so",
        "libpcre.so",
        "libpcre2-8.so",
        "libmount.so",
        "libblkid.so",
        "libselinux.so",
        "libffi.so",
        "libz.so",
    ]
    .iter()
    .any(|prefix| soname.starts_with(prefix))
}

struct PlannedDependency {
    file: File,
    soname: String,
    loader_path: PathBuf,
    resolved_path: PathBuf,
    digest: FileDigest,
    physical: Physical13,
    preliminary_elf: elf::SharedEvidence,
}

fn descriptor_digest(file: &File, cap: u64) -> Result<(FileDigest, Physical13)> {
    let before = physical(&file.metadata()?)?;
    ensure!(
        file.metadata()?.is_file() && before.bytes > 0 && before.bytes <= cap,
        "descriptor size/type differs"
    );
    let mut input = file.try_clone()?;
    input.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut count = 0_u64;
    let mut buffer = vec![0; 256 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        count = count
            .checked_add(read as u64)
            .context("descriptor size overflow")?;
        ensure!(
            count <= before.bytes && count <= cap,
            "descriptor grew during authentication"
        );
        digest.update(&buffer[..read]);
    }
    input.seek(SeekFrom::Start(0))?;
    ensure!(
        count == before.bytes && before == physical(&file.metadata()?)?,
        "descriptor full13 inverse differs"
    );
    Ok((
        FileDigest {
            bytes: count,
            sha256: format!("{:x}", digest.finalize()),
        },
        before,
    ))
}

fn require_loader_inverse(before: &Physical13, after: &Physical13) -> Result<()> {
    let mut comparison = after.clone();
    comparison.atime_ns = before.atime_ns;
    ensure!(
        *before == comparison,
        "official loader changed a byte/identity field beyond observed atime"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn loader_paths(
    binary: &File,
    binary_elf: &elf::ElfEvidence,
    budget: &mut elf::MetadataBudget,
) -> Result<(String, BTreeMap<String, PathBuf>)> {
    use std::os::fd::AsRawFd as _;
    let interpreter = binary_elf
        .interpreter
        .as_deref()
        .context("dynamic application interpreter absent")?;
    ensure!(
        matches!(
            interpreter,
            "/lib64/ld-linux-x86-64.so.2" | "/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2"
        ),
        "unrecognized platform loader; explicit host support required"
    );
    budget.charge(512 * 1024, "bounded loader output transport buffer")?;
    let retained_path = format!("/proc/{}/fd/{}", std::process::id(), binary.as_raw_fd());
    let text = String::from_utf8(command(
        None,
        interpreter,
        &["--list", &retained_path],
        256 * 1024,
    )?)?;
    let paths = parse_loader_paths(&text, interpreter, binary_elf, budget)?;
    Ok((text, paths))
}
#[cfg(any(target_os = "linux", test))]
fn parse_loader_paths(
    text: &str,
    interpreter: &str,
    binary_elf: &elf::ElfEvidence,
    budget: &mut elf::MetadataBudget,
) -> Result<BTreeMap<String, PathBuf>> {
    ensure!(
        text.len() <= 256 * 1024 && !text.contains("not found"),
        "unresolved/oversized native dependencies"
    );
    let mut paths = BTreeMap::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if line.starts_with("linux-vdso.so.1 (") {
            continue;
        }
        if let Some((soname, right)) = line.split_once(" => ") {
            let path = right
                .split_once(" (")
                .context("malformed loader dependency")?
                .0;
            ensure!(
                !soname.is_empty()
                    && soname.len() <= 256
                    && !soname.contains('/')
                    && !soname.chars().any(char::is_control)
                    && path.starts_with('/'),
                "unconfined loader dependency"
            );
            ensure!(
                path.len() <= super::MAX_PATH_BYTES && paths.len() < 256,
                "loader dependency path/count budget differs"
            );
            budget.charge(
                (soname.len() as u64 + path.len() as u64) * 2 + 4096,
                "loader resolution strings/map growth",
            )?;
            ensure!(
                paths
                    .insert(soname.to_owned(), PathBuf::from(path))
                    .is_none(),
                "duplicate loader SONAME"
            );
        } else {
            let path = line
                .split_once(" (")
                .context("unrecognized loader output")?
                .0;
            ensure!(path == interpreter, "unrecognized direct loader member");
            budget.charge(
                (path.len() as u64 + 64) * 2 + 4096,
                "direct loader resolution strings/map growth",
            )?;
            ensure!(
                paths
                    .insert("ld-linux-x86-64.so.2".to_owned(), PathBuf::from(path))
                    .is_none(),
                "duplicate direct loader"
            );
        }
    }
    ensure!(
        !paths.is_empty() && paths.len() <= 256,
        "loader dependency budget differs"
    );
    for needed in &binary_elf.needed {
        ensure!(
            paths.contains_key(needed),
            "missing direct ELF dependency {needed}"
        );
    }
    Ok(paths)
}
#[cfg(target_os = "linux")]
fn plan_dependencies(
    binary: &File,
    binary_elf: &elf::ElfEvidence,
    budget: &mut elf::MetadataBudget,
) -> Result<(String, Vec<PlannedDependency>)> {
    let (text, paths) = loader_paths(binary, binary_elf, budget)?;
    budget.charge(
        (paths.len() as u64) * std::mem::size_of::<PlannedDependency>() as u64 * 2 + 256,
        "retained dependency-plan vector",
    )?;
    let mut dependencies = Vec::new();
    dependencies.try_reserve_exact(paths.len())?;
    for (soname, loader_path) in paths {
        let resolved_path = resolve_noatime_with_budget(&loader_path, budget)?;
        let file = open_noatime(&resolved_path, false)?;
        let (digest, physical) = descriptor_digest(&file, MAX_BINARY_BYTES)?;
        let library_elf = elf::inspect_file_with_budget(file.try_clone()?, budget)?;
        ensure!(
            descriptor_digest(&file, MAX_BINARY_BYTES)? == (digest.clone(), physical.clone()),
            "planned dependency digest/physical inverse differs"
        );
        if let Some(actual) = &library_elf.soname {
            ensure!(actual == &soname, "resolved library SONAME differs");
        }
        let preliminary_elf = elf::SharedEvidence::new(library_elf, budget)?;
        dependencies.push(PlannedDependency {
            file,
            soname,
            loader_path,
            resolved_path,
            digest,
            physical,
            preliminary_elf,
        });
    }
    Ok((text, dependencies))
}
#[cfg(not(target_os = "linux"))]
fn plan_dependencies(
    _: &File,
    _: &elf::ElfEvidence,
    _: &mut elf::MetadataBudget,
) -> Result<(String, Vec<PlannedDependency>)> {
    bail!("retained-descriptor native loader resolution requires Linux")
}
#[cfg(target_os = "linux")]
fn verify_copied_resolution(
    binary: &File,
    binary_elf: &elf::ElfEvidence,
    planned: &[PlannedDependency],
    budget: &mut elf::MetadataBudget,
) -> Result<()> {
    let (_, paths) = loader_paths(binary, binary_elf, budget)?;
    ensure!(
        paths.len() == planned.len(),
        "copied executable dependency closure differs"
    );
    // One new descriptor at a time; do not retain a second full ELF evidence vector.
    for plan in planned {
        let path = paths
            .get(&plan.soname)
            .context("copied resolution omitted dependency")?;
        let resolved = resolve_noatime_with_budget(path, budget)?;
        let file = open_noatime(&resolved, false)?;
        ensure!(
            descriptor_digest(&file, MAX_BINARY_BYTES)?
                == (plan.digest.clone(), plan.physical.clone()),
            "copied executable loader dependency bytes/identity changed since admission"
        );
    }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn verify_copied_resolution(
    _: &File,
    _: &elf::ElfEvidence,
    _: &[PlannedDependency],
    _: &mut elf::MetadataBudget,
) -> Result<()> {
    bail!("copied native loader resolution requires Linux")
}

#[cfg(all(test, target_os = "linux"))]
fn deliver_dependencies(
    planned: &[PlannedDependency],
    directory: &Path,
) -> Result<Vec<Dependency>> {
    deliver_dependencies_with_budget(planned, directory, &mut elf::MetadataBudget::new())
}
fn deliver_dependencies_with_budget(
    planned: &[PlannedDependency],
    directory: &Path,
    budget: &mut elf::MetadataBudget,
) -> Result<Vec<Dependency>> {
    budget.charge(
        planned.len() as u64 * std::mem::size_of::<Dependency>() as u64 * 2 + 256,
        "delivered dependency vector",
    )?;
    let mut dependencies = Vec::new();
    dependencies.try_reserve_exact(planned.len())?;
    for plan in planned {
        ensure!(
            descriptor_digest(&plan.file, MAX_BINARY_BYTES)?
                == (plan.digest.clone(), plan.physical.clone()),
            "dependency changed since admission"
        );
        let delivery = if host_family(&plan.soname) {
            "host"
        } else {
            "bundled"
        };
        let (library_elf, copied_identity) = if delivery == "bundled" {
            let path = directory.join(&plan.soname);
            let mut input = plan.file.try_clone()?;
            input.seek(SeekFrom::Start(0))?;
            copy_verified_descriptor(input, &path, &plan.digest, 0o555)?;
            let copied = open_noatime(&path, false)?;
            let (actual, identity) = descriptor_digest(&copied, MAX_BINARY_BYTES)?;
            ensure!(actual == plan.digest, "copied dependency digest differs");
            let facts = elf::inspect_file_with_budget(copied.try_clone()?, budget)?;
            ensure!(
                descriptor_digest(&copied, MAX_BINARY_BYTES)? == (actual, identity.clone())
                    && identity == physical(&fs::symlink_metadata(&path)?)?,
                "copied dependency ELF/descriptor inverse differs"
            );
            set_mode(&path, 0o755)?;
            ensure!(
                facts == *plan.preliminary_elf,
                "actual copied dependency ELF differs from immutable admitted evidence"
            );
            (plan.preliminary_elf.clone(), Some(identity))
        } else {
            (plan.preliminary_elf.clone(), None)
        };
        ensure!(
            *library_elf == *plan.preliminary_elf
                && descriptor_digest(&plan.file, MAX_BINARY_BYTES)?
                    == (plan.digest.clone(), plan.physical.clone()),
            "admitted/source dependency and actual delivered ELF differ"
        );
        budget.charge(
            (plan.soname.len() as u64
                + plan.loader_path.as_os_str().len() as u64
                + plan.resolved_path.as_os_str().len() as u64)
                * 2
                + 512,
            "delivered dependency identity strings",
        )?;
        dependencies.push(Dependency {
            soname: plan.soname.clone(),
            loader_path: plan.loader_path.clone(),
            resolved_path: plan.resolved_path.clone(),
            digest: plan.digest.clone(),
            physical: plan.physical.clone(),
            delivery,
            elf: library_elf,
            copied_identity,
        });
    }
    Ok(dependencies)
}

#[derive(Serialize)]
struct BoundDependencyObservation {
    soname: String,
    actual_descriptor_target: String,
    delivery: &'static str,
    digest: FileDigest,
    before_loader: Physical13,
    after_loader: Physical13,
}
#[derive(Serialize)]
struct BoundLoaderEvidence {
    actual_command: Vec<String>,
    actual_stdout: String,
    binary_digest: FileDigest,
    binary_before_loader: Physical13,
    binary_after_loader: Physical13,
    dependencies: Vec<BoundDependencyObservation>,
    constructors_entered: bool,
    runtime_compatibility_or_test_acceptance_credit: u32,
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
fn verify_bound_loader(
    binary: &File,
    binary_elf: &elf::ElfEvidence,
    planned: &[PlannedDependency],
    bundled: &Path,
    probe: &Path,
) -> Result<BoundLoaderEvidence> {
    verify_bound_loader_with_budget(
        binary,
        binary_elf,
        planned,
        bundled,
        probe,
        &mut elf::MetadataBudget::new(),
    )
}
#[cfg(target_os = "linux")]
fn verify_bound_loader_with_budget(
    binary: &File,
    binary_elf: &elf::ElfEvidence,
    planned: &[PlannedDependency],
    bundled: &Path,
    probe: &Path,
    budget: &mut elf::MetadataBudget,
) -> Result<BoundLoaderEvidence> {
    use std::os::fd::AsRawFd as _;
    ensure!(
        !planned.is_empty() && planned.len() <= 256,
        "bound loader dependency count differs"
    );
    // The admission plan reserves this private directory and at most 256 links.
    // It is outside runtime; failures retain it and prevent artifact acceptance.
    fs::create_dir(probe).context("bound loader probe directory must be new")?;
    budget.charge(
        planned.len() as u64 * 512 + 1024,
        "bound-loader retained descriptor vector",
    )?;
    let mut delivered = Vec::new();
    delivered.try_reserve_exact(planned.len())?;
    for plan in planned {
        let file = if host_family(&plan.soname) {
            plan.file.try_clone()?
        } else {
            open_noatime(&bundled.join(&plan.soname), false)?
        };
        let (digest, identity) = descriptor_digest(&file, MAX_BINARY_BYTES)?;
        // Equal complete bytes bind immutable parsed evidence; delivery already
        // parsed each actual copied dependency once, without evidence duplication.
        ensure!(
            digest == plan.digest,
            "actual delivered dependency bytes/ELF identity differs before bound loader probe"
        );
        let target = format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd());
        create_symlink(&target, &probe.join(&plan.soname))?;
        delivered.push((file, digest, identity, plan));
    }
    let probe = std::path::absolute(probe)?;
    let probe_text = probe
        .to_str()
        .context("bound loader probe path must be UTF-8")?;
    let binary_path = format!("/proc/{}/fd/{}", std::process::id(), binary.as_raw_fd());
    let interpreter = binary_elf
        .interpreter
        .as_deref()
        .context("bound loader interpreter absent")?;
    let (binary_digest, binary_before) = descriptor_digest(binary, MAX_BINARY_BYTES)?;
    budget.charge(512 * 1024, "bound-loader output transport buffer")?;
    let text = String::from_utf8(command(
        None,
        interpreter,
        &[
            "--inhibit-cache",
            "--library-path",
            probe_text,
            "--list",
            &binary_path,
        ],
        256 * 1024,
    )?)?;
    ensure!(
        !text.contains("not found"),
        "bound loader has an unresolved dependency"
    );
    let mut resolved = BTreeSet::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if line.starts_with("linux-vdso.so.1 (") {
            continue;
        }
        if let Some((soname, right)) = line.split_once(" => ") {
            let path = right
                .split_once(" (")
                .context("malformed bound loader resolution")?
                .0;
            ensure!(
                soname.len() <= 256 && path.len() <= super::MAX_PATH_BYTES,
                "bound loader member string budget differs"
            );
            budget.charge(
                (soname.len() as u64 + probe.as_os_str().len() as u64) * 4 + 4096,
                "bound-loader resolution set/path growth",
            )?;
            ensure!(
                planned.iter().any(|plan| plan.soname == soname)
                    && Path::new(path) == probe.join(soname)
                    && resolved.insert(soname.to_owned()),
                "bound loader escaped a retained dependency descriptor"
            );
        } else {
            ensure!(
                line.split_once(" (")
                    .context("malformed direct bound loader member")?
                    .0
                    == interpreter,
                "unrecognized direct bound loader member"
            );
            budget.charge(4096, "bound-loader direct resolution set growth")?;
            resolved.insert("ld-linux-x86-64.so.2".to_owned());
        }
    }
    ensure!(
        resolved.len() == planned.len()
            && planned.iter().all(|plan| resolved.contains(&plan.soname)),
        "bound loader did not resolve the complete planned dependency closure"
    );
    let (binary_after_digest, binary_after) = descriptor_digest(binary, MAX_BINARY_BYTES)?;
    ensure!(
        binary_after_digest == binary_digest,
        "bound loader binary bytes changed"
    );
    require_loader_inverse(&binary_before, &binary_after)?;
    budget.charge(
        planned.len() as u64 * std::mem::size_of::<BoundDependencyObservation>() as u64 * 2 + 256,
        "bound-loader dependency observation vector",
    )?;
    let mut observations = Vec::new();
    observations.try_reserve_exact(planned.len())?;
    for (file, digest, before, plan) in &delivered {
        let (after_digest, after) = descriptor_digest(file, MAX_BINARY_BYTES)?;
        ensure!(
            after_digest == *digest,
            "bound loader delivered dependency bytes changed"
        );
        require_loader_inverse(before, &after)?;
        budget.charge(
            plan.soname.len() as u64 * 2 + 1024,
            "bound-loader observation identity strings",
        )?;
        observations.push(BoundDependencyObservation {
            soname: plan.soname.clone(),
            actual_descriptor_target: format!(
                "/proc/{}/fd/{}",
                std::process::id(),
                file.as_raw_fd()
            ),
            delivery: if host_family(&plan.soname) {
                "retained_host_descriptor"
            } else {
                "actual_copied_descriptor"
            },
            digest: digest.clone(),
            before_loader: before.clone(),
            after_loader: after.clone(),
        });
        if !host_family(&plan.soname) {
            ensure!(
                after == physical(&fs::symlink_metadata(bundled.join(&plan.soname))?)?,
                "bundled dependency pathname changed during bound loader probe"
            );
        }
    }
    // Cleanup errors are actual packaging failures; no TempDir Drop discards one.
    for plan in planned {
        fs::remove_file(probe.join(&plan.soname))?;
    }
    fs::remove_dir(&probe)?;
    budget.charge(
        (interpreter.len() as u64 + probe_text.len() as u64 + 128) * 4 + 2048,
        "bound-loader actual command evidence strings/vector",
    )?;
    Ok(BoundLoaderEvidence {
        actual_command: vec![
            interpreter.to_owned(),
            "--inhibit-cache".to_owned(),
            "--library-path".to_owned(),
            probe_text.to_owned(),
            "--list".to_owned(),
            binary_path,
        ],
        actual_stdout: text,
        binary_digest,
        binary_before_loader: binary_before,
        binary_after_loader: binary_after,
        dependencies: observations,
        constructors_entered: false,
        runtime_compatibility_or_test_acceptance_credit: 0,
    })
}

#[cfg(not(target_os = "linux"))]
fn verify_bound_loader_with_budget(
    _: &File,
    _: &elf::ElfEvidence,
    _: &[PlannedDependency],
    _: &Path,
    _: &Path,
    _: &mut elf::MetadataBudget,
) -> Result<BoundLoaderEvidence> {
    bail!("bound native loader verification requires Linux")
}

#[derive(Serialize)]
struct NativeEvidence {
    schema_version: u32,
    source_manifest_sha256: String,
    source_owner_receipt_sha256: String,
    source_owner_receipt_role: &'static str,
    build_receipt_sha256: String,
    build: BuildReceipt,
    binary_elf: elf::ElfEvidence,
    copied_binary_digest: FileDigest,
    copied_binary_before_loader: Physical13,
    copied_binary_after_loader: Physical13,
    copied_identity_role: &'static str,
    original_binary_before_loader: Physical13,
    original_binary_after_loader: Physical13,
    official_loader_list: String,
    bound_loader_probe: BoundLoaderEvidence,
    dependencies: Vec<Dependency>,
    runner_glibc: String,
    runner_os_release: String,
    cargo_lock: FileDigest,
    rust_toolchain: FileDigest,
    source_package_policy: &'static str,
    executable_relative_path: &'static str,
    library_directory_relative_path: &'static str,
    dev_asset_root: &'static str,
    dlopen_host_requirements: Vec<&'static str>,
    host_compatibility_and_native_resource_admission_pending: bool,
    build_success_is_not_tests_or_runtime_acceptance: bool,
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn host_glibc() -> Result<String> {
    // SAFETY: glibc returns an immutable NUL-terminated static version string.
    Ok(
        unsafe { std::ffi::CStr::from_ptr(libc::gnu_get_libc_version()) }
            .to_str()?
            .to_owned(),
    )
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn host_glibc() -> Result<String> {
    bail!("artifact packaging requires the admitted GNU/Linux loader")
}

fn payload_inventory(root: &Path) -> Result<PayloadManifest> {
    fn visit(root: &Path, path: &Path, rows: &mut Vec<PayloadEntry>) -> Result<()> {
        ensure!(
            rows.len() < MAX_PAYLOAD_ENTRIES,
            "payload entry budget exhausted"
        );
        let name = if path == root {
            "runtime".to_owned()
        } else {
            format!(
                "runtime/{}",
                path.strip_prefix(root)?
                    .to_str()
                    .context("non-UTF8 payload member")?
            )
        };
        payload_relative(&name)?;
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_dir() {
            set_mode(path, 0o755)?;
            rows.push(PayloadEntry {
                path: name,
                kind: PayloadKind::Directory,
                mode: 0o755,
                bytes: 0,
                sha256: None,
                link_target: None,
            });
            // This is owned generated payload, never a foreign source tree.
            let mut children: Vec<_> = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            children.sort_by_key(|entry| entry.file_name());
            for child in children {
                visit(root, &child.path(), rows)?;
            }
        } else if metadata.file_type().is_symlink() {
            let target = String::from_utf8(host_link(path)?)?;
            normalized_link(&name, &target)?;
            rows.push(PayloadEntry {
                path: name,
                kind: PayloadKind::Symlink,
                mode: 0o777,
                bytes: target.len() as u64,
                sha256: Some(sha256(target.as_bytes())),
                link_target: Some(target),
            });
        } else {
            ensure!(metadata.is_file(), "unsupported payload inode");
            let mode = physical(&metadata)?.mode & 0o777;
            ensure!(
                matches!(mode, 0o644 | 0o755),
                "unsupported generated file mode"
            );
            // Empty tracked files are valid; unlike ELF or metadata they need no
            // nonempty-content requirement. Their SHA remains authenticated.
            let mut file = open_noatime(path, false)?;
            let before = physical(&file.metadata()?)?;
            let mut digest = Sha256::new();
            let mut size = 0_u64;
            let mut buffer = vec![0; 256 * 1024];
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                size = size
                    .checked_add(read as u64)
                    .context("payload size overflow")?;
                ensure!(
                    size <= MAX_BINARY_BYTES,
                    "payload member byte budget exhausted"
                );
                digest.update(&buffer[..read]);
            }
            ensure!(
                size == before.bytes && before == physical(&file.metadata()?)?,
                "payload changed during inventory"
            );
            rows.push(PayloadEntry {
                path: name,
                kind: PayloadKind::File,
                mode,
                bytes: size,
                sha256: Some(format!("{:x}", digest.finalize())),
                link_target: None,
            });
        }
        Ok(())
    }
    let mut rows = Vec::new();
    visit(root, root, &mut rows)?;
    rows.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    let mut bytes = 0_u64;
    let mut directories = 0;
    let mut symlinks = 0;
    let mut files = 0;
    for row in &rows {
        match row.kind {
            PayloadKind::File => {
                files += 1;
                bytes = bytes
                    .checked_add(row.bytes)
                    .context("payload total overflow")?;
            }
            PayloadKind::Directory => directories += 1,
            PayloadKind::Symlink => symlinks += 1,
        }
    }
    let manifest = PayloadManifest {
        schema_version: 1,
        entries: rows,
        unpacked_regular_bytes: bytes,
        directory_count: directories,
        symlink_count: symlinks,
        regular_file_count: files,
    };
    manifest.validate()?;
    Ok(manifest)
}

fn archive_payload(runtime: &Path, manifest: &PayloadManifest, archive: &Path) -> Result<()> {
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(archive)?;
    let bound = producer::compression_bound(bounded_tar::expanded_limit(manifest)?)?;
    ensure!(
        bound <= MAX_ARCHIVE_BYTES,
        "archive worst-case bound exceeds transport cap"
    );
    let encoder = GzEncoder::new(
        producer::LimitedWriter::new(file, bound),
        Compression::new(6),
    );
    let mut builder = tar::Builder::new(encoder);
    for entry in &manifest.entries {
        let mut header = tar::Header::new_gnu();
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_mode(entry.mode);
        match entry.kind {
            PayloadKind::Directory => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
            }
            PayloadKind::Symlink => {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
            }
            PayloadKind::File => {
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(entry.bytes);
            }
        }
        header.set_cksum();
        if entry.kind == PayloadKind::File {
            let path = runtime
                .parent()
                .context("runtime parent absent")?
                .join(&entry.path);
            builder.append_data(&mut header, &entry.path, open_noatime(&path, false)?)?;
        } else if entry.kind == PayloadKind::Symlink {
            builder.append_link(
                &mut header,
                &entry.path,
                entry
                    .link_target
                    .as_deref()
                    .context("archive link target absent")?,
            )?;
        } else {
            builder.append_data(&mut header, &entry.path, std::io::empty())?;
        }
    }
    builder.into_inner()?.finish()?.into_inner().sync_all()?;
    ensure!(
        digest_file(archive, MAX_ARCHIVE_BYTES)?.0.bytes > 0,
        "archive is empty"
    );
    Ok(())
}

fn split_archive(archive: &Path, directory: &Path) -> Result<Vec<Piece>> {
    fs::create_dir(directory)?;
    let mut source = open_noatime(archive, false)?;
    let before = physical(&source.metadata()?)?;
    ensure!(
        before.bytes > 0 && before.bytes <= MAX_ARCHIVE_BYTES,
        "archive byte budget differs"
    );
    let mut pieces = Vec::new();
    let mut buffer = vec![0; 256 * 1024];
    loop {
        let order = pieces.len();
        let name = format!("{order:06}.part");
        let first = source.read(&mut buffer)?;
        if first == 0 {
            break;
        }
        ensure!(order < MAX_PIECES, "piece count budget exhausted");
        let mut piece = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(directory.join(&name))?;
        let mut bytes = 0_u64;
        let mut digest = Sha256::new();
        let mut next = first;
        loop {
            piece.write_all(&buffer[..next])?;
            digest.update(&buffer[..next]);
            bytes += next as u64;
            if bytes == PIECE_BYTES {
                break;
            }
            let remaining = usize::try_from((PIECE_BYTES - bytes).min(buffer.len() as u64))?;
            next = source.read(&mut buffer[..remaining])?;
            if next == 0 {
                break;
            }
        }
        piece.sync_all()?;
        pieces.push(Piece {
            order,
            file: name,
            bytes,
            sha256: format!("{:x}", digest.finalize()),
        });
    }
    ensure!(
        before == physical(&source.metadata()?)?,
        "archive inverse changed while splitting"
    );
    validate_pieces(&pieces, &digest_file(archive, MAX_ARCHIVE_BYTES)?.0)?;
    Ok(pieces)
}

pub(super) fn package(args: PackageArgs) -> Result<()> {
    let output = args.output_dir.clone();
    let mut owned = false;
    let result = package_inner(args, &mut owned);
    preserve_failure(&output, owned, "package-command-failure.json", &result)?;
    result
}

fn package_inner(args: PackageArgs, owned: &mut bool) -> Result<()> {
    let request = request_input(&args.request, &args.request_sha256, Some(&args.source))?;
    let build: BuildReceipt = authenticated_json(
        &args.build_receipt,
        &args.build_receipt_sha256,
        MAX_BUILD_EVIDENCE_BYTES,
    )?;
    ensure!(
        build.schema_version == 1
            && build.request.schema_version == 1
            && build.request.repository == REPOSITORY
            && build.state == "ACTUAL_FIXED_CARGO_CHILD_WAIT_ZERO_NOT_TEST_OR_RUNTIME_ACCEPTANCE"
            && build.request.source_sha == request.source_sha
            && build.request.source_tree == request.source_tree
            && build.request.source_manifest_sha256 == request.source_manifest_sha256
            && build.request.source_owner_receipt_sha256 == request.source_owner_receipt_sha256
            && build.actual_child_exit == 0
            && build.actual_build_finished_success
            && build.observed_command == fixed_build_command()
            && build.default_features
            && build.removed_features.is_empty()
            && build.profile == "dev"
            && build.debug == 0
            && !build.incremental
            && build.binary.digest.bytes > 0
            && build.runtime_test_and_reference_credit == 0,
        "build receipt does not bind the fixed successful native build"
    );
    require_hex(&build.controller_sha, 40)?;
    numeric_identity(&build.run_id)?;
    numeric_identity(&build.run_attempt)?;
    validate_features(&build.feature_receipts)?;
    for binding in [
        &build.source_before,
        &build.source_after,
        &build.build_log,
        &build.binary,
        &build.process_closure,
    ] {
        ensure!(
            digest_file(&binding.path, MAX_BINARY_BYTES)?.0 == binding.digest,
            "named build input digest differs"
        );
    }
    let process_closure: serde_json::Value = authenticated_json(
        &build.process_closure.path,
        &build.process_closure.digest.sha256,
        MAX_METADATA_BYTES,
    )?;
    ensure!(
        process_closure["state"] == "ACTUAL_OWNED_PROCESS_SESSION_CLOSED0"
            && process_closure["leader_exit_code"] == 0
            && process_closure["deadline_exhausted"] == false
            && process_closure["actual_remaining_owned_processes"] == 0
            && process_closure["cleanup_errors"]
                .as_array()
                .is_some_and(Vec::is_empty),
        "actual build process closure cannot grant successful packaging"
    );
    let source_after: SourceObservation = authenticated_json(
        &build.source_after.path,
        &build.source_after.digest.sha256,
        MAX_MANIFEST_BYTES,
    )?;
    ensure!(
        source_after.authentication == Authentication::Materialized
            && source_after.canonical_manifest.sha256 == request.source_manifest_sha256
            && source_after.source_commit == request.source_sha
            && source_after.source_tree == request.source_tree
            && source_after.tracked_entries > 0
            && source_after.tracked_entries == source_after.physically_materialized_entries
            && source_after.physically_materialized_entries == source_after.physical.len()
            && source_after.full_physical_noatime_inverses_equal
            && !source_after.sparse_files_were_not_materialized
            && source_after.closure == "CLOSED0"
            && source_after.runtime_test_or_reference_credit == 0,
        "build physical source observation differs"
    );
    let (manifest, before) = observe(&args.source, Authentication::Materialized)?;
    ensure!(
        before.canonical_manifest.sha256 == request.source_manifest_sha256,
        "package source identity differs"
    );
    // Package may run under sudo for O_NOATIME on root-owned system libraries.
    // Reading the initial source and artifacts never restores their timestamps.
    let package_atime_transitions = source_transitions(&source_after.physical, &before.physical)?;
    // The official loader receives a retained descriptor, not a mutable original
    // pathname. No payload has been copied while resolving this admission plan.
    let binary_source = open_noatime(&build.binary.path, false)?;
    let (source_digest, original_binary_before_loader) =
        descriptor_digest(&binary_source, MAX_BINARY_BYTES)?;
    ensure!(
        source_digest == build.binary.digest,
        "admission binary differs from actual build receipt"
    );
    let mut elf_budget = elf::MetadataBudget::new();
    let preliminary_binary_elf =
        elf::inspect_file_with_budget(binary_source.try_clone()?, &mut elf_budget)?;
    ensure!(
        preliminary_binary_elf.rpath.is_none() && preliminary_binary_elf.runpath.is_none(),
        "relocating an ELF with RPATH/RUNPATH requires an explicit loader policy"
    );
    let (_, planned_dependencies) =
        plan_dependencies(&binary_source, &preliminary_binary_elf, &mut elf_budget)?;
    let (after_loader_digest, original_binary_after_loader) =
        descriptor_digest(&binary_source, MAX_BINARY_BYTES)?;
    ensure!(
        after_loader_digest == source_digest,
        "original binary changed during official loader resolution"
    );
    require_loader_inverse(
        &original_binary_before_loader,
        &original_binary_after_loader,
    )?;
    ensure!(
        original_binary_after_loader == physical(&fs::symlink_metadata(&build.binary.path)?)?,
        "original binary pathname changed during admission"
    );
    let git_plan = producer::git_plan(&args.source)?;
    let evidence_upper = build
        .source_before
        .digest
        .bytes
        .checked_add(build.source_after.digest.bytes)
        .and_then(|n| n.checked_add(build.build_log.digest.bytes))
        .and_then(|n| n.checked_add(build.process_closure.digest.bytes))
        .and_then(|n| {
            n.checked_add(3 * MAX_MANIFEST_BYTES + MAX_BUILD_EVIDENCE_BYTES + MAX_METADATA_BYTES)
        })
        .context("producer evidence bound overflow")?;
    let parent = args
        .output_dir
        .parent()
        .context("producer output parent absent")?;
    let (block, available, inodes) = super::filesystem_space(parent)?;
    elf_budget.charge(
        planned_dependencies.len() as u64 * 1024 + 256,
        "producer dependency-name admission copies",
    )?;
    let admission = producer::plan(
        &manifest,
        &git_plan,
        &build.binary.digest,
        planned_dependencies
            .iter()
            .filter(|plan| !host_family(&plan.soname))
            .map(|plan| (plan.soname.clone(), plan.digest.bytes)),
        evidence_upper,
        block,
        available,
        inodes,
    )?;
    fs::create_dir(&args.output_dir).context("artifact output directory must be new")?;
    *owned = true;
    let admission_path = args.output_dir.join("producer-admission.json");
    write_json(&admission_path, &admission, MAX_METADATA_BYTES)?;
    ensure!(
        admission.fits,
        "producer disk/inode reserve does not fit; genuine failure preflight retained"
    );
    let runtime = args.output_dir.join("runtime");
    fs::create_dir(&runtime)?;
    fs::create_dir(runtime.join("lib"))?;
    fs::create_dir(runtime.join("evidence"))?;
    let source_destination = runtime.join("source");
    copy_source(&args.source, &manifest, &source_destination)?;
    reconstruct_git_with_plan(&args.source, &source_destination, &git_plan)?;
    let executable_destination = source_destination.join("target/debug/koda");
    ensure_parents(&executable_destination)?;
    let mut binary_copy_source = binary_source.try_clone()?;
    binary_copy_source.seek(SeekFrom::Start(0))?;
    copy_verified_descriptor(
        binary_copy_source,
        &executable_destination,
        &build.binary.digest,
        0o555,
    )?;
    let copied_binary = open_noatime(&executable_destination, false)?;
    let (copied_binary_digest, copied_binary_before_loader) =
        descriptor_digest(&copied_binary, MAX_BINARY_BYTES)?;
    ensure!(
        copied_binary_digest == build.binary.digest,
        "packaged executable digest differs"
    );
    let binary_elf = elf::inspect_file_with_budget(copied_binary.try_clone()?, &mut elf_budget)?;
    ensure!(
        binary_elf == preliminary_binary_elf,
        "actual copied ELF differs from admitted build bytes"
    );
    drop(preliminary_binary_elf);
    verify_copied_resolution(
        &copied_binary,
        &binary_elf,
        &planned_dependencies,
        &mut elf_budget,
    )?;
    let (copied_after_digest, copied_binary_after_loader) =
        descriptor_digest(&copied_binary, MAX_BINARY_BYTES)?;
    require_loader_inverse(&copied_binary_before_loader, &copied_binary_after_loader)?;
    ensure!(
        copied_after_digest == copied_binary_digest
            && copied_binary_after_loader
                == physical(&fs::symlink_metadata(&executable_destination)?)?,
        "copied executable loader/ELF identity inverse differs"
    );
    set_mode(&executable_destination, 0o755)?;
    let dependencies = deliver_dependencies_with_budget(
        &planned_dependencies,
        &runtime.join("lib"),
        &mut elf_budget,
    )?;
    let bound_loader_probe = verify_bound_loader_with_budget(
        &copied_binary,
        &binary_elf,
        &planned_dependencies,
        &runtime.join("lib"),
        &args.output_dir.join("loader-fd-probe"),
        &mut elf_budget,
    )?;
    copy_verified(
        &admission_path,
        &runtime.join("evidence/producer-admission.json"),
        &digest_file(&admission_path, MAX_METADATA_BYTES)?.0,
        0o644,
    )?;
    let source_manifest_path = runtime.join("evidence/source-manifest.json");
    write_json(&source_manifest_path, &manifest, MAX_MANIFEST_BYTES)?;
    ensure!(
        digest_file(&source_manifest_path, MAX_MANIFEST_BYTES)?
            .0
            .sha256
            == request.source_manifest_sha256,
        "copied source manifest digest differs"
    );
    for (binding, name) in [
        (&build.source_before, "source-before-build-observation.json"),
        (&build.source_after, "source-after-build-observation.json"),
        (&build.build_log, "cargo-build.jsonl"),
        (&build.process_closure, "cargo-process-closure.json"),
    ] {
        copy_verified(
            &binding.path,
            &runtime.join("evidence").join(name),
            &binding.digest,
            0o644,
        )?;
    }
    write_json(
        &runtime.join("evidence/package-atime-transitions.json"),
        &package_atime_transitions,
        MAX_MANIFEST_BYTES,
    )?;
    elf_budget.charge(
        1024 * 1024,
        "native evidence retained loader output/system metadata",
    )?;
    let runner_os_release_path =
        resolve_noatime_with_budget(Path::new("/etc/os-release"), &mut elf_budget)?;
    let evidence = NativeEvidence {
        schema_version: 1,
        source_manifest_sha256: request.source_manifest_sha256.clone(),
        source_owner_receipt_sha256: request.source_owner_receipt_sha256.clone(),
        source_owner_receipt_role: "Root-declared immutable identity; CI does not authenticate local owner/checkpoint receipt bytes",
        build_receipt_sha256: args.build_receipt_sha256,
        binary_elf,
        copied_binary_digest,
        copied_binary_before_loader,
        copied_binary_after_loader,
        copied_identity_role: "full13 during read-only 0555 ELF/loader inspection; explicit final packaging permission restoration to 0755 is authenticated by payload manifest",
        original_binary_before_loader,
        original_binary_after_loader,
        official_loader_list: bound_loader_probe.actual_stdout.clone(),
        bound_loader_probe,
        dependencies,
        runner_glibc: host_glibc()?,
        runner_os_release: String::from_utf8(read_bounded(&runner_os_release_path, 64 * 1024)?.0)?,
        cargo_lock: digest_file(&args.source.source.join("Cargo.lock"), MAX_MANIFEST_BYTES)?.0,
        rust_toolchain: digest_file(
            &args.source.source.join("rust-toolchain.toml"),
            MAX_METADATA_BYTES,
        )?
        .0,
        source_package_policy: "exact tracked source plus reconstructed commit/tree/blob objects, detached HEAD, shallow boundary, safe core-only config and generated index; no original Git controls copied",
        executable_relative_path: "source/target/debug/koda",
        library_directory_relative_path: "lib",
        dev_asset_root: "source; discovered through executable ancestors containing its real .git directory",
        dlopen_host_requirements: vec![
            "libva.so.2",
            "libva-drm.so.2",
            "libEGL.so.1",
            "Vulkan loader and an independently admitted ICD",
        ],
        host_compatibility_and_native_resource_admission_pending: true,
        build_success_is_not_tests_or_runtime_acceptance: true,
        build,
    };
    let native_evidence = runtime.join("evidence/native-build.json");
    write_json(&native_evidence, &evidence, MAX_BUILD_EVIDENCE_BYTES)?;
    let (_, after) = observe(&args.source, Authentication::Materialized)?;
    ensure!(
        after.canonical_manifest == before.canonical_manifest && after.physical == before.physical,
        "complete package source inverses differ"
    );
    write_json(
        &runtime.join("evidence/source-package-observation.json"),
        &after,
        MAX_MANIFEST_BYTES,
    )?;
    let payload = payload_inventory(&runtime)?;
    ensure!(
        payload.unpacked_regular_bytes <= admission.payload_upper_bytes,
        "admitted producer payload upper bound differs"
    );
    let payload_path = args.output_dir.join("payload-manifest.json");
    write_json(&payload_path, &payload, MAX_MANIFEST_BYTES)?;
    let archive = args.output_dir.join("runtime.tar.gz");
    archive_payload(&runtime, &payload, &archive)?;
    // Inventory is authenticated again after archive creation; no archived source,
    // dependency or build-evidence byte can drift unnoticed before publication.
    ensure!(
        payload_inventory(&runtime)?.entries == payload.entries,
        "complete archived payload inverse differs"
    );
    let archive_digest = digest_file(&archive, MAX_ARCHIVE_BYTES)?.0;
    let pieces = split_archive(&archive, &args.output_dir.join("pieces"))?;
    #[derive(Serialize)]
    struct PieceManifest<'a> {
        schema_version: u32,
        source_sha: &'a str,
        source_tree: &'a str,
        source_owner_receipt_sha256: &'a str,
        concatenated_archive: &'a FileDigest,
        maximum_github_zip_bytes: u64,
        pieces: &'a [Piece],
    }
    let piece_path = args.output_dir.join("piece-manifest.json");
    write_json(
        &piece_path,
        &PieceManifest {
            schema_version: 1,
            source_sha: &request.source_sha,
            source_tree: &request.source_tree,
            source_owner_receipt_sha256: &request.source_owner_receipt_sha256,
            concatenated_archive: &archive_digest,
            maximum_github_zip_bytes: MAX_ZIP_BYTES,
            pieces: &pieces,
        },
        MAX_METADATA_BYTES,
    )?;
    let metadata = ArtifactMetadata {
        schema_version: 1,
        repository: REPOSITORY.to_owned(),
        source_sha: request.source_sha.clone(),
        source_tree: request.source_tree.clone(),
        source_manifest_sha256: request.source_manifest_sha256.clone(),
        source_owner_receipt_sha256: request.source_owner_receipt_sha256.clone(),
        controller_sha: evidence.build.controller_sha.clone(),
        run_id: evidence.build.run_id.clone(),
        run_attempt: evidence.build.run_attempt.clone(),
        runtime_archive: archive_digest,
        payload_manifest: digest_file(&payload_path, MAX_MANIFEST_BYTES)?.0,
        build_evidence: digest_file(&native_evidence, MAX_BUILD_EVIDENCE_BYTES)?.0,
        piece_manifest: digest_file(&piece_path, MAX_METADATA_BYTES)?.0,
        pieces,
        unpacked_regular_bytes: payload.unpacked_regular_bytes,
        directory_count: payload.directory_count,
        symlink_count: payload.symlink_count,
        regular_file_count: payload.regular_file_count,
    };
    metadata.validate(&request)?;
    write_json(
        &args.output_dir.join("artifact-metadata.json"),
        &metadata,
        MAX_METADATA_BYTES,
    )?;
    Ok(())
}

pub(super) fn extract(args: ExtractArgs) -> Result<()> {
    let request = request_input(&args.request, &args.request_sha256, None)?;
    let metadata: ArtifactMetadata =
        authenticated_json(&args.metadata, &args.metadata_sha256, MAX_METADATA_BYTES)?;
    metadata.validate(&request)?;
    let payload: PayloadManifest = authenticated_json(
        &args.payload_manifest,
        &metadata.payload_manifest.sha256,
        MAX_MANIFEST_BYTES,
    )?;
    payload.validate()?;
    ensure!(
        payload.unpacked_regular_bytes == metadata.unpacked_regular_bytes
            && payload.directory_count == metadata.directory_count
            && payload.symlink_count == metadata.symlink_count
            && payload.regular_file_count == metadata.regular_file_count,
        "payload/metadata totals differ"
    );
    // This command starts no app. Root must perform disk preflight before this
    // write and separately admit cgroup/host ABI/graphics/actors before launch.
    fs::create_dir(&args.output_dir).context("extraction destination must be new")?;
    let archive_path = args.output_dir.join("authenticated-runtime.tar.gz");
    let mut archive_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&archive_path)?;
    let mut archive_hash = Sha256::new();
    let mut archive_bytes = 0_u64;
    let mut pieces_before = Vec::new();
    let mut buffer = vec![0; 256 * 1024];
    for piece in &metadata.pieces {
        let path = args.pieces.join(&piece.file);
        let mut input = open_noatime(&path, false)?;
        let before = physical(&input.metadata()?)?;
        ensure!(
            input.metadata()?.is_file() && before.bytes == piece.bytes,
            "received piece size/type differs"
        );
        let mut piece_hash = Sha256::new();
        let mut count = 0_u64;
        loop {
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            count = count
                .checked_add(read as u64)
                .context("piece size overflow")?;
            ensure!(count <= piece.bytes, "received piece grew");
            piece_hash.update(&buffer[..read]);
            archive_hash.update(&buffer[..read]);
            archive_file.write_all(&buffer[..read])?;
        }
        ensure!(
            count == piece.bytes
                && format!("{:x}", piece_hash.finalize()) == piece.sha256
                && before == physical(&input.metadata()?)?,
            "received piece digest or inverse differs"
        );
        archive_bytes = archive_bytes
            .checked_add(count)
            .context("archive size overflow")?;
        pieces_before.push((path, before));
    }
    ensure!(
        archive_bytes == metadata.runtime_archive.bytes
            && format!("{:x}", archive_hash.finalize()) == metadata.runtime_archive.sha256,
        "reassembled archive identity differs"
    );
    archive_file.sync_all()?;
    drop(archive_file);
    let archive_identity = physical(&open_noatime(&archive_path, false)?.metadata()?)?;
    let decoder = bounded_tar::decoder(BufReader::new(open_noatime(&archive_path, false)?))?;
    let mut archive = bounded_tar::RawTar::new(
        decoder,
        bounded_tar::expanded_limit(&payload)?,
        payload
            .entries
            .len()
            .checked_mul(3)
            .context("raw tar member bound overflow")?,
    );
    let declared: BTreeMap<_, _> = payload
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let mut observed = BTreeSet::new();
    let mut deferred_links = Vec::new();
    let mut regular_bytes = 0_u64;
    while let Some(member) = archive.next()? {
        let name = member.name.trim_end_matches('/').to_owned();
        payload_relative(&name)?;
        let expected = declared
            .get(name.as_str())
            .context("undeclared archive member")?;
        ensure!(
            observed.insert(name.clone()) && observed.len() <= payload.entries.len(),
            "duplicate archive member"
        );
        let mode = member.header.mode()?;
        ensure!(
            mode == expected.mode && member.header.uid()? == 0 && member.header.gid()? == 0,
            "archive ownership/mode differs"
        );
        let destination = args.output_dir.join(&name);
        match expected.kind {
            PayloadKind::Directory => {
                ensure!(
                    member.header.entry_type().is_dir() && member.header.size()? == 0,
                    "archive directory type/size differs"
                );
                fs::create_dir(&destination)?;
                set_mode(&destination, 0o755)?;
            }
            PayloadKind::File => {
                ensure!(
                    member.header.entry_type().is_file() && member.header.size()? == expected.bytes,
                    "archive regular type/size differs"
                );
                let mut output = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&destination)?;
                let mut digest = Sha256::new();
                let mut size = 0_u64;
                loop {
                    let read = archive.body(&mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    size = size
                        .checked_add(read as u64)
                        .context("extracted member overflow")?;
                    regular_bytes = regular_bytes
                        .checked_add(read as u64)
                        .context("extracted total overflow")?;
                    ensure!(
                        size <= expected.bytes && regular_bytes <= metadata.unpacked_regular_bytes,
                        "unpacked budget exceeded"
                    );
                    digest.update(&buffer[..read]);
                    output.write_all(&buffer[..read])?;
                }
                ensure!(
                    size == expected.bytes
                        && expected.sha256.as_deref()
                            == Some(format!("{:x}", digest.finalize()).as_str()),
                    "extracted content digest differs"
                );
                output.sync_all()?;
                drop(output);
                set_mode(&destination, mode)?;
            }
            PayloadKind::Symlink => {
                ensure!(
                    member.header.entry_type().is_symlink() && member.header.size()? == 0,
                    "archive link type/size differs"
                );
                let target = member
                    .link
                    .as_deref()
                    .context("archive link target missing")?;
                ensure!(
                    expected.link_target.as_deref() == Some(target),
                    "archive link target differs"
                );
                deferred_links.push((
                    destination,
                    expected
                        .link_target
                        .clone()
                        .context("declared link target absent")?,
                ));
            }
        }
    }
    let compressed = archive.finish_gzip()?;
    ensure!(
        archive_identity == physical(&compressed.get_ref().metadata()?)?
            && digest_file(&archive_path, MAX_ARCHIVE_BYTES)?.0 == metadata.runtime_archive,
        "complete reassembled gzip inverse differs"
    );
    drop(compressed);
    ensure!(
        observed.len() == payload.entries.len() && regular_bytes == metadata.unpacked_regular_bytes,
        "archive omitted declared members or logical bytes"
    );
    for (destination, target) in deferred_links {
        create_symlink(&target, &destination)?;
    }
    let runtime = args.output_dir.join("runtime");
    let actual = payload_inventory(&runtime)?;
    ensure!(
        actual.entries == payload.entries,
        "complete extracted payload identity differs"
    );
    ensure!(
        digest_file(
            &runtime.join("evidence/native-build.json"),
            MAX_BUILD_EVIDENCE_BYTES
        )?
        .0 == metadata.build_evidence,
        "extracted native build/ELF evidence differs"
    );
    ensure!(
        digest_file(
            &runtime.join("evidence/source-manifest.json"),
            MAX_MANIFEST_BYTES
        )?
        .0
        .sha256
            == request.source_manifest_sha256,
        "extracted canonical source manifest differs"
    );
    let source = SourceArgs {
        source: runtime.join("source"),
        sha: request.source_sha.clone(),
        tree: request.source_tree.clone(),
    };
    let (_, observation) = observe(&source, Authentication::Materialized)?;
    ensure!(
        observation.canonical_manifest.sha256 == request.source_manifest_sha256,
        "complete extracted tracked source identity differs"
    );
    for (path, before) in pieces_before {
        ensure!(
            before == physical(&open_noatime(&path, false)?.metadata()?)?,
            "complete received piece inverse differs"
        );
    }
    #[derive(Serialize)]
    struct ExtractionReceipt<'a> {
        state: &'static str,
        source_sha: &'a str,
        source_tree: &'a str,
        archive: &'a FileDigest,
        payload_members: usize,
        complete_source_observation: SourceObservation,
        native_host_elf_memory_and_actor_admission_pending: bool,
        application_launches: u32,
        runtime_ui_performance_and_reference_credit: u32,
    }
    write_json(
        &args.receipt,
        &ExtractionReceipt {
            state: "AUTHENTICATED_CONFINED_EXTRACTION_NOT_RUNTIME_ADMISSION",
            source_sha: &request.source_sha,
            source_tree: &request.source_tree,
            archive: &metadata.runtime_archive,
            payload_members: payload.entries.len(),
            complete_source_observation: observation,
            native_host_elf_memory_and_actor_admission_pending: true,
            application_launches: 0,
            runtime_ui_performance_and_reference_credit: 0,
        },
        MAX_MANIFEST_BYTES,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_features_require_the_actual_media_stack_and_reject_app_test_features() {
        let mut features: Vec<_> = ["koda", "call", "livekit_client", "libwebrtc", "webrtc_sys"]
            .into_iter()
            .map(|target| FeatureReceipt {
                package_id: format!("fixture#{target}@0.1.0"),
                target: target.to_owned(),
                features: Vec::new(),
            })
            .collect();
        assert!(validate_features(&features).is_ok());
        let media = features.pop().unwrap();
        assert!(validate_features(&features).is_err());
        features.push(media);
        features[0].features.push("test-support".to_owned());
        assert!(validate_features(&features).is_err());
        features[0].features = vec!["visual-tests".to_owned()];
        assert!(validate_features(&features).is_err());
    }

    #[test]
    fn atime_transitions_cannot_hide_mtime_size_mode_or_inode_changes() {
        let original = PhysicalEntry {
            path: "source.rs".to_owned(),
            identity: Physical13 {
                dev: 1,
                ino: 2,
                mode: 0o100644,
                uid: 3,
                gid: 4,
                nlink: 1,
                bytes: 5,
                atime_ns: 6,
                mtime_ns: 7,
                ctime_ns: 8,
                blocks: 9,
                blksize: 4096,
                rdev: 0,
            },
        };
        let mut read = original.clone();
        read.identity.atime_ns += 1;
        assert_eq!(
            source_transitions(&[original.clone()], &[read.clone()])
                .unwrap()
                .len(),
            1
        );
        for changed in [
            Physical13 {
                mtime_ns: 10,
                ..read.identity.clone()
            },
            Physical13 {
                bytes: 10,
                ..read.identity.clone()
            },
            Physical13 {
                mode: 0o100755,
                ..read.identity.clone()
            },
            Physical13 {
                ino: 10,
                ..read.identity.clone()
            },
        ] {
            assert!(
                source_transitions(
                    &[original.clone()],
                    &[PhysicalEntry {
                        path: read.path.clone(),
                        identity: changed
                    }]
                )
                .is_err()
            );
        }
        assert!(source_transitions(&[], &[]).is_err());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn reconstructed_git_retains_exact_tree_and_source_without_config_hooks_or_untracked_files() {
        let (_temporary, source) = super::super::tests::fixture();
        fs::write(
            source.source.join("untracked-secret"),
            b"fictional test data",
        )
        .unwrap();
        let mut config = OpenOptions::new()
            .append(true)
            .open(source.source.join(".git/config"))
            .unwrap();
        config
            .write_all(b"\n[http]\nextraheader = fictional-test-header\n")
            .unwrap();
        drop(config);
        fs::write(
            source.source.join(".git/hooks/fixture-hook"),
            b"fictional hook, not executed",
        )
        .unwrap();
        let (original, _) = observe(&source, Authentication::Materialized).unwrap();
        let destination = tempfile::tempdir().unwrap();
        let checkout = destination.path().join("source");
        copy_source(&source, &original, &checkout).unwrap();
        reconstruct_git(&source, &checkout).unwrap();
        let relocated = SourceArgs {
            source: checkout.clone(),
            sha: source.sha.clone(),
            tree: source.tree.clone(),
        };
        let (actual, _) = observe(&relocated, Authentication::Materialized).unwrap();
        assert_eq!(actual, original);
        assert!(!checkout.join("untracked-secret").exists());
        assert!(!checkout.join(".git/hooks").exists());
        let config = read_bounded(&checkout.join(".git/config"), 1024).unwrap().0;
        assert!(!String::from_utf8(config).unwrap().contains("extraheader"));
        assert_eq!(
            command(Some(&checkout), "git", &["rev-parse", "HEAD^{tree}"], 1024).unwrap(),
            format!("{}\n", source.tree).as_bytes()
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn complete_payload_roundtrip_authenticates_real_source_and_retains_tampered_piece_failure() {
        let (_temporary, source) = super::super::tests::fixture();
        let staging = tempfile::tempdir().unwrap();
        let out = staging.path();
        let runtime = out.join("runtime");
        fs::create_dir(&runtime).unwrap();
        fs::create_dir(runtime.join("evidence")).unwrap();
        let (original, _) = observe(&source, Authentication::Materialized).unwrap();
        copy_source(&source, &original, &runtime.join("source")).unwrap();
        reconstruct_git(&source, &runtime.join("source")).unwrap();
        // This is an extraction-mechanics fixture, never a native build receipt.
        write_json(
            &runtime.join("evidence/source-manifest.json"),
            &original,
            MAX_MANIFEST_BYTES,
        )
        .unwrap();
        create_file(
            &runtime.join("evidence/native-build.json"),
            b"{\"fixture_only\":true,\"native_credit\":0}\n",
        )
        .unwrap();
        let payload = payload_inventory(&runtime).unwrap();
        let payload_path = out.join("payload-manifest.json");
        write_json(&payload_path, &payload, MAX_MANIFEST_BYTES).unwrap();
        let archive = out.join("runtime.tar.gz");
        archive_payload(&runtime, &payload, &archive).unwrap();
        let pieces = split_archive(&archive, &out.join("pieces")).unwrap();
        let request = Request {
            schema_version: 1,
            repository: REPOSITORY.to_owned(),
            source_sha: source.sha.clone(),
            source_tree: source.tree.clone(),
            source_manifest_sha256: super::super::sha256(
                &super::super::canonical(&original, MAX_MANIFEST_BYTES).unwrap(),
            ),
            source_owner_receipt_sha256: "4".repeat(64),
        };
        let request_path = out.join("request.json");
        write_json(&request_path, &request, super::super::MAX_REQUEST_BYTES).unwrap();
        let request_sha = digest_file(&request_path, super::super::MAX_REQUEST_BYTES)
            .unwrap()
            .0
            .sha256;
        let metadata = ArtifactMetadata {
            schema_version: 1,
            repository: REPOSITORY.to_owned(),
            source_sha: source.sha,
            source_tree: source.tree,
            source_manifest_sha256: request.source_manifest_sha256.clone(),
            source_owner_receipt_sha256: request.source_owner_receipt_sha256.clone(),
            controller_sha: "6".repeat(40),
            run_id: "1".to_owned(),
            run_attempt: "1".to_owned(),
            runtime_archive: digest_file(&archive, MAX_ARCHIVE_BYTES).unwrap().0,
            payload_manifest: digest_file(&payload_path, MAX_MANIFEST_BYTES).unwrap().0,
            build_evidence: digest_file(
                &runtime.join("evidence/native-build.json"),
                MAX_METADATA_BYTES,
            )
            .unwrap()
            .0,
            piece_manifest: FileDigest {
                bytes: 1,
                sha256: "5".repeat(64),
            },
            pieces,
            unpacked_regular_bytes: payload.unpacked_regular_bytes,
            directory_count: payload.directory_count,
            symlink_count: payload.symlink_count,
            regular_file_count: payload.regular_file_count,
        };
        let metadata_path = out.join("artifact-metadata.json");
        write_json(&metadata_path, &metadata, MAX_METADATA_BYTES).unwrap();
        let metadata_sha = digest_file(&metadata_path, MAX_METADATA_BYTES)
            .unwrap()
            .0
            .sha256;
        extract(ExtractArgs {
            metadata: metadata_path.clone(),
            metadata_sha256: metadata_sha.clone(),
            request: request_path.clone(),
            request_sha256: request_sha.clone(),
            payload_manifest: payload_path.clone(),
            pieces: out.join("pieces"),
            output_dir: out.join("received"),
            receipt: out.join("extraction-receipt.json"),
        })
        .unwrap();
        assert!(out.join("received/runtime/source/README.md").is_file());
        assert!(out.join("received/authenticated-runtime.tar.gz").is_file());
        fs::write(out.join("pieces/000000.part"), b"tampered").unwrap();
        assert!(
            extract(ExtractArgs {
                metadata: metadata_path,
                metadata_sha256: metadata_sha,
                request: request_path,
                request_sha256: request_sha,
                payload_manifest: payload_path,
                pieces: out.join("pieces"),
                output_dir: out.join("received-failure"),
                receipt: out.join("false-pass.json")
            })
            .is_err()
        );
        assert!(out.join("received-failure").is_dir());
        assert!(!out.join("false-pass.json").exists());
        assert_eq!(
            fs::read(out.join("pieces/000000.part")).unwrap(),
            b"tampered"
        );
    }
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
mod descriptor_regression_tests {
    use super::*;

    #[test]
    fn retained_descriptor_copy_and_elf_evidence_stay_bound_when_original_name_is_replaced() {
        let temporary = tempfile::tempdir().unwrap();
        let actual_executable = std::env::current_exe().unwrap();
        let actual_digest = digest_file(&actual_executable, MAX_BINARY_BYTES).unwrap().0;
        let original = temporary.path().join("original");
        copy_verified(&actual_executable, &original, &actual_digest, 0o755).unwrap();
        let held = open_noatime(&original, false).unwrap();
        fs::rename(&original, temporary.path().join("retained-inode")).unwrap();
        fs::write(&original, b"a replacement pathname is not the admitted ELF").unwrap();
        let copied = temporary.path().join("copied");
        let mut source = held.try_clone().unwrap();
        source.seek(SeekFrom::Start(0)).unwrap();
        copy_verified_descriptor(source, &copied, &actual_digest, 0o555).unwrap();
        let copy = open_noatime(&copied, false).unwrap();
        let before = descriptor_digest(&copy, MAX_BINARY_BYTES).unwrap();
        let copied_elf = elf::inspect_file(copy.try_clone().unwrap()).unwrap();
        assert_eq!(copied_elf, elf::inspect(&actual_executable).unwrap());
        assert_eq!(before.0, actual_digest);
        assert_eq!(descriptor_digest(&copy, MAX_BINARY_BYTES).unwrap(), before);
        assert!(
            copy_verified(
                &original,
                &temporary.path().join("wrong-copy"),
                &actual_digest,
                0o755
            )
            .is_err()
        );
        assert!(!temporary.path().join("wrong-copy").exists());
        assert_eq!(
            fs::read(&original).unwrap(),
            b"a replacement pathname is not the admitted ELF"
        );
    }

    #[test]
    fn dependency_drift_fails_before_a_bundle_can_be_reported_delivered() {
        let temporary = tempfile::tempdir().unwrap();
        let actual_executable = std::env::current_exe().unwrap();
        let actual_digest = digest_file(&actual_executable, MAX_BINARY_BYTES).unwrap().0;
        let original = temporary.path().join("library");
        copy_verified(&actual_executable, &original, &actual_digest, 0o755).unwrap();
        let file = open_noatime(&original, false).unwrap();
        let (digest, identity) = descriptor_digest(&file, MAX_BINARY_BYTES).unwrap();
        let preliminary_elf = elf::inspect_file(file.try_clone().unwrap()).unwrap();
        let planned = PlannedDependency {
            file,
            soname: "libfixture.so".to_owned(),
            loader_path: original.clone(),
            resolved_path: original.clone(),
            digest,
            physical: identity,
            preliminary_elf: elf::SharedEvidence::new(
                preliminary_elf,
                &mut elf::MetadataBudget::new(),
            )
            .unwrap(),
        };
        fs::write(&original, b"changed after dependency admission").unwrap();
        let directory = temporary.path().join("bundle");
        fs::create_dir(&directory).unwrap();
        assert!(deliver_dependencies(&[planned], &directory).is_err());
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        assert_eq!(
            fs::read(&original).unwrap(),
            b"changed after dependency admission"
        );
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn actual_loader_resolves_only_retained_dependency_descriptors() {
        let temporary = tempfile::tempdir().unwrap();
        let original = std::env::current_exe().unwrap();
        let source = open_noatime(&original, false).unwrap();
        let facts = elf::inspect_file(source.try_clone().unwrap()).unwrap();
        assert!(facts.rpath.is_none() && facts.runpath.is_none());
        // Fixture preparation copies official runtime libraries into test-owned
        // files. Production O_NOATIME policy is unchanged and no test needs root
        // privileges. These ordinary fixture reads may observe host-library atime
        // changes; no timestamp is restored and no source identity credit is taken.
        use std::os::fd::AsRawFd as _;
        let interpreter = facts.interpreter.as_deref().unwrap();
        let binary_fd = format!("/proc/{}/fd/{}", std::process::id(), source.as_raw_fd());
        let initial = String::from_utf8(
            command(None, interpreter, &["--list", &binary_fd], 256 * 1024).unwrap(),
        )
        .unwrap();
        let fixture_libraries = temporary.path().join("fixture-libraries");
        fs::create_dir(&fixture_libraries).unwrap();
        let mut plan = Vec::new();
        for line in initial.lines().map(str::trim) {
            if line.is_empty() || line.starts_with("linux-vdso.so.1 (") {
                continue;
            }
            let (soname, loader_path) = match line.split_once(" => ") {
                Some((name, right)) => (name, right.split_once(" (").unwrap().0),
                None => ("ld-linux-x86-64.so.2", line.split_once(" (").unwrap().0),
            };
            let original = resolve_noatime(Path::new(loader_path)).unwrap();
            let fixture_path = fixture_libraries.join(soname);
            fs::copy(&original, &fixture_path).unwrap();
            let file = open_noatime(&fixture_path, false).unwrap();
            let (digest, physical) = descriptor_digest(&file, MAX_BINARY_BYTES).unwrap();
            let preliminary_elf = elf::inspect_file(file.try_clone().unwrap()).unwrap();
            plan.push(PlannedDependency {
                file,
                soname: soname.to_owned(),
                loader_path: PathBuf::from(loader_path),
                resolved_path: fixture_path,
                digest,
                physical,
                preliminary_elf: elf::SharedEvidence::new(
                    preliminary_elf,
                    &mut elf::MetadataBudget::new(),
                )
                .unwrap(),
            });
        }
        assert!(!plan.is_empty() && plan.len() <= 256);
        let directory = temporary.path().join("lib");
        fs::create_dir(&directory).unwrap();
        deliver_dependencies(&plan, &directory).unwrap();
        let probe = temporary.path().join("descriptor-probe");
        let receipt = verify_bound_loader(&source, &facts, &plan, &directory, &probe).unwrap();
        assert!(!receipt.constructors_entered);
        assert_eq!(receipt.runtime_compatibility_or_test_acceptance_credit, 0);
        assert_eq!(receipt.dependencies.len(), plan.len());
        for row in receipt.dependencies {
            assert!(row.actual_descriptor_target.starts_with("/proc/"));
            require_loader_inverse(&row.before_loader, &row.after_loader).unwrap();
        }
        assert!(!probe.exists());
    }
}

#[cfg(test)]
mod round2_budget_regressions {
    use super::*;

    #[test]
    fn bounded_loader_paths_charge_the_shared_budget_before_owned_resolution_allocations() {
        let facts = elf::ElfEvidence {
            class: "ELF64",
            endian: "little",
            machine: 62,
            file_type: 3,
            interpreter: None,
            soname: None,
            needed: Vec::new(),
            rpath: None,
            runpath: None,
            version_requirements: Vec::new(),
            version_definitions: Vec::new(),
        };
        let path = format!("/{}", "x".repeat(super::super::MAX_PATH_BYTES - 1));
        let text = format!("libfixture.so => {path} (0x1)\n");
        let mut budget = elf::MetadataBudget::limited(4096);
        assert!(
            parse_loader_paths(&text, "/lib64/ld-linux-x86-64.so.2", &facts, &mut budget).is_err()
        );
        assert_eq!(budget.charged(), 0);
        assert_eq!(
            budget.rejected_phase(),
            Some("loader resolution strings/map growth")
        );
        let mut permitted = elf::MetadataBudget::new();
        let actual =
            parse_loader_paths(&text, "/lib64/ld-linux-x86-64.so.2", &facts, &mut permitted)
                .unwrap();
        assert_eq!(actual.get("libfixture.so").unwrap(), &PathBuf::from(&path));
        assert!(permitted.charged() > 4096);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn multiple_retained_dependencies_and_actual_delivered_copies_share_one_elf_budget() {
        let temporary = tempfile::tempdir().unwrap();
        let fixture = temporary.path().join("fixture.so");
        let bytes = elf::valid_repeated_version_fixture(64, 4096);
        fs::write(&fixture, &bytes).unwrap();
        let mut measured = elf::MetadataBudget::new();
        let facts =
            elf::inspect_file_with_budget(open_noatime(&fixture, false).unwrap(), &mut measured)
                .unwrap();
        assert_eq!(facts.version_definitions.len(), 64);
        let one = measured.charged();
        let cap = one * 3 + one / 2 + 64 * 1024;
        let mut budget = elf::MetadataBudget::limited(cap);
        let mut planned = Vec::new();
        for soname in ["libfixture-one.so", "libfixture-two.so"] {
            let file = open_noatime(&fixture, false).unwrap();
            let (digest, physical) = descriptor_digest(&file, MAX_BINARY_BYTES).unwrap();
            let actual =
                elf::inspect_file_with_budget(file.try_clone().unwrap(), &mut budget).unwrap();
            assert_eq!(actual, facts);
            let preliminary_elf = elf::SharedEvidence::new(actual, &mut budget).unwrap();
            planned.push(PlannedDependency {
                file,
                soname: soname.to_owned(),
                loader_path: fixture.clone(),
                resolved_path: fixture.clone(),
                digest,
                physical,
                preliminary_elf,
            });
        }
        let before_delivery = budget.charged();
        assert!(before_delivery >= 2 * one);
        let directory = temporary.path().join("bundle");
        fs::create_dir(&directory).unwrap();
        assert!(deliver_dependencies_with_budget(&planned, &directory, &mut budget).is_err());
        assert!(budget.charged() <= cap && budget.charged() > before_delivery);
        assert!(budget.rejected_phase().is_some());
        // The genuine partial copies remain as failure evidence; no delivered
        // vector or successful package receipt is returned after budget failure.
        assert_eq!(
            fs::read(directory.join("libfixture-one.so")).unwrap(),
            bytes
        );
        assert_eq!(
            fs::read(directory.join("libfixture-two.so")).unwrap(),
            bytes
        );
        assert_eq!(read_bounded(&fixture, bytes.len() as u64).unwrap().0, bytes);
        // The same genuine files succeed with the normal shared budget. This
        // tests that aggregate failure is admission-specific and exercises the
        // Linux convenience wrapper on every Linux test architecture.
        let admitted = temporary.path().join("admitted-bundle");
        fs::create_dir(&admitted).unwrap();
        let delivered = deliver_dependencies(&planned, &admitted).unwrap();
        assert_eq!(delivered.len(), 2);
        assert!(
            delivered
                .iter()
                .all(|entry| entry.elf.version_definitions.len() == 64)
        );
        assert_eq!(fs::read(admitted.join("libfixture-one.so")).unwrap(), bytes);
        assert_eq!(fs::read(admitted.join("libfixture-two.so")).unwrap(), bytes);
        for plan in planned {
            assert_eq!(
                descriptor_digest(&plan.file, MAX_BINARY_BYTES).unwrap(),
                (plan.digest, plan.physical)
            );
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod gnu_long_symlink_roundtrip_regression {
    use super::*;

    #[test]
    fn actual_tracked_long_link_and_source_path_survive_producer_and_cli_extraction() {
        let (_temporary, mut source) = super::super::tests::fixture();
        let component = "long-".repeat(40);
        let target = format!("{component}/target.txt");
        assert!(target.len() > 100 && target.len() <= super::super::MAX_PATH_BYTES);
        fs::create_dir(source.source.join(&component)).unwrap();
        fs::write(
            source.source.join(&target),
            b"actual tracked long-link target\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(&target, source.source.join("LONG-LINK")).unwrap();
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
                "tracked GNU long-link fixture",
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
        let (original, _) = observe(&source, Authentication::Materialized).unwrap();
        let link = original
            .entries
            .iter()
            .find(|entry| entry.path == "LONG-LINK")
            .unwrap();
        assert_eq!(link.mode, "120000");
        assert_eq!(link.link_target.as_deref(), Some(target.as_str()));
        let staging = tempfile::tempdir().unwrap();
        let out = staging.path();
        let runtime = out.join("runtime");
        fs::create_dir(&runtime).unwrap();
        fs::create_dir(runtime.join("evidence")).unwrap();
        copy_source(&source, &original, &runtime.join("source")).unwrap();
        reconstruct_git(&source, &runtime.join("source")).unwrap();
        write_json(
            &runtime.join("evidence/source-manifest.json"),
            &original,
            MAX_MANIFEST_BYTES,
        )
        .unwrap();
        create_file(
            &runtime.join("evidence/native-build.json"),
            b"{\"fixture_only\":true,\"native_credit\":0}\n",
        )
        .unwrap();
        let payload = payload_inventory(&runtime).unwrap();
        let payload_path = out.join("payload-manifest.json");
        write_json(&payload_path, &payload, MAX_MANIFEST_BYTES).unwrap();
        let archive = out.join("runtime.tar.gz");
        archive_payload(&runtime, &payload, &archive).unwrap();
        let pieces = split_archive(&archive, &out.join("pieces")).unwrap();
        let request = Request {
            schema_version: 1,
            repository: REPOSITORY.to_owned(),
            source_sha: source.sha.clone(),
            source_tree: source.tree.clone(),
            source_manifest_sha256: super::super::sha256(
                &super::super::canonical(&original, MAX_MANIFEST_BYTES).unwrap(),
            ),
            source_owner_receipt_sha256: "4".repeat(64),
        };
        let request_path = out.join("request.json");
        write_json(&request_path, &request, super::super::MAX_REQUEST_BYTES).unwrap();
        let request_sha = digest_file(&request_path, super::super::MAX_REQUEST_BYTES)
            .unwrap()
            .0
            .sha256;
        let metadata = ArtifactMetadata {
            schema_version: 1,
            repository: REPOSITORY.to_owned(),
            source_sha: source.sha,
            source_tree: source.tree,
            source_manifest_sha256: request.source_manifest_sha256.clone(),
            source_owner_receipt_sha256: request.source_owner_receipt_sha256.clone(),
            controller_sha: "6".repeat(40),
            run_id: "1".to_owned(),
            run_attempt: "1".to_owned(),
            runtime_archive: digest_file(&archive, MAX_ARCHIVE_BYTES).unwrap().0,
            payload_manifest: digest_file(&payload_path, MAX_MANIFEST_BYTES).unwrap().0,
            build_evidence: digest_file(
                &runtime.join("evidence/native-build.json"),
                MAX_METADATA_BYTES,
            )
            .unwrap()
            .0,
            piece_manifest: FileDigest {
                bytes: 1,
                sha256: "5".repeat(64),
            },
            pieces,
            unpacked_regular_bytes: payload.unpacked_regular_bytes,
            directory_count: payload.directory_count,
            symlink_count: payload.symlink_count,
            regular_file_count: payload.regular_file_count,
        };
        let metadata_path = out.join("artifact-metadata.json");
        write_json(&metadata_path, &metadata, MAX_METADATA_BYTES).unwrap();
        let metadata_sha = digest_file(&metadata_path, MAX_METADATA_BYTES)
            .unwrap()
            .0
            .sha256;
        extract(ExtractArgs {
            metadata: metadata_path,
            metadata_sha256: metadata_sha,
            request: request_path,
            request_sha256: request_sha,
            payload_manifest: payload_path,
            pieces: out.join("pieces"),
            output_dir: out.join("received"),
            receipt: out.join("extraction-receipt.json"),
        })
        .unwrap();
        let received = out.join("received/runtime/source");
        assert_eq!(
            fs::read_link(received.join("LONG-LINK")).unwrap(),
            PathBuf::from(&target)
        );
        assert_eq!(
            fs::read(received.join("LONG-LINK")).unwrap(),
            b"actual tracked long-link target\n"
        );
        assert_eq!(
            fs::read(received.join(&target)).unwrap(),
            b"actual tracked long-link target\n"
        );
        assert!(out.join("extraction-receipt.json").is_file());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod owned_symlink_initialization_regression {
    use super::*;

    #[test]
    fn newly_created_links_are_initialized_before_frozen_full13_observations() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let target_path = root.join("target.txt");
        fs::write(&target_path, b"owned link target remains untouched\n").unwrap();
        let target_before = physical(&fs::metadata(&target_path).unwrap()).unwrap();
        let long_target = format!("{}missing.txt", "missing-component/".repeat(12));
        for (name, target) in [
            ("short-link", "target.txt"),
            ("long-link", long_target.as_str()),
        ] {
            let path = root.join(name);
            create_symlink(target, &path).unwrap();
            let before = physical(&fs::symlink_metadata(&path).unwrap()).unwrap();
            for _ in 0..3 {
                assert_eq!(host_link(&path).unwrap(), target.as_bytes());
                assert_eq!(
                    physical(&fs::symlink_metadata(&path).unwrap()).unwrap(),
                    before
                );
                let directory = open_noatime(root, true).unwrap();
                let file = open_beneath(&directory, name, true).unwrap();
                assert_eq!(link_bytes(&file).unwrap(), target.as_bytes());
                assert_eq!(physical(&file.metadata().unwrap()).unwrap(), before);
                assert_eq!(
                    physical(&fs::symlink_metadata(&path).unwrap()).unwrap(),
                    before
                );
            }
            assert!(create_symlink("replacement-target", &path).is_err());
            assert_eq!(host_link(&path).unwrap(), target.as_bytes());
            assert_eq!(
                physical(&fs::symlink_metadata(&path).unwrap()).unwrap(),
                before
            );
        }
        assert_eq!(
            physical(&fs::metadata(&target_path).unwrap()).unwrap(),
            target_before
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod owned_checkout_lifecycle_regression {
    use super::*;

    fn cold_fixture_link(source: &SourceArgs) -> Physical13 {
        use std::os::unix::ffi::OsStrExt as _;
        let path = source.source.join("AGENTS.md");
        // This fixture owns its new link. An old timestamp establishes the
        // cold-link precondition before capture; no observed timestamp is restored.
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("README.md", &path).unwrap();
        let path_bytes = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let timestamps = [
            libc::timespec {
                tv_sec: 1,
                tv_nsec: 0,
            },
            libc::timespec {
                tv_sec: 2,
                tv_nsec: 0,
            },
        ];
        // SAFETY: terminated owned fixture path and two initialized timespecs;
        // AT_SYMLINK_NOFOLLOW initializes the test link, never its target.
        let result = unsafe {
            libc::utimensat(
                libc::AT_FDCWD,
                path_bytes.as_ptr(),
                timestamps.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        assert_eq!(result, 0);
        physical(&fs::symlink_metadata(path).unwrap()).unwrap()
    }

    #[test]
    fn constructor_owns_fresh_exact_modes_and_links_without_initializing_borrowed_source() {
        let (_borrowed, source) = super::super::tests::fixture();
        let cold_before = cold_fixture_link(&source);
        let (expected, _) = observe(&source, Authentication::GitObjects).unwrap();
        let staging = tempfile::tempdir().unwrap();
        let destination = staging.path().join("new-checkout");
        let receipt = prepare_owned_checkout(&source, &destination).unwrap();
        assert_eq!(
            physical(&fs::symlink_metadata(source.source.join("AGENTS.md")).unwrap()).unwrap(),
            cold_before
        );
        assert_eq!(receipt.source_commit, source.sha);
        assert_eq!(receipt.source_tree, source.tree);
        assert_eq!(receipt.initialized_new_symlinks, 1);
        assert_eq!(receipt.strict_materialized_entries, expected.entries.len());
        assert!(receipt.complete_source_full13_inverses_equal);
        assert!(receipt.borrowed_materialized_source_was_not_initialized);
        assert!(receipt.first_constructor_tool_is_unaccepted);
        assert_eq!(receipt.runtime_test_or_reference_credit, 0);
        assert_eq!(
            receipt.original_filesystem_floor_bytes,
            super::super::ORIGINAL_FLOOR
        );
        let owned = SourceArgs {
            source: destination.clone(),
            sha: source.sha.clone(),
            tree: source.tree.clone(),
        };
        let initialized_link =
            physical(&fs::symlink_metadata(destination.join("AGENTS.md")).unwrap()).unwrap();
        let (actual, observation) = observe(&owned, Authentication::Materialized).unwrap();
        assert_eq!(actual, expected);
        assert!(observation.full_physical_noatime_inverses_equal);
        assert_eq!(observation.runtime_test_or_reference_credit, 0);
        assert_eq!(
            physical(&fs::symlink_metadata(destination.join("AGENTS.md")).unwrap()).unwrap(),
            initialized_link
        );
        assert_eq!(
            actual
                .entries
                .iter()
                .find(|e| e.path == "script")
                .unwrap()
                .mode,
            "100755"
        );
        assert_eq!(
            actual
                .entries
                .iter()
                .find(|e| e.path == "empty")
                .unwrap()
                .bytes,
            0
        );
        assert_eq!(
            actual
                .entries
                .iter()
                .find(|e| e.path == "AGENTS.md")
                .unwrap()
                .mode,
            "120000"
        );
        assert_eq!(
            physical(&fs::symlink_metadata(source.source.join("AGENTS.md")).unwrap()).unwrap(),
            cold_before
        );
        assert!(!destination.join(".git/hooks").exists());
        assert!(!destination.join(".git/objects/info/alternates").exists());
    }

    #[test]
    fn borrowed_cold_source_still_fails_the_complete_physical_atime_inverse() {
        let (_borrowed, source) = super::super::tests::fixture();
        let cold_before = cold_fixture_link(&source);
        let error = match observe(&source, Authentication::Materialized) {
            Ok(_) => panic!("cold borrowed symlink must not be silently initialized"),
            Err(error) => error,
        };
        let message = format!("{error:#}");
        assert!(message.contains("physical input changed during read for AGENTS.md"));
        assert!(message.contains("atime_ns"));
        let after =
            physical(&fs::symlink_metadata(source.source.join("AGENTS.md")).unwrap()).unwrap();
        assert_eq!(cold_before.changed_fields(&after), vec!["atime_ns"]);
    }

    #[test]
    fn constructor_refuses_existing_directories_and_symlinks_without_relabeling_them() {
        let (_borrowed, source) = super::super::tests::fixture();
        let cold_before = cold_fixture_link(&source);
        let staging = tempfile::tempdir().unwrap();
        let directory = staging.path().join("existing");
        fs::create_dir(&directory).unwrap();
        create_file(
            &directory.join("sentinel"),
            b"existing directory is borrowed",
        )
        .unwrap();
        let before = physical(&fs::symlink_metadata(&directory).unwrap()).unwrap();
        let error = prepare_owned_checkout(&source, &directory).unwrap_err();
        assert!(format!("{error:#}").contains("owned checkout directory must be new"));
        assert_eq!(
            physical(&fs::symlink_metadata(&directory).unwrap()).unwrap(),
            before
        );
        assert!(!directory.join(".git").exists());
        let link = staging.path().join("existing-link");
        std::os::unix::fs::symlink(&source.source, &link).unwrap();
        let link_before = physical(&fs::symlink_metadata(&link).unwrap()).unwrap();
        assert!(prepare_owned_checkout(&source, &link).is_err());
        assert_eq!(
            physical(&fs::symlink_metadata(&link).unwrap()).unwrap(),
            link_before
        );
        assert_eq!(
            physical(&fs::symlink_metadata(source.source.join("AGENTS.md")).unwrap()).unwrap(),
            cold_before
        );
    }

    #[test]
    fn retained_creator_capability_rejects_replaced_root_instead_of_claiming_existing_path() {
        let (_borrowed, source) = super::super::tests::fixture();
        let staging = tempfile::tempdir().unwrap();
        let path = staging.path().join("checkout");
        let owned = OwnedCheckout::create(&source, &path).unwrap();
        owned.require_owned_path().unwrap();
        fs::rename(&path, staging.path().join("retained-original")).unwrap();
        fs::create_dir(&path).unwrap();
        let replacement = physical(&fs::symlink_metadata(&path).unwrap()).unwrap();
        assert_ne!(replacement.ino, owned.created_root.ino);
        let error = owned.require_owned_path().unwrap_err();
        assert!(format!("{error:#}").contains("newly created checkout root was replaced"));
        assert_eq!(
            physical(&fs::symlink_metadata(&path).unwrap()).unwrap(),
            replacement
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod owned_checkout_descriptor_boundary_regression {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn writable_creation_parent_is_refused_before_any_new_directory_or_inode_claim() {
        let (_source_holder, source) = super::super::tests::fixture();
        let staging = tempfile::tempdir().unwrap();
        let parent = staging.path().join("unsafe-parent");
        fs::create_dir(&parent).unwrap();
        for mode in [0o770, 0o777] {
            fs::set_permissions(&parent, fs::Permissions::from_mode(mode)).unwrap();
            let before = physical(&fs::symlink_metadata(&parent).unwrap()).unwrap();
            let destination = parent.join("new-root");
            let error = match OwnedCheckout::create(&source, &destination) {
                Ok(_) => panic!("a writable parent cannot authorize new-root ownership"),
                Err(error) => error,
            };
            assert!(format!("{error:#}").contains("not group/other writable"));
            assert!(!destination.exists());
            assert_eq!(
                physical(&fs::symlink_metadata(&parent).unwrap()).unwrap(),
                before
            );
        }
    }

    #[test]
    fn parent_replacement_after_capability_capture_cannot_redirect_root_creation() {
        let (_source_holder, source) = super::super::tests::fixture();
        let staging = tempfile::tempdir().unwrap();
        let parent_path = staging.path().join("trusted-parent");
        fs::create_dir(&parent_path).unwrap();
        fs::set_permissions(&parent_path, fs::Permissions::from_mode(0o700)).unwrap();
        let parent = TrustedCreationParent::open(&parent_path).unwrap();
        let destination = parent_path.join("new-root");
        fs::rename(&parent_path, staging.path().join("retained-parent")).unwrap();
        fs::create_dir(&parent_path).unwrap();
        let replacement = physical(&fs::symlink_metadata(&parent_path).unwrap()).unwrap();
        let error = match OwnedCheckout::create_beneath(&source, &destination, parent) {
            Ok(_) => panic!("a replaced parent pathname must not be admitted"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("trusted checkout creation parent was replaced"));
        assert!(!destination.exists());
        assert!(!staging.path().join("retained-parent/new-root").exists());
        assert_eq!(
            physical(&fs::symlink_metadata(&parent_path).unwrap()).unwrap(),
            replacement
        );
    }

    #[test]
    fn root_replacement_between_parent_capture_and_file_write_keeps_borrowed_tree_untouched() {
        let (_source_holder, source) = super::super::tests::fixture();
        let staging = tempfile::tempdir().unwrap();
        let path = staging.path().join("checkout");
        let mut owned = OwnedCheckout::create(&source, &path).unwrap();
        owned.require_owned_path().unwrap();
        let (captured, leaf) = owned.parent_for("output.rs").unwrap();
        let retained = staging.path().join("retained-original");
        fs::rename(&path, &retained).unwrap();
        fs::create_dir(&path).unwrap();
        let sentinel = path.join("sentinel");
        create_file(&sentinel, b"borrowed sentinel").unwrap();
        let replacement_before = physical(&fs::symlink_metadata(&path).unwrap()).unwrap();
        let sentinel_before = physical(&fs::symlink_metadata(&sentinel).unwrap()).unwrap();
        // A deliberate same-user test actor inserts the old check/write race.
        // The write itself uses the previously captured directory descriptor.
        let mut output = create_file_at(&captured, &leaf).unwrap();
        output
            .write_all(b"created inode remains under the retained root")
            .unwrap();
        output.sync_all().unwrap();
        drop(output);
        assert!(retained.join("output.rs").is_file());
        assert!(!path.join("output.rs").exists());
        assert!(owned.require_owned_path().is_err());
        assert_eq!(
            physical(&fs::symlink_metadata(&path).unwrap()).unwrap(),
            replacement_before
        );
        assert_eq!(
            physical(&fs::symlink_metadata(&sentinel).unwrap()).unwrap(),
            sentinel_before
        );
    }

    #[test]
    fn descendant_replacement_cannot_redirect_captured_writes_or_be_adopted_for_later_writes() {
        let (_source_holder, source) = super::super::tests::fixture();
        let staging = tempfile::tempdir().unwrap();
        let path = staging.path().join("checkout");
        let mut owned = OwnedCheckout::create(&source, &path).unwrap();
        let (captured, leaf) = owned.parent_for("src/output.rs").unwrap();
        fs::rename(path.join("src"), path.join("retained-src")).unwrap();
        fs::create_dir(path.join("src")).unwrap();
        let borrowed_before = physical(&fs::symlink_metadata(path.join("src")).unwrap()).unwrap();
        let mut output = create_file_at(&captured, &leaf).unwrap();
        output.write_all(b"retained descendant output").unwrap();
        output.sync_all().unwrap();
        drop(output);
        assert!(path.join("retained-src/output.rs").is_file());
        assert!(!path.join("src/output.rs").exists());
        let error = owned
            .write_file("src/later.rs", b"must be refused", 0o644)
            .unwrap_err();
        assert!(format!("{error:#}").contains("owned descendant directory was replaced"));
        assert!(!path.join("src/later.rs").exists());
        assert_eq!(
            physical(&fs::symlink_metadata(path.join("src")).unwrap()).unwrap(),
            borrowed_before
        );
    }

    #[test]
    fn replaced_receipt_parent_is_refused_without_creating_a_file_in_the_borrowed_parent() {
        let (_source_holder, source) = super::super::tests::fixture();
        let staging = tempfile::tempdir().unwrap();
        let parent = staging.path().join("parent");
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        let receipt = prepare_owned_checkout(&source, &parent.join("checkout")).unwrap();
        fs::rename(&parent, staging.path().join("retained-parent")).unwrap();
        fs::create_dir(&parent).unwrap();
        let before = physical(&fs::symlink_metadata(&parent).unwrap()).unwrap();
        let output = parent.join("receipt.json");
        let error = receipt.write_new_receipt(&output).unwrap_err();
        assert!(format!("{error:#}").contains("receipt creation parent was replaced"));
        assert!(!output.exists());
        assert_eq!(
            physical(&fs::symlink_metadata(&parent).unwrap()).unwrap(),
            before
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod ancestor_directory_capability_regression {
    use super::*;
    use std::os::{fd::AsRawFd as _, unix::fs::PermissionsExt as _};

    fn descriptor_flags(file: &File) -> i32 {
        // SAFETY: F_GETFL only queries the live descriptor; it changes no flags.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        flags
    }

    #[test]
    fn system_ancestors_use_path_capabilities_and_owned_descendants_remain_readable() {
        // This must work with a normal runner UID that does not own "/".
        // Checking O_PATH also detects the old privileged-only approach when
        // the test runner happens to own the root or possess CAP_FOWNER.
        let root = open_absolute_directory(Path::new("/")).unwrap();
        let flags = descriptor_flags(&root);
        assert_ne!(flags & libc::O_PATH, 0);
        assert_ne!(flags & libc::O_DIRECTORY, 0);
        assert_ne!(flags & libc::O_NOFOLLOW, 0);
        assert_eq!(flags & libc::O_NOATIME, 0);
        assert!(root.metadata().unwrap().is_dir());
        let temporary = tempfile::tempdir().unwrap();
        let parent_path = temporary.path().canonicalize().unwrap();
        let parent = TrustedCreationParent::open(&parent_path).unwrap();
        assert_ne!(descriptor_flags(&parent.file) & libc::O_PATH, 0);
        parent.require_current().unwrap();
        let source = SourceArgs {
            source: parent_path.join("borrowed-source-not-read"),
            sha: "0".repeat(40),
            tree: "1".repeat(40),
        };
        let destination = parent_path.join("new-checkout");
        let mut owned = OwnedCheckout::create_beneath(&source, &destination, parent).unwrap();
        assert_eq!(descriptor_flags(&owned.root) & libc::O_PATH, 0);
        assert_ne!(descriptor_flags(&owned.root) & libc::O_NOATIME, 0);
        owned.root.sync_all().unwrap();
        owned
            .write_file("src/output.rs", b"retained owned write", 0o644)
            .unwrap();
        owned.require_owned_path().unwrap();
        assert_eq!(
            fs::read(destination.join("src/output.rs")).unwrap(),
            b"retained owned write"
        );
        assert_eq!(
            fs::metadata(destination.join("src/output.rs"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
    }

    #[test]
    fn path_capability_traversal_refuses_symlink_ancestors_without_reading_the_link() {
        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().canonicalize().unwrap();
        let target = parent.join("target");
        fs::create_dir(&target).unwrap();
        fs::create_dir(target.join("nested")).unwrap();
        let link = parent.join("alias");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let link_before = physical(&fs::symlink_metadata(&link).unwrap()).unwrap();
        let target_before = physical(&fs::symlink_metadata(&target).unwrap()).unwrap();
        assert!(open_absolute_directory(&link).is_err());
        assert!(open_absolute_directory(&link.join("nested")).is_err());
        assert!(TrustedCreationParent::open(&link).is_err());
        assert_eq!(
            physical(&fs::symlink_metadata(&link).unwrap()).unwrap(),
            link_before
        );
        assert_eq!(
            physical(&fs::symlink_metadata(&target).unwrap()).unwrap(),
            target_before
        );
    }
}

#[cfg(target_os = "linux")]
fn borrowed_receipt_reserve(block: u64, count: u64) -> Result<(u64, u64)> {
    ensure!(
        block > 0 && block <= 1024 * 1024 && (1..=2).contains(&count),
        "invalid borrowed receipt block size or pending count"
    );
    let bytes = MAX_MANIFEST_BYTES
        .checked_mul(count)
        .and_then(|bytes| {
            block
                .checked_mul(count)
                .and_then(|padding| bytes.checked_add(padding))
        })
        .context("borrowed receipt reserve overflow")?;
    Ok((bytes, count))
}

#[cfg(target_os = "linux")]
fn require_borrowed_receipt_capacity(
    reserve: (u64, u64),
    available: u64,
    available_inodes: u64,
) -> Result<()> {
    let bytes = reserve
        .0
        .checked_add(super::ORIGINAL_FLOOR)
        .context("borrowed receipt floor overflow")?;
    // Keep the existing 32-inode headroom in addition to both new diagnostics.
    let inodes = reserve
        .1
        .checked_add(32)
        .context("borrowed receipt inode reserve overflow")?;
    ensure!(
        available >= bytes && available_inodes >= inodes,
        "borrowed receipt disk/inode reserve does not fit before creation"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct BorrowedPhysicalEntry {
    path: String,
    identity: Option<Physical13>,
}

#[cfg(target_os = "linux")]
struct BorrowedPhysicalGuard {
    source: SourceArgs,
    root: File,
    root_identity: Physical13,
    paths: Vec<String>,
    before: Vec<BorrowedPhysicalEntry>,
    canonical_manifest: FileDigest,
    output_parent: TrustedCreationParent,
    pending_receipts: std::cell::Cell<u64>,
    #[cfg(test)]
    receipt_capacity_ceiling: Option<(u64, u64)>,
}

#[cfg(target_os = "linux")]
fn borrowed_metadata_at(root: &File, path: &str) -> Result<Option<Physical13>> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    if path.is_empty() {
        return Ok(Some(physical(&root.metadata()?)?));
    }
    payload_relative(path)?;
    let parts: Vec<_> = path.split('/').collect();
    let mut directory = root.try_clone()?;
    for (index, part) in parts.iter().enumerate() {
        let name = std::ffi::CString::new(*part)?;
        let mut flags = libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        if index + 1 != parts.len() {
            flags |= libc::O_DIRECTORY;
        }
        // SAFETY: retained directory FD and terminated confined component.
        // O_PATH observes metadata without reading regular files or cold links;
        // intermediate symlinks are refused, and no inode is claimed as owned.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOENT) {
                return Ok(None);
            }
            return Err(error).with_context(|| format!("borrowed metadata component: {path}"));
        }
        // SAFETY: successful openat transferred a new descriptor.
        directory = unsafe { File::from_raw_fd(fd) };
    }
    Ok(Some(physical(&directory.metadata()?)?))
}

#[cfg(target_os = "linux")]
impl BorrowedPhysicalGuard {
    fn capture(source: &SourceArgs, output_parent: TrustedCreationParent) -> Result<Self> {
        let root = open_absolute_directory(&source.source)?;
        let root_identity = physical(&root.metadata()?)?;
        let (manifest, observation) = observe(source, Authentication::GitObjects)?;
        let mut paths = BTreeSet::new();
        let mut path_bytes = 0_u64;
        let mut insert = |path: String| -> Result<()> {
            if !paths.contains(&path) {
                ensure!(
                    paths.len() < super::MAX_ENTRIES * 2 + 1,
                    "borrowed metadata entry budget exhausted"
                );
                let next = path_bytes
                    .checked_add(path.len() as u64)
                    .context("borrowed metadata path byte overflow")?;
                ensure!(
                    next <= MAX_MANIFEST_BYTES / 4,
                    "borrowed metadata path byte budget exhausted"
                );
                paths.insert(path);
                path_bytes = next;
            }
            Ok(())
        };
        insert(String::new())?;
        for entry in &manifest.entries {
            insert(entry.path.clone())?;
            let mut parent = Path::new(&entry.path).parent();
            while let Some(path) = parent.filter(|path| !path.as_os_str().is_empty()) {
                insert(
                    path.to_str()
                        .context("non-UTF8 borrowed parent")?
                        .to_owned(),
                )?;
                parent = path.parent();
            }
        }
        let mut guard = Self {
            source: source.clone(),
            root,
            root_identity,
            paths: paths.into_iter().collect(),
            before: Vec::new(),
            canonical_manifest: observation.canonical_manifest,
            output_parent,
            pending_receipts: std::cell::Cell::new(2),
            #[cfg(test)]
            receipt_capacity_ceiling: None,
        };
        let before = guard.snapshot()?;
        ensure!(
            before == guard.snapshot()?,
            "borrowed metadata changed during initial capture"
        );
        guard.before = before;
        Ok(guard)
    }

    fn require_root(&self) -> Result<()> {
        ensure!(
            self.root_identity == physical(&self.root.metadata()?)?
                && self.root_identity
                    == physical(&open_absolute_directory(&self.source.source)?.metadata()?)?,
            "cold borrowed source root changed"
        );
        Ok(())
    }

    fn snapshot(&self) -> Result<Vec<BorrowedPhysicalEntry>> {
        self.require_root()?;
        let entries = self
            .paths
            .iter()
            .map(|path| {
                Ok(BorrowedPhysicalEntry {
                    path: path.clone(),
                    identity: borrowed_metadata_at(&self.root, path)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        self.require_root()?;
        Ok(entries)
    }

    fn receipt_filesystem_space(&self) -> Result<(u64, u64, u64)> {
        self.output_parent.require_current()?;
        // Every receipt is a sibling below this retained protected parent.
        // Sample its filesystem capability, never the borrowed source's volume
        // or a public pathname that could refer to a different filesystem.
        let (block, available, available_inodes) =
            super::filesystem_space(&descriptor_path(&self.output_parent.file))?;
        self.output_parent.require_current()?;
        #[cfg(test)]
        let (available, available_inodes) = match self.receipt_capacity_ceiling {
            Some((bytes, inodes)) => (available.min(bytes), available_inodes.min(inodes)),
            None => (available, available_inodes),
        };
        Ok((block, available, available_inodes))
    }

    fn pending_receipt_reserve(&self, block: u64) -> Result<(u64, u64)> {
        borrowed_receipt_reserve(block, self.pending_receipts.get())
    }

    fn require_new_receipt_names(&self, paths: &[&Path]) -> Result<()> {
        use std::os::{fd::AsRawFd as _, unix::ffi::OsStrExt as _};
        self.output_parent.require_current()?;
        for path in paths {
            ensure!(
                path.parent() == Some(self.output_parent.path.as_path()),
                "borrowed receipt parent differs"
            );
            let name = std::ffi::CString::new(
                path.file_name()
                    .context("borrowed receipt name absent")?
                    .as_bytes(),
            )?;
            let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: retained parent and terminated one-component name, with
            // correctly sized output. No link is followed or read. O_EXCL in
            // the actual writer remains authoritative if a name later appears.
            let status = unsafe {
                libc::fstatat(
                    self.output_parent.file.as_raw_fd(),
                    name.as_ptr(),
                    metadata.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if status == 0 {
                bail!("borrowed receipt output must be new");
            }
            let error = std::io::Error::last_os_error();
            ensure!(
                error.raw_os_error() == Some(libc::ENOENT),
                "borrowed receipt output lookup failed: {error}"
            );
        }
        self.output_parent.require_current()?;
        Ok(())
    }

    fn write_capture(
        &self,
        path: &Path,
        state: &'static str,
        entries: Option<&[BorrowedPhysicalEntry]>,
        complete_inverse_equal: bool,
        actual_error: Option<String>,
    ) -> Result<()> {
        use std::os::unix::ffi::OsStrExt as _;
        #[derive(Serialize)]
        struct Receipt<'a> {
            schema_version: u32,
            state: &'static str,
            source_commit: &'a str,
            source_tree: &'a str,
            canonical_manifest: &'a FileDigest,
            tracked_and_parent_metadata: Option<&'a [BorrowedPhysicalEntry]>,
            complete_original_cold_full13_inverse_equal: bool,
            actual_error: Option<String>,
            borrowed_bytes_or_link_targets_read: bool,
            missing_or_sparse_states_preserved: bool,
            runtime_test_or_reference_credit: u32,
            closure: &'static str,
        }
        ensure!(
            path.parent() == Some(self.output_parent.path.as_path()),
            "borrowed receipt parent differs"
        );
        let bytes = super::canonical(
            &Receipt {
                schema_version: 1,
                state,
                source_commit: &self.source.sha,
                source_tree: &self.source.tree,
                canonical_manifest: &self.canonical_manifest,
                tracked_and_parent_metadata: entries,
                complete_original_cold_full13_inverse_equal: complete_inverse_equal,
                actual_error,
                borrowed_bytes_or_link_targets_read: false,
                missing_or_sparse_states_preserved: complete_inverse_equal,
                runtime_test_or_reference_credit: 0,
                closure: "CLOSED0",
            },
            MAX_MANIFEST_BYTES,
        )?;
        let (block, available, available_inodes) = self.receipt_filesystem_space()?;
        let reserve = self.pending_receipt_reserve(block)?;
        require_borrowed_receipt_capacity(reserve, available, available_inodes)?;
        self.output_parent.require_current()?;
        let name = std::ffi::CString::new(
            path.file_name()
                .context("borrowed receipt name absent")?
                .as_bytes(),
        )?;
        let mut output = create_file_at(&self.output_parent.file, &name)?;
        output.write_all(&bytes)?;
        output.sync_all()?;
        self.pending_receipts.set(self.pending_receipts.get() - 1);
        self.output_parent.require_current()?;
        Ok(())
    }

    fn write_before(&self, path: &Path) -> Result<()> {
        self.require_root()?;
        self.write_capture(
            path,
            "ORIGINAL_COLD_BORROWED_METADATA_BEFORE_OWNED_CONSTRUCTION",
            Some(&self.before),
            false,
            None,
        )?;
        ensure!(
            self.before == self.snapshot()?,
            "cold borrowed source changed before construction"
        );
        Ok(())
    }

    fn finish(&self, path: &Path) -> Result<()> {
        let observation = self.snapshot().and_then(|after| {
            ensure!(
                after == self.snapshot()?,
                "borrowed metadata changed during final capture"
            );
            Ok(after)
        });
        match observation {
            Ok(after) => {
                let equal = self.before == after;
                self.write_capture(
                    path,
                    "ACTUAL_BORROWED_METADATA_AFTER_OWNED_CONSTRUCTION",
                    Some(&after),
                    equal,
                    None,
                )?;
                ensure!(
                    equal,
                    "owned construction changed the original cold borrowed full13 or missing state"
                );
                Ok(())
            }
            Err(error) => {
                self.write_capture(
                    path,
                    "BORROWED_METADATA_AFTER_CAPTURE_FAILED_ZERO_CREDIT",
                    None,
                    false,
                    Some(format!("{error:#}")),
                )?;
                Err(error)
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn prepare_checkout_with_guard(
    source: &SourceArgs,
    destination: &Path,
    receipt: &Path,
    before: &Path,
    after: &Path,
    guard: &BorrowedPhysicalGuard,
) -> Result<()> {
    // Admit both capped diagnostics before creating either inode. An already
    // borrowed receipt name is refused without overwriting it or constructing
    // the checkout. The retained destination parent is shared by all outputs.
    guard.require_new_receipt_names(&[receipt, before, after])?;
    guard.write_before(before)?;
    let constructed = prepare_owned_checkout_with_receipt_reserve(source, destination, Some(guard))
        .and_then(|checkout| checkout.write_new_receipt(receipt));
    // Remaining AFTER capacity was included in construction's admission, even
    // if it failed before creating the checkout. Recheck actual free space on
    // every receipt write; unrelated disk use cannot authorize spending floor.
    let inverse = guard.finish(after);
    match (constructed, inverse) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(inverse)) => {
            Err(error.context(format!("borrowed full13 inverse also failed: {inverse:#}")))
        }
    }
}

#[cfg(target_os = "linux")]
/// Optional native-job diagnostics, separate from the portable canonical source
/// manifest. The borrowed working tree is only inspected through O_PATH; regular
/// bytes and symlink targets come from Git objects in the unchanged constructor.
/// The retained protected output parent has the same documented effective-user
/// trust boundary as OwnedCheckout. No borrowed inode becomes an owned checkout.
pub(super) fn prepare_checkout_with_borrowed_physical(
    source: &SourceArgs,
    destination: &Path,
    receipt: &Path,
    before: &Path,
    after: &Path,
) -> Result<()> {
    use std::path::Component;
    let confined = |path: &Path| {
        path.is_absolute()
            && path
                .components()
                .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
    };
    ensure!(
        confined(&source.source)
            && confined(destination)
            && !destination.starts_with(&source.source)
            && !source.source.starts_with(destination)
            && [receipt, before, after].iter().all(|path| confined(path)
                && path.parent() == destination.parent()
                && !path.starts_with(&source.source)
                && *path != destination)
            && receipt != before
            && receipt != after
            && before != after,
        "borrowed physical receipts must be distinct new confined siblings outside the source and checkout"
    );
    let parent =
        TrustedCreationParent::open(destination.parent().context("checkout parent absent")?)?;
    let guard = BorrowedPhysicalGuard::capture(source, parent)?;
    prepare_checkout_with_guard(source, destination, receipt, before, after, &guard)
}

#[cfg(not(target_os = "linux"))]
pub(super) fn prepare_checkout_with_borrowed_physical(
    _: &SourceArgs,
    _: &Path,
    _: &Path,
    _: &Path,
    _: &Path,
) -> Result<()> {
    bail!("cold borrowed metadata guard requires Linux")
}

#[cfg(all(test, target_os = "linux"))]
mod native_owned_source_regression {
    use super::*;

    fn cold_link(source: &SourceArgs) -> Physical13 {
        use std::os::unix::ffi::OsStrExt as _;
        let path = source.source.join("AGENTS.md");
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("README.md", &path).unwrap();
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let times = [
            libc::timespec {
                tv_sec: 1,
                tv_nsec: 0,
            },
            libc::timespec {
                tv_sec: 2,
                tv_nsec: 0,
            },
        ];
        // SAFETY: this fixture owns the newly created link. Initialize the cold
        // precondition before observation; never restore a captured timestamp.
        let result = unsafe {
            libc::utimensat(
                libc::AT_FDCWD,
                name.as_ptr(),
                times.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        assert_eq!(result, 0, "cold fixture initialization failed");
        physical(&fs::symlink_metadata(path).unwrap()).unwrap()
    }

    fn receipt_json(path: &Path) -> serde_json::Value {
        serde_json::from_slice(&read_bounded(path, MAX_MANIFEST_BYTES).unwrap().0).unwrap()
    }

    #[test]
    fn selected_source_independent_of_controller_preserves_cold_full13_and_exact_manifest() {
        let (_controller_directory, controller) = super::super::tests::fixture();
        let (_borrowed_directory, mut selected) = super::super::tests::fixture();
        fs::write(
            selected.source.join("native-source-marker"),
            b"different selected application tree\n",
        )
        .unwrap();
        command(
            Some(&selected.source),
            "git",
            &["add", "native-source-marker"],
            4096,
        )
        .unwrap();
        command(
            Some(&selected.source),
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
                "independent application",
            ],
            4096,
        )
        .unwrap();
        selected.sha = String::from_utf8(
            command(Some(&selected.source), "git", &["rev-parse", "HEAD"], 1024).unwrap(),
        )
        .unwrap()
        .trim()
        .to_owned();
        selected.tree = String::from_utf8(
            command(
                Some(&selected.source),
                "git",
                &["rev-parse", "HEAD^{tree}"],
                1024,
            )
            .unwrap(),
        )
        .unwrap()
        .trim()
        .to_owned();
        assert_ne!(selected.sha, controller.sha);
        assert_ne!(selected.tree, controller.tree);
        let cold = cold_link(&selected);
        let (expected, expected_observation) =
            observe(&selected, Authentication::GitObjects).unwrap();
        let output = tempfile::tempdir().unwrap();
        let destination = output.path().join("owned-source");
        let checkout_receipt = output.path().join("checkout.json");
        let before_path = output.path().join("borrowed-before.json");
        let after_path = output.path().join("borrowed-after.json");
        prepare_checkout_with_borrowed_physical(
            &selected,
            &destination,
            &checkout_receipt,
            &before_path,
            &after_path,
        )
        .unwrap();
        let before = receipt_json(&before_path);
        let after = receipt_json(&after_path);
        assert_eq!(
            before["tracked_and_parent_metadata"],
            after["tracked_and_parent_metadata"]
        );
        assert_eq!(after["complete_original_cold_full13_inverse_equal"], true);
        assert_eq!(after["borrowed_bytes_or_link_targets_read"], false);
        assert_eq!(after["source_commit"], selected.sha);
        assert_eq!(after["source_tree"], selected.tree);
        assert_eq!(
            after["canonical_manifest"],
            serde_json::to_value(expected_observation.canonical_manifest).unwrap()
        );
        assert_eq!(
            physical(&fs::symlink_metadata(selected.source.join("AGENTS.md")).unwrap()).unwrap(),
            cold
        );
        let actual = observe(
            &SourceArgs {
                source: destination.clone(),
                sha: selected.sha.clone(),
                tree: selected.tree.clone(),
            },
            Authentication::Materialized,
        )
        .unwrap();
        assert_eq!(actual.0, expected);
        let constructed = receipt_json(&checkout_receipt);
        assert_eq!(constructed["source_commit"], selected.sha);
        assert_eq!(constructed["source_tree"], selected.tree);
        assert_eq!(constructed["runtime_test_or_reference_credit"], 0);
        assert!(constructed["initialized_new_symlinks"].as_u64().unwrap() > 0);
    }

    #[test]
    fn cold_borrowed_link_atime_change_fails_and_retains_both_original_and_actual_metadata() {
        let (_borrowed_directory, source) = super::super::tests::fixture();
        let cold = cold_link(&source);
        let output = tempfile::tempdir().unwrap();
        let before_path = output.path().join("before.json");
        let after_path = output.path().join("after.json");
        let guard = BorrowedPhysicalGuard::capture(
            &source,
            TrustedCreationParent::open(output.path()).unwrap(),
        )
        .unwrap();
        guard.write_before(&before_path).unwrap();
        let original_receipt = digest_file(&before_path, MAX_MANIFEST_BYTES).unwrap();
        assert_eq!(
            physical(&fs::symlink_metadata(source.source.join("AGENTS.md")).unwrap()).unwrap(),
            cold
        );
        assert_eq!(
            fs::read_link(source.source.join("AGENTS.md")).unwrap(),
            Path::new("README.md")
        );
        let changed =
            physical(&fs::symlink_metadata(source.source.join("AGENTS.md")).unwrap()).unwrap();
        assert_eq!(cold.changed_fields(&changed), ["atime_ns"]);
        assert!(guard.finish(&after_path).is_err());
        assert_eq!(
            digest_file(&before_path, MAX_MANIFEST_BYTES).unwrap(),
            original_receipt
        );
        let before = receipt_json(&before_path);
        let after = receipt_json(&after_path);
        assert_ne!(
            before["tracked_and_parent_metadata"],
            after["tracked_and_parent_metadata"]
        );
        assert_eq!(after["complete_original_cold_full13_inverse_equal"], false);
        assert_eq!(after["runtime_test_or_reference_credit"], 0);
        assert_eq!(
            physical(&fs::symlink_metadata(source.source.join("AGENTS.md")).unwrap()).unwrap(),
            changed
        );
    }

    #[test]
    fn missing_borrowed_tracked_file_is_preserved_and_materialized_only_in_owned_destination() {
        let (_borrowed_directory, source) = super::super::tests::fixture();
        cold_link(&source);
        fs::remove_file(source.source.join("README.md")).unwrap();
        let output = tempfile::tempdir().unwrap();
        let destination = output.path().join("owned");
        let before_path = output.path().join("before.json");
        let after_path = output.path().join("after.json");
        prepare_checkout_with_borrowed_physical(
            &source,
            &destination,
            &output.path().join("checkout.json"),
            &before_path,
            &after_path,
        )
        .unwrap();
        let before = receipt_json(&before_path);
        let after = receipt_json(&after_path);
        let missing = before["tracked_and_parent_metadata"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["path"] == "README.md")
            .unwrap();
        assert!(missing["identity"].is_null());
        assert_eq!(
            before["tracked_and_parent_metadata"],
            after["tracked_and_parent_metadata"]
        );
        assert_eq!(after["missing_or_sparse_states_preserved"], true);
        assert!(!source.source.join("README.md").exists());
        assert!(destination.join("README.md").exists());
        assert!(
            observe(
                &SourceArgs {
                    source: destination,
                    sha: source.sha,
                    tree: source.tree
                },
                Authentication::Materialized
            )
            .is_ok()
        );
    }

    #[test]
    fn existing_destination_constructor_failure_still_retains_strict_borrowed_inverse() {
        let (_borrowed_directory, source) = super::super::tests::fixture();
        cold_link(&source);
        let output = tempfile::tempdir().unwrap();
        let destination = output.path().join("existing");
        fs::create_dir(&destination).unwrap();
        fs::write(
            destination.join("sentinel"),
            b"borrowed tree remains intact",
        )
        .unwrap();
        let sentinel = digest_file(&destination.join("sentinel"), 4096).unwrap();
        let before_path = output.path().join("before.json");
        let after_path = output.path().join("after.json");
        let checkout_receipt = output.path().join("checkout.json");
        assert!(
            prepare_checkout_with_borrowed_physical(
                &source,
                &destination,
                &checkout_receipt,
                &before_path,
                &after_path
            )
            .is_err()
        );
        assert_eq!(
            digest_file(&destination.join("sentinel"), 4096).unwrap(),
            sentinel
        );
        assert!(!checkout_receipt.exists());
        assert_eq!(
            receipt_json(&before_path)["tracked_and_parent_metadata"],
            receipt_json(&after_path)["tracked_and_parent_metadata"]
        );
        assert_eq!(
            receipt_json(&after_path)["complete_original_cold_full13_inverse_equal"],
            true
        );
        assert_eq!(
            receipt_json(&after_path)["runtime_test_or_reference_credit"],
            0
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod borrowed_receipt_capacity_regression {
    use super::*;

    fn guard_with_ceiling(
        source: &SourceArgs,
        parent: &Path,
        bytes: u64,
        inodes: u64,
    ) -> BorrowedPhysicalGuard {
        let mut guard =
            BorrowedPhysicalGuard::capture(source, TrustedCreationParent::open(parent).unwrap())
                .unwrap();
        // Test-only limits reduce the real filesystem sample, never increase it.
        // This makes refusal deterministic without filling disks or changing a
        // global environment, filesystem, cache, or other test's budget.
        guard.receipt_capacity_ceiling = Some((bytes, inodes));
        guard
    }

    #[test]
    fn both_receipts_require_exact_caps_padding_floor_and_inode_headroom() {
        let reserve = borrowed_receipt_reserve(4096, 2).unwrap();
        assert_eq!(reserve, (2 * MAX_MANIFEST_BYTES + 8192, 2));
        let required = super::super::ORIGINAL_FLOOR + reserve.0;
        assert!(require_borrowed_receipt_capacity(reserve, required, 34).is_ok());
        assert!(require_borrowed_receipt_capacity(reserve, required - 1, 34).is_err());
        assert!(require_borrowed_receipt_capacity(reserve, required, 33).is_err());
        assert!(require_borrowed_receipt_capacity(reserve, required + 1, 35).is_ok());
        assert!(borrowed_receipt_reserve(0, 2).is_err());
        assert!(borrowed_receipt_reserve(1024 * 1024 + 1, 2).is_err());
        assert!(borrowed_receipt_reserve(4096, 0).is_err());
        assert!(borrowed_receipt_reserve(4096, 3).is_err());
        assert!(require_borrowed_receipt_capacity((u64::MAX, 2), u64::MAX, 34).is_err());
        assert!(require_borrowed_receipt_capacity((1, u64::MAX), u64::MAX, u64::MAX).is_err());
    }

    #[test]
    fn below_both_receipt_space_creates_no_diagnostic_or_checkout_inode() {
        let (_borrowed, source) = super::super::tests::fixture();
        let output = tempfile::tempdir().unwrap();
        let guard = guard_with_ceiling(
            &source,
            output.path(),
            super::super::ORIGINAL_FLOOR + MAX_MANIFEST_BYTES,
            u64::MAX,
        );
        let original = guard.snapshot().unwrap();
        let before = output.path().join("before.json");
        let after = output.path().join("after.json");
        let receipt = output.path().join("checkout.json");
        let destination = output.path().join("owned");
        let error =
            prepare_checkout_with_guard(&source, &destination, &receipt, &before, &after, &guard)
                .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("borrowed receipt disk/inode reserve")
        );
        assert_eq!(guard.pending_receipts.get(), 2);
        assert_eq!(guard.snapshot().unwrap(), original);
        assert!(!before.exists());
        assert!(!after.exists());
        assert!(!receipt.exists());
        assert!(!destination.exists());
    }

    #[test]
    fn insufficient_receipt_inodes_creates_no_diagnostic_or_checkout_inode() {
        let (_borrowed, source) = super::super::tests::fixture();
        let output = tempfile::tempdir().unwrap();
        let guard = guard_with_ceiling(&source, output.path(), u64::MAX, 33);
        let original = guard.snapshot().unwrap();
        let before = output.path().join("before.json");
        let after = output.path().join("after.json");
        let receipt = output.path().join("checkout.json");
        let destination = output.path().join("owned");
        let error =
            prepare_checkout_with_guard(&source, &destination, &receipt, &before, &after, &guard)
                .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("borrowed receipt disk/inode reserve")
        );
        assert_eq!(guard.pending_receipts.get(), 2);
        assert_eq!(guard.snapshot().unwrap(), original);
        assert!(!before.exists());
        assert!(!after.exists());
        assert!(!receipt.exists());
        assert!(!destination.exists());
    }

    #[test]
    fn admitted_receipts_survive_constructor_disk_failure_without_spending_floor() {
        let (_borrowed, source) = super::super::tests::fixture();
        let output = tempfile::tempdir().unwrap();
        let mut guard = BorrowedPhysicalGuard::capture(
            &source,
            TrustedCreationParent::open(output.path()).unwrap(),
        )
        .unwrap();
        let (block, _, _) = guard.receipt_filesystem_space().unwrap();
        let reserve = borrowed_receipt_reserve(block, 2).unwrap();
        let initial = super::super::ORIGINAL_FLOOR + reserve.0;
        guard.receipt_capacity_ceiling = Some((initial, u64::MAX));
        let original = guard.snapshot().unwrap();
        let before = output.path().join("before.json");
        let after = output.path().join("after.json");
        let receipt = output.path().join("checkout.json");
        let destination = output.path().join("owned");
        let error =
            prepare_checkout_with_guard(&source, &destination, &receipt, &before, &after, &guard)
                .unwrap_err();
        assert!(format!("{error:#}").contains("owned checkout disk/inode reserve"));
        assert_eq!(guard.pending_receipts.get(), 0);
        assert!(!receipt.exists());
        assert!(!destination.exists());
        let before_bytes = read_bounded(&before, MAX_MANIFEST_BYTES).unwrap().0;
        let after_bytes = read_bounded(&after, MAX_MANIFEST_BYTES).unwrap().0;
        let before_json: serde_json::Value = serde_json::from_slice(&before_bytes).unwrap();
        let after_json: serde_json::Value = serde_json::from_slice(&after_bytes).unwrap();
        assert_eq!(
            before_json["tracked_and_parent_metadata"],
            after_json["tracked_and_parent_metadata"]
        );
        assert_eq!(
            after_json["complete_original_cold_full13_inverse_equal"],
            true
        );
        assert_eq!(after_json["runtime_test_or_reference_credit"], 0);
        assert_eq!(guard.snapshot().unwrap(), original);
        let consumed = before_bytes.len() as u64 + after_bytes.len() as u64 + 2 * block;
        assert!(initial - consumed >= super::super::ORIGINAL_FLOOR);
        assert!(
            guard
                .write_before(&output.path().join("third.json"))
                .is_err()
        );
        assert!(!output.path().join("third.json").exists());
    }

    #[test]
    fn preexisting_after_receipt_is_refused_without_overwrite_or_other_outputs() {
        let (_borrowed, source) = super::super::tests::fixture();
        let output = tempfile::tempdir().unwrap();
        let after = output.path().join("after.json");
        fs::write(&after, b"borrowed diagnostic remains unchanged").unwrap();
        let sentinel = digest_file(&after, 4096).unwrap();
        let guard = BorrowedPhysicalGuard::capture(
            &source,
            TrustedCreationParent::open(output.path()).unwrap(),
        )
        .unwrap();
        let original = guard.snapshot().unwrap();
        let before = output.path().join("before.json");
        let receipt = output.path().join("checkout.json");
        let destination = output.path().join("owned");
        let error =
            prepare_checkout_with_guard(&source, &destination, &receipt, &before, &after, &guard)
                .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("borrowed receipt output must be new")
        );
        assert_eq!(guard.pending_receipts.get(), 2);
        assert_eq!(digest_file(&after, 4096).unwrap(), sentinel);
        assert_eq!(guard.snapshot().unwrap(), original);
        assert!(!before.exists());
        assert!(!receipt.exists());
        assert!(!destination.exists());
    }

    #[test]
    fn replaced_output_parent_cannot_redirect_capacity_or_receipt_creation() {
        let (_borrowed, source) = super::super::tests::fixture();
        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().join("output");
        fs::create_dir(&parent).unwrap();
        let guard =
            BorrowedPhysicalGuard::capture(&source, TrustedCreationParent::open(&parent).unwrap())
                .unwrap();
        let original = guard.snapshot().unwrap();
        let retained = temporary.path().join("retained-output");
        fs::rename(&parent, &retained).unwrap();
        fs::create_dir(&parent).unwrap();
        fs::write(parent.join("sentinel"), b"replacement remains unchanged").unwrap();
        let sentinel = digest_file(&parent.join("sentinel"), 4096).unwrap();
        assert!(guard.receipt_filesystem_space().is_err());
        assert!(guard.write_before(&parent.join("before.json")).is_err());
        assert_eq!(guard.pending_receipts.get(), 2);
        assert_eq!(
            digest_file(&parent.join("sentinel"), 4096).unwrap(),
            sentinel
        );
        assert_eq!(guard.snapshot().unwrap(), original);
        assert!(!parent.join("before.json").exists());
        assert!(!retained.join("before.json").exists());
    }
}
