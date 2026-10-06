use crate::{kotlin, managed};
use anyhow::{Context as _, Result, bail, ensure};
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const MAX_DOWNLOAD: u64 = 1024 * 1024 * 1024;
const MAX_EXPANDED: u64 = 5 * 1024 * 1024 * 1024;
const MAX_STORAGE: u64 = 12 * 1024 * 1024 * 1024;
const MAX_FILES: usize = 100_000;
const RECIPE: &[u8] = include_bytes!("../provision-manifest.json");

#[derive(Clone, Debug)]
pub struct Discovery {
    pub jdk: Option<PathBuf>,
    pub sdk: Option<PathBuf>,
    pub android_cli: Option<PathBuf>,
    pub compile_sdk: Option<u32>,
    pub issues: Vec<String>,
    pub supported: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Options {
    pub jdk: Option<PathBuf>,
    pub sdk: Option<PathBuf>,
    pub android_cli: Option<PathBuf>,
    pub api_level: u32,
    pub install_sdk: bool,
    pub install_cli: bool,
    pub offline: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            jdk: None,
            sdk: None,
            android_cli: None,
            api_level: 36,
            install_sdk: true,
            install_cli: true,
            offline: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct License {
    pub id: String,
    pub text: String,
    pub sha256: String,
    pub source: String,
}

#[derive(Clone, Debug)]
pub struct LicenseAcceptance {
    pub id: String,
    pub sha256: String,
    pub plan_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetupPlan {
    pub id: String,
    pub jdk_version: String,
    pub packages: Vec<String>,
    pub download_bytes: u64,
    pub licenses: Vec<License>,
    pub installation_directory: PathBuf,
    pub provenance: Vec<String>,
    pub jdk: PathBuf,
    pub sdk: Option<PathBuf>,
    pub android_cli: Option<PathBuf>,
    pub downloads: Vec<Download>,
    pub supported_platform: String,
    slot: String,
    options: Options,
    artifacts: Vec<Artifact>,
    environment_digest: String,
    recipe: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Download {
    pub label: String,
    pub version: String,
    pub publisher: String,
    pub url: String,
    pub bytes: u64,
}

#[derive(Clone, Debug)]
pub struct Progress {
    pub finishing: bool,
    pub message: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Installed {
    pub jdk: PathBuf,
    pub sdk: Option<PathBuf>,
    pub android_cli: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    host: String,
    name: String,
    url: String,
    sha256: String,
    sha1: Option<String>,
    bytes: u64,
    format: String,
    destination: String,
    license_id: Option<String>,
    revision: Option<String>,
    #[serde(default)]
    sdk_api_level: Option<String>,
    #[serde(default)]
    extension_level: Option<u32>,
    #[serde(default)]
    base_extension: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recipe {
    schema: u32,
    java_version: String,
    jdk: Vec<Artifact>,
    sdk_metadata_url: String,
    sdk_metadata_sha256: String,
    sdk: Vec<Artifact>,
    cli: Vec<Artifact>,
    licenses: Vec<License>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Generation {
    schema: u32,
    recipe: String,
    installed: Installed,
    files: BTreeMap<String, String>,
    modes: BTreeMap<String, u32>,
    artifacts: Vec<Artifact>,
    accepted_licenses: Vec<License>,
    api_level: u32,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Active {
    schema: u32,
    slot: String,
    previous: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    environment: managed::Environment,
    active: Option<Active>,
}

pub fn root() -> PathBuf {
    managed::root().join("provision")
}

pub fn platform_label() -> &'static str {
    "Java 21 and Android SDK: macOS (Apple Silicon / Intel), Linux x86_64, Windows x86_64"
}

pub fn supported() -> bool {
    host().is_some()
}

fn host() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("mac-aarch64")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("mac-x86_64")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("linux-x86_64")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("windows-x86_64")
    } else {
        None
    }
}

fn recipe() -> Result<Recipe> {
    let recipe: Recipe = serde_json::from_slice(RECIPE)?;
    ensure!(recipe.schema == 1, "Unsupported provisioning recipe");
    ensure!(
        valid_digest(&recipe.sdk_metadata_sha256),
        "Invalid SDK metadata provenance digest"
    );
    ensure!(
        recipe.sdk_metadata_url == "https://dl.google.com/android/repository/repository2-3.xml",
        "Unexpected SDK metadata origin"
    );
    Ok(recipe)
}

fn recipe_digest() -> String {
    digest(RECIPE)
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn cancelled(cancel: &AtomicBool) -> Result<()> {
    ensure!(
        !cancel.load(Ordering::Relaxed),
        "Setup cancelled. Verified downloads are retained for retry."
    );
    Ok(())
}

pub fn discover(project: Option<&Path>) -> Result<Discovery> {
    let mut issues = Vec::new();
    let selected = match managed::environment() {
        Ok(environment) => environment,
        Err(error) => {
            issues.push(format!("Saved tool paths: {error:#}"));
            managed::Environment::default()
        }
    };
    let jdk = match kotlin::java_home() {
        Ok(path) => Some(path),
        Err(error) => {
            issues.push(format!("Java: {error:#}"));
            None
        }
    };
    let compile_sdk = project.and_then(compile_sdk_hint);
    let project_sdk = match project.map(project_sdk).transpose() {
        Ok(sdk) => sdk.flatten(),
        Err(error) => {
            issues.push(format!("Project SDK: {error:#}"));
            None
        }
    };
    if let (Some(selected), Some(project_sdk)) = (&selected.sdk, &project_sdk) {
        if selected.canonicalize().as_ref().ok() != Some(project_sdk) {
            issues.push("The selected SDK differs from sdk.dir in project local.properties. Gradle uses that project setting; update it explicitly if you want to use the selected SDK.".into());
        }
    }
    let sdk = selected
        .sdk
        .or(project_sdk)
        .or_else(crate::sdk_root)
        .filter(|sdk| {
            match validate_managed_path(sdk).and_then(|()| match compile_sdk {
                Some(api) => validate_sdk(sdk, api),
                None => validate_sdk(sdk, existing_sdk_api(sdk)?),
            }) {
                Ok(()) => true,
                Err(error) => {
                    issues.push(format!("Android SDK: {error:#}"));
                    false
                }
            }
        });
    let android_cli = crate::android_cli_path().ok();
    if sdk.is_none() {
        issues.push("No Android SDK found. Choose an existing SDK or install the selected packages in Koda storage.".into());
    }
    if !supported() {
        issues.push(format!("Automatic installation is unavailable on this platform. {}. Select existing tools instead.", platform_label()));
    }
    if root().join("journal.json").exists() {
        issues.push("A previous setup was interrupted. Retry setup to recover the previous selection before installing.".into());
    }
    Ok(Discovery {
        jdk,
        sdk,
        android_cli,
        compile_sdk,
        issues,
        supported: supported(),
    })
}

fn decode_property(value: &str) -> Result<String> {
    let mut characters = value.chars();
    let mut units = Vec::new();
    while let Some(character) = characters.next() {
        let character = if character == '\\' {
            match characters
                .next()
                .context("The sdk.dir property ends with an incomplete escape")?
            {
                'u' => {
                    let digits = characters.by_ref().take(4).collect::<String>();
                    ensure!(
                        digits.len() == 4,
                        "The sdk.dir property has an incomplete Unicode escape"
                    );
                    units.push(
                        u16::from_str_radix(&digits, 16)
                            .context("The sdk.dir property has an invalid Unicode escape")?,
                    );
                    continue;
                }
                't' => '\t',
                'r' => '\r',
                'n' => '\n',
                'f' => '\u{000c}',
                character => character,
            }
        } else {
            character
        };
        let mut buffer = [0; 2];
        units.extend_from_slice(character.encode_utf16(&mut buffer));
    }
    String::from_utf16(&units).context("The sdk.dir property contains invalid Unicode")
}

fn sdk_property(text: &str) -> Result<Option<String>> {
    let mut logical = String::new();
    let mut selected = None;
    for line in text.lines().chain(std::iter::once("")) {
        let line = if logical.is_empty() {
            line.trim_start()
        } else {
            line.trim_start_matches([' ', '\t', '\u{000c}'])
        };
        if logical.is_empty() && line.starts_with(['#', '!']) {
            continue;
        }
        logical.push_str(line);
        let continuation = logical
            .as_bytes()
            .iter()
            .rev()
            .take_while(|byte| **byte == b'\\')
            .count()
            % 2
            == 1;
        if continuation {
            logical.pop();
            continue;
        }
        if !logical.starts_with(['#', '!']) && !logical.is_empty() {
            let mut escaped = false;
            let mut boundary = None;
            for (offset, character) in logical.char_indices() {
                if !escaped && (matches!(character, '=' | ':') || character.is_whitespace()) {
                    boundary = Some(offset);
                    break;
                }
                escaped = character == '\\' && !escaped;
            }
            let (key, value) = boundary
                .map(|offset| logical.split_at(offset))
                .unwrap_or((&logical, ""));
            if decode_property(key)? == "sdk.dir" {
                let value = value.trim_start();
                let value = value.strip_prefix(['=', ':']).unwrap_or(value).trim_start();
                selected = Some(decode_property(value)?);
            }
        }
        logical.clear();
    }
    Ok(selected)
}

fn project_sdk(project: &Path) -> Result<Option<PathBuf>> {
    let path = project.join("local.properties");
    let bytes = match regular_file(&path, 64 * 1024) {
        Ok(bytes) => bytes,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(error) => {
            return Err(error)
                .context("Could not inspect project local.properties for an existing SDK");
        }
    };
    let text = std::str::from_utf8(&bytes)
        .context("Project local.properties must use UTF-8 for SDK discovery")?;
    let Some(value) = sdk_property(text)? else {
        return Ok(None);
    };
    ensure!(
        !value.is_empty() && !value.contains(['\0', '\n', '\r']),
        "The project's sdk.dir must contain a single existing SDK path"
    );
    let sdk = PathBuf::from(value);
    let sdk = if sdk.is_absolute() {
        sdk
    } else {
        project.join(sdk)
    };
    Ok(Some(sdk.canonicalize().context("The project's sdk.dir points to an unavailable SDK. Choose an existing SDK and update local.properties explicitly.")?))
}

// This is a hint only: evaluating a build script before project trust would execute arbitrary code.
fn compile_sdk_hint(project: &Path) -> Option<u32> {
    let pattern = regex::Regex::new(
        r"(?m)^\s*compileSdk(?:Version)?\s*(?:=\s*|\(\s*|\s+)([0-9]{2})(?:\s|\)|$)",
    )
    .ok()?;
    for relative in [
        "build.gradle.kts",
        "build.gradle",
        "app/build.gradle.kts",
        "app/build.gradle",
        "mobile/build.gradle.kts",
        "mobile/build.gradle",
    ] {
        let path = project.join(relative);
        let metadata = fs::symlink_metadata(&path).ok();
        if !metadata.is_some_and(|metadata| {
            metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 256 * 1024
        }) {
            continue;
        }
        let text = fs::read_to_string(path).ok()?;
        if let Some(value) = pattern
            .captures(&text)
            .and_then(|captures| captures.get(1))
            .and_then(|value| value.as_str().parse().ok())
        {
            return Some(value);
        }
    }
    None
}

fn sdk_platform(api_level: u32) -> Result<&'static str> {
    match api_level {
        36 => Ok("platforms;android-36"),
        37 => Ok("platforms;android-37.0"),
        _ => bail!(
            "Choose a supported pinned Android platform: API 36 or 37.0. Other versions can be selected from an existing SDK."
        ),
    }
}

fn sdk_packages(api_level: u32) -> Result<Vec<String>> {
    Ok(vec![
        "platform-tools".into(),
        sdk_platform(api_level)?.into(),
        format!("build-tools;{}.0.0", api_level),
    ])
}

fn sdk_layout(path: &Path, api_level: u32) -> Result<(String, String)> {
    ensure!(
        path.is_absolute() && path.is_dir(),
        "Choose an existing absolute Android SDK directory"
    );
    ensure!(
        (1..=100).contains(&api_level),
        "Choose a valid Android API level"
    );
    managed::executable(&path.join(if cfg!(windows) {
        "platform-tools/adb.exe"
    } else {
        "platform-tools/adb"
    }))
    .context("The SDK is missing executable platform-tools/adb")?;
    let names = if api_level == 37 {
        vec!["android-37.0".to_owned(), "android-37".to_owned()]
    } else {
        vec![
            format!("android-{api_level}"),
            format!("android-{api_level}.0"),
        ]
    };
    let platform = names.into_iter().find(|name| path.join("platforms").join(name).join("android.jar").is_file()).with_context(|| format!("The SDK is missing Android platform {api_level}. Choose another SDK, or install a supported platform in Koda storage."))?;
    let mut versions = Vec::new();
    let mut inspected = 0usize;
    for entry in fs::read_dir(path.join("build-tools")).context("The SDK is missing build-tools")? {
        let entry = entry?;
        inspected += 1;
        ensure!(
            inspected <= 1000,
            "The SDK contains too many build-tools versions"
        );
        let name = entry
            .file_name()
            .to_str()
            .context("SDK build-tools version must use Unicode")?
            .to_owned();
        if !entry.path().is_dir() {
            continue;
        }
        let numbers = name
            .split('.')
            .map(str::parse::<u32>)
            .collect::<std::result::Result<Vec<_>, _>>();
        if let Ok(numbers) = numbers {
            if numbers.len() == 3 {
                versions.push((numbers, name));
            }
        }
    }
    versions.sort_by(|left, right| right.0.cmp(&left.0));
    let build_tools = versions.into_iter().map(|(_, name)| name).find(|name| {
        let directory = path.join("build-tools").join(name);
        managed::executable(&directory.join(if cfg!(windows) { "aapt2.exe" } else { "aapt2" })).is_ok() && directory.join("lib/d8.jar").is_file()
    }).context("The SDK is missing complete build-tools (executable aapt2 and lib/d8.jar). Choose another SDK or install tools in Koda storage.")?;
    Ok((platform, build_tools))
}

pub fn validate_sdk(path: &Path, api_level: u32) -> Result<()> {
    sdk_layout(path, api_level).map(|_| ())
}

pub fn plan(options: Options, cancel: &AtomicBool) -> Result<SetupPlan> {
    plan_at(&root(), options, cancel)
}

pub fn plan_at(root: &Path, mut options: Options, cancel: &AtomicBool) -> Result<SetupPlan> {
    cancelled(cancel)?;
    ensure!(
        root.is_absolute(),
        "Managed storage must be an absolute directory"
    );
    let recipe = recipe()?;
    let mut artifacts = Vec::new();
    if let Some(jdk) = &options.jdk {
        validate_managed_path(jdk)?;
        kotlin::validate_jdk_21(jdk)?;
        options.jdk = Some(jdk.canonicalize()?);
    } else {
        let host = host().context("Automatic Java installation is unavailable on this platform. Select an existing full JDK 21.")?;
        artifacts.push(
            recipe
                .jdk
                .iter()
                .find(|artifact| artifact.host == host)
                .context("No pinned Java archive for this platform")?
                .clone(),
        );
    }
    let packages = if options.install_sdk {
        if let Some(sdk) = &options.sdk {
            let (platform, build_tools) = sdk_layout(sdk, options.api_level)?;
            vec![
                "platform-tools".into(),
                format!("platforms;{platform}"),
                format!("build-tools;{build_tools}"),
            ]
        } else {
            sdk_packages(options.api_level)?
        }
    } else {
        Vec::new()
    };
    if options.install_sdk {
        if let Some(sdk) = &options.sdk {
            validate_managed_path(sdk)?;
            validate_sdk(sdk, options.api_level)?;
            options.sdk = Some(sdk.canonicalize()?);
        } else {
            let host = host().context("Automatic SDK installation is unavailable on this platform. Choose an existing SDK.")?;
            for package in &packages {
                artifacts.push(
                    recipe
                        .sdk
                        .iter()
                        .find(|artifact| &artifact.name == package && artifact.host == host)
                        .with_context(|| format!("No pinned SDK archive for {package} on {host}"))?
                        .clone(),
                );
            }
        }
    } else {
        options.sdk = None;
    }
    if options.install_cli {
        ensure!(
            options.install_sdk,
            "The Android CLI needs an SDK. Select SDK setup before installing the CLI."
        );
        if let Some(cli) = &options.android_cli {
            validate_managed_path(cli)?;
            managed::executable(cli)?;
            options.android_cli = Some(cli.canonicalize()?);
        } else {
            let host = host().context("Automatic Android CLI installation is unavailable on this platform. Choose an existing Android CLI executable.")?;
            artifacts.push(
                recipe
                    .cli
                    .iter()
                    .find(|artifact| artifact.host == host)
                    .context("No pinned Android CLI for this platform")?
                    .clone(),
            );
        }
    } else {
        options.android_cli = None;
    }
    let mut download_bytes = 0;
    for artifact in &artifacts {
        ensure!(
            valid_digest(&artifact.sha256),
            "The {} archive has no verified SHA-256 pin. Select an existing installation.",
            artifact.name
        );
        ensure!(
            artifact.bytes > 0 && artifact.bytes <= MAX_DOWNLOAD,
            "Download exceeds the per-archive limit"
        );
        checked_url(&artifact.url)?;
        ensure!(
            matches!(artifact.format.as_str(), "zip" | "tar.gz" | "executable"),
            "Unsupported archive format"
        );
        safe_relative(Path::new(&artifact.destination))?;
        download_bytes += artifact.bytes;
    }
    ensure!(
        download_bytes <= 2 * MAX_DOWNLOAD,
        "The selected downloads exceed the setup limit"
    );
    let license_ids = artifacts
        .iter()
        .filter_map(|artifact| artifact.license_id.as_ref())
        .collect::<BTreeSet<_>>();
    let licenses = recipe
        .licenses
        .into_iter()
        .filter(|license| license_ids.contains(&license.id))
        .collect::<Vec<_>>();
    ensure!(
        licenses.len() == license_ids.len(),
        "SDK license information is missing"
    );
    for license in &licenses {
        ensure!(
            digest(license.text.as_bytes()) == license.sha256,
            "SDK license integrity check failed"
        );
    }
    let profile = root
        .parent()
        .context("Managed storage has no profile directory")?;
    let environment_digest = environment_fingerprint(profile)?;
    let slot = format!("generation-{}", uuid::Uuid::new_v4().simple());
    let jdk = options.jdk.clone().unwrap_or_else(|| {
        root.join(&slot).join(if cfg!(target_os = "macos") {
            "jdk/Contents/Home"
        } else {
            "jdk"
        })
    });
    let sdk = options.install_sdk.then(|| {
        options
            .sdk
            .clone()
            .unwrap_or_else(|| root.join(&slot).join("sdk"))
    });
    let android_cli = options.install_cli.then(|| {
        options.android_cli.clone().unwrap_or_else(|| {
            root.join(&slot).join(if cfg!(windows) {
                "android-cli.exe"
            } else {
                "android-cli"
            })
        })
    });
    let downloads = artifacts
        .iter()
        .map(|artifact| Download {
            label: artifact.name.clone(),
            version: artifact
                .revision
                .clone()
                .unwrap_or_else(|| recipe.java_version.clone()),
            publisher: if artifact.destination == "jdk" {
                "Eclipse Adoptium".into()
            } else {
                "Google".into()
            },
            url: artifact.url.clone(),
            bytes: artifact.bytes,
        })
        .collect();
    let mut plan = SetupPlan {
        id: String::new(),
        jdk_version: recipe.java_version,
        packages,
        download_bytes,
        licenses,
        installation_directory: root.to_path_buf(),
        provenance: vec![
            "Eclipse Adoptium / Temurin: publisher SHA-256 pinned per Koda version".into(),
            format!(
                "Google Android repository: {} (metadata SHA-256 {}); archive SHA-256 pinned after matching Google's published SHA-1",
                recipe.sdk_metadata_url, recipe.sdk_metadata_sha256
            ),
        ],
        jdk,
        sdk,
        android_cli,
        downloads,
        supported_platform: platform_label().into(),
        slot,
        options,
        artifacts,
        environment_digest,
        recipe: recipe_digest(),
    };
    plan.id = digest(&serde_json::to_vec(&plan)?);
    if plan.options.offline {
        for artifact in &plan.artifacts {
            verify_download(&download_path(root, artifact), artifact).with_context(|| {
                format!(
                    "Offline setup needs the verified cached {} archive. Connect once and retry.",
                    artifact.name
                )
            })?;
        }
    }
    Ok(plan)
}

fn checked_url(value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value)?;
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.port_or_known_default() == Some(443),
        "Downloads require an uncredentialed HTTPS URL"
    );
    ensure!(
        matches!(
            url.host_str(),
            Some(
                "github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
                    | "dl.google.com"
            )
        ),
        "The download origin is not allowed"
    );
    Ok(url)
}

fn safe_relative(path: &Path) -> Result<()> {
    ensure!(
        !path.as_os_str().is_empty()
            && path
                .to_str()
                .is_some_and(|path| !path.contains('\\') && !path.contains(':')),
        "Invalid archive path"
    );
    ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_))),
        "An archive path escaped the installation"
    );
    Ok(())
}

fn regular_file(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= maximum,
        "{} must be a bounded regular file",
        path.display()
    );
    Ok(fs::read(path)?)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_slice(&regular_file(path, 32 * 1024 * 1024)?)
        .context("The managed setup manifest is invalid. Retry setup or choose existing tools.")
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    ensure!(
        !fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()),
        "Managed settings must not be a symlink"
    );
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().context("Settings have no parent")?)?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    #[cfg(unix)]
    fs::File::open(path.parent().context("Settings have no parent")?)?.sync_all()?;
    Ok(())
}

fn environment_fingerprint(profile: &Path) -> Result<String> {
    let path = profile.join("environment.json");
    match regular_file(&path, 64 * 1024) {
        Ok(bytes) => {
            let _: managed::Environment = serde_json::from_slice(&bytes).context("Saved tool paths are corrupt. Repair the saved selections before installation; the original file has been preserved.")?;
            Ok(digest(&bytes))
        }
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(digest(b"missing"))
        }
        Err(error) => Err(error),
    }
}

fn profile_lock(root: &Path) -> Result<fs::File> {
    let profile = root.parent().context("Managed storage has no profile")?;
    fs::create_dir_all(profile)?;
    kotlin::ensure_directory(profile)?;
    kotlin::ensure_directory(root)?;
    let path = profile.join(".lock");
    ensure!(
        !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()),
        "Unsafe managed tool lock"
    );
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.try_lock_exclusive()
        .context("Another Koda window is managing tools. Wait for it to finish, then retry.")?;
    Ok(file)
}

fn download_path(root: &Path, artifact: &Artifact) -> PathBuf {
    root.join("downloads")
        .join(format!("{}.archive", artifact.sha256))
}

fn verify_download(path: &Path, artifact: &Artifact) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.len() == artifact.bytes,
        "The cached archive size is incorrect"
    );
    let mut file = fs::File::open(path)?;
    let mut sha256 = Sha256::new();
    let mut sha1 = sha1::Sha1::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        sha256.update(&buffer[..count]);
        sha1.update(&buffer[..count]);
    }
    ensure!(
        format!("{:x}", sha256.finalize()) == artifact.sha256,
        "The download SHA-256 integrity check failed. Delete the corrupt cached archive and retry online."
    );
    if let Some(expected) = &artifact.sha1 {
        ensure!(
            format!("{:x}", sha1.finalize()) == *expected,
            "The Google repository archive checksum did not match"
        );
    }
    Ok(())
}

fn download_client_builder() -> reqwest::blocking::ClientBuilder {
    // This fork polls blocking response bodies outside Tokio. Its async read_timeout
    // creates a Tokio timer lazily and panics on GPUI workers; blocking wait timeouts do not.
    reqwest::blocking::Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("Too many download redirects")
            } else if checked_url(attempt.url().as_str()).is_err() {
                attempt.error("The download redirected to an unapproved origin")
            } else {
                attempt.follow()
            }
        }))
}

fn download(
    root: &Path,
    artifact: &Artifact,
    offline: bool,
    cancel: &AtomicBool,
    progress: &impl Fn(Progress),
    preceding: u64,
    total: u64,
) -> Result<PathBuf> {
    let path = download_path(root, artifact);
    if verify_download(&path, artifact).is_ok() {
        progress(Progress {
            finishing: false,
            message: format!("Using verified cached {}", artifact.name),
            downloaded_bytes: preceding + artifact.bytes,
            total_bytes: total,
        });
        return Ok(path);
    }
    ensure!(
        !offline,
        "Offline setup needs the verified {} archive. Connect and retry.",
        artifact.name
    );
    ensure!(
        !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()),
        "The download cache must not contain symlinks"
    );
    let client = download_client_builder().build()?;
    let mut last_error = None;
    for attempt in 0..2 {
        cancelled(cancel)?;
        let result = (|| -> Result<()> {
            let started = Instant::now();
            progress(Progress {
                finishing: false,
                message: format!("Downloading {} (attempt {})", artifact.name, attempt + 1),
                downloaded_bytes: preceding,
                total_bytes: total,
            });
            let mut response = client
                .get(checked_url(&artifact.url)?)
                .send()?
                .error_for_status()?;
            if let Some(size) = response.content_length() {
                ensure!(
                    size == artifact.bytes,
                    "The download's declared size changed. Update Koda or retry."
                );
            }
            let mut temporary = tempfile::NamedTempFile::new_in(root.join("downloads"))?;
            let mut downloaded = 0u64;
            let mut buffer = [0; 64 * 1024];
            let mut last_progress = Instant::now();
            loop {
                cancelled(cancel)?;
                ensure!(
                    started.elapsed() < Duration::from_secs(180),
                    "The download exceeded its three-minute attempt limit. Retry when the connection improves."
                );
                let count = response.read(&mut buffer)?;
                cancelled(cancel)?;
                ensure!(
                    started.elapsed() < Duration::from_secs(180),
                    "The download exceeded its three-minute attempt limit. Retry when the connection improves."
                );
                if count == 0 {
                    break;
                }
                downloaded = downloaded
                    .checked_add(count as u64)
                    .context("Download size overflow")?;
                ensure!(
                    downloaded <= artifact.bytes && downloaded <= MAX_DOWNLOAD,
                    "The download exceeds its pinned size"
                );
                temporary.write_all(&buffer[..count])?;
                if last_progress.elapsed() >= Duration::from_millis(100) {
                    progress(Progress {
                        finishing: false,
                        message: format!("Downloading {}", artifact.name),
                        downloaded_bytes: preceding + downloaded,
                        total_bytes: total,
                    });
                    last_progress = Instant::now();
                }
            }
            temporary.as_file().sync_all()?;
            verify_download(temporary.path(), artifact)?;
            cancelled(cancel)?;
            temporary.persist(&path)?;
            Ok(())
        })();
        match result {
            Ok(()) => return Ok(path),
            Err(error) => {
                cancelled(cancel)?;
                last_error = Some(error);
            }
        }
    }
    Err(last_error.context("The download could not be started")?).with_context(|| format!("Could not download {}. Check your connection and storage, then retry. Verified completed downloads are retained.", artifact.name))
}

fn storage_budget(root: &Path, planned: u64) -> Result<()> {
    let mut total = 0u64;
    let mut count = 0usize;
    let mut pending = vec![root.to_path_buf()];
    while let Some(parent) = pending.pop() {
        for entry in fs::read_dir(parent)? {
            let entry = entry?;
            let metadata = entry.file_type()?;
            count += 1;
            ensure!(
                count <= MAX_FILES * 3,
                "Managed storage contains too many files. Remove an unused installation before retrying."
            );
            if metadata.is_dir() {
                pending.push(entry.path());
            } else {
                total = total
                    .checked_add(fs::symlink_metadata(entry.path())?.len())
                    .context("Managed storage size overflow")?;
            }
            ensure!(
                total.saturating_add(planned) <= MAX_STORAGE,
                "Managed tools exceed the 12 GiB storage budget. Remove an unused installation before retrying."
            );
        }
    }
    ensure_free_space(
        planned.saturating_add(128 * 1024 * 1024),
        fs2::available_space(root)?,
    )?;
    Ok(())
}

fn ensure_free_space(required: u64, available: u64) -> Result<()> {
    const MIB: u64 = 1024 * 1024;
    ensure!(
        available >= required,
        "Setup reserves {} MiB of free space for downloads, extraction, and recovery; {} MiB is available. Free disk space or select fewer components, then retry.",
        required.div_ceil(MIB),
        available / MIB
    );
    Ok(())
}

fn archive_relative(path: &Path) -> Result<PathBuf> {
    safe_relative(path)?;
    let relative = path.components().skip(1).collect::<PathBuf>();
    if !relative.as_os_str().is_empty() {
        safe_relative(&relative)?;
    }
    Ok(relative)
}

fn file_permissions(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        // Archives must not introduce setuid, setgid or world-writable executables.
        let mode = if mode & 0o111 != 0 { 0o755 } else { 0o644 };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

fn parent_directories(directory: &Path, relative: &Path) -> Result<()> {
    let mut parent = directory.to_path_buf();
    if let Some(components) = relative.parent() {
        for component in components.components() {
            parent.push(component.as_os_str());
            kotlin::ensure_directory(&parent)?;
        }
    }
    Ok(())
}

fn copy_bounded(
    input: &mut impl Read,
    file: &mut fs::File,
    cancel: &AtomicBool,
    expanded: &mut u64,
) -> Result<()> {
    let mut buffer = [0; 64 * 1024];
    loop {
        cancelled(cancel)?;
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        *expanded = expanded
            .checked_add(count as u64)
            .context("Archive size overflow")?;
        ensure!(
            *expanded <= MAX_EXPANDED,
            "The expanded archives exceed the 5 GiB setup limit"
        );
        file.write_all(&buffer[..count])?;
    }
    Ok(())
}

fn extract(
    path: &Path,
    artifact: &Artifact,
    destination: &Path,
    cancel: &AtomicBool,
    expanded: &mut u64,
    entries: &mut usize,
) -> Result<()> {
    kotlin::ensure_directory(destination)?;
    if artifact.format == "zip" {
        let mut archive = zip::ZipArchive::new(fs::File::open(path)?)?;
        ensure!(
            archive.len() <= MAX_FILES,
            "The archive contains too many entries"
        );
        for index in 0..archive.len() {
            cancelled(cancel)?;
            *entries += 1;
            ensure!(
                *entries <= MAX_FILES,
                "Setup contains too many archive entries"
            );
            let mut entry = archive.by_index(index)?;
            let relative = archive_relative(Path::new(entry.name()))?;
            if relative.as_os_str().is_empty() {
                ensure!(entry.is_dir(), "An archive file lacks its root directory");
                continue;
            }
            let mode = entry.unix_mode().unwrap_or(0o644);
            ensure!(
                mode & 0o170000 == 0 || matches!(mode & 0o170000, 0o100000 | 0o040000),
                "The ZIP archive contains an unsupported link or special file"
            );
            ensure!(
                entry.size() <= MAX_EXPANDED.saturating_sub(*expanded),
                "The ZIP archive exceeds the expanded-size limit"
            );
            parent_directories(destination, &relative)?;
            let target = destination.join(&relative);
            if entry.is_dir() {
                kotlin::ensure_directory(&target)?;
            } else {
                let mut file = fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&target)?;
                copy_bounded(&mut entry, &mut file, cancel, expanded)?;
                file_permissions(&target, mode)?;
                file.sync_all()?;
            }
        }
    } else if artifact.format == "tar.gz" {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(fs::File::open(path)?));
        let mut links = Vec::new();
        for entry in archive.entries()? {
            cancelled(cancel)?;
            *entries += 1;
            ensure!(
                *entries <= MAX_FILES,
                "Setup contains too many archive entries"
            );
            let mut entry = entry?;
            let relative = archive_relative(&entry.path()?)?;
            let entry_type = entry.header().entry_type();
            if relative.as_os_str().is_empty() {
                ensure!(
                    entry_type.is_dir(),
                    "An archive file lacks its root directory"
                );
                continue;
            }
            parent_directories(destination, &relative)?;
            let target = destination.join(&relative);
            if entry_type.is_dir() {
                kotlin::ensure_directory(&target)?;
            } else if entry_type.is_file() {
                ensure!(
                    entry.size() <= MAX_EXPANDED.saturating_sub(*expanded),
                    "The tar archive exceeds the expanded-size limit"
                );
                let mode = entry.header().mode()?;
                let mut file = fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&target)?;
                copy_bounded(&mut entry, &mut file, cancel, expanded)?;
                file_permissions(&target, mode)?;
                file.sync_all()?;
            } else if entry_type.is_symlink() {
                let link = entry
                    .link_name()?
                    .context("Archive link is missing its target")?
                    .into_owned();
                ensure!(
                    !link.is_absolute()
                        && link
                            .to_str()
                            .is_some_and(|link| !link.contains('\\') && !link.contains(':')),
                    "An archive symlink escaped the installation"
                );
                let mut parts = relative
                    .parent()
                    .context("Archive link lacks a parent")?
                    .components()
                    .map(|part| part.as_os_str().to_owned())
                    .collect::<Vec<_>>();
                for part in link.components() {
                    match part {
                        Component::Normal(part) => parts.push(part.to_owned()),
                        Component::CurDir => {}
                        Component::ParentDir => {
                            ensure!(
                                parts.pop().is_some(),
                                "An archive symlink escaped the installation"
                            );
                        }
                        _ => bail!("Invalid archive symlink"),
                    }
                }
                ensure!(
                    !parts.is_empty(),
                    "Archive links cannot target the installation root"
                );
                links.push((relative, link));
            } else {
                bail!("The tar archive contains an unsupported hard link or special file");
            }
        }
        for (relative, link) in &links {
            cancelled(cancel)?;
            let target = destination.join(&relative);
            #[cfg(unix)]
            std::os::unix::fs::symlink(&link, &target)?;
            #[cfg(not(unix))]
            bail!("This archive requires symlinks that are unsupported on this platform");
        }
        for (relative, _) in &links {
            let target = destination.join(relative);
            ensure!(
                target
                    .canonicalize()?
                    .starts_with(destination.canonicalize()?),
                "An archive link resolved outside the installation"
            );
        }
    } else {
        bail!("Unsupported archive format");
    }
    Ok(())
}

fn inventory(
    directory: &Path,
    cancel: &AtomicBool,
) -> Result<(BTreeMap<String, String>, BTreeMap<String, u32>)> {
    let mut files = BTreeMap::new();
    let mut modes = BTreeMap::new();
    let mut pending = vec![directory.to_path_buf()];
    let canonical = directory.canonicalize()?;
    while let Some(parent) = pending.pop() {
        for entry in fs::read_dir(parent)? {
            cancelled(cancel)?;
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            ensure!(
                files.len() < MAX_FILES,
                "Runtime inventory contains too many files"
            );
            let name = path
                .strip_prefix(directory)?
                .to_str()
                .context("Archive paths must use Unicode")?
                .replace(std::path::MAIN_SEPARATOR, "/");
            if metadata.file_type().is_symlink() {
                ensure!(
                    path.canonicalize()?.starts_with(&canonical),
                    "An installed link escaped the installation"
                );
                files.insert(
                    name.clone(),
                    format!(
                        "link:{}",
                        fs::read_link(&path)?
                            .to_str()
                            .context("Archive link target must use Unicode")?
                    ),
                );
            } else {
                ensure!(
                    metadata.is_file(),
                    "The installation contains an unsupported file type"
                );
                let mut file = fs::File::open(&path)?;
                let mut digest = Sha256::new();
                let mut buffer = [0; 64 * 1024];
                loop {
                    cancelled(cancel)?;
                    let count = file.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    digest.update(&buffer[..count]);
                }
                files.insert(name.clone(), format!("{:x}", digest.finalize()));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                modes.insert(name, metadata.permissions().mode() & 0o7777);
            }
            #[cfg(not(unix))]
            modes.insert(name, 0);
        }
    }
    Ok((files, modes))
}

fn local_package(directory: &Path, artifact: &Artifact, license: &License) -> Result<()> {
    let revision = artifact
        .revision
        .as_ref()
        .context("SDK package revision is missing")?;
    let mut revision = revision.split('.');
    let major = revision
        .next()
        .context("SDK revision is missing its major version")?;
    let minor = revision.next().unwrap_or("0");
    let micro = revision.next().unwrap_or("0");
    ensure!(
        [major, minor, micro]
            .iter()
            .all(|part| part.bytes().all(|byte| byte.is_ascii_digit())),
        "Invalid SDK revision"
    );
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    };
    let details = if let Some(api) = artifact.name.strip_prefix("platforms;android-") {
        ensure!(
            matches!(api, "36" | "37.0"),
            "Unsupported platform metadata"
        );
        ensure!(
            artifact.sdk_api_level.as_deref() == Some(api),
            "SDK API metadata differs from its package"
        );
        let extension = artifact
            .extension_level
            .map(|value| format!("<extension-level>{value}</extension-level>"))
            .unwrap_or_default();
        let base = artifact
            .base_extension
            .map(|value| format!("<base-extension>{value}</base-extension>"))
            .unwrap_or_default();
        format!(
            "<type-details xsi:type=\"sdk:platformDetailsType\"><api-level>{api}</api-level>{extension}{base}<layoutlib api=\"15\"/></type-details>"
        )
    } else {
        "<type-details xsi:type=\"ns3:genericDetailsType\"/>".into()
    };
    let package = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ns2:repository xmlns:ns2=\"http://schemas.android.com/repository/android/common/02\" xmlns:ns3=\"http://schemas.android.com/repository/android/generic/02\" xmlns:sdk=\"http://schemas.android.com/sdk/android/repo/repository2/03\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"><license id=\"{}\" type=\"text\">{}</license><localPackage path=\"{}\" obsolete=\"false\">{details}<revision><major>{major}</major><minor>{minor}</minor><micro>{micro}</micro></revision><display-name>{}</display-name><uses-license ref=\"{}\"/></localPackage></ns2:repository>\n",
        escape(&license.id),
        escape(&license.text),
        escape(&artifact.name),
        escape(&artifact.name),
        escape(&license.id)
    );
    // The archive can contain package.xml; replace its metadata only inside this new private staging directory.
    let path = directory.join("package.xml");
    ensure!(
        !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()),
        "SDK package metadata must not be a symlink"
    );
    fs::write(path, package)?;
    Ok(())
}

fn license_receipts(stage: &Path, licenses: &[License]) -> Result<()> {
    if !licenses
        .iter()
        .any(|license| license.id == "android-sdk-license")
    {
        return Ok(());
    }
    let directory = stage.join("sdk/licenses");
    kotlin::ensure_directory(&directory)?;
    for license in licenses {
        if license.id != "android-sdk-license" {
            continue;
        }
        let hash = format!("{:x}", sha1::Sha1::digest(license.text.as_bytes()));
        fs::write(directory.join(&license.id), format!("{hash}\n"))?;
    }
    Ok(())
}

fn slot_name(value: &str) -> Result<()> {
    ensure!(
        value.starts_with("generation-")
            && value.len() == 43
            && value
                .get(11..)
                .is_some_and(|suffix| suffix.bytes().all(|byte| byte.is_ascii_hexdigit())),
        "Invalid managed installation slot"
    );
    Ok(())
}

fn active_at(root: &Path) -> Result<Option<Active>> {
    let path = root.join("active.json");
    match read_json::<Active>(&path) {
        Ok(active) => {
            ensure!(active.schema == 1, "Unsupported managed setup manifest");
            slot_name(&active.slot)?;
            if let Some(previous) = &active.previous {
                slot_name(previous)?;
            }
            Ok(Some(active))
        }
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn active_for_repair(
    root: &Path,
    progress: &impl Fn(Progress),
    total: u64,
) -> Result<Option<Active>> {
    match active_at(root) {
        Ok(active) => Ok(active),
        Err(error) if error.is::<serde_json::Error>() => {
            let bytes = regular_file(&root.join("active.json"), 64 * 1024)?;
            let mut backup = tempfile::Builder::new().prefix("active.corrupt-").suffix(".json").tempfile_in(root)?;
            backup.write_all(&bytes)?;
            backup.as_file().sync_all()?;
            let (_, path) = backup.keep()?;
            progress(Progress { finishing: true, message: format!("Repairing the setup record. Its original contents were saved to {}", path.display()), downloaded_bytes: total, total_bytes: total });
            Ok(None)
        }
        Err(error) => Err(error).context("The setup record cannot be repaired automatically. Preserve it and check its file permissions, then retry."),
    }
}

fn recover(root: &Path) -> Result<()> {
    let journal_path = root.join("journal.json");
    if !journal_path.try_exists()? {
        return Ok(());
    }
    let journal: Journal = read_json(&journal_path)?;
    let profile = root.parent().context("Managed storage has no profile")?;
    managed::save_environment_unlocked(profile, &journal.environment)?;
    match journal.active {
        Some(active) => {
            slot_name(&active.slot)?;
            write_json(&root.join("active.json"), &active)?;
        }
        None => {
            if root.join("active.json").try_exists()? {
                fs::remove_file(root.join("active.json"))?;
            }
        }
    }
    fs::remove_file(journal_path)?;
    Ok(())
}

#[cfg(test)]
fn validate_generation(root: &Path, slot: &str) -> Result<Installed> {
    validate_generation_cancelled(root, slot, &AtomicBool::new(false))
}

fn validate_generation_cancelled(
    root: &Path,
    slot: &str,
    cancel: &AtomicBool,
) -> Result<Installed> {
    cancelled(cancel)?;
    slot_name(slot)?;
    let directory = root.join(slot);
    let metadata = fs::symlink_metadata(&directory)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "The managed installation must be a regular directory"
    );
    let generation: Generation = read_json(&root.join(format!("{slot}.json")))?;
    ensure!(
        generation.schema == 1,
        "Unsupported managed runtime inventory"
    );
    ensure!(
        generation.recipe == recipe_digest(),
        "The managed tools need an update for this Koda version. Run setup again."
    );
    managed::validate_inventory_cancelled(&directory, &generation.files, cancel)?;
    ensure!(
        generation.modes.keys().eq(generation.files.keys()),
        "The managed permissions manifest is incomplete"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for (name, expected) in &generation.modes {
            cancelled(cancel)?;
            ensure!(
                fs::symlink_metadata(directory.join(name))?
                    .permissions()
                    .mode()
                    & 0o7777
                    == *expected,
                "Managed tool permissions changed: {name}. Run setup again to repair."
            );
        }
    }
    kotlin::validate_jdk_21(&generation.installed.jdk)?;
    if let Some(sdk) = &generation.installed.sdk {
        validate_sdk(sdk, generation.api_level)?;
    }
    if let Some(cli) = &generation.installed.android_cli {
        managed::executable(cli)?;
    }
    Ok(generation.installed)
}

pub fn validate() -> Result<Installed> {
    validate_cancelled(&AtomicBool::new(false))
}

pub fn validate_cancelled(cancel: &AtomicBool) -> Result<Installed> {
    validate_selected_at(&root(), cancel)
}

pub fn validate_at(root: &Path) -> Result<Installed> {
    validate_selected_at(root, &AtomicBool::new(false))
}

fn existing_sdk_api(sdk: &Path) -> Result<u32> {
    let mut api = None;
    let mut count = 0usize;
    for entry in
        fs::read_dir(sdk.join("platforms")).context("The selected SDK has no Android platforms")?
    {
        let entry = entry?;
        count += 1;
        ensure!(
            count <= 1000,
            "The selected SDK contains too many platforms"
        );
        if !entry.path().join("android.jar").is_file() {
            continue;
        }
        let name = entry.file_name();
        let candidate = name
            .to_str()
            .and_then(|name| name.strip_prefix("android-"))
            .and_then(|name| name.split('.').next())
            .and_then(|name| name.parse::<u32>().ok());
        if let Some(candidate) = candidate.filter(|candidate| (1..=100).contains(candidate)) {
            api = Some(api.unwrap_or(0).max(candidate));
        }
    }
    api.context("The selected SDK has no complete Android platform (android.jar). Run setup to repair or choose another SDK.")
}

fn validate_selected_at(root: &Path, cancel: &AtomicBool) -> Result<Installed> {
    cancelled(cancel)?;
    let active = active_at(root)?
        .context("No managed installation is selected. Open Android setup to choose tools.")?;
    let mut installed = validate_generation_cancelled(root, &active.slot, cancel)?;
    let selected =
        managed::environment_at(root.parent().context("Managed storage has no profile")?)?;
    if let Some(jdk) = selected.jdk {
        validate_managed_path_at(root, &jdk, cancel)?;
        kotlin::validate_jdk_21(&jdk)?;
        installed.jdk = jdk;
    }
    if let Some(sdk) = selected.sdk {
        validate_managed_path_at(root, &sdk, cancel)?;
        if installed.sdk.as_ref() != Some(&sdk) {
            validate_sdk(&sdk, existing_sdk_api(&sdk)?)?;
        }
        installed.sdk = Some(sdk);
    }
    if let Some(cli) = selected.android_cli {
        validate_managed_path_at(root, &cli, cancel)?;
        managed::executable(&cli)?;
        installed.android_cli = Some(cli);
    }
    cancelled(cancel)?;
    Ok(installed)
}

pub fn validate_managed_path(path: &Path) -> Result<()> {
    validate_managed_path_at(&root(), path, &AtomicBool::new(false))
}

fn validate_managed_path_at(root: &Path, path: &Path, cancel: &AtomicBool) -> Result<()> {
    cancelled(cancel)?;
    let canonical_root = match root.canonicalize() {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => root.to_path_buf(),
        Err(error) => return Err(error.into()),
    };
    let canonical_path = path.canonicalize().with_context(|| {
        format!(
            "The selected tool path is unavailable: {}. Run setup again to repair it.",
            path.display()
        )
    })?;
    if let Ok(relative) = canonical_path.strip_prefix(&canonical_root) {
        let slot = relative
            .components()
            .next()
            .and_then(|component| component.as_os_str().to_str())
            .context("Managed tool path has no installation slot")?;
        validate_generation_cancelled(root, slot, cancel)?;
    } else if canonical_path.ancestors().any(|ancestor| {
        ancestor.file_name().is_some_and(|name| name == "provision")
            && ancestor.parent().is_some_and(|parent| {
                parent
                    .file_name()
                    .is_some_and(|name| name == "android-tools")
            })
    }) {
        bail!(
            "This tool belongs to another Koda profile. Choose tools from this profile or an independent existing installation."
        );
    }
    Ok(())
}

pub fn install(
    plan: &SetupPlan,
    acceptances: &[LicenseAcceptance],
    cancel: &AtomicBool,
    progress: impl Fn(Progress),
) -> Result<Installed> {
    install_at(&root(), plan, acceptances, cancel, progress)
}

pub fn install_at(
    root: &Path,
    plan: &SetupPlan,
    acceptances: &[LicenseAcceptance],
    cancel: &AtomicBool,
    progress: impl Fn(Progress),
) -> Result<Installed> {
    install_at_with_recipe(root, plan, acceptances, cancel, progress, recipe()?)
}

fn install_at_with_recipe(
    root: &Path,
    plan: &SetupPlan,
    acceptances: &[LicenseAcceptance],
    cancel: &AtomicBool,
    progress: impl Fn(Progress),
    recipe: Recipe,
) -> Result<Installed> {
    cancelled(cancel)?;
    ensure!(
        plan.installation_directory == root && plan.recipe == recipe_digest(),
        "This setup plan belongs to another Koda profile or version. Review a new plan."
    );
    let mut authenticated = plan.clone();
    authenticated.id.clear();
    ensure!(
        digest(&serde_json::to_vec(&authenticated)?) == plan.id,
        "The setup plan changed. Review it again before installation."
    );
    let current_host = host();
    let mut expected_artifacts = Vec::new();
    if plan.options.jdk.is_none() {
        expected_artifacts.push(
            recipe
                .jdk
                .iter()
                .find(|artifact| Some(artifact.host.as_str()) == current_host)
                .context("No Java download for this platform")?
                .clone(),
        );
    }
    if plan.options.install_sdk && plan.options.sdk.is_none() {
        for package in sdk_packages(plan.options.api_level)? {
            expected_artifacts.push(
                recipe
                    .sdk
                    .iter()
                    .find(|artifact| {
                        artifact.name == package && Some(artifact.host.as_str()) == current_host
                    })
                    .context("No SDK download for this platform")?
                    .clone(),
            );
        }
    }
    if plan.options.install_cli && plan.options.android_cli.is_none() {
        expected_artifacts.push(
            recipe
                .cli
                .iter()
                .find(|artifact| Some(artifact.host.as_str()) == current_host)
                .context("No Android CLI download for this platform")?
                .clone(),
        );
    }
    ensure!(
        expected_artifacts == plan.artifacts,
        "The setup plan contains changed downloads. Review a new plan."
    );
    let license_ids = plan
        .artifacts
        .iter()
        .filter_map(|artifact| artifact.license_id.as_ref())
        .collect::<BTreeSet<_>>();
    let expected_licenses = recipe
        .licenses
        .into_iter()
        .filter(|license| license_ids.contains(&license.id))
        .collect::<Vec<_>>();
    ensure!(
        expected_licenses == plan.licenses,
        "The setup plan contains changed license terms. Review a new plan."
    );
    let expected_jdk = plan.options.jdk.clone().unwrap_or_else(|| {
        root.join(&plan.slot).join(if cfg!(target_os = "macos") {
            "jdk/Contents/Home"
        } else {
            "jdk"
        })
    });
    let expected_sdk = plan.options.install_sdk.then(|| {
        plan.options
            .sdk
            .clone()
            .unwrap_or_else(|| root.join(&plan.slot).join("sdk"))
    });
    let expected_cli = plan.options.install_cli.then(|| {
        plan.options.android_cli.clone().unwrap_or_else(|| {
            root.join(&plan.slot).join(if cfg!(windows) {
                "android-cli.exe"
            } else {
                "android-cli"
            })
        })
    });
    ensure!(
        plan.jdk == expected_jdk && plan.sdk == expected_sdk && plan.android_cli == expected_cli,
        "The setup destination changed. Review a new plan."
    );
    ensure!(
        acceptances.len() == plan.licenses.len(),
        "Accept each displayed license explicitly before installation"
    );
    let mut accepted = BTreeSet::new();
    for acceptance in acceptances {
        ensure!(
            acceptance.plan_id == plan.id && accepted.insert(acceptance.id.clone()),
            "The license acceptance is stale or duplicated. Review the current plan."
        );
        ensure!(
            plan.licenses
                .iter()
                .any(|license| license.id == acceptance.id && license.sha256 == acceptance.sha256),
            "The accepted license text does not match the current plan"
        );
    }
    let _lock = profile_lock(root)?;
    recover(root)?;
    for path in [
        &plan.options.jdk,
        &plan.options.sdk,
        &plan.options.android_cli,
    ]
    .into_iter()
    .flatten()
    {
        validate_managed_path(path)?;
    }
    let profile = root.parent().context("Managed storage has no profile")?;
    ensure!(
        environment_fingerprint(profile)? == plan.environment_digest,
        "Another setup changed the saved selections. Review a fresh plan before continuing."
    );
    slot_name(&plan.slot)?;
    ensure!(
        !root.join(&plan.slot).try_exists()?,
        "This installation already exists. Refresh setup before retrying."
    );
    kotlin::ensure_directory(&root.join("downloads"))?;
    let reservation = plan
        .download_bytes
        .saturating_mul(6)
        .min(MAX_EXPANDED)
        .saturating_add(plan.download_bytes);
    storage_budget(root, reservation)?;
    managed::validate_storage_budget(profile, reservation, MAX_FILES)?;
    let stage = tempfile::Builder::new()
        .prefix(".staging-")
        .tempdir_in(root)?;
    let mut expanded = 0u64;
    let mut entries = 0usize;
    let mut preceding = 0u64;
    for artifact in &plan.artifacts {
        cancelled(cancel)?;
        let archive = download(
            root,
            artifact,
            plan.options.offline,
            cancel,
            &progress,
            preceding,
            plan.download_bytes,
        )?;
        preceding += artifact.bytes;
        progress(Progress {
            finishing: false,
            message: format!("Verifying and extracting {}", artifact.name),
            downloaded_bytes: preceding,
            total_bytes: plan.download_bytes,
        });
        safe_relative(Path::new(&artifact.destination))?;
        let destination = stage.path().join(&artifact.destination);
        parent_directories(stage.path(), Path::new(&artifact.destination))?;
        if artifact.format == "executable" {
            let mut input = fs::File::open(&archive)?;
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&destination)?;
            copy_bounded(&mut input, &mut file, cancel, &mut expanded)?;
            file_permissions(&destination, 0o755)?;
            file.sync_all()?;
        } else {
            extract(
                &archive,
                artifact,
                &destination,
                cancel,
                &mut expanded,
                &mut entries,
            )?;
        }
        if let Some(license_id) = &artifact.license_id {
            let license = plan
                .licenses
                .iter()
                .find(|license| &license.id == license_id)
                .context("The package's license was not accepted")?;
            if artifact.destination.starts_with("sdk/") {
                local_package(&destination, artifact, license)?;
            }
        }
    }
    license_receipts(stage.path(), &plan.licenses)?;
    let staged_jdk = if let Some(jdk) = &plan.options.jdk {
        jdk.clone()
    } else {
        stage.path().join(if cfg!(target_os = "macos") {
            "jdk/Contents/Home"
        } else {
            "jdk"
        })
    };
    kotlin::validate_jdk_21(&staged_jdk)?;
    if plan.options.install_sdk {
        let staged_sdk = plan
            .options
            .sdk
            .clone()
            .unwrap_or_else(|| stage.path().join("sdk"));
        validate_sdk(&staged_sdk, plan.options.api_level)?;
    }
    let (files, modes) = inventory(stage.path(), cancel)?;
    let installed = Installed {
        jdk: plan.jdk.clone(),
        sdk: plan.sdk.clone(),
        android_cli: plan.android_cli.clone(),
    };
    let generation = Generation {
        schema: 1,
        recipe: plan.recipe.clone(),
        installed: installed.clone(),
        files,
        modes,
        artifacts: plan.artifacts.clone(),
        accepted_licenses: plan.licenses.clone(),
        api_level: plan.options.api_level,
    };
    cancelled(cancel)?;
    progress(Progress {
        finishing: false,
        message: "Saving validated tools and paths".into(),
        downloaded_bytes: plan.download_bytes,
        total_bytes: plan.download_bytes,
    });
    cancelled(cancel)?;
    progress(Progress {
        finishing: true,
        message: "Finalizing setup. The selection is being saved atomically.".into(),
        downloaded_bytes: plan.download_bytes,
        total_bytes: plan.download_bytes,
    });
    fs::rename(stage.path(), root.join(&plan.slot))?;
    write_json(&root.join(format!("{}.json", plan.slot)), &generation)?;
    let old_active = active_for_repair(root, &progress, plan.download_bytes)?;
    let old_environment = managed::environment_at(profile)?;
    write_json(
        &root.join("journal.json"),
        &Journal {
            environment: old_environment.clone(),
            active: old_active.clone(),
        },
    )?;
    let commit = (|| -> Result<()> {
        let active = Active {
            schema: 1,
            slot: plan.slot.clone(),
            previous: old_active.map(|active| active.slot),
        };
        write_json(&root.join("active.json"), &active)?;
        let environment = managed::Environment {
            jdk: Some(installed.jdk.clone()),
            sdk: installed.sdk.clone().or(old_environment.sdk),
            android_cli: installed
                .android_cli
                .clone()
                .or(old_environment.android_cli),
        };
        managed::save_environment_unlocked(profile, &environment)?;
        fs::remove_file(root.join("journal.json"))?;
        Ok(())
    })();
    if let Err(error) = commit {
        recover(root)
            .context("Setup could not restore the previous selection. Retry setup to recover.")?;
        return Err(error).context(
            "Setup could not save the selected tools; the previous selection was restored",
        );
    }
    progress(Progress {
        finishing: true,
        message: "Setup complete".into(),
        downloaded_bytes: plan.download_bytes,
        total_bytes: plan.download_bytes,
    });
    Ok(installed)
}

pub fn rollback() -> Result<Installed> {
    rollback_at(&root())
}

pub fn rollback_cancelled(cancel: &AtomicBool) -> Result<Installed> {
    rollback_at_cancelled(&root(), cancel)
}

pub fn rollback_at(root: &Path) -> Result<Installed> {
    rollback_at_cancelled(root, &AtomicBool::new(false))
}

fn rollback_at_cancelled(root: &Path, cancel: &AtomicBool) -> Result<Installed> {
    cancelled(cancel)?;
    let _lock = profile_lock(root)?;
    recover(root)?;
    let active = active_at(root)?.context("No installation is available to roll back")?;
    let previous = active
        .previous
        .context("No previous installation is available")?;
    let installed = validate_generation_cancelled(root, &previous, cancel)?;
    for path in std::iter::once(&installed.jdk)
        .chain(installed.sdk.iter())
        .chain(installed.android_cli.iter())
    {
        validate_managed_path_at(root, path, cancel)?;
    }
    let profile = root.parent().context("Managed storage has no profile")?;
    let mut environment = managed::environment_at(profile)?;
    cancelled(cancel)?;
    write_json(
        &root.join("journal.json"),
        &Journal {
            environment: environment.clone(),
            active: Some(Active {
                schema: 1,
                slot: active.slot.clone(),
                previous: Some(previous.clone()),
            }),
        },
    )?;
    environment.jdk = Some(installed.jdk.clone());
    environment.sdk = installed.sdk.clone();
    environment.android_cli = installed.android_cli.clone();
    write_json(
        &root.join("active.json"),
        &Active {
            schema: 1,
            slot: previous,
            previous: Some(active.slot),
        },
    )?;
    if let Err(error) = managed::save_environment_unlocked(profile, &environment) {
        recover(root)?;
        return Err(error);
    }
    fs::remove_file(root.join("journal.json"))?;
    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jdk_fixture(parent: &Path, name: &str) -> Result<PathBuf> {
        let home = parent.join(name);
        fs::create_dir_all(home.join("bin"))?;
        for binary in ["java", "javac"] {
            let path = home.join(if cfg!(windows) {
                format!("bin/{binary}.exe")
            } else {
                format!("bin/{binary}")
            });
            fs::write(&path, "fixture")?;
            file_permissions(&path, 0o755)?;
        }
        fs::write(home.join("release"), "JAVA_VERSION=\"21.0.12.1\"\n")?;
        Ok(home)
    }

    fn java_options(jdk: PathBuf) -> Options {
        Options {
            jdk: Some(jdk),
            install_sdk: false,
            install_cli: false,
            ..Options::default()
        }
    }

    #[test]
    fn recipes_have_complete_pins_and_exact_license_hashes() -> Result<()> {
        let recipe = recipe()?;
        for artifact in recipe.jdk.iter().chain(&recipe.sdk).chain(&recipe.cli) {
            ensure!(valid_digest(&artifact.sha256), "Archive lacks SHA-256");
            checked_url(&artifact.url)?;
            ensure!(
                artifact.bytes > 0 && artifact.bytes < MAX_DOWNLOAD,
                "Archive size invalid"
            );
        }
        for license in &recipe.licenses {
            assert_eq!(digest(license.text.as_bytes()), license.sha256);
        }
        let license = recipe
            .licenses
            .iter()
            .find(|license| license.id == "android-sdk-license")
            .context("Missing SDK license")?;
        assert_eq!(
            format!("{:x}", sha1::Sha1::digest(license.text.as_bytes())),
            "24333f8a63b6825ea9c5514f83c2829b004d1fee"
        );
        for host in [
            "mac-aarch64",
            "mac-x86_64",
            "linux-x86_64",
            "windows-x86_64",
        ] {
            assert_eq!(
                recipe
                    .jdk
                    .iter()
                    .filter(|artifact| artifact.host == host)
                    .count(),
                1
            );
            assert_eq!(
                recipe
                    .cli
                    .iter()
                    .filter(|artifact| artifact.host == host)
                    .count(),
                1
            );
            for package in sdk_packages(36)?.into_iter().chain(sdk_packages(37)?) {
                assert_eq!(
                    recipe
                        .sdk
                        .iter()
                        .filter(|artifact| artifact.host == host && artifact.name == package)
                        .count(),
                    1
                );
            }
        }
        Ok(())
    }

    #[test]
    fn jre_missing_compiler_and_wrong_version_are_actionable() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let home = jdk_fixture(temporary.path(), "jdk")?;
        kotlin::validate_jdk_21(&home)?;
        let compiler = home.join(if cfg!(windows) {
            "bin/javac.exe"
        } else {
            "bin/javac"
        });
        fs::remove_file(&compiler)?;
        assert!(
            format!(
                "{:#}",
                kotlin::validate_jdk_21(&home).expect_err("JRE must fail")
            )
            .contains("bin/javac")
        );
        fs::write(&compiler, "fixture")?;
        file_permissions(&compiler, 0o755)?;
        fs::write(home.join("release"), "JAVA_VERSION=\"25\"\n")?;
        assert!(kotlin::validate_jdk_21(&home).is_err());
        fs::write(
            home.join("release"),
            "JAVA_VERSION=\"21.0.12\"\nOS_ARCH=\"other-architecture\"\n",
        )?;
        assert!(kotlin::validate_jdk_21(&home).is_err());
        Ok(())
    }

    #[test]
    fn existing_java_is_saved_atomically_and_survives_restart_and_rollback() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("android-tools/provision");
        let cancel = AtomicBool::new(false);
        let first = jdk_fixture(temporary.path(), "first")?;
        let second = jdk_fixture(temporary.path(), "second")?;
        let plan = plan_at(&root, java_options(first.clone()), &cancel)?;
        assert!(plan.artifacts.is_empty());
        install_at(&root, &plan, &[], &cancel, |_| {})?;
        assert_eq!(validate_at(&root)?.jdk, first);
        assert_eq!(
            managed::environment_at(root.parent().context("No profile")?)?.jdk,
            Some(first.clone())
        );
        let plan = plan_at(&root, java_options(second.clone()), &cancel)?;
        install_at(&root, &plan, &[], &cancel, |_| {})?;
        assert_eq!(validate_at(&root)?.jdk, second);
        assert_eq!(rollback_at(&root)?.jdk, first);
        assert_eq!(
            managed::environment_at(root.parent().context("No profile")?)?.jdk,
            Some(first)
        );
        Ok(())
    }

    #[test]
    fn concurrent_setup_and_stale_plan_do_not_overwrite_selections() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("android-tools/provision");
        let cancel = AtomicBool::new(false);
        let home = jdk_fixture(temporary.path(), "jdk")?;
        let plan = plan_at(&root, java_options(home.clone()), &cancel)?;
        let lock = profile_lock(&root)?;
        assert!(
            format!(
                "{:#}",
                install_at(&root, &plan, &[], &cancel, |_| {}).expect_err("Lock must fail")
            )
            .contains("Another Koda window")
        );
        drop(lock);
        managed::save_environment_unlocked(
            root.parent().context("No profile")?,
            &managed::Environment {
                jdk: Some(home),
                ..Default::default()
            },
        )?;
        assert!(install_at(&root, &plan, &[], &cancel, |_| {}).is_err());
        assert!(!root.join("active.json").exists());
        Ok(())
    }

    #[test]
    fn declined_wrong_plan_and_changed_license_never_start_downloads() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("android-tools/provision");
        let cancel = AtomicBool::new(false);
        let home = jdk_fixture(temporary.path(), "jdk")?;
        let plan = plan_at(
            &root,
            Options {
                jdk: Some(home),
                ..Default::default()
            },
            &cancel,
        )?;
        assert_eq!(plan.licenses.len(), 2);
        assert!(install_at(&root, &plan, &[], &cancel, |_| {}).is_err());
        let acceptances = plan
            .licenses
            .iter()
            .map(|license| LicenseAcceptance {
                id: license.id.clone(),
                sha256: license.sha256.clone(),
                plan_id: "another-plan".into(),
            })
            .collect::<Vec<_>>();
        assert!(install_at(&root, &plan, &acceptances, &cancel, |_| {}).is_err());
        let mut changed = plan;
        changed.licenses.clear();
        changed.id.clear();
        changed.id = digest(&serde_json::to_vec(&changed)?);
        assert!(install_at(&root, &changed, &[], &cancel, |_| {}).is_err());
        assert!(!root.exists());
        Ok(())
    }

    #[test]
    fn corrupt_environment_is_preserved_and_cancel_does_not_write_paths() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let profile = temporary.path().join("android-tools");
        fs::create_dir(&profile)?;
        fs::write(profile.join("environment.json"), "broken selections")?;
        let home = jdk_fixture(temporary.path(), "jdk")?;
        let cancel = AtomicBool::new(false);
        assert!(
            plan_at(
                &profile.join("provision"),
                java_options(home.clone()),
                &cancel
            )
            .is_err()
        );
        assert_eq!(
            fs::read_to_string(profile.join("environment.json"))?,
            "broken selections"
        );
        fs::remove_file(profile.join("environment.json"))?;
        let plan = plan_at(&profile.join("provision"), java_options(home), &cancel)?;
        cancel.store(true, Ordering::Relaxed);
        assert!(install_at(&profile.join("provision"), &plan, &[], &cancel, |_| {}).is_err());
        assert!(!profile.join("environment.json").exists());
        Ok(())
    }

    #[test]
    fn interrupted_commit_recovers_previous_selection() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("android-tools/provision");
        let cancel = AtomicBool::new(false);
        let first = jdk_fixture(temporary.path(), "first")?;
        let second = jdk_fixture(temporary.path(), "second")?;
        let plan = plan_at(&root, java_options(first.clone()), &cancel)?;
        install_at(&root, &plan, &[], &cancel, |_| {})?;
        let profile = root.parent().context("No profile")?;
        let previous = managed::environment_at(profile)?;
        write_json(
            &root.join("journal.json"),
            &Journal {
                environment: previous,
                active: active_at(&root)?,
            },
        )?;
        managed::save_environment_unlocked(
            profile,
            &managed::Environment {
                jdk: Some(second),
                ..Default::default()
            },
        )?;
        recover(&root)?;
        assert_eq!(managed::environment_at(profile)?.jdk, Some(first));
        assert!(!root.join("journal.json").exists());
        Ok(())
    }

    #[test]
    fn offline_missing_download_and_corrupt_cache_fail_before_installation() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("android-tools/provision");
        let cancel = AtomicBool::new(false);
        let home = jdk_fixture(temporary.path(), "jdk")?;
        assert!(
            plan_at(
                &root,
                Options {
                    jdk: Some(home),
                    offline: true,
                    ..Default::default()
                },
                &cancel
            )
            .is_err()
        );
        let artifact = recipe()?.jdk.into_iter().next().context("No JDK recipe")?;
        fs::write(temporary.path().join("bad.archive"), "wrong")?;
        assert!(verify_download(&temporary.path().join("bad.archive"), &artifact).is_err());
        Ok(())
    }

    #[test]
    fn archive_paths_and_download_origins_are_constrained() {
        for path in [
            "../escape",
            "/absolute",
            "root/../../escape",
            "C:/windows",
            "root\\escape",
        ] {
            assert!(safe_relative(Path::new(path)).is_err());
        }
        for url in [
            "http://dl.google.com/archive",
            "https://example.org/archive",
            "https://user:secret@github.com/archive",
            "https://dl.google.com:444/archive",
        ] {
            assert!(checked_url(url).is_err());
        }
        assert!(
            checked_url("https://dl.google.com/android/repository/platform-36_r02.zip").is_ok()
        );
    }

    #[test]
    fn malicious_zip_entries_cannot_escape_or_set_special_permissions() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join("malicious.zip");
        let mut archive = zip::ZipWriter::new(fs::File::create(&path)?);
        archive.start_file("root/../../escape", zip::write::FileOptions::default())?;
        archive.write_all(b"bad")?;
        archive.finish()?;
        let mut artifact = recipe()?.jdk.into_iter().next().context("No recipe")?;
        artifact.format = "zip".into();
        assert!(
            extract(
                &path,
                &artifact,
                &temporary.path().join("out"),
                &AtomicBool::new(false),
                &mut 0,
                &mut 0
            )
            .is_err()
        );
        assert!(!temporary.path().join("escape").exists());
        Ok(())
    }

    #[test]
    fn compile_sdk_is_only_a_literal_hint_and_never_executes_build_scripts() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        fs::write(
            temporary.path().join("build.gradle.kts"),
            "android {\n compileSdk = providers.exec { commandLine(\"touch\", \"bad\") }\n}\n",
        )?;
        assert_eq!(compile_sdk_hint(temporary.path()), None);
        fs::write(
            temporary.path().join("build.gradle.kts"),
            "android {\n compileSdk = 37\n}\n",
        )?;
        assert_eq!(compile_sdk_hint(temporary.path()), Some(37));
        assert!(!temporary.path().join("bad").exists());
        Ok(())
    }
    fn sdk_fixture_plan(root: &Path, parent: &Path) -> Result<(SetupPlan, Recipe)> {
        let cancel = AtomicBool::new(false);
        let home = jdk_fixture(parent, "existing-jdk")?;
        let mut plan = plan_at(
            root,
            Options {
                jdk: Some(home),
                install_cli: false,
                ..Default::default()
            },
            &cancel,
        )?;
        fs::create_dir_all(root.join("downloads"))?;
        let fixtures = [
            ("platform-tools", vec![("adb", 0o755)]),
            ("platforms;android-36", vec![("android.jar", 0o644)]),
            (
                "build-tools;36.0.0",
                vec![("aapt2", 0o755), ("lib/d8.jar", 0o644)],
            ),
        ];
        for (index, (_, entries)) in fixtures.iter().enumerate() {
            let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            archive.add_directory("root/", zip::write::FileOptions::default())?;
            for (name, mode) in entries {
                let name = if cfg!(windows) && (*name == "adb" || *name == "aapt2") {
                    format!("{name}.exe")
                } else {
                    name.to_string()
                };
                archive.start_file(
                    format!("root/{name}"),
                    zip::write::FileOptions::default().unix_permissions(*mode),
                )?;
                archive.write_all(b"synthetic test SDK component")?;
            }
            let bytes = archive.finish()?.into_inner();
            let artifact = plan
                .artifacts
                .get_mut(index)
                .context("Missing test artifact")?;
            artifact.sha256 = digest(&bytes);
            artifact.sha1 = None;
            artifact.bytes = bytes.len() as u64;
            fs::write(download_path(root, artifact), bytes)?;
        }
        let mut recipe = recipe()?;
        recipe.sdk = plan.artifacts.clone();
        plan.options.offline = true;
        plan.download_bytes = plan.artifacts.iter().map(|artifact| artifact.bytes).sum();
        plan.id.clear();
        plan.id = digest(&serde_json::to_vec(&plan)?);
        Ok((plan, recipe))
    }

    #[test]
    fn native_sdk_installs_verified_archives_and_detects_missing_files() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("android-tools/provision");
        let (plan, recipe) = sdk_fixture_plan(&root, temporary.path())?;
        let accepted = plan
            .licenses
            .iter()
            .map(|license| LicenseAcceptance {
                id: license.id.clone(),
                sha256: license.sha256.clone(),
                plan_id: plan.id.clone(),
            })
            .collect::<Vec<_>>();
        let installed = install_at_with_recipe(
            &root,
            &plan,
            &accepted,
            &AtomicBool::new(false),
            |_| {},
            recipe,
        )?;
        let sdk = installed.sdk.context("Missing installed SDK")?;
        validate_sdk(&sdk, 36)?;
        assert_eq!(
            fs::read_to_string(sdk.join("licenses/android-sdk-license"))?,
            "24333f8a63b6825ea9c5514f83c2829b004d1fee\n"
        );
        assert_eq!(validate_at(&root)?.sdk, Some(sdk.clone()));
        fs::remove_file(sdk.join("platforms/android-36/android.jar"))?;
        assert!(validate_at(&root).is_err());
        Ok(())
    }

    #[test]
    fn native_setup_failure_and_extraction_cancel_keep_previous_paths() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("android-tools/provision");
        let (plan, recipe) = sdk_fixture_plan(&root, temporary.path())?;
        let accepted = plan
            .licenses
            .iter()
            .map(|license| LicenseAcceptance {
                id: license.id.clone(),
                sha256: license.sha256.clone(),
                plan_id: plan.id.clone(),
            })
            .collect::<Vec<_>>();
        let cancel = AtomicBool::new(false);
        assert!(
            install_at_with_recipe(
                &root,
                &plan,
                &accepted,
                &cancel,
                |progress| {
                    if progress.message.starts_with("Verifying and extracting") {
                        cancel.store(true, Ordering::Relaxed);
                    }
                },
                recipe
            )
            .is_err()
        );
        assert!(!root.join("active.json").exists());
        assert!(
            !root
                .parent()
                .context("No profile")?
                .join("environment.json")
                .exists()
        );
        assert!(root.read_dir()?.all(|entry| {
            entry.is_ok_and(|entry| !entry.file_name().to_string_lossy().starts_with(".staging-"))
        }));
        cancel.store(false, Ordering::Relaxed);
        let (plan, recipe) = sdk_fixture_plan(&root, temporary.path())?;
        let accepted = plan
            .licenses
            .iter()
            .map(|license| LicenseAcceptance {
                id: license.id.clone(),
                sha256: license.sha256.clone(),
                plan_id: plan.id.clone(),
            })
            .collect::<Vec<_>>();
        let artifact = plan.artifacts.first().context("Missing test download")?;
        fs::write(download_path(&root, artifact), b"corrupt cached download")?;
        assert!(install_at_with_recipe(&root, &plan, &accepted, &cancel, |_| {}, recipe).is_err());
        assert!(!root.join("active.json").exists());
        assert!(
            !root
                .parent()
                .context("No profile")?
                .join("environment.json")
                .exists()
        );
        Ok(())
    }
    #[test]
    fn corrupt_active_pointer_can_be_repaired_without_discarding_its_contents() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("android-tools/provision");
        let cancel = AtomicBool::new(false);
        let home = jdk_fixture(temporary.path(), "jdk")?;
        fs::create_dir_all(&root)?;
        fs::write(root.join("active.json"), "broken setup record")?;
        let plan = plan_at(&root, java_options(home.clone()), &cancel)?;
        install_at(&root, &plan, &[], &cancel, |_| {})?;
        assert_eq!(validate_at(&root)?.jdk, home);
        let backups = root
            .read_dir()?
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("active.corrupt-")
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        let backup = backups.first().context("Missing backup")?;
        assert_eq!(fs::read_to_string(backup.path())?, "broken setup record");
        Ok(())
    }
    #[test]
    fn cancellation_at_the_publication_boundary_keeps_existing_settings() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("android-tools/provision");
        let cancel = AtomicBool::new(false);
        let home = jdk_fixture(temporary.path(), "jdk")?;
        let plan = plan_at(&root, java_options(home), &cancel)?;
        assert!(
            install_at(&root, &plan, &[], &cancel, |progress| {
                if progress.message == "Saving validated tools and paths" {
                    cancel.store(true, Ordering::Relaxed);
                }
            })
            .is_err()
        );
        assert!(!root.join("active.json").exists());
        assert!(!root.join(&plan.slot).exists());
        assert!(
            !root
                .parent()
                .context("No profile")?
                .join("environment.json")
                .exists()
        );
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn symlink_aliases_cannot_select_another_profiles_managed_tools() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let foreign = temporary
            .path()
            .join("nightly/android-tools/provision/generation-0123456789abcdef0123456789abcdef");
        let home = jdk_fixture(&foreign, "jdk")?;
        let alias = temporary.path().join("jdk-alias");
        std::os::unix::fs::symlink(home, &alias)?;
        assert!(
            format!(
                "{:#}",
                validate_managed_path(&alias).expect_err("Foreign alias must fail")
            )
            .contains("another Koda profile")
        );
        assert!(
            plan_at(
                &temporary.path().join("stable/android-tools/provision"),
                java_options(alias),
                &AtomicBool::new(false)
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn reused_sdk_platform_loss_is_detected_by_validate_and_rollback() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("android-tools/provision");
        let cancel = AtomicBool::new(false);
        let home = jdk_fixture(temporary.path(), "jdk")?;
        let sdk = temporary.path().join("external-sdk");
        for relative in [
            "platform-tools",
            "platforms/android-34",
            "build-tools/35.0.0/lib",
        ] {
            fs::create_dir_all(sdk.join(relative))?;
        }
        for (relative, executable) in [
            (
                if cfg!(windows) {
                    "platform-tools/adb.exe"
                } else {
                    "platform-tools/adb"
                },
                true,
            ),
            ("platforms/android-34/android.jar", false),
            (
                if cfg!(windows) {
                    "build-tools/35.0.0/aapt2.exe"
                } else {
                    "build-tools/35.0.0/aapt2"
                },
                true,
            ),
            ("build-tools/35.0.0/lib/d8.jar", false),
        ] {
            let path = sdk.join(relative);
            fs::write(&path, "test SDK")?;
            file_permissions(&path, if executable { 0o755 } else { 0o644 })?;
        }
        let plan = plan_at(
            &root,
            Options {
                jdk: Some(home.clone()),
                sdk: Some(sdk.clone()),
                api_level: 34,
                install_cli: false,
                ..Default::default()
            },
            &cancel,
        )?;
        assert!(plan.artifacts.is_empty());
        install_at(&root, &plan, &[], &cancel, |_| {})?;
        validate_at(&root)?;
        let java_plan = plan_at(&root, java_options(home), &cancel)?;
        install_at(&root, &java_plan, &[], &cancel, |_| {})?;
        fs::remove_file(sdk.join("platforms/android-34/android.jar"))?;
        assert!(validate_at(&root).is_err());
        assert!(validate_generation(&root, &plan.slot).is_err());
        assert!(rollback_at(&root).is_err());
        Ok(())
    }
    #[test]
    fn project_sdk_properties_support_java_escapes_and_relative_discovery() -> Result<()> {
        assert_eq!(
            sdk_property(r"sdk.dir=C\:\\Users\\Koda\ User\\Android\\Sdk")?,
            Some(r"C:\Users\Koda User\Android\Sdk".into())
        );
        assert_eq!(
            sdk_property("# ignored comment\\\nsdk.dir = ../sdk\\\n   \\u0020folder\n")?,
            Some("../sdk folder".into())
        );
        let temporary = tempfile::tempdir()?;
        let project = temporary.path().join("project");
        let sdk = temporary.path().join("sdk folder");
        fs::create_dir(&project)?;
        fs::create_dir(&sdk)?;
        fs::write(
            project.join("local.properties"),
            "sdk.dir=../sdk\\ folder\nignored.command=touch should-not-run\n",
        )?;
        assert_eq!(project_sdk(&project)?, Some(sdk.canonicalize()?));
        assert!(!project.join("should-not-run").exists());
        Ok(())
    }

    #[test]
    fn project_sdk_discovery_rejects_oversize_symlink_and_invalid_settings() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join("local.properties");
        fs::write(&path, vec![b'x'; 64 * 1024 + 1])?;
        assert!(project_sdk(temporary.path()).is_err());
        fs::write(&path, "sdk.dir=unavailable-sdk\n")?;
        assert!(project_sdk(temporary.path()).is_err());
        fs::write(&path, "sdk.dir=\\uZZZZ\n")?;
        assert!(project_sdk(temporary.path()).is_err());
        #[cfg(unix)]
        {
            fs::remove_file(&path)?;
            let target = temporary.path().join("linked.properties");
            fs::write(&target, "sdk.dir=/sdk\n")?;
            std::os::unix::fs::symlink(target, path)?;
            assert!(project_sdk(temporary.path()).is_err());
        }
        Ok(())
    }
    #[test]
    fn blocking_download_transport_reads_streamed_body_on_a_plain_worker_thread() -> Result<()> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let server = std::thread::spawn(move || -> Result<()> {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
            let mut request = [0; 2048];
            ensure!(
                stream.read(&mut request)? > 0,
                "The loopback fixture did not receive a request"
            );
            let payload = b"Java21fixture";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            )?;
            stream.write_all(&payload[..5])?;
            stream.flush()?;
            std::thread::sleep(Duration::from_millis(10));
            stream.write_all(&payload[5..])?;
            stream.flush()?;
            Ok(())
        });
        let worker = std::thread::spawn(move || -> Result<String> {
            // Only this test client permits its loopback HTTP fixture. Production's
            // URL validation and the same builder continue to require verified HTTPS.
            let client = download_client_builder()
                .https_only(false)
                .no_proxy()
                .build()?;
            let mut response = client
                .get(format!("http://{address}/jdk-fixture"))
                .send()?
                .error_for_status()?;
            let mut body = String::new();
            response.read_to_string(&mut body)?;
            Ok(body)
        });
        let body = worker.join().map_err(|_| {
            anyhow::anyhow!("The blocking download transport panicked without a Tokio runtime")
        })??;
        server
            .join()
            .map_err(|_| anyhow::anyhow!("The loopback fixture server panicked"))??;
        assert_eq!(body, "Java21fixture");
        Ok(())
    }

    #[test]
    fn blocking_download_transport_times_out_a_stalled_body_without_panicking() -> Result<()> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let server = std::thread::spawn(move || -> Result<()> {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
            let mut request = [0; 2048];
            ensure!(
                stream.read(&mut request)? > 0,
                "The loopback fixture did not receive a request"
            );
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\na")?;
            stream.flush()?;
            std::thread::sleep(Duration::from_secs(2));
            Ok(())
        });
        let worker = std::thread::spawn(move || -> Result<()> {
            let client = download_client_builder()
                .https_only(false)
                .no_proxy()
                .timeout(Duration::from_millis(500))
                .build()?;
            let mut response = client
                .get(format!("http://{address}/stalled-fixture"))
                .send()?
                .error_for_status()?;
            let started = Instant::now();
            let mut body = String::new();
            let result = response.read_to_string(&mut body);
            ensure!(
                result.is_err(),
                "A stalled download must fail rather than wait for the whole response"
            );
            ensure!(
                started.elapsed() < Duration::from_millis(1500),
                "The blocking read timeout did not bound the stalled body"
            );
            Ok(())
        });
        worker.join().map_err(|_| {
            anyhow::anyhow!("The blocking download timeout panicked without a Tokio runtime")
        })??;
        server
            .join()
            .map_err(|_| anyhow::anyhow!("The loopback fixture server panicked"))??;
        Ok(())
    }
    #[test]
    fn rollback_rejects_corrupt_reused_managed_dependencies_without_changing_selection()
    -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("android-tools/provision");
        fs::create_dir_all(&root)?;
        let original_slot = "generation-00000000000000000000000000000001";
        let reused_slot = "generation-00000000000000000000000000000002";
        let current_slot = "generation-00000000000000000000000000000003";
        let original = jdk_fixture(&root.join(original_slot), "jdk")?;
        let library = original.join("lib/server/libjvm.so");
        fs::create_dir_all(library.parent().context("Missing library parent")?)?;
        fs::write(&library, "original native JVM library")?;
        fs::create_dir(root.join(reused_slot))?;
        let current = jdk_fixture(&root.join(current_slot), "jdk")?;
        for (slot, jdk) in [
            (original_slot, &original),
            (reused_slot, &original),
            (current_slot, &current),
        ] {
            let (files, modes) = inventory(&root.join(slot), &AtomicBool::new(false))?;
            let generation = Generation {
                schema: 1,
                recipe: recipe_digest(),
                installed: Installed {
                    jdk: jdk.clone(),
                    sdk: None,
                    android_cli: None,
                },
                files,
                modes,
                artifacts: Vec::new(),
                accepted_licenses: Vec::new(),
                api_level: 36,
            };
            write_json(&root.join(format!("{slot}.json")), &generation)?;
        }
        write_json(
            &root.join("active.json"),
            &Active {
                schema: 1,
                slot: current_slot.into(),
                previous: Some(reused_slot.into()),
            },
        )?;
        let profile = root.parent().context("Missing profile")?;
        managed::save_environment_unlocked(
            profile,
            &managed::Environment {
                jdk: Some(current),
                ..Default::default()
            },
        )?;
        let previous_pointer = fs::read(root.join("active.json"))?;
        let previous_environment = fs::read(profile.join("environment.json"))?;
        fs::write(library, "corrupt native JVM library")?;
        kotlin::validate_jdk_21(&original)?;
        validate_generation(&root, reused_slot)?;
        assert!(rollback_at(&root).is_err());
        assert_eq!(fs::read(root.join("active.json"))?, previous_pointer);
        assert_eq!(
            fs::read(profile.join("environment.json"))?,
            previous_environment
        );
        assert!(!root.join("journal.json").exists());
        Ok(())
    }
    #[test]
    fn insufficient_space_reports_required_and_available_reservation() -> Result<()> {
        const MIB: u64 = 1024 * 1024;
        let error = ensure_free_space(10 * MIB + 1, 5 * MIB + MIB / 2)
            .expect_err("Insufficient free space must fail")
            .to_string();
        assert!(error.contains("11 MiB"));
        assert!(error.contains("5 MiB is available"));
        assert!(error.contains("select fewer components"));
        ensure_free_space(10 * MIB, 10 * MIB)?;
        Ok(())
    }
}
