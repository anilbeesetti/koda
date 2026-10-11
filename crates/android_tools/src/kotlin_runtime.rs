use crate::{kotlin, managed, provision};
use anyhow::{Context as _, Result, bail, ensure};
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead as _, BufReader, Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const PATCH_REVISION: &str = "android-9";
pub const UPDATE_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const REPOSITORY: &str = "anilbeesetti/kotlin-lsp";
const PLATFORM: &str = "macos-aarch64";
const ENTRYPOINT: &str = "kotlin-server/bin/intellij-server";
const PINNED: &[u8] = include_bytes!("../kotlin-runtime-manifest.json");

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Runtime {
    schema: u32,
    release_tag: String,
    upstream_version: String,
    patch_revision: String,
    recipe_sha256: String,
    artifacts: Vec<Artifact>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Artifact {
    platform: String,
    name: String,
    sha256: String,
    size: u64,
    entrypoint: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    published_at: Option<String>,
    assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Active {
    schema: u32,
    slot: String,
    previous: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Generation {
    schema: u32,
    runtime: Runtime,
    files: BTreeMap<String, String>,
    modes: BTreeMap<String, u32>,
}

#[derive(Deserialize, Serialize)]
struct LastCheck {
    checked_at: u64,
}

fn version(value: &str) -> Option<[u32; 3]> {
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return None;
    }
    let mut components = value.split('.');
    let result = [
        components.next()?.parse().ok()?,
        components.next()?.parse().ok()?,
        components.next()?.parse().ok()?,
    ];
    (components.next().is_none()).then_some(result)
}

pub(crate) fn compatible_version(value: &str) -> bool {
    version(value).is_some_and(|version| version >= [263, 6379, 0])
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn tag_version(tag: &str) -> Option<[u32; 3]> {
    let (version_text, recipe) = tag
        .strip_prefix("koda-")?
        .split_once(&format!("-{PATCH_REVISION}-"))?;
    (hex(recipe, 12) && compatible_version(version_text))
        .then(|| version(version_text))
        .flatten()
}

fn asset_url(tag: &str, name: &str) -> String {
    format!("https://github.com/{REPOSITORY}/releases/download/{tag}/{name}")
}

impl Runtime {
    fn artifact(&self) -> Result<&Artifact> {
        ensure!(
            self.schema == 1 && self.patch_revision == PATCH_REVISION,
            "Incompatible Kotlin runtime manifest"
        );
        ensure!(
            hex(&self.recipe_sha256, 64),
            "Invalid Kotlin recipe checksum"
        );
        ensure!(
            tag_version(&self.release_tag).is_some()
                && tag_version(&self.release_tag) == version(&self.upstream_version),
            "Kotlin release version does not match its manifest"
        );
        ensure!(
            self.release_tag.ends_with(
                self.recipe_sha256
                    .get(..12)
                    .context("Missing Kotlin recipe identity")?
            ),
            "Kotlin release recipe does not match its manifest"
        );
        let mut artifacts = self
            .artifacts
            .iter()
            .filter(|artifact| artifact.platform == PLATFORM);
        let artifact = artifacts
            .next()
            .context("This Kotlin release has no Apple Silicon runtime")?;
        ensure!(
            artifacts.next().is_none(),
            "Ambiguous Kotlin runtime platform"
        );
        ensure!(
            artifact.name == format!("{}-{PLATFORM}.tar.gz", self.release_tag)
                && artifact.entrypoint == ENTRYPOINT,
            "Unexpected Kotlin archive or entrypoint"
        );
        ensure!(
            hex(&artifact.sha256, 64) && artifact.size > 0 && artifact.size <= 1024 * 1024 * 1024,
            "Invalid Kotlin archive checksum or size"
        );
        Ok(artifact)
    }
}

fn native_root(profile: &Path) -> PathBuf {
    profile.join("kotlin-lsp")
}

fn missing(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

fn active(root: &Path) -> Result<Option<Active>> {
    match managed::read_json::<Active>(&root.join("active.json"), 4096) {
        Ok(active) => {
            ensure!(active.schema == 1, "Unsupported Kotlin active manifest");
            slot_path(root, &active.slot)?;
            if let Some(previous) = &active.previous {
                slot_path(root, previous)?;
            }
            Ok(Some(active))
        }
        Err(error) if missing(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn slot_path(root: &Path, slot: &str) -> Result<PathBuf> {
    ensure!(
        slot.starts_with("install-") && hex(slot.get(8..).unwrap_or_default(), 32),
        "Invalid Kotlin installation slot"
    );
    Ok(root.join(slot))
}

fn generation(root: &Path, slot: &str, cancel: &AtomicBool) -> Result<Generation> {
    ensure!(
        fs::symlink_metadata(root)?.file_type().is_dir(),
        "Unsafe Kotlin storage directory"
    );
    let directory = slot_path(root, slot)?;
    ensure!(
        fs::symlink_metadata(&directory)?.file_type().is_dir(),
        "Missing or unsafe Kotlin installation directory"
    );
    let generation: Generation =
        managed::read_json(&root.join(format!("{slot}.json")), 16 * 1024 * 1024)?;
    ensure!(
        generation.schema == 1,
        "Unsupported Kotlin generation manifest"
    );
    generation.runtime.artifact()?;
    managed::validate_inventory_cancelled(&directory, &generation.files, cancel)?;
    ensure!(
        generation.files.keys().eq(generation.modes.keys()),
        "Incomplete Kotlin permissions inventory"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for (name, mode) in &generation.modes {
            ensure!(
                fs::symlink_metadata(directory.join(name))?
                    .permissions()
                    .mode()
                    & 0o7777
                    == *mode,
                "Kotlin executable permissions changed: {name}"
            );
        }
    }
    validate_distribution(&directory, &generation.runtime)?;
    Ok(generation)
}

pub(crate) fn resolve_at(profile: &Path) -> Result<PathBuf> {
    let root = native_root(profile);
    if let Some(active) = active(&root)? {
        generation(&root, &active.slot, &AtomicBool::new(false))?;
        return Ok(slot_path(&root, &active.slot)?.join(ENTRYPOINT));
    }
    managed::resolve_at(profile, managed::Tool::Kotlin)
}

fn validate_distribution(directory: &Path, runtime: &Runtime) -> Result<()> {
    let server = directory.join("kotlin-server");
    let product: serde_json::Value =
        managed::read_json(&server.join("product-info.json"), 64 * 1024)?;
    ensure!(
        product["buildNumber"] == runtime.upstream_version
            && product["minRequiredJavaVersion"] == 25,
        "Downloaded Kotlin distribution does not match its release"
    );
    let importer: serde_json::Value =
        managed::read_json(&server.join("zed-native-importer.json"), 64 * 1024)?;
    ensure!(
        importer["revision"] == format!("{}+{PATCH_REVISION}", runtime.upstream_version),
        "Downloaded Kotlin distribution has incompatible Android patches"
    );
    managed::executable(&directory.join(ENTRYPOINT))?;
    managed::executable(&server.join("jbr/Contents/Home/bin/java"))?;
    kotlin::private_java_environment_at(&server)?;
    Ok(())
}

fn save<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    ensure!(
        !fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()),
        "Kotlin metadata must not be a symlink"
    );
    let parent = path.parent().context("Kotlin manifest has no parent")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut temporary, value)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn cancelled(cancel: &AtomicBool) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "Kotlin update cancelled");
    Ok(())
}

fn metadata<T: serde::de::DeserializeOwned>(
    url: &str,
    maximum: u64,
    cancel: &AtomicBool,
) -> Result<T> {
    cancelled(cancel)?;
    let client = provision::download_client_builder()
        .user_agent("Koda-Kotlin-runtime")
        .build()?;
    let response = client.get(url).send()?.error_for_status()?;
    let mut bytes = Vec::new();
    response.take(maximum + 1).read_to_end(&mut bytes)?;
    cancelled(cancel)?;
    ensure!(
        bytes.len() as u64 <= maximum,
        "Kotlin release metadata exceeds the size limit"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

fn select_release(releases: Vec<Release>) -> Option<Release> {
    releases
        .into_iter()
        .filter(|release| {
            !release.draft
                && !release.prerelease
                && tag_version(&release.tag_name).is_some()
                && release
                    .assets
                    .iter()
                    .any(|asset| asset.name == format!("{}-{PLATFORM}.tar.gz", release.tag_name))
        })
        .max_by_key(|release| (tag_version(&release.tag_name), release.published_at.clone()))
}

fn latest(cancel: &AtomicBool) -> Result<Runtime> {
    let releases: Vec<Release> = metadata(
        &format!("https://api.github.com/repos/{REPOSITORY}/releases?per_page=100"),
        2 * 1024 * 1024,
        cancel,
    )?;
    let release =
        select_release(releases).context("No compatible published Kotlin runtime is available")?;
    let manifest = release
        .assets
        .iter()
        .find(|asset| asset.name == "runtime.json")
        .context("Kotlin release manifest is missing")?;
    ensure!(
        manifest.browser_download_url == asset_url(&release.tag_name, "runtime.json")
            && manifest.size <= 64 * 1024,
        "Unexpected Kotlin manifest download"
    );
    let runtime: Runtime = metadata(&manifest.browser_download_url, 64 * 1024, cancel)?;
    ensure!(
        runtime.release_tag == release.tag_name,
        "Kotlin manifest belongs to another release"
    );
    let artifact = runtime.artifact()?;
    let published = release
        .assets
        .iter()
        .find(|asset| asset.name == artifact.name)
        .context("Kotlin release archive is missing")?;
    ensure!(
        published.browser_download_url == asset_url(&runtime.release_tag, &artifact.name)
            && published.size == artifact.size,
        "Kotlin release archive does not match its manifest"
    );
    if let Some(digest) = &published.digest {
        ensure!(
            digest == &format!("sha256:{}", artifact.sha256),
            "Kotlin release checksum does not match its manifest"
        );
    }
    Ok(runtime)
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn checked_recently(root: &Path) -> Result<bool> {
    match managed::read_json::<LastCheck>(&root.join("last-check.json"), 4096) {
        Ok(check) => Ok(now()?
            .checked_sub(check.checked_at)
            .is_some_and(|elapsed| elapsed < UPDATE_INTERVAL.as_secs())),
        Err(error) if missing(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

pub fn manage(
    operation: &str,
    offline: bool,
    cancel: &AtomicBool,
    progress: impl Fn(provision::Progress),
) -> Result<PathBuf> {
    ensure!(
        managed::supported(),
        "Managed Kotlin downloads support Apple Silicon macOS"
    );
    manage_at(
        &managed::root(),
        operation,
        offline,
        cancel,
        &progress,
        &latest,
        &probe,
    )
}

struct ProfileLock(fs::File);

impl Drop for ProfileLock {
    fn drop(&mut self) {
        // Release explicitly: another thread may be forking while this descriptor closes.
        if let Err(error) = fs2::FileExt::unlock(&self.0) {
            eprintln!("Unable to release Kotlin runtime lock: {error}");
        }
    }
}

fn manage_at(
    profile: &Path,
    operation: &str,
    offline: bool,
    cancel: &AtomicBool,
    progress: &impl Fn(provision::Progress),
    discover: &impl Fn(&AtomicBool) -> Result<Runtime>,
    probe: &impl Fn(&Path, &Runtime, &AtomicBool) -> Result<()>,
) -> Result<PathBuf> {
    ensure!(
        matches!(operation, "update" | "install" | "validate" | "rollback"),
        "Invalid Kotlin runtime operation"
    );
    fs::create_dir_all(profile)?;
    kotlin::ensure_directory(profile)?;
    let lock_path = profile.join(".lock");
    ensure!(
        !fs::symlink_metadata(&lock_path).is_ok_and(|metadata| metadata.file_type().is_symlink()),
        "Unsafe managed tool lock"
    );
    let lock = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&lock_path)?;
    lock.try_lock_exclusive()
        .context("Another Koda window is managing tools. Wait and retry.")?;
    let _lock = ProfileLock(lock);
    let root = native_root(profile);
    kotlin::ensure_directory(&root)?;
    let previous = active(&root)?;
    let installed = previous
        .as_ref()
        .map(|active| generation(&root, &active.slot, cancel));
    if operation == "validate" {
        return resolve_at(profile);
    }
    if operation == "rollback" {
        let previous_slot = previous
            .as_ref()
            .and_then(|active| active.previous.as_ref())
            .context("No previous compatible Kotlin installation is available")?;
        let generation = generation(&root, previous_slot, cancel)?;
        let directory = slot_path(&root, previous_slot)?;
        probe(&directory, &generation.runtime, cancel)?;
        cancelled(cancel)?;
        save(
            &root.join("last-check.json"),
            &LastCheck { checked_at: now()? },
        )?;
        save(
            &root.join("active.json"),
            &Active {
                schema: 1,
                slot: previous_slot.clone(),
                previous: previous.as_ref().map(|active| active.slot.clone()),
            },
        )?;
        return Ok(directory.join(ENTRYPOINT));
    }
    if operation == "update" && resolve_at(profile).is_ok() && (offline || checked_recently(&root)?)
    {
        return resolve_at(profile);
    }
    let report = |message: String| {
        progress(provision::Progress {
            finishing: false,
            message,
            downloaded_bytes: 0,
            total_bytes: 0,
        })
    };
    report("Checking for compatible Kotlin language server updates…".into());
    let runtime = if offline {
        serde_json::from_slice(PINNED)?
    } else {
        match discover(cancel) {
            Ok(runtime) => runtime,
            Err(error) => {
                cancelled(cancel)?;
                if resolve_at(profile).is_ok() {
                    return Err(error).context(
                        "Could not check Kotlin updates; the installed runtime was preserved",
                    );
                }
                report(format!(
                    "Release discovery unavailable ({error:#}); using Koda's pinned runtime"
                ));
                serde_json::from_slice(PINNED)?
            }
        }
    };
    let artifact = runtime.artifact()?;
    if let Some(Ok(installed)) = &installed {
        if version(&runtime.upstream_version) < version(&installed.runtime.upstream_version) {
            save(
                &root.join("last-check.json"),
                &LastCheck { checked_at: now()? },
            )?;
            return resolve_at(profile);
        }
        if operation == "update" && runtime.release_tag == installed.runtime.release_tag {
            save(
                &root.join("last-check.json"),
                &LastCheck { checked_at: now()? },
            )?;
            return resolve_at(profile);
        }
    }
    managed::validate_storage_budget(profile, artifact.size + 2 * 1024 * 1024 * 1024, 30_000)?;
    ensure!(
        fs2::available_space(profile)? >= artifact.size + 2 * 1024 * 1024 * 1024,
        "Kotlin setup needs space for a staged runtime and its archive"
    );
    kotlin::ensure_directory(&root.join("downloads"))?;
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(".stage-") {
            ensure!(
                entry.file_type()?.is_dir(),
                "Unsafe Kotlin staging recovery entry"
            );
            fs::remove_dir_all(entry.path())?;
        }
    }
    let archive = provision::download_runtime(
        &root,
        &asset_url(&runtime.release_tag, &artifact.name),
        &artifact.sha256,
        artifact.size,
        offline,
        cancel,
        progress,
    )?;
    let staging = tempfile::Builder::new()
        .prefix(".stage-")
        .tempdir_in(&root)?;
    report(format!("Extracting Kotlin {}…", runtime.upstream_version));
    provision::extract(
        &archive,
        "tar.gz",
        false,
        staging.path(),
        cancel,
        &mut 0,
        &mut 0,
    )?;
    ensure!(
        fs::read_dir(staging.path())?.count() == 1,
        "Unexpected Kotlin archive roots"
    );
    validate_distribution(staging.path(), &runtime)?;
    report("Checking Kotlin language server initialization…".into());
    probe(staging.path(), &runtime, cancel)?;
    let (files, modes) = provision::inventory(staging.path(), cancel)?;
    let slot = format!("install-{}", uuid::Uuid::new_v4().simple());
    let directory = slot_path(&root, &slot)?;
    cancelled(cancel)?;
    let generation = Generation {
        schema: 1,
        runtime,
        files,
        modes,
    };
    save(&root.join(format!("{slot}.json")), &generation)?;
    fs::rename(staging.path(), &directory)?;
    cancelled(cancel)?;
    save(
        &root.join("last-check.json"),
        &LastCheck { checked_at: now()? },
    )?;
    save(
        &root.join("active.json"),
        &Active {
            schema: 1,
            slot,
            previous: previous
                .filter(|_| {
                    installed
                        .as_ref()
                        .is_some_and(|installed| installed.is_ok())
                })
                .map(|previous| previous.slot),
        },
    )?;
    report(format!(
        "Kotlin {}+{PATCH_REVISION} is ready",
        generation.runtime.upstream_version
    ));
    Ok(directory.join(ENTRYPOINT))
}

fn read_initialization(output: impl std::io::Read, expected_version: &str) -> Result<()> {
    let mut output = BufReader::new(output);
    for _ in 0..100 {
        let mut length = None;
        for _ in 0..32 {
            let mut line = String::new();
            ensure!(
                output.by_ref().take(4097).read_line(&mut line)? > 0,
                "Kotlin server reset the connection during initialization"
            );
            ensure!(
                line.len() <= 4096,
                "Kotlin response header exceeded the size limit"
            );
            if line.trim().is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length:") {
                length = Some(value.trim().parse::<usize>()?);
            }
        }
        let length = length.context("Missing Kotlin response length")?;
        ensure!(
            length <= 1024 * 1024,
            "Kotlin initialization response exceeded the size limit"
        );
        let mut body = vec![0; length];
        output.read_exact(&mut body)?;
        let response: serde_json::Value = serde_json::from_slice(&body)?;
        if response.get("id") == Some(&json!(1)) {
            ensure!(
                response.get("error").is_none()
                    && response["result"]["serverInfo"]["version"]
                        == format!("ILS-{expected_version}"),
                "Kotlin server returned an incompatible initialization result: {response}"
            );
            return Ok(());
        }
    }
    bail!("Kotlin server did not acknowledge initialization")
}

#[allow(
    clippy::disallowed_methods,
    reason = "The probe runs on a blocking worker and owns its process group and synchronous pipe reader"
)]
fn probe(directory: &Path, runtime: &Runtime, cancel: &AtomicBool) -> Result<()> {
    cancelled(cancel)?;
    let profile = tempfile::tempdir()?;
    let stderr_path = profile.path().join("stderr.log");
    let stderr = fs::File::create(&stderr_path)?;
    let (java_variable, java_home) =
        kotlin::private_java_environment_at(&directory.join("kotlin-server"))?;
    let mut command = Command::new(directory.join(ENTRYPOINT));
    command
        .args(["--stdio", "--data-sharing=none"])
        .arg(format!(
            "--system-path={}",
            profile.path().join("system").display()
        ))
        .env(java_variable, java_home)
        .env(
            "IJ_JAVA_OPTIONS",
            format!(
                "-Didea.config.path={} -Didea.log.path={} -Dintellij.server.eula.persistence=false",
                profile.path().join("config").display(),
                profile.path().join("log").display()
            ),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    let mut stopped = false;
    let result = (|| -> Result<()> {
        let request = serde_json::to_vec(
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":std::process::id(),"rootUri":null,"workspaceFolders":[],"initializationOptions":{"projects":[]},"capabilities":{}}}),
        )?;
        let mut input = child.stdin.take().context("Kotlin probe has no input")?;
        write!(input, "Content-Length: {}\r\n\r\n", request.len())?;
        input.write_all(&request)?;
        input.flush()?;
        let output = child.stdout.take().context("Kotlin probe has no output")?;
        let expected = runtime.upstream_version.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            if sender.send(read_initialization(output, &expected)).is_err() {
                // The owner has already cancelled or timed out and stops the whole process group.
            }
        });
        let started = Instant::now();
        let result = loop {
            if let Err(error) = cancelled(cancel) {
                break Err(error);
            }
            if started.elapsed() >= Duration::from_secs(45) {
                break Err(anyhow::anyhow!("Kotlin initialization timed out"));
            }
            match receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(result) => break result,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(error) => break Err(error.into()),
            }
        };
        let cleanup = stop_probe(&mut child);
        stopped = cleanup.is_ok();
        reader
            .join()
            .map_err(|_| anyhow::anyhow!("Kotlin probe reader failed"))?;
        cleanup?;
        result
    })();
    // Also clean up failures before the reader is started (for example a closed input pipe).
    if !stopped {
        stop_probe(&mut child)?;
    }
    result.with_context(|| {
        format!(
            "Downloaded Kotlin server failed its startup check: {}",
            fs::read_to_string(stderr_path)
                .map(|text| text
                    .chars()
                    .rev()
                    .take(2048)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>())
                .unwrap_or_else(|error| format!("Unable to read server stderr: {error}"))
        )
    })
}

fn stop_probe(child: &mut std::process::Child) -> Result<()> {
    #[cfg(unix)]
    {
        let process = i32::try_from(child.id())?;
        let status = unsafe { libc::kill(-process, libc::SIGKILL) };
        if status != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error.into());
            }
        }
    }
    #[cfg(not(unix))]
    if child.try_wait()?.is_none() {
        child.kill()?;
    }
    child.wait()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest as _, Sha256};

    fn fixture(profile: &Path, version: &str) -> Result<Runtime> {
        let mut runtime: Runtime = serde_json::from_slice(PINNED)?;
        runtime.upstream_version = version.into();
        runtime.release_tag = format!(
            "koda-{version}-{PATCH_REVISION}-{}",
            &runtime.recipe_sha256[..12]
        );
        let product =
            json!({"buildNumber":version,"minRequiredJavaVersion":25,"envVarBaseName":""})
                .to_string();
        let importer = json!({"revision":format!("{version}+{PATCH_REVISION}")}).to_string();
        let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut archive = tar::Builder::new(gzip);
        for (name, contents, mode) in [
            ("kotlin-server/product-info.json", product.as_str(), 0o644),
            (
                "kotlin-server/zed-native-importer.json",
                importer.as_str(),
                0o644,
            ),
            (ENTRYPOINT, "#!/bin/sh\nexit 0\n", 0o755),
            (
                "kotlin-server/jbr/Contents/Home/bin/java",
                "#!/bin/sh\nexit 0\n",
                0o755,
            ),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(mode);
            header.set_cksum();
            archive.append_data(&mut header, name, contents.as_bytes())?;
        }
        let bytes = archive.into_inner()?.finish()?;
        let artifact = runtime
            .artifacts
            .first_mut()
            .context("Missing fixture artifact")?;
        artifact.name = format!("{}-{PLATFORM}.tar.gz", runtime.release_tag);
        artifact.size = bytes.len() as u64;
        artifact.sha256 = format!("{:x}", Sha256::digest(&bytes));
        let downloads = native_root(profile).join("downloads");
        fs::create_dir_all(&downloads)?;
        fs::write(
            downloads.join(format!("{}.archive", artifact.sha256)),
            bytes,
        )?;
        Ok(runtime)
    }

    fn install(profile: &Path, runtime: &Runtime) -> Result<PathBuf> {
        manage_at(
            profile,
            "install",
            false,
            &AtomicBool::new(false),
            &|_| {},
            &|_| Ok(runtime.clone()),
            &|_, _, _| Ok(()),
        )
    }

    #[test]
    fn release_selection_requires_published_compatible_platform() -> Result<()> {
        let release = |tag: &str,
                       draft: bool,
                       prerelease: bool,
                       platform: &str|
         -> Result<Release> {
            Ok(serde_json::from_value(
                json!({"tag_name":tag,"draft":draft,"prerelease":prerelease,"published_at":"2026-10-11T00:00:00Z","assets":[{"name":format!("{tag}-{platform}.tar.gz"),"browser_download_url":"","size":1}]}),
            )?)
        };
        let expected = "koda-263.6379.0-android-9-abcdef012345";
        let releases = vec![
            release(expected, false, false, PLATFORM)?,
            release(
                "koda-263.7000.0-android-9-abcdef012345",
                true,
                false,
                PLATFORM,
            )?,
            release(
                "koda-263.8000.0-android-9-abcdef012345",
                false,
                true,
                PLATFORM,
            )?,
            release(
                "koda-264.1000.0-android-10-abcdef012345",
                false,
                false,
                PLATFORM,
            )?,
            release(
                "koda-263.9000.0-android-9-abcdef012345",
                false,
                false,
                "linux-x64",
            )?,
            release(
                "koda-263.4702.0-android-9-abcdef012345",
                false,
                false,
                PLATFORM,
            )?,
        ];
        assert_eq!(
            select_release(releases)
                .context("No compatible release")?
                .tag_name,
            expected
        );
        assert_eq!(tag_version("koda-+263.6379.0-android-9-abcdef012345"), None);
        assert_eq!(tag_version("koda-263.6379.0-android-9-../../unsafe"), None);
        Ok(())
    }

    #[test]
    fn pinned_manifest_and_identity_validation() -> Result<()> {
        let runtime: Runtime = serde_json::from_slice(PINNED)?;
        runtime.artifact()?;
        for mutate in [
            |r: &mut Runtime| r.schema = 2,
            |r: &mut Runtime| r.patch_revision = "android-10".into(),
            |r: &mut Runtime| r.upstream_version = "invalid".into(),
            |r: &mut Runtime| {
                r.upstream_version = "invalid".into();
                r.release_tag = format!("invalid-{}", &r.recipe_sha256[..12]);
            },
            |r: &mut Runtime| r.artifacts[0].entrypoint = "../bin/server".into(),
            |r: &mut Runtime| r.artifacts[0].sha256 = "bad".into(),
            |r: &mut Runtime| r.artifacts[0].name = "other.tar.gz".into(),
            |r: &mut Runtime| r.recipe_sha256 = "f".repeat(64),
        ] {
            let mut changed = runtime.clone();
            mutate(&mut changed);
            assert!(changed.artifact().is_err());
        }
        Ok(())
    }

    #[test]
    fn upgrades_are_transactional_and_rollback_keeps_both_generations() -> Result<()> {
        let profile = tempfile::tempdir()?;
        let first = fixture(profile.path(), "263.6379.0")?;
        let original = install(profile.path(), &first)?;
        let second = fixture(profile.path(), "263.7000.0")?;
        let failed = manage_at(
            profile.path(),
            "install",
            false,
            &AtomicBool::new(false),
            &|_| {},
            &|_| Ok(second.clone()),
            &|_, _, _| bail!("expired"),
        );
        assert!(failed.is_err());
        assert_eq!(resolve_at(profile.path())?, original);
        let upgraded = install(profile.path(), &second)?;
        assert_ne!(upgraded, original);
        assert!(original.is_file());
        let rolled_back = manage_at(
            profile.path(),
            "rollback",
            true,
            &AtomicBool::new(false),
            &|_| {},
            &|_| bail!("must not discover"),
            &|_, _, _| Ok(()),
        )?;
        assert_eq!(rolled_back, original);
        assert!(upgraded.is_file());
        assert_eq!(resolve_at(profile.path())?, original);
        Ok(())
    }

    #[test]
    fn cancellation_checksum_failure_and_network_failure_preserve_active() -> Result<()> {
        let profile = tempfile::tempdir()?;
        let first = fixture(profile.path(), "263.6379.0")?;
        let original = install(profile.path(), &first)?;
        let next = fixture(profile.path(), "263.7000.0")?;
        let cancel = AtomicBool::new(false);
        let cancelled_install = manage_at(
            profile.path(),
            "install",
            false,
            &cancel,
            &|_| {},
            &|_| Ok(next.clone()),
            &|_, _, cancel| {
                cancel.store(true, Ordering::Relaxed);
                Ok(())
            },
        );
        assert!(cancelled_install.is_err());
        assert_eq!(resolve_at(profile.path())?, original);
        let artifact = first.artifact()?;
        let archive = native_root(profile.path())
            .join("downloads")
            .join(format!("{}.archive", artifact.sha256));
        fs::write(archive, vec![0; artifact.size as usize])?;
        assert!(
            provision::download_runtime(
                &native_root(profile.path()),
                &asset_url(&first.release_tag, &artifact.name),
                &artifact.sha256,
                artifact.size,
                true,
                &AtomicBool::new(false),
                &|_| {}
            )
            .is_err()
        );
        assert!(
            manage_at(
                profile.path(),
                "install",
                true,
                &AtomicBool::new(false),
                &|_| {},
                &|_| Ok(next.clone()),
                &|_, _, _| Ok(())
            )
            .is_err()
        );
        assert_eq!(resolve_at(profile.path())?, original);
        assert!(
            manage_at(
                profile.path(),
                "install",
                false,
                &AtomicBool::new(false),
                &|_| {},
                &|_| bail!("network down"),
                &|_, _, _| Ok(())
            )
            .is_err()
        );
        assert_eq!(resolve_at(profile.path())?, original);
        Ok(())
    }

    #[test]
    fn offline_throttling_and_downgrade_do_not_download() -> Result<()> {
        let profile = tempfile::tempdir()?;
        let current = fixture(profile.path(), "263.7000.0")?;
        let original = install(profile.path(), &current)?;
        for offline in [false, true] {
            let result = manage_at(
                profile.path(),
                "update",
                offline,
                &AtomicBool::new(false),
                &|_| {},
                &|_| bail!("must not discover"),
                &|_, _, _| bail!("must not probe"),
            )?;
            assert_eq!(result, original);
        }
        let older = fixture(profile.path(), "263.6379.0")?;
        assert_eq!(install(profile.path(), &older)?, original);
        Ok(())
    }

    #[test]
    fn inventory_tampering_and_profile_lock_are_detected() -> Result<()> {
        let profile = tempfile::tempdir()?;
        let runtime = fixture(profile.path(), "263.6379.0")?;
        let server = install(profile.path(), &runtime)?;
        let lock = fs::OpenOptions::new()
            .append(true)
            .open(profile.path().join(".lock"))?;
        lock.lock_exclusive()?;
        assert!(install(profile.path(), &runtime).is_err());
        fs2::FileExt::unlock(&lock)?;
        fs::write(server, "tampered executable")?;
        assert!(resolve_at(profile.path()).is_err());
        Ok(())
    }

    #[test]
    fn initialization_rejects_expired_and_incompatible_servers() -> Result<()> {
        let message = |body: serde_json::Value| {
            let body = body.to_string();
            format!("Content-Length: {}\r\n\r\n{body}", body.len())
        };
        let good = message(json!({"id":1,"result":{"serverInfo":{"version":"ILS-263.6379.0"}}}));
        read_initialization(good.as_bytes(), "263.6379.0")?;
        assert!(read_initialization(&b""[..], "263.6379.0").is_err());
        assert!(read_initialization(good.as_bytes(), "263.7000.0").is_err());
        assert!(
            read_initialization(
                message(json!({"id":1,"error":{"message":"expired"}})).as_bytes(),
                "263.6379.0"
            )
            .is_err()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn startup_probe_stops_process_group_and_reports_expiration() -> Result<()> {
        let profile = tempfile::tempdir()?;
        let runtime = fixture(profile.path(), "263.6379.0")?;
        let binary = install(profile.path(), &runtime)?;
        let directory = binary
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .context("Missing fixture directory")?;
        let body = json!({"id":1,"result":{"serverInfo":{"version":"ILS-263.6379.0"}}}).to_string();
        fs::write(
            &binary,
            format!(
                "#!/bin/sh\nprintf '%s' 'Content-Length: {}\r\n\r\n{}'\ncat >/dev/null\n",
                body.len(),
                body
            ),
        )?;
        probe(directory, &runtime, &AtomicBool::new(false))?;
        fs::write(
            &binary,
            "#!/bin/sh\nprintf 'This build of kotlin-server has expired.' >&2\nexit 1\n",
        )?;
        let error = probe(directory, &runtime, &AtomicBool::new(false))
            .expect_err("Expired server must fail activation");
        assert!(format!("{error:#}").contains("expired"));
        Ok(())
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    #[ignore = "Downloads and initializes the published macOS runtime"]
    fn published_runtime_download_and_offline_reuse() -> Result<()> {
        let profile = tempfile::tempdir()?;
        let cancel = AtomicBool::new(false);
        let binary = manage_at(
            profile.path(),
            "install",
            false,
            &cancel,
            &|progress| eprintln!("{}", progress.message),
            &latest,
            &probe,
        )?;
        kotlin::validate_official_server(&binary)?;
        let offline = manage_at(
            profile.path(),
            "update",
            true,
            &cancel,
            &|_| {},
            &|_| bail!("offline release discovery"),
            &probe,
        )?;
        assert_eq!(offline, binary);
        Ok(())
    }
}
