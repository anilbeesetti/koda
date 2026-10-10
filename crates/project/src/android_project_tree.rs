use crate::{
    Project, ProjectPath,
    trusted_worktrees::{PathTrust, TrustedWorktrees, TrustedWorktreesEvent},
};
use android_tools::{
    java_class_facts::MAX_JAVA_FACT_BYTES,
    project_context::{ObservationPhase, RootHandle, RootToken},
    project_model::{ModelToken, ProjectModel, SelectedProject},
    project_tree::SourceGroup,
    project_tree_adapter::{
        AdaptedModuleTree, CaptureBinding, CapturedEntry, CapturedEntryKind, CapturedJavaSource,
        CapturedModuleFiles, MAX_PARSED_JAVA_BYTES, ModuleRootPlan, adapt_captured_module,
    },
    project_tree_facts::RootPresence,
};
use anyhow::{Context as _, Result, ensure};
use fs::{Fs, MTime, Metadata};
use futures::{
    StreamExt as _,
    channel::oneshot,
    future::{Either, select},
};
use gpui::{
    App, AppContext as _, AsyncApp, BackgroundExecutor, Context, Entity, EntityId, Subscription,
    Task, WeakEntity,
};
use parking_lot::Mutex;
use postage::stream::Stream as _;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read as _,
    mem::ManuallyDrop,
    ops::Deref,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use util::{paths::PathStyle, rel_path::RelPath};
use worktree::{Entry, EntryKind, Snapshot, Worktree, WorktreeId};

const MAX_CAPTURE_ROOTS: usize = 4096;
const MAX_CAPTURE_ENTRIES: usize = 100_000;
const MAX_EXPANSION_DIRECTORIES: usize = 4096;
const MAX_CAPTURE_DURATION: Duration = Duration::from_secs(30);
static NEXT_FILE_REVISION: AtomicU64 = AtomicU64::new(1);

/// The caller validates its evaluated module plan before creating the request,
/// and retains that plan's original selection token. Each window owns its task.
#[derive(Clone)]
pub struct AndroidTreeCaptureRequest {
    root: RootHandle,
    context_token: RootToken,
    model_token: ModelToken,
    plan: Arc<ModuleRootPlan>,
    owner_generation: u64,
    request_generation: u64,
    cancelled: Arc<AtomicBool>,
    cancellation_waiters: Arc<Mutex<Vec<oneshot::Sender<()>>>>,
    #[cfg(test)]
    gate: Option<Arc<TestGate>>,
}

impl AndroidTreeCaptureRequest {
    pub fn new(
        root: RootHandle,
        context_token: RootToken,
        model_token: ModelToken,
        plan: Arc<ModuleRootPlan>,
        owner_generation: u64,
        request_generation: u64,
    ) -> Self {
        Self {
            root,
            context_token,
            model_token,
            plan,
            owner_generation,
            request_generation,
            cancelled: Arc::new(AtomicBool::new(false)),
            cancellation_waiters: Arc::new(Mutex::new(Vec::new())),
            #[cfg(test)]
            gate: None,
        }
    }

    /// Also cancels clones retained by an in-flight capture or its result.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        for waiter in self.cancellation_waiters.lock().drain(..) {
            // A completed/dropped task has already released its receiver.
            match waiter.send(()) {
                Ok(()) | Err(()) => {}
            }
        }
    }

    fn cancellation_receiver(&self) -> oneshot::Receiver<()> {
        let (sender, receiver) = oneshot::channel();
        let mut waiters = self.cancellation_waiters.lock();
        if self.cancelled.load(Ordering::Acquire) {
            match sender.send(()) {
                Ok(()) | Err(()) => {}
            }
        } else {
            waiters.push(sender);
        }
        receiver
    }

    fn check_cancelled(&self) -> Result<()> {
        ensure!(
            !self.cancelled.load(Ordering::Acquire),
            "Android tree capture was cancelled"
        );
        Ok(())
    }
}

#[derive(Clone)]
struct CaptureOwner {
    project: EntityId,
    model: Arc<ProjectModel>,
    selected: Arc<SelectedProject>,
    root_path: PathBuf,
    module_directory: PathBuf,
    trust_store: Option<EntityId>,
}

#[derive(Clone)]
struct WorktreeScope {
    worktree: Entity<Worktree>,
    refresh: BTreeSet<Arc<RelPath>>,
    recursive: BTreeSet<Arc<RelPath>>,
    inventory: BTreeSet<Arc<RelPath>>,
}

#[derive(Clone)]
struct WorktreeCapture {
    scope: WorktreeScope,
    id: WorktreeId,
    scan_id: usize,
    completed_scan_id: usize,
    snapshot: Snapshot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileVersion {
    inode: u64,
    mtime: MTime,
    len: u64,
    directory: bool,
    fifo: bool,
    symlink: bool,
}

impl From<Metadata> for FileVersion {
    fn from(metadata: Metadata) -> Self {
        Self {
            inode: metadata.inode,
            mtime: metadata.mtime,
            len: metadata.len,
            directory: metadata.is_dir,
            fifo: metadata.is_fifo,
            symlink: metadata.is_symlink,
        }
    }
}

type PresenceVersions = BTreeMap<PathBuf, Option<FileVersion>>;
type PhysicalEntries = BTreeMap<PathBuf, (ProjectPath, Entry)>;
type JavaSources = BTreeMap<PathBuf, Arc<[u8]>>;
type SharedEntries = Arc<BackgroundDrop<PhysicalEntries>>;
type SharedJavaSources = Arc<BackgroundDrop<JavaSources>>;

/// Pure capture data can contain many allocations. Its final owner may be a
/// cancelled foreground future or a replaced panel result, so release the
/// allocation on the background executor regardless of that owner's lifetime.
/// GPUI entities and subscriptions remain outside this wrapper.
struct BackgroundDrop<T: Send + 'static> {
    value: ManuallyDrop<T>,
    executor: BackgroundExecutor,
}

impl<T: Send + 'static> BackgroundDrop<T> {
    fn new(value: T, executor: BackgroundExecutor) -> Self {
        Self {
            value: ManuallyDrop::new(value),
            executor,
        }
    }
}

impl<T: Send + 'static> Deref for BackgroundDrop<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T: Send + 'static> Drop for BackgroundDrop<T> {
    fn drop(&mut self) {
        // SAFETY: value is initialized exactly once by new, is private, and is
        // taken only during this unique Drop call. ManuallyDrop prevents a
        // second automatic drop, and borrowed access cannot outlive self.
        let value = unsafe { ManuallyDrop::take(&mut self.value) };
        if self.executor.is_main_thread() {
            self.executor.spawn(async move { drop(value) }).detach();
        } else {
            drop(value);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AndroidTreeReadLimit {
    JavaFileTooLarge(PathBuf),
    JavaCaptureBudget(PathBuf),
}

/// Immutable projection and the actual physical handles/revisions it used.
/// It has not been installed in any panel. Consumers check their window's owner
/// and request generations as well as `is_android_tree_capture_current`.
pub struct CapturedAndroidModuleTree {
    request: AndroidTreeCaptureRequest,
    owner: CaptureOwner,
    worktrees: Vec<WorktreeCapture>,
    entries: SharedEntries,
    java_sources: SharedJavaSources,
    tree: BackgroundDrop<AdaptedModuleTree>,
    read_limits: BackgroundDrop<Vec<AndroidTreeReadLimit>>,
    _trust_observer: Option<Subscription>,
}

impl CapturedAndroidModuleTree {
    pub fn tree(&self) -> &AdaptedModuleTree {
        &self.tree
    }

    pub fn owner_generation(&self) -> u64 {
        self.request.owner_generation
    }

    pub fn request_generation(&self) -> u64 {
        self.request.request_generation
    }

    /// Raw bytes, so a navigation consumer can map/revalidate parser offsets
    /// against the opened editor buffer's encoding and line endings.
    pub fn java_source(&self, path: &Path) -> Option<&Arc<[u8]>> {
        self.java_sources.get(path)
    }

    pub fn read_limits(&self) -> &[AndroidTreeReadLimit] {
        &self.read_limits
    }
}

impl Project {
    pub fn capture_android_module_tree(
        &mut self,
        request: AndroidTreeCaptureRequest,
        cx: &mut Context<Self>,
    ) -> Task<Result<CapturedAndroidModuleTree>> {
        let owner = match self.android_tree_capture_owner(&request, cx.entity_id(), cx) {
            Ok(owner) => owner,
            Err(error) => return Task::ready(Err(error)),
        };
        let filesystem = self.fs().clone();
        let worktree_store = self.worktree_store().downgrade();
        let trust_observer = TrustedWorktrees::try_get_global(cx).map(|trusted| {
            cx.subscribe(&trusted, {
                let request = request.clone();
                let root_path = owner.root_path.clone();
                let module_directory = owner.module_directory.clone();
                move |project, _, event, cx| {
                    let (store, paths) = match event {
                        TrustedWorktreesEvent::Trusted(store, paths)
                        | TrustedWorktreesEvent::Restricted(store, paths) => (store, paths),
                    };
                    if store != &worktree_store {
                        return;
                    }
                    if paths.iter().any(|path| {
                        let changed = match path {
                            PathTrust::Worktree(id) => project
                                .worktree_for_id(*id, cx)
                                .map(|tree| tree.read(cx).abs_path()),
                            PathTrust::AbsPath(path) => Some(Arc::from(path.as_path())),
                        };
                        changed.is_some_and(|changed| {
                            root_path.starts_with(changed.as_ref())
                                || module_directory.starts_with(changed.as_ref())
                                || changed.starts_with(&module_directory)
                                || request.plan.source_roots().iter().any(|root| {
                                    root.path.starts_with(changed.as_ref())
                                        || changed.starts_with(&root.path)
                                })
                        })
                    }) {
                        // This permanent cancellation survives revoke→restore,
                        // including after a result has reached its owning window.
                        request.cancel();
                    }
                }
            })
        });
        let owner_observer = cx.observe_self({
            let owner = owner.clone();
            let request = request.clone();
            move |project, cx| {
                if !project.android_tree_owner_is_current(&owner, &request, cx) {
                    request.cancel();
                }
            }
        });
        let cancellation = request.cancellation_receiver();
        let deadline = cx.background_executor().timer(MAX_CAPTURE_DURATION);
        let release_executor = cx.background_executor().clone();
        let stop_request = request.clone();
        cx.spawn(async move |project, cx| {
            let capture = async move {
                let _owner_observer = owner_observer;
                let mut presence_paths = request.plan.required_presence_paths();
                let special_file = owner.module_directory.join("google-services.json");
                presence_paths.insert(special_file.clone());
                ensure!(
                    presence_paths.len() <= MAX_CAPTURE_ROOTS,
                    "Android tree has too many roots"
                );
                let initial_presence = cx
                    .background_spawn({
                        let filesystem = filesystem.clone();
                        let paths = presence_paths.clone();
                        let request = request.clone();
                        async move { probe_paths(&filesystem, &paths, &request).await }
                    })
                    .await?;
                check_owner(&project, &owner, &request, cx)?;

                let scopes = resolve_worktrees(
                    &project,
                    &owner,
                    &request,
                    &initial_presence,
                    &special_file,
                    cx,
                )
                .await?;
                #[cfg(test)]
                wait_at_test_gate(&request, TestPhase::BeforeScan).await?;
                let scanned =
                    scan_worktrees(&project, &owner, &request, &scopes, &filesystem, true, cx)
                        .await?;
                #[cfg(test)]
                wait_at_test_gate(&request, TestPhase::Scanned).await?;
                check_owner(&project, &owner, &request, cx)?;

                let entries = cx
                    .background_spawn({
                        let scanned = scanned.clone();
                        let request = request.clone();
                        let release_executor = release_executor.clone();
                        async move {
                            collect_entries(&scanned, &request).map(|entries| {
                                Arc::new(BackgroundDrop::new(entries, release_executor))
                            })
                        }
                    })
                    .await?;
                let (java_sources, read_limits) = cx
                    .background_spawn({
                        let filesystem = filesystem.clone();
                        let entries = entries.clone();
                        let request = request.clone();
                        let release_executor = release_executor.clone();
                        async move {
                            read_java_sources(&filesystem, &entries, &request, &release_executor)
                                .await
                        }
                    })
                    .await?;
                check_owner(&project, &owner, &request, cx)?;

                // Byte reads are not part of WorktreeSnapshot. Reconcile both the
                // inventory and file versions after a second actual refresh.
                let worktrees =
                    scan_worktrees(&project, &owner, &request, &scopes, &filesystem, false, cx)
                        .await?;
                let final_entries = cx
                    .background_spawn({
                        let worktrees = worktrees.clone();
                        let request = request.clone();
                        let initial_entries = entries;
                        let release_executor = release_executor.clone();
                        async move {
                            let final_entries = collect_entries(&worktrees, &request)?;
                            ensure!(
                                initial_entries.as_ref().deref() == &final_entries,
                                "Android tree files changed during capture"
                            );
                            Ok::<_, anyhow::Error>(Arc::new(BackgroundDrop::new(
                                final_entries,
                                release_executor,
                            )))
                        }
                    })
                    .await?;
                let presence = cx
                    .background_spawn({
                        let filesystem = filesystem.clone();
                        let request = request.clone();
                        let paths = presence_paths;
                        let entries = final_entries.clone();
                        let java_sources = java_sources.clone();
                        async move {
                            let presence = probe_paths(&filesystem, &paths, &request).await?;
                            ensure!(
                                presence == initial_presence,
                                "Android tree roots changed during capture"
                            );
                            verify_file_bytes(&filesystem, &entries, &java_sources, &request)
                                .await?;
                            Ok::<_, anyhow::Error>(presence)
                        }
                    })
                    .await?;
                check_owner(&project, &owner, &request, cx)?;
                check_worktrees(&project, &worktrees, cx)?;

                let file_revision = NEXT_FILE_REVISION
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |revision| {
                        revision.checked_add(1)
                    })
                    .map_err(|_| anyhow::anyhow!("Android file revision space exhausted"))?;
                let tree = cx
                    .background_spawn({
                        let request = request.clone();
                        let entries = final_entries.clone();
                        let java_sources = java_sources.clone();
                        let release_executor = release_executor.clone();
                        async move {
                            request.check_cancelled()?;
                            let mut captured_entries = entries
                                .iter()
                                .map(|(path, (_, entry))| CapturedEntry {
                                    path: path.clone(),
                                    kind: if entry.is_dir() {
                                        CapturedEntryKind::Directory
                                    } else {
                                        CapturedEntryKind::File {
                                            java_source: java_sources.get(path).map(|bytes| {
                                                CapturedJavaSource {
                                                    file_revision,
                                                    bytes: bytes.clone(),
                                                }
                                            }),
                                        }
                                    },
                                })
                                .collect::<Vec<_>>();
                            for (path, version) in &presence {
                                if version.as_ref().is_some_and(|version| version.directory)
                                    && !entries.contains_key(path)
                                {
                                    captured_entries.push(CapturedEntry {
                                        path: path.clone(),
                                        kind: CapturedEntryKind::Directory,
                                    });
                                }
                            }
                            let binding = request.plan.binding();
                            let capture = CapturedModuleFiles {
                                binding: CaptureBinding {
                                    module: binding.module.clone(),
                                    variant: binding.variant.clone(),
                                    model_revision: binding.model_revision,
                                    file_revision,
                                },
                                entries: captured_entries,
                                presence: presence
                                    .into_iter()
                                    .map(|(path, version)| {
                                        let presence = match version {
                                            None => RootPresence::Missing,
                                            Some(version) if version.directory => {
                                                RootPresence::Directory
                                            }
                                            Some(_) => RootPresence::File,
                                        };
                                        (path, presence)
                                    })
                                    .collect(),
                            };
                            let tree = adapt_captured_module(&request.plan, &capture)?;
                            Ok::<_, anyhow::Error>(BackgroundDrop::new(tree, release_executor))
                        }
                    })
                    .await?;
                check_owner(&project, &owner, &request, cx)?;
                check_worktrees(&project, &worktrees, cx)?;
                Ok(CapturedAndroidModuleTree {
                    request,
                    owner,
                    worktrees,
                    entries: final_entries,
                    java_sources,
                    tree,
                    read_limits,
                    _trust_observer: trust_observer,
                })
            };
            let stop = async move {
                match select(Box::pin(cancellation), Box::pin(deadline)).await {
                    Either::Left(_) => anyhow::anyhow!("Android tree capture was cancelled"),
                    Either::Right(_) => {
                        anyhow::anyhow!("Android tree capture exceeded its scan/read deadline")
                    }
                }
            };
            match select(Box::pin(capture), Box::pin(stop)).await {
                Either::Left((result, _)) => result,
                Either::Right((error, pending)) => {
                    stop_request.cancel();
                    drop(pending);
                    Err(error)
                }
            }
        })
    }

    pub fn is_android_tree_capture_current(
        &self,
        capture: &CapturedAndroidModuleTree,
        cx: &App,
    ) -> bool {
        self.android_tree_owner_is_current(&capture.owner, &capture.request, cx)
            && capture.worktrees.iter().all(|captured| {
                self.worktree_for_id(captured.id, cx)
                    .is_some_and(|worktree| {
                        worktree == captured.scope.worktree
                            && !worktree_is_restricted(self, captured.id, cx)
                            && worktree.read(cx).scan_id() == captured.scan_id
                            && worktree.read(cx).completed_scan_id() == captured.completed_scan_id
                    })
            })
    }

    /// Resolves only actual captured entries; virtual groups have no filesystem ID.
    pub fn android_tree_project_path(
        &self,
        capture: &CapturedAndroidModuleTree,
        path: &Path,
        cx: &App,
    ) -> Result<ProjectPath> {
        ensure!(
            self.is_android_tree_capture_current(capture, cx),
            "Android tree capture is outdated"
        );
        let (project_path, expected) = capture
            .entries
            .get(path)
            .context("Android tree target was not captured")?;
        let actual = self
            .entry_for_path(project_path, cx)
            .context("Android tree target was removed")?;
        ensure!(actual == expected, "Android tree target changed");
        Ok(project_path.clone())
    }

    fn android_tree_capture_owner(
        &self,
        request: &AndroidTreeCaptureRequest,
        project: EntityId,
        cx: &App,
    ) -> Result<CaptureOwner> {
        request.check_cancelled()?;
        ensure!(
            self.is_local(),
            "Android tree capture requires local worktrees"
        );
        let context = self.android_context();
        ensure!(
            context.token(request.root).as_ref() == Some(&request.context_token),
            "Android tree root context is outdated or untrusted"
        );
        let snapshot = context
            .snapshot(request.root)
            .context("Evaluate the Android project context first")?;
        ensure!(
            snapshot.phase() == ObservationPhase::Complete,
            "Android project context is incomplete"
        );
        let root_path = context
            .root_path(request.root)
            .context("Android tree root was removed")?
            .to_path_buf();
        let worktree_id = WorktreeId::from_proto(request.root.worktree());
        let worktree = self
            .worktree_for_id(worktree_id, cx)
            .context("Android tree root worktree was removed")?;
        ensure!(
            worktree.read(cx).is_visible()
                && worktree.read(cx).abs_path().as_ref() == root_path.as_path(),
            "Android tree root does not own the visible project"
        );
        ensure!(
            !worktree_is_restricted(self, worktree_id, cx),
            "Android tree root is restricted"
        );
        let state = self.android_model();
        ensure!(
            state.is_current(&request.model_token) && state.root() == Some(root_path.as_path()),
            "Android tree model is outdated"
        );
        let model = state
            .model
            .as_ref()
            .context("Sync the Android project first")?
            .clone();
        ensure!(
            model.root == root_path,
            "Android tree model belongs to another root"
        );
        let selected = state
            .selected
            .as_ref()
            .context("Select an Android variant first")?
            .clone();
        ensure!(
            Arc::ptr_eq(&selected.model, &model),
            "Android tree selection belongs to another model"
        );
        let binding = request.plan.binding();
        ensure!(
            selected.variants.get(&binding.module) == Some(&binding.variant),
            "Android tree plan belongs to another selected variant"
        );
        let module = model
            .modules
            .iter()
            .find(|module| module.path == binding.module)
            .context("Android tree module is absent")?;
        ensure!(
            snapshot
                .modules()
                .any(|observed| observed.path() == module.path
                    && observed.directory() == module.directory),
            "Android tree module has no evaluated context owner"
        );
        let module_directory = module.directory.clone();
        Ok(CaptureOwner {
            project,
            model,
            selected,
            root_path,
            module_directory,
            trust_store: TrustedWorktrees::try_get_global(cx).map(|trusted| trusted.entity_id()),
        })
    }

    fn android_tree_owner_is_current(
        &self,
        owner: &CaptureOwner,
        request: &AndroidTreeCaptureRequest,
        cx: &App,
    ) -> bool {
        request.check_cancelled().is_ok()
            && self.is_local()
            && TrustedWorktrees::try_get_global(cx).map(|trusted| trusted.entity_id())
                == owner.trust_store
            && self.android_context().token(request.root).as_ref() == Some(&request.context_token)
            && self.android_context().root_path(request.root) == Some(owner.root_path.as_path())
            && self
                .android_context()
                .snapshot(request.root)
                .is_some_and(|snapshot| snapshot.phase() == ObservationPhase::Complete)
            && self.android_model().is_current(&request.model_token)
            && self.android_model().root() == Some(owner.root_path.as_path())
            && self
                .android_model()
                .model
                .as_ref()
                .is_some_and(|model| Arc::ptr_eq(model, &owner.model))
            && self
                .android_model()
                .selected
                .as_ref()
                .is_some_and(|selected| Arc::ptr_eq(selected, &owner.selected))
            && self
                .worktree_for_id(WorktreeId::from_proto(request.root.worktree()), cx)
                .is_some_and(|worktree| {
                    worktree.read(cx).is_visible()
                        && worktree.read(cx).abs_path().as_ref() == owner.root_path.as_path()
                })
            && !worktree_is_restricted(self, WorktreeId::from_proto(request.root.worktree()), cx)
    }
}

fn worktree_is_restricted(project: &Project, id: WorktreeId, cx: &App) -> bool {
    TrustedWorktrees::try_get_global(cx).is_some_and(|trusted| {
        trusted
            .read(cx)
            .is_worktree_restricted(&project.worktree_store(), id)
    })
}

fn check_owner(
    project: &WeakEntity<Project>,
    owner: &CaptureOwner,
    request: &AndroidTreeCaptureRequest,
    cx: &AsyncApp,
) -> Result<()> {
    ensure!(
        project.entity_id() == owner.project,
        "Android tree project owner changed"
    );
    ensure!(
        project.read_with(cx, |project, cx| project
            .android_tree_owner_is_current(owner, request, cx))?,
        "Android tree capture owner is outdated or untrusted"
    );
    Ok(())
}

async fn probe_paths(
    filesystem: &Arc<dyn Fs>,
    paths: &BTreeSet<PathBuf>,
    request: &AndroidTreeCaptureRequest,
) -> Result<PresenceVersions> {
    let mut presence = BTreeMap::new();
    for path in paths {
        request.check_cancelled()?;
        let version = filesystem
            .metadata(path)
            .await
            .with_context(|| {
                format!(
                    "Reading Android source-root metadata for {}",
                    path.display()
                )
            })?
            .map(FileVersion::from);
        presence.insert(path.clone(), version);
    }
    Ok(presence)
}

async fn resolve_worktrees(
    project: &WeakEntity<Project>,
    owner: &CaptureOwner,
    request: &AndroidTreeCaptureRequest,
    presence: &PresenceVersions,
    special_file: &Path,
    cx: &mut AsyncApp,
) -> Result<Vec<WorktreeScope>> {
    let mut scopes = BTreeMap::<WorktreeId, WorktreeScope>::new();
    let inventory_paths = request
        .plan
        .source_roots()
        .iter()
        .map(|root| root.path.clone())
        .chain(std::iter::once(special_file.to_path_buf()))
        .collect::<BTreeSet<_>>();
    for path in &inventory_paths {
        check_owner(project, owner, request, cx)?;
        let existing = project.read_with(cx, |project, cx| project.find_worktree(path, cx))?;
        let (worktree, relative) = if let Some(existing) = existing {
            existing
        } else if let Some(version) = presence.get(path).and_then(Option::as_ref) {
            let target = if version.directory {
                path.as_path()
            } else {
                path.parent().context("Android source file has no parent")?
            };
            ensure!(
                target != Path::new("/") && target.parent().is_some(),
                "Android source root is too broad"
            );
            let (worktree, _) = project
                .update(cx, |project, cx| {
                    project.find_or_create_worktree(target, false, cx)
                })?
                .await?;
            check_owner(project, owner, request, cx)?;
            let relative = RelPath::new(
                path.strip_prefix(worktree.read_with(cx, |worktree, _| worktree.abs_path()))?,
                PathStyle::local(),
            )?
            .into_arc();
            (worktree, relative)
        } else {
            // Absence is retained by two actual metadata probes. Do not create
            // an unrelated ancestor worktree to scan a nonexistent external root.
            continue;
        };
        let id = worktree.read_with(cx, |worktree, _| worktree.id());
        ensure!(
            !project.read_with(cx, |project, cx| worktree_is_restricted(project, id, cx))?,
            "Android source worktree is restricted"
        );
        ensure!(
            worktree.read_with(cx, |worktree, _| worktree.is_local()),
            "Android source worktree is remote"
        );
        let scope = scopes.entry(id).or_insert_with(|| WorktreeScope {
            worktree,
            refresh: BTreeSet::new(),
            recursive: BTreeSet::new(),
            inventory: BTreeSet::new(),
        });
        scope.refresh.insert(relative.clone());
        scope.inventory.insert(relative.clone());
        if path != special_file
            && presence
                .get(path)
                .and_then(Option::as_ref)
                .is_some_and(|version| version.directory)
        {
            scope.recursive.insert(relative);
        }
    }
    Ok(scopes.into_values().collect())
}

async fn scan_worktrees(
    project: &WeakEntity<Project>,
    owner: &CaptureOwner,
    request: &AndroidTreeCaptureRequest,
    scopes: &[WorktreeScope],
    filesystem: &Arc<dyn Fs>,
    expand: bool,
    cx: &mut AsyncApp,
) -> Result<Vec<WorktreeCapture>> {
    let mut additional_prefixes = BTreeMap::<WorktreeId, BTreeSet<Arc<RelPath>>>::new();
    let mut expansion_count = 0usize;
    let mut first_pass = true;
    loop {
        let mut pending = Vec::new();
        for scope in scopes {
            check_owner(project, owner, request, cx)?;
            let (next_scan, barriers) =
                scope.worktree.downgrade().read_with(cx, |worktree, _| {
                    let local = worktree
                        .as_local()
                        .context("Android source worktree is remote")?;
                    ensure!(
                        scope
                            .refresh
                            .iter()
                            .all(|path| !local.settings().is_path_excluded(path)),
                        "An Android source root is excluded from worktree scans"
                    );
                    let mut barriers = Vec::new();
                    if expand && first_pass {
                        for path in &scope.recursive {
                            barriers.push(local.add_path_prefix_to_scan(path.clone()));
                        }
                    }
                    if let Some(prefixes) = additional_prefixes.get(&worktree.id()) {
                        for path in prefixes {
                            ensure!(
                                !local.settings().is_path_excluded(path),
                                "An Android source descendant is excluded from worktree scans"
                            );
                            barriers.push(local.add_path_prefix_to_scan(path.clone()));
                        }
                    }
                    barriers.push(
                        local.refresh_entries_for_paths(scope.refresh.iter().cloned().collect()),
                    );
                    Ok::<_, anyhow::Error>((
                        worktree
                            .scan_id()
                            .checked_add(1)
                            .context("Worktree scan revision space exhausted")?,
                        barriers,
                    ))
                })??;
            pending.push((scope.clone(), next_scan, barriers));
        }
        let mut captured = Vec::new();
        for (scope, next_scan, barriers) in pending {
            for mut barrier in barriers {
                barrier.next().await;
                check_owner(project, owner, request, cx)?;
            }
            // A scanner can close its request barrier without advancing its
            // revision when the root has disappeared. Do not await that ID.
            let absolute = scope
                .worktree
                .read_with(cx, |worktree, _| worktree.abs_path());
            let root_exists = cx
                .background_spawn({
                    let filesystem = filesystem.clone();
                    async move { filesystem.metadata(&absolute).await }
                })
                .await?;
            ensure!(
                root_exists.is_some(),
                "Android source worktree root was removed during scanning"
            );
            scope
                .worktree
                .downgrade()
                .update(cx, |worktree, _| worktree.wait_for_snapshot(next_scan))?
                .await?;
            check_owner(project, owner, request, cx)?;
            let (id, scan_id, completed_scan_id, snapshot) =
                scope.worktree.downgrade().read_with(cx, |worktree, _| {
                    ensure!(
                        worktree.completed_scan_id() >= worktree.scan_id(),
                        "Android worktree changed while its scan was captured"
                    );
                    Ok::<_, anyhow::Error>((
                        worktree.id(),
                        worktree.scan_id(),
                        worktree.completed_scan_id(),
                        worktree.snapshot(),
                    ))
                })??;
            captured.push(WorktreeCapture {
                scope,
                id,
                scan_id,
                completed_scan_id,
                snapshot,
            });
        }
        if !expand {
            return Ok(captured);
        }
        let unloaded = cx
            .background_spawn({
                let captured = captured.clone();
                let request = request.clone();
                async move {
                    let mut unloaded = BTreeMap::<WorktreeId, BTreeSet<Arc<RelPath>>>::new();
                    visit_scoped_entries(&captured, &request, |captured, entry| {
                        if entry.kind == EntryKind::UnloadedDir {
                            unloaded
                                .entry(captured.id)
                                .or_default()
                                .insert(entry.path.clone());
                        }
                        Ok(())
                    })?;
                    Ok::<_, anyhow::Error>(unloaded)
                }
            })
            .await?;
        if unloaded.is_empty() {
            return Ok(captured);
        }
        for prefixes in unloaded.values() {
            expansion_count = expansion_count
                .checked_add(prefixes.len())
                .context("Android source expansion budget overflow")?;
        }
        ensure!(
            expansion_count <= MAX_EXPANSION_DIRECTORIES,
            "Android source directory expansion exceeds its capture budget"
        );
        // Loaded parents do not cause Worktree to register a recursive prefix.
        // Target the actual unloaded descendants, always within evaluated roots.
        additional_prefixes = unloaded;
        first_pass = false;
    }
}

fn check_worktrees(
    project: &WeakEntity<Project>,
    worktrees: &[WorktreeCapture],
    cx: &AsyncApp,
) -> Result<()> {
    for captured in worktrees {
        ensure!(
            project.read_with(cx, |project, cx| {
                project
                    .worktree_for_id(captured.id, cx)
                    .is_some_and(|worktree| {
                        worktree == captured.scope.worktree
                            && !worktree_is_restricted(project, captured.id, cx)
                            && worktree.read(cx).scan_id() == captured.scan_id
                            && worktree.read(cx).completed_scan_id() == captured.completed_scan_id
                    })
            })?,
            "Android source worktree changed during capture"
        );
    }
    Ok(())
}

fn visit_scoped_entries(
    worktrees: &[WorktreeCapture],
    request: &AndroidTreeCaptureRequest,
    mut visit: impl FnMut(&WorktreeCapture, &Entry) -> Result<()>,
) -> Result<()> {
    let mut visited = 0usize;
    for captured in worktrees {
        request.check_cancelled()?;
        for root in &captured.scope.recursive {
            request.check_cancelled()?;
            if root
                .ancestors()
                .skip(1)
                .any(|ancestor| captured.scope.recursive.contains(ancestor))
            {
                continue;
            }
            // Seeking a prefix avoids walking unrelated Python/HTML project
            // entries, and overlapping evaluated roots are visited only once.
            for entry in captured
                .snapshot
                .traverse_from_path(true, true, true, root)
                .take_while(|entry| entry.path.starts_with(root))
            {
                request.check_cancelled()?;
                visited += 1;
                ensure!(
                    visited <= MAX_CAPTURE_ENTRIES,
                    "Android tree file inventory exceeds its capture limit"
                );
                visit(captured, entry)?;
            }
        }
        for root in &captured.scope.inventory {
            request.check_cancelled()?;
            if root
                .ancestors()
                .any(|ancestor| captured.scope.recursive.contains(ancestor))
            {
                continue;
            }
            if let Some(entry) = captured.snapshot.entry_for_path(root) {
                visited += 1;
                ensure!(
                    visited <= MAX_CAPTURE_ENTRIES,
                    "Android tree file inventory exceeds its capture limit"
                );
                visit(captured, entry)?;
            }
        }
    }
    Ok(())
}

fn collect_entries(
    worktrees: &[WorktreeCapture],
    request: &AndroidTreeCaptureRequest,
) -> Result<PhysicalEntries> {
    let mut entries = BTreeMap::new();
    visit_scoped_entries(worktrees, request, |captured, entry| {
        ensure!(
            !matches!(entry.kind, EntryKind::UnloadedDir | EntryKind::PendingDir),
            "Android source directory has not completed scanning"
        );
        ensure!(!entry.is_fifo, "Android source root contains a FIFO");
        let absolute = captured.snapshot.abs_path().join(entry.path.as_std_path());
        let value = (
            ProjectPath {
                worktree_id: captured.id,
                path: entry.path.clone(),
            },
            entry.clone(),
        );
        match entries.entry(absolute) {
            std::collections::btree_map::Entry::Occupied(previous) => {
                ensure!(
                    previous.get() == &value,
                    "Android tree has ambiguous physical worktree ownership"
                );
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(value);
            }
        }
        Ok(())
    })?;
    Ok(entries)
}

fn matches_version(entry: &Entry, version: &FileVersion) -> bool {
    entry.inode == version.inode
        && entry.mtime == Some(version.mtime)
        && entry.size == version.len
        && entry.is_dir() == version.directory
        && !version.fifo
}

async fn read_versioned_java(
    filesystem: &Arc<dyn Fs>,
    path: &Path,
    entry: &Entry,
    request: &AndroidTreeCaptureRequest,
) -> Result<Arc<[u8]>> {
    request.check_cancelled()?;
    let before: FileVersion = filesystem
        .metadata(path)
        .await?
        .context("Android Java file was removed")?
        .into();
    ensure!(
        matches_version(entry, &before),
        "Android Java file changed before its byte capture"
    );
    ensure!(
        before.len <= MAX_JAVA_FACT_BYTES as u64,
        "Android Java file exceeds its capture limit"
    );
    let source = filesystem.open_sync(path).await?;
    let mut bytes = Vec::with_capacity(before.len as usize);
    source
        .take(MAX_JAVA_FACT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_JAVA_FACT_BYTES,
        "Android Java file grew beyond its capture limit"
    );
    #[cfg(test)]
    wait_at_test_gate(request, TestPhase::JavaRead).await?;
    request.check_cancelled()?;
    let after: FileVersion = filesystem
        .metadata(path)
        .await?
        .context("Android Java file was removed during capture")?
        .into();
    ensure!(
        before == after && bytes.len() as u64 == before.len,
        "Android Java file changed during its byte capture"
    );
    Ok(bytes.into())
}

async fn read_java_sources(
    filesystem: &Arc<dyn Fs>,
    entries: &PhysicalEntries,
    request: &AndroidTreeCaptureRequest,
    release_executor: &BackgroundExecutor,
) -> Result<(SharedJavaSources, BackgroundDrop<Vec<AndroidTreeReadLimit>>)> {
    let mut sources = BTreeMap::new();
    let mut limits = Vec::new();
    let mut bytes = 0;
    for (path, (_, entry)) in entries {
        request.check_cancelled()?;
        if entry.is_dir()
            || path.extension().is_none_or(|extension| extension != "java")
            || !request.plan.source_roots().iter().any(|root| {
                matches!(
                    root.group,
                    SourceGroup::Java
                        | SourceGroup::Kotlin
                        | SourceGroup::KotlinAndJava
                        | SourceGroup::GeneratedJava
                ) && path.starts_with(&root.path)
            })
        {
            continue;
        }
        if entry.size > MAX_JAVA_FACT_BYTES as u64 {
            limits.push(AndroidTreeReadLimit::JavaFileTooLarge(path.clone()));
            continue;
        }
        let size = entry.size as usize;
        if size > MAX_PARSED_JAVA_BYTES - bytes {
            limits.push(AndroidTreeReadLimit::JavaCaptureBudget(path.clone()));
            continue;
        }
        let source = read_versioned_java(filesystem, path, entry, request).await?;
        bytes += source.len();
        sources.insert(path.clone(), source);
    }
    Ok((
        Arc::new(BackgroundDrop::new(sources, release_executor.clone())),
        BackgroundDrop::new(limits, release_executor.clone()),
    ))
}

async fn verify_file_bytes(
    filesystem: &Arc<dyn Fs>,
    entries: &PhysicalEntries,
    sources: &BTreeMap<PathBuf, Arc<[u8]>>,
    request: &AndroidTreeCaptureRequest,
) -> Result<()> {
    for (path, (_, entry)) in entries {
        request.check_cancelled()?;
        let current: FileVersion = filesystem
            .metadata(path)
            .await?
            .context("Android tree entry was removed during capture")?
            .into();
        ensure!(
            matches_version(entry, &current),
            "Android tree entry changed during capture"
        );
        if let Some(expected) = sources.get(path) {
            let actual = read_versioned_java(filesystem, path, entry, request).await?;
            ensure!(
                actual.as_ref() == expected.as_ref(),
                "Android Java source bytes changed during reconciliation"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum TestPhase {
    BeforeScan,
    Scanned,
    JavaRead,
}

#[cfg(test)]
struct TestGate {
    phase: TestPhase,
    reached: parking_lot::Mutex<Option<futures::channel::oneshot::Sender<()>>>,
    resume: parking_lot::Mutex<Option<futures::channel::oneshot::Receiver<()>>>,
}

#[cfg(test)]
async fn wait_at_test_gate(request: &AndroidTreeCaptureRequest, phase: TestPhase) -> Result<()> {
    if let Some(gate) = &request.gate
        && gate.phase == phase
    {
        let resume = gate.resume.lock().take();
        if let Some(resume) = resume {
            gate.reached
                .lock()
                .take()
                .context("Capture test gate lost its observer")?
                .send(())
                .map_err(|_| anyhow::anyhow!("Capture test observer was dropped"))?;
            resume.await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use android_tools::{
        project_context::{ActiveContext, PluginId, decode_context_record},
        project_model::{SourceScope, VariantId},
        project_tree::NodeKey,
        project_tree_adapter::{
            CapturedModulePresentation, KotlinCapability, prepare_module_roots,
        },
    };
    use fs::{FakeFs, RealFs};
    use futures::channel::oneshot;
    use gpui::TestAppContext;
    use serde_json::json;
    use settings::SettingsStore;

    const JAVA: &str = "// é\r\npackage example;\r\nclass ActualClass {}\r\n";

    fn init(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            release_channel::init(semver::Version::new(0, 0, 0), cx);
        });
    }

    // Supplemental host fixtures, deliberately independent of the six original
    // Android Studio Gradle workflows. These test real capture races, not parity.
    async fn request(
        project: &Entity<Project>,
        root: &Path,
        external: Option<&Path>,
        cx: &mut TestAppContext,
    ) -> Result<AndroidTreeCaptureRequest> {
        let worktree = project
            .read_with(cx, |project, cx| {
                project
                    .visible_worktrees(cx)
                    .next()
                    .map(|tree| tree.read(cx).id())
            })
            .context("Fixture root worktree")?;
        let handle = project.update(cx, |project, cx| {
            project.ensure_android_context(worktree, true, cx)
        })?;
        let discovery = project.update(cx, |project, cx| {
            project.begin_android_context_import(handle, cx)
        })?;
        let mut active = ActiveContext::default();
        active.select(Some(handle), None)?;
        let owner = project
            .read_with(cx, |project, _| {
                active.discovery_token(project.android_context())
            })
            .context("Fixture import owner")?;
        let module_directory = root.join("app");
        let record = json!({"schema":1,"root":root,"gradleVersion":"9.4","phase":"complete",
            "modules":[{"path":":","directory":root,
                "plugins":PluginId::ALL.map(|plugin| json!({"plugin":plugin,"applied":false})),
                "targets":{"status":"unavailable","value":{"detail":"Supplemental fixture root"}},
                "android":{"status":"unavailable","value":{"detail":"Root is not an Android module"}}},
                {"path":":app","directory":module_directory,
                "plugins":PluginId::ALL.map(|plugin| json!({"plugin":plugin,"applied":plugin==PluginId::AndroidApplication})),
                "targets":{"status":"unavailable","value":{"detail":"Supplemental Java fixture"}},
                "android":{"status":"available","value":{"pluginVersion":"9.4.0"}}}]});
        let snapshot = decode_context_record(&serde_json::to_vec(&record)?, root)?;
        project.update(cx, |project, cx| {
            project.publish_android_context(&active, &owner, &discovery, snapshot, cx)
        })?;

        let java_root = external
            .map(Path::to_path_buf)
            .unwrap_or_else(|| module_directory.join("src/main/java"));
        let provider = json!({"name":"main","roots":[
            {"path":java_root,"kind":"java"},
            {"path":module_directory.join("src/main/res"),"kind":"resources"},
            {"path":module_directory.join("src/main/assets"),"kind":"assets"}]});
        let container = json!({"main":provider,"hostTests":[],"deviceTests":[],"fixtures":null});
        let variants = ["debug", "release"].map(|name| json!({"name":name,"outputListing":null,"components":[{
            "name":name,"scope":SourceScope::Main,"namespace":"example","dependencies":[],"sources":[
                {"path":module_directory.join("build/generated/source/config"),"kind":"java","generated":true}]}]}));
        let provider_variants = ["debug", "release"].map(|name| json!({"name":name,"buildType":name,"productFlavors":[],
            "main":{"multiFlavor":null,"variant":null},"hostTests":[],"deviceTests":[],"fixtures":null,"testSuites":[]}));
        let module = json!({"path":":app","directory":module_directory,"namespace":"example","kind":"application",
            "defaultVariant":"debug","sourceProviders":null,"variants":variants,
            "evaluatedProviders":{"status":"available","value":{"version":1,"agpVersion":"9.4.0","modelProducer":{"major":22,"minor":0},
                "defaultSourceSet":container,"buildTypes":[
                    {"name":"debug","container":{"main":{"name":"debug","roots":[]},"hostTests":[],"deviceTests":[],"fixtures":null}},
                    {"name":"release","container":{"main":{"name":"release","roots":[]},"hostTests":[],"deviceTests":[],"fixtures":null}}],
                "productFlavors":[],"variants":provider_variants,"testSuites":[],"nativeMembership":null}}});
        let model: ProjectModel = serde_json::from_value(
            json!({"version":1,"root":root,"modules":[module],"diagnostics":[]}),
        )?;
        let sync = project.update(cx, |project, cx| {
            project.invalidate_android_model(Some(root.to_path_buf()), cx)
        });
        project.update(cx, |project, cx| {
            project.publish_android_model(&sync, model, cx)
        })?;
        project.update(cx, |project, cx| {
            project.select_android_variant(
                Some(VariantId {
                    module: ":app".into(),
                    variant: "debug".into(),
                }),
                cx,
            )
        })?;
        project.read_with(cx, |project, _| {
            let state = project.android_model();
            let model = state.model.as_ref().context("Fixture model")?;
            let plan = prepare_module_roots(
                &model.modules[0],
                "debug",
                state.model_revision().context("Fixture revision")?,
                Some(&CapturedModulePresentation {
                    display_name: Some("app".into()),
                    kotlin: KotlinCapability::Disabled,
                    compact_packages: true,
                }),
            )?;
            Ok(AndroidTreeCaptureRequest::new(
                handle,
                project
                    .android_context()
                    .token(handle)
                    .context("Fixture context")?,
                state.token(),
                Arc::new(plan),
                17,
                23,
            ))
        })
    }

    fn gate(
        request: &mut AndroidTreeCaptureRequest,
        phase: TestPhase,
    ) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (reached_sender, reached) = oneshot::channel();
        let (resume, resume_receiver) = oneshot::channel();
        request.gate = Some(Arc::new(TestGate {
            phase,
            reached: parking_lot::Mutex::new(Some(reached_sender)),
            resume: parking_lot::Mutex::new(Some(resume_receiver)),
        }));
        (reached, resume)
    }

    async fn fake_project(cx: &mut TestAppContext) -> (Arc<FakeFs>, Entity<Project>, PathBuf) {
        init(cx);
        let root = PathBuf::from(util::path!("/android-capture"));
        let filesystem = FakeFs::new(cx.executor());
        filesystem.insert_tree(&root, json!({".gitignore":"app/build/\n", "main.py":"print(1)", "index.html":"<p>Hello</p>", "app":{
            "build.gradle":"", "src":{"main":{"java":{"example":{"Different.java":JAVA}}, "res":{"layout":{"screen.xml":"<View/>"}},"assets":{"nested":{"asset.bin":"asset"}}}},
            "build":{"generated":{"source":{"config":{"BuildConfig.java":"class BuildConfig {}"}}}}}})).await;
        let project = Project::test(filesystem.clone(), [root.as_path()], cx).await;
        (filesystem, project, root)
    }

    #[gpui::test]
    async fn real_ignored_generated_and_external_sources_are_captured_and_navigable(
        cx: &mut TestAppContext,
    ) {
        real_capture_case(cx)
            .await
            .expect("Actual filesystem capture must succeed");
    }

    async fn real_capture_case(cx: &mut TestAppContext) -> Result<()> {
        init(cx);
        cx.executor().allow_parking();
        let temporary = tempfile::TempDir::new()?;
        let root = temporary.path().join("project");
        let external = temporary.path().join("external-java");
        let generated = root.join("app/build/generated/source/config/BuildConfig.java");
        let source = external.join("example/Different.java");
        let resource = root.join("app/src/main/res/layout/screen.xml");
        let asset = root.join("app/src/main/assets/nested/asset.bin");
        for path in [&generated, &source, &resource, &asset] {
            std::fs::create_dir_all(path.parent().context("Fixture file parent")?)?;
        }
        std::fs::create_dir(root.join(".git"))?;
        std::fs::write(root.join(".gitignore"), "app/build/\n")?;
        std::fs::write(root.join("main.py"), "print(1)")?;
        std::fs::write(root.join("index.html"), "<p>Hello</p>")?;
        std::fs::write(&generated, "class BuildConfig {}")?;
        std::fs::write(&source, JAVA)?;
        std::fs::write(&resource, "<View/>")?;
        std::fs::write(&asset, [0, 1, 255])?;
        let project = Project::test(RealFs::new(None, cx.executor()), [root.as_path()], cx).await;
        let request = request(&project, &root, Some(&external), cx).await?;
        let capture = project
            .update(cx, |project, cx| {
                project.capture_android_module_tree(request, cx)
            })
            .await?;
        assert_eq!(capture.owner_generation(), 17);
        assert_eq!(capture.request_generation(), 23);
        assert_eq!(
            capture
                .java_source(&source)
                .context("Captured Java bytes")?
                .as_ref(),
            JAVA.as_bytes()
        );
        assert!(capture.read_limits().is_empty());
        let class = capture.tree().tree.nodes().find(|node| matches!(&node.key, NodeKey::JavaClass { name, path, .. } if name == "ActualClass" && path == &source)).context("Parsed actual class declaration")?;
        let target = class.navigation.as_ref().context("Class navigation")?;
        let offset = target.byte_offset.context("Class byte offset")?;
        assert_eq!(
            &JAVA.as_bytes()[offset..offset + "ActualClass".len()],
            b"ActualClass"
        );
        for path in [&source, &generated, &resource, &asset] {
            let physical = project.read_with(cx, |project, cx| {
                project.android_tree_project_path(&capture, path, cx)
            })?;
            assert_eq!(
                project.read_with(cx, |project, cx| project
                    .entry_for_path(&physical, cx)
                    .map(|entry| entry.id)),
                capture.entries.get(path).map(|(_, entry)| entry.id)
            );
        }
        assert!(
            capture
                .entries
                .get(&generated)
                .is_some_and(|(_, entry)| entry.is_ignored)
        );
        assert_eq!(
            project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()),
            1
        );
        let external_tree = capture
            .worktrees
            .iter()
            .find(|tree| tree.snapshot.abs_path().as_ref() == external.as_path())
            .context("Exact external root worktree")?;
        assert!(
            !external_tree
                .scope
                .worktree
                .read_with(cx, |tree, _| tree.is_visible())
        );
        assert!(!capture.entries.contains_key(&root.join("main.py")));
        assert!(!capture.entries.contains_key(&root.join("index.html")));
        assert_eq!(std::fs::read_to_string(root.join("main.py"))?, "print(1)");
        assert_eq!(
            std::fs::read_to_string(root.join("index.html"))?,
            "<p>Hello</p>"
        );
        Ok(())
    }

    #[gpui::test]
    async fn variant_aba_and_trust_revocation_reject_pending_capture(cx: &mut TestAppContext) {
        stale_owner_case(cx)
            .await
            .expect("Owner invalidation fixture must complete");
    }

    async fn stale_owner_case(cx: &mut TestAppContext) -> Result<()> {
        let (_, project, root) = fake_project(cx).await;
        for revoke_trust in [false, true] {
            let mut request = request(&project, &root, None, cx).await?;
            let handle = request.root;
            let (reached, resume) = gate(&mut request, TestPhase::Scanned);
            let capture = project.update(cx, |project, cx| {
                project.capture_android_module_tree(request, cx)
            });
            reached.await?;
            if revoke_trust {
                project.update(cx, |project, cx| {
                    project.ensure_android_context(
                        WorktreeId::from_proto(handle.worktree()),
                        false,
                        cx,
                    )
                })?;
                project.update(cx, |project, cx| {
                    project.ensure_android_context(
                        WorktreeId::from_proto(handle.worktree()),
                        true,
                        cx,
                    )
                })?;
            } else {
                for variant in ["release", "debug"] {
                    project.update(cx, |project, cx| {
                        project.select_android_variant(
                            Some(VariantId {
                                module: ":app".into(),
                                variant: variant.into(),
                            }),
                            cx,
                        )
                    })?;
                }
            }
            resume
                .send(())
                .map_err(|_| anyhow::anyhow!("Owner gate dropped"))?;
            assert!(
                capture.await.is_err(),
                "An old completion must not publish after an ABA transition"
            );
        }
        Ok(())
    }

    #[gpui::test]
    async fn changed_java_bytes_are_rejected_before_publication_with_watch_events_paused(
        cx: &mut TestAppContext,
    ) {
        changed_bytes_case(cx)
            .await
            .expect("Read-version fixture must complete");
    }

    async fn changed_bytes_case(cx: &mut TestAppContext) -> Result<()> {
        let (filesystem, project, root) = fake_project(cx).await;
        let mut request = request(&project, &root, None, cx).await?;
        let (reached, resume) = gate(&mut request, TestPhase::JavaRead);
        filesystem.pause_events();
        let capture = project.update(cx, |project, cx| {
            project.capture_android_module_tree(request, cx)
        });
        reached.await?;
        // The generated Java root sorts before src; change every Java file so
        // this is independent of read scheduling or scanner enumeration.
        for path in [
            root.join("app/build/generated/source/config/BuildConfig.java"),
            root.join("app/src/main/java/example/Different.java"),
        ] {
            filesystem
                .insert_file(path, b"class ReplacedDuringRead {}".to_vec())
                .await;
        }
        resume
            .send(())
            .map_err(|_| anyhow::anyhow!("Byte gate dropped"))?;
        assert!(
            capture.await.is_err(),
            "Paused watcher delivery must not allow stale Java bytes"
        );
        filesystem.unpause_events_and_flush();
        Ok(())
    }

    #[gpui::test]
    async fn dropping_pending_capture_releases_external_worktree_and_cancelled_navigation(
        cx: &mut TestAppContext,
    ) {
        cancelled_case(cx)
            .await
            .expect("Cancellation fixture must complete");
    }

    async fn cancelled_case(cx: &mut TestAppContext) -> Result<()> {
        let (filesystem, project, root) = fake_project(cx).await;
        let external = PathBuf::from(util::path!("/external-android-java"));
        filesystem
            .insert_tree(&external, json!({"example":{"Different.java":JAVA}}))
            .await;
        let mut request = request(&project, &root, Some(&external), cx).await?;
        let (reached, resume) = gate(&mut request, TestPhase::Scanned);
        let task = project.update(cx, |project, cx| {
            project.capture_android_module_tree(request.clone(), cx)
        });
        reached.await?;
        assert!(filesystem.watched_paths().contains(&external));
        drop(task);
        cx.executor().run_until_parked();
        assert!(
            resume.send(()).is_err(),
            "Dropping the owner task must cancel its pending read gate"
        );
        cx.executor().run_until_parked();
        assert!(
            !filesystem.watched_paths().contains(&external),
            "A cancelled capture must release its invisible worktree watcher"
        );
        request.gate = None;
        let capture = project
            .update(cx, |project, cx| {
                project.capture_android_module_tree(request.clone(), cx)
            })
            .await?;
        let source = external.join("example/Different.java");
        assert!(
            project
                .read_with(cx, |project, cx| project
                    .android_tree_project_path(&capture, &source, cx))
                .is_ok()
        );
        request.cancel();
        assert!(!project.read_with(cx, |project, cx| {
            project.is_android_tree_capture_current(&capture, cx)
        }));
        assert!(
            project
                .read_with(cx, |project, cx| project
                    .android_tree_project_path(&capture, &source, cx))
                .is_err()
        );
        Ok(())
    }

    #[gpui::test]
    async fn outdated_scan_and_wrong_root_tokens_cannot_navigate(cx: &mut TestAppContext) {
        stale_scan_case(cx)
            .await
            .expect("Scan invalidation fixture must complete");
    }

    async fn stale_scan_case(cx: &mut TestAppContext) -> Result<()> {
        let (filesystem, project, root) = fake_project(cx).await;
        let request = request(&project, &root, None, cx).await?;
        let source = root.join("app/src/main/java/example/Different.java");
        let capture = project
            .update(cx, |project, cx| {
                project.capture_android_module_tree(request.clone(), cx)
            })
            .await?;
        filesystem
            .insert_file(source.clone(), b"class Changed {}".to_vec())
            .await;
        cx.condition(&project, |project, cx| {
            !project.is_android_tree_capture_current(&capture, cx)
        })
        .await;
        assert!(
            project
                .read_with(cx, |project, cx| project
                    .android_tree_project_path(&capture, &source, cx))
                .is_err()
        );
        assert_eq!(
            capture
                .java_source(&source)
                .context("Immutable old bytes")?
                .as_ref(),
            JAVA.as_bytes()
        );
        project.update(cx, |project, cx| {
            project.remove_android_context(WorktreeId::from_proto(request.root.worktree()), cx)
        });
        project.update(cx, |project, cx| {
            project.ensure_android_context(
                WorktreeId::from_proto(request.root.worktree()),
                true,
                cx,
            )
        })?;
        assert!(
            project
                .update(cx, |project, cx| project
                    .capture_android_module_tree(request, cx))
                .await
                .is_err()
        );
        Ok(())
    }

    #[gpui::test]
    async fn restricted_invisible_external_worktree_rejects_pending_capture(
        cx: &mut TestAppContext,
    ) {
        external_trust_case(cx)
            .await
            .expect("External trust fixture must complete");
    }

    async fn external_trust_case(cx: &mut TestAppContext) -> Result<()> {
        use crate::trusted_worktrees::{
            DbTrustedPaths, PathTrust, init as init_trust, track_worktree_trust,
        };
        let (filesystem, project, root) = fake_project(cx).await;
        let external = PathBuf::from(util::path!("/restricted-external-android-java"));
        filesystem
            .insert_tree(&external, json!({"example":{"Different.java":JAVA}}))
            .await;
        let mut request = request(&project, &root, Some(&external), cx).await?;
        let (reached, resume) = gate(&mut request, TestPhase::Scanned);
        let task = project.update(cx, |project, cx| {
            project.capture_android_module_tree(request, cx)
        });
        reached.await?;
        let store = project.read_with(cx, |project, _| project.worktree_store());
        let external_id = project
            .read_with(cx, |project, cx| {
                project
                    .find_worktree(&external, cx)
                    .map(|(tree, _)| tree.read(cx).id())
            })
            .context("Captured external worktree")?;
        cx.update(|cx| {
            init_trust(DbTrustedPaths::default(), cx);
            track_worktree_trust(store.clone(), None, None, None, cx);
        });
        let trusted = cx
            .update(|cx| TrustedWorktrees::try_get_global(cx))
            .context("Fixture trust store")?;
        trusted.update(cx, |trusted, cx| {
            trusted.restrict(
                store.downgrade(),
                collections::HashSet::from_iter([PathTrust::Worktree(external_id)]),
                cx,
            )
        });
        assert!(trusted.read_with(cx, |trusted, _| {
            trusted.is_worktree_restricted(&store, external_id)
        }));
        resume
            .send(())
            .map_err(|_| anyhow::anyhow!("External trust gate dropped"))?;
        assert!(
            task.await.is_err(),
            "An explicitly restricted invisible source root must not publish"
        );
        assert_eq!(
            project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()),
            1
        );
        Ok(())
    }

    #[gpui::test]
    async fn external_trust_aba_cancels_pending_and_completed_captures_without_changing_origin(
        cx: &mut TestAppContext,
    ) {
        external_trust_aba_case(cx)
            .await
            .expect("External trust ABA fixture must complete");
    }

    async fn external_trust_aba_case(cx: &mut TestAppContext) -> Result<()> {
        use crate::trusted_worktrees::{DbTrustedPaths, init as init_trust, track_worktree_trust};
        let (filesystem, project, root) = fake_project(cx).await;
        let external = PathBuf::from(util::path!("/external-trust-aba-java"));
        filesystem
            .insert_tree(&external, json!({"example":{"Different.java":JAVA}}))
            .await;
        let store = project.read_with(cx, |project, _| project.worktree_store());
        cx.update(|cx| {
            init_trust(DbTrustedPaths::default(), cx);
            track_worktree_trust(store.clone(), None, None, None, cx);
        });
        let trusted = cx
            .update(|cx| TrustedWorktrees::try_get_global(cx))
            .context("Initialized trust store")?;
        for pending in [true, false] {
            let mut request = request(&project, &root, Some(&external), cx).await?;
            let origin_context = request.context_token.clone();
            let origin_model = request.model_token.clone();
            let origin_model_arc = project
                .read_with(cx, |project, _| project.android_model().model.clone())
                .context("Origin model identity")?;
            let origin_selection = project
                .read_with(cx, |project, _| project.android_model().selected.clone())
                .context("Origin selection identity")?;
            let handle = request.root;
            let (reached, resume) = gate(&mut request, TestPhase::Scanned);
            let mut resume = Some(resume);
            let task = project.update(cx, |project, cx| {
                project.capture_android_module_tree(request, cx)
            });
            reached.await?;
            let mut capture = None;
            let mut task = Some(task);
            if !pending {
                resume
                    .take()
                    .context("Completed capture gate")?
                    .send(())
                    .map_err(|_| anyhow::anyhow!("Completed capture gate dropped"))?;
                capture = Some(task.take().context("Completed capture task")?.await?);
            }
            let external_id = project
                .read_with(cx, |project, cx| {
                    project
                        .find_worktree(&external, cx)
                        .map(|(tree, _)| tree.read(cx).id())
                })
                .context("Exact external worktree")?;
            trusted.update(cx, |trusted, cx| {
                trusted.restrict(
                    store.downgrade(),
                    collections::HashSet::from_iter([PathTrust::Worktree(external_id)]),
                    cx,
                );
                trusted.trust(
                    &store,
                    collections::HashSet::from_iter([PathTrust::Worktree(external_id)]),
                    cx,
                );
            });
            assert!(!trusted.read_with(cx, |trusted, _| {
                trusted.is_worktree_restricted(&store, external_id)
            }));
            assert_eq!(
                project.read_with(cx, |project, _| project.android_context().token(handle)),
                Some(origin_context)
            );
            assert_eq!(
                project.read_with(cx, |project, _| project.android_model().token()),
                origin_model
            );
            assert!(project.read_with(cx, |project, _| {
                project
                    .android_model()
                    .model
                    .as_ref()
                    .is_some_and(|model| Arc::ptr_eq(model, &origin_model_arc))
            }));
            assert!(project.read_with(cx, |project, _| {
                project
                    .android_model()
                    .selected
                    .as_ref()
                    .is_some_and(|selected| Arc::ptr_eq(selected, &origin_selection))
            }));
            if let Some(task) = task {
                let error = match select(
                    Box::pin(task),
                    Box::pin(cx.executor().timer(Duration::from_millis(1))),
                )
                .await
                {
                    Either::Left((result, _)) => result
                        .err()
                        .context("Pending trust-ABA capture unexpectedly published")?,
                    Either::Right(_) => {
                        return Err(anyhow::anyhow!(
                            "External trust ABA did not wake the pending capture"
                        ));
                    }
                };
                assert!(error.to_string().contains("cancelled"));
                assert!(
                    resume
                        .take()
                        .context("Pending capture gate")?
                        .send(())
                        .is_err()
                );
            } else {
                let capture = capture.context("Completed capture")?;
                assert!(!project.read_with(cx, |project, cx| {
                    project.is_android_tree_capture_current(&capture, cx)
                }));
                assert!(
                    project
                        .read_with(cx, |project, cx| project.android_tree_project_path(
                            &capture,
                            &external.join("example/Different.java"),
                            cx
                        ))
                        .is_err()
                );
            }
            cx.executor().run_until_parked();
            assert!(!filesystem.watched_paths().contains(&external));
        }
        Ok(())
    }

    #[gpui::test]
    async fn loaded_java_root_expands_ignored_descendant_without_expanding_unrelated_project_tree(
        cx: &mut TestAppContext,
    ) {
        loaded_ignored_descendant_case(cx)
            .await
            .expect("Loaded-root ignored-source fixture must complete");
    }

    async fn loaded_ignored_descendant_case(cx: &mut TestAppContext) -> Result<()> {
        init(cx);
        cx.executor().allow_parking();
        let temporary = tempfile::TempDir::new()?;
        let root = temporary.path().join("project");
        let source_root = root.join("app/src/main/java");
        let ignored = source_root.join("ignored");
        let source = ignored.join("example/Different.java");
        let unrelated = root.join("other-editor-files");
        std::fs::create_dir_all(source.parent().context("Ignored source parent")?)?;
        std::fs::create_dir_all(&unrelated)?;
        std::fs::create_dir_all(root.join(".git/objects"))?;
        std::fs::create_dir_all(root.join(".git/refs/heads"))?;
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n")?;
        std::fs::write(
            root.join(".git/config"),
            "[core]\nrepositoryformatversion = 0\nbare = false\n",
        )?;
        std::fs::write(
            root.join(".gitignore"),
            "/app/src/main/java/ignored/\n/other-editor-files/\n",
        )?;
        std::fs::write(&source, JAVA)?;
        std::fs::write(unrelated.join("main.py"), "print(1)")?;
        std::fs::write(unrelated.join("index.html"), "<p>Hello</p>")?;
        let project = Project::test(RealFs::new(None, cx.executor()), [root.as_path()], cx).await;
        let (worktree, source_relative) = project
            .read_with(cx, |project, cx| project.find_worktree(&source_root, cx))
            .context("Loaded Java source root")?;
        let ignored_relative =
            RelPath::new(ignored.strip_prefix(&root)?, PathStyle::local())?.into_arc();
        let unrelated_relative =
            RelPath::new(unrelated.strip_prefix(&root)?, PathStyle::local())?.into_arc();
        assert_eq!(
            worktree.read_with(cx, |tree, _| tree
                .entry_for_path(&source_relative)
                .map(|entry| entry.kind)),
            Some(EntryKind::Dir)
        );
        assert_eq!(
            worktree.read_with(cx, |tree, _| tree
                .entry_for_path(&ignored_relative)
                .map(|entry| entry.kind)),
            Some(EntryKind::UnloadedDir)
        );
        assert_eq!(
            worktree.read_with(cx, |tree, _| tree
                .entry_for_path(&unrelated_relative)
                .map(|entry| entry.kind)),
            Some(EntryKind::UnloadedDir)
        );
        let request = request(&project, &root, None, cx).await?;
        let capture = project
            .update(cx, |project, cx| {
                project.capture_android_module_tree(request, cx)
            })
            .await?;
        assert!(
            capture
                .entries
                .get(&source)
                .is_some_and(|(_, entry)| entry.is_ignored)
        );
        assert!(capture.tree().tree.nodes().any(|node| matches!(&node.key, NodeKey::JavaClass { name, path, .. } if name == "ActualClass" && path == &source)));
        let physical = project.read_with(cx, |project, cx| {
            project.android_tree_project_path(&capture, &source, cx)
        })?;
        assert_eq!(
            project.read_with(cx, |project, cx| project
                .entry_for_path(&physical, cx)
                .map(|entry| entry.id)),
            capture.entries.get(&source).map(|(_, entry)| entry.id)
        );
        assert_eq!(
            worktree.read_with(cx, |tree, _| tree
                .entry_for_path(&ignored_relative)
                .map(|entry| entry.kind)),
            Some(EntryKind::Dir)
        );
        assert_eq!(
            worktree.read_with(cx, |tree, _| tree
                .entry_for_path(&unrelated_relative)
                .map(|entry| entry.kind)),
            Some(EntryKind::UnloadedDir)
        );
        assert!(!capture.entries.contains_key(&unrelated.join("main.py")));
        assert_eq!(
            std::fs::read_to_string(unrelated.join("main.py"))?,
            "print(1)"
        );
        assert_eq!(
            std::fs::read_to_string(unrelated.join("index.html"))?,
            "<p>Hello</p>"
        );
        Ok(())
    }

    #[gpui::test]
    async fn explicit_pending_cancel_wakes_capture_and_releases_external_watcher(
        cx: &mut TestAppContext,
    ) {
        pending_cancel_case(cx)
            .await
            .expect("Pending request cancellation fixture must complete");
    }

    async fn pending_cancel_case(cx: &mut TestAppContext) -> Result<()> {
        let (filesystem, project, root) = fake_project(cx).await;
        let external = PathBuf::from(util::path!("/pending-cancel-external-java"));
        filesystem
            .insert_tree(&external, json!({"example":{"Different.java":JAVA}}))
            .await;
        let mut request = request(&project, &root, Some(&external), cx).await?;
        let (reached, resume) = gate(&mut request, TestPhase::Scanned);
        let task = project.update(cx, |project, cx| {
            project.capture_android_module_tree(request.clone(), cx)
        });
        reached.await?;
        assert!(filesystem.watched_paths().contains(&external));
        request.cancel();
        let error = match select(
            Box::pin(task),
            Box::pin(cx.executor().timer(Duration::from_millis(1))),
        )
        .await
        {
            Either::Left((result, _)) => result
                .err()
                .context("Cancelled capture unexpectedly published")?,
            Either::Right(_) => {
                return Err(anyhow::anyhow!(
                    "Explicit cancellation did not wake the pending capture"
                ));
            }
        };
        assert!(error.to_string().contains("cancelled"));
        assert!(resume.send(()).is_err());
        cx.executor().run_until_parked();
        assert!(!filesystem.watched_paths().contains(&external));
        assert_eq!(
            project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()),
            1
        );
        Ok(())
    }

    #[gpui::test]
    async fn deleted_external_root_rejects_refresh_without_waiting_for_unreachable_revision(
        cx: &mut TestAppContext,
    ) {
        deleted_external_root_case(cx)
            .await
            .expect("Deleted external-root fixture must complete");
    }

    async fn deleted_external_root_case(cx: &mut TestAppContext) -> Result<()> {
        let (filesystem, project, root) = fake_project(cx).await;
        let external = PathBuf::from(util::path!("/deleted-external-java"));
        filesystem
            .insert_tree(&external, json!({"example":{"Different.java":JAVA}}))
            .await;
        let mut request = request(&project, &root, Some(&external), cx).await?;
        let (reached, resume) = gate(&mut request, TestPhase::BeforeScan);
        let task = project.update(cx, |project, cx| {
            project.capture_android_module_tree(request, cx)
        });
        reached.await?;
        filesystem.pause_events();
        filesystem
            .remove_dir(
                &external,
                fs::RemoveOptions {
                    recursive: true,
                    ..Default::default()
                },
            )
            .await?;
        resume
            .send(())
            .map_err(|_| anyhow::anyhow!("Deleted-root gate dropped"))?;
        let error = match select(
            Box::pin(task),
            Box::pin(cx.executor().timer(Duration::from_secs(1))),
        )
        .await
        {
            Either::Left((result, _)) => result
                .err()
                .context("Deleted external root unexpectedly published")?,
            Either::Right(_) => {
                return Err(anyhow::anyhow!(
                    "Deleted external-root refresh retained an unreachable scan wait"
                ));
            }
        };
        assert!(error.to_string().contains("removed"));
        filesystem.unpause_events_and_flush();
        cx.executor().run_until_parked();
        assert!(!filesystem.watched_paths().contains(&external));
        assert_eq!(
            project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()),
            1
        );
        Ok(())
    }

    struct ReleaseProbe {
        executor: BackgroundExecutor,
        released: Option<oneshot::Sender<bool>>,
        _owned_payload: Vec<String>,
    }

    impl Drop for ReleaseProbe {
        fn drop(&mut self) {
            if let Some(released) = self.released.take() {
                match released.send(self.executor.is_main_thread()) {
                    Ok(()) | Err(_) => {}
                }
            }
        }
    }

    fn tracked_payload(
        executor: &BackgroundExecutor,
    ) -> (Arc<BackgroundDrop<ReleaseProbe>>, oneshot::Receiver<bool>) {
        let (released, observed) = oneshot::channel();
        let payload = ReleaseProbe {
            executor: executor.clone(),
            released: Some(released),
            _owned_payload: vec!["Owned capture allocation".to_owned(); 16],
        };
        (
            Arc::new(BackgroundDrop::new(payload, executor.clone())),
            observed,
        )
    }

    #[gpui::test]
    async fn capture_payload_cleanup_runs_in_background_for_shared_results_errors_and_cancelled_tasks(
        cx: &mut TestAppContext,
    ) {
        payload_cleanup_case(cx)
            .await
            .expect("Capture allocation cleanup fixture must complete");
    }

    async fn payload_cleanup_case(cx: &mut TestAppContext) -> Result<()> {
        let executor = cx.executor();

        // A retained result can outlive its original owner. Replacing the first
        // foreground result must preserve that reference, and releasing the
        // final result must destroy its actual payload on the background lane.
        let (result, mut completed_release) = tracked_payload(&executor);
        let retained = result.clone();
        cx.spawn({
            let executor = executor.clone();
            move |_| async move {
                assert!(executor.is_main_thread());
                drop(result);
            }
        })
        .await;
        assert_eq!(completed_release.try_recv()?, None);
        cx.spawn({
            let executor = executor.clone();
            move |_| async move {
                assert!(executor.is_main_thread());
                drop(retained);
            }
        })
        .await;
        assert!(!completed_release.await?);

        // Error propagation tears down foreground locals before a result can be
        // published, including data already returned from a background capture.
        let (payload, error_release) = tracked_payload(&executor);
        let failed = cx
            .spawn({
                let executor = executor.clone();
                move |_| async move {
                    assert!(executor.is_main_thread());
                    let _owned = payload;
                    Err::<(), _>(anyhow::anyhow!("Capture failed after data acquisition"))
                }
            })
            .await;
        assert!(failed.is_err());
        assert!(!error_release.await?);

        // Cancel a real foreground GPUI task while it retains an acquired
        // payload across an await. TestDispatcher can execute both lanes on the
        // same OS thread; is_main_thread checks the dispatched lane instead.
        let (payload, mut pending_release) = tracked_payload(&executor);
        let (entered, reached) = oneshot::channel();
        let (resume, wait) = oneshot::channel::<()>();
        let pending = cx.spawn({
            let executor = executor.clone();
            move |_| async move {
                assert!(executor.is_main_thread());
                let _owned = payload;
                entered
                    .send(())
                    .map_err(|_| anyhow::anyhow!("Pending cleanup observer dropped"))?;
                wait.await?;
                Ok::<_, anyhow::Error>(())
            }
        });
        reached.await?;
        assert_eq!(pending_release.try_recv()?, None);
        cx.spawn({
            let executor = executor.clone();
            move |_| async move {
                assert!(executor.is_main_thread());
                drop(pending);
            }
        })
        .await;
        cx.executor().run_until_parked();
        assert!(resume.send(()).is_err());
        assert!(!pending_release.await?);
        Ok(())
    }
}
