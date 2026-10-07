# Android Studio implementation plan

Build an Android development IDE in Rust on Koda's Zed foundation. The five
phases below remain the completion target. Existing Android code is reusable
but does not count as reference parity until mapped Rust tests pass. No feature
implementation is approved for merge by this planning document alone.

## Accepted scope

- Repository: `anilbeesetti/koda`.
- Integration branch: `android-studio`. Preserve `main` and its history.
- Initial snapshot: `30fca99a7a015168dfe4f394f576fbe2955fb4ea`, the observed
  `main` head at planning time. Revalidate the remote before pushing.
- Task branches: `android-studio-task/<phase>-<task>`. Git cannot store both
  `refs/heads/android-studio` and descendants under that same ref name.
- Reference: stable Android Studio Rabbit 1, tag `studio-2026.2.1`.
- Official JVM layout and Compose rendering/compiler runtimes are allowed
  exceptions. Language servers and IDE logic must be Rust.
- NDK/C++, device-specific designers, hosted Google integrations, and IntelliJ
  Marketplace compatibility are deferred. Their applicable tests stay unported;
  deferral is never a reason to mark a test not applicable.

## Feature inventory

Every feature in a planned row is planned. This inventory is a product scope,
not a claim that all upstream test suites have been enumerated. Discoveries in
the pinned sources extend it explicitly rather than silently narrowing parity.

| Phase | Feature group          | Features                                                                                                                           | Decision                                 |
| ----- | ---------------------- | ---------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------- |
| 1     | Entry workflows        | Welcome screen, recent projects, open/import/close/reopen, new project and module templates                                        | Planned                                  |
| 1     | Project structure      | Android modules, application/library/test modules, source roots, generated sources, project structure dialog                       | Planned                                  |
| 1     | Gradle sync            | Wrapper/JDK selection, progress, cancellation, diagnostics, offline mode, last successful model, resync                            | Planned                                  |
| 1     | Build model            | Groovy/Kotlin DSL evaluation through Gradle, AGP models, variants, flavors, source sets, dependencies, version catalogs            | Planned                                  |
| 1     | Advanced model         | Included/composite builds, multi-module projects, dynamic features, Kotlin multiplatform Android targets, custom project locations | Planned                                  |
| 1     | Project views          | Android and filesystem views, package compaction, resource qualifiers, generated files, navigation, excluded roots                 | Planned                                  |
| 1     | Window shell           | Main toolbar, project/run/device selectors, menus, editor tabs, split editors, breadcrumbs, status bar                             | Planned                                  |
| 1     | Tool windows           | Left/right/bottom rails, pin/hide/show, docking, resizing, focus, saved layout, empty/error/loading states                         | Planned                                  |
| 2     | Kotlin language        | Parsing, highlighting, type analysis, completion, diagnostics, imports, formatting, Kotlin/Java interoperability, SDK symbols      | Planned in Rust                          |
| 2     | Java language          | Parsing, highlighting, type analysis, generics, completion, diagnostics, imports, formatting, SDK symbols                          | Planned in Rust                          |
| 2     | XML language           | Namespace/schema validation, completion, formatting, layout/manifest/resource semantics, qualifiers                                | Planned in Rust                          |
| 2     | Navigation             | Definition/type/implementation, references, usages, symbols, structure view, related resources, generated R symbols                | Planned                                  |
| 2     | Refactoring            | Safe rename, move, extract/inline, signature changes, usages preview, conflict reporting, undo                                     | Planned                                  |
| 2     | Editor actions         | Intentions, quick fixes, inspections, code folding, parameter info, documentation, inlay hints, smart selection                    | Planned                                  |
| 2     | Search                 | Search Everywhere, files/classes/symbols/actions, project text search, scope filters                                               | Planned                                  |
| 3     | Build tasks            | Assemble/clean/test/lint, task discovery, build tree, output and error navigation, cancellation                                    | Planned                                  |
| 3     | Packaging              | APK/AAB generation, signing configuration, secrets handling, output selection, install variants                                    | Planned                                  |
| 3     | SDK manager            | Package metadata, platforms/tools/system images, download/install/update/remove, license interaction                               | Planned with SDK exception               |
| 3     | JDK selection          | Compatible JDK discovery, project selection, errors, Gradle JVM integration                                                        | Planned with JDK exception               |
| 3     | AVD manager            | Create/edit/delete AVDs, image selection, launch/stop, cold boot/wipe workflows                                                    | Planned with emulator exception          |
| 3     | Device discovery       | ADB tracking, USB and wireless devices, authorization/offline states, reconnect                                                    | Planned with ADB exception               |
| 3     | Deployment             | Run configurations, activity/service selection, APK selection/install, launch/stop, failure recovery                               | Planned                                  |
| 3     | Test runner            | JVM unit tests, instrumentation tests, results tree, rerun failures, stack navigation, coverage display                            | Planned                                  |
| 3     | Debugger               | Rust JDWP client/DAP adapter, breakpoints, stepping, variables, evaluation, threads, attach, Kotlin source maps                    | Planned                                  |
| 3     | Logcat                 | Stream, structured parsing, query/filter language, colors, pause/clear/export, hyperlinks, reconnect                               | Planned                                  |
| 3     | Device explorer        | Browse/pull/push files, permissions, progress, errors, cancellation                                                                | Planned                                  |
| 4     | XML layout design      | Code/design/split, palette, hierarchy, constraints, properties, configurations, undo                                               | Planned with layoutlib exception         |
| 4     | Previews               | XML and Compose render, device/API/theme/locale, refresh, unsaved edits, source navigation, accessibility checks                   | Planned with JVM render exception        |
| 4     | Resource manager       | Drawables, colors, dimensions, strings, qualifiers, asset/vector import, references, translations                                  | Planned                                  |
| 4     | Manifest editor        | XML semantics, merged manifest, source attribution, conflicts, component and permission helpers                                    | Planned                                  |
| 4     | Lint                   | Rust rules, diagnostics, fixes, suppressions, baselines, Gradle report import, SDK/variant awareness                               | Planned in Rust                          |
| 4     | APK analyzer           | ZIP/APK/AAB/DEX, resources and manifests, size/compression breakdown, comparisons, source links                                    | Planned in Rust                          |
| 4     | Profiler               | Capture control, CPU/memory/network timelines, traces, heap analysis, import/export                                                | Planned with platform capture exceptions |
| 4     | Runtime inspection     | Layout inspector, database inspector, process selection, runtime transport, inspection UI                                          | Planned with device agent exceptions     |
| 4     | Fast iteration         | Apply Changes and Compose Live Edit, compatibility checks, restart fallback, diagnostics                                           | Planned; feasibility spike first         |
| 4     | Upgrade tools          | AGP/SDK upgrade guidance, structured edits, previews, validation, rollback                                                         | Planned                                  |
| 5     | Keymaps                | Android Studio platform shortcuts, actions, conflicts, customization, Vim coexistence                                              | Planned                                  |
| 5     | Appearance             | Light/dark themes, semantic colors, icons, density, fonts, scaling, high contrast                                                  | Planned                                  |
| 5     | Settings               | Searchable settings, project/global scopes, persistence, import/export, compatibility migration                                    | Planned                                  |
| 5     | Accessibility          | Keyboard-only workflows, focus order, text scaling, contrast, supported host assistive APIs                                        | Planned                                  |
| 5     | Version control        | Git change lists, diffs, commit/branch/merge workflows, local history                                                              | Planned                                  |
| 5     | Plugins                | Rust/WASM extension hooks, capability boundaries, versioning, Android extension APIs                                               | Planned                                  |
| 5     | Performance            | Startup, indexing, memory, UI latency, large projects, bounded caches, background cancellation                                     | Planned throughout                       |
| Later | Native development     | NDK/C++, CMake tooling, native LLDB debugging                                                                                      | Deferred; applicable tests unported      |
| Later | Form factors           | Wear/TV/Automotive/XR-specific designers and remote device mirroring                                                               | Deferred; applicable tests unported      |
| Later | Hosted integrations    | Gemini, Firebase/Play consoles, App Quality Insights, remote device services                                                       | Deferred; applicable tests unported      |
| Later | IntelliJ compatibility | IntelliJ Marketplace binary plugins and JVM platform compatibility                                                                 | Deferred; applicable tests unported      |

## Rust ownership and external exceptions

The IDE UI, project data structures, process orchestration, editor features,
language servers, indexing, JDWP/DAP client, Logcat, analysis, and settings are
Rust. Parse static Gradle text only for hints; real build evaluation remains
Gradle's job because scripts execute arbitrary JVM code.

| Component                                     | Reason an external component is required                                                                             | Boundary                                                                                                       |
| --------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| Gradle and Android Gradle Plugin              | Canonical project evaluation and Android build behavior; replacing them changes compatibility with existing projects | Rust process/model bridge; minimal JVM Tooling API adapter only when required                                  |
| JDK and Kotlin/Java build compilers           | Execute Gradle, compile JVM/Dex inputs, and execute approved rendering runtime                                       | Build/render execution only; not an IDE language server                                                        |
| Android SDK and build tools                   | Canonical platform artifacts, packaging, signing, D8/R8/AAPT2 and SDK distribution compatibility                     | Rust management, parsing and orchestration                                                                     |
| ADB                                           | Official transport and device compatibility                                                                          | Rust device state, commands and UI; evaluate a Rust transport where practical                                  |
| Android emulator and system images            | Android OS/device emulation has no practical equivalent Rust implementation                                          | Rust AVD model, management and launch                                                                          |
| layoutlib and Compose compiler/render runtime | Faithful Android/Compose rendering depends on platform/JVM execution; explicitly approved                            | Rust editor/UI/cache/process control, isolated render bridge                                                   |
| Device-side profiling and inspection agents   | Platform hooks and JVMTI/runtime access must execute inside Android's supported runtime                              | Rust capture control, transport, parsing and presentation; each bundled agent needs a license/provenance entry |
| Host OS APIs and graphics drivers             | Native windows, input, accessibility and graphics depend on operating-system/driver interfaces                       | GPUI/Rust bindings; retain required system ABI dependencies                                                    |

Existing Kotlin language servers, JDT-style Java servers, JVM debug adapters,
and Java lint engines are not accepted IDE exceptions. Replace their behavior in
Rust. Inventory inherited C/C++ dependencies (parsers, SQLite, Git/SSH, media and
compression libraries): use Rust alternatives where practical, and justify each
retained dependency individually. This plan does not grant blanket exemptions
to inherited native libraries. SDK/emulator binaries and fixtures may have
licenses other than Apache 2.0; audit artifacts individually.

## Reference revisions and licensing

| Source                  | Immutable revision                         | Evidence                                                                                            |
| ----------------------- | ------------------------------------------ | --------------------------------------------------------------------------------------------------- |
| AOSP tools/adt/idea     | `a84efec3ba9542d9bfa1255103f0dc94833a3796` | `studio-2026.2.1`; studio/version.bzl declares Rabbit 1 and stable                                  |
| AOSP tools/base         | `4a5d2ec9571e2021fc9a4621e24a195f966ee6dd` | Matching `studio-2026.2.1` tag                                                                      |
| AOSP tools/idea         | `1141b43139d42a022d3b9f6d15e9d0b445bd302f` | Matching tag for IntelliJ platform behavior                                                         |
| IntelliJ Android plugin | `132bc7c3cf52598117590637d00e81b929444bde` | Matching `idea/262.9437.185` tag in JetBrains/android; GitHub mirror discloses size-limit omissions |
| IntelliJ Community      | `b75ab523e6adbe1d26112219729eacbcfd24daa0` | Matching `idea/262.9437.185` tag                                                                    |

Canonical release manifest:
<https://android.googlesource.com/platform/manifest/+/refs/tags/studio-2026.2.1/default.xml>.
Do not substitute a current JetBrains master snapshot for the matching release.
The plugin pin matches the platform build family. GradleModuleImportTest.java
is byte-identical between the Android Studio and JetBrains plugin pins, which
does not prove equivalence of their entire trees. Reconcile missing canonical
mirror coverage before claiming exhaustive parity across all requested sources.

Preserve Koda/Zed's existing GPL and other crate licenses. For every adapted
reference file or fixture, retain its upstream copyright and license, original
path, source revision, transformation description and content checksum. Apache
2.0 reference material does not change the fork's overall license. Check NOTICE
requirements and fixture-specific licenses before redistribution.

## Test parity contract

Create a versioned matrix with one entry per reference test and parameterized
case. Record source/revision, suite/class, method/case, source location, fixture
paths and checksums, feature/task, applicability reason, Rust test identifiers,
adaptation rationale, and run evidence tied to the tested commit.

Keep two independent fields:

- Port status: `unported`, `ported`, `adapted`, or `not_applicable`.
- Run status: `not_run`, `passing`, `failing`, or `blocked`.

Unported is necessary to describe unfinished work honestly. A copied test is
not passing unless it ran. A deferred feature is unported, not inapplicable.
Platform-internal tests may be inapplicable only with a specific rationale that
their observable behavior has no counterpart. LSP tests, completion, refactoring,
navigation and Android workflows remain applicable even when implemented
differently. An incorrect upstream test needs a documented explanation and
review, never deletion or weakened assertions.

Full census must include JUnit3, JUnit4/5, Kotlin/backtick test names, inherited
methods, nested suites, parameter providers, dynamic factories, golden/snapshot
tests, native C++/gtest, Python/shell/Bazel test targets and Gradle integration
fixtures. Reconcile source discovery with upstream
build/test discovery. A regex list or truncated GitHub tree is not exhaustive.
Unresolved test discovery blocks an exhaustive-parity claim. Existing local
`#[test]` counts alone do not establish mappings.

Every applicable upstream assertion must have a Rust counterpart. Adapt
platform fixtures with a recorded reason while preserving observable semantics
and edge cases. Store binary fixture licensing separately. Keep ignored,
quarantined and upstream-disabled tests visible; never introduce skips to pass.

## Tasks and acceptance

Each row is a task branch. Large test families must be subdivided into bounded
case lists once census is complete; broad family names below are discovery
targets, not invented test identifiers. Every task gets a frozen matrix slice
before implementation. Common acceptance: task tests pass, integration build
and full suite pass, live app behavior is exercised where affected, and all five
reviewers pass. A task blocked by discovery or environment is not complete.

### Foundation

| Task               | Files or crates                                                                                             | Dependencies                                              | Acceptance                                                                                                                                                 | Reference tests to port                                                                                                                                                                                                                                                                                                                                                                                        |
| ------------------ | ----------------------------------------------------------------------------------------------------------- | --------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1-reference-census | Proposed Rust parity tooling under tooling, docs/android-studio, reference fixture provenance               | Immutable refs, matching plugin source, complete archives | Full source/test discovery reconciled, one entry per case, validation rejects missing mappings/evidence                                                    | Discovery checks against pinned GradleModuleImportTest, AndroidProjectViewTest and lint/build test families; census correctness tests                                                                                                                                                                                                                                                                          |
| 1-build-baseline   | Existing build/CI configuration and external artifacts; no source changes unless a specific fix is required | Rust/native prerequisites, network/artifact access        | Baseline app builds/runs, full suite result known, Android SDK/JDK/device capability recorded                                                              | Existing full Koda suite; inherited failures recorded and fixed without skips                                                                                                                                                                                                                                                                                                                                  |
| 1-gradle-model     | crates/android_tools/src/project_model.rs and project_model.gradle, crates/project/src/project.rs           | Census, baseline                                          | Preserve all modules/variants/source roots/dependencies; custom locations, composites and unsupported modules either supported or explicit blocking errors | GradleModuleImportTest: testImportSimpleGradleProject, testImportSubprojects, testImportSubProjectsWithMissingSubModule, testImportSubProjectWithCustomLocation, testRequiredProjects, testMissingRequiredProjects, testMissingEnclosingProject, testTransitiveDependencies, testCircularDependencies; matching tools/base model tests                                                                         |
| 1-sync-lifecycle   | android_tools project model/process code, android_ui sync state, project integration                        | Gradle model                                              | Progress/cancel/retry, stale response rejection, last good model, errors navigable, no UI blocking                                                         | Pinned Gradle project sync/error/cancellation test families; exact census cases required                                                                                                                                                                                                                                                                                                                       |
| 1-project-view     | crates/project_panel, project Android model APIs, file icons                                                | Gradle model                                              | Android/filesystem switching, package/resource grouping, generated files, source navigation and focus                                                      | AndroidProjectViewTest: testGeneratedSourceFiles_lightClasses, testGeneratedResources, testGeneratedAssets, testShowVisibilityIconsWhenOptionIsSelected, testShowVisibilityIconsWhenOptionIsUnselected, testResourcesPropertiesInAndroidView, testGoogleServicesJsonInAndroidView, testAndroidViewIsDefault, testAndroidViewNotVisibleInUnsupportedProjectSystem, testAndroidViewNotVisibleInNonAndroidProject |
| 1-window-shell     | workspace/dock/pane/status_bar, title_bar, menus, theme/assets, android_ui                                  | Baseline; shares no model-edit files                      | Android Studio reference screenshots at fixed DPI, dock/resize/focus persistence, tabs/splits, loading/empty/error screens                                 | Pinned tool-window/action/project-view cases with equivalent behavior; add Rust GPUI snapshots for visual differences                                                                                                                                                                                                                                                                                          |
| 1-project-creation | android_tools template generation, android_ui dialogs, recent_projects                                      | Model, sync, shell                                        | Generated fixture builds/syncs; validated fields; cancel leaves no partial project; project structure edits previewable                                    | Pinned project/module wizard and template tests; retain fixture licenses                                                                                                                                                                                                                                                                                                                                       |

Model and shell tasks can run independently after prerequisites. Project view
and creation wait for stable model interfaces. Shared workspace wiring is a
separate sequential integration change rather than conflicting owner edits.

### Editing

| Task                      | Files or crates                                                                        | Dependencies                                   | Acceptance                                                                                              | Reference tests to port                                                                                                           |
| ------------------------- | -------------------------------------------------------------------------------------- | ---------------------------------------------- | ------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| 2-language-infrastructure | Proposed Rust analysis/index/LSP crates, language_core/lsp, project/lsp_store.rs       | Phase 1                                        | LSP framing/cancellation, UTF-16 edits, incremental index, classpath/SDK symbols, bounded caches        | Pinned Android code-insight fixture discovery and protocol adaptations                                                            |
| 2-java-analysis           | Proposed Rust Java parser/type/index/server crate, languages/src/java.rs               | Language infrastructure                        | Java parse/type/diagnostics/completion on frozen corpus; error recovery; Kotlin interoperability hooks  | Applicable Android Java completion/inspection/navigation tests plus required semantic fixtures                                    |
| 2-kotlin-analysis         | Proposed Rust Kotlin parser/type/index/server crate, languages/src/kotlin.rs           | Language infrastructure; Java symbol interface | Kotlin scopes/types/generics/nullability/overloads/extensions/interoperability; incremental changes     | Applicable Kotlin Android code-insight/navigation/completion tests; missing compiler/plugin reference coverage explicitly tracked |
| 2-xml-analysis            | Proposed Rust XML/Android resource server, project/src/android_resources.rs, languages | Language infrastructure; resource model        | Namespace/schema, variants/qualifiers, resource references, manifests/layouts, malformed input recovery | Pinned XML resource completion/validation/navigation tests                                                                        |
| 2-editing-workflows       | editor, project LSP integration, structure/search/navigation UI                        | Java/Kotlin/XML servers                        | Completion/docs/diagnostics/usages/formatting/inlay hints work in live app                              | Corresponding frozen upstream code-insight tests and GPUI workflows                                                               |
| 2-refactoring             | Rust analysis crates, editor/workspace transactional edits                             | Editing workflows                              | Rename/move/extract/signature cases preserve semantics, preview conflicts, undo atomically              | Applicable upstream refactoring tests and their original fixtures                                                                 |

Java, Kotlin and XML tasks can overlap after a shared symbol/model contract.
Do not represent a lightweight parser as a full compiler-semantic replacement.

### Build and run

| Task              | Files or crates                                                                          | Dependencies                      | Acceptance                                                                                      | Reference tests to port                                                                              |
| ----------------- | ---------------------------------------------------------------------------------------- | --------------------------------- | ----------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| 3-build-output    | android_tools Gradle orchestration, android_ui/src/android_build.rs                      | Phases 1 and 2                    | Real fixture assemble/test/lint, diagnostics links, stream/cancel, child cleanup                | Pinned build output, task execution and error parser cases                                           |
| 3-sdk-avd         | android_tools SDK/AVD management, android_ui dialogs                                     | Build model; SDK distribution     | Install/remove package flow and AVD CRUD, compatibility checks, errors/cancel                   | Pinned sdklib/repository/AVD and SDK manager tests                                                   |
| 3-adb-devices     | android_tools device transport/discovery, android_ui status                              | SDK/ADB availability              | Device states/reconnect/wireless selection, emulator launch/stop, device explorer               | Pinned ddmlib/device manager/file explorer tests                                                     |
| 3-run-test-deploy | android_tools install/run/APK selection, android_ui run configuration                    | Build/devices                     | Build and run on actual emulator/device, unit/instrumentation results and rerun                 | Pinned run configuration/deployment/test runner cases                                                |
| 3-logcat          | android_ui/src/android_logcat*.rs, android_tools stream                                  | Devices                           | Query syntax and malformed records, pause/clear/export, bounded buffer, reconnect, source links | Pinned Logcat parser/filter/query/view test families                                                 |
| 3-rust-debugger   | Proposed Rust JDWP backend, dap_adapters/debugger_ui, android_ui/src/android_debugger.rs | Rust language/source maps, deploy | Launch/attach, breakpoint/step/evaluate/thread flows on real app, detach cleanup                | Applicable Android debugger/source-position/JDWP cases; adapter platform internals mapped explicitly |

### Android tooling

| Task                      | Files or crates                                                                                         | Dependencies                 | Acceptance                                                                                                          | Reference tests to port                                        |
| ------------------------- | ------------------------------------------------------------------------------------------------------- | ---------------------------- | ------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------- |
| 4-render-runtime          | android_tools preview_bundle*.rs/preview.rs, licensed runtime bridge, android_ui/src/android_preview.rs | Phases 1 to 3                | Reproducible runtime, verified provenance, cancellation/isolation, XML and Compose configurations, artifact cleanup | Pinned layoutlib/rendering/Compose preview cases               |
| 4-layout-editor           | android_ui design/property/hierarchy views, editor transactions                                         | Rendering/XML model          | Design/code/split, palette/properties/constraints/undo, reference visual and interaction checks                     | Applicable layout designer and property tests                  |
| 4-resources-manifest      | android_tools resource/manifest models, project Android resources, android_ui tools                     | XML analysis; model          | Qualifiers, assets/translations, merged manifest and source origins                                                 | Pinned resource repository/qualifier/manifest merge tests      |
| 4-rust-lint               | Proposed Rust lint crate, diagnostics, android_ui problems                                              | Java/Kotlin/XML semantics    | Rule-by-rule exact diagnostics/fixes/suppressions/baselines                                                         | Every applicable pinned tools/base lint detector test and case |
| 4-apk-analyzer            | Proposed Rust APK/DEX/resources analysis, android_ui analyzer                                           | Build packaging              | Real malformed/valid fixture parsing, sizes, compare, bounded memory                                                | Pinned APK analyzer/DEX/resource tests                         |
| 4-profiler                | Proposed Rust trace/heap/transport crates, android_ui profiler                                          | Device runtime and capture   | CPU/memory/network captures and imports, cleanup, real-device validation                                            | Pinned profiler/transport/trace/heap tests                     |
| 4-runtime-inspection      | Rust inspector models/transport/UI, attributed device agents                                            | Devices, XML/Compose/runtime | Layout/database inspections, disconnections, permissions, version compatibility                                     | Pinned layout/database inspector tests                         |
| 4-fast-iteration-upgrades | Rust compatibility/patch/upgrade model and UI                                                           | Build/debug/render tooling   | Spike first; supported patch flow, restart fallback, preview/rollback of upgrades                                   | Pinned deploy/Live Edit/AGP upgrade tests                      |

### Polish

| Task                       | Files or crates                                                           | Dependencies                       | Acceptance                                                                                               | Reference tests to port                                                          |
| -------------------------- | ------------------------------------------------------------------------- | ---------------------------------- | -------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| 5-keymaps-settings-themes  | assets keymaps/themes, settings/UI, actions                               | Phase 4                            | Platform shortcuts, conflicts, persistence, search/settings migration, light/dark screenshots            | Corresponding upstream action/settings/theme behavior tests                      |
| 5-accessibility            | GPUI host adapters, UI/editor/workspace                                   | Complete UI flows                  | Keyboard-only end-to-end flow, focus/order/contrast/scaling, supported assistive API exercise            | Applicable accessibility/navigation tests                                        |
| 5-vcs-history-plugins      | git/git_ui, workspace history, extension_api/extension_host               | Stable Android APIs                | Git workflows/local history, Rust/WASM plugin examples and compatibility                                 | Applicable VCS/history/plugin extension tests; JVM binary compatibility deferred |
| 5-performance-native-audit | Existing benchmark crates, Rust analysis/cache code, dependency manifests | All phases; measures start earlier | Measured startup/memory/latency/large-project runs, agreed budgets, retained native exceptions justified | Applicable upstream performance/scalability cases and repeatable Rust benchmarks |
| 5-final-parity             | Matrix tooling, CI, end-to-end fixture                                    | All tasks                          | All applicable target tests mapped/passing, unresolved discovery zero, app done flow verified            | Full pinned reference matrix plus complete existing fork suite                   |

## Dispatch review and merge

Use isolated worktrees and one branch per task. Independent owners may run in
parallel; dependencies and shared integration edits run sequentially. Limit
concurrent Cargo builds to the available CPU/memory and share stable build
settings. Never merge directly between active owners.

Every owner launches five independent reviewer subagents: code, UI, UX,
performance, and code quality. Each returns pass/fail with precise findings and
evidence. An unaffected area may pass only with an explicit reason grounded in
the actual diff; absent live evidence is a blocked review for affected UI/UX.
Fix all findings and rerun affected reviews. After three unsuccessful fix rounds,
escalate with the exact blockers and leave the branch unmerged.

The lead verifies the tested commit, five verdicts, required ported tests, full
app build, full existing suite, fixture/provenance changes and unchanged `main`.
Resolve conflicts on the task branch, rebuild and rerun affected checks before
merge. Fast-forward or non-rewriting merge into `android-studio`; never force
push either protected branch. Push explicitly to `refs/heads/android-studio`
only after remote head revalidation. Build or test failures block merges.
Authentication and network failures block remote verification and publishing;
they do not prevent local preparation. The current user instruction holds all
GitHub writes. Any local merge still requires every build, test, and review gate.

Report after each completed phase: merged commits, matrix totals by port and run
status, failing/blocked/unported tests, five review evidence, actual app checks,
exceptions, deferred work and next dependencies. Never call an unfinished phase
complete.
