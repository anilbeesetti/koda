# Foundation checkpoint

This is local preparation in `anilbeesetti/koda`, not a completed foundation
phase or IDE. Nothing has been merged into `android-studio`. The
original checkout and cached `origin/main` reference remain at
`30fca99a7a015168dfe4f394f576fbe2955fb4ea`. On 2026-10-07, authenticated shell
and connector reads confirmed restored GitHub access. The user then authorized
pushes. The four preparation branches and unchanged `android-studio` base are
prepared for publication; publication does not make these tasks merge-ready.

The staging branch is `android-studio-task/1-integration-check`, worktree
`/workspace/android-studio-integration`. Its tested source snapshot is
`e77d75de8502597457692f0b8c7c0ebbfe4e98d6`. It combines the plan, Rust default
variant port, recovery fix, parity tooling and reviewed catalog expansion.

## Local results

The default-variant owner passed all eight AOSP reference ports, 50 tooling tests,
105 Android UI tests, clippy, the full app build, real Gradle sync and live
variant recovery. One pre-existing opt-in native capture remains ignored; none
of the eight reference ports is ignored. Keyboard selection, resizing and panel
scrolling were checked in the full app. All five scoped reviewers passed after
three fix rounds. The final source is `c992461ee3`; its report and captured logs
are committed on the owner branch at `dd6ffa031f`.

The ledger and catalog expansion passed all 41 xtask tests, including all 21
parity regressions, repository clippy, formatting, and pinned archive/source/
fixture verification. All five scoped expansion reviewers passed. The original
eight-case validator inputs are now immutable fixture files; their bytes and
all original assertions are preserved.

Eight fresh manifest-bound exact Cargo runs on the staging source passed and
were imported into `test-parity.json`. The CLI independently validated their
source hashes, tested commit, named results and log hashes together with all
31 upstream citations and fixture references. The combined full app also built
successfully in 3 minutes 55 seconds with
`cargo build --locked -p zed --bin koda`; its log is
`/workspace/android-studio-artifacts/combined-app-build.log`. This was a build
check; the live checks above were performed on the owner source.

| Catalog status         | Count |
| ---------------------- | ----: |
| Ported and passing     |     8 |
| Adapted                |     0 |
| Not applicable         |     0 |
| Unported and not run   |    23 |
| Failing reference runs |     0 |

These counts cover three inspected suites. Exhaustive source/runner census is
incomplete and its total remains unknown. JetBrains mirror omissions remain
unverified. The complete-parity gate correctly refuses completion. Additional
model-sync methods and fixture provenance are research inputs outside the
catalog until runner expansions are established.

## Blocking gates and next work

`cargo test --locked --workspace --no-fail-fast` exited 101 during compilation
and linking when the 32 GB filesystem filled. Workspace tests had not begun
execution. Its log is
`/workspace/android-studio-artifacts/full-workspace-tests.log`. Cleaning only
generated output recovered room for focused checks; it does not establish a
full-suite pass. Every protected-branch merge remains blocked until the complete
suite passes. No test was deleted, newly ignored, or weakened.

The foundation still needs exhaustive census, pre-sync module-import planning,
Android project-view policy and tree projection, complete model/sync behavior,
window fidelity and creation workflows. Editing, build/run, Android tooling and
polish remain planned. Rust language servers and the full emulator/device/Logcat
done flow are not complete. This environment has no ADB target or `/dev/kvm`;
device execution was not validated.

Parallel work ran as internal subagents. Sidebar chat creation is unavailable
in this cloud session: the callable app tools have no `create_thread` or
`fork_thread`. Handoff briefs are saved beside this report. A new environment
can fetch the task branches after publication; SDK files, upstream source
archives and external build/UI evidence require separate workspace transfer.

A portable Git bundle is prepared at
`/workspace/android-studio-artifacts/android-studio-local-tasks.bundle`. It carries
the four preparation branches and requires the original base commit above to
exist in the receiving repository. SDK files, upstream source archives and
external build/UI evidence are separate workspace artifacts.
