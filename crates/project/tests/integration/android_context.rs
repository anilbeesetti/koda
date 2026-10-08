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
    let added = project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery.clone(), vec![logic.to_path_buf()], cx)).await?;
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
    assert!(project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery.clone(), vec![logic.to_path_buf()], cx)).await?);
    assert!(!project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery.clone(), vec![logic.to_path_buf()], cx)).await?);
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
    assert!(project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery.clone(), vec![root.to_path_buf()], cx)).await?);
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
    assert!(project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery.clone(), vec![logic.to_path_buf()], cx)).await?);
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
    assert!(!project.update(cx, |project, cx| project.observe_android_context_inputs(handle, discovery.clone(), vec![logic.to_path_buf()], cx)).await?);
    project.update(cx, |project, cx| project.publish_android_context(&active, &owner, &discovery, observed_fixture(root, logic)?, cx))?;
    let token = project.read_with(cx, |project, _| project.android_context().token(handle)).context("Republished root")?;
    filesystem.insert_file(git_directory.join("HEAD"), b"ref: refs/heads/unrelated-task\n".to_vec()).await;
    cx.executor().run_until_parked();
    assert!(!project.read_with(cx, |project, _| project.android_context().is_current(&token)));
    assert_eq!(project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()), 1);
    Ok(())
}
