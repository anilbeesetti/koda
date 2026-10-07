#![allow(clippy::disallowed_methods, reason = "tooling is exempt")]

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};
use clap::Parser;
use flate2::read::MultiGzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 16 * 1024;
const MAX_TOKENS: usize = 100_000;
const MAX_DECLARATIONS: usize = 4096;
const MAX_PREFIX_TOKENS: usize = 1024;
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_METADATA_BYTES: usize = 8 * 1024 * 1024;

#[derive(Parser)]
pub struct AndroidReferenceCensusArgs {
    #[arg(long, default_value = ".")]
    repository: PathBuf,
    #[arg(long)]
    reference_root: PathBuf,
    /// External directory to create, or compare with --check. Existing output is never overwritten.
    #[arg(long)]
    output: PathBuf,
    /// Regenerate into staging and require byte-identical prior output.
    #[arg(long)]
    check: bool,
}

#[derive(Deserialize)]
struct Manifest {
    schema_version: u32,
    baseline: String,
    sources: Vec<Source>,
}

#[derive(Deserialize)]
struct Source {
    id: String,
    revision: String,
    repository: String,
    coverage: String,
    archive: Artifact,
}

#[derive(Deserialize, Serialize)]
struct Artifact {
    path: String,
    sha256: String,
}

#[derive(Serialize)]
struct Summary {
    schema_version: u32,
    baseline: String,
    effective_test_census_complete: bool,
    discovery: &'static str,
    unresolved: Vec<&'static str>,
    sources: Vec<SourceSummary>,
}

#[derive(Serialize)]
struct SourceSummary {
    id: String,
    revision: String,
    repository: String,
    coverage: String,
    archive: Artifact,
    archive_bytes: u64,
    all_members_traversed: bool,
    physical_member_types: BTreeMap<String, usize>,
    member_kinds: BTreeMap<String, usize>,
    candidate_kinds: BTreeMap<String, usize>,
    method_candidates: usize,
    files_with_unresolved_lexing: usize,
    unclassified_regular_members: usize,
    members: Artifact,
    physical_members: Artifact,
    candidates: Artifact,
}

#[derive(Serialize)]
struct Member {
    path: String,
    archive_path: String,
    kind: String,
    bytes: u64,
    sha256: String,
    link_target: Option<String>,
    discovery_classification: String,
}

#[derive(Serialize)]
struct PhysicalMember {
    ordinal: usize,
    type_byte: u8,
    raw_path_bytes: Vec<u8>,
    header_sha256: String,
    bytes: u64,
    payload_sha256: String,
}

#[derive(Serialize)]
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

#[derive(Debug, Default, Serialize)]
struct Declarations {
    package: Option<String>,
    classes: Vec<ClassDeclaration>,
    methods: Vec<MethodDeclaration>,
    annotations: Vec<Annotation>,
    unresolved: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ClassDeclaration {
    name: String,
    line: usize,
    declaration: String,
    abstract_hint: bool,
    inheritance_declaration: String,
    inheritance_declaration_complete: bool,
}

#[derive(Debug, Serialize)]
struct MethodDeclaration {
    suite_candidate: Option<String>,
    name: String,
    line: usize,
    declaration: String,
    annotations: Vec<String>,
    reasons: Vec<String>,
    effective_runtime_cases: Option<usize>,
}

#[derive(Debug, Serialize)]
struct Annotation {
    name: String,
    line: usize,
}

pub fn run(args: AndroidReferenceCensusArgs) -> Result<()> {
    let repository = args.repository.canonicalize()?;
    let reference_root = args.reference_root.canonicalize()?;
    let output_parent = args
        .output
        .parent()
        .context("output needs a parent directory")?;
    let output_parent = output_parent
        .canonicalize()
        .context("output parent must already exist")?;
    let output_name = args
        .output
        .file_name()
        .context("output needs a directory name")?;
    let output = output_parent.join(output_name);
    ensure!(
        !output.starts_with(&repository),
        "census output must stay outside the checkout"
    );
    ensure!(
        args.check == output.exists(),
        "use a new output directory, or --check for an existing census"
    );
    let staging = output_parent.join(format!(
        ".{}-staging-{}",
        output_name.to_string_lossy(),
        std::process::id()
    ));
    fs::create_dir(&staging).context("cannot create isolated census staging directory")?;
    let result = generate(&repository, &reference_root, &staging).and_then(|summary| {
        if args.check {
            compare_outputs(&output, &staging)?;
            fs::remove_dir_all(&staging)?;
        } else {
            publish_without_replacement(&staging, &output)?;
            fs::remove_dir_all(&staging)?;
        }
        println!("Traversed {} pinned archives; discovery candidates remain unresolved for effective test census. Output: {}", summary.sources.len(), output.display());
        Ok(())
    });
    if result.is_err() && staging.exists() {
        fs::remove_dir_all(&staging)
            .context("failed to remove unsuccessful census staging directory")?;
    }
    result
}

fn generate(repository: &Path, reference_root: &Path, output: &Path) -> Result<Summary> {
    let manifest_path = repository.join("docs/android-studio/reference-manifest.json");
    let mut manifest: Manifest =
        serde_json::from_reader(BufReader::new(File::open(manifest_path)?))?;
    ensure!(
        manifest.schema_version == 1,
        "unsupported reference manifest version"
    );
    ensure!(
        !manifest.sources.is_empty(),
        "manifest has no reference sources"
    );
    manifest
        .sources
        .sort_by(|left, right| left.id.cmp(&right.id));
    let mut source_ids = BTreeSet::new();
    for source in &manifest.sources {
        ensure!(
            source_ids.insert(&source.id),
            "duplicate source id: {}",
            source.id
        );
        ensure!(
            !source.id.is_empty()
                && source
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
            "invalid source id"
        );
        ensure!(
            source.revision.len() == 40
                && source
                    .revision
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "source revision must be a full lowercase Git commit"
        );
    }
    let mut sources = Vec::new();
    for source in manifest.sources {
        let source_id = source.id.clone();
        sources.push(
            discover_archive(reference_root, output, source)
                .with_context(|| format!("discovery failed for pinned source {source_id}"))?,
        );
    }
    let summary = Summary {
        schema_version: 1,
        baseline: manifest.baseline,
        effective_test_census_complete: false,
        discovery: "archive traversal and lexical declaration candidates; not runner descriptions or runtime case counts",
        unresolved: vec![
            "build-target membership and upstream runner discovery have not been reconciled",
            "inherited/abstract/nested suites and custom annotation dispatch remain unresolved",
            "parameter providers, repeated cases, dynamic factories and generated tests remain unresolved",
            "disabled/quarantined tests and suite factories require upstream runner inspection",
            "non-JVM declarations and executable/script/generated runners require semantic discovery",
            "fixture-like paths are retained and require review, not automatic exclusion",
            "unclassified payloads remain in the member inventory and require source/runner review",
            "symbolic/hard links are retained without extraction; runner alias materialization remains unresolved",
            "unverified source mirrors cannot establish canonical coverage",
        ],
        sources,
    };
    let mut writer = BufWriter::new(File::create(output.join("summary.json"))?);
    serde_json::to_writer_pretty(&mut writer, &summary)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(summary)
}

fn discover_archive(reference_root: &Path, output: &Path, source: Source) -> Result<SourceSummary> {
    let archive_relative = safe_path(&source.archive.path, false)?;
    let archive_path = reference_root.join(&archive_relative).canonicalize()?;
    ensure!(
        archive_path.starts_with(reference_root),
        "archive escapes the reference root"
    );
    ensure!(
        hash_file(&archive_path)? == source.archive.sha256,
        "pinned archive hash mismatch: {}",
        source.id
    );
    let archive_bytes = fs::metadata(&archive_path)?.len();
    let physical_name = format!("{}-physical-members.jsonl", source.id);
    let physical_member_types = discover_physical_members(
        &archive_path,
        &source.archive.sha256,
        archive_bytes,
        &output.join(&physical_name),
    )?;
    let members_name = format!("{}-members.jsonl", source.id);
    let candidates_name = format!("{}-candidates.jsonl", source.id);
    let mut members_writer = BufWriter::new(File::create(output.join(&members_name))?);
    let mut candidates_writer = BufWriter::new(File::create(output.join(&candidates_name))?);
    let reader = verified_decoder(&archive_path)?;
    let mut archive = tar::Archive::new(reader);
    let mut paths = BTreeSet::new();
    let mut member_kinds = BTreeMap::new();
    let mut candidate_kinds = BTreeMap::new();
    let mut method_candidates = 0;
    let mut files_with_unresolved_lexing = 0;
    let mut unclassified_regular_members = 0;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let archive_path = std::str::from_utf8(&entry.path_bytes())?.to_owned();
        let prefix =
            (source.id == "jetbrains-android").then(|| format!("android-{}/", source.revision));
        let relative = match &prefix {
            Some(prefix) => archive_path
                .strip_prefix(prefix)
                .context("mirror archive member lacks pinned root prefix")?,
            None => &archive_path,
        };
        let kind = member_kind(entry.header().entry_type());
        let path = safe_path(relative, kind == "directory")?;
        ensure!(
            paths.insert(path.clone()),
            "duplicate normalized archive path: {path}"
        );
        let bytes = entry.size();
        let mut candidate_kind = (kind == "file").then(|| candidate_kind(&path)).flatten();
        if kind == "file" && candidate_kind.is_none() && entry.header().mode()? & 0o111 != 0 {
            candidate_kind = Some("other_source_or_runner");
        }
        let mut content = Vec::new();
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut retain = candidate_kind.is_some() && bytes <= MAX_SOURCE_BYTES;
        let mut bytes_read = 0_u64;
        loop {
            let count = entry.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            if kind == "file"
                && bytes_read == 0
                && candidate_kind.is_none()
                && buffer[..count].starts_with(b"#!")
            {
                candidate_kind = Some("other_source_or_runner");
                retain = bytes <= MAX_SOURCE_BYTES;
            }
            hasher.update(&buffer[..count]);
            bytes_read += count as u64;
            if retain {
                content.extend_from_slice(&buffer[..count]);
            }
        }
        ensure!(
            bytes_read == bytes,
            "archive member length mismatch: {path}"
        );
        let hash = format!("{:x}", hasher.finalize());
        let link_target = entry
            .link_name()?
            .map(|path| {
                path.to_str()
                    .map(str::to_owned)
                    .context("non-UTF-8 link target")
            })
            .transpose()?;
        write_json_line(
            &mut members_writer,
            &Member {
                path: path.clone(),
                archive_path,
                kind: kind.to_owned(),
                bytes,
                sha256: hash.clone(),
                link_target,
                discovery_classification: candidate_kind
                    .unwrap_or("unclassified_payload")
                    .to_owned(),
            },
        )?;
        *member_kinds.entry(kind.to_owned()).or_default() += 1;
        if kind == "file" && candidate_kind.is_none() {
            unclassified_regular_members += 1;
        }
        if let Some(kind) = candidate_kind {
            *candidate_kinds.entry(kind.to_owned()).or_default() += 1;
            let mut unresolved = vec!["build-target membership is unreviewed".to_owned()];
            let mut copyright_lines = Vec::new();
            let mut declarations = None;
            if !retain {
                unresolved.push(format!("source exceeds {MAX_SOURCE_BYTES}-byte lexical buffer limit; full member hash retained"));
                files_with_unresolved_lexing += 1;
            } else if let Ok(text) = std::str::from_utf8(&content) {
                copyright_lines = text
                    .lines()
                    .filter(|line| {
                        line.contains("Copyright") || line.contains("SPDX-License-Identifier")
                    })
                    .map(str::to_owned)
                    .collect();
                if kind == "jvm_source" {
                    let scanned = discover_jvm(text, &path);
                    method_candidates += scanned.methods.len();
                    if !scanned.unresolved.is_empty() {
                        files_with_unresolved_lexing += 1;
                    }
                    declarations = Some(scanned);
                } else {
                    unresolved.push(
                        "semantic runner/declaration discovery is unsupported in this bounded tool"
                            .to_owned(),
                    );
                }
            } else {
                unresolved.push(
                    "non-UTF-8 source; full member hash retained and semantic discovery unresolved"
                        .to_owned(),
                );
                files_with_unresolved_lexing += 1;
            }
            write_json_line(
                &mut candidates_writer,
                &Candidate {
                    source: source.id.clone(),
                    revision: source.revision.clone(),
                    fixture_path_hint: fixture_hint(&path),
                    path,
                    sha256: hash,
                    bytes,
                    kind: kind.to_owned(),
                    copyright_lines,
                    declarations,
                    unresolved,
                },
            )?;
        }
    }
    // Tar stops at its terminator. Drain gzip to verify trailers and surface trailing corruption.
    finish_verified_stream(archive.into_inner(), &source.archive.sha256, archive_bytes)?;
    members_writer.flush()?;
    candidates_writer.flush()?;
    Ok(SourceSummary {
        id: source.id,
        revision: source.revision,
        repository: source.repository,
        coverage: source.coverage,
        archive: source.archive,
        archive_bytes,
        all_members_traversed: true,
        physical_member_types,
        member_kinds,
        candidate_kinds,
        method_candidates,
        files_with_unresolved_lexing,
        unclassified_regular_members,
        members: Artifact {
            sha256: hash_file(&output.join(&members_name))?,
            path: members_name,
        },
        physical_members: Artifact {
            sha256: hash_file(&output.join(&physical_name))?,
            path: physical_name,
        },
        candidates: Artifact {
            sha256: hash_file(&output.join(&candidates_name))?,
            path: candidates_name,
        },
    })
}

struct HashingReader<R> {
    inner: R,
    hasher: Sha256,
    bytes: u64,
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.inner.read(buffer)?;
        self.hasher.update(&buffer[..count]);
        self.bytes += count as u64;
        Ok(count)
    }
}

type VerifiedDecoder = MultiGzDecoder<HashingReader<BufReader<File>>>;

fn verified_decoder(path: &Path) -> Result<VerifiedDecoder> {
    Ok(MultiGzDecoder::new(HashingReader {
        inner: BufReader::new(File::open(path)?),
        hasher: Sha256::new(),
        bytes: 0,
    }))
}

fn finish_verified_stream(
    mut decoder: VerifiedDecoder,
    expected_hash: &str,
    expected_bytes: u64,
) -> Result<()> {
    let mut trailing = [0_u8; 64 * 1024];
    loop {
        let count = decoder.read(&mut trailing)?;
        if count == 0 {
            break;
        }
        ensure!(
            trailing[..count].iter().all(|byte| *byte == 0),
            "nonzero trailing archive bytes could hide undiscovered members"
        );
    }
    let reader = decoder.into_inner();
    ensure!(
        reader.bytes == expected_bytes,
        "compressed archive byte count changed during traversal"
    );
    ensure!(
        format!("{:x}", reader.hasher.finalize()) == expected_hash,
        "compressed archive bytes consumed differ from pinned SHA"
    );
    Ok(())
}

fn discover_physical_members(
    path: &Path,
    expected_hash: &str,
    expected_bytes: u64,
    output: &Path,
) -> Result<BTreeMap<String, usize>> {
    let mut archive = tar::Archive::new(verified_decoder(path)?);
    let mut writer = BufWriter::new(File::create(output)?);
    let mut member_types = BTreeMap::new();
    for (ordinal, entry) in archive.entries()?.raw(true).enumerate() {
        let mut entry = entry?;
        ensure!(
            !entry.header().entry_type().is_gnu_sparse(),
            "GNU sparse framing requires a reviewed physical census implementation"
        );
        let bytes = entry.size();
        let kind = entry.header().entry_type();
        ensure!(
            !(kind.is_gnu_longname()
                || kind.is_gnu_longlink()
                || kind.is_pax_local_extensions()
                || kind.is_pax_global_extensions())
                || bytes <= MAX_SOURCE_BYTES,
            "extension metadata exceeds the bounded logical-parser buffer limit"
        );
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut bytes_read = 0_u64;
        loop {
            let count = entry.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
            bytes_read += count as u64;
        }
        ensure!(bytes == bytes_read, "truncated physical archive member");
        let type_byte = entry.header().as_bytes()[156];
        *member_types.entry(format!("{type_byte:02x}")).or_default() += 1;
        write_json_line(
            &mut writer,
            &PhysicalMember {
                ordinal,
                type_byte,
                raw_path_bytes: entry.path_bytes().to_vec(),
                header_sha256: format!("{:x}", Sha256::digest(entry.header().as_bytes())),
                bytes,
                payload_sha256: format!("{:x}", hasher.finalize()),
            },
        )?;
    }
    finish_verified_stream(archive.into_inner(), expected_hash, expected_bytes)?;
    writer.flush()?;
    Ok(member_types)
}

fn publish_without_replacement(staging: &Path, output: &Path) -> Result<()> {
    fs::create_dir(output)
        .context("output appeared before publication; existing directory was preserved")?;
    let mut files = fs::read_dir(staging)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    // The summary is the completion marker; readers must not accept output without it.
    files.sort_by_key(|path| {
        (
            path.file_name().is_some_and(|name| name == "summary.json"),
            path.clone(),
        )
    });
    for file in files {
        let name = file.file_name().context("staged artifact name")?;
        fs::hard_link(&file, output.join(name)).context("publication incomplete; an existing output artifact was preserved; use a fresh output directory")?;
    }
    Ok(())
}

fn member_kind(kind: tar::EntryType) -> &'static str {
    if kind.is_file() {
        "file"
    } else if kind.is_dir() {
        "directory"
    } else if kind.is_symlink() {
        "symlink"
    } else if kind.is_hard_link() {
        "hard_link"
    } else {
        "special"
    }
}

fn safe_path(path: &str, allow_root: bool) -> Result<String> {
    ensure!(
        !path.contains('\\') && !path.starts_with('/'),
        "non-portable or absolute archive path"
    );
    let path = path.strip_suffix('/').unwrap_or(path);
    if allow_root && path.is_empty() {
        return Ok(String::new());
    }
    ensure!(!path.is_empty(), "empty archive path");
    ensure!(
        path.split('/').all(|part| !part.is_empty()
            && part != "."
            && part != ".."
            && !part.contains(':')
            && !part.chars().any(char::is_control)),
        "unsafe or non-normal archive path: {path}"
    );
    Ok(path.to_owned())
}

fn candidate_kind(path: &str) -> Option<&'static str> {
    let file = Path::new(path).file_name()?.to_str()?;
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    if matches!(
        file,
        "BUILD"
            | "BUILD.bazel"
            | "WORKSPACE"
            | "WORKSPACE.bazel"
            | "MODULE.bazel"
            | "pom.xml"
            | "build.xml"
            | "CMakeLists.txt"
            | "Makefile"
            | "Cargo.toml"
            | "Android.bp"
            | "Android.mk"
    ) || matches!(extension, "bzl" | "gradle" | "iml")
        || file.ends_with(".gradle.kts")
    {
        Some("build_runner")
    } else if matches!(extension, "java" | "kt" | "kts") {
        Some("jvm_source")
    } else if matches!(
        extension,
        "groovy"
            | "scala"
            | "py"
            | "c"
            | "cc"
            | "cpp"
            | "cxx"
            | "h"
            | "hh"
            | "hpp"
            | "rs"
            | "sh"
            | "bash"
            | "bat"
            | "cmd"
            | "ps1"
            | "rb"
            | "js"
            | "ts"
            | "m"
            | "mm"
            | "swift"
            | "xml"
            | "yaml"
            | "yml"
            | "json"
            | "toml"
            | "properties"
            | "cfg"
            | "ini"
            | "txt"
            | "test"
            | "tests"
            | "mk"
            | "cmake"
            | "bazel"
    ) || file.to_ascii_lowercase().contains("test")
    {
        Some("other_source_or_runner")
    } else {
        None
    }
}

fn fixture_hint(path: &str) -> bool {
    path.split('/').any(|part| {
        matches!(
            part.to_ascii_lowercase().as_str(),
            "testdata" | "test_data" | "fixtures" | "golden" | "goldens" | "snapshots"
        )
    })
}

fn write_json_line(writer: &mut impl Write, value: &impl Serialize) -> Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn hash_file(path: &Path) -> Result<String> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn compare_outputs(expected: &Path, actual: &Path) -> Result<()> {
    let files = |directory: &Path| -> Result<BTreeSet<PathBuf>> {
        fs::read_dir(directory)?
            .map(|entry| {
                let entry = entry?;
                ensure!(
                    entry.file_type()?.is_file(),
                    "census output contains non-file artifact"
                );
                Ok(PathBuf::from(entry.file_name()))
            })
            .collect()
    };
    let expected_files = files(expected)?;
    ensure!(
        expected_files == files(actual)?,
        "census artifact set changed"
    );
    for file in expected_files {
        ensure!(
            hash_file(&expected.join(&file))? == hash_file(&actual.join(&file))?,
            "census artifact bytes changed: {}",
            file.display()
        );
    }
    Ok(())
}

#[derive(Debug)]
struct Token<'a> {
    text: &'a str,
    line: usize,
    identifier: bool,
    escaped_identifier: bool,
}

impl Token<'_> {
    fn is(&self, text: &str) -> bool {
        !self.escaped_identifier && self.text == text
    }
}

struct LexicalContext {
    body_boundary: Vec<usize>,
    function_boundary: Vec<usize>,
    method_tail_boundary: Vec<usize>,
    prefix_start: Vec<usize>,
    closing_parenthesis: Vec<Option<usize>>,
    unbalanced_parentheses: bool,
}

impl LexicalContext {
    fn new(tokens: &[Token<'_>]) -> Self {
        let boundaries = |names: &[&str]| {
            let mut result = vec![tokens.len(); tokens.len() + 1];
            let mut next = tokens.len();
            for (index, token) in tokens.iter().enumerate().rev() {
                if names.iter().any(|name| token.is(name)) {
                    next = index;
                }
                result[index] = next;
            }
            result
        };
        let mut prefix_start = Vec::with_capacity(tokens.len() + 1);
        let mut closing_parenthesis = vec![None; tokens.len()];
        let mut parentheses = Vec::new();
        let mut unbalanced_parentheses = false;
        let mut start = 0;
        for (index, token) in tokens.iter().enumerate() {
            prefix_start.push(start);
            if token.is("(") {
                parentheses.push(index);
            } else if token.is(")") {
                if let Some(open) = parentheses.pop() {
                    closing_parenthesis[open] = Some(index);
                } else {
                    unbalanced_parentheses = true;
                }
            } else if parentheses.is_empty() && ["{", "}", ";"].iter().any(|text| token.is(text)) {
                start = index + 1;
            }
        }
        prefix_start.push(start);
        Self {
            body_boundary: boundaries(&["{", ";"]),
            function_boundary: boundaries(&["(", "{", "="]),
            method_tail_boundary: boundaries(&["{", ";", "=", "(", ")", "}"]),
            prefix_start,
            closing_parenthesis,
            unbalanced_parentheses: unbalanced_parentheses || !parentheses.is_empty(),
        }
    }
}

fn unresolved_once(declarations: &mut Declarations, reason: &str) {
    if !declarations
        .unresolved
        .iter()
        .any(|existing| existing == reason)
    {
        declarations.unresolved.push(reason.to_owned());
    }
}

fn reserve_metadata(declarations: &mut Declarations, used: &mut usize, additional: usize) -> bool {
    if additional > MAX_METADATA_BYTES.saturating_sub(*used) {
        unresolved_once(
            declarations,
            "retained metadata budget exhausted; remaining declarations unresolved",
        );
        false
    } else {
        *used += additional;
        true
    }
}

fn lex(text: &str, kotlin: bool) -> (Vec<Token<'_>>, Vec<String>) {
    let mut tokens = Vec::new();
    let mut unresolved = Vec::new();
    let mut position = 0;
    let mut line = 1;
    while position < text.len() {
        if tokens.len() == MAX_TOKENS {
            unresolved.push(
                "lexical token budget exhausted; remaining declarations unresolved".to_owned(),
            );
            break;
        }
        let remainder = &text[position..];
        if remainder.starts_with("//") {
            let length = remainder.find('\n').unwrap_or(remainder.len());
            position += length;
            continue;
        }
        if remainder.starts_with("/*") {
            let mut depth = 1;
            position += 2;
            while position < text.len() && depth > 0 {
                let remainder = &text[position..];
                if kotlin && remainder.starts_with("/*") {
                    depth += 1;
                    position += 2;
                } else if remainder.starts_with("*/") {
                    depth -= 1;
                    position += 2;
                } else if let Some(character) = remainder.chars().next() {
                    line += usize::from(character == '\n');
                    position += character.len_utf8();
                }
            }
            if depth != 0 {
                unresolved.push("unterminated block comment".to_owned());
            }
            continue;
        }
        let Some(character) = remainder.chars().next() else {
            break;
        };
        if character.is_whitespace() {
            line += usize::from(character == '\n');
            position += character.len_utf8();
            continue;
        }
        if character == '"' || character == '\'' {
            let triple = remainder.starts_with("\"\"\"");
            let delimiter = if triple {
                "\"\"\""
            } else if character == '"' {
                "\""
            } else {
                "'"
            };
            position += delimiter.len();
            let mut closed = false;
            while position < text.len() {
                let remainder = &text[position..];
                if remainder.starts_with(delimiter) {
                    position += delimiter.len();
                    closed = true;
                    break;
                }
                let Some(character) = remainder.chars().next() else {
                    break;
                };
                line += usize::from(character == '\n');
                position += character.len_utf8();
                if !triple
                    && character == '\\'
                    && let Some(escaped) = text[position..].chars().next()
                {
                    line += usize::from(escaped == '\n');
                    position += escaped.len_utf8();
                }
            }
            if !closed {
                unresolved.push("unterminated string or character literal".to_owned());
            }
            continue;
        }
        let start = position;
        let start_line = line;
        if character == '`' {
            position += 1;
            if let Some(length) = text[position..].find('`') {
                let name = &text[position..position + length];
                line += name.bytes().filter(|byte| *byte == b'\n').count();
                position += length + 1;
                tokens.push(Token {
                    text: name,
                    line: start_line,
                    identifier: true,
                    escaped_identifier: true,
                });
            } else {
                unresolved.push("unterminated backtick identifier".to_owned());
                break;
            }
            continue;
        }
        let identifier = character.is_alphabetic() || character == '_' || character == '$';
        position += character.len_utf8();
        if identifier {
            while let Some(character) = text[position..].chars().next() {
                if character.is_alphanumeric() || character == '_' || character == '$' {
                    position += character.len_utf8();
                } else {
                    break;
                }
            }
        }
        tokens.push(Token {
            text: &text[start..position],
            line: start_line,
            identifier,
            escaped_identifier: false,
        });
    }
    (tokens, unresolved)
}

fn discover_jvm(text: &str, path: &str) -> Declarations {
    if text.lines().any(|line| line.len() > MAX_LINE_BYTES) {
        return Declarations { unresolved: vec!["source line exceeds the declaration buffer limit; discovery deferred with full member hash retained".to_owned()], ..Declarations::default() };
    }
    let kotlin = path.ends_with(".kt") || path.ends_with(".kts");
    let (tokens, mut unresolved) = lex(text, kotlin);
    let context = LexicalContext::new(&tokens);
    if context.unbalanced_parentheses {
        unresolved.push("unbalanced parentheses; lexical dispatch is unresolved".to_owned());
    }
    if !kotlin && text.contains("\\u") {
        unresolved.push(
            "Java Unicode-escape preprocessing is unresolved; lexical declarations may differ"
                .to_owned(),
        );
    }
    let mut declarations = Declarations {
        unresolved,
        ..Declarations::default()
    };
    let lines = text.lines().collect::<Vec<_>>();
    let test_path = path.to_ascii_lowercase().contains("test");
    let mut depth = 0_usize;
    let mut suites: Vec<(usize, String)> = Vec::new();
    let mut pending_class: Option<(usize, String)> = None;
    let mut metadata_bytes = 0;
    for (index, token) in tokens.iter().enumerate() {
        if declarations.classes.len() + declarations.methods.len() >= MAX_DECLARATIONS {
            declarations
                .unresolved
                .push("declaration budget exhausted; remaining declarations unresolved".to_owned());
            break;
        }
        if token.is("package") {
            let mut parts = Vec::new();
            for following in tokens.iter().skip(index + 1) {
                if following.line != token.line || !(following.identifier || following.text == ".")
                {
                    break;
                }
                parts.push(following.text);
            }
            if !parts.is_empty() {
                let package = parts.concat();
                if !reserve_metadata(&mut declarations, &mut metadata_bytes, package.len()) {
                    break;
                }
                declarations.package = Some(package);
            }
        }
        if token.is("@")
            && let Some(annotation) = annotation_at(&tokens, index)
        {
            if !reserve_metadata(&mut declarations, &mut metadata_bytes, annotation.len()) {
                break;
            }
            declarations.annotations.push(Annotation {
                name: annotation,
                line: token.line,
            });
        }
        if !token.escaped_identifier
            && matches!(token.text, "class" | "interface" | "object" | "record")
            && tokens
                .get(index.wrapping_sub(1))
                .is_none_or(|previous| !previous.is("."))
            && let Some(name) = tokens.get(index + 1).filter(|name| name.identifier)
        {
            let end = context
                .body_boundary
                .get(index + 2)
                .copied()
                .unwrap_or(tokens.len());
            let declaration = source_line(&lines, token.line);
            let (prefix, prefix_truncated) = declaration_prefix(&tokens, index, &context);
            if prefix_truncated {
                unresolved_once(
                    &mut declarations,
                    "declaration prefix exceeds token budget; dispatch/modifier discovery unresolved",
                );
            }
            let header = tokens.get(index + 2..end).unwrap_or_default();
            let header_bytes = header
                .iter()
                .take(MAX_PREFIX_TOKENS + 1)
                .map(|token| token.text.len() + 1)
                .sum::<usize>();
            let inheritance_declaration_complete =
                header.len() <= MAX_PREFIX_TOKENS && header_bytes <= MAX_HEADER_BYTES;
            let inheritance_declaration = if inheritance_declaration_complete {
                header
                    .iter()
                    .map(|token| token.text)
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                unresolved_once(
                    &mut declarations,
                    "class inheritance header exceeds bounded discovery; full member hash retained",
                );
                String::new()
            };
            if !reserve_metadata(
                &mut declarations,
                &mut metadata_bytes,
                name.text.len() + declaration.len() + inheritance_declaration.len(),
            ) {
                break;
            }
            declarations.classes.push(ClassDeclaration {
                name: name.text.to_owned(),
                line: token.line,
                declaration,
                abstract_hint: prefix.iter().any(|token| token.is("abstract")),
                inheritance_declaration,
                inheritance_declaration_complete,
            });
            pending_class = Some((end, name.text.to_owned()));
        }
        let (java_eligible, java_prefix_truncated) = if !kotlin
            && token.identifier
            && tokens
                .get(index + 1)
                .is_some_and(|following| following.is("("))
            && suites
                .last()
                .is_some_and(|(suite_depth, _)| *suite_depth == depth)
        {
            java_method(&tokens, index, &context)
        } else {
            (false, false)
        };
        if java_prefix_truncated {
            unresolved_once(
                &mut declarations,
                "declaration prefix exceeds token budget; dispatch/modifier discovery unresolved",
            );
        }
        let method = if kotlin && token.is("fun") {
            let open = context
                .function_boundary
                .get(index + 1)
                .copied()
                .unwrap_or(tokens.len());
            tokens
                .get(open)
                .filter(|token| token.is("("))
                .and_then(|_| {
                    tokens
                        .get(open.wrapping_sub(1))
                        .filter(|name| name.identifier)
                        .map(|name| (index, name))
                })
        } else if java_eligible {
            Some((index, token))
        } else {
            None
        };
        if let Some((method_index, name)) = method {
            let (prefix, prefix_truncated) = declaration_prefix(&tokens, method_index, &context);
            if prefix_truncated {
                unresolved_once(
                    &mut declarations,
                    "declaration prefix exceeds token budget; dispatch/modifier discovery unresolved",
                );
            }
            let prefix = if kotlin {
                let start = prefix
                    .iter()
                    .enumerate()
                    .rposition(|(index, token)| {
                        !token.escaped_identifier
                            && matches!(
                                token.text,
                                "fun" | "val" | "var" | "class" | "interface" | "object"
                            )
                            && prefix
                                .get(index.wrapping_sub(1))
                                .is_none_or(|previous| !matches!(previous.text, ":" | "."))
                    })
                    .map_or(0, |position| position + 1);
                &prefix[start..]
            } else {
                prefix
            };
            let annotations = annotations_in(prefix);
            let mut reasons = Vec::new();
            if java_prefix_truncated {
                reasons.push(
                    "structural method candidate with truncated prefix; dispatch unresolved"
                        .to_owned(),
                );
            }
            if !annotations.is_empty() {
                reasons
                    .push("annotated declaration (aliases/custom dispatch unresolved)".to_owned());
            }
            if name.text.starts_with("test") {
                reasons.push(
                    "test-prefixed declaration (JUnit3/custom dispatch unresolved)".to_owned(),
                );
            }
            if annotations.iter().any(|annotation| {
                matches!(
                    annotation.rsplit('.').next(),
                    Some(
                        "Test"
                            | "ParameterizedTest"
                            | "RepeatedTest"
                            | "TestFactory"
                            | "TestTemplate"
                    )
                )
            }) {
                reasons.push(
                    "test annotation candidate (imports/runner dispatch unresolved)".to_owned(),
                );
            }
            if matches!(name.text, "suite" | "testSuite" | "createTests") {
                reasons.push("suite factory candidate (runtime expansion unresolved)".to_owned());
            }
            if test_path {
                reasons.push(
                    "declaration in test-like path (helper/fixture/custom test role unresolved)"
                        .to_owned(),
                );
            }
            if !reasons.is_empty() {
                let suite_parts = suites
                    .iter()
                    .map(|(_, name)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                let suite_candidate =
                    (!suite_parts.is_empty()).then(|| match &declarations.package {
                        Some(package) => format!("{package}.{suite_parts}"),
                        None => suite_parts,
                    });
                let declaration = source_line(&lines, name.line);
                let additional = suite_candidate.as_ref().map_or(0, String::len)
                    + name.text.len()
                    + declaration.len()
                    + annotations.iter().map(String::len).sum::<usize>()
                    + reasons.iter().map(String::len).sum::<usize>();
                if !reserve_metadata(&mut declarations, &mut metadata_bytes, additional) {
                    break;
                }
                declarations.methods.push(MethodDeclaration {
                    suite_candidate,
                    name: name.text.to_owned(),
                    line: name.line,
                    declaration,
                    annotations,
                    reasons,
                    effective_runtime_cases: None,
                });
            }
        }
        if token.is("{") {
            depth += 1;
            if pending_class
                .as_ref()
                .is_some_and(|(open, _)| *open == index)
                && let Some((_, name)) = pending_class.take()
            {
                suites.push((depth, name));
            }
        } else if token.is("}") {
            if depth == 0 {
                declarations
                    .unresolved
                    .push("unbalanced closing brace".to_owned());
            }
            while suites
                .last()
                .is_some_and(|(suite_depth, _)| *suite_depth == depth)
            {
                suites.pop();
            }
            depth = depth.saturating_sub(1);
        }
    }
    if depth != 0 {
        declarations
            .unresolved
            .push("unbalanced opening brace".to_owned());
    }
    declarations
}

fn java_method(tokens: &[Token<'_>], index: usize, context: &LexicalContext) -> (bool, bool) {
    let (prefix, truncated) = declaration_prefix(tokens, index, context);
    if prefix.is_empty() {
        return (false, truncated);
    }
    let mut annotation_parentheses = 0_usize;
    for token in prefix {
        if token.is("(") {
            annotation_parentheses += 1;
        } else if token.is(")") {
            annotation_parentheses = annotation_parentheses.saturating_sub(1);
        } else if !truncated
            && annotation_parentheses == 0
            && matches!(token.text, "=" | "new" | "return" | "throw")
        {
            return (false, truncated);
        }
    }
    if !tokens
        .get(index.wrapping_sub(1))
        .is_some_and(|token| token.identifier || matches!(token.text, ">" | "]"))
    {
        return (false, truncated);
    }
    let Some(close) = context
        .closing_parenthesis
        .get(index + 1)
        .copied()
        .flatten()
    else {
        return (false, truncated);
    };
    let next = context
        .method_tail_boundary
        .get(close + 1)
        .copied()
        .unwrap_or(tokens.len());
    (
        tokens
            .get(next)
            .is_some_and(|token| token.is("{") || token.is(";")),
        truncated,
    )
}

fn declaration_prefix<'a, 'b>(
    tokens: &'a [Token<'b>],
    index: usize,
    context: &LexicalContext,
) -> (&'a [Token<'b>], bool) {
    let full_start = context.prefix_start.get(index).copied().unwrap_or(index);
    let start = full_start.max(index.saturating_sub(MAX_PREFIX_TOKENS));
    (&tokens[start..index], start != full_start)
}

fn annotation_at(tokens: &[Token<'_>], index: usize) -> Option<String> {
    let mut position = index + 1;
    if tokens
        .get(position + 1)
        .is_some_and(|token| token.text == ":")
    {
        position += 2;
    }
    let first = tokens.get(position).filter(|token| token.identifier)?;
    let mut name = first.text.to_owned();
    while tokens
        .get(position + 1)
        .is_some_and(|token| token.text == ".")
        && let Some(next) = tokens.get(position + 2).filter(|token| token.identifier)
    {
        name.push('.');
        name.push_str(next.text);
        position += 2;
    }
    Some(name)
}

fn annotations_in(tokens: &[Token<'_>]) -> Vec<String> {
    tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| token.is("@"))
        .filter_map(|(index, _)| annotation_at(tokens, index))
        .collect()
}

fn source_line(lines: &[&str], line: usize) -> String {
    lines
        .get(line.saturating_sub(1))
        .copied()
        .unwrap_or_default()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use serde_json::{Value, json};

    fn method_names(declarations: &Declarations) -> BTreeSet<&str> {
        declarations
            .methods
            .iter()
            .map(|method| method.name.as_str())
            .collect()
    }

    #[test]
    fn comments_and_literals_do_not_invent_test_declarations() {
        let source = r#"package example
// @Test fun testComment() {}
/* class Fake { /* nested */ @Test fun testNestedComment() {} } */
class Real {
  val string = "@Test fun testString() {}"
  val multiline = """
    @Test fun testRawString() {}
  """
  @Test fun actual() {}
}"#;
        let scanned = discover_jvm(source, "src/Real.kt");
        assert_eq!(method_names(&scanned), BTreeSet::from(["actual"]));
        assert_eq!(scanned.classes.len(), 1);
        assert_eq!(
            scanned.methods[0].suite_candidate.as_deref(),
            Some("example.Real")
        );
        assert_eq!(scanned.methods[0].line, 9);
        assert!(scanned.unresolved.is_empty());
    }

    #[test]
    fn backtick_nested_parameterized_and_dynamic_declarations_remain_unexpanded() {
        let source = r#"package example
@RunWith(Parameterized::class)
abstract class Outer : InheritedSuite() {
  @ParameterizedTest fun `same value with spaces`(value: Int) {}
  class Nested {
    @org.junit.jupiter.api.TestFactory fun generated() = streamOfTests()
    fun suite() = buildSuite()
  }
}"#;
        let scanned = discover_jvm(source, "tests/Outer.kt");
        assert!(scanned.classes[0].abstract_hint);
        assert!(
            scanned.classes[0]
                .inheritance_declaration
                .contains("InheritedSuite")
        );
        assert!(method_names(&scanned).contains("same value with spaces"));
        assert!(
            scanned
                .methods
                .iter()
                .all(|method| method.effective_runtime_cases.is_none())
        );
        let generated = scanned
            .methods
            .iter()
            .find(|method| method.name == "generated")
            .expect("dynamic factory candidate");
        assert_eq!(
            generated.suite_candidate.as_deref(),
            Some("example.Outer.Nested")
        );
        assert_eq!(generated.annotations, ["org.junit.jupiter.api.TestFactory"]);
        assert!(
            scanned
                .annotations
                .iter()
                .any(|annotation| annotation.name == "RunWith")
        );
    }

    #[test]
    fn escaped_keyword_and_brace_names_do_not_change_lexical_structure() {
        let scanned = discover_jvm(
            "class Example { @Test fun `fun`() {} @Test fun `}`() {} @Test fun `package`() {} }",
            "src/Example.kt",
        );
        assert_eq!(scanned.methods.len(), 3);
        assert_eq!(
            method_names(&scanned),
            BTreeSet::from(["fun", "}", "package"])
        );
        assert!(scanned.unresolved.is_empty());
        assert!(
            scanned
                .methods
                .iter()
                .all(|method| method.suite_candidate.as_deref() == Some("Example"))
        );
    }

    #[test]
    fn junit3_and_suite_factories_do_not_require_annotations() {
        let source = "package example;\nabstract class Parent extends TestCase { public void testInherited() {} }\nclass Child extends Parent { public static Test suite() { return makeSuite(); } public void testOwn() throws Exception {} }";
        let scanned = discover_jvm(source, "src/Child.java");
        assert_eq!(
            method_names(&scanned),
            BTreeSet::from(["testInherited", "testOwn", "suite"])
        );
        assert!(scanned.classes[0].abstract_hint);
        assert_eq!(
            scanned
                .methods
                .iter()
                .filter(|method| method.name == "testInherited")
                .count(),
            1
        );
        assert!(
            scanned
                .methods
                .iter()
                .all(|method| method.effective_runtime_cases.is_none())
        );
    }

    #[test]
    fn java_method_calls_and_field_initializers_are_not_methods() {
        let source = "class Example { Object value = testField(); @Test public void actual() { testCall(); helper(); } void helper() {} }";
        let scanned = discover_jvm(source, "src/Example.java");
        assert_eq!(method_names(&scanned), BTreeSet::from(["actual"]));
    }

    #[test]
    fn custom_dispatch_and_expression_helpers_are_retained_without_annotation_bleed() {
        let scanned = discover_jvm(
            "class AliasTest {\n @Alias fun custom() = true\n @Test fun actual() = true\n fun helper() = false\n}",
            "tests/AliasTest.kt",
        );
        assert_eq!(
            method_names(&scanned),
            BTreeSet::from(["custom", "actual", "helper"])
        );
        let helper = scanned
            .methods
            .iter()
            .find(|method| method.name == "helper")
            .expect("retained helper");
        assert!(helper.annotations.is_empty());
        assert!(
            scanned
                .methods
                .iter()
                .all(|method| method.effective_runtime_cases.is_none())
        );
        let alias = discover_jvm(
            "import org.junit.Test as Case\nclass Example { @Case fun aliased() {} }",
            "src/Example.kt",
        );
        assert_eq!(method_names(&alias), BTreeSet::from(["aliased"]));
        assert_eq!(alias.methods[0].annotations, ["Case"]);
    }

    #[test]
    fn malformed_sources_and_unicode_names_remain_visible() {
        let scanned = discover_jvm(
            "class UnicodeTest { @Test fun `emoji 😀` () {} /* unfinished",
            "tests/UnicodeTest.kt",
        );
        assert!(method_names(&scanned).contains("emoji 😀"));
        assert!(
            scanned
                .unresolved
                .iter()
                .any(|reason| reason.contains("unterminated block"))
        );
        assert!(
            scanned
                .unresolved
                .iter()
                .any(|reason| reason.contains("unbalanced opening"))
        );
        let (_, unresolved) = lex("\"unfinished", true);
        assert_eq!(unresolved, ["unterminated string or character literal"]);
    }

    #[test]
    fn lexical_resource_limits_preserve_explicit_unresolved_discovery() {
        let long_line = format!("class Example {{ {} }}", "x".repeat(MAX_LINE_BYTES));
        let long = discover_jvm(&long_line, "tests/Example.kt");
        assert!(long.methods.is_empty());
        assert!(
            long.unresolved
                .iter()
                .any(|reason| reason.contains("line exceeds"))
        );
        let many_tokens = ";".repeat(MAX_TOKENS + 1);
        let (_, unresolved) = lex(&many_tokens, true);
        assert!(
            unresolved
                .iter()
                .any(|reason| reason.contains("token budget"))
        );
        let mut many_methods = "class Example {\n".to_owned();
        for index in 0..MAX_DECLARATIONS {
            many_methods.push_str(&format!("fun test{index}() {{}}\n"));
        }
        many_methods.push('}');
        let scanned = discover_jvm(&many_methods, "tests/Example.kt");
        assert!(
            scanned
                .unresolved
                .iter()
                .any(|reason| reason.contains("declaration budget"))
        );
        assert!(
            scanned
                .methods
                .iter()
                .all(|method| method.effective_runtime_cases.is_none())
        );
    }

    #[test]
    fn delimiter_free_class_headers_do_not_retain_repeated_source_suffixes() {
        let source = (0..40_000)
            .map(|index| format!("class C{index:05}\n"))
            .collect::<String>();
        let scanned = discover_jvm(&source, "src/Incomplete.kt");
        assert!(
            scanned
                .classes
                .iter()
                .all(|class| class.inheritance_declaration.len() <= MAX_HEADER_BYTES)
        );
        assert!(
            scanned
                .classes
                .iter()
                .any(|class| !class.inheritance_declaration_complete)
        );
        assert!(
            scanned
                .unresolved
                .iter()
                .any(|reason| reason.contains("inheritance header"))
        );
        let retained = scanned
            .classes
            .iter()
            .map(|class| {
                class.name.len() + class.declaration.len() + class.inheritance_declaration.len()
            })
            .sum::<usize>();
        assert!(retained <= MAX_METADATA_BYTES);
    }

    #[test]
    fn expression_helpers_have_bounded_prefix_work_without_becoming_tests() {
        let source = (0..14_000)
            .map(|index| format!("fun helper{index:05}() = 0\n"))
            .collect::<String>();
        let (tokens, _) = lex(&source, true);
        let context = LexicalContext::new(&tokens);
        for index in 0..tokens.len() {
            assert!(declaration_prefix(&tokens, index, &context).0.len() <= MAX_PREFIX_TOKENS);
        }
        let scanned = discover_jvm(&source, "src/Helpers.kt");
        assert!(scanned.methods.is_empty());
        assert!(
            scanned
                .unresolved
                .iter()
                .any(|reason| reason.contains("prefix exceeds"))
        );
    }

    #[test]
    fn nested_suite_strings_consume_the_retained_metadata_budget() {
        let mut source = "package example\n".to_owned();
        for index in 0..100 {
            source.push_str(&format!("class C{index}_{} {{\n", "LongName".repeat(16)));
        }
        for index in 0..1000 {
            source.push_str(&format!("fun test{index}() {{}}\n"));
        }
        source.push_str(&"}\n".repeat(100));
        let scanned = discover_jvm(&source, "tests/Nested.kt");
        assert!(
            scanned
                .unresolved
                .iter()
                .any(|reason| reason.contains("metadata budget"))
        );
        let retained = scanned
            .classes
            .iter()
            .map(|class| {
                class.name.len() + class.declaration.len() + class.inheritance_declaration.len()
            })
            .sum::<usize>()
            + scanned
                .methods
                .iter()
                .map(|method| {
                    method.suite_candidate.as_ref().map_or(0, String::len)
                        + method.name.len()
                        + method.declaration.len()
                        + method.annotations.iter().map(String::len).sum::<usize>()
                        + method.reasons.iter().map(String::len).sum::<usize>()
                })
                .sum::<usize>();
        assert!(retained <= MAX_METADATA_BYTES);
        assert!(
            scanned
                .methods
                .iter()
                .all(|method| method.effective_runtime_cases.is_none())
        );
    }

    #[test]
    fn java_comments_and_unicode_preprocessing_limits_are_explicit() {
        let source =
            "class Example { /* Java comments /* do not nest */ @Test public void actual() {} }";
        let scanned = discover_jvm(source, "src/Example.java");
        assert_eq!(method_names(&scanned), BTreeSet::from(["actual"]));
        let escaped = discover_jvm(
            r"class Escaped { \u0040Test public void example() {} }",
            "src/Escaped.java",
        );
        assert!(
            escaped
                .unresolved
                .iter()
                .any(|reason| reason.contains("Unicode-escape"))
        );
    }

    #[test]
    fn annotation_arguments_do_not_remove_test_annotations() {
        let kotlin = discover_jvm(
            "class Example { @Test(expected = Failure::class) fun actual() {} }",
            "src/Example.kt",
        );
        let java = discover_jvm(
            "class Example { @Test(tags = {\"slow\"}) public void actual() {} }",
            "src/Example.java",
        );
        for declarations in [&kotlin, &java] {
            assert_eq!(method_names(declarations), BTreeSet::from(["actual"]));
            assert_eq!(declarations.methods[0].annotations, ["Test"]);
        }
    }

    #[test]
    fn long_valid_junit_annotation_keeps_an_unresolved_structural_method() {
        let additions = vec!["1"; 601].join(" + ");
        let source = format!(
            "class Example {{ @org.junit.Test(timeout = {additions}, expected = Failure.class) public void actual() {{}} }}"
        );
        let scanned = discover_jvm(&source, "src/Example.java");
        assert!(method_names(&scanned).contains("actual"));
        assert!(
            scanned
                .unresolved
                .iter()
                .any(|reason| reason.contains("prefix exceeds"))
        );
        let actual = scanned
            .methods
            .iter()
            .find(|method| method.name == "actual")
            .expect("structural method retained");
        assert!(
            actual
                .reasons
                .iter()
                .any(|reason| reason.contains("truncated prefix"))
        );
        assert!(actual.effective_runtime_cases.is_none());
    }

    #[test]
    fn complete_original_fixtures_keep_hashes_lines_and_declared_tests() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/reference_census");
        let provenance: Value = serde_json::from_reader(File::open(root.join("provenance.json"))?)?;
        let fixtures = provenance["fixtures"].as_array().context("fixture list")?;
        let expected: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::from([
            (
                "DefaultVariantsTest.kt",
                BTreeSet::from([
                    "oneVariant",
                    "debugPreferred",
                    "preferredBuildType",
                    "preferredProductFlavorOverDebugBuildType",
                    "preferredFlavorsInTwoDimensions",
                    "preferredFlavorInSecondDimensionOnly",
                    "mismatchedProductFlavourLength",
                    "onEmpty",
                ]),
            ),
            (
                "GradleModuleImportTest.java",
                BTreeSet::from([
                    "testImportSimpleGradleProject",
                    "testImportSubprojects",
                    "testImportSubProjectsWithMissingSubModule",
                    "testImportSubProjectWithCustomLocation",
                    "testRequiredProjects",
                    "testMissingRequiredProjects",
                    "testMissingEnclosingProject",
                    "testTransitiveDependencies",
                    "testCircularDependencies",
                ]),
            ),
            (
                "TabbedToolbarTest.kt",
                BTreeSet::from([
                    "componentIsAddedElement",
                    "tabIsAdded",
                    "closedIsCalledWhenClicked",
                    "noCloseButtonWhenNoListener",
                    "iconButtonsCallbackWhenClicked",
                    "can select tab by index",
                    "adding tab should not trigger select listener",
                ]),
            ),
        ]);
        for fixture in fixtures {
            let path = fixture["path"].as_str().context("fixture source path")?;
            let file = root.join("idea").join(path);
            assert_eq!(
                hash_file(&file)?,
                fixture["sha256"].as_str().context("fixture hash")?
            );
            let text = fs::read_to_string(file)?;
            let scanned = discover_jvm(&text, path);
            let basename = Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .context("fixture name")?;
            let names = scanned
                .methods
                .iter()
                .filter(|method| {
                    method
                        .annotations
                        .iter()
                        .any(|annotation| annotation == "Test")
                        || method.name.starts_with("test")
                })
                .map(|method| method.name.as_str())
                .collect::<BTreeSet<_>>();
            assert_eq!(
                &names,
                expected.get(basename).context("expected fixture methods")?,
                "{basename}"
            );
            for method in &scanned.methods {
                assert_eq!(
                    text.lines().nth(method.line - 1).map(str::trim),
                    Some(method.declaration.as_str())
                );
            }
        }
        Ok(())
    }

    fn archive_bytes(files: &[(&str, &[u8])]) -> Result<Vec<u8>> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *bytes)?;
        }
        Ok(builder.into_inner()?)
    }

    fn fixture_archive(root: &Path, files: &[(&str, &[u8])]) -> Result<Source> {
        write_archive(root, &archive_bytes(files)?)
    }

    fn write_archive(root: &Path, tar: &[u8]) -> Result<Source> {
        let path = root.join("idea.tar.gz");
        let mut writer = GzEncoder::new(File::create(&path)?, Compression::fast());
        writer.write_all(tar)?;
        writer.finish()?;
        Ok(Source {
            id: "idea".to_owned(),
            revision: "a84efec3ba9542d9bfa1255103f0dc94833a3796".to_owned(),
            repository: "pinned-test-source".to_owned(),
            coverage: "canonical".to_owned(),
            archive: Artifact {
                path: "idea.tar.gz".to_owned(),
                sha256: hash_file(&path)?,
            },
        })
    }

    #[test]
    fn streaming_census_retains_fixtures_unsupported_sources_and_unknown_cases() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let output = tempfile::tempdir()?;
        let source = fixture_archive(
            directory.path(),
            &[
                ("testData/Fake.kt", b"class Fake { fun testFixture() {} }"),
                ("src/native_test.cc", b"TEST(Suite, Case) {}"),
                ("BUILD", b"cc_test(name='native')"),
                ("tests/runner.py", b"def test_python(): pass"),
                ("scripts/test", b"#!/bin/sh\ntrue"),
                ("opaque.bin", &[0, 255]),
            ],
        )?;
        let summary = discover_archive(directory.path(), output.path(), source)?;
        assert!(summary.all_members_traversed);
        assert_eq!(summary.member_kinds["file"], 6);
        assert_eq!(summary.candidate_kinds["jvm_source"], 1);
        assert_eq!(summary.candidate_kinds["build_runner"], 1);
        assert_eq!(summary.candidate_kinds["other_source_or_runner"], 3);
        let rows = fs::read_to_string(output.path().join("idea-candidates.jsonl"))?
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let fixture = rows
            .iter()
            .find(|row| row["path"] == "testData/Fake.kt")
            .context("fixture retained")?;
        assert_eq!(fixture["fixture_path_hint"], true);
        assert!(fixture["declarations"]["methods"][0]["effective_runtime_cases"].is_null());
        assert!(
            rows.iter()
                .filter(|row| row["kind"] != "jvm_source")
                .all(|row| row["unresolved"]
                    .as_array()
                    .is_some_and(|reasons| reasons.len() >= 2))
        );
        Ok(())
    }

    #[test]
    fn oversized_and_non_utf8_candidates_keep_payload_hashes_and_blockers() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let output = tempfile::tempdir()?;
        let large = vec![b'x'; MAX_SOURCE_BYTES as usize + 1];
        let source = fixture_archive(
            directory.path(),
            &[("LargeTest.java", &large), ("InvalidTest.kt", &[255])],
        )?;
        let summary = discover_archive(directory.path(), output.path(), source)?;
        assert_eq!(summary.files_with_unresolved_lexing, 2);
        let rows = fs::read_to_string(output.path().join("idea-candidates.jsonl"))?
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(rows[0]["sha256"], format!("{:x}", Sha256::digest(&large)));
        assert!(rows.iter().all(|row| row["declarations"].is_null()));
        assert!(
            rows[0]["unresolved"][1]
                .as_str()
                .is_some_and(|reason| reason.contains("buffer limit"))
        );
        assert!(
            rows[1]["unresolved"][1]
                .as_str()
                .is_some_and(|reason| reason.contains("non-UTF-8"))
        );
        Ok(())
    }

    #[test]
    fn extensionless_shebang_runners_and_unclassified_payloads_remain_visible() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let output = tempfile::tempdir()?;
        let source = fixture_archive(
            directory.path(),
            &[
                ("scripts/check", b"#!/usr/bin/python3\nprint('test')"),
                ("opaque.bin", &[0, 255]),
            ],
        )?;
        let summary = discover_archive(directory.path(), output.path(), source)?;
        assert_eq!(summary.candidate_kinds["other_source_or_runner"], 1);
        assert_eq!(summary.unclassified_regular_members, 1);
        let members = fs::read_to_string(output.path().join("idea-members.jsonl"))?
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(
            members[0]["discovery_classification"],
            "other_source_or_runner"
        );
        assert_eq!(
            members[1]["discovery_classification"],
            "unclassified_payload"
        );
        Ok(())
    }

    #[test]
    fn unsafe_and_non_normal_paths_are_rejected() {
        for path in [
            "../test.java",
            "/test.java",
            "a/./test.kt",
            "a//test.kt",
            "a/../test.kt",
            "C:/test.java",
            "a\\test.java",
            "a\n/test.java",
            "",
        ] {
            assert!(safe_path(path, false).is_err(), "{path:?}");
        }
        assert_eq!(
            safe_path("a/test.java", false).expect("safe path"),
            "a/test.java"
        );
        assert_eq!(safe_path("", true).expect("archive root"), "");
    }

    #[test]
    fn pinned_hash_duplicate_paths_and_hidden_trailing_archives_fail() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let output = tempfile::tempdir()?;
        let mut source = fixture_archive(directory.path(), &[("Test.kt", b"class Test")])?;
        source.archive.sha256 = "0".repeat(64);
        assert!(discover_archive(directory.path(), output.path(), source).is_err());
        assert_eq!(fs::read_dir(output.path())?.count(), 0);
        let source = fixture_archive(
            directory.path(),
            &[("same.kt", b"first"), ("same.kt", b"second")],
        )?;
        assert!(discover_archive(directory.path(), output.path(), source).is_err());
        let mut tar = archive_bytes(&[("first.kt", b"first")])?;
        tar.extend_from_slice(&archive_bytes(&[("hidden.kt", b"hidden")])?);
        let source = write_archive(directory.path(), &tar)?;
        assert!(discover_archive(directory.path(), output.path(), source).is_err());
        Ok(())
    }

    #[test]
    fn physical_extension_headers_and_resolved_names_are_both_retained() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let output = tempfile::tempdir()?;
        let long_path = format!("tests/{}/LongTest.kt", "nested/".repeat(30));
        let source = fixture_archive(
            directory.path(),
            &[(long_path.as_str(), b"class LongTest { fun testLong() {} }")],
        )?;
        let summary = discover_archive(directory.path(), output.path(), source)?;
        assert_eq!(summary.physical_member_types.values().sum::<usize>(), 2);
        assert_eq!(summary.physical_member_types["4c"], 1);
        let members = fs::read_to_string(output.path().join("idea-members.jsonl"))?;
        let logical: Value = serde_json::from_str(members.trim())?;
        assert_eq!(logical["path"], long_path);
        let rows = fs::read_to_string(output.path().join("idea-physical-members.jsonl"))?
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["type_byte"], b'L');
        assert!(rows.iter().all(|row| {
            row["header_sha256"]
                .as_str()
                .is_some_and(|hash| hash.len() == 64)
                && row["payload_sha256"]
                    .as_str()
                    .is_some_and(|hash| hash.len() == 64)
        }));
        Ok(())
    }

    #[test]
    fn pax_metadata_payload_and_effective_path_are_retained() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let output = tempfile::tempdir()?;
        let mut builder = tar::Builder::new(Vec::new());
        let effective = "tests/EffectiveTest.kt";
        builder.append_pax_extensions([("path", effective.as_bytes())])?;
        let bytes = b"class EffectiveTest { fun testEffective() {} }";
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, "short.kt", &bytes[..])?;
        let source = write_archive(directory.path(), &builder.into_inner()?)?;
        let summary = discover_archive(directory.path(), output.path(), source)?;
        assert_eq!(summary.physical_member_types["78"], 1);
        assert_eq!(summary.physical_member_types.values().sum::<usize>(), 2);
        let members = fs::read_to_string(output.path().join("idea-members.jsonl"))?;
        let member: Value = serde_json::from_str(members.trim())?;
        assert_eq!(member["path"], effective);
        Ok(())
    }

    #[test]
    fn unsupported_sparse_or_oversized_extension_framing_is_rejected() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let output = tempfile::tempdir()?;
        for (kind, bytes) in [
            (tar::EntryType::GNUSparse, Vec::new()),
            (
                tar::EntryType::XHeader,
                vec![b'x'; MAX_SOURCE_BYTES as usize + 1],
            ),
        ] {
            let mut builder = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, "extended", bytes.as_slice())?;
            let source = write_archive(directory.path(), &builder.into_inner()?)?;
            assert!(discover_archive(directory.path(), output.path(), source).is_err());
        }
        Ok(())
    }

    #[test]
    fn exact_stream_hash_is_rechecked_and_gzip_corruption_fails() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = fixture_archive(directory.path(), &[("Test.kt", b"test")])?;
        let path = directory.path().join(&source.archive.path);
        let expected_bytes = fs::metadata(&path)?.len();
        let mut archive = tar::Archive::new(verified_decoder(&path)?);
        for entry in archive.entries()? {
            std::io::copy(&mut entry?, &mut std::io::sink())?;
        }
        assert!(
            finish_verified_stream(archive.into_inner(), &"0".repeat(64), expected_bytes).is_err()
        );
        let mut bytes = fs::read(&path)?;
        let crc_position = bytes.len() - 8;
        bytes[crc_position] ^= 1;
        fs::write(&path, bytes)?;
        let output = tempfile::tempdir()?;
        assert!(
            discover_physical_members(
                &path,
                &hash_file(&path)?,
                fs::metadata(&path)?.len(),
                &output.path().join("physical.jsonl")
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn publication_never_replaces_a_concurrently_created_directory() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let staging = directory.path().join("staging");
        let output = directory.path().join("output");
        fs::create_dir(&staging)?;
        fs::write(staging.join("summary.json"), b"complete")?;
        fs::create_dir(&output)?;
        fs::write(output.join("user-file"), b"preserved")?;
        assert!(publish_without_replacement(&staging, &output).is_err());
        assert_eq!(fs::read(output.join("user-file"))?, b"preserved");
        assert!(!output.join("summary.json").exists());
        let fresh = directory.path().join("fresh");
        publish_without_replacement(&staging, &fresh)?;
        assert_eq!(fs::read(fresh.join("summary.json"))?, b"complete");
        Ok(())
    }

    #[test]
    fn rejected_checkout_output_has_no_filesystem_side_effects() -> Result<()> {
        let repository = tempfile::tempdir()?;
        let references = tempfile::tempdir()?;
        let output = repository.path().join("generated/census");
        assert!(
            run(AndroidReferenceCensusArgs {
                repository: repository.path().to_owned(),
                reference_root: references.path().to_owned(),
                output: output.clone(),
                check: false
            })
            .is_err()
        );
        assert!(!repository.path().join("generated").exists());
        let output = repository.path().join("census");
        assert!(
            run(AndroidReferenceCensusArgs {
                repository: repository.path().to_owned(),
                reference_root: references.path().to_owned(),
                output: output.clone(),
                check: false
            })
            .is_err()
        );
        assert!(!output.exists());
        Ok(())
    }

    #[test]
    fn deterministic_generation_and_byte_or_artifact_changes_are_detected() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let repository = directory.path().join("repository");
        let references = directory.path().join("references");
        fs::create_dir_all(repository.join("docs/android-studio"))?;
        fs::create_dir(&references)?;
        let source = fixture_archive(
            &references,
            &[("Tests.kt", b"class Tests { @Test fun example() {} }")],
        )?;
        fs::write(
            repository.join("docs/android-studio/reference-manifest.json"),
            serde_json::to_vec(
                &json!({"schema_version":1,"baseline":"pinned fixture","sources":[{"id":source.id,"revision":source.revision,"repository":source.repository,"coverage":source.coverage,"archive":source.archive}]}),
            )?,
        )?;
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        fs::create_dir(&first)?;
        fs::create_dir(&second)?;
        let summary = generate(&repository, &references, &first)?;
        assert!(!summary.effective_test_census_complete);
        assert!(!summary.unresolved.is_empty());
        generate(&repository, &references, &second)?;
        compare_outputs(&first, &second)?;
        fs::write(second.join("summary.json"), b"changed")?;
        assert!(compare_outputs(&first, &second).is_err());
        fs::write(second.join("unexpected.json"), b"extra")?;
        assert!(compare_outputs(&first, &second).is_err());
        Ok(())
    }
}
