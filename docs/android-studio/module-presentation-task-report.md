# Captured module display policy: verified component

Source commit `5e4b752a057bff25d9e281a43ce4b71a8598406f` adds a pure Rust display policy and
20 supplemental source-derived tests. It preserves imported internal, holder,
display and sort identities, Gradle awareness, raw path equality, null-only
fallback, source-set precedence and empty suffixes. Eight pinned originals
retain their exact bytes and Apache 2.0 attribution. No dependency, new non-Rust
runtime, getter producer, public-model field or adapter change is added.

All five independent reviewers passed both static and source-bound runtime
review. There were no blocking findings and no product fix rounds.

| Actual bounded check | Result |
| --- | --- |
| Normal locked `android_tools` tests, all features | 221 passed, 0 failed/ignored/filtered; 20 new supplemental cases |
| Normal locked all-features library build | Passed |
| Repository `./script/clippy --locked -p android_tools` | Passed; release, all targets/features, warnings denied |
| `cargo fmt --all -- --check` | Passed |
| Current-worktree compiler proof | Seven fresh test artifacts with exact source paths and at-run hashes |
| Root's exact seven source-guard patterns | All 4,717 source bindings and Git index/status unchanged during execution; nested port evidence visible |
| Original-source preservation | 159 tracked baseline paths preserved except one additive module export |
| Process cleanup | Task checks and exact measurement child exited; no Gradle/app launched; lease released |

The new frozen policy-test ELF is 1,642,216 bytes with SHA-256
`6dcbba6ac91c0318aaa8cc7a459fd599a1d61eacbdd4ba4444b50b133f4726e4`. Its current exact identity is recorded in
`evidence/module-presentation/frozen-policy-executable.json`. Other shared-target
artifact hashes are historical identities observed at execution time; they
must not be assumed current after later tasks rebuild the shared cache.

One exact existing long-name test exercised a 327,680-byte imported name and a
327,692-byte external ID. Its actual child process measured 1.817 ms elapsed and
11,008 KiB peak RSS through `posix_spawn`/`wait4`, including process startup,
allocation, assertions and output. That intentional exact-test measurement has
one passing case and 19 filtered cases; the normal 221-test run has zero filtered
cases. This is borrowing coverage and a bounded test-process measurement, not
an adversarial delimiter scan, allocation count, memory ceiling, app startup,
hardware, large-project or before/after performance result. Exact external
validation runner source text and hashes are retained as audit metadata.

Raw logs and all ten independent review reports are retained byte-exact in
`evidence/module-presentation`; `manifest.json` binds portable paths and hashes.
The explicitly authorized superseded generated unit-ELF retirement is recorded
with its prior Root 229-test compiler identity, hash/size/nlink and observed
process/FD checks. Two inaccessible system executable links are listed rather
than treated as inspected. No source, fixture, raw log or frozen binary was
retired. The previous execution receipts are not rewritten.

The immutable `test_data/module_presentation/selection.json` records the initial
source inventory before execution. Actual later execution status is in this
report, the scoped port metadata and source-bound raw receipts. Original test
ports and new canonical parity credit remain **zero**; the global matrix is
unchanged. All five original generated-tree methods remain unported/not_run.

Authoritative import/getter transport, qualified/internal-name construction,
source-set/group publication, Kotlin facet/capability import and live tree UI
remain separate tasks. Empty-label policy is implemented here; the current
adapter's rejection remains specifically applicable unported behavior requiring
an intentional adapter extension and edge tests. There is no silent fallback or
N/A classification for it.

Root still owns combined full-app build, native regression, full-workspace CI,
conflict/main verification and protected merge. This bounded component result
is ready for lead integration; it does not establish full task integration,
Android tree UI, phase or IDE completion.
