use anyhow::Result;
#[cfg(any(compose_preview_bundled, test))]
use anyhow::{Context as _, ensure};

#[cfg(compose_preview_bundled)]
pub(super) fn installation() -> Result<std::path::PathBuf> {
    let cache = dirs::cache_dir()
        .context("No application cache directory is available for Compose previews")?
        .join("koda/compose-preview");
    materialize(
        include_bytes!(concat!(env!("OUT_DIR"), "/compose-preview.zip")),
        env!("KODA_COMPOSE_PREVIEW_BUNDLE_ID"),
        &cache,
    )
}

#[cfg(not(compose_preview_bundled))]
pub(super) fn installation() -> Result<std::path::PathBuf> {
    anyhow::bail!("Google layoutlib does not provide Compose previews for this platform")
}

#[cfg(any(compose_preview_bundled, test))]
fn materialize(archive: &[u8], id: &str, cache: &std::path::Path) -> Result<std::path::PathBuf> {
    use fs2::FileExt as _;
    use std::{
        collections::BTreeMap,
        fs,
        io::{Cursor, Read as _},
    };
    ensure!(
        id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Invalid preview bundle identity"
    );
    fs::create_dir_all(cache)?;
    ensure!(
        !fs::symlink_metadata(cache)?.file_type().is_symlink(),
        "Compose preview cache must not be a symbolic link"
    );
    let lock_path = cache.join(".extract.lock");
    if let Ok(metadata) = fs::symlink_metadata(&lock_path) {
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "Invalid preview cache lock file"
        );
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    lock.lock_exclusive()
        .context("Could not lock the Compose preview runtime cache")?;
    let directory = cache.join(id);
    let mut archive = zip::ZipArchive::new(Cursor::new(archive))?;
    let mut manifest = Vec::new();
    archive.by_name(".files.json")?.read_to_end(&mut manifest)?;
    let sizes: BTreeMap<String, u64> = serde_json::from_slice(&manifest)?;
    let valid = |directory: &std::path::Path| -> Result<bool> {
        if !directory.try_exists()? {
            return Ok(false);
        }
        let metadata = fs::symlink_metadata(directory)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "Invalid Compose preview runtime cache directory"
        );
        if !fs::read_to_string(directory.join(".bundle-id")).is_ok_and(|contents| contents == id) {
            return Ok(false);
        }
        for (name, size) in &sizes {
            let metadata = match fs::symlink_metadata(directory.join(name)) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error.into()),
            };
            if !metadata.is_file() || metadata.len() != *size {
                return Ok(false);
            }
            #[cfg(unix)]
            if name == "java/bin/java" {
                use std::os::unix::fs::PermissionsExt as _;
                if metadata.permissions().mode() & 0o111 == 0 {
                    return Ok(false);
                }
            }
        }
        Ok(super::validate_installation(directory).is_ok())
    };
    if valid(&directory)? {
        return Ok(directory);
    }
    let staging = tempfile::Builder::new()
        .prefix("extract-")
        .tempdir_in(cache)?;
    archive
        .extract(staging.path())
        .context("Could not unpack the bundled Compose preview runtime")?;
    super::validate_installation(staging.path())?;
    let java = staging.path().join(if cfg!(windows) {
        "java/bin/java.exe"
    } else {
        "java/bin/java"
    });
    ensure!(java.is_file(), "The preview bundle has no Java runtime");
    fs::write(staging.path().join(".bundle-id"), id)?;
    ensure!(
        valid(staging.path())?,
        "The extracted preview runtime is incomplete"
    );
    if directory.try_exists()? {
        fs::remove_dir_all(&directory)?;
    }
    fs::rename(staging.path(), &directory)
        .context("Could not publish the Compose preview runtime cache")?;
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        fs,
        io::{Cursor, Write as _},
    };

    fn fixture() -> Result<Vec<u8>> {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let mut sizes = BTreeMap::new();
        for (name, contents) in [
            ("PreviewBridge.class", "bridge"),
            ("renderer.jar", "renderer"),
            ("layoutlib.jar", "layoutlib"),
            ("layoutlib/data/framework_res.jar", "resources"),
            (".protocol", "2\n"),
            (
                if cfg!(windows) {
                    "java/bin/java.exe"
                } else {
                    "java/bin/java"
                },
                "java",
            ),
        ] {
            archive.start_file(
                name,
                zip::write::FileOptions::default().unix_permissions(0o755),
            )?;
            archive.write_all(contents.as_bytes())?;
            sizes.insert(name, contents.len());
        }
        archive.start_file(".files.json", zip::write::FileOptions::default())?;
        archive.write_all(&serde_json::to_vec(&sizes)?)?;
        Ok(archive.finish()?.into_inner())
    }

    #[test]
    fn unpacked_runtime_is_reused_and_repaired() -> Result<()> {
        let cache = tempfile::tempdir()?;
        let archive = fixture()?;
        let id = "a".repeat(64);
        let installed = materialize(&archive, &id, cache.path())?;
        let marker = installed.join("reuse-check");
        fs::write(&marker, "keep")?;
        assert_eq!(materialize(&archive, &id, cache.path())?, installed);
        assert!(marker.exists());
        fs::write(installed.join("renderer.jar"), "truncated")?;
        materialize(&archive, &id, cache.path())?;
        assert!(!marker.exists());
        assert_eq!(fs::read(installed.join("renderer.jar"))?, b"renderer");
        fs::remove_file(installed.join("java/bin").join(if cfg!(windows) {
            "java.exe"
        } else {
            "java"
        }))?;
        materialize(&archive, &id, cache.path())?;
        assert!(super::super::java_binary(&installed)?.is_file());
        Ok(())
    }

    #[test]
    fn simultaneous_first_use_publishes_one_complete_runtime() -> Result<()> {
        let cache = tempfile::tempdir()?;
        let archive = fixture()?;
        let id = "b".repeat(64);
        let paths = std::thread::scope(|scope| {
            let workers = (0..4)
                .map(|_| scope.spawn(|| materialize(&archive, &id, cache.path())))
                .collect::<Vec<_>>();
            workers
                .into_iter()
                .map(|worker| {
                    worker
                        .join()
                        .map_err(|_| anyhow::anyhow!("Extraction worker panicked"))?
                })
                .collect::<Result<Vec<_>>>()
        })?;
        let first = paths.first().context("No extraction workers ran")?;
        assert!(paths.iter().all(|path| path == first));
        Ok(())
    }

    #[test]
    fn failed_extraction_does_not_publish_a_cache_entry() -> Result<()> {
        let cache = tempfile::tempdir()?;
        let id = "c".repeat(64);
        assert!(materialize(b"invalid zip", &id, cache.path()).is_err());
        assert!(!cache.path().join(&id).exists());
        assert!(materialize(&fixture()?, &id, cache.path())?.is_dir());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn refuses_cache_symlinks_and_preserves_java_permissions() -> Result<()> {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let cache = tempfile::tempdir()?;
        let unrelated = tempfile::tempdir()?;
        let id = "d".repeat(64);
        symlink(unrelated.path(), cache.path().join(&id))?;
        assert!(materialize(&fixture()?, &id, cache.path()).is_err());
        assert_eq!(fs::read_dir(unrelated.path())?.count(), 0);
        fs::remove_file(cache.path().join(&id))?;
        let installed = materialize(&fixture()?, &id, cache.path())?;
        assert_ne!(
            fs::metadata(installed.join("java/bin/java"))?
                .permissions()
                .mode()
                & 0o111,
            0
        );
        fs::set_permissions(
            installed.join("java/bin/java"),
            fs::Permissions::from_mode(0o644),
        )?;
        materialize(&fixture()?, &id, cache.path())?;
        assert_ne!(
            fs::metadata(installed.join("java/bin/java"))?
                .permissions()
                .mode()
                & 0o111,
            0
        );
        Ok(())
    }

    #[cfg(compose_preview_bundled)]
    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "verify the real bundled Java runtime in a synchronous test"
    )]
    fn real_embedded_runtime_runs_without_an_installer_or_local_jdk() -> Result<()> {
        let cache = tempfile::tempdir()?;
        let installation = materialize(
            include_bytes!(concat!(env!("OUT_DIR"), "/compose-preview.zip")),
            env!("KODA_COMPOSE_PREVIEW_BUNDLE_ID"),
            cache.path(),
        )?;
        let java = super::super::java_binary(&installation)?;
        let output = std::process::Command::new(&java)
            .arg("-version")
            .env_remove("JAVA_HOME")
            .output()?;
        ensure!(
            output.status.success(),
            "Bundled Java failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("21.0.12.1"));
        let model = cache.path().join("model.json");
        let output = cache.path().join("previews.json");
        fs::write(
            &model,
            serde_json::to_vec(&serde_json::json!({"classPath":[],"projectClassPath":[]}))?,
        )?;
        let result = std::process::Command::new(java)
            .args(super::super::bridge_arguments(
                &installation,
                "discover",
                &model,
                Some(&output),
            )?)
            .env_remove("JAVA_HOME")
            .env_remove("ANDROID_IDE_COMPOSE_PREVIEW")
            .output()?;
        ensure!(
            result.status.success(),
            "Bundled bridge failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<Vec<serde_json::Value>>(&fs::read(output)?)?.len(),
            0
        );
        Ok(())
    }
}
