# Evaluated tree capture and selected main roots

This Foundation slice retains the existing sync record through the Android UI
and Project handoff. It prepares roots for a selected MAIN artifact; the generic
Project Panel remains the fallback. It does not implement a visible Android tree.

The implementation starts at production baseline
`594bf18526b3d7197315c82f5c6c3860cfb7bf5a` and includes the reviewed strict-test
correction prerequisite `16c39c113523271855536b342b50edf4a7df7228` through an
ordinary merge. That prerequisite changes only the shared model test's borrowed
assertion and six redundant-clone lint fixes in project-context tests; it preserves
the production baseline and all original assertions. Its separate prior CI/strict
success does not replace checks for this combined implementation.

| Task                        | Files                                                                                                          | Dependencies                                                                                                      | Acceptance                                                                                                                                                                                         |
| --------------------------- | -------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Retain one evaluated record | `android_tools/src/evaluated_tree_inputs.rs`, `project_model.rs`, `generated_artifacts.rs`, `android_tools.rs` | Existing Basic/import/generated decoders                                                                          | Basic remains usable when sidecars are missing or invalid; exact errors and raw generated input remain available; immutable model revision is separate from selection generation.                  |
| Publish actual sync output  | `project/src/android_resources.rs`, `android_ui/src/android_ui.rs`                                             | Retained capture                                                                                                  | Existing owner/root/trust/session/cancellation checks precede publication; selection preserves capture identity; invalidation clears it; old publications cannot replace current data.             |
| Prepare selected MAIN roots | `evaluated_tree_inputs.rs`, `project_tree_adapter.rs`                                                          | Explicit supported consumer identity and current selection; imported presentation remains a separate prerequisite | V2 lists replace native MAIN generated roots; component-wise inclusive light-class filtering preserves surviving order and captured build folder; resources/assets retain their distinct outcomes. |
| Verify and review           | `android_tools/tests/evaluated_tree_inputs.rs`, additive GPUI tests                                            | Root formatting/build/test/runtime leases and five independent reviews                                            | Supplemental capture/publication/root ABA/filter regressions pass; unchanged original tests pass; real Gradle and live app validation remain required before completion.                           |

Live sync has no reviewed Rust-supported `ModelConsumerVersion` yet. It retains
the exact generated sidecar with an explicit unavailable consumer outcome.
Structural validation preserves malformed/stale errors before the unknown
consumer outcome. Preparing decoded roots requires a caller-supplied consumer identity and preserves
the captured minimum requirement; it never derives supported capability from
the producer's own minimum. Positive supplemental fixtures supply their explicit
test consumer. The importer at074 and official getter executor at63 remain
separate prerequisites for presentation/Kotlin publication; this slice supplies
no guessed names, empty getter IDs, or Disabled Kotlin state.

The generated source filter follows AOSP idea
`a84efec3ba9542d9bfa1255103f0dc94833a3796`,
`GradleProjectSystemUtil.java` lines131–181, and `FilenameConstants.kt`.
The retained Apache headers and source hashes in the dispatch preflight remain
the attribution boundary. Supplemental tests do not grant reference parity.

| Exact reference test                                           | Status and remaining acceptance                                                                                                           |
| -------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| `AndroidProjectViewTest.testGeneratedSourceFiles_lightClasses` | UNPORTED: original Gradle fixture, exact Java write, refresh/source-folder wait, and visible BuildConfig path assertions remain required. |
| `AndroidProjectViewTest.testGeneratedResources`                | UNPORTED: original build/refresh/UI workflow remains required.                                                                            |
| `AndroidProjectViewTest.testGeneratedAssets`                   | UNPORTED: original build/refresh/UI workflow remains required.                                                                            |
| `AndroidProjectViewTest.testResourcesPropertiesInAndroidView`  | UNPORTED: original fixture and visible navigation assertion remain required.                                                              |
| `AndroidProjectViewTest.testGoogleServicesJsonInAndroidView`   | UNPORTED: original fixture and visible navigation assertion remain required.                                                              |
| `AndroidSourceTypeNodeTest.testNodeFoldersOrder`               | UNPORTED: ordinary/shuffled main/androidTest/test display-order assertions remain required.                                               |

All five original tree workflows and their ten assertions remain unchanged and
unported. The only non-Rust exception used by this slice is the existing approved
Gradle/JDK/AGP getter exporter: authoritative evaluated models come from official
tooling. Retention, revision checks, filters, root preparation and tests use Rust.

No build, test, lint, live UI, or reference-parity PASS is claimed by this plan.
