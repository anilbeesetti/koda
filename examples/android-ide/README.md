# Android IDE smoke project

This small Compose app tests a module named `mobile`, two product flavors,
generated `R` and `BuildConfig` symbols, a Kotlin call into Java, and an Android
library dependency named `greeting`.

Downloaded Koda apps can provision their own Kotlin, debugger and preview tools
from **Android → Tool setup**, without this checkout or launcher environment
variables. See [Managed tool setup](TOOL_SETUP.md) for prerequisites, repair,
updates, platform limits and acceptance checks.

The Android tool installers require Python 3.12 or newer. Ensure `python3` on
your `PATH` meets that requirement (`python3 --version`) before running setup.

From the repository root, run:

```sh
script/install-android-kotlin
script/android-ide --release examples/android-ide
```

Trusted Android projects sync automatically on open and initially select
**:mobile · demoDebug** in the topbar. Use **Configure official Kotlin** in the Android tools panel. Install the Kotlin extension from Extensions if it is
not already installed. Select a connected device or a stopped emulator in the topbar,
then **Run**; a stopped emulator is booted before deployment. The app should display `dev.zed.androidsample.demo`. Repeat with
`fullDebug`; it should display `dev.zed.androidsample.full`. Both variants should
also display `Android library connected`. Generated symbols refresh automatically
after switching variants.
Use **Stop emulator** when finished to release the VM's memory.

Use **Open Logcat** for the structured device log viewer. See [Logcat](LOGCAT.md)
for filtering syntax, capture controls, saved files, and validation steps.

The official JetBrains Kotlin backend is the only supported backend. If you used
the community backend before, relaunch the IDE and configure Kotlin once to switch
your existing project settings.

Use **Configure official Kotlin** in the Android tools panel. The pinned
`263.4702.0+android-6` build includes a source-built native importer patch for
selected dependency variants, exact library sources, and dynamic features, plus
semantic Compose completion and naming fixes. It prepares the focused Kotlin
file during import and retains analysis caches when the imported model is unchanged.
The installer verifies the upstream
archive, public source, build dependencies, and installed patch. Its manifest and
source license remain with the runtime.

Setup generates R and BuildConfig for the imported variants, using AGP's generation
tasks. Dynamic-feature dependency metadata is generated from artifact identities
without building their classes. A Gradle task-graph guard stops the request before
execution if another generator would compile Kotlin or Java. Generation failures report degraded generated
symbol support while still configuring Kotlin. Gradle, dependency, resource,
manifest, and selected-variant changes refresh the managed backend automatically.
An unavailable selected variant pauses it until a valid variant is selected.
Custom and disabled-server settings are preserved until you explicitly configure Kotlin.

Library source and decompiled `jar:`/`jrt:` tabs support navigation, but their
diagnostics are currently ignored.

The Run menu's Android unit tests and lint actions operate on the selected
variant. To check the fixture directly:

```sh
cd examples/android-ide
./gradlew :mobile:assembleDemoDebug :mobile:testDemoDebugUnitTest :mobile:lintDemoDebug
```

The fixture requires Android SDK 37 and a JDK supported by Gradle 9.6.1. The
macOS launcher discovers the standard SDK and Android Studio runtime; command
line builds need `ANDROID_HOME` and `JAVA_HOME` set. Kotlin setup uses JDK 21
separately. Generated caches and machine-specific settings are ignored.

## Official Kotlin LSP protocol check

`script/test-kotlin-lsp --self-test` checks UTF-16 incremental edits, framing,
no-op document versions, readiness, and error classification without a server.
To run the real compatibility check, use an isolated sample copy and the pinned
official `263.4702.0` server:

```sh
fixture="$(mktemp -d)"
mkdir "$fixture/project"
tar --exclude=build --exclude=.gradle --exclude=.kotlin --exclude=.zed \
  --exclude=local.properties --exclude=workspace.json \
  -C examples/android-ide -cf - . | tar -C "$fixture/project" -xf -
export JAVA_HOME=/path/to/jdk-21
export ANDROID_HOME=/path/to/android-sdk
script/test-kotlin-lsp \
  --project "$fixture/project" \
  --server /path/to/kotlin-server-263.4702.0/bin/intellij-server \
  --output "$fixture/report" \
  --variant demoDebug
```

The project JDK is separate from the server's bundled Java runtime. The probe
uses the native Gradle importer and waits for both import success and actual
indexing completion. It immediately requests completion after each unsaved
incremental change, without sleeps or retries. It executes the returned completion
command, applies edits to its in-memory buffer, and checks the import, call and
caret response. It never saves its source edits or assembles the application.
Add `--generate-resources` to run the app's exact resource-only setup before the
probe, including on a clean copy. It records the generation log and script hash.

`results.json` records the negotiated capabilities, actual imported app/library
variants, source-attachment presence, document versions, request times, target
URIs/ranges, virtual content, and completion edits. `workspace-model.json` preserves
the exported model, including module-scoped R libraries and project dependency
edges. `wire.jsonl` preserves both
protocol directions; import and indexing events and server errors are retained.
The output directory also holds server indexes for repeat-run comparisons.
Use a fresh output directory for cold-index measurements. Gradle can write to the
project and its normal caches; the probe briefly exports and removes
`workspace.json`, and refuses a fixture that already has that file.

Required assertion failures exit nonzero. Use `--require-original-sources` and
`--require-compose` to require the source, nested-navigation, and Compose fixes
included in the patched installation.

To measure first navigation at the original import/index readiness boundary, add
`--navigation-only --prepare-active-file --navigation-samples 30 --buffer-wire`.
Preparation starts after didOpen, matching the editor's active-file command.
The first definition runs before model export or other semantic requests and is
reported separately from warm requests. `--navigation-source <relative-file>` and
`--navigation-symbol <name>` select a larger fixture's measured location; they do
not change preparation. Use distinct output directories and a shared
`--system-path <directory>` for at least 30 fresh server processes before making
percentile claims. Measure fresh-index imports separately. Protocol timings exclude
editor buffer creation and drawing.

`--variant fullRelease` also checks that the dependent library selects `release`;
the unmodified pinned server falls back to `greeting.debug`, which fails this gate.
The patched native importer passes both variants and original Compose source
navigation.
For a custom build type, pass both `--variant fullStaging` and
`--expected-library-variant release` to check the intended `matchingFallbacks`.

For another isolated Gradle project, `--model-only --module :app --variant debug`
checks import/index readiness and the exported model without editing documents.
Repeat `--expected-module <exact-exported-name>` for each required module, including
dynamic feature main/test variants. Module names are matched exactly against the
`active_modules` array in the report. Document and performance checks require the
sample fixture and cannot be combined with `--model-only`.

Add `--extended` to exercise references and rename across application, library,
Kotlin and Java files, overload preservation, diagnostic clearing, quick fixes,
organize imports, formatting, signature help, type/implementation navigation,
and Compose completion contexts. Refactoring edits remain in memory; the probe
checks every touched source file is unchanged on disk. `--require-compose` makes
the Compose naming, required-lambda, and semantic named-argument ordering checks
required and implies `--extended`.
Java-origin references and rename use the same semantic providers as Kotlin.
The probe checks both directions and rejects renaming generated BuildConfig or
read-only library declarations.

Add `--performance-samples 30` for 30 samples each of warm definition, immediate
unsaved Modifier completion, explicit and dot-triggered String completion, and
named arguments. Reports include every latency, p50, nearest-rank p95, failures,
hardware and instantaneous
process RSS. The first definition is a separate single observation. These are
headless request/response timings; they exclude editor input, buffer creation and
rendering. Matching Gradle/JDT/preview/editor processes can belong to another
workspace, so their RSS is not a controlled total-memory measurement. Use the
same output directory for persistent-index restart comparisons and preserve its
previous report before rerunning.
`--warmup diagnostics`, `--warmup document-symbols`, and `--warmup hover` each
issue one bounded request for the active file before the first definition and
report that warmup cost separately from indexing readiness. `--jfr` captures a
bounded Java Flight Recorder profile in the report directory; its overhead is
included in that run's timings.

This is a headless protocol check. Separate Zed client regressions cover
incremental completion acceptance, command edits, caret undo/redo, workspace-edit
failure reporting, and virtual-document ownership/lifecycle in
`crates/editor/src/editor_tests.rs` and `crates/project/tests/integration/lsp_store.rs`.
The client suite separately tests expired sessions and late responses after timeout.

## Real editor check

For release-app timings, launch with `ZED_LOG=editor.interaction_latency=debug`.
The log records successful definition/Cmd-click and visible completion interactions
through a rendered editor frame. It excludes the OS compositor. Record at least
30 samples per scenario and run without concurrent builds or other benchmarks.
The test-support harness below provides correctness coverage; it is not a release
performance benchmark.

The ignored `test_real_kotlin_editor_completion_and_navigation` test uses the
actual server, a GPUI test window, and unsaved source edits in an isolated Gradle
project copy. Set `ZED_KOTLIN_CLIENT_PROBE_CONFIG` to an absolute JSON file containing
`root`, a root-relative Kotlin `source`, absolute report `output`, `warm_samples`
(at least 30 for percentile measurements), `completion_refinement: true`,
`require_compose: true`, and the normal Zed `binary` and `initialization_options`
objects for `kotlin-lsp`. Keep the server's
`--system-path`, `idea.config.path`, and `idea.log.path` under the report directory;
set `LSP_ANDROID_MODULE`, `LSP_ANDROID_VARIANT`, and project JDK in `binary.env`.
Use the same Gradle initialization options as **Configure official Kotlin**.
Prefix refinement deletes/retypes the completion prefix in the existing function;
without it, each sample replaces the probe function. Optional
`completion_warm_samples` overrides `warm_samples` for completion scenarios.

```sh
ZED_KOTLIN_CLIENT_PROBE_CONFIG=/absolute/path/probe.json \
  cargo test -p editor --features project/test-support,gpui/inspector \
  test_real_kotlin_editor_completion_and_navigation -- --ignored --nocapture
```

The test records first and warm navigation, displayed completion rank, acceptance,
imports, caret, and process RSS; it verifies source text remains unchanged on disk.
It excludes release rendering/GPU latency. Shared Gradle daemons are reported
separately, and JDT/preview are inactive; this is not a complete IDE memory budget.
Do not run builds or other performance probes during timing collection.

## Java, debugging, and preview

Run `script/install-android-kotlin` and `script/install-android-debugger`
once before launching the IDE. Compose preview dependencies are bundled by Cargo;
`cargo run --locked -p zed --bin koda` needs no separate preview setup. **Configure Java**
imports the selected variant into JDT LS. Once configured, managed official Kotlin
setup refreshes that Java model after variant or Gradle input changes. Java model
refresh can compile sources; failure leaves Kotlin editing available. Only JDT is
restarted for Java refresh. Default Java settings use official Kotlin for semantic
navigation/refactoring and JDT for Java completion and diagnostics; custom or
disabled Java server lists are preserved.

**Debug** builds and launches the selected app, then attaches the native debugger.
Set breakpoints on the return in `Greeting.java` and `LibraryGreeting.kt`; inspect
variables, step, and disconnect using the debugger controls. On macOS, Control-D
starts Android debugging, Command-Option-R continues, Shift-F8 steps out, and
Command-F2 disconnects. `script/test-android-debugger --device emulator-5554` provides an
explicit emulator-only smoke test after building `demoDebug`.

**Compose preview** opens all previews for the active Kotlin file beside the code,
including multipreview annotations and parameter values. Use **Build & Refresh**
or leave **Auto** enabled to rebuild after edits, including unsaved Kotlin changes.
Click a preview to inspect its layout outlines, then click a component to navigate
to its source. Rendering uses bundled Google tooling, bundled Java 21, and the selected
variant's resources. See [Compose previews](COMPOSE_PREVIEW.md) for controls,
implementation research, compatibility boundaries, and integration checks.

## Shared Android project model

Sync evaluates `kodaAndroidProjectModel` through the project's own Gradle wrapper.
It discovers the application targets, Android library and dynamic-feature modules,
JVM project dependencies, and variant components in one versioned catalog. Main,
unit-test, device-test, and test-fixture components retain separate source roots
and dependency scopes, including component-specific resource namespaces. Sync runs
registered source/resource producers to read task-backed AGP source providers; it
may execute transitive compilation tasks required by those producers. The selected graph uses Gradle's resolved build-type and
flavor attributes, including `matchingFallbacks`; it never assumes that dependency
variants have the application's name.

Each module collects its own sources and resolves its own compile configurations
in a project-owned task. The root task aggregates plain model data after all module
tasks finish, so parallel Gradle execution does not resolve another project's
configurations without its lock. The Java classpath exporter follows the same rule.
Sync's `export.gradle` is written into a unique OS temporary directory (normally
outside the project) and removed when that request finishes or is cancelled. Existing
`.koda/android-model/export.gradle` files from older versions are no longer used
or overwritten. The persistent `.koda/android-model/selected.json` remains the
managed projection used to configure the native Kotlin importer.

Android Studio instead requests AGP's per-project, selected-variant models using
the Gradle Tooling API ([Studio model fetchers](https://android.googlesource.com/platform/tools/adt/idea/+/mirror-goog-studio-main/project-system-gradle-sync/src/com/android/tools/idea/gradle/project/sync/ModelFetchers.kt),
[AGP model builder](https://android.googlesource.com/platform/tools/base/+/mirror-goog-studio-main/build-system/gradle-core/src/main/java/com/android/build/gradle/internal/ide/v2/ModelBuilder.kt)).
Its IntelliJ Gradle integration can inject helper init scripts, but writes them
to temporary storage ([init-script utility](https://github.com/JetBrains/intellij-community/blob/master/plugins/gradle/src/org/jetbrains/plugins/gradle/service/execution/GradleInitScriptUtil.kt)).
Koda's CLI JSON exporter remains a separate bridge; `.koda/android-model` is a
Koda convention. The script's location does not cause Gradle's lock error:
[unsafe configuration resolution](https://docs.gradle.org/current/userguide/declaring_configurations.html#sec:configuration_unsafe_access)
includes resolving another project's configuration from a task.

`Project::android_model()` exposes the shared catalog and immutable selected
snapshot. `SelectedProject::modules()` supplies the selected components and
`ModelState::token()` identifies the current generation. Consumers must retain
that token and check `is_current()` before publishing asynchronous results or
continuing a build/run/debug operation. A root change, a variant change, or changed
Gradle/model inputs invalidates older tokens, including an A → B → A selection.
A failed or cancelled sync leaves no selected shared snapshot; resync restores a
previous variant only if its identity still exists in the fresh catalog. This is
also the integration API for the separate structured Android testing UI: use the
selected component names/scopes and dependencies; do not discover another graph.

On first sync, Koda selects the first application module alphabetically and uses
Android Studio's default-variant ordering: explicit build-type defaults, explicit
flavor defaults in dimension order, `debug`, alphabetical flavors, then build type.
Only enabled variants are considered. The initial choice and manual changes are
remembered per project. An unavailable remembered variant requires a new choice.

The official Kotlin server still owns Kotlin/Java semantic analysis and rename.
The managed `263.4702.0+android-6` importer accepts the resolved variant map and
source-root projection without replacing native compiler settings, friend source
sets, dependency source archives, or semantic features. Reinstall the managed
server after this change. Java's Eclipse/Buildship projection uses the same selected
modules and component roots, marks test folders and test-only libraries with
`test=true`, and may compile selected components to materialize their classpaths.
Configured Java refreshes on selection/input changes even if Kotlin is not
configured. Language-server indexing remains asynchronous; a model sync is not
confirmation that every server has finished indexing.

Preview export retains its separate **runtime** classpath, while validating the
selected module, variant, namespace, and model generation. The embedded gallery
accepts files in the selected main source graph and checks that Gradle actually
compiled the active file. Test-only dependencies and inactive flavors do not become
preview sources. Model changes cancel rendering and source navigation, including
A → B → A selection changes; registered generated roots do not trigger automatic
rebuild loops. Compiled runtime inputs are distinct from Kotlin/Java compile inputs.
Resource navigation uses the selected
Gradle roots rather than scanning every flavor, supports custom/registered generated
roots, and keeps locale/qualifier and overlay declarations visible. It does not
compute the final AGP resource-merge winner. Before the first sync, the existing
conventional `src/.../res` fallback remains available.
Local resource lookup follows the owning component and dependency main resources;
cross-module fixture-specific `R` resources are not resolved locally.

Supported boundaries:

- The picker still selects application APK targets. Library/dynamic-feature and
  JVM modules are imported as part of that selected graph; library-only projects
  are catalogued but cannot currently be selected for language setup/build/preview.
- Included builds, external module/source directories, standalone `com.android.test`
  modules, and `com.android.kotlin.multiplatform.library` have explicit sync
  diagnostics. Nested unit/device tests are distinct from standalone test modules.
- A graph requiring two active variants of one module is rejected, including
  conflicting main/test requirements. Ordinary resolution failures fail sync;
  partial catalogs are not published. Sync resolves the catalog's variants, so an
  unresolved dependency in another variant can also prevent catalog publication.
  A source producer or its transitive compilation failure also fails sync.
  The exporter disables configuration caching and does not support Gradle's
  isolated-project mode; its configuration phase still inspects the build's modules.
- AGP-generated roots use the public `all`/`static` registration APIs where available;
  AGP 8 also normalizes legacy static source sets and build-directory producers.
  Kotlin-plugin-only/JVM roots retain a build-directory classification fallback.
  Unregistered task outputs cannot be discovered. Managed cache files preserve
  unrelated user settings and refuse unmanaged/symlinked cache directories.
- This change does not add a test runner, expand the preview renderer's supported
  modes, or establish new debugger/emulator or Kotlin semantic compatibility claims.

The real Gradle model/Java/Eclipse probe creates isolated fixtures with flavors,
variant fallbacks, a dynamic feature, JVM test dependencies, registered generated roots,
and library test fixtures. AGP 9 retains generator output outside `build/`; AGP 8
relocates this registered output into its managed generated directory. It verifies removal/restoration of a selected
variant, failed sync/recovery, and included-build diagnostics:

```sh
script/test-android-project-model --gradle /path/to/gradle-8.11.1/bin/gradle \
  --sdk /path/to/android-sdk --agp 8.9.1
script/test-android-project-model --gradle /path/to/gradle-9.7.1/bin/gradle \
  --sdk /path/to/android-sdk --agp 9.4.0
cargo test --locked -p android_tools --lib
cargo test --locked -p android_ui --lib
cargo test --locked -p project --features test-support --test integration test_android_resource
python3 script/test-android-kotlin-installer
```

The Gradle probe needs JDK 21 and installed API 35 and AGP-required build tools (35 for AGP 8.9.1, 36 for AGP 9.4.0), and uses the session's
normal network/proxy/trust configuration. Every Gradle invocation enables parallel
execution to cover project-lock ownership for the model and Java exporters.
It does **not** launch the native Kotlin
server, renderer, editor UI, debugger, or an emulator. Those require their separate
runtime probes; successful catalog/Eclipse checks alone must not be reported as
Kotlin semantic, preview-rendering, or device validation.
