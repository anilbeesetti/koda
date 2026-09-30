# Android IDE prototype: validation and handoff

Session: 13–15 September 2026. Platform: Apple Silicon macOS. Changes are published
as draft stacked PRs following the author’s later authorization. Historical
checkpoints below retain their original measurements and limitations.

This is a working Android edit/build/run/debug/Compose-preview prototype with an
Android Studio-inspired shell. Android-aware Java editing and Kotlin object
rename also work on the fixture. It remains a development build: mixed-language
refactoring, advanced Kotlin debugging, broad project-model compatibility and
release packaging are not complete. The
[research and implementation plan](ANDROID_IDE_PLAN.md) covers the longer-term
work; [the review guide](ANDROID_IDE_REVIEW.md) describes the review stack.

## Library archive identity and cold navigation — 15 September 2026

Android Studio was inspected on the same navigation fixture. Its Typography tab
identifies `Gradle: androidx.compose.material3:material3-android:1.4.0@aar`, opens
`commonMain/androidx/compose/material3/Typography.kt` from the attached sources
JAR, and exposes reader mode. The `!/` separator denotes an entry inside an
archive; it is not a directory that a shell can enter.

The relevant IntelliJ implementation is small at the file-system boundary:
[CoreJarFileSystem](https://github.com/JetBrains/intellij-community/blob/master/platform/core-impl/src/com/intellij/openapi/vfs/impl/jar/CoreJarFileSystem.java)
splits the archive path from the entry and reuses an archive handler;
[CoreJarVirtualFile](https://github.com/JetBrains/intellij-community/blob/master/platform/core-impl/src/com/intellij/openapi/vfs/impl/jar/CoreJarVirtualFile.java)
constructs the `archive!/entry` identity, reads bytes through that handler and
rejects writes. Source files for this comparison are retained locally under
`target/android-ide/validation/studio-reference/`. No full Studio source checkout
or extraction of dependency archives was needed.

The prototype had two separate problems:

- The Gradle source query included every component in the resolution graph,
  including multiplatform metadata redirects. Alphabetical source lookup chose
  the common `material3` source JAR before `material3-android`. The exporter now
  queries the actual resolved external artifacts. The fixture exports 46 source
  archives and selects the exact Android artifact used by Studio. Project
  components are excluded from the artifact view to avoid ambiguous Android
  library variants. A real Gradle compile/classpath task verifies this path.
- The server found original source, then exported it to a random temporary file
  for clients without archive support. Android Kotlin setup now enables stable
  archive URIs. Zed reads JAR/ZIP entries through its existing filesystem and
  worktree/buffer flow, with read-only metadata, original file names, directory
  metadata and file watching. Filesystem mutations reject archive-entry paths.
  Attached sources no longer need a temporary copy. Decompiled fallback still
  uses the existing temporary-file path when original source is unavailable.

Cold navigation also exposed a duplicate-analysis race. Diagnostics and a first
navigation request could both observe an uncompiled file, then serialize two
compilations through the compiler's lock. The source-path cache now coordinates
conditional compilation, batch diagnostics and refresh, so the second request
reuses the first result. The lock is fair so navigation can run between indexing
batches, and initial workspace indexing gives the active editor file priority.
Already-cached navigation does not take the compilation lock. The concurrency
regression failed before the fix and passes after it, checking one compilation
and shared declaration identity.

Explicitly opened archive sources can be analyzed outside the workspace root,
but are excluded from the project source set to prevent duplicate library
classes. A runtime regression opens an archive entry and navigates within it.
The actual Compose probe also resolves Typography's `TextStyle` reference into
`ui-text-android-1.10.4-sources.jar!/commonMain/androidx/compose/ui/text/TextStyle.kt`.

Native testing found another routing issue: external buffers registered with
the project's server, but subsequent requests re-applied global language-server
settings. Those settings can select a different Kotlin server. Reused server
nodes now retain the project's server during lookup. The regression reproduces
an empty definition result with different project/global server selections,
then verifies navigation and worktree cleanup after the fix. Virtual archive
paths inherit the default project environment to preserve JDK and PATH discovery.
If the library needs a language server that is not running yet, it starts with
the sole visible project's settings and physical working directory, then binds
the archive buffer to that server. Both initial buffer registration and settings
refresh use the same project-root selection. A first-server regression verifies
the project root, a definition request and a replacement server after changing
initialization settings. Standalone archive viewing
remains available; new-server startup is skipped when multiple project roots are
possible, until explicit source ownership is implemented. Existing server reuse
continues to work.

Studio also has persistent semantic indexes and serialized declaration stubs;
these are separate from archive file loading. Its
[indexing documentation](https://plugins.jetbrains.com/docs/intellij/indexing-and-psi-stubs.html)
explains both the cached lookup mechanism and background indexing readiness.
This change does not replace the prototype's Kotlin compiler engine or eliminate
JVM startup and the first required semantic analysis. It removes redundant work
and preserves source identity without introducing another persistent symbol
cache. The community server's
[SymbolIndex](https://github.com/fwcd/kotlin-language-server/blob/6d9e61b79d4631e75516def7ab7ff0d8b9310467/server/src/main/kotlin/org/javacs/kt/index/SymbolIndex.kt)
stores searchable symbol names and locations; it does not serialize the compiler
binding context required for expression definitions such as `headlineSmall`.

The managed runtime is `+android-sources-8`. Timing separates readiness from
request duration; these are local samples, not statistically established speedups:

| Measurement | Before scheduling fix | After scheduling fix |
| --- | --- | --- |
| First definition, requested 100 ms after `didOpen` (3 alternating JVM runs) | 1.38–1.45 s; median 1.41 s | 1.29–1.35 s; median 1.34 s |
| Initialization in those runs (measured separately) | 0.834–0.908 s | 0.852–0.898 s |

With the final installed runtime, an isolated readiness probe initialized in
1.706 s, published diagnostics at 3.180 s, and resolved the first `headlineSmall`
request in 60 ms. An earlier pass measured 40 ms after readiness. Cold JVM and
machine-load variation remain visible; the archive identity fix does not claim
Android Studio's cold-start responsiveness.

Validation evidence (local files under `target/android-ide/validation/`):

- `archive-runtime-v8-install.log`: rename, archive URI, source-content,
  definition, concurrency, workspace-symbol and explicit-classpath regressions
  pass from a clean application of the pinned patch, before atomic installation.
- `archive-open-library-tests.log`, `archive-nested-navigation-fixed.log`:
  archive buffers remain outside the project source set while supporting further
  navigation, including the actual Typography → TextStyle cross-archive lookup.
- `archive-reused-lsp-registered-before.log`, `archive-reused-lsp-final.log`:
  reproduced external-buffer routing failure and all 19 project LSP integration
  tests passing after the fix.
- `archive-first-server-root-before.log`, `archive-first-server-final.log`:
  reproduced the archive worktree being used as the process root; all 20 project
  LSP tests pass after starting new archive servers in the real project context.
- `archive-first-server-clippy.log`: the final project startup correction passes
  the required Clippy wrapper.
- `archive-refresh-root-before.log`, `archive-refresh-root-final.log`,
  `archive-refresh-root-clippy.log`: native restart exposed settings refresh
  bypassing initial startup routing. The expanded regression reproduces the
  wrong worktree on replacement; all 20 LSP tests and Clippy pass after sharing
  project-root selection between both startup paths.
- `archive-latency-comparison.json`, `archive-navigation-ready-v8.log`:
  alternating cold-request comparison and final installed-runtime readiness probe.
- `cold-definition-race-before.log`, `cold-definition-race-after.log`: reproduced
  duplicate compilation and passing definition suite after sharing analysis.
- `archive-diagnostics-compatible-stdlib.log`: diagnostics, including edits while
  linting, pass with the server's Kotlin 2.1 standard library. The first run
  picked the host's newer Kotlin 2.3 library and failed on incompatible metadata;
  that was isolated through the test JVM's Gradle cache environment.
- `archive-fs-suite.log`: filesystem unit and integration tests, including
  archive reads, implicit directories, canonical paths, handles, read-only
  enforcement and reopening through a new filesystem instance.
- `archive-final-clippy.log`, `archive-android-ui-tests.log`: the repository
  Clippy wrapper passes for filesystem, project and Android UI crates; all five
  Android UI tests pass.

Native archive checks passed on the optimized build at
`780b82a1987a61c89380951247084812311d01f9` (`archive-final-build.log`, 20m 53s):

- `headlineSmall` opens the exact Material3 Android sources JAR and declaration
  at Typography.kt:94, with a locked tab and the original file name.
- Editing/saving the archive tab is blocked, and the physical JAR checksum stays
  unchanged. The tab reopens at the original archive path after a full restart.
- After restart, Typography's `TextStyle` reference opens TextStyle.kt:57 from
  the UI text Android sources JAR. `archive-native-textstyle.png` records it.
- `ComponentActivity` opens ComponentActivity.java:119 from the original activity
  sources JAR. That check exposed the first-Java-server environment/startup gap
  addressed by the startup and settings-refresh follow-ups above.

Final native verification passes on the optimized build at
`0a3893568f2d91ecff509fa922cd8ec098fb4bf8` (`archive-refresh-build.log`, 18m 02s).
The app restores ComponentActivity.java from its original archive. Both the Java
proxy and JDT JVM run with the real test project as their working directory
(`archive-refresh-native-java.json`), and the fresh session logs no errors.
`headlineSmall` again opens Typography.kt:94 at the exact Material3 Android
archive path; navigation from there opens TextStyle.kt:57 in its original UI
text archive. `archive-native-final.png` and `archive-native-final-textstyle.png`
record these checks. The Material3 JAR checksum remains unchanged.

The final managed build caches total 18.5 GiB, below the 30 GiB cleanup threshold
(`archive-native-final-result.json`). The existing scheduled cleanup remains
configured. All checks reported here ran locally; GitHub reported no checks for
the navigation draft PR at this checkpoint.

Native keyboard checks used **Cmd+B**, which reaches the same definition flow
as Cmd+click. The modifier-plus-mouse gesture was not directly automated in this
pass. The fixture's existing **Android: Configure Java** action also completes
successfully for `demoDebug`.

To update an existing project, relaunch the rebuilt app, run **Android: Configure
Kotlin** for the selected variant, and restart the IDE once so the runtime and
workspace settings are both fresh. This writes `externalSources.useArchiveUris`
and the selected variant's source archives. An existing random temporary source
tab can be closed; a fresh definition lookup opens the stable archive path.

## Device picker refresh — 15 September 2026

Both Android device selectors previously captured the device and AVD lists when
the toolbar rendered. Reopening the menu did not query ADB, so externally started
or stopped devices could keep their old state until an explicit refresh.

Opening either picker now starts the existing bounded discovery operation and
updates the open menu when the panel changes. The menu shows refresh progress,
disables stale device rows during discovery and marks the current selection.
Selection remains attached to the device/AVD rather than a stale keyboard row
number after a reorder. Successful selection still closes the menu. Errors direct
the user to the existing Android tool-window details and preserve Refresh devices.

This reuses Zed's persistent ContextMenu and entity observation. No background
polling loop or new dependency was added. Android Studio's
[DeviceProvisioner](https://android.googlesource.com/platform/tools/base/+/refs/heads/mirror-goog-studio-main/device-provisioner/src/main/com/android/sdklib/deviceprovisioner/DeviceProvisioner.kt)
combines device-provider state with ADB's live tracker. This pass covers refresh
on picker opening and discovery completion; continuous background changes while
a menu remains open, and stable physical identity across changed wireless
transport identifiers, remain future work.

Validation: the new GPUI regression replaces the device list while a menu is
open, reorders it and confirms through the same menu entity. It checks both
selection preservation and selecting a newly discovered device. All five
Android-UI tests and changed-crate Clippy pass. The optimized build passes in 14m 02s with debug information and incremental
artifacts disabled. Its embedded source revision is
`d3826af32454fe9e4516ef0cc3cd405f950ac00f`; later changes in this pass are documentation.

Native checks on the existing Pixel_6a test AVD:

1. Open the picker while the AVD is stopped and confirm the current physical
   device remains checked.
2. Close the menu and start the AVD externally. Reopen the picker: it discovers
   the AVD as running without selecting Refresh devices. Select it successfully.
3. Stop that exact test emulator externally and reopen the menu: it changes to
   stopped while keeping the AVD selected.
4. Open the Android tool-window picker: it shows the same selection and state.
   Restore the original physical device at the end.

No APK was deployed in this pass. The owned test emulator was stopped and its
AVD preserved. The source/preview workspace remains open in the new binary.
The initial rebuildable-cache measurement was 11.10 GiB, below the 30 GiB cleanup
threshold, so no caches were removed.

Local evidence is under `target/android-ide/validation/`: the
`device-menu-refresh-*` build/test/Clippy logs and `device-picker-*-20260915.png`
screenshots. Open-menu screenshots include local device identifiers and are kept
local; the closed-picker progress image excludes them.

The running-emulator toolbar currently displays the SDK model name, whereas the
picker displays its AVD name. Consistent naming remains a follow-up UI gap.

## Device and Compose navigation follow-up — 14 September 2026

The connected wireless ADB device used a valid mDNS serial containing a space.
Splitting every field on whitespace truncated its serial and treated the suffix
as its state, so selecting it could not produce an available device. The shared
parser now preserves the complete serial and separates it from a recognized ADB
state. The regression uses an invented serial, including the `(2)` mDNS suffix;
real device identifiers stay out of this report. Offline, unauthorized and
`no permissions` states remain visible.

Kotlin source lookup previously depended on PSI locations, which are absent for
many compiled Kotlin descriptors. The runtime now resolves their owning binary,
reads its SourceFile attribute, finds the attached original source and matches
the Kotlin declaration using PSI. This covers properties, constructor properties,
extension properties, overloads, objects versus same-named functions, implicit
companions and renamed JVM facades. It also preserves multi-dot source names such
as `StringResources.android.kt`.

The actual sample resolves `ComponentActivity`, `headlineSmall`, `typography`,
`MaterialTheme`, `Text`, `padding`, `dp`, `Modifier`, `Column` and `stringResource`.
The native editor opened the original `Typography.kt` declaration of
`headlineSmall` and the `MaterialTheme` object, rather than its same-named function.
A protocol trace confirmed the editor's definition requests and response ranges.

The same trace exposed duplicate Gradle discovery during server initialization.
An explicit root `kls-classpath` hook is now authoritative for that workspace;
projects without a hook retain ordinary build-system discovery. This reuses the
IDE's selected-variant model and avoids a second Gradle import. It does not provide
Gradle DSL semantic completion from that hook: Kotlin-script syntax highlighting
remains available, while a separate accurate Gradle DSL model is future work.

| Native Kotlin measurement | Before root-hook fix | After |
| --- | --- | --- |
| Initialize response | 16.9–20.5 seconds | 1.17 seconds |
| First diagnostics | 19.5–23.2 seconds | 3.22 seconds |
| Source navigation after initialization | 46–57 ms with descriptor fix | 79–124 ms in this run |

These are small-sample wall-clock measurements on this machine while the Rust
release build was running, not a benchmark against Android Studio. Standalone
source probes measured warm lookups around 10–60 ms and a first ComponentActivity
lookup around 1–2.4 seconds. JVM/compiler warm-up still exists; navigation before
server readiness can still return no result. No claim of instant universal
IntelliJ navigation or full Kotlin 2 semantic support is made.

The editor quick-action area now has **Show/Hide Compose Preview** for Kotlin
files in a trusted Android workspace. It uses the existing renderer and image
viewer. Hiding closes the preview image, preserving other tabs and unsaved edits;
showing it again reuses the rendered image for the same project and variant.
The Android tool-window action still refreshes the render after source changes.

Native physical-device verification: select a stopped AVD, then select the
connected physical device again. The toolbar and Android panel both show the
physical device and enable Run. A read-only `adb -s <complete-serial> get-state`
returns `device`; no APK was installed on the physical device in this pass.

Native preview verification: the new toolbar button rendered the fullDebug
fixture, then passed two hide/show cycles with the existing source tabs intact.
Cached reopening did not create another Gradle task. Showing the split currently
focuses the preview pane; retaining editor focus is a remaining polish item.

### Evidence and reproduction

- `cargo test --locked -p android_tools`: four tests pass, including wireless
  serial parsing and the existing project-artifact regressions.
- `cargo test --locked -p android_ui`: four tests pass, including a GPUI test that
  hides a preview beside another tab and preserves unsaved source without saving.
- Pinned Kotlin runtime: rename, definition, attached-source, workspace-symbol and
  explicit-classpath tests pass. The binary-definition fixture compiles real
  Kotlin dependencies and checks exact original-source positions twice.
- `./script/clippy -p android_tools -p android_ui --features gpui/inspector` passes.
- Optimized app build passes in 20m 50s using `CARGO_PROFILE_RELEASE_DEBUG=0`,
  `CARGO_INCREMENTAL=0` and four Cargo workers. The executable embeds commit
  `32dc5e7d51d28655e4542383194842b1153e8c17`; subsequent changes are the separately
  installed Kotlin runtime `+android-sources-5` and documentation. The linker and
  `block 0.1.6` future-compatibility warnings remain upstream build warnings.
- Local evidence is under `target/android-ide/validation/`: `native-navigation-timings.json`,
  `compose-navigation-after.json`, `native-headlineSmall-source.png`, and the
  `device-compose-*` / `compose-runtime-final-tests.log` logs. Protocol tracing was
  temporary and the project's normal language-server settings were restored.

### Android Studio source findings and next comparison passes

The [Android Studio source guide](https://android.googlesource.com/platform/tools/base/+/studio-master-dev/source.md)
is a checkout/build entry point for several repositories. A full checkout is not
required for this comparison. The [Compose preview provider](https://github.com/JetBrains/android/blob/master/compose-designer/src/com/android/tools/idea/compose/preview/ComposePreviewRepresentationProvider.kt)
checks Kotlin files, Compose modules, source rather than library files, and
preview annotations. Its [preview representation](https://github.com/JetBrains/android/blob/master/compose-designer/src/com/android/tools/idea/compose/preview/ComposePreviewRepresentation.kt)
coordinates activation, build invalidation, refresh and renderer disposal. These
are the appropriate behavior references for the next preview pass; no JetBrains
implementation was copied into GPUI.

IntelliJ navigation uses a persistent semantic project model. The
[Kotlin Analysis API](https://kotlin.github.io/analysis-api/index_md.html) exposes
PSI-based symbol resolution with cached analysis, while its standalone mode is
still under development. The official [Kotlin LSP](https://github.com/Kotlin/kotlin-lsp)
is based on IntelliJ/Kotlin plugin infrastructure and currently labels Android
Gradle support experimental. Its [263.4702.0 release notes](https://github.com/Kotlin/kotlin-lsp/blob/main/RELEASES.md)
include source downloads, library workspace symbols and an AGP import fix. Evaluate
it against this fixture and a larger project before changing the working default;
it is not a proven drop-in replacement, and no expiration check was bypassed.

| Comparison area | Current coverage | Next concrete acceptance check |
| --- | --- | --- |
| Device picker | Running devices and stopped AVDs; fresh discovery on opening; selected-device checkmarks | Continuous changes while the menu stays open, stable identity across wireless reconnects and consistent AVD toolbar names |
| Source navigation | Ten Compose/Android symbols and compiled overload fixtures | Nested library-to-library navigation, persistent library tabs across server restarts, larger Kotlin 2 projects, per-source-set classpaths and readiness feedback |
| Preview controls | Toolbar visibility toggle, annotation picker, reusable render split | Limit button to source files in Compose modules; per-file selection, dirty-source refresh, Code/Split/Design behavior |
| Project and variants | Trusted automatic sync, selected variant restoration, flavor builds | Edit Gradle dependencies and switch source sets without manual language setup; actionable sync errors |
| Keymaps and search | Common JetBrains actions, double Shift, floating Find/Replace | Compare remaining native menu bindings and search scopes systematically on macOS |
| Resources and XML | Syntax colors and R-to-XML navigation | Qualifier/overlay precedence, resource usages and dependency resource navigation |
| Logcat and build output | Left rail action and task terminals | Structured process/severity filtering, pause/clear, clickable file locations |
| Debugging | Breakpoints, frames, variables, step/continue | Kotlin inline/coroutine stepping, expression evaluation, reconnect and multiple processes |
| Studio tools | Basic run/build/test/preview workflows | Device manager UI, SDK manager, profiler, layout inspector, Apply Changes and signing workflows remain absent |

Work proceeds one reproducible gap at a time: record Android Studio behavior,
trace the equivalent Zed flow, reuse an existing component, add the smallest
meaningful regression, validate in the native app and update the owning stack
layer. This matrix is a comparison backlog, not a claim that every Android Studio
element or function already matches.

The existing daily 04:00 India-time maintenance follow-up now combines a bounded
parity pass with the previously requested cache cleanup. It preserves the 30 GiB
threshold, idle-build checks and protected binaries/dependencies. Meaningful
progress is sent through the approved Telegram destination; unchanged state stays
quiet. Full parity is ongoing work.

## Run it

The optional language/debug/preview tools are installed in this checkout. To
reproduce their installation on Apple Silicon with JDK 21, run:

```sh
script/install-android-kotlin
script/install-android-debugger
script/install-android-preview
```

Each script verifies pinned source/artifact checksums, preserves an unmanaged
installation, and is repeatable. Kotlin and the debugger build with a task-local
JDK 11; execution uses JDK 21. Preview uses a separate short-lived JVM, without
starting Android Studio or an emulator.

From this checkout, launch the optimized app:

```sh
script/android-ide --release --skip-build examples/android-ide
```

The launcher creates a macOS development bundle under `target/android-ide` and
uses `target/android-ide/profile` for configuration and extensions. It preserves
existing settings. New profiles disable auto-update and telemetry, enable the
Kotlin and Java extensions, and select the community Kotlin server. This is a development
launcher, not a signed installer or an independently branded release.

1. Open the Android tool window using the hammer on the right tool rail.
2. Trusted Android projects sync automatically on open. Four runnable variants
   should appear for `:mobile`. After Gradle edits, save and choose **Sync project**.
   Sync reads files from disk;
   Build/Run/Test/Lint use the task system's save-before-run behavior.
3. Select **:mobile · demoDebug** or **:mobile · fullDebug**.
   The choice is remembered for this project and restored after the next sync.
4. Choose **Configure Kotlin**. It builds that variant, exports the evaluated
   compile classpath, and configures the project with a JDK 21 language server.
   The Kotlin extension must be installed; new launcher profiles request it
   automatically. Repeat setup after upgrading this prototype or changing variants
   or dependencies, so existing projects receive the corrected source settings.
5. Select a connected device or a stopped AVD in the top device picker.
   **Run** and **Debug** start a stopped selection and wait for it before deploying.
6. Choose **Run**. The app displays the selected flavor, its application ID,
   and `Android library connected`.
7. Use **Test**, **Lint**, or **Open Logcat** as needed. Build output remains in
   ordinary task terminals. Ctrl+C interrupts the focused command or Logcat.
8. Choose **Configure Java** for Android-aware Java editing; repeat Java and
   Kotlin setup after a variant/dependency change. Open a Java file to start its
   import, then allow JDT LS to finish before testing navigation.
9. Set Java/Kotlin breakpoints and choose **Debug**. Use the native debugger's
   variables, frames, step/continue and disconnect controls. The adapter supports
   local/field evaluation; arbitrary expression evaluation is not implemented.
10. Use the eye icon beside the Kotlin editor actions to show/hide Compose preview.
    Choose **Compose preview** in the Android tool window to refresh, and
    **Select preview…** for Default or Large text. Refresh after editing. Preview works without a selected device and preserves
    the previous image on failure. Its editor split is reused across refreshes.
11. Use **Stop emulator** when finished. This preserves the AVD and releases its
   VM memory. It is disabled for a physical device.

The project is a real Gradle build and participates in Zed's existing workspace
trust flow. Sync and build execute project code only after the project is
trusted. Opening an arbitrary folder does not automatically build it or install
SDK packages.

## What is implemented and what was actually checked

| Area | Result and scope |
| --- | --- |
| Appearance | Studio Dark and Studio Light render in the native app. Bundled JetBrains Mono, compact typography, left Project pane, side tool rails, bottom output, and top Android controls are present. |
| Layout | Both themes checked at the normal window size; a smaller light-theme window retained an editor and scrollable Android controls. Dock geometry and panel movement have GPUI regression coverage. |
| Keybindings | Existing JetBrains map is the default. Native macOS Build, Run, Sync, and Logcat were exercised. Linux/Windows asset precedence is tested, but those operating systems were not run. |
| Project sync | Uses evaluated Android CLI output. Single-module reference and two-module smoke project both synchronize. Libraries are dependencies, not misleading runnable targets. |
| Variant handling | `:mobile` has demo/full flavors and debug/release build types. Both debug flavors build, test, and deploy with distinct application IDs. No `:app` assumption. |
| Variant restoration | The selected module/variant is stored in Zed's local database, separate from project files. A real quit/relaunch/Sync restored `:mobile · fullDebug`, and Run deployed that flavor without reselection. Fresh sync supplies current artifact paths. A removed variant remains unselected with a visible explanation. Tests cover project isolation and changed output paths. |
| Devices | Explicit serial selection; offline and unauthorized entries retained. Missing device state is visible. Physical-device stop is rejected in tests. |
| Emulator lifecycle | Native Start/Stop exercised on the existing `medium_phone` AVD. The optimized build also started `Pixel_6a` and deployed both flavors there. Stop refreshes the device list without deleting or recreating AVDs. |
| Build and Run | Save-before-build, selected variant, AGP output metadata, explicit device, and visible output. Build failure prevents deployment. Repeated run after fixing an intentional Kotlin type error showed the changed greeting on the emulator. |
| APK selection | Existing AGP metadata/redirects are read. The implementation accepts a single universal APK and rejects ambiguous, filtered/split, missing, traversing, or variant-mismatched artifacts. Split APK installation is deferred. |
| Test and Lint | Native task actions passed. Direct smoke-project builds/test tasks passed for both debug flavors; lint passed with warnings listed below. |
| Logcat | Real selected-device `adb logcat -v threadtime` in a terminal with bounded history. Ctrl+C stops the stream. No dedicated filter/table UI yet. |
| Kotlin Android APIs | Android/Compose resolution, useful hover/completion, and deliberate type-error diagnostics exercised on the reference project. |
| Generated symbols | Smoke `R`, `BuildConfig`, and a Kotlin call to the app's Java helper resolve after setup. Variant changes regenerate the selected `R.jar` and Java output classpath. |
| Library navigation | Native hover and Cmd+B navigate from `MainActivity.kt` to `LibraryGreeting.kt` in `:greeting`. The library's compiled classes JAR appears in the exported classpath. |
| Kotlin rename | The original 1.3.13 object-rename failure is fixed by the pinned upstream build. Native Shift+F6 changed the object, imports and usages across three Kotlin files; all temporary edits were restored. Java callers are not included, so mixed-language rename is not supported. |
| Java editing | Configure Java imports the evaluated AGP compile graph into JDT LS. Native SDK/resource/library hover, type errors and fullDebug-to-demoDebug BuildConfig navigation passed without clearing JDT caches. Existing preferences/comments and unrelated Gradle arguments are preserved. |
| Android Debug | Native build/deploy/debug-wait/attach passed. Java and Kotlin breakpoints, variables, Shift+F8 into the Compose caller and Ctrl+F2 detach were exercised. A portable DAP smoke test checks local evaluation, configuration completion, retained app process and removed owned forward. |
| Compose preview | Downloaded Google renderer/layoutlib produce real images in the native editor split. demoDebug/fullDebug use the correct resources, Java/Kotlin libraries and application IDs. Default/Large text selection works with no emulator and Android Studio closed. An intentional preview exception preserved the last image byte-for-byte and showed an error. |
| Cancellation | A temporary 60-second Gradle task was interrupted using Ctrl+C in the native terminal. Controls recovered, and the restored build passed in 2 seconds. Current task API displays interruption as command failure. |
| Configuration safety | Existing user hooks are preserved with an actionable error. Generated files use atomic replacement. Settings updates preserve unrelated settings/comments and reject stale concurrent settings edits. |
| Isolation | Development launcher profile, bundle identifiers, generated caches, telemetry/update defaults, existing-profile preservation, relative path handling, and running-app guard were checked. |
| Helper cleanup | After quitting the Java probe, a process check found no remaining IDE or language-server commands associated with the isolated profile. Shared Gradle/ADB services are separate; this is an app-quit check, not a project-close stress test. |

## Shortcut reference

| Operation | macOS | Linux/Windows mapping |
| --- | --- | --- |
| Find Action | Cmd+Shift+A | Ctrl+Shift+A |
| Build selected Android variant | Cmd+F9 | Ctrl+F9 |
| Run selected Android variant | Ctrl+R | Shift+F10 |
| Debug selected Android variant | Ctrl+D | Shift+F9 |
| Continue / step out / disconnect | Cmd+Option+R / Shift+F8 / Cmd+F2 | F9 / Shift+F8 / Ctrl+F2 |
| Search Everywhere / Go to Class | Double Shift / Cmd+O | Double Shift / Ctrl+N |
| Find / Replace in Files | Cmd+Shift+F / Cmd+Shift+R | Ctrl+Shift+F / Ctrl+Shift+R |
| Sync Android project | Cmd+Option+Y | Ctrl+Alt+Y |
| Open selected-device Logcat | Cmd+6 | Alt+6 |

Sync originally lost to the default editor's Git Stage shortcut. The regression
test first reproduced `git::ToggleStaged` instead of `android::SyncProject`, then
passed after the Android bindings were given full-editor context as well as
workspace context. Native editor-focus Sync was rechecked after that fix.

## Language-server research changed the implementation choice

The official Kotlin LSP advertises experimental AGP support and remains Alpha;
KMP support is still under development. These are vendor-supported scope
statements, not evidence that this fixture works with that server.
[Kotlin documentation](https://kotlinlang.org/docs/kotlin-lsp.html).

The installed Zed Kotlin extension was version 0.3.2. Its default official
server download, `262.9593.0`, exited because that build had expired when tested
on 13 September. No expiry check was bypassed. JetBrains separately documents
30-day preview-build expiry for its Java/Kotlin VS Code integration and an
Ultimate subscription requirement after preview; do not assume this is a free,
unrestricted redistribution path for this fork.
[JetBrains installation and preview terms](https://www.jetbrains.com/help/intellij-vscode/get_started_vs_code.html).

The initial compatibility path used `fwcd/kotlin-language-server` 1.3.13 with
JDK 21 and its documented project classpath hook. JBR 25 produced a Java-version
parsing error, so Gradle and language-server Java runtimes are selected
separately. The community repository now describes itself as deprecated in
favor of the official server. Its successful diagnostics/navigation here do not
make it a sustainable production language engine.
[Community server status and classpath hooks](https://github.com/fwcd/kotlin-language-server).

The generated Gradle init task reads the selected Kotlin task's libraries and
the corresponding Java compiler output. The latter is necessary for generated
`BuildConfig` and Java helpers. It works when the Java task is `NO-SOURCE` and
with Gradle configuration-cache reuse. It does not parse Gradle source code or
change the user's Gradle build files.

Known ceiling: one selected variant supplies a workspace-wide classpath. This
does not model different variants in different modules, test-specific
classpaths, KMP, included builds, or source-set visibility with Android Studio's
precision. The pinned upstream build below resolves the reproduced rename crash;
a maintained production engine and broader project-model fixtures remain required.

### Fixes verified during the continuation

`script/install-android-kotlin` builds MIT-licensed upstream commit
`6d9e61b79d4631e75516def7ab7ff0d8b9310467`. Its existing front-end exception
fallback fixes the object-rename crash. The bootstrap runs upstream rename tests;
an additional disposable regression covered object rename with a shadowed
parameter. The native IDE then renamed the library object across Kotlin files.
This does not fix Java caller renaming or the workspace-wide classpath ceiling.
[Source revision](https://github.com/fwcd/kotlin-language-server/tree/6d9e61b79d4631e75516def7ab7ff0d8b9310467).

The default and experimental JDT LS Android imports initially failed on AGP 9.4.0.
Configure Java now exports each actual compile task in the selected Gradle task
graph, applies the standard Eclipse model, and updates the root project through
JDT LS. Updating only a module left stale Gradle arguments; refreshing the root
fixed full-to-demo navigation without deleting caches. The JRE container must be
preserved, and task dependencies cannot be read during `projectsEvaluated` under
Gradle 9. Those findings are covered by integration evidence and the review guide.
Java extension 6.8.26 / JDT LS 1.61.0 resolved SDK APIs, R, BuildConfig, compiled
Kotlin libraries and deliberate type errors in the native IDE.
[Java extension](https://github.com/zed-extensions/java),
[JDT LS](https://github.com/eclipse-jdtls/eclipse.jdt.ls).

Android debugging uses MIT-licensed `fwcd/kotlin-debug-adapter` revision
`7f05669b642d21afa46ac7b75307fa5d523a7263`, built with the pinned Kotlin server's
shared module and Kotlin 2.1.0. The checked-in patch completes configurationDone,
registers the intended class-prepare filters, removes obsolete breakpoint
requests, tolerates optional source names, and disposes attached VMs on detach.
The installed adapter passed upstream tests and a real emulator protocol test;
the native UI passed Java/Kotlin breakpoints and stepping. The ordinary Java DAP
adapter was separately proven, but its JDT source lookup did not resolve Kotlin
frames correctly, so it is not the default Android attach path.
[Adapter source](https://github.com/fwcd/kotlin-debug-adapter/tree/7f05669b642d21afa46ac7b75307fa5d523a7263).

A final native test replaced the adapter executable with `/usr/bin/false` for one
isolated process. That exposed a shared Zed DAP disconnect race. Closing request
registration on EOF/write failure, and isolating request maps across TCP
connections, fixes it. The deterministic regression failed before the change;
all four DAP tests pass afterward, including reconnect. The same native failure
now prints “debugger shutdown unexpectedly,” ends its session and removes the
forward automatically. Quitting the earlier failed instance also removed its
owned forward. The real adapter is restored simply by launching without that
process-local test override. Control-D was exercised with editor focus. The real
adapter was then rechecked: Java and Kotlin breakpoints, step-out and detach
passed; application PID 5187 remained alive with an empty forward list.

The cold-launch check also reproduced Android CLI returning before the new app
process exists. Process discovery now retries up to 20 times with a 250 ms gap
and a one-second timeout per ADB command, then surfaces the final failure. A
GPUI regression checks delayed success, bounded failure and invalid PID output.
Successful PID output is still validated before creating the forward. The final
debug build then reached both Java and Kotlin breakpoints on its first cold
launch after force-stopping the disposable full-flavor app.

Compose preview pins Google renderer `0.0.1-alpha15` and all layoutlib components
to `16.2.4`. The newer renderer alpha16 and published layoutlib 17.0.1 had an API
mismatch; mixing native layoutlib versions also failed. The tested matching set
renders independently of Android Studio. Google's compiled preview detector
supplies method names and parameters. A small bridge exits after the alpha15
renderer disposes its framework, because that version otherwise leaves worker
threads alive. Render failures are checked in the result JSON, not inferred from
an exit code. Runtime R classes come from the evaluated resource-processing task;
compile-only R classes omit dependency resource IDs required by Compose.
[Google screenshot tooling](https://developer.android.com/studio/preview/compose-screenshot-testing),
[release notes](https://developer.android.com/studio/preview/compose-screenshot-testing-release-notes).

## Reproducible build environment

| Component | Verified version/location |
| --- | --- |
| Host | Apple Silicon, 16 GiB RAM, macOS 26.6.2 |
| Rust | Repository-pinned 1.98.1, installed with rustfmt, Clippy, rust-src and rust-analyzer |
| Xcode | 26.6, build 17F113; Metal compiler available |
| Android Studio reference | Quail 4, 2026.1.4, `/Applications/Android Studio.app` |
| Gradle JVM used by launcher | Android Studio JBR 25.0.3 when `JAVA_HOME` is unset |
| Kotlin JVM | Temurin 21.0.12.1 in `/Library/Java/JavaVirtualMachines/temurin-21.jdk/Contents/Home` |
| Android CLI | 1.0.16261425, `/opt/homebrew/bin/android` |
| SDK | `/Users/anil/Library/Android/sdk`; platform 37 used for fixture |
| Smoke build | AGP 9.4.0, Gradle 9.6.1, Kotlin Compose plugin 2.2.10, Compose BOM 2026.02.01 |
| Tools installed for this session | Rust components, official gh-stack extension, task-local JDK 11, pinned Kotlin server/debugger builds, Google Compose renderer/layoutlib |
| Tools reused | Xcode/Metal, CMake, Ninja, Homebrew, Android Studio/SDK/CLI, JDK 21, ADB/emulator, telegram-send |

The upstream LiveKit build helper could not retrieve its WebRTC archive through
its normal download path. The matching official archive was downloaded directly
and its extracted headers/libraries were supplied through the helper's supported
`LK_CUSTOM_WEBRTC` override. A copy is retained at the following task-local path:

```sh
export LK_CUSTOM_WEBRTC="$PWD/target/android-ide/dependencies/webrtc-0001d84-4/mac-arm64-release"
cargo build --release -p zed --bin zed --locked -j 4
```

If that ignored cache is removed, retrieve the matching
[official WebRTC archive](https://github.com/zed-industries/livekit-rust-sdks/releases/download/webrtc-0001d84-4/webrtc-mac-arm64-release.zip)
again or let upstream's download work. Do not reuse these prebuilt libraries
after a LiveKit/WebRTC ABI update without checking the dependency's tag.
The launcher also honors `CARGO_TARGET_DIR` and `CARGO_BUILD_JOBS`.

The Gradle wrapper was regenerated using Gradle 9.6.1 and its JAR hash was
matched to the official checksum:

```text
wrapper JAR SHA-256:
497c8c2a7e5031f6aa847f88104aa80a93532ec32ee17bdb8d1d2f67a194a9c7
distribution SHA-256 (pinned in gradle-wrapper.properties):
9c0f7faeeb306cb14e4279a3e084ca6b596894089a0638e68a07c945a32c9e14
```

Sources: [wrapper checksum](https://downloads.gradle.org/distributions/gradle-9.6.1-wrapper.jar.sha256),
[distribution checksum](https://downloads.gradle.org/distributions/gradle-9.6.1-bin.zip.sha256).

## Checks performed

The following checks passed on the combined source changes, including the
shortcut-context and emulator-stop fixes:

```sh
cargo test -p android_tools -j 6
cargo test -p android_ui -j 6
cargo test -p workspace test_toggle_docks_and_panels -j 6
./script/clippy -p android_tools
./script/clippy -p android_ui
./script/clippy -p workspace -p project_panel -p ui --features gpui/inspector
bash -n script/android-ide
git diff --check
git -c core.whitespace=blank-at-eol,blank-at-eof,space-before-tab,cr-at-eol \
  diff --check main..HEAD -- . ':(exclude)assets/fonts/jetbrains-mono/OFL.txt'
```

The complete-stack whitespace check accepts the generated Windows wrapper's
CRLF endings and excludes the unmodified vendored font license, which has one
upstream trailing space. The pinned debugger patch preserves upstream context
whitespace through a scoped Git attribute. An unconfigured `git diff --check main..HEAD` reports
those vendor-file differences; the source/doc check above passes.

Set `LK_CUSTOM_WEBRTC` as above for commands that build the relevant native
dependencies. The core crate has two focused tests covering parsers/artifact
selection and the real filesystem/classpath hook. The Android GPUI test covers
trust, multiple roots, device state, action dispatch, dock movement, settings
preservation, persisted variant selection, and keymap precedence. The existing workspace dock regression was
extended to catch an editor that accidentally collapses after adding tool rails.

The shell Clippy command explicitly enables `gpui/inspector`: without it, the
upstream all-features UI build fails on its inspector derive feature wiring.
No unrelated upstream source fix was added. Release linking also emits an
upstream large-`__eh_frame` warning; Cargo reports future incompatibility in
`block` 0.1.6. These were warnings, not failed validation.

Smoke-project verification:

```sh
cd examples/android-ide
export JAVA_HOME='/Applications/Android Studio.app/Contents/jbr/Contents/Home'
export ANDROID_HOME="$HOME/Library/Android/sdk"
./gradlew :mobile:assembleDemoDebug :mobile:assembleFullDebug \
  :mobile:testDemoDebugUnitTest :mobile:testFullDebugUnitTest \
  :mobile:lintDemoDebug
```

With the Android library module added, this passed in 9 seconds: 135 actionable
tasks, 72 executed and 63 up to date. These are warm local timings, not a build
speed comparison. Lint had five warnings: newer Gradle, Compose BOM and Activity
versions exist; the smoke manifest has a backup-policy warning and no launcher
icon. No lint errors. Versions were kept consistent with the verified fixture
rather than expanding this test into dependency upgrades.

## Historical optimized builds and initial performance measurements

The initial ten-layer prototype compiled in **18 minutes 26 seconds** with four
jobs. That executable reported `1651d598ccc367f3b1a6dbbc46ef5612a9c59103` and
passed native restart/Sync variant restoration, emulator startup, and Ctrl+R
build/deploy of `:mobile · fullDebug` on `medium_phone`. Stop emulator completed
and ADB listed no devices. These results precede the Java/debugger/preview work.

An earlier optimized build passed in 18 minutes 53 seconds using four jobs and
reported `93d7661d5f36e208286d10dfd4be459332441142`. Both flavors deployed on
`Pixel_6a` after the library fixture was added. Repeated debug builds were used
for shorter test/fix cycles. Neither historical revision should be described as
the final continuation binary; the final build is recorded below.

### Warm startup observation

Three launches of the optimized `93d7661d5f` executable against the same already-used
profile and smoke project produced **0.367, 0.169, and 0.165 seconds** from process
creation to receiving Zed's first `Rendered first frame` log. Median: **0.169
seconds**. A monotonic Python timer read the child process's PTY; no UI-automation
tool latency is included. The actual sample-project window was checked after
each launch. No Rust compilation was running during these trials.

This log is emitted at the first workspace render call. It does not establish
frame presentation, editor input latency, project sync completion, or language
server readiness. These are warm-cache observations, with no Android Studio
startup comparison. Raw `startup-1.json` through `startup-3.json`, corresponding
logs, and the measurement script are retained in the local validation directory.

### Idle memory observation

Three samples were taken from 22:27:52 through 22:28:32 IST with the original
small Android Studio-generated Compose project open in both IDEs. Android Studio
and the optimized fork were idle; no Rust compiler was running during sampling.
The measurements used `vmmap` physical footprint because earlier compilation
had caused memory compression, making RSS alone misleading.

| Process or process group | Median physical footprint | Range |
| --- | ---: | ---: |
| Optimized Android IDE editor | 297.8 MiB | 297.7–297.8 MiB |
| Its Kotlin language server | approximately 1.30 GiB | rounded by vmmap |
| Editor plus Kotlin server | approximately 1.59 GiB | approximately 1.59 GiB |
| Android Studio | approximately 3.10 GiB | rounded by vmmap |
| Shared Gradle daemon (JBR 25) | approximately 1.80 GiB | rounded by vmmap |
| Shared Kotlin compiler daemon | 553.7 MiB | 553.7 MiB |
| Additional Gradle toolchain daemon | 701.6 MiB | 701.6 MiB |
| Shared emulator | approximately 5.30 GiB | rounded by vmmap |

The editor-plus-language-server group was about 49% smaller in these samples.
This is an initial observation on one small project, not a feature-equivalent
benchmark or a product-wide memory claim. Server functionality, JVM versions,
process age, compression, and warm caches differ. Gradle and emulator costs are
substantial and do not disappear when changing editors. Android Studio and the
fork may also cause distinct compatible/incompatible Gradle daemons to coexist.

The measured optimized executable reported revision `c0bf3f23`, an earlier
implementation build with the shell and Kotlin setup. Subsequent fixes add
generated Java outputs, emulator controls, and shortcut precedence. Do not label
this sample set as a measurement of the final source revision.

Raw JSON and all three per-process `vmmap` reports are retained in
`target/android-ide/validation/memory-baseline.json` and
`target/android-ide/validation/baseline-*.txt`. This ignored directory is local
evidence, not a committed binary payload.

## Continuation checks and evidence

The continuation adds four independent implementation layers after the initial
validation commit. Focused tests now cover four Android tool tests and three native
panel/debugger tests. Four DAP tests cover transport recovery, and five image-viewer
tests cover reload and asset lifetime behavior. The original
dock/panel tests remain relevant because the
new features reuse the existing workspace and debugger views.

```sh
cargo test -p dap -p android_tools -p android_ui -p image_viewer -j 6
./script/clippy -p dap -p android_tools -p android_ui -p project -p image_viewer \
  --features gpui/inspector -j 6
script/test-android-debugger --device emulator-5554
```

The optimized verification before the image-metadata correction used source
`a602e3b17768f5eebc8978373116493c5d47bb30`. That build passed in 7 minutes 47
seconds, restored fullDebug, rebuilt the app, configured both language servers,
and rendered Compose with Android Studio and the emulator closed. Preview-size
switching then exposed stale image metadata in the existing shared loader.

Final source `371380b5a7420ba8d937c9b642e8feaec3c1078e` refreshes metadata with the
new image bytes before notifying the existing viewer. Its real-file regression
failed with stale 1×1 dimensions before the change and passed with 2×1 afterward.
All five image-viewer tests passed, including asset-cache and split-pane checks.
The native rebuilt app then updated the existing pane from 681×399 to 845×480
and back, with the correct file size and working Fit to View.
The combined 16 tests and strict Clippy pass on that complete source. Its debug
build completed in 1 minute 10 seconds with the warm local cache. The final
documentation commit does not alter the native source. The optimized build of
this exact source completed successfully in **17 minutes 10 seconds** with six
jobs; only the documented upstream linker/future-compatibility warnings remain.

The debugger smoke requires the installed adapter, a running explicitly selected
emulator, and an assembled demoDebug fixture. It affects only the sample app and
its own port forward. Both demo/full local unit-test and lint tasks passed after
the named Compose previews were added. The intentional rendering exception was
removed and the original successful image remained unchanged throughout failure.

| Evidence under `target/android-ide/validation` | What it establishes |
| --- | --- |
| `java-summary.txt`, `java-native-sdk-hover.png`, `java-native-type-diagnostic.png` | Native Android Java resolution, diagnostics and variant import findings. |
| `kotlin-pinned-summary.txt`, `kotlin-upstream-native-rename.diff`, `kotlin-upstream-object-rename.png` | The actual Kotlin-only object rename and its scope. |
| `kotlin-pinned-install.log`, `kotlin-head-rename-tests.log` | Reproducible source install and upstream regression evidence. |
| `debugger-summary.txt`, `debugger-installed-smoke.log` | Java/Kotlin stops, local evaluation, handshake, retained app process and forward removal. |
| `dap-disconnect-before.log`, `dap-disconnect-after.log`, `debugger-startup-recovery.png` | Reproduced transport failure, fixed regression and native failure recovery. |
| `debug-native-kotlin-step.png` | Native stepping from the Kotlin library into its Compose caller. |
| `preview-summary.txt`, `compose-native-workspace.png` | Native standalone Compose rendering and side-by-side source/image layout. |
| `compose-native-full-default.png`, `compose-native-full-large.png` | Distinct default and large-text renders from fullDebug. |
| `preview-native-tests.log`, `preview-native-clippy.log` | Preview model, safe output handling and native panel checks. |
| `debug-process-wait-tests.log`, `debugger-cold-start-fixed.png` | Readiness regression and first-attempt native Java/Kotlin cold-start attachment. |
| `debugger-real-adapter-recovery.png` | Real-adapter stepping after the shared transport fix. |
| `final-sample-tests.log` | Both variants' unit tests and Android lint. |
| `final-combined-tests.log`, `final-combined-clippy.log` | All 16 focused tests and strict Clippy on the complete continuation source. |
| `final-debug-build.log`, `final-release-build.log`, `final-build.json` | Final-source debug and optimized compilation, with binary hash. |
| `final-optimized-workspace.png`, `final-memory.json`, `final-memory-summary.log` | Final running workspace and editor/language-server memory costs. |
| `preview-metadata-before.log`, `preview-metadata-after.log`, `preview-metadata-clippy.log` | Reproduced stale metadata and verified shared image reload fix. |
| `preview-metadata-native-large.png`, `preview-metadata-native-default.png` | Correct dimensions and file size after native preview-size switches. |

These artifacts are local and ignored. Telegram updates were sent to the
explicitly approved configured destination, including native debugger and Compose
images. They contain only this test IDE/project evidence.

## Historical running build and process snapshot

The optimized `371380b5a7420ba8d937c9b642e8feaec3c1078e` executable is left open
on the smoke project. Sync restored `:mobile · fullDebug`, and the existing source
and real Compose preview are visible. Both language servers are running. Android
Studio and the test emulator are closed, and `adb forward --list` is empty.
The final workspace screenshot is `final-optimized-workspace.png`.

Binary SHA-256:
`05c66620bdb7b122d6318f3ea00f97b86f09756efd50e3f6efd8d83a5902b632`.
`final-build.json` records the source revision, size and completed checks.

Three physical-footprint samples were taken from 09:29:19 through 09:29:44 IST on
14 September with no Rust compiler running. These are post-startup observations
on the two-module fixture, not a controlled large-project benchmark. The first
sample still had editor/Kotlin activity; the latter two showed about 0.4% editor
CPU and 0% for the language servers.

| Process group | Median physical footprint | Range |
| --- | ---: | ---: |
| Optimized editor | 334.3 MiB | 322.3–386.8 MiB |
| Pinned Kotlin server | approximately 1.40 GiB | rounded by vmmap |
| Java language server | 695.6 MiB | 695.6–699.5 MiB |
| Java proxy | 1.48 MiB | 1.48 MiB |
| Editor plus Java/Kotlin support | approximately 2.41 GiB | approximately 2.40–2.46 GiB |

The owned terminal shell adds about 5.31 MiB. macOS denied `vmmap` access to its
protected `/usr/bin/login` parent; that footprint is recorded as unavailable,
not zero. Shared Gradle/ADB services are outside this process group. The earlier
Android Studio measurement used a different state and fixture, so no percentage
improvement over Studio is inferred from these final samples. Language-server
cost remains the main memory target for future work.

`final-memory.json`, `final-memory-summary.log`, and `final-memory-*.txt` retain
all measurements, CPU/RSS observations and the unavailable-helper result.
`measure-final-memory.py <editor-pid>` reproduces the scoped sample and verifies
that the PID belongs to the isolated optimized app.

## Remaining acceptance gates

1. **Language engine and project model:** a maintained production Kotlin engine,
   mixed Java/Kotlin refactoring, test/source-set visibility, KMP and included
   builds. The observed object rename and basic Android Java import failures are
   fixed; they are no longer pending implementation tasks.
2. **Advanced debugging:** coroutine/inline/SMAP mappings, complex expressions,
   deep multi-module source roots, process-death/reconnect stress and NDK/LLDB.
   Basic Java/Kotlin deploy/attach, variables, stepping and detach are implemented.
3. **Preview breadth:** XML design tools, interactive Compose/live edit,
   multi-value PreviewParameter galleries, larger dependency graphs and broader
   device/theme/locale matrices. Static Compose rendering, variant correctness,
   annotation selection and failure preservation are implemented.
4. **Project view and device tools:** logical Android grouping, native Logcat
   filters, AVD/SDK creation and management, split APK support, and an embedded
   emulator need their own tested increments.
5. **Performance:** repeat measurements on representative large projects, record
   input/frame latency and LSP-ready time, test repeated sync/close cycles, and
   include total helper-process cost. Deep file scanning is enabled so nested
   Kotlin files appear in projects without Git; its cost on large or overly
   broad roots still needs measurement. No responsiveness comparison is
   inferred from editor memory alone.
6. **Distribution:** validate Linux and Windows; separate product identity,
   logs/caches/update/crash policy from upstream; review license obligations;
   provide a signed installer and upgrade path. The current launcher is macOS
   development tooling only.

## Local evidence

The ignored `target/android-ide/validation` directory preserves the actual
screenshots and logs without adding build artifacts to the review stack:

| Artifact | What it demonstrates |
| --- | --- |
| `zed-android-studio-reference.png` | Installed Android Studio layout. This early capture predates recovery of the reference project's initial sync failure. |
| `zed-android-studio-shell.png` | Running native fork with Studio-style tool rails and build/run output. |
| `zed-final-native-run.png` | Initial ten-layer optimized executable after full-flavor deployment on `medium_phone`. |
| `saved-variant-restored.png` | `fullDebug` restored by Sync after a real app quit and relaunch. |
| `zed-android-studio-light.png` | Real light-theme editor, project tree, Android controls, and successful build. |
| `zed-android-narrow.png` | Smaller window with Android controls still reachable by scrolling. |
| `smoke-full-final.png` | The full flavor on Pixel_6a, including its application ID and Android library output. |
| `zed-android-cancel.png` | Deliberately interrupted Gradle task and the current interruption-as-failure UI. |
| `zed-android-rename-error.png`, `zed-kotlin-rename-server-error.png` | Reproduced community Kotlin rename failure. |
| `zed-java-android-import-error.png`, `jdtls-android-import.log`, `AndroidIdeJavaProbe.java` | Failed Android Java import after enabling JDT LS Android support. |
| `zed-final-combined-tests.log`, `zed-final-combined-clippy.log` | Combined stack checks after the variant-restoration change. |
| `zed-selection-release-build.log`, `zed-final-system-specs.txt` | Initial ten-layer optimized build and its source revision. |
| `memory-baseline.json`, `baseline-*.txt` | Three raw per-process memory samples. |
| `helper-cleanup.json` | Scoped process check after closing the development app and Java probe. |
| `startup-*.json`, `startup-*.log`, `zed-startup-measure.py` | Monotonic warm first-workspace-render measurements and harness. |

All requested progress messages were sent using `telegram-send` to the
explicitly approved configured destination. Screenshots show running local
software, not generated mockups. The root README review notice remains in place
for the human author to remove only after reviewing the stack.

## Review regression pass — 14 September 2026

### Fixes and focused evidence

- Dependency sources: Configure Kotlin exports the selected Gradle variant’s
  source archives. The pinned runtime reads the original Java/Kotlin file before
  trying decompilation. The disposable fixture resolved `ComponentActivity` to
  the original Java source, with a 1.22-second definition request in the recorded
  probe. This is one functional probe, not a performance benchmark.
- Resources: `R.string.app_name` and `@string/app_name` resolve from module XML
  buffers before calling the language server. The test covers locales, module
  boundaries, framework/foreign namespaces, ignored comments, and malformed XML.
  This works without waiting for a generated `R` class or an initialized server.
- Language installation: default automatic extensions now include Kotlin, Java
  and XML. The Java extension supplies Gradle Kotlin DSL highlighting. Its Kotlin grammar
  is registered under its extension owner so another Kotlin grammar version
  cannot replace it. XML handles
  both the manifest and values files. User extension overrides remain respected.
- Repeated command-clicks: cached definition links move the caret to the actual
  clicked symbol before falling back to references. The existing picker now
  handles one reference and replaces an already-open query instead of closing it.
- Search Everywhere: actual Shift press/release pairs, class filtering, combined
  file/action/symbol results, category navigation and opening a class have GPUI
  coverage. Language-server failure is visible while file/action search remains
  available.
- Find/Replace: Cmd+Shift+F/R on macOS and Ctrl+Shift+F/R on Linux/Windows
  deploy a modal, including with an editor focused.
  Project-panel Find keeps the selected directory filter. The regression replaces
  three occurrences in two files, preserves tabs, cancels close, then saves safely.
- Automatic sync, selected stopped AVDs, trust boundaries and device eligibility
  are covered in the Android panel test. Debug shares the same emulator startup
  path as Run.

The initial combined review run passed 128 tests: Android tools 4, Android UI 3, command palette
20, LSP locations 8, and search 93. The final resource navigation integration
test also passed. `cargo fmt --all -- --check` and the repository Clippy script
for all changed crates passed. Raw logs
and screenshots remain in ignored `target/android-ide/validation/review-*` files.

Re-run the focused checks from the repository root, with the installed WebRTC
path exported as in the build instructions above:

```sh
cargo test --locked -p search -p command_palette -p android_ui -p android_tools -p lsp_locations
cargo test --locked -p project --features test-support test_android_resource_definitions_without_language_server
cargo test --locked -p editor test_cached_declaration_click_moves_caret_before_usages
./script/clippy -p android_ui -p android_tools -p platform_title_bar -p project -p editor -p lsp_locations -p command_palette -p search -p project_panel --features gpui/inspector
```

### Native verification and integration corrections

The native test project was a disposable copy under `/private/tmp`, trusted as
one project. It populated all four variants without a manual Sync. After a quit
and relaunch, it automatically restored `:mobile · demoDebug`.

- Navigating `R.string.app_name` opened the declaration in `strings.xml` before
  Kotlin setup. Both values XML and `AndroidManifest.xml` were highlighted.
- Configure Kotlin initially exposed a settings-contract error: the Kotlin
  extension wraps its settings in `kotlin`, so our generated configuration must
  contain `externalSources` directly. The corrected configuration opened
  `ComponentActivity.java` at its declaration, with bytes identical to the
  AndroidX activity source archive. A final native Configure Kotlin run generated
  the correct settings shape with 76 source archives. Runtime `+android-sources-3` also limits
  fallback decompiler logging to warnings/errors so per-class logs cannot block
  the LSP connection. Workspace symbols now contain full locations because Zed
  does not advertise deferred symbol resolution. Native Go to Class returned and
  opened `MainActivity`. Attached-source, definition, workspace-symbol and rename
  tests passed before installing this runtime.
- Go to Definition on a `LibraryGreeting` use opened its Kotlin declaration;
  invoking it on the declaration opened a four-entry usages popup.
- The top device picker included stopped AVDs. Selecting `medium_phone` and
  pressing Run booted it, deployed `demoDebug` to `emulator-5554`, and displayed
  the expected flavor, package ID and library text. `adb emu avd name` verified
  the selected AVD. The bottom-left Logcat icon opened that emulator’s stream.
  The stream and this test emulator were stopped afterward.
- Cmd+Shift+F and Cmd+Shift+R opened floating Find and Replace windows after a
  clean native restart. Escape returned to the existing editor. Full inherited
  keymap tests caught category Tab, result-editor Escape, global Cmd+O Open, and
  Pane-level Cmd+Shift+F conflicts. The final tests cover macOS Cmd and Linux
  Ctrl Find/Replace from a focused editor, preserving tabs and dirty-close safety.
  Search Everywhere keeps category buttons visible with zero matches.
- Both `build.gradle.kts` and `settings.gradle.kts` were highlighted in the native
  editor. The grammar fix qualifies the actual loader configuration as well as
  registry metadata. Its regression loads a language using the owning grammar,
  so checking registry names alone cannot hide another loading regression.

The app-level strict loader test resolves every binding in both shipped JetBrains
maps, and the action-namespace registration test passes. These checks caught
misnamed debugger actions that had compiled but prevented the earlier optimized
binary from opening a window. The final Android UI suite also passes (3 tests),
including source configuration and trust/variant/device behavior.

The extension grammar regression passes. A wider extension-host run passed 44
of 45 tests; the unrelated development-extension build test failed when its
WASI SDK download was denied by the sandbox’s network restriction. The grammar
registration and extension removal/restoration tests passed in that run.

Screenshots: `review-resource-native.png`, `review-component-source-native.png`,
`review-usages-native.png`, `review-stopped-emulator-run.png`,
`review-class-search-native.png`, `review-gradle-native.png`, and
`review-find-final-native.png` in the local
validation directory. The screen-sharing badge obscures the window buttons in
earlier captures; the final optimized captures show the settled, centered native
controls. Their position uses the titlebar and actual button heights.

Latest focused logs: `review-go-to-class-before.log` / `review-go-to-class-after.log`,
`review-find-shortcuts-before.log` / `review-find-shortcuts-after.log`,
`review-grammar-loading-test.log`, `review-android-ui-final-tests.log`,
`review-strict-keymap-test.log`, `review-action-namespaces-test.log`,
`review-kotlin-symbols-build.log`, and `review-native-kotlin-settings.log`.
The shortcut tests failed before the fixes and pass afterward. The latest
normal debug build and focused Clippy checks passed; the optimized build is
recorded separately below. No temporary user keymap override remains.

The Kotlin configuration contract was checked against the
[installed extension’s implementation](https://github.com/zed-extensions/kotlin/blob/main/src/kotlin.rs).

### Shortcut audit

Checked the [Android Studio shortcut reference](https://developer.android.com/studio/intro/keyboard-shortcuts)
and [IntelliJ macOS keymap](https://www.jetbrains.com/help/idea/reference-keymap-mac-default.html).
The Mac map now corrects documentation, breakpoints, Resume, tab switching,
Version Control, block comments and auto-indent. Both maps retain separate Find
Usages results and the Show Usages popup. Search Everywhere and floating project
Find/Replace are implemented by the new layers, with existing editor features
used for navigation, folding, formatting, completion and rename.

This is not complete IntelliJ action parity: live templates, statement completion,
smart completion, Generate, and several structural refactorings need additional
language support. An untitled Zed buffer substitutes for a scratch file. macOS
may reserve Ctrl+Left/Right for Spaces; change the OS shortcut if it intercepts
editor-tab switching. Linux/Windows keymap loading is tested on macOS, not on
those operating systems.

### JetBrains Android plugin reference

The [resource model documentation](https://github.com/JetBrains/android/blob/master/android/src/com/android/tools/idea/res/README.md)
is useful for the next resource-navigation phase: it distinguishes module,
project, dependency and framework resources and explains source-set overlays.
The current implementation intentionally returns matching declarations in this
module’s source sets/locales, without pretending to choose the runtime qualifier
or selected-variant overlay. Resources in other modules/AARs fall back to LSP.

The [Android light-class documentation](https://github.com/JetBrains/android/blob/master/android/docs/android-light-classes.md)
explains why navigating to generated `R` code is the wrong editor destination
and how Studio redirects those symbols to their source resources before a build.
Our implementation follows that behavior through Zed’s existing definition
pipeline. The plugin’s PSI extension points are IntelliJ-specific; directly
embedding them into GPUI is not a small integration. No JetBrains plugin code was
copied. A Gradle-backed resource overlay model is the next step for namespace,
flavor, dependency and framework resource parity.

### Optimized review build

The optimized build passed in **27 minutes 9 seconds**. The running executable
reports `1.21.0+dev.45baad4154940b206f5c2f8662ca68868d992209`; later commits change
only the review/validation documents. Binary SHA-256:
`52b6f8030dc8b84fafe78d4126e28e7b253ee45041ab3cc7bc15a14b726371e8`.

It is left open on the bundled `examples/android-ide` project. Automatic sync
restored `:mobile · fullDebug`. Configure Kotlin completed and refreshed the
sample’s settings with 76 source archives. Native Gradle Kotlin highlighting,
Cmd+O class search, Cmd+Shift+F floating Find and Escape dismissal passed on this
optimized executable. The native traffic lights are centered in the settled
window. No test emulator is running.

`review-release-build.json`, `review-release-final-build.log`,
`review-release-system-specs.log`, `review-release-final-launch.log`,
`review-optimized-find.png`, `review-optimized-titlebar.png`, and
`review-optimized-workspace.png` retain the final evidence. The last focused
Clippy run also passed (`review-keymap-clippy-final.log`); its reported elapsed
time includes waiting for the release build’s Cargo lock.

The existing test-source limitation remains: the exported language-server
classpath is the selected production variant’s classpath. Opening unit-test
results can therefore show missing JUnit symbols even though Gradle tests build
and pass. Test-only classpaths and a complete source-set model remain in the
language-engine acceptance gate above.

All **19 draft PRs** in GitHub stack **#16** were verified against the local branch
heads, ordered parent branches, draft status, navigation links and release notes.
The [review guide](ANDROID_IDE_REVIEW.md) links every layer. GitHub reported no CI
checks on the fork; the checks documented here ran locally. Nothing was merged.
