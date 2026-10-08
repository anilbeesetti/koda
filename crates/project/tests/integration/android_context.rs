use android_tools::project_context::{ActiveContext, ContextSnapshot, PluginId, decode_context_record};
use anyhow::{Context as _, Result};
use fs::FakeFs;
use gpui::TestAppContext;
use project::Project;
use serde_json::json;
use std::path::Path;
use util::path;

fn observed_fixture(root: &Path, logic: &Path) -> Result<ContextSnapshot> {
    let raw = json!({"schema":1,"root":root,"gradleVersion":"9.4","phase":"complete",
        "modules":[{"path":":","directory":root,
            "plugins":PluginId::ALL.map(|plugin| json!({"plugin":plugin,"applied":plugin==PluginId::AndroidApplication})),
            "targets":{"status":"unavailable","value":{"detail":"Java-only fixture"}},
            "android":{"status":"available","value":{"pluginVersion":"9.2.0"}}}],
        "buildLogicDirectories":[logic],
        "buildLayouts":[{"directory":logic,"buildDirectory":logic.join("custom-output"),"sourceDirectories":[logic.join("src")]}]});
    decode_context_record(&serde_json::to_vec(&raw)?, root)
}

#[gpui::test]
async fn context_input_observers_preserve_outputs_and_invalidate_real_external_inputs(cx: &mut TestAppContext) -> Result<()> {
    let root = path!("/context-project");
    let logic = path!("/external-convention");
    let filesystem = FakeFs::new(cx.executor());
    filesystem.insert_tree(root, json!({"build.gradle":"", "main.py":"print(1)"})).await;
    filesystem.insert_tree(logic, json!({"src":{"Convention.kt":"original"}, "custom-output":{"plugin.properties":"original"}})).await;
    let project = Project::test(filesystem.clone(), [root.as_ref()], cx).await;
    let worktree = project.read_with(cx, |project, cx| project.visible_worktrees(cx).next().map(|worktree| worktree.read(cx).id())).context("Root worktree")?;
    let handle = project.update(cx, |project, cx| project.ensure_android_context(worktree, true, cx))?;
    let discovery = project.update(cx, |project, cx| project.begin_android_context_import(handle, cx))?;
    let mut active = ActiveContext::default();
    active.select(Some(handle), None)?;
    let owner = project.read_with(cx, |project, _| active.discovery_token(project.android_context())).context("Import owner")?;
    let snapshot = observed_fixture(root, logic)?;
    project.update(cx, |project, _| project.verify_android_context_inputs(&discovery, &snapshot))?;
    let added = project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery.clone(), vec![logic.to_path_buf()], None, cx)).await?;
    assert!(added);
    assert!(filesystem.watch_calls().contains(&logic.to_path_buf()));
    assert_eq!(project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()), 1);
    assert!(project.read_with(cx, |project, _| project.android_context().snapshot(handle).is_none()));
    filesystem.insert_file(logic.join("custom-output/plugin.properties"), b"generated".to_vec()).await;
    cx.executor().run_until_parked();
    project.update(cx, |project, cx| project.publish_android_context(&active, &owner, &discovery, snapshot, cx))?;
    let token = project.read_with(cx, |project, _| project.android_context().token(handle)).context("Published root")?;
    filesystem.insert_file(root.join("main.py"), b"print(2)".to_vec()).await;
    cx.executor().run_until_parked();
    assert!(project.read_with(cx, |project, _| project.android_context().is_current(&token)));
    filesystem.insert_file(logic.join("src/Convention.kt"), b"changed".to_vec()).await;
    cx.executor().run_until_parked();
    assert!(!project.read_with(cx, |project, _| project.android_context().is_current(&token)));
    assert!(project.read_with(cx, |project, _| project.android_context().snapshot(handle).is_none()));
    assert_eq!(project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()), 1);
    Ok(())
}

#[gpui::test]
async fn input_observers_are_reused_and_pending_source_edits_reject_publication(cx: &mut TestAppContext) -> Result<()> {
    let root = path!("/context-reimport");
    let logic = path!("/convention-reimport");
    let filesystem = FakeFs::new(cx.executor());
    filesystem.insert_tree(root, json!({"build.gradle":""})).await;
    filesystem.insert_tree(logic, json!({"src":{"Convention.kt":"original"}})).await;
    let project = Project::test(filesystem.clone(), [root.as_ref()], cx).await;
    let worktree = project.read_with(cx, |project, cx| project.visible_worktrees(cx).next().map(|worktree| worktree.read(cx).id())).context("Root worktree")?;
    let handle = project.update(cx, |project, cx| project.ensure_android_context(worktree, true, cx))?;
    let discovery = project.update(cx, |project, cx| project.begin_android_context_import(handle, cx))?;
    let mut active = ActiveContext::default();
    active.select(Some(handle), None)?;
    let owner = project.read_with(cx, |project, _| active.discovery_token(project.android_context())).context("Import owner")?;
    let snapshot = observed_fixture(root, logic)?;
    assert!(project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery.clone(), vec![logic.to_path_buf()], None, cx)).await?);
    assert!(!project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery.clone(), vec![logic.to_path_buf()], None, cx)).await?);
    filesystem.insert_file(logic.join("src/Convention.kt"), b"changed during import".to_vec()).await;
    cx.executor().run_until_parked();
    assert!(project.update(cx, |project, cx| project.publish_android_context(&active, &owner, &discovery, snapshot, cx)).is_err());
    assert!(!project.read_with(cx, |project, _| project.android_context().import_is_current(&discovery)));
    project.update(cx, |project, cx| project.ensure_android_context(worktree, false, cx))?;
    assert!(project.read_with(cx, |project, _| project.android_context().token(handle).is_none()));
    assert!(!project.read_with(cx, |project, _| project.android_context_observes(handle, logic)));
    assert_eq!(project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()), 1);
    Ok(())
}

#[gpui::test]
async fn selected_root_observer_detects_later_buildsrc_creation_without_new_worktrees(cx: &mut TestAppContext) -> Result<()> {
    let root = path!("/later-convention-project");
    let logic = root.join("buildSrc");
    let filesystem = FakeFs::new(cx.executor());
    filesystem.insert_tree(root, json!({"build.gradle":""})).await;
    let project = Project::test(filesystem.clone(), [root.as_ref()], cx).await;
    let worktree = project.read_with(cx, |project, cx| project.visible_worktrees(cx).next().map(|worktree| worktree.read(cx).id())).context("Root worktree")?;
    let handle = project.update(cx, |project, cx| project.ensure_android_context(worktree, true, cx))?;
    let discovery = project.update(cx, |project, cx| project.begin_android_context_import(handle, cx))?;
    let mut active = ActiveContext::default();
    active.select(Some(handle), None)?;
    let owner = project.read_with(cx, |project, _| active.discovery_token(project.android_context())).context("Import owner")?;
    assert!(project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery.clone(), vec![root.to_path_buf()], None, cx)).await?);
    assert!(project.read_with(cx, |project, _| project.android_context_observes(handle, &logic)));
    assert!(!project.read_with(cx, |project, _| project.android_context_observes(handle, path!("/different-project"))));
    project.update(cx, |project, cx| project.publish_android_context(&active, &owner, &discovery, observed_fixture(root, &logic)?, cx))?;
    let token = project.read_with(cx, |project, _| project.android_context().token(handle)).context("Published root")?;
    filesystem.insert_tree(&logic, json!({"src":{"Convention.kt":"new convention"}})).await;
    cx.executor().run_until_parked();
    assert!(!project.read_with(cx, |project, _| project.android_context().is_current(&token)));
    assert_eq!(project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()), 1);
    Ok(())
}

#[gpui::test]
async fn external_linked_git_observer_tracks_actual_head_without_cancelling_on_unrelated_refs(cx: &mut TestAppContext) -> Result<()> {
    let root = path!("/head-context-project");
    let logic = path!("/head-external-logic");
    let common = path!("/shared-context-git");
    let git_directory = common.join("worktrees/logic");
    let original = "1".repeat(40);
    let unrelated = "2".repeat(40);
    let next = "3".repeat(40);
    let filesystem = FakeFs::new(cx.executor());
    filesystem.insert_tree(root, json!({"build.gradle":""})).await;
    filesystem.insert_tree(logic, json!({".git":"gitdir: ../shared-context-git/worktrees/logic\n", "src":{"Convention.kt":"original"}})).await;
    filesystem.insert_tree(common, json!({
        "worktrees":{"logic":{"HEAD":"ref: refs/heads/external\n", "commondir":"../..\n"}},
        "refs":{"heads":{}}, "packed-refs":format!("# pack-refs with: peeled\n{original} refs/heads/external\n")
    })).await;
    let project = Project::test(filesystem.clone(), [root.as_ref()], cx).await;
    let worktree = project.read_with(cx, |project, cx| project.visible_worktrees(cx).next().map(|worktree| worktree.read(cx).id())).context("Root worktree")?;
    let handle = project.update(cx, |project, cx| project.ensure_android_context(worktree, true, cx))?;
    let discovery = project.update(cx, |project, cx| project.begin_android_context_import(handle, cx))?;
    let mut active = ActiveContext::default();
    active.select(Some(handle), None)?;
    let owner = project.read_with(cx, |project, _| active.discovery_token(project.android_context())).context("Import owner")?;
    assert!(project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery.clone(), vec![logic.to_path_buf()], None, cx)).await?);
    assert!(filesystem.watch_calls().contains(&git_directory));
    assert!(filesystem.watch_calls().contains(&common.to_path_buf()));
    project.update(cx, |project, cx| project.publish_android_context(&active, &owner, &discovery, observed_fixture(root, logic)?, cx))?;
    let token = project.read_with(cx, |project, _| project.android_context().token(handle)).context("Published root")?;
    filesystem.insert_file(common.join("refs/heads/unrelated-task"), format!("{unrelated}\n").into_bytes()).await;
    filesystem.insert_file(common.join("packed-refs"), format!("{original} refs/heads/external\n{unrelated} refs/heads/another-task\n").into_bytes()).await;
    cx.executor().run_until_parked();
    assert!(project.read_with(cx, |project, _| project.android_context().is_current(&token)));
    filesystem.insert_file(common.join("refs/heads/external"), format!("{next}\n").into_bytes()).await;
    cx.executor().run_until_parked();
    assert!(!project.read_with(cx, |project, _| project.android_context().is_current(&token)));
    assert!(project.read_with(cx, |project, _| project.android_context().snapshot(handle).is_none()));
    let discovery = project.update(cx, |project, cx| project.begin_android_context_import(handle, cx))?;
    let owner = project.read_with(cx, |project, _| active.discovery_token(project.android_context())).context("Reimport owner")?;
    assert!(!project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery.clone(), vec![logic.to_path_buf()], None, cx)).await?);
    project.update(cx, |project, cx| project.publish_android_context(&active, &owner, &discovery, observed_fixture(root, logic)?, cx))?;
    let token = project.read_with(cx, |project, _| project.android_context().token(handle)).context("Republished root")?;
    filesystem.insert_file(git_directory.join("HEAD"), b"ref: refs/heads/unrelated-task\n".to_vec()).await;
    cx.executor().run_until_parked();
    assert!(!project.read_with(cx, |project, _| project.android_context().is_current(&token)));
    assert_eq!(project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()), 1);
    Ok(())
}

#[gpui::test]
async fn recursive_inputs_track_nested_and_enclosing_git_owners_without_extra_worktrees(cx: &mut TestAppContext) -> Result<()> {
    for nested in [true, false] {
        let root = if nested { path!("/nested-head-project") } else { path!("/enclosing-head-project") };
        let owner = if nested { root.join("buildSrc") } else { path!("/enclosing-logic-repository").to_path_buf() };
        let logic = if nested { owner.clone() } else { owner.join("build-logic") };
        let filesystem = FakeFs::new(cx.executor());
        filesystem.insert_tree(root, json!({"build.gradle":""})).await;
        filesystem.insert_tree(&logic, json!({"src":{"deep":{"Convention.kt":"original"}}})).await;
        filesystem.insert_tree(owner.join(".git"), json!({"HEAD":"ref: refs/heads/current\n", "refs":{"heads":{"current":format!("{}\n", "1".repeat(40))}}})).await;
        let project = Project::test(filesystem.clone(), [root.as_ref()], cx).await;
        let worktree = project.read_with(cx, |project, cx| project.visible_worktrees(cx).next().map(|worktree| worktree.read(cx).id())).context("Root worktree")?;
        let handle = project.update(cx, |project, cx| project.ensure_android_context(worktree, true, cx))?;
        let discovery = project.update(cx, |project, cx| project.begin_android_context_import(handle, cx))?;
        let mut active = ActiveContext::default();
        active.select(Some(handle), None)?;
        let active_owner = project.read_with(cx, |project, _| active.discovery_token(project.android_context())).context("Import owner")?;
        assert!(project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery.clone(), vec![root.to_path_buf()], None, cx)).await?);
        let added = project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery.clone(), vec![logic.clone()], None, cx)).await?;
        assert_eq!(added, !nested);
        assert!(filesystem.watch_calls().contains(&owner.join(".git")));
        project.update(cx, |project, cx| project.publish_android_context(&active, &active_owner, &discovery, observed_fixture(root, &logic)?, cx))?;
        let token = project.read_with(cx, |project, _| project.android_context().token(handle)).context("Published root")?;
        filesystem.insert_file(owner.join(".git/refs/heads/unrelated-task"), format!("{}\n", "2".repeat(40)).into_bytes()).await;
        cx.executor().run_until_parked();
        assert!(project.read_with(cx, |project, _| project.android_context().is_current(&token)));
        filesystem.insert_file(owner.join(".git/HEAD"), format!("{}\n", "3".repeat(40)).into_bytes()).await;
        cx.executor().run_until_parked();
        assert!(!project.read_with(cx, |project, _| project.android_context().is_current(&token)));
        assert_eq!(project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()), 1);
    }
    Ok(())
}

#[gpui::test]
async fn root_removal_releases_only_owned_inputs_and_keeps_other_worktrees(cx: &mut TestAppContext) -> Result<()> {
    let root_a = path!("/owned-context-a");
    let root_b = path!("/owned-context-b");
    let logic_a = path!("/owned-logic-a");
    let logic_b = path!("/owned-logic-b");
    let filesystem = FakeFs::new(cx.executor());
    for root in [root_a, root_b] { filesystem.insert_tree(root, json!({"build.gradle":"", "main.py":"print(1)"})).await; }
    for logic in [logic_a, logic_b] { filesystem.insert_tree(logic, json!({"src":{"Convention.kt":"original"}})).await; }
    let project = Project::test(filesystem.clone(), [root_a.as_ref(), root_b.as_ref()], cx).await;
    let worktree_a = project.read_with(cx, |project, cx| project.find_worktree(root_a, cx).map(|(worktree, _)| worktree.read(cx).id())).context("First worktree")?;
    let worktree_b = project.read_with(cx, |project, cx| project.find_worktree(root_b, cx).map(|(worktree, _)| worktree.read(cx).id())).context("Second worktree")?;
    let handle_a = project.update(cx, |project, cx| project.ensure_android_context(worktree_a, true, cx))?;
    let handle_b = project.update(cx, |project, cx| project.ensure_android_context(worktree_b, true, cx))?;
    assert!(!project.read_with(cx, |project, _| project.android_context_observes(handle_a, logic_a)));
    assert!(!project.read_with(cx, |project, _| project.android_context_observes(handle_b, logic_b)));
    for (handle, logic) in [(handle_a, logic_a), (handle_b, logic_b)] {
        let discovery = project.update(cx, |project, cx| project.begin_android_context_import(handle, cx))?;
        assert!(project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery, vec![logic.to_path_buf()], None, cx)).await?);
    }
    assert!(filesystem.watched_paths().contains(&logic_a.to_path_buf()));
    assert!(filesystem.watched_paths().contains(&logic_b.to_path_buf()));
    project.update(cx, |project, cx| project.remove_worktree(worktree_a, cx));
    cx.executor().run_until_parked();
    assert!(project.read_with(cx, |project, _| project.android_context().handle(worktree_a.to_proto()).is_none()));
    assert!(!filesystem.watched_paths().contains(&logic_a.to_path_buf()));
    assert!(filesystem.watched_paths().contains(&logic_b.to_path_buf()));
    assert!(project.read_with(cx, |project, _| project.android_context_observes(handle_b, logic_b)));
    assert_eq!(project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()), 1);
    assert!(project.read_with(cx, |project, cx| project.worktree_for_id(worktree_b, cx).is_some()));
    Ok(())
}

#[cfg(target_os = "linux")]
#[gpui::test]
async fn real_linux_observer_registers_deep_inputs_and_later_directories(cx: &mut TestAppContext) -> Result<()> {
    use fs::{Fs as _, RealFs, fs_watcher::OsWatcherKind};
    cx.executor().allow_parking();
    let fixture = tempfile::TempDir::new()?;
    let root = fixture.path().join("project");
    let logic = fixture.path().join("external-logic");
    let source = logic.join("src/deep/Convention.kt");
    let output = logic.join("custom-output/classes");
    let common_git = fixture.path().join("shared-git");
    let git_directory = common_git.join("worktrees/logic");
    let current_ref = common_git.join("refs/heads/android/current");
    std::fs::create_dir_all(&root)?;
    std::fs::create_dir_all(source.parent().context("Source parent")?)?;
    std::fs::create_dir_all(&output)?;
    std::fs::create_dir_all(&git_directory)?;
    std::fs::create_dir_all(current_ref.parent().context("Current Git reference parent")?)?;
    std::fs::write(root.join("build.gradle"), "")?;
    std::fs::write(&source, "original")?;
    std::fs::write(logic.join(".git"), "gitdir: ../shared-git/worktrees/logic\n")?;
    std::fs::write(git_directory.join("HEAD"), "ref: refs/heads/android/current\n")?;
    std::fs::write(git_directory.join("commondir"), "../..\n")?;
    std::fs::write(&current_ref, format!("{}\n", "1".repeat(40)))?;
    let filesystem = RealFs::new(None, cx.executor());
    let recording = filesystem.record_watcher_diagnostics().context("Native watcher diagnostics")?;
    let project = Project::test(filesystem, [root.as_path()], cx).await;
    let worktree = project.read_with(cx, |project, cx| project.visible_worktrees(cx).next().map(|worktree| worktree.read(cx).id())).context("Root worktree")?;
    cx.executor().run_until_parked();
    let baseline = recording.snapshot();
    assert!(!baseline.watchers.iter().flat_map(|watcher| &watcher.roots).any(|registered| Path::new(&registered.path).starts_with(&logic)
        || Path::new(&registered.path).starts_with(&common_git)));
    let handle = project.update(cx, |project, cx| project.ensure_android_context(worktree, true, cx))?;
    let discovery = project.update(cx, |project, cx| project.begin_android_context_import(handle, cx))?;
    let mut active = ActiveContext::default();
    active.select(Some(handle), None)?;
    let owner = project.read_with(cx, |project, _| active.discovery_token(project.android_context())).context("Import owner")?;
    let snapshot = observed_fixture(&root, &logic)?;
    assert!(project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery.clone(), vec![logic.clone()], Some(snapshot.clone()), cx)).await?);
    let native = recording.snapshot().watchers.into_iter().filter(|watcher| watcher.backend == OsWatcherKind::Native).collect::<Vec<_>>();
    let source_directory = source.parent().context("Source parent")?;
    assert!(native.iter().all(|watcher| !watcher.recursive));
    assert!(native.iter().flat_map(|watcher| &watcher.roots).any(|registered| registered.path == source_directory.to_string_lossy()));
    assert!(!native.iter().flat_map(|watcher| &watcher.roots).any(|registered| Path::new(&registered.path).starts_with(&output)));
    assert_eq!(native.iter().flat_map(|watcher| &watcher.roots).filter(|registered| Path::new(&registered.path).starts_with(&logic)).count(), 3);
    let current_ref_directory = current_ref.parent().context("Current Git reference parent")?;
    assert!(native.iter().flat_map(|watcher| &watcher.roots).any(|registered| registered.path == current_ref_directory.to_string_lossy()));
    project.update(cx, |project, cx| project.publish_android_context(&active, &owner, &discovery, snapshot, cx))?;
    let token = project.read_with(cx, |project, _| project.android_context().token(handle)).context("Published root")?;
    std::fs::write(&source, "changed input")?;
    cx.condition(&project, |project, _| !project.android_context().is_current(&token)).await;
    let discovery = project.update(cx, |project, cx| project.begin_android_context_import(handle, cx))?;
    let owner = project.read_with(cx, |project, _| active.discovery_token(project.android_context())).context("Reimport owner")?;
    let reimport_snapshot = observed_fixture(&root, &logic)?;
    assert!(!project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery.clone(), vec![logic.clone()], Some(reimport_snapshot), cx)).await?);
    project.update(cx, |project, cx| project.publish_android_context(&active, &owner, &discovery, observed_fixture(&root, &logic)?, cx))?;
    let token = project.read_with(cx, |project, _| project.android_context().token(handle)).context("Git reference root")?;
    std::fs::write(&current_ref, format!("{}\n", "2".repeat(40)))?;
    cx.condition(&project, |project, _| !project.android_context().is_current(&token)).await;
    let discovery = project.update(cx, |project, cx| project.begin_android_context_import(handle, cx))?;
    let owner = project.read_with(cx, |project, _| active.discovery_token(project.android_context())).context("New directory owner")?;
    project.update(cx, |project, cx| project.publish_android_context(&active, &owner, &discovery, observed_fixture(&root, &logic)?, cx))?;
    let token = project.read_with(cx, |project, _| project.android_context().token(handle)).context("Republished root")?;
    let new_source = logic.join("src/later/nested/Convention.kt");
    std::fs::create_dir_all(new_source.parent().context("Later source parent")?)?;
    std::fs::write(&new_source, "new input")?;
    cx.condition(&project, |project, _| !project.android_context().is_current(&token)).await;
    let later_directory = new_source.parent().context("Later source parent")?;
    assert!(recording.snapshot().watchers.iter().flat_map(|watcher| &watcher.roots).any(|registered| registered.path == later_directory.to_string_lossy()));
    assert_eq!(project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()), 1);
    project.update(cx, |project, cx| project.ensure_android_context(worktree, false, cx))?;
    cx.executor().run_until_parked();
    assert!(!project.read_with(cx, |project, _| project.android_context_observes(handle, &logic)));
    let remaining = recording.snapshot();
    assert!(!remaining.watchers.iter().flat_map(|watcher| &watcher.roots).any(|registered| Path::new(&registered.path).starts_with(&logic)
        || Path::new(&registered.path).starts_with(&common_git)));
    for original in baseline.watchers.iter().flat_map(|watcher| &watcher.roots) {
        assert!(remaining.watchers.iter().flat_map(|watcher| &watcher.roots).any(|registered| registered.path == original.path));
    }
    Ok(())
}
