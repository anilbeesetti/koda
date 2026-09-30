# Android IDE on Zed

## Direction

Build an Android development environment on Zed that preserves the familiar
Android Studio workflow while keeping the editor responsive. The product should
open an existing Android project, help edit Kotlin and Java, build the selected
variant, run it on a selected device, and expose failures without making the
developer assemble a collection of terminal commands.

Android Studio's current dark UI is the reference. Familiarity includes the
position of controls, keyboard actions, selection behavior, and useful error
messages, as well as colors. It does not imply that all Android Studio features
already exist in this fork. Compose rendering and JVM debugging now have tested
implementations; advanced refactorings, Kotlin coroutine debugging, interactive
previews, XML design tools, and profilers still need separate acceptance tests.

The initial target is local development on Apple Silicon macOS, the environment
available for development and testing. Keep subprocess construction and settings
portable, then validate Linux and Windows explicitly. Preserve the upstream
licenses and notices. Use a distinct distribution identity before shipping an
installer; this is an independent fork, not an official Android Studio build.

## Findings and architectural decisions

### Implementation checkpoint after the language, debugger, and preview work

The local stack now contains the original ten layers plus Java support, the
pinned Kotlin runtime, Android debugging, Compose preview, and final validation.
See [validation evidence](ANDROID_IDE_VALIDATION.md) and the
[per-layer review guide](ANDROID_IDE_REVIEW.md) for exact branches and checks.
Nothing has been pushed or submitted to GitHub.

The native app includes Studio themes/tool rails/keybindings, evaluated Android
targets, variant persistence, build/run/test/lint/Logcat, emulator start/stop,
and an isolated macOS launcher. The two-module/two-flavor fixture exercises
Android resources, generated symbols, Java/Kotlin calls and Compose previews.

Java setup exports the actual selected AGP compile graph into JDT LS's normal
Eclipse/Buildship import. Android SDK, generated resources and BuildConfig,
library symbols, type diagnostics, and full-to-demo navigation were verified.
The pinned community Kotlin build fixes the reproduced object-rename crash;
Java callers are still outside that Kotlin rename transaction. The deprecated
server remains a compatibility choice, so a maintained production language
engine and broader source-set/KMP coverage remain product gates.

Android Debug uses the existing native debugger with a pinned, minimally patched
community JVM adapter. Native Java/Kotlin breakpoints, local variables, stepping
and detach work on the emulator. Compose preview uses downloaded Google tooling,
compiled annotation discovery, the selected variant's runtime classes/resources,
and the existing image viewer. Both variants and multiple preview configurations
render with Android Studio closed and no device connected.

The final optimized editor measured about 334 MiB, and about 2.41 GiB including
its Java and Kotlin language support, on the small fixture. Exact samples and
the earlier warm first-workspace-render measurements are in the validation
report. Language-server cost remains the main memory target; these observations
do not establish a feature-equivalent Android Studio performance advantage.

### What the fork already provides

The starting revision is `7960b2a7c9`. It includes a Rust/GPUI desktop editor,
native windows, theme loading, a project tree, docking, terminals, tasks, search,
LSP, DAP, Git, and extension installation. Replacing these components would add
maintenance work and put the responsiveness goal at risk.

| Need | Existing implementation | Decision |
| --- | --- | --- |
| Familiar shortcuts | `assets/keymaps/macos/jetbrains.json`, `assets/keymaps/linux/jetbrains.json` | Make JetBrains the fork default and add Android-specific actions only where necessary. |
| Dark/light appearance | `assets/themes`, `crates/theme`, `crates/theme_settings` | Bundle Studio-inspired themes through the normal theme loader. |
| Project, terminal, Git, structure | `crates/project_panel`, `crates/terminal_view`, `crates/git_ui`, `crates/outline_panel` | Reuse their focus, resizing, persistence, and keyboard behavior. |
| Top-level controls | `crates/title_bar` | Add working Android controls without replacing window handling. |
| Build output and cancellation | `crates/workspace/src/tasks.rs`, `crates/tasks_ui` | Schedule real tool commands using the existing terminal task lifecycle. |
| Kotlin/Java editing | Language extensions and the project LSP store | Validate language servers against Android fixtures; do not build another parser or indexer. |
| Android project operations | Gradle wrapper, Android CLI, SDK platform tools | Use supported tool interfaces instead of interpreting Gradle source text. |
| Debugging presentation | Existing DAP UI | Reuse it once Android attach and adapter compatibility are demonstrated. |

The existing extension API exposes language servers and related capabilities,
but it is not a general native panel API. Android-specific native controls should
therefore live in the fork while language grammars and LSP integrations remain
extensions. Keep the Android integration isolated enough that upstream merges
mostly affect a few initialization points.

### Android Studio familiarity

Google documents the New UI around project and Git widgets, device selection,
run configurations, and tool windows docked to the sides. The Android project
view is a logical grouping of manifests, source, resources, and Gradle files;
it is different from the filesystem tree.[^1][^2]

Use these visual targets:

| Element | Initial target |
| --- | --- |
| Editor background | `#1E1F22` |
| Tool windows and main toolbar | `#2B2D30` |
| Dividers | `#393B40` |
| Primary text | `#DFE1E5` |
| Focus and active selection accent | `#3574F0` |
| Muted text | `#9DA0A8` |
| Editor text | 14 px, compact line height, monospace |
| UI text | 14 px with normal sentence capitalization |
| Project pane | Left, initially about 280 px wide |
| Tool output | Bottom; editor remains visible during builds |
| Run controls | Top, next to device and run-target selection |

These are implementation targets, not claims of pixel-perfect equivalence. Check
the installed reference at the same window size and scaling. Prefer accessible
contrast over copying an unreadable shade. Include tooltips and keyboard focus
for icon controls. Avoid misleading decorative controls: Debug or Preview
should not appear enabled before the workflow works.

Retain the operating system's shortcut conventions. The existing JetBrains map
covers Find Action, recent files, navigation, rename, formatting, and tool
windows. Android actions add Run, Build, and Logcat bindings. Google's shortcut
reference establishes different macOS and Linux/Windows combinations.[^3]

### Build and project model

Gradle scripts are programs. A search for `applicationId = ...` cannot correctly
resolve plugins, flavors, convention plugins, included builds, or application ID
suffixes. The Gradle wrapper is the project's build-version authority. CLI builds
and installs are supported independently of Android Studio.[^4]

First reuse the official Android CLI where it can describe the project and
build/deploy a selected target. Validate its actual installed command line and
output before depending on it. Keep an explicit prerequisite check with a useful
error if it is absent. Use the wrapper directly for common build, test, and lint
tasks. Do not silently install SDK packages or change the project's Gradle files
when opening it.

For richer synchronization, use an evaluated model through the Android CLI or a
small Gradle Tooling API bridge. The Tooling API supports requesting models,
running builds, progress reporting, and cancellation. It does not remove the
need to choose and test the Android-specific model contract.[^5]

Represent project root, application module, variant, build tasks, artifact
locations, and selected device explicitly. Discover all supported application
targets; never assume every project uses `:app` or has only a `debug` variant.
Invalidate target data after build configuration changes or explicit refresh.
Only run project code after the workspace's existing trust flow allows it.

### Kotlin and Java intelligence

JetBrains' official Kotlin LSP currently documents Gradle and Maven support and
experimental Android Gradle Plugin support. It is Alpha, with KMP support still
under development. The implementation includes proprietary components from
JetBrains products; its repository is not evidence that the whole server is
permissively redistributable.[^6][^7]

This makes it a promising candidate, not a completed Android Studio replacement.
Start with the maintained Zed Kotlin extension and an explicit server choice.
Measure Android symbol resolution, generated sources, diagnostics, completion,
and rename on the reference project before making a server the default. Document
server versions and installation behavior. Do not bundle proprietary server
binaries without reviewing the actual distribution terms.

Use the same approach for Java through Zed's language-server integration. Mixed
Kotlin/Java navigation, Android resources, and generated `R`/`BuildConfig` symbols
must be checked together. Plain Kotlin syntax highlighting is not proof that
Android project intelligence works. The current Zed Kotlin guide describes the
extension installation path.[^8]

Two candidates deserve explicit follow-up testing:

* Zed's Java extension uses Eclipse JDT LS. Its current upstream documentation
  describes Android Gradle import as experimental and requires at least Java
  21. Test Android classpaths, generated symbols, and navigation rather than
  assuming ordinary Java-project support covers AGP.
  [Zed Java guide](https://zed.dev/docs/languages/java),
  [Eclipse JDT LS](https://github.com/eclipse-jdtls/eclipse.jdt.ls).
* `ktlsp` is a newer MIT-licensed Rust alternative for Kotlin and Java. Its
  authors explicitly say type inference, overload resolution, and Gradle
  modeling are not compiler-complete. Its own benchmark also shows that an
  eager native index can use more memory than a JVM server. Treat it as a
  compatibility spike candidate, not an automatic replacement because it uses
  Rust. It was researched but not installed or validated in this session.
  [ktlsp scope and limitations](https://github.com/pepegar/ktlsp).

### Devices, run, and logs

ADB identifies devices by serial and reports connected, offline, and unauthorized
states. It supports explicit device selection. Always bind a run or log session
to the selected serial instead of letting an ambient default choose a device.
Use the existing SDK installation and propagate tool failures to the UI.[^9]

For the first working run path, delegate build/deploy/launch to Android CLI and
show its output in a normal Zed task terminal. Follow with native artifact-aware
deployment only if the CLI measurably blocks the required workflow. Keep emulator
startup separate from Run so a missing device does not unexpectedly start a
large VM.

Logcat initially uses a selected-device task with bounded terminal history. A
native Logcat table can follow when timestamp, PID, tag, severity, filtering,
pause/resume, and burst handling have test coverage. Logcat is a streaming tool;
an unbounded in-memory list would contradict the product goal.[^10]

### Debugger, preview, and profilers

Treat these as substantial later milestones. DAP presentation alone does not
provide Android JDWP attach, source mapping, process selection, or Kotlin-aware
evaluation. Define an adapter compatibility spike and a reproducible breakpoint
test before exposing Debug as a supported feature.

Compose preview requires rendering real application code and resources against
the selected variant. Reusing Android Studio remotely for a preview can be a
developer reference tool, but it does not meet the goal of operating without
Android Studio. Investigate a standalone rendering service in a separate spike.
Keep that service off until requested, with timeouts and bounded caches. XML
preview, live edit, layout inspection, native debugging, and profilers each need
their own dependency and performance assessment.

## Memory and responsiveness contract

The motivation is lower resource use, but no improvement percentage is claimed
before measurement. A Rust editor can still launch a Gradle JVM, a language-server
JVM, ADB, and an emulator. Compare both the editor process and the total workflow
process set; otherwise a small editor can hide expensive helper processes.

Use the same project, tool versions, device, window size, and warm/cold state.
Record revision, build profile, operating system, memory pressure, and enabled
extensions. A debug Zed build is suitable for functionality checks, not a fair
comparison with a production Android Studio binary.

| Scenario | Measure | Proposed acceptance gate |
| --- | --- | --- |
| Open empty window | Time to interactive editor; resident/physical memory | Establish a release-build baseline first. |
| Open small Compose project | Time to editable file and first useful completion | No UI freeze while Gradle or LSP starts. |
| Open representative multi-module project | Import time and total process memory | Correct target model; bounded growth across repeated imports. |
| Warm edit and navigate | Input/frame latency; CPU during idle | No new periodic work while Android tools are unused. |
| Build and run | Total duration, cancellation time, errors | Correct target/device and visible actionable failure. |
| Logcat burst | UI responsiveness and memory over time | Bounded history and no growth after session close. |
| Close project | Remaining helper processes and memory | Owned tasks and streams stop; shared SDK services remain usable. |

After baseline collection, set numerical budgets from real measurements. Report
medians and ranges over repeated runs, and retain raw data. Separate build-system
time from editor time. Avoid claiming a benefit from disabling functionality in
one product while leaving it enabled in the other.

## Step-by-step implementation and review stack

Every layer stays local until publication is explicitly requested. Use
`gh stack view --json` to inspect the chain, commit one concern at a time, and use
`gh stack add <branch>` only when starting that concern. Do not merge the stack
or push branches during this work.

### 1. Research and development baseline

Branch: `codex/android-ide/research`.

1. Inspect the fork, licenses, existing keymaps, theme loading, task lifecycle,
   title bar, trust boundary, and panel API.
2. Install the pinned Rust toolchain and the requested `gh-stack` extension.
3. Verify Xcode, Metal compiler, CMake, Android SDK, JDK, Android CLI, and emulator
   availability; reuse installations where possible.
4. Build upstream-derived Zed with the pinned lockfile.
5. Create an isolated Compose project in Android Studio and record the reference
   layout and tool versions.
6. Save this plan with evidence, acceptance gates, and known gaps.

Gate: reproducible build command and a controlled reference project. A build
failure is recorded with the command and diagnostic, not treated as a passing
baseline.

### 2. Studio appearance and familiar defaults

Branch: `codex/android-ide/studio-defaults`.

1. Bundle Studio-inspired dark and light themes.
2. Select the dark theme and existing JetBrains keymap as defaults.
3. Set compact editor/UI typography and the Project pane on the left.
4. Keep terminals at the bottom; align outline/Git placement with the editor
   layout and move optional assistant UI to the right.
5. Reduce unrelated startup chrome through existing settings.
6. Validate theme parsing, settings loading, both appearances, editor contrast,
   command palette, tooltips, and keybindings in a clean profile.

Gate: launchable editor with the intended layout, no missing-theme fallback, and
working existing actions. Existing user settings must continue to override
defaults. Capture the actual running fork, not a mockup.

### 3. Android tool discovery and command model

Branch: `codex/android-ide/android-tools`.

1. Identify local Gradle roots without executing project code.
2. Resolve SDK/ADB through configured environment and installed locations, and
   surface missing prerequisites clearly.
3. Parse ADB device output while retaining unauthorized/offline states.
4. Read evaluated Android build targets from a supported tool interface.
5. Construct commands with explicit working directory, target, and device.
6. Add focused tests for malformed output, multiple devices, missing tools,
   flavor targets, and paths with spaces or shell metacharacters.

Gate: command selection cannot silently deploy to another device or guess a
module. Tool calls run off the UI thread, with ownership and cancellation.

### 4. Native Android workflow

Branch: `codex/android-ide/android-workflow`.

1. Add an Android tool window using the existing Panel interface.
2. Show project/tool status and a device picker with explicit selection.
3. Add build-target selection, Build, Run, Test, Lint, Refresh, and Logcat through
   existing task terminals where supported.
4. Wire Android Studio run/build/log shortcuts to these same actions.
5. Expose working controls in the main toolbar; retain the tool window for
   discovery, less common operations, and useful error descriptions.
6. Verify save-before-build, failed build, disconnected device, missing CLI,
   repeat run, cancellation, and project close.

Gate: edit the fixture, build it, run the changed app on the selected emulator,
and inspect output from inside the fork. Test empty and invalid states too.

### 5. Kotlin/Java Android intelligence

Implemented layers: `codex/android-ide/java-support` and
`codex/android-ide/kotlin-runtime`. The basic Android editing gate is met on the
fixture; mixed-language rename, test/source-set visibility and KMP remain open.

1. Install and pin a candidate Kotlin server through the existing extension path.
2. Verify completion for Android and Compose APIs, imports, diagnostics, and
   generated symbols in the reference project.
3. Exercise navigation and rename across files and mixed Java/Kotlin modules.
4. Record indexing time, memory, version compatibility, and unsupported cases.
5. Keep language support lazy and show server/import failures with recovery.

Gate: Android-aware editing demonstrated against real fixtures. If experimental
AGP support fails, retain syntax editing/build workflows and report the failure
precisely rather than labeling the IDE feature complete.

### 6. Evaluated Android project view and variants

Branch: `codex/android-ide/project-model`.

1. Introduce a cached model only after the chosen producer's schema is validated.
2. Add logical manifests, source, resources, tests, and Gradle groups while
   preserving the ordinary Project view.
3. Support application/library modules, flavors, generated roots, version
   catalogs, convention plugins, included builds, and changed application IDs.
4. Keep selection stable during refresh and expose model progress/failures.

Gate: switching variants changes build targets and relevant source/resource data
without stale navigation or edits to the project's build configuration.

### 7. Dedicated Logcat and device management

Branch: `codex/android-ide/logcat-devices`.

Implement incremental device tracking, authorized/offline recovery, explicit AVD
startup, app/process selection, bounded log buffers, severity/tag/text filters,
pause, clear, copy, and source links. Test rapid reconnects and log floods before
replacing the terminal fallback. Keep device-side changes explicit.

### 8. Android debugging

Branch: `codex/android-ide/debugger`. Basic deploy/attach, Java/Kotlin line
breakpoints, local evaluation, stepping and detach are implemented and tested.
Coroutine/inline mappings, advanced expressions and native debugging remain open.

Prove a DAP/JDWP adapter path with a debuggable fixture. Add deploy-and-attach,
breakpoint mapping, step/continue, variables, evaluation, and detach. Test Kotlin
coroutines, mixed sources, attach failure, and process death separately. Do not
advertise native C++ debugging until LLDB/NDK support has its own test.

### 9. UI preview spike

Implemented branch: `codex/android-ide/compose-preview`. Standalone static
Compose rendering, variant resources, annotation selection and failure handling
are verified. Interactive previews, multi-value parameter galleries and XML
design tooling remain open.

Produce an evidence-backed prototype for standalone Compose rendering. Establish
variant/resource classpaths, classloader isolation, rendering process lifetime,
error mapping, and device/theme/locale configurations. Measure render startup and
cache growth. A static image or a preview generated by a running Android Studio
instance is reference evidence, not standalone preview completion.

### 10. Performance and release hardening

Branch: `codex/android-ide/performance-validation`.

Build an optimized binary, run the shared scenario matrix against Android Studio,
measure the total process set, and fix the largest demonstrated regressions.
Validate Linux/Windows, keyboard-only access, high DPI, persistence, long paths,
offline operation, installation, and upgrades. Give the fork separate config,
cache, bundle identity, update source, and crash-reporting policy before release.
Publish only after manual review and explicit authorization.

## Development and validation commands

The repository's macOS guide calls for Rust, Xcode/command-line tools, and CMake.
The local Metal toolchain is installed.[^11]

```sh
rustup show
xcodebuild -version
xcrun -f metal
cargo build -p zed --bin zed --locked -j 6
cargo test -p <changed-crate> <focused-test>
./script/clippy -p <changed-crate>
gh stack view --json
```

Use a separate development profile/config directory for interactive tests.
Open the Android fixture, not the Zed source tree, in the development binary to
avoid rust-analyzer triggering unrelated Cargo rebuilds. Keep a log of validation
commands and outcomes in `ANDROID_IDE_VALIDATION.md` as implementation progresses.
Use the native UI to verify appearance and shortcuts; compilation alone cannot
show that a control is visible, reachable, or correctly placed.

## Release and maintenance boundaries

Zed's repository contains multiple license scopes, including GPL application
code. Preserve notices and satisfy the applicable source/distribution obligations
before releasing binaries.[^12] The prototype should preserve attribution while
using its own eventual product name and update mechanism.

Avoid premature crate removal to pursue memory savings: disabling a UI feature
does not prove a runtime or binary-size benefit. Profile startup and helper
lifetimes first. Keep Android integration concentrated in a small number of
modules, with upstream APIs reused and changes documented per stack layer.

The implemented workflow now covers editing, build/run, Java/Kotlin debugging
and standalone Compose rendering. The remaining milestones prioritize broader
project compatibility, robust language semantics, representative performance
measurements and a distributable product. They are not claims of completed
Android Studio parity.

## Sources

Sources checked on 13 September 2026. Vendor pages without a stable version
should be rechecked when implementing the corresponding milestone.

[^1]: Google, [New UI in Android Studio](https://developer.android.com/studio/intro/new-ui), updated 6 March 2026.
[^2]: Google, [Meet Android Studio](https://developer.android.com/studio/intro), updated 1 September 2026.
[^3]: Google, [Keyboard shortcuts](https://developer.android.com/studio/intro/keyboard-shortcuts).
[^4]: Google, [Build your app from the command line](https://developer.android.com/build/building-cmdline).
[^5]: Gradle, [Tooling API](https://docs.gradle.org/current/userguide/tooling_api.html).
[^6]: JetBrains, [Kotlin Language Server](https://kotlinlang.org/docs/kotlin-lsp.html).
[^7]: JetBrains, [Kotlin/kotlin-lsp repository](https://github.com/Kotlin/kotlin-lsp), project status and installation documentation.
[^8]: Zed Industries, [Kotlin language support](https://zed.dev/docs/languages/kotlin).
[^9]: Google, [Android Debug Bridge](https://developer.android.com/tools/adb).
[^10]: Google, [Logcat command-line tool](https://developer.android.com/tools/logcat).
[^11]: Zed Industries, [Building Zed for macOS](https://zed.dev/docs/development/macos); local `docs/src/development/macos.md`.
[^12]: Zed Industries, [GPL license](https://github.com/zed-industries/zed/blob/main/LICENSE-GPL); local crate manifests and license files determine the applicable scope.
