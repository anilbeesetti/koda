use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

fn main() {
    if let Err(error) = run() {
        eprintln!("Android debugger: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.is_empty() {
        return android_debugger::run();
    }
    match args[0].to_str() {
        Some("--version") if args.len() == 1 => {
            println!("{}", android_debugger::INSTALL_REVISION);
            Ok(())
        }
        Some("install") if args.len() == 2 => install(PathBuf::from(&args[1])),
        Some("probe-jvm") => android_debugger::fixture::run_cli(&args[1..]),
        Some("probe-test-runner") => android_debugger::runner_fixture::run(&args[1..]),
        Some("probe-test-runner-worker") => {
            android_debugger::runner_fixture::run_worker(&args[1..])
        }
        _ => bail!(
            "usage: android-debugger [--version | install INSTALL_DIRECTORY | probe-jvm --adapter PATH --java-home PATH --compiler-classpath CLASSPATH --stdlib PATH | probe-test-runner --root REPOSITORY [--device SERIAL]]"
        ),
    }
}

/// Install only the Rust executable that Cargo just built. No downloads, JDK,
/// Kotlin language server, Java adapter, shell expansion or archive extraction.
fn install(root: PathBuf) -> Result<()> {
    let root = if root.is_absolute() {
        root
    } else {
        std::env::current_dir()?.join(root)
    };
    let source = std::env::current_exe()?;
    if root.exists() {
        ensure!(
            existing_installation_matches(&root, &source)?,
            "installation already exists; move it aside explicitly before upgrading"
        );
        println!(
            "Already installed Rust Android debugger: {}",
            root.display()
        );
        return Ok(());
    }
    let parent = root.parent().context("invalid installation directory")?;
    fs::create_dir_all(parent)?;
    fs::create_dir(&root)?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    let executable = bin.join(if cfg!(windows) {
        "kotlin-debug-adapter.exe"
    } else {
        "kotlin-debug-adapter"
    });
    let mut input = fs::File::open(&source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&executable)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    fs::set_permissions(&executable, fs::metadata(&source)?.permissions())?;
    let mut marker = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(".revision"))?;
    writeln!(marker, "{}", android_debugger::INSTALL_REVISION)?;
    marker.sync_all()?;
    println!("Installed Rust Android debugger: {}", executable.display());
    Ok(())
}

/// A repeated install is a read-only success only for the exact current Rust
/// binary, revision and expected files. Unknown or changed content is preserved.
fn existing_installation_matches(root: &Path, source: &Path) -> Result<bool> {
    if !fs::symlink_metadata(root)?.is_dir() || !directory_has_only(root, &[".revision", "bin"])? {
        return Ok(false);
    }
    let name = if cfg!(windows) {
        "kotlin-debug-adapter.exe"
    } else {
        "kotlin-debug-adapter"
    };
    let bin = root.join("bin");
    if !fs::symlink_metadata(&bin)?.is_dir() || !directory_has_only(&bin, &[name])? {
        return Ok(false);
    }
    let marker = root.join(".revision");
    let marker_metadata = fs::symlink_metadata(&marker)?;
    let expected_marker = format!("{}\n", android_debugger::INSTALL_REVISION);
    if !marker_metadata.is_file() || marker_metadata.len() != expected_marker.len() as u64 {
        return Ok(false);
    }
    let mut marker_file = open_regular(&marker)?;
    if !file_matches_metadata(&marker_file, &marker_metadata)? {
        return Ok(false);
    }
    let mut actual_marker = vec![0; expected_marker.len()];
    marker_file.read_exact(&mut actual_marker)?;
    if actual_marker != expected_marker.as_bytes()
        || !file_matches_metadata(&marker_file, &marker_metadata)?
    {
        return Ok(false);
    }
    let executable = bin.join(name);
    let installed_metadata = fs::symlink_metadata(&executable)?;
    let source_metadata = fs::metadata(source)?;
    if !installed_metadata.is_file() || installed_metadata.len() != source_metadata.len() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if installed_metadata.permissions().mode() != source_metadata.permissions().mode() {
            return Ok(false);
        }
    }
    #[cfg(not(unix))]
    if installed_metadata.permissions().readonly() != source_metadata.permissions().readonly() {
        return Ok(false);
    }
    let mut installed = open_regular(&executable)?;
    let mut original = open_regular(source)?;
    if !file_matches_metadata(&installed, &installed_metadata)?
        || !file_matches_metadata(&original, &source_metadata)?
    {
        return Ok(false);
    }
    let mut remaining = source_metadata.len();
    let mut installed_chunk = [0; 65536];
    let mut original_chunk = [0; 65536];
    while remaining != 0 {
        let count = remaining.min(installed_chunk.len() as u64) as usize;
        installed.read_exact(&mut installed_chunk[..count])?;
        original.read_exact(&mut original_chunk[..count])?;
        if installed_chunk[..count] != original_chunk[..count] {
            return Ok(false);
        }
        remaining -= count as u64;
    }
    Ok(file_matches_metadata(&installed, &installed_metadata)?
        && file_matches_metadata(&original, &source_metadata)?)
}

fn file_matches_metadata(file: &fs::File, expected: &fs::Metadata) -> Result<bool> {
    let actual = file.metadata()?;
    if !actual.is_file()
        || actual.len() != expected.len()
        || actual.modified()? != expected.modified()?
    {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if actual.dev() != expected.dev()
            || actual.ino() != expected.ino()
            || actual.permissions().mode() != expected.permissions().mode()
        {
            return Ok(false);
        }
    }
    #[cfg(not(unix))]
    if actual.permissions().readonly() != expected.permissions().readonly() {
        return Ok(false);
    }
    Ok(true)
}

fn open_regular(path: &Path) -> Result<fs::File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Refuse a substituted leaf symlink or blocking special file. Matching
        // cached installs are only read; no existing target is ever overwritten.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "installer input is not a regular file"
    );
    Ok(file)
}

fn directory_has_only(path: &Path, expected: &[&str]) -> Result<bool> {
    let mut count = 0;
    for entry in fs::read_dir(path)? {
        let name = entry?.file_name();
        count += 1;
        if count > expected.len()
            || !expected
                .iter()
                .any(|expected| name.as_os_str() == std::ffi::OsStr::new(expected))
        {
            return Ok(false);
        }
    }
    Ok(count == expected.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installation_refuses_existing_user_content_without_overwriting_it() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("installed");
        fs::create_dir(&root).unwrap();
        fs::write(root.join(".revision"), "user-owned-marker").unwrap();
        assert!(install(root.clone()).is_err());
        assert_eq!(
            fs::read_to_string(root.join(".revision")).unwrap(),
            "user-owned-marker"
        );
        assert!(!root.join("bin").exists());
    }

    #[test]
    fn installation_contains_only_rust_binary_and_revision_without_jvm_downloads() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("installed");
        install(root.clone()).unwrap();
        assert_eq!(
            fs::read_to_string(root.join(".revision")).unwrap().trim(),
            android_debugger::INSTALL_REVISION
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
        assert_eq!(fs::read_dir(root.join("bin")).unwrap().count(), 1);
        let executable = root.join("bin").join(if cfg!(windows) {
            "kotlin-debug-adapter.exe"
        } else {
            "kotlin-debug-adapter"
        });
        assert!(executable.is_file());
        assert_eq!(
            fs::metadata(executable).unwrap().len(),
            fs::metadata(std::env::current_exe().unwrap())
                .unwrap()
                .len()
        );
    }

    #[test]
    fn matching_repeat_install_is_read_only_and_requires_no_jdk_or_download() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("installed");
        install(root.clone()).unwrap();
        let name = if cfg!(windows) {
            "kotlin-debug-adapter.exe"
        } else {
            "kotlin-debug-adapter"
        };
        let executable = root.join("bin").join(name);
        let marker = root.join(".revision");
        let binary_before = fs::metadata(&executable).unwrap();
        let marker_before = fs::metadata(&marker).unwrap();
        install(root.clone()).unwrap();
        assert_eq!(
            fs::metadata(executable).unwrap().modified().unwrap(),
            binary_before.modified().unwrap()
        );
        assert_eq!(
            fs::metadata(&marker).unwrap().modified().unwrap(),
            marker_before.modified().unwrap()
        );
        assert_eq!(
            fs::read_to_string(marker).unwrap(),
            format!("{}\n", android_debugger::INSTALL_REVISION)
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
        assert_eq!(fs::read_dir(root.join("bin")).unwrap().count(), 1);
    }

    #[test]
    fn matching_revision_with_changed_binary_is_refused_without_overwrite() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("installed");
        install(root.clone()).unwrap();
        let name = if cfg!(windows) {
            "kotlin-debug-adapter.exe"
        } else {
            "kotlin-debug-adapter"
        };
        let executable = root.join("bin").join(name);
        let mut changed = OpenOptions::new().write(true).open(&executable).unwrap();
        changed.write_all(b"changed").unwrap();
        changed.sync_all().unwrap();
        drop(changed);
        assert!(install(root.clone()).is_err());
        let mut prefix = [0; 7];
        fs::File::open(executable)
            .unwrap()
            .read_exact(&mut prefix)
            .unwrap();
        assert_eq!(&prefix, b"changed");
        assert_eq!(
            fs::read_to_string(root.join(".revision")).unwrap(),
            format!("{}\n", android_debugger::INSTALL_REVISION)
        );
    }

    #[test]
    fn installation_propagates_parent_filesystem_failure_without_replacing_user_data() {
        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().join("user-file");
        fs::write(&parent, b"preserve").unwrap();
        let root = parent.join("installed");
        assert!(install(root.clone()).is_err());
        assert_eq!(fs::read(parent).unwrap(), b"preserve");
        assert!(!root.exists());
    }

    #[test]
    fn matching_repeat_refuses_unknown_extra_content_without_touching_it() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("installed");
        install(root.clone()).unwrap();
        fs::write(root.join("user-data"), b"preserve").unwrap();
        assert!(install(root.clone()).is_err());
        assert_eq!(fs::read(root.join("user-data")).unwrap(), b"preserve");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn matching_repeat_refuses_a_substituted_revision_symlink() {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("installed");
        install(root.clone()).unwrap();
        let outside = temporary.path().join("outside-marker");
        fs::write(
            &outside,
            format!("{}\n", android_debugger::INSTALL_REVISION),
        )
        .unwrap();
        fs::remove_file(root.join(".revision")).unwrap();
        symlink(&outside, root.join(".revision")).unwrap();
        assert!(install(root.clone()).is_err());
        assert!(
            fs::symlink_metadata(root.join(".revision"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(
            fs::read_to_string(outside).unwrap(),
            format!("{}\n", android_debugger::INSTALL_REVISION)
        );
    }
}
