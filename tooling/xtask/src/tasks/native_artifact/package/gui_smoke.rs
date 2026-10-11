//! Run the delivered native package, not another executable with a similar name.
//!
//! UI input and screenshots use Rust X11/XTEST. Source33 does not expose theme
//! selector rows to accessibility, so screenshots require a separate visual
//! review. A saved settings value is evidence of persistence, not a pixel claim.

use super::*;
use std::{io, process::Child};

const MIB: u64 = 1024 * 1024;
const OUTPUT_LIMIT: u64 = 128 * MIB;
const LOG_LIMIT: u64 = 8 * MIB;
const SCREENSHOT_LIMIT: u64 = 8 * MIB;
const MAX_SCREENSHOTS: usize = 8;
const EXPECTED_SCREENSHOTS: [&str; MAX_SCREENSHOTS] = [
    "01-first-run.png",
    "02-python-saved-reopened.png",
    "03-html-saved-reopened.png",
    "04-first-run-theme-selector.png",
    "05-explicit-one-dark-selector.png",
    "06-explicit-one-light-selector.png",
    "07-studio-light-reloaded-selector.png",
    "08-studio-dark-reloaded-selector.png",
];
const WIDTH: u16 = 1600;
const HEIGHT: u16 = 1000;
const PYTHON_RESULT: &str = "print('koda_gui_smoke_python_saved')\n";
const HTML_RESULT: &str = "<!doctype html>\n<title>koda_gui_smoke_html_saved</title>\n";

#[derive(Args)]
pub struct GuiSmokeArgs {
    #[arg(long)]
    request: PathBuf,
    #[arg(long)]
    request_sha256: String,
    #[arg(long)]
    controller_sha: String,
    /// The completed package directory on the same runner; never unpack another ELF.
    #[arg(long)]
    package_dir: PathBuf,
    /// Must be absent; only this new directory receives profiles and captures.
    #[arg(long)]
    output_dir: PathBuf,
}

#[derive(Args)]
pub struct GuiSmokeWorkerArgs {
    #[arg(long)]
    configuration: PathBuf,
    #[arg(long)]
    configuration_sha256: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    schema_version: u32,
    executable: PathBuf,
    working_directory: PathBuf,
    library_directory: PathBuf,
    output_directory: PathBuf,
    source_sha: String,
    source_tree: String,
    executable_sha256: String,
    recorder_sha256: String,
    vulkan_icd: PathBuf,
    vulkan_icd_sha256: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkerReport {
    schema_version: u32,
    screenshots: Vec<String>,
    python_saved: bool,
    html_saved: bool,
    studio_light_persisted: bool,
    studio_dark_persisted: bool,
    explicit_one_settings_preserved: bool,
    active_theme_visual_review: String,
    chooser_visual_review: String,
    syntax_highlighting_and_extensions_review: String,
    android_context_validation: String,
}

#[derive(Serialize)]
struct RuntimeBinding {
    path: String,
    physical: Physical13,
}

fn runtime_bindings(directory: &Path, manifest: &PayloadManifest) -> Result<Vec<RuntimeBinding>> {
    // This is the newly generated runtime, not either source checkout. Directory
    // inventory reads are included in the explicit owned-runtime access-time
    // observations, and cannot hide a newly created undeclared package member.
    let declared: BTreeSet<_> = manifest
        .entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    let mut observed = BTreeSet::new();
    let mut pending = vec![directory.join("runtime")];
    while let Some(path) = pending.pop() {
        let name = path
            .strip_prefix(directory)?
            .to_str()
            .context("non-UTF8 runtime path")?
            .to_owned();
        payload_relative(&name)?;
        ensure!(
            observed.insert(name) && observed.len() <= MAX_PAYLOAD_ENTRIES,
            "runtime path budget or duplicate differs"
        );
        if fs::symlink_metadata(&path)?.is_dir() {
            for entry in fs::read_dir(&path)? {
                pending.push(entry?.path());
                ensure!(
                    pending.len() + observed.len() <= MAX_PAYLOAD_ENTRIES,
                    "runtime inventory queue budget exhausted"
                );
            }
        }
    }
    ensure!(observed == declared, "delivered runtime paths changed");
    let mut rows = Vec::with_capacity(manifest.entries.len());
    for entry in &manifest.entries {
        let path = directory.join(&entry.path);
        let metadata = fs::symlink_metadata(&path)?;
        match entry.kind {
            PayloadKind::File => {
                ensure!(metadata.is_file(), "declared runtime file changed type");
                let (digest, _) = digest_file(&path, MAX_BINARY_BYTES)?;
                ensure!(
                    digest.bytes == entry.bytes && Some(&digest.sha256) == entry.sha256.as_ref(),
                    "delivered runtime content differs: {}",
                    entry.path
                );
            }
            PayloadKind::Directory => ensure!(metadata.is_dir(), "runtime directory changed type"),
            PayloadKind::Symlink => {
                ensure!(
                    metadata.file_type().is_symlink(),
                    "runtime link changed type"
                );
                let target = String::from_utf8(host_link(&path)?)?;
                ensure!(
                    Some(&target) == entry.link_target.as_ref(),
                    "runtime link target differs"
                );
            }
        }
        rows.push(RuntimeBinding {
            path: entry.path.clone(),
            physical: physical(&fs::symlink_metadata(&path)?)?,
        });
    }
    Ok(rows)
}

fn require_read_transitions(before: &[RuntimeBinding], after: &[RuntimeBinding]) -> Result<()> {
    ensure!(before.len() == after.len(), "runtime binding count changed");
    for (before, after) in before.iter().zip(after) {
        ensure!(before.path == after.path, "runtime binding path changed");
        let changed = before.physical.changed_fields(&after.physical);
        ensure!(
            changed.iter().all(|field| *field == "atime_ns"),
            "app changed delivered runtime beyond authorized access-time reads: {} {:?}",
            before.path,
            changed
        );
    }
    Ok(())
}

fn effective_available_memory() -> Result<u64> {
    let info = fs::read_to_string("/proc/meminfo")?;
    let available = info
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))
        .context("MemAvailable missing")?
        .split_whitespace()
        .next()
        .context("MemAvailable value missing")?
        .parse::<u64>()?
        .checked_mul(1024)
        .context("MemAvailable overflow")?;
    let groups = fs::read_to_string("/proc/self/cgroup")?;
    let relative = groups
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .context("cgroup v2 identity missing")?;
    let mut directory = Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/'));
    let mut result = available;
    loop {
        let maximum = fs::read_to_string(directory.join("memory.max"))?;
        let current: u64 = fs::read_to_string(directory.join("memory.current"))?
            .trim()
            .parse()?;
        if maximum.trim() != "max" {
            result = result.min(maximum.trim().parse::<u64>()?.saturating_sub(current));
        }
        if directory == Path::new("/sys/fs/cgroup") {
            break;
        }
        ensure!(
            directory.pop() && directory.starts_with("/sys/fs/cgroup"),
            "cgroup parent escaped mount"
        );
    }
    Ok(result)
}

#[cfg(target_os = "linux")]
fn require_disk_capacity(path: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let name = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let mut statistics = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: initialized C path and writable statvfs allocation.
    ensure!(
        unsafe { libc::statvfs(name.as_ptr(), statistics.as_mut_ptr()) } == 0,
        "statvfs failed: {}",
        io::Error::last_os_error()
    );
    // SAFETY: successful statvfs initialized every field.
    let statistics = unsafe { statistics.assume_init() };
    let available = statistics
        .f_bavail
        .checked_mul(statistics.f_frsize)
        .context("disk capacity overflow")?;
    ensure!(
        available >= OUTPUT_LIMIT + 384 * MIB && statistics.f_favail >= 96,
        "GUI smoke requires 512MiB effective disk capacity and 96 available inodes"
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn require_disk_capacity(_: &Path) -> Result<()> {
    bail!("GUI smoke requires Linux")
}

pub(crate) fn run(args: GuiSmokeArgs) -> Result<()> {
    let request = request_input(&args.request, &args.request_sha256, None)?;
    let metadata_path = args.package_dir.join("artifact-metadata.json");
    let metadata_bytes = read_bounded(&metadata_path, MAX_METADATA_BYTES)?.0;
    let metadata: ArtifactMetadata = serde_json::from_slice(&metadata_bytes)?;
    metadata.validate(&request)?;
    require_hex(&args.controller_sha, 40)?;
    ensure!(
        metadata.controller_sha == args.controller_sha,
        "GUI controller/package revision differs"
    );
    let payload_path = args.package_dir.join("payload-manifest.json");
    validate_metadata_members(
        &metadata,
        &request,
        &payload_path,
        &args
            .package_dir
            .join("runtime/evidence/source-manifest.json"),
        &args.package_dir.join("piece-manifest.json"),
        &args.package_dir.join("runtime/evidence/native-build.json"),
    )?;
    let payload: PayloadManifest = authenticated_json(
        &payload_path,
        &metadata.payload_manifest.sha256,
        MAX_MANIFEST_BYTES,
    )?;
    let binary = payload
        .entries
        .iter()
        .find(|entry| entry.path == "runtime/source/target/debug/koda")
        .context("delivered full application absent")?;
    ensure!(
        binary.kind == PayloadKind::File && binary.mode == 0o755 && binary.bytes > 0,
        "delivered application is not a regular executable"
    );
    ensure!(
        effective_available_memory()?
            >= binary
                .bytes
                .checked_add(2592 * MIB)
                .context("memory envelope overflow")?,
        "GUI smoke cannot admit the delivered ELF plus unchanged 2592MiB envelope"
    );
    let parent = args
        .output_dir
        .parent()
        .context("GUI output parent absent")?;
    require_disk_capacity(parent)?;
    ensure!(
        !args.output_dir.try_exists()?,
        "GUI output directory must be absent"
    );
    fs::create_dir(&args.output_dir)?;
    let result: Result<()> = (|| {
        let before = runtime_bindings(&args.package_dir, &payload)?;
        write_json(
            &args.output_dir.join("runtime-before.json"),
            &before,
            LOG_LIMIT,
        )?;
        let icds = [
            "/usr/share/vulkan/icd.d/lvp_icd.x86_64.json",
            "/usr/share/vulkan/icd.d/lvp_icd.json",
        ];
        let vulkan_icd = icds
            .iter()
            .find(|path| Path::new(path).is_file())
            .context("installed Lavapipe ICD absent")?;
        let recorder_path = std::env::current_exe()?;
        let recorder_digest = digest_file(&recorder_path, MAX_BINARY_BYTES)?.0;
        let configuration = Configuration {
            schema_version: 1,
            executable: args.package_dir.join(&binary.path),
            working_directory: args.package_dir.join("runtime/source"),
            library_directory: args.package_dir.join("runtime/lib"),
            output_directory: args.output_dir.clone(),
            source_sha: request.source_sha.clone(),
            source_tree: request.source_tree.clone(),
            executable_sha256: binary.sha256.clone().context("executable digest absent")?,
            recorder_sha256: recorder_digest.sha256.clone(),
            vulkan_icd: PathBuf::from(vulkan_icd),
            vulkan_icd_sha256: digest_file(Path::new(vulkan_icd), MIB)?.0.sha256,
        };
        let configuration_path = args.output_dir.join("configuration.json");
        write_json(&configuration_path, &configuration, MIB)?;
        // Digest reads can change actual resident/cache pressure. Admit again at
        // the launch boundary; clean cache is never added back to this estimate.
        ensure!(
            effective_available_memory()?
                >= binary
                    .bytes
                    .checked_add(2592 * MIB)
                    .context("memory envelope overflow")?,
            "GUI launch headroom changed after package authentication"
        );
        require_disk_capacity(parent)?;
        let mut command = Command::new(&recorder_path);
        command
            .args(["native-artifact", "gui-smoke-worker", "--configuration"])
            .arg(&configuration_path)
            .arg("--configuration-sha256")
            .arg(digest_file(&configuration_path, MIB)?.0.sha256)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // The worker sends only its bounded result to stdout; diagnostics are written
        // into its capped, owned log. A blocked X11 call is stopped by this owner.
        command.stderr(Stdio::from(
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(args.output_dir.join("worker-stderr.log"))?,
        ));
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt as _;
            // The worker and its descendants inherit a per-file ceiling in
            // addition to the checked aggregate directory budget.
            unsafe {
                command.pre_exec(|| {
                    let limit = libc::rlimit {
                        rlim_cur: LOG_LIMIT,
                        rlim_max: LOG_LIMIT,
                    };
                    if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let actual_command: Vec<_> = std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|value| {
                value
                    .to_str()
                    .context("non-UTF8 GUI worker argument")
                    .map(str::to_owned)
            })
            .collect::<Result<_>>()?;
        let mut worker = OwnedChild::spawn(&mut command, Duration::from_secs(120))?;
        worker.set_receipt(args.output_dir.join("process-closure.json"));
        let capture = (|| {
            let mut bytes = Vec::new();
            worker.stdout()?.take(MIB + 1).read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() as u64 <= MIB,
                "worker result byte cap exhausted"
            );
            Ok::<_, anyhow::Error>(bytes)
        })();
        let closure = worker.finish(capture.is_err())?;
        write_json(
            &args.output_dir.join("actual-worker-closure.json"),
            &closure,
            MIB,
        )?;
        let capture = match capture {
            Ok(bytes) => bytes,
            Err(error) => {
                closure
                    .require_closed()
                    .with_context(|| format!("original GUI worker capture failure: {error:#}"))?;
                return Err(error
                    .context("GUI worker result capture failed; actual cleanup receipt retained"));
            }
        };
        closure.require_success()?;
        let report: WorkerReport = serde_json::from_slice(&capture)?;
        ensure!(
            report.schema_version == 1
                && report.python_saved
                && report.html_saved
                && report.studio_light_persisted
                && report.studio_dark_persisted
                && report.explicit_one_settings_preserved,
            "worker functional checks incomplete"
        );
        require_file_bytes(
            &args.output_dir.join("fixture/qa_main.py"),
            PYTHON_RESULT.as_bytes(),
        )?;
        require_file_bytes(
            &args.output_dir.join("fixture/qa_index.html"),
            HTML_RESULT.as_bytes(),
        )?;
        ensure!(
            report
                .screenshots
                .iter()
                .map(String::as_str)
                .eq(EXPECTED_SCREENSHOTS),
            "required eight scenario captures are missing or out of order"
        );
        for (name, expected) in [("one-dark", "One Dark"), ("one-light", "One Light")] {
            let before: serde_json::Value = serde_json::from_slice(
                &read_bounded(
                    &args
                        .output_dir
                        .join(format!("{name}-settings-before-mode.json")),
                    MIB,
                )?
                .0,
            )?;
            let after: serde_json::Value = serde_json::from_slice(
                &read_bounded(
                    &args.output_dir.join(name).join("config/settings.json"),
                    MIB,
                )?
                .0,
            )?;
            ensure!(
                before["theme"] == expected
                    && after["theme"]["light"] == "One Light"
                    && after["theme"]["dark"] == "One Dark",
                "One-family UI persistence records differ"
            );
        }
        for (name, expected) in [
            ("studio-light-selected-settings.json", "Studio Light"),
            ("studio-dark-selected-settings.json", "Studio Dark"),
        ] {
            let saved: serde_json::Value =
                serde_json::from_slice(&read_bounded(&args.output_dir.join(name), MIB)?.0)?;
            ensure!(
                saved["theme"] == expected,
                "Studio selection persistence record differs"
            );
        }
        for capture in &report.screenshots {
            ensure!(
                !capture.contains('/') && capture.ends_with(".png"),
                "unconfined capture name"
            );
            let digest = digest_file(&args.output_dir.join(capture), SCREENSHOT_LIMIT)?.0;
            ensure!(digest.bytes > 0, "empty screenshot");
        }
        let after = runtime_bindings(&args.package_dir, &payload)?;
        write_json(
            &args.output_dir.join("runtime-after.json"),
            &after,
            LOG_LIMIT,
        )?;
        require_read_transitions(&before, &after)?;
        write_json(
            &args.output_dir.join("functional-smoke.json"),
            &serde_json::json!({
                "schema_version": 1,
                "state": "ACTUAL_UI_SAVE_AND_THEME_PERSISTENCE_PASS_VISUAL_REVIEW_PENDING",
                "source_sha": request.source_sha, "source_tree": request.source_tree,
                "controller_sha": metadata.controller_sha,
                "gui_recorder_executable": recorder_digest,
                "actual_worker_command": actual_command,
                "package_metadata_sha256": sha256(&metadata_bytes),
                "delivered_executable_sha256": configuration.executable_sha256,
                "vulkan_icd_sha256": configuration.vulkan_icd_sha256,
                "worker": report, "reference_parity_credit": 0,
                "app_visual_acceptance": "PENDING_INDEPENDENT_SCREENSHOT_REVIEW",
                "android_gating": "NOT_TESTED_THIS_THEME_AND_EDITING_SCENARIO_SET"
            }),
            MIB,
        )?;
        require_output_budget(&args.output_dir)?;
        Ok(())
    })();
    preserve_failure(&args.output_dir, true, "gui-smoke-failure.json", &result)?;
    result
}

fn require_file_bytes(path: &Path, expected: &[u8]) -> Result<()> {
    ensure!(
        read_bounded(path, MIB)?.0 == expected,
        "actual UI save bytes differ: {}",
        path.display()
    );
    Ok(())
}

fn require_output_budget(root: &Path) -> Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::FileTypeExt as _;
    let mut pending = vec![root.to_owned()];
    let mut count = 0_usize;
    let mut bytes = 0_u64;
    while let Some(path) = pending.pop() {
        count += 1;
        ensure!(count <= 8192, "GUI output inode budget exhausted");
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "GUI output symlink is unsupported"
        );
        if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                pending.push(entry?.path());
                ensure!(
                    pending.len() + count <= 8192,
                    "GUI output traversal budget exhausted"
                );
            }
        } else if metadata.is_file() {
            bytes = bytes
                .checked_add(metadata.len())
                .context("GUI output size overflow")?;
            ensure!(bytes <= OUTPUT_LIMIT, "GUI output byte budget exhausted");
        } else {
            #[cfg(unix)]
            ensure!(
                metadata.file_type().is_socket(),
                "GUI output contains unsupported inode"
            );
            #[cfg(not(unix))]
            bail!("GUI output contains unsupported inode");
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::{
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::{fs::PermissionsExt as _, process::CommandExt as _},
        },
        thread,
    };
    use x11rb::{
        connection::Connection as _,
        protocol::{
            xproto::{self, ConnectionExt as _, ImageFormat, InputFocus, Window},
            xtest::ConnectionExt as _,
        },
        rust_connection::RustConnection,
    };

    struct ChildGuard {
        child: Child,
        pidfd: OwnedFd,
        closed: bool,
    }

    impl ChildGuard {
        fn spawn(command: &mut Command, pty: bool) -> Result<Self> {
            Self::spawn_with_pidfd(command, pty, |child| {
                // This direct child has not been reaped; its PID cannot be recycled.
                // SAFETY: pidfd_open takes a numeric PID and flags, no pointers.
                let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id() as i32, 0) };
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: successful pidfd_open returns a new owned descriptor.
                Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
            })
        }

        fn spawn_with_pidfd(
            command: &mut Command,
            pty: bool,
            acquire: impl FnOnce(&Child) -> io::Result<OwnedFd>,
        ) -> Result<Self> {
            // SAFETY: only async-signal-safe syscalls are performed after fork.
            unsafe {
                command.pre_exec(move || {
                    if libc::setsid() < 0
                        || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) < 0
                    {
                        return Err(io::Error::last_os_error());
                    }
                    if pty && libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    let limit = libc::rlimit {
                        rlim_cur: LOG_LIMIT,
                        rlim_max: LOG_LIMIT,
                    };
                    if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = command.spawn().context("spawn owned GUI child")?;
            let pidfd = match acquire(&child) {
                Ok(pidfd) => pidfd,
                Err(acquisition_error) => match Self::stop_without_pidfd(&mut child) {
                    Ok(status) => bail!(
                        "pin GUI child pidfd failed: {acquisition_error}; acquisition cleanup: reaped direct child with {status}"
                    ),
                    Err(cleanup_error) => bail!(
                        "pin GUI child pidfd failed: {acquisition_error}; acquisition cleanup error: {cleanup_error:#}; outer owned-session cleanup required"
                    ),
                },
            };
            Ok(Self {
                child,
                pidfd,
                closed: false,
            })
        }

        fn stop_without_pidfd(child: &mut Child) -> Result<std::process::ExitStatus> {
            if let Some(status) = child.try_wait().context("poll unpinned direct child")? {
                return Ok(status);
            }
            // A held, unreaped direct child pins its numeric PID even when pidfd
            // acquisition fails. Signal only that child; never a reaped PID/group.
            let signal_error = child.kill().err();
            let signal_outcome = signal_error
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "SIGKILL delivered".to_owned());
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Some(status) = child.try_wait().with_context(|| {
                    format!("poll direct child after acquisition failure; signal outcome: {signal_outcome}")
                })? {
                    if let Some(error) = signal_error.as_ref()
                        && error.raw_os_error() != Some(libc::ESRCH)
                    {
                        bail!("direct child reaped with {status}, but signaling failed: {error}");
                    }
                    return Ok(status);
                }
                ensure!(
                    Instant::now() < deadline,
                    "direct child cleanup exceeded two seconds without reaping; signal outcome: {signal_outcome}"
                );
                thread::sleep(Duration::from_millis(10));
            }
        }
        fn signal(&self, signal: i32) -> Result<()> {
            // SAFETY: owned pidfd prevents signaling a recycled or unrelated PID.
            let result = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.pidfd.as_raw_fd(),
                    signal,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            };
            if result < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                bail!(
                    "signal owned GUI child failed: {}",
                    io::Error::last_os_error()
                );
            }
            Ok(())
        }
        fn close(&mut self) -> Result<()> {
            self.signal(libc::SIGTERM)?;
            let graceful = Instant::now() + Duration::from_secs(5);
            loop {
                if self.child.try_wait()?.is_some() {
                    self.closed = true;
                    return Ok(());
                }
                if Instant::now() >= graceful {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            self.signal(libc::SIGKILL)?;
            let forced = Instant::now() + Duration::from_secs(2);
            while Instant::now() < forced {
                if self.child.try_wait()?.is_some() {
                    self.closed = true;
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(10));
            }
            bail!("direct GUI child cleanup incomplete; outer owned-session cleanup required")
        }
    }
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            if !self.closed {
                if let Err(error) = self.close() {
                    eprintln!("actual GUI child cleanup failure: {error:#}");
                }
            }
        }
    }

    struct Terminal {
        master: File,
        slave: File,
        log: File,
        bytes: u64,
    }
    impl Terminal {
        fn new(path: &Path) -> Result<Self> {
            let mut master = -1;
            let mut slave = -1;
            let size = libc::winsize {
                ws_row: 40,
                ws_col: 120,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            // SAFETY: writable integer descriptors, no name buffer, initialized dimensions.
            ensure!(
                unsafe {
                    libc::openpty(
                        &mut master,
                        &mut slave,
                        std::ptr::null_mut(),
                        std::ptr::null(),
                        &size,
                    )
                } == 0,
                "openpty failed: {}",
                io::Error::last_os_error()
            );
            // SAFETY: openpty returned two distinct owned descriptors.
            let master = unsafe { File::from_raw_fd(master) };
            let slave = unsafe { File::from_raw_fd(slave) };
            for descriptor in [master.as_raw_fd(), slave.as_raw_fd()] {
                // SAFETY: descriptor belongs to one of the two live PTY files.
                ensure!(
                    unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
                    "make owned PTY descriptor close-on-exec failed"
                );
            }
            // SAFETY: flags are applied to the live owned PTY descriptor.
            let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
            ensure!(flags >= 0, "read owned PTY descriptor flags failed");
            ensure!(
                unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                    == 0,
                "make PTY nonblocking failed"
            );
            Ok(Self {
                master,
                slave,
                log: OpenOptions::new().create_new(true).write(true).open(path)?,
                bytes: 0,
            })
        }
        fn drain(&mut self) -> Result<()> {
            let mut buffer = [0; 8192];
            loop {
                match self.master.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        self.bytes += count as u64;
                        ensure!(
                            self.bytes <= LOG_LIMIT,
                            "application PTY log budget exhausted"
                        );
                        self.log.write_all(&buffer[..count])?;
                    }
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock
                            || error.raw_os_error() == Some(libc::EIO) =>
                    {
                        break;
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(())
        }
    }

    struct Desktop {
        connection: RustConnection,
        root: Window,
        keyboard: xproto::GetKeyboardMappingReply,
        minimum: u8,
        screenshots: Vec<String>,
    }
    impl Desktop {
        fn connect(display: &str) -> Result<Self> {
            let (connection, screen) = x11rb::connect(Some(display))?;
            let root = connection
                .setup()
                .roots
                .get(screen)
                .context("X11 selected screen missing")?
                .root;
            let minimum = connection.setup().min_keycode;
            let count = connection
                .setup()
                .max_keycode
                .checked_sub(minimum)
                .and_then(|count| count.checked_add(1))
                .context("X11 keyboard mapping range invalid")?;
            let keyboard = connection.get_keyboard_mapping(minimum, count)?.reply()?;
            connection.xtest_get_version(2, 2)?.reply()?;
            Ok(Self {
                connection,
                root,
                keyboard,
                minimum,
                screenshots: Vec::new(),
            })
        }
        fn keycode(&self, symbol: u32) -> Result<(u8, bool)> {
            let width = usize::from(self.keyboard.keysyms_per_keycode);
            ensure!(width > 0, "empty X11 keyboard mapping");
            for (index, symbols) in self.keyboard.keysyms.chunks(width).enumerate() {
                for (column, candidate) in symbols.iter().take(2).enumerate() {
                    if *candidate == symbol {
                        return Ok((
                            u8::try_from(index)?
                                .checked_add(self.minimum)
                                .context("X11 keycode overflow")?,
                            column == 1,
                        ));
                    }
                }
            }
            bail!("X11 symbol unavailable: {symbol:#x}")
        }
        fn key(&self, code: u8, press: bool) -> Result<()> {
            self.connection
                .xtest_fake_input(
                    if press {
                        xproto::KEY_PRESS_EVENT
                    } else {
                        xproto::KEY_RELEASE_EVENT
                    },
                    code,
                    x11rb::CURRENT_TIME,
                    self.root,
                    0,
                    0,
                    0,
                )?
                .check()?;
            self.connection.flush()?;
            Ok(())
        }
        fn chord(&self, symbol: u32, control: bool, alt: bool) -> Result<()> {
            let (code, shifted) = self.keycode(symbol)?;
            let mut modifiers = Vec::new();
            if control {
                modifiers.push(self.keycode(0xffe3)?.0);
            }
            if alt {
                modifiers.push(self.keycode(0xffe9)?.0);
            }
            if shifted {
                modifiers.push(self.keycode(0xffe1)?.0);
            }
            for modifier in &modifiers {
                self.key(*modifier, true)?;
            }
            self.key(code, true)?;
            self.key(code, false)?;
            for modifier in modifiers.iter().rev() {
                self.key(*modifier, false)?;
            }
            Ok(())
        }
        fn text(&self, text: &str) -> Result<()> {
            ensure!(
                text.is_ascii() && text.len() <= 4096,
                "GUI text input budget or alphabet differs"
            );
            for byte in text.bytes() {
                let symbol = match byte {
                    b'\n' => 0xff0d,
                    b'\t' => 0xff09,
                    _ => u32::from(byte),
                };
                self.chord(symbol, false, false)?;
            }
            Ok(())
        }
        fn owned_window(&self, pid: u32) -> Result<Option<Window>> {
            let atom = self
                .connection
                .intern_atom(false, b"_NET_WM_PID")?
                .reply()?
                .atom;
            let windows = self.connection.query_tree(self.root)?.reply()?.children;
            ensure!(windows.len() <= 256, "X11 root window budget exhausted");
            let mut matches = Vec::new();
            for window in windows {
                let property = self
                    .connection
                    .get_property(false, window, atom, xproto::AtomEnum::CARDINAL, 0, 1)?
                    .reply()?;
                let owned = property
                    .value32()
                    .is_some_and(|mut values| values.next() == Some(pid));
                if owned
                    && self
                        .connection
                        .get_window_attributes(window)?
                        .reply()?
                        .map_state
                        == xproto::MapState::VIEWABLE
                {
                    matches.push(window);
                }
            }
            ensure!(
                matches.len() <= 1,
                "multiple mapped windows for owned app PID"
            );
            Ok(matches.first().copied())
        }
        fn focus(&self, window: Window) -> Result<()> {
            self.connection
                .set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME)?
                .check()?;
            self.connection
                .configure_window(
                    window,
                    &xproto::ConfigureWindowAux::new()
                        .x(0)
                        .y(0)
                        .width(u32::from(WIDTH))
                        .height(u32::from(HEIGHT)),
                )?
                .check()?;
            self.connection.flush()?;
            Ok(())
        }
        fn screenshot(&mut self, output: &Path, name: &str) -> Result<()> {
            ensure!(
                self.screenshots.len() < MAX_SCREENSHOTS
                    && !name.contains('/')
                    && name.ends_with(".png"),
                "screenshot budget/name differs"
            );
            let reply = self
                .connection
                .get_image(
                    ImageFormat::Z_PIXMAP,
                    self.root,
                    0,
                    0,
                    WIDTH,
                    HEIGHT,
                    u32::MAX,
                )?
                .reply()?;
            let setup = self.connection.setup();
            let format = setup
                .pixmap_formats
                .iter()
                .find(|format| format.depth == reply.depth)
                .context("X11 pixel format missing")?;
            ensure!(
                reply.depth == 24
                    && format.bits_per_pixel == 32
                    && format.scanline_pad == 32
                    && setup.image_byte_order == xproto::ImageOrder::LSB_FIRST,
                "unsupported Xvfb pixel format"
            );
            let visual = setup
                .roots
                .iter()
                .flat_map(|screen| &screen.allowed_depths)
                .flat_map(|depth| &depth.visuals)
                .find(|visual| visual.visual_id == reply.visual)
                .context("X11 image visual missing")?;
            ensure!(
                visual.red_mask == 0xff0000
                    && visual.green_mask == 0xff00
                    && visual.blue_mask == 0xff,
                "unsupported Xvfb visual masks"
            );
            let rgb = decode_bgrx(&reply.data, usize::from(WIDTH), usize::from(HEIGHT))?;
            let file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(output.join(name))?;
            let mut encoder = png::Encoder::new(
                producer::LimitedWriter::new(file, SCREENSHOT_LIMIT),
                u32::from(WIDTH),
                u32::from(HEIGHT),
            );
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header()?;
            writer.write_image_data(&rgb)?;
            writer.finish()?;
            self.screenshots.push(name.to_owned());
            Ok(())
        }
    }

    fn pause(terminal: &mut Terminal, output: &Path, duration: Duration) -> Result<()> {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            terminal.drain()?;
            require_output_budget(output)?;
            thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    fn profile(output: &Path, name: &str, theme: Option<&str>) -> Result<PathBuf> {
        let directory = output.join(name);
        fs::create_dir(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        fs::create_dir(directory.join("config"))?;
        let mut settings = serde_json::json!({"auto_update": false, "telemetry": {"diagnostics": false,"metrics": false}, "enable_language_server": false, "use_autoclose": false, "auto_indent": "none", "show_edit_predictions": false, "format_on_save": "off", "restore_on_startup": "empty_tab", "session": {"restore_unsaved_buffers": false,"trust_all_worktrees": false}});
        if let Some(theme) = theme {
            settings["theme"] = serde_json::json!(theme);
        }
        write_json(&directory.join("config/settings.json"), &settings, MIB)?;
        write_json(
            &directory.join("config/keymap.json"),
            &serde_json::json!([{"context":"Workspace","bindings":{"ctrl-alt-t":"theme_selector::Toggle", "ctrl-alt-m":"theme::ToggleMode"}}]),
            MIB,
        )?;
        Ok(directory)
    }

    fn launch(
        configuration: &Configuration,
        display: &str,
        profile: &Path,
        suffix: &str,
    ) -> Result<(OwnedChild, Terminal, Desktop)> {
        let mut terminal = Terminal::new(
            &configuration
                .output_directory
                .join(format!("app-{suffix}.pty.log")),
        )?;
        let runtime = profile.join(format!("runtime-{suffix}"));
        fs::create_dir(&runtime)?;
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))?;
        let mut command = Command::new(&configuration.executable);
        command
            .current_dir(&configuration.working_directory)
            .arg("--user-data-dir")
            .arg(profile)
            .arg(configuration.output_directory.join("fixture"))
            .arg(configuration.output_directory.join("fixture/qa_main.py"))
            .env("DISPLAY", display)
            .env_remove("WAYLAND_DISPLAY")
            .env("ZED_ALLOW_EMULATED_GPU", "1")
            .env_remove("LD_PRELOAD")
            .env_remove("LD_AUDIT")
            .env("WGPU_BACKEND", "vulkan")
            .env("VK_DRIVER_FILES", &configuration.vulkan_icd)
            .env("VK_ICD_FILENAMES", &configuration.vulkan_icd)
            .env("LD_LIBRARY_PATH", &configuration.library_directory)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("XDG_CONFIG_HOME", profile.join("xdg-config"))
            .env("XDG_DATA_HOME", profile.join("xdg-data"))
            .env("XDG_CACHE_HOME", profile.join("xdg-cache"))
            .stdin(Stdio::from(terminal.slave.try_clone()?))
            .stdout(Stdio::from(terminal.slave.try_clone()?))
            .stderr(Stdio::from(terminal.slave.try_clone()?));
        // OwnedChild supplies the new session and pidfd/descendant cleanup. The
        // existing Xvfb direct child is in its excluded baseline; stdout is a real
        // PTY without needing a controlling terminal for graphical input.
        let mut child = OwnedChild::spawn(&mut command, Duration::from_secs(100))?;
        child.set_receipt(
            configuration
                .output_directory
                .join(format!("app-{suffix}-closure.json")),
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            ensure!(
                Instant::now() < deadline,
                "owned app startup window deadline exhausted"
            );
            ensure!(
                child.child.try_wait()?.is_none(),
                "owned app exited before its window"
            );
            if let Ok(desktop) = Desktop::connect(display) {
                if let Some(window) = desktop.owned_window(child.child.id())? {
                    desktop.focus(window)?;
                    pause(
                        &mut terminal,
                        &configuration.output_directory,
                        Duration::from_secs(1),
                    )?;
                    return Ok((child, terminal, desktop));
                }
            }
            pause(
                &mut terminal,
                &configuration.output_directory,
                Duration::from_millis(50),
            )?;
        }
    }

    fn edit(
        desktop: &Desktop,
        terminal: &mut Terminal,
        output: &Path,
        file: &str,
        content: &str,
    ) -> Result<()> {
        desktop.chord(u32::from(b'p'), true, false)?;
        pause(terminal, output, Duration::from_millis(350))?;
        desktop.text(file)?;
        pause(terminal, output, Duration::from_millis(700))?;
        desktop.chord(0xff0d, false, false)?;
        pause(terminal, output, Duration::from_millis(350))?;
        desktop.chord(u32::from(b'a'), true, false)?;
        desktop.text(content)?;
        desktop.chord(u32::from(b's'), true, false)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if read_bounded(&output.join("fixture").join(file), MIB)?.0 == content.as_bytes() {
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "actual editor save deadline exhausted: {file}"
            );
            pause(terminal, output, Duration::from_millis(50))?;
        }
        // Open the file again through the public UI, retaining a capture for review.
        desktop.chord(u32::from(b'p'), true, false)?;
        pause(terminal, output, Duration::from_millis(350))?;
        desktop.text(file)?;
        pause(terminal, output, Duration::from_millis(350))?;
        desktop.chord(0xff0d, false, false)?;
        pause(terminal, output, Duration::from_millis(500))?;
        Ok(())
    }

    fn selector(
        desktop: &mut Desktop,
        terminal: &mut Terminal,
        output: &Path,
        capture: &str,
    ) -> Result<()> {
        desktop.chord(u32::from(b't'), true, true)?;
        pause(terminal, output, Duration::from_millis(700))?;
        desktop.screenshot(output, capture)?;
        desktop.chord(0xff1b, false, false)?;
        pause(terminal, output, Duration::from_millis(300))?;
        Ok(())
    }

    fn select_theme(
        desktop: &Desktop,
        terminal: &mut Terminal,
        output: &Path,
        profile: &Path,
        theme: &str,
    ) -> Result<()> {
        desktop.chord(u32::from(b't'), true, true)?;
        pause(terminal, output, Duration::from_millis(350))?;
        desktop.text(theme)?;
        pause(terminal, output, Duration::from_millis(500))?;
        desktop.chord(0xff0d, false, false)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let settings: serde_json::Value = serde_json::from_slice(
                &read_bounded(&profile.join("config/settings.json"), MIB)?.0,
            )?;
            if settings["theme"] == theme {
                let name = format!(
                    "{}-selected-settings.json",
                    theme.replace(' ', "-").to_ascii_lowercase()
                );
                write_json(&output.join(name), &settings, MIB)?;
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "UI-selected theme was not saved: {theme}"
            );
            pause(terminal, output, Duration::from_millis(50))?;
        }
        Ok(())
    }

    pub(super) fn worker(args: GuiSmokeWorkerArgs) -> Result<()> {
        let configuration: Configuration =
            authenticated_json(&args.configuration, &args.configuration_sha256, MIB)?;
        ensure!(
            configuration.schema_version == 1,
            "GUI configuration schema differs"
        );
        ensure!(
            digest_file(&configuration.executable, MAX_BINARY_BYTES)?
                .0
                .sha256
                == configuration.executable_sha256
                && digest_file(&std::env::current_exe()?, MAX_BINARY_BYTES)?
                    .0
                    .sha256
                    == configuration.recorder_sha256
                && digest_file(&configuration.vulkan_icd, MIB)?.0.sha256
                    == configuration.vulkan_icd_sha256,
            "worker executable or graphics identity changed"
        );
        let output = &configuration.output_directory;
        fs::create_dir(output.join("fixture"))?;
        create_file(
            &output.join("fixture/qa_main.py"),
            b"# edit me through the app\n",
        )?;
        create_file(
            &output.join("fixture/qa_index.html"),
            b"<!-- edit me through the app -->\n",
        )?;
        let display_path = output.join("xvfb-display-number");
        let display_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&display_path)?;
        // File descriptor 1 is owned, inherited stdout; -displayfd selects a fresh
        // display rather than connecting to another user's already-running server.
        let mut xvfb_command = Command::new("Xvfb");
        xvfb_command
            .args([
                "-displayfd",
                "1",
                "-screen",
                "0",
                "1600x1000x24",
                "-nolisten",
                "tcp",
                "-ac",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::from(display_file))
            .stderr(Stdio::from(
                OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(output.join("xvfb.log"))?,
            ));
        let mut xvfb = ChildGuard::spawn(&mut xvfb_command, false)?;
        let startup = Instant::now() + Duration::from_secs(10);
        let display = loop {
            ensure!(
                xvfb.child.try_wait()?.is_none() && Instant::now() < startup,
                "owned Xvfb startup failed"
            );
            let bytes = read_bounded(&display_path, 32)?.0;
            if bytes.last() == Some(&b'\n') {
                let number: u16 = std::str::from_utf8(&bytes)?.trim().parse()?;
                break format!(":{number}");
            }
            thread::sleep(Duration::from_millis(20));
        };
        let default_profile = profile(output, "first-run", None)?;
        let mut captures = Vec::new();
        let (mut app, mut terminal, mut desktop) =
            launch(&configuration, &display, &default_profile, "first")?;
        desktop.screenshot(output, "01-first-run.png")?;
        edit(&desktop, &mut terminal, output, "qa_main.py", PYTHON_RESULT)?;
        desktop.screenshot(output, "02-python-saved-reopened.png")?;
        edit(
            &desktop,
            &mut terminal,
            output,
            "qa_index.html",
            HTML_RESULT,
        )?;
        desktop.screenshot(output, "03-html-saved-reopened.png")?;
        selector(
            &mut desktop,
            &mut terminal,
            output,
            "04-first-run-theme-selector.png",
        )?;
        select_theme(
            &desktop,
            &mut terminal,
            output,
            &default_profile,
            "Studio Light",
        )?;
        captures.append(&mut desktop.screenshots);
        app.finish(true)?.require_closed()?;
        terminal.drain()?;
        for (name, theme, capture) in [
            ("one-dark", "One Dark", "05-explicit-one-dark-selector.png"),
            (
                "one-light",
                "One Light",
                "06-explicit-one-light-selector.png",
            ),
        ] {
            let profile = profile(output, name, Some(theme))?;
            let (mut app, mut terminal, mut desktop) =
                launch(&configuration, &display, &profile, name)?;
            selector(&mut desktop, &mut terminal, output, capture)?;
            let settings: serde_json::Value = serde_json::from_slice(
                &read_bounded(&profile.join("config/settings.json"), MIB)?.0,
            )?;
            ensure!(
                settings["theme"] == theme,
                "explicit One choice changed after startup"
            );
            write_json(
                &output.join(format!("{name}-settings-before-mode.json")),
                &settings,
                MIB,
            )?;
            desktop.chord(u32::from(b'm'), true, true)?;
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let settings: serde_json::Value = serde_json::from_slice(
                    &read_bounded(&profile.join("config/settings.json"), MIB)?.0,
                )?;
                if settings["theme"]["light"] == "One Light"
                    && settings["theme"]["dark"] == "One Dark"
                {
                    write_json(
                        &output.join(format!("{name}-settings-after-mode.json")),
                        &settings,
                        MIB,
                    )?;
                    break;
                }
                ensure!(
                    Instant::now() < deadline,
                    "explicit One appearance-mode change did not preserve its family"
                );
                pause(&mut terminal, output, Duration::from_millis(50))?;
            }
            app.finish(true)?.require_closed()?;
            terminal.drain()?;
            captures.append(&mut desktop.screenshots);
        }
        // Reusing the profile keeps settings; a fresh runtime directory avoids
        // trusting stale sockets left by the previously closed application.
        let (mut app, mut terminal, mut desktop) =
            launch(&configuration, &display, &default_profile, "light-reload")?;
        selector(
            &mut desktop,
            &mut terminal,
            output,
            "07-studio-light-reloaded-selector.png",
        )?;
        select_theme(
            &desktop,
            &mut terminal,
            output,
            &default_profile,
            "Studio Dark",
        )?;
        captures.append(&mut desktop.screenshots);
        app.finish(true)?.require_closed()?;
        terminal.drain()?;
        let (mut app, mut terminal, mut desktop) =
            launch(&configuration, &display, &default_profile, "dark-reload")?;
        selector(
            &mut desktop,
            &mut terminal,
            output,
            "08-studio-dark-reloaded-selector.png",
        )?;
        captures.append(&mut desktop.screenshots);
        app.finish(true)?.require_closed()?;
        terminal.drain()?;
        xvfb.close()?;
        require_file_bytes(&output.join("fixture/qa_main.py"), PYTHON_RESULT.as_bytes())?;
        require_file_bytes(
            &output.join("fixture/qa_index.html"),
            HTML_RESULT.as_bytes(),
        )?;
        require_output_budget(output)?;
        let report = WorkerReport {
            schema_version: 1, screenshots: captures, python_saved: true, html_saved: true,
            studio_light_persisted: true, studio_dark_persisted: true, explicit_one_settings_preserved: true,
            active_theme_visual_review: "PENDING: initial selected row/check icon in actual public theme-selector screenshots; Source33 rows lack accessible roles".to_owned(),
            chooser_visual_review: "PENDING: top chooser visible in actual app screenshots".to_owned(),
            syntax_highlighting_and_extensions_review: "PENDING: Python/HTML saves are verified; highlighting and extension language servers need separate visual/functional review".to_owned(),
            android_context_validation: "NOT_TESTED: this scenario set checks themes and generic editing only. Source33 predates Source42 context gating; known inherited generic Android surfaces still require a separately matching Source42 run".to_owned(),
        };
        let bytes = super::super::super::canonical(&report, MIB)?;
        io::stdout().write_all(&bytes)?;
        io::stdout().flush()?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn pidfd_acquisition_failure_stops_and_reaps_the_spawned_direct_child() -> Result<()> {
            const FIXTURE: &str = "KODA_GUI_PIDFD_ACQUISITION_FIXTURE";
            if let Some(ready) = std::env::var_os(FIXTURE) {
                fs::write(ready, b"running")?;
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    ensure!(
                        Instant::now() < deadline,
                        "acquisition fixture was not stopped"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
            }
            let temporary = tempfile::tempdir()?;
            let ready = temporary.path().join("child-ready");
            let module = module_path!()
                .split_once("::")
                .context("test module path")?
                .1;
            let name = format!(
                "{module}::pidfd_acquisition_failure_stops_and_reaps_the_spawned_direct_child"
            );
            let mut command = Command::new(std::env::current_exe()?);
            command
                .arg("--exact")
                .arg(name)
                .arg("--nocapture")
                .env(FIXTURE, &ready)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let pid = std::cell::Cell::new(0_u32);
            let result = ChildGuard::spawn_with_pidfd(&mut command, false, |child| {
                pid.set(child.id());
                let deadline = Instant::now() + Duration::from_secs(2);
                while !ready.try_exists()? {
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "acquisition child fixture did not start",
                        ));
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(io::Error::from_raw_os_error(libc::EMFILE))
            });
            let error = match result {
                Ok(mut guard) => {
                    guard.close()?;
                    bail!("injected pidfd acquisition unexpectedly succeeded");
                }
                Err(error) => format!("{error:#}"),
            };
            assert!(ready.try_exists()?);
            assert!(pid.get() > 0);
            assert!(error.contains("pin GUI child pidfd failed"));
            assert!(error.contains(&format!("os error {}", libc::EMFILE)));
            assert!(error.contains("acquisition cleanup: reaped direct child"));
            let mut status = 0;
            // SAFETY: waitpid has a valid status pointer and targets only the
            // recorded direct child; it never signals a PID after cleanup.
            let waited = unsafe { libc::waitpid(pid.get() as i32, &mut status, libc::WNOHANG) };
            let wait_error = io::Error::last_os_error();
            assert_eq!(waited, -1);
            assert_eq!(wait_error.raw_os_error(), Some(libc::ECHILD));
            Ok(())
        }

        #[test]
        fn first_run_profile_omits_theme_and_never_enables_global_trust() -> Result<()> {
            let temporary = tempfile::tempdir()?;
            let directory = profile(temporary.path(), "first-run", None)?;
            let settings: serde_json::Value = serde_json::from_slice(
                &read_bounded(&directory.join("config/settings.json"), MIB)?.0,
            )?;
            assert!(settings.get("theme").is_none());
            assert_eq!(settings["session"]["trust_all_worktrees"], false);
            assert_eq!(settings["enable_language_server"], false);
            Ok(())
        }

        #[test]
        fn explicit_one_profiles_keep_the_requested_family() -> Result<()> {
            let temporary = tempfile::tempdir()?;
            for (name, theme) in [("one-dark", "One Dark"), ("one-light", "One Light")] {
                let directory = profile(temporary.path(), name, Some(theme))?;
                let settings: serde_json::Value = serde_json::from_slice(
                    &read_bounded(&directory.join("config/settings.json"), MIB)?.0,
                )?;
                assert_eq!(settings["theme"], theme);
                assert_eq!(settings["session"]["trust_all_worktrees"], false);
            }
            Ok(())
        }
    }
}

fn decode_bgrx(bytes: &[u8], width: usize, height: usize) -> Result<Vec<u8>> {
    let pixels = width
        .checked_mul(height)
        .context("screenshot dimensions overflow")?;
    ensure!(
        width > 0 && height > 0 && pixels <= usize::from(WIDTH) * usize::from(HEIGHT),
        "screenshot dimensions exceed budget"
    );
    ensure!(
        bytes.len()
            == pixels
                .checked_mul(4)
                .context("screenshot byte length overflow")?,
        "X11 image byte length differs"
    );
    let mut result = Vec::with_capacity(pixels * 3);
    for pixel in bytes.chunks_exact(4) {
        result.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
    }
    Ok(result)
}

#[cfg(target_os = "linux")]
pub(crate) fn worker(args: GuiSmokeWorkerArgs) -> Result<()> {
    linux::worker(args)
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn worker(_: GuiSmokeWorkerArgs) -> Result<()> {
    bail!("GUI smoke requires Linux")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x11_pixels_decode_without_including_padding_alpha() -> Result<()> {
        assert_eq!(
            decode_bgrx(&[3, 2, 1, 255, 6, 5, 4, 0], 2, 1)?,
            vec![1, 2, 3, 4, 5, 6]
        );
        assert!(decode_bgrx(&[0; 7], 2, 1).is_err());
        assert!(decode_bgrx(&[], usize::MAX, 2).is_err());
        assert!(decode_bgrx(&[], 0, 1).is_err());
        Ok(())
    }

    #[test]
    fn fixture_writes_cannot_receive_credit_without_exact_app_save_bytes() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("qa_main.py");
        fs::write(&file, b"# edit me through the app\n")?;
        assert!(require_file_bytes(&file, PYTHON_RESULT.as_bytes()).is_err());
        fs::write(&file, PYTHON_RESULT)?;
        assert!(require_file_bytes(&file, PYTHON_RESULT.as_bytes()).is_ok());
        assert!(require_file_bytes(&file, HTML_RESULT.as_bytes()).is_err());
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn runtime_access_time_observation_never_waives_content_or_identity() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("asset");
        fs::write(&file, b"same asset")?;
        let before = physical(&fs::symlink_metadata(&file)?)?;
        let mut accessed = before.clone();
        accessed.atime_ns += 1;
        let rows = |value| {
            vec![RuntimeBinding {
                path: "runtime/source/assets/example".to_owned(),
                physical: value,
            }]
        };
        assert!(require_read_transitions(&rows(before.clone()), &rows(accessed.clone())).is_ok());
        accessed.mtime_ns += 1;
        assert!(require_read_transitions(&rows(before), &rows(accessed)).is_err());
        Ok(())
    }

    #[test]
    fn screenshot_and_output_budget_failure_are_not_visual_acceptance() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("overflow");
        File::create(&file)?.set_len(OUTPUT_LIMIT + 1)?;
        assert!(require_output_budget(directory.path()).is_err());
        assert_eq!(MAX_SCREENSHOTS, 8);
        Ok(())
    }
}
