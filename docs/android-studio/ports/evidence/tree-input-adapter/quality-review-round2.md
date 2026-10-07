# Tree input adapter: independent code quality review, round 2

Verdict: **FAIL** — one required test-import cleanup for the strict repository Clippy gate. The affected adapter source contracts otherwise pass static quality review.

Worktree: `/workspace/android-studio-tree-input-adapter`.

Bound hashes, independently verified before and after review:

- Adapter: `d57ba8070321039cdd8b9749bc43df923e694a31c27299a1ec577b5974edfc3b`.
- Supplemental tests: `1fa2f0e7eedf5bf6cf2cbdbdcd5a1df8604325956cf6bfce78a9626872f6f69d`.
- Additive export: `f4b033f8aa13b8141f706a1f24455c1d5500940ef5e4adffa46edd8baf1f2891`.

## Required finding

**P2 — Remove the unused `std::path::Path` import.**

The task owner flagged this during self-check, and I independently confirmed it: `crates/android_tools/tests/project_tree_adapter.rs:41` imports `Path`, while the only remaining whole-word occurrence is the diagnostic string `.context("Path")` at `:873`. The type is unused. This will trigger the Rust unused-import warning and conflict with the strict repository Clippy deny-warnings gate. No compiler or Clippy invocation has run here, so this is a static finding, not a recorded failed command.

Remove only `Path` from this new test file's import, preserving `PathBuf`, all assertions and fixtures. Re-freeze the test hash and obtain a narrow quality re-review, followed by the actual required tools under Root's lease. Production adapter SHA `d57ba8070321039cdd8b9749bc43df923e694a31c27299a1ec577b5974edfc3b` should remain unchanged.

## Affected quality checks

- Shared Java/Kotlin roots retain their original common group until exact-root deduplication has established the winner (`crates/android_tools/src/project_tree_adapter.rs:248-250,309-329`). Explicit Kotlin capability is consulted only for surviving common occurrences, with a typed unavailable error when their presentation still depends on Unknown capability (`:330-346`). The reason comment explains the upstream ordering requirement. Stable final sorting preserves encounters within each resulting group (`:355-357`). No unchecked indexing, panics or discarded errors were introduced.
- Module presence is now validated before Java parsing/projection. The checked explicit presence constructor is retained (`:463-467`), normalized unique inventory paths are checked once (`:468-475`), and actual module rows or component-wise physical descendants can establish Directory (`:477-488`). External generated descendants do not establish module presence. Unknown, Missing and File produce distinct typed provider failures with the module path (`:491-511`), while contradictory evidence propagates through `add_entry` rather than being overwritten. The existing Java parser/offset/fallback behavior remains intact (`:519-598`).
- Four additive regressions have meaningful supported-input assertions: cross-provider Kotlin-only/common roots for Enabled, Disabled and Unknown (`tests/project_tree_adapter.rs:1110-1132`); common roots shadowed by Java-only membership without required capability (`:1134-1152`); Unknown/Missing/File module states and a positively known empty module (`:1154-1180`); and external generated children failing to prove the module directory until explicit Directory proof is supplied (`:1182-1219`). Existing same-provider assertions still reject Unknown when a common occurrence survives.

## Test correction and history integrity

`fix-round1.json` explicitly retains the three static findings, before/after source hashes, one fix round, the four added regressions and **runtime checks not run**. The earlier FAIL reports remain present. No runtime failure is invented or suppressed.

The retained historical test snapshot `round1-source/project_tree_adapter.rs` identifies the original 23-test file by SHA `eeaaaa01809c3dabebbcdca3f84b3ad22b02db6060de54db4ffb858c4314133e`. It is a test snapshot, not the production adapter snapshot. Comparing that file with the first 23 tests in the current file finds exactly two setup substitutions:

- The presentation-only successful case now includes an actual synthetic module directory (`tests/project_tree_adapter.rs:297`). Its display-name/module-identity assertions are unchanged. The old setup's finite-world helper declared the directory Missing, so expecting a ready module was a wrong newly written setup.
- The unknown-assets-root case now includes the module directory (`:679`), so the test continues to isolate missing assets scan proof and retains every original reason/path/absence assertion.

Reversing only those two setup substitutions reconstructs the original test snapshot byte-for-byte and reproduces its SHA. No assertion in the original 23 additive tests was deleted, skipped or weakened. The new file contains **27 tests**, with no ignore or conditional skip attributes. The finite-world helper itself remains unchanged and explicitly synthetic (`:183-205`).

## Retained provenance and scope

All **97** protected paths in `immutable-inputs-before.json` were independently rehashed with zero changes, including original component/facts tests, parser/source fixtures, attributed upstream sources, Gradle exporter, canonical manifest and ledger. Workspace/crate Cargo manifests and Cargo.lock are byte-identical to HEAD; no dependency was introduced. The SDK fixture still matches `69dc1298842c340e278a48b898f3157d5c301fbdbb2a7aa292ed5481bd696c7f`; the prior independent verification of exactly 425 root-string normalization changes is unchanged. Apache headers, fixture attribution and the supported-subset/future-producer limits remain intact.

This is a **static quality failure until the unused import is corrected and re-reviewed**. Tests, strict repository Clippy, formatting, build, other affected reviewer verdicts, combined full-app/native checks and the full workspace gate remain separate requirements. The canonical matrix grants no adapter credit: all five original tree methods and ten original assertions remain unported/not_run. No Cargo invocation, GUI activity, Git mutation, source write or original fixture edit occurred; only this external review artifact was written.
