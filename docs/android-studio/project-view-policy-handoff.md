# Android foundation: project-view policy

Prepare the next bounded Rust task for `anilbeesetti/koda`: Android project-view
availability, defaults, preferences and migration. Read `AGENTS.md` and
`docs/android-studio/implementation-plan.md` first. This task has not started
implementation and has no test-pass claim.

Use a separate branch such as `android-studio-task/1-project-view-policy`.
The authoritative read-only task proposal and exact declarations are in
`/workspace/android-studio-references/samples/foundation-source-provenance.json`.
Nine of the 14 declared `AndroidProjectViewTest` methods cover policy and can
use explicit Rust capability inputs, temporary configuration files, and local
notification/event collectors. The five tree cases remain separate and unported.

Proposed files are an `android_tools` project-view preferences module, its module
export, Rust reference tests, and the existing canonical ledger. Acceptance
includes Android/backend availability, default-view policy, visibility icons,
legacy property migration without damaging unrelated content, exactly one
migration notification, ordered change events and idempotent assignments.
Map every upstream assertion and fixture adaptation before implementation.

The lead must clear prerequisites before implementation. The current full-suite
build is blocked by the 32 GB filesystem. All five reviewers, the full app build,
the complete existing suite and applicable reference tests must pass before
merge. Keep `main` untouched and GitHub writes blocked. Never mark deferred or
unfinished behavior not applicable. This brief authorizes preparation within
the accepted foundation scope; it does not waive any verification gate.
