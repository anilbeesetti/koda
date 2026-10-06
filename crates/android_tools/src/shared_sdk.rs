use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use tempfile::TempDir;

const MAX_LICENSE_BYTES: u64 = 1024 * 1024;
const MAX_PACKAGE_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const MAX_PACKAGE_FILES: usize = 100_000;
const STAGE_PREFIX: &str = ".koda-sdk-stage-";
const STAGE_BINDING: &str = ".koda-shared-destination.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Destination {
    pub path: PathBuf,
    anchor: PathBuf,
    anchor_identity: Identity,
    root_identity: Option<Identity>,
}

#[derive(Clone, Debug)]
pub(crate) struct Package {
    pub relative: PathBuf,
    pub revision: String,
}

#[derive(Clone, Debug)]
pub(crate) struct LicenseReceipt {
    pub id: String,
    pub hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    volume: u64,
    file: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StageBinding {
    destination: PathBuf,
    root: Identity,
    stage: Identity,
}

pub(crate) fn recovery_notice(path: &Path) -> Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure!(
            metadata.is_dir() && !is_link(&metadata),
            "The SDK destination must be a directory without links"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Could not inspect temporary SDK downloads"),
    }
    let directory = Directory::open(path, false)?;
    for (index, entry) in fs::read_dir(&directory.path)?.enumerate() {
        if index >= 512 {
            return Ok(Some(format!(
                "The SDK folder {} has too many entries to check all temporary downloads. After closing all Koda windows, inspect .koda-sdk-stage-* folders left by interrupted setup if you need to reclaim space. Keep installed SDK packages and licenses.",
                path.display()
            )));
        }
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(STAGE_PREFIX)
            && entry.file_type()?.is_dir()
        {
            return Ok(Some(format!(
                "Temporary SDK downloads are present in {}. Leave .koda-sdk-stage-* folders while setup is running. After closing all Koda windows, interrupted staging folders can be removed to reclaim space. Keep installed SDK packages and licenses; retry reuses completed packages.",
                path.display()
            )));
        }
    }
    Ok(None)
}

pub(crate) fn prepare_destination(path: &Path) -> Result<Destination> {
    ensure!(
        path.is_absolute(),
        "The shared Android SDK path must be absolute"
    );
    ensure!(
        path.file_name().is_some(),
        "The Android SDK cannot be a filesystem root"
    );
    for component in path.components() {
        ensure!(
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            ),
            "The shared Android SDK path must not contain . or .."
        );
    }
    let mut anchor = path.to_path_buf();
    loop {
        match fs::symlink_metadata(&anchor) {
            Ok(metadata) => {
                ensure!(
                    if is_link(&metadata) {
                        anchor != path && anchor.canonicalize()?.is_dir()
                    } else {
                        metadata.is_dir()
                    },
                    "The SDK destination must be a directory and must not itself be a link"
                );
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                anchor = anchor
                    .parent()
                    .context("The SDK path has no existing parent")?
                    .to_path_buf();
            }
            Err(error) => return Err(error).context("Could not inspect the SDK destination"),
        }
    }
    // Bind the canonical ancestor once; later operations must not resolve a
    // changed path to an unrelated SDK selected after license review.
    let canonical_anchor = anchor.canonicalize()?;
    let directory = Directory::open(&canonical_anchor, false)?;
    let anchor_identity = directory.identity()?;
    let path = canonical_anchor.join(path.strip_prefix(&anchor)?);
    let root_identity = (path == canonical_anchor).then_some(anchor_identity.clone());
    Ok(Destination {
        path,
        anchor: canonical_anchor,
        anchor_identity,
        root_identity,
    })
}

impl Destination {
    fn verify(&self) -> Result<()> {
        ensure!(
            self.path.is_absolute() && self.path.starts_with(&self.anchor),
            "The SDK destination binding is invalid"
        );
        let anchor = Directory::open(&self.anchor, false)?;
        ensure!(
            anchor.identity()? == self.anchor_identity,
            "The SDK destination's parent changed. Review setup again."
        );
        if let Some(identity) = &self.root_identity {
            ensure!(
                Directory::open(&self.path, false)?.identity()? == *identity,
                "The Android SDK directory changed. Review setup again."
            );
        }
        Ok(())
    }
}

pub(crate) fn create_stage(destination: &Destination) -> Result<TempDir> {
    destination.verify()?;
    let root = Directory::open(&destination.path, true)?;
    destination.verify()?;
    let stage = tempfile::Builder::new()
        .prefix(STAGE_PREFIX)
        .tempdir_in(&destination.path)?;
    ensure!(
        Directory::open(&destination.path, false)?.identity()? == root.identity()?,
        "The SDK destination changed while preparing downloads"
    );
    let directory = Directory::open(stage.path(), false)?;
    let binding = StageBinding {
        destination: destination.path.clone(),
        root: root.identity()?,
        stage: directory.identity()?,
    };
    let mut receipt = directory.file_options(OsStr::new(STAGE_BINDING), false, true, true)?;
    receipt.write_all(&serde_json::to_vec(&binding)?)?;
    receipt.sync_all()?;
    directory.sync()?;
    verify_root(destination, &binding.root)?;
    Ok(stage)
}

pub(crate) fn publish(
    destination: &Destination,
    staged_sdk: &Path,
    packages: &[Package],
    receipts: &[LicenseReceipt],
    cancel: &AtomicBool,
    validate_package: impl Fn(&Path, &Package) -> Result<()>,
) -> Result<()> {
    let mut published = Vec::new();
    let result = publish_inner(
        destination,
        staged_sdk,
        packages,
        receipts,
        cancel,
        &validate_package,
        &mut published,
    );
    result.with_context(|| {
        if published.is_empty() {
            "Shared SDK setup did not replace any existing packages. Retry after resolving the error.".to_owned()
        } else {
            format!(
                "Shared SDK packages already published are retained: {}. Retry will reuse complete packages; rollback never removes shared tools.",
                published.iter().map(|path: &PathBuf| path.display().to_string()).collect::<Vec<_>>().join(", ")
            )
        }
    })
}

fn publish_inner(
    destination: &Destination,
    staged_sdk: &Path,
    packages: &[Package],
    receipts: &[LicenseReceipt],
    cancel: &AtomicBool,
    validate_package: &impl Fn(&Path, &Package) -> Result<()>,
    published: &mut Vec<PathBuf>,
) -> Result<()> {
    cancelled(cancel)?;
    destination.verify()?;
    let root = Directory::open(&destination.path, false)?;
    let root_identity = root.identity()?;
    let stage = staged_sdk.parent().context("SDK staging has no parent")?;
    ensure!(
        stage.parent() == Some(destination.path.as_path())
            && stage
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(STAGE_PREFIX))
            && staged_sdk.file_name() == Some(OsStr::new("sdk")),
        "SDK packages must be extracted into Koda's private shared-volume staging directory"
    );
    let staged = Directory::open(staged_sdk, false)?;
    let stage_directory = Directory::open(stage, false)?;
    let mut binding_file =
        stage_directory.file_options(OsStr::new(STAGE_BINDING), false, false, false)?;
    validate_regular(&binding_file, 4096)?;
    let mut binding_bytes = Vec::new();
    (&mut binding_file)
        .take(4097)
        .read_to_end(&mut binding_bytes)?;
    ensure!(
        binding_bytes.len() <= 4096,
        "SDK staging binding is too large"
    );
    let binding: StageBinding = serde_json::from_slice(&binding_bytes)
        .context("SDK staging destination binding is missing or corrupt")?;
    ensure!(
        binding.destination == destination.path
            && binding.root == root_identity
            && binding.stage == stage_directory.identity()?,
        "The SDK destination or staging directory changed during download. Review setup again."
    );
    ensure!(
        root.identity()?.volume == staged.identity()?.volume,
        "Shared SDK staging must be on the SDK filesystem"
    );
    let lock = root.open_file(OsStr::new(".koda-install.lock"), false)?;
    validate_regular(&lock, MAX_LICENSE_BYTES)?;
    fs2::FileExt::try_lock_exclusive(&lock).context(
        "Another Koda window is installing into this Android SDK. Retry when it finishes.",
    )?;
    let mut unique = BTreeSet::new();
    for package in packages {
        validate_relative(&package.relative)?;
        ensure!(
            unique.insert(&package.relative),
            "The SDK plan contains a duplicate package"
        );
        ensure!(
            !package.revision.is_empty()
                && package
                    .revision
                    .chars()
                    .all(|character| character.is_ascii_digit() || character == '.'),
            "The SDK package revision is invalid"
        );
        let source = staged_sdk.join(&package.relative);
        check_package_tree(&source, cancel)?;
        validate_package(&source, package)?;
    }
    for receipt in receipts {
        ensure!(
            !receipt.id.is_empty()
                && receipt
                    .id
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric()
                        || matches!(character, '_' | '-'))
                && receipt.hash.len() == 40
                && receipt
                    .hash
                    .chars()
                    .all(|character| character.is_ascii_hexdigit()
                        && !character.is_ascii_uppercase()),
            "The explicit SDK license receipt is invalid"
        );
    }
    verify_root(destination, &root_identity)?;
    cancelled(cancel)?;
    if !receipts.is_empty() {
        let licenses = root.child(OsStr::new("licenses"), true)?;
        for receipt in receipts {
            cancelled(cancel)?;
            verify_root(destination, &root_identity)?;
            append_receipt(&licenses, receipt)?;
        }
        licenses.sync()?;
    }
    for package in packages {
        cancelled(cancel)?;
        verify_root(destination, &root_identity)?;
        let relative_parent = package
            .relative
            .parent()
            .context("SDK package has no parent")?;
        let source_parent = staged.descend(relative_parent, false)?;
        let target_parent = root.descend(relative_parent, true)?;
        let name = package
            .relative
            .file_name()
            .context("SDK package has no name")?;
        let target = destination.path.join(&package.relative);
        match move_without_replacing(&source_parent, &target_parent, name) {
            Ok(()) => {
                published.push(package.relative.clone());
                target_parent.sync()?;
                verify_root(destination, &root_identity)?;
                validate_package(&target, package).with_context(|| format!("Published SDK package {} failed validation", package.relative.display()))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                target_parent.child(name, false)?;
                validate_package(&target, package).with_context(|| format!("{} already exists but is incomplete or incompatible. Repair it in Android Studio or choose another SDK; Koda will not overwrite it.", target.display()))?;
            }
            Err(error) => return Err(error).context("The filesystem could not publish an SDK package atomically without replacing existing tools"),
        }
    }
    verify_root(destination, &root_identity)?;
    root.sync()?;
    Ok(())
}

fn cancelled(cancel: &AtomicBool) -> Result<()> {
    ensure!(
        !cancel.load(Ordering::Acquire),
        "Shared SDK setup cancelled. Any completed shared packages are retained."
    );
    Ok(())
}

fn verify_root(destination: &Destination, identity: &Identity) -> Result<()> {
    destination.verify()?;
    ensure!(
        Directory::open(&destination.path, false)?.identity()? == *identity,
        "The shared Android SDK root changed while installing. Retry setup."
    );
    Ok(())
}

fn validate_relative(path: &Path) -> Result<()> {
    let components = path
        .components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name),
            _ => bail!("SDK package paths must be relative and must not contain . or .."),
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        matches!(components.as_slice(), [name] if *name == OsStr::new("platform-tools"))
            || matches!(components.as_slice(), [category, name]
                if (*category == OsStr::new("platforms") || *category == OsStr::new("build-tools"))
                    && !name.is_empty()
                    && name.to_string_lossy().chars().all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '.'))),
        "Unsupported shared SDK package destination"
    );
    Ok(())
}

fn check_package_tree(root: &Path, cancel: &AtomicBool) -> Result<()> {
    let directory = Directory::open(root, false)?;
    let canonical = directory.path.canonicalize()?;
    let mut pending = vec![root.to_path_buf()];
    let mut count = 0usize;
    let mut bytes = 0u64;
    while let Some(path) = pending.pop() {
        cancelled(cancel)?;
        count += 1;
        ensure!(
            count <= MAX_PACKAGE_FILES,
            "SDK package contains too many files"
        );
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            ensure!(
                !fs::read_link(&path)?.is_absolute(),
                "SDK package contains an absolute link that would break after publication"
            );
            let target = path.canonicalize()?;
            ensure!(
                target.starts_with(&canonical),
                "SDK package contains a link outside its package"
            );
        } else if metadata.is_dir() {
            ensure!(!is_link(&metadata), "SDK package contains a reparse point");
            for entry in fs::read_dir(path)? {
                ensure!(
                    count + pending.len() < MAX_PACKAGE_FILES,
                    "SDK package contains too many files"
                );
                pending.push(entry?.path());
            }
        } else {
            ensure!(
                metadata.is_file() && !is_link(&metadata),
                "SDK package contains an unsupported file type"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                ensure!(
                    metadata.nlink() == 1,
                    "Staged SDK package contains a hard link"
                );
            }
            bytes = bytes
                .checked_add(metadata.len())
                .context("SDK package size overflow")?;
            ensure!(
                bytes <= MAX_PACKAGE_BYTES,
                "SDK package exceeds the unpacked size limit"
            );
        }
    }
    Ok(())
}

fn append_receipt(directory: &Directory, receipt: &LicenseReceipt) -> Result<()> {
    let mut file = directory.open_file(OsStr::new(&receipt.id), true)?;
    validate_regular(&file, MAX_LICENSE_BYTES)?;
    let mut existing = String::new();
    (&mut file)
        .take(MAX_LICENSE_BYTES + 1)
        .read_to_string(&mut existing)?;
    ensure!(
        existing.len() as u64 <= MAX_LICENSE_BYTES,
        "Existing SDK license file is too large"
    );
    if existing.lines().any(|line| line.trim() == receipt.hash) {
        return Ok(());
    }
    let record = format!(
        "{}{}\n",
        if existing.is_empty() || existing.ends_with('\n') {
            ""
        } else {
            "\n"
        },
        receipt.hash
    );
    ensure!(
        existing.len() + record.len() <= MAX_LICENSE_BYTES as usize,
        "SDK license receipt file is full"
    );
    file.write_all(record.as_bytes())?;
    file.sync_all()?;
    let current = directory.open_file(OsStr::new(&receipt.id), true)?;
    ensure!(
        file_identity(&current)? == file_identity(&file)?,
        "The SDK license file changed during acceptance. Retry setup."
    );
    validate_regular(&file, MAX_LICENSE_BYTES)?;
    Ok(())
}

fn validate_regular(file: &File, max_bytes: u64) -> Result<()> {
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && !is_link(&metadata) && metadata.len() <= max_bytes,
        "SDK lock or license file must be a bounded regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        ensure!(
            metadata.nlink() == 1,
            "SDK lock or license file must not be a hard link"
        );
    }
    #[cfg(windows)]
    ensure!(
        windows_information(file)?.links == 1,
        "SDK lock or license file must not be a hard link"
    );
    Ok(())
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    metadata.file_type().is_symlink()
}

struct Directory {
    path: PathBuf,
    file: File,
    #[cfg(windows)]
    ancestors: Vec<File>,
}

impl Directory {
    fn open(path: &Path, create: bool) -> Result<Self> {
        ensure!(path.is_absolute(), "SDK directory must be absolute");
        #[cfg(unix)]
        {
            let file = OpenOptions::new().read(true).open("/")?;
            let mut directory = Self {
                path: PathBuf::from("/"),
                file,
            };
            for component in path.components() {
                match component {
                    Component::RootDir => {}
                    Component::Normal(name) => directory = directory.child(name, create)?,
                    _ => bail!("SDK directory contains an invalid path component"),
                }
            }
            Ok(directory)
        }
        #[cfg(windows)]
        {
            let mut current = PathBuf::new();
            let mut directory = None;
            for component in path.components() {
                match component {
                    Component::Prefix(prefix) => {
                        ensure!(
                            matches!(
                                prefix.kind(),
                                std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)
                            ),
                            "Shared SDK installation requires a local Windows drive"
                        );
                        current.push(prefix.as_os_str());
                    }
                    Component::RootDir => {
                        current.push(component.as_os_str());
                        directory = Some(Self::windows_open(current.clone(), Vec::new())?);
                    }
                    Component::Normal(name) => {
                        directory = Some(
                            directory
                                .context("SDK path has no drive root")?
                                .child(name, create)?,
                        )
                    }
                    _ => bail!("SDK directory contains an invalid path component"),
                }
            }
            directory.context("SDK path has no drive root")
        }
        #[cfg(not(any(unix, windows)))]
        bail!("Shared SDK installation is unsupported on this platform")
    }

    fn identity(&self) -> Result<Identity> {
        file_identity(&self.file)
    }

    fn child(&self, name: &OsStr, create: bool) -> Result<Self> {
        ensure!(
            matches!(
                Path::new(name).components().collect::<Vec<_>>().as_slice(),
                [Component::Normal(_)]
            ),
            "SDK directory name must be a single component"
        );
        #[cfg(unix)]
        {
            use std::os::{
                fd::{AsRawFd as _, FromRawFd as _},
                unix::ffi::OsStrExt as _,
            };
            let name_bytes = std::ffi::CString::new(name.as_bytes())?;
            let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
            let mut descriptor =
                unsafe { libc::openat(self.file.as_raw_fd(), name_bytes.as_ptr(), flags) };
            if descriptor < 0
                && create
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
            {
                let result =
                    unsafe { libc::mkdirat(self.file.as_raw_fd(), name_bytes.as_ptr(), 0o755) };
                if result < 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
                {
                    return Err(std::io::Error::last_os_error())
                        .context("Could not create the shared SDK directory");
                }
                descriptor =
                    unsafe { libc::openat(self.file.as_raw_fd(), name_bytes.as_ptr(), flags) };
            }
            ensure!(
                descriptor >= 0,
                "Could not open SDK directory {} without following links: {}",
                self.path.join(name).display(),
                std::io::Error::last_os_error()
            );
            Ok(Self {
                path: self.path.join(name),
                file: unsafe { File::from_raw_fd(descriptor) },
            })
        }
        #[cfg(windows)]
        {
            let path = self.path.join(name);
            if create {
                match fs::create_dir(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => {
                        return Err(error).context("Could not create the shared SDK directory");
                    }
                }
            }
            let mut ancestors = self
                .ancestors
                .iter()
                .map(File::try_clone)
                .collect::<std::io::Result<Vec<_>>>()?;
            ancestors.push(self.file.try_clone()?);
            Self::windows_open(path, ancestors)
        }
        #[cfg(not(any(unix, windows)))]
        bail!("Shared SDK installation is unsupported on this platform")
    }

    fn descend(&self, relative: &Path, create: bool) -> Result<Self> {
        let mut directory = Directory::open(&self.path, false)?;
        ensure!(
            directory.identity()? == self.identity()?,
            "The SDK directory changed during publication"
        );
        for component in relative.components() {
            match component {
                Component::Normal(name) => directory = directory.child(name, create)?,
                _ => bail!("SDK package parent must be relative"),
            }
        }
        Ok(directory)
    }

    fn open_file(&self, name: &OsStr, append: bool) -> Result<File> {
        self.file_options(name, append, true, false)
    }

    fn file_options(
        &self,
        name: &OsStr,
        append: bool,
        create: bool,
        exclusive: bool,
    ) -> Result<File> {
        ensure!(
            matches!(
                Path::new(name).components().collect::<Vec<_>>().as_slice(),
                [Component::Normal(_)]
            ),
            "SDK file name must be a single component"
        );
        #[cfg(unix)]
        {
            use std::os::{
                fd::{AsRawFd as _, FromRawFd as _},
                unix::ffi::OsStrExt as _,
            };
            let name = std::ffi::CString::new(name.as_bytes())?;
            let flags = (if create { libc::O_RDWR } else { libc::O_RDONLY })
                | (if create { libc::O_CREAT } else { 0 })
                | (if exclusive { libc::O_EXCL } else { 0 })
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK
                | if append { libc::O_APPEND } else { 0 };
            let descriptor =
                unsafe { libc::openat(self.file.as_raw_fd(), name.as_ptr(), flags, 0o644) };
            ensure!(
                descriptor >= 0,
                "Could not open SDK lock or license file without following links: {}",
                std::io::Error::last_os_error()
            );
            Ok(unsafe { File::from_raw_fd(descriptor) })
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            Ok(OpenOptions::new()
                .read(true)
                .write(create && !append)
                .append(append)
                .create(create && !exclusive)
                .create_new(exclusive)
                .custom_flags(0x00200000)
                .share_mode(1 | 2)
                .open(self.path.join(name))?)
        }
        #[cfg(not(any(unix, windows)))]
        bail!("Shared SDK installation is unsupported on this platform")
    }

    fn sync(&self) -> Result<()> {
        #[cfg(unix)]
        self.file.sync_all()?;
        Ok(())
    }

    #[cfg(windows)]
    fn windows_open(path: PathBuf, ancestors: Vec<File>) -> Result<Self> {
        use std::os::windows::fs::OpenOptionsExt as _;
        // Holding every directory without FILE_SHARE_DELETE prevents a junction
        // or parent replacement while path-based Win32 publication is in flight.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(0x02000000 | 0x00200000)
            .share_mode(1 | 2)
            .open(&path)?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_dir() && !is_link(&metadata),
            "The SDK path must not contain a Windows reparse point"
        );
        Ok(Self {
            path,
            file,
            ancestors,
        })
    }
}

pub(crate) fn same_filesystem(left: &Path, right: &Path) -> Result<bool> {
    Ok(Directory::open(&left.canonicalize()?, false)?
        .identity()?
        .volume
        == Directory::open(&right.canonicalize()?, false)?
            .identity()?
            .volume)
}

fn file_identity(file: &File) -> Result<Identity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = file.metadata()?;
        Ok(Identity {
            volume: metadata.dev(),
            file: metadata.ino(),
        })
    }
    #[cfg(windows)]
    {
        let information = windows_information(file)?;
        Ok(Identity {
            volume: information.volume as u64,
            file: (information.index_high as u64) << 32 | information.index_low as u64,
        })
    }
    #[cfg(not(any(unix, windows)))]
    bail!("SDK directory identity is unsupported on this platform")
}

fn move_without_replacing(
    source: &Directory,
    destination: &Directory,
    name: &OsStr,
) -> std::io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::{fd::AsRawFd as _, unix::ffi::OsStrExt as _};
        let name = std::ffi::CString::new(name.as_bytes())?;
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                source.file.as_raw_fd(),
                name.as_ptr(),
                destination.file.as_raw_fd(),
                name.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(target_os = "macos")]
        let result = unsafe {
            libc::renameatx_np(
                source.file.as_raw_fd(),
                name.as_ptr(),
                destination.file.as_raw_fd(),
                name.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
        }
        let source = source
            .path
            .join(name)
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let destination = destination
            .path
            .join(name)
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        // Never enable REPLACE_EXISTING or COPY_ALLOWED: publication must be a
        // same-volume move that fails atomically when Studio won the race.
        if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 8) } != 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Atomic no-replace SDK publication is unsupported on this platform",
    ))
}

#[cfg(windows)]
#[repr(C)]
struct WindowsInformation {
    attributes: u32,
    creation_time: [u32; 2],
    access_time: [u32; 2],
    write_time: [u32; 2],
    volume: u32,
    size_high: u32,
    size_low: u32,
    links: u32,
    index_high: u32,
    index_low: u32,
}

#[cfg(windows)]
fn windows_information(file: &File) -> Result<WindowsInformation> {
    use std::os::windows::io::AsRawHandle as _;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(
            handle: *mut std::ffi::c_void,
            information: *mut WindowsInformation,
        ) -> i32;
    }
    let mut information = std::mem::MaybeUninit::<WindowsInformation>::uninit();
    ensure!(
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) } != 0,
        "Could not identify SDK filesystem object: {}",
        std::io::Error::last_os_error()
    );
    Ok(unsafe { information.assume_init() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Result<(TempDir, Destination)> {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().canonicalize()?.join("Android/Sdk");
        Ok((temporary, prepare_destination(&path)?))
    }

    fn stage_package(stage: &Path, relative: &str, revision: &str) -> Result<Package> {
        let path = stage.join("sdk").join(relative);
        fs::create_dir_all(&path)?;
        fs::write(path.join("revision"), revision)?;
        Ok(Package {
            relative: relative.into(),
            revision: revision.into(),
        })
    }

    fn validate(path: &Path, package: &Package) -> Result<()> {
        ensure!(
            fs::read_to_string(path.join("revision"))? == package.revision,
            "SDK revision mismatch"
        );
        Ok(())
    }

    #[test]
    fn missing_destination_is_read_only_until_staging_and_publishes_additively() -> Result<()> {
        let (_temporary, destination) = fixture()?;
        assert!(!destination.path.exists());
        let stage = create_stage(&destination)?;
        let package = stage_package(stage.path(), "platforms/android-36", "2")?;
        publish(
            &destination,
            &stage.path().join("sdk"),
            &[package],
            &[],
            &AtomicBool::new(false),
            validate,
        )?;
        assert_eq!(
            fs::read_to_string(destination.path.join("platforms/android-36/revision"))?,
            "2"
        );
        assert!(!destination.path.join("licenses").exists());
        Ok(())
    }

    #[test]
    fn interrupted_stage_guidance_preserves_downloads_and_shared_packages() -> Result<()> {
        let (_temporary, destination) = fixture()?;
        assert!(recovery_notice(&destination.path)?.is_none());
        assert!(!destination.path.exists());
        let stage = create_stage(&destination)?;
        stage_package(stage.path(), "platform-tools", "37.0.1")?;
        let installed = destination.path.join("platforms/android-36/android.jar");
        fs::create_dir_all(installed.parent().context("Platform parent")?)?;
        fs::write(&installed, b"Studio-owned platform")?;
        let notice = recovery_notice(&destination.path)?.context("Staging guidance")?;
        assert!(notice.contains("After closing all Koda windows"));
        assert!(stage.path().join("sdk/platform-tools/revision").is_file());
        assert_eq!(fs::read(&installed)?, b"Studio-owned platform");
        drop(stage);
        assert!(recovery_notice(&destination.path)?.is_none());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn absolute_internal_links_are_rejected_before_package_publication() -> Result<()> {
        let (_temporary, destination) = fixture()?;
        let stage = create_stage(&destination)?;
        let package = stage_package(stage.path(), "platform-tools", "37.0.1")?;
        let staged_package = stage.path().join("sdk/platform-tools");
        std::os::unix::fs::symlink(
            staged_package.join("revision"),
            staged_package.join("alias"),
        )?;
        let error = publish(
            &destination,
            &stage.path().join("sdk"),
            &[package],
            &[],
            &AtomicBool::new(false),
            validate,
        )
        .expect_err("Absolute links break when a staged package moves");
        assert!(format!("{error:#}").contains("absolute link"));
        assert!(!destination.path.join("platform-tools").exists());
        assert!(staged_package.join("revision").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn ancestor_alias_is_bound_canonically_but_linked_sdk_roots_are_rejected() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let parent = temporary.path().canonicalize()?;
        let original = parent.join("original");
        let replacement = parent.join("replacement");
        fs::create_dir(&original)?;
        fs::create_dir(&replacement)?;
        let alias = parent.join("system-alias");
        std::os::unix::fs::symlink(&original, &alias)?;
        let destination = prepare_destination(&alias.join("Android/Sdk"))?;
        assert_eq!(destination.path, original.join("Android/Sdk"));
        fs::remove_file(&alias)?;
        std::os::unix::fs::symlink(&replacement, &alias)?;
        let stage = create_stage(&destination)?;
        assert!(stage.path().starts_with(&original));
        assert!(!replacement.join("Android/Sdk").exists());
        let linked_sdk = parent.join("linked-sdk");
        std::os::unix::fs::symlink(&destination.path, &linked_sdk)?;
        assert!(prepare_destination(&linked_sdk).is_err());
        Ok(())
    }

    #[test]
    fn existing_complete_packages_win_and_are_never_replaced() -> Result<()> {
        let (_temporary, destination) = fixture()?;
        let stage = create_stage(&destination)?;
        let package = stage_package(stage.path(), "platform-tools", "37.0.1")?;
        let existing = destination.path.join("platform-tools");
        fs::create_dir(&existing)?;
        fs::write(existing.join("revision"), "37.0.1")?;
        fs::write(existing.join("studio-owned"), "keep")?;
        publish(
            &destination,
            &stage.path().join("sdk"),
            &[package],
            &[],
            &AtomicBool::new(false),
            validate,
        )?;
        assert_eq!(fs::read_to_string(existing.join("studio-owned"))?, "keep");
        assert!(stage.path().join("sdk/platform-tools/revision").exists());
        Ok(())
    }

    #[test]
    fn incompatible_collision_retains_preceding_commit_and_other_packages() -> Result<()> {
        let (_temporary, destination) = fixture()?;
        let stage = create_stage(&destination)?;
        let platform = stage_package(stage.path(), "platforms/android-36", "2")?;
        let tools = stage_package(stage.path(), "build-tools/36.0.0", "36.0.0")?;
        let existing = destination.path.join("build-tools/36.0.0");
        fs::create_dir_all(&existing)?;
        fs::write(existing.join("revision"), "broken")?;
        let result = publish(
            &destination,
            &stage.path().join("sdk"),
            &[platform, tools],
            &[],
            &AtomicBool::new(false),
            validate,
        );
        assert!(
            format!("{:#}", result.expect_err("incompatible package must fail"))
                .contains("already published are retained")
        );
        assert!(
            destination
                .path
                .join("platforms/android-36/revision")
                .exists()
        );
        assert_eq!(fs::read_to_string(existing.join("revision"))?, "broken");
        Ok(())
    }

    #[test]
    fn explicit_receipts_append_without_rewriting_existing_text() -> Result<()> {
        let (_temporary, destination) = fixture()?;
        let stage = create_stage(&destination)?;
        let package = stage_package(stage.path(), "platform-tools", "37.0.1")?;
        let path = destination.path.join("licenses/android-sdk-license");
        fs::create_dir_all(path.parent().context("license parent")?)?;
        fs::write(&path, "existing Studio receipt")?;
        let receipt = LicenseReceipt {
            id: "android-sdk-license".into(),
            hash: "0123456789abcdef0123456789abcdef01234567".into(),
        };
        publish(
            &destination,
            &stage.path().join("sdk"),
            &[package],
            &[receipt],
            &AtomicBool::new(false),
            validate,
        )?;
        assert_eq!(
            fs::read_to_string(path)?,
            "existing Studio receipt\n0123456789abcdef0123456789abcdef01234567\n"
        );
        Ok(())
    }

    #[test]
    fn cancellation_before_publication_writes_no_receipts_or_packages() -> Result<()> {
        let (_temporary, destination) = fixture()?;
        let stage = create_stage(&destination)?;
        let package = stage_package(stage.path(), "platform-tools", "37.0.1")?;
        assert!(
            publish(
                &destination,
                &stage.path().join("sdk"),
                &[package],
                &[],
                &AtomicBool::new(true),
                validate
            )
            .is_err()
        );
        assert!(!destination.path.join("platform-tools").exists());
        assert!(!destination.path.join("licenses").exists());
        Ok(())
    }

    #[test]
    fn shared_root_lock_coordinates_independent_profile_publishers() -> Result<()> {
        let (_temporary, destination) = fixture()?;
        let stage = create_stage(&destination)?;
        let package = stage_package(stage.path(), "platform-tools", "37.0.1")?;
        let root = Directory::open(&destination.path, false)?;
        let lock = root.open_file(OsStr::new(".koda-install.lock"), false)?;
        fs2::FileExt::try_lock_exclusive(&lock)?;
        assert!(
            publish(
                &destination,
                &stage.path().join("sdk"),
                &[package],
                &[],
                &AtomicBool::new(false),
                validate
            )
            .is_err()
        );
        assert!(!destination.path.join("platform-tools").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn replaced_root_or_symlink_package_and_license_are_rejected() -> Result<()> {
        use std::os::unix::fs::symlink;
        let (temporary, initial) = fixture()?;
        fs::create_dir_all(&initial.path)?;
        let destination = prepare_destination(&initial.path)?;
        let stage = create_stage(&destination)?;
        let package = stage_package(stage.path(), "platform-tools", "37.0.1")?;
        let outside = temporary.path().canonicalize()?.join("outside");
        fs::create_dir(&outside)?;
        symlink(&outside, destination.path.join("platform-tools"))?;
        assert!(
            publish(
                &destination,
                &stage.path().join("sdk"),
                std::slice::from_ref(&package),
                &[],
                &AtomicBool::new(false),
                validate
            )
            .is_err()
        );
        fs::remove_file(destination.path.join("platform-tools"))?;
        fs::create_dir(destination.path.join("licenses"))?;
        let receipt_file = outside.join("receipt");
        fs::write(&receipt_file, "preserve")?;
        symlink(
            &receipt_file,
            destination.path.join("licenses/android-sdk-license"),
        )?;
        let receipt = LicenseReceipt {
            id: "android-sdk-license".into(),
            hash: "0123456789abcdef0123456789abcdef01234567".into(),
        };
        assert!(
            publish(
                &destination,
                &stage.path().join("sdk"),
                &[package],
                &[receipt],
                &AtomicBool::new(false),
                validate
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(receipt_file)?, "preserve");
        fs::rename(
            &destination.path,
            destination.path.with_file_name("old-sdk"),
        )?;
        fs::create_dir(&destination.path)?;
        assert!(create_stage(&destination).is_err());
        Ok(())
    }

    #[test]
    fn malformed_package_paths_and_receipts_fail_before_publication() -> Result<()> {
        for path in [
            "../platform-tools",
            "/platform-tools",
            "platforms/../platform-tools",
            "licenses/android-sdk-license",
            "platforms/android-36/subdir",
        ] {
            assert!(validate_relative(Path::new(path)).is_err(), "{path}");
        }
        let (_temporary, destination) = fixture()?;
        let stage = create_stage(&destination)?;
        let package = stage_package(stage.path(), "platform-tools", "37.0.1")?;
        let receipt = LicenseReceipt {
            id: "../license".into(),
            hash: "short".into(),
        };
        assert!(
            publish(
                &destination,
                &stage.path().join("sdk"),
                &[package],
                &[receipt],
                &AtomicBool::new(false),
                validate
            )
            .is_err()
        );
        assert!(!destination.path.join("platform-tools").exists());
        Ok(())
    }

    #[test]
    fn external_package_created_during_validation_wins_without_replacement() -> Result<()> {
        let (_temporary, destination) = fixture()?;
        let stage = create_stage(&destination)?;
        let package = stage_package(stage.path(), "platform-tools", "37.0.1")?;
        let created = AtomicBool::new(false);
        let target = destination.path.join("platform-tools");
        publish(
            &destination,
            &stage.path().join("sdk"),
            &[package],
            &[],
            &AtomicBool::new(false),
            |path, package| {
                validate(path, package)?;
                if !created.swap(true, Ordering::AcqRel) {
                    fs::create_dir(&target)?;
                    fs::write(target.join("revision"), "37.0.1")?;
                    fs::write(target.join("studio-owned"), "keep")?;
                }
                Ok(())
            },
        )?;
        assert_eq!(fs::read_to_string(target.join("studio-owned"))?, "keep");
        assert!(stage.path().join("sdk/platform-tools/revision").exists());
        Ok(())
    }

    #[test]
    fn cancellation_after_first_commit_preserves_that_package() -> Result<()> {
        let (_temporary, destination) = fixture()?;
        let stage = create_stage(&destination)?;
        let first = stage_package(stage.path(), "platform-tools", "37.0.1")?;
        let second = stage_package(stage.path(), "platforms/android-36", "2")?;
        let cancel = AtomicBool::new(false);
        let result = publish(
            &destination,
            &stage.path().join("sdk"),
            &[first, second],
            &[],
            &cancel,
            |path, package| {
                validate(path, package)?;
                if path == destination.path.join("platform-tools") {
                    cancel.store(true, Ordering::Release);
                }
                Ok(())
            },
        );
        assert!(
            format!("{:#}", result.expect_err("cancelled setup must fail"))
                .contains("already published are retained")
        );
        assert!(destination.path.join("platform-tools/revision").exists());
        assert!(!destination.path.join("platforms/android-36").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn missing_root_binding_rejects_a_stage_moved_into_a_replacement_root() -> Result<()> {
        let (_temporary, destination) = fixture()?;
        let stage = create_stage(&destination)?;
        let package = stage_package(stage.path(), "platform-tools", "37.0.1")?;
        let stage_name = stage
            .path()
            .file_name()
            .context("stage name")?
            .to_os_string();
        let previous_root = destination.path.with_file_name("old-sdk");
        fs::rename(&destination.path, &previous_root)?;
        fs::create_dir(&destination.path)?;
        fs::rename(previous_root.join(stage_name), stage.path())?;
        assert!(
            publish(
                &destination,
                &stage.path().join("sdk"),
                &[package],
                &[],
                &AtomicBool::new(false),
                validate,
            )
            .is_err()
        );
        assert!(!destination.path.join("platform-tools").exists());
        Ok(())
    }
}
