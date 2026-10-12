//! Required live integration fixture, invoked by `script/test-android-debugger-jvm`.
//! The Java/Kotlin compiler and VM are fixture runtimes; every debugger and test
//! orchestration operation is Rust. Errors retain bounded protocol diagnostics.

use crate::dap::Framing;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    ffi::OsString,
    fs::{self, File},
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};

const DEADLINE: Duration = Duration::from_secs(30);
const MAX_LOG: usize = 8 * 1024 * 1024;

pub struct FixtureConfig {
    pub adapter: PathBuf,
    pub java_home: PathBuf,
    pub compiler_classpath: OsString,
    pub stdlib: PathBuf,
    pub repository: PathBuf,
}

pub fn run_cli(args: &[OsString]) -> Result<()> {
    let mut adapter = None;
    let mut java_home = None;
    let mut compiler = None;
    let mut stdlib = None;
    let mut repository = std::env::current_dir()?;
    let mut at = 0;
    while at < args.len() {
        let option = args[at].to_str().context("fixture option is not UTF-8")?;
        let value = args
            .get(at + 1)
            .context("fixture option requires a value")?
            .clone();
        match option {
            "--adapter" => adapter = Some(PathBuf::from(value)),
            "--java-home" => java_home = Some(PathBuf::from(value)),
            "--compiler-classpath" => compiler = Some(value),
            "--stdlib" => stdlib = Some(PathBuf::from(value)),
            "--repository" => repository = PathBuf::from(value),
            _ => bail!("unknown JVM fixture option {option}"),
        }
        at += 2;
    }
    run(FixtureConfig {
        adapter: adapter.context("--adapter is required")?,
        java_home: java_home.context("--java-home is required")?,
        compiler_classpath: compiler.context("--compiler-classpath is required")?,
        stdlib: stdlib.context("--stdlib is required")?,
        repository,
    })
}

const CLEANUP_DEADLINE: Duration = Duration::from_secs(2);
thread_local! {
    static CLEANUP_FAILURES: std::cell::RefCell<(u64,VecDeque<String>)> = const { std::cell::RefCell::new((0,VecDeque::new())) };
}

trait ChildControl {
    fn running(&mut self) -> Result<bool>;
    fn kill_owned(&mut self) -> Result<()>;
}
impl ChildControl for Child {
    fn running(&mut self) -> Result<bool> {
        Ok(self.try_wait()?.is_none())
    }
    fn kill_owned(&mut self) -> Result<()> {
        self.kill().context("cannot kill owned fixture child")
    }
}

/// Never calls blocking wait(). After a signal failure it checks for the exit
/// race once, and otherwise returns an observable incomplete-cleanup error.
fn stop_child(child: &mut impl ChildControl, end: Instant) -> Result<()> {
    if !child
        .running()
        .context("cannot inspect owned fixture child")?
    {
        return Ok(());
    }
    if let Err(error) = child.kill_owned() {
        return match child.running() {
            Ok(false) => Ok(()),
            Ok(true) => {
                Err(error.context("owned fixture child remains running; cleanup incomplete"))
            }
            Err(observe) => Err(error.context(format!("cleanup state unknown: {observe:#}"))),
        };
    }
    loop {
        if !child.running().context("cannot reap owned fixture child")? {
            return Ok(());
        }
        ensure!(
            Instant::now() < end,
            "owned fixture child cleanup deadline exceeded; child remains unreaped"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Owns exactly one unreaped child. Cleanup retains the Child capability, never
/// a numeric PID after reaping. All explicit/fallback cleanup is bounded.
struct OwnedChild {
    child: Child,
    closed: bool,
}
impl OwnedChild {
    fn spawn(command: &mut Command) -> Result<Self> {
        Ok(Self {
            child: command.spawn()?,
            closed: false,
        })
    }
    fn alive(&mut self) -> Result<bool> {
        let alive = self.child.running()?;
        if !alive {
            self.closed = true;
        }
        Ok(alive)
    }
    fn stop(&mut self) -> Result<()> {
        if !self.closed {
            stop_child(&mut self.child, Instant::now() + CLEANUP_DEADLINE)?;
            self.closed = true;
        }
        Ok(())
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            let diagnostic = format!("fixture fallback cleanup incomplete: {error:#}");
            eprintln!("{diagnostic}");
            CLEANUP_FAILURES.with(|failures| {
                let mut failures = failures.borrow_mut();
                failures.0 = failures.0.saturating_add(1);
                if failures.1.len() == 32 {
                    failures.1.pop_front();
                }
                failures.1.push_back(diagnostic);
            });
        }
    }
}

#[cfg(unix)]
fn nonblocking<R: std::os::fd::AsRawFd>(reader: &R) -> Result<()> {
    let fd = reader.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(flags >= 0, "cannot inspect fixture pipe");
    ensure!(
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0,
        "cannot configure fixture pipe"
    );
    Ok(())
}

#[cfg(not(unix))]
fn nonblocking<R>(_: &R) -> Result<()> {
    bail!("JVM fixture pipe supervision currently requires Unix")
}

fn bounded_command(command: &mut Command, log: &Path) -> Result<()> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = OwnedChild::spawn(command)?;
    let mut stdout = child
        .child
        .stdout
        .take()
        .context("compiler stdout missing")?;
    let mut stderr = child
        .child
        .stderr
        .take()
        .context("compiler stderr missing")?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let mut saved = File::create(log)?;
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut used = 0;
    let mut eof = [false; 2];
    let mut status = None;
    loop {
        for (index, reader) in [&mut stdout as &mut dyn Read, &mut stderr as &mut dyn Read]
            .into_iter()
            .enumerate()
        {
            if eof[index] {
                continue;
            }
            let mut bytes = [0; 8192];
            match reader.read(&mut bytes) {
                Ok(0) => eof[index] = true,
                Ok(count) => {
                    used += count;
                    ensure!(used <= MAX_LOG, "compiler diagnostics exceed budget");
                    saved.write_all(&bytes[..count])?;
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e.into()),
            }
        }
        if status.is_none() {
            status = child.child.try_wait()?;
        }
        if let Some(status) = status.filter(|_| eof.into_iter().all(|v| v)) {
            ensure!(
                status.success(),
                "fixture compiler failed: {status}; diagnostics {}",
                log.display()
            );
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "fixture compiler timed out; diagnostics {}",
            log.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

struct Protocol {
    adapter: OwnedChild,
    stdin: ChildStdin,
    stdout: ChildStdout,
    framing: Framing,
    pending: VecDeque<Value>,
    sequence: u64,
    log: File,
    logged: usize,
}

impl Protocol {
    fn spawn(config: &FixtureConfig, log: File, errors: File) -> Result<Self> {
        let mut command = Command::new(&config.adapter);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(errors);
        let mut adapter = OwnedChild::spawn(&mut command)?;
        let stdin = adapter
            .child
            .stdin
            .take()
            .context("adapter stdin missing")?;
        let stdout = adapter
            .child
            .stdout
            .take()
            .context("adapter stdout missing")?;
        nonblocking(&stdout)?;
        Ok(Self {
            adapter,
            stdin,
            stdout,
            framing: Framing::default(),
            pending: VecDeque::new(),
            sequence: 1,
            log,
            logged: 0,
        })
    }

    fn record(&mut self, direction: &str, message: &Value) -> Result<()> {
        let bytes = serde_json::to_vec(&json!({"direction":direction,"message":message}))?;
        self.logged = self
            .logged
            .checked_add(bytes.len() + 1)
            .context("protocol log size overflow")?;
        ensure!(
            self.logged <= MAX_LOG,
            "fixture protocol log exceeds budget"
        );
        self.log.write_all(&bytes)?;
        self.log.write_all(b"\n")?;
        self.log.flush()?;
        Ok(())
    }

    fn send(&mut self, command: &str, arguments: Value) -> Result<u64> {
        let sequence = self.sequence;
        self.sequence += 1;
        let message =
            json!({"seq":sequence,"type":"request","command":command,"arguments":arguments});
        self.record("request", &message)?;
        let bytes = serde_json::to_vec(&message)?;
        write!(self.stdin, "Content-Length: {}\r\n\r\n", bytes.len())?;
        self.stdin.write_all(&bytes)?;
        self.stdin.flush()?;
        Ok(sequence)
    }

    fn receive(&mut self, predicate: impl Fn(&Value) -> bool) -> Result<Value> {
        if let Some(index) = self.pending.iter().position(&predicate) {
            return Ok(self.pending.remove(index).unwrap());
        }
        let deadline = Instant::now() + DEADLINE;
        loop {
            while let Some(message) = self.framing.next_message()? {
                self.record("response", &message)?;
                if predicate(&message) {
                    return Ok(message);
                }
                ensure!(
                    self.pending.len() < 10_000,
                    "fixture pending message budget exceeded"
                );
                self.pending.push_back(message);
            }
            let mut bytes = [0; 8192];
            match self.stdout.read(&mut bytes) {
                Ok(0) => {
                    self.framing.finish()?;
                    bail!("adapter exited before expected DAP message");
                }
                Ok(count) => self.framing.push(&bytes[..count])?,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e.into()),
            }
            ensure!(
                Instant::now() < deadline,
                "timed out waiting for DAP message"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn response(&mut self, sequence: u64) -> Result<Value> {
        self.receive(|m| m["request_seq"] == sequence)
    }
    fn request(&mut self, command: &str, args: Value) -> Result<Value> {
        let sequence = self.send(command, args)?;
        let response = self.response(sequence)?;
        ensure!(response["success"] == true, "{command} failed: {response}");
        Ok(response["body"].clone())
    }
    fn rejected(&mut self, command: &str, args: Value) -> Result<()> {
        let sequence = self.send(command, args)?;
        let response = self.response(sequence)?;
        ensure!(
            response["success"] == false,
            "{command} unexpectedly succeeded: {response}"
        );
        Ok(())
    }
    fn stop(
        &mut self,
        source: &Path,
        file: &str,
        line: u32,
        reason: Option<&str>,
    ) -> Result<(Value, Value)> {
        let event = self.receive(|m| m["event"] == "stopped")?["body"].clone();
        let thread = event["threadId"].clone();
        let frames = self.request("stackTrace", json!({"threadId":thread}))?["stackFrames"]
            .as_array()
            .context("stackFrames missing")?
            .clone();
        if let Some(reason) = reason {
            ensure!(
                event["reason"] == reason,
                "wrong stop reason: {event}; frames {frames:?}"
            );
        }
        let frame = frames.first().context("stopped thread has no frames")?;
        ensure!(
            frame["source"]["path"] == json!(source.join(file)) && frame["line"] == line,
            "wrong stop location: {frames:?}"
        );
        Ok((thread, frame["id"].clone()))
    }
}

fn spawn_vm(
    config: &FixtureConfig,
    classes: &Path,
    main: &str,
    extra: &[&str],
) -> Result<(OwnedChild, u16)> {
    let available = TcpListener::bind(("127.0.0.1", 0))?;
    let port = available.local_addr()?.port();
    drop(available);
    let classpath = std::env::join_paths([classes, &config.stdlib])?;
    let mut command = Command::new(config.java_home.join("bin/java"));
    command
        .args(extra)
        .arg(format!(
            "-agentlib:jdwp=transport=dt_socket,server=y,suspend=y,address=127.0.0.1:{port}"
        ))
        .arg("-cp")
        .arg(classpath)
        .arg(main)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut vm = OwnedChild::spawn(&mut command)?;
    let mut stdout = vm.child.stdout.take().context("VM stdout missing")?;
    let mut stderr = vm.child.stderr.take().context("VM stderr missing")?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let deadline = Instant::now() + DEADLINE;
    let mut output = Vec::new();
    loop {
        for reader in [&mut stdout as &mut dyn Read, &mut stderr as &mut dyn Read] {
            let mut bytes = [0; 8192];
            match reader.read(&mut bytes) {
                Ok(count) => {
                    ensure!(
                        output.len() + count <= 1024 * 1024,
                        "VM startup diagnostics exceed budget"
                    );
                    output.extend(&bytes[..count]);
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e.into()),
            }
        }
        if String::from_utf8_lossy(&output).contains("Listening for transport dt_socket") {
            // Fixtures print only bounded startup diagnostics. Keep their pipes
            // open to avoid SIGPIPE; no drain thread remains after cleanup.
            vm.child.stdout = Some(stdout);
            vm.child.stderr = Some(stderr);
            return Ok((vm, port));
        }
        ensure!(
            vm.alive()?,
            "JVM exited during startup: {}",
            String::from_utf8_lossy(&output)
        );
        ensure!(
            Instant::now() < deadline,
            "JVM startup timed out: {}",
            String::from_utf8_lossy(&output)
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn connect(
    config: &FixtureConfig,
    source: &Path,
    port: u16,
    breakpoints: &[(&str, &[u32])],
    log: &Path,
    errors: &Path,
) -> Result<Protocol> {
    let mut protocol = Protocol::spawn(
        config,
        File::options().append(true).create(true).open(log)?,
        File::options().append(true).create(true).open(errors)?,
    )?;
    let capabilities=protocol.request("initialize",json!({"adapterID":"kotlin","linesStartAt1":true,"columnsStartAt1":true,"pathFormat":"path"}))?;
    ensure!(
        capabilities["supportsEvaluateForHovers"] == true,
        "hover evaluation capability absent"
    );
    let attach=protocol.send("attach",json!({"projectRoot":source.parent(),"sourceRoots":[source],"hostName":"127.0.0.1","port":port,"timeout":5000}))?;
    protocol.receive(|m| m["event"] == "initialized")?;
    for (file, lines) in breakpoints {
        protocol.request("setBreakpoints",json!({"source":{"path":source.join(file)},"breakpoints":lines.iter().map(|line|json!({"line":line})).collect::<Vec<_>>()}))?;
    }
    protocol.request("setExceptionBreakpoints", json!({"filters":[]}))?;
    let configured = protocol.send("configurationDone", json!({}))?;
    ensure!(
        protocol.response(attach)?["success"] == true,
        "attach failed"
    );
    ensure!(
        protocol.response(configured)?["success"] == true,
        "configuration failed"
    );
    Ok(protocol)
}

pub fn run(config: FixtureConfig) -> Result<()> {
    // Capture fallback cleanup after all local child guards have dropped. A
    // successful predicate run cannot become success with unreaped children.
    let before = CLEANUP_FAILURES.with(|failures| failures.borrow().0);
    let result = run_inner(config);
    let (after, cleanup) = CLEANUP_FAILURES.with(|failures| {
        let failures = failures.borrow();
        (
            failures.0,
            failures.1.iter().cloned().collect::<Vec<_>>().join("; "),
        )
    });
    if before == after && after != u64::MAX {
        result
    } else {
        match result {
            Ok(()) => bail!("{cleanup}"),
            Err(error) => Err(error.context(cleanup)),
        }
    }
}

fn run_inner(config: FixtureConfig) -> Result<()> {
    let repository = config.repository.canonicalize()?;
    let source = repository.join("examples/android-ide/debugger-fixture/src");
    let validation = repository.join("target/android-ide/validation/jvm-debugger");
    fs::create_dir_all(&validation)?;
    let temporary = tempfile::tempdir()?;
    let classes = temporary.path().join("classes");
    fs::create_dir(&classes)?;
    let mut javac = Command::new(config.java_home.join("bin/javac"));
    javac
        .arg("-g")
        .arg("-d")
        .arg(&classes)
        .arg(source.join("app/Values.java"));
    bounded_command(&mut javac, &validation.join("javac.log"))?;
    let mut compiler = Command::new(config.java_home.join("bin/java"));
    compiler
        .arg("-cp")
        .arg(&config.compiler_classpath)
        .arg("org.jetbrains.kotlin.cli.jvm.K2JVMCompiler")
        .args(["-no-stdlib", "-no-reflect", "-classpath"])
        .arg(std::env::join_paths([&classes, &config.stdlib])?)
        .arg("-d")
        .arg(&classes)
        .arg(source.join("app/DebugFixture.kt"))
        .arg(source.join("app/Inline.kt"))
        .arg(source.join("other/Inline.kt"));
    bounded_command(&mut compiler, &validation.join("kotlinc.log"))?;
    let log = validation.join("protocol.jsonl");
    let errors = validation.join("adapter.log");
    let (mut vm, port) = spawn_vm(&config, &classes, "app.DebugFixtureKt", &[])?;
    let mut protocol = connect(
        &config,
        &source,
        port,
        &[
            ("app/DebugFixture.kt", &[9, 20, 28]),
            ("app/Values.java", &[9]),
        ],
        &log,
        &errors,
    )?;
    let (thread, frame) = protocol.stop(&source, "app/DebugFixture.kt", 9, None)?;
    let evaluation = protocol.request(
        "evaluate",
        json!({"frameId":frame,"expression":"values[1]","context":"watch"}),
    )?;
    ensure!(
        evaluation["result"] == "5",
        "array evaluation differs: {evaluation}"
    );
    let old_frame = frame;
    protocol.request("next", json!({"threadId":thread}))?;
    let (thread, _) = protocol.stop(&source, "app/DebugFixture.kt", 10, Some("step"))?;
    protocol.rejected(
        "evaluate",
        json!({"frameId":old_frame,"expression":"values[0]","context":"watch"}),
    )?;
    protocol.request(
        "setBreakpoints",
        json!({"source":{"path":source.join("other/Inline.kt")},"breakpoints":[{"line":8}]}),
    )?;
    protocol.request("continue", json!({"threadId":thread}))?;
    let (thread, _) = protocol.stop(&source, "other/Inline.kt", 8, None)?;
    protocol.request("next", json!({"threadId":thread}))?;
    let (thread, _) = protocol.stop(&source, "other/Inline.kt", 9, Some("step"))?;
    protocol.request("stepOut", json!({"threadId":thread}))?;
    let (thread, _) = protocol.stop(&source, "app/DebugFixture.kt", 12, Some("step"))?;
    protocol.request(
        "setBreakpoints",
        json!({"source":{"path":source.join("other/Inline.kt")},"breakpoints":[]}),
    )?;
    let unsupported=protocol.request("setBreakpoints",json!({"source":{"path":source.join("other/Inline.kt")},"breakpoints":[{"line":8,"condition":"value > 0"},{"line":9,"logMessage":"value"}]}))?;
    ensure!(unsupported["breakpoints"].as_array().context("breakpoint replies missing")?.iter().all(|b|b["verified"]==false && b["message"].as_str().is_some_and(|s|!s.is_empty())),"unsupported breakpoint was accepted: {unsupported}");
    protocol.request("continue", json!({"threadId":thread}))?;
    let (thread, frame) = protocol.stop(&source, "app/Values.java", 9, None)?;
    for (expression, expected) in [
        ("this.field", "7"),
        ("field", "7"),
        ("values[0]", "3"),
        ("large[2000]", "42"),
        ("untouched", "null"),
    ] {
        let value = protocol.request(
            "evaluate",
            json!({"frameId":frame,"expression":expression,"context":"watch"}),
        )?;
        ensure!(
            value["result"] == expected,
            "readonly evaluation {expression} differs: {value}"
        );
    }
    for expression in ["", "field + 1", "toString()", "values[-1]", "values[1000]"] {
        protocol.rejected(
            "evaluate",
            json!({"frameId":frame,"expression":expression,"context":"watch"}),
        )?;
    }
    let completions = protocol.request(
        "completions",
        json!({"frameId":frame,"text":"this.","column":6}),
    )?;
    ensure!(
        completions["targets"]
            .as_array()
            .context("completion targets missing")?
            .iter()
            .any(|c| c["label"] == "field"),
        "field completion missing: {completions}"
    );
    protocol.request("stepIn", json!({"threadId":thread}))?;
    let (thread, _) = protocol.stop(&source, "app/Values.java", 18, Some("step"))?;
    protocol.request("stepOut", json!({"threadId":thread}))?;
    let (thread, _) = protocol.stop(&source, "app/Values.java", 9, Some("step"))?;
    protocol.request("setExceptionBreakpoints", json!({"filters":["C"]}))?;
    protocol.request("continue", json!({"threadId":thread}))?;
    let (thread, _) = protocol.stop(&source, "app/Values.java", 12, Some("exception"))?;
    let info = protocol.request("exceptionInfo", json!({"threadId":thread}))?;
    ensure!(
        info.to_string().contains("debug-fixture-caught"),
        "exception message missing: {info}"
    );
    protocol.request("setExceptionBreakpoints", json!({"filters":[]}))?;
    protocol.request("continue", json!({"threadId":thread}))?;
    let (thread, frame) = protocol.stop(&source, "app/DebugFixture.kt", 20, None)?;
    let scopes = protocol.request("scopes", json!({"frameId":frame}))?;
    let scope = scopes["scopes"]
        .as_array()
        .context("scopes missing")?
        .iter()
        .find(|s| s["name"] == "Coroutine state (raw)")
        .context("raw coroutine scope missing")?;
    let values = protocol.request(
        "variables",
        json!({"variablesReference":scope["variablesReference"]}),
    )?;
    let variables = values["variables"]
        .as_array()
        .context("coroutine variables missing")?;
    ensure!(
        variables.iter().any(|v| v["name"] == "label"),
        "continuation label missing: {values}"
    );
    ensure!(
        variables.iter().any(|v| v["name"] == "L$0"
            && v["value"]
                .as_str()
                .is_some_and(|s| s.contains("retained-value"))),
        "continuation captured value missing: {values}"
    );
    protocol.request("continue", json!({"threadId":thread}))?;
    protocol.stop(&source, "app/DebugFixture.kt", 28, None)?;
    protocol.request("disconnect", json!({"terminateDebuggee":false}))?;
    protocol.adapter.stop()?;
    drop(protocol);
    ensure!(vm.alive()?, "detach killed the debuggee");
    let mut protocol = connect(&config, &source, port, &[], &log, &errors)?;
    let threads = protocol.request("threads", json!({}))?;
    ensure!(
        !threads["threads"]
            .as_array()
            .context("threads missing")?
            .is_empty(),
        "reconnect did not discover threads"
    );
    vm.stop()?;
    protocol.receive(|m| m["event"] == "terminated")?;
    protocol.adapter.stop()?;
    drop(protocol);
    let (mut vm, port) = spawn_vm(&config, &classes, "app.InlineKt", &[])?;
    let mut protocol = connect(
        &config,
        &source,
        port,
        &[("other/Inline.kt", &[8])],
        &log,
        &errors,
    )?;
    let (thread, _) = protocol.stop(&source, "other/Inline.kt", 8, None)?;
    protocol.request("next", json!({"threadId":thread}))?;
    let (thread, _) = protocol.stop(&source, "other/Inline.kt", 9, Some("step"))?;
    protocol.request("stepOut", json!({"threadId":thread}))?;
    protocol.stop(&source, "app/Inline.kt", 5, Some("step"))?;
    protocol.request("disconnect", json!({"terminateDebuggee":false}))?;
    protocol.adapter.stop()?;
    ensure!(vm.alive()?, "inline fixture detach killed the debuggee");
    vm.stop()?;
    sample_workspace_fixture(&config, &validation, &log, &errors)?;
    println!(
        "PASS: Rust JDWP Kotlin/Java arrays, fields, unloaded/null types, rejected expressions, completions, inline/source identity and stepping, exceptions, raw continuation, stale handles, detach/reconnect and VM death"
    );
    Ok(())
}

// Adapted from the MIT-licensed upstream SampleWorkspaceTest. The original test
// launches through its JDI adapter; Android owns launch outside DAP, so this Rust
// fixture launches the same application/runtime arguments then attaches JDWP.
fn sample_workspace_fixture(
    config: &FixtureConfig,
    validation: &Path,
    log: &Path,
    errors: &Path,
) -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let source = temporary.path().join("src");
    let package = source.join("sample/workspace");
    fs::create_dir_all(&package)?;
    fs::write(
        package.join("App.kt"),
        include_bytes!("../tests/fixtures/sample/workspace/App.kt"),
    )?;
    let classes = temporary.path().join("classes");
    fs::create_dir(&classes)?;
    let mut compiler = Command::new(config.java_home.join("bin/java"));
    compiler
        .arg("-cp")
        .arg(&config.compiler_classpath)
        .arg("org.jetbrains.kotlin.cli.jvm.K2JVMCompiler")
        .args(["-no-stdlib", "-no-reflect", "-classpath"])
        .arg(&config.stdlib)
        .arg("-d")
        .arg(&classes)
        .arg(package.join("App.kt"));
    bounded_command(&mut compiler, &validation.join("sample-kotlinc.log"))?;
    let (mut vm, port) = spawn_vm(
        config,
        &classes,
        "sample.workspace.AppKt",
        &["-Dtest=testVmArgs"],
    )?;
    let mut protocol = connect(
        config,
        &source,
        port,
        &[("sample/workspace/App.kt", &[8])],
        log,
        errors,
    )?;
    let (thread, _) = protocol.stop(&source, "sample/workspace/App.kt", 8, Some("breakpoint"))?;
    let frames = protocol.request("stackTrace", json!({"threadId":thread}))?;
    let mut locals = Vec::new();
    for frame in frames["stackFrames"]
        .as_array()
        .context("sample frames missing")?
    {
        let scopes = protocol.request("scopes", json!({"frameId":frame["id"]}))?;
        for scope in scopes["scopes"]
            .as_array()
            .context("sample scopes missing")?
        {
            let variables = protocol.request(
                "variables",
                json!({"variablesReference":scope["variablesReference"]}),
            )?;
            locals.extend(
                variables["variables"]
                    .as_array()
                    .context("sample variables missing")?
                    .iter()
                    .cloned(),
            );
        }
    }
    ensure!(
        locals
            .iter()
            .any(|v| v["name"] == "local" && v["value"] == "123"),
        "sample local=123 missing: {locals:?}"
    );
    let receiver = locals
        .iter()
        .find(|v| v["name"] == "this")
        .context("sample receiver missing")?;
    let members = protocol.request(
        "variables",
        json!({"variablesReference":receiver["variablesReference"]}),
    )?;
    let members = members["variables"]
        .as_array()
        .context("sample members missing")?;
    ensure!(
        members.len() == 1
            && members[0]["name"] == "member"
            && members[0]["value"] == "\"testVmArgs\"",
        "sample receiver members differ: {members:?}"
    );
    protocol.request("disconnect", json!({"terminateDebuggee":false}))?;
    protocol.adapter.stop()?;
    vm.stop()?;
    Ok(())
}

#[cfg(test)]
mod cleanup_regressions {
    use super::*;
    struct MockChild {
        observations: usize,
        kills: usize,
        inspect_error: bool,
        kill_error: bool,
        exits: bool,
    }
    impl ChildControl for MockChild {
        fn running(&mut self) -> Result<bool> {
            self.observations += 1;
            ensure!(!self.inspect_error, "injected try_wait failure");
            Ok(!(self.exits && self.observations > 1))
        }
        fn kill_owned(&mut self) -> Result<()> {
            self.kills += 1;
            ensure!(!self.kill_error, "injected kill failure");
            Ok(())
        }
    }
    #[test]
    fn inspection_failure_is_observable_without_killing_or_blocking_wait() {
        let mut child = MockChild {
            observations: 0,
            kills: 0,
            inspect_error: true,
            kill_error: false,
            exits: false,
        };
        assert!(
            stop_child(&mut child, Instant::now() + Duration::from_secs(1))
                .unwrap_err()
                .to_string()
                .contains("inspect")
        );
        assert_eq!(child.kills, 0);
        assert_eq!(child.observations, 1);
    }
    #[test]
    fn signal_failure_reports_incomplete_cleanup_and_checks_exit_race_once() {
        let mut child = MockChild {
            observations: 0,
            kills: 0,
            inspect_error: false,
            kill_error: true,
            exits: false,
        };
        assert!(
            stop_child(&mut child, Instant::now() + Duration::from_secs(1))
                .unwrap_err()
                .to_string()
                .contains("cleanup incomplete")
        );
        assert_eq!(child.kills, 1);
        assert_eq!(child.observations, 2);
        child.observations = 0;
        child.kills = 0;
        child.exits = true;
        stop_child(&mut child, Instant::now() + Duration::from_secs(1)).unwrap();
        assert_eq!(child.kills, 1);
    }
    #[test]
    fn unreapable_child_respects_cleanup_deadline_and_retains_failure() {
        let mut child = MockChild {
            observations: 0,
            kills: 0,
            inspect_error: false,
            kill_error: false,
            exits: false,
        };
        let start = Instant::now();
        assert!(
            stop_child(&mut child, start + Duration::from_millis(3))
                .unwrap_err()
                .to_string()
                .contains("deadline")
        );
        assert!(start.elapsed() < Duration::from_millis(500));
        assert_eq!(child.kills, 1);
    }
}
