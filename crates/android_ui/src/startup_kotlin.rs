use super::{android_build, default_kotlin_server_settings};
use android_tools::{kotlin, managed};
use anyhow::Result;
use futures::{
    FutureExt as _, StreamExt as _,
    channel::{mpsc, oneshot},
    future::BoxFuture,
};
use gpui::{App, AppContext as _, BackgroundExecutor, Global, Task};
use settings::Settings as _;
use std::{collections::VecDeque, sync::Arc, time::Duration};

const OUTPUT_CAPACITY: usize = 32;
const MAX_DETAILS_BYTES: usize = 16 * 1024;
const MAX_DETAIL_LINE_BYTES: usize = 2048;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum State {
    Checking,
    WaitingForSetup,
    WaitingForJava(String),
    Installing(String),
    Ready,
    Failed(String),
    Cancelled,
}

pub(super) struct StartupKotlin {
    state: Option<State>,
    task: Option<Task<()>>,
    cancel: Option<oneshot::Sender<()>>,
    cancel_requested: bool,
    operation: u64,
    offline: bool,
    details: OutputDetails,
    provider: Arc<dyn RuntimeProvider>,
}

impl Global for StartupKotlin {}

trait RuntimeProvider: Send + Sync {
    fn supported(&self) -> bool;
    fn runtime(&self) -> Result<()>;
    fn java(&self) -> Result<()>;
    fn prepare(&self, offline: bool) -> BoxFuture<'static, Result<managed::Prepared>>;
    fn install(
        &self,
        prepared: managed::Prepared,
        executor: BackgroundExecutor,
        output: mpsc::Sender<android_build::OutputLine>,
        cancel: oneshot::Receiver<()>,
    ) -> BoxFuture<'static, Result<android_build::ProcessOutput>>;
}

struct ManagedRuntime;

impl RuntimeProvider for ManagedRuntime {
    fn supported(&self) -> bool {
        managed::supported() && std::env::var_os("ANDROID_IDE_OFFICIAL_KOTLIN_SERVER").is_none()
    }

    fn runtime(&self) -> Result<()> {
        kotlin::official_server_binary().map(|_| ())
    }

    fn java(&self) -> Result<()> {
        let home = kotlin::java_home()?;
        anyhow::ensure!(
            home.join("bin/javac").is_file(),
            "A full JDK 21 is required"
        );
        Ok(())
    }

    fn prepare(&self, offline: bool) -> BoxFuture<'static, Result<managed::Prepared>> {
        async move { managed::prepare(managed::Tool::Kotlin, "install", offline) }.boxed()
    }

    fn install(
        &self,
        prepared: managed::Prepared,
        executor: BackgroundExecutor,
        output: mpsc::Sender<android_build::OutputLine>,
        cancel: oneshot::Receiver<()>,
    ) -> BoxFuture<'static, Result<android_build::ProcessOutput>> {
        async move {
            let mut command = util::command::new_std_command(&prepared.program);
            command
                .args(&prepared.arguments)
                .envs(&prepared.environment)
                .current_dir(prepared.directory.path());
            let result = android_build::command_output_with_cleanup(
                command,
                &executor,
                Duration::from_secs(1800),
                output,
                cancel,
                false,
            )
            .await;
            // Recipes must survive until the managed process group has been stopped.
            drop(prepared.directory);
            result
        }
        .boxed()
    }
}

pub(super) fn initialize(cx: &mut App) {
    if cx.has_global::<StartupKotlin>() {
        return;
    }
    initialize_with_provider(Arc::new(ManagedRuntime), cx);
}

fn initialize_with_provider(provider: Arc<dyn RuntimeProvider>, cx: &mut App) {
    cx.set_global(StartupKotlin {
        state: None,
        task: None,
        cancel: None,
        cancel_requested: false,
        operation: 0,
        offline: false,
        details: OutputDetails::default(),
        provider,
    });
    cx.on_app_quit(shutdown).detach();
    retry(false, cx);
}

fn shutdown(cx: &mut App) -> impl Future<Output = ()> + use<> {
    cancel(cx);
    let task = if cx.has_global::<StartupKotlin>() {
        cx.global_mut::<StartupKotlin>().task.take()
    } else {
        None
    };
    async move {
        if let Some(task) = task {
            task.await;
        }
    }
}

pub(super) fn state(cx: &App) -> Option<&State> {
    cx.try_global::<StartupKotlin>()?.state.as_ref()
}

pub(super) fn offline(cx: &App) -> Option<bool> {
    cx.try_global::<StartupKotlin>()
        .map(|startup| startup.offline)
}

#[cfg(test)]
pub(super) fn set_state_for_test(state: State, cx: &mut App) {
    let offline = offline(cx).unwrap_or(false);
    cancel(cx);
    let active = matches!(state, State::Checking | State::Installing(_));
    cx.set_global(StartupKotlin {
        state: Some(state),
        task: active.then(|| Task::ready(())),
        cancel: None,
        cancel_requested: false,
        operation: 0,
        offline,
        details: OutputDetails::default(),
        provider: Arc::new(ManagedRuntime),
    });
}

pub(super) fn busy(cx: &App) -> bool {
    cx.try_global::<StartupKotlin>()
        .is_some_and(|startup| startup.task.is_some())
}

fn preferences_allow_install(cx: &App) -> bool {
    let languages = language::language_settings::AllLanguageSettings::get_global(cx);
    let kotlin = languages.language(None, Some(&"Kotlin".into()), cx);
    let servers =
        kotlin.customized_language_servers(&[lsp::LanguageServerName("kotlin-lsp".into())]);
    if !kotlin.enable_language_server || servers.len() != 1 || servers[0].0.as_ref() != "kotlin-lsp"
    {
        return false;
    }
    project::project_settings::ProjectSettings::get_global(cx)
        .lsp
        .get(&lsp::LanguageServerName("kotlin-lsp".into()))
        .is_none_or(|server| {
            default_kotlin_server_settings(server)
                || server.binary.as_ref().is_some_and(|binary| {
                    binary
                        .env
                        .as_ref()
                        .and_then(|environment| environment.get(kotlin::MANAGED_RUNTIME_ENV))
                        .is_some_and(|marker| marker == "1")
                        && binary
                            .path
                            .as_deref()
                            .is_none_or(|path| path == kotlin::MANAGED_RUNTIME_PATH)
                })
        })
}

pub(super) fn resume(cx: &mut App) {
    if matches!(
        state(cx),
        Some(State::WaitingForJava(_) | State::WaitingForSetup)
    ) && super::first_launch_setup_resolved_for_kotlin(cx)
    {
        let offline = cx.global::<StartupKotlin>().offline;
        retry(offline, cx);
    }
}

pub(super) fn set_offline(offline: bool, cx: &mut App) {
    if cx
        .try_global::<StartupKotlin>()
        .is_some_and(|startup| startup.offline != offline)
    {
        cx.global_mut::<StartupKotlin>().offline = offline;
    }
}

pub(super) fn cancel(cx: &mut App) {
    let Some(startup) = cx.try_global::<StartupKotlin>() else {
        return;
    };
    if startup.task.is_none() || startup.cancel_requested {
        return;
    }
    let startup = cx.global_mut::<StartupKotlin>();
    startup.cancel_requested = true;
    startup.state = Some(State::Installing("Cancelling Kotlin setup…".into()));
    if let Some(cancel) = startup.cancel.take()
        && cancel.send(()).is_err()
    {
        log::debug!("Kotlin startup setup already stopped before cancellation");
    }
}

pub(super) fn retry(offline: bool, cx: &mut App) {
    set_offline(offline, cx);
    let Some(startup) = cx.try_global::<StartupKotlin>() else {
        return;
    };
    if busy(cx) {
        return;
    }
    if !startup.provider.supported() || !preferences_allow_install(cx) {
        if startup.state.is_some() {
            cx.global_mut::<StartupKotlin>().state = None;
        }
        return;
    }
    let provider = startup.provider.clone();
    let (cancel, mut cancelled) = oneshot::channel();
    let operation = {
        let startup = cx.global_mut::<StartupKotlin>();
        startup.operation = startup.operation.wrapping_add(1);
        startup.cancel = Some(cancel);
        startup.cancel_requested = false;
        startup.details = OutputDetails::default();
        startup.state = Some(State::Checking);
        startup.operation
    };
    let executor = cx.background_executor().clone();
    let task = cx.spawn(async move |cx| {
        let runtime = cx
            .background_spawn({
                let provider = provider.clone();
                async move { provider.runtime() }
            })
            .await;
        if !cx.update(|cx| continue_operation(operation, false, cx)) {
            return;
        }
        if runtime.is_ok() {
            cx.update(|cx| finish(operation, Some(State::Ready), cx));
            return;
        }
        if !cx.update(|cx| continue_operation(operation, true, cx)) {
            return;
        }
        let java = cx
            .background_spawn({
                let provider = provider.clone();
                async move { provider.java() }
            })
            .await;
        if !cx.update(|cx| continue_operation(operation, true, cx)) {
            return;
        }
        if let Err(error) = java {
            cx.update(|cx| {
                finish(
                    operation,
                    Some(State::WaitingForJava(bounded(
                        &format!("Choose or download a full JDK 21 in Android: Setup, then finish setup to install Kotlin automatically. {error:#}"),
                        MAX_DETAILS_BYTES,
                    ))),
                    cx,
                );
            });
            return;
        }
        cx.update(|cx| {
            cx.global_mut::<StartupKotlin>().state =
                Some(State::Installing("Preparing Kotlin language server…".into()));
        });
        let prepared = cx
            .background_spawn({
                let provider = provider.clone();
                async move { provider.prepare(offline).await }
            })
            .await;
        if !cx.update(|cx| continue_operation(operation, true, cx)) {
            return;
        }
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                cx.update(|cx| fail(operation, error, cx));
                return;
            }
        };
        if !matches!(cancelled.try_recv(), Ok(None)) {
            cx.update(|cx| finish(operation, Some(State::Cancelled), cx));
            return;
        }
        let (output, mut lines) = mpsc::channel(OUTPUT_CAPACITY);
        cx.update(|cx| {
            cx.global_mut::<StartupKotlin>().state = Some(State::Installing(
                "Downloading and validating Kotlin language server…".into(),
            ));
        });
        let installation = cx.background_spawn({
            let provider = provider.clone();
            async move { provider.install(prepared, executor, output, cancelled).await }
        });
        let mut details = OutputDetails::default();
        while let Some(line) = lines.next().await {
            details.record(line);
        }
        let result = installation.await;
        cx.update(|cx| {
            if cx.global::<StartupKotlin>().operation == operation {
                cx.global_mut::<StartupKotlin>().details = details;
            }
        });
        if !cx.update(|cx| continue_operation(operation, false, cx)) {
            return;
        }
        match result {
            Ok(android_build::ProcessOutput::Cancelled) => {
                cx.update(|cx| finish(operation, Some(State::Cancelled), cx));
            }
            Err(error) => cx.update(|cx| fail(operation, error, cx)),
            Ok(android_build::ProcessOutput::Success(_)) => {
                let validated = cx.background_spawn(async move { provider.runtime() }).await;
                if !cx.update(|cx| continue_operation(operation, false, cx)) {
                    return;
                }
                cx.update(|cx| match validated {
                    Ok(()) => finish(operation, Some(State::Ready), cx),
                    Err(error) => fail(
                        operation,
                        error.context("The installed Kotlin runtime did not pass validation"),
                        cx,
                    ),
                });
            }
        }
    });
    cx.global_mut::<StartupKotlin>().task = Some(task);
}

fn continue_operation(operation: u64, require_setup: bool, cx: &mut App) -> bool {
    let Some(startup) = cx.try_global::<StartupKotlin>() else {
        return false;
    };
    if startup.operation != operation {
        return false;
    }
    if startup.cancel_requested {
        finish(operation, Some(State::Cancelled), cx);
        return false;
    }
    if !startup.provider.supported() || !preferences_allow_install(cx) {
        finish(operation, None, cx);
        return false;
    }
    if require_setup && !super::first_launch_setup_resolved_for_kotlin(cx) {
        finish(operation, Some(State::WaitingForSetup), cx);
        return false;
    }
    true
}

fn finish(operation: u64, state: Option<State>, cx: &mut App) {
    let Some(startup) = cx.try_global::<StartupKotlin>() else {
        return;
    };
    if startup.operation != operation {
        return;
    }
    if state == Some(State::Ready) {
        log::info!("Kotlin startup setup: validated language server is ready");
    }
    let startup = cx.global_mut::<StartupKotlin>();
    startup.state = state;
    startup.cancel = None;
    startup.task = None;
}

fn fail(operation: u64, error: anyhow::Error, cx: &mut App) {
    let Some(startup) = cx.try_global::<StartupKotlin>() else {
        return;
    };
    if startup.operation != operation {
        return;
    }
    let details = startup
        .details
        .lines
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    let error = bounded(&format!("{error:#}"), 4096);
    let details = bounded_recent(&details, 8192);
    let message = bounded(
        &format!(
            "Kotlin setup failed: {error}\n{details}\nOpen Android: Setup and retry Kotlin setup. The previous managed runtime is preserved."
        ),
        MAX_DETAILS_BYTES,
    );
    log::error!("{message}");
    finish(operation, Some(State::Failed(message)), cx);
}

#[derive(Default)]
struct OutputDetails {
    lines: VecDeque<String>,
    bytes: usize,
}

impl OutputDetails {
    fn record(&mut self, line: android_build::OutputLine) {
        let text = bounded(&line.text, MAX_DETAIL_LINE_BYTES);
        if text.trim().is_empty() {
            return;
        }
        if line.stderr {
            log::warn!("Kotlin setup: {text}");
        } else {
            log::info!("Kotlin setup: {text}");
        }
        self.bytes += text.len();
        self.lines.push_back(text);
        while self.bytes > MAX_DETAILS_BYTES || self.lines.len() > 32 {
            if let Some(discarded) = self.lines.pop_front() {
                self.bytes -= discarded.len();
            }
        }
    }
}

fn bounded(text: &str, maximum: usize) -> String {
    if text.len() <= maximum {
        return text.to_owned();
    }
    let mut end = maximum.saturating_sub("…".len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn bounded_recent(text: &str, maximum: usize) -> String {
    if text.len() <= maximum {
        return text.to_owned();
    }
    let mut start = text.len() - maximum.saturating_sub("…".len());
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &text[start..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{
        SinkExt as _,
        future::{Either, select},
    };
    use gpui::{BorrowAppContext as _, TestAppContext};
    use std::{
        collections::BTreeMap,
        path::PathBuf,
        sync::{
            Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    #[derive(Default)]
    struct FakeRuntime {
        ready: Arc<AtomicBool>,
        java_ready: AtomicBool,
        fail_install: Arc<AtomicBool>,
        checks: AtomicUsize,
        java_checks: AtomicUsize,
        preparations: AtomicUsize,
        installations: Arc<AtomicUsize>,
        cleanups: Arc<AtomicUsize>,
        offline: Mutex<Vec<bool>>,
        prepare_gate: Mutex<Option<oneshot::Receiver<()>>>,
        install_gate: Mutex<Option<oneshot::Receiver<()>>>,
    }

    impl RuntimeProvider for FakeRuntime {
        fn supported(&self) -> bool {
            true
        }

        fn runtime(&self) -> Result<()> {
            self.checks.fetch_add(1, Ordering::SeqCst);
            anyhow::ensure!(self.ready.load(Ordering::SeqCst), "Missing managed runtime");
            Ok(())
        }

        fn java(&self) -> Result<()> {
            self.java_checks.fetch_add(1, Ordering::SeqCst);
            anyhow::ensure!(self.java_ready.load(Ordering::SeqCst), "JDK 21 is missing");
            Ok(())
        }

        fn prepare(&self, offline: bool) -> BoxFuture<'static, Result<managed::Prepared>> {
            self.preparations.fetch_add(1, Ordering::SeqCst);
            self.offline.lock().unwrap().push(offline);
            let gate = self.prepare_gate.lock().unwrap().take();
            async move {
                if let Some(gate) = gate {
                    gate.await?;
                }
                Ok(managed::Prepared {
                    directory: tempfile::tempdir()?,
                    program: PathBuf::from("unused-test-installer"),
                    arguments: Vec::new(),
                    environment: BTreeMap::new(),
                })
            }
            .boxed()
        }

        fn install(
            &self,
            prepared: managed::Prepared,
            _: BackgroundExecutor,
            mut output: mpsc::Sender<android_build::OutputLine>,
            cancel: oneshot::Receiver<()>,
        ) -> BoxFuture<'static, Result<android_build::ProcessOutput>> {
            let ready = self.ready.clone();
            let fail = self.fail_install.clone();
            let installations = self.installations.clone();
            let cleanups = self.cleanups.clone();
            let gate = self.install_gate.lock().unwrap().take();
            async move {
                installations.fetch_add(1, Ordering::SeqCst);
                for index in 0..80 {
                    output
                        .send(android_build::OutputLine {
                            text: format!("{} installer-line-{index}", "é".repeat(1000)),
                            stderr: true,
                        })
                        .await?;
                }
                if let Some(gate) = gate {
                    match select(gate, cancel).await {
                        Either::Left((result, _)) => result?,
                        Either::Right(_) => {
                            drop(prepared);
                            cleanups.fetch_add(1, Ordering::SeqCst);
                            return Ok(android_build::ProcessOutput::Cancelled);
                        }
                    }
                }
                drop(prepared);
                cleanups.fetch_add(1, Ordering::SeqCst);
                anyhow::ensure!(!fail.load(Ordering::SeqCst), "Simulated failed download");
                ready.store(true, Ordering::SeqCst);
                Ok(android_build::ProcessOutput::Success(String::new()))
            }
            .boxed()
        }
    }

    fn setup(provider: Arc<FakeRuntime>, resolved: bool, cx: &mut TestAppContext) {
        cx.update(|cx| {
            settings::init(cx);
            cx.set_global(super::super::FirstLaunchSetup {
                enabled: true,
                resolved,
                ..Default::default()
            });
            initialize_with_provider(provider, cx);
        });
    }

    fn set_preferences(text: &str, cx: &mut App) {
        cx.update_global::<settings::SettingsStore, _>(|store, cx| {
            store
                .set_user_settings(text, cx)
                .expect("Valid test preferences");
        });
    }

    #[gpui::test]
    fn valid_runtime_is_ready_before_setup_and_initialize_is_idempotent(cx: &mut TestAppContext) {
        let provider = Arc::new(FakeRuntime::default());
        provider.ready.store(true, Ordering::SeqCst);
        setup(provider.clone(), false, cx);
        cx.update(|cx| {
            initialize(cx);
            initialize(cx);
        });
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(state(cx), Some(&State::Ready));
            assert!(!busy(cx));
        });
        assert_eq!(provider.checks.load(Ordering::SeqCst), 1);
        assert_eq!(provider.java_checks.load(Ordering::SeqCst), 0);
        assert_eq!(provider.installations.load(Ordering::SeqCst), 0);
    }

    #[gpui::test]
    fn waits_for_setup_and_java_then_installs_once_with_saved_offline_setting(
        cx: &mut TestAppContext,
    ) {
        let provider = Arc::new(FakeRuntime::default());
        setup(provider.clone(), false, cx);
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(state(cx), Some(&State::WaitingForSetup));
            assert!(!busy(cx));
            set_offline(true, cx);
            resume(cx);
        });
        assert_eq!(provider.java_checks.load(Ordering::SeqCst), 0);
        cx.update(|cx| {
            cx.global_mut::<super::super::FirstLaunchSetup>().resolved = true;
            resume(cx);
        });
        cx.run_until_parked();
        cx.read(|cx| assert!(matches!(state(cx), Some(State::WaitingForJava(_)))));
        assert_eq!(provider.preparations.load(Ordering::SeqCst), 0);
        provider.java_ready.store(true, Ordering::SeqCst);
        cx.update(|cx| {
            resume(cx);
            resume(cx);
            retry(true, cx);
        });
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(state(cx), Some(&State::Ready)));
        assert_eq!(provider.installations.load(Ordering::SeqCst), 1);
        assert_eq!(*provider.offline.lock().unwrap(), vec![true]);
    }

    #[gpui::test]
    fn cancellation_during_prepare_prevents_install_and_requires_explicit_retry(
        cx: &mut TestAppContext,
    ) {
        let provider = Arc::new(FakeRuntime::default());
        provider.java_ready.store(true, Ordering::SeqCst);
        let (release, gate) = oneshot::channel();
        *provider.prepare_gate.lock().unwrap() = Some(gate);
        setup(provider.clone(), true, cx);
        cx.run_until_parked();
        assert_eq!(provider.preparations.load(Ordering::SeqCst), 1);
        cx.update(|cx| {
            cancel(cx);
            assert!(busy(cx), "Prepare still owns the operation until it exits");
            retry(false, cx);
        });
        release.send(()).expect("Preparation is still pending");
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(state(cx), Some(&State::Cancelled));
            assert!(!busy(cx));
            resume(cx);
        });
        assert_eq!(provider.installations.load(Ordering::SeqCst), 0);
        cx.update(|cx| retry(false, cx));
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(state(cx), Some(&State::Ready)));
        assert_eq!(provider.installations.load(Ordering::SeqCst), 1);
    }

    #[gpui::test]
    fn failed_download_has_bounded_recent_details_and_no_automatic_retry(cx: &mut TestAppContext) {
        let provider = Arc::new(FakeRuntime::default());
        provider.java_ready.store(true, Ordering::SeqCst);
        provider.fail_install.store(true, Ordering::SeqCst);
        setup(provider.clone(), true, cx);
        cx.run_until_parked();
        cx.update(|cx| {
            let Some(State::Failed(message)) = state(cx) else {
                panic!("Expected a surfaced download error");
            };
            assert!(message.len() <= MAX_DETAILS_BYTES);
            assert!(message.contains("Simulated failed download"));
            assert!(message.contains("retry Kotlin setup"));
            assert!(message.contains("installer-line-79"));
            assert!(cx.global::<StartupKotlin>().details.bytes <= MAX_DETAILS_BYTES);
            assert!(
                cx.global::<StartupKotlin>()
                    .details
                    .lines
                    .back()
                    .unwrap()
                    .contains("installer-line-79")
            );
            resume(cx);
        });
        assert_eq!(provider.installations.load(Ordering::SeqCst), 1);
        provider.fail_install.store(false, Ordering::SeqCst);
        cx.update(|cx| retry(false, cx));
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(state(cx), Some(&State::Ready)));
        assert_eq!(provider.installations.load(Ordering::SeqCst), 2);
    }

    #[gpui::test]
    fn changed_custom_preferences_during_prepare_prevent_install_and_are_preserved(
        cx: &mut TestAppContext,
    ) {
        let provider = Arc::new(FakeRuntime::default());
        provider.java_ready.store(true, Ordering::SeqCst);
        let (release, gate) = oneshot::channel();
        *provider.prepare_gate.lock().unwrap() = Some(gate);
        setup(provider.clone(), true, cx);
        cx.run_until_parked();
        cx.update(|cx| {
            set_preferences(
                r#"{"lsp":{"kotlin-lsp":{"binary":{"path":"/custom/server"}}}}"#,
                cx,
            )
        });
        release.send(()).expect("Preparation is still pending");
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(state(cx), None);
            assert_eq!(
                project::project_settings::ProjectSettings::get_global(cx).lsp
                    [&lsp::LanguageServerName("kotlin-lsp".into())]
                    .binary
                    .as_ref()
                    .unwrap()
                    .path
                    .as_deref(),
                Some("/custom/server")
            );
        });
        assert_eq!(provider.installations.load(Ordering::SeqCst), 0);
    }

    #[gpui::test]
    fn disabled_and_custom_global_preferences_skip_startup_without_probe(cx: &mut TestAppContext) {
        let provider = Arc::new(FakeRuntime::default());
        cx.update(|cx| {
            settings::init(cx);
            set_preferences(
                r#"{"languages":{"Kotlin":{"enable_language_server":false}}}"#,
                cx,
            );
            initialize_with_provider(provider.clone(), cx);
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(state(cx), None);
            set_preferences(
                r#"{"languages":{"Kotlin":{"language_servers":["custom-kotlin"]}}}"#,
                cx,
            );
            retry(false, cx);
            assert_eq!(state(cx), None);
            set_preferences(
                r#"{"lsp":{"kotlin-lsp":{"binary":{"arguments":["--custom"]}}}}"#,
                cx,
            );
            retry(false, cx);
            assert_eq!(state(cx), None);
        });
        cx.run_until_parked();
        assert_eq!(provider.checks.load(Ordering::SeqCst), 0);
        assert_eq!(provider.preparations.load(Ordering::SeqCst), 0);
    }

    #[gpui::test]
    fn existing_managed_global_selection_remains_eligible(cx: &mut TestAppContext) {
        let provider = Arc::new(FakeRuntime::default());
        provider.ready.store(true, Ordering::SeqCst);
        cx.update(|cx| {
            settings::init(cx);
            set_preferences(
                r#"{"lsp":{"kotlin-lsp":{"binary":{"env":{"KODA_MANAGED_KOTLIN_RUNTIME":"1"}}}}}"#,
                cx,
            );
            initialize_with_provider(provider.clone(), cx);
        });
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(state(cx), Some(&State::Ready)));
        assert_eq!(provider.checks.load(Ordering::SeqCst), 1);
    }

    #[gpui::test]
    async fn app_quit_cancels_and_awaits_installer_cleanup(cx: &mut TestAppContext) {
        let provider = Arc::new(FakeRuntime::default());
        provider.java_ready.store(true, Ordering::SeqCst);
        let (_release, gate) = oneshot::channel();
        *provider.install_gate.lock().unwrap() = Some(gate);
        setup(provider.clone(), true, cx);
        cx.run_until_parked();
        cx.read(|cx| assert!(matches!(state(cx), Some(State::Installing(_)))));
        assert_eq!(provider.installations.load(Ordering::SeqCst), 1);
        cx.update(shutdown).await;
        cx.run_until_parked();
        assert_eq!(provider.cleanups.load(Ordering::SeqCst), 1);
        cx.read(|cx| {
            assert_eq!(state(cx), Some(&State::Cancelled));
            assert!(!busy(cx));
        });
    }
}
