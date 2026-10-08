use crate::{Project, WorktreeId};
use android_tools::project_context::{
    ActiveContext, ActiveContextToken, ContextSnapshot, ContextStore, DiscoveryToken, RootHandle,
};
use anyhow::{Context as _, Result};
use futures::StreamExt as _;
use gpui::{Context, Task};
use std::{path::{Component, Path, PathBuf}, sync::Arc, time::Duration};
use util::ResultExt as _;
use worktree::{PathChange, UpdatedEntriesSet};

#[derive(Clone, Debug, PartialEq, Eq)]
struct GitHead {
    directory: PathBuf,
    common_directory: PathBuf,
    head: String,
    resolved: Option<String>,
}

async fn bounded_git_file(filesystem: &Arc<dyn fs::Fs>, path: &Path, limit: u64) -> Result<Option<String>> {
    let Some(metadata) = filesystem.metadata(path).await? else { return Ok(None); };
    anyhow::ensure!(!metadata.is_dir && !metadata.is_fifo && metadata.len <= limit, "Git provenance file is unsupported or too large: {}", path.display());
    let value = filesystem.load(path).await?;
    anyhow::ensure!(value.len() as u64 <= limit, "Git provenance changed beyond its byte limit");
    Ok(Some(value))
}

fn valid_git_object(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

async fn git_head(filesystem: &Arc<dyn fs::Fs>, root: &Path) -> Result<Option<GitHead>> {
    let dot_git = root.join(".git");
    let Some(metadata) = filesystem.metadata(&dot_git).await? else { return Ok(None); };
    let directory = if metadata.is_dir { dot_git } else {
        let file = bounded_git_file(filesystem, &dot_git, 32768).await?.context("Git directory reference disappeared")?;
        let reference = file.trim().strip_prefix("gitdir: ").context("Invalid Gradle input Git directory reference")?;
        filesystem.canonicalize(&root.join(reference)).await?
    };
    let common_directory = if let Some(common) = bounded_git_file(filesystem, &directory.join("commondir"), 32768).await? {
        filesystem.canonicalize(&directory.join(common.trim())).await?
    } else { directory.clone() };
    let head = bounded_git_file(filesystem, &directory.join("HEAD"), 4096).await?.context("Gradle input Git HEAD is unavailable")?.trim().to_owned();
    let resolved = if let Some(reference) = head.strip_prefix("ref: ") {
        let reference = Path::new(reference);
        anyhow::ensure!(reference.starts_with("refs") && reference.components().all(|component| matches!(component, Component::Normal(_)))
            && !head.chars().any(|character| character.is_control()), "Unsupported Gradle input Git reference");
        let loose = bounded_git_file(filesystem, &directory.join(reference), 4096).await?;
        let loose = match loose {
            Some(value) => Some(value),
            None if common_directory != directory => bounded_git_file(filesystem, &common_directory.join(reference), 4096).await?,
            None => None,
        };
        if let Some(value) = loose { Some(value.trim().to_owned()) } else {
            let packed = bounded_git_file(filesystem, &common_directory.join("packed-refs"), 8 * 1024 * 1024).await?;
            let name = reference.to_str().context("Non-UTF-8 Git reference")?;
            packed.as_deref().and_then(|packed| packed.lines().find_map(|line| {
                let (object, candidate) = line.split_once(' ')?;
                (candidate == name).then(|| object.to_owned())
            }))
        }
    } else {
        anyhow::ensure!(valid_git_object(&head), "Unsupported detached Gradle input Git HEAD");
        Some(head.clone())
    };
    anyhow::ensure!(resolved.as_ref().is_none_or(|value| valid_git_object(value)), "Invalid Gradle input Git object");
    Ok(Some(GitHead { directory, common_directory, head, resolved }))
}

fn may_change_git_head(path: &Path, root: &Path, head: Option<&GitHead>) -> bool {
    let dot_git = root.join(".git");
    if path == dot_git || dot_git.starts_with(path) { return true; }
    let Some(head) = head else {
        return path.starts_with(dot_git) && path.file_name().is_some_and(|name| name == "HEAD" || name == "commondir" || name == "packed-refs");
    };
    [head.directory.join("HEAD"), head.directory.join("commondir"), head.common_directory.join("packed-refs")]
        .into_iter().any(|required| required == path || required.starts_with(path))
        || head.head.strip_prefix("ref: ").is_some_and(|reference| {
            [head.directory.join(reference), head.common_directory.join(reference)]
                .into_iter().any(|required| required == path || required.starts_with(path))
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
        let path = self.worktree_for_id(worktree, cx)
            .context("Project root is no longer open")?
            .read(cx).abs_path().to_path_buf();
        if let Some(handle) = self.android_context.handle(worktree.to_proto()) {
            if self.android_context.root_path(handle) == Some(path.as_path()) {
                let previous = self.android_context.token(handle);
                self.android_context.set_trusted(handle, trusted)?;
                if self.android_context.token(handle) != previous {
                    if !trusted { self.android_context_observers.remove(&handle); }
                    self.clear_android_model_for_root(&path, cx);
                    cx.emit(crate::Event::AndroidProjectContextChanged);
                    cx.notify();
                }
                return Ok(handle);
            }
            self.remove_android_context(worktree, cx);
        }
        self.android_context.add_root(worktree.to_proto(), path, trusted)
    }

    pub fn begin_android_context_import(&mut self, root: RootHandle, cx: &mut Context<Self>) -> Result<DiscoveryToken> {
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

    pub fn verify_android_context_inputs(&mut self, token: &DiscoveryToken, snapshot: &ContextSnapshot) -> Result<()> {
        self.android_context.verify_import_inputs(token, snapshot)
    }

    pub fn android_context_observes(&self, root: RootHandle, directory: &Path) -> bool {
        self.android_context_observers.get(&root).is_some_and(|observers| observers.keys().any(|observed| directory.starts_with(observed)))
    }

    pub fn observe_android_context_inputs(&mut self, root: RootHandle, token: DiscoveryToken, directories: Vec<PathBuf>, cx: &mut Context<Self>) -> Task<Result<bool>> {
        if directories.len() > 4096 { return Task::ready(Err(anyhow::anyhow!("Too many evaluated Gradle observer roots"))); }
        let needed = directories.into_iter().filter(|directory| !self.android_context_observes(root, directory)).collect::<Vec<_>>();
        let filesystem = self.fs().clone();
        cx.spawn(async move |project, cx| {
            let mut added = false;
            for directory in needed {
                let directory_for_watch = directory.clone();
                let filesystem_for_watch = filesystem.clone();
                let (mut events, watchers, mut head) = cx.background_spawn(async move {
                    let mut paths = vec![(directory_for_watch.clone(), false)];
                    let head = git_head(&filesystem_for_watch, &directory_for_watch).await?;
                    if let Some(head) = &head {
                        paths.push((head.directory.clone(), true));
                        if head.common_directory != head.directory { paths.push((head.common_directory.clone(), true)); }
                    }
                    let mut streams = Vec::new();
                    let mut watchers = Vec::new();
                    for (path, git) in paths {
                        let (events, watcher) = filesystem_for_watch.watch(&path, Duration::from_millis(100)).await;
                        watcher.add(&path).context("Establish Gradle input observer")?;
                        streams.push(events.map(move |events| (events, git)).boxed());
                        watchers.push(watcher);
                    }
                    anyhow::ensure!(git_head(&filesystem_for_watch, &directory_for_watch).await? == head, "Gradle input Git HEAD changed while establishing its observer");
                    Ok::<_, anyhow::Error>((futures::stream::select_all(streams), watchers, head))
                }).await?;
                let filesystem = filesystem.clone();
                project.update(cx, |project, cx| {
                    anyhow::ensure!(project.android_context.import_is_current(&token), "Gradle observer owner changed during installation");
                    let observer_directory = directory.clone();
                    let task = cx.spawn(async move |project, cx| {
                        let _watchers = watchers;
                        while let Some((batch, git)) = events.next().await {
                            let mut changed_head = false;
                            let mut changed_git_directory = false;
                            if batch.iter().any(|event| event.kind == Some(fs::PathEventKind::Rescan)
                                || may_change_git_head(&event.path, &observer_directory, head.as_ref())) {
                                let filesystem = filesystem.clone();
                                let directory = observer_directory.clone();
                                match cx.background_spawn(async move { git_head(&filesystem, &directory).await }).await {
                                    Ok(current) => {
                                        changed_head = current != head;
                                        changed_git_directory = current.as_ref().map(|head| (&head.directory, &head.common_directory))
                                            != head.as_ref().map(|head| (&head.directory, &head.common_directory));
                                        head = current;
                                    }
                                    Err(error) => {
                                        log::error!("Cannot verify Gradle input Git HEAD: {error:#}");
                                        changed_head = true;
                                        changed_git_directory = true;
                                    }
                                }
                            }
                            if project.update(cx, |project, cx| {
                                if changed_head { project.invalidate_android_context_handle(root, cx).log_err(); }
                                for event in batch {
                                    if !git && event.kind == Some(fs::PathEventKind::Rescan) {
                                        project.invalidate_android_context_handle(root, cx).log_err();
                                    } else if !git {
                                        project.on_owned_android_context_input_change(root, &event.path, cx);
                                    }
                                }
                            }).is_err() { break; }
                            if changed_git_directory { break; }
                        }
                        project.update(cx, |project, cx| {
                            if let Some(observers) = project.android_context_observers.get_mut(&root) {
                                observers.remove(&observer_directory);
                            }
                            if project.android_context.token(root).is_some() {
                                project.invalidate_android_context_handle(root, cx).log_err();
                            }
                        }).log_err();
                    });
                    project.android_context_observers.entry(root).or_default().insert(directory, task);
                    Ok::<_, anyhow::Error>(())
                })??;
                added = true;
            }
            Ok(added)
        })
    }

    pub fn finish_failed_android_context_import(&mut self, token: &DiscoveryToken, cx: &mut Context<Self>) -> Result<()> {
        self.android_context.finish_failed_import(token)?;
        cx.emit(crate::Event::AndroidProjectContextChanged);
        cx.notify();
        Ok(())
    }

    pub fn invalidate_android_context_for_repository(&mut self, directory: &Path, cx: &mut Context<Self>) {
        let handles = self.android_context.handles().filter(|handle| {
            self.android_context.root_path(*handle).is_some_and(|root| root.starts_with(directory) || directory.starts_with(root))
                || self.android_context.snapshot(*handle).is_some_and(|snapshot| snapshot.build_logic_directories().iter().any(|root| root.starts_with(directory) || directory.starts_with(root)))
        }).collect::<Vec<_>>();
        for handle in handles {
            self.invalidate_android_context_handle(handle, cx).log_err();
        }
    }

    fn clear_android_model_for_root(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.android_model.root() == Some(path) {
            self.invalidate_android_model(Some(path.to_path_buf()), cx);
        }
    }

    fn invalidate_android_context_handle(&mut self, handle: RootHandle, cx: &mut Context<Self>) -> Result<()> {
        let path = self.android_context.root_path(handle).map(Path::to_path_buf);
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
            let path = self.android_context.root_path(handle).map(Path::to_path_buf);
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

    pub(crate) fn invalidate_android_context_inputs(&mut self, worktree: WorktreeId, changes: &UpdatedEntriesSet, cx: &mut Context<Self>) {
        let Some(worktree) = self.worktree_for_id(worktree, cx) else { return };
        let directory = worktree.read(cx).abs_path();
        for (path, _, _) in changes.iter().filter(|(_, _, change)| *change != PathChange::Loaded) {
            let absolute = directory.join(path.as_std_path());
            self.on_android_context_input_change(&absolute, cx);
        }
    }

    fn on_android_context_input_change(&mut self, absolute: &Path, cx: &mut Context<Self>) {
        match self.android_context.observe_input_change(absolute) {
            Ok(handles) => for handle in handles { self.notify_android_context_input_invalidation(handle, cx); },
            Err(error) => log::error!("Cannot track Gradle input change: {error:#}"),
        }
    }

    fn on_owned_android_context_input_change(&mut self, root: RootHandle, absolute: &Path, cx: &mut Context<Self>) {
        match self.android_context.observe_root_input_change(root, absolute) {
            Ok(true) => self.notify_android_context_input_invalidation(root, cx),
            Ok(false) => {}
            Err(error) => log::error!("Cannot track owned Gradle input change: {error:#}"),
        }
    }

    fn notify_android_context_input_invalidation(&mut self, root: RootHandle, cx: &mut Context<Self>) {
        if let Some(path) = self.android_context.root_path(root).map(Path::to_path_buf) {
            self.clear_android_model_for_root(&path, cx);
        }
        cx.emit(crate::Event::AndroidProjectContextChanged);
        cx.notify();
    }
}
