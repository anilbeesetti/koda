//! Process-family supervision for the opt-in Gradle fixture CLI.
//!
//! Linux child-subreaping and signal handlers belong only to an isolated CLI
//! worker, never to the IDE or an ordinary library test process. The wrapper
//! leader remains unreaped while its process-group number is used. Adopted
//! children are signalled and reaped through pidfds, not recycled numeric PIDs.
//! Every cleanup has its own finite deadline; failure is reported, not hidden.

// These synchronous CLI probes supervise OS processes outside GPUI.
#![allow(clippy::disallowed_methods)]

use anyhow::Result;
#[cfg(not(target_os = "linux"))]
use anyhow::bail;
#[cfg(target_os = "linux")]
use anyhow::{Context, ensure};
use std::{
    ffi::OsString,
    path::Path,
    process::{Command, ExitStatus},
    time::Duration,
};

#[cfg(target_os = "linux")]
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);

/// Replace the public CLI process with the isolated supervision entry point.
/// Inherited stdout/stderr preserve the historical output without a pipe pump
/// or a parent that could reap the worker before its descendants are closed.
pub(crate) fn exec_worker(args: &[OsString]) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        let error = Command::new(std::env::current_exe()?)
            .arg("probe-test-runner-worker")
            .args(args)
            .exec();
        Err(error).context("Could not replace the test-runner CLI with its worker")
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        bail!(
            "The Gradle fixture acceptance CLI requires Linux pidfd/subreaper supervision; a Windows Job Object or other platform process-family backend has not been implemented"
        )
    }
}

/// Execute fixture logic only after entering a fresh single-threaded worker.
pub(crate) fn in_worker(work: impl FnOnce() -> Result<()>) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        ensure!(
            std::env::args_os().nth(1).as_deref()
                == Some(std::ffi::OsStr::new("probe-test-runner-worker")),
            "Fixture supervision must run in the dedicated CLI worker"
        );
        let mut worker = linux::Worker::enter(true)?;
        let result = work();
        merge_cleanup(result, worker.finish())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = work;
        bail!("This platform has no implemented bounded fixture process-family supervisor")
    }
}

pub(crate) fn wait_for_command(
    command: Command,
    timeout: Duration,
    log_path: Option<&Path>,
    max_log_bytes: u64,
) -> Result<ExitStatus> {
    #[cfg(target_os = "linux")]
    {
        linux::wait_for_command(command, timeout, log_path, max_log_bytes)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (command, timeout, log_path, max_log_bytes);
        bail!("Bounded fixture process-family supervision is unavailable on this platform")
    }
}

#[cfg(target_os = "linux")]
fn merge_cleanup<T>(result: Result<T>, cleanup: Result<()>) -> Result<T> {
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(error), Err(cleanup)) => Err(error.context(format!("Cleanup failed: {cleanup:#}"))),
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::{
        cell::Cell,
        collections::{BTreeMap, BTreeSet},
        fs,
        io::{Read, Write},
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::process::CommandExt,
        },
        process::Child,
        sync::atomic::{AtomicBool, Ordering},
        thread,
        time::Instant,
    };

    const POLL: Duration = Duration::from_millis(10);
    const MAX_TASKS: usize = 128;
    const MAX_CHILDREN: usize = 4096;
    const MAX_PROCESS_ENTRIES: usize = 32 * 1024;
    const MAX_PROCESS_STAT_BYTES: usize = 4096;
    const MAX_CENSUS_BYTES: usize = 8 * 1024 * 1024;
    const MAX_CLEANUP_ERRORS: usize = 16;
    static CANCELLED: AtomicBool = AtomicBool::new(false);
    thread_local! { static WORKER_ACTIVE: Cell<bool> = const { Cell::new(false) }; }

    extern "C" fn request_stop(_: libc::c_int) {
        CANCELLED.store(true, Ordering::Relaxed);
    }

    pub(super) struct Worker {
        old_subreaper: libc::c_int,
        old_signals: Vec<(libc::c_int, libc::sigaction)>,
        finished: bool,
        cleanup_attempted: bool,
    }

    impl Worker {
        pub(super) fn enter(single_threaded: bool) -> Result<Self> {
            ensure!(!WORKER_ACTIVE.get(), "Fixture worker is already active");
            let deadline = Instant::now() + CLEANUP_TIMEOUT;
            let (tasks, children) = child_ids(deadline)?;
            ensure!(children.is_empty(), "Fixture worker already owns children");
            ensure!(
                !single_threaded || tasks == 1,
                "Fixture worker must be single-threaded"
            );
            verify_pidfd_support()?;
            let mut old_subreaper: libc::c_int = 0;
            // SAFETY: PR_GET_CHILD_SUBREAPER writes one int to this valid pointer.
            syscall_result(
                unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut old_subreaper, 0, 0, 0) },
                "Read child-subreaper state",
            )?;
            // This process is a dedicated worker; no IDE or parallel-test flags change.
            syscall_result(
                unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) },
                "Enable fixture child-subreaping",
            )?;
            let mut worker = Self {
                old_subreaper,
                old_signals: Vec::new(),
                finished: false,
                cleanup_attempted: false,
            };
            CANCELLED.store(false, Ordering::Relaxed);
            WORKER_ACTIVE.set(true);
            for signal in [libc::SIGINT, libc::SIGTERM] {
                // SAFETY: zero is the initial libc sigaction representation, and
                // sigemptyset initializes its mask before installation.
                let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
                let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
                action.sa_sigaction = request_stop as *const () as usize;
                unsafe { libc::sigemptyset(&mut action.sa_mask) };
                syscall_result(
                    unsafe { libc::sigaction(signal, &action, &mut old) },
                    "Install fixture cancellation handler",
                )?;
                worker.old_signals.push((signal, old));
            }
            Ok(worker)
        }

        pub(super) fn finish(&mut self) -> Result<()> {
            self.cleanup_attempted = true;
            let deadline = Instant::now() + CLEANUP_TIMEOUT;
            let mut errors = Vec::new();
            let closed = loop {
                match close_children(None, deadline) {
                    Ok(0) => break true,
                    Ok(_) => (),
                    Err(error) => retain_error(&mut errors, "Worker child closure", error),
                }
                if Instant::now() >= deadline {
                    break false;
                }
                thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
            };
            // Restore only after complete descendant closure. An incomplete
            // worker retains subreaping until its imminent failing CLI exit.
            if closed {
                self.finished = true;
                WORKER_ACTIVE.set(false);
                for (signal, old) in &self.old_signals {
                    if let Err(error) = syscall_result(
                        unsafe { libc::sigaction(*signal, old, std::ptr::null_mut()) },
                        "Restore fixture signal handler",
                    ) {
                        retain_error(&mut errors, "Signal restoration", error);
                    }
                }
                if let Err(error) = syscall_result(
                    unsafe {
                        libc::prctl(libc::PR_SET_CHILD_SUBREAPER, self.old_subreaper, 0, 0, 0)
                    },
                    "Restore child-subreaper state",
                ) {
                    retain_error(&mut errors, "Subreaper restoration", error);
                }
            }
            cleanup_result(closed, errors)
        }
    }

    impl Drop for Worker {
        fn drop(&mut self) {
            if !self.finished && !self.cleanup_attempted {
                if let Err(error) = self.finish() {
                    let _ = writeln!(
                        std::io::stderr(),
                        "Fixture worker teardown incomplete: {error:#}"
                    );
                }
            }
        }
    }

    trait Control {
        fn peek_leader(&mut self) -> Result<Option<ExitStatus>>;
        fn signal_group(&mut self) -> Result<()>;
        fn signal_leader(&mut self) -> Result<()>;
        fn close_descendants(&mut self, deadline: Instant) -> Result<usize>;
        fn reap_leader(&mut self) -> Result<Option<ExitStatus>>;
    }

    fn cleanup(control: &mut impl Control, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let mut errors = Vec::new();
        if let Err(error) = control.signal_group() {
            retain_error(&mut errors, "Signal owned process group", error);
        }
        if let Err(error) = control.signal_leader() {
            retain_error(&mut errors, "Signal held leader", error);
        }
        let closed = loop {
            let exited = match control.peek_leader() {
                Ok(status) => status.is_some(),
                Err(error) => {
                    retain_error(&mut errors, "Observe held leader", error);
                    false
                }
            };
            let descendants_closed = match control.close_descendants(deadline) {
                Ok(count) => count == 0,
                Err(error) => {
                    retain_error(&mut errors, "Close owned descendants", error);
                    false
                }
            };
            // The leader is deliberately reaped last. No later group signal is
            // allowed: its numeric PID/PGID authority ends at this transition.
            if exited && descendants_closed {
                match control.reap_leader() {
                    Ok(Some(_)) => break true,
                    Ok(None) => (),
                    Err(error) => retain_error(&mut errors, "Reap held leader", error),
                }
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
        };
        cleanup_result(closed, errors)
    }

    fn cleanup_result(closed: bool, errors: Vec<String>) -> Result<()> {
        ensure!(
            closed && errors.is_empty(),
            "Fixture cleanup {}{}",
            if closed {
                "closed with errors"
            } else {
                "incomplete at its deadline"
            },
            if errors.is_empty() {
                String::new()
            } else {
                format!(": {}", errors.join("; "))
            }
        );
        Ok(())
    }

    fn retain_error(errors: &mut Vec<String>, context: &str, error: anyhow::Error) {
        if errors.len() < MAX_CLEANUP_ERRORS {
            let description = format!("{context}: {error:#}");
            let mut end = description.len().min(2048);
            while !description.is_char_boundary(end) {
                end -= 1;
            }
            errors.push(description[..end].to_owned());
        }
    }

    struct OwnedGroup {
        child: Child,
        pid: libc::pid_t,
        reaped: bool,
        cleanup_attempted: bool,
    }

    impl Control for OwnedGroup {
        fn peek_leader(&mut self) -> Result<Option<ExitStatus>> {
            ensure!(!self.reaped, "Leader authority has already been released");
            let information = wait_information(libc::P_PID, self.pid as libc::id_t, true)?;
            Ok(information.map(|_| {
                // A Child retains the real exit status at the final reap; this
                // status is only the non-destructive 'has exited' observation.
                use std::os::unix::process::ExitStatusExt;
                ExitStatus::from_raw(0)
            }))
        }
        fn signal_group(&mut self) -> Result<()> {
            ensure!(
                !self.reaped,
                "Cannot signal a reaped leader's process group"
            );
            syscall_result(
                unsafe { libc::kill(-self.pid, libc::SIGKILL) },
                "Kill owned Gradle group",
            )
        }
        fn signal_leader(&mut self) -> Result<()> {
            ensure!(!self.reaped, "Cannot signal a reaped leader");
            // Child::kill never waits; the held unreaped PID cannot be recycled.
            self.child.kill().context("Kill held Gradle leader")
        }
        fn close_descendants(&mut self, deadline: Instant) -> Result<usize> {
            close_children(Some(self.pid), deadline)
        }
        fn reap_leader(&mut self) -> Result<Option<ExitStatus>> {
            ensure!(!self.reaped, "Leader was already reaped");
            let status = self
                .child
                .try_wait()
                .context("Reap Gradle leader without blocking")?;
            self.reaped = status.is_some();
            Ok(status)
        }
    }

    impl Drop for OwnedGroup {
        fn drop(&mut self) {
            if !self.reaped && !self.cleanup_attempted {
                self.cleanup_attempted = true;
                if let Err(error) = cleanup(self, CLEANUP_TIMEOUT) {
                    let _ = writeln!(
                        std::io::stderr(),
                        "Fixture process teardown incomplete during unwind: {error:#}"
                    );
                }
            }
        }
    }

    pub(super) fn wait_for_command(
        mut command: Command,
        timeout: Duration,
        log_path: Option<&Path>,
        max_log_bytes: u64,
    ) -> Result<ExitStatus> {
        ensure!(
            WORKER_ACTIVE.get(),
            "Gradle probes require the isolated fixture worker"
        );
        command.process_group(0);
        let child = command.spawn().context("Could not start fixture Gradle")?;
        // Linux PIDs are positive signed pid_t values; obtaining this PID from
        // our just-spawned Child cannot overflow before the guard is installed.
        let pid = child.id() as libc::pid_t;
        let mut group = OwnedGroup {
            child,
            pid,
            reaped: false,
            cleanup_attempted: false,
        };
        let deadline = Instant::now() + timeout;
        let result = loop {
            match group.peek_leader() {
                Ok(Some(_)) => break Ok(()),
                Ok(None) => (),
                Err(error) => break Err(error),
            }
            if CANCELLED.load(Ordering::Relaxed) {
                break Err(anyhow::anyhow!("Fixture Gradle cancelled"));
            }
            if Instant::now() >= deadline {
                break Err(anyhow::anyhow!(
                    "Fixture Gradle timed out after {} seconds",
                    timeout.as_secs()
                ));
            }
            if let Some(path) = log_path {
                match fs::metadata(path) {
                    Ok(metadata) if metadata.len() <= max_log_bytes => (),
                    Ok(_) => {
                        break Err(anyhow::anyhow!(
                            "Fixture Gradle output exceeds {max_log_bytes} bytes"
                        ));
                    }
                    Err(error) => break Err(error.into()),
                }
            }
            thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
        };
        group.cleanup_attempted = true;
        let teardown = cleanup(&mut group, CLEANUP_TIMEOUT);
        merge_cleanup(result, teardown)?;
        // try_wait caches the actual status after the single final nonblocking
        // reap. It cannot wait or send a signal using the released numeric PID.
        group
            .child
            .try_wait()?
            .context("Closed Gradle leader has no exit status")
    }

    fn syscall_result(result: libc::c_int, action: &str) -> Result<()> {
        if result == -1 {
            return Err(std::io::Error::last_os_error()).context(action.to_owned());
        }
        Ok(())
    }

    fn verify_pidfd_support() -> Result<()> {
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, libc::getpid(), 0) };
        ensure!(
            raw != -1,
            "Linux pidfd_open is unavailable: {}",
            std::io::Error::last_os_error()
        );
        let descriptor = unsafe { OwnedFd::from_raw_fd(raw as libc::c_int) };
        let probe = wait_information(libc::P_PIDFD, descriptor.as_raw_fd() as libc::id_t, true);
        ensure!(
            probe
                .as_ref()
                .err()
                .and_then(|error| error.downcast_ref::<std::io::Error>())
                .and_then(std::io::Error::raw_os_error)
                == Some(libc::ECHILD),
            "Linux waitid(P_PIDFD) is unavailable: {}",
            probe
                .as_ref()
                .err()
                .map(ToString::to_string)
                .unwrap_or_else(|| "unexpected child observation".into())
        );
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                descriptor.as_raw_fd(),
                0,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        ensure!(
            result != -1,
            "Linux pidfd_send_signal is unavailable: {}",
            std::io::Error::last_os_error()
        );
        Ok(())
    }

    fn wait_information(
        kind: libc::idtype_t,
        id: libc::id_t,
        peek: bool,
    ) -> Result<Option<libc::siginfo_t>> {
        let mut information: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let options = libc::WEXITED | libc::WNOHANG | if peek { libc::WNOWAIT } else { 0 };
        syscall_result(
            unsafe { libc::waitid(kind, id, &mut information, options) },
            "Nonblocking child observation",
        )?;
        Ok((unsafe { information.si_pid() } != 0).then_some(information))
    }

    /// Examine only this worker's direct children. Subreaping makes orphaned
    /// Gradle grandchildren direct children once their parent exits, including
    /// descendants that have escaped the original process group/session.
    fn child_ids(deadline: Instant) -> Result<(usize, BTreeSet<libc::pid_t>)> {
        let (tasks, children) = child_records(deadline)?;
        Ok((tasks, children.into_keys().collect()))
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct ProcessIdentity {
        pid: libc::pid_t,
        parent: libc::pid_t,
        start_time: u64,
    }

    #[derive(Clone, Copy)]
    struct CensusLimits {
        entries: usize,
        tasks: usize,
        children: usize,
        bytes: usize,
    }

    impl Default for CensusLimits {
        fn default() -> Self {
            Self {
                entries: MAX_PROCESS_ENTRIES,
                tasks: MAX_TASKS,
                children: MAX_CHILDREN,
                bytes: MAX_CENSUS_BYTES,
            }
        }
    }

    fn observation_deadline(deadline: Instant) -> Result<()> {
        ensure!(
            Instant::now() < deadline,
            "Child observation deadline elapsed"
        );
        Ok(())
    }

    fn parse_stat(contents: &[u8], expected_pid: libc::pid_t) -> Result<ProcessIdentity> {
        // comm can contain spaces, closing parentheses, and non-UTF-8 bytes.
        // The final ')' precedes stat's ASCII numeric fields.
        let name_start = contents
            .iter()
            .position(|byte| *byte == b'(')
            .context("Process stat has no command name")?;
        let name_end = contents
            .iter()
            .rposition(|byte| *byte == b')')
            .context("Process stat has no command-name terminator")?;
        ensure!(name_end > name_start, "Malformed process stat command name");
        let pid: libc::pid_t = std::str::from_utf8(&contents[..name_start])?
            .trim()
            .parse()
            .context("Invalid process stat PID")?;
        ensure!(
            pid == expected_pid && pid > 0,
            "Process stat PID does not match its directory"
        );
        let mut fields = std::str::from_utf8(&contents[name_end + 1..])?.split_ascii_whitespace();
        let state = fields.next().context("Process stat has no state")?;
        ensure!(
            state.len() == 1 && state.as_bytes()[0].is_ascii_alphabetic(),
            "Invalid process state"
        );
        let parent: libc::pid_t = fields
            .next()
            .context("Process stat has no parent")?
            .parse()
            .context("Invalid process stat parent")?;
        ensure!(parent >= 0, "Invalid negative process stat parent");
        // After state (field 3) and PPID (field 4), starttime is field 22.
        let start_time: u64 = fields
            .nth(17)
            .context("Process stat has no start time")?
            .parse()
            .context("Invalid process stat start time")?;
        Ok(ProcessIdentity {
            pid,
            parent,
            start_time,
        })
    }

    fn process_stat(
        proc_root: &Path,
        pid: libc::pid_t,
        remaining_bytes: &mut usize,
        deadline: Instant,
    ) -> Result<Option<ProcessIdentity>> {
        observation_deadline(deadline)?;
        let directory = proc_root.join(pid.to_string());
        let file = match fs::File::open(directory.join("stat")) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return match fs::metadata(&directory) {
                    Err(gone) if gone.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Ok(_) => Err(error).context("Live process has no supported stat telemetry"),
                    Err(error) => Err(error).context("Verify vanished process directory"),
                };
            }
            Err(error) => return Err(error).context("Read process ownership stat"),
        };
        let limit = (*remaining_bytes).min(MAX_PROCESS_STAT_BYTES);
        let mut contents = Vec::new();
        if let Err(error) = file.take(limit as u64 + 1).read_to_end(&mut contents) {
            if error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ESRCH)
            {
                match fs::metadata(&directory) {
                    Err(gone) if gone.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Ok(_) => (),
                    Err(error) => {
                        return Err(error).context("Verify process after failed stat read");
                    }
                }
            }
            return Err(error).context("Read bounded process stat");
        }
        ensure!(
            contents.len() <= limit,
            "Process stat exceeds its record or census byte limit"
        );
        *remaining_bytes -= contents.len();
        observation_deadline(deadline)?;
        parse_stat(&contents, pid).map(Some)
    }

    fn worker_tasks(
        proc_root: &Path,
        worker: libc::pid_t,
        limit: usize,
        deadline: Instant,
    ) -> Result<BTreeSet<libc::pid_t>> {
        let mut tasks = BTreeSet::new();
        for task in fs::read_dir(proc_root.join(worker.to_string()).join("task"))
            .context("List fixture worker tasks")?
        {
            observation_deadline(deadline)?;
            ensure!(tasks.len() < limit, "Fixture worker exceeds its task limit");
            let name = task?.file_name();
            let id: libc::pid_t = name
                .to_str()
                .context("Invalid worker task name")?
                .parse()
                .context("Invalid worker task ID")?;
            ensure!(
                id > 0 && tasks.insert(id),
                "Invalid or duplicate worker task ID"
            );
        }
        ensure!(tasks.contains(&worker), "Worker task census is incomplete");
        observation_deadline(deadline)?;
        Ok(tasks)
    }

    fn child_records(deadline: Instant) -> Result<(usize, BTreeMap<libc::pid_t, u64>)> {
        child_records_in(
            Path::new("/proc"),
            std::process::id() as libc::pid_t,
            CensusLimits::default(),
            deadline,
        )
    }

    fn child_records_in(
        proc_root: &Path,
        worker: libc::pid_t,
        limits: CensusLimits,
        deadline: Instant,
    ) -> Result<(usize, BTreeMap<libc::pid_t, u64>)> {
        let mut remaining_bytes = limits.bytes;
        let identity = process_stat(proc_root, worker, &mut remaining_bytes, deadline)?
            .context("Worker process stat is unavailable")?;
        let tasks = worker_tasks(proc_root, worker, limits.tasks, deadline)?;
        let mut children = BTreeMap::new();
        let mut observed_worker = false;
        for (index, entry) in fs::read_dir(proc_root)
            .context("List process ownership census")?
            .enumerate()
        {
            observation_deadline(deadline)?;
            ensure!(
                index < limits.entries,
                "Process census exceeds its entry limit"
            );
            let name = entry?.file_name();
            let Some(name) = name
                .to_str()
                .filter(|name| !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit()))
            else {
                continue;
            };
            let pid: libc::pid_t = name.parse().context("Invalid process census PID")?;
            ensure!(pid > 0, "Invalid nonpositive process census PID");
            let Some(observed) = process_stat(proc_root, pid, &mut remaining_bytes, deadline)?
            else {
                continue;
            };
            if pid == worker {
                ensure!(
                    observed == identity,
                    "Worker process identity changed during census"
                );
                observed_worker = true;
            } else if observed.parent == worker {
                ensure!(
                    children.len() < limits.children,
                    "Fixture worker exceeds its child limit"
                );
                ensure!(
                    children.insert(pid, observed.start_time).is_none(),
                    "Duplicate process census PID"
                );
            }
        }
        ensure!(observed_worker, "Process census omitted the fixture worker");
        ensure!(
            process_stat(proc_root, worker, &mut remaining_bytes, deadline)? == Some(identity),
            "Worker process identity changed after census"
        );
        ensure!(
            worker_tasks(proc_root, worker, limits.tasks, deadline)? == tasks,
            "Worker task census changed during observation"
        );
        observation_deadline(deadline)?;
        Ok((tasks.len(), children))
    }

    fn same_owned_process(pid: libc::pid_t, start_time: u64, deadline: Instant) -> Result<bool> {
        let mut remaining_bytes = MAX_PROCESS_STAT_BYTES;
        Ok(
            process_stat(Path::new("/proc"), pid, &mut remaining_bytes, deadline)?.is_some_and(
                |identity| {
                    identity.parent == std::process::id() as libc::pid_t
                        && identity.start_time == start_time
                },
            ),
        )
    }

    fn close_children(held_leader: Option<libc::pid_t>, deadline: Instant) -> Result<usize> {
        let (_, children) = child_records(deadline)?;
        let witnessed_descendants = children
            .keys()
            .filter(|pid| Some(**pid) != held_leader)
            .count();
        let mut remaining = 0;
        let mut first_error = None;
        for (pid, start_time) in children {
            if Some(pid) == held_leader {
                continue;
            }
            if Instant::now() >= deadline {
                first_error
                    .get_or_insert_with(|| anyhow::anyhow!("Descendant closure deadline elapsed"));
                break;
            }
            match same_owned_process(pid, start_time, deadline) {
                Ok(true) => (),
                Ok(false) => continue,
                Err(error) => {
                    first_error.get_or_insert(error);
                    remaining += 1;
                    continue;
                }
            }
            // The pidfd names the exact task, and waitid additionally verifies
            // that it is our child before any signal. A stale /proc list cannot
            // authorize signalling an unrelated recycled PID.
            let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
            if raw == -1 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    continue;
                }
                first_error.get_or_insert_with(|| {
                    anyhow::Error::from(error).context("Open owned child pidfd")
                });
                remaining += 1;
                continue;
            }
            let descriptor = unsafe { OwnedFd::from_raw_fd(raw as libc::c_int) };
            match same_owned_process(pid, start_time, deadline) {
                Ok(true) => (),
                Ok(false) => continue,
                Err(error) => {
                    first_error.get_or_insert(error);
                    remaining += 1;
                    continue;
                }
            }
            let child = wait_information(libc::P_PIDFD, descriptor.as_raw_fd() as libc::id_t, true);
            match child {
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .and_then(std::io::Error::raw_os_error)
                        == Some(libc::ECHILD) =>
                {
                    continue;
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                    remaining += 1;
                    continue;
                }
                Ok(None) => {
                    let signal = unsafe {
                        libc::syscall(
                            libc::SYS_pidfd_send_signal,
                            descriptor.as_raw_fd(),
                            libc::SIGKILL,
                            std::ptr::null::<libc::siginfo_t>(),
                            0,
                        )
                    };
                    if signal == -1 {
                        let error = std::io::Error::last_os_error();
                        if error.raw_os_error() != Some(libc::ESRCH) {
                            first_error.get_or_insert_with(|| {
                                anyhow::Error::from(error).context("Signal owned child pidfd")
                            });
                        }
                    }
                }
                Ok(Some(_)) => (),
            }
            match wait_information(libc::P_PIDFD, descriptor.as_raw_fd() as libc::id_t, false) {
                Ok(Some(_)) => (),
                Ok(None) => remaining += 1,
                Err(error) => {
                    first_error.get_or_insert(error);
                    remaining += 1;
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            // A just-reaped child may have orphaned another process after its
            // earlier directory position was scanned. Require an empty follow-up
            // census before declaring closure and releasing the held leader.
            None => Ok(remaining.max(witnessed_descendants)),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use anyhow::bail;
        use std::os::unix::process::ExitStatusExt;

        #[derive(Default)]
        struct Fake {
            calls: Vec<&'static str>,
            passes: usize,
            close_after: usize,
            signal_error: bool,
            inspection_error: bool,
            never_close: bool,
            reaped: bool,
        }
        impl Control for Fake {
            fn peek_leader(&mut self) -> Result<Option<ExitStatus>> {
                self.calls.push("peek");
                if self.inspection_error {
                    bail!("injected inspection failure");
                }
                Ok(Some(ExitStatus::from_raw(0)))
            }
            fn signal_group(&mut self) -> Result<()> {
                ensure!(!self.reaped, "group signal after reap");
                self.calls.push("group");
                if self.signal_error {
                    bail!("injected group signal failure");
                }
                Ok(())
            }
            fn signal_leader(&mut self) -> Result<()> {
                self.calls.push("leader");
                Ok(())
            }
            fn close_descendants(&mut self, _: Instant) -> Result<usize> {
                self.calls.push("descendants");
                self.passes += 1;
                Ok(usize::from(
                    self.never_close || self.passes < self.close_after,
                ))
            }
            fn reap_leader(&mut self) -> Result<Option<ExitStatus>> {
                ensure!(
                    !self.never_close && self.passes >= self.close_after,
                    "reaped before descendants closed"
                );
                self.calls.push("reap");
                self.reaped = true;
                Ok(Some(ExitStatus::from_raw(0)))
            }
        }

        #[test]
        fn cleanup_retains_signal_failure_even_when_family_closes() {
            let mut fake = Fake {
                signal_error: true,
                ..Fake::default()
            };
            let error = cleanup(&mut fake, Duration::from_millis(50)).unwrap_err();
            assert!(error.to_string().contains("injected group signal failure"));
            assert!(fake.reaped);
            assert_eq!(fake.calls.last(), Some(&"reap"));
        }

        #[test]
        fn cleanup_inspection_failure_is_bounded_and_never_reaps_without_observation() {
            let mut fake = Fake {
                inspection_error: true,
                ..Fake::default()
            };
            let started = Instant::now();
            let error = cleanup(&mut fake, Duration::from_millis(25)).unwrap_err();
            assert!(error.to_string().contains("injected inspection failure"));
            assert!(error.to_string().contains("incomplete"));
            assert!(!fake.reaped);
            assert!(started.elapsed() < Duration::from_secs(1));
        }

        #[test]
        fn cleanup_waits_for_delayed_descendants_before_releasing_group_authority() {
            let mut fake = Fake {
                close_after: 3,
                ..Fake::default()
            };
            cleanup(&mut fake, Duration::from_millis(100)).unwrap();
            assert_eq!(fake.passes, 3);
            assert!(fake.reaped);
            assert_eq!(
                fake.calls.iter().filter(|call| **call == "group").count(),
                1
            );
            assert_eq!(fake.calls.last(), Some(&"reap"));
        }

        #[test]
        fn incomplete_descendant_closure_reports_failure_without_reaping_leader() {
            let mut fake = Fake {
                never_close: true,
                ..Fake::default()
            };
            assert!(cleanup(&mut fake, Duration::from_millis(25)).is_err());
            assert!(!fake.reaped);
            assert!(!fake.calls.contains(&"reap"));
        }

        #[test]
        fn cleanup_failure_preserves_original_timeout_diagnostic() {
            let error = merge_cleanup::<()>(
                Err(anyhow::anyhow!("fixture timed out")),
                Err(anyhow::anyhow!("descendant closure incomplete")),
            )
            .unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains("fixture timed out"));
            assert!(message.contains("descendant closure incomplete"));
        }

        // A real process-family regression lives in a separate invocation of
        // this exact test. Only that fresh helper may enable process-subreaping;
        // the normal parallel test process never changes its subreaper state.
        #[test]
        fn isolated_helper_process() {
            let Some(mode) = std::env::var_os("KODA_RUNNER_PROCESS_HELPER") else {
                return;
            };
            match mode.to_str().unwrap() {
                "descendant" => {
                    fs::write(std::env::var_os("KODA_RUNNER_READY").unwrap(), b"ready").unwrap();
                    thread::sleep(Duration::from_secs(10));
                }
                "leader" => {
                    // Deliberately orphan the descendant to test subreaper cleanup.
                    #[allow(clippy::zombie_processes)]
                    let child = helper_command("descendant").spawn().unwrap();
                    fs::write(
                        std::env::var_os("KODA_RUNNER_DESCENDANT_PID").unwrap(),
                        child.id().to_string(),
                    )
                    .unwrap();
                    let ready =
                        std::path::PathBuf::from(std::env::var_os("KODA_RUNNER_READY").unwrap());
                    let deadline = Instant::now() + Duration::from_secs(1);
                    while !ready.is_file() {
                        assert!(Instant::now() < deadline, "descendant did not become ready");
                        thread::sleep(POLL);
                    }
                    // Deliberately leave the sleeping descendant behind.
                }
                "supervisor" => {
                    let mut worker = Worker::enter(false).unwrap();
                    let command = helper_command("leader");
                    let status =
                        wait_for_command(command, Duration::from_secs(3), None, 0).unwrap();
                    assert!(status.success());
                    worker.finish().unwrap();
                    let pid: libc::pid_t =
                        fs::read_to_string(std::env::var_os("KODA_RUNNER_DESCENDANT_PID").unwrap())
                            .unwrap()
                            .parse()
                            .unwrap();
                    // ECHILD checks that this exact adopted child was reaped;
                    // a global kill(pid, 0) would race unrelated PID reuse.
                    let mut status = 0;
                    assert_eq!(
                        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
                        -1
                    );
                    assert_eq!(
                        std::io::Error::last_os_error().raw_os_error(),
                        Some(libc::ECHILD)
                    );
                    assert!(
                        child_ids(Instant::now() + Duration::from_secs(1))
                            .unwrap()
                            .1
                            .is_empty()
                    );
                }
                _ => panic!("Unknown isolated helper mode"),
            }
        }

        fn helper_command(mode: &str) -> Command {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "runner_process::linux::tests::isolated_helper_process",
                    "--nocapture",
                ])
                .env("KODA_RUNNER_PROCESS_HELPER", mode);
            command
        }

        #[test]
        fn successful_wrapper_exit_still_closes_and_reaps_lingering_real_descendant() {
            let directory = tempfile::tempdir().unwrap();
            let pid = directory.path().join("descendant.pid");
            let ready = directory.path().join("ready");
            let mut command = helper_command("supervisor");
            command
                .env("KODA_RUNNER_DESCENDANT_PID", &pid)
                .env("KODA_RUNNER_READY", &ready);
            let mut child = command.spawn().unwrap();
            let deadline = Instant::now() + Duration::from_secs(6);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if Instant::now() >= deadline {
                    // The supervisor itself owns the family. SIGTERM requests
                    // its bounded cleanup instead of killing/reaping it first.
                    assert_eq!(
                        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) },
                        0
                    );
                    let grace = Instant::now() + Duration::from_secs(3);
                    loop {
                        if child.try_wait().unwrap().is_some() {
                            break;
                        }
                        assert!(
                            Instant::now() < grace,
                            "isolated supervisor cleanup exceeded deadline"
                        );
                        thread::sleep(POLL);
                    }
                    panic!("isolated process regression exceeded deadline");
                }
                thread::sleep(POLL);
            };
            assert!(status.success());
            assert!(pid.is_file());
            assert!(ready.is_file());
        }

        fn fixture_stat(pid: libc::pid_t, parent: libc::pid_t, start_time: u64) -> String {
            format!(
                "{pid} (fixture) S {parent} 200 200 {} {start_time}\n",
                vec!["0"; 15].join(" ")
            )
        }

        fn census_fixture() -> tempfile::TempDir {
            let directory = tempfile::tempdir().unwrap();
            for task in [7, 8] {
                fs::create_dir_all(directory.path().join(format!("7/task/{task}"))).unwrap();
            }
            for (pid, parent, start_time) in [(7, 1, 1000), (13, 99, 1010), (17, 1, 1020)] {
                fs::create_dir_all(directory.path().join(pid.to_string())).unwrap();
                fs::write(
                    directory.path().join(format!("{pid}/stat")),
                    fixture_stat(pid, parent, start_time),
                )
                .unwrap();
            }
            directory
        }

        fn fixture_census(
            directory: &Path,
            limits: CensusLimits,
        ) -> Result<(usize, BTreeMap<libc::pid_t, u64>)> {
            child_records_in(
                directory,
                7,
                limits,
                Instant::now() + Duration::from_secs(1),
            )
        }

        #[test]
        fn stat_census_finds_adopted_child_without_task_children_interface() {
            let directory = census_fixture();
            assert!(!directory.path().join("7/task/7/children").exists());
            assert!(!directory.path().join("7/task/8/children").exists());
            let (tasks, children) =
                fixture_census(directory.path(), CensusLimits::default()).unwrap();
            assert_eq!(tasks, 2);
            assert!(children.is_empty());
            // The adopted child retains a foreign group and session. Ownership
            // follows its current PPID, independent of the old group/session.
            fs::write(directory.path().join("13/stat"), fixture_stat(13, 7, 1010)).unwrap();
            let (_, children) = fixture_census(directory.path(), CensusLimits::default()).unwrap();
            assert_eq!(children, BTreeMap::from([(13, 1010)]));
        }

        #[test]
        fn stat_census_rejects_missing_live_stat_or_task_telemetry() {
            for missing in ["13/stat", "7/stat", "7/task"] {
                let directory = census_fixture();
                let path = directory.path().join(missing);
                if path.is_dir() {
                    fs::remove_dir_all(path).unwrap();
                } else {
                    fs::remove_file(path).unwrap();
                }
                let error = fixture_census(directory.path(), CensusLimits::default()).unwrap_err();
                let description = format!("{error:#}");
                assert!(
                    description.contains("stat telemetry") || description.contains("worker tasks"),
                    "{missing}: {description}"
                );
            }
        }

        #[test]
        fn stat_census_rejects_malformed_identity_and_truncated_records() {
            for malformed in [
                fixture_stat(14, 7, 1010),
                fixture_stat(13, -1, 1010),
                "13 (fixture) S 7 200 200\n".to_owned(),
                "13 (fixture S 7 200 200\n".to_owned(),
                fixture_stat(13, 7, 1010).replace("1010", "18446744073709551616"),
            ] {
                let directory = census_fixture();
                fs::write(directory.path().join("13/stat"), malformed).unwrap();
                assert!(fixture_census(directory.path(), CensusLimits::default()).is_err());
            }
        }

        #[test]
        fn stat_census_enforces_record_entry_task_child_and_total_byte_limits() {
            let directory = census_fixture();
            fs::write(directory.path().join("13/stat"), fixture_stat(13, 7, 1010)).unwrap();
            for limits in [
                CensusLimits {
                    entries: 2,
                    ..CensusLimits::default()
                },
                CensusLimits {
                    tasks: 1,
                    ..CensusLimits::default()
                },
                CensusLimits {
                    children: 0,
                    ..CensusLimits::default()
                },
                CensusLimits {
                    bytes: 1,
                    ..CensusLimits::default()
                },
            ] {
                assert!(fixture_census(directory.path(), limits).is_err());
            }
            fs::write(
                directory.path().join("13/stat"),
                vec![b'0'; MAX_PROCESS_STAT_BYTES + 1],
            )
            .unwrap();
            assert!(
                fixture_census(directory.path(), CensusLimits::default())
                    .unwrap_err()
                    .to_string()
                    .contains("byte limit")
            );
            assert!(
                child_records_in(directory.path(), 7, CensusLimits::default(), Instant::now())
                    .is_err()
            );
        }

        #[test]
        fn stat_census_ignores_only_a_vanished_process_directory() {
            let directory = tempfile::tempdir().unwrap();
            let mut remaining_bytes = MAX_CENSUS_BYTES;
            let deadline = Instant::now() + Duration::from_secs(1);
            assert_eq!(
                process_stat(directory.path(), 13, &mut remaining_bytes, deadline).unwrap(),
                None
            );
            fs::create_dir(directory.path().join("13")).unwrap();
            assert!(
                process_stat(directory.path(), 13, &mut remaining_bytes, deadline)
                    .unwrap_err()
                    .to_string()
                    .contains("Live process")
            );
        }

        #[test]
        fn stat_parser_accepts_parenthesized_and_non_utf8_commands() {
            let original = fixture_stat(13, 7, 1010);
            let mut bytes = original
                .replace("(fixture)", "(parenthesized (command))")
                .into_bytes();
            bytes[4] = 0xff;
            assert_eq!(
                parse_stat(&bytes, 13).unwrap(),
                ProcessIdentity {
                    pid: 13,
                    parent: 7,
                    start_time: 1010
                }
            );
        }

        #[test]
        fn isolated_escaped_census_helper_process() {
            let Some(mode) = std::env::var_os("KODA_RUNNER_CENSUS_HELPER") else {
                return;
            };
            match mode.to_str().unwrap() {
                "descendant" => {
                    // Only this separate helper process may change its session.
                    let session = unsafe { libc::setsid() };
                    assert!(session > 0, "Could not escape the wrapper session");
                    let group = unsafe { libc::getpgrp() };
                    fs::write(
                        std::env::var_os("KODA_RUNNER_SESSION").unwrap(),
                        format!("{} {session} {group}", std::process::id()),
                    )
                    .unwrap();
                    fs::write(std::env::var_os("KODA_RUNNER_READY").unwrap(), b"ready").unwrap();
                    thread::sleep(Duration::from_secs(10));
                }
                "leader" => {
                    // Deliberately orphan the escaped descendant to test cleanup.
                    #[allow(clippy::zombie_processes)]
                    let child = escaped_helper_command("descendant").spawn().unwrap();
                    fs::write(
                        std::env::var_os("KODA_RUNNER_DESCENDANT_PID").unwrap(),
                        child.id().to_string(),
                    )
                    .unwrap();
                    let ready =
                        std::path::PathBuf::from(std::env::var_os("KODA_RUNNER_READY").unwrap());
                    let deadline = Instant::now() + Duration::from_secs(1);
                    while !ready.is_file() {
                        assert!(
                            Instant::now() < deadline,
                            "escaped descendant did not become ready"
                        );
                        thread::sleep(POLL);
                    }
                    // The wrapper exits while its separate-session child lives.
                }
                "supervisor" => {
                    let mut worker = Worker::enter(false).unwrap();
                    let status = wait_for_command(
                        escaped_helper_command("leader"),
                        Duration::from_secs(3),
                        None,
                        0,
                    )
                    .unwrap();
                    assert!(status.success());
                    worker.finish().unwrap();
                    let pid: libc::pid_t =
                        fs::read_to_string(std::env::var_os("KODA_RUNNER_DESCENDANT_PID").unwrap())
                            .unwrap()
                            .parse()
                            .unwrap();
                    let mut status = 0;
                    assert_eq!(
                        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
                        -1
                    );
                    assert_eq!(
                        std::io::Error::last_os_error().raw_os_error(),
                        Some(libc::ECHILD)
                    );
                    assert!(
                        child_ids(Instant::now() + Duration::from_secs(1))
                            .unwrap()
                            .1
                            .is_empty()
                    );
                }
                _ => panic!("Unknown escaped helper mode"),
            }
        }

        fn escaped_helper_command(mode: &str) -> Command {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "runner_process::linux::tests::isolated_escaped_census_helper_process",
                    "--nocapture",
                ])
                .env("KODA_RUNNER_CENSUS_HELPER", mode);
            command
        }

        #[test]
        fn escaped_session_descendant_is_adopted_and_reaped_by_isolated_worker() {
            let directory = tempfile::tempdir().unwrap();
            let pid = directory.path().join("descendant.pid");
            let ready = directory.path().join("ready");
            let session = directory.path().join("session");
            let mut command = escaped_helper_command("supervisor");
            command
                .env("KODA_RUNNER_DESCENDANT_PID", &pid)
                .env("KODA_RUNNER_READY", &ready)
                .env("KODA_RUNNER_SESSION", &session);
            let mut child = command.spawn().unwrap();
            let deadline = Instant::now() + Duration::from_secs(6);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if Instant::now() >= deadline {
                    assert_eq!(
                        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) },
                        0
                    );
                    let grace = Instant::now() + Duration::from_secs(3);
                    loop {
                        if child.try_wait().unwrap().is_some() {
                            break;
                        }
                        assert!(
                            Instant::now() < grace,
                            "escaped supervisor cleanup exceeded deadline"
                        );
                        thread::sleep(POLL);
                    }
                    panic!("escaped process regression exceeded deadline");
                }
                thread::sleep(POLL);
            };
            assert!(status.success());
            let recorded_pid: libc::pid_t = fs::read_to_string(pid).unwrap().parse().unwrap();
            let session_record: Vec<libc::pid_t> = fs::read_to_string(session)
                .unwrap()
                .split_ascii_whitespace()
                .map(|number| number.parse().unwrap())
                .collect();
            assert_eq!(session_record, vec![recorded_pid; 3]);
            assert!(ready.is_file());
        }
    }
}
