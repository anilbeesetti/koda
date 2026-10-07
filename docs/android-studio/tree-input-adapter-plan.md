# Immutable per-module Android tree input adapter

This task starts at `ac58815bb268d338bea40a31fff630734cd719f4` on
`android-studio-task/1-tree-input-adapter`. It adds a pure Rust boundary in
`android_tools`; the project panel, Gradle bridge, scanner and canonical parity
matrix remain unchanged.

The adapter prepares supported roots from the selected evaluated
`ActiveProviderIndex`, preserves captured SDK generation flags and explicit
producer encounter order, then combines revision-bound file/presence/Java-byte
facts using `project_tree_with_facts`. Missing module presentation, required
Kotlin enablement, selected provider metadata, file capture or presence has a
typed unavailable outcome. It performs no filesystem I/O. Static roots come
from active providers, including the pinned retained host containers; generated
roots come only from actual selected component entries marked generated.

Acceptance:

- Per-provider Java/Kotlin differences and intersections follow the pinned
  AndroidSourceType subset; unknown Kotlin enablement cannot guess a shared
  root's group. Static roots under build and generated roots outside build keep
  their reported provenance. Unsupported source kinds stay explicit.
- Exact module/variant/model/file revision binding prevents mixed captures.
  Missing inventory entries remain unknown without explicit presence facts;
  directory/file/missing/unknown and root encounter coverage remain distinct.
- Actual captured Java bytes produce top-level names and original UTF-8 offsets.
  Unsupported input, missing bytes, duplicate names and budget exhaustion keep
  ordinary file presentation with per-entry diagnostics. Stale captured bytes
  reject the capture. Kotlin declarations never become Java facts.
- Original 21 tree component tests, 29 facts tests, Java parser tests, 26 sample
  files and 21 reference sources remain byte-identical. All five original tree
  methods and ten assertions remain unported/not_run; supplemental adapter tests
  grant zero canonical reference credit.
- Add meaningful boundary tests for selected membership/order, root grouping,
  presence/staleness, generation provenance, actual Java fallback/offsets,
  special files, and large inventories. Run unfiltered android_tools tests,
  strict repository clippy and formatting under Root's Cargo lease.
- Obtain five independent scoped review passes. After three fix rounds, escalate
  before further fixes. Root owns later combined full-app/live/workspace gates;
  passing pure tests does not claim a visible Android tree.

Owned files are the new `project_tree_adapter.rs`, additive module export in
`android_tools.rs`, new integration tests and scoped plan/report/evidence.
Reference adaptation retains Apache 2.0 attribution and links to existing
unchanged originals. No new non-Rust runtime or dependency is added.

Actual Kotlin capability and module display identity still need a later
source-backed evaluated producer. Generated BuildConfig roots need official
artifact-model export; filenames cannot substitute. Targeted worktree scans,
Java byte capture, stale request publication, logical rows/selector/navigation
and the exact five-case Gradle fixture driver are subsequent tasks.

The root plan records only this producer's iterator order. It does not establish
AndroidSourceTypeNode's source-provider sorted visible folder order. Likewise
raw generated component roots do not prove Studio's visible generated-root
collection: the pinned Gradle project system filters AAPT/data-binding/SafeArgs
roots represented by light classes. Both source-backed capabilities remain
explicit prerequisites; the adapter cannot claim complete generated-group
coverage or promote their reference tests.
