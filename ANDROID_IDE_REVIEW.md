# Stacked pull request review guide

The changes are reviewed as draft pull requests in
[GitHub stacked PRs](https://github.com/anilbeesetti/zed/pulls) (stack #16).
The original fifteen layers remain in order; this review adds navigation,
Search Everywhere, floating Find/Replace, and validation layers. The combined
application is on `codex/android-ide/review-validation`. The base is `main` at
`7960b2a7c9568e90fbe0727332149e5b2a5fd57a`.

## Inspect and test the stack

```sh
gh stack view --json
git log --oneline main..codex/android-ide/review-validation
git diff --stat main..codex/android-ide/review-validation
```

For one layer, compare its branch against the parent in this table. This keeps
review focused without losing the runnable combined application at the top.
The actual Git branch tips are authoritative if a cached `gh-stack` head field
has not refreshed after a commit.

| Layer | Branch suffix under `codex/android-ide/` | Parent | Draft PR |
| --- | --- | --- | --- |
| 1 | `research` | `main` | [#1](https://github.com/anilbeesetti/zed/pull/1) |
| 2 | `studio-defaults` | `research` | [#2](https://github.com/anilbeesetti/zed/pull/2) |
| 3 | `android-tools` | `studio-defaults` | [#3](https://github.com/anilbeesetti/zed/pull/3) |
| 4 | `android-workflow` | `android-tools` | [#4](https://github.com/anilbeesetti/zed/pull/4) |
| 5 | `studio-shell` | `android-workflow` | [#5](https://github.com/anilbeesetti/zed/pull/5) |
| 6 | `kotlin-setup` | `studio-shell` | [#6](https://github.com/anilbeesetti/zed/pull/6) |
| 7 | `dev-launcher` | `kotlin-setup` | [#7](https://github.com/anilbeesetti/zed/pull/7) |
| 8 | `emulator-start` | `dev-launcher` | [#8](https://github.com/anilbeesetti/zed/pull/8) |
| 9 | `smoke-project` | `emulator-start` | [#9](https://github.com/anilbeesetti/zed/pull/9) |
| 10 | `validation` | `smoke-project` | [#10](https://github.com/anilbeesetti/zed/pull/10) |
| 11 | `java-support` | `validation` | [#11](https://github.com/anilbeesetti/zed/pull/11) |
| 12 | `kotlin-runtime` | `java-support` | [#12](https://github.com/anilbeesetti/zed/pull/12) |
| 13 | `debugger` | `kotlin-runtime` | [#13](https://github.com/anilbeesetti/zed/pull/13) |
| 14 | `compose-preview` | `debugger` | [#14](https://github.com/anilbeesetti/zed/pull/14) |
| 15 | `final-validation` | `compose-preview` | [#15](https://github.com/anilbeesetti/zed/pull/15) |
| 16 | `navigation` | `final-validation` | [#17](https://github.com/anilbeesetti/zed/pull/17) |
| 17 | `search-everywhere` | `navigation` | [#18](https://github.com/anilbeesetti/zed/pull/18) |
| 18 | `find-replace-popup` | `search-everywhere` | [#19](https://github.com/anilbeesetti/zed/pull/19) |
| 19 | `review-validation` | `find-replace-popup` | [#20](https://github.com/anilbeesetti/zed/pull/20) |

For example:

```sh
git diff codex/android-ide/android-tools..codex/android-ide/android-workflow
git diff codex/android-ide/studio-shell..codex/android-ide/kotlin-setup
```

Fixes were committed to their owning layer and descendants rebased locally.
Future edits should follow the same approach: check out that layer, make and
validate the change, commit, then use `gh stack rebase --upstack --remote origin`
and return with `gh stack top`. A rebase may fetch its base; that is not a push.
The author authorized publishing this stack as draft PRs. Do not merge it without a separate request.

Start with the [validation report](ANDROID_IDE_VALIDATION.md) for build commands,
supported workflows, known failures, performance caveats, and reproduction.
The root README notice is intentionally retained for human review. Do not let
an agent remove it while preparing a future submission.

## 1. Document the Android IDE architecture and validation plan

Branch: `codex/android-ide/research`

Describe the Android Studio migration target, reuse points in Zed, toolchain
prerequisites, language-server risks, phased implementation, and acceptance
gates. Add the required human-review notice to the root README.

Validation: inspect the cited official documentation and local source paths;
confirm the roadmap distinguishes implemented work from future parity goals.

Release Notes:

- N/A

## 2. Adopt Studio themes and JetBrains editing defaults

Branch: `codex/android-ide/studio-defaults`

Provide Studio Dark/Light themes, licensed JetBrains Mono fonts, compact typography, initial tool-window placement, and the JetBrains keymap by default. Automatically request Kotlin, Java and XML extensions and use the existing LSP results picker, preserving user overrides.

Align common editing/debugging shortcuts with the Android Studio reference, including Mac documentation, breakpoints, Resume, editor-tab switching, Version Control, block comments and auto-indent. Search Everywhere and floating Find/Replace behavior are introduced in their later dependent layers.

Validation: native theme/layout checks from the original checkpoint, shortcut precedence regression for both platform maps, combined 128-test run, formatting, and changed-crate Clippy. Strict app-level keymap loading and action-namespace tests pass, including the debugger action names that previously caused native startup failure. Full IntelliJ refactoring/action parity is not claimed.

### Suggested .rules additions

“When adding built-in keybindings, validate the platform assets with `KeymapFile::load_asset` in the app-level regression. Partial keymap loading can silently drop unknown action names that make native startup fail.”

“When changing base keymaps, test the combined default and base maps with an editor focused. Inherited global and Pane bindings can outrank Workspace overrides.”

Release Notes:

- Improved Android Studio appearance, language defaults and familiar shortcuts

## 3. Model Android projects, devices, and build artifacts

Branch: `codex/android-ide/android-tools`

Add a small non-UI crate for Gradle-root detection, SDK tool discovery, evaluated
Android targets, device states, and artifact selection. Commands identify the
project and device explicitly. Reject ambiguous or unsafe APK metadata paths
instead of guessing what to deploy.

Preserve complete wireless ADB identifiers containing spaces; parse the state
separately so the same full serial is used for selection, Run, Debug and Logcat.

Validation: focused parser/artifact tests cover malformed output, multiple
devices, wireless mDNS identifiers, permission states, module/flavor targets,
metadata redirects, filters and missing files. Native selection switches from a
stopped AVD back to the physical device and enables Run.
`cargo test -p android_tools` and `./script/clippy -p android_tools` pass.

Release Notes:

- Added Android project, device, and build-artifact discovery

## 4. Add native Android build and run controls

Branch: `codex/android-ide/android-workflow`

Add Android targets, device selection, toolbar controls and Build/Run/Test/Lint/Logcat tasks using the existing workspace, trust and terminal mechanisms. Save edits before builds and deploy only a validated selected-variant artifact to the selected serial. Restore the saved module/variant only against a fresh Gradle model.

Trusted Android projects synchronize automatically after their worktree is loaded. Root/trust changes are observed without reentrant entity updates; a new root clears the old model and the picker shows sync progress. Manual sync remains available after Gradle edits.

Validation: native build/run/failure/cancellation workflows and GPUI trust, root, auto-sync, device, dock and shortcut coverage. Combined regressions and changed-crate Clippy pass.

Release Notes:

- Added native Android workflows and automatic trusted-project synchronization

## 5. Arrange the workspace around Studio-style tool rails

Branch: `codex/android-ide/studio-shell`

Place existing dock controls on side tool rails, keep bottom controls together at the lower left, and add Project collapse/hide affordances. Preserve editor height and reuse existing dock state.

Center macOS traffic lights against the actual titlebar and native button heights. Add a keyboard-accessible lower-left Logcat icon that dispatches the shared Android action.

Validation: workspace geometry/dock regressions, platform-titlebar and Android compilation, changed-crate Clippy, plus native layout checks recorded in the combined validation report.

Release Notes:

- Improved Studio-style tool rails, macOS window controls and Logcat access

## 6. Configure Kotlin with the selected Android classpath

Branch: `codex/android-ide/kotlin-setup`

Configure Kotlin using the selected Android variant’s evaluated Gradle classpath, generated Java output and dependency source archives. Write a protected classpath hook and merge project settings without replacing unrelated preferences. Use JDK 21 and restart Kotlin language support.

Resolve source artifacts from the variant compile configuration and export them separately from runtime/compiler binaries. The later pinned-runtime layer consumes those archives for original library source navigation.

Validation: Gradle source/classpath export and configuration-cache reuse; Rust settings/path tests, generated R/BuildConfig and library integration probes; changed-crate Clippy. The initial community object-rename failure is fixed in the later runtime layer.

Use the Kotlin extension’s configuration shape: it adds the outer kotlin key, so generated sourceArchives belong directly under settings.externalSources. Native ComponentActivity source navigation passes after this correction.

Release Notes:

- Added selected-variant Kotlin classpaths and attached dependency sources

## 7. Launch the fork with an isolated development profile

Branch: `codex/android-ide/dev-launcher`

Add `script/android-ide` to build/package/run a macOS development app with a
dedicated bundle ID and profile. Discover the existing Android SDK/JBR, preserve
existing profiles, and initialize new profiles with updates/telemetry disabled
and Kotlin support selected. Refuse to overwrite a running development bundle.

Validation: shell syntax, help/errors, debug/release launch, fresh and existing
profiles, paths relative to the caller, SDK discovery, and the running-app guard.

Release Notes:

- Added an isolated macOS launcher for Android IDE development

## 8. Start and run a selected Android emulator

Branch: `codex/android-ide/emulator-start`

Show running devices and stopped SDK AVDs in the same device picker. Resolve a connected emulator’s AVD name through adb so the selected virtual device keeps its identity across startup. Run starts a stopped selection, waits for Android CLI readiness, resolves that exact emulator serial, then deploys the requested variant. Reject stale completion after a project change.

Keep standalone start/stop actions, preserve the AVD when stopping, and disable stop for physical or unavailable devices. Device refresh remains available with an empty device list.

Validation: Android panel state/trust tests and native selected-emulator workflows in the combined report. Debug reuses this startup path in its later dependent layer. Changed-crate Clippy passes.

Both device selectors refresh on opening and update the deployed menu when
bounded discovery completes. Mark the current selection, disable stale rows
while refreshing and reset keyboard selection after a reorder. Five combined
Android-UI tests, changed-crate Clippy and the optimized native build pass.
Native external-emulator start/stop and both picker surfaces pass without a
manual refresh. Continuous ADB tracking remains a separate parity item.

Release Notes:

- Added stopped-emulator selection and automatic startup before Run

## 9. Add a multi-module Android smoke project

Branch: `codex/android-ide/smoke-project`

Add a small reproducible Compose project with a non-`:app` application module,
demo/full flavors, an Android library dependency, generated symbols, a Java
helper, and a focused variant test. Pin and verify the Gradle wrapper. Keep
machine-specific settings, caches and build outputs ignored.

Validation: both debug builds and unit-test variants, lint with documented
warnings, native target selection and deployment, classpath export, library
hover and Go to Definition. The app visibly identifies its flavor and library.

Release Notes:

- Added a multi-module Android sample for testing IDE workflows

## 10. Record validation evidence and the local review stack

Branch: `codex/android-ide/validation`

Record exact tool versions, checks, reproduction commands, initial memory and
startup measurements, remaining product gaps, and local stack review steps.
Keep screenshots and raw performance/build logs in the ignored local validation
directory. Add the language-server findings to the implementation roadmap.

Validation: cross-check report claims against command logs, native screenshots,
source revisions, and branch ancestry. No publication occurs in this layer.

Release Notes:

- N/A


## 11. Configure Android-aware Java editing from the selected Gradle variant

Branch: `codex/android-ide/java-support`

Default JDT LS import does not model the AGP 9 fixture correctly. Add Configure
Java to export the actual selected compile graph and import it through the
standard Eclipse/Buildship model. Preserve existing settings/comments, Gradle
arguments, bundles and the JRE container. Refresh the Gradle root when the server
becomes ready so changing variants updates cached Buildship arguments.

Validation: focused Rust/GPUI tests, Clippy and native build; SDK/resource/library
hover, type diagnostics and fullDebug-to-demoDebug BuildConfig navigation without
cache deletion. The initial failed import and successful replacement are both
recorded as evidence. Mixed-language refactoring remains outside this change.

### Suggested .rules additions

Proposed for Android/JDT integration rules: “When changing JDT LS Gradle import
arguments, send `java/projectConfigurationsUpdate` for the Gradle root URI.
Updating only module URIs can retain the previous root import arguments.”

Java and Kotlin extensions now retain their own grammar versions, so installing both cannot break Gradle Kotlin DSL highlighting. Unqualified grammar aliases remain available for languages borrowing a grammar. Qualify both registry metadata and the actual language loader configuration. The regression loads a language, beyond checking registry names. Focused grammar loading/removal/restoration checks and native highlighting in build.gradle.kts and settings.gradle.kts pass. A wider run passed 44 tests; the unrelated extension fixture failed while downloading its WASI SDK in the restricted environment.

Release Notes:

- Added selected-variant Android Java language support

## 12. Install Kotlin runtime fixes for rename and library sources

Branch: `codex/android-ide/kotlin-runtime`

Build a checksum-pinned Kotlin server revision containing the upstream object-rename fix. Apply a small source-navigation patch that resolves configured dependency source archives before decompilation, including Java/Kotlin files, common Android/KMP source folders, nested classes, and paths with spaces. Recompute declaration ranges in original source. Limit fallback decompiler logging to warnings/errors so thousands of per-class messages cannot block the LSP output pipe.

Return complete workspace-symbol locations because Zed does not advertise deferred symbol resolution. This restores Kotlin classes and symbols in Search Everywhere.

Run upstream rename/definition/workspace-symbol/source-archive tests before installing. Managed runtime upgrades keep the old installation until replacement succeeds; unmanaged installations are preserved. The launcher and Configure Kotlin use the pinned runtime with JDK 21, independently of the build JDK 11.

Validation: bootstrap and upgrade, upstream tests, Rust settings tests and Clippy. The +android-sources-3 runtime returns native class search results. A real LSP probe and native editor navigation open ComponentActivity’s original source; the native file matches the source archive byte for byte. Native rename was checked at the earlier checkpoint. Mixed Java/Kotlin rename and full Kotlin semantic parity remain unsupported.

The runtime follow-up also resolves compiled Kotlin descriptors without PSI to
original attached source, with exact class/property/overload positions and renamed
JVM facade filenames. An explicit root classpath hook supplies the selected
project model without duplicate Gradle discovery. The native initialization
measurement drops from 16.9–20.5 seconds to 1.17 seconds. Binary-definition and
explicit-classpath fixtures pass; see the validation report for warm-up and
Gradle DSL limitations. The current managed installer version is `+android-sources-8`.

The latest runtime preserves attached `archive!/entry` URIs, queries sources for
the actual selected Gradle artifacts, shares in-flight diagnostics compilation,
and prioritizes the active file before workspace indexing. Explicitly opened
archive sources support further navigation without becoming project sources.
See the latest validation section for measured cold-start limits and tests.

Release Notes:

- Fixed Kotlin object rename and navigation to attached library sources

## 13. Add native Android Java and Kotlin debugging

Branch: `codex/android-ide/debugger`

Debug now builds the selected variant, launches its validated APK through Android
CLI in debug mode, discovers its application process and creates an ephemeral
JDWP forward. Register the pinned community JVM adapter with Zed's existing
native debugger. Scope forward cleanup to that session and reject overlapping
Android debug launches. Add toolbar, tool-window, menu and JetBrains shortcuts.

The source patch fixes the adapter's configuration handshake, breakpoint class
filters, obsolete requests, optional source names and VM detach. The bootstrap
checks source hashes and runs upstream adapter tests. No new debug protocol or
expression engine is introduced.

Validation: native Java and Kotlin breakpoints, variables, step-out into the
Compose caller and disconnect. The app remains alive and the owned forward is
removed. `script/test-android-debugger --device emulator-5554` checks both source
languages, local evaluation, handshake and cleanup. Rust tests, Clippy and the
native build pass. Advanced Kotlin inline/coroutine mapping, arbitrary expressions
and NDK debugging are not claimed.

A controlled `/usr/bin/false` adapter exposed a shared DAP race: EOF only flushed
current callbacks, and write failures only logged, allowing a later initialize
request to wait forever. Close request registration on disconnect and give each
TCP connection its own request map so old I/O cannot close a replacement. The
regression fails before the change and passes afterward for stdio, TCP, pending
requests, later requests and reconnection. Native startup failure now reports an
error, ends the session and removes the owned Android forward automatically.

A cold launch also showed that Android CLI can finish before ActivityManager
creates the app process. Retry process discovery with bounded attempts and
per-command timeouts; preserve the final failure for the UI. A deterministic
test covers delayed success, exhaustion and invalid process IDs.

Debug also accepts a stopped AVD from the shared device picker, waits for that exact emulator, and then continues the existing deploy/attach path. Run and Debug share the startup/trust/project-change checks.

Release Notes:

- Added Android Java and Kotlin debugging in the native IDE

## 14. Render selected Android Compose previews beside source

Branch: `codex/android-ide/compose-preview`

Build the selected variant, export its evaluated runtime classes and resources,
and use Google's compiled preview detector and standalone renderer. The native
Android tool window offers Compose preview and annotation selection, opening the
result in a reusable editor split. A failed render preserves the previous image;
validated successful output replaces it atomically. Bound the renderer process
and dispose temporary files after each invocation.

Pin a compatible Google renderer/layoutlib set and compile a small Java bridge
that ensures the alpha15 process exits after disposing its framework. Preserve
existing project files and require workspace trust before running project code.
The fixture adds named Default and Large text previews.

Validation: Rust model/output/cache tests, native builds and Clippy; demoDebug and
fullDebug output, generated resources, Java/Kotlin library text, 845×480 large-text
and 681×399 default fullDebug images, selector changes, pane reuse and a deliberate
runtime failure preserving the image hash. Rendering works with Android Studio
closed and no emulator. Interactive/XML previews and multi-value parameter
galleries remain separate work.

Preview-size switching also exposed an existing image reload bug: pixels changed
while dimensions and file size stayed stale. Refresh metadata from the same new
bytes before emitting the existing reload event. A real-file GPUI regression
fails before the change; all five image-viewer tests pass afterward, including
asset-cache and split-pane checks. Project/image-viewer Clippy passes with the
existing inspector feature flag.

The editor toolbar now exposes Show/Hide Compose Preview. It preserves unrelated
tabs and unsaved edits when hiding, then reopens a same-project/variant cached
image without another build. A GPUI regression and two native hide/show cycles
pass. Per-file Compose eligibility, automatic refresh and source-focus retention
remain in the comparison backlog.

Release Notes:

- Added standalone Compose previews for the selected Android build variant

## 15. Record the final feature evidence and remaining product gates

Branch: `codex/android-ide/final-validation`

Update the original research checkpoint, local stack guide and validation report
with the completed Java/Kotlin/debugger/preview work, exact tool revisions,
reproduction steps and honest limits. Preserve earlier failures and performance
measurements as historical evidence rather than attributing them to the final
build. Keep all screenshots, logs and runtime downloads local and ignored.

Validation: cross-check the final build and feature evidence, local branch
ancestry and diff hygiene. At this historical checkpoint publication had not
yet been authorized; the current stack is published as draft PRs.

Release Notes:

- N/A

## Review fixes, 14 September

The owning layers contain the defaults/shortcut corrections (2), trusted-project
auto-sync (4), traffic-light alignment and Logcat rail control (5), dependency
source archive export (6), stopped-AVD selection and Run startup (8), isolated
Java/Kotlin extension grammar versions (11), patched
source navigation runtime (12), and Debug startup for a stopped AVD (13).
Their descendants were rebased with `gh stack rebase --upstack --remote origin`.

Layer 16 adds Android resource-to-XML navigation and fixes cached declaration
clicks to find usages at the clicked location. It uses the existing references
picker, including one-result and repeated-query cases. It also adds read-only
JAR/ZIP entry support to the existing filesystem, preserving Gradle source paths
across restarts. Reused language-server nodes retain project settings for library
buffer requests. New archive servers use the sole visible project's settings
and working directory, including replacements after settings changes; source
ownership for multi-root startup remains a limit.
Filesystem and project-LSP regressions cover these paths.

Layer 17 combines existing file, workspace-symbol and action search in Search
Everywhere. It includes category tabs and Double Shift/Go to Class shortcuts.
File/action results appear while the language server is still searching.

Layer 18 wraps the existing project search view/bar in a floating modal. Find and
Replace retain filters, matching, replacements and save handling. Closing a dirty
result asks Save/Discard/Cancel, and the prior editor tabs remain in place.

Layer 19 records the review evidence and limits. See the latest section of
[the validation report](ANDROID_IDE_VALIDATION.md) before testing.
