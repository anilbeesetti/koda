//! Owned sessions have deadline-aware acquisition, linear descendant discovery and
//! explicit error cleanup. Kernel calls are synchronous: the CI/Root supervising
//! owner supplies a hard outer cap; an exhausted cooperative deadline never passes.

use anyhow::{Context as _, Result, bail, ensure};
use serde::Serialize;
#[cfg(target_os = "linux")]
use std::{
    cell::Cell,
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fs,
    process::ExitStatus,
    sync::{Mutex, MutexGuard, TryLockError},
};
use std::{
    io::{self, Read},
    path::PathBuf,
    process::{Child, ChildStdout, Command},
    time::{Duration, Instant},
};
#[cfg(target_os = "linux")]
static CHILD_SESSION: Mutex<()> = Mutex::new(());
#[cfg(target_os = "linux")]
const MAX_CHILDREN: usize = 8192;
#[cfg(target_os = "linux")]
const SHUTDOWN: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Serialize)]
pub(super) struct Identity {
    pid: i32,
    parent: i32,
    group: i32,
    session: i32,
    start: u64,
    state: char,
}

#[derive(Debug, Serialize)]
pub(super) struct CloseReceipt {
    pub(super) state: &'static str,
    pub(super) leader_pid: i32,
    pub(super) process_group: i32,
    pub(super) leader_start_ticks: u64,
    pub(super) leader_exit_code: Option<i32>,
    pub(super) leader_signal: Option<i32>,
    pub(super) deadline_exhausted: bool,
    pub(super) observed_descendants: Vec<Identity>,
    pub(super) actual_reaped_descendants: Vec<(i32, i32)>,
    pub(super) actual_remaining_owned_processes: usize,
    pub(super) cleanup_errors: Vec<String>,
}

impl CloseReceipt {
    pub(super) fn require_closed(&self) -> Result<()> {
        ensure!(
            self.cleanup_errors.is_empty()
                && self.actual_remaining_owned_processes == 0
                && self.state == "ACTUAL_OWNED_PROCESS_SESSION_CLOSED0",
            "owned process cleanup failed: {self:?}"
        );
        Ok(())
    }
    pub(super) fn require_success(&self) -> Result<()> {
        self.require_closed()?;
        ensure!(
            !self.deadline_exhausted && self.leader_exit_code == Some(0),
            "actual owned child exit/deadline failed: {self:?}"
        );
        Ok(())
    }
}

#[cfg(target_os = "linux")]
struct Deadline {
    end: Instant,
    checks: Cell<usize>,
    #[cfg(test)]
    maximum_checks: Option<usize>,
}
#[cfg(target_os = "linux")]
impl Deadline {
    fn at(end: Instant) -> Self {
        Self {
            end,
            checks: Cell::new(0),
            #[cfg(test)]
            maximum_checks: None,
        }
    }
    fn after(duration: Duration) -> Self {
        Self::at(Instant::now() + duration)
    }
    fn check(&self, phase: &str) -> Result<()> {
        ensure!(
            Instant::now() < self.end,
            "owned session deadline exhausted before {phase}"
        );
        #[cfg(test)]
        if let Some(maximum) = self.maximum_checks {
            ensure!(
                self.checks.get() < maximum,
                "instrumented deadline exhausted before {phase}"
            );
        }
        self.checks.set(
            self.checks
                .get()
                .checked_add(1)
                .context("deadline check counter overflow")?,
        );
        Ok(())
    }
}
#[cfg(target_os = "linux")]
fn acquire_session(deadline: &Deadline) -> Result<MutexGuard<'static, ()>> {
    loop {
        deadline.check("child-session mutex acquisition")?;
        match CHILD_SESSION.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(2)),
            Err(TryLockError::Poisoned(error)) => bail!("owned child mutex poisoned: {error}"),
        }
    }
}
#[cfg(target_os = "linux")]
fn identity_until(pid: i32, deadline: &Deadline) -> Result<Option<Identity>> {
    deadline.check("process identity open")?;
    let file = match fs::File::open(format!("/proc/{pid}/stat")) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut stat = String::new();
    file.take(4097).read_to_string(&mut stat)?;
    ensure!(stat.len() <= 4096, "process stat byte budget exhausted");
    deadline.check("process identity decode")?;
    let (_, suffix) = stat.rsplit_once(") ").context("malformed process stat")?;
    let fields: Vec<_> = suffix.split_whitespace().collect();
    ensure!(fields.len() >= 20, "short process stat");
    Ok(Some(Identity {
        pid,
        state: fields[0].chars().next().context("empty process state")?,
        parent: fields[1].parse()?,
        group: fields[2].parse()?,
        session: fields[3].parse()?,
        start: fields[19].parse()?,
    }))
}
#[cfg(all(test, target_os = "linux"))]
fn identity(pid: i32) -> Result<Option<Identity>> {
    identity_until(pid, &Deadline::after(Duration::from_secs(1)))
}
#[cfg(target_os = "linux")]
fn census_until(deadline: &Deadline) -> Result<HashMap<i32, Identity>> {
    deadline.check("census directory open")?;
    let mut rows = HashMap::new();
    for entry in fs::read_dir("/proc")? {
        deadline.check("next census row")?;
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        if let Some(row) = identity_until(pid, deadline)? {
            ensure!(
                rows.len() < 8 * MAX_CHILDREN,
                "total process census budget exhausted"
            );
            rows.insert(pid, row);
        }
    }
    Ok(rows)
}
#[cfg(target_os = "linux")]
#[derive(Default, Debug)]
struct DiscoveryWork {
    indexed_rows: usize,
    seed_rows: usize,
    queue_visits: usize,
    edge_visits: usize,
}
#[cfg(target_os = "linux")]
fn ownership_graph(
    rows: &HashMap<i32, Identity>,
    leader: &Identity,
    authenticated: bool,
    own_pid: i32,
    baseline: &BTreeMap<i32, u64>,
    deadline: &Deadline,
    work: &mut DiscoveryWork,
) -> Result<Vec<Identity>> {
    ensure!(
        rows.len() <= 8 * MAX_CHILDREN,
        "ownership input census budget exhausted"
    );
    deadline.check("parent-index allocation")?;
    let mut children: HashMap<i32, Vec<&Identity>> = HashMap::new();
    for row in rows.values() {
        deadline.check("parent-index row")?;
        children.entry(row.parent).or_default().push(row);
        work.indexed_rows += 1;
    }
    let mut queue = VecDeque::new();
    let mut reached = HashSet::new();
    for row in rows.values() {
        deadline.check("ownership seed row")?;
        work.seed_rows += 1;
        let is_leader = row.pid == leader.pid && (!authenticated || row.start == leader.start);
        // A PID reused since the baseline is a different identity, not an excluded
        // foreign child. The serialized CLI owns newly adopted direct children.
        let adopted = row.parent == own_pid
            && row.pid != leader.pid
            && baseline.get(&row.pid) != Some(&row.start)
            && row.start >= leader.start;
        if (is_leader || adopted) && reached.insert(row.pid) {
            queue.push_back(row);
        }
        ensure!(
            reached.len() <= MAX_CHILDREN,
            "owned seed count budget exhausted"
        );
    }
    let mut result = Vec::new();
    while let Some(row) = queue.pop_front() {
        deadline.check("ownership queue row")?;
        work.queue_visits += 1;
        result.push(row.clone());
        if let Some(descendants) = children.get(&row.pid) {
            for child in descendants {
                deadline.check("ownership edge")?;
                work.edge_visits += 1;
                if reached.insert(child.pid) {
                    ensure!(
                        reached.len() <= MAX_CHILDREN,
                        "owned descendant census budget exceeded"
                    );
                    queue.push_back(*child);
                }
            }
        }
    }
    Ok(result)
}
#[cfg(target_os = "linux")]
struct PidHandle {
    file: std::fs::File,
    identity: Identity,
}
#[cfg(target_os = "linux")]
impl PidHandle {
    fn open_until(expected: Identity, deadline: &Deadline) -> Result<Self> {
        use std::os::fd::FromRawFd as _;
        deadline.check("pidfd pin")?;
        // SAFETY: scalar PID/flags return a newly owned FD for the actual process.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, expected.pid, 0) };
        ensure!(
            fd >= 0,
            "pidfd_open({}) failed: {}",
            expected.pid,
            io::Error::last_os_error()
        );
        // SAFETY: successful pidfd_open returns a unique descriptor.
        let file = unsafe { std::fs::File::from_raw_fd(i32::try_from(fd)?) };
        let actual = identity_until(expected.pid, deadline)?
            .context("owned PID exited before pidfd authentication")?;
        ensure!(
            actual.start == expected.start,
            "PID identity changed before pidfd authentication"
        );
        Ok(Self {
            file,
            identity: actual,
        })
    }
    #[cfg(test)]
    fn open(expected: Identity) -> Result<Self> {
        Self::open_until(expected, &Deadline::after(Duration::from_secs(1)))
    }
    fn signal(&self, signal: i32) -> Result<()> {
        use std::os::fd::AsRawFd as _;
        // SAFETY: pidfd targets the authenticated identity, including PID reuse.
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.file.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                bail!(
                    "signal {signal} to owned PID {}: {error}",
                    self.identity.pid
                );
            }
        }
        Ok(())
    }
    fn signal_until(&self, signal: i32, deadline: &Deadline) -> Result<()> {
        deadline.check("authenticated signal")?;
        self.signal(signal)
    }
}
pub(super) struct TimedReader<R> {
    inner: R,
    deadline: Instant,
}

#[cfg(target_os = "linux")]
impl<R: Read + std::os::fd::AsRawFd> Read for TimedReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            let remaining = self
                .deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "owned child pipe deadline exhausted",
                    )
                })?;
            let mut fd = libc::pollfd {
                fd: self.inner.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let millis =
                i32::try_from(remaining.as_millis().clamp(1, 200)).map_err(io::Error::other)?;
            // SAFETY: one initialized pollfd, live FD and bounded timeout.
            let result = unsafe { libc::poll(&mut fd, 1, millis) };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if result > 0 {
                if fd.revents & libc::POLLNVAL != 0 {
                    return Err(io::Error::other("owned pipe FD became invalid"));
                }
                return self.inner.read(bytes);
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
impl<R: Read> Read for TimedReader<R> {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        let _ = (&self.inner, self.deadline);
        Err(io::Error::other(
            "owned native child sessions require Linux",
        ))
    }
}

#[cfg(target_os = "linux")]
pub(super) struct OwnedChild {
    pub(super) child: Child,
    leader: Identity,
    leader_identity_authenticated: bool,
    baseline_direct_children: BTreeMap<i32, u64>,
    deadline: Instant,
    closed: bool,
    receipt: Option<PathBuf>,
    serial: Option<MutexGuard<'static, ()>>,
    #[cfg(test)]
    injected_census_failure: bool,
}
#[cfg(not(target_os = "linux"))]
pub(super) struct OwnedChild {
    pub(super) child: Child,
    closed: bool,
}
#[cfg(target_os = "linux")]
impl OwnedChild {
    pub(super) fn spawn(command: &mut Command, timeout: Duration) -> Result<Self> {
        use std::os::unix::process::CommandExt as _;
        ensure!(!timeout.is_zero(), "owned child timeout must be positive");
        let deadline = Instant::now()
            .checked_add(timeout)
            .context("owned deadline overflow")?;
        let acquisition = Deadline::at(deadline);
        let serial = acquire_session(&acquisition)?;
        acquisition.check("subreaper setup")?;
        // SAFETY: enables subreaping only for this serialized command-line owner.
        ensure!(
            unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } == 0,
            "cannot enable owned descendant reaping: {}",
            io::Error::last_os_error()
        );
        let own_pid = i32::try_from(std::process::id())?;
        let mut baseline_direct_children = BTreeMap::new();
        for (pid, row) in census_until(&acquisition)? {
            acquisition.check("baseline PID/start identity row")?;
            if row.parent == own_pid {
                baseline_direct_children.insert(pid, row.start);
            }
        }
        // SAFETY: post-fork closure performs only async-signal-safe syscalls.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != own_pid {
                    return Err(io::Error::from_raw_os_error(libc::ECHILD));
                }
                Ok(())
            });
        }
        acquisition.check("owned child spawn")?;
        let child = command.spawn()?;
        let pid = child.id() as i32;
        let mut owned = Self {
            child,
            leader: Identity {
                pid,
                parent: own_pid,
                group: pid,
                session: pid,
                start: 0,
                state: '?',
            },
            leader_identity_authenticated: false,
            baseline_direct_children,
            deadline,
            closed: false,
            receipt: None,
            serial: Some(serial),
            #[cfg(test)]
            injected_census_failure: false,
        };
        let authenticated: Result<()> = (|| {
            let leader = identity_until(pid, &acquisition)?
                .context("owned child absent before identity authentication")?;
            ensure!(
                leader.pid == leader.group
                    && leader.pid == leader.session
                    && leader.parent == own_pid,
                "spawned child does not own the requested new session"
            );
            owned.leader = leader;
            owned.leader_identity_authenticated = true;
            Ok(())
        })();
        if let Err(error) = authenticated {
            let receipt = owned.finish(true)?;
            eprintln!(
                "actual spawn-authentication cleanup receipt: {}",
                serde_json::to_string(&receipt)?
            );
            receipt
                .require_closed()
                .with_context(|| format!("original child identity failure: {error:#}"))?;
            bail!("owned child identity authentication failed; actually closed: {error:#}");
        }
        Ok(owned)
    }
    pub(super) fn stdout(&mut self) -> Result<TimedReader<ChildStdout>> {
        Ok(TimedReader {
            inner: self
                .child
                .stdout
                .take()
                .context("owned child stdout absent")?,
            deadline: self.deadline,
        })
    }
    pub(super) fn set_receipt(&mut self, path: PathBuf) {
        self.receipt = Some(path);
    }
    fn session_census(&mut self, deadline: &Deadline) -> Result<HashMap<i32, Identity>> {
        #[cfg(test)]
        if std::mem::take(&mut self.injected_census_failure) {
            bail!("injected actual-owned-child census failure");
        }
        census_until(deadline)
    }
    fn owned(&self, rows: &HashMap<i32, Identity>, deadline: &Deadline) -> Result<Vec<Identity>> {
        ownership_graph(
            rows,
            &self.leader,
            self.leader_identity_authenticated,
            i32::try_from(std::process::id())?,
            &self.baseline_direct_children,
            deadline,
            &mut DiscoveryWork::default(),
        )
    }
    pub(super) fn finish(&mut self, abort: bool) -> Result<CloseReceipt> {
        ensure!(!self.closed, "owned process session already closed");
        self.child.stdin.take();
        self.child.stdout.take();
        self.child.stderr.take();
        let mut receipt = CloseReceipt {
            state: "ACTUAL_OWNED_PROCESS_SESSION_CLOSURE_FAILED",
            leader_pid: self.leader.pid,
            process_group: self.leader.group,
            leader_start_ticks: self.leader.start,
            leader_exit_code: None,
            leader_signal: None,
            deadline_exhausted: Instant::now() >= self.deadline,
            observed_descendants: Vec::new(),
            actual_reaped_descendants: Vec::new(),
            actual_remaining_owned_processes: 0,
            cleanup_errors: Vec::new(),
        };
        let mut status: Option<ExitStatus> = None;
        if !abort {
            while Instant::now() < self.deadline {
                match self.child.try_wait() {
                    Ok(Some(actual)) => {
                        status = Some(actual);
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(2)),
                    Err(error) => {
                        receipt
                            .cleanup_errors
                            .push(format!("leader try_wait: {error}"));
                        break;
                    }
                }
            }
            if status.is_none() && Instant::now() >= self.deadline {
                receipt.deadline_exhausted = true;
            }
        }
        let shutdown = Deadline::after(SHUTDOWN);
        let mut handles = BTreeMap::new();
        loop {
            let rows = match self.session_census(&shutdown) {
                Ok(rows) => rows,
                Err(error) => {
                    receipt
                        .cleanup_errors
                        .push(format!("closure census: {error:#}"));
                    break;
                }
            };
            let owned = match self.owned(&rows, &shutdown) {
                Ok(rows) => rows,
                Err(error) => {
                    receipt
                        .cleanup_errors
                        .push(format!("ownership census: {error:#}"));
                    break;
                }
            };
            let mut added = false;
            for row in owned {
                if let Err(error) = shutdown.check("next pidfd pin/stop batch") {
                    receipt
                        .cleanup_errors
                        .push(format!("pin deadline: {error:#}"));
                    break;
                }
                if handles.contains_key(&row.pid) {
                    continue;
                }
                if row.pid != self.leader.pid {
                    receipt.observed_descendants.push(row.clone());
                }
                match PidHandle::open_until(row.clone(), &shutdown) {
                    Ok(handle) => {
                        if let Err(error) = handle.signal_until(libc::SIGSTOP, &shutdown) {
                            receipt.cleanup_errors.push(format!("stop: {error:#}"));
                        }
                        handles.insert(row.pid, handle);
                        added = true;
                    }
                    Err(error) => match identity_until(row.pid, &shutdown) {
                        Ok(None) => {}
                        Ok(Some(actual)) if actual.start != row.start => {}
                        _ => receipt
                            .cleanup_errors
                            .push(format!("pin owned process: {error:#}")),
                    },
                }
            }
            if !added || !receipt.cleanup_errors.is_empty() {
                break;
            }
        }
        // Mandatory emergency termination targets ONLY already authenticated
        // pidfds, even if the cooperative deadline expired. The hard outer owner
        // remains responsible for kernel stalls; late cleanup is a FAILED receipt.
        let mut late_kill = false;
        for handle in handles.values() {
            if let Err(error) = shutdown.check("next owned kill batch") {
                if !late_kill {
                    receipt
                        .cleanup_errors
                        .push(format!("kill deadline: {error:#}"));
                    late_kill = true;
                }
            }
            if let Err(error) = handle.signal(libc::SIGKILL) {
                receipt.cleanup_errors.push(format!("kill: {error:#}"));
            }
        }
        // Procfs/ownership errors never suppress our still-unreaped Child handle.
        if status.is_none() {
            if let Err(error) = shutdown.check("safe direct-owned-child fallback") {
                receipt
                    .cleanup_errors
                    .push(format!("direct leader fallback deadline: {error:#}"));
            }
            match self.child.try_wait() {
                Ok(Some(actual)) => status = Some(actual),
                Ok(None) => {
                    if let Err(error) = self.child.kill() {
                        receipt
                            .cleanup_errors
                            .push(format!("direct owned leader kill: {error}"));
                    }
                }
                Err(error) => {
                    receipt
                        .cleanup_errors
                        .push(format!("fallback leader status: {error}"));
                    if let Err(error) = self.child.kill() {
                        receipt
                            .cleanup_errors
                            .push(format!("fallback direct leader kill: {error}"));
                    }
                }
            }
        }
        loop {
            if let Err(error) = shutdown.check("next reaping/census batch") {
                receipt
                    .cleanup_errors
                    .push(format!("reap deadline: {error:#}"));
                receipt.deadline_exhausted = true;
                receipt.actual_remaining_owned_processes = 1;
                break;
            }
            if status.is_none() {
                match self.child.try_wait() {
                    Ok(Some(actual)) => status = Some(actual),
                    Ok(None) => {}
                    Err(error) => receipt.cleanup_errors.push(format!("leader wait: {error}")),
                }
            }
            let mut reap_expired = false;
            for (pid, handle) in &handles {
                if *pid == self.leader.pid {
                    continue;
                }
                if let Err(error) = shutdown.check("targeted descendant reap") {
                    receipt
                        .cleanup_errors
                        .push(format!("descendant reap deadline: {error:#}"));
                    reap_expired = true;
                    break;
                }
                let mut actual = 0;
                // SAFETY: targeted authenticated descendant PID and nonblocking wait.
                let reaped = unsafe { libc::waitpid(*pid, &mut actual, libc::WNOHANG) };
                if reaped == *pid {
                    receipt.actual_reaped_descendants.push((*pid, actual));
                } else if reaped < 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ECHILD) {
                        receipt.cleanup_errors.push(format!(
                            "wait descendant {}@{}: {error}",
                            pid, handle.identity.start
                        ));
                    }
                }
            }
            if reap_expired {
                receipt.actual_remaining_owned_processes = 1;
                break;
            }
            match self
                .session_census(&shutdown)
                .and_then(|rows| self.owned(&rows, &shutdown))
            {
                Ok(remaining) => receipt.actual_remaining_owned_processes = remaining.len(),
                Err(error) => {
                    receipt
                        .cleanup_errors
                        .push(format!("final census: {error:#}"));
                    receipt.actual_remaining_owned_processes = 1;
                    break;
                }
            }
            if status.is_some() && receipt.actual_remaining_owned_processes == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if let Some(status) = status {
            use std::os::unix::process::ExitStatusExt as _;
            receipt.leader_exit_code = status.code();
            receipt.leader_signal = status.signal();
        }
        self.closed =
            receipt.cleanup_errors.is_empty() && receipt.actual_remaining_owned_processes == 0;
        if self.closed {
            receipt.state = "ACTUAL_OWNED_PROCESS_SESSION_CLOSED0";
            // Release before any subsequent Git/Cargo child; owner may stay alive.
            self.serial.take();
        }
        if let Some(path) = self.receipt.take() {
            super::write_json(&path, &receipt, super::MAX_METADATA_BYTES).with_context(|| {
                format!("preserve actual process closure receipt {}", path.display())
            })?;
        }
        Ok(receipt)
    }
}
#[cfg(not(target_os = "linux"))]
impl OwnedChild {
    pub(super) fn spawn(_: &mut Command, _: Duration) -> Result<Self> {
        bail!("native child sessions require Linux");
    }
    pub(super) fn stdout(&mut self) -> Result<TimedReader<ChildStdout>> {
        bail!("native child sessions require Linux");
    }
    pub(super) fn set_receipt(&mut self, _: PathBuf) {}
    pub(super) fn finish(&mut self, _: bool) -> Result<CloseReceipt> {
        bail!("native child sessions require Linux");
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.closed {
            match self.finish(true) {
                Ok(receipt) => {
                    if let Err(error) = receipt.require_closed() {
                        eprintln!("artifact child emergency cleanup failed: {error:#}");
                    }
                }
                Err(error) => eprintln!(
                    "artifact child emergency cleanup returned an actual error: {error:#}"
                ),
            }
        }
    }
}
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::{
        io::{BufReader, Write},
        process::Stdio,
    };
    // Test scheduling is separate from production CHILD_SESSION ownership. Live
    // fixtures wait here BEFORE their short operation/watchdog deadline starts;
    // ordinary cargo-test parallelism cannot consume another fixture's deadline.
    static LIVE_FIXTURE_SESSION: Mutex<()> = Mutex::new(());

    #[test]
    fn helper_child_process() {
        let Ok(mode) = std::env::var("KODA_ARTIFACT_PROCESS_FIXTURE") else {
            return;
        };
        let _fixture_session = LIVE_FIXTURE_SESSION.lock().unwrap();
        if mode == "quick" {
            return;
        }
        if mode == "sequential" {
            let mut first_command = fixture_command("quick");
            let mut first = OwnedChild::spawn(&mut first_command, Duration::from_secs(2)).unwrap();
            first.finish(false).unwrap().require_success().unwrap();
            assert!(first.serial.is_none());
            // The closed first owner remains alive while a second session and the
            // same helper used by post-build observe() acquire ownership again.
            let mut next_command = fixture_command("quick");
            let mut next = OwnedChild::spawn(&mut next_command, Duration::from_secs(2)).unwrap();
            next.finish(false).unwrap().require_success().unwrap();
            assert!(
                !super::super::command(None, "git", &["--version"], 4096)
                    .unwrap()
                    .is_empty()
            );
            return;
        }
        if mode == "descendant" || mode == "foreign" {
            std::thread::sleep(Duration::from_secs(30));
            return;
        }
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tasks::native_artifact::process::tests::helper_child_process",
                "--nocapture",
            ])
            .env("KODA_ARTIFACT_PROCESS_FIXTURE", "descendant")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        fs::write(
            std::env::var("KODA_ARTIFACT_PROCESS_MARKER").unwrap(),
            child.id().to_string(),
        )
        .unwrap();
        if mode == "malformed" {
            println!("ARTIFACT_FIXTURE {{malformed}}");
        }
        if mode == "budget" {
            println!("ARTIFACT_FIXTURE {}", "x".repeat(4096));
        }
        std::io::stdout().flush().unwrap();
        std::thread::sleep(Duration::from_secs(30));
        drop(child);
    }

    #[test]
    fn real_descendants_close_after_malformed_budget_and_timeout_without_signaling_foreign_child() {
        let _fixture_session = LIVE_FIXTURE_SESSION.lock().unwrap();
        let executable = std::env::current_exe().unwrap();
        let fixture = "tasks::native_artifact::process::tests::helper_child_process";
        for mode in ["malformed", "budget", "timeout"] {
            let temporary = tempfile::tempdir().unwrap();
            let marker = temporary.path().join("descendant.pid");
            // Serialize fixture creation with all other owned CLI sessions. The
            // foreign direct child is already present in every subsequent baseline.
            let fixture_creation = CHILD_SESSION.lock().unwrap();
            let foreign = Command::new(&executable)
                .args(["--exact", fixture, "--nocapture"])
                .env("KODA_ARTIFACT_PROCESS_FIXTURE", "foreign")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let mut foreign = ForeignFixtureGuard { child: foreign };
            drop(fixture_creation);
            let mut command = Command::new(&executable);
            command
                .args(["--exact", fixture, "--nocapture"])
                .env("KODA_ARTIFACT_PROCESS_FIXTURE", mode)
                .env("KODA_ARTIFACT_PROCESS_MARKER", &marker)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let mut child = OwnedChild::spawn(&mut command, Duration::from_secs(2)).unwrap();
            child.set_receipt(temporary.path().join("actual-close.json"));
            let marker_deadline = Instant::now() + Duration::from_secs(1);
            while !marker.exists() && Instant::now() < marker_deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(marker.exists());
            let descendant: i32 = fs::read_to_string(&marker).unwrap().parse().unwrap();
            let mut output = BufReader::new(child.stdout().unwrap());
            let failure: Result<()> = (|| {
                while let Some(line) = super::super::bounded_line(&mut output, 1024)? {
                    if let Some(json) = line.strip_prefix(b"ARTIFACT_FIXTURE ") {
                        let _: serde_json::Value = serde_json::from_slice(json)?;
                    }
                }
                bail!("fixture unexpectedly returned valid EOF")
            })();
            assert!(failure.is_err());
            drop(output);
            let receipt = child.finish(true).unwrap();
            receipt.require_closed().unwrap();
            assert!(
                receipt
                    .observed_descendants
                    .iter()
                    .any(|row| row.pid == descendant)
            );
            assert!(
                receipt
                    .actual_reaped_descendants
                    .iter()
                    .any(|(pid, _)| *pid == descendant)
            );
            assert!(identity(descendant).unwrap().is_none());
            assert!(foreign.try_wait().unwrap().is_none());
            foreign.kill().unwrap();
            assert!(!foreign.wait().unwrap().success());
            assert!(temporary.path().join("actual-close.json").exists());
        }
    }

    #[test]
    fn cleanup_errors_and_owned_remaining_processes_cannot_be_called_closed() {
        let mut receipt = CloseReceipt {
            state: "ACTUAL_OWNED_PROCESS_SESSION_CLOSED0",
            leader_pid: 1,
            process_group: 1,
            leader_start_ticks: 2,
            leader_exit_code: Some(0),
            leader_signal: None,
            deadline_exhausted: false,
            observed_descendants: vec![],
            actual_reaped_descendants: vec![],
            actual_remaining_owned_processes: 0,
            cleanup_errors: vec!["actual kill/wait failed".to_owned()],
        };
        assert!(receipt.require_closed().is_err());
        receipt.cleanup_errors.clear();
        receipt.actual_remaining_owned_processes = 1;
        assert!(receipt.require_success().is_err());
        let missing = Identity {
            pid: i32::MAX,
            parent: 1,
            group: 1,
            session: 1,
            start: 1,
            state: 'R',
        };
        assert!(PidHandle::open(missing).is_err());
    }
    struct ForeignFixtureGuard {
        child: Child,
    }
    impl std::ops::Deref for ForeignFixtureGuard {
        type Target = Child;
        fn deref(&self) -> &Child {
            &self.child
        }
    }
    impl std::ops::DerefMut for ForeignFixtureGuard {
        fn deref_mut(&mut self) -> &mut Child {
            &mut self.child
        }
    }
    impl Drop for ForeignFixtureGuard {
        fn drop(&mut self) {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => {
                    if let Err(error) = self.child.kill() {
                        eprintln!("foreign fixture kill failed: {error}");
                    }
                }
                Err(error) => {
                    eprintln!("foreign fixture status failed: {error}");
                    if let Err(error) = self.child.kill() {
                        eprintln!("foreign fixture fallback kill failed: {error}");
                    }
                }
            }
            let end = Instant::now() + Duration::from_secs(3);
            loop {
                match self.child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) if Instant::now() < end => {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    Ok(None) => {
                        eprintln!(
                            "foreign fixture actual reaping deadline failed for {}",
                            self.child.id()
                        );
                        return;
                    }
                    Err(error) => {
                        eprintln!("foreign fixture actual reaping failed: {error}");
                        return;
                    }
                }
            }
        }
    }
    fn fixture_command(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "tasks::native_artifact::process::tests::helper_child_process",
                "--nocapture",
            ])
            .env("KODA_ARTIFACT_PROCESS_FIXTURE", mode)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }
    #[test]
    fn successful_sequential_owned_sessions_release_mutex_under_actual_outer_watchdog() {
        let _fixture_session = LIVE_FIXTURE_SESSION.lock().unwrap();
        let creation = CHILD_SESSION.lock().unwrap();
        let child = fixture_command("sequential").spawn().unwrap();
        let mut watchdog = ForeignFixtureGuard { child };
        drop(creation);
        let end = Instant::now() + Duration::from_secs(8);
        let status = loop {
            if let Some(status) = watchdog.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < end,
                "sequential session regression exceeded its actual watchdog"
            );
            std::thread::sleep(Duration::from_millis(2));
        };
        assert!(status.success());
    }
    #[test]
    fn session_mutex_acquisition_has_a_real_deadline_before_any_spawn() {
        let _fixture_session = LIVE_FIXTURE_SESSION.lock().unwrap();
        let guard = CHILD_SESSION.lock().unwrap();
        let start = Instant::now();
        let outcome = acquire_session(&Deadline::after(Duration::from_millis(20)));
        let failed = outcome.is_err();
        drop(outcome);
        drop(guard);
        assert!(failed);
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn injected_census_failure_terminates_real_owned_leader_and_retains_failed_receipt() {
        let _fixture_session = LIVE_FIXTURE_SESSION.lock().unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("failed-close.json");
        let mut command = fixture_command("foreign");
        let mut child = OwnedChild::spawn(&mut command, Duration::from_secs(2)).unwrap();
        let pid = child.child.id() as i32;
        child.set_receipt(path.clone());
        child.injected_census_failure = true;
        let receipt = child.finish(true).unwrap();
        assert!(receipt.require_closed().is_err());
        assert!(
            receipt
                .cleanup_errors
                .iter()
                .any(|error| error.contains("injected"))
        );
        assert!(receipt.leader_signal.is_some());
        assert!(identity(pid).unwrap().is_none());
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(
            saved["state"],
            "ACTUAL_OWNED_PROCESS_SESSION_CLOSURE_FAILED"
        );
        drop(child);
    }
    #[test]
    fn foreign_fixture_guard_reaps_its_actual_child_during_assertion_unwind() {
        let _fixture_session = LIVE_FIXTURE_SESSION.lock().unwrap();
        let creation = CHILD_SESSION.lock().unwrap();
        let child = fixture_command("foreign").spawn().unwrap();
        let pid = child.id() as i32;
        let guard = ForeignFixtureGuard { child };
        drop(creation);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _owned_foreign_guard = guard;
            panic!("intentional assertion-path fixture");
        }));
        assert!(panic.is_err());
        assert!(identity(pid).unwrap().is_none());
    }
    #[test]
    fn parent_index_discovery_is_linear_deadline_checked_and_pid_start_exclusions_are_exact() {
        let own_pid = 9;
        let leader = Identity {
            pid: 10000,
            parent: own_pid,
            group: 10000,
            session: 10000,
            start: 50,
            state: 'S',
        };
        let mut rows = HashMap::new();
        rows.insert(leader.pid, leader.clone());
        for offset in 1..=4000 {
            // Descending PIDs make the old repeated ordered closure maximally unfavorable.
            let pid = leader.pid - offset;
            rows.insert(
                pid,
                Identity {
                    pid,
                    parent: pid + 1,
                    group: leader.group,
                    session: leader.session,
                    start: 51,
                    state: 'S',
                },
            );
        }
        for pid in 20000..40000 {
            rows.insert(
                pid,
                Identity {
                    pid,
                    parent: 1,
                    group: 1,
                    session: 1,
                    start: 1,
                    state: 'S',
                },
            );
        }
        let foreign = Identity {
            pid: 50000,
            parent: own_pid,
            group: 50000,
            session: 50000,
            start: 60,
            state: 'S',
        };
        rows.insert(foreign.pid, foreign.clone());
        let baseline = BTreeMap::from([(foreign.pid, foreign.start)]);
        let mut work = DiscoveryWork::default();
        let found = ownership_graph(
            &rows,
            &leader,
            true,
            own_pid,
            &baseline,
            &Deadline::after(Duration::from_secs(2)),
            &mut work,
        )
        .unwrap();
        assert_eq!(found.len(), 4001);
        assert!(!found.iter().any(|row| row.pid == foreign.pid));
        assert_eq!(work.indexed_rows, rows.len());
        assert_eq!(work.seed_rows, rows.len());
        assert_eq!(work.queue_visits, 4001);
        assert_eq!(work.edge_visits, 4000);
        assert!(
            work.indexed_rows + work.seed_rows + work.queue_visits + work.edge_visits
                <= 4 * rows.len()
        );
        let mut expired = Deadline::after(Duration::from_secs(2));
        expired.maximum_checks = Some(rows.len() + 3);
        let mut limited_work = DiscoveryWork::default();
        assert!(
            ownership_graph(
                &rows,
                &leader,
                true,
                own_pid,
                &baseline,
                &expired,
                &mut limited_work
            )
            .is_err()
        );
        assert_eq!(limited_work.queue_visits, 0);
        assert!(limited_work.seed_rows <= 2);
        let past = Deadline::at(Instant::now() - Duration::from_millis(1));
        assert!(census_until(&past).is_err());
        assert!(PidHandle::open_until(foreign.clone(), &past).is_err());
        rows.get_mut(&foreign.pid).unwrap().start += 1;
        let mut reused_work = DiscoveryWork::default();
        let reused = ownership_graph(
            &rows,
            &leader,
            true,
            own_pid,
            &baseline,
            &Deadline::after(Duration::from_secs(2)),
            &mut reused_work,
        )
        .unwrap();
        assert!(
            reused
                .iter()
                .any(|row| row.pid == foreign.pid && row.start != foreign.start)
        );
    }
}
