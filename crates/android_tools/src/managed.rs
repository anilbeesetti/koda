use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Kotlin,
    Debugger,
    Preview,
}

const KOTLIN: &[u8] = include_bytes!("../../../script/install-android-kotlin");
const KOTLIN_PATCH: &[u8] = include_bytes!("../../../script/android-kotlin-official.patch");
const COMPOSE: &[u8] = include_bytes!("../../../script/android-kotlin-compose.kt");
const NAVIGATION: &[u8] = include_bytes!("../../../script/android-kotlin-navigation.kt");
const DEBUGGER: &[u8] = include_bytes!("../../../script/install-android-debugger");
const DEBUGGER_PATCH: &[u8] = include_bytes!("../../../script/android-kotlin-debugger.patch");
const DEBUGGER_VERIFICATION: &[u8] =
    include_bytes!("../../../script/android-debugger-verification.xml");
const PREVIEW: &[u8] = include_bytes!("../../../script/install-android-preview");
const BRIDGE: &[u8] = include_bytes!("PreviewBridge.java");
const MANAGER: &[u8] = include_bytes!("../../../script/manage-android-tools");

impl Tool {
    pub const ALL: [Self; 3] = [Self::Kotlin, Self::Debugger, Self::Preview];

    pub fn name(self) -> &'static str {
        match self {
            Self::Kotlin => "kotlin",
            Self::Debugger => "debugger",
            Self::Preview => "preview",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Kotlin => "Kotlin language server",
            Self::Debugger => "Android debugger",
            Self::Preview => "Compose preview",
        }
    }

    fn entrypoint(self) -> &'static str {
        match self {
            Self::Kotlin => "kotlin-server-263.4702.0/bin/intellij-server",
            Self::Debugger => "bin/kotlin-debug-adapter",
            Self::Preview => "PreviewBridge.class",
        }
    }

    fn recipes(self) -> Vec<(&'static str, &'static [u8])> {
        let mut files = vec![
            ("script/manage-android-tools", MANAGER),
            ("script/install-android-kotlin", KOTLIN),
        ];
        match self {
            Self::Kotlin => files.extend([
                ("script/android-kotlin-official.patch", KOTLIN_PATCH),
                ("script/android-kotlin-compose.kt", COMPOSE),
                ("script/android-kotlin-navigation.kt", NAVIGATION),
            ]),
            Self::Debugger => files.extend([
                ("script/install-android-debugger", DEBUGGER),
                ("script/android-kotlin-debugger.patch", DEBUGGER_PATCH),
                (
                    "script/android-debugger-verification.xml",
                    DEBUGGER_VERIFICATION,
                ),
            ]),
            Self::Preview => files.extend([
                ("script/install-android-preview", PREVIEW),
                ("crates/android_tools/src/PreviewBridge.java", BRIDGE),
            ]),
        }
        files
    }

    pub fn recipe(self) -> String {
        let mut digest = Sha256::new();
        for (name, bytes) in self.recipes() {
            digest.update(name.as_bytes());
            digest.update([0]);
            digest.update(bytes);
        }
        format!("{digest:x}", digest = digest.finalize())
    }
}

pub fn supported() -> bool {
    cfg!(all(target_os = "macos", target_arch = "aarch64"))
}

pub fn root() -> PathBuf {
    paths::data_dir().join("android-tools")
}

#[derive(Debug, Deserialize)]
struct Manifest {
    schema: u32,
    tool: String,
    recipe: String,
    slot: String,
    entrypoint: String,
    files: BTreeMap<String, String>,
    modes: BTreeMap<String, u32>,
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path, maximum: u64) -> Result<T> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "{} must be a regular file",
        path.display()
    );
    ensure!(
        metadata.len() <= maximum,
        "{} exceeds the size limit",
        path.display()
    );
    serde_json::from_slice(&fs::read(path)?).context("Invalid managed tool manifest")
}

fn digest_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn validate_inventory(directory: &Path, files: &BTreeMap<String, String>) -> Result<()> {
    static VERIFIED: OnceLock<Mutex<BTreeMap<PathBuf, String>>> = OnceLock::new();
    let mut paths = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    let mut total = 0u64;
    while let Some(parent) = pending.pop() {
        for entry in fs::read_dir(parent)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                pending.push(path);
                ensure!(
                    pending.len() + paths.len() < 100_000,
                    "Runtime file count exceeds the limit"
                );
                continue;
            }
            ensure!(
                metadata.is_file() || metadata.file_type().is_symlink(),
                "Unexpected runtime file type"
            );
            total = total.saturating_add(metadata.len());
            ensure!(
                total <= 10 * 1024 * 1024 * 1024 && paths.len() < 100_000,
                "Runtime inventory exceeds the limit"
            );
            let name = path
                .strip_prefix(directory)?
                .to_str()
                .context("Runtime contains a non-Unicode path")?
                .replace(std::path::MAIN_SEPARATOR, "/");
            ensure!(files.contains_key(&name), "Unexpected runtime file: {name}");
            paths.push((name, path, metadata));
        }
    }
    ensure!(
        paths.len() == files.len(),
        "Runtime files are missing. Choose Install / repair"
    );
    paths.sort_by(|left, right| left.0.cmp(&right.0));
    let canonical = directory.canonicalize()?;
    let mut signature = Sha256::new();
    for (name, path, metadata) in &paths {
        signature.update(name.as_bytes());
        signature.update(
            files
                .get(name)
                .context("Missing runtime digest")?
                .as_bytes(),
        );
        signature.update(metadata.len().to_le_bytes());
        signature.update(format!("{:?}", metadata.modified()?).as_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            for value in [
                metadata.ino(),
                metadata.dev(),
                metadata.ctime() as u64,
                metadata.ctime_nsec() as u64,
                metadata.mtime_nsec() as u64,
                u64::from(metadata.mode()),
            ] {
                signature.update(value.to_le_bytes());
            }
        }
        if metadata.file_type().is_symlink() {
            ensure!(
                path.canonicalize()?.starts_with(&canonical),
                "Runtime link escaped its installation"
            );
            let link = format!(
                "link:{}",
                fs::read_link(path)?
                    .to_str()
                    .context("Non-Unicode runtime link")?
            );
            ensure!(
                files.get(name) == Some(&link),
                "Runtime link integrity check failed: {name}"
            );
            signature.update(link.as_bytes());
        }
    }
    let signature = format!("{:x}", signature.finalize());
    let mut verified = VERIFIED
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| anyhow::anyhow!("Runtime validation lock failed"))?;
    if verified.get(&canonical) == Some(&signature) {
        return Ok(());
    }
    for (name, path, metadata) in &paths {
        if metadata.is_file() {
            ensure!(
                files.get(name) == Some(&digest_file(path)?),
                "Runtime integrity check failed: {name}. Choose Install / repair"
            );
        }
    }
    if verified.len() >= 8 {
        verified.clear();
    }
    verified.insert(canonical, signature);
    Ok(())
}

pub fn resolve(tool: Tool) -> Result<PathBuf> {
    resolve_at(&root(), tool).with_context(|| format!("{} is unavailable. Open Android tools → Tool setup, then Install / repair or Validate.", tool.label()))
}

pub fn validate_language_server_binary(path: &Path) -> Result<()> {
    validate_language_server_binary_at(&root(), path)
}

fn validate_language_server_binary_at(root: &Path, path: &Path) -> Result<()> {
    let canonical_path = path.canonicalize().ok();
    let current_profile = path.starts_with(root)
        || root
            .canonicalize()
            .ok()
            .zip(canonical_path.as_ref())
            .is_some_and(|(root, path)| path.starts_with(root));
    let managed_installation = |path: &Path| {
        path.ancestors().any(|ancestor| {
            ancestor
                .file_name()
                .is_some_and(|name| name == "android-tools")
                && path
                    .strip_prefix(ancestor)
                    .ok()
                    .and_then(|relative| relative.components().next())
                    .and_then(|component| component.as_os_str().to_str())
                    .is_some_and(|slot| {
                        slot.starts_with("install-")
                            && slot.len() == 40
                            && slot[8..].bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
        })
    };
    let managed = current_profile
        || managed_installation(path)
        || canonical_path.as_deref().is_some_and(managed_installation);
    if managed {
        ensure!(
            current_profile,
            "This project references another Koda profile's managed Kotlin installation. Configure Kotlin to select this profile's validated runtime"
        );
        let current = resolve_at(root, Tool::Kotlin).context("Managed Kotlin is unavailable. Open Android → Tool setup and Install / repair, then Configure Kotlin")?;
        ensure!(
            path.canonicalize()? == current.canonicalize()?,
            "This project references an older managed Kotlin installation. Configure Kotlin to select this Koda version's validated runtime"
        );
    }
    Ok(())
}

fn resolve_at(root: &Path, tool: Tool) -> Result<PathBuf> {
    let manifest: Manifest = read_json(
        &root.join(format!("{}.json", tool.name())),
        16 * 1024 * 1024,
    )?;
    ensure!(
        manifest.schema == 1 && manifest.tool == tool.name(),
        "Unsupported runtime manifest"
    );
    ensure!(
        manifest.recipe == tool.recipe(),
        "The runtime needs an update for this Koda version"
    );
    ensure!(
        manifest.slot.starts_with("install-")
            && manifest.slot.len() == 40
            && manifest.slot[8..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit()),
        "Invalid managed installation slot"
    );
    ensure!(
        manifest.entrypoint == tool.entrypoint(),
        "Unexpected runtime entrypoint"
    );
    let directory = root.join(&manifest.slot);
    ensure!(
        !fs::symlink_metadata(&directory)?.file_type().is_symlink(),
        "Managed installation must not be a symlink"
    );
    let canonical = directory.canonicalize()?;
    ensure!(
        canonical.starts_with(root.canonicalize()?),
        "Runtime escaped the managed directory"
    );
    let entrypoint = directory.join(tool.entrypoint());
    ensure!(
        entrypoint.canonicalize()?.starts_with(&canonical),
        "Runtime entrypoint escaped its installation"
    );
    validate_inventory(&directory, &manifest.files)?;
    ensure!(
        manifest.modes.keys().eq(manifest.files.keys()),
        "Runtime permissions manifest is incomplete"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for (name, mode) in &manifest.modes {
            ensure!(
                fs::symlink_metadata(directory.join(name))?
                    .permissions()
                    .mode()
                    & 0o7777
                    == *mode,
                "Runtime permissions changed: {name}. Choose Install / repair"
            );
        }
    }
    if tool != Tool::Preview {
        executable(&entrypoint)?;
    }
    if tool == Tool::Kotlin {
        executable(&directory.join("kotlin-server-263.4702.0/jbr/Contents/Home/bin/java"))?;
    }
    Ok(if tool == Tool::Preview {
        directory
    } else {
        entrypoint
    })
}

pub fn executable(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute() && path.is_file(),
        "Choose an existing absolute executable path"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        ensure!(
            fs::metadata(path)?.permissions().mode() & 0o111 != 0,
            "{} must be executable",
            path.display()
        );
    }
    Ok(())
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    pub sdk: Option<PathBuf>,
    pub jdk: Option<PathBuf>,
    pub android_cli: Option<PathBuf>,
}

pub fn environment() -> Result<Environment> {
    environment_at(&root())
}

fn environment_at(root: &Path) -> Result<Environment> {
    let path = root.join("environment.json");
    match read_json(&path, 64 * 1024) {
        Ok(environment) => Ok(environment),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(Environment::default())
        }
        Err(error) => Err(error),
    }
}

#[derive(Clone, Copy)]
pub enum Dependency {
    Sdk,
    Jdk,
    AndroidCli,
}

pub fn save_dependency(dependency: Dependency, path: &Path) -> Result<()> {
    save_dependency_at(&root(), dependency, path)
}

fn save_dependency_at(root: &Path, dependency: Dependency, path: &Path) -> Result<()> {
    let path = path.canonicalize()?;
    match dependency {
        Dependency::Sdk => executable(&path.join(if cfg!(windows) {
            "platform-tools/adb.exe"
        } else {
            "platform-tools/adb"
        }))?,
        Dependency::Jdk => {
            ensure!(
                super::kotlin::is_java_21(&path),
                "Choose a JDK 21 home containing release and bin/java"
            );
            executable(&path.join(if cfg!(windows) {
                "bin/java.exe"
            } else {
                "bin/java"
            }))?;
            executable(&path.join(if cfg!(windows) {
                "bin/javac.exe"
            } else {
                "bin/javac"
            }))?;
        }
        Dependency::AndroidCli => executable(&path)?,
    }
    fs::create_dir_all(root)?;
    super::kotlin::ensure_directory(&root)?;
    let lock_path = root.join(".lock");
    ensure!(
        !fs::symlink_metadata(&lock_path).is_ok_and(|metadata| metadata.file_type().is_symlink()),
        "Unsafe managed tool lock"
    );
    let lock = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(lock_path)?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd as _;
        // Use the same kernel lock as the provisioning process; a crash releases it.
        ensure!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "Another Koda window is managing tools. Wait, then retry."
        );
    }
    let mut environment = match environment_at(root) {
        Ok(environment) => environment,
        Err(error) if error.is::<serde_json::Error>() => {
            let backup = tempfile::Builder::new()
                .prefix("environment.corrupt-")
                .suffix(".json")
                .tempfile_in(&root)?
                .keep()?;
            fs::rename(root.join("environment.json"), &backup.1)?;
            Environment::default()
        }
        Err(error) => return Err(error),
    };
    match dependency {
        Dependency::Sdk => environment.sdk = Some(path),
        Dependency::Jdk => environment.jdk = Some(path),
        Dependency::AndroidCli => environment.android_cli = Some(path),
    }
    let destination = root.join("environment.json");
    ensure!(
        !fs::symlink_metadata(&destination).is_ok_and(|metadata| metadata.file_type().is_symlink()),
        "Environment settings must not be a symlink"
    );
    let mut temporary = tempfile::NamedTempFile::new_in(&root)?;
    serde_json::to_writer_pretty(&mut temporary, &environment)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(destination)?;
    Ok(())
}

pub fn command_environment() -> Result<BTreeMap<String, String>> {
    let selected = environment()?;
    let mut values = BTreeMap::new();
    let jdk = selected
        .jdk
        .map(Ok)
        .unwrap_or_else(|| match std::env::var_os("JAVA_HOME") {
            Some(path) => Ok(PathBuf::from(path)),
            None => super::kotlin::java_home(),
        });
    if let Ok(jdk) = jdk {
        values.insert("JAVA_HOME".into(), jdk.to_string_lossy().into_owned());
    }
    if let Some(sdk) = selected.sdk.or_else(super::sdk_root) {
        values.insert("ANDROID_HOME".into(), sdk.to_string_lossy().into_owned());
        values.insert(
            "ANDROID_SDK_ROOT".into(),
            sdk.to_string_lossy().into_owned(),
        );
    }
    Ok(values)
}

pub fn language_environment() -> Result<BTreeMap<String, String>> {
    let mut values = command_environment()?;
    if environment()?.jdk.is_none() {
        values.remove("JAVA_HOME");
    }
    Ok(values)
}

pub struct Prepared {
    pub directory: tempfile::TempDir,
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
}

pub fn prepare(tool: Tool, operation: &str, offline: bool) -> Result<Prepared> {
    ensure!(
        supported(),
        "Managed tool provisioning supports Apple Silicon macOS only"
    );
    ensure!(
        matches!(operation, "install" | "validate" | "rollback"),
        "Invalid tool operation"
    );
    let directory = tempfile::tempdir()?;
    for (name, bytes) in tool.recipes() {
        let destination = directory.path().join(name);
        fs::create_dir_all(
            destination
                .parent()
                .context("Recipe directory is missing")?,
        )?;
        fs::write(destination, bytes)?;
    }
    let program = python()?;
    let mut arguments = vec![
        directory
            .path()
            .join("script/manage-android-tools")
            .to_string_lossy()
            .into_owned(),
        operation.into(),
        tool.name().into(),
        root().to_string_lossy().into_owned(),
        tool.recipe(),
    ];
    if offline {
        arguments.push("--offline".into());
    }
    let mut environment = command_environment()?;
    if operation == "install" {
        let jdk = super::kotlin::java_home()?;
        executable(&jdk.join("bin/javac"))?;
        environment.insert("JAVA21_HOME".into(), jdk.to_string_lossy().into_owned());
    }
    environment.insert(
        "GRADLE_USER_HOME".into(),
        root().join("gradle").to_string_lossy().into_owned(),
    );
    Ok(Prepared {
        directory,
        program,
        arguments,
        environment,
    })
}

pub fn python() -> Result<PathBuf> {
    let candidates = [
        which::which("python3").ok(),
        which::which("python3.12").ok(),
        which::which("python3.13").ok(),
        which::which("python3.14").ok(),
        Some(PathBuf::from("/opt/homebrew/bin/python3")),
        Some(PathBuf::from("/usr/local/bin/python3")),
    ];
    for path in candidates
        .into_iter()
        .flatten()
        .filter(|path| path.is_file())
    {
        let mut child = match smol::process::Command::new(&path)
            .args(["-c", "import sys; sys.exit(sys.version_info < (3, 12))"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(_) => continue,
        };
        let mut finished = false;
        for _ in 0..20 {
            if let Some(status) = child.try_status()? {
                finished = true;
                if status.success() {
                    return Ok(path);
                }
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if !finished {
            child.kill()?;
            smol::block_on(child.status())?;
        }
    }
    anyhow::bail!("Install Python 3.12 or newer from python.org or Homebrew, then retry")
}

pub fn status() -> Vec<String> {
    let mut lines = vec![if supported() {
        "Managed runtimes: Apple Silicon macOS".into()
    } else {
        "Managed runtimes require Apple Silicon macOS".into()
    }];
    for (name, value) in [
        ("Python 3.12+", python()),
        ("JDK 21", super::kotlin::java_home()),
        ("SDK / adb", super::adb_path()),
        ("Android CLI", super::android_cli_path()),
    ] {
        lines.push(match value {
            Ok(path) => format!("{name}: {}", path.display()),
            Err(error) => format!("{name}: {error:#}"),
        });
    }
    for tool in Tool::ALL {
        lines.push(match resolve(tool) {
            Ok(path) => format!("{}: {}", tool.label(), path.display()),
            Err(error) => format!("{}: {error:#}", tool.label()),
        });
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Component;

    #[test]
    fn manifest_rejects_corruption_updates_and_escaped_paths() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path();
        let slot = "install-0123456789abcdef0123456789abcdef";
        let entrypoint = root.join(slot).join(Tool::Preview.entrypoint());
        fs::create_dir_all(entrypoint.parent().context("Missing parent")?)?;
        fs::write(&entrypoint, "bridge")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&entrypoint, fs::Permissions::from_mode(0o644))?;
        }
        let mut manifest = serde_json::json!({"schema":1, "tool":"preview", "recipe":Tool::Preview.recipe(), "slot":slot, "entrypoint":Tool::Preview.entrypoint(), "files":{Tool::Preview.entrypoint(): digest_file(&entrypoint)?}, "modes":{Tool::Preview.entrypoint(): 0o644}});
        let path = root.join("preview.json");
        fs::write(&path, manifest.to_string())?;
        assert_eq!(resolve_at(root, Tool::Preview)?, root.join(slot));
        fs::write(&entrypoint, "corrupt")?;
        assert!(resolve_at(root, Tool::Preview).is_err());
        fs::write(&entrypoint, "bridge")?;
        manifest["recipe"] = "old".into();
        fs::write(&path, manifest.to_string())?;
        assert!(resolve_at(root, Tool::Preview).is_err());
        manifest["recipe"] = Tool::Preview.recipe().into();
        manifest["slot"] = "../outside".into();
        fs::write(&path, manifest.to_string())?;
        assert!(resolve_at(root, Tool::Preview).is_err());
        Ok(())
    }

    #[test]
    fn recipes_are_embedded_and_identified_independently() {
        for tool in Tool::ALL {
            assert_eq!(tool.recipe().len(), 64);
            assert!(tool.recipes().iter().all(|(path, bytes)| {
                !bytes.is_empty()
                    && Path::new(path)
                        .components()
                        .all(|component| matches!(component, Component::Normal(_)))
            }));
        }
        assert_ne!(Tool::Kotlin.recipe(), Tool::Debugger.recipe());
    }

    #[test]
    fn dependency_choices_persist_and_repair_corrupt_settings() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("profile");
        fs::create_dir(&root)?;
        let cli = temporary.path().join(if cfg!(windows) {
            "android.exe"
        } else {
            "android"
        });
        fs::write(&cli, "cli")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&cli, fs::Permissions::from_mode(0o755))?;
        }
        fs::write(root.join("environment.json"), "{corrupt")?;
        save_dependency_at(&root, Dependency::AndroidCli, &cli)?;
        assert_eq!(
            environment_at(&root)?.android_cli.as_deref(),
            Some(cli.as_path())
        );
        assert!(
            fs::read_dir(&root)?
                .filter_map(|entry| entry.ok())
                .any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("environment.corrupt-"))
        );
        let sdk = temporary.path().join("sdk");
        let adb = sdk.join(if cfg!(windows) {
            "platform-tools/adb.exe"
        } else {
            "platform-tools/adb"
        });
        fs::create_dir_all(adb.parent().context("Missing SDK directory")?)?;
        fs::write(&adb, "adb")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&adb, fs::Permissions::from_mode(0o755))?;
        }
        save_dependency_at(&root, Dependency::Sdk, &sdk)?;
        let saved = environment_at(&root)?;
        assert_eq!(saved.sdk.as_deref(), Some(sdk.as_path()));
        assert_eq!(saved.android_cli.as_deref(), Some(cli.as_path()));
        Ok(())
    }

    #[test]
    fn cached_inventory_rejects_library_changes_and_added_files() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let directory = temporary.path();
        let library = directory.join("renderer.jar");
        fs::write(&library, "original")?;
        let files = BTreeMap::from([("renderer.jar".into(), digest_file(&library)?)]);
        validate_inventory(directory, &files)?;
        let modified = fs::metadata(&library)?.modified()?;
        fs::write(&library, "tampered")?;
        fs::File::open(&library)?.set_times(fs::FileTimes::new().set_modified(modified))?;
        assert!(validate_inventory(directory, &files).is_err());
        fs::write(&library, "original")?;
        validate_inventory(directory, &files)?;
        let replacement = directory.join("replacement");
        fs::write(&replacement, "tampered")?;
        fs::File::open(&replacement)?.set_times(fs::FileTimes::new().set_modified(modified))?;
        fs::rename(replacement, &library)?;
        assert!(validate_inventory(directory, &files).is_err());
        fs::write(&library, "original")?;
        validate_inventory(directory, &files)?;
        fs::write(directory.join("extra.jar"), "injected")?;
        assert!(validate_inventory(directory, &files).is_err());
        fs::remove_file(directory.join("extra.jar"))?;
        fs::remove_file(library)?;
        assert!(validate_inventory(directory, &files).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn persisted_language_server_paths_require_the_current_validated_runtime() -> Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let temporary = tempfile::tempdir()?;
        let root = &temporary.path().join("android-tools");
        fs::create_dir(root)?;
        let create = |slot: &str| -> Result<PathBuf> {
            let directory = root.join(slot);
            let names = [
                Tool::Kotlin.entrypoint(),
                "kotlin-server-263.4702.0/jbr/Contents/Home/bin/java",
            ];
            let mut files = BTreeMap::new();
            let mut modes = BTreeMap::new();
            for name in names {
                let binary = directory.join(name);
                fs::create_dir_all(binary.parent().context("Missing runtime directory")?)?;
                fs::write(&binary, "binary")?;
                fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))?;
                files.insert(name, digest_file(&binary)?);
                modes.insert(name, 0o755);
            }
            let manifest = serde_json::json!({"schema":1,"tool":"kotlin","recipe":Tool::Kotlin.recipe(),"slot":slot,"entrypoint":Tool::Kotlin.entrypoint(),"files":files,"modes":modes});
            fs::write(root.join("kotlin.json"), manifest.to_string())?;
            Ok(directory.join(Tool::Kotlin.entrypoint()))
        };
        let first = create("install-0123456789abcdef0123456789abcdef")?;
        validate_language_server_binary_at(root, &first)?;
        fs::write(&first, "corrupt")?;
        assert!(validate_language_server_binary_at(root, &first).is_err());
        fs::write(&first, "binary")?;
        let second = create("install-abcdef0123456789abcdef0123456789")?;
        validate_language_server_binary_at(root, &second)?;
        assert!(validate_language_server_binary_at(root, &first).is_err());
        let other_root = temporary.path().join("other-profile/android-tools");
        assert!(validate_language_server_binary_at(&other_root, &second).is_err());
        let alias = temporary.path().join("foreign-server-alias");
        std::os::unix::fs::symlink(&second, &alias)?;
        assert!(validate_language_server_binary_at(&other_root, &alias).is_err());
        validate_language_server_binary_at(root, Path::new("/custom/server"))?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn inventory_rejects_escaped_links_and_executable_permission_loss() -> Result<()> {
        use std::os::unix::{fs::PermissionsExt as _, fs::symlink};
        let temporary = tempfile::tempdir()?;
        let root = temporary.path();
        let directory = root.join("runtime");
        fs::create_dir(&directory)?;
        let binary = directory.join("java");
        fs::write(&binary, "java")?;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))?;
        let files = BTreeMap::from([("java".into(), digest_file(&binary)?)]);
        validate_inventory(&directory, &files)?;
        executable(&binary)?;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o644))?;
        assert!(executable(&binary).is_err());
        symlink(root.join("outside"), directory.join("link"))?;
        assert!(validate_inventory(&directory, &files).is_err());
        Ok(())
    }
}
