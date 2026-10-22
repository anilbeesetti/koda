use crate::kotlin::{atomic_write, ensure_directory};
use anyhow::{Context as _, Result, ensure};
use fs2::FileExt as _;
use sha2::{Digest as _, Sha256};
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

pub fn directory(root: &Path) -> PathBuf {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    directory_for_profile(&root, paths::data_dir())
}

fn directory_for_profile(root: &Path, profile: &Path) -> PathBuf {
    let mut digest = Sha256::new();
    for path in [profile, root] {
        let bytes = path.as_os_str().as_encoded_bytes();
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    let key = format!("{:x}", digest.finalize());
    paths::temp_dir()
        .join("projects")
        .join(key)
        .join("external_build_system")
        .join("android-v1")
}

pub(crate) fn prepare(root: &Path) -> Result<PathBuf> {
    let root = root.canonicalize()?;
    let directory = directory(&root);
    // Inspect each app-owned ancestor so a redirected cache cannot overwrite an
    // unrelated directory. The profile cache itself may be relocated by the user.
    let mut parent = paths::temp_dir().clone();
    fs::create_dir_all(&parent)?;
    ensure_directory(&parent)?;
    let relative = directory.strip_prefix(&parent)?.to_path_buf();
    for component in relative.components() {
        parent.push(component);
        ensure_cache_directory(&parent)?;
    }
    Ok(directory)
}

fn ensure_cache_directory(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => ensure_directory(path),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn prepare_area(root: &Path, name: &str, marker: &str) -> Result<PathBuf> {
    let directory = prepare(root)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("cache.lock"))?;
    lock.lock_exclusive()?;
    let area = directory.join(name);
    if area.try_exists()? {
        ensure_directory(&area)?;
        ensure!(
            fs::read_to_string(area.join(".gitignore"))? == marker,
            "The {} directory contains unmanaged files. Move it aside before configuring language support.",
            area.display()
        );
    } else {
        ensure_cache_directory(&area)?;
        atomic_write(&area.join(".gitignore"), marker, false)?;
    }
    Ok(area)
}

pub fn invalidate(root: &Path) -> Result<()> {
    // Importer snapshots can still be in use by another window. Only the complete
    // catalog is invalidated here; projections are regenerated before import.
    let directory = directory(root);
    if !directory.try_exists()? {
        return Ok(());
    }
    let path = prepare(root)?.join("project.json");
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("Could not invalidate {}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caches_are_outside_the_project_and_isolated_by_canonical_root() -> Result<()> {
        let first = tempfile::tempdir()?;
        let second = tempfile::tempdir()?;
        let cache = prepare(first.path())?;
        assert!(cache.starts_with(paths::temp_dir()));
        assert!(cache.ends_with("external_build_system/android-v1"));
        assert!(!cache.starts_with(first.path()));
        assert_ne!(cache, directory(second.path()));
        assert_ne!(
            directory_for_profile(first.path(), Path::new("/profile-one")),
            directory_for_profile(first.path(), Path::new("/profile-two"))
        );
        assert_eq!(cache, directory(&first.path().join(".")));
        #[cfg(unix)]
        {
            let alias = second.path().join("alias");
            std::os::unix::fs::symlink(first.path(), &alias)?;
            assert_eq!(cache, directory(&alias));
        }
        fs::remove_dir_all(cache)?;
        Ok(())
    }

    #[test]
    fn invalidation_keeps_importer_snapshots_used_by_other_windows() -> Result<()> {
        let root = tempfile::tempdir()?;
        let cache = prepare(root.path())?;
        ensure_directory(&cache.join("java"))?;
        for name in [
            "project.json",
            "selected.json",
            "java/model.json",
            "java/.gitignore",
            "java/import.gradle",
        ] {
            fs::write(cache.join(name), "fixture")?;
        }
        assert!(cache.join("project.json").exists());
        invalidate(root.path())?;
        invalidate(root.path())?;
        assert!(!cache.join("project.json").exists());
        assert!(cache.join("selected.json").exists());
        assert!(cache.join("java/model.json").exists());
        assert!(cache.join("java/.gitignore").exists());
        assert!(cache.join("java/import.gradle").exists());
        assert_eq!(fs::read_dir(root.path())?.count(), 0);
        fs::remove_dir_all(cache)?;
        Ok(())
    }

    #[test]
    fn simultaneous_cache_creation_is_idempotent() -> Result<()> {
        let root = tempfile::tempdir()?;
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            let threads = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        prepare_area(root.path(), "java", "generated fixture")
                    })
                })
                .collect::<Vec<_>>();
            for thread in threads {
                thread.join().expect("Cache creation thread")?;
            }
            Ok::<_, anyhow::Error>(())
        })?;
        fs::remove_dir_all(directory(root.path()))?;
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn cache_subdirectories_cannot_redirect_writes_or_invalidation() -> Result<()> {
        let root = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let cache = prepare(root.path())?;
        fs::write(outside.path().join("model.json"), "user data")?;
        std::os::unix::fs::symlink(outside.path(), cache.join("java"))?;
        assert!(ensure_cache_directory(&cache.join("java")).is_err());
        assert_eq!(
            fs::read_to_string(outside.path().join("model.json"))?,
            "user data"
        );
        fs::remove_dir_all(cache)?;
        Ok(())
    }
}
