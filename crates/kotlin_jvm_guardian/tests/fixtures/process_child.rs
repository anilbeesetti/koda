#[cfg(target_os = "linux")]
fn run() -> std::io::Result<()> {
    use kotlin_jvm_guardian::{
        CONFIG_ENV, Configuration, Role, connect_registered, watch_connection,
    };
    use std::{
        fs,
        io::{self, BufRead as _, BufReader, Write as _},
        net::TcpStream,
        path::Path,
        process::{Command, Stdio},
        sync::atomic::AtomicBool,
        thread,
        time::{Duration, Instant},
    };

    fn ready(path: &Path) -> io::Result<()> {
        fs::write(path, std::process::id().to_string())
    }

    fn wait_ready(path: &Path) -> io::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.is_file() {
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    fn forever() -> ! {
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    }

    fn lifetime() -> io::Result<std::os::unix::net::UnixStream> {
        let configuration = std::env::var(CONFIG_ENV)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        connect_registered(&Configuration::parse(&configuration)?, Role::Jvm)
    }

    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let mode = std::env::var("KODA_GUARDIAN_FIXTURE_MODE").unwrap_or_else(|_| {
        if arguments
            .iter()
            .any(|argument| argument.starts_with("-Dkoda.kotlin.capture.port="))
        {
            "getter-final-output".to_owned()
        } else {
            arguments.first().cloned().unwrap_or_default()
        }
    });
    let ready_path = std::env::var_os("KODA_GUARDIAN_FIXTURE_READY").map(std::path::PathBuf::from);
    match mode.as_str() {
        "blocking" | "unrelated" => {
            if let Some(path) = std::env::var_os("KODA_GUARDIAN_FIXTURE_CONFIG_FILE") {
                use std::os::unix::fs::OpenOptionsExt as _;
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(path)?;
                let configuration = std::env::var(CONFIG_ENV)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
                file.write_all(configuration.as_bytes())?;
            }
            if let Some(path) = &ready_path {
                ready(path)?;
            }
            forever();
        }
        "detached-unregistered" => {
            // This starter never loads the agent; only ancestry containment can close it.
            if unsafe { libc::setsid() } == -1 {
                return Err(io::Error::last_os_error());
            }
            if let Some(path) = &ready_path {
                ready(path)?;
            }
            forever();
        }
        "detached-guardian" => {
            if unsafe { libc::setsid() } == -1 {
                return Err(io::Error::last_os_error());
            }
            let mut connection = lifetime()?;
            if let Some(path) = &ready_path {
                ready(path)?;
            }
            if watch_connection(&mut connection, &AtomicBool::new(false)).is_err() {
                // Match the agent's cancellation path without C exit handlers.
                unsafe { libc::_exit(70) }
            }
        }
        "rejected-guardian" => {
            if lifetime().is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Unowned peer was accepted",
                ));
            }
            if let Some(path) = &ready_path {
                ready(path)?;
            }
            forever();
        }
        "exit-status" => {
            let code = std::env::var("KODA_GUARDIAN_FIXTURE_EXIT_CODE")
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?
                .parse::<i32>()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            std::process::exit(code);
        }
        "parent-blocking" | "parent-exits" => {
            let ready_path = ready_path.ok_or(io::ErrorKind::InvalidInput)?;
            let child_mode = std::env::var("KODA_GUARDIAN_FIXTURE_CHILD_MODE")
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            let mut child = Command::new(std::env::current_exe()?)
                .env("KODA_GUARDIAN_FIXTURE_MODE", child_mode)
                .env("KODA_GUARDIAN_FIXTURE_READY", &ready_path)
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()?;
            if let Err(error) = wait_ready(&ready_path) {
                child.kill()?;
                child.wait()?;
                return Err(error);
            }
            if mode == "parent-blocking" {
                forever();
            }
        }
        "getter-final-output" => {
            let _lifetime = lifetime()?;
            ready(Path::new("fixture-pid"))?;
            let port = arguments
                .iter()
                .find_map(|argument| argument.strip_prefix("-Dkoda.kotlin.capture.port="))
                .ok_or(io::ErrorKind::InvalidInput)?
                .parse::<u16>()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            let token = arguments
                .iter()
                .find_map(|argument| argument.strip_prefix("-Dkoda.kotlin.capture.token="))
                .ok_or(io::ErrorKind::InvalidInput)?;
            if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            let mut socket = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))?;
            socket.set_read_timeout(Some(Duration::from_secs(5)))?;
            socket.set_write_timeout(Some(Duration::from_secs(5)))?;
            writeln!(socket, "{{\"kind\":\"hello\",\"value\":\"{token}\"}}")?;
            let mut request = String::new();
            BufReader::new(socket.try_clone()?).read_line(&mut request)?;
            if request.trim() != "{\"kind\":\"finish\",\"value\":null}" {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Expected finish request",
                ));
            }
            let stream = fs::read_to_string("final-output-stream")?;
            if stream == "stdout" {
                io::stdout().write_all(&vec![b'O'; 8192])?;
                io::stdout().flush()?;
            } else if stream == "stderr" {
                io::stderr().write_all(&vec![b'E'; 4096])?;
                io::stderr().flush()?;
            } else {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            writeln!(socket, "{{\"kind\":\"finished\",\"value\":null}}")?;
        }
        _ => return Err(io::ErrorKind::InvalidInput.into()),
    }
    Ok(())
}

fn main() {
    #[cfg(target_os = "linux")]
    let outcome = run();
    #[cfg(not(target_os = "linux"))]
    let outcome: std::io::Result<()> = Err(std::io::ErrorKind::Unsupported.into());
    if let Err(error) = outcome {
        eprintln!("Rust guardian process fixture failed: {error}");
        std::process::exit(72);
    }
}
