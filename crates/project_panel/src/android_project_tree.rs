use android_tools::{
    evaluated_tree_inputs::{PreparedLiveModulePlan, prepare_live_module_plan},
    project_model::{ModelState, ModelToken},
};
use android_ui::{AndroidTreeContext, AndroidTreeOwner};
use anyhow::{Context as _, Result, ensure};
use gpui::{
    AnyWindowHandle, App, BackgroundExecutor, Context, Entity, Subscription, Task, WeakEntity,
    Window,
};
use project::{
    Project,
    android_project_tree::{AndroidTreeCaptureRequest, CapturedAndroidModuleTree},
    trusted_worktrees::{PathTrust, TrustedWorktrees, TrustedWorktreesEvent},
};
use std::sync::Arc;
use util::ResultExt as _;
use workspace::{MultiWorkspace, MultiWorkspaceEvent, Workspace};

struct BackgroundOwned<T: Send + 'static> {
    value: Option<T>,
    executor: BackgroundExecutor,
}

impl<T: Send + 'static> BackgroundOwned<T> {
    fn new(value: T, executor: BackgroundExecutor) -> Self {
        Self {
            value: Some(value),
            executor,
        }
    }

    fn value(&self) -> Result<&T> {
        self.value
            .as_ref()
            .context("Android tree data was released")
    }
}

impl<T: Send + 'static> Drop for BackgroundOwned<T> {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            // An obsolete preparation can retain the last model/wire-record Arc.
            // Cancelling its foreground waiter must not destroy those maps there.
            if self.executor.is_main_thread() {
                self.executor.spawn(async move { drop(value) }).detach();
            } else {
                drop(value);
            }
        }
    }
}

type PreparedPlan = Arc<BackgroundOwned<PreparedLiveModulePlan>>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct RequestOwner {
    context: AndroidTreeOwner,
    model: ModelToken,
    owner_generation: u64,
    request_generation: u64,
}

struct PendingCapture {
    owner: RequestOwner,
    refresh_revision: u64,
    is_retry: bool,
    plan: Option<PreparedPlan>,
    request: Option<AndroidTreeCaptureRequest>,
}

struct PublishedTree {
    owner: RequestOwner,
    plan: PreparedPlan,
    capture: CapturedAndroidModuleTree,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Status {
    Unavailable,
    Loading,
    Ready,
    Stale,
    Failed(Arc<str>),
}

pub(crate) struct AndroidProjectTree {
    workspace: WeakEntity<Workspace>,
    window: AnyWindowHandle,
    project: Entity<Project>,
    context: Option<Entity<AndroidTreeContext>>,
    context_subscription: Option<Subscription>,
    window_workspace_subscription: Option<(WeakEntity<MultiWorkspace>, Subscription)>,
    owner: Option<AndroidTreeOwner>,
    owner_generation: u64,
    request_generation: u64,
    refresh_revision: u64,
    attempted: Option<(ModelToken, u64)>,
    retry_next: bool,
    pending: Option<PendingCapture>,
    task: Option<Task<()>>,
    published: Option<PublishedTree>,
    status: Status,
    reconcile_scheduled: bool,
    _subscriptions: Vec<Subscription>,
    #[cfg(test)]
    completion_for_test: Option<
        futures::channel::oneshot::Sender<(RequestOwner, Result<CapturedAndroidModuleTree>)>,
    >,
}

impl AndroidProjectTree {
    pub(crate) fn new(
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = vec![cx.observe_in(&project, window, |this, _, window, cx| {
            this.request_reconcile(window, cx);
        })];
        subscriptions.push(
            cx.subscribe_in(&project, window, |this, _, event, window, cx| {
                if matches!(
                    event,
                    project::Event::WorktreeUpdatedEntries(..)
                        | project::Event::WorktreeAdded(_)
                        | project::Event::WorktreeRemoved(_)
                        | project::Event::WorktreePathsChanged { .. }
                        | project::Event::WorktreeOrderChanged
                ) {
                    if let Some(revision) = this.refresh_revision.checked_add(1) {
                        this.refresh_revision = revision;
                    } else {
                        this.clear(cx);
                        log::error!("Android tree refresh revision exhausted");
                        return;
                    }
                    this.request_reconcile(window, cx);
                }
            }),
        );
        if let Some(trust) = TrustedWorktrees::try_get_global(cx) {
            subscriptions.push(
                cx.subscribe_in(&trust, window, |this, _, event, window, cx| {
                    let (TrustedWorktreesEvent::Trusted(store, _)
                    | TrustedWorktreesEvent::Restricted(store, _)) = event;
                    if *store == this.project.read(cx).worktree_store().downgrade() {
                        // Trust notifications run while the trust entity is leased.
                        // Cancel from captured paths now; re-read trust after deferral.
                        if let TrustedWorktreesEvent::Restricted(_, paths) = event
                            && paths
                                .iter()
                                .any(|path| this.restriction_affects_owner(path, cx))
                        {
                            this.clear(cx);
                        }
                        this.request_reconcile(window, cx);
                    }
                }),
            );
        }
        if let Some(workspace) = workspace.upgrade() {
            subscriptions.push(cx.subscribe_in(
                &workspace,
                window,
                |this, _, event, window, cx| {
                    if matches!(
                        event,
                        workspace::Event::ActiveProjectPathChanged(_) | workspace::Event::Activate
                    ) {
                        // The workspace can still be leased by its action listener.
                        this.request_reconcile(window, cx);
                    }
                },
            ));
        }
        let mut this = Self {
            workspace,
            window: window.window_handle(),
            project,
            context: None,
            context_subscription: None,
            window_workspace_subscription: None,
            owner: None,
            owner_generation: 0,
            request_generation: 0,
            refresh_revision: 0,
            attempted: None,
            retry_next: false,
            pending: None,
            task: None,
            published: None,
            status: Status::Unavailable,
            reconcile_scheduled: false,
            _subscriptions: subscriptions,
            #[cfg(test)]
            completion_for_test: None,
        };
        this.request_reconcile(window, cx);
        this
    }

    fn request_reconcile(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.reconcile_scheduled {
            return;
        }
        self.reconcile_scheduled = true;
        cx.defer_in(window, |this, window, cx| {
            this.reconcile_scheduled = false;
            if let Err(error) = this.reconcile(window, cx) {
                this.cancel_pending();
                this.set_status(Status::Failed(format!("{error:#}").into()), cx);
                log::error!("Android project tree refresh failed: {error:#}");
            }
        });
    }

    fn window_is_current(&self, window: &Window, cx: &App) -> bool {
        window.window_handle() == self.window
            && Workspace::for_window(window, cx)
                .or_else(|| window.root::<Workspace>().flatten())
                .is_some_and(|workspace| {
                    workspace.entity_id() == self.workspace.entity_id()
                        && workspace.read(cx).project() == &self.project
                })
    }

    fn bind_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let context = AndroidTreeContext::for_workspace(&self.workspace, cx);
        if self.context == context {
            return;
        }
        self.context_subscription = context.as_ref().map(|context| {
            cx.observe_in(context, window, |this, _, window, cx| {
                this.request_reconcile(window, cx);
            })
        });
        self.context = context;
    }

    fn bind_window_workspace(&mut self, window: &Window, cx: &mut Context<Self>) {
        let multi_workspace = window.root::<MultiWorkspace>().flatten();
        if self
            .window_workspace_subscription
            .as_ref()
            .map(|(workspace, _)| workspace.entity_id())
            == multi_workspace.as_ref().map(Entity::entity_id)
        {
            return;
        }
        self.window_workspace_subscription = multi_workspace.map(|workspace| {
            let subscription = cx.subscribe_in(&workspace, window, |this, _, event, window, cx| {
                if matches!(event, MultiWorkspaceEvent::ActiveWorkspaceChanged { .. })
                    || matches!(event, MultiWorkspaceEvent::WorkspaceRemoved(id)
                        if *id == this.workspace.entity_id())
                {
                    this.clear(cx);
                    this.request_reconcile(window, cx);
                }
            });
            (workspace.downgrade(), subscription)
        });
    }

    fn restriction_affects_owner(&self, path: &PathTrust, cx: &App) -> bool {
        let Some(owner) = &self.owner else {
            return false;
        };
        let changed = match path {
            PathTrust::Worktree(id) => {
                if id.to_proto() == owner.root().worktree() || owner.source_worktree() == Some(*id)
                {
                    return true;
                }
                self.project
                    .read(cx)
                    .worktree_for_id(*id, cx)
                    .map(|worktree| worktree.read(cx).abs_path())
            }
            PathTrust::AbsPath(path) => Some(Arc::from(path.as_path())),
        };
        let Some(changed) = changed else {
            return false;
        };
        if owner.root_path().starts_with(changed.as_ref()) || changed.starts_with(owner.root_path())
        {
            return true;
        }
        let affected_plan = |plan: &PreparedPlan| {
            plan.value().is_ok_and(|plan| {
                plan.plan().source_roots().iter().any(|root| {
                    root.path.starts_with(changed.as_ref()) || changed.starts_with(&root.path)
                })
            })
        };
        self.pending
            .as_ref()
            .and_then(|pending| pending.plan.as_ref())
            .is_some_and(affected_plan)
            || self
                .published
                .as_ref()
                .is_some_and(|published| affected_plan(&published.plan))
    }

    fn set_status(&mut self, status: Status, cx: &mut Context<Self>) {
        if self.status != status {
            self.status = status;
            cx.notify();
        }
    }

    fn cancel_pending(&mut self) {
        if let Some(pending) = self.pending.take()
            && let Some(request) = pending.request
        {
            request.cancel();
        }
        self.task = None;
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        let changed = self.owner.is_some()
            || self.pending.is_some()
            || self.published.is_some()
            || self.task.is_some();
        self.cancel_pending();
        self.owner = None;
        self.attempted = None;
        self.retry_next = false;
        self.published = None;
        self.set_status(Status::Unavailable, cx);
        if changed {
            cx.notify();
        }
    }

    fn next_request(&mut self) -> Result<u64> {
        self.request_generation = self
            .request_generation
            .checked_add(1)
            .context("Android tree request generation exhausted")?;
        Ok(self.request_generation)
    }

    fn reconcile(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<()> {
        self.bind_window_workspace(window, cx);
        if !self.window_is_current(window, cx) {
            self.clear(cx);
            return Ok(());
        }
        self.bind_context(window, cx);
        let owner = self
            .context
            .as_ref()
            .and_then(|context| context.read(cx).checked_owner(cx))
            .filter(|owner| {
                owner.workspace() == self.workspace.entity_id()
                    && owner.project() == self.project.entity_id()
            });
        if owner != self.owner {
            self.clear(cx);
            self.owner_generation = self
                .owner_generation
                .checked_add(1)
                .context("Android tree owner generation exhausted")?;
            self.next_request()?;
            self.owner = owner;
        }
        let Some(owner) = self.owner.clone() else {
            return Ok(());
        };
        let (model, selected, root_is_current) = {
            let state = self.project.read(cx).android_model();
            let selected = state
                .selected
                .as_ref()
                .filter(|selected| {
                    state
                        .model
                        .as_ref()
                        .is_some_and(|model| Arc::ptr_eq(model, &selected.model))
                })
                .map(|selected| selected.selected.clone());
            (
                state.token(),
                selected,
                state.root() == Some(owner.root_path()),
            )
        };
        if !root_is_current || selected.is_none() {
            if self.pending.is_some() {
                self.cancel_pending();
                self.next_request()?;
            }
            self.set_status(
                if self.published.is_some() {
                    Status::Stale
                } else {
                    Status::Unavailable
                },
                cx,
            );
            return Ok(());
        }
        if self.current_capture(window, cx).is_some() {
            self.set_status(Status::Ready, cx);
            return Ok(());
        }
        if let Some(pending) = &self.pending {
            if pending.owner.model == model && self.request_is_current(&pending.owner, window, cx) {
                return Ok(());
            }
            self.cancel_pending();
        }
        if !self.retry_next
            && self.attempted.as_ref() == Some(&(model.clone(), self.refresh_revision))
        {
            return Ok(());
        }
        let state = BackgroundOwned::<ModelState>::new(
            self.project.read(cx).android_model().clone(),
            cx.background_executor().clone(),
        );
        let selected = selected.context("Android tree selection is unavailable")?;
        let request_generation = self.next_request()?;
        let key = RequestOwner {
            context: owner,
            model,
            owner_generation: self.owner_generation,
            request_generation,
        };
        self.attempted = Some((key.model.clone(), self.refresh_revision));
        self.pending = Some(PendingCapture {
            owner: key.clone(),
            refresh_revision: self.refresh_revision,
            is_retry: std::mem::take(&mut self.retry_next),
            plan: None,
            request: None,
        });
        self.set_status(Status::Loading, cx);
        let executor = cx.background_executor().clone();
        let preparation = executor.clone().spawn({
            let selection = key.model.clone();
            async move {
                let plan = prepare_live_module_plan(state.value()?, &selection, selected, true)?;
                Ok::<_, anyhow::Error>(Arc::new(BackgroundOwned::new(plan, executor)))
            }
        });
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let prepared = preparation.await;
            let capture = this.update_in(cx, |this, window, cx| {
                this.begin_capture(&key, prepared, window, cx)
            });
            let Some(capture) = capture.log_err().flatten() else {
                return;
            };
            let result = capture.await;
            #[cfg(test)]
            if let Some(completion) = this
                .update_in(cx, |this, _, _| this.completion_for_test.take())
                .log_err()
                .flatten()
            {
                completion
                    .send((key, result))
                    .map_err(|_| anyhow::anyhow!("Android tree completion observer was dropped"))
                    .log_err();
                return;
            }
            this.update_in(cx, |this, window, cx| {
                this.finish_capture(&key, result, window, cx);
            })
            .log_err();
        }));
        Ok(())
    }

    fn request_is_current(&self, owner: &RequestOwner, window: &Window, cx: &App) -> bool {
        self.window_is_current(window, cx)
            && self.owner_generation == owner.owner_generation
            && self.request_generation == owner.request_generation
            && self.owner.as_ref() == Some(&owner.context)
            && self
                .context
                .as_ref()
                .is_some_and(|context| context.read(cx).is_current(&owner.context, cx))
            && self
                .project
                .read(cx)
                .android_model()
                .is_current(&owner.model)
    }

    fn begin_capture(
        &mut self,
        owner: &RequestOwner,
        prepared: Result<PreparedPlan>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<CapturedAndroidModuleTree>>> {
        if !self.request_is_current(owner, window, cx)
            || self
                .pending
                .as_ref()
                .is_none_or(|pending| &pending.owner != owner)
        {
            return None;
        }
        let prepare = || {
            let prepared = prepared?;
            let plan = prepared.value()?;
            plan.ensure_current(self.project.read(cx).android_model())?;
            ensure!(
                plan.model_token() == &owner.model,
                "Android tree preparation changed its original selection token"
            );
            let request = AndroidTreeCaptureRequest::new(
                owner.context.root(),
                owner.context.context_token().clone(),
                plan.model_token().clone(),
                plan.plan().clone(),
                owner.owner_generation,
                owner.request_generation,
            );
            Ok::<_, anyhow::Error>((prepared, request))
        };
        match prepare() {
            Ok((plan, request)) => {
                let task = self.project.update(cx, |project, cx| {
                    project.capture_android_module_tree(request.clone(), cx)
                });
                if let Some(pending) = &mut self.pending {
                    pending.plan = Some(plan);
                    pending.request = Some(request);
                }
                Some(task)
            }
            Err(error) => {
                self.cancel_pending();
                self.attempted = Some((owner.model.clone(), self.refresh_revision));
                self.set_status(Status::Failed(format!("{error:#}").into()), cx);
                log::error!("Android project tree preparation unavailable: {error:#}");
                None
            }
        }
    }

    fn finish_capture(
        &mut self,
        owner: &RequestOwner,
        result: Result<CapturedAndroidModuleTree>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| &pending.owner != owner)
        {
            return;
        }
        let result = result.and_then(|capture| {
            ensure!(
                self.request_is_current(owner, window, cx),
                "Android tree owner changed before publication"
            );
            let pending = self
                .pending
                .as_ref()
                .context("Android tree request stopped")?;
            let plan = pending
                .plan
                .as_ref()
                .context("Android tree plan is unavailable")?;
            plan.value()?
                .ensure_current(self.project.read(cx).android_model())?;
            ensure!(
                capture.owner_generation() == owner.owner_generation
                    && capture.request_generation() == owner.request_generation
                    && self
                        .project
                        .read(cx)
                        .is_android_tree_capture_current(&capture, cx),
                "Android tree capture changed before publication"
            );
            Ok((plan.clone(), capture))
        });
        let retry = self.pending.as_ref().is_some_and(|pending| {
            !pending.is_retry && pending.refresh_revision != self.refresh_revision
        });
        self.pending = None;
        self.task = None;
        self.attempted = Some((owner.model.clone(), self.refresh_revision));
        match result {
            Ok((plan, capture)) => {
                self.retry_next = false;
                self.published = Some(PublishedTree {
                    owner: owner.clone(),
                    plan,
                    capture,
                });
                self.set_status(Status::Ready, cx);
                cx.notify();
            }
            Err(error) => {
                // A capture includes its own refresh events. Consume those at
                // completion and allow only one follow-up for a concurrent edit.
                self.retry_next = retry && self.request_is_current(owner, window, cx);
                self.set_status(Status::Failed(format!("{error:#}").into()), cx);
                log::error!("Android project tree capture failed: {error:#}");
            }
        }
        self.request_reconcile(window, cx);
    }

    pub(crate) fn current_capture(
        &self,
        window: &Window,
        cx: &App,
    ) -> Option<&CapturedAndroidModuleTree> {
        let published = self.published.as_ref()?;
        (self.request_is_current(&published.owner, window, cx)
            && published
                .plan
                .value()
                .ok()?
                .ensure_current(self.project.read(cx).android_model())
                .is_ok()
            && self
                .project
                .read(cx)
                .is_android_tree_capture_current(&published.capture, cx))
        .then_some(&published.capture)
    }
}

impl Drop for AndroidProjectTree {
    fn drop(&mut self) {
        self.cancel_pending();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use android_tools::{
        evaluated_tree_inputs::EvaluatedTreeInputs,
        project_context::{ActiveContext, PluginId, decode_context_record},
        project_model::{MODEL_OUTPUT_PREFIX, VariantId},
        project_tree::NodeKey,
    };
    use futures::channel::oneshot;
    use gpui::{TestAppContext, VisualTestContext};
    use project::{FakeFs, Fs as _, WorktreeId, trusted_worktrees};
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use workspace::AppState;

    const ROOT: &str = "/tree-owner";
    const GENERIC: &str = "/tree-generic";
    const MAIN: &str = "/tree-owner/app/src/main/java/example/Main.java";
    const GENERATED: &str =
        "/tree-owner/app/build/generated/ap_generated_sources/demoDebug/out/example/Generated.java";
    const ORIGINAL_ROOT: &str =
        "/workspace/android-studio-artifacts/source-providers-smoke/project";
    const EXPORTED_MODEL: &str = include_str!(
        "../../android_tools/test_data/evaluated_tree_inputs/attempt2-default-complete-wire-model.json"
    );

    // Supplemental host-race fixtures use the production wire decoder and live
    // FakeFs worktree scans. They do not run Gradle or port the six original tests.
    async fn fixture(cx: &mut TestAppContext) -> Result<(Entity<Project>, Arc<FakeFs>)> {
        cx.update(|cx| {
            let state = AppState::test(cx);
            editor::init(cx);
            crate::init(cx);
            workspace::init(state, cx);
            trusted_worktrees::init(Default::default(), cx);
            android_ui::init(cx);
        });
        let filesystem = FakeFs::new(cx.executor());
        filesystem
            .insert_tree(
                ROOT,
                json!({
                    "app": {
                        "src": {"main": {
                            "AndroidManifest.xml": "<manifest package=\"example\"/>",
                            "java": {"example": {"Main.java": "package example; class Main {}"}}
                        }},
                        "build": {"generated": {"ap_generated_sources": {"demoDebug": {"out": {
                            "example": {"Generated.java": "package example; class Generated {}"}
                        }}}}}
                    }
                }),
            )
            .await;
        filesystem
            .insert_tree(
                GENERIC,
                json!({
                    "main.py": "print('generic')\n",
                    "index.html": "<p>generic</p>\n",
                    "build.gradle.kts": "// A filename cannot establish Android ownership.\n"
                }),
            )
            .await;
        let project = Project::test_with_worktree_trust(
            filesystem.clone(),
            [Path::new(ROOT), Path::new(GENERIC)],
            cx,
        )
        .await;
        cx.update(|cx| {
            let store = project.read(cx).worktree_store();
            let paths = project
                .read(cx)
                .visible_worktrees(cx)
                .map(|worktree| PathTrust::Worktree(worktree.read(cx).id()))
                .collect();
            TrustedWorktrees::try_get_global(cx)
                .context("Trust store")?
                .update(cx, |trust, cx| trust.trust(&store, paths, cx));
            publish_context(&project, cx)?;
            publish_model(&project, cx)
        })?;
        Ok((project, filesystem))
    }

    fn root_id(project: &Entity<Project>, cx: &App) -> Result<WorktreeId> {
        project
            .read(cx)
            .visible_worktrees(cx)
            .find(|worktree| worktree.read(cx).abs_path().as_ref() == Path::new(ROOT))
            .map(|worktree| worktree.read(cx).id())
            .context("Fixture root worktree")
    }

    fn publish_context(project: &Entity<Project>, cx: &mut App) -> Result<()> {
        let root = Path::new(ROOT);
        let record = json!({
            "schema": 1, "root": ROOT, "gradleVersion": "9.6.1", "phase": "complete",
            "modules": [{"path": ":app", "directory": root.join("app"),
                "plugins": PluginId::ALL.into_iter().map(|plugin| json!({
                    "plugin": plugin, "applied": plugin == PluginId::AndroidApplication
                })).collect::<Vec<_>>(),
                "targets": {"status": "available", "value": []}
            }]
        });
        let snapshot = decode_context_record(&serde_json::to_vec(&record)?, root)?;
        project.update(cx, |project, cx| {
            let worktree = project
                .visible_worktrees(cx)
                .find(|worktree| worktree.read(cx).abs_path().as_ref() == root)
                .context("Fixture context root")?
                .read(cx)
                .id();
            let handle = project.ensure_android_context(worktree, true, cx)?;
            let discovery = project.begin_android_context_import(handle, cx)?;
            let mut active = ActiveContext::default();
            active.select(Some(handle), None)?;
            let owner = active
                .discovery_token(project.android_context())
                .context("Fixture discovery owner")?;
            project.publish_android_context(&active, &owner, &discovery, snapshot, cx)
        })
    }

    fn publish_model(project: &Entity<Project>, cx: &mut App) -> Result<()> {
        project.update(cx, |project, cx| {
            let token = project.invalidate_android_model(Some(PathBuf::from(ROOT)), cx);
            let record = format!(
                "{MODEL_OUTPUT_PREFIX}{}",
                EXPORTED_MODEL.replace(ORIGINAL_ROOT, ROOT)
            );
            let capture = EvaluatedTreeInputs::decode_sync(&record, Path::new(ROOT), None, &token)?;
            project.publish_evaluated_android_model(&token, capture, cx)?;
            project.select_android_variant(
                Some(VariantId {
                    module: ":app".into(),
                    variant: "demoDebug".into(),
                }),
                cx,
            )
        })
    }

    struct TestWindow {
        multi_workspace: Entity<MultiWorkspace>,
        workspace: Entity<Workspace>,
        panel: Entity<crate::ProjectPanel>,
        visual: VisualTestContext,
    }

    impl TestWindow {
        fn new(project: &Entity<Project>, cx: &mut TestAppContext) -> Result<Self> {
            let window =
                cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
            let multi_workspace = window.root(cx)?;
            let workspace = multi_workspace
                .read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());
            let mut visual = VisualTestContext::from_window(window.into(), cx);
            let panel = workspace.update_in(&mut visual, crate::ProjectPanel::new);
            visual.run_until_parked();
            Ok(Self {
                multi_workspace,
                workspace,
                panel,
                visual,
            })
        }

        fn tree(&self) -> Entity<AndroidProjectTree> {
            self.panel
                .read_with(&self.visual, |panel, _| panel._android_tree.clone())
        }

        async fn open(&mut self, path: &Path) -> Result<()> {
            self.workspace
                .update_in(&mut self.visual, |workspace, window, cx| {
                    workspace.open_abs_path(path.to_path_buf(), Default::default(), window, cx)
                })
                .await?;
            self.visual.run_until_parked();
            Ok(())
        }

        async fn ready(&mut self) {
            let tree = self.tree();
            self.visual
                .condition(&tree, |tree, _| tree.status == Status::Ready)
                .await;
            self.visual.run_until_parked();
        }

        fn published_owner(&self) -> Result<RequestOwner> {
            self.tree().read_with(&self.visual, |tree, _| {
                Ok(tree
                    .published
                    .as_ref()
                    .context("Published tree")?
                    .owner
                    .clone())
            })
        }

        fn current_file_revision(&mut self) -> Result<u64> {
            self.tree().update_in(&mut self.visual, |tree, window, cx| {
                Ok(tree
                    .current_capture(window, cx)
                    .context("Current tree")?
                    .tree()
                    .tree
                    .file_revision)
            })
        }

        fn hold_next_completion(
            &mut self,
        ) -> oneshot::Receiver<(RequestOwner, Result<CapturedAndroidModuleTree>)> {
            let (sender, receiver) = oneshot::channel();
            self.tree().update(&mut self.visual, |tree, _| {
                tree.completion_for_test = Some(sender)
            });
            receiver
        }
    }

    #[gpui::test]
    async fn android_tree_capture_rejects_out_of_order_completion(cx: &mut TestAppContext) {
        let result: Result<()> = async {
            let (project, _) = fixture(cx).await?;
            let mut window = TestWindow::new(&project, cx)?;
            let delayed = window.hold_next_completion();
            window.open(Path::new(MAIN)).await?;
            let (old_owner, old_capture) = delayed.await?;
            let old_capture = old_capture?;
            project.update(&mut window.visual, |project, cx| {
                project.select_android_variant(
                    Some(VariantId {
                        module: ":app".into(),
                        variant: "demoDebug".into(),
                    }),
                    cx,
                )
            })?;
            window.ready().await;
            let latest = window.published_owner()?;
            let revision = window.current_file_revision()?;
            assert!(latest.request_generation > old_owner.request_generation);
            assert_ne!(latest.model, old_owner.model);
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    tree.finish_capture(&old_owner, Ok(old_capture), window, cx);
                    assert_eq!(tree.status, Status::Ready);
                });
            assert_eq!(window.published_owner()?, latest);
            assert_eq!(
                window.current_file_revision()?,
                revision,
                "A delayed actual capture must not replace the newer published capture"
            );
            Ok(())
        }
        .await;
        result.expect("Out-of-order live capture fixture must complete");
    }

    #[gpui::test]
    async fn android_tree_capture_clears_on_workspace_or_root_change(cx: &mut TestAppContext) {
        let result: Result<()> = async {
            let (project, _) = fixture(cx).await?;
            let mut window = TestWindow::new(&project, cx)?;
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            let original = window.published_owner()?;
            window.open(&Path::new(GENERIC).join("main.py")).await?;
            window.tree().read_with(&window.visual, |tree, _| {
                assert!(tree.owner.is_none());
                assert!(tree.pending.is_none());
                assert!(tree.published.is_none());
                assert_eq!(tree.status, Status::Unavailable);
            });
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            let restored = window.published_owner()?;
            assert!(restored.owner_generation > original.owner_generation);
            let replacement = window.multi_workspace.update_in(
                &mut window.visual,
                |multi_workspace, window, cx| {
                    multi_workspace.test_add_workspace(project.clone(), window, cx)
                },
            );
            window.visual.run_until_parked();
            assert_ne!(replacement.entity_id(), window.workspace.entity_id());
            window.tree().read_with(&window.visual, |tree, _| {
                assert!(tree.published.is_none());
                assert!(tree.pending.is_none());
                assert!(tree.owner.is_none());
            });
            let workspace = window.workspace.clone();
            window
                .multi_workspace
                .update_in(&mut window.visual, |multi_workspace, window, cx| {
                    multi_workspace.activate(workspace, None, window, cx);
                });
            window.ready().await;
            assert!(window.published_owner()?.owner_generation > restored.owner_generation);
            let root = window.visual.update(|_, cx| root_id(&project, cx))?;
            project.update(&mut window.visual, |project, cx| {
                project.remove_worktree(root, cx)
            });
            window.visual.run_until_parked();
            window.tree().read_with(&window.visual, |tree, _| {
                assert!(tree.published.is_none());
                assert!(tree.pending.is_none());
            });
            assert!(
                project.read_with(&window.visual, |project, cx| {
                    project
                        .find_project_path(&Path::new(GENERIC).join("index.html"), cx)
                        .is_some()
                }),
                "Removing Android must preserve the generic root"
            );
            Ok(())
        }
        .await;
        result.expect("Workspace/root live capture fixture must complete");
    }

    #[gpui::test]
    async fn android_tree_capture_rejects_trust_revoke_restore(cx: &mut TestAppContext) {
        let result: Result<()> = async {
            let (project, _) = fixture(cx).await?;
            let mut window = TestWindow::new(&project, cx)?;
            let delayed = window.hold_next_completion();
            window.open(Path::new(MAIN)).await?;
            let (old_owner, old_capture) = delayed.await?;
            let old_capture = old_capture?;
            window.visual.update(|_, cx| {
                let root = root_id(&project, cx)?;
                let store = project.read(cx).worktree_store();
                let trust = TrustedWorktrees::try_get_global(cx).context("Trust store")?;
                trust.update(cx, |trust, cx| {
                    trust.restrict(
                        store.downgrade(),
                        [PathTrust::Worktree(root)].into_iter().collect(),
                        cx,
                    );
                });
                trust.update(cx, |trust, cx| {
                    trust.trust(
                        &store,
                        [PathTrust::Worktree(root)].into_iter().collect(),
                        cx,
                    );
                });
                Ok::<_, anyhow::Error>(())
            })?;
            window.ready().await;
            let restored = window.published_owner()?;
            assert!(restored.owner_generation > old_owner.owner_generation);
            assert!(
                !project.read_with(&window.visual, |project, cx| {
                    project.is_android_tree_capture_current(&old_capture, cx)
                }),
                "Restoring trust must not revive the old capture request"
            );
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    tree.finish_capture(&old_owner, Ok(old_capture), window, cx);
                });
            assert_eq!(window.published_owner()?, restored);
            window.current_file_revision()?;
            Ok(())
        }
        .await;
        result.expect("Trust revoke/restore live capture fixture must complete");
    }

    #[gpui::test]
    async fn android_tree_refresh_updates_generated_file_deletion(cx: &mut TestAppContext) {
        let result: Result<()> = async {
            let (project, filesystem) = fixture(cx).await?;
            let mut window = TestWindow::new(&project, cx)?;
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            let before = window.current_file_revision()?;
            let original = window.published_owner()?;
            window.tree().read_with(&window.visual, |tree, _| {
                assert!(
                    tree.published
                        .as_ref()
                        .expect("Published tree")
                        .capture
                        .tree()
                        .tree
                        .nodes()
                        .any(|node| {
                            matches!(&node.key, NodeKey::JavaClass { path, name, .. }
                        if path == Path::new(GENERATED) && name == "Generated")
                        })
                );
            });
            filesystem
                .remove_file(Path::new(GENERATED), Default::default())
                .await?;
            window.visual.run_until_parked();
            window.ready().await;
            assert_ne!(window.current_file_revision()?, before);
            assert_eq!(window.published_owner()?.model, original.model);
            window.tree().read_with(&window.visual, |tree, _| {
                let capture = &tree.published.as_ref().expect("Updated tree").capture;
                assert!(!capture.tree().tree.nodes().any(|node| {
                    node.navigation
                        .as_ref()
                        .is_some_and(|target| target.path == Path::new(GENERATED))
                }));
                assert!(capture.tree().tree.nodes().any(|node| {
                    matches!(&node.key, NodeKey::JavaClass { name, .. } if name == "Main")
                }));
            });
            Ok(())
        }
        .await;
        result.expect("Generated-file deletion live capture fixture must complete");
    }

    #[gpui::test]
    async fn android_tree_failed_sync_retains_only_same_owner(cx: &mut TestAppContext) {
        let result: Result<()> = async {
            let (project, _) = fixture(cx).await?;
            let mut window = TestWindow::new(&project, cx)?;
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            let original = window.published_owner()?;
            let revision = window.current_file_revision()?;
            project.update(&mut window.visual, |project, cx| {
                project.invalidate_android_model(Some(PathBuf::from(ROOT)), cx);
            });
            window.visual.run_until_parked();
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    let published = tree.published.as_ref().context("Last good tree")?;
                    assert_eq!(published.owner, original);
                    assert_eq!(published.capture.tree().tree.file_revision, revision);
                    assert_eq!(tree.status, Status::Stale);
                    assert!(
                        tree.current_capture(window, cx).is_none(),
                        "A retained failed-sync tree cannot navigate"
                    );
                    assert!(
                        tree.context
                            .as_ref()
                            .context("Window context")?
                            .read(cx)
                            .is_current(&original.context, cx)
                    );
                    Ok::<_, anyhow::Error>(())
                })?;
            window.visual.update(|_, cx| publish_model(&project, cx))?;
            window.ready().await;
            assert_ne!(window.published_owner()?.model, original.model);
            assert_ne!(window.current_file_revision()?, revision);
            window.open(&Path::new(GENERIC).join("index.html")).await?;
            window.tree().read_with(&window.visual, |tree, _| {
                assert!(tree.published.is_none());
                assert!(tree.pending.is_none());
            });
            Ok(())
        }
        .await;
        result.expect("Failed-sync live capture fixture must complete");
    }

    #[gpui::test]
    async fn android_tree_panel_drop_cancels_pending_capture(cx: &mut TestAppContext) {
        let result: Result<()> = async {
            let (project, _) = fixture(cx).await?;
            let mut window = TestWindow::new(&project, cx)?;
            let delayed = window.hold_next_completion();
            window.open(Path::new(MAIN)).await?;
            let (_, capture) = delayed.await?;
            let capture = capture?;
            let (tree, request) = {
                let tree = window.tree();
                let request = tree.read_with(&window.visual, |tree, _| {
                    Ok::<_, anyhow::Error>(
                        tree.pending
                            .as_ref()
                            .context("Pending capture")?
                            .request
                            .as_ref()
                            .context("Capture request")?
                            .clone(),
                    )
                })?;
                (tree.downgrade(), request)
            };
            let TestWindow {
                panel, mut visual, ..
            } = window;
            visual.update(|_, _| drop(panel));
            visual.run_until_parked();
            assert!(
                tree.upgrade().is_none(),
                "Panel observers/tasks must not strongly retain its backend"
            );
            assert!(!project.read_with(&visual, |project, cx| {
                project.is_android_tree_capture_current(&capture, cx)
            }));
            let failure = project
                .update(&mut visual, |project, cx| {
                    project.capture_android_module_tree(request, cx)
                })
                .await;
            assert!(
                failure.is_err(),
                "Dropping the owning panel must cancel every clone of its actual request"
            );
            Ok(())
        }
        .await;
        result.expect("Panel-drop live capture fixture must complete");
    }

    #[gpui::test]
    async fn android_tree_generic_editors_and_multiple_windows_remain_isolated(
        cx: &mut TestAppContext,
    ) {
        let result: Result<()> = async {
            let (project, _) = fixture(cx).await?;
            let mut android = TestWindow::new(&project, cx)?;
            android.open(Path::new(MAIN)).await?;
            android.ready().await;
            let owner = android.published_owner()?;
            let revision = android.current_file_revision()?;
            let mut generic = TestWindow::new(&project, cx)?;
            for name in ["main.py", "index.html"] {
                generic.open(&Path::new(GENERIC).join(name)).await?;
                generic.tree().read_with(&generic.visual, |tree, _| {
                    assert!(tree.owner.is_none());
                    assert!(tree.pending.is_none());
                    assert!(tree.published.is_none());
                    assert_eq!(tree.status, Status::Unavailable);
                });
                generic
                    .workspace
                    .read_with(&generic.visual, |workspace, cx| {
                        let item = workspace.active_item(cx).expect("Generic editor");
                        let path = item.project_path(cx).expect("Generic project path");
                        assert_eq!(path.path.as_unix_str(), name);
                    });
                assert_eq!(android.published_owner()?, owner);
                assert_eq!(android.current_file_revision()?, revision);
            }
            assert_ne!(android.workspace.entity_id(), generic.workspace.entity_id());
            assert_ne!(android.tree().entity_id(), generic.tree().entity_id());
            let context = generic
                .visual
                .update(|_, cx| {
                    AndroidTreeContext::for_workspace(&generic.workspace.downgrade(), cx)
                })
                .context("Generic workspace context observer")?;
            assert!(
                !context.read_with(&generic.visual, |context, cx| context
                    .is_current(&owner.context, cx)),
                "An opaque owner cannot be reused in another workspace of the same Project"
            );
            Ok(())
        }
        .await;
        result.expect("Generic/multiple-window live capture fixture must complete");
    }

    #[gpui::test]
    async fn two_android_windows_share_content_freshness_without_sharing_owners(
        cx: &mut TestAppContext,
    ) {
        let result: Result<()> = async {
            let (project, filesystem) = fixture(cx).await?;
            let mut first = TestWindow::new(&project, cx)?;
            first.open(Path::new(MAIN)).await?;
            first.ready().await;
            let first_owner = first.published_owner()?;
            let first_revision = first.current_file_revision()?;
            let mut second = TestWindow::new(&project, cx)?;
            second.open(Path::new(MAIN)).await?;
            second.ready().await;
            let second_owner = second.published_owner()?;
            let second_revision = second.current_file_revision()?;
            assert_ne!(first.workspace.entity_id(), second.workspace.entity_id());
            assert_ne!(first.tree().entity_id(), second.tree().entity_id());
            assert_ne!(first_owner.context, second_owner.context);
            assert_eq!(first_owner.model, second_owner.model);
            assert_eq!(
                first.current_file_revision()?,
                first_revision,
                "Opening a second Android window must preserve the first usable capture"
            );
            assert_eq!(second.current_file_revision()?, second_revision);
            for window in [&mut first, &mut second] {
                window
                    .tree()
                    .update_in(&mut window.visual, |tree, window, cx| {
                        let capture = tree
                            .current_capture(window, cx)
                            .context("Both Android captures must be current")?;
                        for path in [MAIN, GENERATED] {
                            let physical = project.read(cx).android_tree_project_path(
                                capture,
                                Path::new(path),
                                cx,
                            )?;
                            assert!(project.read(cx).entry_for_path(&physical, cx).is_some());
                        }
                        Ok::<_, anyhow::Error>(())
                    })?;
            }

            let first_completion = first.hold_next_completion();
            let second_completion = second.hold_next_completion();
            filesystem
                .remove_file(Path::new(GENERATED), Default::default())
                .await?;
            let (first_pending, first_capture) = first_completion.await?;
            let (second_pending, second_capture) = second_completion.await?;
            let first_capture = first_capture?;
            let second_capture = second_capture?;
            for window in [&mut first, &mut second] {
                window
                    .tree()
                    .update_in(&mut window.visual, |tree, window, cx| {
                        let old = &tree
                            .published
                            .as_ref()
                            .context("Last good capture during refresh")?
                            .capture;
                        assert!(!project.read(cx).is_android_tree_capture_current(old, cx));
                        assert!(
                            project
                                .read(cx)
                                .android_tree_project_path(old, Path::new(GENERATED), cx)
                                .is_err()
                        );
                        assert!(
                            tree.current_capture(window, cx).is_none(),
                            "Neither Android window may navigate its stale generated-file capture"
                        );
                        Ok::<_, anyhow::Error>(())
                    })?;
            }
            first
                .tree()
                .update_in(&mut first.visual, |tree, window, cx| {
                    tree.finish_capture(&first_pending, Ok(first_capture), window, cx);
                });
            second
                .tree()
                .update_in(&mut second.visual, |tree, window, cx| {
                    tree.finish_capture(&second_pending, Ok(second_capture), window, cx);
                });
            first.ready().await;
            second.ready().await;
            assert_ne!(first.current_file_revision()?, first_revision);
            assert_ne!(second.current_file_revision()?, second_revision);
            for window in [&mut first, &mut second] {
                window
                    .tree()
                    .update_in(&mut window.visual, |tree, window, cx| {
                        let capture = tree
                            .current_capture(window, cx)
                            .context("Refreshed Android capture")?;
                        assert!(!capture.tree().tree.nodes().any(|node| {
                            node.navigation
                                .as_ref()
                                .is_some_and(|target| target.path == Path::new(GENERATED))
                        }));
                        project
                            .read(cx)
                            .android_tree_project_path(capture, Path::new(MAIN), cx)?;
                        Ok::<_, anyhow::Error>(())
                    })?;
            }

            let first_before_revoke = first.published_owner()?;
            let second_before_revoke = second.published_owner()?;
            first.visual.update(|_, cx| {
                let root = root_id(&project, cx)?;
                let store = project.read(cx).worktree_store();
                let trust = TrustedWorktrees::try_get_global(cx).context("Trust store")?;
                trust.update(cx, |trust, cx| {
                    trust.restrict(
                        store.downgrade(),
                        [PathTrust::Worktree(root)].into_iter().collect(),
                        cx,
                    );
                });
                trust.update(cx, |trust, cx| {
                    trust.trust(
                        &store,
                        [PathTrust::Worktree(root)].into_iter().collect(),
                        cx,
                    );
                });
                Ok::<_, anyhow::Error>(())
            })?;
            first.ready().await;
            second.ready().await;
            assert!(
                first.published_owner()?.owner_generation > first_before_revoke.owner_generation
            );
            assert!(
                second.published_owner()?.owner_generation > second_before_revoke.owner_generation
            );
            first.current_file_revision()?;
            second.current_file_revision()?;
            Ok(())
        }
        .await;
        result.expect(
            "Two Android windows must preserve content freshness and independent ownership",
        );
    }
}
