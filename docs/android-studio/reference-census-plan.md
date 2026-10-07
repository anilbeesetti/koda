# Reference source discovery plan

This task adds a Rust discovery command for the pinned reference archives. It
produces review candidates and unresolved runner requirements; it cannot establish
the effective runtime test census or parity completion.

Branch: `android-studio-task/1-reference-census`, based on `6ac8e50dce`.

| Task                    | Files                                                                                                       | Dependencies                                  | Acceptance                                                                                                                                                                                                                                                       |
| ----------------------- | ----------------------------------------------------------------------------------------------------------- | --------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Archive discovery       | `tooling/xtask/src/tasks/android_reference_census.rs`, command dispatch, existing locked `tar` and `flate2` | Immutable archive hashes and source revisions | Verify each archive before reading it, stream every member without extracting it, record source/runner candidates with byte hashes, reject unsafe or duplicate normalized paths, and publish deterministic output only after all sources succeed.                |
| JVM declarations        | Same Rust task, attributed bounded fixtures under `tooling/xtask/test_data/reference_census/`               | Comment/string-aware lexical scanner          | Preserve declared class/method/annotation locations, JUnit3 names, Kotlin backtick names and suite factories; record inherited, parameterized, dynamic and custom runner expansion as unresolved. Parser output is a candidate list, never a runtime case count. |
| Other runners           | Same task and external generated JSONL                                                                      | All archive members                           | Retain Python, C/C++, Rust, shell, Gradle, Bazel and other build/runner sources even when semantic discovery is unsupported. Retain fixture-like JVM sources for review rather than silently removing them.                                                      |
| Verification and review | Task report and retained external outputs                                                                   | Cargo lease and five scoped reviewers         | Meaningful parser/archive regression tests, deterministic rerun and changed-byte/hash rejection, required formatting/clippy, and each reviewer passes. The existing canonical 92 rows and 26 passing mappings remain unchanged.                                  |

The current archive mirror remains unverified against canonical IntelliJ sources.
Abstract and inherited suites require hierarchy resolution; parameter providers,
dynamic factories, nested runners, disabled tests and build-target membership
require upstream runner reconciliation. Those blockers remain explicit even when
every archive member was traversed. Deferred product features remain unported.

The command writes an isolated candidate census outside the repository initially.
Reviewed methods can later enter the canonical manifest and ledger with their
fixtures, runner evidence and individual applicability decisions. No automated
discovery label implies `not_applicable`, `ported` or `passing`.

The CLI affects developer tooling only. Full app validation is still required by
repository rules before claiming this implementation complete; the report will
distinguish unchanged product source from actual checks and any concrete blockers.
