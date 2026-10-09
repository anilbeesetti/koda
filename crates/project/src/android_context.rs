use crate::{Project, WorktreeId};
use android_tools::project_context::{
    ActiveContext, ActiveContextToken, ActiveProjectToken, ContextSnapshot, ContextStore,
    DiscoveryToken, RootHandle,
};
use anyhow::{Context as _, Result};
use futures::StreamExt as _;
use gpui::{AppContext as _, Context, Task};
use parking_lot::Mutex;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use util::ResultExt as _;
use worktree::{PathChange, UpdatedEntriesSet};

#[derive(Clone, Debug, PartialEq, Eq)]
struct GitHead {
    work_directory: PathBuf,
    directory: PathBuf,
    common_directory: PathBuf,
    head: String,
    resolved: Option<String>,
}

async fn bounded_git_file(
    filesystem: &Arc<dyn fs::Fs>,
    path: &Path,
    limit: u64,
) -> Result<Option<String>> {
    let Some(metadata) = filesystem.metadata(path).await? else {
        return Ok(None);
    };
    anyhow::ensure!(
        !metadata.is_dir && !metadata.is_fifo && metadata.len <= limit,
        "Git provenance file is unsupported or too large: {}",
        path.display()
    );
    let value = filesystem.load(path).await?;
    anyhow::ensure!(
        value.len() as u64 <= limit,
        "Git provenance changed beyond its byte limit"
    );
    Ok(Some(value))
}

fn valid_git_object(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

async fn git_head(filesystem: &Arc<dyn fs::Fs>, root: &Path) -> Result<Option<GitHead>> {
    let mut root = root.to_path_buf();
    let metadata = loop {
        if let Some(metadata) = filesystem.metadata(&root.join(".git")).await? {
            break metadata;
        }
        if !root.pop() {
            return Ok(None);
        }
    };
    let dot_git = root.join(".git");
    let directory = if metadata.is_dir {
        dot_git
    } else {
        let file = bounded_git_file(filesystem, &dot_git, 32768)
            .await?
            .context("Git directory reference disappeared")?;
        let reference = file
            .trim()
            .strip_prefix("gitdir: ")
            .context("Invalid Gradle input Git directory reference")?;
        filesystem.canonicalize(&root.join(reference)).await?
    };
    let common_directory = if let Some(common) =
        bounded_git_file(filesystem, &directory.join("commondir"), 32768).await?
    {
        filesystem
            .canonicalize(&directory.join(common.trim()))
            .await?
    } else {
        directory.clone()
    };
    let head = bounded_git_file(filesystem, &directory.join("HEAD"), 4096)
        .await?
        .context("Gradle input Git HEAD is unavailable")?
        .trim()
        .to_owned();
    let resolved = if let Some(reference) = head.strip_prefix("ref: ") {
        let reference = Path::new(reference);
        anyhow::ensure!(
            reference.starts_with("refs")
                && reference
                    .components()
                    .all(|component| matches!(component, Component::Normal(_)))
                && !head.chars().any(|character| character.is_control()),
            "Unsupported Gradle input Git reference"
        );
        let loose = bounded_git_file(filesystem, &directory.join(reference), 4096).await?;
        let loose = match loose {
            Some(value) => Some(value),
            None if common_directory != directory => {
                bounded_git_file(filesystem, &common_directory.join(reference), 4096).await?
            }
            None => None,
        };
        if let Some(value) = loose {
            Some(value.trim().to_owned())
        } else {
            let packed = bounded_git_file(
                filesystem,
                &common_directory.join("packed-refs"),
                8 * 1024 * 1024,
            )
            .await?;
            let name = reference.to_str().context("Non-UTF-8 Git reference")?;
            packed.as_deref().and_then(|packed| {
                packed.lines().find_map(|line| {
                    let (object, candidate) = line.split_once(' ')?;
                    (candidate == name).then(|| object.to_owned())
                })
            })
        }
    } else {
        anyhow::ensure!(
            valid_git_object(&head),
            "Unsupported detached Gradle input Git HEAD"
        );
        Some(head.clone())
    };
    anyhow::ensure!(
        resolved
            .as_ref()
            .is_none_or(|value| valid_git_object(value)),
        "Invalid Gradle input Git object"
    );
    Ok(Some(GitHead {
        work_directory: root,
        directory,
        common_directory,
        head,
        resolved,
    }))
}

fn register_ready(watcher: &Arc<dyn fs::Watcher>, directory: &Path) -> Result<()> {
    watcher
        .add(directory)
        .context("Establish Gradle input observer")?;
    anyhow::ensure!(
        watcher.is_watching(directory),
        "Gradle input observer is pending or unavailable: {}",
        directory.display()
    );
    Ok(())
}

struct InputScan {
    discovered: Vec<PathBuf>,
    git_roots: BTreeSet<PathBuf>,
    coverage_updates: BTreeMap<PathBuf, Option<u64>>,
}

#[derive(Clone)]
struct InputCoverage {
    root: PathBuf,
    root_inode: u64,
    watcher: Arc<dyn fs::Watcher>,
    directories: Arc<Mutex<BTreeMap<PathBuf, u64>>>,
    provenance: Arc<Mutex<Option<Arc<ContextSnapshot>>>>,
}

pub(super) struct InputObserver {
    _task: Task<()>,
    coverage: InputCoverage,
}

impl InputCoverage {
    fn nearest_observed_directory(
        &self,
        filesystem: &dyn fs::Fs,
        directory: &Path,
    ) -> Option<PathBuf> {
        if !filesystem.path_exists(&self.root) || !self.watcher.is_watching(&self.root) {
            return None;
        }
        let existing = directory
            .ancestors()
            .find(|path| filesystem.path_exists(path))?;
        (existing.starts_with(&self.root)
            && self.directories.lock().contains_key(existing)
            && self.watcher.is_watching(existing))
        .then(|| existing.to_path_buf())
    }

    async fn current(&self, filesystem: &Arc<dyn fs::Fs>, directory: &Path) -> Result<bool> {
        let root = filesystem.metadata(&self.root).await?;
        anyhow::ensure!(
            root.is_some_and(|metadata| metadata.is_dir
                && !metadata.is_symlink
                && metadata.inode == self.root_inode)
                && self.watcher.is_watching(&self.root),
            "Gradle input observation root changed: {}",
            self.root.display()
        );
        let Some(existing) = self.nearest_observed_directory(filesystem.as_ref(), directory) else {
            return Ok(false);
        };
        let expected = self.directories.lock().get(&existing).copied();
        let current = filesystem.metadata(&existing).await?;
        anyhow::ensure!(
            current.is_some_and(|metadata| metadata.is_dir
                && !metadata.is_symlink
                && Some(metadata.inode) == expected)
                && self.watcher.is_watching(&existing),
            "Gradle input directory changed while observed: {}",
            existing.display()
        );
        Ok(true)
    }
}

async fn scan_inputs(
    filesystem: &Arc<dyn fs::Fs>,
    watcher: &Arc<dyn fs::Watcher>,
    roots: Vec<PathBuf>,
    watched: &mut BTreeMap<PathBuf, u64>,
    provenance: Option<&ContextSnapshot>,
) -> Result<InputScan> {
    let mut queue = VecDeque::from(roots);
    let mut discovered = Vec::new();
    let mut git_roots = BTreeSet::new();
    let mut coverage_updates = BTreeMap::new();
    let mut entries = 0;
    while let Some(directory) = queue.pop_front() {
        if provenance.is_some_and(|snapshot| snapshot.is_generated_output(&directory)) {
            continue;
        }
        let Some(metadata) = filesystem.metadata(&directory).await? else {
            continue;
        };
        if !metadata.is_dir || metadata.is_symlink {
            continue;
        }
        if watched.get(&directory) == Some(&metadata.inode) {
            continue;
        }
        if watched.contains_key(&directory) {
            let previous = watched
                .keys()
                .filter(|path| path.starts_with(&directory))
                .cloned()
                .collect::<Vec<_>>();
            for path in previous {
                watcher.remove(&path)?;
                if watched.remove(&path).is_some() {
                    coverage_updates.insert(path, None);
                }
            }
        }
        anyhow::ensure!(
            watched.len() < 32768,
            "Gradle input tree exceeds its directory observation limit"
        );
        register_ready(watcher, &directory)?;
        watched.insert(directory.clone(), metadata.inode);
        coverage_updates.insert(directory.clone(), Some(metadata.inode));
        discovered.push(directory.clone());
        let mut children = filesystem.read_dir(&directory).await?;
        while let Some(child) = children.next().await {
            let child = child?;
            entries += 1;
            anyhow::ensure!(
                entries <= 262144,
                "Gradle input scan exceeds its entry limit"
            );
            if child.file_name().is_some_and(|name| name == ".git") {
                git_roots.insert(directory.clone());
                continue;
            }
            if child
                .file_name()
                .is_some_and(|name| name == ".gradle" || name == ".kotlin")
                || provenance.is_some_and(|snapshot| snapshot.is_generated_output(&child))
            {
                continue;
            }
            discovered.push(child.clone());
            if filesystem
                .metadata(&child)
                .await?
                .is_some_and(|metadata| metadata.is_dir && !metadata.is_symlink)
            {
                queue.push_back(child);
            }
        }
    }
    Ok(InputScan {
        discovered,
        git_roots,
        coverage_updates,
    })
}

async fn register_current_ref(
    filesystem: &Arc<dyn fs::Fs>,
    watcher: &Arc<dyn fs::Watcher>,
    directory: &Path,
    head: &GitHead,
) -> Result<()> {
    let Some(reference) = head.head.strip_prefix("ref: ") else {
        return Ok(());
    };
    let mut parent = directory
        .join(reference)
        .parent()
        .context("Git reference has no parent")?
        .to_path_buf();
    while !filesystem
        .metadata(&parent)
        .await?
        .is_some_and(|metadata| metadata.is_dir)
    {
        anyhow::ensure!(
            parent.pop() && parent.starts_with(directory),
            "Git reference parent is unavailable"
        );
    }
    register_ready(watcher, &parent)
}

fn may_change_git_head(path: &Path, root: &Path, head: Option<&GitHead>) -> bool {
    let dot_git = root.join(".git");
    if path == dot_git || dot_git.starts_with(path) {
        return true;
    }
    let Some(head) = head else {
        return path.starts_with(dot_git)
            && path.file_name().is_some_and(|name| {
                name == "HEAD" || name == "commondir" || name == "packed-refs"
            });
    };
    [
        head.directory.join("HEAD"),
        head.directory.join("commondir"),
        head.common_directory.join("packed-refs"),
    ]
    .into_iter()
    .any(|required| required == path || required.starts_with(path))
        || head.head.strip_prefix("ref: ").is_some_and(|reference| {
            [
                head.directory.join(reference),
                head.common_directory.join(reference),
            ]
            .into_iter()
            .any(|required| required == path || required.starts_with(path))
        })
}

impl Project {
    pub fn android_context(&self) -> &ContextStore {
        &self.android_context
    }

    pub fn ensure_android_context(
        &mut self,
        worktree: WorktreeId,
        trusted: bool,
        cx: &mut Context<Self>,
    ) -> Result<RootHandle> {
        let path = self
            .worktree_for_id(worktree, cx)
            .context("Project root is no longer open")?
            .read(cx)
            .abs_path()
            .to_path_buf();
        if let Some(handle) = self.android_context.handle(worktree.to_proto()) {
            if self.android_context.root_path(handle) == Some(path.as_path()) {
                let previous = self.android_context.token(handle);
                self.android_context.set_trusted(handle, trusted)?;
                if self.android_context.token(handle) != previous {
                    if !trusted {
                        self.android_context_observers.remove(&handle);
                    }
                    self.clear_android_model_for_root(&path, cx);
                    cx.emit(crate::Event::AndroidProjectContextChanged);
                    cx.notify();
                }
                return Ok(handle);
            }
            self.remove_android_context(worktree, cx);
        }
        self.android_context
            .add_root(worktree.to_proto(), path, trusted)
    }

    pub fn begin_android_context_import(
        &mut self,
        root: RootHandle,
        cx: &mut Context<Self>,
    ) -> Result<DiscoveryToken> {
        let token = self.android_context.begin_import(root)?;
        if let Some(path) = self.android_context.root_path(root).map(Path::to_path_buf) {
            self.clear_android_model_for_root(&path, cx);
        }
        cx.emit(crate::Event::AndroidProjectContextChanged);
        cx.notify();
        Ok(token)
    }

    pub fn publish_android_context(
        &mut self,
        active: &ActiveContext,
        owner: &ActiveContextToken,
        discovery: &DiscoveryToken,
        snapshot: ContextSnapshot,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        active.publish(&mut self.android_context, owner, discovery, snapshot)?;
        cx.emit(crate::Event::AndroidProjectContextChanged);
        cx.notify();
        Ok(())
    }

    pub fn verify_android_context_inputs(
        &mut self,
        token: &DiscoveryToken,
        snapshot: &ContextSnapshot,
    ) -> Result<()> {
        self.android_context.verify_import_inputs(token, snapshot)
    }

    pub fn publish_android_project_context(
        &mut self,
        active: &ActiveContext,
        owner: &ActiveProjectToken,
        discovery: &DiscoveryToken,
        snapshot: ContextSnapshot,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        active.publish_project(&mut self.android_context, owner, discovery, snapshot)?;
        cx.emit(crate::Event::AndroidProjectContextChanged);
        cx.notify();
        Ok(())
    }

    pub fn android_context_observes(&self, root: RootHandle, directory: &Path) -> bool {
        self.android_context_coverage(root, directory)
            .iter()
            .any(|coverage| {
                coverage
                    .nearest_observed_directory(self.fs().as_ref(), directory)
                    .is_some()
            })
    }

    fn android_context_coverage(&self, root: RootHandle, directory: &Path) -> Vec<InputCoverage> {
        let Some(observers) = self.android_context_observers.get(&root) else {
            return Vec::new();
        };
        directory
            .ancestors()
            .filter_map(|ancestor| {
                observers
                    .get(ancestor)
                    .map(|observer| observer.coverage.clone())
            })
            .collect()
    }

    pub fn verify_android_context_observers(
        &mut self,
        root: RootHandle,
        token: DiscoveryToken,
        directories: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        if directories.len() > 4096 {
            return Task::ready(Err(anyhow::anyhow!(
                "Too many evaluated Gradle input folders"
            )));
        }
        if token.root() != root || !self.android_context.import_is_current(&token) {
            return Task::ready(Err(anyhow::anyhow!(
                "Project changed before checking Gradle inputs"
            )));
        }
        let filesystem = self.fs().clone();
        let directories = directories
            .into_iter()
            .map(|directory| {
                let observers = self.android_context_coverage(root, &directory);
                (directory, observers)
            })
            .collect::<Vec<_>>();
        cx.spawn(async move |project, cx| {
            let result = cx
                .background_spawn(async move {
                    for (directory, observers) in directories {
                        let mut covered = false;
                        for observer in observers {
                            covered |= observer.current(&filesystem, &directory).await?;
                        }
                        anyhow::ensure!(
                            covered,
                            "Evaluated Gradle input coverage is unavailable: {}",
                            directory.display()
                        );
                    }
                    Ok::<_, anyhow::Error>(())
                })
                .await;
            project.update(cx, |project, cx| {
                anyhow::ensure!(
                    project.android_context.import_is_current(&token),
                    "Gradle observer owner changed during verification"
                );
                if result.is_err() {
                    project.android_context_observers.remove(&root);
                    project.invalidate_android_context_handle(root, cx)?;
                }
                result
            })?
        })
    }

    pub fn observe_android_context_inputs(
        &mut self,
        root: RootHandle,
        token: DiscoveryToken,
        directories: Vec<PathBuf>,
        provenance: Option<ContextSnapshot>,
        cx: &mut Context<Self>,
    ) -> Task<Result<bool>> {
        if directories.len() > 4096 {
            return Task::ready(Err(anyhow::anyhow!(
                "Too many evaluated Gradle observer roots"
            )));
        }
        if token.root() != root || !self.android_context.import_is_current(&token) {
            return Task::ready(Err(anyhow::anyhow!(
                "Project changed before Gradle input setup"
            )));
        }
        if provenance
            .as_ref()
            .is_some_and(|snapshot| self.android_context.root_path(root) != Some(snapshot.root()))
        {
            return Task::ready(Err(anyhow::anyhow!(
                "Gradle inputs belong to another project"
            )));
        }
        let provenance = provenance.map(Arc::new);
        if let Some(snapshot) = &provenance {
            if let Some(observers) = self.android_context_observers.get(&root) {
                for observer in observers.values() {
                    *observer.coverage.provenance.lock() = Some(snapshot.clone());
                }
            }
        }
        let filesystem = self.fs().clone();
        cx.spawn(async move |project, cx| {
            let mut added = false;
            for directory in directories {
                let observers = project.read_with(cx, |project, _| project.android_context_coverage(root, &directory))?;
                let filesystem_for_verify = filesystem.clone();
                let directory_for_verify = directory.clone();
                let observed = cx.background_spawn(async move {
                    for observer in observers.iter().filter(|observer| directory_for_verify.starts_with(&observer.root)) {
                        if observer.current(&filesystem_for_verify, &directory_for_verify).await? { return Ok(true); }
                    }
                    Ok::<_, anyhow::Error>(false)
                }).await;
                if let Err(error) = observed {
                    project.update(cx, |project, cx| {
                        if project.android_context.import_is_current(&token) {
                            project.android_context_observers.remove(&root);
                            project.invalidate_android_context_handle(root, cx)?;
                        }
                        Ok::<_, anyhow::Error>(())
                    })??;
                    return Err(error);
                }
                if observed? { continue; }
                let directory_for_watch = directory.clone();
                let filesystem_for_watch = filesystem.clone();
                let scan_provenance = provenance.clone();
                let (mut events, input_watcher, git_watchers, mut watched, mut heads, mut known_inputs) = cx.background_spawn(async move {
                    let (input_events, input_watcher) = filesystem_for_watch.watch(&directory_for_watch, Duration::from_millis(100)).await;
                    let mut watched = BTreeMap::new();
                    let scan = scan_inputs(&filesystem_for_watch, &input_watcher, vec![directory_for_watch.clone()], &mut watched, scan_provenance.as_deref()).await?;
                    let known_inputs = scan.discovered.iter().filter(|path| android_tools::project_context::is_context_input(path)).cloned().collect::<BTreeSet<_>>();
                    anyhow::ensure!(watched.contains_key(&directory_for_watch), "Gradle input root is not currently observable");
                    let mut git_roots = scan.git_roots;
                    git_roots.insert(directory_for_watch.clone());
                    let mut heads = BTreeMap::new();
                    for candidate in git_roots {
                        if let Some(head) = git_head(&filesystem_for_watch, &candidate).await? {
                            anyhow::ensure!(heads.len() < 256 || heads.contains_key(&head.work_directory), "Too many owning Git repositories in Gradle inputs");
                            heads.insert(head.work_directory.clone(), head);
                        }
                    }
                    let mut streams = vec![input_events.map(|events| (events, false)).boxed()];
                    let mut git_watchers = BTreeMap::new();
                    for head in heads.values() {
                        for path in [&head.directory, &head.common_directory] {
                            if git_watchers.contains_key(path) { continue; }
                            let (events, watcher) = filesystem_for_watch.watch(path, Duration::from_millis(100)).await;
                            register_ready(&watcher, path)?;
                            streams.push(events.map(|events| (events, true)).boxed());
                            git_watchers.insert(path.clone(), watcher);
                        }
                        for path in [&head.directory, &head.common_directory] {
                            let watcher = git_watchers.get(path).context("Owning Git observer disappeared")?;
                            register_current_ref(&filesystem_for_watch, watcher, path, head).await?;
                        }
                    }
                    for head in heads.values() {
                        anyhow::ensure!(git_head(&filesystem_for_watch, &head.work_directory).await?.as_ref() == Some(head), "Gradle input Git HEAD changed while establishing observers");
                    }
                    Ok::<_, anyhow::Error>((futures::stream::select_all(streams), input_watcher, git_watchers, watched, heads, known_inputs))
                }).await?;
                let filesystem = filesystem.clone();
                project.update(cx, |project, cx| {
                    anyhow::ensure!(project.android_context.import_is_current(&token), "Gradle observer owner changed during installation");
                    let observer_directory = directory.clone();
                    let coverage = InputCoverage { root: directory.clone(), root_inode: *watched.get(&directory).context("Observed Gradle root disappeared")?,
                        watcher: input_watcher.clone(), directories: Arc::new(Mutex::new(watched.clone())),
                        provenance: Arc::new(Mutex::new(provenance.clone())) };
                    let task_coverage = coverage.clone();
                    let task = cx.spawn(async move |project, cx| {
                        while let Some((batch, git)) = events.next().await {
                            let relevant = heads.values().filter(|head| batch.iter().any(|event| event.kind == Some(fs::PathEventKind::Rescan)
                                || may_change_git_head(&event.path, &head.work_directory, Some(head))))
                                .cloned().collect::<Vec<_>>();
                            let mut failed = !git && batch.iter().any(|event| event.kind == Some(fs::PathEventKind::Removed)
                                && observer_directory.starts_with(&event.path));
                            if !failed {
                                let coverage = task_coverage.clone();
                                let filesystem = filesystem.clone();
                                let directory = observer_directory.clone();
                                match cx.background_spawn(async move { coverage.current(&filesystem, &directory).await }).await {
                                    Ok(true) => {}
                                    Ok(false) => failed = true,
                                    Err(error) => { log::error!("Gradle input root is no longer current: {error:#}"); failed = true; }
                                }
                            }
                            let mut changed_head = false;
                            for previous in relevant {
                                let filesystem = filesystem.clone();
                                let previous_for_read = previous.clone();
                                let watchers = git_watchers.clone();
                                let result = cx.background_spawn(async move {
                                    let current = git_head(&filesystem, &previous_for_read.work_directory).await?;
                                    if let Some(current) = &current {
                                        if current.directory == previous_for_read.directory && current.common_directory == previous_for_read.common_directory {
                                            for directory in [&current.directory, &current.common_directory] {
                                                register_current_ref(&filesystem, watchers.get(directory).context("Git observer is unavailable")?, directory, current).await?;
                                            }
                                        }
                                    }
                                    Ok::<_, anyhow::Error>(current)
                                }).await;
                                match result {
                                    Ok(current) if current.as_ref() == Some(&previous) => {}
                                    Ok(Some(current)) if current.directory == previous.directory && current.common_directory == previous.common_directory => {
                                        heads.insert(current.work_directory.clone(), current);
                                        changed_head = true;
                                    }
                                    Ok(_) => { failed = true; break; }
                                    Err(error) => { log::error!("Cannot verify Gradle input Git HEAD: {error:#}"); failed = true; break; }
                                }
                            }
                            let mut discovered = Vec::new();
                            if !git && !failed {
                                let mut coverage_updates = BTreeMap::new();
                                let paths = batch.iter().map(|event| event.path.clone()).collect::<Vec<_>>();
                                let scan_provenance = task_coverage.provenance.lock().clone();
                                for event in batch.iter().filter(|event| event.kind == Some(fs::PathEventKind::Removed)) {
                                    let removed = watched.keys().filter(|directory| directory.starts_with(&event.path)).cloned().collect::<Vec<_>>();
                                    for directory in removed {
                                        if let Err(error) = input_watcher.remove(&directory) { log::error!("Cannot retire removed Gradle input directory: {error:#}"); failed = true; }
                                        if watched.remove(&directory).is_some() {
                                            coverage_updates.insert(directory, None);
                                        }
                                    }
                                    let removed_inputs = known_inputs.iter().filter(|path| path.starts_with(&event.path)).cloned().collect::<Vec<_>>();
                                    for path in removed_inputs { known_inputs.remove(&path); discovered.push(path); }
                                }
                                let new_git_marker = batch.iter().any(|event| event.path.file_name().is_some_and(|name| name == ".git"));
                                failed |= new_git_marker || batch.iter().any(|event| event.kind == Some(fs::PathEventKind::Rescan));
                                if !failed {
                                    let filesystem = filesystem.clone();
                                    let watcher = input_watcher.clone();
                                    let mut scanned = std::mem::take(&mut watched);
                                    match cx.background_spawn(async move {
                                        let result = scan_inputs(&filesystem, &watcher, paths, &mut scanned, scan_provenance.as_deref()).await;
                                        (scanned, result)
                                    }).await {
                                        (scanned, Ok(scan)) => {
                                            watched = scanned;
                                            coverage_updates.extend(scan.coverage_updates);
                                            if !coverage_updates.is_empty() {
                                                let mut directories = task_coverage.directories.lock();
                                                for (directory, inode) in coverage_updates {
                                                    if let Some(inode) = inode {
                                                        directories.insert(directory, inode);
                                                    } else {
                                                        directories.remove(&directory);
                                                    }
                                                }
                                            }
                                            known_inputs.extend(scan.discovered.iter().filter(|path| android_tools::project_context::is_context_input(path)).cloned());
                                            discovered.extend(scan.discovered);
                                            if scan.git_roots.iter().any(|root| !heads.contains_key(root)) { failed = true; }
                                        }
                                        (scanned, Err(error)) => {
                                            watched = scanned;
                                            log::error!("Cannot extend Gradle input observation: {error:#}");
                                            failed = true;
                                        }
                                    }
                                }
                            }
                            if failed { break; }
                            if project.update(cx, |project, cx| {
                                if changed_head { project.invalidate_android_context_handle(root, cx).log_err(); }
                                if !git {
                                    for path in discovered { project.on_owned_android_context_input_change(root, &path, cx); }
                                    for event in batch { project.on_owned_android_context_input_change(root, &event.path, cx); }
                                }
                            }).is_err() { break; }
                        }
                        task_coverage.directories.lock().clear();
                        project.update(cx, |project, cx| {
                            let owns_entry = project.android_context_observers.get(&root).and_then(|observers| observers.get(&observer_directory))
                                .is_some_and(|observer| Arc::ptr_eq(&observer.coverage.directories, &task_coverage.directories));
                            if owns_entry {
                                if let Some(observers) = project.android_context_observers.get_mut(&root) { observers.remove(&observer_directory); }
                                if project.android_context.token(root).is_some() { project.invalidate_android_context_handle(root, cx).log_err(); }
                            }
                        }).log_err();
                    });
                    project.android_context_observers.entry(root).or_default().insert(directory, InputObserver { _task: task, coverage });
                    Ok::<_, anyhow::Error>(())
                })??;
                added = true;
            }
            anyhow::ensure!(project.read_with(cx, |project, _| project.android_context.import_is_current(&token))?,
                "Project changed while setting up Gradle inputs");
            Ok(added)
        })
    }

    pub fn finish_failed_android_context_import(
        &mut self,
        token: &DiscoveryToken,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.android_context.finish_failed_import(token)?;
        cx.emit(crate::Event::AndroidProjectContextChanged);
        cx.notify();
        Ok(())
    }

    pub fn invalidate_android_context_for_repository(
        &mut self,
        directory: &Path,
        cx: &mut Context<Self>,
    ) {
        let handles =
            self.android_context
                .handles()
                .filter(|handle| {
                    self.android_context.root_path(*handle).is_some_and(|root| {
                        root.starts_with(directory) || directory.starts_with(root)
                    }) || self
                        .android_context
                        .snapshot(*handle)
                        .is_some_and(|snapshot| {
                            snapshot.build_logic_directories().iter().any(|root| {
                                root.starts_with(directory) || directory.starts_with(root)
                            }) || snapshot.modules().any(|module| {
                                module.directory().starts_with(directory)
                                    || directory.starts_with(module.directory())
                            })
                        })
                })
                .collect::<Vec<_>>();
        for handle in handles {
            self.invalidate_android_context_handle(handle, cx).log_err();
        }
    }

    fn clear_android_model_for_root(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.android_model.root() == Some(path) {
            self.invalidate_android_model(Some(path.to_path_buf()), cx);
        }
    }

    fn invalidate_android_context_handle(
        &mut self,
        handle: RootHandle,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let path = self
            .android_context
            .root_path(handle)
            .map(Path::to_path_buf);
        self.android_context.invalidate(handle)?;
        if let Some(path) = path {
            self.clear_android_model_for_root(&path, cx);
        }
        cx.emit(crate::Event::AndroidProjectContextChanged);
        cx.notify();
        Ok(())
    }

    pub(crate) fn remove_android_context(&mut self, worktree: WorktreeId, cx: &mut Context<Self>) {
        if let Some(handle) = self.android_context.handle(worktree.to_proto()) {
            let path = self
                .android_context
                .root_path(handle)
                .map(Path::to_path_buf);
            if self.android_context.remove_root(handle) {
                self.android_context_observers.remove(&handle);
                if let Some(path) = path {
                    self.clear_android_model_for_root(&path, cx);
                }
                cx.emit(crate::Event::AndroidProjectContextChanged);
                cx.notify();
            }
        }
    }

    pub(crate) fn invalidate_android_context_inputs(
        &mut self,
        worktree: WorktreeId,
        changes: &UpdatedEntriesSet,
        cx: &mut Context<Self>,
    ) {
        let Some(worktree) = self.worktree_for_id(worktree, cx) else {
            return;
        };
        let directory = worktree.read(cx).abs_path();
        for (path, _, _) in changes
            .iter()
            .filter(|(_, _, change)| *change != PathChange::Loaded)
        {
            let absolute = directory.join(path.as_std_path());
            self.on_android_context_input_change(&absolute, cx);
        }
    }

    fn on_android_context_input_change(&mut self, absolute: &Path, cx: &mut Context<Self>) {
        match self.android_context.observe_input_change(absolute) {
            Ok(handles) => {
                for handle in handles {
                    self.notify_android_context_input_invalidation(handle, cx);
                }
            }
            Err(error) => log::error!("Cannot track Gradle input change: {error:#}"),
        }
    }

    fn on_owned_android_context_input_change(
        &mut self,
        root: RootHandle,
        absolute: &Path,
        cx: &mut Context<Self>,
    ) {
        match self
            .android_context
            .observe_root_input_change(root, absolute)
        {
            Ok(true) => self.notify_android_context_input_invalidation(root, cx),
            Ok(false) => {}
            Err(error) => log::error!("Cannot track owned Gradle input change: {error:#}"),
        }
    }

    fn notify_android_context_input_invalidation(
        &mut self,
        root: RootHandle,
        cx: &mut Context<Self>,
    ) {
        if let Some(path) = self.android_context.root_path(root).map(Path::to_path_buf) {
            self.clear_android_model_for_root(&path, cx);
        }
        cx.emit(crate::Event::AndroidProjectContextChanged);
        cx.notify();
    }
}
