# Android logical tree component plan

This task prepares a Rust projection component on
`android-studio-task/1-project-tree`, based on `f8114bfafa`. It does not install
the Android view in the app or complete any of the five remaining upstream
`AndroidProjectViewTest` cases.

## Reviewed reference and integration boundaries

The five cases and recursive `getAllNodes` helper are read in full. All 26
`SIMPLE_APPLICATION` fixture files were read and verified against their recorded
SHA256 values. The pinned project/module/default-provider/source/resource nodes
define virtual groups, generated suffixes, source-provider annotations,
compacted directories, resource variants, and file targets. Original sources,
fixture bytes, and Apache notices remain unchanged.

The existing model has Java, Kotlin, resources, and manifest roots. It lacks
assets roots and source-provider names. Its component name/scope does not
identify every source set within that component. Inferring `main` from an
arbitrary root path would give incorrect results for custom roots. The minimal
Gradle bridge currently omits `component.sources.assets`.

The `project_panel` stores filesystem `ProjectEntryId` rows; opening a row emits
`OpenedEntry`, and visible-entry rebuilding already runs on a background
executor. Virtual Android groups require their own stable keys and a mapping
from physical file targets to worktree entries. The model is published through
`project::android_resources`; a later adapter must reject obsolete model/file
snapshots and preserve the last successful tree during failed syncs.

Reference test strings differ from visual labels. Android modules add
`(Android)` only in `toTestString`; compacted package directory test strings use
the final directory name. Java class nodes depend on class facts, not the file
stem. The component will keep those distinctions explicit.

## Bounded tasks

| Task                     | Files                                                                                                                                                                        | Dependencies                                                                                        | Acceptance                                                                                                                                                                                                                                                                           |
| ------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Immutable tree data      | New `crates/android_tools/src/project_tree.rs`; crate-root export                                                                                                            | Reviewed source contracts                                                                           | Typed virtual node kinds, stable module/source/file keys, separate presentation annotations and reference test strings, physical navigation targets; no filesystem or GPUI side effects                                                                                              |
| Projection               | Same module                                                                                                                                                                  | Immutable module/source-root/file snapshot; explicit source-provider and class facts when available | Java/generated groups, resource type and duplicate-name grouping, literal asset hierarchy, manifest group, module `google-services.json`, `resources.properties` main-provider handling, deterministic ordering and duplicate-root/file handling; preserve each physical target      |
| Component verification   | New `crates/android_tools/tests/project_tree_component.rs`; attributed fixtures and provenance under `test_data/project_tree`; task report under `docs/android-studio/ports` | Original 26 fixture files and selected production sources                                           | Rust tests exercise projection against real fixture file inventories and explicit typed metadata; cover qualifier groups, navigation targets, stale/missing metadata rejection, overlapping roots, empty roots, input-order stability, and deep paths without recursive stack growth |
| Review and source freeze | Task report and captured evidence                                                                                                                                            | Focused tests, clippy, formatting, five independent reviewers                                       | All scoped reviewers pass; freeze source before exact component-test captures; app/build/live and workspace checks remain lead merge gates                                                                                                                                           |

The projection consumes immutable data rather than walking disk or asking
Gradle during rendering. The future producer selects the current variant and
supplies actual source roots, provider names, class facts, and physical file
entries. Missing required metadata is explicit; it must not be invented to
manufacture a passing reference case. A model adapter may initially cover only
the metadata that the current `ProjectModel` actually provides.

The component must avoid an all-files-by-all-roots scan and preserve stable
node identities across input reordering. Sorting/index construction is allowed;
per-frame disk I/O and repeated whole-project rebuilding are not.

## Upstream test completion dependencies

| Reference case                          | Required original assertions                                                                                                                          | Remaining integration prerequisites                                                                                                                                                         |
| --------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `testGeneratedSourceFiles_lightClasses` | Android model exists; generated source folders are nonempty; a model-provided `buildConfig` folder exists; exact generated class leaf path is present | Real pinned-fixture Gradle import, generated-root export, file refresh, and Rust class/package presentation facts; a synthetic folder or filename-derived class name gives no parity credit |
| `testGeneratedResources`                | The model supplies a `my_generated_resources` folder; exact generated raw-resource leaf path is present                                               | Preserve the original inline Gradle producer registration, real sync, model root assertion, exact written file bytes, and refreshed tree                                                    |
| `testGeneratedAssets`                   | The model supplies a `createAssets` folder; exact generated asset leaf path is present                                                                | Asset source kind and bridge export, original producer registration, real model root assertion, exact written file bytes, and refreshed literal asset hierarchy                             |
| `testResourcesPropertiesInAndroidView`  | Exact `res/resources.properties (main)` leaf path is present                                                                                          | Real fixture import, source-provider metadata, exact empty added file, file refresh, and tree integration                                                                                   |
| `testGoogleServicesJsonInAndroidView`   | Exact module-level `google-services.json` leaf path is present                                                                                        | Real fixture import, exact empty added file, file refresh, and tree integration                                                                                                             |

All five canonical rows stay **unported** for this component-only task. Its
reference-shaped component regressions are supplemental evidence, not a
replacement for Gradle import or any model/generated-folder assertion.

The fixture loader itself adapts the stored AGP 1.5 configuration to the pinned
`AGP_CURRENT` environment through `AndroidGradleTests.defaultPatchPreparedProject`.
A faithful Rust integration driver must inspect and record those preparation
steps before modifying a temporary copy. The original fixture includes an
intentional failing sample unit test; tree/sync checks must retain its bytes and
must not claim that running that sample unit-test task passes.

## Sequential product wiring

After model metadata and this component stabilize, a separate task adds the
Android/filesystem selector, background snapshot production and stale-response
guards, expansion/selection persistence, keyboard navigation, file opening,
settings durability, and GPUI notification delivery. It must exercise real
Gradle-generated roots and every original assertion, then add live screenshots
and relevant GPUI tests. This component alone establishes neither Android
Studio visual fidelity nor complete project-view workflows.
