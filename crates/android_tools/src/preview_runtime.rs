//! Release-pinned preview libraries, installed independently of the application binary.
use anyhow::{Context as _, Result, ensure};
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read as _, Write as _},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const MANIFEST: &[u8] = include_bytes!("../preview-manifest.json");
const BRIDGE: &[u8] = include_bytes!("PreviewBridge.java");
const MAX_DOWNLOAD: u64 = 1024 * 1024 * 1024;
const MAX_EXPANDED: u64 = 2 * 1024 * 1024 * 1024;
const MAX_FILES: usize = 100_000;

/// Owned by the UI task, not its blocking worker, so dropping the task cancels installation.
#[derive(Default)]
pub struct InstallationCancellation(Arc<AtomicBool>);

impl InstallationCancellation {
    pub fn token(&self) -> Arc<AtomicBool> {
        self.0.clone()
    }
}

impl Drop for InstallationCancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    url: String,
    sha256: String,
    bytes: u64,
    expanded_bytes: u64,
}

#[derive(Deserialize)]
struct InstalledArtifact {
    path: String,
    #[serde(flatten)]
    artifact: Artifact,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    protocol: String,
    artifacts: Vec<InstalledArtifact>,
    layoutlib: BTreeMap<String, Artifact>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct File {
    size: u64,
    sha256: String,
    mode: u32,
}

type Inventory = BTreeMap<String, File>;

fn cancelled(cancel: &AtomicBool) -> Result<()> {
    ensure!(
        !cancel.load(Ordering::Acquire),
        "Compose preview setup cancelled. Verified downloads are retained for retry."
    );
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn identity(manifest: &[u8], bridge: &[u8], platform: &str) -> String {
    let mut hash = Sha256::new();
    for input in [manifest, bridge, platform.as_bytes()] {
        hash.update((input.len() as u64).to_le_bytes());
        hash.update(input);
    }
    format!("{:x}", hash.finalize())
}

fn hash_file(path: &Path, cancel: &AtomicBool) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        cancelled(cancel)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn relative(name: &str) -> Result<&Path> {
    let path = Path::new(name);
    ensure!(
        !name.is_empty()
            && !name.contains(['\\', ':'])
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "Unsafe preview runtime path"
    );
    Ok(path)
}

fn regular_file(path: &Path, maximum: u64) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= maximum,
        "Invalid preview cache file: {}",
        path.display()
    );
    Ok(metadata)
}

fn ensure_directory(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Preview storage must be a regular directory"
    );
    Ok(())
}

pub(super) fn installation(
    cancel: &AtomicBool,
    mut progress: impl FnMut(String),
) -> Result<PathBuf> {
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let manifest: Manifest = serde_json::from_slice(MANIFEST)?;
    ensure!(
        manifest.schema == 1 && manifest.protocol == "2",
        "Unsupported preview manifest"
    );
    let native = manifest
        .layoutlib
        .get(&platform)
        .context("Google layoutlib does not provide Compose previews for this platform")?;
    let id = identity(MANIFEST, BRIDGE, &platform);
    fs::create_dir_all(paths::data_dir())?;
    ensure_directory(&crate::managed::root())?;
    let cache = super::cache_directory();
    ensure_directory(&cache)?;
    install_in(&cache, &id, cancel, &mut progress, |staging, progress| {
        // Resolve Java before starting any network activity. Previews require the selected full JDK.
        let java = super::java_binary(staging)?;
        let downloads = cache.join("downloads");
        ensure_directory(&downloads)?;
        let mut additional = 1024 * 1024; // Bridge, inventory and provenance files.
        for artifact in manifest
            .artifacts
            .iter()
            .map(|artifact| &artifact.artifact)
            .chain(std::iter::once(native))
        {
            ensure!(
                artifact.expanded_bytes <= MAX_EXPANDED,
                "Preview artifact exceeds the expanded-size limit"
            );
            additional += artifact.expanded_bytes;
            let cached = downloads.join(&artifact.sha256);
            let verified = regular_file(&cached, artifact.bytes)
                .is_ok_and(|metadata| metadata.len() == artifact.bytes)
                && hash_file(&cached, cancel).is_ok_and(|hash| hash == artifact.sha256);
            cancelled(cancel)?;
            if !verified {
                additional += artifact.bytes;
            }
        }
        crate::managed::validate_storage_budget(&crate::managed::root(), additional, MAX_FILES)?;
        let required = additional + 128 * 1024 * 1024;
        ensure!(
            fs2::available_space(&cache)? >= required,
            "Compose preview setup needs {} MiB of free space for downloads and extraction. Free space, then retry Build & Refresh.",
            required.div_ceil(1024 * 1024)
        );
        let client = download_client()?;
        for artifact in &manifest.artifacts {
            cancelled(cancel)?;
            let relative = relative(&artifact.path)?;
            let path = download(&client, &downloads, &artifact.artifact, cancel, progress)?;
            let destination = staging.join(relative);
            fs::create_dir_all(
                destination
                    .parent()
                    .context("Preview artifact has no parent")?,
            )?;
            fs::copy(path, destination)?;
        }
        let native = download(&client, &downloads, native, cancel, progress)?;
        progress("Extracting Compose preview libraries…".into());
        let expanded = extract_native(&native, &staging.join("layoutlib"), cancel)?;
        ensure!(
            expanded == manifest.layoutlib[&platform].expanded_bytes,
            "Native preview archive expanded size differs from its pin"
        );
        progress("Preparing Compose preview bridge…".into());
        compile_bridge(staging, &java, cancel)?;
        fs::write(
            staging.join(".protocol"),
            format!("{}\n", manifest.protocol),
        )?;
        fs::write(
            staging.join("SOURCE.txt"),
            format!(
                "Google Android tooling: Apache-2.0; preserve JAR and layoutlib notices.\nJava 21 is selected through Android Setup and is not bundled.\n{}\n{}\n",
                manifest
                    .artifacts
                    .iter()
                    .map(|artifact| artifact.artifact.url.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                manifest.layoutlib[&platform].url,
            ),
        )?;
        Ok(())
    })
}

fn install_in(
    cache: &Path,
    id: &str,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(String),
    prepare: impl FnOnce(&Path, &mut dyn FnMut(String)) -> Result<()>,
) -> Result<PathBuf> {
    ensure!(
        id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Invalid preview runtime identity"
    );
    cancelled(cancel)?;
    ensure_directory(cache)?;
    let lock_path = cache.join(".install.lock");
    if lock_path.try_exists()? {
        regular_file(&lock_path, 0)?;
    }
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let lock = options.open(&lock_path)?;
    progress("Checking Compose preview libraries…".into());
    let deadline = Instant::now() + Duration::from_secs(30 * 60);
    let mut waiting = false;
    loop {
        cancelled(cancel)?;
        match lock.try_lock_exclusive() {
            Ok(()) => break,
            Err(error)
                if error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
                    && Instant::now() < deadline =>
            {
                if !waiting {
                    progress("Another Koda window is preparing Compose previews. Waiting…".into());
                    waiting = true;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                return Err(error)
                    .context("Could not lock Compose preview storage. Retry Build & Refresh.");
            }
        }
    }
    let selector = cache.join(format!("{id}.active"));
    if let Ok(metadata) = fs::symlink_metadata(&selector) {
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "Invalid preview runtime selector"
        );
        let selected = (|| -> Result<PathBuf> {
            regular_file(&selector, 256)?;
            let slot = fs::read_to_string(&selector)?;
            let prefix = format!("{id}-install-");
            let suffix = slot
                .strip_prefix(&prefix)
                .context("Invalid preview runtime selector")?;
            ensure!(
                suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "Invalid preview runtime slot"
            );
            Ok(cache.join(slot))
        })();
        if let Ok(directory) = &selected {
            if valid(directory, id, cancel).is_ok() {
                return Ok(directory.clone());
            }
            cancelled(cancel)?;
        } else {
            fs::rename(
                &selector,
                cache.join(format!("{id}.corrupt-{}", uuid::Uuid::new_v4().simple())),
            )?;
        }
    }
    // Only interrupted staging directories are removable; published generations may be in use.
    for entry in fs::read_dir(cache)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("prepare-") && entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        }
    }
    let downloads = cache.join("downloads");
    if downloads.try_exists()? {
        ensure_directory(&downloads)?;
        for entry in fs::read_dir(downloads)? {
            cancelled(cancel)?;
            let entry = entry?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(".download-")
                && entry.file_type()?.is_file()
            {
                fs::remove_file(entry.path())?;
            }
        }
    }
    let budget_root = if cache
        .file_name()
        .is_some_and(|name| name == "compose-preview")
    {
        cache.parent().context("Preview cache has no parent")?
    } else {
        cache
    };
    crate::managed::validate_storage_budget(budget_root, 0, 0)?;
    let staging = tempfile::Builder::new()
        .prefix("prepare-")
        .tempdir_in(cache)?;
    prepare(staging.path(), progress).context("Could not prepare Compose previews. Check your connection and storage, then retry Build & Refresh. Verified downloads are retained.")?;
    cancelled(cancel)?;
    super::validate_installation(staging.path())?;
    fs::write(staging.path().join(".runtime-id"), id)?;
    let inventory = inventory(staging.path(), cancel)?;
    fs::write(
        staging.path().join(".files.json"),
        serde_json::to_vec(&inventory)?,
    )?;
    fs::OpenOptions::new()
        .write(true)
        .open(staging.path().join(".files.json"))?
        .sync_all()?;
    #[cfg(unix)]
    fs::File::open(staging.path())?.sync_all()?;
    valid(staging.path(), id, cancel)?;
    crate::managed::validate_storage_budget(budget_root, 0, 0)?;
    cancelled(cancel)?;
    let slot = format!("{id}-install-{}", uuid::Uuid::new_v4().simple());
    let directory = cache.join(&slot);
    fs::rename(staging.path(), &directory)
        .context("Could not publish Compose preview libraries")?;
    // Replace the selector only after this generation is complete and verified.
    let mut temporary = tempfile::NamedTempFile::new_in(cache)?;
    temporary.write_all(slot.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(selector)
        .map_err(|error| error.error)
        .context("Could not select Compose preview libraries")?;
    #[cfg(unix)]
    fs::File::open(cache)?.sync_all()?;
    Ok(directory)
}

fn inventory(root: &Path, cancel: &AtomicBool) -> Result<Inventory> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    let mut total = 0u64;
    let mut entries = 0;
    while let Some(parent) = pending.pop() {
        for entry in fs::read_dir(parent)? {
            cancelled(cancel)?;
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            entries += 1;
            ensure!(
                entries <= MAX_FILES,
                "Preview runtime contains too many entries"
            );
            ensure!(
                !metadata.file_type().is_symlink(),
                "Preview runtime contains a link"
            );
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            ensure!(metadata.is_file(), "Unexpected preview runtime file type");
            let name = path
                .strip_prefix(root)?
                .to_str()
                .context("Non-Unicode preview path")?
                .replace('\\', "/");
            relative(&name)?;
            if name == ".files.json" {
                continue;
            }
            total = total.saturating_add(metadata.len());
            ensure!(
                total <= MAX_EXPANDED,
                "Preview runtime exceeds the expanded-size limit"
            );
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt as _;
                metadata.permissions().mode() & 0o7777
            };
            #[cfg(not(unix))]
            let mode = 0o644;
            files.insert(
                name,
                File {
                    size: metadata.len(),
                    sha256: hash_file(&path, cancel)?,
                    mode,
                },
            );
            fs::OpenOptions::new().write(true).open(&path)?.sync_all()?;
        }
    }
    Ok(files)
}

fn valid(directory: &Path, id: &str, cancel: &AtomicBool) -> Result<()> {
    let metadata = fs::symlink_metadata(directory)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Invalid preview runtime directory"
    );
    super::validate_installation(directory)?;
    regular_file(&directory.join(".runtime-id"), 64)?;
    ensure!(
        fs::read_to_string(directory.join(".runtime-id"))? == id,
        "Preview runtime identity mismatch"
    );
    let path = directory.join(".files.json");
    regular_file(&path, 16 * 1024 * 1024)?;
    let bytes = fs::read(&path)?;
    let inventory: Inventory = serde_json::from_slice(&bytes)?;
    ensure!(
        inventory.len() <= MAX_FILES,
        "Preview inventory is too large"
    );
    let mut hashes = BTreeMap::new();
    for (name, file) in inventory {
        relative(&name)?;
        ensure!(
            file.mode <= 0o777
                && file.sha256.len() == 64
                && file.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Invalid preview inventory metadata"
        );
        let metadata = regular_file(&directory.join(&name), file.size)?;
        ensure!(metadata.len() == file.size, "Preview library size mismatch");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            ensure!(
                metadata.permissions().mode() & 0o7777 == file.mode,
                "Preview library permissions changed"
            );
        }
        hashes.insert(name, file.sha256);
    }
    hashes.insert(".files.json".into(), digest(&bytes));
    crate::managed::validate_inventory_cancelled(directory, &hashes, cancel)
}

struct DownloadClient {
    client: reqwest::Client,
    executor: DownloadExecutor,
}

struct DownloadExecutor(Option<tokio::runtime::Runtime>);

impl std::ops::Deref for DownloadExecutor {
    type Target = tokio::runtime::Runtime;

    fn deref(&self) -> &Self::Target {
        self.0
            .as_ref()
            .expect("Download executor exists until drop")
    }
}

impl Drop for DownloadExecutor {
    fn drop(&mut self) {
        // System DNS lookups use blocking tasks which cannot be aborted. Do not let
        // a stalled resolver keep the installation lock after the UI cancels.
        if let Some(executor) = self.0.take() {
            executor.shutdown_background();
        }
    }
}

fn download_client() -> Result<DownloadClient> {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let client = {
        let _enter = executor.enter();
        reqwest::Client::builder()
            .https_only(true)
            .redirect_policy(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 || !allowed_url(attempt.url()) {
                    attempt.error("Unapproved preview download redirect")
                } else {
                    attempt.follow()
                }
            }))
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(15 * 60))
            .user_agent("Koda-Compose-Preview")
            .build()?
    };
    Ok(DownloadClient {
        client,
        executor: DownloadExecutor(Some(executor)),
    })
}

fn allowed_url(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("dl.google.com")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
}

fn download(
    client: &DownloadClient,
    cache: &Path,
    artifact: &Artifact,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(String),
) -> Result<PathBuf> {
    let url = reqwest::Url::parse(&artifact.url)?;
    ensure!(
        allowed_url(&url)
            && artifact.bytes > 0
            && artifact.bytes <= MAX_DOWNLOAD
            && artifact.sha256.len() == 64
            && artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Invalid pinned preview artifact"
    );
    let path = cache.join(&artifact.sha256);
    if path.try_exists()? {
        regular_file(&path, MAX_DOWNLOAD)?;
        if fs::metadata(&path)?.len() == artifact.bytes
            && hash_file(&path, cancel)? == artifact.sha256
        {
            return Ok(path);
        }
    }
    cancelled(cancel)?;
    let label = if artifact.url.contains("layoutlib-runtime") {
        "native preview engine"
    } else if artifact.url.contains("layoutlib-resources") {
        "preview resources"
    } else if artifact.url.contains("compose-preview-renderer") {
        "Compose renderer"
    } else {
        "preview libraries"
    };
    progress(format!("Downloading {label}…"));
    let mut response = network(&client.executor, cancel, async {
        client.client.get(url).send().await
    })?
    .error_for_status()?;
    ensure!(
        response
            .content_length()
            .is_none_or(|bytes| bytes == artifact.bytes),
        "The preview download size changed. Update Koda or retry."
    );
    let mut temporary = tempfile::Builder::new()
        .prefix(".download-")
        .tempfile_in(cache)?;
    let mut downloaded = 0u64;
    let mut reported = Instant::now();
    loop {
        cancelled(cancel)?;
        let Some(chunk) = network(&client.executor, cancel, response.chunk())? else {
            break;
        };
        downloaded += chunk.len() as u64;
        ensure!(
            downloaded <= artifact.bytes,
            "Preview download exceeds its pinned size"
        );
        temporary.write_all(&chunk)?;
        if reported.elapsed() >= Duration::from_millis(100) {
            progress(format!(
                "Downloading {label}: {} / {} MiB",
                downloaded / (1024 * 1024),
                artifact.bytes.div_ceil(1024 * 1024)
            ));
            reported = Instant::now();
        }
    }
    temporary.as_file().sync_all()?;
    ensure!(
        downloaded == artifact.bytes && hash_file(temporary.path(), cancel)? == artifact.sha256,
        "Preview download checksum mismatch. Retry Build & Refresh."
    );
    temporary.persist(&path).map_err(|error| error.error)?;
    Ok(path)
}

fn network<T>(
    executor: &tokio::runtime::Runtime,
    cancel: &AtomicBool,
    operation: impl std::future::Future<Output = reqwest::Result<T>>,
) -> Result<T> {
    executor.block_on(async {
        tokio::pin!(operation);
        loop {
            cancelled(cancel)?;
            tokio::select! {
                result = &mut operation => return Ok(result?),
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }
    })
}

fn extract_native(archive: &Path, root: &Path, cancel: &AtomicBool) -> Result<u64> {
    crate::kotlin::ensure_directory(root)?;
    let mut archive = zip::ZipArchive::new(fs::File::open(archive)?)?;
    ensure!(
        archive.len() <= MAX_FILES,
        "Preview archive contains too many entries"
    );
    let mut expanded = 0u64;
    for index in 0..archive.len() {
        cancelled(cancel)?;
        let mut file = archive.by_index(index)?;
        let name = file.name().trim_end_matches('/');
        let path = root.join(relative(name)?);
        let mode = file.unix_mode().unwrap_or(0o644);
        ensure!(
            mode & 0o170000 == 0 || matches!(mode & 0o170000, 0o100000 | 0o040000),
            "Preview archive contains a link or special file"
        );
        expanded = expanded.saturating_add(file.size());
        ensure!(
            expanded <= MAX_EXPANDED,
            "Preview archive exceeds the expanded-size limit"
        );
        if file.is_dir() {
            crate::kotlin::ensure_directory(&path)?;
            continue;
        }
        fs::create_dir_all(
            path.parent()
                .context("Preview archive entry has no parent")?,
        )?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let expected = file.size();
        let mut written = 0u64;
        let mut buffer = [0; 64 * 1024];
        loop {
            cancelled(cancel)?;
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            written += count as u64;
            ensure!(
                written <= expected,
                "Preview archive entry exceeds its declared size"
            );
            output.write_all(&buffer[..count])?;
        }
        ensure!(written == expected, "Incomplete preview archive entry");
        output.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(
                path,
                fs::Permissions::from_mode(if mode & 0o111 != 0 { 0o755 } else { 0o644 }),
            )?;
        }
    }
    Ok(expanded)
}

#[expect(
    clippy::disallowed_methods,
    reason = "Runs in a blocking installation worker; direct child handles provide bounded compilation and cleanup"
)]
fn compile_bridge(root: &Path, java: &Path, cancel: &AtomicBool) -> Result<()> {
    let source = root.join("PreviewBridge.java");
    fs::write(&source, BRIDGE)?;
    let log = tempfile::NamedTempFile::new_in(root)?;
    let javac = java.with_file_name(if cfg!(windows) { "javac.exe" } else { "javac" });
    let mut command = Command::new(javac);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW for the GUI application's compiler.
    }
    let child = command
        .args([
            "--release",
            "21",
            "-proc:none",
            "-encoding",
            "UTF-8",
            "-classpath",
        ])
        .arg(root.join("renderer.jar"))
        .arg("-d")
        .arg(root)
        .arg(&source)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.as_file().try_clone()?))
        .stderr(Stdio::from(log.as_file().try_clone()?))
        .spawn()
        .context("Could not compile the preview bridge. Choose a full JDK 21 in Android Setup.")?;
    let mut child = CompilerProcess(child);
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if cancel.load(Ordering::Acquire)
            || Instant::now() >= deadline
            || log.as_file().metadata()?.len() > 1024 * 1024
        {
            child.0.kill().ok();
            child.0.wait().ok();
            cancelled(cancel)?;
            anyhow::bail!(
                "Preview bridge compilation exceeded its time or output limit. Retry with a full JDK 21."
            );
        }
        match child.0.try_wait()? {
            Some(status) => break status,
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    let mut output = String::new();
    fs::File::open(log.path())?
        .take(16 * 1024)
        .read_to_string(&mut output)?;
    ensure!(
        status.success(),
        "Preview bridge compilation failed: {output}"
    );
    fs::remove_file(source)?;
    Ok(())
}

struct CompilerProcess(std::process::Child);

impl Drop for CompilerProcess {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn fixture(root: &Path, _: &mut dyn FnMut(String)) -> Result<()> {
        fs::create_dir_all(root.join("layoutlib/data"))?;
        for (name, contents) in [
            ("PreviewBridge.class", "bridge"),
            ("renderer.jar", "renderer"),
            ("layoutlib.jar", "layoutlib"),
            ("layoutlib/data/framework_res.jar", "resources"),
            (".protocol", "2\n"),
        ] {
            fs::write(root.join(name), contents)?;
        }
        Ok(())
    }

    fn install(cache: &Path, id: &str) -> Result<PathBuf> {
        install_in(cache, id, &AtomicBool::new(false), &mut |_| {}, fixture)
    }

    #[test]
    fn release_identity_includes_manifest_bridge_and_platform() -> Result<()> {
        let original = identity(MANIFEST, BRIDGE, "linux-x86_64");
        assert_ne!(original, identity(b"new libraries", BRIDGE, "linux-x86_64"));
        assert_ne!(original, identity(MANIFEST, b"new bridge", "linux-x86_64"));
        assert_ne!(original, identity(MANIFEST, BRIDGE, "macos-aarch64"));
        let manifest: Manifest = serde_json::from_slice(MANIFEST)?;
        assert_eq!(manifest.protocol, "2");
        assert_eq!(manifest.artifacts.len(), 3);
        for artifact in manifest
            .artifacts
            .iter()
            .map(|entry| &entry.artifact)
            .chain(manifest.layoutlib.values())
        {
            assert!(allowed_url(&reqwest::Url::parse(&artifact.url)?));
            assert_eq!(artifact.sha256.len(), 64);
            assert!(artifact.bytes > 0 && artifact.bytes < MAX_DOWNLOAD);
        }
        Ok(())
    }

    #[test]
    fn installed_generation_is_reused_offline_and_corruption_is_repaired() -> Result<()> {
        let cache = tempfile::tempdir()?;
        let id = "a".repeat(64);
        let original = install(cache.path(), &id)?;
        let reused = install_in(
            cache.path(),
            &id,
            &AtomicBool::new(false),
            &mut |_| {},
            |_, _| anyhow::bail!("offline"),
        )?;
        assert_eq!(original, reused);
        let modified = fs::metadata(original.join("renderer.jar"))?.modified()?;
        fs::write(original.join("renderer.jar"), "corrupte")?;
        fs::OpenOptions::new()
            .write(true)
            .open(original.join("renderer.jar"))?
            .set_modified(modified)?;
        let repaired = install(cache.path(), &id)?;
        assert_ne!(repaired, original);
        assert!(original.is_dir());
        fs::write(repaired.join("unexpected.jar"), "extra")?;
        let repaired_again = install(cache.path(), &id)?;
        assert_ne!(repaired_again, repaired);
        assert!(!repaired_again.join("unexpected.jar").exists());
        Ok(())
    }

    #[test]
    fn updates_and_failed_repairs_preserve_previous_generations() -> Result<()> {
        let cache = tempfile::tempdir()?;
        let old_id = "b".repeat(64);
        let new_id = "c".repeat(64);
        let old = install(cache.path(), &old_id)?;
        assert!(
            install_in(
                cache.path(),
                &new_id,
                &AtomicBool::new(false),
                &mut |_| {},
                |root, _| {
                    fs::write(root.join("partial"), "interrupted")?;
                    anyhow::bail!("offline")
                }
            )
            .is_err()
        );
        assert_eq!(install(cache.path(), &old_id)?, old);
        assert!(!cache.path().join(format!("{new_id}.active")).exists());
        let new = install(cache.path(), &new_id)?;
        assert_ne!(old, new);
        assert_eq!(install(cache.path(), &old_id)?, old);
        let selector = cache.path().join(format!("{new_id}.active"));
        let selected = fs::read(&selector)?;
        fs::write(new.join("renderer.jar"), "corrupt")?;
        assert!(
            install_in(
                cache.path(),
                &new_id,
                &AtomicBool::new(false),
                &mut |_| {},
                |_, _| anyhow::bail!("offline")
            )
            .is_err()
        );
        assert_eq!(fs::read(selector)?, selected);
        assert!(old.is_dir() && new.is_dir());
        assert!(!fs::read_dir(cache.path())?.any(|entry| {
            entry.is_ok_and(|entry| entry.file_name().to_string_lossy().starts_with("prepare-"))
        }));
        Ok(())
    }

    #[test]
    fn malformed_selectors_and_interrupted_staging_recover() -> Result<()> {
        let cache = tempfile::tempdir()?;
        let id = "d".repeat(64);
        let staging = cache.path().join("prepare-interrupted");
        fs::create_dir(&staging)?;
        fs::write(staging.join("partial"), "partial")?;
        let downloads = cache.path().join("downloads");
        fs::create_dir(&downloads)?;
        let partial = downloads.join(".download-interrupted");
        fs::write(&partial, "partial")?;
        let unrelated = downloads.join("unrelated-file");
        fs::write(&unrelated, "preserve")?;
        for corrupt in [
            b"truncated".as_slice(),
            b"../outside",
            &[0xff],
            &[b'x'; 257],
        ] {
            fs::write(cache.path().join(format!("{id}.active")), corrupt)?;
            assert!(install(cache.path(), &id)?.join("renderer.jar").is_file());
        }
        assert!(!staging.exists());
        assert!(!partial.exists());
        assert_eq!(fs::read(unrelated)?, b"preserve");
        assert_eq!(
            fs::read_dir(cache.path())?
                .filter(|entry| entry
                    .as_ref()
                    .is_ok_and(|entry| entry.file_name().to_string_lossy().contains(".corrupt-")))
                .count(),
            4
        );
        Ok(())
    }

    #[test]
    fn cancellation_does_not_activate_an_incomplete_generation() -> Result<()> {
        let cache = tempfile::tempdir()?;
        let id = "e".repeat(64);
        let guard = InstallationCancellation::default();
        let token = guard.token();
        drop(guard);
        assert!(token.load(Ordering::Acquire));
        assert!(install_in(cache.path(), &id, &token, &mut |_| {}, fixture).is_err());
        let cancel = AtomicBool::new(false);
        assert!(
            install_in(cache.path(), &id, &cancel, &mut |_| {}, |root, progress| {
                fixture(root, progress)?;
                cancel.store(true, Ordering::Release);
                Ok(())
            })
            .is_err()
        );
        assert!(!cache.path().join(format!("{id}.active")).exists());
        Ok(())
    }

    #[test]
    fn concurrent_windows_share_one_generation_and_lock_wait_is_cancellable() -> Result<()> {
        let cache = tempfile::tempdir()?;
        let id = "f".repeat(64);
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(cache.path().join(".install.lock"))?;
        lock.lock_exclusive()?;
        let cancel = AtomicBool::new(false);
        let result = install_in(
            cache.path(),
            &id,
            &cancel,
            &mut |message| {
                if message.contains("Waiting") {
                    cancel.store(true, Ordering::Release);
                }
            },
            fixture,
        );
        assert!(result.is_err());
        lock.unlock()?;
        let installed = std::thread::scope(|scope| {
            let first = scope.spawn(|| install(cache.path(), &id));
            let second = scope.spawn(|| install(cache.path(), &id));
            Ok::<_, anyhow::Error>((first.join().unwrap()?, second.join().unwrap()?))
        })?;
        assert_eq!(installed.0, installed.1);
        Ok(())
    }

    #[test]
    fn concurrent_first_use_creates_cache_without_a_directory_race() -> Result<()> {
        let root = tempfile::tempdir()?;
        let cache = root.path().join("new-cache");
        let id = "2".repeat(64);
        let barrier = std::sync::Barrier::new(2);
        let installed = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                barrier.wait();
                install(&cache, &id)
            });
            let second = scope.spawn(|| {
                barrier.wait();
                install(&cache, &id)
            });
            Ok::<_, anyhow::Error>((first.join().unwrap()?, second.join().unwrap()?))
        })?;
        assert_eq!(installed.0, installed.1);
        Ok(())
    }

    #[test]
    fn stalled_network_operation_is_promptly_cancellable() -> Result<()> {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let result = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(50));
                cancel.store(true, Ordering::Release);
            });
            network::<()>(&executor, &cancel, std::future::pending())
        });
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        assert!(started.elapsed() < Duration::from_secs(2));
        Ok(())
    }

    #[test]
    fn download_client_disposal_does_not_wait_for_blocking_dns_work() -> Result<()> {
        let client = download_client()?;
        let (started, receive_started) = std::sync::mpsc::channel();
        let (finish, receive_finish) = std::sync::mpsc::channel();
        client.executor.spawn_blocking(move || {
            started.send(()).ok();
            receive_finish.recv_timeout(Duration::from_secs(2)).ok();
        });
        receive_started.recv_timeout(Duration::from_secs(2))?;
        let started = Instant::now();
        drop(client);
        let elapsed = started.elapsed();
        finish.send(()).ok();
        assert!(
            elapsed < Duration::from_millis(500),
            "Client disposal waited for a blocking resolver: {elapsed:?}"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn cache_symlinks_are_rejected_and_nested_links_repair_without_touching_outside() -> Result<()>
    {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let cache = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let id = "1".repeat(64);
        symlink(
            outside.path().join("missing"),
            cache.path().join(".install.lock"),
        )?;
        assert!(install(cache.path(), &id).is_err());
        fs::remove_file(cache.path().join(".install.lock"))?;
        symlink(outside.path(), cache.path().join(format!("{id}.active")))?;
        assert!(install(cache.path(), &id).is_err());
        fs::remove_file(cache.path().join(format!("{id}.active")))?;
        let original = install(cache.path(), &id)?;
        fs::rename(original.join("layoutlib"), outside.path().join("layoutlib"))?;
        symlink(outside.path().join("layoutlib"), original.join("layoutlib"))?;
        let repaired = install(cache.path(), &id)?;
        assert_ne!(original, repaired);
        assert_eq!(
            fs::read(outside.path().join("layoutlib/data/framework_res.jar"))?,
            b"resources"
        );
        fs::set_permissions(
            repaired.join("renderer.jar"),
            fs::Permissions::from_mode(0o777),
        )?;
        assert_ne!(install(cache.path(), &id)?, repaired);
        Ok(())
    }

    fn native_zip(name: &str, mode: u32) -> Result<Vec<u8>> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file(
            name,
            zip::write::FileOptions::default().unix_permissions(mode),
        )?;
        zip.write_all(b"native")?;
        Ok(zip.finish()?.into_inner())
    }

    #[test]
    fn native_extraction_rejects_traversal_and_retains_executable_permissions() -> Result<()> {
        for name in ["../outside", "/absolute", "C:/drive", "nested\\escape"] {
            let cache = tempfile::tempdir()?;
            let archive = cache.path().join("native.zip");
            fs::write(&archive, native_zip(name, 0o644)?)?;
            assert!(
                extract_native(
                    &archive,
                    &cache.path().join("layoutlib"),
                    &AtomicBool::new(false)
                )
                .is_err()
            );
        }
        let cache = tempfile::tempdir()?;
        let archive = cache.path().join("native.zip");
        fs::write(&archive, native_zip("bin/native", 0o755)?)?;
        let root = cache.path().join("layoutlib");
        extract_native(&archive, &root, &AtomicBool::new(false))?;
        assert_eq!(fs::read(root.join("bin/native"))?, b"native");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(root.join("bin/native"))?.permissions().mode() & 0o777,
                0o755
            );
        }
        Ok(())
    }

    #[test]
    fn checksum_verified_download_cache_is_reused_without_network() -> Result<()> {
        let cache = tempfile::tempdir()?;
        let bytes = b"cached renderer";
        let artifact = Artifact {
            url: "https://dl.google.com/fixture.jar".into(),
            bytes: bytes.len() as u64,
            expanded_bytes: bytes.len() as u64,
            sha256: digest(bytes),
        };
        let path = cache.path().join(&artifact.sha256);
        fs::write(&path, bytes)?;
        assert_eq!(
            download(
                &download_client()?,
                cache.path(),
                &artifact,
                &AtomicBool::new(false),
                &mut |_| panic!("cached download must not use network")
            )?,
            path
        );
        for url in [
            "http://dl.google.com/file",
            "https://example.com/file",
            "https://dl.google.com:444/file",
            "https://user@dl.google.com/file",
        ] {
            assert!(!allowed_url(&reqwest::Url::parse(url)?));
        }
        Ok(())
    }

    #[test]
    #[ignore = "Downloads pinned Google libraries and requires a local full JDK 21"]
    #[expect(
        clippy::disallowed_methods,
        reason = "Synchronous, opt-in real-library integration test"
    )]
    fn real_preview_libraries_compile_and_run_the_bridge() -> Result<()> {
        let cache = tempfile::tempdir()?;
        let downloads = cache.path().join("downloads");
        fs::create_dir(&downloads)?;
        let manifest: Manifest = serde_json::from_slice(MANIFEST)?;
        let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        let id = identity(MANIFEST, BRIDGE, &platform);
        let cancel = AtomicBool::new(false);
        let client = download_client()?;
        let java = std::env::var_os("PREVIEW_TEST_JAVA")
            .map(PathBuf::from)
            .context("Set PREVIEW_TEST_JAVA to a full JDK 21 java executable")?;
        let installed = install_in(
            cache.path(),
            &id,
            &cancel,
            &mut |message| eprintln!("{message}"),
            |root, progress| {
                for artifact in &manifest.artifacts {
                    let path =
                        download(&client, &downloads, &artifact.artifact, &cancel, progress)?;
                    let destination = root.join(&artifact.path);
                    fs::create_dir_all(destination.parent().unwrap())?;
                    fs::copy(path, destination)?;
                }
                let path = download(
                    &client,
                    &downloads,
                    &manifest.layoutlib[&platform],
                    &cancel,
                    progress,
                )?;
                extract_native(&path, &root.join("layoutlib"), &cancel)?;
                compile_bridge(root, &java, &cancel)?;
                fs::write(root.join(".protocol"), "2\n")?;
                Ok(())
            },
        )?;
        let model = cache.path().join("model.json");
        let output = cache.path().join("previews.json");
        fs::write(&model, r#"{"classPath":[],"projectClassPath":[]}"#)?;
        let result = Command::new(java)
            .args(super::super::bridge_arguments(
                &installed,
                "discover",
                &model,
                Some(&output),
            )?)
            .output()?;
        ensure!(
            result.status.success(),
            "Bridge failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(fs::read_to_string(output)?.trim(), "[]");
        assert_eq!(
            install_in(
                cache.path(),
                &id,
                &cancel,
                &mut |_| {},
                |_, _| anyhow::bail!("offline")
            )?,
            installed
        );
        Ok(())
    }
}
