# Android IDE smoke project

This small Compose app tests a module named `mobile`, two product flavors,
generated `R` and `BuildConfig` symbols, a Kotlin call into Java, and an Android
library dependency named `greeting`.

The Android tool installers require Python 3.12 or newer. Ensure `python3` on
your `PATH` meets that requirement (`python3 --version`) before running setup.

From the repository root, run:

```sh
script/install-android-kotlin
script/android-ide --release examples/android-ide
```

Trusted Android projects sync automatically on open. Select **:mobile · demoDebug**
in the topbar and use **Configure official Kotlin** in the Android tools panel. Install the Kotlin extension from Extensions if it is
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
`263.4702.0+android-5` build includes a source-built native importer patch for
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
