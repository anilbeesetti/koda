# Record pinned target and runner source evidence

`android-reference-membership` links literal module declarations in the pinned
AOSP `adt-ui/BUILD` and `project-system-gradle-sync/BUILD` to the pinned
`iml_module` producer and custom JUnit runner source. It records declared input
order and byte spans without evaluating Starlark or loading compiled classes.

```sh
cargo run --locked -p xtask -- android-reference-membership \
  --source-root tooling/xtask/test_data/reference_membership/sources \
  --selection tooling/xtask/test_data/reference_membership/selection.json \
  --output /absolute/external/path/membership.json

cargo run --locked -p xtask -- android-reference-membership \
  --source-root tooling/xtask/test_data/reference_membership/sources \
  --selection tooling/xtask/test_data/reference_membership/selection.json \
  --output /absolute/external/path/membership.json --check
```

Creation refuses an existing output. Check mode requires byte-identical evidence
and never writes. All 16 original inputs must match the immutable selection's
source revisions, retained archive identities, paths, SHA-256 hashes, lengths,
and licenses. Regular input files and directories are required; unsafe relative
paths, symlinks, duplicate identities, and exhausted budgets fail explicitly.

## Scope and limits

The bounded grammar recognizes loaded `iml_module` names and aliases plus
literal strings, nonnegative integers, booleans, `None`, lists, and string-keyed
dictionaries. It retains source attributes, exclusions, resources, dependency
order, configured suite names, JVM flags, split dictionaries, manual tags, and
shard values. Source-derived test-library, unsplit, split, and aggregate labels
remain conditional producer relationships, not observed configured targets.

Computed expressions, globs, conditionals, unverified macros, shadowed bindings,
and unsupported input types produce explicit incomplete or unresolved evidence.
Malformed lexical input and exhausted bounds return an error. Directory inputs
still require the upstream glob and configured build closure, even when every
attribute is literal. `literal_coverage_complete` describes the supported BUILD
syntax only; it never means compiled or runnable membership is complete.

Exact, hash-bound producer and runner excerpts retain classpath scanning,
include/exclude filtering, inherited finalizer handling, JUnit3 assignability,
public inherited-method discovery, ignored-runner dispatch, parameter-provider
values, and assumptions. Matching source names do not establish a class
hierarchy or expanded runtime cases. The output always reports runtime counts
as null, applicability as unresolved, global census completion as false, and
new original behavior credit as zero. The canonical 95-row parity ledger,
reference manifest, product crates, and existing census contracts are unchanged.

## Task and acceptance criteria

| Task                              | Files                                                                         | Dependencies                                                                                                                                   | Acceptance and reference evidence                                                                                                                                    |
| --------------------------------- | ----------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Verify complete input boundaries  | `reference_membership/selection.json`, `provenance.json`, complete `sources/` | Previously verified retained archive-member identities                                                                                         | Reject changed pins, paths, hashes, lengths, duplicates, symlinks, unsafe paths, and budgets; preserve all original bytes and attribution                            |
| Record literal BUILD inputs       | `android_reference_membership.rs`                                             | Complete AOSP BUILD files and macro                                                                                                            | Preserve literal values, order, aliases, exact spans, and explicit incomplete evidence for unsupported syntax                                                        |
| Link producer and runner evidence | Same Rust module                                                              | Complete `bazel.bzl`, `JarTestSuiteRunner`, `TestGroup`, `DelegatingRunnerBuilder`, `IdeaTestSuiteBase`, suite and representative test sources | Keep separate module labels, split/manual/shard facts and source runner witnesses; no compiled membership or runtime inference                                       |
| Expose deterministic CLI          | `main.rs`, `tasks.rs`, task documentation                                     | Input and evidence implementation                                                                                                              | Create/check external bounded evidence; refuse replacement; add complete-original and meaningful synthetic boundary/discovery regressions                            |
| Validate and integrate            | External validation receipts                                                  | Lead runtime lease; normal protected dependency merge if integration advances                                                                  | All five scoped area reviews, normal locked xtask tests and CLI, repository Clippy and formatting, lead full app/current full workspace CI and main/protected guards |

The discovery fixture matrix covers all 16 files listed in `selection.json`.
Every original behavior test in those files remains **unported** by this task.
Original BUILD and runner infrastructure are retained discovery evidence;
representative original tests are discovery regressions with zero behavior
credit. No original test is marked not applicable by this task.

The added Rust tests cover complete original labels and attributes, exact runner
witness spans, immutable pin changes, duplicate/missing inputs, changed source
bytes, safe paths and symlinks, computed/glob/conditional/shadowed syntax,
unverified loads, literal Unicode and order, split/manual behavior, disabled and
empty sources, split/shard conflicts, lexical/input/output bounds, deterministic
no-overwrite/check behavior, and CLI argument requirements. They supplement the
parity workflow and must pass without deleting, ignoring, or weakening tests.

Source preparation and direct formatting can run while another task holds the
shared runtime lease. Cargo tests, Clippy, complete repository formatting, the
real CLI, full app and CI validation remain pending until their actual receipts
exist. UI and UX review assess tooling behavior and report clarity; this task
claims no Android Studio app UI fidelity, startup, memory, or large-project
performance.

See [reference census limits](reference-census.md) and
[JUnit4 declaration evidence](junit4-declaration-task.md) for the independent
existing contracts and the remaining exhaustive census work.

## Checked owner component

On source `5fbebd9df3a60b6c29e09f7236c1b916fe4c4418`, all five scoped owner
review areas passed after two semantic fix rounds. The earlier failed code
reviews, exact pre-fix source, all original fixtures and all earlier test bodies
remain preserved. The normal locked xtask suite passed 136 tests, including all
22 membership regressions, with no failed, ignored or filtered tests. Normal
current-worktree CLI compilation was fresh. Strict scoped repository Clippy and
workspace Rust formatting passed; optional cargo-shear was unavailable.

Ten actual CLI cases passed their expected results. Create and check emitted
byte-identical 53,129-byte evidence. Existing-output and mismatched-check cases
preserved their files; pin, path, byte-count, duplicate, input-budget and changed
complete-source negatives rejected their inputs without producing output.
Every gate retained all 4,750 scoped source files and the full index/status.

[Portable evidence](evidence/reference-runner-membership/artifact-manifest.json)
contains complete UTF-8 payloads with original hashes and lengths, deduplicating
only byte-identical files. The frozen compiler-generated executable stays
external; its receipt records its exact hash and size. Full workspace CI,
current protected dependency integration and lead app/integration gates remain
pending. These component checks give zero original behavior, runtime membership
or Android Studio UI fidelity credit.
