use android_tools::{
    evaluated_tree_inputs::{PreparedLiveModulePlan, prepare_live_module_plan},
    project_model::{ModelState, ModelToken},
    project_tree::{NodeKey, SourceGroup, TreeSnapshot},
    project_view_preferences::DefaultView,
};
use android_ui::{AndroidTreeContext, AndroidTreeOwner};
use anyhow::{Context as _, Result, ensure};
use gpui::{
    AnyElement, AnyWindowHandle, App, BackgroundExecutor, Context, Entity, FocusHandle,
    ListHorizontalSizingBehavior, ListSizingBehavior, Render, SharedString, Subscription, Task,
    UniformListScrollHandle, WeakEntity, Window, uniform_list,
};
use project::{
    Project,
    android_project_tree::{AndroidTreeCaptureRequest, CapturedAndroidModuleTree},
    trusted_worktrees::{PathTrust, TrustedWorktrees, TrustedWorktreesEvent},
};
use settings::{ProjectPanelEntrySpacing, Settings as _};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use ui::{ContextMenu, ListItem, ListItemSpacing, PopoverMenu, prelude::*};
use util::ResultExt as _;
use workspace::{MultiWorkspace, MultiWorkspaceEvent, Workspace, WorkspaceWindowOwnership};

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

    fn into_value(mut self) -> Result<T> {
        self.value.take().context("Android tree data was released")
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SelectorState {
    pub(crate) view: DefaultView,
    android_enabled: bool,
    reason: Option<SharedString>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RenderedOwner {
    context: AndroidTreeOwner,
    model: ModelToken,
    owner_generation: u64,
    request_generation: u64,
    presentation_generation: u64,
    row_generation: u64,
}

#[derive(Clone, Debug)]
struct LogicalRow {
    key: NodeKey,
    id: SharedString,
    label: SharedString,
    annotation: Option<SharedString>,
    depth: usize,
    has_children: bool,
    icon: IconName,
}

struct CachedLogicalRow {
    parent: Option<usize>,
    row: LogicalRow,
    children: Vec<usize>,
}

struct LogicalRowCache {
    owner: RequestOwner,
    rows: Vec<CachedLogicalRow>,
    roots: Vec<usize>,
    keys: BTreeMap<NodeKey, usize>,
    #[cfg(test)]
    drop_observer: Option<(BackgroundExecutor, futures::channel::oneshot::Sender<bool>)>,
}

#[cfg(test)]
impl Drop for LogicalRowCache {
    fn drop(&mut self) {
        if let Some((executor, observer)) = self.drop_observer.take() {
            observer
                .send(executor.is_main_thread())
                .map_err(|_| anyhow::anyhow!("Android row drop observer was released"))
                .log_err();
        }
    }
}

#[derive(Clone)]
enum RowChange {
    Toggle(NodeKey),
    CollapseAll,
    ExpandAll,
}

#[derive(Clone, Copy)]
enum RowReset {
    CollapseAll,
    ExpandAll,
}

// A persistent sparse bitset: one u64 covers 64 cache indices. Updating a toggle
// copies only its logarithmic branch; jobs share the immutable root in O(1).
enum ToggleNode {
    Leaf(u64),
    Branch {
        left: Option<Arc<ToggleNode>>,
        right: Option<Arc<ToggleNode>>,
    },
}

struct IndexToggles {
    root: Option<Arc<ToggleNode>>,
    depth: u32,
    count: usize,
    #[cfg(test)]
    drop_observer: Option<(BackgroundExecutor, futures::channel::oneshot::Sender<bool>)>,
}

#[cfg(test)]
impl Drop for IndexToggles {
    fn drop(&mut self) {
        if let Some((executor, observer)) = self.drop_observer.take() {
            observer
                .send(executor.is_main_thread())
                .map_err(|_| anyhow::anyhow!("Android command drop observer was released"))
                .log_err();
        }
    }
}

impl IndexToggles {
    fn empty(rows: usize) -> Self {
        let chunks = rows.div_ceil(64).max(1);
        Self {
            root: None,
            depth: usize::BITS - (chunks - 1).leading_zeros(),
            count: 0,
            #[cfg(test)]
            drop_observer: None,
        }
    }

    fn toggled(&self, index: usize) -> Self {
        fn update(
            node: Option<&Arc<ToggleNode>>,
            depth: u32,
            chunk: usize,
            bit: u64,
        ) -> (Option<Arc<ToggleNode>>, bool) {
            if depth == 0 {
                let prior = match node.map(Arc::as_ref) {
                    Some(ToggleNode::Leaf(bits)) => *bits,
                    _ => 0,
                };
                let next = prior ^ bit;
                return (
                    (next != 0).then(|| Arc::new(ToggleNode::Leaf(next))),
                    prior & bit == 0,
                );
            }
            let (left, right) = match node.map(Arc::as_ref) {
                Some(ToggleNode::Branch { left, right }) => (left.clone(), right.clone()),
                _ => (None, None),
            };
            let (left, right, added) = if chunk & (1 << (depth - 1)) == 0 {
                let (next, added) = update(left.as_ref(), depth - 1, chunk, bit);
                (next, right, added)
            } else {
                let (next, added) = update(right.as_ref(), depth - 1, chunk, bit);
                (left, next, added)
            };
            let next = (left.is_some() || right.is_some())
                .then(|| Arc::new(ToggleNode::Branch { left, right }));
            (next, added)
        }
        let (root, added) = update(
            self.root.as_ref(),
            self.depth,
            index / 64,
            1 << (index % 64),
        );
        Self {
            root,
            depth: self.depth,
            count: if added {
                self.count + 1
            } else {
                self.count - 1
            },
            #[cfg(test)]
            drop_observer: None,
        }
    }

    fn for_each(
        &self,
        cancelled: &AtomicBool,
        mut visit: impl FnMut(usize) -> Result<()>,
    ) -> Result<()> {
        fn walk(
            node: &ToggleNode,
            depth: u32,
            chunk: usize,
            cancelled: &AtomicBool,
            visit: &mut impl FnMut(usize) -> Result<()>,
        ) -> Result<()> {
            check_rows_cancelled(cancelled)?;
            match node {
                ToggleNode::Leaf(bits) => {
                    let mut remaining = *bits;
                    while remaining != 0 {
                        check_rows_cancelled(cancelled)?;
                        let bit = remaining.trailing_zeros() as usize;
                        visit(chunk * 64 + bit)?;
                        remaining &= remaining - 1;
                    }
                }
                ToggleNode::Branch { left, right } => {
                    if let Some(left) = left {
                        walk(left, depth - 1, chunk, cancelled, visit)?;
                    }
                    if let Some(right) = right {
                        walk(
                            right,
                            depth - 1,
                            chunk | (1 << (depth - 1)),
                            cancelled,
                            visit,
                        )?;
                    }
                }
            }
            Ok(())
        }
        if let Some(root) = &self.root {
            walk(root, self.depth, 0, cancelled, &mut visit)?;
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
struct RowChanges {
    reset: Option<RowReset>,
    // Every index remains bound to this exact immutable cache, even if a later
    // model capture inserts/reorders nodes. Materialization maps its NodeKeys
    // into the destination projection in the background.
    cache: Option<Arc<BackgroundOwned<LogicalRowCache>>>,
    toggles: Option<Arc<BackgroundOwned<IndexToggles>>>,
}

impl RowChanges {
    fn is_empty(&self) -> bool {
        self.reset.is_none()
            && self
                .toggles
                .as_ref()
                .is_none_or(|toggles| toggles.value().is_ok_and(|toggles| toggles.count == 0))
    }

    fn queue(
        &mut self,
        change: RowChange,
        cache: Arc<BackgroundOwned<LogicalRowCache>>,
        executor: &BackgroundExecutor,
    ) -> Result<()> {
        match change {
            RowChange::CollapseAll | RowChange::ExpandAll => {
                self.reset = Some(if matches!(change, RowChange::CollapseAll) {
                    RowReset::CollapseAll
                } else {
                    RowReset::ExpandAll
                });
                self.cache = Some(cache);
                self.toggles = None;
            }
            RowChange::Toggle(key) => {
                let source = self.cache.get_or_insert(cache).value()?;
                let index = *source
                    .keys
                    .get(&key)
                    .context("Android disclosure key is unavailable in its command cache")?;
                let next = if let Some(toggles) = &self.toggles {
                    toggles.value()?.toggled(index)
                } else {
                    IndexToggles::empty(source.rows.len()).toggled(index)
                };
                self.toggles = Some(Arc::new(BackgroundOwned::new(next, executor.clone())));
            }
        }
        Ok(())
    }

    fn materialize(&self, cancelled: &AtomicBool) -> Result<Vec<RowChange>> {
        check_rows_cancelled(cancelled)?;
        let mut changes = Vec::new();
        if let Some(reset) = self.reset {
            changes.push(match reset {
                RowReset::CollapseAll => RowChange::CollapseAll,
                RowReset::ExpandAll => RowChange::ExpandAll,
            });
        }
        if let Some(toggles) = &self.toggles {
            let source = self
                .cache
                .as_ref()
                .context("Android command cache is unavailable")?
                .value()?;
            toggles.value()?.for_each(cancelled, |index| {
                let row = source
                    .rows
                    .get(index)
                    .context("Android command index is unavailable")?;
                changes.push(RowChange::Toggle(row.row.key.clone()));
                Ok(())
            })?;
        }
        Ok(changes)
    }
}

#[derive(Clone, Copy)]
struct RowJobCursor {
    generation: u64,
    selection_generation: u64,
    change_generation: u64,
}

struct LogicalRowsUpdate {
    cache: Arc<BackgroundOwned<LogicalRowCache>>,
    rows: Vec<LogicalRow>,
    visible_keys: BTreeSet<NodeKey>,
    expanded_keys: Arc<BTreeSet<NodeKey>>,
    selected_key: Option<NodeKey>,
}

fn check_rows_cancelled(cancelled: &AtomicBool) -> Result<()> {
    ensure!(
        !cancelled.load(Ordering::Acquire),
        "Android row preparation was cancelled"
    );
    Ok(())
}

fn logical_icon(key: &NodeKey) -> IconName {
    match key {
        NodeKey::Module { .. } => IconName::BoxOpen,
        NodeKey::Source { .. } | NodeKey::ResourceType { .. } => IconName::Folder,
        NodeKey::Directory { group, .. } => match group {
            SourceGroup::Java
            | SourceGroup::Kotlin
            | SourceGroup::KotlinAndJava
            | SourceGroup::GeneratedJava => IconName::Box,
            _ => IconName::Folder,
        },
        NodeKey::ResourceGroup { .. } => IconName::FileMultiple,
        NodeKey::JavaClass { .. } => IconName::Code,
        NodeKey::File { .. } => IconName::File,
    }
}

impl LogicalRowCache {
    fn visible_selection(
        &self,
        selected: Option<&NodeKey>,
        visible: &BTreeSet<NodeKey>,
    ) -> Option<NodeKey> {
        let mut index = self.keys.get(selected?).copied();
        while let Some(current) = index {
            let row = self.rows.get(current)?;
            if visible.contains(&row.row.key) {
                return Some(row.row.key.clone());
            }
            index = row.parent;
        }
        None
    }

    fn prepare(
        owner: RequestOwner,
        snapshot: &TreeSnapshot,
        cancelled: &AtomicBool,
    ) -> Result<Self> {
        let mut keys = BTreeMap::new();
        let mut rows = Vec::new();
        for (index, node) in snapshot.nodes().enumerate() {
            check_rows_cancelled(cancelled)?;
            keys.insert(node.key.clone(), index);
            rows.push(CachedLogicalRow {
                parent: None,
                row: LogicalRow {
                    key: node.key.clone(),
                    id: format!("android-logical-row:{:?}", node.key).into(),
                    label: node.label.clone().into(),
                    annotation: node
                        .annotation
                        .as_ref()
                        .map(|annotation| annotation.trim_start().to_owned().into()),
                    depth: 0,
                    has_children: !node.children.is_empty(),
                    icon: logical_icon(&node.key),
                },
                children: Vec::new(),
            });
        }
        for (node, row) in snapshot.nodes().zip(&mut rows) {
            check_rows_cancelled(cancelled)?;
            row.children = node
                .children
                .iter()
                .map(|id| {
                    let child = snapshot
                        .node(*id)
                        .context("Android tree child is unavailable")?;
                    keys.get(&child.key)
                        .copied()
                        .context("Android child key is unavailable")
                })
                .collect::<Result<Vec<_>>>()?;
        }
        // Parent indices let late selection normalize in O(tree depth), rather
        // than scanning all rows after an asynchronous disclosure publication.
        for parent in 0..rows.len() {
            check_rows_cancelled(cancelled)?;
            for offset in 0..rows[parent].children.len() {
                let child = rows[parent].children[offset];
                rows[child].parent = Some(parent);
            }
        }
        let roots = snapshot
            .roots
            .iter()
            .map(|id| {
                let root = snapshot
                    .node(*id)
                    .context("Android tree root is unavailable")?;
                keys.get(&root.key)
                    .copied()
                    .context("Android root key is unavailable")
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            owner,
            rows,
            roots,
            keys,
            #[cfg(test)]
            drop_observer: None,
        })
    }
}

fn prepare_visible_rows(
    cache: Arc<BackgroundOwned<LogicalRowCache>>,
    expanded: Arc<BTreeSet<NodeKey>>,
    mut selected_key: Option<NodeKey>,
    initialized: bool,
    changes: Vec<RowChange>,
    cancelled: &AtomicBool,
) -> Result<LogicalRowsUpdate> {
    check_rows_cancelled(cancelled)?;
    let data = cache.value()?;
    let mut expanded_keys = expanded.as_ref().clone();
    check_rows_cancelled(cancelled)?;
    expanded_keys.retain(|key| !cancelled.load(Ordering::Acquire) && data.keys.contains_key(key));
    check_rows_cancelled(cancelled)?;
    if selected_key
        .as_ref()
        .is_some_and(|key| !data.keys.contains_key(key))
    {
        selected_key = None;
    }
    if !initialized {
        expanded_keys.extend(
            data.roots
                .iter()
                .filter_map(|index| data.rows.get(*index).map(|row| row.row.key.clone())),
        );
    }
    for change in changes {
        check_rows_cancelled(cancelled)?;
        match change {
            RowChange::Toggle(key) => {
                if !data.keys.contains_key(&key) {
                    continue;
                }
                if !expanded_keys.remove(&key) {
                    expanded_keys.insert(key);
                }
            }
            RowChange::CollapseAll => {
                expanded_keys.clear();
                selected_key = data
                    .roots
                    .first()
                    .and_then(|index| data.rows.get(*index).map(|row| row.row.key.clone()));
            }
            RowChange::ExpandAll => {
                for row in &data.rows {
                    check_rows_cancelled(cancelled)?;
                    if row.row.has_children {
                        expanded_keys.insert(row.row.key.clone());
                    }
                }
            }
        }
    }
    let mut pending = data
        .roots
        .iter()
        .rev()
        .map(|index| (*index, 0))
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    let mut visible_keys = BTreeSet::new();
    while let Some((index, depth)) = pending.pop() {
        check_rows_cancelled(cancelled)?;
        let cached = data
            .rows
            .get(index)
            .context("Android cached row is unavailable")?;
        let mut row = cached.row.clone();
        row.depth = depth;
        visible_keys.insert(row.key.clone());
        if expanded_keys.contains(&row.key) {
            pending.extend(
                cached
                    .children
                    .iter()
                    .rev()
                    .map(|child| (*child, depth + 1)),
            );
        }
        rows.push(row);
    }
    Ok(LogicalRowsUpdate {
        cache,
        rows,
        visible_keys,
        expanded_keys: Arc::new(expanded_keys),
        selected_key,
    })
}

pub(crate) struct AndroidProjectTree {
    workspace: WeakEntity<Workspace>,
    window_ownership: Option<WorkspaceWindowOwnership>,
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
    focus_handle: FocusHandle,
    view_choices: BTreeMap<PathBuf, DefaultView>,
    logical_rows: Vec<LogicalRow>,
    logical_cache: Option<Arc<BackgroundOwned<LogicalRowCache>>>,
    visible_keys: BTreeSet<NodeKey>,
    expanded_keys: Arc<BTreeSet<NodeKey>>,
    row_changes: RowChanges,
    row_change_generation: u64,
    row_task: Option<Task<()>>,
    row_cancelled: Option<Arc<AtomicBool>>,
    row_generation: u64,
    row_executor: BackgroundExecutor,
    row_reconcile_scheduled: bool,
    presentation_generation: u64,
    selected_key: Option<NodeKey>,
    selection_generation: u64,
    logical_scroll: UniformListScrollHandle,
    logical_initialized: bool,
    reconcile_scheduled: bool,
    _subscriptions: Vec<Subscription>,
    #[cfg(test)]
    row_gate_for_test: Option<futures::channel::oneshot::Receiver<()>>,
    #[cfg(test)]
    completion_for_test: Option<
        futures::channel::oneshot::Sender<(RequestOwner, Result<CapturedAndroidModuleTree>)>,
    >,
}

impl AndroidProjectTree {
    pub(crate) fn new(
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        focus_handle: FocusHandle,
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
            window_ownership: None,
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
            focus_handle,
            view_choices: BTreeMap::new(),
            logical_rows: Vec::new(),
            logical_cache: None,
            visible_keys: BTreeSet::new(),
            expanded_keys: Arc::new(BTreeSet::new()),
            row_changes: RowChanges::default(),
            row_change_generation: 0,
            row_task: None,
            row_cancelled: None,
            row_generation: 0,
            row_executor: cx.background_executor().clone(),
            row_reconcile_scheduled: false,
            presentation_generation: 0,
            selected_key: None,
            selection_generation: 0,
            logical_scroll: UniformListScrollHandle::new(),
            logical_initialized: false,
            reconcile_scheduled: false,
            _subscriptions: subscriptions,
            #[cfg(test)]
            row_gate_for_test: None,
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
            if this.current_capture(window, cx).is_none() {
                this.cancel_rows();
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

    fn ui_window_is_current(&self, window: &Window, _cx: &App) -> bool {
        if window.window_handle() != self.window
            || self.workspace.upgrade().is_none()
            || self.window_ownership.as_ref().is_none_or(|ownership| {
                !ownership.is_active(self.workspace.entity_id(), self.project.entity_id())
            })
        {
            return false;
        }
        match window.root::<MultiWorkspace>().flatten() {
            Some(root) => self
                .window_workspace_subscription
                .as_ref()
                .is_some_and(|(bound, _)| bound.entity_id() == root.entity_id()),
            None => window
                .root::<Workspace>()
                .flatten()
                .is_some_and(|root| root.entity_id() == self.workspace.entity_id()),
        }
    }

    fn ui_request_is_current(&self, request: &RequestOwner, window: &Window, cx: &App) -> bool {
        self.ui_window_is_current(window, cx)
            && self.owner_generation == request.owner_generation
            && self.request_generation == request.request_generation
            && self.owner.as_ref() == Some(&request.context)
            && self
                .context
                .as_ref()
                .is_some_and(|context| context.read(cx).is_current(&request.context, cx))
            && self
                .project
                .read(cx)
                .android_model()
                .is_current(request.model)
    }

    fn bind_window_workspace(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.window_ownership = self
            .workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).window_ownership());
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
        self.cancel_rows();
        let obsolete_rows = std::mem::take(&mut self.logical_rows);
        let obsolete_keys = std::mem::take(&mut self.visible_keys);
        let obsolete_expansion =
            std::mem::replace(&mut self.expanded_keys, Arc::new(BTreeSet::new()));
        let obsolete_changes = std::mem::take(&mut self.row_changes);
        let obsolete_selection = self.selected_key.take();
        self.logical_cache = None;
        cx.background_spawn(async move {
            drop((
                obsolete_rows,
                obsolete_keys,
                obsolete_expansion,
                obsolete_changes,
                obsolete_selection,
            ));
        })
        .detach();
        self.logical_scroll = UniformListScrollHandle::new();
        self.logical_initialized = false;
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
                self.refresh_logical_rows(window, cx);
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

    pub(crate) fn selector_state(&self, window: &Window, cx: &App) -> Option<SelectorState> {
        let owner = self.owner.as_ref()?;
        if !self.ui_window_is_current(window, cx)
            || !self.context.as_ref()?.read(cx).is_current(owner, cx)
        {
            return None;
        }
        let project = self.project.read(cx);
        let snapshot = project.android_context().snapshot(owner.root())?;
        let supports_target = snapshot
            .project_view_capabilities(true)
            .is_android_view_visible();
        let model = project.android_model();
        let selected_model = model.root() == Some(owner.root_path())
            && model.selected.as_ref().is_some_and(|selected| {
                model
                    .model
                    .as_ref()
                    .is_some_and(|model| Arc::ptr_eq(model, &selected.model))
            });
        let android_enabled = supports_target
            && (self.published.is_some()
                || (selected_model && matches!(self.status, Status::Loading | Status::Ready)));
        let choice = self.view_choices.get(owner.root_path()).copied();
        let preferred = choice.unwrap_or(DefaultView::Android);
        Some(SelectorState {
            view: if supports_target
                && (selected_model
                    || self.published.is_some()
                    || choice == Some(DefaultView::Android))
            {
                preferred
            } else {
                DefaultView::Project
            },
            android_enabled,
            reason: (!android_enabled).then(|| {
                if supports_target {
                    "Sync the project to load Android view".into()
                } else {
                    "Android view requires an evaluated Android target".into()
                }
            }),
        })
    }

    fn choose_view(&mut self, view: DefaultView, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.selector_state(window, cx) else {
            return;
        };
        if view == DefaultView::Android && !state.android_enabled {
            return;
        }
        let Some(owner) = self.owner.as_ref() else {
            return;
        };
        let path = owner.root_path().to_path_buf();
        if self.view_choices.get(&path).copied() != Some(view) {
            let Some(generation) = self.presentation_generation.checked_add(1) else {
                self.set_status(
                    Status::Failed("Android presentation generation exhausted".into()),
                    cx,
                );
                log::error!("Android presentation generation exhausted");
                return;
            };
            self.view_choices.insert(path, view);
            self.presentation_generation = generation;
        }
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    pub(crate) fn rendered_owner(&self, window: &Window, cx: &App) -> Option<RenderedOwner> {
        let context = self.owner.as_ref()?;
        if !self.ui_window_is_current(window, cx)
            || !self.context.as_ref()?.read(cx).is_current(context, cx)
        {
            return None;
        }
        Some(RenderedOwner {
            context: context.clone(),
            model: self.project.read(cx).android_model().token(),
            owner_generation: self.owner_generation,
            request_generation: self.request_generation,
            presentation_generation: self.presentation_generation,
            row_generation: self.row_generation,
        })
    }

    fn rendered_owner_is_current(&self, owner: &RenderedOwner, window: &Window, cx: &App) -> bool {
        self.rendered_owner(window, cx).as_ref() == Some(owner)
    }

    fn choose_rendered_view(
        &mut self,
        owner: &RenderedOwner,
        view: DefaultView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rendered_owner_is_current(owner, window, cx) {
            self.choose_view(view, window, cx);
        }
    }

    pub(crate) fn render_selector(
        &self,
        state: SelectorState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(owner) = self.rendered_owner(window, cx) else {
            return div().into_any_element();
        };
        let tree = cx.weak_entity();
        let view = state.view;
        let enabled = state.android_enabled;
        let reason = state.reason.clone();
        PopoverMenu::new(SharedString::from(format!(
            "android-project-view-selector:{}:{}:{}:{}",
            owner.owner_generation,
            owner.request_generation,
            owner.presentation_generation,
            owner.row_generation,
        )))
        .trigger(
            Button::new(
                "android-project-view-trigger",
                match view {
                    DefaultView::Android => "Android",
                    DefaultView::Project => "Project",
                },
            )
            .end_icon(Icon::new(IconName::ChevronDown))
            .label_size(LabelSize::Small)
            .tab_index(0isize)
            .aria_label("Project view"),
        )
        .menu(move |window, cx| {
            let android_tree = tree.clone();
            let project_tree = tree.clone();
            let android_owner = owner.clone();
            let project_owner = owner.clone();
            let reason = reason.clone();
            Some(ContextMenu::build(window, cx, move |menu, _, _| {
                menu.when_some(reason, |menu, reason| menu.header(reason))
                    .toggleable_entry_disabled_when(
                        "Android",
                        view == DefaultView::Android,
                        !enabled,
                        IconPosition::Start,
                        None,
                        move |window, cx| {
                            android_tree
                                .update(cx, |tree, cx| {
                                    tree.choose_rendered_view(
                                        &android_owner,
                                        DefaultView::Android,
                                        window,
                                        cx,
                                    )
                                })
                                .log_err();
                        },
                    )
                    .toggleable_entry(
                        "Project",
                        view == DefaultView::Project,
                        IconPosition::Start,
                        None,
                        move |window, cx| {
                            project_tree
                                .update(cx, |tree, cx| {
                                    tree.choose_rendered_view(
                                        &project_owner,
                                        DefaultView::Project,
                                        window,
                                        cx,
                                    )
                                })
                                .log_err();
                        },
                    )
            }))
        })
        .into_any_element()
    }

    fn cancel_rows(&mut self) {
        if let Some(cancelled) = self.row_cancelled.take() {
            cancelled.store(true, Ordering::Release);
        }
        self.row_task = None;
    }

    fn refresh_logical_rows(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.row_reconcile_scheduled {
            return;
        }
        self.row_reconcile_scheduled = true;
        cx.defer_in(window, |tree, window, cx| {
            tree.row_reconcile_scheduled = false;
            if let Err(error) = tree.start_rows(window, cx) {
                tree.set_status(Status::Failed(format!("{error:#}").into()), cx);
                log::error!("Android row preparation failed: {error:#}");
            }
        });
    }

    fn start_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<()> {
        if self.current_capture(window, cx).is_none() {
            return Ok(());
        }
        self.cancel_rows();
        let published = self
            .published
            .as_ref()
            .context("Android rows have no capture")?;
        let owner = published.owner.clone();
        let snapshot = published.capture.shared_tree();
        let cache = self
            .logical_cache
            .clone()
            .filter(|cache| cache.value().is_ok_and(|cache| cache.owner == owner));
        let generation = self
            .row_generation
            .checked_add(1)
            .context("Android row generation exhausted")?;
        self.row_generation = generation;
        let expanded = self.expanded_keys.clone();
        let selected = self.selected_key.clone();
        let selection_generation = self.selection_generation;
        let initialized = self.logical_initialized;
        let changes = if self.row_changes.is_empty() {
            RowChanges::default()
        } else {
            self.row_changes.clone()
        };
        let cursor = RowJobCursor {
            generation,
            selection_generation,
            change_generation: self.row_change_generation,
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        self.row_cancelled = Some(cancelled.clone());
        let executor = cx.background_executor().clone();
        #[cfg(test)]
        let gate = self.row_gate_for_test.take();
        let preparation = cx.background_spawn({
            let owner = owner.clone();
            async move {
                check_rows_cancelled(&cancelled)?;
                #[cfg(test)]
                if let Some(gate) = gate {
                    gate.await.context("Android row test gate was released")?;
                    check_rows_cancelled(&cancelled)?;
                }
                let cache = match cache {
                    Some(cache) => cache,
                    None => Arc::new(BackgroundOwned::new(
                        LogicalRowCache::prepare(owner, &snapshot.tree, &cancelled)?,
                        executor.clone(),
                    )),
                };
                let rows = prepare_visible_rows(
                    cache,
                    expanded,
                    selected,
                    initialized,
                    changes.materialize(&cancelled)?,
                    &cancelled,
                )?;
                Ok::<_, anyhow::Error>(BackgroundOwned::new(rows, executor))
            }
        });
        self.row_task = Some(cx.spawn_in(window, async move |tree, cx| {
            let result = preparation.await;
            tree.update_in(cx, |tree, window, cx| {
                tree.finish_rows(&owner, cursor, result, window, cx);
            })
            .log_err();
        }));
        cx.notify();
        Ok(())
    }

    fn finish_rows(
        &mut self,
        owner: &RequestOwner,
        cursor: RowJobCursor,
        result: Result<BackgroundOwned<LogicalRowsUpdate>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let RowJobCursor {
            generation,
            selection_generation,
            change_generation,
        } = cursor;
        // Completion releases only its own job slot, even if the source model
        // became stale. Never clear a replacement job's cancellation ownership.
        if self.row_generation != generation {
            return;
        }
        self.row_task = None;
        self.row_cancelled = None;
        if self
            .published
            .as_ref()
            .is_none_or(|published| &published.owner != owner)
            || !self.request_is_current(owner, window, cx)
            || self.current_capture(window, cx).is_none()
            || self.row_change_generation != change_generation
        {
            cx.notify();
            return;
        }
        match result.and_then(BackgroundOwned::into_value) {
            Ok(update) => {
                let obsolete_rows = std::mem::replace(&mut self.logical_rows, update.rows);
                let obsolete_keys = std::mem::replace(&mut self.visible_keys, update.visible_keys);
                let obsolete_expansion =
                    std::mem::replace(&mut self.expanded_keys, update.expanded_keys);
                let obsolete_changes = std::mem::take(&mut self.row_changes);
                let requested_selection = if self.selection_generation == selection_generation {
                    update.selected_key.as_ref()
                } else {
                    self.selected_key.as_ref()
                };
                let selected = update.cache.value().ok().and_then(|cache| {
                    cache.visible_selection(requested_selection, &self.visible_keys)
                });
                let obsolete_selection = std::mem::replace(&mut self.selected_key, selected);
                self.logical_cache = Some(update.cache);
                self.logical_initialized = true;
                cx.background_spawn(async move {
                    drop((
                        obsolete_rows,
                        obsolete_keys,
                        obsolete_expansion,
                        obsolete_changes,
                        obsolete_selection,
                    ));
                })
                .detach();
                cx.notify();
            }
            Err(error) => {
                self.set_status(Status::Failed(format!("{error:#}").into()), cx);
                log::error!("Android row preparation failed: {error:#}");
            }
        }
    }

    fn rows_are_current(&self, window: &Window, cx: &App) -> bool {
        self.current_capture(window, cx).is_some()
            && self.published.as_ref().is_some_and(|published| {
                self.logical_cache.as_ref().is_some_and(|cache| {
                    cache
                        .value()
                        .is_ok_and(|cache| cache.owner == published.owner)
                })
            })
    }

    pub(crate) fn collapse_all_rendered(
        &mut self,
        owner: &RenderedOwner,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rendered_owner_is_current(owner, window, cx) {
            self.collapse_all(window, cx);
        }
    }

    pub(crate) fn collapse_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.rows_are_current(window, cx) {
            return;
        }
        let selected = self
            .logical_cache
            .as_ref()
            .and_then(|cache| cache.value().ok())
            .and_then(|cache| cache.roots.first().and_then(|index| cache.rows.get(*index)))
            .map(|row| row.row.key.clone());
        if !self.set_selected_key(selected, cx) {
            return;
        }
        self.queue_row_change(RowChange::CollapseAll, cx);
        self.refresh_logical_rows(window, cx);
        self.logical_scroll
            .scroll_to_item(0, gpui::ScrollStrategy::Top);
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    pub(crate) fn expand_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.rows_are_current(window, cx) {
            return;
        }
        self.queue_row_change(RowChange::ExpandAll, cx);
        self.refresh_logical_rows(window, cx);
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn select_rendered_row(
        &mut self,
        owner: &RenderedOwner,
        key: &NodeKey,
        expand: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rendered_owner_is_current(owner, window, cx) {
            self.select_row(key, expand, window, cx);
        }
    }

    fn set_selected_key(&mut self, key: Option<NodeKey>, cx: &mut Context<Self>) -> bool {
        if self.selected_key == key {
            return true;
        }
        let Some(generation) = self.selection_generation.checked_add(1) else {
            self.set_status(
                Status::Failed("Android selection generation exhausted".into()),
                cx,
            );
            log::error!("Android selection generation exhausted");
            return false;
        };
        self.selection_generation = generation;
        self.selected_key = key;
        true
    }

    fn queue_row_change(&mut self, change: RowChange, cx: &mut Context<Self>) {
        let Some(generation) = self.row_change_generation.checked_add(1) else {
            self.set_status(
                Status::Failed("Android row command generation exhausted".into()),
                cx,
            );
            return;
        };
        let result = self
            .logical_cache
            .clone()
            .context("Android command cache is unavailable")
            .and_then(|cache| self.row_changes.queue(change, cache, &self.row_executor));
        if let Err(error) = result {
            self.set_status(Status::Failed(format!("{error:#}").into()), cx);
            log::error!("Android row command failed: {error:#}");
            return;
        }
        self.row_change_generation = generation;
        // A queued command supersedes the in-flight snapshot immediately. The
        // deferred scheduler coalesces a burst into one replacement preparation.
        self.cancel_rows();
    }

    fn select_row(
        &mut self,
        key: &NodeKey,
        expand: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.rows_are_current(window, cx) || !self.visible_keys.contains(key) {
            return;
        }
        let has_children = self
            .logical_cache
            .as_ref()
            .and_then(|cache| cache.value().ok())
            .and_then(|cache| cache.keys.get(key).and_then(|index| cache.rows.get(*index)))
            .is_some_and(|row| row.row.has_children);
        if !self.set_selected_key(Some(key.clone()), cx) {
            return;
        }
        if expand && has_children {
            self.queue_row_change(RowChange::Toggle(key.clone()), cx);
            self.refresh_logical_rows(window, cx);
        }
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn render_row(
        &self,
        index: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let row = self.logical_rows.get(index)?;
        let selected = self.selected_key.as_ref() == Some(&row.key);
        let enabled = self.rows_are_current(window, cx);
        let owner = self.rendered_owner(window, cx)?;
        let disclosure_owner = owner.clone();
        let key = row.key.clone();
        let disclosure_key = row.key.clone();
        let settings = super::ProjectPanelSettings::get_global(cx);
        Some(
            div()
                .when(index == 0, |element| {
                    element.debug_selector(|| "android-logical-first-row".into())
                })
                .child(
                    ListItem::new(row.id.clone())
                        .indent_level(row.depth)
                        .indent_step_size(px(settings.indent_size))
                        .spacing(match settings.entry_spacing {
                            ProjectPanelEntrySpacing::Comfortable => ListItemSpacing::Dense,
                            ProjectPanelEntrySpacing::Standard => ListItemSpacing::ExtraDense,
                        })
                        .toggle_state(selected)
                        .disabled(!enabled)
                        .toggle(
                            row.has_children
                                .then(|| self.expanded_keys.contains(&row.key)),
                        )
                        .on_toggle(cx.listener(move |tree, _, window, cx| {
                            tree.select_rendered_row(
                                &disclosure_owner,
                                &disclosure_key,
                                true,
                                window,
                                cx,
                            )
                        }))
                        .start_slot(Icon::new(row.icon).size(IconSize::Small))
                        .child(
                            h_flex()
                                .debug_selector(|| "android-logical-row-text".into())
                                .min_w_0()
                                .gap_1()
                                .child(
                                    Label::new(row.label.clone())
                                        .single_line()
                                        .size(LabelSize::Small),
                                )
                                .when_some(row.annotation.clone(), |element, annotation| {
                                    element.child(
                                        Label::new(annotation)
                                            .single_line()
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    )
                                }),
                        )
                        .on_click(cx.listener(move |tree, _, window, cx| {
                            tree.select_rendered_row(&owner, &key, false, window, cx)
                        })),
                )
                .into_any_element(),
        )
    }

    pub(crate) fn current_capture(
        &self,
        window: &Window,
        cx: &App,
    ) -> Option<&CapturedAndroidModuleTree> {
        let published = self.published.as_ref()?;
        (self.ui_request_is_current(&published.owner, window, cx)
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

impl Render for AndroidProjectTree {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let available = self
            .selector_state(window, cx)
            .is_some_and(|state| state.view == DefaultView::Android);
        let message: Option<SharedString> = if !available {
            Some("Android view is unavailable for this project".into())
        } else {
            match &self.status {
                Status::Loading => Some("Loading Android view…".into()),
                Status::Stale => {
                    Some("Android view is out of date. Sync the project to refresh.".into())
                }
                Status::Failed(_) => {
                    Some("Couldn't load Android view. Sync the project to retry.".into())
                }
                Status::Unavailable => Some("Sync the project to load Android view".into()),
                Status::Ready
                    if (self.row_reconcile_scheduled || self.row_task.is_some())
                        && !self.rows_are_current(window, cx) =>
                {
                    Some("Loading Android view…".into())
                }
                Status::Ready if self.logical_rows.is_empty() => {
                    Some("No Android source groups are available".into())
                }
                Status::Ready => None,
            }
        };
        v_flex()
            .id("android-logical-tree")
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .track_focus(&self.focus_handle)
            .key_context("AndroidProjectTree")
            .on_action(|_: &super::Rename, _, cx| cx.stop_propagation())
            .on_action(|_: &super::Duplicate, _, cx| cx.stop_propagation())
            .on_action(|_: &super::Delete, _, cx| cx.stop_propagation())
            .on_action(|_: &super::Trash, _, cx| cx.stop_propagation())
            .on_action(|_: &super::Copy, _, cx| cx.stop_propagation())
            .on_action(|_: &super::Cut, _, cx| cx.stop_propagation())
            .on_action(|_: &super::Paste, _, cx| cx.stop_propagation())
            .on_action(|_: &git::FileHistory, _, cx| cx.stop_propagation())
            .on_action(
                cx.listener(|tree, _: &super::CollapseAllEntries, window, cx| {
                    tree.collapse_all(window, cx)
                }),
            )
            .on_action(
                cx.listener(|tree, _: &super::ExpandAllEntries, window, cx| {
                    tree.expand_all(window, cx)
                }),
            )
            .when_some(message, |element, message| {
                element.child(
                    div()
                        .debug_selector({
                            let message = message.clone();
                            move || format!("android-tree-message:{message}")
                        })
                        .px_2()
                        .py_1()
                        .child(
                            Label::new(message)
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ),
                )
            })
            .when(available, |element| {
                element.child(
                    uniform_list(
                        "android-logical-rows",
                        self.logical_rows.len(),
                        cx.processor(|tree, range: std::ops::Range<usize>, window, cx| {
                            range
                                .filter_map(|index| tree.render_row(index, window, cx))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .size_full()
                    .pl(rems(1.))
                    .flex_1()
                    .with_sizing_behavior(ListSizingBehavior::Infer)
                    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::FitList)
                    .track_scroll(&self.logical_scroll),
                )
            })
    }
}

impl Drop for AndroidProjectTree {
    fn drop(&mut self) {
        self.cancel_pending();
        self.cancel_rows();
        let rows = std::mem::take(&mut self.logical_rows);
        let keys = std::mem::take(&mut self.visible_keys);
        let expansion = std::mem::replace(&mut self.expanded_keys, Arc::new(BTreeSet::new()));
        let changes = std::mem::take(&mut self.row_changes);
        let selection = self.selected_key.take();
        self.row_executor
            .spawn(async move {
                drop((rows, keys, expansion, changes, selection));
            })
            .detach();
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

    // Supplemental host-race fixtures use shared model/sidecar decoding and live
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
        // Complete Gradle observations include the root even when it applies no supported plugin.
        let record = json!({
            "schema": 1, "root": ROOT, "gradleVersion": "9.6.1", "phase": "complete",
            "modules": [{
                "path": ":", "directory": root,
                "plugins": PluginId::ALL.into_iter().map(|plugin| json!({
                    "plugin": plugin, "applied": false
                })).collect::<Vec<_>>(),
                "targets": {"status": "unavailable", "value": {
                    "detail": "No evaluated Kotlin target extension"
                }}
            }, {"path": ":app", "directory": root.join("app"),
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
            // These fixture directories exist in FakeFs and contain no symlinks;
            // std::fs cannot canonicalize them during model decoding.
            let root = Path::new(ROOT);
            let directories = [root.to_path_buf(), root.join("app")]
                .into_iter()
                .map(|path| (path.clone(), path))
                .collect();
            let capture = EvaluatedTreeInputs::decode_fixture(&record, root, &directories, &token)?;
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

    // C2 GPUI coverage is supplemental: the shared exported model is decoded
    // above, while filesystem changes are observed through actual FakeFs worktrees.
    // None of these tests credits the six original Gradle import workflows.
    fn selector(window: &mut TestWindow) -> Option<SelectorState> {
        window
            .tree()
            .update_in(&mut window.visual, |tree, window, cx| {
                tree.selector_state(window, cx)
            })
    }

    fn mount_panel(window: &mut TestWindow) {
        let panel = window.panel.clone();
        window
            .workspace
            .update_in(&mut window.visual, |workspace, window, cx| {
                workspace.add_panel(panel, window, cx);
                workspace.open_panel::<crate::ProjectPanel>(window, cx);
            });
        window.visual.run_until_parked();
    }

    fn choose(window: &mut TestWindow, view: DefaultView) {
        window
            .tree()
            .update_in(&mut window.visual, |tree, window, cx| {
                tree.choose_view(view, window, cx)
            });
        window.visual.run_until_parked();
    }

    fn draw_panel(window: &mut TestWindow, width: f32, height: f32) {
        let panel = window.panel.clone();
        window.visual.draw(
            gpui::point(px(0.), px(0.)),
            gpui::size(px(width), px(height)),
            |_, _| panel.into_any_element(),
        );
    }

    fn expand_visible_groups(window: &mut TestWindow) -> Result<()> {
        for _ in 0..256 {
            let next = window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    let next = tree
                        .logical_rows
                        .iter()
                        .find(|row| row.has_children && !tree.expanded_keys.contains(&row.key))
                        .map(|row| row.key.clone());
                    if let Some(key) = &next {
                        tree.select_row(key, true, window, cx);
                    }
                    next
                });
            window.visual.run_until_parked();
            if next.is_none() {
                return Ok(());
            }
        }
        anyhow::bail!("Supplemental fixture unexpectedly exceeds 256 expandable rows")
    }

    async fn rows_ready(window: &mut TestWindow) {
        let tree = window.tree();
        window
            .visual
            .condition(&tree, |tree, _| {
                tree.row_task.is_none()
                    && !tree.row_reconcile_scheduled
                    && tree.logical_cache.as_ref().is_some_and(|cache| {
                        cache.value().is_ok_and(|cache| {
                            tree.published
                                .as_ref()
                                .is_some_and(|published| cache.owner == published.owner)
                        })
                    })
            })
            .await;
        window.visual.run_until_parked();
    }

    fn draw_workspace(window: &mut TestWindow) {
        window.visual.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn focus_workspace(window: &mut TestWindow) {
        let workspace = window.workspace.clone();
        let panel = window.panel.clone();
        window.visual.update(|window, cx| {
            let focus = gpui::Focusable::focus_handle(workspace.read(cx), cx);
            focus.focus(window, cx);
            assert!(!panel.read(cx).focus_handle.contains_focused(window, cx));
        });
    }

    fn publish_non_android_kmp_context(project: &Entity<Project>, cx: &mut App) -> Result<()> {
        let root = Path::new(ROOT);
        let record = json!({
            "schema": 1, "root": ROOT, "gradleVersion": "9.6.1", "phase": "complete",
            "modules": [
                {"path": ":", "directory": root,
                 "plugins": PluginId::ALL.into_iter().map(|plugin| json!({
                     "plugin": plugin, "applied": false
                 })).collect::<Vec<_>>(),
                 "targets": {"status": "available", "value": []}},
                {"path": ":app", "directory": root.join("app"),
                 "plugins": PluginId::ALL.into_iter().map(|plugin| json!({
                     "plugin": plugin, "applied": plugin == PluginId::KotlinMultiplatform
                 })).collect::<Vec<_>>(),
                 "targets": {"status": "available", "value": []}}
            ]
        });
        let snapshot = decode_context_record(&serde_json::to_vec(&record)?, root)?;
        project.update(cx, |project, cx| {
            let worktree = project
                .visible_worktrees(cx)
                .find(|worktree| worktree.read(cx).abs_path().as_ref() == root)
                .context("KMP fixture root worktree")?
                .read(cx)
                .id();
            let handle = project.ensure_android_context(worktree, true, cx)?;
            let discovery = project.begin_android_context_import(handle, cx)?;
            let mut active = ActiveContext::default();
            active.select(Some(handle), None)?;
            let owner = active
                .discovery_token(project.android_context())
                .context("KMP fixture discovery owner")?;
            project.publish_android_context(&active, &owner, &discovery, snapshot, cx)
        })
    }

    #[gpui::test]
    async fn android_selector_follows_selected_root_capability(cx: &mut TestAppContext) {
        let result: Result<()> = async {
            let (project, _) = fixture(cx).await?;
            let mut window = TestWindow::new(&project, cx)?;
            mount_panel(&mut window);
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            let state = selector(&mut window).context("Android selector")?;
            assert_eq!(state.view, DefaultView::Android);
            assert!(state.android_enabled);
            assert!(state.reason.is_none());
            for name in ["main.py", "index.html", "build.gradle.kts"] {
                window.open(&Path::new(GENERIC).join(name)).await?;
                assert!(
                    selector(&mut window).is_none(),
                    "Generic files and Gradle filenames cannot establish Android capability"
                );
                window.tree().read_with(&window.visual, |tree, _| {
                    assert!(tree.logical_rows.is_empty());
                    assert!(tree.selected_key.is_none());
                });
            }
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            window
                .visual
                .update(|_, cx| publish_non_android_kmp_context(&project, cx))?;
            window.visual.run_until_parked();
            let state = selector(&mut window).context("KMP selector with no Android target")?;
            assert_eq!(state.view, DefaultView::Project);
            assert!(!state.android_enabled);
            assert_eq!(
                state.reason.as_deref(),
                Some("Android view requires an evaluated Android target")
            );
            choose(&mut window, DefaultView::Android);
            assert_eq!(
                selector(&mut window).context("Disabled KMP selector")?.view,
                DefaultView::Project
            );
            window.visual.update(|_, cx| {
                publish_context(&project, cx)?;
                publish_model(&project, cx)
            })?;
            window.ready().await;
            window.visual.update(|_, cx| {
                let root = root_id(&project, cx)?;
                let store = project.read(cx).worktree_store();
                TrustedWorktrees::try_get_global(cx)
                    .context("Trust store")?
                    .update(cx, |trust, cx| {
                        trust.restrict(
                            store.downgrade(),
                            [PathTrust::Worktree(root)].into_iter().collect(),
                            cx,
                        )
                    });
                Ok::<_, anyhow::Error>(())
            })?;
            window.visual.run_until_parked();
            assert!(
                selector(&mut window).is_none(),
                "Revoked trust must remove Android chrome immediately"
            );
            Ok(())
        }
        .await;
        result.expect("C2 capability-driven selector fixture must complete");
    }

    #[gpui::test]
    async fn android_selector_preserves_explicit_project_choice(cx: &mut TestAppContext) {
        let result: Result<()> = async {
            let (project, _) = fixture(cx).await?;
            let mut window = TestWindow::new(&project, cx)?;
            mount_panel(&mut window);
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            choose(&mut window, DefaultView::Project);
            assert_eq!(
                selector(&mut window).context("Project choice")?.view,
                DefaultView::Project
            );
            let owner = window.published_owner()?;
            window.visual.update(|_, cx| publish_model(&project, cx))?;
            window.ready().await;
            assert_eq!(
                selector(&mut window)
                    .context("Project after model refresh")?
                    .view,
                DefaultView::Project
            );
            assert!(window.published_owner()?.request_generation > owner.request_generation);
            window.open(&Path::new(GENERIC).join("main.py")).await?;
            assert!(selector(&mut window).is_none());
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            assert_eq!(
                selector(&mut window)
                    .context("Restored per-root Project choice")?
                    .view,
                DefaultView::Project
            );
            let mut independent = TestWindow::new(&project, cx)?;
            mount_panel(&mut independent);
            independent.open(Path::new(MAIN)).await?;
            independent.ready().await;
            assert_eq!(
                selector(&mut independent)
                    .context("Independent default")?
                    .view,
                DefaultView::Android,
                "A second workspace must not inherit another panel's choice"
            );
            choose(&mut window, DefaultView::Android);
            window.ready().await;
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    let root = tree
                        .logical_rows
                        .first()
                        .context("Actual module row")?
                        .key
                        .clone();
                    tree.select_row(&root, false, window, cx);
                    Ok::<_, anyhow::Error>(())
                })?;
            project.update(&mut window.visual, |project, cx| {
                project.invalidate_android_model(Some(PathBuf::from(ROOT)), cx);
            });
            window.visual.run_until_parked();
            assert_eq!(
                selector(&mut window)
                    .context("Remembered Android after failure")?
                    .view,
                DefaultView::Android
            );
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    assert!(tree.current_capture(window, cx).is_none());
                    let selected = tree.selected_key.clone();
                    let row = tree
                        .logical_rows
                        .first()
                        .context("Retained disabled row")?
                        .key
                        .clone();
                    let expanded = tree.expanded_keys.clone();
                    tree.select_row(&row, true, window, cx);
                    assert_eq!(tree.selected_key, selected);
                    assert_eq!(
                        tree.expanded_keys, expanded,
                        "Failed sync must disable interactions with retained logical rows"
                    );
                    Ok::<_, anyhow::Error>(())
                })?;
            Ok(())
        }
        .await;
        result.expect("C2 explicit view preference fixture must complete");
    }

    #[gpui::test]
    async fn android_rows_preserve_labels_annotations_and_node_keys(cx: &mut TestAppContext) {
        let result: Result<()> = async {
            let (project, filesystem) = fixture(cx).await?;
            filesystem
                .insert_tree(
                    "/tree-owner/app/src/main/java",
                    json!({"com": {"sample": {"nested": {"Other.java":
                    "package com.sample.nested; class Other {}"}}}}),
                )
                .await;
            filesystem
                .insert_tree(
                    "/tree-owner/app/flavor-assets",
                    json!({"one": {"two": {"three": {"asset.txt": "literal asset"}}}}),
                )
                .await;
            let mut window = TestWindow::new(&project, cx)?;
            mount_panel(&mut window);
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            expand_visible_groups(&mut window)?;
            let (selected, row_id) = window.tree().update_in(
                &mut window.visual,
                |tree, window, cx| {
                    let module = tree.logical_rows.first().context("Imported module row")?;
                    assert_eq!(module.label.as_ref(), "app");
                    assert!(
                        module.annotation.is_none(),
                        "The reference (Android) suffix is not UI text"
                    );
                    assert!(matches!(module.key, NodeKey::Module { .. }));
                    assert!(
                        tree.logical_rows
                            .iter()
                            .any(|row| row.label.as_ref() == "java"
                                && row.annotation.as_deref() == Some("(generated)"))
                    );
                    assert!(
                        tree.logical_rows
                            .iter()
                            .any(|row| row.label.as_ref() == "com.sample.nested"
                                && row.annotation.is_none())
                    );
                    for name in ["one", "two", "three"] {
                        assert!(
                            tree.logical_rows
                                .iter()
                                .any(|row| row.label.as_ref() == name
                                    && row.annotation.as_deref() == Some("(demo)")),
                            "Assets retain literal directory rows and actual provider annotations"
                        );
                    }
                    assert!(
                        !tree
                            .logical_rows
                            .iter()
                            .any(|row| row.label.as_ref() == "one.two.three")
                    );
                    assert!(!tree.logical_rows.iter().any(|row| {
                        row.annotation
                            .as_deref()
                            .is_some_and(|value| value.contains("( ("))
                    }));
                    let row = tree
                        .logical_rows
                        .iter()
                        .find(|row| row.label.as_ref() == "Main")
                        .context("Actual main file row")?;
                    let key = row.key.clone();
                    let id = row.id.clone();
                    assert!(
                        row.annotation.is_none(),
                        "main is omitted in rendered annotation"
                    );
                    tree.select_row(&key, false, window, cx);
                    assert_eq!(tree.selected_key.as_ref(), Some(&key));
                    Ok::<_, anyhow::Error>((key, id))
                },
            )?;
            let revision = window.current_file_revision()?;
            filesystem
                .insert_tree(
                    "/tree-owner/app/src/main/java/example",
                    json!({"Added.java": "package example; class Added {}"}),
                )
                .await;
            window.visual.run_until_parked();
            window.ready().await;
            assert_ne!(window.current_file_revision()?, revision);
            window.tree().read_with(&window.visual, |tree, _| {
                assert_eq!(tree.selected_key.as_ref(), Some(&selected));
                let row = tree
                    .logical_rows
                    .iter()
                    .find(|row| row.key == selected)
                    .expect("Retained logical identity");
                assert_eq!(row.id, row_id);
                assert!(
                    tree.logical_rows
                        .iter()
                        .any(|row| row.label.as_ref() == "Added")
                );
            });
            Ok(())
        }
        .await;
        result.expect("C2 logical labels and durable identity fixture must complete");
    }

    #[gpui::test]
    async fn android_rows_resize_scroll_and_restore_focus(cx: &mut TestAppContext) {
        let result: Result<()> = async {
            let (project, filesystem) = fixture(cx).await?;
            let files = (0..80)
                .map(|index| {
                    (
                        format!("Class{index:02}.java"),
                        json!(format!("package example; class Class{index:02} {{}}")),
                    )
                })
                .collect::<serde_json::Map<_, _>>();
            filesystem
                .insert_tree("/tree-owner/app/src/main/java/example", json!(files))
                .await;
            let mut window = TestWindow::new(&project, cx)?;
            mount_panel(&mut window);
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            expand_visible_groups(&mut window)?;
            let physical_scroll = window.panel.read_with(&window.visual, |panel, _| {
                panel.scroll_handle.0.borrow().base_handle.offset()
            });
            for width in [180., 480.] {
                window
                    .visual
                    .simulate_resize(gpui::size(px(width), px(300.)));
                draw_panel(&mut window, width, 300.);
                window.tree().read_with(&window.visual, |tree, _| {
                    let handle = tree.logical_scroll.0.borrow();
                    let geometry = handle
                        .last_item_size
                        .expect("Rendered uniform list geometry");
                    assert!(geometry.item.width > px(0.));
                    assert!(geometry.item.width <= px(width));
                    assert!(geometry.item.height > px(0.));
                    assert!(
                        geometry.item.height < px(300.),
                        "Selector reserves header space"
                    );
                    assert!(geometry.contents.height > geometry.item.height);
                    assert!(tree.logical_rows.len() > 80);
                });
            }
            window.tree().update(&mut window.visual, |tree, cx| {
                tree.logical_scroll.scroll_to_item_strict(
                    tree.logical_rows.len() - 1,
                    gpui::ScrollStrategy::Bottom,
                );
                cx.notify();
            });
            draw_panel(&mut window, 480., 300.);
            let scroll = window.tree().read_with(&window.visual, |tree, _| {
                assert!(tree.logical_scroll.is_scrollable());
                let offset = tree.logical_scroll.0.borrow().base_handle.offset();
                assert!(
                    offset.y < px(0.),
                    "Logical rows actually scroll within the viewport"
                );
                offset
            });
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    let key = tree
                        .logical_rows
                        .last()
                        .context("Last visible logical row")?
                        .key
                        .clone();
                    tree.select_row(&key, false, window, cx);
                    assert!(tree.focus_handle.is_focused(window));
                    Ok::<_, anyhow::Error>(())
                })?;
            choose(&mut window, DefaultView::Project);
            let panel = window.panel.clone();
            window.visual.update(|window, cx| {
                assert!(panel.read(cx).focus_handle.is_focused(window));
            });
            choose(&mut window, DefaultView::Android);
            window.ready().await;
            draw_panel(&mut window, 480., 300.);
            window.tree().read_with(&window.visual, |tree, _| {
                assert_eq!(tree.logical_scroll.0.borrow().base_handle.offset(), scroll);
                assert!(tree.selected_key.is_some());
            });
            assert_eq!(
                window.panel.read_with(&window.visual, |panel, _| panel
                    .scroll_handle
                    .0
                    .borrow()
                    .base_handle
                    .offset()),
                physical_scroll
            );
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    tree.collapse_all(window, cx);
                });
            rows_ready(&mut window).await;
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, _cx| {
                    assert!(tree.expanded_keys.is_empty());
                    assert_eq!(tree.logical_rows.len(), 1);
                    assert!(matches!(tree.logical_rows[0].key, NodeKey::Module { .. }));
                    assert_eq!(tree.selected_key.as_ref(), Some(&tree.logical_rows[0].key));
                    assert!(tree.focus_handle.is_focused(window));
                });
            assert_eq!(
                window.panel.read_with(&window.visual, |panel, _| panel
                    .scroll_handle
                    .0
                    .borrow()
                    .base_handle
                    .offset()),
                physical_scroll,
                "Logical Collapse All must preserve physical Project scroll"
            );
            Ok(())
        }
        .await;
        result.expect("C2 rendered geometry, logical scroll and panel focus fixture must complete");
    }

    #[gpui::test]
    async fn generic_project_keeps_physical_panel_and_top_chooser(cx: &mut TestAppContext) {
        let result: Result<()> =
            async {
                let (project, filesystem) = fixture(cx).await?;
                let mut window = TestWindow::new(&project, cx)?;
                mount_panel(&mut window);
                window.open(&Path::new(GENERIC).join("main.py")).await?;
                assert!(selector(&mut window).is_none());
                let generic_path = window
                    .visual
                    .update(|_, cx| {
                        project
                            .read(cx)
                            .find_project_path(&Path::new(GENERIC).join("main.py"), cx)
                    })
                    .context("Actual generic ProjectPath")?;
                window.panel.update(&mut window.visual, |panel, cx| {
                    panel.select_path_for_test(generic_path.clone(), cx);
                });
                draw_panel(&mut window, 300., 400.);
                assert_eq!(
                    window.panel.read_with(&window.visual, |panel, cx| panel
                        .selected_entry_project_path(cx)),
                    Some(generic_path.clone())
                );
                let workspace_id = window.workspace.entity_id();
                let root_id = window.multi_workspace.entity_id();
                window.open(Path::new(MAIN)).await?;
                window.ready().await;
                choose(&mut window, DefaultView::Project);
                window.panel.update(&mut window.visual, |panel, cx| {
                    panel.select_path_for_test(generic_path.clone(), cx);
                });
                let selection = window
                    .panel
                    .read_with(&window.visual, |panel, _| panel.selection);
                let expanded = window.panel.read_with(&window.visual, |panel, _| {
                    panel.state.expanded_dir_ids.clone()
                });
                choose(&mut window, DefaultView::Android);
                window.ready().await;
                draw_panel(&mut window, 300., 400.);
                window
                    .tree()
                    .update_in(&mut window.visual, |tree, window, cx| {
                        let module = tree
                            .logical_rows
                            .first()
                            .context("Actual logical module row")?
                            .key
                            .clone();
                        tree.select_row(&module, false, window, cx);
                        Ok::<_, anyhow::Error>(())
                    })?;
                // A logical row must never route the physical panel's rename/copy/cut/paste
                // actions to a retained physical selection. Leaf navigation belongs to C3.
                window.visual.update(|window, cx| window.draw(cx).clear(cx));
                window.visual.dispatch_action(crate::Rename);
                window.visual.dispatch_action(crate::Copy);
                window.visual.dispatch_action(crate::Cut);
                window.visual.dispatch_action(crate::Paste);
                window.panel.read_with(&window.visual, |panel, _| {
                    assert!(panel.state.edit_state.is_none());
                    assert!(panel.clipboard.is_none());
                    assert_eq!(panel.selection, selection);
                    assert_eq!(panel.state.expanded_dir_ids, expanded);
                    assert!(panel.context_menu.is_none());
                    assert!(panel.drag_target_entry.is_none());
                });
                assert!(
                    filesystem
                        .is_file(&Path::new(GENERIC).join("main.py"))
                        .await
                );
                choose(&mut window, DefaultView::Project);
                assert_eq!(
                    window.panel.read_with(&window.visual, |panel, cx| panel
                        .selected_entry_project_path(cx)),
                    Some(generic_path)
                );
                for name in ["main.py", "index.html"] {
                    window.open(&Path::new(GENERIC).join(name)).await?;
                    assert!(selector(&mut window).is_none());
                    window.workspace.read_with(&window.visual, |workspace, cx| {
                        let item = workspace.active_item(cx).expect("Preserved generic editor");
                        assert_eq!(
                            item.project_path(cx)
                                .expect("Generic project path")
                                .path
                                .as_unix_str(),
                            name
                        );
                    });
                }
                assert_eq!(window.workspace.entity_id(), workspace_id);
                assert_eq!(window.multi_workspace.entity_id(), root_id);
                window.multi_workspace.read_with(&window.visual, |multi_workspace, _| {
                assert_eq!(multi_workspace.workspace().entity_id(), workspace_id,
                    "The workspace root and its existing project/branch chooser remain mounted");
            });
                Ok(())
            }
            .await;
        result.expect("C2 generic editing and physical panel preservation fixture must complete");
    }
    fn publish_context_at(root_path: &str, project: &Entity<Project>, cx: &mut App) -> Result<()> {
        let root = Path::new(root_path);
        // Complete Gradle observations include the root even when it applies no supported plugin.
        let record = json!({
            "schema": 1, "root": root_path, "gradleVersion": "9.6.1", "phase": "complete",
            "modules": [{
                "path": ":", "directory": root,
                "plugins": PluginId::ALL.into_iter().map(|plugin| json!({
                    "plugin": plugin, "applied": false
                })).collect::<Vec<_>>(),
                "targets": {"status": "unavailable", "value": {
                    "detail": "No evaluated Kotlin target extension"
                }}
            }, {"path": ":app", "directory": root.join("app"),
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

    fn publish_model_at(root_path: &str, project: &Entity<Project>, cx: &mut App) -> Result<()> {
        project.update(cx, |project, cx| {
            let token = project.invalidate_android_model(Some(PathBuf::from(root_path)), cx);
            let record = format!(
                "{MODEL_OUTPUT_PREFIX}{}",
                EXPORTED_MODEL.replace(ORIGINAL_ROOT, root_path)
            );
            // These fixture directories exist in FakeFs and contain no symlinks;
            // std::fs cannot canonicalize them during model decoding.
            let root = Path::new(root_path);
            let directories = [root.to_path_buf(), root.join("app")]
                .into_iter()
                .map(|path| (path.clone(), path))
                .collect();
            let capture = EvaluatedTreeInputs::decode_fixture(&record, root, &directories, &token)?;
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

    #[gpui::test]
    async fn android_workspace_fallbacks_preserve_hidden_physical_selection(
        cx: &mut TestAppContext,
    ) {
        use futures::TryStreamExt as _;
        let result: Result<()> = async {
            let (project, filesystem) = fixture(cx).await?;
            let mut window = TestWindow::new(&project, cx)?;
            mount_panel(&mut window);
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            rows_ready(&mut window).await;
            choose(&mut window, DefaultView::Project);
            let generic = window
                .visual
                .update(|_, cx| {
                    project
                        .read(cx)
                        .find_project_path(&Path::new(GENERIC).join("main.py"), cx)
                })
                .context("Retained generic physical path")?;
            window.panel.update(&mut window.visual, |panel, cx| {
                panel.select_path_for_test(generic.clone(), cx);
                panel.marked_entries = panel.selection.into_iter().collect();
            });
            let history = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let physical_panel = window.panel.clone();
            window.visual.update(|window, cx| {
                git_ui_core::set_file_history_opener(
                    {
                        let history = history.clone();
                        move |_, path, _, _| history.borrow_mut().push(path.clone())
                    },
                    cx,
                );
                let focus = physical_panel.read(cx).focus_handle.clone();
                focus.focus(window, cx);
            });
            draw_workspace(&mut window);
            window.visual.dispatch_action(git::FileHistory);
            assert_eq!(
                history.borrow().as_slice(),
                &[generic.clone()],
                "The mounted physical File History ancestor route must actually execute"
            );
            history.borrow_mut().clear();
            let (selection, marked, expanded) =
                window.panel.read_with(&window.visual, |panel, _| {
                    (
                        panel.selection,
                        panel.marked_entries.clone(),
                        panel.state.expanded_dir_ids.clone(),
                    )
                });
            let files_before = filesystem
                .read_dir(Path::new(GENERIC))
                .await?
                .try_collect::<Vec<_>>()
                .await?;
            choose(&mut window, DefaultView::Android);
            rows_ready(&mut window).await;
            draw_workspace(&mut window);
            focus_workspace(&mut window);
            window.visual.dispatch_action(crate::Rename);
            focus_workspace(&mut window);
            window.visual.dispatch_action(crate::Duplicate);
            focus_workspace(&mut window);
            window
                .visual
                .dispatch_action(crate::Delete { skip_prompt: true });
            let panel = window.panel.clone();
            window.visual.update(|window, cx| {
                let focus = panel.read(cx).focus_handle.clone();
                focus.focus(window, cx);
            });
            draw_workspace(&mut window);
            window.visual.dispatch_action(git::FileHistory);
            assert!(
                history.borrow().is_empty(),
                "Android rows cannot open hidden physical File History"
            );
            window.panel.read_with(&window.visual, |panel, _| {
                assert!(panel.state.edit_state.is_none());
                assert!(panel.clipboard.is_none());
                assert_eq!(panel.selection, selection);
                assert_eq!(panel.marked_entries, marked);
                assert_eq!(panel.state.expanded_dir_ids, expanded);
            });
            let files_after = filesystem
                .read_dir(Path::new(GENERIC))
                .await?
                .try_collect::<Vec<_>>()
                .await?;
            assert_eq!(
                files_after, files_before,
                "Duplicate/Delete must leave actual physical files unchanged"
            );
            assert!(
                filesystem
                    .is_file(&Path::new(GENERIC).join("main.py"))
                    .await
            );
            focus_workspace(&mut window);
            window.visual.dispatch_action(crate::CollapseAllEntries);
            rows_ready(&mut window).await;
            window.tree().read_with(&window.visual, |tree, _| {
                assert!(tree.expanded_keys.is_empty());
                assert_eq!(tree.logical_rows.len(), 1);
            });
            focus_workspace(&mut window);
            window.visual.dispatch_action(crate::ExpandAllEntries);
            rows_ready(&mut window).await;
            window.tree().read_with(&window.visual, |tree, _| {
                assert!(tree.logical_rows.len() > 1);
                assert!(
                    tree.logical_rows
                        .iter()
                        .any(|row| row.label.as_ref() == "Main")
                );
            });
            window.panel.read_with(&window.visual, |panel, _| {
                assert_eq!(panel.state.expanded_dir_ids, expanded);
            });
            Ok(())
        }
        .await;
        result.expect("Mounted workspace commands must respect the visible logical view");
    }

    #[gpui::test]
    async fn android_rendered_callbacks_reject_replacement_root_with_repeated_keys(
        cx: &mut TestAppContext,
    ) {
        const SECOND: &str = "/tree-owner-second";
        let result: Result<()> = async {
            let (project, filesystem) = fixture(cx).await?;
            filesystem
                .insert_tree(
                    SECOND,
                    json!({"app": {"src": {"main": {
                        "AndroidManifest.xml": "<manifest package=\"example\"/>",
                        "java": {"example": {"Main.java": "package example; class Main {}"}}
                    }}}}),
                )
                .await;
            let (worktree, _) = project
                .update(cx, |project, cx| {
                    project.find_or_create_worktree(Path::new(SECOND), true, cx)
                })
                .await?;
            cx.update(|cx| {
                let store = project.read(cx).worktree_store();
                TrustedWorktrees::try_get_global(cx)
                    .context("Trust store")?
                    .update(cx, |trust, cx| {
                        trust.trust(
                            &store,
                            [PathTrust::Worktree(worktree.read(cx).id())]
                                .into_iter()
                                .collect(),
                            cx,
                        )
                    });
                Ok::<_, anyhow::Error>(())
            })?;
            let mut window = TestWindow::new(&project, cx)?;
            mount_panel(&mut window);
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            rows_ready(&mut window).await;
            let (obsolete_owner, repeated_key) =
                window
                    .tree()
                    .update_in(&mut window.visual, |tree, window, cx| {
                        Ok::<_, anyhow::Error>((
                            tree.rendered_owner(window, cx)
                                .context("Rendered first owner")?,
                            tree.logical_rows
                                .first()
                                .context("First module row")?
                                .key
                                .clone(),
                        ))
                    })?;
            window.visual.update(|_, cx| {
                publish_context_at(SECOND, &project, cx)?;
                publish_model_at(SECOND, &project, cx)
            })?;
            window
                .open(&Path::new(SECOND).join("app/src/main/java/example/Main.java"))
                .await?;
            window.ready().await;
            rows_ready(&mut window).await;
            let current_owner = window.published_owner()?;
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    assert_eq!(
                        tree.logical_rows.first().expect("Replacement module").key,
                        repeated_key
                    );
                    let selected = tree.selected_key.clone();
                    let expanded = tree.expanded_keys.clone();
                    tree.choose_rendered_view(&obsolete_owner, DefaultView::Project, window, cx);
                    tree.select_rendered_row(&obsolete_owner, &repeated_key, true, window, cx);
                    tree.collapse_all_rendered(&obsolete_owner, window, cx);
                    assert_eq!(tree.selected_key, selected);
                    assert_eq!(tree.expanded_keys, expanded);
                    assert!(tree.row_changes.is_empty());
                    assert_eq!(
                        tree.selector_state(window, cx)
                            .expect("Replacement selector")
                            .view,
                        DefaultView::Android
                    );
                });
            assert_eq!(window.published_owner()?, current_owner);
            Ok(())
        }
        .await;
        result.expect("Rendered callbacks must retain their original root/capture owner");
    }

    #[gpui::test]
    async fn android_view_switches_reuse_capture_and_row_jobs_coalesce_cancel_and_drop_off_thread(
        cx: &mut TestAppContext,
    ) {
        let result: Result<()> = async {
            let (project, _) = fixture(cx).await?;
            let mut window = TestWindow::new(&project, cx)?;
            mount_panel(&mut window);
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            rows_ready(&mut window).await;
            let owner = window.published_owner()?;
            let file_revision = window.current_file_revision()?;
            for _ in 0..3 {
                choose(&mut window, DefaultView::Project);
                choose(&mut window, DefaultView::Android);
                assert_eq!(window.published_owner()?, owner);
                assert_eq!(window.current_file_revision()?, file_revision);
            }
            let initial_generation = window
                .tree()
                .read_with(&window.visual, |tree, _| tree.row_generation);
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    tree.collapse_all(window, cx);
                    tree.expand_all(window, cx);
                });
            rows_ready(&mut window).await;
            window.tree().read_with(&window.visual, |tree, _| {
                assert_eq!(
                    tree.row_generation,
                    initial_generation + 1,
                    "Same-turn disclosure jobs coalesce"
                );
                assert_eq!(
                    tree.logical_rows.len(),
                    tree.logical_cache
                        .as_ref()
                        .expect("Cache")
                        .value()
                        .expect("Cache data")
                        .rows
                        .len()
                );
            });
            let (release, gate) = oneshot::channel();
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    tree.row_gate_for_test = Some(gate);
                    tree.collapse_all(window, cx);
                });
            window.visual.run_until_parked();
            let cancelled = window.tree().read_with(&window.visual, |tree, _| {
                tree.row_cancelled.clone().expect("Running gated row job")
            });
            assert!(!cancelled.load(Ordering::Acquire));
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    tree.expand_all(window, cx)
                });
            rows_ready(&mut window).await;
            assert!(cancelled.load(Ordering::Acquire));
            assert!(
                release.send(()).is_err(),
                "Superseded background row future releases its pending gate"
            );
            assert_eq!(window.published_owner()?, owner);
            assert_eq!(window.current_file_revision()?, file_revision);
            window.tree().read_with(&window.visual, |tree, _| {
                assert_eq!(
                    tree.logical_rows.len(),
                    tree.logical_cache
                        .as_ref()
                        .expect("Cache")
                        .value()
                        .expect("Cache data")
                        .rows
                        .len()
                );
            });
            let (dropped, observe_drop) = oneshot::channel();
            window.tree().update(&mut window.visual, |tree, cx| {
                let cached = Arc::get_mut(tree.logical_cache.as_mut().expect("Current row cache"))
                    .expect("Completed row cache has one owner");
                cached.value.as_mut().expect("Cached rows").drop_observer =
                    Some((cx.background_executor().clone(), dropped));
            });
            window.open(&Path::new(GENERIC).join("main.py")).await?;
            assert!(
                !observe_drop.await?,
                "Released cached rows are destroyed on the background executor"
            );
            window.tree().read_with(&window.visual, |tree, _| {
                assert!(tree.logical_rows.is_empty());
                assert!(tree.logical_cache.is_none());
                assert!(tree.row_task.is_none());
            });
            Ok(())
        }
        .await;
        result.expect("Presentation changes and row scheduling must preserve capture ownership and UI responsiveness");
    }

    #[gpui::test]
    async fn android_row_projection_handles_capture_entry_cap_on_background_executor(
        cx: &mut TestAppContext,
    ) {
        use android_tools::project_tree::{
            FileFact, FileKind, SourceProvider, TreeFiles, TreeModel, TreeModule, TreeSourceRoot,
            project_tree,
        };
        let result: Result<()> = async {
            let (project, _) = fixture(cx).await?;
            let mut window = TestWindow::new(&project, cx)?;
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            let owner = window.published_owner()?;
            let executor = cx.executor();
            executor
                .clone()
                .spawn(async move {
                    assert!(!executor.is_main_thread());
                    let directory = PathBuf::from("/large/app/src/main/assets");
                    let model = TreeModel {
                        revision: 1,
                        modules: vec![TreeModule {
                            id: ":app".into(),
                            display_name: "app".into(),
                            directory: "/large/app".into(),
                            source_roots: vec![TreeSourceRoot {
                                path: directory.clone(),
                                group: SourceGroup::Assets,
                                provider: SourceProvider::Named("main".into()),
                            }],
                            compact_packages: true,
                        }],
                    };
                    let mut entries = Vec::with_capacity(100_000);
                    entries.push(FileFact {
                        path: directory.clone(),
                        kind: FileKind::Directory,
                    });
                    for index in 0..99_999 {
                        entries.push(FileFact {
                            path: directory.join(format!("asset-{index:06}.txt")),
                            kind: FileKind::File {
                                java_classes: Vec::new(),
                            },
                        });
                    }
                    assert_eq!(entries.len(), 100_000);
                    let snapshot = project_tree(
                        &model,
                        &TreeFiles {
                            model_revision: 1,
                            revision: 1,
                            entries,
                        },
                    )?;
                    let node_count = snapshot.nodes().count();
                    let cancelled = AtomicBool::new(false);
                    let cache = Arc::new(BackgroundOwned::new(
                        LogicalRowCache::prepare(owner.clone(), &snapshot, &cancelled)?,
                        executor.clone(),
                    ));
                    let update = prepare_visible_rows(
                        cache.clone(),
                        Arc::new(BTreeSet::new()),
                        None,
                        false,
                        vec![RowChange::ExpandAll],
                        &cancelled,
                    )?;
                    assert_eq!(update.rows.len(), node_count);
                    assert_eq!(update.visible_keys.len(), node_count);
                    assert_eq!(
                        update
                            .rows
                            .iter()
                            .filter(|row| matches!(row.key, NodeKey::File { .. }))
                            .count(),
                        99_999
                    );
                    let cancelled = AtomicBool::new(true);
                    assert!(LogicalRowCache::prepare(owner, &snapshot, &cancelled).is_err());
                    assert!(
                        prepare_visible_rows(
                            cache,
                            Arc::new(BTreeSet::new()),
                            None,
                            false,
                            vec![RowChange::ExpandAll],
                            &cancelled
                        )
                        .is_err()
                    );
                    Ok::<_, anyhow::Error>(())
                })
                .await?;
            Ok(())
        }
        .await;
        result
            .expect("The physical capture entry cap must remain usable by background logical rows");
    }

    #[gpui::test]
    async fn android_collapsed_root_disclosure_remains_inside_viewport_and_accepts_pointer(
        cx: &mut TestAppContext,
    ) {
        let result: Result<()> = async {
            let (project, _) = fixture(cx).await?;
            let mut window = TestWindow::new(&project, cx)?;
            mount_panel(&mut window);
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            rows_ready(&mut window).await;
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    tree.collapse_all(window, cx)
                });
            rows_ready(&mut window).await;
            for width in [180., 480.] {
                draw_panel(&mut window, width, 300.);
                let root = window
                    .visual
                    .debug_bounds("android-logical-first-row")
                    .context("Rendered collapsed module")?;
                let half_rem = window.visual.update(|window, _| window.rem_size() * 0.5);
                let disclosure = gpui::point(root.origin.x - half_rem, root.center().y);
                assert!(
                    disclosure.x >= px(0.),
                    "Disclosure gutter must stay inside the clipped viewport"
                );
                window.visual.simulate_click(disclosure, Default::default());
                rows_ready(&mut window).await;
                window.tree().read_with(&window.visual, |tree, _| {
                    assert!(
                        tree.logical_rows.len() > 1,
                        "Actual root disclosure pointer expands rows"
                    );
                });
                window
                    .tree()
                    .update_in(&mut window.visual, |tree, window, cx| {
                        tree.collapse_all(window, cx)
                    });
                rows_ready(&mut window).await;
            }
            window.tree().read_with(&window.visual, |tree, _| {
                let cache = tree
                    .logical_cache
                    .as_ref()
                    .expect("Cached kinds")
                    .value()
                    .expect("Cached rows");
                assert_eq!(
                    cache.rows.first().expect("Module row").row.icon,
                    IconName::BoxOpen
                );
                assert!(
                    cache
                        .rows
                        .iter()
                        .filter(|row| matches!(row.row.key, NodeKey::Directory { .. }))
                        .all(|row| row.row.icon != IconName::File)
                );
                assert!(
                    cache
                        .rows
                        .iter()
                        .filter(|row| matches!(row.row.key, NodeKey::JavaClass { .. }))
                        .all(|row| row.row.icon == IconName::Code)
                );
            });
            Ok(())
        }
        .await;
        result.expect("Root disclosure must be visible and clickable after Collapse All");
    }

    // Round-two regressions exercise the actual GPUI window/action routes and
    // gated jobs. They supplement, rather than replace, all prior assertions.
    fn draw_logical_row(
        window: &mut TestWindow,
        key: &NodeKey,
    ) -> Result<gpui::Bounds<gpui::Pixels>> {
        let tree = window.tree();
        let index = tree
            .read_with(&window.visual, |tree, _| {
                tree.logical_rows.iter().position(|row| &row.key == key)
            })
            .context("Visible row to paint")?;
        window.visual.draw(
            gpui::point(px(0.), px(0.)),
            gpui::size(px(600.), px(100.)),
            |window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.render_row(index, window, cx)
                        .expect("Actual logical row element")
                })
            },
        );
        window
            .visual
            .debug_bounds("android-logical-row-text")
            .context("Painted logical text bounds")
    }

    #[gpui::test]
    async fn android_full_window_project_commands_use_physical_selection_without_parent_leases(
        cx: &mut TestAppContext,
    ) {
        use futures::TryStreamExt as _;
        let result: Result<()> = async {
            let (project, filesystem) = fixture(cx).await?;
            let mut window = TestWindow::new(&project, cx)?;
            mount_panel(&mut window);
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            rows_ready(&mut window).await;
            choose(&mut window, DefaultView::Project);
            let path = window
                .visual
                .update(|_, cx| {
                    project
                        .read(cx)
                        .find_project_path(&Path::new(GENERIC).join("main.py"), cx)
                })
                .context("Physical command path")?;
            window.panel.update(&mut window.visual, |panel, cx| {
                panel.select_path_for_test(path.clone(), cx)
            });
            // This paints MultiWorkspace::actions while both parent entities are
            // leased, including the mounted ProjectPanel FileHistory renderer.
            let history = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let panel = window.panel.clone();
            window.visual.update(|window, cx| {
                git_ui_core::set_file_history_opener(
                    {
                        let history = history.clone();
                        move |_, path, _, _| history.borrow_mut().push(path.clone())
                    },
                    cx,
                );
                let focus = panel.read(cx).focus_handle.clone();
                focus.focus(window, cx);
            });
            draw_workspace(&mut window);
            window.visual.dispatch_action(git::FileHistory);
            assert_eq!(history.borrow().as_slice(), &[path.clone()]);
            focus_workspace(&mut window);
            window.visual.dispatch_action(crate::Rename);
            window.panel.read_with(&window.visual, |panel, _| {
                assert!(
                    panel.state.edit_state.is_some(),
                    "Project fallback Rename must execute"
                );
            });
            window
                .panel
                .update_in(&mut window.visual, |panel, window, cx| {
                    panel.cancel(&menu::Cancel, window, cx)
                });
            window.panel.update(&mut window.visual, |panel, cx| {
                panel.select_path_for_test(path.clone(), cx)
            });
            let before = filesystem
                .read_dir(Path::new(GENERIC))
                .await?
                .try_collect::<Vec<_>>()
                .await?;
            focus_workspace(&mut window);
            window.visual.dispatch_action(crate::Duplicate);
            window.visual.run_until_parked();
            let after = filesystem
                .read_dir(Path::new(GENERIC))
                .await?
                .try_collect::<Vec<_>>()
                .await?;
            assert_eq!(
                after.len(),
                before.len() + 1,
                "Project fallback Duplicate must create a real file"
            );
            window.panel.read_with(&window.visual, |panel, _| {
                assert!(panel.clipboard.is_some())
            });
            window
                .panel
                .update_in(&mut window.visual, |panel, window, cx| {
                    panel.cancel(&menu::Cancel, window, cx)
                });
            focus_workspace(&mut window);
            window.visual.dispatch_action(crate::ExpandAllEntries);
            window.visual.run_until_parked();
            let expanded = window.panel.read_with(&window.visual, |panel, _| {
                panel
                    .state
                    .expanded_dir_ids
                    .values()
                    .map(Vec::len)
                    .sum::<usize>()
            });
            focus_workspace(&mut window);
            window.visual.dispatch_action(crate::CollapseAllEntries);
            window.visual.run_until_parked();
            let collapsed = window.panel.read_with(&window.visual, |panel, _| {
                panel
                    .state
                    .expanded_dir_ids
                    .values()
                    .map(Vec::len)
                    .sum::<usize>()
            });
            assert!(
                expanded > collapsed,
                "Project fallback expansion must affect physical directories"
            );
            window.panel.update(&mut window.visual, |panel, cx| {
                panel.select_path_for_test(path.clone(), cx)
            });
            focus_workspace(&mut window);
            window
                .visual
                .dispatch_action(crate::Delete { skip_prompt: true });
            window.visual.run_until_parked();
            assert!(
                !filesystem
                    .is_file(&Path::new(GENERIC).join("main.py"))
                    .await,
                "Project fallback Delete must execute"
            );
            assert_eq!(
                selector(&mut window)
                    .context("Explicit Project selector")?
                    .view,
                DefaultView::Project
            );
            draw_workspace(&mut window);
            Ok(())
        }
        .await;
        result.expect("Mounted explicit Project paint/actions must stay lease-safe and retain physical behavior");
    }

    #[gpui::test]
    async fn android_stale_sync_cancels_held_row_job_and_renders_recovery(cx: &mut TestAppContext) {
        let result: Result<()> = async {
            let (project, _) = fixture(cx).await?;
            let mut window = TestWindow::new(&project, cx)?;
            mount_panel(&mut window);
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            rows_ready(&mut window).await;
            choose(&mut window, DefaultView::Android);
            let (release, gate) = oneshot::channel();
            window.tree().update_in(&mut window.visual, |tree, window, cx| {
                tree.row_gate_for_test = Some(gate);
                tree.expand_all(window, cx);
            });
            window.visual.run_until_parked();
            let cancelled = window.tree().read_with(&window.visual, |tree, _| {
                assert!(tree.row_task.is_some());
                tree.row_cancelled.clone().expect("Held row cancellation identity")
            });
            project.update(&mut window.visual, |project, cx| {
                project.invalidate_android_model(Some(PathBuf::from(ROOT)), cx);
            });
            window.visual.run_until_parked();
            assert!(cancelled.load(Ordering::Acquire));
            assert!(release.send(()).is_err(), "Obsolete row work releases its held receiver");
            window.tree().update_in(&mut window.visual, |tree, window, cx| {
                assert!(tree.row_task.is_none());
                assert!(tree.row_cancelled.is_none());
                assert_eq!(tree.status, Status::Stale);
                assert!(tree.current_capture(window, cx).is_none());
                assert!(!tree.logical_rows.is_empty(), "Stale rows remain available as disabled information");
                let selected = tree.selected_key.clone();
                let key = tree.logical_rows[0].key.clone();
                tree.select_row(&key, true, window, cx);
                assert_eq!(tree.selected_key, selected);
            });
            draw_panel(&mut window, 480., 300.);
            assert!(window.visual.debug_bounds("android-tree-message:Android view is out of date. Sync the project to refresh.").is_some());
            assert!(window.visual.debug_bounds("android-tree-message:Loading Android view…").is_none());
            window.visual.update(|_, cx| publish_model(&project, cx))?;
            window.ready().await;
            rows_ready(&mut window).await;
            window.tree().update_in(&mut window.visual, |tree, window, cx| {
                assert_eq!(tree.status, Status::Ready);
                assert!(tree.rows_are_current(window, cx));
                assert!(tree.row_task.is_none());
            });
            Ok(())
        }.await;
        result.expect("Invalidation during held row work must release job ownership and display sync recovery");
    }

    #[gpui::test]
    async fn android_late_collapse_click_selects_visible_ancestor_and_keeps_new_visible_choice(
        cx: &mut TestAppContext,
    ) {
        let result: Result<()> = async {
            let (project, _) = fixture(cx).await?;
            let mut window = TestWindow::new(&project, cx)?;
            mount_panel(&mut window);
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            expand_visible_groups(&mut window)?;
            let (root, child) = window.tree().read_with(&window.visual, |tree, _| {
                Ok::<_, anyhow::Error>((
                    tree.logical_rows.first().context("Module")?.key.clone(),
                    tree.logical_rows
                        .iter()
                        .find(|row| row.label.as_ref() == "Main")
                        .context("Visible child")?
                        .key
                        .clone(),
                ))
            })?;
            for click_visible_root_last in [false, true] {
                window
                    .tree()
                    .update_in(&mut window.visual, |tree, window, cx| {
                        tree.expand_all(window, cx)
                    });
                rows_ready(&mut window).await;
                let (release, gate) = oneshot::channel();
                window
                    .tree()
                    .update_in(&mut window.visual, |tree, window, cx| {
                        tree.row_gate_for_test = Some(gate);
                        tree.collapse_all(window, cx);
                    });
                window.visual.run_until_parked();
                let before = window
                    .tree()
                    .read_with(&window.visual, |tree, _| tree.selection_generation);
                let bounds = draw_logical_row(&mut window, &child)?;
                window
                    .visual
                    .simulate_click(bounds.center(), gpui::Modifiers::default());
                window.tree().read_with(&window.visual, |tree, _| {
                    assert_eq!(
                        tree.selected_key.as_ref(),
                        Some(&child),
                        "The actual old child callback fires during the held collapse"
                    );
                    assert!(tree.selection_generation > before);
                });
                if click_visible_root_last {
                    let bounds = draw_logical_row(&mut window, &root)?;
                    window
                        .visual
                        .simulate_click(bounds.center(), gpui::Modifiers::default());
                    window.tree().read_with(&window.visual, |tree, _| {
                        assert_eq!(tree.selected_key.as_ref(), Some(&root))
                    });
                }
                release
                    .send(())
                    .map_err(|_| anyhow::anyhow!("Current collapse gate was cancelled"))?;
                rows_ready(&mut window).await;
                window.tree().read_with(&window.visual, |tree, _| {
                    assert_eq!(tree.selected_key.as_ref(), Some(&root));
                    assert!(
                        tree.visible_keys
                            .contains(tree.selected_key.as_ref().expect("Visible selection"))
                    );
                    assert!(!tree.visible_keys.contains(&child));
                    assert!(tree.expanded_keys.is_empty());
                });
            }
            Ok(())
        }
        .await;
        result.expect(
            "Late real pointer selection must remain visible after background collapse publication",
        );
    }

    #[gpui::test]
    async fn android_command_snapshots_coalesce_parity_and_keep_exact_cache_identity_on_reorder(
        cx: &mut TestAppContext,
    ) {
        let result: Result<()> = async {
            let (project, filesystem) = fixture(cx).await?;
            let directories = (0..64)
                .map(|index| (format!("z-{index:03}"), json!({"asset.txt":"asset"})))
                .collect::<serde_json::Map<_, _>>();
            filesystem
                .insert_tree("/tree-owner/app/flavor-assets", json!(directories))
                .await;
            let mut window = TestWindow::new(&project, cx)?;
            mount_panel(&mut window);
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    tree.expand_all(window, cx)
                });
            rows_ready(&mut window).await;
            let (cache, keys) = window.tree().read_with(&window.visual, |tree, _| {
                (
                    tree.logical_cache.clone().expect("Initial cache"),
                    tree.logical_rows
                        .iter()
                        .filter(|row| row.has_children && row.label.starts_with("z-"))
                        .map(|row| row.key.clone())
                        .collect::<Vec<_>>(),
                )
            });
            assert_eq!(keys.len(), 64);
            let mut retired = Vec::new();
            for key in &keys {
                let (release, gate) = oneshot::channel();
                window
                    .tree()
                    .update_in(&mut window.visual, |tree, window, cx| {
                        tree.row_gate_for_test = Some(gate);
                        // Three toggles retain exactly one command for this key.
                        for _ in 0..3 {
                            tree.select_row(key, true, window, cx);
                        }
                        let commands = tree.row_changes.clone();
                        assert!(Arc::ptr_eq(
                            commands.cache.as_ref().expect("Bound command cache"),
                            &cache
                        ));
                        assert!(Arc::ptr_eq(
                            commands.toggles.as_ref().expect("Shared snapshot root"),
                            tree.row_changes.toggles.as_ref().expect("Current root")
                        ));
                        assert!(
                            commands
                                .toggles
                                .as_ref()
                                .expect("Parity set")
                                .value()
                                .expect("Parity data")
                                .count
                                <= keys.len()
                        );
                    });
                window.visual.run_until_parked();
                retired.push(release);
            }
            window.tree().read_with(&window.visual, |tree, _| {
                assert_eq!(
                    tree.row_changes
                        .toggles
                        .as_ref()
                        .expect("Bounded backlog")
                        .value()
                        .expect("Backlog data")
                        .count,
                    keys.len()
                );
                assert!(tree.row_task.is_some());
            });
            let old_index = *cache
                .value()?
                .keys
                .get(&keys[0])
                .context("Old target index")?;
            filesystem
                .insert_tree(
                    "/tree-owner/app/flavor-assets",
                    json!({"000-before":{"asset.txt":"new predecessor"}}),
                )
                .await;
            window.visual.run_until_parked();
            window.ready().await;
            rows_ready(&mut window).await;
            for release in retired {
                assert!(
                    release.send(()).is_err(),
                    "Superseded row jobs release all held gates"
                );
            }
            window.tree().read_with(&window.visual, |tree, _| {
                let current = tree.logical_cache.as_ref().expect("Replacement cache");
                assert!(!Arc::ptr_eq(current, &cache));
                assert_ne!(
                    *current
                        .value()
                        .expect("Current data")
                        .keys
                        .get(&keys[0])
                        .expect("Same target key"),
                    old_index
                );
                for key in &keys {
                    assert!(
                        !tree.expanded_keys.contains(key),
                        "Parity must address the original NodeKey after cache reorder"
                    );
                }
                assert!(
                    tree.row_changes.is_empty(),
                    "Only successful matching publication acknowledges the cursor"
                );
            });
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    let root = tree.logical_rows[0].key.clone();
                    tree.expand_all(window, cx);
                    tree.select_row(&root, true, window, cx);
                    tree.collapse_all(window, cx);
                    tree.select_row(&root, true, window, cx);
                });
            rows_ready(&mut window).await;
            window.tree().read_with(&window.visual, |tree, _| {
                assert_eq!(
                    tree.expanded_keys.len(),
                    1,
                    "Last reset wins and subsequent toggles retain order"
                );
                assert!(tree.expanded_keys.contains(&tree.logical_rows[0].key));
                assert!(tree.logical_rows.len() > 1);
                assert!(tree.row_changes.is_empty());
            });
            // Once expanded again, an even burst cancels rather than accumulating.
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    tree.expand_all(window, cx)
                });
            rows_ready(&mut window).await;
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    for _ in 0..2048 {
                        tree.select_row(&keys[0], true, window, cx);
                    }
                    assert!(
                        tree.row_changes.is_empty(),
                        "An even toggle burst has no retained commands"
                    );
                });
            rows_ready(&mut window).await;
            window.tree().read_with(&window.visual, |tree, _| {
                assert!(tree.expanded_keys.contains(&keys[0]))
            });
            Ok(())
        }
        .await;
        result.expect("Coalesced immutable commands stay bounded, cancel superseded work, and survive actual cache reindexing");
    }

    #[gpui::test]
    async fn android_captured_filename_and_provider_controls_render_as_single_line(
        cx: &mut TestAppContext,
    ) {
        use android_tools::project_tree::{
            FileFact, FileKind, SourceProvider, TreeFiles, TreeModel, TreeModule, TreeSourceRoot,
            project_tree,
        };
        let result: Result<()> = async {
            let (project, filesystem) = fixture(cx).await?;
            filesystem
                .insert_tree(
                    "/tree-owner/app/src/main/assets",
                    json!({"plain.txt":"plain", "odd\n\tname.txt":"control filename"}),
                )
                .await;
            filesystem
                .insert_tree(
                    "/tree-owner/app/flavor-assets",
                    json!({"provider.txt":"control provider", "both\n\tname.txt":"both"}),
                )
                .await;
            let mut window = TestWindow::new(&project, cx)?;
            mount_panel(&mut window);
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    tree.expand_all(window, cx)
                });
            rows_ready(&mut window).await;
            let keys = window.tree().read_with(&window.visual, |tree, _| {
                ["plain.txt", "odd\n\tname.txt"]
                    .into_iter()
                    .map(|name| {
                        let row = tree
                            .logical_rows
                            .iter()
                            .find(|row| row.label.as_ref() == name)
                            .context("Actually captured Unix filename")?;
                        assert!(row.annotation.is_none());
                        Ok(row.key.clone())
                    })
                    .collect::<Result<Vec<_>>>()
            })?;
            let plain_height = draw_logical_row(&mut window, &keys[0])?.size.height;
            assert!(plain_height > px(0.));
            let bounds = draw_logical_row(&mut window, &keys[1])?;
            assert_eq!(
                bounds.size.height, plain_height,
                "Actually captured newline/tab filename retains one-line geometry"
            );
            window
                .visual
                .simulate_click(bounds.center(), gpui::Modifiers::default());
            window.tree().read_with(&window.visual, |tree, _| {
                assert_eq!(tree.selected_key.as_ref(), Some(&keys[1]))
            });

            // Exported Gradle provider names reject controls. Keep that boundary
            // intact; exercise annotation defenses through the real lower-level
            // TreeModel producer/projection and guarded row publication instead.
            let owner = window.published_owner()?;
            let executor = cx.executor();
            let update = executor
                .clone()
                .spawn({
                    let owner = owner.clone();
                    async move {
                        let directory = PathBuf::from("/tree-owner/app/flavor-assets");
                        let model = TreeModel {
                            revision: 1,
                            modules: vec![TreeModule {
                                id: ":app".into(),
                                display_name: "app".into(),
                                directory: "/tree-owner/app".into(),
                                source_roots: vec![TreeSourceRoot {
                                    path: directory.clone(),
                                    group: SourceGroup::Assets,
                                    provider: SourceProvider::Named("flavor\n\tname".into()),
                                }],
                                compact_packages: true,
                            }],
                        };
                        let entries = vec![
                            FileFact {
                                path: directory.clone(),
                                kind: FileKind::Directory,
                            },
                            FileFact {
                                path: directory.join("provider.txt"),
                                kind: FileKind::File {
                                    java_classes: Vec::new(),
                                },
                            },
                            FileFact {
                                path: directory.join("both\n\tname.txt"),
                                kind: FileKind::File {
                                    java_classes: Vec::new(),
                                },
                            },
                        ];
                        let snapshot = project_tree(
                            &model,
                            &TreeFiles {
                                model_revision: 1,
                                revision: 1,
                                entries,
                            },
                        )?;
                        let cancelled = AtomicBool::new(false);
                        let cache = Arc::new(BackgroundOwned::new(
                            LogicalRowCache::prepare(owner, &snapshot, &cancelled)?,
                            executor.clone(),
                        ));
                        let update = prepare_visible_rows(
                            cache,
                            Arc::new(BTreeSet::new()),
                            None,
                            false,
                            vec![RowChange::ExpandAll],
                            &cancelled,
                        )?;
                        Ok::<_, anyhow::Error>(BackgroundOwned::new(update, executor))
                    }
                })
                .await?;
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    let cursor = RowJobCursor {
                        generation: tree.row_generation,
                        selection_generation: tree.selection_generation,
                        change_generation: tree.row_change_generation,
                    };
                    tree.finish_rows(&owner, cursor, Ok(update), window, cx);
                });
            window.visual.run_until_parked();
            let keys = window.tree().read_with(&window.visual, |tree, _| {
                ["provider.txt", "both\n\tname.txt"]
                    .into_iter()
                    .map(|name| {
                        let row = tree
                            .logical_rows
                            .iter()
                            .find(|row| row.label.as_ref() == name)
                            .context("Actual producer control annotation")?;
                        assert_eq!(row.annotation.as_deref(), Some("(flavor\n\tname)"));
                        Ok(row.key.clone())
                    })
                    .collect::<Result<Vec<_>>>()
            })?;
            for key in &keys {
                let bounds = draw_logical_row(&mut window, key)?;
                assert_eq!(
                    bounds.size.height, plain_height,
                    "Actual provider annotation and filename controls retain one-line geometry"
                );
                window
                    .visual
                    .simulate_click(bounds.center(), gpui::Modifiers::default());
                window.tree().read_with(&window.visual, |tree, _| {
                    assert_eq!(tree.selected_key.as_ref(), Some(key))
                });
            }
            Ok(())
        }
        .await;
        result.expect("Captured filenames and producer annotations with newline/tab retain one-line geometry and actual pointer selection");
    }

    #[gpui::test]
    async fn android_discarded_command_snapshot_releases_its_trie_off_thread(
        cx: &mut TestAppContext,
    ) {
        let result: Result<()> = async {
            let (project, _) = fixture(cx).await?;
            let mut window = TestWindow::new(&project, cx)?;
            window.open(Path::new(MAIN)).await?;
            window.ready().await;
            rows_ready(&mut window).await;
            let (release, gate) = oneshot::channel();
            let (dropped, observe_drop) = oneshot::channel();
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    let root = tree.logical_rows[0].key.clone();
                    tree.row_gate_for_test = Some(gate);
                    tree.select_row(&root, true, window, cx);
                    let commands = Arc::get_mut(
                        tree.row_changes
                            .toggles
                            .as_mut()
                            .expect("Unique queued toggle root"),
                    )
                    .expect("Before the deferred job starts its immutable snapshot is unshared");
                    commands
                        .value
                        .as_mut()
                        .expect("Actual toggle payload")
                        .drop_observer = Some((cx.background_executor().clone(), dropped));
                });
            window.visual.run_until_parked();
            let cancelled = window.tree().read_with(&window.visual, |tree, _| {
                tree.row_cancelled.clone().expect("Held snapshot job")
            });
            window
                .tree()
                .update_in(&mut window.visual, |tree, window, cx| {
                    tree.collapse_all(window, cx)
                });
            rows_ready(&mut window).await;
            assert!(cancelled.load(Ordering::Acquire));
            assert!(release.send(()).is_err());
            assert!(
                !observe_drop.await?,
                "Discarding a shared command trie must destroy it on the background executor"
            );
            window.tree().read_with(&window.visual, |tree, _| {
                assert!(tree.row_changes.is_empty());
                assert!(tree.row_task.is_none());
                assert!(tree.expanded_keys.is_empty());
            });
            Ok(())
        }
        .await;
        result.expect("Command cancellation/reset preserves off-thread payload destruction");
    }
}
