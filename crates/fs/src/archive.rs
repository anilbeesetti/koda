use crate::{FileHandle, Fs, Watcher};
use anyhow::{Context as _, Result, ensure};
use async_zip::base::read::seek::ZipFileReader;
use futures::io::BufReader;
use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

pub(crate) struct ArchivePath {
    pub archive: PathBuf,
    pub entry: String,
}

impl ArchivePath {
    pub fn parse(path: &Path) -> Option<Self> {
        for parent in path.ancestors() {
            let Some(name) = parent.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.ends_with(".jar!") || name.ends_with(".zip!") {
                return Some(Self {
                    archive: parent.with_file_name(name.strip_suffix('!')?),
                    entry: path
                        .strip_prefix(parent)
                        .ok()?
                        .to_str()?
                        .replace(std::path::MAIN_SEPARATOR, "/"),
                });
            }
        }
        None
    }

    pub fn path_in(&self, archive: &Path) -> PathBuf {
        let mut root = archive.as_os_str().to_owned();
        root.push("!");
        PathBuf::from(root).join(&self.entry)
    }

    async fn open(&self) -> Result<ZipFileReader<BufReader<smol::fs::File>>> {
        ensure!(
            Path::new(&self.entry)
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
            "Invalid archive entry: {}",
            self.entry
        );
        let file = smol::fs::File::open(&self.archive).await?;
        Ok(ZipFileReader::new(BufReader::new(file)).await?)
    }

    pub async fn metadata(&self) -> Result<Option<(bool, u64)>> {
        let reader = self.open().await?;
        let prefix = format!("{}/", self.entry);
        for entry in reader.file().entries() {
            let name = entry.filename().as_str()?;
            if name == self.entry {
                return Ok(Some((entry.dir()?, entry.uncompressed_size())));
            }
            if self.entry.is_empty() || name.starts_with(&prefix) {
                return Ok(Some((true, 0)));
            }
        }
        Ok(self.entry.is_empty().then_some((true, 0)))
    }

    pub async fn read_dir(&self) -> Result<Vec<PathBuf>> {
        let reader = self.open().await?;
        let prefix = if self.entry.is_empty() {
            String::new()
        } else {
            format!("{}/", self.entry)
        };
        let root = self.path_in(&self.archive);
        let mut children = std::collections::BTreeSet::new();
        for entry in reader.file().entries() {
            if let Some(name) = entry
                .filename()
                .as_str()?
                .strip_prefix(&prefix)
                .and_then(|name| name.split('/').next())
                && !name.is_empty()
                && Path::new(name)
                    .components()
                    .all(|component| matches!(component, Component::Normal(_)))
                && !name.contains('\\')
            {
                children.insert(root.join(name));
            }
        }
        Ok(children.into_iter().collect())
    }

    pub async fn read(&self) -> Result<Vec<u8>> {
        let mut reader = self.open().await?;
        let index = reader
            .file()
            .entries()
            .iter()
            .position(|entry| entry.filename().as_str().ok() == Some(self.entry.as_str()))
            .with_context(|| {
                format!(
                    "Archive entry {} does not exist in {}",
                    self.entry,
                    self.archive.display()
                )
            })?;
        let mut contents = Vec::new();
        reader
            .reader_with_entry(index)
            .await?
            .read_to_end_checked(&mut contents)
            .await?;
        Ok(contents)
    }
}

pub(crate) fn ensure_writable(path: &Path) -> Result<()> {
    ensure!(
        ArchivePath::parse(path).is_none(),
        "Archive entries are read-only: {}",
        path.display()
    );
    Ok(())
}

#[derive(Debug)]
pub(crate) struct ArchiveHandle {
    pub file: Arc<dyn FileHandle>,
    pub entry: String,
}

impl FileHandle for ArchiveHandle {
    fn current_path(&self, fs: &Arc<dyn Fs>) -> Result<PathBuf> {
        let archive = self.file.current_path(fs)?;
        Ok(ArchivePath {
            archive: archive.clone(),
            entry: self.entry.clone(),
        }
        .path_in(&archive))
    }
}

pub(crate) struct ArchiveWatcher(pub Arc<dyn Watcher>);

impl Watcher for ArchiveWatcher {
    fn add(&self, path: &Path) -> Result<()> {
        self.0
            .add(&ArchivePath::parse(path).map_or_else(|| path.to_path_buf(), |path| path.archive))
    }

    fn remove(&self, path: &Path) -> Result<()> {
        self.0.remove(
            &ArchivePath::parse(path).map_or_else(|| path.to_path_buf(), |path| path.archive),
        )
    }
}
