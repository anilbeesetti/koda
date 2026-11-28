use super::*;
use android_tools::project_model::{ModelToken, ProjectModel, SourceKind};
use db::kvp::KeyValueStore;
use futures::{FutureExt as _, StreamExt as _, future::Shared};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
struct Row {
    key: String,
    parent: Option<String>,
    label: String,
    depth: usize,
    target: Option<SelectedEntry>,
    branch: bool,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct SavedTree {
    enabled: bool,
    collapsed: BTreeSet<String>,
    selected: Option<String>,
}

#[derive(Default)]
pub(super) struct AndroidTree {
    pub enabled: bool,
    workspace_id: Option<workspace::WorkspaceId>,
    root: Option<PathBuf>,
    token: Option<ModelToken>,
    scans: Vec<(WorktreeId, usize)>,
    rows: Vec<Row>,
    visible: Vec<usize>,
    saved: SavedTree,
    job: Option<Task<()>>,
    save: Option<Shared<Task<()>>>,
    saved_roots: BTreeMap<PathBuf, SavedTree>,
    revision: u64,
    published_revision: u64,
    scroll: UniformListScrollHandle,
    loading: bool,
    pending_reveal: Option<ProjectEntryId>,
}

fn key(parts: &[&str]) -> String {
    // JSON encoding avoids collisions between paths, module names and separators.
    serde_json::to_string(parts).expect("serializing string slices cannot fail")
}

fn project_rows(
    model: &ProjectModel,
    variants: &BTreeMap<String, String>,
    snapshots: &[worktree::Snapshot],
) -> Vec<Row> {
    let files: BTreeMap<PathBuf, SelectedEntry> = snapshots
        .iter()
        .flat_map(|snapshot| {
            snapshot
                .entries(true, 0)
                .filter(|entry| entry.is_file())
                .map(|entry| {
                    (
                        snapshot.abs_path().join(entry.path.as_std_path()),
                        SelectedEntry {
                            worktree_id: snapshot.id(),
                            entry_id: entry.id,
                        },
                    )
                })
        })
        .collect();
    let mut rows = Vec::new();
    let mut modules = model.modules.iter().collect::<Vec<_>>();
    modules.sort_by_key(|module| &module.path);
    for module in modules {
        let module_key = key(&[&module.path]);
        rows.push(Row {
            key: module_key.clone(),
            parent: None,
            label: module.path.clone(),
            depth: 0,
            target: None,
            branch: true,
        });
        let variant = variants
            .get(&module.path)
            .or(module.default_variant.as_ref())
            .and_then(|name| module.variants.iter().find(|variant| &variant.name == name));
        if let Some(variant) = variant {
            for component in &variant.components {
                let component_key = key(&[&module.path, &component.name]);
                rows.push(Row {
                    key: component_key.clone(),
                    parent: Some(module_key.clone()),
                    label: component.name.clone(),
                    depth: 1,
                    target: None,
                    branch: true,
                });
                let mut seen_sources = BTreeSet::new();
                for source in &component.sources {
                    let source_path = source.path.to_string_lossy();
                    let shared_language_root =
                        matches!(source.kind, SourceKind::Java | SourceKind::Kotlin)
                            && component.sources.iter().any(|other| {
                                other.path == source.path
                                    && matches!(other.kind, SourceKind::Java | SourceKind::Kotlin)
                                    && other.kind != source.kind
                            });
                    let kind = match source.kind {
                        SourceKind::Java | SourceKind::Kotlin if shared_language_root => {
                            "java/kotlin"
                        }
                        SourceKind::Java => "java",
                        SourceKind::Kotlin => "kotlin",
                        SourceKind::Resources => "res",
                        SourceKind::Assets => "assets",
                        SourceKind::Manifest => "manifest",
                    };
                    if !seen_sources.insert((kind, &source.path)) {
                        continue;
                    }
                    let source_key = key(&[&module.path, &component.name, kind, &source_path]);
                    if source.kind == SourceKind::Manifest {
                        if let Some(target) = files.get(&source.path) {
                            rows.push(Row {
                                key: source_key,
                                parent: Some(component_key.clone()),
                                label: "AndroidManifest.xml".into(),
                                depth: 2,
                                target: Some(*target),
                                branch: false,
                            });
                        }
                        continue;
                    }
                    let relative = source
                        .path
                        .strip_prefix(&module.directory)
                        .unwrap_or(&source.path);
                    let label = format!(
                        "{kind}{} · {}",
                        if source.generated { " (generated)" } else { "" },
                        relative.display()
                    );
                    rows.push(Row {
                        key: source_key.clone(),
                        parent: Some(component_key.clone()),
                        label,
                        depth: 2,
                        target: None,
                        branch: true,
                    });
                    let mut groups: BTreeMap<String, Vec<(&PathBuf, &SelectedEntry)>> =
                        BTreeMap::new();
                    for (path, target) in files.range(source.path.clone()..) {
                        if !path.starts_with(&source.path) {
                            break;
                        }
                        let Ok(relative) = path.strip_prefix(&source.path) else {
                            continue;
                        };
                        let parent = relative.parent().unwrap_or(Path::new(""));
                        let package =
                            if matches!(source.kind, SourceKind::Java | SourceKind::Kotlin) {
                                parent
                                    .components()
                                    .map(|part| part.as_os_str().to_string_lossy())
                                    .collect::<Vec<_>>()
                                    .join(".")
                            } else {
                                parent.to_string_lossy().into_owned()
                            };
                        groups.entry(package).or_default().push((path, target));
                    }
                    for (group, files) in groups {
                        let group_key =
                            key(&[&module.path, &component.name, kind, &source_path, &group]);
                        let (parent, depth) = if group.is_empty() {
                            (source_key.clone(), 3)
                        } else {
                            rows.push(Row {
                                key: group_key.clone(),
                                parent: Some(source_key.clone()),
                                label: group,
                                depth: 3,
                                target: None,
                                branch: true,
                            });
                            (group_key, 4)
                        };
                        for (path, target) in files {
                            rows.push(Row {
                                key: key(&[
                                    &module.path,
                                    &component.name,
                                    kind,
                                    &source_path,
                                    &path.to_string_lossy(),
                                ]),
                                parent: Some(parent.clone()),
                                label: path
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .into_owned(),
                                depth,
                                target: Some(*target),
                                branch: false,
                            });
                        }
                    }
                }
            }
        }
        for name in [
            "build.gradle.kts",
            "build.gradle",
            "google-services.json",
            "resources.properties",
        ] {
            let path = module.directory.join(name);
            if let Some(target) = files.get(&path) {
                rows.push(Row {
                    key: key(&[&module.path, name]),
                    parent: Some(module_key.clone()),
                    label: name.into(),
                    depth: 1,
                    target: Some(*target),
                    branch: false,
                });
            }
        }
    }
    rows
}

impl AndroidTree {
    pub(super) fn new(workspace_id: Option<workspace::WorkspaceId>) -> Self {
        Self {
            workspace_id,
            ..Default::default()
        }
    }
    fn rebuild_visible(&mut self) {
        self.visible.clear();
        let mut hidden_depth = None;
        for (index, row) in self.rows.iter().enumerate() {
            if hidden_depth.is_some_and(|depth| row.depth > depth) {
                continue;
            }
            hidden_depth = None;
            self.visible.push(index);
            if self.saved.collapsed.contains(&row.key) {
                hidden_depth = Some(row.depth);
            }
        }
        while let Some(selected) = self.saved.selected.as_ref() {
            if self
                .visible
                .iter()
                .any(|index| self.rows[*index].key == *selected)
            {
                break;
            }
            self.saved.selected = self
                .rows
                .iter()
                .find(|row| row.key == *selected)
                .and_then(|row| row.parent.clone());
        }
    }

    fn selected_row(&self) -> Option<&Row> {
        let key = self.saved.selected.as_ref()?;
        self.rows.iter().find(|row| &row.key == key)
    }
}

impl ProjectPanel {
    pub(super) fn observe_android_tree(&mut self, cx: &mut Context<Self>) {
        cx.observe(&self.project, |this, _, cx| this.update_android_tree(cx))
            .detach();
        cx.subscribe(&self.project, |this, _, event, cx| {
            let relevant = match event {
                project::Event::WorktreeUpdatedEntries(id, _) => this
                    .android_worktree(cx)
                    .is_some_and(|worktree| worktree.read(cx).id() == *id),
                project::Event::WorktreeRemoved(_) | project::Event::WorktreeAdded(_) => true,
                _ => false,
            };
            if relevant {
                this.android_tree.revision = this.android_tree.revision.wrapping_add(1);
                this.update_android_tree(cx);
            }
        })
        .detach();
        if let Some(trust) = project::trusted_worktrees::TrustedWorktrees::try_get_global(cx) {
            cx.observe(&trust, |this, _, cx| this.update_android_tree(cx))
                .detach();
        }
        self.update_android_tree(cx);
    }

    fn android_worktree(&self, cx: &App) -> Option<Entity<Worktree>> {
        let project = self.project.read(cx);
        let root = project.android_model().root()?;
        project
            .worktrees(cx)
            .filter(|worktree| root.starts_with(worktree.read(cx).abs_path().as_ref()))
            .max_by_key(|worktree| worktree.read(cx).abs_path().components().count())
    }

    fn android_available(&self, cx: &App) -> bool {
        let project = self.project.read(cx);
        let Some(worktree) = self.android_worktree(cx) else {
            return false;
        };
        project.is_local()
            && project.android_model().model.is_some()
            && project::trusted_worktrees::TrustedWorktrees::try_get_global(cx).is_none_or(
                |trusted| {
                    !trusted
                        .read(cx)
                        .restricted_worktrees(&project.worktree_store(), cx)
                        .iter()
                        .any(|(id, _)| *id == worktree.read(cx).id())
                },
            )
    }

    fn android_scans(&self, cx: &App) -> Vec<(WorktreeId, usize)> {
        self.android_worktree(cx)
            .into_iter()
            .map(|worktree| (worktree.read(cx).id(), worktree.read(cx).scan_id()))
            .collect()
    }

    fn android_storage_key(&self, _cx: &App) -> Option<String> {
        let workspace_id = self.android_tree.workspace_id?;
        Some(format!(
            "{}:{}",
            i64::from(workspace_id),
            self.android_tree.root.as_ref()?.display()
        ))
    }

    fn save_android_tree(&mut self, cx: &mut Context<Self>) {
        self.android_tree.saved.enabled = self.android_tree.enabled;
        let Some(key) = self.android_storage_key(cx) else {
            return;
        };
        let Some(value) = serde_json::to_string(&self.android_tree.saved).log_err() else {
            return;
        };
        let db = KeyValueStore::global(cx);
        let previous = self.android_tree.save.take();
        let save = cx
            .spawn(async move |this, cx| {
                if let Some(previous) = previous {
                    previous.await;
                }
                if let Err(error) = db.scoped("AndroidProjectTree.v1").write(key, value).await {
                    this.update(cx, |this, cx| {
                        this.workspace
                            .update(cx, |workspace, cx| {
                                workspace.show_error(error, cx);
                            })
                            .log_err();
                    })
                    .log_err();
                }
            })
            .shared();
        self.android_tree.save = Some(save.clone());
        cx.spawn(async move |_, _| save.await).detach();
    }

    fn update_android_tree(&mut self, cx: &mut Context<Self>) {
        let project = self.project.read(cx);
        let token = project.android_model().token();
        let root = project.android_model().root().map(Path::to_path_buf);
        if self.android_tree.root != root {
            if let Some(previous) = self.android_tree.root.take() {
                self.android_tree.saved.enabled = self.android_tree.enabled;
                self.android_tree
                    .saved_roots
                    .insert(previous, self.android_tree.saved.clone());
            }
            self.android_tree.root = root;
            self.android_tree.saved = SavedTree::default();
            self.android_tree.pending_reveal = None;
            if let Some(saved) = self
                .android_tree
                .root
                .as_ref()
                .and_then(|root| self.android_tree.saved_roots.get(root))
                .cloned()
            {
                self.android_tree.saved = saved;
            } else if let Some(key) = self.android_storage_key(cx) {
                if let Some(value) = KeyValueStore::global(cx)
                    .scoped("AndroidProjectTree.v1")
                    .read(&key)
                    .log_err()
                    .flatten()
                {
                    if let Some(saved) = serde_json::from_str(&value).log_err() {
                        self.android_tree.saved = saved;
                    }
                }
            }
            self.android_tree.enabled = self.android_tree.saved.enabled;
            self.android_tree.scroll = UniformListScrollHandle::new();
        }
        if !self.android_available(cx) {
            self.android_tree.job = None;
            self.android_tree.rows.clear();
            self.android_tree.visible.clear();
            self.android_tree.token = None;
            self.android_tree.loading = false;
            cx.notify();
            return;
        }
        let project = self.project.read(cx);
        let model = project
            .android_model()
            .model
            .clone()
            .expect("availability checked");
        let variants = project
            .android_model()
            .selected
            .as_ref()
            .map(|selected| selected.variants.clone())
            .unwrap_or_default();
        let worktrees = self.android_worktree(cx).into_iter().collect::<Vec<_>>();
        let scans = worktrees
            .iter()
            .map(|worktree| (worktree.read(cx).id(), worktree.read(cx).scan_id()))
            .collect::<Vec<_>>();
        if self.android_tree.token.as_ref() == Some(&token)
            && (self.android_tree.loading
                || (self.android_tree.scans == scans
                    && self.android_tree.published_revision == self.android_tree.revision))
        {
            return;
        }
        let new_model = self.android_tree.token.as_ref() != Some(&token);
        self.android_tree.token = Some(token.clone());
        self.android_tree.scans = scans;
        self.android_tree.loading = true;
        let mut barriers = Vec::new();
        if new_model {
            self.android_tree.rows.clear();
            self.android_tree.visible.clear();
            for worktree in &worktrees {
                let worktree = worktree.read(cx);
                if let Some(local) = worktree.as_local() {
                    for source in model
                        .modules
                        .iter()
                        .filter_map(|module| {
                            variants
                                .get(&module.path)
                                .or(module.default_variant.as_ref())
                                .and_then(|name| {
                                    module.variants.iter().find(|variant| &variant.name == name)
                                })
                        })
                        .flat_map(|variant| &variant.components)
                        .flat_map(|component| &component.sources)
                    {
                        if let Ok(relative) = source.path.strip_prefix(worktree.abs_path().as_ref())
                        {
                            if let Ok(relative) = RelPath::new(relative, PathStyle::local()) {
                                barriers.push(local.add_path_prefix_to_scan(relative.into()));
                            }
                        }
                    }
                }
            }
        }
        self.android_tree.job = Some(cx.spawn(async move |this, cx| {
            for mut barrier in barriers {
                barrier.next().await;
            }
            let Ok(completions) = this.update(cx, |_, cx| {
                worktrees
                    .iter()
                    .filter_map(|worktree| {
                        worktree
                            .read(cx)
                            .as_local()
                            .map(|local| local.scan_complete())
                    })
                    .collect::<Vec<_>>()
            }) else {
                return;
            };
            for completion in completions {
                completion.await;
            }
            let Ok((snapshots, revision)) = this.update(cx, |this, cx| {
                (
                    worktrees
                        .iter()
                        .map(|worktree| worktree.read(cx).snapshot())
                        .collect::<Vec<_>>(),
                    this.android_tree.revision,
                )
            }) else {
                return;
            };
            let snapshot_scans = snapshots
                .iter()
                .map(|snapshot| (snapshot.id(), snapshot.scan_id()))
                .collect::<Vec<_>>();
            let rows = cx
                .background_spawn(async move { project_rows(&model, &variants, &snapshots) })
                .await;
            this.update(cx, |this, cx| {
                if this.project.read(cx).android_model().is_current(&token)
                    && this.android_available(cx)
                {
                    this.android_tree.loading = false;
                    let current_scans = this.android_scans(cx);
                    if current_scans != snapshot_scans || this.android_tree.revision != revision {
                        this.android_tree.scans.clear();
                        this.update_android_tree(cx);
                        return;
                    }
                    this.android_tree.scans = current_scans;
                    this.android_tree.published_revision = revision;
                    this.android_tree.rows = rows;
                    this.android_tree.saved.collapsed.retain(|key| {
                        this.android_tree
                            .rows
                            .iter()
                            .any(|row| row.branch && &row.key == key)
                    });
                    this.android_tree.rebuild_visible();
                    this.android_tree.loading = false;
                    if let Some(entry) = this.android_tree.pending_reveal.take() {
                        this.reveal_android_entry(entry, cx);
                    }
                    cx.notify();
                }
            })
            .log_err();
        }));
        cx.notify();
    }

    pub(super) fn render_project_view_selector(&self, cx: &Context<Self>) -> AnyElement {
        h_flex()
            .gap_1()
            .child(
                Button::new("project-view", "Project")
                    .toggle_state(!self.android_tree.enabled)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.android_tree.enabled = false;
                        this.save_android_tree(cx);
                        window.focus(&this.focus_handle, cx);
                        cx.notify();
                    })),
            )
            .when(
                self.android_available(cx) || self.android_tree.enabled,
                |element| {
                    element.child(
                        Button::new("android-view", "Android")
                            .disabled(self.state.edit_state.is_some())
                            .toggle_state(self.android_tree.enabled)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.android_tree.enabled = true;
                                this.save_android_tree(cx);
                                this.update_android_tree(cx);
                                window.focus(&this.focus_handle, cx);
                                cx.notify();
                            })),
                    )
                },
            )
            .into_any_element()
    }

    pub(super) fn android_collapse_all(&mut self, cx: &mut Context<Self>) {
        self.android_tree.saved.collapsed.extend(
            self.android_tree
                .rows
                .iter()
                .filter(|row| row.branch)
                .map(|row| row.key.clone()),
        );
        self.android_changed(cx);
    }

    pub(super) fn android_expand_all(&mut self, cx: &mut Context<Self>) {
        self.android_tree.saved.collapsed.clear();
        self.android_changed(cx);
    }

    pub(super) fn android_selected_path(&self, cx: &App) -> Option<ProjectPath> {
        if !self.android_available(cx)
            || self.android_tree.loading
            || self.android_tree.token.as_ref()
                != Some(&self.project.read(cx).android_model().token())
        {
            return None;
        }
        let target = self.android_tree.selected_row()?.target?;
        let worktree = self
            .project
            .read(cx)
            .worktree_for_id(target.worktree_id, cx)?
            .read(cx);
        let entry = worktree.entry_for_id(target.entry_id)?;
        Some(ProjectPath {
            worktree_id: target.worktree_id,
            path: entry.path.clone(),
        })
    }

    fn android_changed(&mut self, cx: &mut Context<Self>) {
        self.android_tree.rebuild_visible();
        if let Some(index) = self.android_tree.visible.iter().position(|index| {
            Some(&self.android_tree.rows[*index].key) == self.android_tree.saved.selected.as_ref()
        }) {
            self.android_tree
                .scroll
                .scroll_to_item(index, ScrollStrategy::Nearest);
        }
        self.save_android_tree(cx);
        cx.notify();
    }

    fn android_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let tree = &mut self.android_tree;
        if tree.visible.is_empty() {
            return;
        }
        let current = tree
            .visible
            .iter()
            .position(|index| Some(&tree.rows[*index].key) == tree.saved.selected.as_ref());
        let next = current
            .map(|index| {
                index
                    .saturating_add_signed(delta)
                    .min(tree.visible.len() - 1)
            })
            .unwrap_or(if delta == isize::MAX {
                tree.visible.len() - 1
            } else {
                0
            });
        tree.saved.selected = Some(tree.rows[tree.visible[next]].key.clone());
        self.android_changed(cx);
    }

    fn android_expand(&mut self, expand: bool, cx: &mut Context<Self>) {
        let Some(row) = self.android_tree.selected_row().cloned() else {
            self.android_move(0, cx);
            return;
        };
        if expand {
            if !row.branch {
                return;
            }
            if !self.android_tree.saved.collapsed.remove(&row.key) {
                let has_children = self
                    .android_tree
                    .rows
                    .iter()
                    .any(|child| child.parent.as_ref() == Some(&row.key));
                if has_children {
                    self.android_move(1, cx);
                }
                return;
            }
        } else if row.branch && !self.android_tree.saved.collapsed.contains(&row.key) {
            self.android_tree.saved.collapsed.insert(row.key);
        } else {
            self.android_tree.saved.selected = row.parent;
        }
        self.android_changed(cx);
    }

    fn android_open(
        &mut self,
        preview: bool,
        focus: bool,
        split: Option<SplitDirection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.android_available(cx)
            || self.android_tree.loading
            || self.android_tree.token.as_ref()
                != Some(&self.project.read(cx).android_model().token())
        {
            return;
        }
        let Some(row) = self.android_tree.selected_row().cloned() else {
            return;
        };
        if let Some(target) = row.target {
            if self
                .project
                .read(cx)
                .worktree_for_entry(target.entry_id, cx)
                .is_none()
            {
                self.update_android_tree(cx);
                return;
            }
            let project = self.project.read(cx);
            let Some(worktree) = project.worktree_for_entry(target.entry_id, cx) else {
                return;
            };
            let Some(entry) = worktree.read(cx).entry_for_id(target.entry_id) else {
                return;
            };
            let path = ProjectPath {
                worktree_id: target.worktree_id,
                path: entry.path.clone(),
            };
            self.workspace
                .update(cx, |workspace, cx| {
                    if split.is_some() {
                        workspace
                            .split_path_preview(path, preview, split, window, cx)
                            .detach_and_notify_err(workspace.weak_handle(), window, cx);
                    } else {
                        workspace
                            .open_path_preview(path, None, focus, preview, true, window, cx)
                            .detach_and_notify_err(workspace.weak_handle(), window, cx);
                    }
                })
                .log_err();
        } else {
            if !self.android_tree.saved.collapsed.remove(&row.key) {
                self.android_tree.saved.collapsed.insert(row.key);
            }
            self.android_changed(cx);
        }
    }

    pub(super) fn reveal_android_entry(
        &mut self,
        entry: ProjectEntryId,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.android_tree.loading || self.project.read(cx).android_model().model.is_none() {
            self.android_tree.pending_reveal = Some(entry);
            return false;
        }
        let Some(row) = self
            .android_tree
            .rows
            .iter()
            .find(|row| row.target.is_some_and(|target| target.entry_id == entry))
            .cloned()
        else {
            return false;
        };
        self.android_tree.saved.selected = Some(row.key.clone());
        let mut parent = row.parent;
        while let Some(key) = parent {
            self.android_tree.saved.collapsed.remove(&key);
            parent = self
                .android_tree
                .rows
                .iter()
                .find(|row| row.key == key)
                .and_then(|row| row.parent.clone());
        }
        self.android_changed(cx);
        true
    }

    pub(super) fn render_android_tree(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let available = self.android_available(cx);
        v_flex().id("android-project-panel").size_full().track_focus(&self.focus_handle)
            .key_context(self.dispatch_context(window, cx))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.android_move(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.android_move(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.android_move(isize::MIN, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| this.android_move(isize::MAX, cx)))
            .on_action(cx.listener(|this, _: &ExpandSelectedEntry, _, cx| this.android_expand(true, cx)))
            .on_action(cx.listener(|this, _: &CollapseSelectedEntry, _, cx| this.android_expand(false, cx)))
            .on_action(cx.listener(|this, _: &Open, window, cx| {
                let preview = PreviewTabsSettings::get_global(cx).enable_preview_from_project_panel;
                this.android_open(preview, !preview, None, window, cx);
            }))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.android_open(false, true, None, window, cx)))
            .on_action(cx.listener(|this, _: &OpenPermanent, window, cx| this.android_open(false, true, None, window, cx)))
            .on_action(cx.listener(|this, _: &OpenSplitVertical, window, cx| this.android_open(false, true, Some(SplitDirection::vertical(cx)), window, cx)))
            .on_action(cx.listener(|this, _: &OpenSplitHorizontal, window, cx| this.android_open(false, true, Some(SplitDirection::horizontal(cx)), window, cx)))
            .on_action(cx.listener(|this, _: &CollapseAllEntries, _, cx| {
                this.android_collapse_all(cx);
            }))
            .on_action(cx.listener(|this, _: &ExpandAllEntries, _, cx| { this.android_expand_all(cx); }))
            .child(h_flex().h_8().flex_none().px_2().justify_between().child(self.render_project_view_selector(cx)).child(IconButton::new("hide-android-tree", IconName::Close).tooltip(Tooltip::text("Hide Project")).on_click(cx.listener(|_, _, _, cx| cx.emit(PanelEvent::Close)))))
            .when(!available, |element| element.child(Label::new("Sync a trusted Android project to load its tree.").color(Color::Muted)))
            .when(available && self.android_tree.loading, |element| element.child(Label::new("Updating Android tree…").color(Color::Muted)))
            .when(available, |element| element.child(uniform_list("android-tree-rows", self.android_tree.visible.len(), cx.processor(|this, range: Range<usize>, _, cx| {
                range.filter_map(|index| {
                    let row = this.android_tree.rows.get(*this.android_tree.visible.get(index)?)?.clone();
                    let key = row.key.clone();
                    let token = this.android_tree.token.clone();
                    let revision = this.android_tree.published_revision;
                    let selected = this.android_tree.saved.selected.as_ref() == Some(&row.key);
                    let icon = if row.branch { if this.android_tree.saved.collapsed.contains(&row.key) { IconName::ChevronRight } else { IconName::ChevronDown } } else { IconName::File };
                    Some(ListItem::new(SharedString::from(key.clone())).toggle_state(selected).inset(true)
                        .child(h_flex().min_w_0().pl(px(row.depth as f32 * 12.)).gap_1().child(Icon::new(icon).size(IconSize::Small)).child(Label::new(row.label).truncate()))
                        .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                            // Resolve captured keys again: a sync may have replaced the displayed range.
                            if this.android_tree.token != token || this.android_tree.revision != revision || this.android_tree.loading { return; }
                            if !this.android_tree.visible.iter().any(|index| this.android_tree.rows[*index].key == key) { return; }
                            this.android_tree.saved.selected = Some(key.clone());
                            window.focus(&this.focus_handle, cx);
                            let preview = event.click_count() < 2 && PreviewTabsSettings::get_global(cx).enable_preview_from_project_panel;
                            this.android_open(preview, event.click_count() >= 2, None, window, cx);
                            this.android_changed(cx);
                        })).into_any_element())
                }).collect::<Vec<_>>()
            })).track_scroll(&self.android_tree.scroll).w_full().flex_1().min_h_0()))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use android_tools::project_model::{
        Component, Module, ModuleKind, SourceRoot, SourceScope, Variant, VariantId,
    };
    use gpui::{TestAppContext, VisualTestContext};
    use project::FakeFs;
    use serde_json::json;
    use workspace::MultiWorkspace;

    fn model() -> ProjectModel {
        ProjectModel {
            version: 1,
            root: "/app".into(),
            diagnostics: Vec::new(),
            modules: vec![Module {
                path: ":mobile".into(),
                directory: "/app/mobile".into(),
                namespace: None,
                kind: ModuleKind::Application,
                default_variant: Some("debug".into()),
                variants: vec![Variant {
                    name: "debug".into(),
                    output_listing: None,
                    components: vec![Component {
                        name: "debug".into(),
                        namespace: None,
                        scope: SourceScope::Main,
                        dependencies: Vec::new(),
                        sources: vec![
                            SourceRoot {
                                path: "/app/mobile/src/main/java".into(),
                                kind: SourceKind::Java,
                                generated: false,
                            },
                            SourceRoot {
                                path: "/app/mobile/src/main/java".into(),
                                kind: SourceKind::Kotlin,
                                generated: false,
                            },
                            SourceRoot {
                                path: "/app/mobile/src/main/res".into(),
                                kind: SourceKind::Resources,
                                generated: false,
                            },
                            SourceRoot {
                                path: "/app/mobile/build/generated/java".into(),
                                kind: SourceKind::Java,
                                generated: true,
                            },
                            SourceRoot {
                                path: "/app/mobile/build/generated/res".into(),
                                kind: SourceKind::Resources,
                                generated: true,
                            },
                            SourceRoot {
                                path: "/app/mobile/build/generated/assets".into(),
                                kind: SourceKind::Assets,
                                generated: true,
                            },
                        ],
                    }],
                }],
            }],
        }
    }

    async fn fixture(
        cx: &mut TestAppContext,
    ) -> (
        Entity<ProjectPanel>,
        Entity<Project>,
        Arc<FakeFs>,
        VisualTestContext,
    ) {
        crate::project_panel_tests::init_test_with_editor(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/app", json!({".gitignore": "mobile/build/", "mobile": {
            "build.gradle.kts": "", "google-services.json": "", "resources.properties": "",
            "src": {"main": {
                "java": {"dev": {"example": {"Main.java": "class Main {}"}}},
                "res": {"resources.properties": "", "layout": {"main.xml": "<View/>"}, "layout-land": {"main.xml": "<View/>"}}
            }},
            "build": {"generated": {"java": {"dev": {"example": {"BuildConfig.java": "class BuildConfig {}"}}}, "res": {"values": {"generated.xml": "<resources/>"}}, "assets": {"nested": {"data.txt": "payload"}}}}
        }})).await;
        let project = Project::test(fs.clone(), [Path::new("/app")], cx).await;
        let window =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = window
            .read_with(cx, |multi, _| multi.workspace().clone())
            .unwrap();
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        let panel = workspace.update_in(&mut visual, |workspace, window, cx| {
            workspace.set_random_database_id();
            ProjectPanel::new(workspace, window, cx)
        });
        project.update(&mut visual, |project, cx| {
            let token = project.invalidate_android_model(Some("/app".into()), cx);
            project.publish_android_model(&token, model(), cx).unwrap();
            project
                .select_android_variant(
                    Some(VariantId {
                        module: ":mobile".into(),
                        variant: "debug".into(),
                    }),
                    cx,
                )
                .unwrap();
        });
        visual.run_until_parked();
        (panel, project, fs, visual)
    }

    #[gpui::test]
    async fn logical_rows_keep_qualified_resources_and_generated_assets(cx: &mut TestAppContext) {
        let (panel, _, _, mut cx) = fixture(cx).await;
        panel.read_with(&cx, |panel, _| {
            let rows = &panel.android_tree.rows;
            assert!(rows.iter().any(|row| row.label == "dev.example"));
            assert_eq!(
                rows.iter().filter(|row| row.label == "Main.java").count(),
                1
            );
            assert!(
                rows.iter()
                    .any(|row| row.label.starts_with("java/kotlin ·"))
            );
            assert!(
                rows.iter()
                    .any(|row| row.label == "BuildConfig.java" && row.target.is_some())
            );
            assert!(
                rows.iter()
                    .any(|row| row.label == "generated.xml" && row.target.is_some())
            );
            assert!(
                rows.iter()
                    .any(|row| row.label.starts_with("assets (generated)"))
            );
            assert!(
                rows.iter()
                    .any(|row| row.label == "data.txt" && row.target.is_some())
            );
            assert!(rows.iter().any(|row| row.label == "google-services.json"));
            assert_eq!(
                rows.iter()
                    .filter(|row| row.label == "resources.properties")
                    .count(),
                2
            );
            let resources = rows
                .iter()
                .filter(|row| row.label == "main.xml")
                .collect::<Vec<_>>();
            assert_eq!(resources.len(), 2);
            assert_ne!(resources[0].key, resources[1].key);
            assert_ne!(resources[0].target, resources[1].target);
            assert_eq!(
                rows.len(),
                rows.iter()
                    .map(|row| &row.key)
                    .collect::<BTreeSet<_>>()
                    .len()
            );
        });
        panel.update(&mut cx, |panel, cx| {
            let package = panel
                .android_tree
                .rows
                .iter()
                .find(|row| row.label == "dev.example")
                .unwrap()
                .clone();
            let leaf = panel
                .android_tree
                .rows
                .iter()
                .find(|row| row.label == "Main.java")
                .unwrap()
                .clone();
            panel.android_tree.saved.selected = Some(leaf.key.clone());
            for _ in 0..3 {
                panel
                    .android_tree
                    .saved
                    .collapsed
                    .insert(package.key.clone());
                panel.android_changed(cx);
                assert_eq!(
                    panel.android_tree.saved.selected.as_ref(),
                    Some(&package.key)
                );
                assert!(
                    !panel
                        .android_tree
                        .visible
                        .iter()
                        .any(|index| panel.android_tree.rows[*index].key == leaf.key)
                );
                assert!(panel.reveal_android_entry(leaf.target.unwrap().entry_id, cx));
                assert_eq!(panel.android_tree.saved.selected.as_ref(), Some(&leaf.key));
            }
            let saved = serde_json::to_string(&panel.android_tree.saved).unwrap();
            let restored: SavedTree = serde_json::from_str(&saved).unwrap();
            assert_eq!(restored.selected, panel.android_tree.saved.selected);
            assert_eq!(restored.collapsed, panel.android_tree.saved.collapsed);
        });
    }

    #[gpui::test]
    async fn logical_tree_survives_resync_and_discards_removed_files(cx: &mut TestAppContext) {
        let (panel, project, fs, mut cx) = fixture(cx).await;
        let selection = panel.update(&mut cx, |panel, _| {
            let key = panel
                .android_tree
                .rows
                .iter()
                .find(|row| row.label == "Main.java")
                .unwrap()
                .key
                .clone();
            panel.android_tree.saved.selected = Some(key.clone());
            panel.android_tree.enabled = true;
            key
        });
        project.update(&mut cx, |project, cx| {
            let stale = project.invalidate_android_model(Some("/app".into()), cx);
            let current = project.invalidate_android_model(Some("/app".into()), cx);
            assert!(project.publish_android_model(&stale, model(), cx).is_err());
            project
                .publish_android_model(&current, model(), cx)
                .unwrap();
        });
        cx.run_until_parked();
        panel.read_with(&cx, |panel, _| {
            assert!(panel.android_tree.enabled);
            assert_eq!(panel.android_tree.saved.selected.as_ref(), Some(&selection));
            assert!(!panel.android_tree.loading);
        });
        fs.remove_file(
            Path::new("/app/mobile/src/main/java/dev/example/Main.java"),
            Default::default(),
        )
        .await
        .unwrap();
        cx.run_until_parked();
        panel.read_with(&cx, |panel, _| {
            assert!(
                !panel
                    .android_tree
                    .rows
                    .iter()
                    .any(|row| row.label == "Main.java")
            );
            assert_ne!(panel.android_tree.saved.selected.as_ref(), Some(&selection));
        });
        project.update(&mut cx, |project, cx| {
            project.invalidate_android_model(Some("/other".into()), cx);
        });
        cx.run_until_parked();
        panel.read_with(&cx, |panel, _| {
            assert!(panel.android_tree.rows.is_empty());
            assert!(panel.android_tree.saved.selected.is_none());
        });
    }

    #[gpui::test]
    async fn logical_tree_persists_across_roots_and_panel_release(cx: &mut TestAppContext) {
        let (panel, project, _, mut cx) = fixture(cx).await;
        let workspace = panel.read_with(&cx, |panel, _| panel.workspace.upgrade().unwrap());
        let selected = panel.update(&mut cx, |panel, cx| {
            panel.android_tree.enabled = true;
            let selected = panel
                .android_tree
                .rows
                .iter()
                .find(|row| row.label == "Main.java")
                .unwrap()
                .key
                .clone();
            panel.android_tree.saved.selected = Some(selected);
            panel.android_collapse_all(cx);
            let selected = panel.android_tree.saved.selected.clone().unwrap();
            panel.save_android_tree(cx);
            selected
        });
        // Switch roots before the queued SQLite write has an opportunity to finish.
        project.update(&mut cx, |project, cx| {
            project.invalidate_android_model(Some("/other".into()), cx);
        });
        cx.run_until_parked();
        project.update(&mut cx, |project, cx| {
            let token = project.invalidate_android_model(Some("/app".into()), cx);
            project.publish_android_model(&token, model(), cx).unwrap();
        });
        cx.run_until_parked();
        panel.read_with(&cx, |panel, _| {
            assert!(panel.android_tree.enabled);
            assert_eq!(panel.android_tree.saved.selected.as_ref(), Some(&selected));
            assert!(!panel.android_tree.saved.collapsed.is_empty());
        });
        let (storage_key, db) = panel.update(&mut cx, |panel, cx| {
            panel.android_tree.enabled = false;
            panel.save_android_tree(cx);
            (
                panel.android_storage_key(cx).unwrap(),
                KeyValueStore::global(cx),
            )
        });
        drop(panel);
        cx.run_until_parked();
        let saved: SavedTree = serde_json::from_str(
            &db.scoped("AndroidProjectTree.v1")
                .read(&storage_key)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(!saved.enabled);
        assert_eq!(saved.selected.as_ref(), Some(&selected));
        let restored = workspace.update_in(&mut cx, ProjectPanel::new);
        cx.run_until_parked();
        restored.read_with(&cx, |panel, _| {
            assert!(!panel.android_tree.enabled);
            assert_eq!(panel.android_tree.saved.selected.as_ref(), Some(&selected));
            assert_eq!(panel.android_tree.saved.collapsed, saved.collapsed);
        });
    }

    #[gpui::test]
    async fn logical_navigation_opens_physical_file_and_preserves_project_selection(
        cx: &mut TestAppContext,
    ) {
        let (panel, _, _, mut cx) = fixture(cx).await;
        let workspace = panel.read_with(&cx, |panel, _| panel.workspace.upgrade().unwrap());
        crate::project_panel_tests::select_path(&panel, "app/mobile/build.gradle.kts", &mut cx);
        let physical_selection = panel.read_with(&cx, |panel, _| panel.selection);
        panel.update_in(&mut cx, |panel, window, cx| {
            panel.android_tree.enabled = true;
            panel.android_tree.saved.selected = None;
            panel.android_move(isize::MAX, cx);
            let last = panel.android_tree.rows[*panel.android_tree.visible.last().unwrap()]
                .key
                .clone();
            assert_eq!(panel.android_tree.saved.selected.as_ref(), Some(&last));
            let leaf = panel
                .android_tree
                .rows
                .iter()
                .find(|row| row.label == "Main.java")
                .unwrap()
                .key
                .clone();
            panel.android_tree.saved.selected = Some(leaf.clone());
            panel.android_expand(true, cx);
            assert_eq!(panel.android_tree.saved.selected.as_ref(), Some(&leaf));
            panel.android_open(true, false, None, window, cx);
        });
        cx.run_until_parked();
        workspace.read_with(&cx, |workspace, cx| {
            let path = workspace.active_item(cx).unwrap().project_path(cx).unwrap();
            assert_eq!(
                path.path.as_unix_str(),
                "mobile/src/main/java/dev/example/Main.java"
            );
        });
        panel.read_with(&cx, |panel, _| {
            assert_eq!(panel.selection, physical_selection)
        });
        panel.update_in(&mut cx, |panel, window, cx| {
            panel.android_open(false, true, None, window, cx)
        });
        cx.run_until_parked();
        workspace.read_with(&cx, |workspace, cx| {
            assert!(workspace.active_pane().read(cx).preview_item_id().is_none())
        });
        panel.update_in(&mut cx, |panel, window, cx| {
            panel.android_tree.saved.selected = Some(
                panel
                    .android_tree
                    .rows
                    .iter()
                    .find(|row| row.label == "main.xml")
                    .unwrap()
                    .key
                    .clone(),
            );
            panel.android_open(false, true, Some(SplitDirection::vertical(cx)), window, cx);
        });
        cx.run_until_parked();
        workspace.read_with(&cx, |workspace, cx| {
            assert_eq!(workspace.panes().len(), 2);
            assert!(workspace.panes().iter().any(|pane| {
                pane.read(cx)
                    .active_item()
                    .and_then(|item| item.project_path(cx))
                    .is_some_and(|path| path.path.as_unix_str().ends_with("main.xml"))
            }));
        });
        panel.read_with(&cx, |panel, _| {
            assert_eq!(panel.selection, physical_selection)
        });
    }

    #[gpui::test]
    async fn logical_tree_blocks_workspace_mutations_of_hidden_selection(cx: &mut TestAppContext) {
        let (panel, _, fs, mut cx) = fixture(cx).await;
        let workspace = panel.read_with(&cx, |panel, _| panel.workspace.upgrade().unwrap());
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.add_panel(panel.clone(), window, cx);
            workspace.open_panel::<ProjectPanel>(window, cx);
        });
        crate::project_panel_tests::select_path(&panel, "app/mobile/build.gradle.kts", &mut cx);
        panel.update_in(&mut cx, |panel, window, cx| {
            panel.android_tree.enabled = true;
            panel.android_tree.saved.selected = Some(panel.android_tree.rows[0].key.clone());
            window.focus(&panel.focus_handle, cx);
            cx.notify();
        });
        cx.run_until_parked();
        cx.dispatch_action(Rename);
        cx.dispatch_action(Duplicate);
        cx.dispatch_action(Delete { skip_prompt: true });
        cx.run_until_parked();
        panel.read_with(&cx, |panel, cx| {
            assert!(panel.state.edit_state.is_none());
            assert!(panel.selected_entry_project_path(cx).is_none());
        });
        assert_eq!(
            fs.load(Path::new("/app/mobile/build.gradle.kts"))
                .await
                .unwrap(),
            ""
        );
        panel.update_in(&mut cx, |panel, window, cx| {
            panel.android_tree.enabled = false;
            panel.rename(&Rename, window, cx);
        });
        panel.read_with(&cx, |panel, _| assert!(panel.state.edit_state.is_some()));
    }

    #[gpui::test(iterations = 5)]
    async fn logical_tree_refreshes_generated_files_from_delayed_event_batches(
        cx: &mut TestAppContext,
    ) {
        let (panel, _, fs, cx) = fixture(cx).await;
        fs.pause_events();
        for batch in 0..3 {
            let name = format!("Batch{batch}.java");
            fs.insert_tree(
                "/app/mobile/build/generated/java/dev/example",
                json!({(name): "class Generated {}"}),
            )
            .await;
            fs.flush_events(1);
            cx.run_until_parked();
        }
        fs.unpause_events_and_flush();
        cx.run_until_parked();
        panel.read_with(&cx, |panel, _| {
            assert!(!panel.android_tree.loading);
            for batch in 0..3 {
                assert!(
                    panel.android_tree.rows.iter().any(|row| row.label
                        == format!("Batch{batch}.java")
                        && row.target.is_some())
                );
            }
        });
    }
}
