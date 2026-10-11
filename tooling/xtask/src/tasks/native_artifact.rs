#![allow(clippy::disallowed_methods, reason = "tooling is exempt")]

//! Exact-source native artifacts. A Git-object manifest authenticates immutable
//! tracked content without claiming that a sparse checkout materializes it. A
//! materialized observation has the same canonical content manifest and a
//! separate physical receipt. Neither observation grants runtime or test credit.

mod bootstrap;
mod package;
mod process;
mod transport;
use process::{OwnedChild, TimedReader};

#[cfg(test)]
mod tests;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{ChildStdin, ChildStdout, Command, Stdio},
    time::Duration,
};

use anyhow::{Context as _, Result, bail, ensure};
use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256};

const REPOSITORY: &str = "anilbeesetti/koda";
const MAX_REQUEST_BYTES: u64 = 8192;
const MAX_MANIFEST_BYTES: u64 = 32 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const MAX_TREE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_OBJECT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 25_000;
const MAX_PATH_BYTES: usize = 1024;
const PIECE_BYTES: u64 = 30 * 1024 * 1024;
const MAX_ZIP_BYTES: u64 = 31 * 1024 * 1024;
const MAX_PIECES: usize = 256;
const ORIGINAL_FLOOR: u64 = 256 * 1024 * 1024;

#[derive(Subcommand)]
pub enum NativeArtifactCommand {
    /// Select a bounded literal request at the exact same-repository PR head.
    SelectRequest(SelectRequestArgs),
    /// Produce canonical content identity; Git-object mode permits sparse input.
    Snapshot(SnapshotArgs),
    /// Verify all tracked physical files against the independently pinned manifest.
    Verify(VerifyArgs),
    /// Run the fixed default-feature Cargo build and record its actual child wait.
    Build(package::BuildArgs),
    /// Bind the successful build, ELF, source and dependencies and split its payload.
    Package(package::PackageArgs),
    /// Explicitly transform owned ELF copies before verifying their delivered loader closure.
    PackageWithLoaderPolicy(package::PackageWithLoaderPolicyArgs),
    /// Run the delivered full app with real keyboard input and retain screenshots.
    GuiSmoke(package::GuiSmokeArgs),
    #[command(hide = true)]
    GuiSmokeWorker(package::GuiSmokeWorkerArgs),
    /// Check bounded metadata and actual supplied GitHub sizes, without downloading.
    Admission(AdmissionArgs),
    /// Authenticate a bounded GitHub ZIP and unpack only literal declared members.
    UnpackGithubZip(transport::UnpackZipArgs),
    /// Authenticate ordered raw pieces and extract only the declared confined payload.
    Extract(package::ExtractArgs),
    /// Construct a new exact checkout; never initialize links in a borrowed source.
    PrepareBootstrapCheckout(bootstrap::PrepareCheckoutArgs),
    /// Export the remotely built controller tool and independently inspectable bootstrap evidence.
    ExportBootstrap(bootstrap::ExportArgs),
    /// Export bounded failure evidence without granting build, test or native acceptance.
    ExportFailureEvidence(bootstrap::FailureArgs),
}

pub fn run(command: NativeArtifactCommand) -> Result<()> {
    match command {
        NativeArtifactCommand::SelectRequest(args) => select_request(args),
        NativeArtifactCommand::Snapshot(args) => snapshot(args),
        NativeArtifactCommand::Verify(args) => verify(args),
        NativeArtifactCommand::Build(args) => package::build(args),
        NativeArtifactCommand::Package(args) => package::package(args),
        NativeArtifactCommand::PackageWithLoaderPolicy(args) => {
            package::package_with_loader_policy(args)
        }
        NativeArtifactCommand::GuiSmoke(args) => package::gui_smoke(args),
        NativeArtifactCommand::GuiSmokeWorker(args) => package::gui_smoke_worker(args),
        NativeArtifactCommand::Admission(args) => admission(args),
        NativeArtifactCommand::UnpackGithubZip(args) => transport::unpack(args),
        NativeArtifactCommand::Extract(args) => package::extract(args),
        NativeArtifactCommand::PrepareBootstrapCheckout(args) => bootstrap::prepare_checkout(args),
        NativeArtifactCommand::ExportBootstrap(args) => bootstrap::export(args),
        NativeArtifactCommand::ExportFailureEvidence(args) => bootstrap::export_failure(args),
    }
}

#[derive(Args)]
pub struct SelectRequestArgs {
    #[arg(long)]
    controller: PathBuf,
    #[arg(long)]
    controller_sha: String,
    #[arg(long)]
    repository: String,
    #[arg(long)]
    head_repository: String,
    #[arg(long)]
    github_output: PathBuf,
}

#[derive(Args, Clone)]
pub struct SourceArgs {
    #[arg(long)]
    source: PathBuf,
    #[arg(long)]
    sha: String,
    #[arg(long)]
    tree: String,
}

#[derive(Clone, Copy, Debug, ValueEnum, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Authentication {
    GitObjects,
    Materialized,
}

#[derive(Args)]
pub struct SnapshotArgs {
    #[command(flatten)]
    source: SourceArgs,
    /// Required: never silently equate sparse Git objects with physical files.
    #[arg(long, value_enum)]
    authentication: Authentication,
    #[arg(long)]
    output: PathBuf,
    /// Separate observation receipt; excluded from the canonical content identity.
    #[arg(long)]
    receipt: PathBuf,
}

#[derive(Args)]
pub struct VerifyArgs {
    #[command(flatten)]
    source: SourceArgs,
    #[arg(long)]
    manifest_sha256: String,
    #[arg(long)]
    receipt: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema_version: u32,
    repository: String,
    source_sha: String,
    source_tree: String,
    source_manifest_sha256: String,
    source_owner_receipt_sha256: String,
}

impl Request {
    fn parse(bytes: &[u8]) -> Result<Self> {
        ensure!(
            !bytes.is_empty() && bytes.len() as u64 <= MAX_REQUEST_BYTES,
            "request must contain 1..8192 bytes"
        );
        // Serde's derived struct deserializer also rejects duplicate named fields.
        let request: Self = serde_json::from_slice(bytes).context("malformed native request")?;
        ensure!(
            request.schema_version == 1 && request.repository == REPOSITORY,
            "unsupported request schema or repository"
        );
        require_hex(&request.source_sha, 40)?;
        require_hex(&request.source_tree, 40)?;
        require_hex(&request.source_manifest_sha256, 64)?;
        require_hex(&request.source_owner_receipt_sha256, 64)?;
        Ok(request)
    }
}

fn require_hex(value: &str, length: usize) -> Result<()> {
    ensure!(
        value.len() == length
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "identity must be exact lowercase hex"
    );
    Ok(())
}

fn relative(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= MAX_PATH_BYTES
            && !value.starts_with('/')
            && !value.contains('\\')
            && !value.chars().any(char::is_control),
        "unconfined or oversized relative path"
    );
    ensure!(
        value
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".." | ".git")),
        "relative path contains an unsafe component"
    );
    Ok(())
}

// Payload includes deliberately generated .git metadata; it still rejects traversal.
fn payload_relative(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= MAX_PATH_BYTES
            && !value.starts_with('/')
            && !value.contains('\\')
            && !value.chars().any(char::is_control)
            && value
                .split('/')
                .all(|part| !matches!(part, "" | "." | "..")),
        "unconfined payload path"
    );
    Ok(())
}

fn normalized_link(path: &str, target: &str) -> Result<String> {
    payload_relative(path)?;
    ensure!(
        !target.is_empty()
            && target.len() <= MAX_PATH_BYTES
            && !target.starts_with('/')
            && !target.contains('\\')
            && !target.chars().any(char::is_control),
        "absolute, invalid or oversized link target"
    );
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                ensure!(parts.pop().is_some(), "link escapes root");
            }
            part => parts.push(part),
        }
    }
    ensure!(!parts.is_empty(), "link targets the payload root");
    Ok(parts.join("/"))
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct FileDigest {
    bytes: u64,
    sha256: String,
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn git_digest(kind: &str, bytes: &[u8]) -> String {
    let mut digest = Sha1::new();
    digest.update(format!("{kind} {}\0", bytes.len()).as_bytes());
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SourceEntry {
    path: String,
    mode: String,
    git_blob: String,
    bytes: u64,
    sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    link_target: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SourceManifest {
    schema_version: u32,
    repository: String,
    commit: String,
    tree: String,
    entries: Vec<SourceEntry>,
}

impl SourceManifest {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1 && self.repository == REPOSITORY,
            "manifest schema or repository differs"
        );
        require_hex(&self.commit, 40)?;
        require_hex(&self.tree, 40)?;
        ensure!(
            !self.entries.is_empty() && self.entries.len() <= MAX_ENTRIES,
            "manifest must have a bounded nonempty tracked set"
        );
        let mut previous: Option<&str> = None;
        let mut total = 0_u64;
        let mut links = BTreeMap::new();
        for entry in &self.entries {
            relative(&entry.path)?;
            ensure!(
                previous.is_none_or(|old| old.as_bytes() < entry.path.as_bytes()),
                "tracked entries must be unique and byte-sorted"
            );
            previous = Some(&entry.path);
            require_hex(&entry.git_blob, 40)?;
            require_hex(&entry.sha256, 64)?;
            total = total
                .checked_add(entry.bytes)
                .context("source size overflow")?;
            ensure!(
                entry.bytes <= MAX_OBJECT_BYTES && total <= MAX_SOURCE_BYTES,
                "tracked source exceeds its byte budget"
            );
            match entry.mode.as_str() {
                "100644" | "100755" => ensure!(
                    entry.link_target.is_none(),
                    "regular entry has a link target"
                ),
                "120000" => {
                    let target = entry.link_target.as_deref().context("link lacks target")?;
                    ensure!(
                        entry.bytes == target.len() as u64
                            && entry.sha256 == sha256(target.as_bytes())
                            && entry.git_blob == git_digest("blob", target.as_bytes()),
                        "link identity differs"
                    );
                    links.insert(entry.path.clone(), normalized_link(&entry.path, target)?);
                }
                _ => bail!("unsupported tracked mode; an explicit implementation is required"),
            }
        }
        ensure!(
            total > 0,
            "zero total source content is not a usable native source"
        );
        validate_link_graph(self.entries.iter().map(|entry| entry.path.as_str()), &links)?;
        Ok(())
    }
}

fn validate_link_graph<'a>(
    paths: impl Iterator<Item = &'a str>,
    links: &BTreeMap<String, String>,
) -> Result<()> {
    let all: BTreeSet<&str> = paths.collect();
    for path in &all {
        let mut prefix = String::new();
        for component in path
            .split('/')
            .take(path.split('/').count().saturating_sub(1))
        {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            ensure!(
                !links.contains_key(&prefix),
                "entry traverses a tracked symlink"
            );
        }
    }
    for path in links.keys() {
        let mut current = path.as_str();
        let mut seen = BTreeSet::new();
        while let Some(next) = links.get(current) {
            ensure!(
                seen.len() < 64 && seen.insert(current),
                "cyclic or excessively deep symlink chain"
            );
            current = next;
        }
        ensure!(
            all.contains(current)
                || all
                    .iter()
                    .any(|path| path.starts_with(&format!("{current}/"))),
            "link target is outside the declared payload or missing"
        );
    }
    Ok(())
}

/// Struct field order and byte-sorted entries define canonical v1 bytes. Physical
/// observation fields and JSON maps are deliberately absent from this format.
fn canonical<T: Serialize>(value: &T, limit: u64) -> Result<Vec<u8>> {
    struct Limited {
        bytes: Vec<u8>,
        limit: u64,
    }
    impl Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if (self.bytes.len() as u64)
                .checked_add(bytes.len() as u64)
                .is_none_or(|len| len > self.limit)
            {
                return Err(std::io::Error::other("bounded JSON output exhausted"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Limited {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut output, value)?;
    output.write_all(b"\n")?;
    Ok(output.bytes)
}

fn create_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create new {}", path.display()))?;
    output.write_all(bytes)?;
    output.sync_all()?;
    Ok(())
}

fn write_json<T: Serialize>(path: &Path, value: &T, limit: u64) -> Result<()> {
    create_file(path, &canonical(value, limit)?)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Physical13 {
    dev: u64,
    ino: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    nlink: u64,
    bytes: u64,
    atime_ns: i64,
    mtime_ns: i64,
    ctime_ns: i64,
    blocks: u64,
    blksize: u64,
    rdev: u64,
}

impl Physical13 {
    fn changed_fields(&self, after: &Self) -> Vec<&'static str> {
        let mut changed = Vec::new();
        macro_rules! compare {
            ($($field:ident),+ $(,)?) => {
                $(if self.$field != after.$field {
                    changed.push(stringify!($field));
                })+
            };
        }
        compare!(
            dev, ino, mode, uid, gid, nlink, bytes, atime_ns, mtime_ns, ctime_ns, blocks, blksize,
            rdev,
        );
        changed
    }
}

#[cfg(unix)]
fn physical(metadata: &fs::Metadata) -> Result<Physical13> {
    use std::os::unix::fs::MetadataExt as _;
    let ns = |seconds: i64, nanos: i64| {
        seconds
            .checked_mul(1_000_000_000)
            .and_then(|seconds| seconds.checked_add(nanos))
            .context("physical timestamp overflow")
    };
    Ok(Physical13 {
        dev: metadata.dev(),
        ino: metadata.ino(),
        mode: metadata.mode(),
        uid: metadata.uid(),
        gid: metadata.gid(),
        nlink: metadata.nlink(),
        bytes: metadata.size(),
        atime_ns: ns(metadata.atime(), metadata.atime_nsec())?,
        mtime_ns: ns(metadata.mtime(), metadata.mtime_nsec())?,
        ctime_ns: ns(metadata.ctime(), metadata.ctime_nsec())?,
        blocks: metadata.blocks(),
        blksize: metadata.blksize(),
        rdev: metadata.rdev(),
    })
}

#[cfg(not(unix))]
fn physical(_: &fs::Metadata) -> Result<Physical13> {
    bail!("physical native-artifact authentication requires Unix")
}

#[cfg(target_os = "linux")]
fn open_noatime(path: &Path, directory: bool) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let flags = libc::O_NOATIME
        | libc::O_NOFOLLOW
        | libc::O_CLOEXEC
        | libc::O_NONBLOCK
        | if directory { libc::O_DIRECTORY } else { 0 };
    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
        .with_context(|| format!("no-atime/no-follow open {}", path.display()))?)
}

#[cfg(not(target_os = "linux"))]
fn open_noatime(_: &Path, _: bool) -> Result<File> {
    bail!("no-atime physical authentication requires Linux")
}

#[cfg(target_os = "linux")]
fn open_beneath(root: &File, relative_path: &str, link: bool) -> Result<File> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    relative(relative_path)?;
    let mut directory = root.try_clone()?;
    let mut components = relative_path.split('/').peekable();
    while let Some(component) = components.next() {
        let name = std::ffi::CString::new(component)?;
        let last = components.peek().is_none();
        let flags = libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if last && link {
                libc::O_PATH
            } else {
                libc::O_RDONLY
                    | libc::O_NOATIME
                    | libc::O_NONBLOCK
                    | if last { 0 } else { libc::O_DIRECTORY }
            };
        // SAFETY: live directory FD, terminated component without separators, and
        // owned result FD; O_NOFOLLOW forbids both ancestor and leaf substitution.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        ensure!(
            fd >= 0,
            "confined open failed for {relative_path}: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: successful openat transfers a newly owned descriptor.
        let next = unsafe { File::from_raw_fd(fd) };
        if last {
            return Ok(next);
        }
        directory = next;
    }
    bail!("empty confined path")
}

#[cfg(not(target_os = "linux"))]
fn open_beneath(_: &File, _: &str, _: bool) -> Result<File> {
    bail!("confined native-artifact reads require Linux")
}

#[cfg(target_os = "linux")]
fn link_bytes(file: &File) -> Result<Vec<u8>> {
    use std::os::fd::AsRawFd as _;
    let mut bytes = vec![0; MAX_PATH_BYTES + 1];
    // SAFETY: O_PATH descriptor points at the link itself; empty-path readlinkat
    // reads that inode without pathname following. Linux may still update link
    // atime; readers retain strict full13 inverses, and newly owned output links
    // complete their creation readback before admission freezes a baseline.
    let count = unsafe {
        libc::readlinkat(
            file.as_raw_fd(),
            c"".as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    ensure!(
        count >= 0 && (count as usize) <= MAX_PATH_BYTES,
        "link read failed or exceeds its bound"
    );
    bytes.truncate(count as usize);
    Ok(bytes)
}

#[cfg(not(target_os = "linux"))]
fn link_bytes(_: &File) -> Result<Vec<u8>> {
    bail!("no-atime link reads require Linux")
}

fn read_bounded(path: &Path, limit: u64) -> Result<(Vec<u8>, Physical13)> {
    let mut file = open_noatime(path, false)?;
    let before = physical(&file.metadata()?)?;
    ensure!(
        file.metadata()?.is_file() && before.bytes <= limit,
        "input is not a bounded regular file"
    );
    let mut bytes = Vec::new();
    (&mut file).take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit
            && bytes.len() as u64 == before.bytes
            && before == physical(&file.metadata()?)?
            && before == physical(&fs::symlink_metadata(path)?)?,
        "input changed during no-atime read"
    );
    Ok((bytes, before))
}

fn digest_file(path: &Path, maximum: u64) -> Result<(FileDigest, Physical13)> {
    let mut file = open_noatime(path, false)?;
    let metadata = file.metadata()?;
    let before = physical(&metadata)?;
    ensure!(
        metadata.is_file() && before.bytes <= maximum,
        "expected a bounded regular file: path={} file_type={:?} is_regular={} observed_bytes={} maximum_bytes={}",
        path.display(),
        metadata.file_type(),
        metadata.is_file(),
        before.bytes,
        maximum
    );
    let mut digest = Sha256::new();
    let mut count = 0_u64;
    let mut buffer = vec![0; 256 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        count = count
            .checked_add(read as u64)
            .context("file size overflow")?;
        ensure!(count <= maximum, "file grew beyond its budget");
        digest.update(&buffer[..read]);
    }
    ensure!(
        count == before.bytes
            && before == physical(&file.metadata()?)?
            && before == physical(&fs::symlink_metadata(path)?)?,
        "file changed during digest"
    );
    Ok((
        FileDigest {
            bytes: count,
            sha256: format!("{:x}", digest.finalize()),
        },
        before,
    ))
}

fn bounded_line(reader: &mut impl BufRead, limit: usize) -> Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let count = chunk
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(chunk.len(), |index| index + 1);
        ensure!(
            line.len()
                .checked_add(count)
                .is_some_and(|len| len <= limit),
            "line exceeds its byte budget"
        );
        let complete = chunk[count - 1] == b'\n';
        line.extend_from_slice(&chunk[..count]);
        reader.consume(count);
        if complete {
            return Ok(Some(line));
        }
    }
}

fn command(source: Option<&Path>, program: &str, args: &[&str], maximum: u64) -> Result<Vec<u8>> {
    let mut command = Command::new(program);
    if program == "git" {
        if let Some(source) = source {
            command.args([
                "-c",
                &format!("safe.directory={}", std::path::absolute(source)?.display()),
            ]);
        }
    }
    // Official loader introspection must not inherit LD_PRELOAD/LD_AUDIT or
    // ambient library search overrides. This branch never enters constructors.
    if matches!(
        program,
        "/lib64/ld-linux-x86-64.so.2" | "/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2"
    ) {
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C")
            .env("LC_ALL", "C");
    }
    command
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if let Some(source) = source {
        command.current_dir(source);
    }
    let mut child = OwnedChild::spawn(&mut command, Duration::from_secs(300))
        .with_context(|| format!("spawn {program}"))?;
    let result: Result<Vec<u8>> = (|| {
        let mut output = Vec::new();
        let mut stdout = child.stdout()?;
        (&mut stdout).take(maximum + 1).read_to_end(&mut output)?;
        ensure!(
            output.len() as u64 <= maximum,
            "{program} output exceeds its budget"
        );
        Ok(output)
    })();
    let receipt = child.finish(result.is_err())?;
    receipt.require_closed()?;
    let output = result?;
    receipt.require_success()?;
    Ok(output)
}

struct GitBatch {
    child: OwnedChild,
    input: Option<ChildStdin>,
    output: Option<BufReader<TimedReader<ChildStdout>>>,
}

impl GitBatch {
    fn new(source: &Path) -> Result<Self> {
        let mut command = Command::new("git");
        command
            .current_dir(source)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .args([
                "-c",
                &format!("safe.directory={}", std::path::absolute(source)?.display()),
                "cat-file",
                "--batch",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = OwnedChild::spawn(&mut command, Duration::from_secs(300))?;
        let input = child.child.stdin.take().context("Git stdin absent")?;
        let output = BufReader::new(child.stdout()?);
        Ok(Self {
            child,
            input: Some(input),
            output: Some(output),
        })
    }

    fn object(&mut self, oid: &str, kind: &str) -> Result<Vec<u8>> {
        require_hex(oid, 40)?;
        let input = self.input.as_mut().context("Git batch input closed")?;
        writeln!(input, "{oid}")?;
        input.flush()?;
        let output = self.output.as_mut().context("Git batch output closed")?;
        let header = bounded_line(output, 1024)?.context("Git object header absent")?;
        let header = std::str::from_utf8(&header)?.trim_end();
        let parts: Vec<_> = header.split_whitespace().collect();
        ensure!(
            parts.len() == 3 && parts[0] == oid && parts[1] == kind,
            "Git object identity/type differs"
        );
        let size: u64 = parts[2].parse()?;
        ensure!(
            size <= MAX_OBJECT_BYTES,
            "Git object exceeds its byte budget"
        );
        let mut bytes = Vec::new();
        (&mut *output).take(size).read_to_end(&mut bytes)?;
        ensure!(bytes.len() as u64 == size, "Git object truncated");
        let mut newline = [0];
        output.read_exact(&mut newline)?;
        ensure!(
            newline == [b'\n'] && git_digest(kind, &bytes) == oid,
            "Git object bytes differ from its SHA1"
        );
        Ok(bytes)
    }

    fn finish(mut self) -> Result<()> {
        self.input.take();
        self.output.take();
        self.child.finish(false)?.require_success()?;
        Ok(())
    }
}

#[derive(Clone)]
struct TrackedEntry {
    path: String,
    mode: String,
    blob: String,
}

fn tracked(source: &SourceArgs) -> Result<Vec<TrackedEntry>> {
    require_hex(&source.sha, 40)?;
    require_hex(&source.tree, 40)?;
    let mut batch = GitBatch::new(&source.source)?;
    let commit = batch.object(&source.sha, "commit")?;
    ensure!(
        commit.starts_with(format!("tree {}\n", source.tree).as_bytes()),
        "exact commit tree differs"
    );
    batch.finish()?;
    let raw = command(
        Some(&source.source),
        "git",
        &["ls-tree", "-rz", "--full-tree", &source.sha],
        MAX_TREE_BYTES,
    )?;
    ensure!(raw.last() == Some(&0), "Git tree listing is incomplete");
    let mut entries = Vec::new();
    for record in raw
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        ensure!(
            entries.len() < MAX_ENTRIES,
            "tracked entry budget exhausted"
        );
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .context("malformed Git entry")?;
        let header = std::str::from_utf8(&record[..tab])?;
        let parts: Vec<_> = header.split_whitespace().collect();
        ensure!(
            parts.len() == 3 && parts[1] == "blob",
            "submodules or unsupported Git objects are not portable"
        );
        ensure!(
            matches!(parts[0], "100644" | "100755" | "120000"),
            "unsupported tracked Git mode"
        );
        require_hex(parts[2], 40)?;
        let path = std::str::from_utf8(&record[tab + 1..])?.to_owned();
        relative(&path)?;
        entries.push(TrackedEntry {
            path,
            mode: parts[0].to_owned(),
            blob: parts[2].to_owned(),
        });
    }
    ensure!(
        !entries.is_empty(),
        "empty tracked manifest is not source authentication"
    );
    entries.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    Ok(entries)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceObservation {
    schema_version: u32,
    authentication: Authentication,
    source_commit: String,
    source_tree: String,
    canonical_manifest: FileDigest,
    tracked_entries: usize,
    physically_materialized_entries: usize,
    physical: Vec<PhysicalEntry>,
    full_physical_noatime_inverses_equal: bool,
    sparse_files_were_not_materialized: bool,
    runtime_test_or_reference_credit: u32,
    closure: String,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PhysicalEntry {
    path: String,
    identity: Physical13,
}

fn observe(
    source: &SourceArgs,
    authentication: Authentication,
) -> Result<(SourceManifest, SourceObservation)> {
    let entries = tracked(source)?;
    let root = match authentication {
        Authentication::Materialized => {
            let head = command(Some(&source.source), "git", &["rev-parse", "HEAD"], 1024)?;
            ensure!(
                std::str::from_utf8(&head)?.trim() == source.sha,
                "materialized checkout HEAD differs"
            );
            Some(open_noatime(&source.source, true)?)
        }
        Authentication::GitObjects => None,
    };
    let mut batch = GitBatch::new(&source.source)?;
    let mut rows = Vec::new();
    let mut physical_rows = Vec::new();
    let mut total = 0_u64;
    for entry in entries {
        let bytes = match &root {
            Some(root) => {
                let mut file = open_beneath(root, &entry.path, entry.mode == "120000")?;
                let before = physical(&file.metadata()?)?;
                ensure!(
                    before.bytes <= MAX_OBJECT_BYTES,
                    "physical source byte budget exhausted"
                );
                let bytes = if entry.mode == "120000" {
                    ensure!(
                        file.metadata()?.file_type().is_symlink(),
                        "tracked symlink changed type"
                    );
                    link_bytes(&file)?
                } else {
                    ensure!(
                        file.metadata()?.is_file()
                            && (before.mode & 0o111 != 0) == (entry.mode == "100755"),
                        "tracked regular type/mode differs"
                    );
                    let mut bytes = Vec::new();
                    (&mut file)
                        .take(MAX_OBJECT_BYTES + 1)
                        .read_to_end(&mut bytes)?;
                    bytes
                };
                let after = physical(&file.metadata()?)?;
                ensure!(
                    bytes.len() as u64 == before.bytes
                        && bytes.len() as u64 <= MAX_OBJECT_BYTES
                        && before == after,
                    "physical input changed during read for {}: bytes {} (expected {}), fields {:?}",
                    entry.path,
                    bytes.len(),
                    before.bytes,
                    before.changed_fields(&after)
                );
                let again = open_beneath(root, &entry.path, entry.mode == "120000")?;
                let path_after = physical(&again.metadata()?)?;
                ensure!(
                    before == path_after,
                    "physical path was replaced for {}: fields {:?}",
                    entry.path,
                    before.changed_fields(&path_after)
                );
                physical_rows.push(PhysicalEntry {
                    path: entry.path.clone(),
                    identity: before,
                });
                bytes
            }
            None => batch.object(&entry.blob, "blob")?,
        };
        total = total
            .checked_add(bytes.len() as u64)
            .context("source byte overflow")?;
        ensure!(
            total <= MAX_SOURCE_BYTES && git_digest("blob", &bytes) == entry.blob,
            "source blob mismatch or total budget exhausted"
        );
        let link_target = if entry.mode == "120000" {
            Some(String::from_utf8(bytes.clone())?)
        } else {
            None
        };
        rows.push(SourceEntry {
            path: entry.path,
            mode: entry.mode,
            git_blob: entry.blob,
            bytes: bytes.len() as u64,
            sha256: sha256(&bytes),
            link_target,
        });
    }
    batch.finish()?;
    let manifest = SourceManifest {
        schema_version: 1,
        repository: REPOSITORY.to_owned(),
        commit: source.sha.clone(),
        tree: source.tree.clone(),
        entries: rows,
    };
    manifest.validate()?;
    // The second pass checks all 13 fields for the entire observed set, including
    // source symlink inodes. No source atime is restored or changed by these reads.
    if let Some(root) = &root {
        for (entry, before) in manifest.entries.iter().zip(&physical_rows) {
            let mut file = open_beneath(root, &entry.path, entry.mode == "120000")?;
            ensure!(
                before.identity == physical(&file.metadata()?)?,
                "complete physical inverse differs"
            );
            let bytes = if entry.mode == "120000" {
                link_bytes(&file)?
            } else {
                let mut bytes = Vec::new();
                (&mut file)
                    .take(MAX_OBJECT_BYTES + 1)
                    .read_to_end(&mut bytes)?;
                bytes
            };
            ensure!(
                bytes.len() as u64 == entry.bytes
                    && sha256(&bytes) == entry.sha256
                    && git_digest("blob", &bytes) == entry.git_blob
                    && before.identity == physical(&file.metadata()?)?,
                "complete source SHA/physical inverse differs"
            );
        }
    }
    let canonical = canonical(&manifest, MAX_MANIFEST_BYTES)?;
    let observation = SourceObservation {
        schema_version: 1,
        authentication,
        source_commit: source.sha.clone(),
        source_tree: source.tree.clone(),
        canonical_manifest: FileDigest {
            bytes: canonical.len() as u64,
            sha256: sha256(&canonical),
        },
        tracked_entries: manifest.entries.len(),
        physically_materialized_entries: physical_rows.len(),
        physical: physical_rows,
        full_physical_noatime_inverses_equal: root.is_some(),
        sparse_files_were_not_materialized: root.is_none(),
        runtime_test_or_reference_credit: 0,
        closure: "CLOSED0".to_owned(),
    };
    Ok((manifest, observation))
}

fn snapshot(args: SnapshotArgs) -> Result<()> {
    let (manifest, receipt) = observe(&args.source, args.authentication)?;
    write_json(&args.output, &manifest, MAX_MANIFEST_BYTES)?;
    write_json(&args.receipt, &receipt, MAX_MANIFEST_BYTES)?;
    Ok(())
}

fn verify(args: VerifyArgs) -> Result<()> {
    require_hex(&args.manifest_sha256, 64)?;
    let (_, receipt) = observe(&args.source, Authentication::Materialized)?;
    ensure!(
        receipt.canonical_manifest.sha256 == args.manifest_sha256,
        "independent canonical source manifest differs"
    );
    write_json(&args.receipt, &receipt, MAX_MANIFEST_BYTES)
}

fn select_request(args: SelectRequestArgs) -> Result<()> {
    ensure!(
        args.repository == REPOSITORY && args.head_repository == args.repository,
        "cross-repository native requests are rejected"
    );
    require_hex(&args.controller_sha, 40)?;
    let head = command(Some(&args.controller), "git", &["rev-parse", "HEAD"], 1024)?;
    ensure!(
        std::str::from_utf8(&head)?.trim() == args.controller_sha,
        "controller differs from exact PR head"
    );
    let root = open_noatime(&args.controller, true)?;
    let path = args.controller.join(".ci/native-artifact-request.json");
    let literal = command(
        Some(&args.controller),
        "git",
        &[
            "ls-tree",
            "-z",
            &args.controller_sha,
            "--",
            ".ci/native-artifact-request.json",
        ],
        2048,
    )?;
    let selected = if literal.is_empty() {
        ensure!(
            matches!(fs::symlink_metadata(&path), Err(error) if error.kind() == std::io::ErrorKind::NotFound),
            "untracked literal request is not an absent committed request"
        );
        None
    } else {
        let header = literal
            .split(|byte| *byte == b'\t')
            .next()
            .context("request tree header absent")?;
        let fields: Vec<_> = std::str::from_utf8(header)?.split_whitespace().collect();
        ensure!(
            fields.len() == 3 && fields[1] == "blob" && matches!(fields[0], "100644" | "100755"),
            "committed request is not a regular Git blob"
        );
        let metadata =
            fs::symlink_metadata(&path).context("committed request is physically missing")?;
        ensure!(
            metadata.is_file() && metadata.len() <= MAX_REQUEST_BYTES,
            "request is not a bounded regular file"
        );
        let mut file = open_beneath(&root, ".ci/native-artifact-request.json", false)?;
        let before = physical(&file.metadata()?)?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_REQUEST_BYTES + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            before == physical(&metadata)?
                && before == physical(&file.metadata()?)?
                && before.bytes == bytes.len() as u64,
            "request changed"
        );
        let request = Request::parse(&bytes)?;
        let original = command(
            Some(&args.controller),
            "git",
            &[
                "show",
                &format!("{}:.ci/native-artifact-request.json", args.controller_sha),
            ],
            MAX_REQUEST_BYTES,
        )?;
        ensure!(
            original == bytes
                && before
                    == physical(
                        &open_beneath(&root, ".ci/native-artifact-request.json", false)?
                            .metadata()?
                    )?,
            "request differs from exact controller Git blob or complete physical inverse"
        );
        Some(request)
    };
    // Only validated lowercase identities are written; request text can never
    // inject additional GitHub output keys or shell syntax.
    let mut output = OpenOptions::new().append(true).open(&args.github_output)?;
    if let Some(request) = selected {
        let exact_request = command(
            Some(&args.controller),
            "git",
            &[
                "show",
                &format!("{}:.ci/native-artifact-request.json", args.controller_sha),
            ],
            MAX_REQUEST_BYTES,
        )?;
        writeln!(
            output,
            "selected=true\nsource_sha={}\nsource_tree={}\nsource_manifest_sha256={}\nsource_owner_receipt_sha256={}\nrequest_sha256={}",
            request.source_sha,
            request.source_tree,
            request.source_manifest_sha256,
            request.source_owner_receipt_sha256,
            sha256(&exact_request)
        )?;
    } else {
        writeln!(output, "selected=false")?;
    }
    Ok(())
}

#[derive(Args)]
pub struct AdmissionArgs {
    #[arg(long)]
    metadata: PathBuf,
    #[arg(long)]
    metadata_sha256: String,
    #[arg(long)]
    payload_manifest: PathBuf,
    #[arg(long)]
    source_manifest: PathBuf,
    #[arg(long)]
    piece_manifest: PathBuf,
    #[arg(long)]
    build_evidence: PathBuf,
    #[arg(long)]
    request: PathBuf,
    #[arg(long)]
    request_sha256: String,
    #[arg(long)]
    github_artifacts: PathBuf,
    #[arg(long)]
    github_artifacts_sha256: String,
    #[arg(long)]
    staging: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    original_floor_bytes: u64,
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    scratch_bytes: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GithubArtifacts {
    schema_version: u32,
    repository: String,
    source_sha: String,
    run_id: String,
    run_attempt: String,
    /// Root-bound, actual GitHub API observations; this tool makes no API calls.
    metadata_zip: GithubArtifact,
    piece_zips: Vec<GithubArtifact>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GithubArtifact {
    id: u64,
    name: String,
    bytes: u64,
    sha256: String,
}

#[derive(Debug, Serialize)]
struct DiskPlan {
    state: &'static str,
    outer_zip_bytes: u64,
    raw_piece_bytes: u64,
    reassembled_archive_bytes: u64,
    unpacked_logical_bytes: u64,
    block_padding: u64,
    scratch_bytes: u64,
    original_floor_bytes: u64,
    required_bytes: u64,
    available_bytes: u64,
    required_inodes: u64,
    available_inodes: u64,
    fits: bool,
    memory_host_elf_and_actor_admission_pending: bool,
    downloads_extractions_and_application_launches: u32,
}

fn disk_plan(
    metadata: &package::ArtifactMetadata,
    zip_bytes: u64,
    block: u64,
    available_bytes: u64,
    available_inodes: u64,
    floor: u64,
    scratch: u64,
) -> Result<DiskPlan> {
    ensure!(
        floor >= ORIGINAL_FLOOR && block > 0 && scratch >= 64 * 1024 * 1024,
        "original filesystem floor or scratch reserve cannot be reduced"
    );
    ensure!(
        zip_bytes > 0 && metadata.unpacked_regular_bytes > 0 && metadata.regular_file_count > 0,
        "zero transport or payload does not qualify for admission"
    );
    package::validate_pieces(&metadata.pieces, &metadata.runtime_archive)?;
    let inodes = metadata
        .regular_file_count
        .checked_add(metadata.directory_count)
        .and_then(|count| count.checked_add(metadata.symlink_count))
        .context("inode overflow")?;
    ensure!(inodes > 0, "empty payload does not qualify for admission");
    // Retain every outer ZIP, raw piece and its private unpack directory. The
    // additional 32 cover metadata members, archive, receipts, staging parents
    // and a reserve; none of these consume payload-declared inodes.
    let transport_inodes = u64::try_from(metadata.pieces.len())?
        .checked_mul(3)
        .and_then(|count| count.checked_add(32))
        .context("transport inode overflow")?;
    let required_inodes = inodes
        .checked_add(transport_inodes)
        .context("inode reserve overflow")?;
    let padding = required_inodes
        .checked_mul(block)
        .context("block padding overflow")?;
    let required = zip_bytes
        .checked_add(metadata.runtime_archive.bytes)
        .and_then(|bytes| bytes.checked_add(metadata.runtime_archive.bytes))
        .and_then(|bytes| bytes.checked_add(metadata.unpacked_regular_bytes))
        .and_then(|bytes| bytes.checked_add(padding))
        .and_then(|bytes| bytes.checked_add(scratch))
        .and_then(|bytes| bytes.checked_add(floor))
        .context("disk admission arithmetic overflow")?;
    Ok(DiskPlan {
        state: "DISK_PREFLIGHT_ONLY_NOT_NATIVE_RESOURCE_ADMISSION",
        outer_zip_bytes: zip_bytes,
        raw_piece_bytes: metadata.runtime_archive.bytes,
        reassembled_archive_bytes: metadata.runtime_archive.bytes,
        unpacked_logical_bytes: metadata.unpacked_regular_bytes,
        block_padding: padding,
        scratch_bytes: scratch,
        original_floor_bytes: floor,
        required_bytes: required,
        available_bytes,
        required_inodes,
        available_inodes,
        fits: available_bytes >= required && available_inodes >= required_inodes,
        memory_host_elf_and_actor_admission_pending: true,
        downloads_extractions_and_application_launches: 0,
    })
}

fn authenticated_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
    expected_sha: &str,
    limit: u64,
) -> Result<T> {
    require_hex(expected_sha, 64)?;
    let (bytes, _) = read_bounded(path, limit)?;
    ensure!(
        !bytes.is_empty() && sha256(&bytes) == expected_sha,
        "named metadata digest differs"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

fn admission(args: AdmissionArgs) -> Result<()> {
    let metadata: package::ArtifactMetadata =
        authenticated_json(&args.metadata, &args.metadata_sha256, MAX_METADATA_BYTES)?;
    let request_bytes = read_bounded(&args.request, MAX_REQUEST_BYTES)?.0;
    require_hex(&args.request_sha256, 64)?;
    ensure!(
        sha256(&request_bytes) == args.request_sha256,
        "Root request digest differs"
    );
    let request = Request::parse(&request_bytes)?;
    metadata.validate(&request)?;
    package::validate_metadata_members(
        &metadata,
        &request,
        &args.payload_manifest,
        &args.source_manifest,
        &args.piece_manifest,
        &args.build_evidence,
    )?;
    let github: GithubArtifacts = authenticated_json(
        &args.github_artifacts,
        &args.github_artifacts_sha256,
        MAX_METADATA_BYTES,
    )?;
    ensure!(
        github.schema_version == 1
            && github.repository == REPOSITORY
            && github.source_sha == request.source_sha
            && github.run_id == metadata.run_id
            && github.run_attempt == metadata.run_attempt
            && github.piece_zips.len() == metadata.pieces.len(),
        "actual supplied GitHub run/artifact association differs"
    );
    let prefix = format!(
        "{}-{}-{}",
        request.source_sha, metadata.run_id, metadata.run_attempt
    );
    let mut ids = BTreeSet::new();
    let mut zip_bytes = 0_u64;
    for (order, artifact) in std::iter::once((None, &github.metadata_zip)).chain(
        github
            .piece_zips
            .iter()
            .enumerate()
            .map(|(order, artifact)| (Some(order), artifact)),
    ) {
        let expected_name = match order {
            None => format!("native-metadata-{prefix}"),
            Some(order) => format!("native-piece-{prefix}-{order:06}"),
        };
        ensure!(
            artifact.id > 0
                && ids.insert(artifact.id)
                && artifact.name == expected_name
                && artifact.bytes > 0
                && artifact.bytes <= MAX_ZIP_BYTES,
            "ZIP identity, unique ID or 31MiB limit differs"
        );
        require_hex(&artifact.sha256, 64)?;
        if let Some(order) = order {
            ensure!(
                artifact.bytes >= metadata.pieces[order].bytes,
                "uncompressed outer ZIP is smaller than its piece"
            );
        }
        zip_bytes = zip_bytes
            .checked_add(artifact.bytes)
            .context("ZIP total overflow")?;
    }
    let (block, bytes, inodes) = filesystem_space(&args.staging)?;
    let plan = disk_plan(
        &metadata,
        zip_bytes,
        block,
        bytes,
        inodes,
        args.original_floor_bytes,
        args.scratch_bytes,
    )?;
    write_json(&args.output, &plan, MAX_METADATA_BYTES)?;
    ensure!(
        plan.fits,
        "original filesystem/inode floor would be violated"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn filesystem_space(path: &Path) -> Result<(u64, u64, u64)> {
    use std::os::fd::AsRawFd as _;
    let directory = open_noatime(path, true)?;
    let mut space = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: live directory FD and correctly sized writable statvfs storage.
    ensure!(
        unsafe { libc::fstatvfs(directory.as_raw_fd(), space.as_mut_ptr()) } == 0,
        "statvfs failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: successful fstatvfs initialized the structure.
    let space = unsafe { space.assume_init() };
    let block = space.f_frsize;
    let available = space
        .f_bavail
        .checked_mul(block)
        .context("filesystem capacity overflow")?;
    Ok((block, available, space.f_favail))
}

#[cfg(not(target_os = "linux"))]
fn filesystem_space(_: &Path) -> Result<(u64, u64, u64)> {
    bail!("native artifact admission requires Linux")
}

#[cfg(test)]
mod physical_change_diagnostic_regression {
    use super::*;

    #[test]
    fn diagnostic_fields_identify_each_actual_physical_difference() {
        let before = Physical13 {
            dev: 1,
            ino: 2,
            mode: 0o120777,
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
        };
        assert!(before.changed_fields(&before).is_empty());
        macro_rules! changed {
            ($field:ident) => {
                let mut after = before.clone();
                after.$field += 1;
                assert_eq!(before.changed_fields(&after), vec![stringify!($field)]);
            };
        }
        changed!(dev);
        changed!(ino);
        changed!(mode);
        changed!(uid);
        changed!(gid);
        changed!(nlink);
        changed!(bytes);
        changed!(atime_ns);
        changed!(mtime_ns);
        changed!(ctime_ns);
        changed!(blocks);
        changed!(blksize);
        changed!(rdev);
        let mut after = before.clone();
        after.atime_ns += 1;
        after.bytes += 1;
        after.ino += 1;
        assert_eq!(
            before.changed_fields(&after),
            vec!["ino", "bytes", "atime_ns"]
        );
    }
}
