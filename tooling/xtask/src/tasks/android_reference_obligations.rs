use anyhow::{Context as _, Result, bail, ensure};
use clap::Args;
use flate2::bufread::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_DESCRIPTOR_BYTES: u64 = 1024 * 1024;
const MAX_MATRIX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_MATRIX_ROWS: usize = 16_384;
const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;
const MAX_STREAM_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_STREAM_RECORDS: u64 = 2_000_000;
const SHARDS: usize = 64;
const MAX_SHARD_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SHARD_ROWS: usize = 16_384;

const PINS: [(&str, &str, &str, &str); 3] = [
    (
        "base",
        "4a5d2ec9571e2021fc9a4621e24a195f966ee6dd",
        "https://android.googlesource.com/platform/tools/base",
        "0cbac492c1e2b47273fc9a1e2929caa6460660cf475cbd7773256f443cb30bb4",
    ),
    (
        "idea",
        "a84efec3ba9542d9bfa1255103f0dc94833a3796",
        "https://android.googlesource.com/platform/tools/adt/idea",
        "f15d0ee82d4719da82aa7d549dc4c3c14c180dac8112c4a1a73a65474419cf84",
    ),
    (
        "jetbrains-android",
        "132bc7c3cf52598117590637d00e81b929444bde",
        "https://github.com/JetBrains/android",
        "d7bb6a155c077166a6aed54ddaaa7d1629cbee8c200ceced0bddc0bcedfd92e2",
    ),
];

#[derive(Args)]
pub struct AndroidReferenceObligationsArgs {
    /// Checkout whose exact canonical manifest and ledger are conserved.
    #[arg(long)]
    repository: PathBuf,
    /// Directory containing the retained inventories, without symlink components.
    #[arg(long)]
    input_root: PathBuf,
    /// Exact intake descriptor; its independent SHA-256 is required.
    #[arg(long)]
    intake: PathBuf,
    #[arg(long)]
    intake_sha256: String,
    /// New output directory outside the checkout, or an existing directory for --check.
    #[arg(long)]
    output: PathBuf,
    /// Require an identical artifact set and bytes; never replace existing output.
    #[arg(long)]
    check: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    path: String,
    bytes: u64,
    sha256: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CompressedBinding {
    file: Binding,
    decompressed_bytes: u64,
    decompressed_sha256: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Intake {
    schema_version: u32,
    summary: CompressedBinding,
    retention: Binding,
    sources: Vec<SourceIntake>,
    manifest: Binding,
    ledger: Binding,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceIntake {
    id: String,
    members: CompressedBinding,
    physical_members: CompressedBinding,
    candidates: CompressedBinding,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DiscoverySummary {
    schema_version: u32,
    baseline: String,
    effective_test_census_complete: bool,
    discovery: String,
    unresolved: Vec<String>,
    sources: Vec<SourceSummary>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceSummary {
    id: String,
    revision: String,
    repository: String,
    coverage: String,
    archive: ReferenceArtifact,
    archive_bytes: u64,
    all_members_traversed: bool,
    physical_member_types: BTreeMap<String, u64>,
    member_kinds: BTreeMap<String, u64>,
    candidate_kinds: BTreeMap<String, u64>,
    method_candidates: u64,
    files_with_unresolved_lexing: u64,
    unclassified_regular_members: u64,
    members: ReferenceArtifact,
    physical_members: ReferenceArtifact,
    candidates: ReferenceArtifact,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReferenceArtifact {
    path: String,
    sha256: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Retention {
    source_commit: String,
    rust_cli_sha256: String,
    raw_artifact_file_bytes: u64,
    compressed_artifact_file_bytes: u64,
    compression: String,
    raw_artifacts_retained: bool,
    artifacts: Vec<RetainedArtifact>,
    raw_path_status: String,
    retirement_receipt: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RetainedArtifact {
    raw_path: String,
    raw_bytes: u64,
    raw_sha256: String,
    gzip_path: String,
    gzip_bytes: u64,
    gzip_sha256: String,
    decompressed_bytes: u64,
    decompressed_sha256: String,
    decompression_verified: bool,
    raw_path_status: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Member {
    path: String,
    archive_path: String,
    kind: String,
    bytes: u64,
    sha256: String,
    link_target: Option<String>,
    discovery_classification: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PhysicalMember {
    ordinal: u64,
    type_byte: u8,
    raw_path_bytes: Vec<u8>,
    header_sha256: String,
    bytes: u64,
    payload_sha256: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    source: String,
    revision: String,
    path: String,
    sha256: String,
    bytes: u64,
    kind: String,
    fixture_path_hint: bool,
    copyright_lines: Vec<String>,
    declarations: Option<Declarations>,
    unresolved: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Declarations {
    package: Option<String>,
    classes: Vec<ClassDeclaration>,
    methods: Vec<MethodDeclaration>,
    annotations: Vec<Annotation>,
    unresolved: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ClassDeclaration {
    name: String,
    line: u64,
    declaration: String,
    abstract_hint: bool,
    inheritance_declaration: String,
    inheritance_declaration_complete: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MethodDeclaration {
    suite_candidate: Option<String>,
    name: String,
    line: u64,
    declaration: String,
    annotations: Vec<String>,
    reasons: Vec<String>,
    effective_runtime_cases: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Annotation {
    name: String,
    line: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Matrix {
    schema_version: u32,
    baseline: String,
    census_complete: bool,
    sources: Vec<MatrixSource>,
    tests: Vec<ReferenceTest>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MatrixSource {
    id: String,
    repository: String,
    revision: String,
    tag: Option<String>,
    license: String,
    archive: ReferenceArtifact,
    coverage: String,
    coverage_reason: String,
    coverage_evidence: Option<ReferenceArtifact>,
    census_complete: bool,
    census_evidence: Option<ReferenceArtifact>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReferenceTest {
    id: String,
    source: String,
    suite: String,
    method: String,
    case: Option<String>,
    file: ReferenceArtifact,
    line: u64,
    declaration: String,
    copyright: String,
    fixtures: Vec<ReferenceArtifact>,
    fixture_reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    schema_version: u32,
    entries: Vec<Entry>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    reference_id: String,
    port_status: String,
    run_status: String,
    reason: Option<String>,
    run_blocker: Option<String>,
    rust_targets: Vec<RustTarget>,
    fixtures: Vec<FixturePort>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RustTarget {
    package: String,
    integration_target: Option<String>,
    test_name: String,
    manifest: ReferenceArtifact,
    source: ReferenceArtifact,
    run: Option<RunEvidence>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RunEvidence {
    tested_commit: String,
    command: Vec<String>,
    exit_code: i32,
    log: ReferenceArtifact,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FixturePort {
    reference_path: String,
    destination: ReferenceArtifact,
    adapted: bool,
    reason: Option<String>,
}

struct CanonicalJoin {
    test: ReferenceTest,
    entry: Entry,
    count: u64,
    first_identity: Option<String>,
}

type JoinKey = (String, String, String, String, String, u64);

#[derive(Default, Serialize)]
struct Counts {
    members: u64,
    physical_members: u64,
    candidates: u64,
    classes: u64,
    methods: u64,
    build_obligations: u64,
    unresolved_records: u64,
    canonical_matches: u64,
    files_with_unresolved_lexing: u64,
    unclassified_regular_members: u64,
    member_kinds: BTreeMap<String, u64>,
    physical_member_types: BTreeMap<String, u64>,
    candidate_kinds: BTreeMap<String, u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct IndexItem {
    path: String,
    sha256: String,
    bytes: u64,
    kind: String,
    ordinal: u64,
}

#[derive(Serialize)]
struct OutputSummary<'a> {
    schema_version: u32,
    scope: &'static str,
    intake_sha256: &'a str,
    intake: &'a Intake,
    retention: &'a Retention,
    manifest: &'a Binding,
    ledger: &'a Binding,
    baseline: &'a str,
    canonical_rows: usize,
    canonical_join_unresolved: usize,
    census_complete: bool,
    effective_runtime_cases: Option<u64>,
    new_behavioral_parity_credit: u64,
    original_archive_traversal_performed: bool,
    source_discovery: &'a DiscoverySummary,
    sources: BTreeMap<String, Counts>,
    artifacts: Vec<Binding>,
}

#[derive(Debug, PartialEq, Eq)]
struct PhysicalState {
    length: u64,
    #[cfg(unix)]
    fields: [u64; 16],
    #[cfg(not(unix))]
    modified: Option<SystemTime>,
}

fn physical_state(metadata: &fs::Metadata) -> PhysicalState {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt as _;
    PhysicalState {
        length: metadata.len(),
        #[cfg(unix)]
        fields: [
            metadata.dev(),
            metadata.ino(),
            metadata.mode() as u64,
            metadata.nlink(),
            metadata.uid() as u64,
            metadata.gid() as u64,
            metadata.rdev(),
            metadata.size(),
            metadata.atime() as u64,
            metadata.atime_nsec() as u64,
            metadata.mtime() as u64,
            metadata.mtime_nsec() as u64,
            metadata.ctime() as u64,
            metadata.ctime_nsec() as u64,
            metadata.blksize(),
            metadata.blocks(),
        ],
        #[cfg(not(unix))]
        modified: metadata.modified().ok(),
    }
}

fn regular_file(path: &Path) -> Result<(File, PhysicalState)> {
    ensure!(
        path.is_absolute(),
        "input path must be absolute: {}",
        path.display()
    );
    let mut prefix = PathBuf::new();
    for component in path.components() {
        ensure!(
            !matches!(component, Component::ParentDir | Component::CurDir),
            "non-normal input path"
        );
        prefix.push(component.as_os_str());
        ensure!(
            !fs::symlink_metadata(&prefix)?.file_type().is_symlink(),
            "symlink input component: {}",
            prefix.display()
        );
    }
    let before = fs::symlink_metadata(path)?;
    ensure!(
        before.is_file(),
        "input must be a regular file: {}",
        path.display()
    );
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        // Immutable retained evidence includes access times; ordinary reads invalidate its binding.
        options.custom_flags(0x40000 | 0x20000);
    }
    let file = options
        .open(path)
        .with_context(|| format!("opening immutable input {}", path.display()))?;
    let state = physical_state(&before);
    ensure!(
        physical_state(&file.metadata()?) == state,
        "input changed while opening"
    );
    Ok((file, state))
}

fn unchanged(file: &File, path: &Path, state: &PhysicalState) -> Result<()> {
    ensure!(
        physical_state(&file.metadata()?) == *state
            && physical_state(&fs::symlink_metadata(path)?) == *state,
        "input bytes or physical metadata changed: {}",
        path.display()
    );
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_hash(hash: &str, length: usize) -> bool {
    hash.len() == length
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn safe_relative(path: &str, allow_empty: bool) -> Result<()> {
    ensure!(
        (allow_empty || !path.is_empty())
            && !path.contains(['\\', '\0'])
            && !Path::new(path).is_absolute(),
        "unsafe relative path: {path:?}"
    );
    ensure!(
        path.split('/')
            .all(|part| !matches!(part, "." | "..") && (!part.is_empty() || path.is_empty())),
        "non-normal relative path: {path:?}"
    );
    Ok(())
}

fn validate_binding(binding: &Binding, maximum: u64) -> Result<()> {
    safe_relative(&binding.path, false)?;
    ensure!(
        binding.bytes <= maximum && valid_hash(&binding.sha256, 64),
        "invalid or oversized binding: {}",
        binding.path
    );
    Ok(())
}

fn bound_bytes(path: &Path, binding: &Binding, maximum: u64) -> Result<Vec<u8>> {
    validate_binding(binding, maximum)?;
    let (mut file, state) = regular_file(path)?;
    ensure!(
        state.length == binding.bytes,
        "input length disagrees with binding: {}",
        path.display()
    );
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 == binding.bytes && digest(&bytes) == binding.sha256,
        "input hash disagrees with binding: {}",
        path.display()
    );
    unchanged(&file, path, &state)?;
    Ok(bytes)
}

struct HashReader<R> {
    inner: R,
    hash: Sha256,
    bytes: u64,
}

impl<R: Read> Read for HashReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let count = self.inner.read(bytes)?;
        self.hash.update(&bytes[..count]);
        self.bytes = self
            .bytes
            .checked_add(count as u64)
            .ok_or_else(|| std::io::Error::other("input byte count overflow"))?;
        Ok(count)
    }
}

fn compressed_stream<T: serde::de::DeserializeOwned>(
    root: &Path,
    binding: &CompressedBinding,
    mut consume: impl FnMut(T, u64, &str) -> Result<()>,
) -> Result<u64> {
    validate_binding(&binding.file, MAX_STREAM_BYTES)?;
    ensure!(
        binding.decompressed_bytes <= MAX_STREAM_BYTES
            && valid_hash(&binding.decompressed_sha256, 64),
        "invalid decompressed binding"
    );
    let path = root.join(&binding.file.path);
    let (file, state) = regular_file(&path)?;
    ensure!(
        state.length == binding.file.bytes,
        "compressed input length changed"
    );
    let hashed = HashReader {
        inner: file,
        hash: Sha256::new(),
        bytes: 0,
    };
    let compressed = BufReader::new(hashed);
    let decoder = GzDecoder::new(compressed);
    let mut reader = BufReader::new(decoder);
    let mut raw_hash = Sha256::new();
    let mut raw_bytes = 0_u64;
    let mut records = 0_u64;
    loop {
        let mut line = Vec::new();
        reader
            .by_ref()
            .take(MAX_RECORD_BYTES as u64 + 1)
            .read_until(b'\n', &mut line)?;
        if line.is_empty() {
            break;
        }
        ensure!(
            line.len() <= MAX_RECORD_BYTES && line.last() == Some(&b'\n'),
            "oversized or unterminated JSONL record: {}",
            binding.file.path
        );
        raw_bytes = raw_bytes
            .checked_add(line.len() as u64)
            .context("raw byte count overflow")?;
        records = records.checked_add(1).context("record count overflow")?;
        ensure!(
            raw_bytes <= binding.decompressed_bytes && records <= MAX_STREAM_RECORDS,
            "stream budget exceeded: {}",
            binding.file.path
        );
        raw_hash.update(&line);
        let value = serde_json::from_slice(&line)
            .with_context(|| format!("invalid {} record {records}", binding.file.path))?;
        consume(value, records - 1, &digest(&line))?;
    }
    let decoder = reader.into_inner();
    let mut compressed = decoder.into_inner();
    ensure!(
        compressed.fill_buf()?.is_empty(),
        "trailing or concatenated gzip content: {}",
        binding.file.path
    );
    let hashed = compressed.into_inner();
    ensure!(
        hashed.bytes == binding.file.bytes
            && format!("{:x}", hashed.hash.finalize()) == binding.file.sha256,
        "compressed input hash changed: {}",
        binding.file.path
    );
    ensure!(
        raw_bytes == binding.decompressed_bytes
            && format!("{:x}", raw_hash.finalize()) == binding.decompressed_sha256,
        "decompressed input hash changed: {}",
        binding.file.path
    );
    unchanged(&hashed.inner, &path, &state)?;
    Ok(records)
}

fn compressed_document<T: serde::de::DeserializeOwned>(
    root: &Path,
    binding: &CompressedBinding,
) -> Result<T> {
    ensure!(
        binding.decompressed_bytes <= MAX_DESCRIPTOR_BYTES,
        "descriptor exceeds byte budget"
    );
    validate_binding(&binding.file, MAX_DESCRIPTOR_BYTES)?;
    ensure!(
        valid_hash(&binding.decompressed_sha256, 64),
        "invalid decompressed descriptor hash"
    );
    let path = root.join(&binding.file.path);
    let (file, state) = regular_file(&path)?;
    ensure!(
        state.length == binding.file.bytes,
        "compressed descriptor length changed"
    );
    let hashed = HashReader {
        inner: file,
        hash: Sha256::new(),
        bytes: 0,
    };
    let mut decoder = GzDecoder::new(BufReader::new(hashed));
    let mut bytes = Vec::new();
    decoder
        .by_ref()
        .take(binding.decompressed_bytes + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 == binding.decompressed_bytes
            && digest(&bytes) == binding.decompressed_sha256,
        "decompressed descriptor changed"
    );
    let mut compressed = decoder.into_inner();
    ensure!(
        compressed.fill_buf()?.is_empty(),
        "trailing or concatenated gzip descriptor"
    );
    let hashed = compressed.into_inner();
    ensure!(
        hashed.bytes == binding.file.bytes
            && format!("{:x}", hashed.hash.finalize()) == binding.file.sha256,
        "compressed descriptor hash changed"
    );
    unchanged(&hashed.inner, &path, &state)?;
    Ok(serde_json::from_slice(&bytes)?)
}

struct LimitedWriter<'a, W> {
    writer: &'a mut W,
    bytes: usize,
}

impl<W: Write> Write for LimitedWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let next = self
            .bytes
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("record byte count overflow"))?;
        if next > MAX_RECORD_BYTES {
            return Err(std::io::Error::other("output record budget exceeded"));
        }
        self.writer.write_all(bytes)?;
        self.bytes = next;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

fn json_line(writer: &mut impl Write, value: &impl Serialize) -> Result<()> {
    let mut limited = LimitedWriter { writer, bytes: 0 };
    serde_json::to_writer(&mut limited, value)?;
    limited.write_all(b"\n")?;
    Ok(())
}

fn identity(
    source: &SourceSummary,
    path: &str,
    hash: &str,
    role: &str,
    ordinal: u64,
    child: Option<usize>,
) -> Result<String> {
    Ok(digest(&serde_json::to_vec(&(
        &source.id,
        &source.revision,
        path,
        hash,
        role,
        ordinal,
        child,
    ))?))
}

fn checked_increment(count: &mut u64) -> Result<()> {
    *count = count.checked_add(1).context("count overflow")?;
    Ok(())
}

fn increment(counts: &mut BTreeMap<String, u64>, key: &str) -> Result<()> {
    let count = counts.entry(key.to_owned()).or_default();
    *count = count.checked_add(1).context("count overflow")?;
    Ok(())
}

struct ShardWriters {
    paths: Vec<PathBuf>,
    writers: Vec<BufWriter<File>>,
    bytes: Vec<u64>,
}

impl ShardWriters {
    fn new(directory: &Path, prefix: &str) -> Result<Self> {
        let mut paths = Vec::new();
        let mut writers = Vec::new();
        for index in 0..SHARDS {
            let path = directory.join(format!("{prefix}-{index:02}.jsonl"));
            writers.push(BufWriter::new(
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?,
            ));
            paths.push(path);
        }
        Ok(Self {
            paths,
            writers,
            bytes: vec![0; SHARDS],
        })
    }

    fn push(&mut self, item: &IndexItem) -> Result<()> {
        let first = Sha256::digest(item.path.as_bytes())[0] as usize;
        let index = first % SHARDS;
        let bytes = serde_json::to_vec(item)?;
        let total = self.bytes.get_mut(index).context("invalid shard index")?;
        *total = total
            .checked_add(bytes.len() as u64 + 1)
            .context("shard byte count overflow")?;
        ensure!(
            *total <= MAX_SHARD_BYTES,
            "conservation shard exceeds byte budget; discovery remains unresolved"
        );
        let writer = self
            .writers
            .get_mut(index)
            .context("invalid shard writer")?;
        writer.write_all(&bytes)?;
        writer.write_all(b"\n")?;
        Ok(())
    }

    fn finish(mut self) -> Result<Vec<PathBuf>> {
        for writer in &mut self.writers {
            writer.flush()?;
        }
        Ok(self.paths)
    }
}

fn shard_items(path: &Path) -> Result<Vec<IndexItem>> {
    let (file, state) = regular_file(path)?;
    ensure!(
        state.length <= MAX_SHARD_BYTES,
        "oversized conservation shard"
    );
    let mut reader = BufReader::new(file);
    let mut items = Vec::new();
    loop {
        let mut line = Vec::new();
        reader
            .by_ref()
            .take(MAX_RECORD_BYTES as u64 + 1)
            .read_until(b'\n', &mut line)?;
        if line.is_empty() {
            break;
        }
        ensure!(
            line.len() <= MAX_RECORD_BYTES && line.last() == Some(&b'\n'),
            "malformed conservation shard"
        );
        ensure!(
            items.len() < MAX_SHARD_ROWS,
            "conservation shard exceeds row budget; discovery remains unresolved"
        );
        items.push(serde_json::from_slice(&line)?);
    }
    unchanged(reader.get_ref(), path, &state)?;
    items.sort_by(|left: &IndexItem, right| left.path.cmp(&right.path));
    ensure!(
        items.windows(2).all(|pair| pair[0].path != pair[1].path),
        "duplicate inventory path"
    );
    Ok(items)
}

fn conserve_shards(members: &[PathBuf], candidates: &[PathBuf]) -> Result<()> {
    ensure!(
        members.len() == SHARDS && candidates.len() == SHARDS,
        "incomplete conservation shard set"
    );
    for (member_path, candidate_path) in members.iter().zip(candidates) {
        let members = shard_items(member_path)?;
        let candidates = shard_items(candidate_path)?;
        for candidate in candidates {
            let index = members
                .binary_search_by(|member| member.path.cmp(&candidate.path))
                .map_err(|_| {
                    anyhow::anyhow!("candidate has no original member: {}", candidate.path)
                })?;
            let member = members.get(index).context("missing indexed member")?;
            ensure!(
                member.kind == "file"
                    && member.sha256 == candidate.sha256
                    && member.bytes == candidate.bytes,
                "candidate disagrees with original member: {}",
                candidate.path
            );
        }
    }
    Ok(())
}

fn matrix_joins(
    matrix: Matrix,
    ledger: Ledger,
) -> Result<(String, Vec<CanonicalJoin>, BTreeMap<JoinKey, Vec<usize>>)> {
    ensure!(
        matrix.schema_version == 1 && ledger.schema_version == 1,
        "unsupported canonical matrix schema"
    );
    ensure!(
        matrix.tests.len() <= MAX_MATRIX_ROWS && ledger.entries.len() == matrix.tests.len(),
        "canonical row budget or cardinality mismatch"
    );
    ensure!(
        matrix.sources.len() == PINS.len(),
        "incomplete canonical pin set"
    );
    let mut source_revisions = BTreeMap::new();
    for source in &matrix.sources {
        let pin = PINS
            .iter()
            .find(|pin| pin.0 == source.id)
            .context("unknown canonical source")?;
        ensure!(
            source.revision == pin.1
                && source.repository == pin.2
                && source.archive.sha256 == pin.3
                && source.license == "Apache-2.0",
            "canonical source pin changed"
        );
        ensure!(
            source_revisions
                .insert(source.id.clone(), source.revision.clone())
                .is_none(),
            "duplicate canonical source"
        );
    }
    let mut entries = BTreeMap::new();
    for entry in ledger.entries {
        ensure!(
            matches!(
                entry.port_status.as_str(),
                "unported" | "ported" | "adapted" | "not_applicable"
            ) && matches!(
                entry.run_status.as_str(),
                "not_run" | "passing" | "failing" | "blocked"
            ),
            "unknown canonical status"
        );
        ensure!(
            entry.port_status != "not_applicable"
                || entry
                    .reason
                    .as_ref()
                    .is_some_and(|reason| !reason.trim().is_empty()),
            "not-applicable canonical row requires its individual reason"
        );
        ensure!(
            entries.insert(entry.reference_id.clone(), entry).is_none(),
            "duplicate canonical ledger ID"
        );
    }
    let mut joins = Vec::new();
    let mut keys: BTreeMap<JoinKey, Vec<usize>> = BTreeMap::new();
    for test in matrix.tests {
        safe_relative(&test.file.path, false)?;
        ensure!(
            valid_hash(&test.file.sha256, 64) && test.line > 0,
            "invalid canonical declaration binding"
        );
        let revision = source_revisions
            .get(&test.source)
            .context("unknown canonical test source")?;
        let suffix = test
            .case
            .as_ref()
            .map(|case| format!("[{case}]"))
            .unwrap_or_default();
        ensure!(
            test.id
                == format!(
                    "{}@{}:{}#{}.{}{}",
                    test.source, revision, test.file.path, test.suite, test.method, suffix
                ),
            "noncanonical reference ID"
        );
        let entry = entries
            .remove(&test.id)
            .context("missing or duplicate canonical reference ID")?;
        keys.entry((
            test.source.clone(),
            test.file.path.clone(),
            test.file.sha256.clone(),
            test.suite.clone(),
            test.method.clone(),
            test.line,
        ))
        .or_default()
        .push(joins.len());
        joins.push(CanonicalJoin {
            test,
            entry,
            count: 0,
            first_identity: None,
        });
    }
    ensure!(entries.is_empty(), "unmatched canonical ledger IDs");
    Ok((matrix.baseline, joins, keys))
}

fn check_retention(intake: &Intake, retention: &Retention) -> Result<()> {
    ensure!(
        valid_hash(&retention.source_commit, 40) && valid_hash(&retention.rust_cli_sha256, 64),
        "invalid retained producer identity"
    );
    ensure!(
        !retention.compression.is_empty()
            && !retention.raw_path_status.is_empty()
            && !retention.retirement_receipt.is_empty(),
        "missing retention provenance"
    );
    let mut expected = BTreeMap::new();
    expected.insert(intake.summary.file.path.as_str(), &intake.summary);
    for source in &intake.sources {
        for binding in [
            &source.members,
            &source.physical_members,
            &source.candidates,
        ] {
            ensure!(
                expected
                    .insert(binding.file.path.as_str(), binding)
                    .is_none(),
                "duplicate intake artifact"
            );
        }
    }
    ensure!(
        retention.artifacts.len() == expected.len(),
        "incomplete retained artifact set"
    );
    let mut compressed_total = 0_u64;
    let mut raw_total = 0_u64;
    for artifact in &retention.artifacts {
        let name = Path::new(&artifact.gzip_path)
            .file_name()
            .and_then(|name| name.to_str())
            .context("invalid retained artifact path")?;
        let binding = expected
            .remove(name)
            .context("unexpected or duplicate retained artifact")?;
        ensure!(
            Path::new(&artifact.raw_path)
                .file_name()
                .and_then(|name| name.to_str())
                == name.strip_suffix(".gz"),
            "retained raw/gzip roles disagree"
        );
        ensure!(
            artifact.gzip_bytes == binding.file.bytes
                && artifact.gzip_sha256 == binding.file.sha256
                && artifact.raw_bytes == binding.decompressed_bytes
                && artifact.decompressed_bytes == binding.decompressed_bytes
                && artifact.raw_sha256 == binding.decompressed_sha256
                && artifact.decompressed_sha256 == binding.decompressed_sha256
                && artifact.decompression_verified
                && !artifact.raw_path_status.is_empty(),
            "retention/intake binding mismatch"
        );
        compressed_total = compressed_total
            .checked_add(artifact.gzip_bytes)
            .context("retention byte count overflow")?;
        raw_total = raw_total
            .checked_add(artifact.raw_bytes)
            .context("retention byte count overflow")?;
    }
    ensure!(
        expected.is_empty()
            && compressed_total == retention.compressed_artifact_file_bytes
            && raw_total == retention.raw_artifact_file_bytes,
        "retention conservation mismatch"
    );
    // Storage retirement is provenance, not evidence that tests ran or archives were re-traversed.
    let _retained_storage_state = retention.raw_artifacts_retained;
    Ok(())
}

fn source_matches(source: &SourceSummary, input: &SourceIntake) -> Result<()> {
    let pin = PINS
        .iter()
        .find(|pin| pin.0 == source.id)
        .context("unknown discovery source")?;
    ensure!(
        source.revision == pin.1
            && source.repository == pin.2
            && source.archive.sha256 == pin.3
            && source.archive_bytes > 0,
        "discovery source pin changed"
    );
    ensure!(
        matches!(
            source.coverage.as_str(),
            "canonical" | "unverified_mirror" | "verified_mirror"
        ) && (source.id != "jetbrains-android" || source.coverage != "canonical"),
        "invalid discovery coverage"
    );
    ensure!(
        source.all_members_traversed,
        "retained scanner traversal is incomplete"
    );
    for (role, artifact, binding) in [
        ("members", &source.members, &input.members),
        (
            "physical-members",
            &source.physical_members,
            &input.physical_members,
        ),
        ("candidates", &source.candidates, &input.candidates),
    ] {
        ensure!(
            artifact.path == format!("{}-{role}.jsonl", source.id)
                && binding.file.path == format!("{}.gz", artifact.path)
                && artifact.sha256 == binding.decompressed_sha256,
            "discovery/intake role binding mismatch"
        );
    }
    Ok(())
}

struct ArtifactWriter {
    inner: BufWriter<File>,
    bytes: u64,
}

impl Write for ArtifactWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let next = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| std::io::Error::other("artifact byte count overflow"))?;
        if next > MAX_ARTIFACT_BYTES {
            return Err(std::io::Error::other(
                "artifact byte budget exceeded; discovery remains unresolved",
            ));
        }
        self.inner.write_all(bytes)?;
        self.bytes = next;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn writer(directory: &Path, name: &str) -> Result<ArtifactWriter> {
    Ok(ArtifactWriter {
        inner: BufWriter::new(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(directory.join(name))?,
        ),
        bytes: 0,
    })
}

fn generate_source(
    root: &Path,
    output: &Path,
    scratch: &Path,
    source: &SourceSummary,
    input: &SourceIntake,
    joins: &mut [CanonicalJoin],
    keys: &BTreeMap<JoinKey, Vec<usize>>,
    match_writer: &mut impl Write,
) -> Result<Counts> {
    source_matches(source, input)?;
    let mut counts = Counts::default();
    let mut members_writer = writer(output, &format!("{}-members.jsonl", source.id))?;
    let mut physical_writer = writer(output, &format!("{}-physical-members.jsonl", source.id))?;
    let mut candidates_writer = writer(output, &format!("{}-candidates.jsonl", source.id))?;
    let mut classes_writer = writer(output, &format!("{}-classes.jsonl", source.id))?;
    let mut methods_writer = writer(output, &format!("{}-declarations.jsonl", source.id))?;
    let mut build_writer = writer(output, &format!("{}-build-obligations.jsonl", source.id))?;
    let mut unresolved_writer = writer(output, &format!("{}-unresolved.jsonl", source.id))?;
    let mut member_shards = ShardWriters::new(scratch, "members")?;
    let mut candidate_shards = ShardWriters::new(scratch, "candidates")?;
    let mut metadata_names = BTreeSet::new();
    counts.members = compressed_stream(
        root,
        &input.members,
        |member: Member, ordinal, raw_hash| {
            safe_relative(&member.path, member.kind == "directory")?;
            ensure!(
                valid_hash(&member.sha256, 64)
                    && matches!(
                        member.kind.as_str(),
                        "file" | "directory" | "symlink" | "hardlink" | "global_pax_metadata"
                    ),
                "invalid original member"
            );
            increment(&mut counts.member_kinds, &member.kind)?;
            if member.kind == "file" && member.discovery_classification == "unclassified_payload" {
                checked_increment(&mut counts.unclassified_regular_members)?;
            }
            let id = identity(
                source,
                &member.path,
                &member.sha256,
                "member",
                ordinal,
                None,
            )?;
            json_line(
                &mut members_writer,
                &serde_json::json!({"identity":id,"source":source.id,"revision":source.revision,"record_ordinal":ordinal,"raw_record_sha256":raw_hash,"member":member}),
            )?;
            if member.kind == "global_pax_metadata" {
                ensure!(
                    metadata_names.len() < 8 && metadata_names.insert(member.path.clone()),
                    "duplicate or excessive archive metadata records"
                );
            } else {
                member_shards.push(&IndexItem {
                    path: member.path.clone(),
                    sha256: member.sha256.clone(),
                    bytes: member.bytes,
                    kind: member.kind.clone(),
                    ordinal,
                })?;
            }
            if member.kind != "directory"
                && (member.kind != "file"
                    || member.discovery_classification == "unclassified_payload")
            {
                json_line(
                    &mut unresolved_writer,
                    &serde_json::json!({"parent_identity":id,"scope":"member_discovery","reason":"unclassified payload or unresolved link materialization; no automatic test exclusion","effective_runtime_cases":null}),
                )?;
                checked_increment(&mut counts.unresolved_records)?;
            }
            Ok(())
        },
    )?;
    counts.physical_members = compressed_stream(
        root,
        &input.physical_members,
        |member: PhysicalMember, ordinal, raw_hash| {
            ensure!(
                member.ordinal == ordinal
                    && valid_hash(&member.header_sha256, 64)
                    && valid_hash(&member.payload_sha256, 64),
                "invalid physical member ordinal or hash"
            );
            increment(
                &mut counts.physical_member_types,
                &format!("{:02x}", member.type_byte),
            )?;
            json_line(
                &mut physical_writer,
                &serde_json::json!({"source":source.id,"revision":source.revision,"raw_record_sha256":raw_hash,"physical_member":member}),
            )?;
            Ok(())
        },
    )?;
    counts.candidates = compressed_stream(
        root,
        &input.candidates,
        |candidate: Candidate, ordinal, raw_hash| {
            ensure!(
                candidate.source == source.id && candidate.revision == source.revision,
                "candidate source identity changed"
            );
            safe_relative(&candidate.path, false)?;
            ensure!(
                valid_hash(&candidate.sha256, 64)
                    && matches!(
                        candidate.kind.as_str(),
                        "jvm_source" | "build_runner" | "other_source_or_runner"
                    ),
                "invalid candidate identity or kind"
            );
            increment(&mut counts.candidate_kinds, &candidate.kind)?;
            candidate_shards.push(&IndexItem {
                path: candidate.path.clone(),
                sha256: candidate.sha256.clone(),
                bytes: candidate.bytes,
                kind: candidate.kind.clone(),
                ordinal,
            })?;
            let parent = identity(
                source,
                &candidate.path,
                &candidate.sha256,
                "candidate",
                ordinal,
                None,
            )?;
            json_line(
                &mut candidates_writer,
                &serde_json::json!({"identity":parent,"record_ordinal":ordinal,"raw_record_sha256":raw_hash,"candidate":candidate,"review_state":"unreviewed","effective_runtime_cases":null}),
            )?;
            if candidate.kind == "build_runner" {
                json_line(
                    &mut build_writer,
                    &serde_json::json!({"parent_identity":parent,"source":source.id,"revision":source.revision,"path":candidate.path,"configured_target":null,"module_identity":null,"status":"unresolved","reason":"source build definition is not configured target membership"}),
                )?;
                checked_increment(&mut counts.build_obligations)?;
            }
            json_line(
                &mut unresolved_writer,
                &serde_json::json!({"parent_identity":parent,"scope":"source_membership","original_reasons":candidate.unresolved,"reason":"loaded classpath, configured target, non-JVM dispatch and generated inputs remain unresolved","effective_runtime_cases":null}),
            )?;
            checked_increment(&mut counts.unresolved_records)?;
            let lexical_input_unresolved = candidate.unresolved.iter().any(|reason| {
                reason.starts_with("source exceeds ") || reason.starts_with("non-UTF-8 source;")
            });
            if lexical_input_unresolved {
                checked_increment(&mut counts.files_with_unresolved_lexing)?;
            }
            if let Some(declarations) = &candidate.declarations {
                if !declarations.unresolved.is_empty() {
                    checked_increment(&mut counts.files_with_unresolved_lexing)?;
                }
                json_line(
                    &mut unresolved_writer,
                    &serde_json::json!({"parent_identity":parent,"scope":"lexical_remainder","original_reasons":declarations.unresolved,"declaration_discovery_complete":false}),
                )?;
                checked_increment(&mut counts.unresolved_records)?;
                for (child, class) in declarations.classes.iter().enumerate() {
                    let id = identity(
                        source,
                        &candidate.path,
                        &candidate.sha256,
                        "class",
                        ordinal,
                        Some(child),
                    )?;
                    json_line(
                        &mut classes_writer,
                        &serde_json::json!({"identity":id,"parent_identity":parent,"class_ordinal":child,"class":class,"effective_suite":null,"expansion_status":"unresolved","expansion_reason":"abstract/nested/helper classification, inheritance and custom runner descriptions remain unresolved"}),
                    )?;
                    checked_increment(&mut counts.classes)?;
                }
                for (child, method) in declarations.methods.iter().enumerate() {
                    ensure!(
                        method.effective_runtime_cases.is_none(),
                        "candidate illegally claims runtime expansion"
                    );
                    let id = identity(
                        source,
                        &candidate.path,
                        &candidate.sha256,
                        "method",
                        ordinal,
                        Some(child),
                    )?;
                    json_line(
                        &mut methods_writer,
                        &serde_json::json!({"identity":id,"parent_identity":parent,"method_ordinal":child,"declaration":method,"applicability":"unresolved","expansion_status":"unresolved","expansion_reason":"source candidate may be a helper; inherited, parameterized, factory, custom and conditional runner dispatch remain unresolved"}),
                    )?;
                    checked_increment(&mut counts.methods)?;
                    if let Some(suite) = &method.suite_candidate {
                        let key = (
                            source.id.clone(),
                            candidate.path.clone(),
                            candidate.sha256.clone(),
                            suite.clone(),
                            method.name.clone(),
                            method.line,
                        );
                        if let Some(indices) = keys.get(&key) {
                            for index in indices {
                                let join = joins
                                    .get_mut(*index)
                                    .context("invalid canonical join index")?;
                                join.count = join
                                    .count
                                    .checked_add(1)
                                    .context("canonical match count overflow")?;
                                if join.first_identity.is_none() {
                                    join.first_identity = Some(id.clone());
                                }
                                json_line(
                                    match_writer,
                                    &serde_json::json!({"reference_id":join.test.id,"declaration_identity":id,"binding_kind":"exact_source_hash_name_owner_line_candidate","runtime_verified":false}),
                                )?;
                                checked_increment(&mut counts.canonical_matches)?;
                            }
                        }
                    }
                }
            }
            Ok(())
        },
    )?;
    let member_paths = member_shards.finish()?;
    let candidate_paths = candidate_shards.finish()?;
    conserve_shards(&member_paths, &candidate_paths)?;
    ensure!(
        counts.member_kinds == source.member_kinds
            && counts.physical_member_types == source.physical_member_types
            && counts.candidate_kinds == source.candidate_kinds
            && counts.methods == source.method_candidates
            && counts.files_with_unresolved_lexing == source.files_with_unresolved_lexing
            && counts.unclassified_regular_members == source.unclassified_regular_members,
        "retained summary conservation mismatch: {}",
        source.id
    );
    for writer in [
        &mut members_writer,
        &mut physical_writer,
        &mut candidates_writer,
        &mut classes_writer,
        &mut methods_writer,
        &mut build_writer,
        &mut unresolved_writer,
    ] {
        writer.flush()?;
    }
    Ok(counts)
}

fn file_binding(path: &Path, name: String) -> Result<Binding> {
    let (mut file, state) = regular_file(path)?;
    ensure!(
        state.length <= MAX_ARTIFACT_BYTES,
        "artifact exceeds byte budget"
    );
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
            .context("artifact length overflow")?;
    }
    unchanged(&file, path, &state)?;
    Ok(Binding {
        path: name,
        bytes,
        sha256: format!("{:x}", hash.finalize()),
    })
}

fn artifact_names(directory: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        ensure!(
            entry.file_type()?.is_file(),
            "unexpected output directory or symlink"
        );
        names.push(
            entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("non-UTF-8 output name"))?,
        );
    }
    names.sort();
    Ok(names)
}

fn generate(
    repository: &Path,
    root: &Path,
    intake_path: &Path,
    intake_sha256: &str,
    output: &Path,
    scratch: &Path,
) -> Result<()> {
    ensure!(valid_hash(intake_sha256, 64), "invalid intake SHA-256");
    let metadata = fs::symlink_metadata(intake_path)?;
    let binding = Binding {
        path: "intake.json".to_owned(),
        bytes: metadata.len(),
        sha256: intake_sha256.to_owned(),
    };
    let intake: Intake =
        serde_json::from_slice(&bound_bytes(intake_path, &binding, MAX_DESCRIPTOR_BYTES)?)?;
    ensure!(
        intake.schema_version == 1 && intake.sources.len() == PINS.len(),
        "unsupported or incomplete intake"
    );
    ensure!(
        intake.manifest.path == "docs/android-studio/reference-manifest.json"
            && intake.ledger.path == "docs/android-studio/test-parity.json",
        "intake must bind this checkout's canonical matrix"
    );
    ensure!(
        intake.summary.file.path == "summary.json.gz"
            && intake.retention.path == "inventory-retention.json",
        "invalid descriptor role"
    );
    let mut intake_sources = BTreeSet::new();
    for source in &intake.sources {
        ensure!(
            PINS.iter().any(|pin| pin.0 == source.id) && intake_sources.insert(&source.id),
            "unknown or duplicate intake source"
        );
        for (role, binding) in [
            ("members", &source.members),
            ("physical-members", &source.physical_members),
            ("candidates", &source.candidates),
        ] {
            ensure!(
                binding.file.path == format!("{}-{role}.jsonl.gz", source.id),
                "intake source role changed"
            );
        }
    }
    let mut input_guards = vec![(intake_path.to_owned(), binding, physical_state(&metadata))];
    for (path, binding, maximum) in std::iter::once((
        root.join(&intake.retention.path),
        &intake.retention,
        MAX_DESCRIPTOR_BYTES,
    ))
    .chain(std::iter::once((
        root.join(&intake.summary.file.path),
        &intake.summary.file,
        MAX_DESCRIPTOR_BYTES,
    )))
    .chain(
        intake
            .sources
            .iter()
            .flat_map(|source| {
                [
                    &source.members,
                    &source.physical_members,
                    &source.candidates,
                ]
            })
            .map(|binding| {
                (
                    root.join(&binding.file.path),
                    &binding.file,
                    MAX_STREAM_BYTES,
                )
            }),
    )
    .chain([
        (
            repository.join(&intake.manifest.path),
            &intake.manifest,
            MAX_MATRIX_BYTES,
        ),
        (
            repository.join(&intake.ledger.path),
            &intake.ledger,
            MAX_MATRIX_BYTES,
        ),
    ]) {
        validate_binding(binding, maximum)?;
        let (_, state) = regular_file(&path)?;
        ensure!(
            state.length == binding.bytes,
            "input length changed before discovery"
        );
        input_guards.push((path, binding.clone(), state));
    }
    let retention: Retention = serde_json::from_slice(&bound_bytes(
        &root.join(&intake.retention.path),
        &intake.retention,
        MAX_DESCRIPTOR_BYTES,
    )?)?;
    check_retention(&intake, &retention)?;
    let summary: DiscoverySummary = compressed_document(root, &intake.summary)?;
    ensure!(
        summary.schema_version == 1
            && summary.sources.len() == PINS.len()
            && !summary.effective_test_census_complete,
        "unsupported or promoted source census"
    );
    let matrix: Matrix = serde_json::from_slice(&bound_bytes(
        &repository.join(&intake.manifest.path),
        &intake.manifest,
        MAX_MATRIX_BYTES,
    )?)?;
    let ledger: Ledger = serde_json::from_slice(&bound_bytes(
        &repository.join(&intake.ledger.path),
        &intake.ledger,
        MAX_MATRIX_BYTES,
    )?)?;
    let (baseline, mut joins, keys) = matrix_joins(matrix, ledger)?;
    let mut sources = BTreeMap::new();
    let mut match_writer = writer(output, "canonical-matches.jsonl")?;
    let mut seen = BTreeSet::new();
    for source in &summary.sources {
        ensure!(seen.insert(&source.id), "duplicate discovery source");
        let inputs = intake
            .sources
            .iter()
            .filter(|input| input.id == source.id)
            .collect::<Vec<_>>();
        ensure!(inputs.len() == 1, "missing or duplicate intake source");
        let input = inputs.first().context("missing intake source")?;
        let source_scratch = scratch.join(&source.id);
        fs::create_dir(&source_scratch)?;
        sources.insert(
            source.id.clone(),
            generate_source(
                root,
                output,
                &source_scratch,
                source,
                input,
                &mut joins,
                &keys,
                &mut match_writer,
            )?,
        );
    }
    match_writer.flush()?;
    let mut canonical_writer = writer(output, "canonical.jsonl")?;
    let mut unresolved_count = 0;
    for join in &joins {
        let resolved = join.count == 1;
        if !resolved {
            unresolved_count += 1;
        }
        json_line(
            &mut canonical_writer,
            &serde_json::json!({"reference_id":join.test.id,"reference_test":join.test,"ledger_entry":join.entry,"historical_evidence_only":true,"matching_candidate_count":join.count,"declaration_identity":if resolved { join.first_identity.as_deref() } else { None },"reconciliation_status":if resolved { "source_candidate_bound_runtime_unresolved" } else { "unresolved_zero_or_multiple_source_candidates" },"effective_runtime_cases":null,"new_behavioral_parity_credit":0}),
        )?;
    }
    canonical_writer.flush()?;
    let mut artifacts = Vec::new();
    for name in artifact_names(output)? {
        artifacts.push(file_binding(&output.join(&name), name)?);
    }
    for (path, binding, state) in &input_guards {
        ensure!(
            physical_state(&fs::symlink_metadata(path)?) == *state,
            "immutable input physical metadata changed before closure: {}",
            path.display()
        );
        let actual = file_binding(path, binding.path.clone())?;
        ensure!(
            actual.bytes == binding.bytes
                && actual.sha256 == binding.sha256
                && physical_state(&fs::symlink_metadata(path)?) == *state,
            "immutable input hash or physical metadata changed at closure: {}",
            path.display()
        );
    }
    let output_summary = OutputSummary {
        schema_version: 1,
        scope: "Conserved source discovery obligations and unchanged canonical rows; effective runner census remains incomplete",
        intake_sha256,
        intake: &intake,
        retention: &retention,
        manifest: &intake.manifest,
        ledger: &intake.ledger,
        baseline: &baseline,
        canonical_rows: joins.len(),
        canonical_join_unresolved: unresolved_count,
        census_complete: false,
        effective_runtime_cases: None,
        new_behavioral_parity_credit: 0,
        original_archive_traversal_performed: false,
        source_discovery: &summary,
        sources,
        artifacts,
    };
    let mut summary_writer = writer(output, "summary.json")?;
    serde_json::to_writer_pretty(&mut summary_writer, &output_summary)?;
    summary_writer.write_all(b"\n")?;
    summary_writer.flush()?;
    Ok(())
}

fn compare_outputs(expected: &Path, actual: &Path) -> Result<()> {
    let names = artifact_names(expected)?;
    ensure!(
        names == artifact_names(actual)? && names.iter().any(|name| name == "summary.json"),
        "output artifact set or completed summary differs"
    );
    for name in names {
        let first = file_binding(&expected.join(&name), name.clone())?;
        let second = file_binding(&actual.join(&name), name.clone())?;
        ensure!(
            first.bytes == second.bytes && first.sha256 == second.sha256,
            "output changed: {name}"
        );
    }
    Ok(())
}

fn fresh_stage(parent: &Path) -> Result<PathBuf> {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    for attempt in 0..32 {
        let path = parent.join(format!(
            ".android-reference-obligations-{}-{stamp}-{attempt}",
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    bail!("cannot reserve fresh staging directory")
}

fn checked_directory(path: &Path) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "directory path must be absolute");
    let mut prefix = PathBuf::new();
    for component in path.components() {
        ensure!(
            !matches!(component, Component::ParentDir | Component::CurDir),
            "non-normal directory path"
        );
        prefix.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&prefix)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "directory component is not a regular directory: {}",
            prefix.display()
        );
    }
    Ok(path.to_owned())
}

pub fn run(args: AndroidReferenceObligationsArgs) -> Result<()> {
    ensure!(
        args.repository.is_absolute()
            && args.input_root.is_absolute()
            && args.intake.is_absolute()
            && args.output.is_absolute(),
        "all CLI paths must be absolute"
    );
    let repository = checked_directory(&args.repository)?;
    let input_root = checked_directory(&args.input_root)?;
    let parent = checked_directory(
        args.output
            .parent()
            .context("output needs an existing parent")?,
    )?;
    ensure!(
        !parent.starts_with(&repository) && !parent.starts_with(&input_root),
        "output must be outside the checkout and immutable inventories"
    );
    let output = parent.join(
        args.output
            .file_name()
            .context("output needs a directory name")?,
    );
    if args.check {
        ensure!(
            fs::symlink_metadata(&output)?.is_dir()
                && !fs::symlink_metadata(&output)?.file_type().is_symlink(),
            "check output must be an existing regular directory"
        );
    } else {
        ensure!(
            fs::symlink_metadata(&output)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
            "output already exists or cannot be inspected"
        );
    }
    let stage = fresh_stage(&parent)?;
    let artifacts = stage.join("artifacts");
    let scratch = stage.join("conservation");
    fs::create_dir(&artifacts)?;
    fs::create_dir(&scratch)?;
    let result: Result<()> = (|| {
        generate(
            &repository,
            &input_root,
            &args.intake,
            &args.intake_sha256,
            &artifacts,
            &scratch,
        )?;
        if args.check {
            compare_outputs(&output, &artifacts)?;
        } else {
            fs::create_dir(&output).context("reserving output without replacement")?;
            let mut names = artifact_names(&artifacts)?;
            names.retain(|name| name != "summary.json");
            names.push("summary.json".to_owned());
            for name in names {
                let source_path = artifacts.join(&name);
                let (mut source, state) = regular_file(&source_path)?;
                let mut destination = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(output.join(name))?;
                ensure!(
                    std::io::copy(&mut source, &mut destination)? == state.length,
                    "staged artifact length changed while publishing"
                );
                destination.flush()?;
                unchanged(&source, &source_path, &state)?;
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            fs::remove_dir_all(&stage)
                .context("removing only this completed invocation's staging directory")?;
            println!(
                "Conserved reference obligations; census incomplete, runtime cases unknown, new behavior credit 0."
            );
            Ok(())
        }
        Err(error) => Err(error.context(format!(
            "incomplete staging retained at {}; no complete census or parity credit",
            stage.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use serde_json::{Value, json};
    use tempfile::TempDir;

    const ORIGINAL_BASE_CANDIDATES: &[u8] =
        include_bytes!("../../test_data/reference_obligations/base-candidates.jsonl");
    const ORIGINAL_BASE_MEMBERS: &[u8] =
        include_bytes!("../../test_data/reference_obligations/base-members.jsonl");
    const ORIGINAL_IDEA_CANDIDATES: &[u8] =
        include_bytes!("../../test_data/reference_obligations/idea-candidates.jsonl");
    const ORIGINAL_IDEA_MEMBERS: &[u8] =
        include_bytes!("../../test_data/reference_obligations/idea-members.jsonl");

    fn records<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<Vec<T>> {
        std::str::from_utf8(bytes)?
            .lines()
            .map(|line| Ok(serde_json::from_str(line)?))
            .collect()
    }

    fn fixture_records() -> Result<(Vec<Candidate>, Vec<(String, Member)>)> {
        let mut candidates = records(ORIGINAL_BASE_CANDIDATES)?;
        candidates.extend(records::<Candidate>(ORIGINAL_IDEA_CANDIDATES)?);
        let mut members = records::<Member>(ORIGINAL_BASE_MEMBERS)?
            .into_iter()
            .map(|member| ("base".to_owned(), member))
            .collect::<Vec<_>>();
        members.extend(
            records::<Member>(ORIGINAL_IDEA_MEMBERS)?
                .into_iter()
                .map(|member| ("idea".to_owned(), member)),
        );
        Ok((candidates, members))
    }

    fn jsonl<T: Serialize>(values: &[T]) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        for value in values {
            json_line(&mut bytes, value)?;
        }
        Ok(bytes)
    }

    fn write_binding(root: &Path, path: &str, bytes: &[u8]) -> Result<Binding> {
        let destination = root.join(path);
        fs::create_dir_all(destination.parent().context("fixture needs parent")?)?;
        fs::write(destination, bytes)?;
        Ok(Binding {
            path: path.to_owned(),
            bytes: bytes.len() as u64,
            sha256: digest(bytes),
        })
    }

    fn gzip_binding(root: &Path, path: &str, raw: &[u8]) -> Result<CompressedBinding> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(raw)?;
        let bytes = encoder.finish()?;
        Ok(CompressedBinding {
            file: write_binding(root, path, &bytes)?,
            decompressed_bytes: raw.len() as u64,
            decompressed_sha256: digest(raw),
        })
    }

    fn fixture_sources() -> Vec<MatrixSource> {
        PINS.iter()
            .map(|pin| MatrixSource {
                id: pin.0.to_owned(),
                repository: pin.2.to_owned(),
                revision: pin.1.to_owned(),
                tag: (pin.0 == "jetbrains-android").then(|| "idea/262.9437.185".to_owned()),
                license: "Apache-2.0".to_owned(),
                archive: ReferenceArtifact {
                    path: format!("{}.tar.gz", pin.0),
                    sha256: pin.3.to_owned(),
                },
                coverage: if pin.0 == "jetbrains-android" {
                    "unverified_mirror"
                } else {
                    "canonical"
                }
                .to_owned(),
                coverage_reason:
                    "Pinned original discovery fixture; configured execution remains unverified"
                        .to_owned(),
                coverage_evidence: None,
                census_complete: false,
                census_evidence: None,
            })
            .collect()
    }

    fn canonical_test(
        candidate: &Candidate,
        method: &MethodDeclaration,
        case: Option<&str>,
    ) -> Result<ReferenceTest> {
        let suite = method
            .suite_candidate
            .clone()
            .context("fixture method needs owner")?;
        let suffix = case.map(|value| format!("[{value}]")).unwrap_or_default();
        Ok(ReferenceTest {
            id: format!(
                "{}@{}:{}#{}.{}{}",
                candidate.source, candidate.revision, candidate.path, suite, method.name, suffix
            ),
            source: candidate.source.clone(),
            suite,
            method: method.name.clone(),
            case: case.map(str::to_owned),
            file: ReferenceArtifact {
                path: candidate.path.clone(),
                sha256: candidate.sha256.clone(),
            },
            line: method.line,
            declaration: method.declaration.clone(),
            copyright: "Copyright The Android Open Source Project; Apache-2.0".to_owned(),
            fixtures: Vec::new(),
            fixture_reason: Some(
                "Discovery-only fixture; no original behavioral port is claimed".to_owned(),
            ),
        })
    }

    fn entry(test: &ReferenceTest) -> Entry {
        Entry {
            reference_id: test.id.clone(),
            port_status: "unported".to_owned(),
            run_status: "not_run".to_owned(),
            reason: None,
            run_blocker: None,
            rust_targets: Vec::new(),
            fixtures: Vec::new(),
        }
    }

    struct Harness {
        temporary: TempDir,
        repository: PathBuf,
        inputs: PathBuf,
        intake_path: PathBuf,
        intake: Intake,
        summary: DiscoverySummary,
        matrix: Matrix,
        ledger: Ledger,
        intake_hash: String,
    }

    impl Harness {
        fn new(candidates: Vec<Candidate>, members: Vec<(String, Member)>) -> Result<Self> {
            let temporary = TempDir::new()?;
            let repository = temporary.path().join("repository");
            let inputs = temporary.path().join("inputs");
            fs::create_dir(&repository)?;
            fs::create_dir(&inputs)?;
            let mut source_inputs = Vec::new();
            let mut source_summaries = Vec::new();
            for pin in PINS {
                let source_candidates = candidates
                    .iter()
                    .filter(|candidate| candidate.source == pin.0)
                    .collect::<Vec<_>>();
                let source_members = members
                    .iter()
                    .filter(|(source, _)| source == pin.0)
                    .map(|(_, member)| member)
                    .collect::<Vec<_>>();
                let mut member_kinds = BTreeMap::new();
                let mut candidate_kinds = BTreeMap::new();
                let mut physical_member_types = BTreeMap::new();
                let mut method_candidates = 0;
                let mut files_with_unresolved_lexing = 0;
                let mut unclassified_regular_members = 0;
                for member in &source_members {
                    increment(&mut member_kinds, &member.kind)?;
                    if member.kind == "file"
                        && member.discovery_classification == "unclassified_payload"
                    {
                        unclassified_regular_members += 1;
                    }
                }
                for candidate in &source_candidates {
                    increment(&mut candidate_kinds, &candidate.kind)?;
                    if candidate.unresolved.iter().any(|reason| {
                        reason.starts_with("source exceeds ")
                            || reason.starts_with("non-UTF-8 source;")
                    }) {
                        files_with_unresolved_lexing += 1;
                    }
                    if let Some(declarations) = &candidate.declarations {
                        method_candidates += declarations.methods.len() as u64;
                        if !declarations.unresolved.is_empty() {
                            files_with_unresolved_lexing += 1;
                        }
                    }
                }
                // Only the transport is synthetic. Original source bytes and scanner records are unchanged.
                let physical = source_members
                    .iter()
                    .enumerate()
                    .map(|(ordinal, member)| PhysicalMember {
                        ordinal: ordinal as u64,
                        type_byte: b'0',
                        raw_path_bytes: member.archive_path.as_bytes().to_vec(),
                        header_sha256: digest(b"explicit synthetic unit-test transport header"),
                        bytes: member.bytes,
                        payload_sha256: member.sha256.clone(),
                    })
                    .collect::<Vec<_>>();
                if !physical.is_empty() {
                    physical_member_types.insert("30".to_owned(), physical.len() as u64);
                }
                let member_binding = gzip_binding(
                    &inputs,
                    &format!("{}-members.jsonl.gz", pin.0),
                    &jsonl(&source_members)?,
                )?;
                let physical_binding = gzip_binding(
                    &inputs,
                    &format!("{}-physical-members.jsonl.gz", pin.0),
                    &jsonl(&physical)?,
                )?;
                let candidate_binding = gzip_binding(
                    &inputs,
                    &format!("{}-candidates.jsonl.gz", pin.0),
                    &jsonl(&source_candidates)?,
                )?;
                source_summaries.push(SourceSummary {
                    id: pin.0.to_owned(),
                    revision: pin.1.to_owned(),
                    repository: pin.2.to_owned(),
                    coverage: if pin.0 == "jetbrains-android" {
                        "unverified_mirror"
                    } else {
                        "canonical"
                    }
                    .to_owned(),
                    archive: ReferenceArtifact {
                        path: format!("{}.tar.gz", pin.0),
                        sha256: pin.3.to_owned(),
                    },
                    archive_bytes: 1,
                    all_members_traversed: true,
                    physical_member_types,
                    member_kinds,
                    candidate_kinds,
                    method_candidates,
                    files_with_unresolved_lexing,
                    unclassified_regular_members,
                    members: ReferenceArtifact {
                        path: format!("{}-members.jsonl", pin.0),
                        sha256: member_binding.decompressed_sha256.clone(),
                    },
                    physical_members: ReferenceArtifact {
                        path: format!("{}-physical-members.jsonl", pin.0),
                        sha256: physical_binding.decompressed_sha256.clone(),
                    },
                    candidates: ReferenceArtifact {
                        path: format!("{}-candidates.jsonl", pin.0),
                        sha256: candidate_binding.decompressed_sha256.clone(),
                    },
                });
                source_inputs.push(SourceIntake {
                    id: pin.0.to_owned(),
                    members: member_binding,
                    physical_members: physical_binding,
                    candidates: candidate_binding,
                });
            }
            let tests = candidates
                .iter()
                .filter(|candidate| {
                    candidate.path.ends_with("DefaultVariantsTest.kt")
                        || candidate.path.ends_with("TestGroupTest.kt")
                })
                .flat_map(|candidate| {
                    candidate.declarations.iter().flat_map(move |declarations| {
                        declarations
                            .methods
                            .iter()
                            .filter(|method| {
                                method
                                    .annotations
                                    .iter()
                                    .any(|annotation| annotation == "Test")
                            })
                            .map(move |method| canonical_test(candidate, method, None))
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let ledger = Ledger {
                schema_version: 1,
                entries: tests.iter().map(entry).collect(),
            };
            let matrix = Matrix { schema_version: 1, baseline: "Synthetic transport of bounded original declaration fixtures; zero behavior credit".to_owned(), census_complete: false, sources: fixture_sources(), tests };
            let summary = DiscoverySummary { schema_version: 1, baseline: matrix.baseline.clone(), effective_test_census_complete: false, discovery: "Synthetic unit-test transport; complete pinned source fixtures are not an effective upstream runner census".to_owned(), unresolved: vec!["Configured runner membership and generated/inherited/parameter expansion unresolved".to_owned()], sources: source_summaries };
            let manifest_binding = write_binding(
                &repository,
                "docs/android-studio/reference-manifest.json",
                &serde_json::to_vec_pretty(&matrix)?,
            )?;
            let ledger_binding = write_binding(
                &repository,
                "docs/android-studio/test-parity.json",
                &serde_json::to_vec_pretty(
                    &json!({"schema_version":ledger.schema_version,"entries":ledger.entries}),
                )?,
            )?;
            let summary_binding = gzip_binding(
                &inputs,
                "summary.json.gz",
                &serde_json::to_vec_pretty(&summary)?,
            )?;
            let placeholder = Binding {
                path: "inventory-retention.json".to_owned(),
                bytes: 0,
                sha256: digest(b""),
            };
            let intake = Intake {
                schema_version: 1,
                summary: summary_binding,
                retention: placeholder,
                sources: source_inputs,
                manifest: manifest_binding,
                ledger: ledger_binding,
            };
            let intake_path = temporary.path().join("intake.json");
            let mut harness = Self {
                temporary,
                repository,
                inputs,
                intake_path,
                intake,
                summary,
                matrix,
                ledger,
                intake_hash: String::new(),
            };
            harness.refresh()?;
            Ok(harness)
        }

        fn originals() -> Result<Self> {
            let (candidates, members) = fixture_records()?;
            Self::new(candidates, members)
        }

        fn refresh(&mut self) -> Result<()> {
            self.intake.manifest = write_binding(
                &self.repository,
                "docs/android-studio/reference-manifest.json",
                &serde_json::to_vec_pretty(&self.matrix)?,
            )?;
            self.intake.ledger = write_binding(
                &self.repository,
                "docs/android-studio/test-parity.json",
                &serde_json::to_vec_pretty(
                    &json!({"schema_version":self.ledger.schema_version,"entries":self.ledger.entries}),
                )?,
            )?;
            self.intake.summary = gzip_binding(
                &self.inputs,
                "summary.json.gz",
                &serde_json::to_vec_pretty(&self.summary)?,
            )?;
            let mut artifacts = Vec::new();
            for binding in std::iter::once(&self.intake.summary).chain(
                self.intake.sources.iter().flat_map(|source| {
                    [
                        &source.members,
                        &source.physical_members,
                        &source.candidates,
                    ]
                }),
            ) {
                artifacts.push(RetainedArtifact {
                    raw_path: format!(
                        "/synthetic/{}",
                        binding
                            .file
                            .path
                            .strip_suffix(".gz")
                            .context("fixture gzip suffix")?
                    ),
                    raw_bytes: binding.decompressed_bytes,
                    raw_sha256: binding.decompressed_sha256.clone(),
                    gzip_path: format!("/synthetic/{}", binding.file.path),
                    gzip_bytes: binding.file.bytes,
                    gzip_sha256: binding.file.sha256.clone(),
                    decompressed_bytes: binding.decompressed_bytes,
                    decompressed_sha256: binding.decompressed_sha256.clone(),
                    decompression_verified: true,
                    raw_path_status: "synthetic-unit-test-transport".to_owned(),
                });
            }
            let retention = Retention {
                source_commit: "0".repeat(40),
                rust_cli_sha256: "0".repeat(64),
                raw_artifact_file_bytes: artifacts.iter().map(|artifact| artifact.raw_bytes).sum(),
                compressed_artifact_file_bytes: artifacts
                    .iter()
                    .map(|artifact| artifact.gzip_bytes)
                    .sum(),
                compression:
                    "Synthetic Rust-generated gzip wrappers, original source records unchanged"
                        .to_owned(),
                raw_artifacts_retained: true,
                artifacts,
                raw_path_status: "synthetic-unit-test-transport".to_owned(),
                retirement_receipt: "no-original-artifact-retirement".to_owned(),
            };
            self.intake.retention = write_binding(
                &self.inputs,
                "inventory-retention.json",
                &serde_json::to_vec_pretty(&retention)?,
            )?;
            let bytes = serde_json::to_vec_pretty(&self.intake)?;
            fs::write(&self.intake_path, &bytes)?;
            self.intake_hash = digest(&bytes);
            Ok(())
        }

        fn args(&self, name: &str, check: bool) -> AndroidReferenceObligationsArgs {
            AndroidReferenceObligationsArgs {
                repository: self.repository.clone(),
                input_root: self.inputs.clone(),
                intake: self.intake_path.clone(),
                intake_sha256: self.intake_hash.clone(),
                output: self.temporary.path().join(name),
                check,
            }
        }

        fn output_json(&self, name: &str) -> Result<Value> {
            Ok(serde_json::from_slice(&fs::read(
                self.temporary.path().join("output").join(name),
            )?)?)
        }

        fn output_rows(&self, name: &str) -> Result<Vec<Value>> {
            records(&fs::read(self.temporary.path().join("output").join(name))?)
        }
    }

    const ORIGINAL_ATTRIBUTION_BINDINGS: [(&str, &str, u64, &str); 17] = [
        (
            "base",
            "testutils/BUILD",
            4107,
            "c1c78be6ae712f5c346525a5cb4318d2fd55d8da99b4fad7c7271f17fc5617f1",
        ),
        (
            "base",
            "testutils/src/main/java/com/android/testutils/JarTestSuiteRunner.java",
            10038,
            "da503cfd8096ae4be970e29f5a96d882bbc6eeb31f0c1ec8a47bed9a7d2abc3a",
        ),
        (
            "base",
            "testutils/src/main/java/com/android/testutils/TestGroup.java",
            10390,
            "6d384e55511e1208a6a1c2e950f02acfef4f12f6b059d4e0f76c149db7cc415a",
        ),
        (
            "base",
            "testutils/src/test/java/com/android/testutils/TestGroupTest.kt",
            3006,
            "0f6a26754d7afa899e37e82936450f33fbad2339aa907924468c8c31dddf4743",
        ),
        (
            "idea",
            "adt-testutils/src/main/java/com/android/tools/tests/IdeaTestSuiteBase.java",
            9419,
            "26cd53f9592d29bc21f531c7e22c142e29922b412a31bd3937a71d720b0614c4",
        ),
        (
            "idea",
            "adt-testutils/src/main/java/com/android/tools/tests/LastInIdeaTestSuite.kt",
            2053,
            "67b883002362195992b7111d0e8d88cda30ccfeeae355a2ada761bb83f95c274",
        ),
        (
            "idea",
            "adt-ui/src/test/java/com/android/tools/adtui/AdtUiTestSuite.java",
            879,
            "16a828d742ef6bec8b5cf52c813624c3c3a99d97dafcc40f9e1c5b89d06462b5",
        ),
        (
            "idea",
            "adt-ui/src/test/java/com/android/tools/adtui/common/ColorPaletteManagerTest.kt",
            5447,
            "33d296249b80ad61a7de3dd25abdf44779cd2bd6ade5847024d0b30b2cb6d09e",
        ),
        (
            "idea",
            "android/gradle/testSrc/com/android/tools/idea/gradle/project/GradleModuleImportTest.java",
            16649,
            "e63aa8bcd300922c731ba1c27be470dcafd808021c8906082d62f8f0e7e6c762",
        ),
        (
            "idea",
            "project-system-gradle-sync/BUILD",
            3721,
            "5c82bd87f7cabac58ef3752e4062d03faeab4f0f9b20e88399b50207c197ff26",
        ),
        (
            "idea",
            "project-system-gradle-sync/testSrc/com/android/tools/idea/gradle/project/sync/DefaultVariantsTest.kt",
            4006,
            "c21c3dbc6fe94377ae7e82b2028d357c0ce0285793eecafa65140df63cc84b15",
        ),
        (
            "idea",
            "project-system-gradle-sync/testSrc/com/android/tools/idea/gradle/project/sync/InternedModelsTest.kt",
            15928,
            "550d9f4399f7d0c50c7d7b2b4120d9bc246555ac816daed16e2db2c43865634f",
        ),
        (
            "idea",
            "project-system-gradle-sync/testSrc/com/android/tools/idea/gradle/project/sync/ModelResultTest.kt",
            3246,
            "cee8d470ed6d0ca5027a8be2fe83d63952a7f1da4ff0855d1c82b3072f85754a",
        ),
        (
            "idea",
            "project-system-gradle-sync/testSrc/com/android/tools/idea/gradle/project/sync/ModelVersionsTest.kt",
            3273,
            "9239c0688b3066bf964d801ba7b231dd5f70719a97331645fd53f10060252aef",
        ),
        (
            "idea",
            "project-system-gradle-sync/testSrc/com/android/tools/idea/gradle/project/sync/PhasedSyncVariantNameResolutionTest.kt",
            61975,
            "d0d75603108b990ec8b803d68128c23d65fed32b35abfc246fd8e30296125e7d",
        ),
        (
            "idea",
            "project-system-gradle-sync/testSrc/com/android/tools/idea/gradle/project/sync/VariantNameResolutionTest.kt",
            3924,
            "5fcb2361a523b78e4b2df65fb6d480b2a145aaaad88a8347d0b26e65ca3a7dfe",
        ),
        (
            "idea",
            "project-system-gradle-sync/testSrc/com/android/tools/idea/projectsystem/gradle/sync/GradleProjectSystemSyncTestSuite.java",
            1319,
            "419a1a1dcda7e59c0a145439117680df5fc378d31dd89e972cf9c338d935518b",
        ),
    ];

    const COMPLETE_AOSP_LICENSE_HEADER: &str = concat!(
        " *\n",
        " * Licensed under the Apache License, Version 2.0 (the \"License\");\n",
        " * you may not use this file except in compliance with the License.\n",
        " * You may obtain a copy of the License at\n",
        " *\n",
        " *      http://www.apache.org/licenses/LICENSE-2.0\n",
        " *\n",
        " * Unless required by applicable law or agreed to in writing, software\n",
        " * distributed under the License is distributed on an \"AS IS\" BASIS,\n",
        " * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.\n",
        " * See the License for the specific language governing permissions and\n",
        " * limitations under the License.\n",
        " */\n",
    );

    fn inherited_license_proof(source: &str) -> Result<Value> {
        let pin = PINS
            .iter()
            .find(|pin| pin.0 == source)
            .context("license source pin")?;
        let (
            path,
            ordinal,
            record_hash,
            inventory_bytes,
            inventory_hash,
            compressed_bytes,
            compressed_hash,
        ) = match source {
            "base" => (
                "apkparser/binary-resources/LICENSE",
                991_u64,
                "ca47f0e5e51049ce22c8d17dfb8539fe7108c60187b09cce11854540ca73b4ab",
                11_234_145_u64,
                "c30bd69ecc3391916bbc97b6a2deb801ad108fb598b003262f35e77ea444df34",
                1_295_243_u64,
                "67645f1b568eababc853798f02fb52e1674b6030f9aec638f7659d8a292dcfad",
            ),
            "idea" => (
                "aswb/LICENSE",
                56_599_u64,
                "ec59cf331ea5a8558f3eda6e309c307d56a1889c73e92109558b4e08b0f0a001",
                29_442_713_u64,
                "425511c489f57e32065c185441007ba8d9656521c7e2df4fe8e25e58988a9d0d",
                3_286_690_u64,
                "c2f5ec02bf26d5254322b284eb67cfd797058679a30958a5dde22f5858632f9a",
            ),
            _ => bail!("no inherited license proof for this original source"),
        };
        Ok(json!({
            "source": source,
            "repository": pin.2,
            "revision": pin.1,
            "archive_sha256": pin.3,
            "original_license_path": path,
            "member_record_ordinal": ordinal,
            "member_record_sha256": record_hash,
            "members_inventory": {
                "file": {"path": format!("{source}-members.jsonl.gz"), "bytes": compressed_bytes, "sha256": compressed_hash},
                "decompressed_bytes": inventory_bytes,
                "decompressed_sha256": inventory_hash,
            },
            "fixture": {
                "path": "../reference_declarations/LICENSE-APACHE-2.0.txt",
                "bytes": 11_358,
                "sha256": "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30",
            },
            "scope": "Canonical repository Apache-2.0 declaration; full license text matches a pinned repository member. The license member's module is evidence of the text, not a claim of module-wide scope over another directory.",
        }))
    }

    fn verify_original_license_attribution(
        root: &Path,
        sources: &[MatrixSource],
        original: &Value,
        bytes: &[u8],
    ) -> Result<()> {
        let source = original["source"].as_str().context("attribution source")?;
        let path = original["path"].as_str().context("attribution path")?;
        let expected = ORIGINAL_ATTRIBUTION_BINDINGS
            .iter()
            .find(|expected| expected.0 == source && expected.1 == path)
            .context("no exact pinned original attribution identity")?;
        let pin = PINS
            .iter()
            .find(|pin| pin.0 == source)
            .context("attribution pin")?;
        let canonical = sources
            .iter()
            .find(|candidate| candidate.id == source)
            .context("canonical attribution source")?;
        ensure!(
            canonical.repository == pin.2
                && canonical.revision == pin.1
                && canonical.archive.sha256 == pin.3
                && canonical.license == "Apache-2.0",
            "canonical inherited repository license or origin changed"
        );
        ensure!(
            original["revision"] == pin.1
                && original["bytes"] == expected.2
                && original["sha256"] == expected.3,
            "exact original attribution revision, length or hash changed"
        );
        let attribution = &original["attribution"];
        ensure!(
            attribution["repository"] == pin.2 && attribution["license"] == "Apache-2.0",
            "original attribution repository or license changed"
        );
        match (source, path) {
            ("base", "testutils/BUILD") | ("idea", "project-system-gradle-sync/BUILD") => {
                ensure!(
                    attribution["mode"] == "inherited_repository_license",
                    "headerless pinned original requires inherited license attribution"
                );
                let expected_proof = inherited_license_proof(source)?;
                ensure!(
                    attribution["license_proof"] == expected_proof,
                    "missing or incorrect pinned inherited license proof"
                );
                let binding: Binding = serde_json::from_value(expected_proof["fixture"].clone())?;
                let license_bytes =
                    bound_bytes(&root.join(&binding.path), &binding, MAX_RECORD_BYTES as u64)?;
                ensure!(
                    license_bytes
                        .starts_with(b"\n                                 Apache License\n"),
                    "inherited license text is not the complete pinned Apache license"
                );
            }
            _ => {
                ensure!(
                    attribution["mode"] == "retained_aosp_header"
                        && attribution["license_proof"].is_null(),
                    "only the two exact pinned headerless originals may inherit attribution"
                );
                let text = std::str::from_utf8(bytes)?;
                let header = text
                    .strip_prefix("/*\n * Copyright (C) ")
                    .and_then(|header| header.split_once(" The Android Open Source Project\n"));
                let complete_header = header.is_some_and(|(year, remainder)| {
                    year.len() == 4
                        && year.bytes().all(|byte| byte.is_ascii_digit())
                        && remainder.starts_with(COMPLETE_AOSP_LICENSE_HEADER)
                });
                ensure!(
                    complete_header,
                    "complete AOSP copyright and license header is missing"
                );
            }
        }
        ensure!(
            bytes.len() as u64 == expected.2 && digest(bytes) == expected.3,
            "complete original bytes changed during attribution verification"
        );
        Ok(())
    }

    #[test]
    fn complete_original_sources_and_scanner_slices_keep_exact_hashes_and_attribution() -> Result<()>
    {
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/reference_obligations");
        let provenance: Value = serde_json::from_slice(include_bytes!(
            "../../test_data/reference_obligations/provenance.json"
        ))?;
        let originals = provenance["original_sources"]
            .as_array()
            .context("original provenance")?;
        assert_eq!(originals.len(), 17);
        assert_eq!(provenance["original_behavior_credit"], 0);
        let matrix: Matrix = serde_json::from_slice(include_bytes!(
            "../../../../docs/android-studio/reference-manifest.json"
        ))?;
        let mut identities = BTreeSet::new();
        for original in originals {
            let source = original["source"].as_str().context("source")?;
            let path = original["path"].as_str().context("path")?;
            assert!(
                identities.insert((source, path)),
                "duplicate complete original attribution"
            );
            let binding = Binding {
                path: format!("sources/{source}/{path}"),
                bytes: original["bytes"].as_u64().context("bytes")?,
                sha256: original["sha256"].as_str().context("hash")?.to_owned(),
            };
            let bytes = bound_bytes(&root.join(&binding.path), &binding, MAX_RECORD_BYTES as u64)?;
            verify_original_license_attribution(&root, &matrix.sources, original, &bytes)?;
        }
        assert_eq!(
            digest(include_bytes!(
                "../../test_data/reference_obligations/sources/base/testutils/src/test/java/com/android/testutils/TestGroupTest.kt"
            )),
            "0f6a26754d7afa899e37e82936450f33fbad2339aa907924468c8c31dddf4743"
        );
        assert_eq!(
            digest(include_bytes!(
                "../../test_data/reference_obligations/sources/idea/project-system-gradle-sync/testSrc/com/android/tools/idea/gradle/project/sync/DefaultVariantsTest.kt"
            )),
            "c21c3dbc6fe94377ae7e82b2028d357c0ce0285793eecafa65140df63cc84b15"
        );
        for slice in provenance["inventory_slices"]
            .as_array()
            .context("inventory slices")?
        {
            let path = format!(
                "{}-{}.jsonl",
                slice["source"].as_str().context("source")?,
                slice["role"].as_str().context("role")?
            );
            let (mut file, state) = regular_file(&root.join(path))?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            assert_eq!(digest(&bytes), slice["fixture_bytes_sha256"]);
            assert_eq!(
                records::<Value>(&bytes)?.len() as u64,
                slice["fixture_record_count"]
                    .as_u64()
                    .context("record count")?
            );
            assert_eq!(physical_state(&file.metadata()?), state);
        }
        Ok(())
    }

    #[test]
    fn original_attribution_rejects_missing_license_wrong_origin_and_removed_headers() -> Result<()>
    {
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/reference_obligations");
        let provenance: Value = serde_json::from_slice(include_bytes!(
            "../../test_data/reference_obligations/provenance.json"
        ))?;
        let matrix: Matrix = serde_json::from_slice(include_bytes!(
            "../../../../docs/android-studio/reference-manifest.json"
        ))?;
        let originals = provenance["original_sources"]
            .as_array()
            .context("attribution originals")?;
        for (source, path) in [
            ("base", "testutils/BUILD"),
            ("idea", "project-system-gradle-sync/BUILD"),
        ] {
            let original = originals
                .iter()
                .find(|original| original["source"] == source && original["path"] == path)
                .context("headerless pinned original")?;
            let binding = Binding {
                path: format!("sources/{source}/{path}"),
                bytes: original["bytes"].as_u64().context("original bytes")?,
                sha256: original["sha256"]
                    .as_str()
                    .context("original hash")?
                    .to_owned(),
            };
            let bytes = bound_bytes(&root.join(&binding.path), &binding, MAX_RECORD_BYTES as u64)?;
            verify_original_license_attribution(&root, &matrix.sources, original, &bytes)?;

            let mut missing_proof = original.clone();
            missing_proof["attribution"]
                .as_object_mut()
                .context("attribution object")?
                .remove("license_proof");
            assert!(
                verify_original_license_attribution(&root, &matrix.sources, &missing_proof, &bytes)
                    .is_err()
            );
            let mut wrong_license_hash = original.clone();
            wrong_license_hash["attribution"]["license_proof"]["fixture"]["sha256"] =
                json!("0".repeat(64));
            assert!(
                verify_original_license_attribution(
                    &root,
                    &matrix.sources,
                    &wrong_license_hash,
                    &bytes
                )
                .is_err()
            );
            let mut wrong_license_revision = original.clone();
            wrong_license_revision["attribution"]["license_proof"]["revision"] =
                json!("0".repeat(40));
            assert!(
                verify_original_license_attribution(
                    &root,
                    &matrix.sources,
                    &wrong_license_revision,
                    &bytes
                )
                .is_err()
            );
            let mut wrong_original_hash = original.clone();
            wrong_original_hash["sha256"] = json!("0".repeat(64));
            assert!(
                verify_original_license_attribution(
                    &root,
                    &matrix.sources,
                    &wrong_original_hash,
                    &bytes
                )
                .is_err()
            );
            let mut unrelated_build = original.clone();
            unrelated_build["path"] = json!("another-module/BUILD");
            assert!(
                verify_original_license_attribution(
                    &root,
                    &matrix.sources,
                    &unrelated_build,
                    &bytes
                )
                .is_err()
            );

            let temporary = TempDir::new()?;
            let missing_license_root = temporary.path().join("reference_obligations");
            fs::create_dir_all(&missing_license_root)?;
            assert!(
                verify_original_license_attribution(
                    &missing_license_root,
                    &matrix.sources,
                    original,
                    &bytes
                )
                .is_err()
            );
            let mut corrupt_license =
                include_bytes!("../../test_data/reference_declarations/LICENSE-APACHE-2.0.txt")
                    .to_vec();
            *corrupt_license.first_mut().context("full license bytes")? = b'X';
            write_binding(
                &temporary.path().join("reference_declarations"),
                "LICENSE-APACHE-2.0.txt",
                &corrupt_license,
            )?;
            assert!(
                verify_original_license_attribution(
                    &missing_license_root,
                    &matrix.sources,
                    original,
                    &bytes
                )
                .is_err()
            );
        }

        let original = originals
            .iter()
            .find(|original| {
                original["source"] == "base"
                    && original["path"]
                        == "testutils/src/main/java/com/android/testutils/JarTestSuiteRunner.java"
            })
            .context("header-bearing original")?;
        let binding = Binding {
            path: format!(
                "sources/base/{}",
                original["path"].as_str().context("original path")?
            ),
            bytes: original["bytes"].as_u64().context("original bytes")?,
            sha256: original["sha256"]
                .as_str()
                .context("original hash")?
                .to_owned(),
        };
        let bytes = bound_bytes(&root.join(&binding.path), &binding, MAX_RECORD_BYTES as u64)?;
        verify_original_license_attribution(&root, &matrix.sources, original, &bytes)?;
        let text = std::str::from_utf8(&bytes)?;
        let header_end = text.find(" */\n").context("complete original header")? + " */\n".len();
        let missing_header = verify_original_license_attribution(
            &root,
            &matrix.sources,
            original,
            &bytes[header_end..],
        )
        .err()
        .context("removed original header must fail")?;
        assert!(
            missing_header
                .to_string()
                .contains("complete AOSP copyright and license header")
        );
        let incomplete_header = text.replacen(" * limitations under the License.\n", "", 1);
        assert!(incomplete_header.contains("Licensed under the Apache License"));
        let truncated_header = verify_original_license_attribution(
            &root,
            &matrix.sources,
            original,
            incomplete_header.as_bytes(),
        )
        .err()
        .context("incomplete original header must fail")?;
        assert!(
            truncated_header
                .to_string()
                .contains("complete AOSP copyright and license header")
        );
        let mut forged_header_exemption = original.clone();
        forged_header_exemption["attribution"]["mode"] = json!("inherited_repository_license");
        forged_header_exemption["attribution"]["license_proof"] = inherited_license_proof("base")?;
        assert!(
            verify_original_license_attribution(
                &root,
                &matrix.sources,
                &forged_header_exemption,
                &bytes
            )
            .is_err()
        );
        let mut wrong_canonical: Matrix = serde_json::from_slice(include_bytes!(
            "../../../../docs/android-studio/reference-manifest.json"
        ))?;
        wrong_canonical
            .sources
            .iter_mut()
            .find(|source| source.id == "base")
            .context("canonical base source")?
            .license = "wrong-license".to_owned();
        assert!(
            verify_original_license_attribution(&root, &wrong_canonical.sources, original, &bytes)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn original_helpers_and_named_tests_are_conserved_without_runtime_or_port_credit() -> Result<()>
    {
        let harness = Harness::originals()?;
        run(harness.args("output", false))?;
        let summary = harness.output_json("summary.json")?;
        assert_eq!(summary["canonical_rows"], 10);
        assert_eq!(summary["canonical_join_unresolved"], 0);
        assert_eq!(summary["new_behavioral_parity_credit"], 0);
        assert_eq!(summary["census_complete"], false);
        assert_eq!(summary["original_archive_traversal_performed"], false);
        assert!(summary["effective_runtime_cases"].is_null());
        let (candidates, _) = fixture_records()?;
        for (suffix, expected) in [("DefaultVariantsTest.kt", 9), ("TestGroupTest.kt", 3)] {
            let candidate = candidates
                .iter()
                .find(|candidate| candidate.path.ends_with(suffix))
                .context("original candidate")?;
            assert_eq!(
                candidate
                    .declarations
                    .as_ref()
                    .context("original declarations")?
                    .methods
                    .len(),
                expected
            );
        }
        assert!(
            harness
                .output_rows("idea-declarations.jsonl")?
                .iter()
                .any(|row| row["declaration"]["name"] == "variant"
                    && row["declaration"]["suite_candidate"].is_null())
        );
        assert!(
            harness
                .output_rows("base-declarations.jsonl")?
                .iter()
                .any(|row| row["declaration"]["name"] == "createJarWithClassPath")
        );
        assert!(harness.output_rows("canonical.jsonl")?.iter().all(
            |row| row["ledger_entry"]["port_status"] == "unported"
                && row["ledger_entry"]["run_status"] == "not_run"
                && row["effective_runtime_cases"].is_null()
        ));
        Ok(())
    }

    #[test]
    fn zero_method_suite_inheritance_parameter_provider_and_finalizer_stay_unresolved() -> Result<()>
    {
        let harness = Harness::originals()?;
        run(harness.args("output", false))?;
        let classes = harness.output_rows("idea-classes.jsonl")?;
        assert!(classes.iter().any(|row| row["class"]["name"]
            == "GradleProjectSystemSyncTestSuite"
            && row["effective_suite"].is_null()
            && row["expansion_status"] == "unresolved"));
        let methods = harness.output_rows("idea-declarations.jsonl")?;
        for name in ["checkForLeaks", "data", "testImportSimpleGradleProject"] {
            assert!(methods.iter().any(|row| row["declaration"]["name"] == name
                && row["applicability"] == "unresolved"
                && row["declaration"]["effective_runtime_cases"].is_null()));
        }
        assert!(
            harness
                .output_rows("idea-build-obligations.jsonl")?
                .iter()
                .all(|row| row["configured_target"].is_null() && row["module_identity"].is_null())
        );
        Ok(())
    }

    fn synthetic_candidate(source: &str) -> Result<Candidate> {
        let pin = PINS
            .iter()
            .find(|pin| pin.0 == source)
            .context("synthetic source")?;
        Ok(Candidate {
            source: source.to_owned(),
            revision: pin.1.to_owned(),
            path: "synthetic/Same.kt".to_owned(),
            sha256: digest(b"explicit synthetic overload fixture"),
            bytes: 35,
            kind: "jvm_source".to_owned(),
            fixture_path_hint: true,
            copyright_lines: Vec::new(),
            unresolved: vec!["synthetic test record; no original behavioral credit".to_owned()],
            declarations: Some(Declarations {
                package: Some("synthetic".to_owned()),
                classes: Vec::new(),
                annotations: Vec::new(),
                unresolved: vec!["custom overload dispatch unresolved".to_owned()],
                methods: (0..2)
                    .map(|_| MethodDeclaration {
                        suite_candidate: Some("synthetic.Same".to_owned()),
                        name: "same".to_owned(),
                        line: 1,
                        declaration: "fun same(...)".to_owned(),
                        annotations: vec!["Test".to_owned()],
                        reasons: vec!["synthetic overload identity regression".to_owned()],
                        effective_runtime_cases: None,
                    })
                    .collect(),
            }),
        })
    }

    fn candidate_member(candidate: &Candidate) -> (String, Member) {
        (
            candidate.source.clone(),
            Member {
                path: candidate.path.clone(),
                archive_path: candidate.path.clone(),
                kind: "file".to_owned(),
                bytes: candidate.bytes,
                sha256: candidate.sha256.clone(),
                link_target: None,
                discovery_classification: candidate.kind.clone(),
            },
        )
    }

    #[test]
    fn same_line_overloads_and_identical_mirror_paths_keep_distinct_provenance_identities()
    -> Result<()> {
        let candidates = vec![
            synthetic_candidate("idea")?,
            synthetic_candidate("jetbrains-android")?,
        ];
        let members = candidates.iter().map(candidate_member).collect();
        let harness = Harness::new(candidates, members)?;
        run(harness.args("output", false))?;
        let mut identities = BTreeSet::new();
        for source in ["idea", "jetbrains-android"] {
            let rows = harness.output_rows(&format!("{source}-declarations.jsonl"))?;
            assert_eq!(rows.len(), 2);
            for row in rows {
                assert!(
                    identities.insert(row["identity"].as_str().context("identity")?.to_owned())
                );
            }
        }
        assert_eq!(identities.len(), 4);
        Ok(())
    }

    #[test]
    fn explicit_case_rows_keep_each_id_and_historical_status_without_promotion() -> Result<()> {
        let candidate = synthetic_candidate("idea")?;
        let method = candidate
            .declarations
            .as_ref()
            .context("declarations")?
            .methods
            .first()
            .context("method")?;
        let tests = [
            canonical_test(&candidate, method, Some("synthetic-tuple-A"))?,
            canonical_test(&candidate, method, Some("synthetic-tuple-B"))?,
        ];
        let members = vec![candidate_member(&candidate)];
        let mut harness = Harness::new(vec![candidate], members)?;
        harness.matrix.tests = tests.into_iter().collect();
        harness.ledger.entries = harness.matrix.tests.iter().map(entry).collect();
        harness
            .ledger
            .entries
            .get_mut(1)
            .context("second case")?
            .port_status = "adapted".to_owned();
        harness
            .ledger
            .entries
            .get_mut(1)
            .context("second case")?
            .run_status = "blocked".to_owned();
        harness
            .ledger
            .entries
            .get_mut(1)
            .context("second case")?
            .reason =
            Some("Synthetic explicit case fixture; no actual parameter discovery".to_owned());
        harness.refresh()?;
        run(harness.args("output", false))?;
        let rows = harness.output_rows("canonical.jsonl")?;
        assert_eq!(rows.len(), 2);
        for (row, test) in rows.iter().zip(&harness.matrix.tests) {
            assert_eq!(row["reference_id"], test.id);
            assert_eq!(row["matching_candidate_count"], 2);
            assert!(row["declaration_identity"].is_null());
            assert_eq!(row["new_behavioral_parity_credit"], 0);
        }
        assert_eq!(
            rows.first().context("first row")?["ledger_entry"]["port_status"],
            "unported"
        );
        assert_eq!(
            rows.get(1).context("second row")?["ledger_entry"]["port_status"],
            "adapted"
        );
        assert_eq!(
            rows.get(1).context("second row")?["ledger_entry"]["run_status"],
            "blocked"
        );
        assert_eq!(harness.output_rows("canonical-matches.jsonl")?.len(), 4);
        Ok(())
    }

    #[test]
    fn corrupt_hash_pins_unknown_schema_and_claimed_runtime_expansion_are_rejected() -> Result<()> {
        let harness = Harness::originals()?;
        let mut args = harness.args("bad-hash", false);
        args.intake_sha256 = "0".repeat(64);
        assert!(run(args).is_err());
        assert!(
            !harness
                .temporary
                .path()
                .join("bad-hash/summary.json")
                .exists()
        );
        let mut harness = Harness::originals()?;
        harness
            .summary
            .sources
            .first_mut()
            .context("source")?
            .revision = "0".repeat(40);
        harness.refresh()?;
        assert!(run(harness.args("bad-pin", false)).is_err());
        let mut descriptor = serde_json::to_value(&harness.intake)?;
        descriptor
            .as_object_mut()
            .context("descriptor")?
            .insert("ported".to_owned(), json!(true));
        let bytes = serde_json::to_vec(&descriptor)?;
        fs::write(&harness.intake_path, &bytes)?;
        harness.intake_hash = digest(&bytes);
        assert!(run(harness.args("unknown-schema", false)).is_err());
        let (mut candidates, members) = fixture_records()?;
        let method = candidates
            .iter_mut()
            .find_map(|candidate| {
                candidate
                    .declarations
                    .as_mut()
                    .and_then(|declarations| declarations.methods.first_mut())
            })
            .context("method")?;
        method.effective_runtime_cases = Some(1);
        let harness = Harness::new(candidates, members)?;
        assert!(run(harness.args("illegal-runtime-count", false)).is_err());
        assert!(
            !harness
                .temporary
                .path()
                .join("illegal-runtime-count/summary.json")
                .exists()
        );
        Ok(())
    }

    #[test]
    fn duplicate_and_missing_original_paths_fail_conservation_instead_of_dropping_candidates()
    -> Result<()> {
        for mode in ["candidate", "member", "missing"] {
            let (mut candidates, mut members) = fixture_records()?;
            match mode {
                "candidate" => candidates.push(serde_json::from_value(serde_json::to_value(
                    candidates.first().context("candidate")?,
                )?)?),
                "member" => members.push(members.first().context("member")?.clone()),
                "missing" => {
                    members.remove(0);
                }
                _ => bail!("unexpected fixture mode"),
            }
            let harness = Harness::new(candidates, members)?;
            assert!(run(harness.args("output", false)).is_err(), "{mode}");
            assert!(
                !harness
                    .temporary
                    .path()
                    .join("output/summary.json")
                    .exists()
            );
        }
        Ok(())
    }

    #[test]
    fn gzip_crc_trailing_content_raw_hash_and_unterminated_records_are_rejected() -> Result<()> {
        let root = TempDir::new()?;
        let raw = b"{\"value\":1}\n";
        let binding = gzip_binding(root.path(), "input.jsonl.gz", raw)?;
        let mut wrong = binding.clone();
        wrong.decompressed_sha256 = "0".repeat(64);
        assert!(compressed_stream::<Value>(root.path(), &wrong, |_, _, _| Ok(())).is_err());
        let mut corrupt = fs::read(root.path().join(&binding.file.path))?;
        let index = corrupt.len().checked_sub(8).context("gzip CRC")?;
        *corrupt.get_mut(index).context("gzip CRC byte")? ^= 1;
        let corrupt_binding = CompressedBinding {
            file: write_binding(root.path(), "corrupt.gz", &corrupt)?,
            ..binding.clone()
        };
        assert!(
            compressed_stream::<Value>(root.path(), &corrupt_binding, |_, _, _| Ok(())).is_err()
        );
        let first = fs::read(root.path().join(&binding.file.path))?;
        let joined = [first.as_slice(), first.as_slice()].concat();
        let joined_binding = CompressedBinding {
            file: write_binding(root.path(), "joined.gz", &joined)?,
            ..binding.clone()
        };
        assert!(
            compressed_stream::<Value>(root.path(), &joined_binding, |_, _, _| Ok(())).is_err()
        );
        let no_newline = gzip_binding(root.path(), "unterminated.gz", b"{\"value\":1}")?;
        assert!(compressed_stream::<Value>(root.path(), &no_newline, |_, _, _| Ok(())).is_err());
        let multiple_documents = gzip_binding(root.path(), "multiple.gz", b"{}\n{}\n")?;
        assert!(compressed_document::<Value>(root.path(), &multiple_documents).is_err());
        Ok(())
    }

    #[test]
    fn input_and_output_record_limits_fail_without_silent_truncation() -> Result<()> {
        let root = TempDir::new()?;
        let raw = vec![b' '; MAX_RECORD_BYTES + 1];
        let binding = gzip_binding(root.path(), "large.jsonl.gz", &raw)?;
        assert!(compressed_stream::<Value>(root.path(), &binding, |_, _, _| Ok(())).is_err());
        let mut bytes = Vec::new();
        let mut writer = LimitedWriter {
            writer: &mut bytes,
            bytes: MAX_RECORD_BYTES - 1,
        };
        assert!(writer.write_all(b"xx").is_err());
        assert!(bytes.is_empty());
        let mut shards = ShardWriters::new(root.path(), "bound")?;
        let item = IndexItem {
            path: "bound".to_owned(),
            sha256: digest(b""),
            bytes: 0,
            kind: "file".to_owned(),
            ordinal: 0,
        };
        let index = Sha256::digest(item.path.as_bytes())[0] as usize % SHARDS;
        *shards.bytes.get_mut(index).context("shard")? = MAX_SHARD_BYTES;
        assert!(shards.push(&item).is_err());
        let mut artifact = super::writer(root.path(), "artifact-bound")?;
        artifact.bytes = MAX_ARTIFACT_BYTES;
        assert!(artifact.write_all(b"x").is_err());
        artifact.flush()?;
        assert_eq!(fs::metadata(root.path().join("artifact-bound"))?.len(), 0);
        Ok(())
    }

    #[test]
    fn blocked_non_jvm_unclassified_links_and_distinct_metadata_namespace_remain_visible()
    -> Result<()> {
        let (mut candidates, mut members) = fixture_records()?;
        let mut candidate = synthetic_candidate("idea")?;
        candidate.path = "synthetic/blocked.kt".to_owned();
        candidate.declarations = None;
        candidate.unresolved = vec![
            "non-UTF-8 source; full member hash retained and semantic discovery unresolved"
                .to_owned(),
        ];
        members.push(candidate_member(&candidate));
        candidates.push(candidate);
        let mut script = synthetic_candidate("idea")?;
        script.path = "synthetic/runner.py".to_owned();
        script.kind = "other_source_or_runner".to_owned();
        script.declarations = None;
        script.unresolved = vec![
            "semantic runner/declaration discovery is unsupported in this bounded tool".to_owned(),
        ];
        members.push(candidate_member(&script));
        candidates.push(script);
        for (path, kind, classification) in [
            ("unknown.payload", "file", "unclassified_payload"),
            ("alias", "symlink", "unclassified_payload"),
            (
                "pax_global_header",
                "global_pax_metadata",
                "archive_metadata",
            ),
            ("pax_global_header", "file", "unclassified_payload"),
        ] {
            members.push((
                "idea".to_owned(),
                Member {
                    path: path.to_owned(),
                    archive_path: path.to_owned(),
                    kind: kind.to_owned(),
                    bytes: 0,
                    sha256: digest(b""),
                    link_target: (kind == "symlink").then(|| "../unresolved-target".to_owned()),
                    discovery_classification: classification.to_owned(),
                },
            ));
        }
        let harness = Harness::new(candidates, members)?;
        run(harness.args("output", false))?;
        let rows = harness.output_rows("idea-members.jsonl")?;
        assert_eq!(
            rows.iter()
                .filter(|row| row["member"]["path"] == "pax_global_header")
                .count(),
            2
        );
        assert!(rows.iter().any(|row| row["member"]["path"] == "alias"
            && row["member"]["link_target"] == "../unresolved-target"));
        let unresolved = harness.output_rows("idea-unresolved.jsonl")?;
        assert!(
            unresolved
                .iter()
                .any(|row| row["scope"] == "member_discovery")
        );
        assert!(
            unresolved
                .iter()
                .any(|row| row["original_reasons"]
                    .as_array()
                    .is_some_and(|reasons| reasons.iter().any(|reason| reason
                        .as_str()
                        .is_some_and(|reason| reason.starts_with("non-UTF-8")))))
        );
        assert!(harness.output_json("summary.json")?["effective_runtime_cases"].is_null());
        Ok(())
    }

    #[test]
    fn unknown_missing_and_duplicate_canonical_ids_fail_but_unmatched_declarations_survive()
    -> Result<()> {
        let mut harness = Harness::originals()?;
        harness.ledger.entries.pop();
        harness.refresh()?;
        assert!(run(harness.args("missing", false)).is_err());
        let mut harness = Harness::originals()?;
        let reference_id = harness
            .ledger
            .entries
            .first()
            .context("first row")?
            .reference_id
            .clone();
        harness
            .ledger
            .entries
            .get_mut(1)
            .context("second row")?
            .reference_id = reference_id;
        harness.refresh()?;
        assert!(run(harness.args("duplicate", false)).is_err());
        let mut harness = Harness::originals()?;
        harness
            .ledger
            .entries
            .first_mut()
            .context("entry")?
            .port_status = "deferred_is_not_applicable".to_owned();
        harness.refresh()?;
        assert!(run(harness.args("bad-status", false)).is_err());
        let mut harness = Harness::originals()?;
        harness
            .ledger
            .entries
            .first_mut()
            .context("entry")?
            .port_status = "not_applicable".to_owned();
        harness.refresh()?;
        assert!(run(harness.args("missing-individual-reason", false)).is_err());
        let mut harness = Harness::originals()?;
        let test = harness.matrix.tests.first_mut().context("test")?;
        let old_method = test.method.clone();
        test.method = "synthetic-unmatched-declaration".to_owned();
        test.id = test
            .id
            .strip_suffix(&old_method)
            .context("original method suffix")?
            .to_owned()
            + &test.method;
        harness
            .ledger
            .entries
            .first_mut()
            .context("entry")?
            .reference_id = test.id.clone();
        harness.refresh()?;
        run(harness.args("output", false))?;
        assert_eq!(
            harness
                .output_rows("canonical.jsonl")?
                .first()
                .context("row")?["matching_candidate_count"],
            0
        );
        assert_eq!(harness.output_rows("canonical.jsonl")?.len(), 10);
        assert_eq!(
            harness.output_json("summary.json")?["canonical_join_unresolved"],
            1
        );
        Ok(())
    }

    #[test]
    fn deterministic_create_check_rejects_changed_extra_missing_and_promoted_outputs() -> Result<()>
    {
        let harness = Harness::originals()?;
        run(harness.args("output", false))?;
        run(harness.args("output", true))?;
        run(harness.args("second", false))?;
        compare_outputs(
            &harness.temporary.path().join("output"),
            &harness.temporary.path().join("second"),
        )?;
        assert!(run(harness.args("output", false)).is_err());
        let canonical_path = harness.temporary.path().join("output/canonical.jsonl");
        let canonical = fs::read(&canonical_path)?;
        let mut rows: Vec<Value> = records(&canonical)?;
        rows.first_mut().context("row")?["ledger_entry"]["port_status"] = json!("ported");
        fs::write(&canonical_path, jsonl(&rows)?)?;
        assert!(run(harness.args("output", true)).is_err());
        fs::write(&canonical_path, canonical)?;
        let extra = harness.temporary.path().join("output/extra.jsonl");
        fs::write(&extra, b"{}\n")?;
        assert!(run(harness.args("output", true)).is_err());
        fs::remove_file(extra)?;
        fs::remove_file(
            harness
                .temporary
                .path()
                .join("output/idea-unresolved.jsonl"),
        )?;
        assert!(run(harness.args("output", true)).is_err());
        Ok(())
    }

    #[test]
    fn checkout_outputs_and_duplicate_schema_fields_are_rejected_without_matrix_changes()
    -> Result<()> {
        let harness = Harness::originals()?;
        let before = fs::read(
            harness
                .repository
                .join("docs/android-studio/test-parity.json"),
        )?;
        let mut args = harness.args("inside", false);
        args.output = harness.repository.join("inside");
        assert!(run(args).is_err());
        assert!(!harness.repository.join("inside").exists());
        let mut args = harness.args("inside-inputs", false);
        args.output = harness.inputs.join("inside-inputs");
        assert!(run(args).is_err());
        assert!(!harness.inputs.join("inside-inputs").exists());
        let duplicate = format!(
            "{{\"schema_version\":1,\"schema_version\":1,\"summary\":{},\"retention\":{},\"sources\":{},\"manifest\":{},\"ledger\":{}}}",
            serde_json::to_string(&harness.intake.summary)?,
            serde_json::to_string(&harness.intake.retention)?,
            serde_json::to_string(&harness.intake.sources)?,
            serde_json::to_string(&harness.intake.manifest)?,
            serde_json::to_string(&harness.intake.ledger)?
        );
        assert!(serde_json::from_str::<Intake>(&duplicate).is_err());
        assert_eq!(
            before,
            fs::read(
                harness
                    .repository
                    .join("docs/android-studio/test-parity.json")
            )?
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn immutable_input_symlinks_and_output_symlinks_are_rejected() -> Result<()> {
        use std::os::unix::fs::symlink;
        let harness = Harness::originals()?;
        let alias = harness.temporary.path().join("alias-intake.json");
        symlink(&harness.intake_path, &alias)?;
        let mut args = harness.args("output", false);
        args.intake = alias;
        assert!(run(args).is_err());
        let output = harness.temporary.path().join("existing");
        fs::create_dir(&output)?;
        symlink(&output, harness.temporary.path().join("alias-output"))?;
        assert!(run(harness.args("alias-output", true)).is_err());
        Ok(())
    }
}
