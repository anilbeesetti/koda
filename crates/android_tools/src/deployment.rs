use anyhow::{Context as _, Result, bail, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApkIdentity {
    pub application_id: String,
    pub debuggable: bool,
    pub launchable_component: Option<String>,
}

pub fn validate_application_id(package: &str) -> Result<()> {
    ensure!(
        package.contains('.')
            && package.split('.').all(|segment| {
                segment
                    .chars()
                    .next()
                    .is_some_and(|first| first.is_ascii_alphabetic())
                    && segment
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '_')
            }),
        "The APK has an invalid application ID: {package:?}"
    );
    Ok(())
}

pub fn parse_apk_badging(
    output: &str,
    metadata_application_id: Option<&str>,
    debug: bool,
) -> Result<ApkIdentity> {
    validate_output(output)?;
    let mut application_id = None;
    let mut application_seen = false;
    let mut application_flags = false;
    let mut debuggable = false;
    let mut launchable_activities = Vec::new();
    for line in output.lines().map(str::trim) {
        ensure!(!is_error(line), "Unable to inspect the APK: {line}");
        if line.starts_with("package:") {
            ensure!(application_id.is_none(), "AAPT2 returned multiple packages");
            let fields = line
                .strip_prefix("package: name='")
                .context("AAPT2 did not return a valid APK package record")?;
            let (package, remainder) = fields
                .split_once('\'')
                .context("AAPT2 returned an unterminated APK package name")?;
            ensure!(
                remainder.is_empty() || remainder.starts_with(' '),
                "AAPT2 returned an invalid APK package record"
            );
            validate_application_id(package)?;
            application_id = Some(package.to_owned());
            application_flags = false;
        } else if line.starts_with("application:") {
            ensure!(
                application_id.is_some() && !application_seen,
                "AAPT2 returned an unexpected APK application record"
            );
            validate_application_record(line)?;
            application_seen = true;
            application_flags = true;
        } else if line.starts_with("application-debuggable") {
            ensure!(
                line == "application-debuggable" && application_flags && !debuggable,
                "AAPT2 returned an invalid APK debuggable record"
            );
            debuggable = true;
        } else if line.starts_with("launchable-activity:") {
            let fields = line
                .strip_prefix("launchable-activity: name='")
                .context("AAPT2 returned an invalid launcher activity record")?;
            let (name, remainder) = fields
                .split_once('\'')
                .context("AAPT2 returned an unterminated launcher activity name")?;
            ensure!(
                remainder.starts_with(' '),
                "AAPT2 returned an invalid launcher activity record"
            );
            launchable_activities.push(name.to_owned());
            application_flags = false;
        } else if application_flags
            && (line == "application-isGame" || line.starts_with("testOnly='"))
        {
            // AAPT2 emits these application attributes before the debuggable flag.
        } else if !line.is_empty() {
            application_flags = false;
        }
    }
    let application_id = application_id.context(
        "AAPT2 did not return an APK package. Build again or repair SDK build-tools in Android setup.",
    )?;
    if let Some(expected) = metadata_application_id {
        validate_application_id(expected).context("The Gradle APK metadata is invalid")?;
        ensure!(
            application_id == expected,
            "The APK package ({application_id}) differs from Gradle metadata ({expected}). Sync and rebuild before running."
        );
    }
    ensure!(
        !debug || debuggable,
        "The selected APK is not debuggable. Select and build a debuggable variant before starting Debug."
    );
    let mut launchable_component = None;
    for activity in &launchable_activities {
        let activity = if activity.contains('.') {
            activity.clone()
        } else {
            format!("{application_id}.{activity}")
        };
        let component = format!("{application_id}/{activity}");
        validate_component(&component, &application_id)?;
        if launchable_activities.len() == 1 {
            launchable_component = Some(component);
        }
    }
    Ok(ApkIdentity {
        application_id,
        debuggable,
        launchable_component,
    })
}

fn validate_application_record(line: &str) -> Result<()> {
    let attributes = line
        .strip_prefix("application: label='")
        .context("AAPT2 returned an invalid application record")?;
    // AAPT2 normalizes newlines in labels but leaves apostrophes unescaped.
    let (_, icon) = attributes
        .rsplit_once("' icon='")
        .context("AAPT2 returned an invalid application icon attribute")?;
    ensure!(
        icon.ends_with('\''),
        "AAPT2 returned an unterminated application record"
    );
    Ok(())
}

pub fn validate_component(component: &str, package: &str) -> Result<()> {
    validate_application_id(package)?;
    let (component_package, class) = component
        .split_once('/')
        .context("Android did not return a launcher component")?;
    ensure!(
        component_package == package,
        "Android resolved a launcher outside the expected package ({package}): {component:?}"
    );
    let class = class.strip_prefix('.').unwrap_or(class);
    ensure!(
        !class.is_empty()
            && class.split('.').all(|segment| {
                segment
                    .chars()
                    .next()
                    .is_some_and(|first| first.is_ascii_alphabetic() || matches!(first, '_' | '$'))
                    && segment.chars().all(|character| {
                        character.is_ascii_alphanumeric() || matches!(character, '_' | '$')
                    })
            }),
        "Android returned an invalid launcher component: {component:?}"
    );
    Ok(())
}

fn validate_serial(serial: &str) -> Result<()> {
    ensure!(
        !serial.trim().is_empty()
            && !serial.starts_with('-')
            && !serial.chars().any(char::is_control),
        "The selected Android device has an invalid serial"
    );
    Ok(())
}

fn apk_argument(apk: &Path) -> Result<String> {
    ensure!(
        apk.is_absolute()
            && apk.is_file()
            && apk.extension().is_some_and(|extension| extension == "apk"),
        "Build the selected standalone APK before running it"
    );
    let argument = apk
        .to_str()
        .context("The APK path cannot be represented as a command argument")?;
    ensure!(
        !argument.contains('\0'),
        "The APK path contains an invalid command character"
    );
    Ok(argument.to_owned())
}

pub fn install_arguments(serial: &str, apks: &[PathBuf]) -> Result<Vec<String>> {
    validate_serial(serial)?;
    ensure!(
        apks.len() == 1,
        "Run and Debug support one standalone APK. Split APK sets are not supported."
    );
    let apk = apks
        .first()
        .context("Build the selected APK before running")?;
    Ok(vec![
        "-s".into(),
        serial.into(),
        "install".into(),
        "-r".into(),
        "-t".into(),
        apk_argument(apk)?,
    ])
}

fn shell_arguments(serial: &str, arguments: &[&str]) -> Result<Vec<String>> {
    validate_serial(serial)?;
    // ADB joins shell arguments before sending them to the device shell, so host
    // argv boundaries alone do not protect a Java class containing '$'.
    let command = arguments
        .iter()
        .map(|argument| format!("'{}'", argument.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(vec!["-s".into(), serial.into(), "shell".into(), command])
}

pub fn resolve_activity_arguments(serial: &str, package: &str) -> Result<Vec<String>> {
    validate_application_id(package)?;
    shell_arguments(
        serial,
        &[
            "cmd",
            "package",
            "resolve-activity",
            "--components",
            "--user",
            "current",
            "-a",
            "android.intent.action.MAIN",
            "-c",
            "android.intent.category.LAUNCHER",
            "-p",
            package,
        ],
    )
}

pub fn parse_launcher(output: &str, package: &str) -> Result<String> {
    validate_output(output)?;
    validate_application_id(package)?;
    let mut lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let component = lines.next().context(
        "Android did not resolve a launcher. The selected application needs a MAIN/LAUNCHER activity for the current user.",
    )?;
    ensure!(
        lines.next().is_none(),
        "Android returned multiple or ambiguous launchers for {package}"
    );
    ensure!(
        component != "No activity found" && !is_error(component),
        "Android could not resolve a MAIN/LAUNCHER activity for {package}: {component}"
    );
    validate_component(component, package)?;
    Ok(component.to_owned())
}

pub fn device_api_level(properties: &str) -> Result<Option<u32>> {
    validate_output(properties)?;
    let mut api_level = None;
    for line in properties.lines().map(str::trim) {
        if !line.starts_with("[ro.build.version.sdk]") {
            continue;
        }
        ensure!(
            api_level.is_none(),
            "Android returned duplicate SDK version properties"
        );
        let value = line
            .strip_prefix("[ro.build.version.sdk]: [")
            .and_then(|value| value.strip_suffix(']'))
            .context("Android returned an invalid SDK version property")?;
        ensure!(
            !value.is_empty() && value.chars().all(|value| value.is_ascii_digit()),
            "Android returned an invalid SDK version: {value:?}"
        );
        let value = value
            .parse::<u32>()
            .context("Android SDK version is out of range")?;
        ensure!(
            value > 0,
            "Android returned an invalid SDK version: {value}"
        );
        api_level = Some(value);
    }
    Ok(api_level)
}

pub fn start_activity_arguments(serial: &str, component: &str, debug: bool) -> Result<Vec<String>> {
    let (package, _) = component
        .split_once('/')
        .context("Android did not return a launcher component")?;
    validate_component(component, package)?;
    let mut arguments = vec!["am", "start", "-S"];
    if debug {
        // Waiting for launch completion would deadlock before the debugger attaches.
        arguments.push("-D");
    } else {
        arguments.push("-W");
    }
    arguments.extend([
        "--user",
        "current",
        "-a",
        "android.intent.action.MAIN",
        "-c",
        "android.intent.category.LAUNCHER",
        "-n",
        component,
    ]);
    shell_arguments(serial, &arguments)
}

fn validate_output(output: &str) -> Result<()> {
    ensure!(
        output.len() <= 8 * 1024 * 1024
            && !output
                .chars()
                .any(|character| character.is_control() && !matches!(character, '\r' | '\n' | '\t')),
        "The Android tool returned invalid or excessive output"
    );
    Ok(())
}

fn is_error(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.starts_with("error")
        || line.starts_with("failure")
        || line.starts_with("failed")
        || line.starts_with("adb:")
        || line.starts_with("exception")
        || line.starts_with("securityexception")
        || line.starts_with("java.lang.")
        || line.starts_with("permission denial")
}

pub fn validate_install_output(output: &str) -> Result<()> {
    validate_output(output)?;
    let mut successes = 0;
    for line in output.lines().map(str::trim) {
        ensure!(!is_error(line), "APK installation failed: {line}");
        successes += usize::from(line == "Success");
    }
    ensure!(
        successes == 1,
        "ADB did not confirm APK installation. Check the selected device and retry."
    );
    Ok(())
}

pub fn validate_start_output(output: &str) -> Result<()> {
    validate_output(output)?;
    let mut started = false;
    for line in output.lines().map(str::trim) {
        ensure!(!is_error(line), "Android could not start the app: {line}");
        if let Some(status) = line.strip_prefix("Status:") {
            ensure!(status.trim() == "ok", "Android launch failed: {line}");
        }
        if let Some(warning) = line.strip_prefix("Warning: Activity not started") {
            ensure!(
                warning == ", intent has been delivered to currently running top-most instance."
                    || warning == ", its current task has been brought to the front",
                "Android did not start the selected app: {line}"
            );
        }
        started |= line.starts_with("Starting: Intent {");
    }
    ensure!(
        started,
        "Android did not confirm app launch. Check the selected device and launcher activity."
    );
    Ok(())
}

pub fn validate_debug_start_output(output: &str) -> Result<()> {
    validate_start_output(output)?;
    ensure!(
        !output
            .lines()
            .any(|line| line.trim().starts_with("Warning: Activity not started")),
        "Android kept an existing activity instead of starting a debugger waiter. Stop the application and retry Debug."
    );
    Ok(())
}

pub fn emulator_kill_arguments(serial: &str) -> Result<Vec<String>> {
    validate_serial(serial)?;
    let port = serial
        .strip_prefix("emulator-")
        .filter(|port| !port.is_empty() && port.chars().all(|value| value.is_ascii_digit()))
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port > 0)
        .context("Stop Emulator requires a selected running Android emulator")?;
    ensure!(
        port % 2 == 0,
        "The selected emulator console port is invalid"
    );
    Ok(vec![
        "-s".into(),
        serial.into(),
        "emu".into(),
        "kill".into(),
    ])
}

pub fn validate_emulator_kill_output(output: &str) -> Result<()> {
    validate_output(output)?;
    let mut acknowledged = false;
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let lower = line.to_ascii_lowercase();
        ensure!(
            !is_error(line)
                && !lower.starts_with("ko")
                && !lower.contains("authentication")
                && !lower.contains("unauthorized"),
            "Android could not stop the selected emulator: {line}"
        );
        acknowledged |= line == "OK" || line == "OK: killing emulator, bye bye";
    }
    ensure!(
        acknowledged,
        "The emulator did not acknowledge shutdown. Check its state and retry."
    );
    Ok(())
}

pub fn aapt2_path() -> Result<PathBuf> {
    let selected = crate::managed::environment()?;
    let sdk = selected.sdk.or_else(crate::sdk_root).context(
        "Choose an Android SDK in Android setup before running. SDK build-tools are required to inspect the APK.",
    )?;
    crate::provision::validate_managed_path(&sdk)?;
    let executable = aapt2_path_in(&sdk)?;
    crate::provision::validate_managed_path(&executable)?;
    Ok(executable)
}

fn stable_build_tools_version(name: &str) -> Option<(u32, u32, u32)> {
    let mut parts = name.split('.');
    let parse = |part: &str| {
        (!part.is_empty() && part.chars().all(|value| value.is_ascii_digit()))
            .then(|| part.parse::<u32>().ok())
            .flatten()
    };
    let major = parse(parts.next()?)?;
    let minor = parse(parts.next()?)?;
    let patch = parse(parts.next()?)?;
    parts.next().is_none().then_some((major, minor, patch))
}

fn aapt2_path_in(sdk: &Path) -> Result<PathBuf> {
    ensure!(sdk.is_absolute(), "Choose an absolute Android SDK path");
    let mut versions = Vec::new();
    for (index, entry) in fs::read_dir(sdk.join("build-tools"))
        .context("The Android SDK has no build-tools. Repair tools in Android setup.")?
        .enumerate()
    {
        ensure!(
            index < 1000,
            "The Android SDK has too many build-tools directories"
        );
        let entry = entry?;
        if let Some(version) = entry
            .file_name()
            .to_str()
            .and_then(stable_build_tools_version)
        {
            versions.push((version, entry.path()));
        }
    }
    versions.sort_by(|left, right| right.cmp(left));
    for (_, directory) in versions {
        let executable = directory.join(if cfg!(windows) { "aapt2.exe" } else { "aapt2" });
        if directory.join("lib/d8.jar").is_file() && crate::managed::executable(&executable).is_ok()
        {
            return executable
                .canonicalize()
                .with_context(|| format!("Unable to resolve SDK AAPT2: {}", executable.display()));
        }
    }
    bail!(
        "The Android SDK has no complete stable build-tools (executable aapt2 and lib/d8.jar). Repair tools in Android setup."
    )
}

pub fn aapt2_arguments(apk: &Path) -> Result<Vec<String>> {
    Ok(vec!["dump".into(), "badging".into(), apk_argument(apk)?])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn badging(package: &str, debuggable: bool) -> String {
        format!(
            "package: name='{package}' versionCode='1' versionName='1.0'\nsdkVersion:'23'\napplication-label:'Test'\napplication: label='Test' icon='res/mipmap/icon.png'\n{}launchable-activity: name='other.namespace.Launcher' label='' icon=''\n",
            if debuggable {
                "application-debuggable\n"
            } else {
                ""
            }
        )
    }

    #[test]
    fn apk_identity_is_checked_against_actual_badging_and_gradle_metadata() -> Result<()> {
        let output = badging("com.example.app.debug", true);
        assert_eq!(
            parse_apk_badging(&output, Some("com.example.app.debug"), true)?,
            ApkIdentity {
                application_id: "com.example.app.debug".into(),
                debuggable: true,
                launchable_component: Some("com.example.app.debug/other.namespace.Launcher".into()),
            }
        );
        assert!(parse_apk_badging(&output, Some("com.example.app"), false).is_err());
        assert!(parse_apk_badging(&output, Some(""), false).is_err());
        assert!(!parse_apk_badging(&badging("com.example.app", false), None, false)?.debuggable);
        assert!(parse_apk_badging(&badging("com.example.app", false), None, true).is_err());
        assert!(parse_apk_badging("", Some("com.example.app"), false).is_err());
        assert!(
            parse_apk_badging("Error: cannot read APK", Some("com.example.app"), false).is_err()
        );
        let apostrophe_label = output.replace("label='Test'", "label='Joe's app'");
        assert!(parse_apk_badging(&apostrophe_label, None, true)?.debuggable);
        Ok(())
    }

    #[test]
    fn malformed_badging_cannot_supply_a_package_or_debug_flag() {
        for output in [
            "package: name='com.example.app'evil\n",
            "package: name='com.example.app;id' versionCode='1'\n",
            "package: name='com.example.app'\npackage: name='com.other.app'\n",
            "package: name='com.example.app'\napplication-debuggable\n",
            "package: name='com.example.app'\napplication: label='unterminated icon='x'\napplication-debuggable\n",
            "package: name='com.example.app'\napplication: label='' icon=''\napplication-debuggable=true\n",
            "package: name='com.example.app'\napplication: label='' icon=''\napplication-debuggable\napplication-debuggable\n",
        ] {
            assert!(parse_apk_badging(output, None, true).is_err(), "{output}");
        }
        let forged = format!(
            "{}application-debuggable\n",
            badging("com.example.app", false)
        );
        assert!(parse_apk_badging(&forged, None, true).is_err());
    }

    #[test]
    fn legacy_launcher_uses_actual_package_and_requires_one_valid_activity() -> Result<()> {
        let output = badging("com.example.debug", true);
        for (name, expected) in [
            (
                "other.namespace.Launcher",
                "com.example.debug/other.namespace.Launcher",
            ),
            (".LauncherAlias", "com.example.debug/.LauncherAlias"),
            (
                "Main$Alias",
                "com.example.debug/com.example.debug.Main$Alias",
            ),
        ] {
            let output = output.replace("other.namespace.Launcher", name);
            assert_eq!(
                parse_apk_badging(&output, None, true)?
                    .launchable_component
                    .as_deref(),
                Some(expected)
            );
        }
        let missing = output
            .lines()
            .filter(|line| !line.starts_with("launchable-activity:"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            parse_apk_badging(&missing, None, true)?
                .launchable_component
                .is_none()
        );
        for extra in [
            "launchable-activity: name='other.namespace.Launcher' label='' icon=''\n",
            "launchable-activity: name='.Second' label='' icon=''\n",
        ] {
            let ambiguous = format!("{output}{extra}");
            assert!(
                parse_apk_badging(&ambiguous, None, true)?
                    .launchable_component
                    .is_none()
            );
        }
        for name in ["", ".Main;id", "com.other/.Main", ".Main$(id)"] {
            assert!(
                parse_apk_badging(
                    &output.replace("other.namespace.Launcher", name),
                    None,
                    true
                )
                .is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn only_a_valid_sdk_property_can_select_legacy_android_resolution() -> Result<()> {
        assert_eq!(
            device_api_level("[ro.build.version.sdk]: [21]\n[ro.product.cpu.abilist]: [x86_64]\n")?,
            Some(21)
        );
        assert_eq!(
            device_api_level("[ro.build.version.sdk]: [23]\r\n")?,
            Some(23)
        );
        assert_eq!(
            device_api_level("[ro.build.version.sdk]: [37]\n")?,
            Some(37)
        );
        assert_eq!(device_api_level("[ro.product.model]: [API 21]\n")?, None);
        assert_eq!(device_api_level("")?, None);
        for properties in [
            "[ro.build.version.sdk]: [21]\n[ro.build.version.sdk]: [23]\n",
            "[ro.build.version.sdk]: [0]\n",
            "[ro.build.version.sdk]: [-21]\n",
            "[ro.build.version.sdk]: [+21]\n",
            "[ro.build.version.sdk]: [21;id]\n",
            "[ro.build.version.sdk]: [4294967296]\n",
            "[ro.build.version.sdk]: [21\n",
        ] {
            assert!(device_api_level(properties).is_err(), "{properties}");
        }
        Ok(())
    }

    #[test]
    fn resolver_accepts_current_package_aliases_and_rejects_ambiguous_or_foreign_launchers()
    -> Result<()> {
        for component in [
            "com.example.app/.LauncherAlias",
            "com.example.app/com.base.namespace.MainActivity",
            "com.example.app/.MainActivity$Alias",
        ] {
            assert_eq!(
                parse_launcher(&format!("{component}\r\n"), "com.example.app")?,
                component
            );
        }
        for output in [
            "",
            "No activity found",
            "android/com.android.internal.app.ResolverActivity",
            "com.other.app/.Launcher",
            "com.example.app/.One\ncom.example.app/.Two",
            "com.example.app/.One\ncom.example.app/.One",
            "com.example.app/.Main;id",
            "com.example.app/.Main$(id)",
            "com.example.app/../Main",
            "com.example.app/.Main/extra",
            "com.example.app/",
            "Error: permission denied",
        ] {
            assert!(
                parse_launcher(output, "com.example.app").is_err(),
                "{output}"
            );
        }
        Ok(())
    }

    #[test]
    fn exact_standalone_apk_paths_survive_native_install_arguments() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let apk = directory.path().join("app 日本語, quote' $debug.apk");
        fs::write(&apk, b"fixture")?;
        let arguments = install_arguments(
            "wireless device._adb-tls-connect._tcp",
            std::slice::from_ref(&apk),
        )?;
        assert_eq!(
            arguments.last(),
            apk.to_str().map(|path| path.to_owned()).as_ref()
        );
        assert_eq!(
            arguments.get(1).map(String::as_str),
            Some("wireless device._adb-tls-connect._tcp")
        );
        assert!(arguments.iter().any(|argument| argument == "-t"));
        assert_eq!(aapt2_arguments(&apk)?.last(), arguments.last());
        assert!(install_arguments("usb", &[]).is_err());
        assert!(install_arguments("usb", &[apk.clone(), apk]).is_err());
        assert!(
            install_arguments("usb\n-s other", &[directory.path().join("missing.apk")]).is_err()
        );
        Ok(())
    }

    #[test]
    fn shell_commands_quote_components_and_debug_does_not_wait_for_attach() -> Result<()> {
        let component = "com.example.app/.Main$Alias";
        let debug = start_activity_arguments("emulator-5554", component, true)?;
        let command = debug.last().context("Missing debug command")?;
        assert!(command.contains("'-S' '-D'"));
        assert!(!command.contains("'-W'"));
        assert!(command.contains("'com.example.app/.Main$Alias'"));
        assert!(command.contains("'--user' 'current'"));
        let run = start_activity_arguments("emulator-5554", component, false)?;
        assert!(
            run.last()
                .is_some_and(|command| command.contains("'-S' '-W'"))
        );
        let resolve = resolve_activity_arguments("emulator-5554", "com.example.app")?;
        let command = resolve.last().context("Missing resolution command")?;
        assert!(command.contains("'--components' '--user' 'current'"));
        assert!(command.contains("'android.intent.action.MAIN'"));
        assert!(command.contains("'android.intent.category.LAUNCHER'"));
        assert!(
            start_activity_arguments("emulator-5554", "com.example.app/.Main;id", false).is_err()
        );
        assert!(resolve_activity_arguments("emulator-5554", "com.example.app;id").is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn device_shell_receives_a_literal_nested_class_alias() -> Result<()> {
        use smol::process::Command;
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir()?;
        let activity_manager = directory.path().join("am");
        fs::write(&activity_manager, b"#!/bin/sh\nprintf '%s\\n' \"$@\"\n")?;
        fs::set_permissions(&activity_manager, fs::Permissions::from_mode(0o755))?;
        let component = "com.example.app/.Main$Alias";
        let arguments = start_activity_arguments("emulator-5554", component, true)?;
        let command = arguments.last().context("Missing device shell command")?;
        let output = smol::block_on(
            Command::new("/bin/sh")
                .arg("-c")
                .arg(command)
                .env("PATH", directory.path())
                .env("Alias", "expanded_incorrectly")
                .output(),
        )?;
        ensure!(output.status.success(), "The shell fixture failed");
        let output = String::from_utf8(output.stdout)?;
        assert_eq!(output.lines().last(), Some(component));
        assert!(output.lines().any(|argument| argument == "-D"));
        assert!(!output.contains("expanded_incorrectly"));
        Ok(())
    }

    #[test]
    fn zero_exit_android_failures_do_not_count_as_success() -> Result<()> {
        validate_install_output("Performing Streamed Install\nSuccess\n")?;
        for output in [
            "Performing Streamed Install\n",
            "Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE]\n",
            "Success\nadb: failed to install package\n",
            "Success\nFailure [INSTALL_FAILED_TEST_ONLY]\n",
            "Success\nSuccess\n",
        ] {
            assert!(validate_install_output(output).is_err(), "{output}");
        }
        validate_start_output(
            "Starting: Intent { act=android.intent.action.MAIN cmp=com.example.app/.Main }\n",
        )?;
        validate_start_output(
            "Starting: Intent { cmp=com.example.app/.Main }\nStatus: ok\nComplete\n",
        )?;
        for output in [
            "",
            "Starting: Intent { cmp=com.example.app/.Main }\nError type 3\nError: Activity class does not exist\n",
            "Starting: Intent { cmp=com.example.app/.Main }\nStatus: timeout\n",
            "Starting: Intent { cmp=com.example.app/.Main }\nWarning: Activity not started because intent should be handled by the caller\n",
            "Starting: Intent { cmp=com.example.app/.Main }\nSecurityException: Permission Denial\n",
        ] {
            assert!(validate_start_output(output).is_err(), "{output}");
        }
        Ok(())
    }

    #[test]
    fn debug_requires_a_new_waiter_instead_of_delivering_to_an_existing_activity() -> Result<()> {
        let started = "Starting: Intent { cmp=com.example.app/.Main }\n";
        validate_debug_start_output(started)?;
        for warning in [
            "Warning: Activity not started, intent has been delivered to currently running top-most instance.",
            "Warning: Activity not started, its current task has been brought to the front",
        ] {
            let output = format!("{started}{warning}\n");
            validate_start_output(&output)?;
            assert!(validate_debug_start_output(&output).is_err());
        }
        assert!(
            validate_debug_start_output(&format!(
                "{started}Error: Activity not started, unable to resolve Intent\n"
            ))
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn emulator_shutdown_requires_selected_emulator_and_console_acknowledgement() -> Result<()> {
        assert_eq!(
            emulator_kill_arguments("emulator-5554")?,
            ["-s", "emulator-5554", "emu", "kill"]
        );
        for serial in [
            "usb-123",
            "localhost:5555",
            "emulator-",
            "emulator-5555",
            "emulator-0",
            "emulator-65536",
            "emulator-5554;kill",
        ] {
            assert!(emulator_kill_arguments(serial).is_err(), "{serial}");
        }
        validate_emulator_kill_output("OK: killing emulator, bye bye\nOK\n")?;
        for output in [
            "",
            "KO: unknown command\n",
            "KO: authentication required\nOK\n",
            "Android Console: Authentication required\nOK\n",
            "adb: device offline\n",
        ] {
            assert!(validate_emulator_kill_output(output).is_err(), "{output}");
        }
        Ok(())
    }

    fn build_tools(sdk: &Path, version: &str, complete: bool) -> Result<PathBuf> {
        let directory = sdk.join("build-tools").join(version);
        fs::create_dir_all(directory.join("lib"))?;
        let executable = directory.join(if cfg!(windows) { "aapt2.exe" } else { "aapt2" });
        fs::write(&executable, b"fixture")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
        }
        if complete {
            fs::write(directory.join("lib/d8.jar"), b"fixture")?;
        }
        Ok(executable)
    }

    #[test]
    fn aapt2_discovery_prefers_complete_stable_semantic_versions() -> Result<()> {
        let sdk = tempfile::tempdir()?;
        build_tools(sdk.path(), "9.0.0", true)?;
        let selected = build_tools(sdk.path(), "35.0.0", true)?;
        build_tools(sdk.path(), "36.0.0", false)?;
        build_tools(sdk.path(), "37.0.0-rc1", true)?;
        build_tools(sdk.path(), "999.0.0.extra", true)?;
        build_tools(sdk.path(), "+999.0.0", true)?;
        assert_eq!(aapt2_path_in(sdk.path())?, selected.canonicalize()?);
        fs::remove_file(
            selected
                .parent()
                .context("Missing build-tools directory")?
                .join("lib/d8.jar"),
        )?;
        assert!(
            aapt2_path_in(sdk.path())?.ends_with(Path::new("9.0.0").join(if cfg!(windows) {
                "aapt2.exe"
            } else {
                "aapt2"
            }))
        );
        let absent = tempfile::tempdir()?;
        assert!(aapt2_path_in(absent.path()).is_err());
        assert!(aapt2_path_in(Path::new("relative-sdk")).is_err());
        Ok(())
    }
}
