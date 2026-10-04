use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fs, io::Read as _, path::Path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    pub name: String,
}

pub fn processes(jdwp: &str, process_list: &str, application: &str) -> Result<Vec<Process>> {
    ensure!(
        jdwp.len() <= 65536 && process_list.len() <= 1024 * 1024,
        "Android process list is too large"
    );
    let debuggable = jdwp
        .split_whitespace()
        .map(|pid| {
            let pid = pid.parse::<u32>().context("Invalid JDWP process ID")?;
            ensure!(pid > 0, "Invalid zero JDWP process ID");
            Ok(pid)
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let mut processes = Vec::new();
    for line in process_list.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(name)) = (fields.next(), fields.next()) else {
            continue;
        };
        let Ok(pid) = pid.parse::<u32>() else {
            continue;
        };
        if debuggable.contains(&pid)
            && (name == application
                || name
                    .strip_prefix(application)
                    .is_some_and(|suffix| suffix.starts_with(':')))
        {
            processes.push(Process {
                pid,
                name: name.into(),
            });
        }
    }
    processes.sort_by(|left, right| left.name.cmp(&right.name).then(left.pid.cmp(&right.pid)));
    processes.dedup();
    Ok(processes)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunConfiguration {
    pub name: String,
    #[serde(default)]
    pub activity: Option<String>,
    #[serde(default)]
    pub deep_link: Option<String>,
    #[serde(default)]
    pub flags: u32,
    #[serde(default)]
    pub process: Option<String>,
    #[serde(default)]
    pub wait_for_debugger: bool,
    #[serde(default)]
    pub force_stop: bool,
}

pub fn read_configurations(root: &Path) -> Result<Vec<RunConfiguration>> {
    let file = match fs::File::open(root.join(".zed/android-run.json")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65536, ".zed/android-run.json exceeds 64 KiB");
    let configurations: Vec<RunConfiguration> =
        serde_json::from_slice(&bytes).context("Invalid .zed/android-run.json")?;
    ensure!(
        configurations.len() <= 100,
        "At most 100 Android run configurations are supported"
    );
    let mut names = BTreeSet::new();
    for configuration in &configurations {
        ensure!(
            !configuration.name.trim().is_empty()
                && configuration.name.len() <= 120
                && names.insert(&configuration.name),
            "Run configuration names must be nonempty and unique"
        );
        ensure!(
            configuration.activity.is_some() || configuration.deep_link.is_some(),
            "Specify an activity or deepLink for {}",
            configuration.name
        );
        if let Some(process) = &configuration.process {
            ensure!(
                !process.is_empty()
                    && process.len() <= 512
                    && process
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric()
                            || "._:".contains(character)),
                "Invalid process in {}",
                configuration.name
            );
        }
        if let Some(activity) = &configuration.activity {
            ensure!(
                !activity.is_empty()
                    && activity.len() <= 512
                    && activity
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric()
                            || "._$/".contains(character)),
                "Invalid activity in {}",
                configuration.name
            );
        }
        if let Some(link) = &configuration.deep_link {
            ensure!(
                link.len() <= 4096 && link.contains(':') && !link.chars().any(char::is_control),
                "Invalid deepLink in {}",
                configuration.name
            );
        }
    }
    Ok(configurations)
}

// adb joins shell arguments before sending them to Android's shell.
pub fn shell_argument(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

impl RunConfiguration {
    pub fn process_name(&self, application: &str) -> Result<String> {
        let name = self.process.as_deref().unwrap_or(application);
        let name = if name.starts_with(':') {
            format!("{application}{name}")
        } else {
            name.to_owned()
        };
        ensure!(
            name == application
                || name
                    .strip_prefix(application)
                    .is_some_and(|suffix| suffix.starts_with(':') && suffix.len() > 1),
            "Debug process must belong to {application}"
        );
        Ok(name)
    }

    pub fn launch_arguments(&self, application: &str, debug: bool) -> Result<Vec<String>> {
        let mut arguments = vec!["shell".into(), "am".into(), "start".into()];
        if self.force_stop {
            arguments.push("-S".into());
        }
        if debug && self.wait_for_debugger {
            arguments.push("-D".into());
        } else {
            arguments.push("-W".into());
        }
        if let Some(activity) = &self.activity {
            let component = if activity.contains('/') {
                activity.clone()
            } else {
                format!("{application}/{activity}")
            };
            ensure!(
                component
                    .split_once('/')
                    .is_some_and(|(package, _)| package == application),
                "Activity must belong to the installed application {application}"
            );
            arguments.extend(["-n".into(), shell_argument(&component)]);
        }
        if let Some(link) = &self.deep_link {
            arguments.extend([
                "-a".into(),
                "android.intent.action.VIEW".into(),
                "-d".into(),
                shell_argument(link),
                "-p".into(),
                shell_argument(application),
            ]);
        }
        if self.flags != 0 {
            arguments.extend(["-f".into(), self.flags.to_string()]);
        }
        Ok(arguments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_selects_debuggable_package_processes() -> Result<()> {
        let found = processes(
            "10\n11\n12",
            "PID NAME\n10 dev.app\n11 dev.app:worker\n12 dev.application\n13 dev.app:other",
            "dev.app",
        )?;
        assert_eq!(
            found,
            vec![
                Process {
                    pid: 10,
                    name: "dev.app".into()
                },
                Process {
                    pid: 11,
                    name: "dev.app:worker".into()
                }
            ]
        );
        assert!(processes("0", "PID NAME", "dev.app").is_err());
        assert!(processes("oops", "PID NAME", "dev.app").is_err());
        Ok(())
    }
    #[test]
    fn secondary_process_and_launch_failures_are_explicit() -> Result<()> {
        let configuration = RunConfiguration {
            process: Some(":worker".into()),
            ..Default::default()
        };
        assert_eq!(configuration.process_name("dev.app")?, "dev.app:worker");
        let foreign = RunConfiguration {
            process: Some("other.app:worker".into()),
            ..Default::default()
        };
        assert!(foreign.process_name("dev.app").is_err());
        for output in [
            "Starting: Intent {}\nError: Activity class does not exist",
            "Error: Activity not started, unable to resolve Intent",
            "Exception: SecurityException",
        ] {
            assert!(validate_launch_output(output).is_err());
        }
        validate_launch_output("Starting: Intent {}\nStatus: ok\nComplete")?;
        Ok(())
    }

    #[test]
    fn validates_reusable_launches_and_quotes_links() -> Result<()> {
        let directory = tempfile::tempdir()?;
        fs::create_dir(directory.path().join(".zed"))?;
        let path = directory.path().join(".zed/android-run.json");
        fs::write(
            &path,
            r#"[{"name":"Profile","activity":".MainActivity","deepLink":"sample://profile?name=O'Brien&value=$(id)","forceStop":true,"waitForDebugger":true,"flags":268435456}]"#,
        )?;
        let configuration = read_configurations(directory.path())?.remove(0);
        let arguments = configuration.launch_arguments("dev.app", true)?;
        assert!(arguments.contains(&"'dev.app/.MainActivity'".into()));
        assert!(arguments.contains(&"'sample://profile?name=O'\\''Brien&value=$(id)'".into()));
        assert!(arguments.contains(&"-D".into()));
        assert!(arguments.contains(&"-S".into()));
        for content in [
            r#"[{"name":"x"}]"#,
            r#"[{"name":"x","activity":".A","unknown":true}]"#,
            r#"[{"name":"x","activity":";echo"}]"#,
            r#"[{"name":"x","activity":".A"},{"name":"x","activity":".B"}]"#,
        ] {
            fs::write(&path, content)?;
            assert!(read_configurations(directory.path()).is_err());
        }
        let wrong = RunConfiguration {
            activity: Some("other.app/.A".into()),
            ..Default::default()
        };
        assert!(wrong.launch_arguments("dev.app", false).is_err());
        Ok(())
    }
}

pub fn validate_launch_output(output: &str) -> Result<()> {
    ensure!(
        !output
            .lines()
            .any(|line| line.trim_start().starts_with("Error:")
                || line.trim_start().starts_with("Exception:")),
        "Android activity launch failed: {}",
        output.chars().take(4000).collect::<String>()
    );
    Ok(())
}
