use crate::init_test;
use android_tools::project_context::{
    ActiveContext, ContextSnapshot, PluginId, decode_context_record,
};
use anyhow::{Context as _, Result};
use fs::FakeFs;
use gpui::TestAppContext;
use project::Project;
use serde_json::json;
use std::path::Path;
use util::path;

fn observed_fixture_record(root: &Path, logic: &Path) -> serde_json::Value {
    json!({"schema":1,"root":root,"gradleVersion":"9.4","phase":"complete",
        "modules":[{"path":":","directory":root,
            "plugins":PluginId::ALL.map(|plugin| json!({"plugin":plugin,"applied":plugin==PluginId::AndroidApplication})),
            "targets":{"status":"unavailable","value":{"detail":"Java-only fixture"}},
            "android":{"status":"available","value":{"pluginVersion":"9.2.0"}}}],
        "buildLogicDirectories":[logic],
        "buildLayouts":[{"directory":logic,"buildDirectory":logic.join("custom-output"),"sourceDirectories":[logic.join("src")]}]})
}

fn observed_fixture(root: &Path, logic: &Path) -> Result<ContextSnapshot> {
    let raw = observed_fixture_record(root, logic);
    decode_context_record(&serde_json::to_vec(&raw)?, root)
}

#[gpui::test]
async fn context_input_observers_preserve_outputs_and_invalidate_real_external_inputs(
    cx: &mut TestAppContext,
) {
    context_input_observers_preserve_outputs_and_invalidate_real_external_inputs_case(cx)
        .await
        .expect("Android project-context fixture must complete successfully");
}

async fn context_input_observers_preserve_outputs_and_invalidate_real_external_inputs_case(
    cx: &mut TestAppContext,
) -> Result<()> {
    init_test(cx);
    let root = Path::new(path!("/context-project"));
    let logic = Path::new(path!("/external-convention"));
    let filesystem = FakeFs::new(cx.executor());
    filesystem
        .insert_tree(root, json!({"build.gradle":"", "main.py":"print(1)"}))
        .await;
    filesystem.insert_tree(logic, json!({"src":{"Convention.kt":"original"}, "custom-output":{"plugin.properties":"original"}})).await;
    let project = Project::test(filesystem.clone(), [root.as_ref()], cx).await;
    let worktree = project
        .read_with(cx, |project, cx| {
            project
                .visible_worktrees(cx)
                .next()
                .map(|worktree| worktree.read(cx).id())
        })
        .context("Root worktree")?;
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
        .context("Import owner")?;
    let snapshot = observed_fixture(root, logic)?;
    project.update(cx, |project, _| {
        project.verify_android_context_inputs(&discovery, &snapshot)
    })?;
    let added = project
        .update(cx, |project, cx| {
            project.observe_android_context_inputs(
                handle,
                discovery.clone(),
                vec![logic.to_path_buf()],
                None,
                cx,
            )
        })
        .await?;
    assert!(added);
    assert!(filesystem.watch_calls().contains(&logic.to_path_buf()));
    assert_eq!(
        project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()),
        1
    );
    assert!(project.read_with(cx, |project, _| {
        project.android_context().snapshot(handle).is_none()
    }));
    filesystem
        .insert_file(
            logic.join("custom-output/plugin.properties"),
            b"generated".to_vec(),
        )
        .await;
    cx.executor().run_until_parked();
    project.update(cx, |project, cx| {
        project.publish_android_context(&active, &owner, &discovery, snapshot, cx)
    })?;
    let token = project
        .read_with(cx, |project, _| project.android_context().token(handle))
        .context("Published root")?;
    filesystem
        .insert_file(root.join("main.py"), b"print(2)".to_vec())
        .await;
    cx.executor().run_until_parked();
    assert!(project.read_with(cx, |project, _| {
        project.android_context().is_current(&token)
    }));
    filesystem
        .insert_file(logic.join("src/Convention.kt"), b"changed".to_vec())
        .await;
    cx.executor().run_until_parked();
    assert!(!project.read_with(cx, |project, _| {
        project.android_context().is_current(&token)
    }));
    assert!(project.read_with(cx, |project, _| {
        project.android_context().snapshot(handle).is_none()
    }));
    assert_eq!(
        project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()),
        1
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[gpui::test]
async fn real_linux_deleted_input_root_requires_ready_reinstallation_before_reimport(
    cx: &mut TestAppContext,
) {
    real_linux_deleted_input_root_requires_ready_reinstallation_before_reimport_case(cx)
        .await
        .expect("Android project-context fixture must complete successfully");
}

#[cfg(target_os = "linux")]
async fn real_linux_deleted_input_root_requires_ready_reinstallation_before_reimport_case(
    cx: &mut TestAppContext,
) -> Result<()> {
    init_test(cx);
    use fs::RealFs;
    cx.executor().allow_parking();
    let fixture = tempfile::TempDir::new()?;
    let root = fixture.path().join("project");
    let logic = fixture.path().join("external-logic");
    let source = logic.join("src/deep/Convention.kt");
    std::fs::create_dir_all(&root)?;
    std::fs::create_dir_all(source.parent().context("Source parent")?)?;
    std::fs::write(root.join("build.gradle"), "")?;
    std::fs::write(&source, "original")?;
    let project = Project::test(RealFs::new(None, cx.executor()), [root.as_path()], cx).await;
    let worktree = project
        .read_with(cx, |project, cx| {
            project
                .visible_worktrees(cx)
                .next()
                .map(|worktree| worktree.read(cx).id())
        })
        .context("Root worktree")?;
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
        .context("Import owner")?;
    let snapshot = observed_fixture(&root, &logic)?;
    assert!(
        project
            .update(cx, |project, cx| project.observe_android_context_inputs(
                handle,
                discovery.clone(),
                vec![logic.clone()],
                Some(snapshot.clone()),
                cx
            ))
            .await?
    );
    std::fs::remove_dir_all(&logic)?;
    cx.condition(&project, |project, _| {
        !project.android_context().import_is_current(&discovery)
    })
    .await;
    assert!(!project.read_with(cx, |project, _| {
        project.android_context_observes(handle, &logic)
    }));
    assert!(
        project
            .update(cx, |project, cx| project.publish_android_context(
                &active, &owner, &discovery, snapshot, cx
            ))
            .is_err()
    );
    std::fs::create_dir_all(source.parent().context("Recreated source parent")?)?;
    std::fs::write(&source, "recreated input")?;
    assert!(!project.read_with(cx, |project, _| {
        project.android_context_observes(handle, &logic)
    }));
    let discovery = project.update(cx, |project, cx| {
        project.begin_android_context_import(handle, cx)
    })?;
    let owner = project
        .read_with(cx, |project, _| {
            active.discovery_token(project.android_context())
        })
        .context("Reimport owner")?;
    let snapshot = observed_fixture(&root, &logic)?;
    assert!(
        project
            .update(cx, |project, cx| project.observe_android_context_inputs(
                handle,
                discovery.clone(),
                vec![logic.clone()],
                Some(snapshot.clone()),
                cx
            ))
            .await?
    );
    project
        .update(cx, |project, cx| {
            project.verify_android_context_observers(
                handle,
                discovery.clone(),
                vec![logic.clone()],
                cx,
            )
        })
        .await?;
    project.update(cx, |project, cx| {
        project.publish_android_context(&active, &owner, &discovery, snapshot, cx)
    })?;
    let token = project
        .read_with(cx, |project, _| project.android_context().token(handle))
        .context("Recreated root")?;
    std::fs::write(&source, "changed after reimport")?;
    cx.condition(&project, |project, _| {
        !project.android_context().is_current(&token)
    })
    .await;
    assert_eq!(
        project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()),
        1
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[gpui::test]
async fn real_linux_new_evaluated_build_logic_inside_old_output_installs_deep_coverage(
    cx: &mut TestAppContext,
) {
    real_linux_new_evaluated_build_logic_inside_old_output_installs_deep_coverage_case(cx)
        .await
        .expect("Android project-context fixture must complete successfully");
}

#[cfg(target_os = "linux")]
async fn real_linux_new_evaluated_build_logic_inside_old_output_installs_deep_coverage_case(
    cx: &mut TestAppContext,
) -> Result<()> {
    init_test(cx);
    use fs::{Fs as _, RealFs};
    cx.executor().allow_parking();
    let fixture = tempfile::TempDir::new()?;
    let root = fixture.path().join("project");
    let logic = fixture.path().join("external-logic");
    let child = logic.join("custom-output/child-logic");
    let child_source = child.join("src/deep/Convention.kt");
    std::fs::create_dir_all(&root)?;
    std::fs::create_dir_all(logic.join("src"))?;
    std::fs::create_dir_all(logic.join("custom-output"))?;
    std::fs::write(root.join("build.gradle"), "")?;
    let filesystem = RealFs::new(None, cx.executor());
    let recording = filesystem
        .record_watcher_diagnostics()
        .context("Native watcher diagnostics")?;
    let project = Project::test(filesystem, [root.as_path()], cx).await;
    let worktree = project
        .read_with(cx, |project, cx| {
            project
                .visible_worktrees(cx)
                .next()
                .map(|worktree| worktree.read(cx).id())
        })
        .context("Root worktree")?;
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
        .context("Import owner")?;
    let snapshot = observed_fixture(&root, &logic)?;
    assert!(
        project
            .update(cx, |project, cx| project.observe_android_context_inputs(
                handle,
                discovery.clone(),
                vec![logic.clone()],
                Some(snapshot.clone()),
                cx
            ))
            .await?
    );
    project.update(cx, |project, cx| {
        project.publish_android_context(&active, &owner, &discovery, snapshot, cx)
    })?;
    std::fs::create_dir_all(child_source.parent().context("Child source parent")?)?;
    std::fs::write(&child_source, "new evaluated source")?;
    assert!(!project.read_with(cx, |project, _| {
        project.android_context_observes(handle, &child)
    }));
    let discovery = project.update(cx, |project, cx| {
        project.begin_android_context_import(handle, cx)
    })?;
    let owner = project
        .read_with(cx, |project, _| {
            active.discovery_token(project.android_context())
        })
        .context("Reimport owner")?;
    let mut raw = observed_fixture_record(&root, &logic);
    raw["buildLogicDirectories"] = json!([&logic, &child]);
    raw["buildLayouts"]
        .as_array_mut()
        .context("Build layouts")?
        .push(json!({"directory":&child,
        "buildDirectory":child.join("generated"),"sourceDirectories":[child.join("src")]}));
    let snapshot = decode_context_record(&serde_json::to_vec(&raw)?, &root)?;
    let directories = snapshot.observer_directories()?;
    assert!(directories.contains(&logic));
    assert!(directories.contains(&child));
    assert!(directories.contains(&child.join("src")));
    assert!(
        project
            .update(cx, |project, cx| project.observe_android_context_inputs(
                handle,
                discovery.clone(),
                directories.clone(),
                Some(snapshot.clone()),
                cx
            ))
            .await?
    );
    let source_directory = child_source.parent().context("Child source parent")?;
    assert!(
        recording
            .snapshot()
            .watchers
            .iter()
            .flat_map(|watcher| &watcher.roots)
            .any(|registered| registered.path == source_directory.to_string_lossy())
    );
    project
        .update(cx, |project, cx| {
            project.verify_android_context_observers(handle, discovery.clone(), directories, cx)
        })
        .await?;
    project.update(cx, |project, cx| {
        project.publish_android_context(&active, &owner, &discovery, snapshot, cx)
    })?;
    let token = project
        .read_with(cx, |project, _| project.android_context().token(handle))
        .context("New layout root")?;
    std::fs::write(&child_source, "changed evaluated source")?;
    cx.condition(&project, |project, _| {
        !project.android_context().is_current(&token)
    })
    .await;
    assert_eq!(
        project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()),
        1
    );
    Ok(())
}

#[gpui::test]
async fn input_observers_are_reused_and_pending_source_edits_reject_publication(
    cx: &mut TestAppContext,
) {
    input_observers_are_reused_and_pending_source_edits_reject_publication_case(cx)
        .await
        .expect("Android project-context fixture must complete successfully");
}

async fn input_observers_are_reused_and_pending_source_edits_reject_publication_case(
    cx: &mut TestAppContext,
) -> Result<()> {
    init_test(cx);
    let root = Path::new(path!("/context-reimport"));
    let logic = Path::new(path!("/convention-reimport"));
    let filesystem = FakeFs::new(cx.executor());
    filesystem
        .insert_tree(root, json!({"build.gradle":""}))
        .await;
    filesystem
        .insert_tree(logic, json!({"src":{"Convention.kt":"original"}}))
        .await;
    let project = Project::test(filesystem.clone(), [root.as_ref()], cx).await;
    let worktree = project
        .read_with(cx, |project, cx| {
            project
                .visible_worktrees(cx)
                .next()
                .map(|worktree| worktree.read(cx).id())
        })
        .context("Root worktree")?;
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
        .context("Import owner")?;
    let snapshot = observed_fixture(root, logic)?;
    assert!(
        project
            .update(cx, |project, cx| project.observe_android_context_inputs(
                handle,
                discovery.clone(),
                vec![logic.to_path_buf()],
                None,
                cx
            ))
            .await?
    );
    assert!(
        !project
            .update(cx, |project, cx| project.observe_android_context_inputs(
                handle,
                discovery.clone(),
                vec![logic.to_path_buf()],
                None,
                cx
            ))
            .await?
    );
    filesystem
        .insert_file(
            logic.join("src/Convention.kt"),
            b"changed during import".to_vec(),
        )
        .await;
    cx.executor().run_until_parked();
    assert!(
        project
            .update(cx, |project, cx| project.publish_android_context(
                &active, &owner, &discovery, snapshot, cx
            ))
            .is_err()
    );
    assert!(!project.read_with(cx, |project, _| {
        project.android_context().import_is_current(&discovery)
    }));
    project.update(cx, |project, cx| {
        project.ensure_android_context(worktree, false, cx)
    })?;
    assert!(project.read_with(cx, |project, _| {
        project.android_context().token(handle).is_none()
    }));
    assert!(!project.read_with(cx, |project, _| {
        project.android_context_observes(handle, logic)
    }));
    assert_eq!(
        project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()),
        1
    );
    Ok(())
}

#[gpui::test]
async fn selected_root_observer_detects_later_buildsrc_creation_without_new_worktrees(
    cx: &mut TestAppContext,
) {
    selected_root_observer_detects_later_buildsrc_creation_without_new_worktrees_case(cx)
        .await
        .expect("Android project-context fixture must complete successfully");
}

async fn selected_root_observer_detects_later_buildsrc_creation_without_new_worktrees_case(
    cx: &mut TestAppContext,
) -> Result<()> {
    init_test(cx);
    let root = Path::new(path!("/later-convention-project"));
    let logic = root.join("buildSrc");
    let filesystem = FakeFs::new(cx.executor());
    filesystem
        .insert_tree(root, json!({"build.gradle":""}))
        .await;
    let project = Project::test(filesystem.clone(), [root.as_ref()], cx).await;
    let worktree = project
        .read_with(cx, |project, cx| {
            project
                .visible_worktrees(cx)
                .next()
                .map(|worktree| worktree.read(cx).id())
        })
        .context("Root worktree")?;
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
        .context("Import owner")?;
    assert!(
        project
            .update(cx, |project, cx| project.observe_android_context_inputs(
                handle,
                discovery.clone(),
                vec![root.to_path_buf()],
                None,
                cx
            ))
            .await?
    );
    assert!(project.read_with(cx, |project, _| {
        project.android_context_observes(handle, &logic)
    }));
    assert!(!project.read_with(cx, |project, _| {
        project.android_context_observes(handle, Path::new(path!("/different-project")))
    }));
    project.update(cx, |project, cx| {
        project.publish_android_context(
            &active,
            &owner,
            &discovery,
            observed_fixture(root, &logic)?,
            cx,
        )
    })?;
    let token = project
        .read_with(cx, |project, _| project.android_context().token(handle))
        .context("Published root")?;
    filesystem
        .insert_tree(&logic, json!({"src":{"Convention.kt":"new convention"}}))
        .await;
    cx.executor().run_until_parked();
    assert!(!project.read_with(cx, |project, _| {
        project.android_context().is_current(&token)
    }));
    assert_eq!(
        project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()),
        1
    );
    Ok(())
}

#[gpui::test]
async fn external_linked_git_observer_tracks_actual_head_without_cancelling_on_unrelated_refs(
    cx: &mut TestAppContext,
) {
    external_linked_git_observer_tracks_actual_head_without_cancelling_on_unrelated_refs_case(cx)
        .await
        .expect("Android project-context fixture must complete successfully");
}

async fn external_linked_git_observer_tracks_actual_head_without_cancelling_on_unrelated_refs_case(
    cx: &mut TestAppContext,
) -> Result<()> {
    init_test(cx);
    let root = Path::new(path!("/head-context-project"));
    let logic = Path::new(path!("/head-external-logic"));
    let common = Path::new(path!("/shared-context-git"));
    let git_directory = common.join("worktrees/logic");
    let original = "1".repeat(40);
    let unrelated = "2".repeat(40);
    let next = "3".repeat(40);
    let filesystem = FakeFs::new(cx.executor());
    filesystem
        .insert_tree(root, json!({"build.gradle":""}))
        .await;
    filesystem.insert_tree(logic, json!({".git":"gitdir: ../shared-context-git/worktrees/logic\n", "src":{"Convention.kt":"original"}})).await;
    filesystem.insert_tree(common, json!({
        "worktrees":{"logic":{"HEAD":"ref: refs/heads/external\n", "commondir":"../..\n"}},
        "refs":{"heads":{}}, "packed-refs":format!("# pack-refs with: peeled\n{original} refs/heads/external\n")
    })).await;
    let project = Project::test(filesystem.clone(), [root.as_ref()], cx).await;
    let worktree = project
        .read_with(cx, |project, cx| {
            project
                .visible_worktrees(cx)
                .next()
                .map(|worktree| worktree.read(cx).id())
        })
        .context("Root worktree")?;
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
        .context("Import owner")?;
    assert!(
        project
            .update(cx, |project, cx| project.observe_android_context_inputs(
                handle,
                discovery.clone(),
                vec![logic.to_path_buf()],
                None,
                cx
            ))
            .await?
    );
    assert!(filesystem.watch_calls().contains(&git_directory));
    assert!(filesystem.watch_calls().contains(&common.to_path_buf()));
    project.update(cx, |project, cx| {
        project.publish_android_context(
            &active,
            &owner,
            &discovery,
            observed_fixture(root, logic)?,
            cx,
        )
    })?;
    let token = project
        .read_with(cx, |project, _| project.android_context().token(handle))
        .context("Published root")?;
    filesystem
        .insert_file(
            common.join("refs/heads/unrelated-task"),
            format!("{unrelated}\n").into_bytes(),
        )
        .await;
    filesystem
        .insert_file(
            common.join("packed-refs"),
            format!("{original} refs/heads/external\n{unrelated} refs/heads/another-task\n")
                .into_bytes(),
        )
        .await;
    cx.executor().run_until_parked();
    assert!(project.read_with(cx, |project, _| {
        project.android_context().is_current(&token)
    }));
    filesystem
        .insert_file(
            common.join("refs/heads/external"),
            format!("{next}\n").into_bytes(),
        )
        .await;
    cx.executor().run_until_parked();
    assert!(!project.read_with(cx, |project, _| {
        project.android_context().is_current(&token)
    }));
    assert!(project.read_with(cx, |project, _| {
        project.android_context().snapshot(handle).is_none()
    }));
    let discovery = project.update(cx, |project, cx| {
        project.begin_android_context_import(handle, cx)
    })?;
    let owner = project
        .read_with(cx, |project, _| {
            active.discovery_token(project.android_context())
        })
        .context("Reimport owner")?;
    assert!(
        !project
            .update(cx, |project, cx| project.observe_android_context_inputs(
                handle,
                discovery.clone(),
                vec![logic.to_path_buf()],
                None,
                cx
            ))
            .await?
    );
    project.update(cx, |project, cx| {
        project.publish_android_context(
            &active,
            &owner,
            &discovery,
            observed_fixture(root, logic)?,
            cx,
        )
    })?;
    let token = project
        .read_with(cx, |project, _| project.android_context().token(handle))
        .context("Republished root")?;
    filesystem
        .insert_file(
            git_directory.join("HEAD"),
            b"ref: refs/heads/unrelated-task\n".to_vec(),
        )
        .await;
    cx.executor().run_until_parked();
    assert!(!project.read_with(cx, |project, _| {
        project.android_context().is_current(&token)
    }));
    assert_eq!(
        project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()),
        1
    );
    Ok(())
}

#[gpui::test]
async fn recursive_inputs_track_nested_and_enclosing_git_owners_without_extra_worktrees(
    cx: &mut TestAppContext,
) {
    recursive_inputs_track_nested_and_enclosing_git_owners_without_extra_worktrees_case(cx)
        .await
        .expect("Android project-context fixture must complete successfully");
}

async fn recursive_inputs_track_nested_and_enclosing_git_owners_without_extra_worktrees_case(
    cx: &mut TestAppContext,
) -> Result<()> {
    init_test(cx);
    for nested in [true, false] {
        let root = if nested {
            Path::new(path!("/nested-head-project"))
        } else {
            Path::new(path!("/enclosing-head-project"))
        };
        let owner = if nested {
            root.join("buildSrc")
        } else {
            Path::new(path!("/enclosing-logic-repository")).to_path_buf()
        };
        let logic = if nested {
            owner.clone()
        } else {
            owner.join("build-logic")
        };
        let filesystem = FakeFs::new(cx.executor());
        filesystem
            .insert_tree(root, json!({"build.gradle":""}))
            .await;
        filesystem
            .insert_tree(&logic, json!({"src":{"deep":{"Convention.kt":"original"}}}))
            .await;
        filesystem.insert_tree(owner.join(".git"), json!({"HEAD":"ref: refs/heads/current\n", "refs":{"heads":{"current":format!("{}\n", "1".repeat(40))}}})).await;
        let project = Project::test(filesystem.clone(), [root.as_ref()], cx).await;
        let worktree = project
            .read_with(cx, |project, cx| {
                project
                    .visible_worktrees(cx)
                    .next()
                    .map(|worktree| worktree.read(cx).id())
            })
            .context("Root worktree")?;
        let handle = project.update(cx, |project, cx| {
            project.ensure_android_context(worktree, true, cx)
        })?;
        let discovery = project.update(cx, |project, cx| {
            project.begin_android_context_import(handle, cx)
        })?;
        let mut active = ActiveContext::default();
        active.select(Some(handle), None)?;
        let active_owner = project
            .read_with(cx, |project, _| {
                active.discovery_token(project.android_context())
            })
            .context("Import owner")?;
        assert!(
            project
                .update(cx, |project, cx| project.observe_android_context_inputs(
                    handle,
                    discovery.clone(),
                    vec![root.to_path_buf()],
                    None,
                    cx
                ))
                .await?
        );
        let added = project
            .update(cx, |project, cx| {
                project.observe_android_context_inputs(
                    handle,
                    discovery.clone(),
                    vec![logic.clone()],
                    None,
                    cx,
                )
            })
            .await?;
        assert_eq!(added, !nested);
        assert!(filesystem.watch_calls().contains(&owner.join(".git")));
        project.update(cx, |project, cx| {
            project.publish_android_context(
                &active,
                &active_owner,
                &discovery,
                observed_fixture(root, &logic)?,
                cx,
            )
        })?;
        let token = project
            .read_with(cx, |project, _| project.android_context().token(handle))
            .context("Published root")?;
        filesystem
            .insert_file(
                owner.join(".git/refs/heads/unrelated-task"),
                format!("{}\n", "2".repeat(40)).into_bytes(),
            )
            .await;
        cx.executor().run_until_parked();
        assert!(project.read_with(cx, |project, _| {
            project.android_context().is_current(&token)
        }));
        filesystem
            .insert_file(
                owner.join(".git/HEAD"),
                format!("{}\n", "3".repeat(40)).into_bytes(),
            )
            .await;
        cx.executor().run_until_parked();
        assert!(!project.read_with(cx, |project, _| {
            project.android_context().is_current(&token)
        }));
        assert_eq!(
            project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()),
            1
        );
    }
    Ok(())
}

#[gpui::test]
async fn root_removal_releases_only_owned_inputs_and_keeps_other_worktrees(
    cx: &mut TestAppContext,
) {
    root_removal_releases_only_owned_inputs_and_keeps_other_worktrees_case(cx)
        .await
        .expect("Android project-context fixture must complete successfully");
}

async fn root_removal_releases_only_owned_inputs_and_keeps_other_worktrees_case(
    cx: &mut TestAppContext,
) -> Result<()> {
    init_test(cx);
    let root_a = Path::new(path!("/owned-context-a"));
    let root_b = Path::new(path!("/owned-context-b"));
    let logic_a = Path::new(path!("/owned-logic-a"));
    let logic_b = Path::new(path!("/owned-logic-b"));
    let filesystem = FakeFs::new(cx.executor());
    for root in [root_a, root_b] {
        filesystem
            .insert_tree(root, json!({"build.gradle":"", "main.py":"print(1)"}))
            .await;
    }
    for logic in [logic_a, logic_b] {
        filesystem
            .insert_tree(logic, json!({"src":{"Convention.kt":"original"}}))
            .await;
    }
    let project = Project::test(filesystem.clone(), [root_a.as_ref(), root_b.as_ref()], cx).await;
    let worktree_a = project
        .read_with(cx, |project, cx| {
            project
                .find_worktree(root_a, cx)
                .map(|(worktree, _)| worktree.read(cx).id())
        })
        .context("First worktree")?;
    let worktree_b = project
        .read_with(cx, |project, cx| {
            project
                .find_worktree(root_b, cx)
                .map(|(worktree, _)| worktree.read(cx).id())
        })
        .context("Second worktree")?;
    let handle_a = project.update(cx, |project, cx| {
        project.ensure_android_context(worktree_a, true, cx)
    })?;
    let handle_b = project.update(cx, |project, cx| {
        project.ensure_android_context(worktree_b, true, cx)
    })?;
    assert!(!project.read_with(cx, |project, _| {
        project.android_context_observes(handle_a, logic_a)
    }));
    assert!(!project.read_with(cx, |project, _| {
        project.android_context_observes(handle_b, logic_b)
    }));
    let wrong_owner = project.update(cx, |project, cx| {
        project.begin_android_context_import(handle_a, cx)
    })?;
    assert!(
        project
            .update(cx, |project, cx| project.observe_android_context_inputs(
                handle_b,
                wrong_owner,
                vec![logic_b.to_path_buf()],
                None,
                cx
            ))
            .await
            .is_err()
    );
    assert!(!project.read_with(cx, |project, _| {
        project.android_context_observes(handle_b, logic_b)
    }));
    for (handle, logic) in [(handle_a, logic_a), (handle_b, logic_b)] {
        let discovery = project.update(cx, |project, cx| {
            project.begin_android_context_import(handle, cx)
        })?;
        assert!(
            project
                .update(cx, |project, cx| project.observe_android_context_inputs(
                    handle,
                    discovery,
                    vec![logic.to_path_buf()],
                    None,
                    cx
                ))
                .await?
        );
    }
    assert!(filesystem.watched_paths().contains(&logic_a.to_path_buf()));
    assert!(filesystem.watched_paths().contains(&logic_b.to_path_buf()));
    project.update(cx, |project, cx| project.remove_worktree(worktree_a, cx));
    cx.executor().run_until_parked();
    assert!(project.read_with(cx, |project, _| {
        project
            .android_context()
            .handle(worktree_a.to_proto())
            .is_none()
    }));
    assert!(!filesystem.watched_paths().contains(&logic_a.to_path_buf()));
    assert!(filesystem.watched_paths().contains(&logic_b.to_path_buf()));
    assert!(project.read_with(cx, |project, _| {
        project.android_context_observes(handle_b, logic_b)
    }));
    assert_eq!(
        project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()),
        1
    );
    assert!(project.read_with(cx, |project, cx| {
        project.worktree_for_id(worktree_b, cx).is_some()
    }));
    Ok(())
}

#[cfg(target_os = "linux")]
#[gpui::test]
async fn real_linux_observer_registers_deep_inputs_and_later_directories(cx: &mut TestAppContext) {
    real_linux_observer_registers_deep_inputs_and_later_directories_case(cx)
        .await
        .expect("Android project-context fixture must complete successfully");
}

#[cfg(target_os = "linux")]
async fn real_linux_observer_registers_deep_inputs_and_later_directories_case(
    cx: &mut TestAppContext,
) -> Result<()> {
    init_test(cx);
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
    std::fs::create_dir_all(
        current_ref
            .parent()
            .context("Current Git reference parent")?,
    )?;
    std::fs::write(root.join("build.gradle"), "")?;
    std::fs::write(&source, "original")?;
    std::fs::write(
        logic.join(".git"),
        "gitdir: ../shared-git/worktrees/logic\n",
    )?;
    std::fs::write(
        git_directory.join("HEAD"),
        "ref: refs/heads/android/current\n",
    )?;
    std::fs::write(git_directory.join("commondir"), "../..\n")?;
    std::fs::write(&current_ref, format!("{}\n", "1".repeat(40)))?;
    let filesystem = RealFs::new(None, cx.executor());
    let recording = filesystem
        .record_watcher_diagnostics()
        .context("Native watcher diagnostics")?;
    let project = Project::test(filesystem, [root.as_path()], cx).await;
    let worktree = project
        .read_with(cx, |project, cx| {
            project
                .visible_worktrees(cx)
                .next()
                .map(|worktree| worktree.read(cx).id())
        })
        .context("Root worktree")?;
    cx.executor().run_until_parked();
    let baseline = recording.snapshot();
    assert!(
        !baseline
            .watchers
            .iter()
            .flat_map(|watcher| &watcher.roots)
            .any(|registered| Path::new(&registered.path).starts_with(&logic)
                || Path::new(&registered.path).starts_with(&common_git))
    );
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
        .context("Import owner")?;
    let snapshot = observed_fixture(&root, &logic)?;
    assert!(
        project
            .update(cx, |project, cx| project.observe_android_context_inputs(
                handle,
                discovery.clone(),
                vec![logic.clone()],
                Some(snapshot.clone()),
                cx
            ))
            .await?
    );
    let native = recording
        .snapshot()
        .watchers
        .into_iter()
        .filter(|watcher| watcher.backend == OsWatcherKind::Native)
        .collect::<Vec<_>>();
    let source_directory = source.parent().context("Source parent")?;
    assert!(native.iter().all(|watcher| !watcher.recursive));
    assert!(
        native
            .iter()
            .flat_map(|watcher| &watcher.roots)
            .any(|registered| registered.path == source_directory.to_string_lossy())
    );
    assert!(
        !native
            .iter()
            .flat_map(|watcher| &watcher.roots)
            .any(|registered| Path::new(&registered.path).starts_with(&output))
    );
    assert_eq!(
        native
            .iter()
            .flat_map(|watcher| &watcher.roots)
            .filter(|registered| Path::new(&registered.path).starts_with(&logic))
            .count(),
        3
    );
    let current_ref_directory = current_ref
        .parent()
        .context("Current Git reference parent")?;
    assert!(
        native
            .iter()
            .flat_map(|watcher| &watcher.roots)
            .any(|registered| registered.path == current_ref_directory.to_string_lossy())
    );
    project.update(cx, |project, cx| {
        project.publish_android_context(&active, &owner, &discovery, snapshot, cx)
    })?;
    let token = project
        .read_with(cx, |project, _| project.android_context().token(handle))
        .context("Published root")?;
    std::fs::write(&source, "changed input")?;
    cx.condition(&project, |project, _| {
        !project.android_context().is_current(&token)
    })
    .await;
    let discovery = project.update(cx, |project, cx| {
        project.begin_android_context_import(handle, cx)
    })?;
    let owner = project
        .read_with(cx, |project, _| {
            active.discovery_token(project.android_context())
        })
        .context("Reimport owner")?;
    let reimport_snapshot = observed_fixture(&root, &logic)?;
    assert!(
        !project
            .update(cx, |project, cx| project.observe_android_context_inputs(
                handle,
                discovery.clone(),
                vec![logic.clone()],
                Some(reimport_snapshot),
                cx
            ))
            .await?
    );
    project.update(cx, |project, cx| {
        project.publish_android_context(
            &active,
            &owner,
            &discovery,
            observed_fixture(&root, &logic)?,
            cx,
        )
    })?;
    let token = project
        .read_with(cx, |project, _| project.android_context().token(handle))
        .context("Git reference root")?;
    std::fs::write(&current_ref, format!("{}\n", "2".repeat(40)))?;
    cx.condition(&project, |project, _| {
        !project.android_context().is_current(&token)
    })
    .await;
    let discovery = project.update(cx, |project, cx| {
        project.begin_android_context_import(handle, cx)
    })?;
    let owner = project
        .read_with(cx, |project, _| {
            active.discovery_token(project.android_context())
        })
        .context("New directory owner")?;
    project.update(cx, |project, cx| {
        project.publish_android_context(
            &active,
            &owner,
            &discovery,
            observed_fixture(&root, &logic)?,
            cx,
        )
    })?;
    let token = project
        .read_with(cx, |project, _| project.android_context().token(handle))
        .context("Republished root")?;
    let new_source = logic.join("src/later/nested/Convention.kt");
    std::fs::create_dir_all(new_source.parent().context("Later source parent")?)?;
    std::fs::write(&new_source, "new input")?;
    cx.condition(&project, |project, _| {
        !project.android_context().is_current(&token)
    })
    .await;
    let later_directory = new_source.parent().context("Later source parent")?;
    assert!(
        recording
            .snapshot()
            .watchers
            .iter()
            .flat_map(|watcher| &watcher.roots)
            .any(|registered| registered.path == later_directory.to_string_lossy())
    );
    assert_eq!(
        project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()),
        1
    );
    project.update(cx, |project, cx| {
        project.ensure_android_context(worktree, false, cx)
    })?;
    cx.executor().run_until_parked();
    assert!(!project.read_with(cx, |project, _| {
        project.android_context_observes(handle, &logic)
    }));
    let remaining = recording.snapshot();
    assert!(
        !remaining
            .watchers
            .iter()
            .flat_map(|watcher| &watcher.roots)
            .any(|registered| Path::new(&registered.path).starts_with(&logic)
                || Path::new(&registered.path).starts_with(&common_git))
    );
    for original in baseline.watchers.iter().flat_map(|watcher| &watcher.roots) {
        assert!(
            remaining
                .watchers
                .iter()
                .flat_map(|watcher| &watcher.roots)
                .any(|registered| registered.path == original.path)
        );
    }
    Ok(())
}

#[gpui::test]
async fn removed_input_batches_retire_watchers_on_background_and_do_not_revive_restricted_roots(
    cx: &mut TestAppContext,
) {
    async {
        use fs::{Fs as _, RemoveOptions};
        use parking_lot::Mutex;
        use std::sync::Arc;

        init_test(cx);
        let root_a = Path::new(path!("/background-removal-owner"));
        let root_b = Path::new(path!("/retained-removal-owner"));
        let filesystem = FakeFs::new(cx.executor());
        for root in [root_a, root_b] {
            filesystem
                .insert_tree(
                    root,
                    json!({"build.gradle":"", "retained":{"main.py":"print(1)"}}),
                )
                .await;
        }
        for index in 0..512 {
            filesystem
                .insert_tree(
                    root_a.join("ordinary").join(index.to_string()),
                    json!({"main.py":"print(1)", "index.html":"<p>retained</p>"}),
                )
                .await;
        }
        let project = Project::test(filesystem.clone(), [root_a, root_b], cx).await;
        cx.executor().run_until_parked();
        let baseline_owner_a_watchers = filesystem
            .watched_paths()
            .into_iter()
            .filter(|path| path == root_a)
            .count();
        let removals = Arc::new(Mutex::new(Vec::new()));
        let executor = cx.executor();
        filesystem.observe_new_watcher_removals(Arc::new({
            let removals = removals.clone();
            move |path| {
                removals
                    .lock()
                    .push((path.to_path_buf(), executor.is_main_thread()));
            }
        }));
        let mut contexts = Vec::new();
        for root in [root_a, root_b] {
            let worktree = project
                .read_with(cx, |project, cx| {
                    project
                        .visible_worktrees(cx)
                        .find(|worktree| worktree.read(cx).abs_path().as_ref() == root)
                        .map(|worktree| worktree.read(cx).id())
                })
                .context("Owning worktree")?;
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
                .context("Input observer import owner")?;
            let snapshot = observed_fixture(root, &root.join("build-logic"))?;
            assert!(
                project
                    .update(cx, |project, cx| project.observe_android_context_inputs(
                        handle,
                        discovery.clone(),
                        vec![root.to_path_buf()],
                        Some(snapshot.clone()),
                        cx,
                    ))
                    .await?
            );
            project.update(cx, |project, cx| {
                project.publish_android_context(&active, &owner, &discovery, snapshot, cx)
            })?;
            let token = project
                .read_with(cx, |project, _| project.android_context().token(handle))
                .context("Published input observer owner")?;
            contexts.push((worktree, handle, token));
        }
        cx.executor().run_until_parked();
        assert!(removals.lock().is_empty());
        let (worktree_a, handle_a, token_a) = contexts.first().cloned().context("First owner")?;
        let (_, handle_b, token_b) = contexts.get(1).cloned().context("Second owner")?;
        assert_eq!(
            filesystem
                .watched_paths()
                .into_iter()
                .filter(|path| path == root_a)
                .count(),
            baseline_owner_a_watchers + 1,
        );

        filesystem.pause_events();
        for index in 0..512 {
            filesystem
                .remove_file(
                    &root_a
                        .join("ordinary")
                        .join(index.to_string())
                        .join("main.py"),
                    RemoveOptions::default(),
                )
                .await?;
        }
        let removed_directory = root_a.join("ordinary/17");
        filesystem
            .remove_dir(
                &removed_directory,
                RemoveOptions {
                    recursive: true,
                    ..Default::default()
                },
            )
            .await?;
        assert!(filesystem.buffered_event_count() >= 512);
        filesystem.unpause_events_and_flush();

        // Run one scheduler task at a time. Once retirement is recorded, its
        // background task has returned but its owning foreground continuation has
        // not run; revoke that owner's trust before resuming it.
        while removals.lock().is_empty() {
            assert!(
                cx.executor().tick(),
                "Actual input observer retirement must become runnable"
            );
        }
        let recorded = removals.lock().clone();
        assert!(recorded.iter().any(|(path, _)| path == &removed_directory));
        assert!(recorded.iter().all(|(_, main_thread)| !main_thread));
        assert!(recorded.iter().all(|(path, _)| path.starts_with(root_a)));
        project.read_with(cx, |project, _| {
            assert!(project.android_context().is_current(&token_a));
            assert!(project.android_context().is_current(&token_b));
        });
        project.update(cx, |project, cx| {
            project.ensure_android_context(worktree_a, false, cx)
        })?;
        cx.executor().run_until_parked();
        project.read_with(cx, |project, _| {
            assert!(!project.android_context().is_current(&token_a));
            assert!(project.android_context().snapshot(handle_a).is_none());
            assert!(project.android_context().is_current(&token_b));
            assert!(project.android_context().snapshot(handle_b).is_some());
        });
        assert_eq!(
            filesystem
                .watched_paths()
                .into_iter()
                .filter(|path| path == root_a)
                .count(),
            baseline_owner_a_watchers,
            "Only the revoked root's input observer is released",
        );
        filesystem
            .insert_file(
                root_b.join("build.gradle"),
                b"changed retained input".to_vec(),
            )
            .await;
        cx.executor().run_until_parked();
        assert!(!project.read_with(cx, |project, _| {
            project.android_context().is_current(&token_b)
        }));
        Ok::<(), anyhow::Error>(())
    }
    .await
    .expect("Actual watcher retirement must run on background and preserve context ownership");
}
