# Android foundation: Rust default variants

Continue the bounded default-variant task for `anilbeesetti/koda` in the Koda
cloud environment. Read `AGENTS.md`, the local app-validation skill, and
`docs/android-studio/implementation-plan.md` before edits.

The owner worktree is `/workspace/android-studio-default-variants`, branch
`android-studio-task/1-default-variants`. Its report is
`docs/android-studio/ports/default-variants.json`. The internal owner finished the
scoped checks and reviews at source commit
`c992461ee337253cca680554f2c18e1bced8c7e3`; evidence commit
`dd6ffa031f17fb136ecd2921efcdb83bd917cf6e` leaves the branch clean. Inspect that
report and the branch before taking ownership. Coordinate any Cargo build.

The Rust comparator ports all eight pinned AOSP `DefaultVariantsTest` cases.
The Groovy bridge exports facts, while Rust selects the default. The task also
fixes stale unavailable-variant status after successful recovery and isolates
unrelated host ADB work in an existing GPUI status-test fixture without removing
assertions. The latest focused run passed 50 tooling tests and 105 UI tests;
one pre-existing opt-in native capture remains ignored. The exact final clippy
retry, full app build, live recovery, and all five scoped reviewers passed.
Eight fresh exact runs also pass on the combined integration source snapshot.

Retain every reviewer verdict and live-check result before accepting the task.
The lead's full-workspace test build exhausted the 32 GB filesystem, so this
task is not approved for merge. Preserve `main` and `android-studio`, never
force-push. The user authorized GitHub pushes after access was restored;
publication does not waive any merge gate.

Use `/workspace/android-studio-artifacts/android-env.sh` to activate the retained
Rust/native, SDK and Gradle environment. Keep logs and screenshots external;
preserve original Apache notices and test assertions. Report the exact tested
commit, results, five reviewer verdicts and remaining blockers.
