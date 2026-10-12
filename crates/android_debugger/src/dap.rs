use crate::session::Session;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::io::Write;

const MAX_MESSAGE: usize = 8 * 1024 * 1024;
const MAX_HEADER: usize = 8192;

#[derive(Default)]
pub struct Framing {
    bytes: Vec<u8>,
}

impl Framing {
    pub fn push(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(
            self.bytes
                .len()
                .checked_add(bytes.len())
                .is_some_and(|n| n <= MAX_MESSAGE + MAX_HEADER),
            "DAP input exceeds framing budget"
        );
        self.bytes.try_reserve(bytes.len())?;
        self.bytes.extend(bytes);
        Ok(())
    }

    pub fn next_message(&mut self) -> Result<Option<Value>> {
        let Some(end) = self.bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
            ensure!(self.bytes.len() <= MAX_HEADER, "DAP header exceeds budget");
            return Ok(None);
        };
        ensure!(end <= MAX_HEADER, "DAP header exceeds budget");
        let header = std::str::from_utf8(&self.bytes[..end])?;
        let mut length = None;
        for line in header.split("\r\n") {
            let (name, value) = line.split_once(':').context("invalid DAP header")?;
            if name.eq_ignore_ascii_case("Content-Length") {
                ensure!(length.is_none(), "duplicate DAP Content-Length");
                let parsed = value
                    .trim()
                    .parse::<usize>()
                    .context("invalid DAP Content-Length")?;
                ensure!(
                    parsed > 0 && parsed <= MAX_MESSAGE,
                    "DAP message size exceeds budget"
                );
                length = Some(parsed);
            }
        }
        let length = length.context("missing DAP Content-Length")?;
        let end = end + 4;
        let total = end
            .checked_add(length)
            .context("DAP message offset overflow")?;
        if self.bytes.len() < total {
            return Ok(None);
        }
        let message = serde_json::from_slice(&self.bytes[end..total])?;
        self.bytes.drain(..total);
        Ok(Some(message))
    }

    pub fn finish(&self) -> Result<()> {
        ensure!(self.bytes.is_empty(), "truncated DAP input on EOF");
        Ok(())
    }
}

struct Output<W> {
    writer: W,
    sequence: u64,
}
impl<W: Write> Output<W> {
    fn send(&mut self, mut message: Value) -> Result<()> {
        message["seq"] = json!(self.sequence);
        self.sequence = self
            .sequence
            .checked_add(1)
            .context("DAP sequence overflow")?;
        let mut bytes = BoundedJson(Vec::new());
        serde_json::to_writer(&mut bytes, &message)?;
        let bytes = bytes.0;
        write!(self.writer, "Content-Length: {}\r\n\r\n", bytes.len())?;
        self.writer.write_all(&bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    fn event(&mut self, event: Value) -> Result<()> {
        let mut event = event;
        event["type"] = json!("event");
        self.send(event)
    }

    fn response(&mut self, request: &Value, result: Result<Value>) -> Result<()> {
        let message = match result {
            Ok(body) => {
                json!({"type":"response","request_seq":request["seq"],"command":request["command"],"success":true,"body":body})
            }
            Err(error) => {
                json!({"type":"response","request_seq":request["seq"],"command":request["command"],"success":false,"message":format!("{error:#}")})
            }
        };
        self.send(message)
    }
}

/// Serialize incrementally: a limit is checked before every Vec growth rather
/// than after an oversized serialized response has already been allocated.
struct BoundedJson(Vec<u8>);
impl Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|total| total > MAX_MESSAGE)
        {
            return Err(std::io::Error::other("DAP output exceeds message budget"));
        }
        self.0
            .try_reserve(bytes.len())
            .map_err(std::io::Error::other)?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(unix)]
struct NonblockingFd {
    fd: i32,
    flags: i32,
    restored: bool,
}
#[cfg(unix)]
impl NonblockingFd {
    fn new(fd: i32) -> Result<Self> {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        ensure!(
            flags >= 0,
            "cannot inspect DAP descriptor: {}",
            std::io::Error::last_os_error()
        );
        ensure!(
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0,
            "cannot configure DAP descriptor: {}",
            std::io::Error::last_os_error()
        );
        Ok(Self {
            fd,
            flags,
            restored: false,
        })
    }
    fn restore(&mut self) -> Result<()> {
        if !self.restored {
            ensure!(
                unsafe { libc::fcntl(self.fd, libc::F_SETFL, self.flags) } == 0,
                "cannot restore DAP descriptor flags: {}",
                std::io::Error::last_os_error()
            );
            self.restored = true;
        }
        Ok(())
    }
}
#[cfg(unix)]
impl Drop for NonblockingFd {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            eprintln!("DAP descriptor cleanup incomplete: {error:#}");
        }
    }
}

#[cfg(unix)]
struct DeadlineWriter {
    fd: i32,
    timeout: std::time::Duration,
    end: Option<std::time::Instant>,
}
#[cfg(unix)]
impl Write for DeadlineWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let end = *self
            .end
            .get_or_insert_with(|| std::time::Instant::now() + self.timeout);
        loop {
            if std::time::Instant::now() >= end {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "DAP client stopped consuming stdout",
                ));
            }
            let count = unsafe { libc::write(self.fd, bytes.as_ptr().cast(), bytes.len()) };
            if count >= 0 {
                return Ok(count as usize);
            }
            let error = std::io::Error::last_os_error();
            match error.kind() {
                std::io::ErrorKind::Interrupted => continue,
                std::io::ErrorKind::WouldBlock => {
                    let mut fd = libc::pollfd {
                        fd: self.fd,
                        events: libc::POLLOUT,
                        revents: 0,
                    };
                    let remaining = end
                        .saturating_duration_since(std::time::Instant::now())
                        .as_millis()
                        .clamp(1, 10) as i32;
                    let result = unsafe { libc::poll(&mut fd, 1, remaining) };
                    if result < 0 {
                        let error = std::io::Error::last_os_error();
                        if error.kind() != std::io::ErrorKind::Interrupted {
                            return Err(error);
                        }
                    }
                    if fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            "DAP stdout peer closed",
                        ));
                    }
                }
                _ => return Err(error),
            }
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.end = None;
        Ok(())
    }
}

/// Reads client cancellation/EOF at JDWP checkpoints on the same thread. The
/// pending queue is bounded and kept in order; it never drops a valid request.
#[cfg(unix)]
struct InputPump {
    fd: i32,
    framing: Framing,
    pending: std::collections::VecDeque<(Value, usize)>,
    bytes: usize,
    cancelled: std::collections::BTreeSet<u64>,
    eof: bool,
}
#[cfg(unix)]
impl InputPump {
    fn new(fd: i32) -> Self {
        Self {
            fd,
            framing: Framing::default(),
            pending: Default::default(),
            bytes: 0,
            cancelled: Default::default(),
            eof: false,
        }
    }
    fn pump(&mut self) -> Result<()> {
        if !self.eof {
            // At most 64 KiB per checkpoint; a continuously writing client must
            // not starve a target operation or consume an unbounded input queue.
            for _ in 0..8 {
                let mut bytes = [0; 8192];
                let count = unsafe { libc::read(self.fd, bytes.as_mut_ptr().cast(), bytes.len()) };
                if count == 0 {
                    self.eof = true;
                    break;
                }
                if count < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() == std::io::ErrorKind::WouldBlock {
                        break;
                    }
                    if error.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error.into());
                }
                self.framing.push(&bytes[..count as usize])?;
                ensure!(
                    self.bytes
                        .checked_add(self.framing.bytes.len())
                        .is_some_and(|n| n <= MAX_MESSAGE + MAX_HEADER),
                    "DAP queued input exceeds budget"
                );
            }
        }
        while let Some(request) = self.framing.next_message()? {
            ensure!(
                self.pending.len() < 32,
                "DAP pending request count exceeds budget"
            );
            let mut encoded = BoundedJson(Vec::new());
            serde_json::to_writer(&mut encoded, &request)?;
            let size = encoded.0.len();
            self.bytes = self
                .bytes
                .checked_add(size)
                .context("DAP queue size overflow")?;
            ensure!(self.bytes <= MAX_MESSAGE, "DAP queued input exceeds budget");
            if request["command"] == "cancel" {
                let id = request["arguments"]["requestId"]
                    .as_u64()
                    .context("cancel requestId is required")?;
                ensure!(
                    self.cancelled.len() < 32 || self.cancelled.contains(&id),
                    "DAP cancellation count exceeds budget"
                );
                self.cancelled.insert(id);
            }
            self.pending.push_back((request, size));
        }
        if self.eof {
            self.framing.finish()?;
        }
        Ok(())
    }
    fn checkpoint(&mut self, sequence: u64) -> Result<()> {
        self.pump()?;
        ensure!(!self.eof, "DAP client disconnected during operation");
        ensure!(
            !self.cancelled.contains(&sequence),
            "DAP request was cancelled"
        );
        Ok(())
    }
    fn pop(&mut self) -> Option<Value> {
        self.pending.pop_front().map(|(request, bytes)| {
            self.bytes -= bytes;
            request
        })
    }
}

fn capabilities() -> Value {
    json!({"supportsConfigurationDoneRequest":true,"supportsEvaluateForHovers":true,
        "supportsCompletionsRequest":true,"supportsExceptionInfoRequest":true,
        "supportsVariablePaging":true,"supportsCancelRequest":true,"supportsTerminateRequest":false,
        "supportsConditionalBreakpoints":false,"supportsHitConditionalBreakpoints":false,
        "supportsLogPoints":false,"supportsSetVariable":false,
        "exceptionBreakpointFilters":[{"filter":"C","label":"Caught exceptions","default":false},
                                     {"filter":"U","label":"Uncaught exceptions","default":false}]})
}

/// Error messages are diagnostics, not unbounded target-controlled responses.
fn background_diagnostic(error: &anyhow::Error, retry: bool) -> String {
    struct Prefix(String);
    impl std::fmt::Write for Prefix {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            let mut count = (4096 - self.0.len()).min(text.len());
            while !text.is_char_boundary(count) {
                count -= 1;
            }
            self.0.push_str(&text[..count]);
            Ok(())
        }
    }
    let mut output = Prefix(String::new());
    use std::fmt::Write as _;
    // Prefix::write_str always succeeds and never grows past 4 KiB.
    let result = write!(
        &mut output,
        "Android debugger background work {}: {error:#}",
        if retry {
            "will retry"
        } else {
            "failed; target cleanup may be incomplete"
        }
    );
    debug_assert!(result.is_ok());
    output.0.push('\n');
    output.0
}

#[cfg(unix)]
enum PollOutcome {
    Ready,
    Retry,
    Terminated,
}

#[cfg(unix)]
fn poll_session(
    session: &mut Session,
    output: &mut Output<DeadlineWriter>,
    last_diagnostic: &mut Option<String>,
    pending_attach: &mut Option<Value>,
) -> Result<PollOutcome> {
    let result = session.poll();
    let delivery_end = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let deliver = |output: &mut Output<DeadlineWriter>, event| -> Result<()> {
        // Every notification in this turn shares one delivery deadline. A slow
        // client cannot restart five seconds for each retained outbox entry.
        output.writer.end = Some(delivery_end);
        output.event(event)
    };
    match result {
        Ok(events) => {
            if session.is_terminated() {
                if let Some(attach) = pending_attach.take() {
                    output.response(
                        &attach,
                        Err(anyhow::anyhow!(
                            "Target terminated before configuration completed"
                        )),
                    )?;
                }
            }
            for event in events {
                deliver(output, event)?;
            }
            *last_diagnostic = None;
            Ok(if session.is_terminated() {
                PollOutcome::Terminated
            } else {
                PollOutcome::Ready
            })
        }
        Err(error) => {
            if session.is_terminated() {
                // VM_DEAD112 is authoritative terminal evidence even when the
                // well-framed socket remains open. Never retry a dead target.
                if let Some(attach) = pending_attach.take() {
                    output.response(
                        &attach,
                        Err(anyhow::anyhow!(
                            "Target terminated before configuration completed"
                        )),
                    )?;
                }
                deliver(
                    output,
                    json!({"event":"output","body":{"category":"console","output":"JDWP target terminated; the debug session has ended.\n"}}),
                )?;
                while let Some(event) = session.take_pending_event() {
                    deliver(output, event)?;
                }
                return Ok(PollOutcome::Terminated);
            }
            // Successful stops and verification notifications are committed
            // independently of a later source/cleanup error. Deliver them first.
            while let Some(event) = session.take_pending_event() {
                deliver(output, event)?;
            }
            let retry = session.poll_error_is_recoverable(&error);
            let diagnostic = background_diagnostic(&error, retry);
            if last_diagnostic.as_ref() != Some(&diagnostic) {
                deliver(
                    output,
                    json!({"event":"output","body":{"category":"stderr","output":diagnostic}}),
                )?;
                *last_diagnostic = Some(diagnostic);
            }
            if retry {
                Ok(PollOutcome::Retry)
            } else {
                deliver(output, json!({"event":"terminated","body":{}}))?;
                Err(error.context("fatal Android debugger background failure"))
            }
        }
    }
}

#[cfg(unix)]
pub fn serve(input: std::io::Stdin, output: std::io::Stdout) -> Result<()> {
    use std::os::fd::AsRawFd;
    let mut stdin = NonblockingFd::new(input.as_raw_fd())?;
    let mut stdout = NonblockingFd::new(output.as_raw_fd())?;
    let result = serve_inner(stdin.fd, stdout.fd);
    let input_cleanup = stdin.restore();
    let output_cleanup = stdout.restore();
    match (result, input_cleanup.and(output_cleanup)) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("descriptor cleanup incomplete: {cleanup:#}")))
        }
    }
}

#[cfg(unix)]
fn serve_inner(fd: i32, output: i32) -> Result<()> {
    let pump = std::rc::Rc::new(std::cell::RefCell::new(InputPump::new(fd)));
    let mut out = Output {
        writer: DeadlineWriter {
            fd: output,
            timeout: std::time::Duration::from_secs(5),
            end: None,
        },
        sequence: 1,
    };
    let mut session: Option<Session> = None;
    let mut pending_attach: Option<Value> = None;
    let mut initialized = false;
    let mut next_background_poll = std::time::Instant::now();
    let mut last_background_diagnostic = None;
    loop {
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let polled = unsafe { libc::poll(&mut descriptor, 1, 10) };
        if polled < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        ensure!(
            descriptor.revents & (libc::POLLERR | libc::POLLNVAL) == 0,
            "debugger stdin poll failed"
        );
        pump.borrow_mut().pump()?;
        if pump.borrow().eof && pump.borrow().pending.is_empty() {
            if let Some(mut session) = session {
                session.disconnect(false)?;
            }
            return Ok(());
        }
        // Bound requests per turn so a continuously writing client cannot
        // starve VM death, breakpoint, class-prepare, or cancellation events.
        for _ in 0..1 {
            let Some(request) = pump.borrow_mut().pop() else {
                break;
            };
            ensure!(request["type"] == "request", "DAP input must be a request");
            ensure!(
                request["seq"].as_u64().is_some(),
                "DAP request sequence is required"
            );
            let command = request["command"]
                .as_str()
                .context("DAP command is required")?;
            let args = &request["arguments"];
            match command {
                "initialize" => {
                    let result = (|| {
                        ensure!(!initialized, "adapter was already initialized");
                        ensure!(
                            args["linesStartAt1"].as_bool().unwrap_or(true)
                                && args["columnsStartAt1"].as_bool().unwrap_or(true),
                            "only one-based source coordinates are supported"
                        );
                        ensure!(
                            args["pathFormat"].as_str().unwrap_or("path") == "path",
                            "source paths must use native path format"
                        );
                        initialized = true;
                        Ok(capabilities())
                    })();
                    out.response(&request, result)?;
                }
                "attach" => {
                    let result = (|| {
                        ensure!(initialized, "initialize must precede attach");
                        ensure!(session.is_none(), "adapter already owns a VM connection");
                        let checkpoint = pump.clone();
                        let sequence = request["seq"]
                            .as_u64()
                            .context("request sequence is required")?;
                        Session::attach_with_checkpoint(
                            args,
                            Some(Box::new(move || {
                                checkpoint.borrow_mut().checkpoint(sequence)
                            })),
                        )
                    })();
                    match result {
                        Ok(attached) => {
                            let mut attached = attached;
                            attached.set_checkpoint(None);
                            session = Some(attached);
                            pending_attach = Some(request.clone());
                            out.event(json!({"event":"initialized","body":{}}))?;
                        }
                        Err(error) => out.response(&request, Err(error))?,
                    }
                }
                "cancel" => {
                    out.response(&request, Ok(json!({})))?;
                }
                "disconnect" => {
                    let result = session
                        .as_mut()
                        .context("no attached VM")
                        .and_then(|session| {
                            session.disconnect(args["terminateDebuggee"].as_bool().unwrap_or(false))
                        });
                    out.response(&request, result)?;
                    out.event(json!({"event":"terminated","body":{}}))?;
                    return Ok(());
                }
                _ => {
                    let result = (|| {
                        let session = session.as_mut().context("no attached VM")?;
                        let pump = pump.clone();
                        let sequence = request["seq"]
                            .as_u64()
                            .context("request sequence is required")?;
                        session.set_checkpoint(Some(Box::new(move || {
                            pump.borrow_mut().checkpoint(sequence)
                        })));
                        match command {
                            "configurationDone" => session.configure(),
                            "setBreakpoints" => session.set_breakpoints(args),
                            "setExceptionBreakpoints" => session.set_exception_breakpoints(args),
                            "threads" => session.threads(),
                            "stackTrace" => session.stack_trace(args),
                            "scopes" => session.scopes(args),
                            "variables" => session.variables(args),
                            "evaluate" => session.evaluate(args),
                            "completions" => session.completions(args),
                            "exceptionInfo" => session.exception_info(args),
                            "continue" => session.continue_request(args),
                            "pause" => session.pause(args),
                            "next" | "stepIn" | "stepOut" => session.step(command, args),
                            _ => bail!("unsupported DAP command {command}"),
                        }
                    })();
                    if let Some(session) = session.as_mut() {
                        session.set_checkpoint(None);
                    }
                    pump.borrow_mut().cancelled.remove(
                        &request["seq"]
                            .as_u64()
                            .context("request sequence is required")?,
                    );
                    if command == "configurationDone" && result.is_ok() {
                        if let Some(attach) = pending_attach.take() {
                            out.response(&attach, Ok(json!({})))?;
                        }
                    }
                    let paused = command == "pause" && result.is_ok();
                    let resumed = matches!(command, "continue" | "next" | "stepIn" | "stepOut")
                        && result.is_ok();
                    let pause_thread = result
                        .as_ref()
                        .ok()
                        .and_then(|v| v.get("threadId"))
                        .cloned();
                    out.response(&request, result)?;
                    if paused {
                        out.event(json!({"event":"stopped","body":{"reason":"pause","threadId":pause_thread,"allThreadsStopped":true}}))?;
                    }
                    if resumed {
                        out.event(json!({"event":"continued","body":{"threadId":args["threadId"],"allThreadsContinued":true}}))?;
                    }
                }
            }
        }
        if let Some(session) = session.as_mut() {
            if std::time::Instant::now() >= next_background_poll {
                let retry = match poll_session(
                    session,
                    &mut out,
                    &mut last_background_diagnostic,
                    &mut pending_attach,
                )? {
                    PollOutcome::Terminated => return Ok(()),
                    PollOutcome::Retry => true,
                    PollOutcome::Ready => false,
                };
                // Backoff applies only to background retries. Client requests,
                // including disconnect, continue to be handled every turn.
                next_background_poll = std::time::Instant::now()
                    + if retry {
                        std::time::Duration::from_millis(100)
                    } else {
                        std::time::Duration::ZERO
                    };
            }
        }
    }
}

#[cfg(not(unix))]
pub fn serve(_: std::io::Stdin, _: std::io::Stdout) -> Result<()> {
    bail!(
        "Android Rust debugger stdio transport currently requires Unix; JDWP and framing are platform-independent"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragmented_frames_preserve_pipelined_request_order() {
        let mut f = Framing::default();
        let body = br#"{"seq":1,"type":"request","command":"threads"}"#;
        let mut packet = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        packet.extend(body);
        for byte in &packet[..packet.len() - 1] {
            f.push(&[*byte]).unwrap();
            assert!(f.next_message().unwrap().is_none());
        }
        f.push(&packet[packet.len() - 1..]).unwrap();
        assert_eq!(f.next_message().unwrap().unwrap()["command"], "threads");
        f.push(&packet).unwrap();
        f.push(&packet).unwrap();
        assert_eq!(f.next_message().unwrap().unwrap()["seq"], 1);
        assert_eq!(f.next_message().unwrap().unwrap()["seq"], 1);
        f.finish().unwrap();
    }

    #[test]
    fn malformed_and_oversized_headers_are_rejected() {
        for header in [
            "Content-Length: 999999999\r\n\r\n",
            "Content-Length: 1\r\nContent-Length: 1\r\n\r\n",
            "Content-Length: -1\r\n\r\n",
            "Other: x\r\n\r\n",
        ] {
            let mut f = Framing::default();
            f.push(header.as_bytes()).unwrap();
            assert!(f.next_message().is_err());
        }
        let mut f = Framing::default();
        f.push(b"Content-Length: 1\r\n\r\n").unwrap();
        assert!(f.finish().is_err());
    }

    #[test]
    fn capabilities_never_claim_target_evaluation() {
        let c = capabilities();
        assert_eq!(c["supportsEvaluateForHovers"], true);
        assert_eq!(c["supportsConditionalBreakpoints"], false);
        assert_eq!(c["supportsSetVariable"], false);
    }
}

#[cfg(test)]
mod review_regressions {
    use super::*;

    #[test]
    fn serialization_is_bounded_before_buffer_growth_or_delivery() {
        let mut buffer = BoundedJson(vec![0; MAX_MESSAGE - 1]);
        assert!(buffer.write_all(&[1, 2]).is_err());
        assert_eq!(buffer.0.len(), MAX_MESSAGE - 1);
        let mut output = Output {
            writer: Vec::new(),
            sequence: 1,
        };
        assert!(
            output
                .send(json!({"payload":"x".repeat(MAX_MESSAGE)}))
                .is_err()
        );
        assert!(output.writer.is_empty());
    }

    #[cfg(unix)]
    fn pipe() -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd) {
        use std::os::fd::FromRawFd;
        let mut fds = [-1; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        unsafe {
            (
                std::os::fd::OwnedFd::from_raw_fd(fds[0]),
                std::os::fd::OwnedFd::from_raw_fd(fds[1]),
            )
        }
    }

    #[test]
    #[cfg(unix)]
    fn stalled_real_stdout_pipe_times_out_and_restores_shared_descriptor_flags() {
        use std::os::fd::AsRawFd;
        let (_reader, writer) = pipe();
        let fd = writer.as_raw_fd();
        let original = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        let mut flags = NonblockingFd::new(fd).unwrap();
        let bytes = [0_u8; 4096];
        loop {
            let count = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
            if count < 0 {
                assert_eq!(
                    std::io::Error::last_os_error().kind(),
                    std::io::ErrorKind::WouldBlock
                );
                break;
            }
            assert!(count > 0);
        }
        let start = std::time::Instant::now();
        let mut output = DeadlineWriter {
            fd,
            timeout: std::time::Duration::from_millis(10),
            end: None,
        };
        assert_eq!(
            output.write_all(&[1]).unwrap_err().kind(),
            std::io::ErrorKind::TimedOut
        );
        assert!(start.elapsed() < std::time::Duration::from_millis(500));
        flags.restore().unwrap();
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFL) }, original);
        drop(flags);
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFL) }, original);
    }

    #[test]
    #[cfg(unix)]
    fn input_checkpoints_observe_cancellation_and_disconnect_without_dropping_requests() {
        use std::os::fd::AsRawFd;
        let (reader, writer) = pipe();
        let mut flags = NonblockingFd::new(reader.as_raw_fd()).unwrap();
        let message =
            br#"{"seq":2,"type":"request","command":"cancel","arguments":{"requestId":1}}"#;
        let mut packet = format!("Content-Length: {}\r\n\r\n", message.len()).into_bytes();
        packet.extend(message);
        assert_eq!(
            unsafe { libc::write(writer.as_raw_fd(), packet.as_ptr().cast(), packet.len()) },
            packet.len() as isize
        );
        let mut pump = InputPump::new(reader.as_raw_fd());
        assert!(
            pump.checkpoint(1)
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
        assert_eq!(pump.pop().unwrap()["seq"], 2);
        assert!(pump.pop().is_none());
        drop(writer);
        assert!(
            pump.checkpoint(3)
                .unwrap_err()
                .to_string()
                .contains("disconnected")
        );
        flags.restore().unwrap();
    }
}

#[cfg(all(test, unix))]
mod background_delivery_regressions {
    use super::*;
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        os::{fd::AsRawFd, unix::net::UnixStream},
        time::{Duration, Instant},
    };

    fn text(text: &str) -> Vec<u8> {
        let mut bytes = (text.len() as u32).to_be_bytes().to_vec();
        bytes.extend(text.as_bytes());
        bytes
    }

    fn request(stream: &mut TcpStream) -> (u32, u8, u8, Vec<u8>) {
        let mut header = [0; 11];
        stream.read_exact(&mut header).unwrap();
        let size = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        assert!((11..=crate::wire::MAX_PACKET_BYTES).contains(&size));
        assert_eq!(header[8], 0);
        let mut body = vec![0; size - 11];
        stream.read_exact(&mut body).unwrap();
        (
            u32::from_be_bytes(header[4..8].try_into().unwrap()),
            header[9],
            header[10],
            body,
        )
    }

    fn reply(id: u32, error: u16, body: Vec<u8>) -> Vec<u8> {
        let mut packet = ((11 + body.len()) as u32).to_be_bytes().to_vec();
        packet.extend(id.to_be_bytes());
        packet.push(0x80);
        packet.extend(error.to_be_bytes());
        packet.extend(body);
        packet
    }

    fn composite(body: Vec<u8>) -> Vec<u8> {
        let mut packet = ((11 + body.len()) as u32).to_be_bytes().to_vec();
        packet.extend(1000_u32.to_be_bytes());
        packet.extend([0, 64, 100]);
        packet.extend(body);
        packet
    }

    fn target(
        f: impl FnOnce(&mut TcpStream) -> Vec<(u8, u8, Vec<u8>)> + Send + 'static,
    ) -> (u16, std::thread::JoinHandle<Vec<(u8, u8, Vec<u8>)>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = std::thread::spawn(move || {
            let end = Instant::now() + Duration::from_secs(3);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < end, "adapter did not attach");
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("target accept: {error}"),
                }
            };
            stream.set_nodelay(true).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut handshake = [0; 14];
            stream.read_exact(&mut handshake).unwrap();
            assert_eq!(&handshake, b"JDWP-Handshake");
            stream.write_all(&handshake).unwrap();
            for (set, command, body) in [
                (
                    1,
                    7,
                    [8_u32; 5].into_iter().flat_map(u32::to_be_bytes).collect(),
                ),
                (1, 8, vec![]),
                (1, 3, 0_u32.to_be_bytes().to_vec()),
                (15, 1, 100_u32.to_be_bytes().to_vec()),
                (15, 1, 101_u32.to_be_bytes().to_vec()),
            ] {
                let (id, s, c, _) = request(&mut stream);
                assert_eq!((s, c), (set, command));
                stream.write_all(&reply(id, 0, body)).unwrap();
            }
            f(&mut stream)
        });
        (port, task)
    }

    struct Client {
        stream: UnixStream,
    }
    impl Client {
        fn send(&mut self, sequence: u64, command: &str, arguments: Value) {
            let body = serde_json::to_vec(
                &json!({"seq":sequence,"type":"request","command":command,"arguments":arguments}),
            )
            .unwrap();
            write!(self.stream, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
            self.stream.write_all(&body).unwrap();
        }
        fn receive(&mut self) -> Value {
            let mut framing = Framing::default();
            loop {
                let mut byte = [0; 1];
                self.stream.read_exact(&mut byte).unwrap();
                framing.push(&byte).unwrap();
                if let Some(message) = framing.next_message().unwrap() {
                    return message;
                }
            }
        }
        fn attach(&mut self, port: u16, root: &std::path::Path) {
            self.send(1, "initialize", json!({}));
            let response = self.receive();
            assert_eq!(response["request_seq"], 1);
            assert_eq!(response["success"], true);
            self.send(2,"attach",json!({"hostName":"127.0.0.1","port":port,"projectRoot":root,"sourceRoots":[root],"timeout":1000}));
            assert_eq!(self.receive()["event"], "initialized");
        }
    }

    fn adapter() -> (Client, std::thread::JoinHandle<Result<()>>) {
        let (server, stream) = UnixStream::pair().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let task = std::thread::spawn(move || {
            let mut flags = NonblockingFd::new(server.as_raw_fd())?;
            let result = serve_inner(server.as_raw_fd(), server.as_raw_fd());
            let cleanup = flags.restore();
            match (result, cleanup) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(error), Ok(())) => Err(error),
                (Ok(()), Err(error)) => Err(error),
                (Err(error), Err(cleanup)) => {
                    Err(error.context(format!("fixture descriptor cleanup: {cleanup:#}")))
                }
            }
        });
        (Client { stream }, task)
    }

    #[test]
    fn actual_dap_loop_reports_recoverable_resume_and_keeps_client_requests_alive() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("Main.java");
        std::fs::write(&source, "class Main {}\n").unwrap();
        let (port, vm) = target(|stream| {
            let (id, s, c, body) = request(stream);
            assert_eq!((s, c), (15, 1));
            assert_eq!(body, [8, 1, 0, 0, 0, 0]);
            stream
                .write_all(&reply(id, 0, 500_u32.to_be_bytes().to_vec()))
                .unwrap();
            let (id, s, c, _) = request(stream);
            assert_eq!((s, c), (1, 3));
            let mut packet = reply(id, 0, 0_u32.to_be_bytes().to_vec());
            let mut event = vec![1];
            event.extend(1_u32.to_be_bytes());
            event.push(8);
            event.extend(500_u32.to_be_bytes());
            event.extend(77_u64.to_be_bytes());
            event.push(1);
            event.extend(99_u64.to_be_bytes());
            event.extend(text("Lforeign/None;"));
            event.extend(2_u32.to_be_bytes());
            packet.extend(composite(event));
            stream.write_all(&packet).unwrap();
            let mut requests = Vec::new();
            let mut resumes = 0;
            loop {
                let (id, s, c, body) = request(stream);
                requests.push((s, c, body.clone()));
                match (s, c) {
                    (2, 12) | (2, 7) => stream.write_all(&reply(id, 101, vec![])).unwrap(),
                    (11, 3) => {
                        assert_eq!(body, 77_u64.to_be_bytes());
                        resumes += 1;
                        stream
                            .write_all(&reply(id, if resumes == 1 { 20 } else { 0 }, vec![]))
                            .unwrap();
                        if resumes == 2 {
                            let mut event = vec![0];
                            event.extend(1_u32.to_be_bytes());
                            event.push(6);
                            event.extend(100_u32.to_be_bytes());
                            event.extend(88_u64.to_be_bytes());
                            stream.write_all(&composite(event)).unwrap();
                        }
                    }
                    (1, 4) => stream
                        .write_all(&reply(id, 0, 0_u32.to_be_bytes().to_vec()))
                        .unwrap(),
                    (1, 6) => {
                        stream.write_all(&reply(id, 0, vec![])).unwrap();
                        break;
                    }
                    _ => panic!("unexpected target command {s}.{c}"),
                }
            }
            assert_eq!(resumes, 2);
            requests
        });
        let (mut client, adapter) = adapter();
        client.attach(port, temporary.path());
        client.send(
            3,
            "setBreakpoints",
            json!({"source":{"path":source},"breakpoints":[{"line":9}]}),
        );
        let mut diagnosed = false;
        let mut response = false;
        for _ in 0..4 {
            let message = client.receive();
            if message["request_seq"] == 3 {
                assert_eq!(message["success"], true);
                response = true;
            }
            if message["event"] == "output" {
                assert_eq!(message["body"]["category"], "stderr");
                assert!(
                    message["body"]["output"]
                        .as_str()
                        .unwrap()
                        .contains("will retry")
                );
                diagnosed = true;
            }
            if response && diagnosed {
                break;
            }
        }
        assert!(response && diagnosed);
        client.send(4, "threads", json!({}));
        let mut threads = false;
        let mut resumed = false;
        for _ in 0..4 {
            let message = client.receive();
            if message["request_seq"] == 4 {
                assert_eq!(message["success"], true);
                threads = true;
            }
            if message["event"] == "thread" {
                assert_eq!(message["body"]["reason"], "started");
                resumed = true;
            }
            if threads && resumed {
                break;
            }
        }
        assert!(threads && resumed);
        client.send(5, "disconnect", json!({}));
        assert_eq!(client.receive()["success"], true);
        assert_eq!(client.receive()["event"], "terminated");
        adapter.join().unwrap().unwrap();
        let requests = vm.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (11, 3))
                .count(),
            2
        );
        assert!(requests.iter().all(|(s, c, _)| (*s, *c) != (1, 9)));
    }

    #[test]
    fn actual_dap_delivers_a_real_stop_before_the_later_indexing_error() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary.path().join("app")).unwrap();
        let source = temporary.path().join("app/Main.java");
        std::fs::write(&source, "package app; class Main {}\n").unwrap();
        let (port, vm) = target(|stream| {
            let mut requests = Vec::new();
            let mut classes = 2_u32.to_be_bytes().to_vec();
            for (id, signature) in [(99_u64, "Lapp/Main;"), (100, "Lforeign/Unknown;")] {
                classes.push(1);
                classes.extend(id.to_be_bytes());
                classes.extend(text(signature));
                classes.extend(2_u32.to_be_bytes());
            }
            let mut methods = 1_u32.to_be_bytes().to_vec();
            methods.extend(7_u64.to_be_bytes());
            methods.extend(text("main"));
            methods.extend(text("()V"));
            methods.extend(0_u32.to_be_bytes());
            let mut lines = 0_u64.to_be_bytes().to_vec();
            lines.extend(0_u64.to_be_bytes());
            lines.extend(1_u32.to_be_bytes());
            lines.extend(0_u64.to_be_bytes());
            lines.extend(9_u32.to_be_bytes());
            let expected = vec![
                (15, 1, 0, 500_u32.to_be_bytes().to_vec()),
                (1, 3, 0, classes),
                (2, 12, 101, vec![]),
                (2, 7, 0, text("Main.java")),
                (2, 5, 0, methods),
                (6, 1, 0, lines),
                (15, 1, 0, 900_u32.to_be_bytes().to_vec()),
            ];
            for (turn, (set, command, error, body)) in expected.into_iter().enumerate() {
                let (id, s, c, bytes) = request(stream);
                assert_eq!((s, c), (set, command));
                requests.push((s, c, bytes));
                let mut packet = reply(id, error, body);
                if turn == 6 {
                    let mut event = vec![2];
                    event.extend(1_u32.to_be_bytes());
                    event.push(2);
                    event.extend(900_u32.to_be_bytes());
                    event.extend(77_u64.to_be_bytes());
                    event.push(1);
                    event.extend(99_u64.to_be_bytes());
                    event.extend(7_u64.to_be_bytes());
                    event.extend(0_u64.to_be_bytes());
                    packet.extend(composite(event));
                }
                stream.write_all(&packet).unwrap();
            }
            let (id, s, c, body) = request(stream);
            assert_eq!((s, c), (2, 12));
            assert_eq!(body, 100_u64.to_be_bytes());
            requests.push((s, c, body));
            stream.write_all(&reply(id, 20, vec![])).unwrap();
            loop {
                let (id, s, c, body) = request(stream);
                requests.push((s, c, body));
                match (s, c) {
                    (2, 12) | (2, 7) => stream.write_all(&reply(id, 101, vec![])).unwrap(),
                    (1, 4) => stream
                        .write_all(&reply(id, 0, 0_u32.to_be_bytes().to_vec()))
                        .unwrap(),
                    (1, 6) => {
                        stream.write_all(&reply(id, 0, vec![])).unwrap();
                        break;
                    }
                    _ => panic!("unexpected target command {s}.{c}"),
                }
            }
            requests
        });
        let (mut client, adapter) = adapter();
        client.attach(port, temporary.path());
        client.send(
            3,
            "setBreakpoints",
            json!({"source":{"path":source},"breakpoints":[{"line":9}]}),
        );
        let response = client.receive();
        assert_eq!(response["request_seq"], 3);
        assert_eq!(response["body"]["breakpoints"][0]["verified"], true);
        let stopped = client.receive();
        assert_eq!(stopped["event"], "stopped");
        assert_eq!(stopped["body"]["reason"], "breakpoint");
        assert_eq!(stopped["body"]["allThreadsStopped"], true);
        let diagnostic = client.receive();
        assert_eq!(diagnostic["event"], "output");
        assert!(
            diagnostic["body"]["output"]
                .as_str()
                .unwrap()
                .contains("will retry")
        );
        client.send(4, "threads", json!({}));
        let response = client.receive();
        assert_eq!(response["request_seq"], 4);
        assert_eq!(response["success"], true);
        client.send(5, "disconnect", json!({}));
        assert_eq!(client.receive()["success"], true);
        assert_eq!(client.receive()["event"], "terminated");
        adapter.join().unwrap().unwrap();
        let requests = vm.join().unwrap();
        assert!(
            requests
                .iter()
                .all(|(s, c, _)| !matches!((*s, *c), (1, 9) | (11, 3)))
        );
    }

    #[test]
    fn actual_dap_reports_a_poisoned_transport_as_fatal_and_does_not_retry_it() {
        let (port, vm) = target(|stream| {
            let mut malformed = 10_u32.to_be_bytes().to_vec();
            malformed.extend(1000_u32.to_be_bytes());
            malformed.extend([0, 64, 100]);
            stream.write_all(&malformed).unwrap();
            Vec::new()
        });
        let (mut client, adapter) = adapter();
        client.attach(port, std::path::Path::new("/android"));
        let diagnostic = client.receive();
        assert_eq!(diagnostic["event"], "output");
        let output = diagnostic["body"]["output"].as_str().unwrap();
        assert!(output.contains("failed; target cleanup may be incomplete"));
        assert!(!output.contains("will retry"));
        assert_eq!(client.receive()["event"], "terminated");
        assert!(
            adapter
                .join()
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("fatal Android debugger")
        );
        assert!(vm.join().unwrap().is_empty());
    }

    fn replacement_step_failure_messages(
        include_breakpoint: bool,
    ) -> (Vec<Value>, Vec<(u8, u8, Vec<u8>)>) {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary.path().join("app")).unwrap();
        let source = temporary.path().join("app/Main.java");
        std::fs::write(
            &source,
            "package app;\nclass Main {\n\n\n\n\n\n\nvoid main() {}\n}\n",
        )
        .unwrap();
        let (port, vm) = target(move |stream| {
            let mut requests = Vec::new();
            let mut respond = |set: u8, command: u8, error: u16, body: Vec<u8>| {
                let (id, s, c, request_body) = request(stream);
                assert_eq!((s, c), (set, command));
                requests.push((s, c, request_body));
                stream.write_all(&reply(id, error, body)).unwrap();
            };
            respond(15, 1, 0, 500_u32.to_be_bytes().to_vec());
            let mut classes = 1_u32.to_be_bytes().to_vec();
            classes.push(1);
            classes.extend(3_u64.to_be_bytes());
            classes.extend(text("Lapp/Main;"));
            classes.extend(2_u32.to_be_bytes());
            respond(1, 3, 0, classes);
            respond(2, 12, 101, vec![]);
            respond(2, 7, 0, text("Main.java"));
            let mut methods = 1_u32.to_be_bytes().to_vec();
            methods.extend(4_u64.to_be_bytes());
            methods.extend(text("main"));
            methods.extend(text("()V"));
            methods.extend(0_u32.to_be_bytes());
            respond(2, 5, 0, methods);
            let mut lines = 0_u64.to_be_bytes().to_vec();
            lines.extend(1_u64.to_be_bytes());
            lines.extend(1_u32.to_be_bytes());
            lines.extend(0_u64.to_be_bytes());
            lines.extend(9_u32.to_be_bytes());
            respond(6, 1, 0, lines);
            respond(15, 1, 0, 901_u32.to_be_bytes().to_vec());
            let mut threads = 1_u32.to_be_bytes().to_vec();
            threads.extend(7_u64.to_be_bytes());
            respond(1, 4, 0, threads.clone());
            respond(11, 1, 0, text("main"));
            let location = || {
                let mut location = vec![1];
                location.extend(3_u64.to_be_bytes());
                location.extend(4_u64.to_be_bytes());
                location.extend(0_u64.to_be_bytes());
                location
            };
            let mut frames = 1_u32.to_be_bytes().to_vec();
            frames.extend(8_u64.to_be_bytes());
            frames.extend(location());
            respond(11, 6, 0, frames);
            respond(15, 1, 0, 900_u32.to_be_bytes().to_vec());
            // One legal ALL event set: step and optional breakpoint share the
            // same thread and location. A ClassPrepare mix is deliberately absent.
            drop(respond);
            let (id, s, c, body) = request(stream);
            assert_eq!((s, c), (1, 9));
            requests.push((s, c, body));
            let mut events = vec![2];
            events.extend((if include_breakpoint { 2_u32 } else { 1_u32 }).to_be_bytes());
            events.push(1);
            events.extend(900_u32.to_be_bytes());
            events.extend(7_u64.to_be_bytes());
            events.extend(location());
            if include_breakpoint {
                events.push(2);
                events.extend(901_u32.to_be_bytes());
                events.extend(7_u64.to_be_bytes());
                events.extend(location());
            }
            let mut packet = reply(id, 0, vec![]);
            packet.extend(composite(events));
            stream.write_all(&packet).unwrap();
            let (id, s, c, body) = request(stream);
            assert_eq!((s, c), (15, 2));
            assert_eq!(body, [vec![1], 900_u32.to_be_bytes().to_vec()].concat());
            requests.push((s, c, body));
            stream.write_all(&reply(id, 0, vec![])).unwrap();
            let (id, s, c, body) = request(stream);
            assert_eq!((s, c), (15, 1));
            assert_eq!(body[0], 1);
            requests.push((s, c, body));
            stream.write_all(&reply(id, 113, vec![])).unwrap();
            for (set, command, body) in [(1, 4, threads), (11, 1, text("main")), (1, 6, vec![])] {
                let (id, s, c, request_body) = request(stream);
                assert_eq!((s, c), (set, command));
                requests.push((s, c, request_body));
                stream.write_all(&reply(id, 0, body)).unwrap();
            }
            requests
        });
        let (mut client, adapter) = adapter();
        client.attach(port, temporary.path());
        client.send(
            3,
            "setBreakpoints",
            json!({"source":{"path":source},"breakpoints":[{"line":9}]}),
        );
        let response = client.receive();
        assert_eq!(response["request_seq"], 3);
        assert_eq!(response["body"]["breakpoints"][0]["verified"], true);
        client.send(4, "threads", json!({}));
        let response = client.receive();
        assert_eq!(response["request_seq"], 4);
        assert_eq!(response["body"]["threads"][0]["id"], 1);
        client.send(5, "next", json!({"threadId":1}));
        let response = client.receive();
        assert_eq!(response["request_seq"], 5);
        assert_eq!(response["success"], true);
        assert_eq!(client.receive()["event"], "continued");
        let mut messages = Vec::new();
        for _ in 0..if include_breakpoint { 3 } else { 2 } {
            messages.push(client.receive());
        }
        client.send(6, "threads", json!({}));
        let response = client.receive();
        assert_eq!(response["request_seq"], 6);
        assert_eq!(response["success"], true);
        client.send(7, "disconnect", json!({}));
        assert_eq!(client.receive()["success"], true);
        assert_eq!(client.receive()["event"], "terminated");
        adapter.join().unwrap().unwrap();
        (messages, vm.join().unwrap())
    }

    #[test]
    fn actual_dap_delivers_a_consumed_step_after_clear_ack_and_replacement_set_failure() {
        let (messages, requests) = replacement_step_failure_messages(false);
        assert_eq!(messages[0]["event"], "stopped");
        assert_eq!(messages[0]["body"]["reason"], "step");
        assert_eq!(messages[0]["body"]["allThreadsStopped"], true);
        assert_eq!(messages[1]["event"], "output");
        assert!(
            messages[1]["body"]["output"]
                .as_str()
                .unwrap()
                .contains("will retry")
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (1, 9))
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (15, 2))
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, b)| (*s, *c) == (15, 1) && b[0] == 1)
                .count(),
            2
        );
    }

    #[test]
    fn actual_dap_preserves_the_same_location_breakpoint_tail_after_replacement_step_set_failure() {
        let (messages, requests) = replacement_step_failure_messages(true);
        assert_eq!(messages[0]["event"], "stopped");
        assert_eq!(messages[0]["body"]["reason"], "step");
        assert_eq!(messages[1]["event"], "stopped");
        assert_eq!(messages[1]["body"]["reason"], "breakpoint");
        assert_eq!(
            messages[0]["body"]["threadId"],
            messages[1]["body"]["threadId"]
        );
        assert_eq!(messages[1]["body"]["allThreadsStopped"], true);
        assert_eq!(messages[2]["event"], "output");
        assert!(
            messages[2]["body"]["output"]
                .as_str()
                .unwrap()
                .contains("will retry")
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (1, 9))
                .count(),
            1
        );
        assert!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (15, 2))
                .all(|(_, _, body)| body == &[vec![1], 900_u32.to_be_bytes().to_vec()].concat())
        );
    }

    #[test]
    fn actual_dap_treats_vm_dead_as_terminal_even_while_the_target_socket_remains_open() {
        let (port, vm) = target(|stream| {
            let mut requests = Vec::new();
            let (id, s, c, body) = request(stream);
            assert_eq!((s, c), (1, 9));
            requests.push((s, c, body));
            stream.write_all(&reply(id, 0, vec![])).unwrap();
            let (id, s, c, body) = request(stream);
            assert_eq!((s, c), (15, 1));
            requests.push((s, c, body));
            stream
                .write_all(&reply(id, 0, 500_u32.to_be_bytes().to_vec()))
                .unwrap();
            let (id, s, c, body) = request(stream);
            assert_eq!((s, c), (1, 3));
            requests.push((s, c, body));
            let mut event = vec![1];
            event.extend(1_u32.to_be_bytes());
            event.push(8);
            event.extend(500_u32.to_be_bytes());
            event.extend(77_u64.to_be_bytes());
            event.push(1);
            event.extend(99_u64.to_be_bytes());
            event.extend(text("Lforeign/None;"));
            event.extend(2_u32.to_be_bytes());
            let mut packet = reply(id, 0, 0_u32.to_be_bytes().to_vec());
            packet.extend(composite(event));
            stream.write_all(&packet).unwrap();
            for (set, command, error) in [(2, 12, 101), (2, 7, 101), (11, 3, 112)] {
                let (id, s, c, body) = request(stream);
                assert_eq!((s, c), (set, command));
                if set == 11 {
                    assert_eq!(body, 77_u64.to_be_bytes());
                }
                requests.push((s, c, body));
                stream.write_all(&reply(id, error, vec![])).unwrap();
            }
            // The target does not close first or send VMDeath. Terminal JDWP
            // evidence must close the adapter without issuing another request.
            let mut byte = [0; 1];
            assert_eq!(stream.read(&mut byte).unwrap(), 0);
            requests
        });
        let (mut client, adapter) = adapter();
        client.attach(port, std::path::Path::new("/android"));
        client.send(3, "configurationDone", json!({}));
        let attached = client.receive();
        assert_eq!(attached["request_seq"], 2);
        assert_eq!(attached["success"], true);
        let configured = client.receive();
        assert_eq!(configured["request_seq"], 3);
        assert_eq!(configured["success"], true);
        client.send(
            4,
            "setBreakpoints",
            json!({"source":{"path":"/android/Main.java"},"breakpoints":[{"line":9}]}),
        );
        let response = client.receive();
        assert_eq!(response["request_seq"], 4);
        assert_eq!(response["success"], true);
        let diagnostic = client.receive();
        assert_eq!(diagnostic["event"], "output");
        assert!(
            diagnostic["body"]["output"]
                .as_str()
                .unwrap()
                .contains("target terminated")
        );
        assert!(
            !diagnostic["body"]["output"]
                .as_str()
                .unwrap()
                .contains("will retry")
        );
        assert_eq!(client.receive()["event"], "terminated");
        adapter.join().unwrap().unwrap();
        let mut byte = [0; 1];
        assert_eq!(client.stream.read(&mut byte).unwrap(), 0);
        let requests = vm.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (11, 3))
                .count(),
            1
        );
        assert!(requests.iter().all(|(s, c, _)| (*s, *c) != (1, 6)));
    }

    #[test]
    fn actual_dap_filtered_step_then_promoted_prepare_still_acquires_a_real_user_pause() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary.path().join("app")).unwrap();
        let source = temporary.path().join("app/Main.java");
        std::fs::write(
            &source,
            "package app;\nclass Main {\n\n\n\n\n\n\nvoid main() {}\n}\n",
        )
        .unwrap();
        let (port, vm) = target(|stream| {
            let mut classes = 1_u32.to_be_bytes().to_vec();
            classes.push(1);
            classes.extend(3_u64.to_be_bytes());
            classes.extend(text("Lapp/Main;"));
            classes.extend(2_u32.to_be_bytes());
            let mut methods = 1_u32.to_be_bytes().to_vec();
            methods.extend(4_u64.to_be_bytes());
            methods.extend(text("main"));
            methods.extend(text("()V"));
            methods.extend(0_u32.to_be_bytes());
            let mut lines = 0_u64.to_be_bytes().to_vec();
            lines.extend(1_u64.to_be_bytes());
            lines.extend(1_u32.to_be_bytes());
            lines.extend(0_u64.to_be_bytes());
            lines.extend(9_u32.to_be_bytes());
            let mut threads = 1_u32.to_be_bytes().to_vec();
            threads.extend(7_u64.to_be_bytes());
            let mut location = vec![1];
            location.extend(3_u64.to_be_bytes());
            location.extend(4_u64.to_be_bytes());
            location.extend(0_u64.to_be_bytes());
            let mut frames = 1_u32.to_be_bytes().to_vec();
            frames.extend(8_u64.to_be_bytes());
            frames.extend(&location);
            let mut requests = Vec::new();
            for (set, command, error, body) in [
                (15, 1, 0, 500_u32.to_be_bytes().to_vec()),
                (1, 3, 0, classes),
                (2, 12, 101, vec![]),
                (2, 7, 0, text("Main.java")),
                (2, 5, 0, methods),
                (6, 1, 0, lines),
                (15, 1, 0, 901_u32.to_be_bytes().to_vec()),
                (1, 4, 0, threads),
                (11, 1, 0, text("main")),
                (11, 6, 0, frames),
                (15, 1, 0, 900_u32.to_be_bytes().to_vec()),
            ] {
                let (id, s, c, request_body) = request(stream);
                assert_eq!((s, c), (set, command));
                requests.push((s, c, request_body));
                stream.write_all(&reply(id, error, body)).unwrap();
            }
            let (id, s, c, body) = request(stream);
            assert_eq!((s, c), (1, 9));
            requests.push((s, c, body));
            let mut step = vec![2];
            step.extend(1_u32.to_be_bytes());
            step.push(1);
            step.extend(900_u32.to_be_bytes());
            step.extend(7_u64.to_be_bytes());
            step.extend(location);
            let mut packet = reply(id, 0, vec![]);
            packet.extend(composite(step));
            stream.write_all(&packet).unwrap();
            let (id, s, c, body) = request(stream);
            assert_eq!((s, c), (15, 2));
            assert_eq!(body, [vec![1], 900_u32.to_be_bytes().to_vec()].concat());
            requests.push((s, c, body));
            stream.write_all(&reply(id, 0, vec![])).unwrap();
            let (id, s, c, body) = request(stream);
            assert_eq!((s, c), (15, 1));
            assert_eq!(body[0], 1);
            requests.push((s, c, body));
            stream
                .write_all(&reply(id, 0, 902_u32.to_be_bytes().to_vec()))
                .unwrap();
            let (id, s, c, body) = request(stream);
            assert_eq!((s, c), (1, 9));
            requests.push((s, c, body));
            // This separate event set is a valid promoted system-thread prepare,
            // not a ClassPrepare+SingleStep mixed composite.
            let mut prepare = vec![2];
            prepare.extend(1_u32.to_be_bytes());
            prepare.push(8);
            prepare.extend(500_u32.to_be_bytes());
            prepare.extend(0_u64.to_be_bytes());
            prepare.push(1);
            prepare.extend(99_u64.to_be_bytes());
            prepare.extend(text("Lforeign/Other;"));
            prepare.extend(2_u32.to_be_bytes());
            let mut packet = reply(id, 0, vec![]);
            packet.extend(composite(prepare));
            stream.write_all(&packet).unwrap();
            for (set, command, error) in [(2, 12, 101), (2, 7, 101)] {
                let (id, s, c, body) = request(stream);
                assert_eq!((s, c), (set, command));
                assert_eq!(body, 99_u64.to_be_bytes());
                requests.push((s, c, body));
                stream.write_all(&reply(id, error, vec![])).unwrap();
            }
            let (id, s, c, body) = request(stream);
            assert_eq!((s, c), (1, 9));
            requests.push((s, c, body));
            // A normal thread-start event provides a deterministic client barrier
            // after the internal ALL Resume, without a timing-based test sleep.
            let mut started = vec![0];
            started.extend(1_u32.to_be_bytes());
            started.push(6);
            started.extend(100_u32.to_be_bytes());
            started.extend(8_u64.to_be_bytes());
            let mut packet = reply(id, 0, vec![]);
            packet.extend(composite(started));
            stream.write_all(&packet).unwrap();
            for (set, command) in [(1, 8), (15, 2), (1, 9), (1, 6)] {
                let (id, s, c, body) = request(stream);
                assert_eq!((s, c), (set, command));
                if (s, c) == (15, 2) {
                    assert_eq!(body, [vec![1], 902_u32.to_be_bytes().to_vec()].concat());
                }
                requests.push((s, c, body));
                stream.write_all(&reply(id, 0, vec![])).unwrap();
            }
            requests
        });
        let (mut client, adapter) = adapter();
        client.attach(port, temporary.path());
        client.send(
            3,
            "setBreakpoints",
            json!({"source":{"path":source},"breakpoints":[{"line":9}]}),
        );
        assert_eq!(client.receive()["body"]["breakpoints"][0]["verified"], true);
        client.send(4, "threads", json!({}));
        assert_eq!(client.receive()["body"]["threads"][0]["id"], 1);
        client.send(5, "next", json!({"threadId":1}));
        let response = client.receive();
        assert_eq!(response["request_seq"], 5);
        assert_eq!(response["success"], true);
        assert_eq!(client.receive()["event"], "continued");
        let started = client.receive();
        assert_eq!(started["event"], "thread");
        assert_eq!(started["body"]["reason"], "started");
        client.send(6, "pause", json!({"threadId":1}));
        let response = client.receive();
        assert_eq!(response["request_seq"], 6);
        assert_eq!(response["success"], true);
        let stopped = client.receive();
        assert_eq!(stopped["event"], "stopped");
        assert_eq!(stopped["body"]["reason"], "pause");
        client.send(7, "continue", json!({"threadId":1}));
        assert_eq!(client.receive()["success"], true);
        assert_eq!(client.receive()["event"], "continued");
        client.send(8, "disconnect", json!({}));
        assert_eq!(client.receive()["success"], true);
        assert_eq!(client.receive()["event"], "terminated");
        adapter.join().unwrap().unwrap();
        let requests = vm.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (1, 8))
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(s, c, _)| (*s, *c) == (1, 9))
                .count(),
            4
        );
        assert!(requests.iter().all(|(s, c, _)| (*s, *c) != (11, 3)));
    }

    #[test]
    fn background_error_diagnostics_are_bounded_at_a_utf8_boundary() {
        let error = anyhow::anyhow!("{}", "🦀".repeat(5000));
        let diagnostic = background_diagnostic(&error, true);
        assert!(diagnostic.len() <= 4097);
        assert!(diagnostic.starts_with("Android debugger background work will retry:"));
        assert!(diagnostic.ends_with('\n'));
        assert!(std::str::from_utf8(diagnostic.as_bytes()).is_ok());
    }
}
