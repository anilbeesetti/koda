# Draft: evaluated Android tree and GPUI integration

This is a read-only planning handoff, not implementation or parity evidence.
It follows reviewed component source `e1dce993f70969878ab40ff77b532b031e4dd8a9`
and the pending evaluated source-provider/assets task. No root/protected branch,
project model, or UI runtime has been changed for this draft. All five original
Gradle-backed tree cases remain **unported/not_run**, including their ten
original assertions.

## Inspected current boundaries

The integration checkout at `0c69111bd84f4020444b83df370141f849bd18f0` has a
physical `ProjectPanel`. Its header renders `Project`; `State`, selection,
expansion, visible rows and `OpenedEntry` carry `ProjectEntryId`. Virtual Android
groups cannot use fabricated filesystem entry IDs. The panel already builds
visible physical rows on a background executor and uses a uniform list. A new
logical-row path can reuse those scheduling/rendering conventions.

`Project::publish_android_model`, `invalidate_android_model` and
`select_android_variant` notify observers. The panel currently subscribes to
project/worktree events, so publication needs an observer or explicit model
event and a comparison of `ModelToken`. The token is root-aware and changes
through invalidation/selection; its private generation need not be exposed.
The producer can assign its own monotonically increasing projection revision,
while publication still checks the original token and current file snapshots.

`WorktreeSnapshot::entries(true, ...)` includes scanned ignored entries, but
ignored/hidden/external directory contents can remain unscanned. The adapter
must request targeted model-root scans, await their barriers and capture new
scan IDs. It cannot conclude that a generated root is absent because the normal
project tree has not expanded `build/`. Existing local methods include
`add_path_prefix_to_scan` and `refresh_entries_for_paths`; remote worktrees have
their own expansion protocol. External roots need existing worktree/project
path handling, not writes to the source project or whole-SDK scans.

`OpenedEntry` currently opens a `ProjectPath` through
`Workspace::open_path_preview`. Android leaves can resolve their physical
targets through the project/worktree path APIs; class leaves also need the
supplied byte offset applied after the buffer opens. Virtual groups toggle
expansion, and every resource variant keeps its physical target. Selecting a
resource group needs the still-pending qualified-resource selection behavior.

Project-panel settings are registered through `ProjectPanelSettingsContent`
and `ProjectPanelSettings`; `ProjectPanel::load` currently constructs a new
panel without its own node-state deserialization. Workspace persistence has
scoped key/value storage. Expansion and selection should persist `NodeKey`,
scoped to workspace/root and schema version, never snapshot-local `NodeId`.

## Metadata prerequisites

The sibling catalog shape is `Module.source_providers: Option<SourceProviderCatalog>`.
Its ordered providers have exact evaluated names and typed physical roots:
Java, Kotlin, Android resources, AIDL, RenderScript, assets, JNI libraries and
manifest. The catalog's order is **GradleSourceSetIteration**, not textual DSL
declaration order and not Android Studio's active/overlay ordering. It contains
inactive providers. `source_provider_candidates(path)` returns all configured
candidates, retaining shared/nested ambiguity. `None` means unknown metadata.
Component assets retain the existing generated flag.

The reference `AndroidViewNodes.getSourceProviders` searches current main
providers, host tests, device tests, test suites, then fixtures. A producer needs
authoritative active membership and that order before choosing the first
matching provider. A single configured candidate is not proof of active
membership. Missing or ambiguous metadata must remain explicit rather than
become an invented `main` provider or known absence. Generated roots may use
`Unattributed` only when the complete relevant provider search proves absence.

The reviewed component currently attaches one provider to each source root.
The reference performs its ordered provider search for each directory/file;
overlapping roots can therefore change the winning provider inside one outer
root. The integration must extend the projection input with authoritative
per-entry provider facts or an equivalent ordered, pure lookup. It also needs
explicit root encounter ordinals for reference-sensitive ties. The current
constant root annotation and deterministic path ordering do not establish
those behaviors. These are dependent interface changes, not changes to the
frozen component source.

Java/Kotlin shared-directory grouping additionally needs actual language
enablement and root intersections. Class names and offsets require parsed Rust
language facts; `BuildConfig.java` cannot become a `BuildConfig` class by its
filename. The current component can preserve file targets without class facts,
but that fallback cannot pass the original generated-class assertion.

Exact root-order ties, resource qualifier ordering/best-resource selection,
remaining source kinds, build scripts, dependencies and non-Android/included
modules are still missing contracts. Installing supported groups alone must
not be described as complete Android-view fidelity.

## Dependent task sequence

| Task | Files/crates | Dependencies | Acceptance and reference tests |
| --- | --- | --- | --- |
| Pin and prepare the real reference driver | Rust test support in `android_tools`, attributed loader inputs, task-local toolchain/preparation report | Exact helper/build constants, required runtime artifacts | Copy all 26 original files unchanged into a temporary project. Record every preparation change, its source and before/after hashes. Preserve the intentional sample failing unit test. Run actual Gradle import; do not substitute build-output discovery for model-generated folders. This is a prerequisite for all five tree methods, not parity credit by itself. |
| Resolve active source/provider/class facts | `android_tools/project_model`, evaluated Gradle exporter, Rust class-fact producer, projection adapter and typed `project_tree` input extension | Catalog/assets task; actual selected-variant and provider overlay facts; parsed source facts | Preserve names, physical roots, active main/host/device/suite/fixture order and explicit root encounter order. Resolve providers per directory/file, including changing winners within overlapping roots, using evaluated facts. Unknown or ambiguous input has a typed unavailable outcome. Correct Java/Kotlin intersection grouping and actual class offsets exist; add meaningful scope/ambiguity regressions. This unlocks the original model/root/class assertions. |
| Produce and publish snapshots asynchronously | New logical-tree adapter in `project` or a coherent `project_panel` component; targeted project/worktree APIs | Reviewed projection, authoritative metadata | Coalesce model/file changes, request only relevant root scans, await barriers and build immutable facts off the foreground thread. Guard publication by project identity, root, variant, `ModelToken`, file scan IDs and request generation. Discard project/variant ABA and canceled results. Retain the tagged last successful tree for a failed sync in the same project; clear it across projects. No disk I/O during render. GPUI tests cover delayed/out-of-order scans, generated files arriving, deletion, failed sync and cleanup. |
| Add selector and virtual rows | `project_panel.rs`, dedicated logical-row state/renderer, `project_panel` dependency on `android_tools`, theme/icons only as needed | Published snapshot and real availability | Render an actual Android/Project selector in the header. Enable Android only for a supported Android project with usable model facts; preserve Project access. Honor policy defaults and explicit user selection. Use uniform-list rows keyed by `NodeKey`, correct labels/gray annotations, generated groups, package compaction and literal assets. Exercise loading, empty, unavailable and error states, focus, clipping, resizing and scrolling in GPUI tests and live screenshots. Policy's supported/non-Android/default cases must also pass through the real adapter. |
| Wire file/class navigation and refresh | Logical-row actions and `ProjectPath`/workspace opening boundary; existing physical panel handlers preserved | Virtual rows, parsed class offsets and scanned physical targets | Mouse/Enter/preview/split opening reaches the actual file and class offset. Arrows expand/collapse/move selection; reveal active file selects the correct occurrence deterministically. Missing/deleted targets report the error and do not open another file. Group rows never emit fake `OpenedEntry` IDs. Resource variants remain independently navigable. Refresh and selected-variant changes update visible generated nodes. Add GPUI workflow tests and execute all five full original methods through real sync and file refresh. |
| Persist preferences and logical node state | `ProjectPanelSettingsContent`, registered settings/defaults; workspace-scoped persistence; staged policy adapter | Selector/actions and policy source | Persist explicit view choice plus expansion/selection keys independently of arena IDs. Restore across restart, prune removed keys, isolate roots/workspaces and preserve physical-panel state. Stage migration preflight, persist changed preferences before legacy cleanup, perform file operations in the background, publish one notification/event after durability, and propagate failures. Retain every original policy/migration/event assertion; add write-failure and reload integration coverage. Visibility overlays require actual symbol metadata. |

Each owner receives an isolated task branch after dependencies stabilize. Each
task needs all five area verdicts, applicable ported tests, build, formatting and
clippy before lead integration. Full-app/live and full-workspace gates remain
required; headless component tests cannot satisfy selector or workflow checks.

## Original integration acceptance

`testGeneratedSourceFiles_lightClasses` must import the prepared original
project, assert that the Android model exists, its generated-source collection
is nonempty and its `buildConfig` root exists, write the exact original Java
source there, await real refresh/source processing, and verify the class leaf.

`testGeneratedResources` and `testGeneratedAssets` must retain their entire
original inline Gradle registrations, import them, assert the corresponding
model-generated root, write the exact newline-prefixed bytes and verify the
refreshed resource/asset path. Supplying synthetic roots or only looking at
files left by a build gives no parity credit.

`testResourcesPropertiesInAndroidView` and `testGoogleServicesJsonInAndroidView`
must import the prepared original project, create the exact empty special
files, await real refresh, and verify the complete original path assertions.

The reference helper is more than a version substitution. `AGP_CURRENT` is
`AGP_LATEST`, with `compileSdk="34"`, AGP/Gradle/JDK/Kotlin resolved from their
documented providers. Pinned `SdkConstants` specifies Gradle `9.6.0` and build
tools `36.0.0`. `com.android.Version` loads generated properties; its latest AGP
also depends on `USE_ALONGSIDE_AGP` and override behavior. Resolve and record the
test-runtime flag and generated resource inputs before choosing the exact
tuple. The pinned resource generator yields `9.5.0-dev` alongside and `9.4.0`
last stable; `IS_AGP_RELEASE_BRANCH` is false. The navigator's Bazel target
supplies the locally built AGP ZIP and offline runtime repositories, so a public
artifact is not automatically equivalent. `FeatureConfiguration` is lazy and
uses `ApplicationInfo.fullVersion` or `dev` when no application exists; first
initialization and the external Studio SDK resource must be established.
The test Kotlin version is `2.4.20-RC3`, the helper's minimum SDK is `16`, and
the JDK comes from the actual configured SDK. `IdeSdks` prefers JDK 25 but that
preference alone does not prove the test's chosen JDK.

The existing AGP 9.4/platform 37 smoke is separate evidence. An explicit,
reviewed environment adaptation may preserve the original assertions, but must
state each changed runtime/version and why, without claiming it is the pinned
helper's exact environment.

Only after all ten original assertions and their real setup/refresh behavior
pass may the lead promote the five canonical rows to ported/adapted passing.
Supplemental GPUI tests cover selectors/navigation/persistence that these five
methods do not assert; further source-backed navigator cases still need their
own census and ports.
