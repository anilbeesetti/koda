use super::*;
use async_trait::async_trait;
use collections::HashMap;
use dap::{
    adapters::{
        DapDelegate, DebugAdapter, DebugAdapterBinary, DebugAdapterName, DebugTaskDefinition,
        StartDebuggingRequestArguments, StartDebuggingRequestArgumentsRequest,
    },
    client::SessionId,
};
use gpui::AsyncApp;
use project::debugger::dap_store::DapStoreEvent;
use serde_json::{Value, json};
use task::{DebugScenario, ZedDebugConfig};

pub(super) const ADAPTER: &str = "Android Kotlin";

// ponytail: the community adapter maps JVM lines; advanced Kotlin inline/SMAP support needs a compiler-aware adapter.
pub(super) struct AndroidKotlinAdapter;

pub(super) fn binary() -> Result<PathBuf> {
    let path = std::env::var_os("ANDROID_IDE_KOTLIN_DEBUGGER").map(PathBuf::from)
        .context("Run script/install-android-debugger, then relaunch script/android-ide to enable Android debugging.")?;
    ensure!(
        path.is_absolute() && path.is_file(),
        "ANDROID_IDE_KOTLIN_DEBUGGER must point to the installed debugger executable"
    );
    Ok(path)
}

#[async_trait(?Send)]
impl DebugAdapter for AndroidKotlinAdapter {
    fn name(&self) -> DebugAdapterName {
        ADAPTER.into()
    }

    async fn config_from_zed_format(&self, _: ZedDebugConfig) -> Result<DebugScenario> {
        bail!("Use Android: Debug to build and attach to the selected Android device")
    }

    async fn get_binary(
        &self,
        _: &Arc<dyn DapDelegate>,
        config: &DebugTaskDefinition,
        user_installed_path: Option<PathBuf>,
        user_args: Option<Vec<String>>,
        user_env: Option<HashMap<String, String>>,
        _: &mut AsyncApp,
    ) -> Result<DebugAdapterBinary> {
        let executable = match user_installed_path {
            Some(path) => path,
            None => binary()?,
        };
        ensure!(
            executable.is_absolute() && executable.is_file(),
            "Android debugger executable is missing"
        );
        ensure!(
            config.config["request"] == "attach",
            "Android debugging requires an attach configuration"
        );
        let mut envs = user_env.unwrap_or_default();
        envs.entry("JAVA_HOME".into()).or_insert(
            android_tools::kotlin::java_home()?
                .to_string_lossy()
                .into_owned(),
        );
        Ok(DebugAdapterBinary {
            command: Some(executable.to_string_lossy().into_owned()),
            arguments: user_args.unwrap_or_default(),
            envs,
            cwd: None,
            connection: None,
            request_args: StartDebuggingRequestArguments {
                request: StartDebuggingRequestArgumentsRequest::Attach,
                configuration: config.config.clone(),
            },
        })
    }

    fn dap_schema(&self) -> Value {
        json!({"type":"object", "required":["request", "hostName", "port", "projectRoot"],
            "properties": {"request":{"const":"attach"}, "hostName":{"type":"string"},
                "port":{"type":"integer", "minimum":1, "maximum":65535},
                "projectRoot":{"type":"string"}, "timeout":{"type":"integer"}}})
    }
}

pub(super) struct Forward {
    executor: BackgroundExecutor,
    adb: PathBuf,
    serial: String,
    port: u16,
    label: SharedString,
    session: Option<SessionId>,
}

impl Drop for Forward {
    fn drop(&mut self) {
        let adb = self.adb.clone();
        let args = vec![
            "-s".into(),
            self.serial.clone(),
            "forward".into(),
            "--remove".into(),
            format!("tcp:{}", self.port),
        ];
        let executor = self.executor.clone();
        self.executor
            .spawn(async move {
                tool_output(
                    adb,
                    args,
                    Path::new("/"),
                    &executor,
                    Duration::from_secs(10),
                )
                .await
                .log_err();
            })
            .detach();
    }
}

fn positive_number<T: std::str::FromStr + PartialEq + Default>(
    output: &str,
    name: &str,
) -> Result<T> {
    let text = output.trim();
    ensure!(
        !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()),
        "Invalid {name} from adb: {text}"
    );
    let value = text
        .parse::<T>()
        .map_err(|_| anyhow::anyhow!("Invalid {name} from adb: {text}"))?;
    ensure!(value != T::default(), "Invalid zero {name} from adb");
    Ok(value)
}

async fn wait_for_application_process<ReadProcess: Future<Output = Result<String>>>(
    mut read_process: impl FnMut() -> ReadProcess,
    executor: &BackgroundExecutor,
) -> Result<u32> {
    let mut attempts = 0;
    loop {
        match read_process().await {
            Ok(output) => return positive_number(&output, "application process ID"),
            Err(error) => {
                attempts += 1;
                if attempts == 20 {
                    return Err(error.context("The Android app did not start a debuggable process"));
                }
                // Android CLI can return before ActivityManager has created the process.
                executor.timer(Duration::from_millis(250)).await;
            }
        }
    }
}

impl AndroidPanel {
    pub(super) fn observe_debugger(&self, cx: &mut Context<Self>) -> Vec<Subscription> {
        let store = self.project.read(cx).dap_store();
        vec![
            cx.observe(&store, |panel, store, cx| {
                if let Some(forward) = &mut panel.debug_forward
                    && forward.session.is_none()
                {
                    forward.session = store.read(cx).sessions().find_map(|session| {
                        let session = session.read(cx);
                        (session.label().as_ref() == Some(&forward.label)
                            && session.adapter().as_ref() == ADAPTER)
                            .then(|| session.session_id())
                    });
                }
            }),
            cx.subscribe(&store, |panel, _, event, cx| {
                if let DapStoreEvent::DebugClientShutdown(id) = event
                    && panel
                        .debug_forward
                        .as_ref()
                        .is_some_and(|forward| forward.session == Some(*id))
                {
                    panel.debug_forward = None;
                    panel.status = "Android debugger disconnected".into();
                    cx.notify();
                }
            }),
        ]
    }

    pub(super) fn attach_debugger(
        &mut self,
        root: PathBuf,
        serial: String,
        application_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = self.trusted_root(cx).and_then(|current| {
            ensure!(
                current == root,
                "The Android project changed before debugger attachment"
            );
            Ok(())
        }) {
            self.fail(error, window, cx);
            return;
        }
        self.running = true;
        self.status = "Attaching Android debugger…".into();
        let executor = cx.background_executor().clone();
        self.debug_task = Some(cx.spawn_in(window, async move |panel, cx| {
            let result = cx.background_spawn({
                let root = root.clone();
                async move {
                    let adb = adb_path()?;
                    let process = wait_for_application_process(
                        || tool_output(adb.clone(), vec!["-s".into(), serial.clone(), "shell".into(), "pidof".into(), "-s".into(), application_id.clone()], &root, &executor, Duration::from_secs(1)),
                        &executor,
                    ).await?;
                    let output = tool_output(adb.clone(), vec!["-s".into(), serial.clone(), "forward".into(), "tcp:0".into(), format!("jdwp:{process}")], &root, &executor, Duration::from_secs(10)).await?;
                    let port: u16 = positive_number(&output, "debugger port")?;
                    Ok::<_, anyhow::Error>(Forward { executor, adb, serial, port,
                        label: format!("Android · {application_id} · {process}:{port}").into(), session: None })
                }
            }).await;
            panel.update_in(cx, |panel, window, cx| {
                panel.running = false;
                let result = result.and_then(|forward| {
                    ensure!(panel.trusted_root(cx)? == root, "The Android project changed during debugger attachment");
                    let worktree = panel.project.read(cx).visible_worktrees(cx)
                        .find(|worktree| worktree.read(cx).abs_path().as_ref() == root)
                        .map(|worktree| worktree.read(cx).id()).context("The Android project was closed")?;
                    let provider = panel.workspace.read_with(cx, |workspace, _| workspace.debugger_provider())?
                        .context("The native debugger is unavailable")?;
                    let scenario = DebugScenario { adapter: ADAPTER.into(), label: forward.label.clone(), build: None,
                        config: json!({"request":"attach", "hostName":"127.0.0.1", "port":forward.port, "projectRoot":root, "timeout":5000}), tcp_connection: None };
                    panel.debug_forward = Some(forward);
                    provider.start_session(scenario, TaskContext { cwd: Some(root), ..Default::default() }.into(), None, Some(worktree), window, cx);
                    panel.status = "Android debugger started. Use the debugger controls to inspect, step, resume, or disconnect.".into();
                    Ok(())
                });
                if let Err(error) = result { panel.fail(error, window, cx); }
                cx.notify();
            }).log_err();
        }));
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    async fn waits_for_application_process(executor: BackgroundExecutor) {
        let mut attempts = 0;
        let process = wait_for_application_process(
            || {
                attempts += 1;
                std::future::ready(if attempts < 3 {
                    Err(anyhow::anyhow!("process not created"))
                } else {
                    Ok("1234\n".into())
                })
            },
            &executor,
        )
        .await
        .expect("The app process should appear after two retries");
        assert_eq!(process, 1234);
        assert_eq!(attempts, 3);

        let mut attempts = 0;
        let error = wait_for_application_process(
            || {
                attempts += 1;
                std::future::ready(Err(anyhow::anyhow!("device offline")))
            },
            &executor,
        )
        .await
        .expect_err("An unavailable device must stop retrying");
        assert_eq!(attempts, 20);
        assert!(format!("{error:#}").contains("device offline"));
        assert!(
            wait_for_application_process(|| std::future::ready(Ok("1234 5678".into())), &executor,)
                .await
                .is_err()
        );
    }

    #[test]
    fn validates_adb_process_and_port() -> Result<()> {
        assert_eq!(positive_number::<u16>("62983\n", "port")?, 62983);
        assert_eq!(positive_number::<u32>("1234", "process")?, 1234);
        for output in ["", "0", "-1", "1 2", "1\n2", "123;echo", "65536"] {
            assert!(positive_number::<u16>(output, "port").is_err());
        }
        Ok(())
    }
}
