pub mod logcat;
pub mod preview;
pub mod project_model;
pub mod project_view_preferences;
use anyhow::{Context as _, Result, bail, ensure};
pub mod java;
pub mod kotlin;
use serde::Deserialize;
use std::{
    env, fs,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub serial: String,
    pub state: String,
    pub model: String,
}

impl Device {
    pub fn is_available(&self) -> bool {
        self.state == "device"
    }
}

pub fn parse_devices(output: &str) -> Result<Vec<Device>> {
    let mut lines = output
        .lines()
        .skip_while(|line| !line.starts_with("List of devices attached"));
    ensure!(lines.next().is_some(), "ADB did not return a device list");
    lines
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            // Wireless mDNS serials can contain spaces. ADB places the state after
            // the complete serial, so find the last state token before metadata.
            let (serial, state, details) = line
                .match_indices(char::is_whitespace)
                .rev()
                .find_map(|(offset, _)| {
                    let remainder = line.get(offset..)?.trim_start();
                    let state = if remainder.starts_with("no permissions") {
                        "no permissions"
                    } else {
                        remainder.split_whitespace().next()?
                    };
                    matches!(
                        state,
                        "device"
                            | "offline"
                            | "unauthorized"
                            | "authorizing"
                            | "connecting"
                            | "bootloader"
                            | "recovery"
                            | "rescue"
                            | "sideload"
                            | "host"
                            | "unknown"
                            | "no permissions"
                    )
                    .then_some((
                        line.get(..offset)?.trim_end(),
                        state,
                        remainder,
                    ))
                })
                .context("ADB device state is missing or unsupported")?;
            ensure!(!serial.is_empty(), "Device serial is missing");
            let model = details
                .split_whitespace()
                .find_map(|field| field.strip_prefix("model:"))
                .unwrap_or(serial)
                .replace('_', " ");
            Ok(Device {
                serial: serial.into(),
                state: state.into(),
                model,
            })
        })
        .collect()
}

/// Read Android's ordered ABI list from `adb -s <serial> shell getprop`.
/// Pre-Lollipop devices expose only abi/abi2 instead of abilist.
pub fn parse_device_abis(output: &str) -> Result<Vec<String>> {
    let property = |name: &str| {
        let prefix = format!("[{name}]: [");
        output
            .lines()
            .find_map(|line| line.trim().strip_prefix(&prefix)?.strip_suffix(']'))
    };
    let list = property("ro.product.cpu.abilist").unwrap_or_default();
    let values = if list.trim().is_empty() {
        vec![
            property("ro.product.cpu.abi").unwrap_or_default(),
            property("ro.product.cpu.abi2").unwrap_or_default(),
        ]
    } else {
        list.split(',').collect()
    };
    let mut abis = Vec::new();
    for value in values
        .into_iter()
        .map(str::trim)
        .filter(|abi| !abi.is_empty())
    {
        ensure!(
            value.chars().all(
                |character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            ),
            "ADB returned an invalid device ABI: {value}"
        );
        if !abis.iter().any(|abi| abi == value) {
            abis.push(value.to_owned());
        }
    }
    ensure!(!abis.is_empty(), "ADB returned no supported device ABIs");
    Ok(abis)
}

pub fn adb_path() -> Result<PathBuf> {
    sdk_tool_path("platform-tools", "adb")
}

pub fn emulator_path() -> Result<PathBuf> {
    sdk_tool_path("emulator", "emulator")
}

fn sdk_tool_path(directory: &str, name: &str) -> Result<PathBuf> {
    let executable = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    for variable in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(root) = env::var_os(variable).filter(|value| !value.is_empty()) {
            let path = PathBuf::from(root).join(directory).join(&executable);
            ensure!(
                path.is_file(),
                "{variable} does not contain {directory}/{executable}"
            );
            return Ok(path);
        }
    }
    if let Ok(path) = which::which(&executable) {
        return Ok(path);
    }
    if let Some(home) = dirs::home_dir() {
        let root = if cfg!(target_os = "macos") {
            home.join("Library/Android/sdk")
        } else if cfg!(windows) {
            home.join("AppData/Local/Android/Sdk")
        } else {
            home.join("Android/Sdk")
        };
        let path = root.join(directory).join(&executable);
        if path.is_file() {
            return Ok(path);
        }
    }
    bail!("{executable} was not found. Install Android SDK {directory} and set ANDROID_HOME.")
}

pub fn parse_emulators(output: &str) -> Result<Vec<String>> {
    let mut names = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|name| {
            ensure!(
                !name.starts_with('-')
                    && name.chars().all(|character| {
                        character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
                    }),
                "Invalid Android virtual device name: {name}"
            );
            Ok(name.to_owned())
        })
        .collect::<Result<Vec<_>>>()?;
    names.sort();
    names.dedup();
    Ok(names)
}

pub fn android_cli_path() -> Result<PathBuf> {
    which::which("android")
        .context("Android CLI was not found. Install Google's Android CLI and add it to PATH.")
}

pub fn is_gradle_project(root: &Path) -> bool {
    (root.join("gradlew").is_file() || root.join("gradlew.bat").is_file())
        && [
            "settings.gradle.kts",
            "settings.gradle",
            "build.gradle.kts",
            "build.gradle",
        ]
        .iter()
        .any(|name| root.join(name).is_file())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AndroidTarget {
    pub module: String,
    pub variant: String,
    pub output_listing: PathBuf,
}

pub struct AndroidApk {
    pub paths: Vec<PathBuf>,
    pub application_id: Option<String>,
}

impl AndroidApk {
    pub fn debug_application_id(&self) -> Result<&str> {
        let application_id = self
            .application_id
            .as_deref()
            .context("The APK metadata has no application ID")?;
        ensure!(
            application_id.contains('.')
                && application_id.split('.').all(|part| {
                    !part.is_empty()
                        && part.starts_with(|character: char| character.is_ascii_alphabetic())
                        && part
                            .chars()
                            .all(|character| character.is_ascii_alphanumeric() || character == '_')
                }),
            "The APK metadata has an invalid application ID"
        );
        Ok(application_id)
    }
}

impl AndroidTarget {
    pub fn label(&self) -> String {
        format!("{} · {}", self.module, self.variant)
    }

    pub fn gradle_task(&self, prefix: &str, suffix: &str) -> String {
        let mut characters = self.variant.chars();
        let capitalized = characters
            .next()
            .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
            .unwrap_or_default();
        format!(
            "{}:{prefix}{capitalized}{suffix}",
            self.module.trim_end_matches(':')
        )
    }

    pub fn apk_paths(&self) -> Result<Vec<PathBuf>> {
        Ok(self.apk()?.paths)
    }

    /// Select a universal APK for tools that do not target a device (such as previews).
    pub fn apk(&self) -> Result<AndroidApk> {
        self.select_apk(None)
    }

    /// Select one standalone APK using Android Studio's version-code and ABI ordering.
    pub fn apk_for_device(&self, abis: &[String]) -> Result<AndroidApk> {
        ensure!(!abis.is_empty(), "The device has no supported ABIs");
        self.select_apk(Some(abis))
    }

    fn select_apk(&self, abis: Option<&[String]>) -> Result<AndroidApk> {
        let listing = fs::read_to_string(&self.output_listing).with_context(|| {
            format!(
                "Build {} before running it: the APK listing is missing",
                self.label()
            )
        })?;
        let metadata_path = if listing.trim_start().starts_with('{') {
            self.output_listing.clone()
        } else {
            let relative = listing
                .lines()
                .find_map(|line| line.strip_prefix("listingFile="))
                .context("The APK redirect does not contain listingFile")?;
            self.output_listing
                .parent()
                .context("APK listing has no parent directory")?
                .join(relative)
        };
        let metadata: ApkMetadata = serde_json::from_str(&fs::read_to_string(&metadata_path)?)
            .context("Unable to read Android APK metadata")?;
        ensure!(
            metadata.version == 3,
            "Unsupported APK metadata version: {}",
            metadata.version
        );
        ensure!(
            metadata.artifact_type.kind == "APK",
            "This target does not produce APKs"
        );
        ensure!(
            metadata.variant_name == self.variant,
            "The APK metadata belongs to a different build variant; sync and build again"
        );
        ensure!(!metadata.elements.is_empty(), "The build produced no APKs");
        let element = select_apk_element(&metadata.elements, abis)?;
        let directory = metadata_path
            .parent()
            .context("APK metadata has no parent directory")?
            .canonicalize()?;
        let paths = std::iter::once(element)
            .map(|element| {
                let path = Path::new(&element.output_file);
                ensure!(
                    !path.as_os_str().is_empty()
                        && path
                            .components()
                            .all(|component| matches!(component, Component::Normal(_))),
                    "APK output must be relative to its metadata directory"
                );
                let path = directory
                    .join(path)
                    .canonicalize()
                    .context("The built APK is missing")?;
                ensure!(
                    path.starts_with(&directory)
                        && path.is_file()
                        && path.extension().is_some_and(|extension| extension == "apk"),
                    "APK output is not an APK inside its metadata directory"
                );
                ensure!(
                    !path.to_string_lossy().contains(','),
                    "Android CLI cannot accept an APK path containing a comma"
                );
                Ok(path)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(AndroidApk {
            paths,
            application_id: metadata.application_id,
        })
    }
}

// ponytail: Android CLI 1.0 exposes targets as text; replace this adapter when it offers a versioned JSON command.
pub fn parse_targets(output: &str) -> Result<Vec<AndroidTarget>> {
    let mut module = None;
    let mut variant = None;
    let mut targets = Vec::new();
    for line in output.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("Task: ") {
            ensure!(
                value.starts_with(':')
                    && value
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric()
                            || ":_-.$".contains(character)),
                "Android CLI returned an invalid Gradle module path"
            );
            module = Some(value.to_owned());
            variant = None;
        } else if let Some(value) = line.strip_prefix("Variant: ") {
            ensure!(
                !value.is_empty()
                    && value
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric()
                            || matches!(character, '_' | '-')),
                "Android CLI returned an invalid variant name"
            );
            variant = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("Output Listing File: ") {
            if value == "null" || value.is_empty() {
                continue;
            }
            let output_listing = PathBuf::from(value);
            ensure!(
                output_listing.is_absolute(),
                "Android CLI returned a relative APK listing path"
            );
            let target = AndroidTarget {
                module: module
                    .clone()
                    .context("Android CLI returned a target without a module")?,
                variant: variant
                    .clone()
                    .context("Android CLI returned a target without a variant")?,
                output_listing,
            };
            ensure!(
                !targets
                    .iter()
                    .any(|previous: &AndroidTarget| previous.module == target.module
                        && previous.variant == target.variant),
                "Android CLI returned duplicate build targets"
            );
            targets.push(target);
        }
    }
    ensure!(
        !targets.is_empty(),
        "No Android application variants were found. Open a Gradle project with an Android application module and sync again."
    );
    targets
        .sort_by(|left, right| (&left.module, &left.variant).cmp(&(&right.module, &right.variant)));
    Ok(targets)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApkMetadata {
    version: u32,
    artifact_type: ArtifactType,
    variant_name: String,
    application_id: Option<String>,
    elements: Vec<ApkElement>,
}

#[derive(Deserialize)]
struct ArtifactType {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApkElement {
    output_file: String,
    filters: Vec<ApkFilter>,
    #[serde(rename = "type")]
    output_type: Option<String>,
    version_code: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApkFilter {
    filter_type: String,
    value: String,
}

fn select_apk_element<'a>(
    elements: &'a [ApkElement],
    abis: Option<&[String]>,
) -> Result<&'a ApkElement> {
    let mut best = None;
    let mut ambiguous = false;
    for element in elements {
        // ABI splits are alternative complete APKs, not base/config split sets.
        // Do not treat an unknown output type or filter as a universal APK.
        ensure!(
            element
                .output_type
                .as_deref()
                .is_none_or(|kind| { matches!(kind, "SINGLE" | "UNIVERSAL" | "ONE_OF_MANY") }),
            "Unsupported APK output type; base/configuration split sets cannot be installed as standalone APKs"
        );
        let abi = match element.filters.as_slice() {
            [] => None,
            [filter] if filter.filter_type == "ABI" && !filter.value.is_empty() => {
                Some(filter.value.as_str())
            }
            _ => continue,
        };
        let preference = match (abis, abi) {
            (None, None) => 0,
            (None, Some(_)) => continue,
            (Some(abis), None) => abis.len() - 1,
            (Some(abis), Some(abi)) => match abis.iter().position(|value| value == abi) {
                Some(0) => abis.len(),
                Some(index) => abis.len() - index - 1,
                None => continue,
            },
        };
        let rank = (element.version_code.unwrap_or(1), preference);
        match best {
            None => best = Some((rank, element)),
            Some((best_rank, _)) if rank > best_rank => {
                best = Some((rank, element));
                ambiguous = false;
            }
            Some((best_rank, _)) if rank == best_rank => ambiguous = true,
            _ => {}
        }
    }
    ensure!(
        !ambiguous,
        "Multiple APK outputs have the same version code and ABI preference; cannot choose one safely"
    );
    best.map(|(_, element)| element).with_context(|| match abis {
        Some(abis) => format!(
            "No standalone APK matches device ABIs [{}]. Build a matching ABI or universal APK. Density/configuration split sets are not supported.",
            abis.join(", ")
        ),
        None => "This operation needs a universal APK. Build one or use Run/Debug with a compatible device for ABI-filtered outputs.".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_abis_preserve_preference_and_support_legacy_properties() -> Result<()> {
        assert_eq!(
            parse_device_abis(
                "[ro.product.cpu.abi]: [ignored]\r\n[ro.product.cpu.abilist]: [x86_64,x86,arm64-v8a,x86]\r\n[unrelated]: [value]\n"
            )?,
            ["x86_64", "x86", "arm64-v8a"]
        );
        assert_eq!(
            parse_device_abis(
                "[ro.product.cpu.abilist]: []\n[ro.product.cpu.abi]: [armeabi-v7a]\n[ro.product.cpu.abi2]: [armeabi]\n"
            )?,
            ["armeabi-v7a", "armeabi"]
        );
        assert_eq!(
            parse_device_abis("[ro.product.cpu.abi]: [arm64-v8a]\n")?,
            ["arm64-v8a"]
        );
        assert!(parse_device_abis("").is_err());
        assert!(parse_device_abis("[ro.product.cpu.abilist]: [x86;bad]\n").is_err());
        assert!(parse_device_abis("[ro.product.cpu.abilist]: [x86_64\n").is_err());
        Ok(())
    }

    fn apk_fixture(elements: serde_json::Value) -> Result<(tempfile::TempDir, AndroidTarget)> {
        let directory = tempfile::tempdir()?;
        let metadata = serde_json::json!({
            "version": 3, "artifactType": {"type": "APK", "kind": "Directory"},
            "applicationId": "dev.anilbeesetti.nextplayer.debug", "variantName": "debug",
            "elements": elements
        });
        for element in metadata["elements"]
            .as_array()
            .context("Expected elements")?
        {
            let path = directory
                .path()
                .join(element["outputFile"].as_str().context("Expected output")?);
            fs::write(path, b"APK fixture")?;
        }
        fs::write(
            directory.path().join("output-metadata.json"),
            serde_json::to_vec(&metadata)?,
        )?;
        // Real AGP redirect files can point outside the intermediates directory.
        let intermediates = directory.path().join("intermediates");
        fs::create_dir(&intermediates)?;
        let listing = intermediates.join("redirect.txt");
        fs::write(
            &listing,
            "#- File Locator -\nlistingFile=../output-metadata.json\n",
        )?;
        Ok((
            directory,
            AndroidTarget {
                module: ":app".into(),
                variant: "debug".into(),
                output_listing: listing,
            },
        ))
    }

    fn output(file: &str, abi: Option<&str>, version: u32) -> serde_json::Value {
        serde_json::json!({
            "type": if abi.is_some() { "ONE_OF_MANY" } else { "SINGLE" },
            "filters": abi.map(|abi| vec![serde_json::json!({"filterType": "ABI", "value": abi})]).unwrap_or_default(),
            "versionCode": version, "versionName": "0.18.0", "outputFile": file
        })
    }

    fn selected_file(target: &AndroidTarget, abis: &[&str]) -> Result<String> {
        let apk =
            target.apk_for_device(&abis.iter().map(|abi| (*abi).to_owned()).collect::<Vec<_>>())?;
        assert_eq!(
            apk.paths.len(),
            1,
            "ABI alternatives must never be installed together"
        );
        assert_eq!(
            apk.debug_application_id()?,
            "dev.anilbeesetti.nextplayer.debug"
        );
        Ok(apk.paths[0]
            .file_name()
            .context("Expected filename")?
            .to_string_lossy()
            .into_owned())
    }

    #[test]
    fn nextplayer_selects_one_apk_for_each_device_and_a_universal_for_preview() -> Result<()> {
        let mut universal = output("universal.apk", None, 74);
        universal["type"] = "UNIVERSAL".into();
        let (_directory, target) = apk_fixture(serde_json::json!([
            output("arm.apk", Some("armeabi-v7a"), 74),
            output("arm64.apk", Some("arm64-v8a"), 74),
            output("x86.apk", Some("x86"), 74),
            output("x86_64.apk", Some("x86_64"), 74),
            universal,
        ]))?;
        assert_eq!(selected_file(&target, &["x86_64", "x86"])?, "x86_64.apk");
        assert_eq!(
            selected_file(&target, &["arm64-v8a", "armeabi-v7a"])?,
            "arm64.apk"
        );
        assert_eq!(selected_file(&target, &["x86"])?, "x86.apk");
        assert_eq!(selected_file(&target, &["riscv64"])?, "universal.apk");
        assert_eq!(target.apk_paths()?[0].file_name().unwrap(), "universal.apk");
        assert!(target.apk_for_device(&[]).is_err());
        Ok(())
    }

    #[test]
    fn selection_matches_studio_version_code_and_abi_order() -> Result<()> {
        let (_directory, target) = apk_fixture(serde_json::json!([
            output("secondary.apk", Some("x86"), 7),
            output("primary.apk", Some("x86_64"), 6),
            output("universal.apk", None, 6),
            output("incompatible.apk", Some("arm64-v8a"), 100),
        ]))?;
        assert_eq!(selected_file(&target, &["x86_64", "x86"])?, "secondary.apk");
        let (_directory, target) = apk_fixture(serde_json::json!([
            output("secondary.apk", Some("x86"), 7),
            output("universal.apk", None, 7),
        ]))?;
        assert_eq!(selected_file(&target, &["x86_64", "x86"])?, "universal.apk");
        let (_directory, target) = apk_fixture(serde_json::json!([
            output("third.apk", Some("armeabi-v7a"), 7),
            output("secondary.apk", Some("x86"), 7),
        ]))?;
        assert_eq!(
            selected_file(&target, &["x86_64", "x86", "armeabi-v7a"])?,
            "secondary.apk"
        );
        assert!(target.apk().is_err());
        let error = target
            .apk_for_device(&["arm64-v8a".into()])
            .err()
            .context("Expected mismatch")?;
        assert!(error.to_string().contains("arm64-v8a"));
        // Missing/null version codes have Android's default version code of 1.
        let mut missing_version = output("default.apk", None, 1);
        missing_version["versionCode"] = serde_json::Value::Null;
        let (_directory, target) = apk_fixture(serde_json::json!([
            missing_version,
            output("primary.apk", Some("x86_64"), 1),
        ]))?;
        assert_eq!(selected_file(&target, &["x86_64"])?, "primary.apk");
        Ok(())
    }

    #[test]
    fn rejects_ambiguous_and_unsupported_outputs() -> Result<()> {
        let (_directory, target) = apk_fixture(serde_json::json!([
            output("one.apk", Some("x86_64"), 7),
            output("two.apk", Some("x86_64"), 7),
        ]))?;
        assert!(
            selected_file(&target, &["x86_64"])
                .unwrap_err()
                .to_string()
                .contains("same version code")
        );
        // Lower-ranked ties must not poison a unique best candidate.
        let (_directory, target) = apk_fixture(serde_json::json!([
            output("one.apk", None, 7),
            output("two.apk", None, 7),
            output("best.apk", Some("x86_64"), 7),
        ]))?;
        assert_eq!(selected_file(&target, &["x86_64"])?, "best.apk");
        for filters in [
            serde_json::json!([{"filterType": "DENSITY", "value": "xxhdpi"}]),
            serde_json::json!([{"filterType": "UNKNOWN", "value": "x86_64"}]),
            serde_json::json!([{"filterType": "ABI", "value": ""}]),
            serde_json::json!([{"filterType": "ABI", "value": "x86_64"}, {"filterType": "ABI", "value": "x86_64"}]),
        ] {
            let mut element = output("unsupported.apk", None, 7);
            element["filters"] = filters;
            let (_directory, target) = apk_fixture(serde_json::json!([element]))?;
            assert!(selected_file(&target, &["x86_64"]).is_err());
            assert!(target.apk().is_err());
        }
        let mut element = output("config.apk", None, 7);
        element["type"] = "SPLIT".into();
        let (_directory, target) =
            apk_fixture(serde_json::json!([output("base.apk", None, 7), element]))?;
        assert!(selected_file(&target, &["x86_64"]).is_err());
        Ok(())
    }

    #[test]
    fn selected_filtered_apk_retains_filesystem_and_variant_checks() -> Result<()> {
        let (directory, target) = apk_fixture(serde_json::json!([
            output("selected.apk", Some("x86_64"), 7),
            output("other.apk", Some("arm64-v8a"), 7),
        ]))?;
        fs::remove_file(directory.path().join("other.apk"))?;
        assert_eq!(selected_file(&target, &["x86_64"])?, "selected.apk");
        let metadata_path = directory.path().join("output-metadata.json");
        let original: serde_json::Value = serde_json::from_slice(&fs::read(&metadata_path)?)?;
        fs::write(directory.path().join("selected.txt"), b"fixture")?;
        fs::write(directory.path().join("comma,apk.apk"), b"fixture")?;
        for file in [
            "../outside.apk",
            "/tmp/outside.apk",
            "missing.apk",
            "selected.txt",
            "comma,apk.apk",
        ] {
            let mut metadata = original.clone();
            metadata["elements"][0]["outputFile"] = file.into();
            fs::write(&metadata_path, serde_json::to_vec(&metadata)?)?;
            assert!(selected_file(&target, &["x86_64"]).is_err());
        }
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir()?;
            fs::write(outside.path().join("outside.apk"), b"fixture")?;
            std::os::unix::fs::symlink(
                outside.path().join("outside.apk"),
                directory.path().join("link.apk"),
            )?;
            let mut metadata = original.clone();
            metadata["elements"][0]["outputFile"] = "link.apk".into();
            fs::write(&metadata_path, serde_json::to_vec(&metadata)?)?;
            assert!(selected_file(&target, &["x86_64"]).is_err());
        }
        let mut metadata = original;
        metadata["variantName"] = "release".into();
        fs::write(&metadata_path, serde_json::to_vec(&metadata)?)?;
        assert!(selected_file(&target, &["x86_64"]).is_err());
        Ok(())
    }

    #[test]
    fn android_targets_devices_and_artifacts() -> Result<()> {
        assert_eq!(
            parse_emulators("Pixel_6a\r\nmedium_phone\nPixel_6a\n")?,
            ["Pixel_6a", "medium_phone"]
        );
        assert!(parse_emulators("")?.is_empty());
        assert!(parse_emulators("--help").is_err());
        assert!(parse_emulators("../outside").is_err());
        assert!(parse_emulators("unexpected tool output").is_err());
        let devices = parse_devices(
            "* daemon started successfully *\nList of devices attached\nemulator-5554 device product:sdk model:Pixel_6 transport_id:1\nusb-123 unauthorized usb:1\nremote:5555 offline\n",
        )?;
        assert_eq!(devices.len(), 3);
        assert_eq!(
            devices.first().map(|device| device.model.as_str()),
            Some("Pixel 6")
        );
        assert_eq!(
            devices
                .iter()
                .filter(|device| device.is_available())
                .count(),
            1
        );
        assert!(parse_devices("List of devices attached\n")?.is_empty());
        assert!(parse_devices("adb: cannot connect").is_err());
        assert!(parse_devices("List of devices attached\ninvalid").is_err());
        let serial = "adb-test-device (2)._adb-tls-connect._tcp";
        let wireless = parse_devices(&format!(
            "List of devices attached\n{serial} device product:phone model:CPH2689 device:phone transport_id:11\n"
        ))?;
        assert_eq!(
            wireless,
            vec![Device {
                serial: serial.into(),
                state: "device".into(),
                model: "CPH2689".into(),
            }]
        );
        assert!(wireless.first().is_some_and(Device::is_available));
        let unavailable = parse_devices(
            "List of devices attached\nusb-123 no permissions (user is not in the plugdev group)\n",
        )?;
        assert_eq!(
            unavailable.first().map(|device| device.state.as_str()),
            Some("no permissions")
        );

        let directory = tempfile::tempdir()?;
        let root = directory.path().join("project with spaces $literal");
        fs::create_dir(&root)?;
        fs::write(root.join("gradlew"), "")?;
        fs::write(root.join("settings.gradle.kts"), "")?;
        assert!(is_gradle_project(&root));
        assert!(!is_gradle_project(directory.path()));
        let output_listing = root.join("redirect.txt");
        let description = format!(
            "Task: :mobile:application\n  Variants:\n    Variant: freeDebug\n      Output Listing File: {}\n",
            output_listing.display()
        );
        let targets = parse_targets(&description)?;
        let target = targets.first().context("Expected the described target")?;
        assert_eq!(
            target.gradle_task("assemble", ""),
            ":mobile:application:assembleFreeDebug"
        );
        assert_eq!(
            target.gradle_task("test", "UnitTest"),
            ":mobile:application:testFreeDebugUnitTest"
        );
        assert!(
            parse_targets("Task: :app\n Variant: debug\n Output Listing File: relative.json")
                .is_err()
        );
        assert!(parse_targets("Output Listing File: /tmp/metadata.json").is_err());
        assert!(parse_targets("No variants").is_err());
        let hyphenated = parse_targets(&format!(
            "Task: :app\n Variant: debug-non-debuggable\n Output Listing File: {}\n",
            output_listing.display()
        ))?;
        assert_eq!(
            hyphenated
                .first()
                .map(|target| target.gradle_task("assemble", "")),
            Some(":app:assembleDebug-non-debuggable".into())
        );
        assert!(parse_targets(&format!("{description}{description}")).is_err());
        assert!(target.apk_paths().is_err());

        fs::write(
            &output_listing,
            "#- File Locator -\nlistingFile=output-metadata.json\n",
        )?;
        fs::write(root.join("app.apk"), b"test fixture")?;
        let metadata_path = root.join("output-metadata.json");
        let metadata = serde_json::json!({"version": 3, "artifactType": {"type": "APK"}, "variantName": "freeDebug", "applicationId": "dev.zed.sample", "elements": [{"filters": [], "outputFile": "app.apk"}]});
        fs::write(&metadata_path, serde_json::to_vec(&metadata)?)?;
        assert_eq!(
            target.apk_paths()?,
            vec![root.join("app.apk").canonicalize()?]
        );
        assert_eq!(target.apk()?.debug_application_id()?, "dev.zed.sample");
        for invalid_id in ["", "one", "dev..app", "dev.app;id", "dev.1app", "dev.app\n"] {
            let apk = AndroidApk {
                paths: Vec::new(),
                application_id: Some(invalid_id.into()),
            };
            assert!(apk.debug_application_id().is_err());
        }
        for field in ["../app.apk", "/tmp/app.apk", "missing.apk"] {
            let mut invalid = metadata.clone();
            invalid["elements"][0]["outputFile"] = field.into();
            fs::write(&metadata_path, serde_json::to_vec(&invalid)?)?;
            assert!(target.apk_paths().is_err());
        }
        let mut invalid = metadata.clone();
        invalid["variantName"] = "release".into();
        fs::write(&metadata_path, serde_json::to_vec(&invalid)?)?;
        assert!(target.apk_paths().is_err());
        let mut invalid = metadata;
        invalid["elements"][0]["filters"] =
            serde_json::json!([{"filterType": "ABI", "value": "x86"}]);
        fs::write(&metadata_path, serde_json::to_vec(&invalid)?)?;
        assert!(target.apk_paths().is_err());
        Ok(())
    }
}
