use crate::android_build::{self, BuildEvent, BuildPanel, BuildStatus, BuildTab, CapturedProcessOutput};
use android_tools::project_context::{
    ActiveContext, ActiveContextToken, ContextCapabilities, ContextSnapshot, DiscoveryToken, ObservationPhase,
    OperationalReadiness, RootHandle, decode_context_output,
};
use anyhow::{Context as _, Result, ensure};
use futures::{FutureExt as _, channel::{mpsc, oneshot}, future::{BoxFuture, Shared}};
use gpui::{App, BackgroundExecutor, Context, Entity, EntityId, Global, InteractiveElement as _, Subscription, Task, WeakEntity, Window, actions};
use project::{Project, WorktreeId, git_store::{GitStoreEvent, RepositoryEvent}, trusted_worktrees::{TrustedWorktrees, TrustedWorktreesEvent}};
use std::{collections::HashMap, path::PathBuf, time::Duration};
use util::ResultExt as _;
use workspace::{Toast, Workspace, notifications::NotificationId};

actions!(project, [
    /// Evaluates this trusted Gradle project's applied plugins and targets.
    ImportGradleProject,
]);

#[derive(Default)]
struct Controllers(HashMap<EntityId, WeakEntity<ProjectContextController>>);
impl Global for Controllers {}

pub(crate) fn for_workspace(workspace: &WeakEntity<Workspace>, cx: &App) -> Option<Entity<ProjectContextController>> {
    cx.try_global::<Controllers>()?.0.get(&workspace.entity_id())?.upgrade()
}

pub(crate) fn register(
    workspace: &mut Workspace,
    build_panel: Entity<BuildPanel>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if !cx.has_global::<Controllers>() {
        cx.set_global(Controllers::default());
    }
    let project = workspace.project().clone();
    let weak_workspace = workspace.weak_handle();
    let controller = cx.new(|cx| ProjectContextController::new(weak_workspace.clone(), project, build_panel, window, cx));
    cx.global_mut::<Controllers>().0.insert(weak_workspace.entity_id(), controller.downgrade());
    workspace.register_action_renderer(move |element, _, _, cx| {
        if controller.read(cx).import_candidate(cx).is_some() {
            let controller = controller.clone();
            element.on_action(cx.listener(move |_, _: &ImportGradleProject, window, cx| {
                controller.update(cx, |controller, cx| controller.import(window, cx));
            }))
        } else {
            element
        }
    });
}

struct ImportOwner {
    root: RootHandle,
    active: ActiveContextToken,
    discovery: DiscoveryToken,
    session: u64,
}

type Cancellation = Shared<BoxFuture<'static, ()>>;

fn observer_directories(snapshot: &ContextSnapshot) -> Result<Vec<PathBuf>> {
    let mut directories = snapshot.build_logic_directories().to_vec();
    for layout in snapshot.build_layouts().iter().filter(|layout| snapshot.build_logic_directories().iter().any(|directory| layout.directory.starts_with(directory))) {
        directories.extend(layout.source_directories.iter().filter(|directory| snapshot.is_input(directory)).cloned());
    }
    directories.sort();
    directories.dedup();
    let candidates = directories.iter().cloned().collect::<std::collections::BTreeSet<_>>();
    directories.retain(|directory| !directory.ancestors().skip(1).any(|parent| candidates.contains(parent)));
    ensure!(directories.len() <= 4096, "Too many evaluated build-logic input observers");
    Ok(directories)
}

async fn evaluate(root: PathBuf, executor: BackgroundExecutor, output: mpsc::Sender<android_build::OutputLine>, cancelled: Cancellation) -> Result<CapturedProcessOutput> {
    let adapter = android_tools::project_context::prepare()?;
    let program = if cfg!(windows) { root.join("gradlew.bat") } else { PathBuf::from("/bin/sh") };
    let mut command = util::command::new_std_command(program);
    if !cfg!(windows) { command.arg("./gradlew"); }
    command.arg("--init-script").arg(adapter.path().join("context.gradle"))
        .arg(format!(":{}", android_tools::project_context::CONTEXT_TASK))
        .args(["--no-configuration-cache", "--console=plain"]).current_dir(&root);
    android_build::project_context_output(command, &executor, Duration::from_secs(300), output, cancelled).await
}

pub(crate) struct ProjectContextController {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    build_panel: Entity<BuildPanel>,
    active: ActiveContext,
    selected_root: Option<WorktreeId>,
    import_owner: Option<ImportOwner>,
    last_import_session: Option<(RootHandle, u64)>,
    cancel: Option<oneshot::Sender<()>>,
    task: Option<Task<()>>,
    reconcile_scheduled: bool,
    _subscriptions: Vec<Subscription>,
}

impl ProjectContextController {
    fn new(workspace: WeakEntity<Workspace>, project: Entity<Project>, build_panel: Entity<BuildPanel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        let workspace_id = workspace.entity_id();
        subscriptions.push(cx.on_release(move |_, cx| {
            if cx.has_global::<Controllers>() { cx.global_mut::<Controllers>().0.remove(&workspace_id); }
        }));
        if let Some(workspace) = workspace.upgrade() {
            subscriptions.push(cx.subscribe_in(&workspace, window, |this, _, event, window, cx| {
                if matches!(event, workspace::Event::ActiveItemChanged) {
                    this.clear_active(cx);
                    this.request_reconcile(window, cx);
                } else if matches!(event, workspace::Event::Activate) {
                    this.request_reconcile(window, cx);
                }
            }));
        }
        subscriptions.push(cx.subscribe_in(&project, window, |this, _, event, window, cx| {
            if matches!(event, project::Event::AndroidProjectContextChanged | project::Event::WorktreeAdded(_) | project::Event::WorktreeRemoved(_) | project::Event::WorktreePathsChanged { .. } | project::Event::WorktreeOrderChanged) {
                this.request_reconcile(window, cx);
            }
        }));
        let git_store = project.read(cx).git_store().clone();
        subscriptions.push(cx.subscribe_in(&git_store, window, |this, store, event, window, cx| {
            match event {
                GitStoreEvent::ActiveRepositoryChanged(_) if window.is_window_active() => {
                    this.clear_active(cx);
                    if let Some(repository) = store.read(cx).active_repository() {
                        let path = repository.read(cx).work_directory_abs_path.clone();
                        this.selected_root = this.project.read(cx).find_worktree(&path, cx).map(|(worktree, _)| worktree.read(cx).id());
                    }
                    this.request_reconcile(window, cx);
                }
                GitStoreEvent::RepositoryUpdated(id, RepositoryEvent::HeadChanged, _) => {
                    if let Some(repository) = store.read(cx).repositories().get(id) {
                        let directory = repository.read(cx).work_directory_abs_path.clone();
                        this.project.update(cx, |project, cx| project.invalidate_android_context_for_repository(&directory, cx));
                    }
                    this.request_reconcile(window, cx);
                }
                _ => {}
            }
        }));
        if let Some(trust) = TrustedWorktrees::try_get_global(cx) {
            subscriptions.push(cx.subscribe_in(&trust, window, |this, _, event, window, cx| {
                let (TrustedWorktreesEvent::Trusted(store, _) | TrustedWorktreesEvent::Restricted(store, _)) = event;
                if *store == this.project.read(cx).worktree_store().downgrade() {
                    this.clear_active(cx);
                    this.request_reconcile(window, cx);
                }
            }));
        }
        subscriptions.push(cx.subscribe_in(&build_panel, window, |this, _, event, window, cx| {
            match event {
                BuildEvent::Stop(BuildTab::Sync) if this.import_owner.is_some() => this.cancel_import(cx),
                BuildEvent::Rerun(BuildTab::Sync) if this.owns_sync_session(cx) => {
                    if let Err(error) = this.reconcile(cx) {
                        this.notify_import_error(error, cx);
                    } else if this.last_import_session.is_some_and(|(root, _)| this.active.root() == Some(root)) {
                        this.import(window, cx);
                    } else {
                        this.notify_import_error(anyhow::anyhow!("Select the original trusted Gradle root before rerunning its import"), cx);
                    }
                }
                _ => {}
            }
        }));
        let mut this = Self { workspace, project, build_panel, active: ActiveContext::default(), selected_root: None,
            import_owner: None, last_import_session: None, cancel: None, task: None, reconcile_scheduled: false, _subscriptions: subscriptions };
        // The Workspace is still borrowed by observe_new. Read it only after that
        // callback returns, and coalesce root notifications without polling.
        this.request_reconcile(window, cx);
        this
    }

    fn request_reconcile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reconcile_scheduled { return; }
        self.reconcile_scheduled = true;
        let controller = cx.entity().downgrade();
        window.defer(cx, move |_, cx| {
            controller.update(cx, |controller, cx| {
                controller.reconcile_scheduled = false;
                controller.reconcile(cx).log_err();
            }).log_err();
        });
    }

    fn clear_active(&mut self, cx: &mut Context<Self>) {
        self.cancel_import(cx);
        self.active.select(None, None).log_err();
        cx.notify();
    }

    fn cancel_import(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.cancel.take() {
            if cancel.send(()).is_err() { log::debug!("Gradle context operation already stopped"); }
        }
        if let Some(owner) = self.import_owner.take() {
            self.project.update(cx, |project, cx| project.finish_failed_android_context_import(&owner.discovery, cx)).log_err();
            self.build_panel.update(cx, |panel, cx| panel.finish(BuildTab::Sync, owner.session, BuildStatus::Cancelled, "Gradle import cancelled because its project context changed.".into(), cx));
        }
        self.task = None;
    }

    fn reconcile(&mut self, cx: &mut Context<Self>) -> Result<()> {
        let project = self.project.clone();
        let roots = project.read(cx).visible_worktrees(cx).filter(|worktree| !worktree.read(cx).is_single_file())
            .map(|worktree| (worktree.read(cx).id(), worktree.read(cx).abs_path())).collect::<Vec<_>>();
        for (id, _) in &roots {
            let trusted = TrustedWorktrees::try_get_global(cx).is_some_and(|trust| {
                trust.update(cx, |trust, cx| trust.can_trust(&project.read(cx).worktree_store(), *id, cx))
            });
            project.update(cx, |project, cx| project.ensure_android_context(*id, trusted, cx))?;
        }
        let workspace = self.workspace.upgrade().context("Project window closed")?;
        let item = workspace.read(cx).active_item(cx);
        let path = item.as_ref().and_then(|item| item.project_path(cx));
        let (worktree, owner) = if let Some(path) = path {
            let worktree = project.read(cx).worktree_for_id(path.worktree_id, cx).context("Active editor root was removed")?;
            (Some(path.worktree_id), Some(worktree.read(cx).abs_path().join(path.path.as_std_path())))
        } else if item.is_some() {
            (None, None)
        } else {
            let selected = self.selected_root.filter(|id| roots.iter().any(|(root, _)| root == id))
                .or_else(|| (roots.len() == 1).then(|| roots.first().map(|(id, _)| *id)).flatten());
            (selected, None)
        };
        let handle = worktree.and_then(|id| project.read(cx).android_context().handle(id.to_proto()));
        self.active.select(handle, owner)?;
        if worktree.is_some() { self.selected_root = worktree; }
        if self.import_owner.as_ref().is_some_and(|owner| !self.active.is_current(&owner.active, project.read(cx).android_context())) {
            self.cancel_import(cx);
        }
        workspace.update(cx, |_, cx| cx.notify());
        cx.notify();
        Ok(())
    }

    pub(crate) fn root(&self, cx: &App) -> Option<PathBuf> {
        let store = self.project.read(cx).android_context();
        let handle = self.active.root()?;
        store.token(handle)?;
        store.root_path(handle).map(PathBuf::from)
    }

    pub(crate) fn capabilities(&self, readiness: OperationalReadiness<'_>, cx: &App) -> ContextCapabilities {
        self.active.capabilities(self.project.read(cx).android_context(), readiness)
    }

    pub(crate) fn owns_sync_session(&self, cx: &App) -> bool {
        self.last_import_session.is_some_and(|(_, session)| self.build_panel.read(cx).session_id(BuildTab::Sync) == Some(session))
    }

    fn import_candidate(&self, cx: &App) -> Option<PathBuf> {
        let root = self.root(cx)?;
        (self.project.read(cx).is_local() && android_tools::is_gradle_project(&root)).then_some(root)
    }

    fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.import_owner.is_some() { return; }
        if let Err(error) = self.begin_import(window, cx) {
            self.notify_import_error(error, cx);
        }
    }

    fn notify_import_error(&self, error: anyhow::Error, cx: &mut Context<Self>) {
        log::error!("Gradle import did not start: {error:#}");
        let workspace = self.workspace.clone();
        let message = format!("Gradle import did not start: {error:#}");
        cx.defer(move |cx| {
            workspace.update(cx, |workspace, cx| workspace.show_toast(Toast::new(NotificationId::unique::<ImportGradleProject>(), message).autohide(), cx)).log_err();
        });
    }

    fn begin_import(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<()> {
        self.reconcile(cx)?;
        let root = self.import_candidate(cx).context("Select a trusted Gradle root before importing")?;
        let handle = self.active.root().context("No active project root")?;
        let discovery = self.project.update(cx, |project, cx| project.begin_android_context_import(handle, cx))?;
        let active = self.active.discovery_token(self.project.read(cx).android_context()).context("Gradle import has no current trusted owner")?;
        let (session, output, logs) = self.build_panel.update(cx, |panel, cx| panel.begin(BuildTab::Sync, format!("Import Gradle project {}", root.display()), false, window, cx));
        self.import_owner = Some(ImportOwner { root: handle, active: active.clone(), discovery: discovery.clone(), session });
        self.last_import_session = Some((handle, session));
        let (cancel, cancelled) = oneshot::channel();
        self.cancel = Some(cancel);
        let cancelled = cancelled.map(|_| ()).boxed().shared();
        let executor = cx.background_executor().clone();
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let expected_root = root.clone();
            let result = async {
                let project = this.update(cx, |this, _| this.project.clone())?;
                // Observe the selected root before evaluation. Missing buildSrc
                // and custom output directories can then be tracked without a
                // second evaluation or relying on ignored Worktree entries.
                project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery.clone(), vec![root.clone()], cx)).await?;
                this.update(cx, |this, cx| {
                    ensure!(this.active.is_current(&active, this.project.read(cx).android_context()), "Project context changed before evaluation");
                    Ok::<_, anyhow::Error>(())
                })??;
                let first = cx.background_spawn(evaluate(root.clone(), executor.clone(), output.clone(), cancelled.clone())).await?;
                let mut result = first;
                if let CapturedProcessOutput::Completed { stdout, status } = &result {
                    if status.success() {
                        let snapshots = decode_context_output(stdout, &root)?;
                        let snapshot = snapshots.last().context("Gradle context has no final observation")?;
                        ensure!(snapshot.phase() == ObservationPhase::Complete, "Gradle context evaluation did not complete");
                        let project = this.update(cx, |this, cx| {
                            ensure!(this.active.is_current(&active, this.project.read(cx).android_context()), "Project context changed before observer installation");
                            this.project.update(cx, |project, _| project.verify_android_context_inputs(&discovery, snapshot))?;
                            Ok::<_, anyhow::Error>(this.project.clone())
                        })??;
                        let directories = observer_directories(snapshot)?;
                        let filesystem = project.read_with(cx, |project, _| project.fs().clone());
                        let directories = cx.background_spawn(async move {
                            let mut existing = Vec::new();
                            for directory in directories {
                                if filesystem.metadata(&directory).await?.is_some_and(|metadata| metadata.is_dir) { existing.push(directory); }
                            }
                            Ok::<_, anyhow::Error>(existing)
                        }).await?;
                        let observer_task = project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery.clone(), directories, cx));
                        let observers_added = observer_task.await?;
                        this.update(cx, |this, cx| {
                            ensure!(this.active.is_current(&active, this.project.read(cx).android_context()), "Project context changed while establishing observers");
                            Ok::<_, anyhow::Error>(())
                        })??;
                        // First observations only establish provenance. Newly owned
                        // watchers require one bounded verification evaluation; no
                        // capabilities are published between the two evaluations.
                        if observers_added {
                            result = cx.background_spawn(evaluate(root.clone(), executor, output.clone(), cancelled)).await?;
                        }
                    }
                }
                if let CapturedProcessOutput::Completed { stdout, status } = &result {
                    if status.success() {
                        let snapshots = decode_context_output(stdout, &root)?;
                        let snapshot = snapshots.last().context("Gradle verification emitted no context")?;
                        let directories = observer_directories(snapshot)?;
                        ensure!(project.read_with(cx, |project, _| directories.iter().all(|directory| project.android_context_observes(handle, directory))),
                            "Gradle evaluation introduced unwatched or unavailable external inputs; import again explicitly after establishing their directories");
                    }
                }
                Ok::<_, anyhow::Error>(result)
            }.await;
            drop(output);
            logs.await;
            this.update_in(cx, |this, _, cx| {
                let Some(owner) = &this.import_owner else { return; };
                if owner.session != session || owner.root != handle { return; }
                let result = result.and_then(|result| {
                    ensure!(this.active.is_current(&active, this.project.read(cx).android_context()), "Discarded stale Gradle import result");
                    match result {
                        CapturedProcessOutput::Cancelled => Ok(None),
                        CapturedProcessOutput::Completed { stdout, status } => {
                            let snapshots = decode_context_output(&stdout, &expected_root)?;
                            let complete = snapshots.last().is_some_and(|snapshot| snapshot.phase() == ObservationPhase::Complete);
                            // A failing command cannot publish operational capabilities,
                            // even if a plugin printed a complete record before failing.
                            ensure!(!complete || status.success(), "Gradle failed after context evaluation ({status})");
                            for snapshot in snapshots {
                                this.project.update(cx, |project, cx| project.publish_android_context(&this.active, &active, &discovery, snapshot, cx))?;
                            }
                            if !status.success() {
                                anyhow::bail!("Gradle import failed ({status}); evaluated plugin observations were retained for an explicit retry.");
                            }
                            ensure!(complete, "Gradle did not complete project context evaluation");
                            Ok(Some(()))
                        }
                    }
                });
                if !matches!(result, Ok(Some(()))) {
                    this.project.update(cx, |project, cx| project.finish_failed_android_context_import(&discovery, cx)).log_err();
                }
                let (status, message) = match result {
                    Ok(Some(())) => (BuildStatus::Succeeded, "Gradle project imported.".to_owned()),
                    Ok(None) => (BuildStatus::Cancelled, "Gradle import cancelled.".to_owned()),
                    Err(error) => (BuildStatus::Failed, format!("{error:#}")),
                };
                this.build_panel.update(cx, |panel, cx| panel.finish(BuildTab::Sync, session, status, message, cx));
                this.import_owner = None;
                this.cancel = None;
                this.task = None;
                cx.notify();
            }).log_err();
        }));
        cx.notify();
        Ok(())
    }
}
