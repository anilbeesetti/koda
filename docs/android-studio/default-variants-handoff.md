# Android foundation: Rust default variants

Continue the bounded default-variant task for `anilbeesetti/koda` in the Koda
cloud environment. Read `AGENTS.md`, the local app-validation skill, and
`docs/android-studio/implementation-plan.md` before edits.

The owner worktree is `/workspace/android-studio-default-variants`, branch
`android-studio-task/1-default-variants`. Its report is
`docs/android-studio/ports/default-variants.json`. An internal owner is completing
the final app build and live recovery review; inspect that report and the branch
before taking ownership. Do not duplicate edits or launch a competing Cargo build.

The Rust comparator ports all eight pinned AOSP `DefaultVariantsTest` cases.
The Groovy bridge exports facts, while Rust selects the default. The task also
fixes stale unavailable-variant status after successful recovery and isolates
unrelated host ADB work in an existing GPUI status-test fixture without removing
assertions. The latest focused run passed 50 tooling tests and 105 UI tests;
one pre-existing opt-in native capture remains ignored. The exact final clippy
retry passed. Consult the report for final app and reviewer results.

Finish every affected reviewer and actual live check before accepting the task.
The lead's full-workspace test build exhausted the 32 GB filesystem, so this
task is not approved for merge. Preserve `main` and `android-studio`, never
force-push, and keep all GitHub writes blocked per the user's instruction.

Use `/workspace/android-studio-artifacts/android-env.sh` to activate the retained
Rust/native, SDK and Gradle environment. Keep logs and screenshots external;
preserve original Apache notices and test assertions. Report the exact tested
commit, results, five reviewer verdicts and remaining blockers.
