mod kotlin_native_capture;

use std::{
    ffi::{OsStr, OsString, c_char, c_void},
    io,
    path::Path,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

#[cfg(unix)]
use std::{
    io::{Read, Write},
    thread,
    time::{Duration, Instant},
};

pub const CONFIG_ENV: &str = "KODA_KOTLIN_GUARDIAN_CONFIG";
pub const LAUNCH_ENV: &str = "KODA_KOTLIN_GUARDIAN_LAUNCH";
pub const SHUTDOWN_ENV: &str = "KODA_KOTLIN_GUARDIAN_SHUTDOWN_MILLIS";
pub const REGISTER_BYTES: usize = 33;
pub const ACCEPT: u8 = 1;
pub const STOP: u8 = 0;
pub const CLOSED: u8 = 2;
pub const MAX_LAUNCH_BYTES: usize = 1024 * 1024;
#[cfg(unix)]
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(unix)]
const POLL_INTERVAL: Duration = Duration::from_millis(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Role {
    Jvm = b'J',
    Launcher = b'L',
}

#[derive(Clone, Debug)]
pub struct Configuration {
    pub endpoint: String,
    pub token: [u8; 32],
}

impl Configuration {
    pub fn parse(value: &str) -> io::Result<Self> {
        let (token, endpoint) = value.split_once(',').ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "Missing guardian endpoint")
        })?;
        if token.len() != 64
            || endpoint.is_empty()
            || endpoint.len() >= 104
            || endpoint.contains(['\0', '\n', '\r', ','])
            || !Path::new(endpoint).is_absolute()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid guardian configuration",
            ));
        }
        let mut decoded = [0_u8; 32];
        for (destination, pair) in decoded.iter_mut().zip(token.as_bytes().chunks_exact(2)) {
            let high = decode_hex(pair[0])?;
            let low = decode_hex(pair[1])?;
            *destination = high * 16 + low;
        }
        Ok(Self {
            endpoint: endpoint.to_owned(),
            token: decoded,
        })
    }

    pub fn encode(&self) -> String {
        let token = self
            .token
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        format!("{token},{}", self.endpoint)
    }

    pub fn registration(&self, role: Role) -> [u8; REGISTER_BYTES] {
        let mut registration = [0; REGISTER_BYTES];
        registration[0] = role as u8;
        registration[1..].copy_from_slice(&self.token);
        registration
    }
}

fn decode_hex(byte: u8) -> io::Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Guardian token is not lowercase hexadecimal",
        )),
    }
}

#[cfg(unix)]
pub fn connect_registered(
    configuration: &Configuration,
    role: Role,
) -> io::Result<std::os::unix::net::UnixStream> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let mut stream = connect_nonblocking(&configuration.endpoint, deadline)?;
    write_before(&mut stream, &configuration.registration(role), deadline)?;
    let mut response = [0; 1];
    loop {
        match stream.read(&mut response) {
            Ok(1) if response[0] == ACCEPT => return Ok(stream),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Guardian registration was not accepted",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                pause_before(deadline)?;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

#[cfg(unix)]
fn connect_nonblocking(
    endpoint: &str,
    deadline: Instant,
) -> io::Result<std::os::unix::net::UnixStream> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::net::UnixStream;

    // The endpoint is private, but a full accept queue must not block VM startup.
    loop {
        // SAFETY: socket creates an owned descriptor; OwnedFd closes every error path.
        let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        if raw == -1 {
            return Err(io::Error::last_os_error());
        }
        let descriptor = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: both fcntl calls operate on the live, owned socket descriptor.
        if unsafe { libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) } == -1
            || unsafe { libc::fcntl(raw, libc::F_SETFL, libc::O_NONBLOCK) } == -1
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a zeroed sockaddr_un has no invalid Rust values.
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        let bytes = endpoint.as_bytes();
        if bytes.len() >= address.sun_path.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Guardian socket path is too long",
            ));
        }
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (destination, source) in address.sun_path.iter_mut().zip(bytes) {
            *destination = *source as c_char;
        }
        #[cfg(target_os = "macos")]
        {
            address.sun_len = std::mem::size_of::<libc::sockaddr_un>() as u8;
        }
        // SAFETY: address points to a fully initialized sockaddr_un for this call.
        let result = unsafe {
            libc::connect(
                descriptor.as_raw_fd(),
                &address as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
            )
        };
        if result == 0 {
            return Ok(UnixStream::from(descriptor));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EINPROGRESS) {
            loop {
                let mut poll = libc::pollfd {
                    fd: raw,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // SAFETY: poll references one live descriptor and an initialized row.
                let ready = unsafe { libc::poll(&mut poll, 1, 0) };
                if ready < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::Interrupted {
                        return Err(error);
                    }
                } else if ready > 0 {
                    let mut error_code: libc::c_int = 0;
                    let mut size = std::mem::size_of_val(&error_code) as libc::socklen_t;
                    // SAFETY: SO_ERROR writes one c_int into the correctly sized buffer.
                    if unsafe {
                        libc::getsockopt(
                            raw,
                            libc::SOL_SOCKET,
                            libc::SO_ERROR,
                            &mut error_code as *mut _ as *mut c_void,
                            &mut size,
                        )
                    } == -1
                    {
                        return Err(io::Error::last_os_error());
                    }
                    if error_code == 0 {
                        return Ok(UnixStream::from(descriptor));
                    }
                    return Err(io::Error::from_raw_os_error(error_code));
                }
                pause_before(deadline)?;
            }
        }
        if error.kind() != io::ErrorKind::WouldBlock {
            return Err(error);
        }
        pause_before(deadline)?;
    }
}

#[cfg(unix)]
pub fn write_before(
    stream: &mut std::os::unix::net::UnixStream,
    bytes: &[u8],
    deadline: Instant,
) -> io::Result<()> {
    let mut remaining = bytes;
    while !remaining.is_empty() {
        match stream.write(remaining) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => remaining = &remaining[count..],
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => pause_before(deadline)?,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn pause_before(deadline: Instant) -> io::Result<()> {
    let now = Instant::now();
    if now >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Guardian operation exceeded its deadline",
        ));
    }
    thread::sleep(POLL_INTERVAL.min(deadline - now));
    Ok(())
}

#[cfg(unix)]
pub fn watch_connection(
    stream: &mut std::os::unix::net::UnixStream,
    unloading: &AtomicBool,
) -> io::Result<()> {
    let mut message = [0; 1];
    while !unloading.load(Ordering::Acquire) {
        match stream.read(&mut message) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "Owned JVM lifetime ended",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(POLL_INTERVAL);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

static UNLOADING: OnceLock<Arc<AtomicBool>> = OnceLock::new();

/// # Safety
/// The JVM supplies a NUL-terminated agent options string for the duration of
/// this call, following the native-agent ABI. No JVM pointer is dereferenced.
#[unsafe(export_name = "Agent_OnLoad")]
pub unsafe extern "system" fn agent_on_load(
    _virtual_machine: *mut c_void,
    options: *mut c_char,
    _reserved: *mut c_void,
) -> i32 {
    let result = std::panic::catch_unwind(|| {
        if options.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Guardian options are required",
            ));
        }
        // SAFETY: options is the JVM-owned, NUL-terminated native-agent argument.
        let value = unsafe { std::ffi::CStr::from_ptr(options) }
            .to_str()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        install_agent(&Configuration::parse(value)?)
    });
    match result {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => {
            eprintln!("Unable to start Koda JVM lifetime guardian: {error}");
            -1
        }
        Err(_) => {
            eprintln!("Koda JVM lifetime guardian failed during native-agent startup");
            -1
        }
    }
}

fn install_agent(configuration: &Configuration) -> io::Result<()> {
    #[cfg(unix)]
    {
        let mut connection = connect_registered(configuration, Role::Jvm)?;
        let unloading = Arc::new(AtomicBool::new(false));
        UNLOADING.set(unloading.clone()).map_err(|_| {
            io::Error::new(io::ErrorKind::AlreadyExists, "Guardian was loaded twice")
        })?;
        let monitor = thread::Builder::new()
            .name("koda-jvm-lifetime".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    watch_connection(&mut connection, &unloading)
                }));
                if !matches!(result, Ok(Ok(()))) && !unloading.load(Ordering::Acquire) {
                    // C exit handlers and JVM shutdown hooks can block during cancellation.
                    unsafe { libc::_exit(70) }
                }
            })?;
        drop(monitor);
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _configuration = configuration;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "JVM guardian transport is unavailable on this platform",
        ))
    }
}

/// # Safety
/// Called by the JVM during native-agent unloading. The JVM pointer is not used.
#[unsafe(export_name = "Agent_OnUnload")]
pub unsafe extern "system" fn agent_on_unload(_virtual_machine: *mut c_void) {
    if let Some(unloading) = UNLOADING.get() {
        unloading.store(true, Ordering::Release);
    }
}

pub fn encode_launch(program: &OsStr, arguments: &[OsString]) -> io::Result<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let mut bytes = b"KODA-GRADLE-LAUNCH\0\x01".to_vec();
        for (index, item) in std::iter::once(program)
            .chain(arguments.iter().map(OsString::as_os_str))
            .enumerate()
        {
            let item = item.as_bytes();
            if item.is_empty() && index == 0 || item.contains(&0) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Invalid Gradle launcher argument",
                ));
            }
            let size = u32::try_from(item.len())
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            bytes.extend_from_slice(&size.to_be_bytes());
            bytes.extend_from_slice(item);
            if bytes.len() > MAX_LAUNCH_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Gradle launcher arguments exceed their byte limit",
                ));
            }
        }
        Ok(bytes)
    }
    #[cfg(not(unix))]
    {
        let (_program, _arguments) = (program, arguments);
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(target_os = "linux")]
pub fn launcher_entrypoint() -> io::Result<i32> {
    use std::{
        fs::File,
        process::{Command, Stdio},
    };

    let configuration = std::env::var(CONFIG_ENV)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let configuration = Configuration::parse(&configuration)?;
    let mut lifetime = connect_registered(&configuration, Role::Launcher)?;
    let shutdown_millis = std::env::var(SHUTDOWN_ENV)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?
        .parse::<u64>()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    if shutdown_millis == 0 || shutdown_millis > 30_000 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid descendant shutdown deadline",
        ));
    }
    let shutdown = Duration::from_millis(shutdown_millis);
    let path = std::env::var_os(LAUNCH_ENV).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "Missing launch specification")
    })?;
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_LAUNCH_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let (program, arguments) = decode_launch(&bytes)?;

    // The launcher remains an ancestor even when Gradle double-forks and setsid.
    // No JVM can be released by the host before this subreaper has been installed.
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // Ignoring SIGCHLD or SA_NOCLDWAIT would auto-reap children and invalidate
    // the guarantee that child PIDs remain reserved until our cleanup loop.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = libc::SIG_DFL;
    if unsafe { libc::sigemptyset(&mut action.sa_mask) } == -1
        || unsafe { libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()) } == -1
    {
        return Err(io::Error::last_os_error());
    }
    let mut command = Command::new(program);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let outcome = match command.spawn() {
        Ok(mut child) => loop {
            let mut message = [0; 1];
            match lifetime.read(&mut message) {
                Ok(_) => break Ok(70),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => break Err(error),
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    use std::os::unix::process::ExitStatusExt as _;
                    break Ok(status
                        .code()
                        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)));
                }
                Ok(None) => thread::sleep(POLL_INTERVAL),
                Err(error) => break Err(error),
            }
        },
        Err(error) => Err(error),
    };
    finish_launcher_with(
        outcome,
        || retain_descendant_closure(shutdown),
        || write_before(&mut lifetime, &[CLOSED], Instant::now() + shutdown),
    )
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct LauncherFailures {
    wrapper_code: Option<i32>,
    operation: Option<io::Error>,
    closure: Option<io::Error>,
    acknowledgement: Option<io::Error>,
}

#[cfg(target_os = "linux")]
impl std::fmt::Display for LauncherFailures {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut separator = "";
        if let Some(code) = self.wrapper_code {
            write!(formatter, "wrapper exited with code {code}")?;
            separator = "; ";
        }
        for (phase, failure) in [
            ("wrapper operation", &self.operation),
            ("descendant closure", &self.closure),
            ("CLOSED acknowledgement delivery", &self.acknowledgement),
        ] {
            if let Some(failure) = failure {
                write!(formatter, "{separator}{phase} failed: {failure}")?;
                separator = "; ";
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl std::error::Error for LauncherFailures {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.operation
            .as_ref()
            .or(self.closure.as_ref())
            .or(self.acknowledgement.as_ref())
            .map(|error| error as &(dyn std::error::Error + 'static))
    }
}

#[cfg(target_os = "linux")]
fn finish_launcher_with(
    outcome: io::Result<i32>,
    close: impl FnOnce() -> io::Result<()>,
    acknowledge: impl FnOnce() -> io::Result<()>,
) -> io::Result<i32> {
    let closure = close();
    // An acknowledgement cannot authorize successful closure if ECHILD was
    // not established within the issued closure operation.
    let acknowledgement = if closure.is_ok() {
        acknowledge()
    } else {
        Ok(())
    };
    if outcome.is_ok() && closure.is_ok() && acknowledgement.is_ok() {
        return outcome;
    }
    let failures = LauncherFailures {
        wrapper_code: outcome.as_ref().ok().copied(),
        operation: outcome.err(),
        closure: closure.err(),
        acknowledgement: acknowledgement.err(),
    };
    let kind = failures
        .operation
        .as_ref()
        .or(failures.closure.as_ref())
        .or(failures.acknowledgement.as_ref())
        .map_or(io::ErrorKind::Other, io::Error::kind);
    Err(io::Error::new(kind, failures))
}

#[cfg(target_os = "linux")]
fn retain_descendant_closure(shutdown: Duration) -> io::Result<()> {
    let first = close_descendants(Instant::now() + shutdown);
    if first.is_err() {
        // Keep subreaper ownership after the reported deadline. Exiting here
        // would orphan delayed children; the host can report its bounded error
        // while its retained supervisor waits for this launcher to finish.
        loop {
            if close_descendants(Instant::now() + shutdown).is_ok() {
                break;
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
    first
}

#[cfg(target_os = "linux")]
fn decode_launch(bytes: &[u8]) -> io::Result<(OsString, Vec<OsString>)> {
    use std::os::unix::ffi::OsStringExt as _;

    let prefix = b"KODA-GRADLE-LAUNCH\0\x01";
    if bytes.len() > MAX_LAUNCH_BYTES || !bytes.starts_with(prefix) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid Gradle launch specification",
        ));
    }
    let mut remaining = &bytes[prefix.len()..];
    let mut items = Vec::new();
    while !remaining.is_empty() {
        let size_bytes: [u8; 4] = remaining
            .get(..4)
            .ok_or(io::ErrorKind::UnexpectedEof)?
            .try_into()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let size = u32::from_be_bytes(size_bytes) as usize;
        remaining = &remaining[4..];
        let item = remaining.get(..size).ok_or(io::ErrorKind::UnexpectedEof)?;
        if item.contains(&0) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        items.push(OsString::from_vec(item.to_vec()));
        remaining = &remaining[size..];
    }
    let mut items = items.into_iter();
    let program = items
        .next()
        .filter(|program| !program.is_empty())
        .ok_or(io::ErrorKind::InvalidData)?;
    Ok((program, items.collect()))
}

#[cfg(target_os = "linux")]
fn close_descendants(deadline: Instant) -> io::Result<()> {
    loop {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        // Children remain unreaped while signalling: their numeric PIDs cannot
        // be reused for unrelated processes between the census and kill call.
        for task in std::fs::read_dir("/proc/self/task")? {
            let task = task?;
            let children = std::fs::read_to_string(task.path().join("children"))?;
            for child in children.split_ascii_whitespace() {
                if Instant::now() >= deadline {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                let child = child
                    .parse::<libc::pid_t>()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                if child <= 1 || child == std::process::id() as libc::pid_t {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Invalid owned child identity",
                    ));
                }
                // SAFETY: this unreaped PID is an immediate child of this subreaper.
                if unsafe { libc::kill(child, libc::SIGKILL) } == -1 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        return Err(error);
                    }
                }
            }
        }
        loop {
            let mut status = 0;
            // SAFETY: status is a valid output buffer; only this launcher reaps its children.
            let child = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if child > 0 {
                continue;
            }
            if child == -1 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ECHILD) {
                    return Ok(());
                }
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            break;
        }
        pause_before(deadline)?;
    }
}

pub fn main() {
    #[cfg(target_os = "linux")]
    let outcome = launcher_entrypoint();
    #[cfg(not(target_os = "linux"))]
    let outcome: io::Result<i32> = Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Rust Gradle process containment is unavailable on this platform",
    ));
    match outcome {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("Owned Gradle launcher failed: {error}");
            std::process::exit(71);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guardian_configuration_rejects_ambiguous_or_unbounded_identity() {
        for invalid in [
            "",
            ",/tmp/socket",
            "0123,/tmp/socket",
            &format!("{},relative", "0".repeat(64)),
            &format!("{},/tmp/socket,other", "0".repeat(64)),
            &format!("{},/tmp/socket", "F".repeat(64)),
            &format!("{},/tmp/socket\n", "0".repeat(64)),
        ] {
            assert!(Configuration::parse(invalid).is_err());
        }
        let configuration = Configuration {
            endpoint: "/tmp/socket".into(),
            token: [0x5a; 32],
        };
        let decoded = Configuration::parse(&configuration.encode()).expect("Round-trip options");
        assert_eq!(decoded.endpoint, configuration.endpoint);
        assert_eq!(decoded.token, configuration.token);
        assert_eq!(decoded.registration(Role::Jvm)[0], b'J');
        assert_ne!(
            decoded.registration(Role::Jvm),
            decoded.registration(Role::Launcher)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn launch_specification_preserves_non_utf8_and_empty_arguments() {
        use std::os::unix::ffi::OsStringExt as _;
        let program = OsString::from("/tmp/program");
        let arguments = vec![
            OsString::from(""),
            OsString::from("spaces \" $ ` ; "),
            OsString::from_vec(vec![0xff, 0x80]),
        ];
        let bytes = encode_launch(&program, &arguments).expect("Encode launch");
        let (decoded_program, decoded_arguments) = decode_launch(&bytes).expect("Decode launch");
        assert_eq!(decoded_program, program);
        assert_eq!(decoded_arguments, arguments);
        assert!(decode_launch(&bytes[..bytes.len() - 1]).is_err());
        assert!(encode_launch(OsStr::new(""), &[]).is_err());
        assert!(encode_launch(OsStr::new("/tmp/program"), &[OsString::from("\0")]).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn launcher_keeps_wrapper_and_descendant_closure_errors_without_acknowledging() {
        let acknowledged = std::cell::Cell::new(false);
        let error = finish_launcher_with(
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Original wrapper spawn failure",
            )),
            || {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Injected descendant closure failure",
                ))
            },
            || {
                acknowledged.set(true);
                Ok(())
            },
        )
        .expect_err("Both causes must survive");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(error.to_string().contains("Original wrapper spawn failure"));
        assert!(
            error
                .to_string()
                .contains("Injected descendant closure failure")
        );
        assert!(!acknowledged.get());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn launcher_keeps_wrapper_and_acknowledgement_delivery_errors() {
        let error = finish_launcher_with(
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Original wrapper wait failure",
            )),
            || Ok(()),
            || {
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Injected CLOSED delivery failure",
                ))
            },
        )
        .expect_err("Operation and acknowledgement causes must survive");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("Original wrapper wait failure"));
        assert!(
            error
                .to_string()
                .contains("Injected CLOSED delivery failure")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn launcher_keeps_observed_wrapper_codes_alongside_failed_cleanup_or_acknowledgement() {
        for code in [23, 128 + libc::SIGTERM] {
            let acknowledged = std::cell::Cell::new(false);
            let error = finish_launcher_with(
                Ok(code),
                || Err(io::Error::new(io::ErrorKind::TimedOut, "Closure failed")),
                || {
                    acknowledged.set(true);
                    Ok(())
                },
            )
            .expect_err("A known wrapper code cannot authorize failed closure");
            assert!(
                error
                    .to_string()
                    .contains(&format!("wrapper exited with code {code}"))
            );
            assert!(error.to_string().contains("Closure failed"));
            assert!(!acknowledged.get());
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);

            let error = finish_launcher_with(
                Ok(code),
                || Ok(()),
                || Err(io::Error::new(io::ErrorKind::BrokenPipe, "CLOSED failed")),
            )
            .expect_err("The code and acknowledgement failure must survive");
            assert!(
                error
                    .to_string()
                    .contains(&format!("wrapper exited with code {code}"))
            );
            assert!(error.to_string().contains("CLOSED failed"));
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        }
        assert_eq!(
            finish_launcher_with(Ok(23), || Ok(()), || Ok(())).expect("Closed"),
            23
        );
    }
}
