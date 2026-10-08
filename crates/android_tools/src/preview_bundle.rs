use anyhow::Result;
#[cfg(any(compose_preview_bundled, test))]
use anyhow::{Context as _, ensure};

#[cfg(compose_preview_bundled)]
pub(super) fn installation() -> Result<std::path::PathBuf> {
    std::fs::create_dir_all(paths::data_dir())?;
    crate::kotlin::ensure_directory(&crate::managed::root())?;
    let cache = super::cache_directory();
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
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BundledFile {
    size: u64,
    sha256: String,
    mode: u32,
}

#[cfg(any(compose_preview_bundled, test))]
fn materialize(archive: &[u8], id: &str, cache: &std::path::Path) -> Result<std::path::PathBuf> {
    use fs2::FileExt as _;
    use sha2::{Digest as _, Sha256};
    use std::{
        collections::BTreeMap,
        fs,
        io::{Cursor, Read as _, Write as _},
    };
    ensure!(
        id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Invalid preview bundle identity"
    );
    fs::create_dir_all(cache)?;
    ensure!(
        fs::symlink_metadata(cache)?.is_dir()
            && !fs::symlink_metadata(cache)?.file_type().is_symlink(),
        "Compose preview cache must be a regular directory"
    );
    let lock_path = cache.join(".extract.lock");
    if let Ok(metadata) = fs::symlink_metadata(&lock_path) {
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "Invalid preview cache lock file"
        );
    }
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let lock = options.open(lock_path)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => break,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(50))
            }
            Err(error) => {
                return Err(error).context(
                    "Another Koda window is preparing Compose previews. Wait, then retry",
                );
            }
        }
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(archive))?;
    ensure!(
        archive.len() <= 100_000,
        "Preview bundle contains too many entries"
    );
    let mut manifest = Vec::new();
    archive
        .by_name(".files.json")?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut manifest)?;
    ensure!(
        manifest.len() <= 16 * 1024 * 1024,
        "Preview inventory is too large"
    );
    let files: BTreeMap<String, BundledFile> = serde_json::from_slice(&manifest)?;
    let mut total = 0u64;
    let mut hashes = BTreeMap::new();
    for (name, file) in &files {
        ensure!(
            name != ".files.json"
                && name != ".bundle-id"
                && !name.is_empty()
                && std::path::Path::new(name)
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_)))
                && !name.contains('\\'),
            "Unsafe preview inventory path"
        );
        ensure!(
            file.sha256.len() == 64
                && file.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
                && file.mode <= 0o777,
            "Invalid preview file metadata"
        );
        total = total.saturating_add(file.size);
        ensure!(
            total <= 10 * 1024 * 1024 * 1024,
            "Preview bundle exceeds 10 GiB"
        );
        hashes.insert(name.clone(), file.sha256.clone());
    }
    ensure!(
        archive.len() == files.len() + 1,
        "Preview archive inventory is incomplete"
    );
    hashes.insert(
        ".files.json".into(),
        format!("{:x}", Sha256::digest(&manifest)),
    );
    hashes.insert(
        ".bundle-id".into(),
        format!("{:x}", Sha256::digest(id.as_bytes())),
    );
    let valid = |directory: &std::path::Path| -> Result<bool> {
        if !directory.try_exists()? {
            return Ok(false);
        }
        let metadata = fs::symlink_metadata(directory)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "Invalid Compose preview runtime directory"
        );
        for (name, file) in &files {
            let metadata = match fs::symlink_metadata(directory.join(name)) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error.into()),
            };
            if !metadata.is_file() || metadata.len() != file.size {
                return Ok(false);
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                if metadata.permissions().mode() & 0o7777 != file.mode {
                    return Ok(false);
                }
            }
        }
        Ok(super::validate_installation(directory).is_ok()
            && crate::managed::validate_inventory(directory, &hashes).is_ok())
    };
    let selector = cache.join(format!("{id}.active"));
    if let Ok(metadata) = fs::symlink_metadata(&selector) {
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "Invalid preview runtime selector. Reveal managed storage and repair this file"
        );
        let selected = (|| -> Result<std::path::PathBuf> {
            ensure!(metadata.len() <= 256, "Preview selector is too large");
            let slot = fs::read_to_string(&selector)?;
            let prefix = format!("{id}-install-");
            let suffix = slot
                .strip_prefix(&prefix)
                .context("Invalid preview runtime selector")?;
            ensure!(
                suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "Invalid preview runtime slot"
            );
            Ok(cache.join(slot))
        })();
        match selected {
            Ok(directory) if valid(&directory)? => return Ok(directory),
            Ok(_) => {}
            Err(_) => {
                let backup = cache.join(format!("{id}.corrupt-{}", uuid::Uuid::new_v4().simple()));
                fs::rename(&selector, backup)
                    .context("Could not preserve the malformed preview selector")?;
            }
        }
    }
    for entry in fs::read_dir(cache)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with("extract-") {
            let metadata = entry.file_type()?;
            if metadata.is_dir() && !metadata.is_symlink() {
                fs::remove_dir_all(entry.path())?;
            }
        }
    }
    let budget_root = if cache
        .file_name()
        .is_some_and(|name| name == "compose-preview")
    {
        cache.parent().context("Preview cache has no profile")?
    } else {
        cache
    };
    crate::managed::validate_storage_budget(
        budget_root,
        total + manifest.len() as u64 + 64,
        files.len() * 2 + 4,
    )?;
    let staging = tempfile::Builder::new()
        .prefix("extract-")
        .tempdir_in(cache)?;
    for index in 0..archive.len() {
        let file = archive.by_index(index)?;
        let name = file.name().to_owned();
        let expected = if name == ".files.json" {
            manifest.len() as u64
        } else {
            files
                .get(&name)
                .context("Unknown preview archive entry")?
                .size
        };
        ensure!(
            file.size() == expected
                && file
                    .unix_mode()
                    .is_none_or(|mode| mode & 0o170000 != 0o120000),
            "Invalid preview archive entry"
        );
        let destination = staging.path().join(&name);
        fs::create_dir_all(
            destination
                .parent()
                .context("Preview entry has no parent")?,
        )?;
        let mut output = fs::File::create(&destination)?;
        ensure!(
            std::io::copy(&mut file.take(expected + 1), &mut output)? == expected,
            "Preview archive entry exceeds its inventory"
        );
        output.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(
                &destination,
                fs::Permissions::from_mode(files.get(&name).map_or(0o644, |file| file.mode)),
            )?;
        }
    }
    let mut marker = fs::File::create(staging.path().join(".bundle-id"))?;
    marker.write_all(id.as_bytes())?;
    marker.sync_all()?;
    #[cfg(unix)]
    {
        fs::File::open(staging.path())?.sync_all()?;
    }
    ensure!(
        valid(staging.path())?,
        "The extracted preview runtime failed integrity validation"
    );
    crate::managed::validate_storage_budget(budget_root, 0, 0)?;
    let slot = format!("{id}-install-{}", uuid::Uuid::new_v4().simple());
    let directory = cache.join(&slot);
    fs::rename(staging.path(), &directory)
        .context("Could not publish the bundled Compose runtime")?;
    let mut temporary = tempfile::NamedTempFile::new_in(cache)?;
    temporary.write_all(slot.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(selector)
        .map_err(|error| error.error)
        .context("Could not select the bundled Compose runtime")?;
    #[cfg(unix)]
    {
        fs::File::open(cache)?.sync_all()?;
    }
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
        ] {
            archive.start_file(
                name,
                zip::write::FileOptions::default().unix_permissions(0o755),
            )?;
            archive.write_all(contents.as_bytes())?;
            use sha2::{Digest as _, Sha256};
            sizes.insert(name, serde_json::json!({"size":contents.len(),"sha256":format!("{:x}",Sha256::digest(contents.as_bytes())),"mode":0o755}));
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
        assert_eq!(materialize(&archive, &id, cache.path())?, installed);
        let original_time = fs::metadata(installed.join("renderer.jar"))?.modified()?;
        fs::write(installed.join("renderer.jar"), "corrupte")?;
        fs::OpenOptions::new()
            .write(true)
            .open(installed.join("renderer.jar"))?
            .set_modified(original_time)?;
        let repaired = materialize(&archive, &id, cache.path())?;
        assert_ne!(repaired, installed);
        assert!(installed.is_dir());
        assert_eq!(fs::read(repaired.join("renderer.jar"))?, b"renderer");
        fs::write(repaired.join("unexpected.jar"), "extra")?;
        let repaired_again = materialize(&archive, &id, cache.path())?;
        assert_ne!(repaired_again, repaired);
        assert!(!repaired_again.join("unexpected.jar").exists());
        Ok(())
    }

    #[test]
    fn malformed_selectors_and_interrupted_extraction_recover_offline() -> Result<()> {
        let cache = tempfile::tempdir()?;
        let archive = fixture()?;
        let id = "e".repeat(64);
        let selector = cache.path().join(format!("{id}.active"));
        let staging = cache.path().join("extract-interrupted");
        fs::create_dir(&staging)?;
        fs::write(staging.join("partial"), "partial")?;
        for corrupt in [
            b"truncated".as_slice(),
            b"../outside",
            &[0xff],
            &[b'x'; 257],
        ] {
            fs::write(&selector, corrupt)?;
            let installed = materialize(&archive, &id, cache.path())?;
            assert!(installed.join("renderer.jar").is_file());
            assert!(!staging.exists());
        }
        assert_eq!(
            fs::read_dir(cache.path())?
                .collect::<std::io::Result<Vec<_>>>()?
                .into_iter()
                .filter(|entry| entry.file_name().to_string_lossy().contains(".corrupt-"))
                .count(),
            4
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn nested_links_and_permission_corruption_repair_without_touching_outside() -> Result<()> {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let cache = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let archive = fixture()?;
        let id = "f".repeat(64);
        let installed = materialize(&archive, &id, cache.path())?;
        fs::rename(
            installed.join("layoutlib"),
            outside.path().join("layoutlib"),
        )?;
        symlink(
            outside.path().join("layoutlib"),
            installed.join("layoutlib"),
        )?;
        let repaired = materialize(&archive, &id, cache.path())?;
        assert_ne!(repaired, installed);
        assert_eq!(
            fs::read(outside.path().join("layoutlib/data/framework_res.jar"))?,
            b"resources"
        );
        fs::set_permissions(
            repaired.join("renderer.jar"),
            fs::Permissions::from_mode(0o644),
        )?;
        let repaired_again = materialize(&archive, &id, cache.path())?;
        assert_ne!(repaired_again, repaired);
        assert_ne!(
            fs::metadata(repaired_again.join("renderer.jar"))?
                .permissions()
                .mode()
                & 0o111,
            0
        );
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
    fn upgrade_preserves_prior_identity_and_missing_renderer_recovers() -> Result<()> {
        let cache = tempfile::tempdir()?;
        let archive = fixture()?;
        let old_id = "1".repeat(64);
        let new_id = "2".repeat(64);
        let old = materialize(&archive, &old_id, cache.path())?;
        let new = materialize(&archive, &new_id, cache.path())?;
        assert_ne!(old, new);
        assert_eq!(materialize(&archive, &old_id, cache.path())?, old);
        fs::remove_file(new.join("renderer.jar"))?;
        let repaired = materialize(&archive, &new_id, cache.path())?;
        assert_ne!(repaired, new);
        assert!(repaired.join("renderer.jar").is_file());
        assert!(old.join("renderer.jar").is_file());
        Ok(())
    }

    #[test]
    fn failed_extraction_does_not_publish_a_cache_entry() -> Result<()> {
        let cache = tempfile::tempdir()?;
        let id = "c".repeat(64);
        assert!(materialize(b"invalid zip", &id, cache.path()).is_err());
        assert!(!cache.path().join(format!("{id}.active")).exists());
        assert!(materialize(&fixture()?, &id, cache.path())?.is_dir());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn refuses_cache_symlinks_and_preserves_file_permissions() -> Result<()> {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let cache = tempfile::tempdir()?;
        let unrelated = tempfile::tempdir()?;
        let id = "d".repeat(64);
        symlink(unrelated.path(), cache.path().join(format!("{id}.active")))?;
        assert!(materialize(&fixture()?, &id, cache.path()).is_err());
        assert_eq!(fs::read_dir(unrelated.path())?.count(), 0);
        fs::remove_file(cache.path().join(format!("{id}.active")))?;
        let installed = materialize(&fixture()?, &id, cache.path())?;
        assert_ne!(
            fs::metadata(installed.join("renderer.jar"))?
                .permissions()
                .mode()
                & 0o111,
            0
        );
        fs::set_permissions(
            installed.join("renderer.jar"),
            fs::Permissions::from_mode(0o644),
        )?;
        let repaired = materialize(&fixture()?, &id, cache.path())?;
        assert_ne!(installed, repaired);
        assert_eq!(
            fs::metadata(installed.join("renderer.jar"))?
                .permissions()
                .mode()
                & 0o111,
            0
        );
        assert_ne!(
            fs::metadata(repaired.join("renderer.jar"))?
                .permissions()
                .mode()
                & 0o111,
            0
        );
        Ok(())
    }

    #[cfg(compose_preview_bundled)]
    #[test]
    fn embedded_preview_contains_bridge_and_renderer_without_a_java_runtime() -> Result<()> {
        let archive = include_bytes!(concat!(env!("OUT_DIR"), "/compose-preview.zip"));
        let mut entries = zip::ZipArchive::new(Cursor::new(archive))?;
        for index in 0..entries.len() {
            let entry = entries.by_index(index)?;
            ensure!(
                !entry.name().starts_with("java/"),
                "Preview must not ship a Java runtime"
            );
        }
        let cache = tempfile::tempdir()?;
        let installation = materialize(
            archive,
            env!("KODA_COMPOSE_PREVIEW_BUNDLE_ID"),
            cache.path(),
        )?;
        assert!(installation.join("PreviewBridge.class").is_file());
        assert!(installation.join("renderer.jar").is_file());
        assert!(installation.join("layoutlib.jar").is_file());
        assert!(!installation.join("java").exists());
        Ok(())
    }
}
