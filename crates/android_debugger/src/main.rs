use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
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
    ensure!(
        !root.exists(),
        "installation already exists; move it aside explicitly before upgrading"
    );
    let parent = root.parent().context("invalid installation directory")?;
    fs::create_dir_all(parent)?;
    fs::create_dir(&root)?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    let source = std::env::current_exe()?;
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
}
