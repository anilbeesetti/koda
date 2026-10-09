use std::{error::Error, fmt};

#[cfg(not(target_os = "linux"))]
use anyhow::{Result, bail};
#[cfg(not(target_os = "linux"))]
use std::{
    path::Path,
    process::{Command, ExitStatus, Stdio},
    time::Duration,
};

#[derive(Debug)]
pub struct CapabilityUnavailable {
    pub platform: &'static str,
}

impl fmt::Display for CapabilityUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Owned Gradle process containment is unavailable on {}",
            self.platform
        )
    }
}

impl Error for CapabilityUnavailable {}

#[cfg(target_os = "linux")]
pub use linux::OwnedGradleRuntime;

#[cfg(not(target_os = "linux"))]
pub struct OwnedGradleRuntime;

#[cfg(not(target_os = "linux"))]
impl OwnedGradleRuntime {
    pub fn spawn(
        _command: Command,
        _launcher_path: &Path,
        _library_path: &Path,
        _stdout: Stdio,
        _stderr: Stdio,
        _shutdown_timeout: Duration,
    ) -> Result<Self> {
        Err(CapabilityUnavailable {
            platform: std::env::consts::OS,
        }
        .into())
    }

    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        bail!("An unsupported Gradle lifetime cannot be started")
    }

    pub fn close(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use anyhow::{Context as _, Result, ensure};
    use kotlin_jvm_guardian::{
        ACCEPT, CLOSED, CONFIG_ENV, Configuration, LAUNCH_ENV, REGISTER_BYTES, Role, SHUTDOWN_ENV,
        STOP, encode_launch,
    };
    use std::{
        ffi::OsString,
        fs::{self, File, OpenOptions},
        io::{self, Read, Write},
        net::Shutdown,
        os::{
            fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
            unix::{
                fs::{OpenOptionsExt as _, PermissionsExt as _},
                net::{UnixListener, UnixStream},
            },
        },
        path::Path,
        process::{Child, Command, ExitStatus, Stdio},
        sync::{Arc, Mutex, MutexGuard},
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    };
    use tempfile::TempDir;

    const POLL_INTERVAL: Duration = Duration::from_millis(5);
    const AUTHENTICATION_TIMEOUT: Duration = Duration::from_secs(1);
    const MAX_PENDING: usize = 16;
    const MAX_MEMBERS: usize = 256;

    pub struct OwnedGradleRuntime {
        launcher: Child,
        supervisor: Supervisor,
        _directory: TempDir,
        shutdown_timeout: Duration,
        launcher_status: Option<ExitStatus>,
        closure: Option<std::result::Result<(), String>>,
    }

    impl OwnedGradleRuntime {
        /// `command` supplies the wrapper, arguments, directory, and environment;
        /// its standard streams are supplied separately because Command cannot
        /// transfer configured streams when changing the executable.
        pub fn spawn(
            command: Command,
            launcher_path: &Path,
            library_path: &Path,
            stdout: Stdio,
            stderr: Stdio,
            shutdown_timeout: Duration,
        ) -> Result<Self> {
            ensure!(
                shutdown_timeout.as_millis() > 0 && shutdown_timeout <= Duration::from_secs(30),
                "Owned Gradle shutdown deadline must be within 30 seconds"
            );
            let (probe, other) = UnixStream::pair()?;
            match ProcessIdentity::from_peer(&probe) {
                Ok(identity) => drop(identity),
                Err(error)
                    if matches!(error.raw_os_error(), Some(libc::ENOPROTOOPT | libc::EINVAL)) =>
                {
                    return Err(super::CapabilityUnavailable {
                        platform: "Linux kernels without SO_PEERPIDFD (Linux 6.5 or newer required)",
                    }
                    .into());
                }
                Err(error) => return Err(error).context("Preflight Linux process ownership"),
            }
            drop((probe, other));
            let launcher_path = checked_artifact(launcher_path)?;
            let library_path = checked_artifact(library_path)?;
            let library_path = library_path
                .to_str()
                .context("Native guardian library path is not UTF-8")?;
            ensure!(
                !library_path.contains(['\0', '\n', '\r', '"', '=']),
                "Native guardian library path cannot be quoted safely by the JVM"
            );
            let directory = tempfile::Builder::new().prefix("koda-jvm-").tempdir()?;
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
            let socket = directory.path().join("lifetime");
            let endpoint = socket
                .to_str()
                .context("Guardian socket path is not UTF-8")?
                .to_owned();
            ensure!(
                endpoint.len() < 104 && !endpoint.contains([',', '"', '\n', '\r']),
                "Guardian socket path cannot be represented by the native runtime"
            );
            let configuration = Configuration {
                endpoint,
                token: rand::random(),
            };
            let arguments = command.get_args().map(OsString::from).collect::<Vec<_>>();
            let specification = encode_launch(command.get_program(), &arguments)?;
            let specification_path = directory.path().join("launch");
            let mut specification_file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&specification_path)?;
            specification_file.write_all(&specification)?;
            specification_file.sync_all()?;
            drop(specification_file);

            let mut supervised = Command::new(launcher_path);
            if let Some(directory) = command.get_current_dir() {
                supervised.current_dir(directory);
            }
            let mut java_options = std::env::var_os("JAVA_TOOL_OPTIONS");
            for (key, value) in command.get_envs() {
                if key == "JAVA_TOOL_OPTIONS" {
                    java_options = value.map(OsString::from);
                }
                match value {
                    Some(value) => {
                        supervised.env(key, value);
                    }
                    None => {
                        supervised.env_remove(key);
                    }
                }
            }
            let mut java_options = java_options.unwrap_or_default();
            if !java_options.is_empty() {
                java_options.push(" ");
            }
            java_options.push(format!(
                "\"-agentpath:{library_path}={}\"",
                configuration.encode()
            ));
            supervised
                .env("JAVA_TOOL_OPTIONS", java_options)
                .env(CONFIG_ENV, configuration.encode())
                .env(LAUNCH_ENV, specification_path)
                .env(SHUTDOWN_ENV, shutdown_timeout.as_millis().to_string())
                .stdin(Stdio::null())
                .stdout(stdout)
                .stderr(stderr);

            let mut supervisor = Supervisor::start(&configuration)?;
            let launcher = match supervised.spawn() {
                Ok(launcher) => launcher,
                Err(error) => {
                    return match supervisor.stop() {
                        Ok(()) => Err(error).context("Start Rust Gradle containment launcher"),
                        Err(closure) => Err(error).context(format!(
                            "Start Rust Gradle containment launcher; supervisor closure failed: {closure:#}"
                        )),
                    };
                }
            };
            let mut runtime = Self {
                launcher,
                supervisor,
                _directory: directory,
                shutdown_timeout,
                launcher_status: None,
                closure: None,
            };
            if let Err(error) = runtime.supervisor.expect_launcher(runtime.launcher.id()) {
                return match runtime.close() {
                    Ok(()) => Err(error),
                    Err(closure) => Err(error.context(format!(
                        "Owned Gradle startup closure also failed: {closure:#}"
                    ))),
                };
            }
            Ok(runtime)
        }

        pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
            self.supervisor.check_failures()?;
            if self.launcher_status.is_none() {
                self.launcher_status = self.launcher.try_wait()?;
            }
            if self.launcher_status.is_some() && self.supervisor.all_closed()? {
                Ok(self.launcher_status)
            } else {
                Ok(None)
            }
        }

        pub fn launcher_id(&self) -> u32 {
            self.launcher.id()
        }

        pub fn close(&mut self) -> Result<()> {
            if let Some(outcome) = &self.closure {
                return outcome
                    .as_ref()
                    .map(|()| ())
                    .map_err(|error| anyhow::anyhow!(error.clone()));
            }
            let outcome = self.close_inner();
            self.closure = Some(
                outcome
                    .as_ref()
                    .map(|()| ())
                    .map_err(|error| format!("{error:#}")),
            );
            outcome
        }

        fn close_inner(&mut self) -> Result<()> {
            let deadline = Instant::now() + self.shutdown_timeout;
            let outcome = (|| -> Result<()> {
                self.supervisor.begin_closure()?;
                loop {
                    if self.launcher_status.is_none() {
                        self.launcher_status = self.launcher.try_wait()?;
                    }
                    self.supervisor.kill_jvms()?;
                    if self.launcher_status.is_some() && self.supervisor.all_closed()? {
                        self.supervisor.check_failures()?;
                        return Ok(());
                    }
                    ensure!(
                        Instant::now() < deadline,
                        "Owned Gradle closure timed out; descendant absence was not established"
                    );
                    thread::sleep(POLL_INTERVAL);
                }
            })();
            match (outcome, self.supervisor.stop()) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(error), Ok(())) => Err(error),
                (Ok(()), Err(error)) => Err(error),
                (Err(error), Err(secondary)) => Err(error.context(format!(
                    "Guardian supervisor closure also failed: {secondary:#}"
                ))),
            }
        }
    }

    impl Drop for OwnedGradleRuntime {
        fn drop(&mut self) {
            if let Err(error) = self.close() {
                log::error!("Unable to establish owned Gradle JVM closure: {error:#}");
            }
        }
    }

    fn checked_artifact(path: &Path) -> Result<std::path::PathBuf> {
        ensure!(
            path.is_absolute(),
            "Guardian runtime artifact must be absolute"
        );
        let path = path.canonicalize()?;
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            metadata.is_file(),
            "Guardian runtime artifact must be a regular file"
        );
        File::open(&path).context("Open guardian runtime artifact")?;
        Ok(path)
    }

    struct ProcessIdentity {
        descriptor: OwnedFd,
    }

    impl ProcessIdentity {
        fn from_peer(connection: &UnixStream) -> io::Result<Self> {
            // Linux 6.5's SO_PEERPIDFD pins the socket's original peer, even if it
            // exits before registration. Opening a reported PID would leave a
            // reuse race between SO_PEERCRED and pidfd_open.
            const SO_PEERPIDFD: libc::c_int = 77;
            let mut descriptor: libc::c_int = -1;
            let mut size = std::mem::size_of_val(&descriptor) as libc::socklen_t;
            // SAFETY: SO_PEERPIDFD writes one owned descriptor into the sized buffer.
            if unsafe {
                libc::getsockopt(
                    connection.as_raw_fd(),
                    libc::SOL_SOCKET,
                    SO_PEERPIDFD,
                    &mut descriptor as *mut _ as *mut libc::c_void,
                    &mut size,
                )
            } == -1
            {
                return Err(io::Error::last_os_error());
            }
            if descriptor < 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Kernel returned an invalid peer pidfd",
                ));
            }
            // SAFETY: this call received ownership of a new descriptor from the kernel.
            let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
            if size as usize != std::mem::size_of::<libc::c_int>() {
                return Err(io::ErrorKind::InvalidData.into());
            }
            Ok(Self { descriptor })
        }

        fn exited(&self) -> io::Result<bool> {
            let mut poll = libc::pollfd {
                fd: self.descriptor.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: the poll row references this live owned pidfd.
            let result = unsafe { libc::poll(&mut poll, 1, 0) };
            if result == -1 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    return Ok(false);
                }
                return Err(error);
            }
            if poll.revents & libc::POLLNVAL != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Owned pidfd became invalid",
                ));
            }
            Ok(poll.revents & libc::POLLIN != 0)
        }

        fn kill(&self) -> io::Result<()> {
            if self.exited()? {
                return Ok(());
            }
            // SAFETY: this signals a pinned process object, never a reusable numeric PID.
            let result = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.descriptor.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            };
            if result == -1 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
            Ok(())
        }
    }

    struct Member {
        identity: ProcessIdentity,
        role: Role,
        connection: Option<UnixStream>,
        stop_sent: bool,
    }

    struct State {
        expected_launcher: Option<u32>,
        launcher_registered: bool,
        launcher_closed: bool,
        closing: bool,
        stopping: bool,
        members: Vec<Member>,
        failures: Vec<String>,
    }

    fn lock(state: &Mutex<State>) -> io::Result<MutexGuard<'_, State>> {
        state
            .lock()
            .map_err(|_| io::Error::other("Guardian registry lock was poisoned"))
    }

    struct Pending {
        connection: UnixStream,
        bytes: [u8; REGISTER_BYTES],
        count: usize,
        started: Instant,
    }

    struct Supervisor {
        state: Arc<Mutex<State>>,
        worker: Option<JoinHandle<io::Result<()>>>,
    }

    impl Supervisor {
        fn start(configuration: &Configuration) -> Result<Self> {
            let listener = UnixListener::bind(&configuration.endpoint)?;
            fs::set_permissions(&configuration.endpoint, fs::Permissions::from_mode(0o600))?;
            listener.set_nonblocking(true)?;
            let state = Arc::new(Mutex::new(State {
                expected_launcher: None,
                launcher_registered: false,
                launcher_closed: false,
                closing: false,
                stopping: false,
                members: Vec::new(),
                failures: Vec::new(),
            }));
            let worker_state = state.clone();
            let token = configuration.token;
            let worker = thread::Builder::new()
                .name("koda-gradle-lifetime".into())
                .spawn(move || {
                    let outcome = supervise(listener, token, &worker_state);
                    if let Err(error) = &outcome {
                        match lock(&worker_state) {
                            Ok(mut state) => state.failures.push(error.to_string()),
                            Err(secondary) => log::error!(
                                "Guardian registry failed while reporting failure: {secondary}"
                            ),
                        }
                    }
                    outcome
                })?;
            Ok(Self {
                state,
                worker: Some(worker),
            })
        }

        fn expect_launcher(&mut self, pid: u32) -> Result<()> {
            lock(&self.state)?.expected_launcher = Some(pid);
            Ok(())
        }

        fn check_failures(&self) -> Result<()> {
            let state = lock(&self.state)?;
            ensure!(
                state.failures.is_empty(),
                "Guardian failure: {}",
                state.failures.join("; ")
            );
            Ok(())
        }

        fn begin_closure(&self) -> Result<()> {
            lock(&self.state)?.closing = true;
            Ok(())
        }

        fn kill_jvms(&self) -> Result<()> {
            let state = lock(&self.state)?;
            for member in &state.members {
                if member.role == Role::Jvm {
                    member.identity.kill()?;
                }
            }
            Ok(())
        }

        fn all_closed(&self) -> Result<bool> {
            let state = lock(&self.state)?;
            if !state.launcher_registered || !state.launcher_closed {
                return Ok(false);
            }
            for member in &state.members {
                if !member.identity.exited()? {
                    return Ok(false);
                }
            }
            Ok(true)
        }

        fn stop(&mut self) -> Result<()> {
            let shutdown = (|| -> Result<()> {
                let mut state = lock(&self.state)?;
                state.stopping = true;
                let mut failures = Vec::new();
                for member in &mut state.members {
                    if let Some(connection) = member.connection.take() {
                        match connection.shutdown(Shutdown::Both) {
                            Ok(()) => {}
                            Err(error) if error.kind() == io::ErrorKind::NotConnected => {}
                            Err(error) => failures.push(error.to_string()),
                        }
                    }
                }
                if failures.is_empty() {
                    Ok(())
                } else {
                    state.failures.extend(failures.iter().cloned());
                    Err(anyhow::anyhow!(
                        "Guardian socket closure failed: {}",
                        failures.join("; ")
                    ))
                }
            })();
            let joined = if let Some(worker) = self.worker.take() {
                match worker.join() {
                    Ok(outcome) => outcome.context("Stop guardian registry worker"),
                    Err(_) => Err(anyhow::anyhow!("Guardian registry worker panicked")),
                }
            } else {
                Ok(())
            };
            match (shutdown, joined) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
                (Err(error), Err(join_error)) => Err(error.context(format!(
                    "Guardian worker closure also failed: {join_error:#}"
                ))),
            }
        }
    }

    impl Drop for Supervisor {
        fn drop(&mut self) {
            if let Err(error) = self.stop() {
                log::error!("Unable to stop Gradle guardian registry: {error:#}");
            }
        }
    }

    fn supervise(listener: UnixListener, token: [u8; 32], shared: &Mutex<State>) -> io::Result<()> {
        let mut pending = Vec::<Pending>::new();
        loop {
            if lock(shared)?.stopping {
                return Ok(());
            }
            for _ in 0..MAX_PENDING {
                match listener.accept() {
                    Ok((connection, _)) if pending.len() < MAX_PENDING => {
                        connection.set_nonblocking(true)?;
                        pending.push(Pending {
                            connection,
                            bytes: [0; REGISTER_BYTES],
                            count: 0,
                            started: Instant::now(),
                        });
                    }
                    Ok((connection, _)) => connection.shutdown(Shutdown::Both)?,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
            }
            let mut index = 0;
            while index < pending.len() {
                let item = &mut pending[index];
                let mut remove = item.started.elapsed() >= AUTHENTICATION_TIMEOUT;
                if !remove && item.count < REGISTER_BYTES {
                    match item.connection.read(&mut item.bytes[item.count..]) {
                        Ok(0) => remove = true,
                        Ok(count) => item.count += count,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
                            ) =>
                        {
                            remove = true
                        }
                        Err(error) => return Err(error),
                    }
                }
                if !remove && item.count == REGISTER_BYTES {
                    let mut state = lock(shared)?;
                    let role = match item.bytes[0] {
                        b'J' => Some(Role::Jvm),
                        b'L' => Some(Role::Launcher),
                        _ => None,
                    };
                    if item.bytes[1..] != token || role.is_none() {
                        remove = true;
                    } else {
                        let role = role.ok_or(io::ErrorKind::InvalidData)?;
                        let pid = authenticated_peer(&item.connection)?;
                        if role == Role::Launcher && state.expected_launcher.is_none() {
                            index += 1;
                            continue;
                        }
                        if role == Role::Launcher
                            && (state.expected_launcher != Some(pid as u32)
                                || state.launcher_registered)
                        {
                            remove = true;
                        } else {
                            if state.members.len() >= MAX_MEMBERS {
                                return Err(io::Error::other(
                                    "Guardian JVM registry exceeded its member limit",
                                ));
                            }
                            let identity = ProcessIdentity::from_peer(&item.connection)?;
                            if identity.exited()? {
                                drop(state);
                                let item = pending.swap_remove(index);
                                item.connection.shutdown(Shutdown::Both)?;
                                continue;
                            }
                            if role == Role::Jvm && !owned_descendant(pid, &state)? {
                                drop(state);
                                let item = pending.swap_remove(index);
                                item.connection.shutdown(Shutdown::Both)?;
                                continue;
                            }
                            let reply = if state.closing && role == Role::Jvm {
                                STOP
                            } else {
                                ACCEPT
                            };
                            match item.connection.write(&[reply]) {
                                Ok(1) => {
                                    let item = pending.swap_remove(index);
                                    if role == Role::Launcher {
                                        state.launcher_registered = true;
                                    }
                                    let stop_sent = state.closing && role == Role::Jvm;
                                    state.members.push(Member {
                                        identity,
                                        role,
                                        connection: Some(item.connection),
                                        stop_sent,
                                    });
                                    continue;
                                }
                                Ok(_) => remove = true,
                                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                                Err(error) => return Err(error),
                            }
                        }
                    }
                }
                if remove {
                    let item = pending.swap_remove(index);
                    match item.connection.shutdown(Shutdown::Both) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::NotConnected => {}
                        Err(error) => return Err(error),
                    }
                } else {
                    index += 1;
                }
            }
            {
                let mut state = lock(shared)?;
                let closing = state.closing;
                let already_closed = state.launcher_closed;
                let mut closed = false;
                let mut failures = Vec::new();
                for member in &mut state.members {
                    let Some(connection) = member.connection.as_mut() else {
                        continue;
                    };
                    if closing && !member.stop_sent {
                        match connection.write(&[STOP]) {
                            Ok(1) => member.stop_sent = true,
                            Ok(_) => return Err(io::ErrorKind::WriteZero.into()),
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
                                ) => {}
                            Err(error) => return Err(error),
                        }
                    }
                    let mut message = [0; 1];
                    match connection.read(&mut message) {
                        Ok(1) if member.role == Role::Launcher && message[0] == CLOSED => {
                            closed = true;
                        }
                        Ok(0) => {
                            if member.role == Role::Launcher && !already_closed && !closed {
                                failures.push("Rust launcher lifeline ended before verified descendant closure".into());
                            }
                            member.connection = None;
                        }
                        Ok(_) => {
                            failures.push("Guardian peer sent an invalid lifetime message".into())
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
                            ) =>
                        {
                            member.connection = None
                        }
                        Err(error) => return Err(error),
                    }
                }
                state.launcher_closed |= closed;
                state.failures.extend(failures);
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn authenticated_peer(connection: &UnixStream) -> io::Result<libc::pid_t> {
        // SO_PEERCRED binds this connection to a kernel PID; no JVM-supplied PID
        // is ever accepted as permission to signal another process.
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut size = std::mem::size_of_val(&credentials) as libc::socklen_t;
        // SAFETY: SO_PEERCRED writes the credentials structure into its sized buffer.
        if unsafe {
            libc::getsockopt(
                connection.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut credentials as *mut _ as *mut libc::c_void,
                &mut size,
            )
        } == -1
        {
            return Err(io::Error::last_os_error());
        }
        if size as usize != std::mem::size_of::<libc::ucred>()
            || credentials.pid <= 1
            || credentials.pid == std::process::id() as libc::pid_t
            || credentials.uid != unsafe { libc::geteuid() }
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Invalid guardian OS peer identity",
            ));
        }
        Ok(credentials.pid)
    }

    fn owned_descendant(mut pid: libc::pid_t, state: &State) -> io::Result<bool> {
        let Some(launcher) = state.expected_launcher else {
            return Ok(false);
        };
        let Some(anchor) = state
            .members
            .iter()
            .find(|member| member.role == Role::Launcher)
        else {
            return Ok(false);
        };
        if anchor.identity.exited()? {
            return Ok(false);
        }
        for _ in 0..1024 {
            if pid == launcher as libc::pid_t {
                return Ok(true);
            }
            if pid <= 1 || pid == std::process::id() as libc::pid_t {
                return Ok(false);
            }
            let status = match fs::read_to_string(format!("/proc/{pid}/stat")) {
                Ok(status) => status,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error),
            };
            let (_, fields) = status.rsplit_once(") ").ok_or(io::ErrorKind::InvalidData)?;
            let parent = fields
                .split_ascii_whitespace()
                .nth(1)
                .ok_or(io::ErrorKind::InvalidData)?
                .parse::<libc::pid_t>()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            if parent == pid {
                return Err(io::ErrorKind::InvalidData.into());
            }
            pid = parent;
        }
        Err(io::Error::other(
            "Guardian ownership ancestry exceeded its depth limit",
        ))
    }
}
