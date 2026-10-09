//! Rust-owned requests over Gradle's official JVM getter runtime. The JVM bridge
//! reports observations; it never supplies the expected context for a packet.
//! Reflection boundaries follow the Apache-2.0 files in test_data/kotlin_import_facts.

use crate::{
    import_facts::ImportFactsSnapshot,
    kotlin_import_facts::{
        CaptureBinding, CaptureContext, CaptureLimits, CaptureMode, CaptureObject, CaptureValue,
        Consumer, ContainerOrder, GetterArgument, GetterEvent, GetterOutcome, GetterPurpose,
        GetterRequest, ImportIdentity, KotlinFactsSnapshot, MethodCatalogue, MethodSelection,
        MissingMethod, ObjectKind, RequestParameter, ReturnShape, RuntimeIdentity, ValueKind,
        parse_kotlin_facts,
    },
    project_model::ProjectModel,
};
use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const BRIDGE: &str = include_str!("kotlin_getter_capture.gradle");
pub const KOTLIN_PLUGIN_IDS: &[&str] = &[
    "kotlin",
    "kotlin2js",
    "kotlin-android",
    "kotlin-platform-jvm",
    "kotlin-platform-js",
    "kotlin-platform-common",
    "org.jetbrains.kotlin.multiplatform",
    "kotlin-multiplatform",
];
pub const ANDROID_PLUGIN_IDS: &[&str] = &[
    "com.android.application",
    "com.android.library",
    "com.android.dynamic-feature",
    "com.android.test",
    "com.android.kotlin.multiplatform.library",
];
const WRAPPER: &str = "org.jetbrains.kotlin.gradle.plugin.KotlinPluginWrapperKt";
const RESOLVER: &str = "org.jetbrains.kotlin.gradle.plugin.ide.IdeCompilerArgumentsResolver";
const KOTLIN_TASK_CLASSES: &[&str] = &[
    "org.jetbrains.kotlin.gradle.tasks.KotlinCompile_Decorated",
    "org.jetbrains.kotlin.gradle.tasks.KotlinCompileWithWorkers_Decorated",
    "org.jetbrains.kotlin.gradle.tasks.Kotlin2JsCompile_Decorated",
    "org.jetbrains.kotlin.gradle.tasks.KotlinCompileCommon_Decorated",
    "org.jetbrains.kotlin.gradle.tasks.Kotlin2JsCompileWithWorkers_Decorated",
    "org.jetbrains.kotlin.gradle.tasks.KotlinCompileCommonWithWorkers_Decorated",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureFile {
    pub relative_path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}

/// An explicit original/transformed fixture file inventory. Generated build/cache
/// files are outside this boundary; omission of an original file is a caller error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureBoundary {
    pub root: PathBuf,
    pub files: Vec<FixtureFile>,
    pub sha256: String,
}

impl FixtureBoundary {
    pub fn capture(root: &Path, relative_paths: &[PathBuf]) -> Result<Self> {
        let root = root
            .canonicalize()
            .context("Resolve capture fixture root")?;
        let mut paths = BTreeSet::new();
        let mut files = Vec::new();
        let mut digest = Sha256::new();
        for relative_path in relative_paths {
            ensure!(
                !relative_path.as_os_str().is_empty()
                    && relative_path.components().collect::<PathBuf>().as_os_str()
                        == relative_path.as_os_str()
                    && relative_path
                        .components()
                        .all(|part| matches!(part, Component::Normal(_))),
                "Fixture paths must be unambiguous relative file paths"
            );
            ensure!(paths.insert(relative_path), "Duplicate fixture path");
        }
        for relative_path in paths {
            let mut path = root.clone();
            for part in relative_path.components() {
                path.push(part.as_os_str());
                ensure!(
                    !fs::symlink_metadata(&path)?.file_type().is_symlink(),
                    "Fixture path contains a symlink"
                );
            }
            let (bytes, sha256) = hash_regular_file(&path)?;
            let name = relative_path
                .to_str()
                .context("Fixture path is not UTF-8")?;
            digest.update((name.len() as u64).to_be_bytes());
            digest.update(name.as_bytes());
            digest.update(bytes.to_be_bytes());
            digest.update(sha256.as_bytes());
            files.push(FixtureFile {
                relative_path: relative_path.clone(),
                bytes,
                sha256,
            });
        }
        ensure!(
            !files.is_empty(),
            "Capture requires an explicit nonempty fixture inventory"
        );
        Ok(Self {
            root,
            files,
            sha256: format!("{:x}", digest.finalize()),
        })
    }

    pub fn ensure_unchanged(&self) -> Result<()> {
        let paths = self
            .files
            .iter()
            .map(|file| file.relative_path.clone())
            .collect::<Vec<_>>();
        ensure!(
            Self::capture(&self.root, &paths)? == *self,
            "Fixture changed during Kotlin getter capture"
        );
        Ok(())
    }
}

fn hash_regular_file(path: &Path) -> Result<(u64, String)> {
    let before = fs::symlink_metadata(path)?;
    ensure!(
        before.is_file() && !before.file_type().is_symlink(),
        "Runtime/fixture artifact must be a regular file"
    );
    let mut file = File::open(path)?;
    let opened = file.metadata()?;
    ensure!(
        same_file(&before, &opened),
        "Artifact identity changed before opening"
    );
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .context("Artifact byte count overflow")?;
        digest.update(&buffer[..count]);
    }
    let after = file.metadata()?;
    let path_after = fs::symlink_metadata(path)?;
    ensure!(
        same_file(&before, &after) && same_file(&before, &path_after) && bytes == before.len(),
        "Artifact changed while hashing"
    );
    Ok((bytes, format!("{:x}", digest.finalize())))
}

fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        left.dev() == right.dev()
            && left.ino() == right.ino()
            && left.mode() == right.mode()
            && left.len() == right.len()
            && left.mtime() == right.mtime()
            && left.mtime_nsec() == right.mtime_nsec()
            && left.ctime() == right.ctime()
            && left.ctime_nsec() == right.ctime_nsec()
    }
    #[cfg(not(unix))]
    {
        left.is_file() == right.is_file()
            && left.len() == right.len()
            && left.modified().ok() == right.modified().ok()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassLookupAttempt {
    pub loader: String,
    pub class: Option<String>,
    pub failures: Vec<crate::kotlin_import_facts::ExceptionCause>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassLookup {
    pub project: String,
    pub name: String,
    pub classes: Vec<String>,
    pub attempts: Vec<ClassLookupAttempt>,
    pub failures: Vec<crate::kotlin_import_facts::ExceptionCause>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscoveryInventory {
    pub runtime: RuntimeIdentity,
    pub objects: Vec<CaptureObject>,
    pub catalogues: Vec<MethodCatalogue>,
    pub project_objects: BTreeMap<String, String>,
    pub class_lookups: Vec<ClassLookup>,
}

impl DiscoveryInventory {
    fn verify_artifacts(&self) -> Result<()> {
        let mut ids = BTreeSet::new();
        for artifact in &self.runtime.artifacts {
            ensure!(
                ids.insert(&artifact.id),
                "Duplicate runtime artifact identity"
            );
            ensure!(
                artifact.path.is_absolute() && artifact.path.canonicalize()? == artifact.path,
                "Runtime artifact is not canonical"
            );
            let (bytes, digest) = hash_regular_file(&artifact.path)?;
            ensure!(
                bytes == artifact.bytes && digest == artifact.sha256,
                "Runtime artifact bytes/digest disagree with owned discovery"
            );
        }
        ensure!(
            ids.contains(&self.runtime.java.artifact),
            "Java runtime artifact was not independently observed"
        );
        Ok(())
    }

    fn extend(&mut self, next: Self) -> Result<()> {
        ensure!(
            self.runtime.gradle_version == next.runtime.gradle_version
                && self.runtime.java == next.runtime.java
                && self.runtime.locale == next.runtime.locale
                && self.project_objects == next.project_objects
                && self.class_lookups == next.class_lookups,
            "Owned runtime/discovery identity changed"
        );
        preserve_rows(&self.runtime.artifacts, &next.runtime.artifacts, |row| {
            &row.id
        })?;
        preserve_rows(&self.runtime.classes, &next.runtime.classes, |row| &row.id)?;
        preserve_rows(&self.objects, &next.objects, |row| &row.id)?;
        preserve_rows(&self.catalogues, &next.catalogues, |row| &row.id)?;
        let mut loader_ids = BTreeSet::new();
        for loader in &next.runtime.loaders {
            ensure!(loader_ids.insert(&loader.id), "Duplicate runtime loader");
        }
        for loader in &self.runtime.loaders {
            let current = next
                .runtime
                .loaders
                .iter()
                .find(|row| row.id == loader.id)
                .context("Runtime loader disappeared")?;
            ensure!(
                current.parent == loader.parent && current.artifacts.starts_with(&loader.artifacts),
                "Runtime loader provenance was rewritten"
            );
        }
        // New return objects are discovered in a separate frame, before an event
        // can refer to them. Existing rows are never accepted from event metadata.
        for artifact in &next.runtime.artifacts {
            if !self
                .runtime
                .artifacts
                .iter()
                .any(|old| old.id == artifact.id)
            {
                let (bytes, digest) = hash_regular_file(&artifact.path)?;
                ensure!(
                    bytes == artifact.bytes && digest == artifact.sha256,
                    "New runtime artifact differs from discovery"
                );
            }
        }
        *self = next;
        Ok(())
    }

    fn object(&self, id: &str) -> Result<&CaptureObject> {
        self.objects
            .iter()
            .find(|object| object.id == id)
            .context("Getter receiver object was not discovered")
    }

    fn catalogue(&self, owner: &str) -> Result<&MethodCatalogue> {
        let object = self.object(owner)?;
        self.catalogues
            .iter()
            .find(|catalogue| catalogue.class_id == object.class_id)
            .context("Getter receiver lacks a reflected catalogue")
    }

    fn named_class_catalogue(&self, project: &str, name: &str) -> Result<Option<&MethodCatalogue>> {
        let lookup = self
            .class_lookups
            .iter()
            .find(|lookup| lookup.project == project && lookup.name == name)
            .context("Class was not explicitly looked up")?;
        ensure!(
            lookup.classes.len() <= 1,
            "Several plugin loaders resolve the requested class; choose a captured loader explicitly"
        );
        Ok(lookup.classes.first().and_then(|class| {
            self.catalogues
                .iter()
                .find(|catalogue| &catalogue.class_id == class)
        }))
    }
}

fn preserve_rows<T: PartialEq>(before: &[T], after: &[T], id: impl Fn(&T) -> &str) -> Result<()> {
    let mut ids = BTreeSet::new();
    for row in after {
        ensure!(ids.insert(id(row)), "Duplicate discovery identity");
    }
    for row in before {
        ensure!(
            after
                .iter()
                .any(|current| id(current) == id(row) && current == row),
            "Discovery catalogue was overwritten or truncated"
        );
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
enum Response {
    Hello(String),
    Discovery(DiscoveryInventory),
    Event(GetterEvent),
    Finished(()),
    Failure(String),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DiscoveryRequest<'a> {
    projects: &'a [String],
    class_names: &'a [&'a str],
}

#[derive(Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
enum Request<'a> {
    Discover(DiscoveryRequest<'a>),
    Invoke(&'a GetterRequest),
    Finish(()),
}

/// A transport must deliver discovery independently of event responses. Tests
/// can supply a controlled transport without pretending to execute Gradle.
pub trait GetterTransport {
    fn discover(&mut self, projects: &[String]) -> Result<DiscoveryInventory>;
    fn invoke(&mut self, request: &GetterRequest) -> Result<(DiscoveryInventory, GetterEvent)>;
    fn finish(&mut self) -> Result<()>;
}

struct OwnedGradleChild(Child);

impl std::ops::Deref for OwnedGradleChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl std::ops::DerefMut for OwnedGradleChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}
impl Drop for OwnedGradleChild {
    fn drop(&mut self) {
        if let Err(error) = terminate(&mut self.0) {
            log::error!("Unable to reap owned Gradle getter child: {error:#}");
        }
    }
}

pub struct GradleTransport {
    child: OwnedGradleChild,
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    _directory: TempDir,
    logs_directory: PathBuf,
    diagnostic_bytes: u64,
    deadline: Instant,
    cancelled: std::sync::Arc<AtomicBool>,
    finished: bool,
}

#[derive(Clone, Debug)]
pub struct GradleCaptureOptions {
    pub wrapper: PathBuf,
    pub project_root: PathBuf,
    /// Runtime artifacts and fixture must be on this host, not a remote daemon.
    pub java_home: PathBuf,
    pub timeout: Duration,
    /// Fresh caller-owned directory; logs survive successful or failed capture.
    pub logs_directory: PathBuf,
    /// Exceeding this combined stdout/stderr budget rejects the entire capture.
    pub diagnostic_bytes: u64,
    pub cancelled: std::sync::Arc<AtomicBool>,
}

impl GradleTransport {
    pub fn start(options: &GradleCaptureOptions) -> Result<Self> {
        ensure!(
            !options.timeout.is_zero() && options.timeout.as_millis() <= i32::MAX as u128,
            "Capture timeout must fit the JVM socket timeout"
        );
        ensure!(
            options.wrapper.is_absolute()
                && options.project_root.is_absolute()
                && options.java_home.is_absolute(),
            "Owned Gradle paths must be absolute"
        );
        ensure!(
            options.diagnostic_bytes > 0 && options.logs_directory.is_absolute(),
            "Capture requires an absolute log directory and diagnostic budget"
        );
        fs::create_dir(&options.logs_directory)
            .context("Create fresh owned Gradle log directory")?;
        let directory = tempfile::Builder::new()
            .prefix("koda-kotlin-getters-")
            .tempdir()?;
        let script = directory.path().join("capture.gradle");
        fs::write(&script, BRIDGE)?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let token: String = (0..4)
            .map(|_| format!("{:016x}", rand::random::<u64>()))
            .collect();
        let port = listener.local_addr()?.port();
        let output = File::create(options.logs_directory.join("stdout.log"))?;
        let errors = File::create(options.logs_directory.join("stderr.log"))?;
        let mut child = OwnedGradleChild(
            Command::new(&options.wrapper)
                .current_dir(&options.project_root)
                .env("JAVA_HOME", &options.java_home)
                .args(["--no-daemon", "--console=plain", "--init-script"])
                .arg(&script)
                .arg(format!("-Dkoda.kotlin.capture.port={port}"))
                .arg(format!("-Dkoda.kotlin.capture.token={token}"))
                .arg(format!(
                    "-Dkoda.kotlin.capture.timeoutMillis={}",
                    options.timeout.as_millis()
                ))
                .arg("help")
                .stdin(Stdio::null())
                .stdout(Stdio::from(output))
                .stderr(Stdio::from(errors))
                .spawn()
                .context("Start owned Gradle getter runtime")?,
        );
        let deadline = Instant::now()
            .checked_add(options.timeout)
            .context("Capture deadline overflow")?;
        let stream = loop {
            if let Err(error) = check_deadline(deadline, &options.cancelled)
                .and_then(|()| check_diagnostics(&options.logs_directory, options.diagnostic_bytes))
            {
                terminate(&mut child)?;
                return Err(error);
            }
            match listener.accept() {
                Ok((stream, peer)) => {
                    ensure!(
                        peer.ip().is_loopback(),
                        "Getter transport accepted a nonlocal connection"
                    );
                    break stream;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if let Some(status) = child.try_wait()? {
                        bail!(
                            "Gradle getter runtime exited before discovery: {status}; logs in {}",
                            options.logs_directory.display()
                        );
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => {
                    terminate(&mut child)?;
                    return Err(error.into());
                }
            }
        };
        stream.set_read_timeout(Some(Duration::from_millis(100)))?;
        stream.set_write_timeout(Some(Duration::from_millis(100)))?;
        let writer = stream.try_clone()?;
        let mut transport = Self {
            child,
            reader: BufReader::new(stream),
            writer,
            _directory: directory,
            logs_directory: options.logs_directory.clone(),
            diagnostic_bytes: options.diagnostic_bytes,
            deadline,
            cancelled: options.cancelled.clone(),
            finished: false,
        };
        match transport.receive()? {
            Response::Hello(actual) if actual == token => Ok(transport),
            _ => bail!("Getter runtime did not authenticate the owned session"),
        }
    }

    pub fn logs_directory(&self) -> &Path {
        &self.logs_directory
    }

    fn send(&mut self, request: &Request<'_>) -> Result<()> {
        check_deadline(self.deadline, &self.cancelled)?;
        check_diagnostics(&self.logs_directory, self.diagnostic_bytes)?;
        let mut bytes = serde_json::to_vec(request)?;
        ensure!(
            bytes.len() <= MAX_FRAME_BYTES,
            "Getter request exceeds frame budget"
        );
        bytes.push(b'\n');
        self.writer.write_all(&bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    fn receive(&mut self) -> Result<Response> {
        let logs_directory = &self.logs_directory;
        let diagnostic_bytes = self.diagnostic_bytes;
        let bytes = read_frame_monitored(
            &mut self.reader,
            self.deadline,
            &self.cancelled,
            MAX_FRAME_BYTES,
            || check_diagnostics(logs_directory, diagnostic_bytes),
        )?;
        let response: Response =
            serde_json::from_slice(&bytes).context("Decode owned getter response frame")?;
        if let Response::Failure(detail) = response {
            bail!("Gradle getter discovery unavailable: {detail}");
        }
        Ok(response)
    }
}

impl GetterTransport for GradleTransport {
    fn discover(&mut self, projects: &[String]) -> Result<DiscoveryInventory> {
        self.send(&Request::Discover(DiscoveryRequest {
            projects,
            class_names: &[WRAPPER, RESOLVER],
        }))?;
        match self.receive()? {
            Response::Discovery(inventory) => Ok(inventory),
            _ => bail!("Expected separate discovery frame"),
        }
    }
    fn invoke(&mut self, request: &GetterRequest) -> Result<(DiscoveryInventory, GetterEvent)> {
        self.send(&Request::Invoke(request))?;
        let inventory = match self.receive()? {
            Response::Discovery(inventory) => inventory,
            _ => bail!("Getter event arrived without discovery metadata"),
        };
        let event = match self.receive()? {
            Response::Event(event) => event,
            _ => bail!("Expected exact getter event"),
        };
        Ok((inventory, event))
    }
    fn finish(&mut self) -> Result<()> {
        self.send(&Request::Finish(()))?;
        ensure!(
            matches!(self.receive()?, Response::Finished(())),
            "Runtime did not acknowledge capture completion"
        );
        loop {
            check_deadline(self.deadline, &self.cancelled)?;
            check_diagnostics(&self.logs_directory, self.diagnostic_bytes)?;
            if let Some(status) = self.child.try_wait()? {
                ensure!(
                    status.success(),
                    "Gradle runtime failed after getter capture: {status}"
                );
                self.finished = true;
                return Ok(());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for GradleTransport {
    fn drop(&mut self) {
        if !self.finished {
            if let Err(error) = self.writer.shutdown(std::net::Shutdown::Both) {
                log::error!("Unable to close owned Kotlin getter socket: {error:#}");
            }
            if let Err(error) = terminate(&mut self.child) {
                log::error!("Unable to close owned Kotlin getter runtime: {error:#}");
            }
        }
    }
}

fn terminate(child: &mut Child) -> Result<()> {
    if child.try_wait()?.is_none() {
        child.kill()?;
        child.wait()?;
    }
    Ok(())
}

fn check_deadline(deadline: Instant, cancelled: &AtomicBool) -> Result<()> {
    ensure!(
        !cancelled.load(Ordering::Acquire),
        "Kotlin getter capture was cancelled"
    );
    ensure!(
        Instant::now() < deadline,
        "Kotlin getter capture deadline elapsed"
    );
    Ok(())
}

fn read_frame_monitored(
    reader: &mut impl BufRead,
    deadline: Instant,
    cancelled: &AtomicBool,
    maximum: usize,
    mut monitor: impl FnMut() -> Result<()>,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        check_deadline(deadline, cancelled)?;
        monitor()?;
        let buffer = match reader.fill_buf() {
            Ok(buffer) => buffer,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        ensure!(
            !buffer.is_empty(),
            "Getter runtime closed an incomplete frame"
        );
        let count = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |offset| offset + 1);
        let complete = buffer.get(count - 1) == Some(&b'\n');
        ensure!(
            bytes.len().checked_add(count).is_some_and(|length| maximum
                .checked_add(1)
                .is_some_and(|maximum| length <= maximum)),
            "Getter response exceeds frame budget"
        );
        bytes.extend_from_slice(&buffer[..count]);
        reader.consume(count);
        if complete {
            bytes.pop();
            return Ok(bytes);
        }
    }
}

fn check_diagnostics(directory: &Path, maximum: u64) -> Result<()> {
    let stdout = fs::metadata(directory.join("stdout.log"))?.len();
    let stderr = fs::metadata(directory.join("stderr.log"))?.len();
    ensure!(
        stdout
            .checked_add(stderr)
            .is_some_and(|bytes| bytes <= maximum),
        "Gradle diagnostic output exceeded capture budget"
    );
    Ok(())
}

#[cfg(test)]
fn read_frame(
    reader: &mut impl BufRead,
    deadline: Instant,
    cancelled: &AtomicBool,
    maximum: usize,
) -> Result<Vec<u8>> {
    read_frame_monitored(reader, deadline, cancelled, maximum, || Ok(()))
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfficialProjectRequests {
    pub project: String,
    pub plugin_lookups: BTreeMap<String, String>,
    pub plugin_iteration: Option<String>,
    pub extension_lookup: Option<String>,
    pub compiler_version: Option<String>,
    pub task_iteration: Option<String>,
    pub source_set_names: BTreeMap<String, String>,
    pub compiler_arguments: BTreeMap<String, String>,
    pub unavailable: Vec<String>,
}

pub struct KotlinGetterCapture<T: GetterTransport, H: Clone + PartialEq> {
    issued_host: H,
    transport: T,
    binding: CaptureBinding,
    fixture: FixtureBoundary,
    imports: ImportIdentity,
    inventory: DiscoveryInventory,
    requests: Vec<GetterRequest>,
    events: Vec<GetterEvent>,
    event_ids: BTreeSet<String>,
}

pub struct CapturedKotlinGetters<H> {
    pub issued_host: H,
    pub snapshot: KotlinFactsSnapshot,
    pub expected: CaptureContext,
    pub projects: Vec<OfficialProjectRequests>,
    pub fixture: FixtureBoundary,
}

impl<T: GetterTransport, H: Clone + PartialEq> KotlinGetterCapture<T, H> {
    /// `issued_host` is the original host ImportRevision, including its context
    /// generation/import revision. The current-host callback must read the host;
    /// returning a freshly made matching token defeats the publication boundary.
    /// The adapter consumes this same original token after strict validation.
    pub fn discover(
        mut transport: T,
        issued_host: H,
        mut current_host: impl FnMut() -> Result<H>,
        binding: CaptureBinding,
        fixture: FixtureBoundary,
        model: &ProjectModel,
        imports: &ImportFactsSnapshot,
    ) -> Result<Self> {
        ensure!(
            current_host()? == issued_host,
            "Host import revision changed before discovery"
        );
        fixture.ensure_unchanged()?;
        ensure!(
            model.root.canonicalize()? == fixture.root,
            "Fixture boundary belongs to a different project root"
        );
        ensure!(
            binding.fixture_before_sha256 == fixture.sha256
                && binding.fixture_after_sha256 == fixture.sha256,
            "Host capture binding does not match fixture boundary"
        );
        ensure!(
            binding.model_revision == imports.binding().model_revision
                && binding.selection_revision == imports.binding().selection_revision,
            "Host capture revision does not match import identity"
        );
        ensure!(
            binding
                .selected_variants
                .iter()
                .map(|variant| (&variant.module, &variant.variant))
                .eq(imports
                    .binding()
                    .selected_variants
                    .iter()
                    .map(|variant| (&variant.module, &variant.variant))),
            "Host capture selection differs from import identity"
        );
        imports
            .ensure_current(model, imports.binding())
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let projects = imports
            .projects()
            .map_err(|error| anyhow::anyhow!("{error:?}"))?
            .iter()
            .map(|project| {
                project
                    .project_path
                    .as_ref()
                    .context("Imported project path was not captured")?
                    .available()
                    .cloned()
                    .map_err(|error| anyhow::anyhow!("{error:?}"))
            })
            .collect::<Result<Vec<_>>>()?;
        let inventory = transport.discover(&projects)?;
        ensure!(
            inventory.runtime.gradle_version == imports.gradle_version(),
            "Runtime Gradle version differs from imported identity"
        );
        ensure!(
            inventory.project_objects.keys().collect::<BTreeSet<_>>()
                == projects.iter().collect::<BTreeSet<_>>(),
            "Discovery omitted or invented imported projects"
        );
        inventory.verify_artifacts()?;
        ensure!(
            current_host()? == issued_host,
            "Host import revision changed during discovery"
        );
        Ok(Self {
            issued_host,
            transport,
            binding,
            fixture,
            imports: ImportIdentity {
                build_identity: imports.build_identity().clone(),
                project_catalogue: imports.catalogue_observation().clone(),
            },
            inventory,
            requests: Vec::new(),
            events: Vec::new(),
            event_ids: BTreeSet::new(),
        })
    }

    pub fn inventory(&self) -> &DiscoveryInventory {
        &self.inventory
    }

    fn issue(
        &mut self,
        project: &str,
        owner: Option<&str>,
        catalogue: &str,
        name: &str,
        descriptor: &str,
        shape: ReturnShape,
        arguments: Vec<GetterArgument>,
        purpose: GetterPurpose,
        after: Option<&str>,
    ) -> Result<GetterEvent> {
        let catalogue = self
            .inventory
            .catalogues
            .iter()
            .find(|value| value.id == catalogue)
            .context("Requested catalogue has not been discovered")?;
        let method = select_exact_method(catalogue, name, descriptor, owner.is_none())?;
        let request = GetterRequest {
            id: format!(
                "{}:request:{}",
                self.binding.capture_id,
                self.requests.len()
            ),
            project: project.into(),
            consumer: Consumer::Kotlin,
            model_call: format!("{}:{project}:kotlin", self.binding.capture_id),
            parameter: RequestParameter::Absent(()),
            variant: None,
            owner: owner.map(str::to_owned),
            catalogue: catalogue.id.clone(),
            method,
            arguments,
            return_shape: shape,
            purpose,
            after: after.map(str::to_owned),
        };
        // Retain the request before handing it to any transport. Neither a result
        // nor a catalogue update can replace this issued request or its order.
        self.requests.push(request.clone());
        let (discovery, event) = self.transport.invoke(&request)?;
        self.inventory.extend(discovery)?;
        ensure!(
            event.request == request.id && self.event_ids.insert(event.id.clone()),
            "Getter event identity/order differs from issued request"
        );
        self.events.push(event.clone());
        Ok(event)
    }

    fn instance(
        &mut self,
        project: &str,
        owner: &str,
        name: &str,
        descriptor: &str,
        shape: ReturnShape,
        arguments: Vec<GetterArgument>,
        purpose: GetterPurpose,
        after: Option<&str>,
    ) -> Result<GetterEvent> {
        let catalogue = self.inventory.catalogue(owner)?.id.clone();
        self.issue(
            project,
            Some(owner),
            &catalogue,
            name,
            descriptor,
            shape,
            arguments,
            purpose,
            after,
        )
    }

    pub fn capture_official_project(&mut self, project: &str) -> Result<OfficialProjectRequests> {
        let project_object = self
            .inventory
            .project_objects
            .get(project)
            .cloned()
            .context("Project object is missing from discovery")?;
        let mut plan = OfficialProjectRequests {
            project: project.into(),
            ..Default::default()
        };
        let plugins = self.instance(
            project,
            &project_object,
            "getPlugins",
            "()Lorg/gradle/api/plugins/PluginContainer;",
            object_shape(ObjectKind::Container, false),
            vec![],
            GetterPurpose::Raw,
            None,
        )?;
        let iteration = self.instance(
            project,
            &project_object,
            "getPlugins",
            "()Lorg/gradle/api/plugins/PluginContainer;",
            objects_shape(ObjectKind::Plugin, ContainerOrder::Iterable, false),
            vec![],
            GetterPurpose::Raw,
            None,
        )?;
        plan.plugin_iteration = Some(iteration.request.clone());
        if let Some(container) = object_result(&plugins) {
            for plugin in KOTLIN_PLUGIN_IDS.iter().chain(ANDROID_PLUGIN_IDS) {
                let event = self.instance(
                    project,
                    &container,
                    "findPlugin",
                    "(Ljava/lang/String;)Lorg/gradle/api/Plugin;",
                    object_shape(ObjectKind::Plugin, true),
                    vec![GetterArgument::String((*plugin).into())],
                    GetterPurpose::Raw,
                    Some(&plugins.request),
                )?;
                plan.plugin_lookups.insert((*plugin).into(), event.request);
            }
        } else {
            plan.unavailable
                .push("Project.getPlugins() unavailable".into());
        }
        let extensions = self.instance(
            project,
            &project_object,
            "getExtensions",
            "()Lorg/gradle/api/plugins/ExtensionContainer;",
            object_shape(ObjectKind::Container, false),
            vec![],
            GetterPurpose::Raw,
            None,
        )?;
        if let Some(container) = object_result(&extensions) {
            let event = self.instance(
                project,
                &container,
                "findByName",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                object_shape(ObjectKind::Extension, true),
                vec![GetterArgument::String("kotlin".into())],
                GetterPurpose::Raw,
                Some(&extensions.request),
            )?;
            plan.extension_lookup = Some(event.request);
        } else {
            plan.unavailable
                .push("Project.getExtensions() unavailable".into());
        }
        if let Some(catalogue) = self
            .inventory
            .named_class_catalogue(project, WRAPPER)?
            .map(|catalogue| catalogue.id.clone())
        {
            let event = self.issue(
                project,
                None,
                &catalogue,
                "getKotlinPluginVersion",
                "(Lorg/gradle/api/Project;)Ljava/lang/String;",
                scalar_shape(ValueKind::String, true),
                vec![GetterArgument::Object(project_object.clone())],
                GetterPurpose::Raw,
                None,
            )?;
            plan.compiler_version = Some(event.request);
        } else {
            plan.unavailable
                .push(format!("Class lookup unavailable: {WRAPPER}"));
        }
        let task_map = self.instance(
            project,
            &project_object,
            "getAllTasks",
            "(Z)Ljava/util/Map;",
            object_shape(ObjectKind::Container, false),
            vec![GetterArgument::Boolean(false)],
            GetterPurpose::Raw,
            None,
        )?;
        let Some(task_map_object) = object_result(&task_map) else {
            plan.unavailable
                .push("Project.getAllTasks(false) unavailable".into());
            return Ok(plan);
        };
        let tasks = self.instance(
            project,
            &task_map_object,
            "get",
            "(Ljava/lang/Object;)Ljava/lang/Object;",
            objects_shape(ObjectKind::Task, ContainerOrder::ProjectTaskMapValues, true),
            vec![GetterArgument::Object(project_object.clone())],
            GetterPurpose::ContainerIterate,
            Some(&task_map.request),
        )?;
        plan.task_iteration = Some(tasks.request.clone());
        let task_ids = match &tasks.outcome {
            GetterOutcome::Available(Some(CaptureValue::Objects(ids))) => ids.clone(),
            _ => {
                plan.unavailable
                    .push("Task-map project values unavailable".into());
                return Ok(plan);
            }
        };
        let resolver = if let Some(catalogue) = self
            .inventory
            .named_class_catalogue(project, RESOLVER)?
            .map(|catalogue| catalogue.id.clone())
        {
            // The return descriptor comes from the exact reflected method, never
            // from a guessed plugin version or a synthetic resolver class.
            let method = self
                .inventory
                .catalogues
                .iter()
                .find(|value| value.id == catalogue)
                .and_then(|value| {
                    value.methods.iter().find(|method| {
                        method.name == "instance"
                            && method.is_static
                            && method.descriptor.starts_with("(Lorg/gradle/api/Project;)")
                    })
                })
                .cloned();
            if let Some(method) = method {
                Some(self.issue(
                    project,
                    None,
                    &catalogue,
                    &method.name,
                    &method.descriptor,
                    object_shape(ObjectKind::Resolver, true),
                    vec![GetterArgument::Object(project_object.clone())],
                    GetterPurpose::ResolverInstance,
                    None,
                )?)
            } else {
                plan.unavailable
                    .push("Compiler resolver instance method unavailable".into());
                None
            }
        } else {
            plan.unavailable
                .push(format!("Class lookup unavailable: {RESOLVER}"));
            None
        };
        for task in task_ids {
            let object = self.inventory.object(&task)?;
            let class = self
                .inventory
                .runtime
                .classes
                .iter()
                .find(|class| class.id == object.class_id)
                .context("Task runtime class is undiscovered")?;
            if !KOTLIN_TASK_CLASSES.contains(&class.name.as_str()) {
                continue;
            }
            let catalogue = self.inventory.catalogue(&task)?;
            let source_method = catalogue
                .methods
                .iter()
                .find(|method| {
                    !method.is_static
                        && method.name.starts_with("getSourceSetName")
                        && method.descriptor.starts_with("()")
                })
                .cloned();
            if let Some(method) = source_method {
                let property = method
                    .return_class
                    .as_deref()
                    .is_some_and(|class| self.is_class(class, "org.gradle.api.provider.Property"));
                let shape = if property {
                    object_shape(ObjectKind::Property, true)
                } else {
                    scalar_shape(ValueKind::String, true)
                };
                let source = self.instance(
                    project,
                    &task,
                    &method.name,
                    &method.descriptor,
                    shape,
                    vec![],
                    GetterPurpose::SourceSet,
                    None,
                )?;
                if property {
                    if let Some(owner) = object_result(&source) {
                        let terminal = self.instance(
                            project,
                            &owner,
                            "get",
                            "()Ljava/lang/Object;",
                            scalar_shape(ValueKind::String, true),
                            vec![],
                            GetterPurpose::PropertyGet,
                            Some(&source.request),
                        )?;
                        plan.source_set_names.insert(task.clone(), terminal.request);
                    } else {
                        plan.source_set_names.insert(task.clone(), source.request);
                    }
                } else {
                    plan.source_set_names.insert(task.clone(), source.request);
                }
            } else {
                plan.unavailable
                    .push(format!("Source-set getter unavailable for {task}"));
            }
            if let Some(resolver_event) = &resolver
                && let Some(owner) = object_result(resolver_event)
            {
                let event = self.instance(
                    project,
                    &owner,
                    "resolveCompilerArguments",
                    "(Ljava/lang/Object;)Ljava/util/List;",
                    strings_shape(true),
                    vec![GetterArgument::Object(task.clone())],
                    GetterPurpose::CompilerArguments,
                    Some(&resolver_event.request),
                )?;
                plan.compiler_arguments.insert(task, event.request);
            }
        }
        Ok(plan)
    }

    fn is_class(&self, class: &str, name: &str) -> bool {
        let mut pending = vec![class];
        let mut seen = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let Some(class) = self
                .inventory
                .runtime
                .classes
                .iter()
                .find(|class| class.id == id)
            {
                if class.name == name {
                    return true;
                }
                pending.extend(class.superclass.iter().map(String::as_str));
                pending.extend(class.interfaces.iter().map(String::as_str));
            }
        }
        false
    }

    /// Compare the host's exact current ImportRevision with the originally
    /// retained token, including A→B→A context generation and import revision.
    pub fn finish(
        mut self,
        model: &ProjectModel,
        imports: &ImportFactsSnapshot,
        projects: Vec<OfficialProjectRequests>,
        mut current_host: impl FnMut() -> Result<H>,
    ) -> Result<CapturedKotlinGetters<H>> {
        ensure!(
            current_host()? == self.issued_host,
            "Host import revision changed during getter capture"
        );
        self.fixture.ensure_unchanged()?;
        self.inventory.verify_artifacts()?;
        self.transport.finish()?;
        ensure!(
            current_host()? == self.issued_host,
            "Host import revision changed during getter capture"
        );
        self.fixture.ensure_unchanged()?;
        self.inventory.verify_artifacts()?;
        let expected = CaptureContext {
            binding: self.binding,
            imports: self.imports,
            mode: CaptureMode::Invocation,
            runtime: self.inventory.runtime,
            objects: self.inventory.objects,
            catalogues: self.inventory.catalogues,
            requests: self.requests,
        };
        let modules = model.modules.iter().map(|module| serde_json::json!({"module": module.path, "directory": module.directory, "kind": module.kind, "variants": module.variants.iter().map(|variant| &variant.name).collect::<Vec<_>>() })).collect::<Vec<_>>();
        let packet = serde_json::json!({"kotlinFacts": {"schema":1, "root":model.root, "modules":modules, "context":expected, "events":self.events}});
        let output = format!(
            "KODA_ANDROID_PROJECT_MODEL={}",
            serde_json::to_string(&packet)?
        );
        let snapshot =
            parse_kotlin_facts(&output, model, imports, &expected, CaptureLimits::default())
                .map_err(|error| anyhow::anyhow!("Strict Kotlin capture rejected: {error:?}"))?;
        Ok(CapturedKotlinGetters {
            issued_host: self.issued_host,
            snapshot,
            expected,
            projects,
            fixture: self.fixture,
        })
    }
}

pub fn select_exact_method(
    catalogue: &MethodCatalogue,
    name: &str,
    descriptor: &str,
    is_static: bool,
) -> Result<MethodSelection> {
    let methods = catalogue
        .methods
        .iter()
        .filter(|method| {
            method.name == name && method.descriptor == descriptor && method.is_static == is_static
        })
        .collect::<Vec<_>>();
    ensure!(
        methods.len() <= 1,
        "Ambiguous reflected getter; select a declaring method explicitly"
    );
    Ok(match methods.first() {
        Some(method) => MethodSelection::Selected(method.id.clone()),
        None => MethodSelection::Missing(MissingMethod {
            name: name.into(),
            descriptor: descriptor.into(),
            is_static,
        }),
    })
}

fn scalar_shape(kind: ValueKind, nullable: bool) -> ReturnShape {
    ReturnShape {
        kind,
        nullable,
        object_kind: None,
        order: None,
    }
}
fn object_shape(kind: ObjectKind, nullable: bool) -> ReturnShape {
    ReturnShape {
        kind: ValueKind::Object,
        nullable,
        object_kind: Some(kind),
        order: None,
    }
}
fn objects_shape(kind: ObjectKind, order: ContainerOrder, nullable: bool) -> ReturnShape {
    ReturnShape {
        kind: ValueKind::Objects,
        nullable,
        object_kind: Some(kind),
        order: Some(order),
    }
}
fn strings_shape(nullable: bool) -> ReturnShape {
    ReturnShape {
        kind: ValueKind::Strings,
        nullable,
        object_kind: None,
        order: Some(ContainerOrder::List),
    }
}
fn object_result(event: &GetterEvent) -> Option<String> {
    match &event.outcome {
        GetterOutcome::Available(Some(CaptureValue::Object(id))) => Some(id.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kotlin_import_facts::{GetterFailure, GetterFailureKind, GetterFailureStage};
    use std::io::Cursor;

    fn binding(digest: &str) -> CaptureBinding {
        CaptureBinding {
            capture_id: "synthetic-host-session".into(),
            source_epoch: "synthetic-host-epoch".into(),
            fixture_before_sha256: digest.into(),
            fixture_after_sha256: digest.into(),
            model_revision: 7,
            selection_revision: 3,
            selected_variants: vec![],
        }
    }

    fn inventory() -> Result<(TempDir, DiscoveryInventory, ImportIdentity)> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        let template = include_str!("../test_data/kotlin_import_facts/protocol-template.json")
            .replace(
                "$ROOT",
                root.to_str().context("Synthetic fixture path is UTF-8")?,
            );
        let packet: serde_json::Value = serde_json::from_str(&template)?;
        let mut context: CaptureContext = serde_json::from_value(packet["context"].clone())?;
        for artifact in &mut context.runtime.artifacts {
            fs::create_dir_all(
                artifact
                    .path
                    .parent()
                    .context("Synthetic artifact parent")?,
            )?;
            fs::write(
                &artifact.path,
                format!("synthetic artifact {}", artifact.id),
            )?;
            let (bytes, digest) = hash_regular_file(&artifact.path)?;
            artifact.bytes = bytes;
            artifact.sha256 = digest;
        }
        let project_objects = context
            .objects
            .iter()
            .filter(|object| object.kind == ObjectKind::Project)
            .map(|object| (object.project.clone(), object.id.clone()))
            .collect();
        Ok((
            directory,
            DiscoveryInventory {
                runtime: context.runtime,
                objects: context.objects,
                catalogues: context.catalogues,
                project_objects,
                class_lookups: vec![],
            },
            context.imports,
        ))
    }

    #[test]
    fn fixture_boundary_is_stable_for_inventory_order_and_detects_mutation() -> Result<()> {
        let directory = tempfile::tempdir()?;
        fs::write(
            directory.path().join("settings.gradle"),
            "rootProject.name='original'",
        )?;
        fs::write(directory.path().join("build.gradle"), "plugins {}")?;
        let first = FixtureBoundary::capture(
            directory.path(),
            &["settings.gradle".into(), "build.gradle".into()],
        )?;
        let reverse = FixtureBoundary::capture(
            directory.path(),
            &["build.gradle".into(), "settings.gradle".into()],
        )?;
        assert_eq!(first, reverse);
        first.ensure_unchanged()?;
        // Generated outputs do not change the explicit original fixture boundary.
        fs::write(directory.path().join("generated.log"), "output")?;
        first.ensure_unchanged()?;
        fs::write(
            directory.path().join("settings.gradle"),
            "rootProject.name='changed!'",
        )?;
        assert!(first.ensure_unchanged().is_err());
        fs::remove_file(directory.path().join("settings.gradle"))?;
        assert!(first.ensure_unchanged().is_err());
        Ok(())
    }

    #[test]
    fn fixture_boundary_rejects_empty_duplicate_and_escaping_paths() -> Result<()> {
        let directory = tempfile::tempdir()?;
        fs::write(directory.path().join("build.gradle"), "plugins {}")?;
        assert!(FixtureBoundary::capture(directory.path(), &[]).is_err());
        assert!(
            FixtureBoundary::capture(
                directory.path(),
                &["build.gradle".into(), "build.gradle".into()]
            )
            .is_err()
        );
        for path in [
            PathBuf::new(),
            PathBuf::from("../build.gradle"),
            PathBuf::from("/outside.gradle"),
        ] {
            assert!(FixtureBoundary::capture(directory.path(), &[path]).is_err());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn fixture_boundary_rejects_symlink_files_and_directory_ancestors() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        fs::write(outside.path().join("build.gradle"), "plugins {}")?;
        std::os::unix::fs::symlink(
            outside.path().join("build.gradle"),
            directory.path().join("file.gradle"),
        )?;
        std::os::unix::fs::symlink(outside.path(), directory.path().join("linked"))?;
        assert!(FixtureBoundary::capture(directory.path(), &["file.gradle".into()]).is_err());
        assert!(
            FixtureBoundary::capture(directory.path(), &["linked/build.gradle".into()]).is_err()
        );
        Ok(())
    }

    #[test]
    fn framed_transport_preserves_following_frame_and_rejects_limits_eof_and_cancel() -> Result<()>
    {
        let deadline = Instant::now() + Duration::from_secs(10);
        let cancelled = AtomicBool::new(false);
        let mut reader = BufReader::with_capacity(2, Cursor::new(b"first\nsecond\n"));
        assert_eq!(read_frame(&mut reader, deadline, &cancelled, 5)?, b"first");
        assert_eq!(read_frame(&mut reader, deadline, &cancelled, 6)?, b"second");
        assert!(read_frame(&mut Cursor::new(b"sixsix\n"), deadline, &cancelled, 5).is_err());
        assert!(read_frame(&mut Cursor::new(b"partial"), deadline, &cancelled, 20).is_err());
        cancelled.store(true, Ordering::Release);
        assert!(read_frame(&mut Cursor::new(b"ready\n"), deadline, &cancelled, 20).is_err());
        assert!(
            read_frame(
                &mut Cursor::new(b"ready\n"),
                Instant::now(),
                &AtomicBool::new(false),
                20
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn exact_method_plan_distinguishes_overloads_static_missing_and_ambiguity() -> Result<()> {
        let (_directory, inventory, _) = inventory()?;
        let mut catalogue = inventory
            .catalogues
            .first()
            .context("Synthetic catalogue")?
            .clone();
        let mut method = catalogue
            .methods
            .first()
            .context("Synthetic reflected method")?
            .clone();
        method.name = "getSourceSetName".into();
        method.descriptor = "()Ljava/lang/String;".into();
        method.is_static = false;
        catalogue.methods = vec![method.clone()];
        assert_eq!(
            select_exact_method(&catalogue, &method.name, &method.descriptor, false)?,
            MethodSelection::Selected(method.id.clone())
        );
        for (descriptor, static_call) in [
            ("(Ljava/lang/String;)Ljava/lang/String;", false),
            ("()Ljava/lang/String;", true),
        ] {
            assert!(matches!(
                select_exact_method(&catalogue, &method.name, descriptor, static_call)?,
                MethodSelection::Missing(_)
            ));
        }
        let mut duplicate = method.clone();
        duplicate.id = "different-declaring-method".into();
        catalogue.methods.push(duplicate);
        assert!(select_exact_method(&catalogue, &method.name, &method.descriptor, false).is_err());
        Ok(())
    }

    #[test]
    fn discovery_cannot_rewrite_prior_method_class_loader_or_locale() -> Result<()> {
        let (_directory, inventory, _) = inventory()?;
        let mut changed = inventory.clone();
        changed
            .catalogues
            .first_mut()
            .context("Catalogue")?
            .methods
            .clear();
        assert!(inventory.clone().extend(changed).is_err());
        let mut changed = inventory.clone();
        changed.runtime.classes.first_mut().context("Class")?.name = "counterfeit.Class".into();
        assert!(inventory.clone().extend(changed).is_err());
        let mut changed = inventory.clone();
        changed
            .runtime
            .loaders
            .first_mut()
            .context("Loader")?
            .parent = Some("counterfeit-loader".into());
        assert!(inventory.clone().extend(changed).is_err());
        let mut changed = inventory.clone();
        changed.runtime.locale.identifier = "en-US".into();
        assert!(inventory.clone().extend(changed).is_err());
        let mut changed = inventory.clone();
        changed.objects.clear();
        assert!(inventory.clone().extend(changed).is_err());
        let mut changed = inventory.clone();
        changed.runtime.artifacts.push(
            changed
                .runtime
                .artifacts
                .first()
                .context("Artifact")?
                .clone(),
        );
        assert!(inventory.clone().extend(changed).is_err());
        Ok(())
    }

    #[test]
    fn artifact_verification_uses_host_bytes_and_rejects_mutation() -> Result<()> {
        let (_directory, inventory, _) = inventory()?;
        inventory.verify_artifacts()?;
        let artifact = inventory.runtime.artifacts.first().context("Artifact")?;
        fs::write(&artifact.path, "counterfeit runtime")?;
        assert!(inventory.verify_artifacts().is_err());
        let mut self_authorized = inventory.clone();
        let (_, digest) = hash_regular_file(&artifact.path)?;
        self_authorized
            .runtime
            .artifacts
            .first_mut()
            .context("Artifact")?
            .sha256 = digest;
        // A follow-up discovery frame cannot bless changed bytes using its own digest.
        assert!(inventory.clone().extend(self_authorized).is_err());
        Ok(())
    }

    struct ScriptedTransport {
        inventory: DiscoveryInventory,
        wrong_request: bool,
        catalogue_changed: bool,
        observed: Vec<GetterRequest>,
    }
    impl GetterTransport for ScriptedTransport {
        fn discover(&mut self, _: &[String]) -> Result<DiscoveryInventory> {
            Ok(self.inventory.clone())
        }
        fn invoke(&mut self, request: &GetterRequest) -> Result<(DiscoveryInventory, GetterEvent)> {
            self.observed.push(request.clone());
            let mut inventory = self.inventory.clone();
            if self.catalogue_changed {
                inventory.catalogues.clear();
            }
            Ok((
                inventory,
                GetterEvent {
                    id: format!("event:{}", self.observed.len()),
                    request: if self.wrong_request {
                        "counterfeit-request".into()
                    } else {
                        request.id.clone()
                    },
                    outcome: GetterOutcome::Unavailable(GetterFailure {
                        kind: GetterFailureKind::MissingMethod,
                        stage: GetterFailureStage::MethodDiscovery,
                        capability: request.id.clone(),
                        detail: "Synthetic missing capability".into(),
                        actual_class: None,
                        exceptions: vec![],
                    }),
                    container: None,
                },
            ))
        }
        fn finish(&mut self) -> Result<()> {
            Ok(())
        }
    }

    fn scripted_capture(
        wrong_request: bool,
        catalogue_changed: bool,
    ) -> Result<(TempDir, KotlinGetterCapture<ScriptedTransport, String>)> {
        let (directory, inventory, imports) = inventory()?;
        fs::write(directory.path().join("build.gradle"), "synthetic")?;
        let fixture = FixtureBoundary::capture(directory.path(), &["build.gradle".into()])?;
        let capture = KotlinGetterCapture {
            issued_host: "synthetic-exact-host-revision".to_string(),
            transport: ScriptedTransport {
                inventory: inventory.clone(),
                wrong_request,
                catalogue_changed,
                observed: vec![],
            },
            binding: binding(&fixture.sha256),
            fixture,
            imports,
            inventory,
            requests: vec![],
            events: vec![],
            event_ids: BTreeSet::new(),
        };
        Ok((directory, capture))
    }

    #[test]
    fn immutable_issued_request_survives_bad_event_without_becoming_successful() -> Result<()> {
        for (wrong_request, catalogue_changed) in [(true, false), (false, true)] {
            let (_directory, mut capture) = scripted_capture(wrong_request, catalogue_changed)?;
            let catalogue = capture
                .inventory
                .catalogues
                .first()
                .context("Catalogue")?
                .id
                .clone();
            assert!(
                capture
                    .issue(
                        ":android",
                        None,
                        &catalogue,
                        "absentOfficialMethod",
                        "()Ljava/lang/String;",
                        scalar_shape(ValueKind::String, true),
                        vec![],
                        GetterPurpose::Raw,
                        None
                    )
                    .is_err()
            );
            assert_eq!(capture.requests.len(), 1);
            assert!(capture.events.is_empty());
            assert_eq!(capture.requests, capture.transport.observed);
            assert!(matches!(
                capture.requests.first().context("Issued request")?.method,
                MethodSelection::Missing(_)
            ));
        }
        Ok(())
    }

    #[test]
    fn missing_capability_is_retained_as_an_event_and_not_an_empty_value() -> Result<()> {
        let (_directory, mut capture) = scripted_capture(false, false)?;
        let catalogue = capture
            .inventory
            .catalogues
            .first()
            .context("Catalogue")?
            .id
            .clone();
        let event = capture.issue(
            ":android",
            None,
            &catalogue,
            "absentOfficialMethod",
            "()Ljava/lang/String;",
            scalar_shape(ValueKind::String, true),
            vec![],
            GetterPurpose::Raw,
            None,
        )?;
        assert!(matches!(event.outcome, GetterOutcome::Unavailable(_)));
        assert_eq!(capture.requests, capture.transport.observed);
        assert_eq!(capture.events.len(), 1);
        assert_eq!(
            capture.events.first().context("Getter event")?.request,
            capture.requests.first().context("Issued request")?.id
        );
        Ok(())
    }
    #[test]
    fn returning_to_a_with_a_new_host_generation_cannot_finish_old_capture() -> Result<()> {
        let (directory, capture) = scripted_capture(false, false)?;
        let root = directory.path().canonicalize()?;
        for name in ["android", "strange-parent", "shared-directory"] {
            fs::create_dir(root.join(name))?;
        }
        let wire = include_str!("../test_data/import_facts/wire-template.json")
            .replace("$ROOT", root.to_str().context("Synthetic root")?);
        let output = format!("KODA_ANDROID_PROJECT_MODEL={wire}");
        let model = crate::project_model::parse_model(&output, &root)?;
        let imports = crate::import_facts::parse_import_facts(
            &output,
            &model,
            crate::import_facts::ImportFactsBinding {
                model_revision: 7,
                selection_revision: 3,
                selected_variants: vec![
                    crate::project_model::VariantId {
                        module: ":android".into(),
                        variant: "debug".into(),
                    },
                    crate::project_model::VariantId {
                        module: ":nested:library".into(),
                        variant: "jvm".into(),
                    },
                ],
            },
        )?;
        // Same A model/selection can return after B. A new host generation must
        // reject the old capture before any strict packet or publication exists.
        let error = capture
            .finish(&model, &imports, vec![], || {
                Ok("fresh-A-host-generation".into())
            })
            .err()
            .context("Old capture must not finish")?;
        assert!(error.to_string().contains("Host import revision changed"));
        Ok(())
    }
    #[test]
    fn diagnostic_budget_rejects_capture_instead_of_discarding_output() -> Result<()> {
        let directory = tempfile::tempdir()?;
        fs::write(directory.path().join("stdout.log"), b"12345")?;
        fs::write(directory.path().join("stderr.log"), b"67890")?;
        check_diagnostics(directory.path(), 10)?;
        assert!(check_diagnostics(directory.path(), 9).is_err());
        let error = read_frame_monitored(
            &mut Cursor::new(b"ready\n"),
            Instant::now() + Duration::from_secs(10),
            &AtomicBool::new(false),
            20,
            || check_diagnostics(directory.path(), 9),
        )
        .err()
        .context("Oversized runtime logs must stop capture")?;
        assert!(error.to_string().contains("diagnostic output exceeded"));
        assert_eq!(fs::read(directory.path().join("stdout.log"))?, b"12345");
        assert_eq!(fs::read(directory.path().join("stderr.log"))?, b"67890");
        Ok(())
    }
}
