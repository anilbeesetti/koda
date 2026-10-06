# Android Setup

Downloaded Koda can provision Java and Android tools from **Android → Android
Setup…** in the title bar. A trusted first Android project opens setup if its
Java or SDK dependencies are missing. The Android sidebar has been removed;
project, variant and device selection remain in the title bar, with Build Output
and Logcat in their existing docks.

## Setup flow

1. **Welcome** explains the supported components and platform limits.
2. **Components** offers Standard or Custom setup. Existing compatible full JDK
   21 installations are reused; otherwise Koda downloads Eclipse Temurin JDK 21.
   Choose existing SDK/JDK/Android CLI paths for nonstandard locations.
   SDK discovery also reads the project’s existing local.properties sdk.dir. A JRE
   without `javac` is rejected with guidance.
3. **Verify** shows versions, publishers, source URLs, download sizes and the
   private installation directory before anything is downloaded.
4. **Licenses** shows the full applicable Google terms. Every agreement starts
   unchecked and is tied to the displayed text and installation plan. Declining
   or cancelling prevents that installation. Koda records acceptance only when
   the user starts the corresponding installation.
5. **Installing** shows bounded progress and error details, with cancellation.
   Failed operations return to Components to create a fresh plan; verified cached
   downloads can be reused. Cancellation before publication preserves the
   previous selection. Once atomic publication starts it finishes or recovers
   the previous selection.
6. **Ready** lists the selected paths and remaining prerequisites. Java/SDK setup
   alone does not install the Kotlin extension, configure language servers, add
   an emulator or establish working Run/Debug/Preview for a project.

The automatic SDK catalog currently contains platform-tools and platform/build
tools for API **36** and **37.0**, pinned to reviewed releases. An existing SDK
can supply other APIs when the requested platform, `android.jar`, `adb`, `aapt2`
and D8 are present. Koda never modifies an externally selected SDK. If it needs
to download packages, it creates a separate private SDK. `compileSdk` detection
is a bounded literal hint, not evaluation of Gradle: computed values and unusual
project layouts may need manual selection. Selecting an SDK does not rewrite
`local.properties`; fix an obsolete project SDK path there if Gradle overrides
the saved selection.

SDK/AVD browsing, emulator images, NDK/CMake and arbitrary package management are
outside this setup flow. Use Android Studio or Google's supported tools for those
components. Koda can start and stop existing emulators.

## Java and platforms

Java 21 remains the project and Compose Preview default. Downloads use a full
**Eclipse Temurin 21.0.12.1+1** JDK. Compatible installed JDK 21 distributions,
including Android Studio's runtime when it is a full JDK 21, are reused. Preview
ships its renderer, layoutlib and bridge, but **no Java runtime**. Build machines
still use a pinned host Java to compile the bundled renderer; it is not shipped.

Running Gradle on Java 21 requires Gradle **8.5+** and compatible project plugins.
This setup does not choose a separate older JVM for legacy Gradle wrappers.
See [Gradle’s Java compatibility matrix](https://docs.gradle.org/current/userguide/compatibility.html).

The pinned official Kotlin LSP `263.4702.0` declares Java **25** in its upstream
`product-info.json` and includes its own private JetBrains Runtime. That runtime
is separate from the selected project Java. This change does not reuse Java 25
for project builds or Preview. Gradle's supported runtime version and each
project's plugins must be checked before any future reuse policy changes.

Native Java/SDK/CLI downloads have pins for **macOS Apple Silicon, macOS Intel,
Linux x86_64 and Windows x86_64**. Other hosts can select existing compatible
tools but cannot request unavailable automatic downloads. Cross-platform pins
and compilation do not establish full IDE runtime support on every platform.

Native Java/SDK/CLI setup needs **no Python**. The existing advanced managed
Kotlin/debugger source installers currently support **Apple Silicon macOS** and
still require Python **3.12+** and Apple's Command Line Tools. Their UI lists
missing prerequisites and streams failures into Build Output. Configure Kotlin
explicitly after installing its extension and runtime. Custom/disabled language
server preferences are preserved until that action is selected.

## Persistence, repair and updates

Tools belong to `android-tools` under the app's data profile. On macOS these are
`~/Library/Application Support/Koda`, `Koda Nightly` and `Koda Dev`; a custom
`--user-data-dir` owns its own state. Stable, Nightly, Dev and Zed remain isolated.
Saved paths live in `environment.json`, outside project settings, and are applied
to Android command and language-server environments without shell exports.
Google CLI state also uses the managed profile's Android user directory.

Advanced setup exposes **Validate**, **Repair / update**, **Restore previous**,
offline mode and managed storage. Validation checks saved dependencies and full
managed inventories. Missing/corrupt files, incompatible recipes and another
profile's managed paths fail with actionable guidance. Repair uses the current
embedded manifest and new immutable installation slots. Restore requires a
validated previous selection compatible with the running app's recipe.

An app update changes embedded runtime identities when their recipes change.
It does not silently download or replace tools on startup. Return to setup to
review and install the new plan, then configure affected projects again. Old
slots remain because another window or persisted project path may use them.
Close all Koda windows before manually reclaiming obsolete storage.

Native installs use the same nonblocking, profile-wide kernel lock as advanced
installers. Concurrent windows receive a retryable busy error. Downloads and
extraction happen in staging; complete files, permissions and link targets are
inventoried before an atomic journalled selection of paths and generation.
Restart recovers interrupted publication. Corrupt regular settings/selectors are
preserved for diagnosis; unsafe links, unknown formats or an ambiguous corrupt
journal produce repair guidance instead of being overwritten.

## Download boundaries

Native manifests contain immutable HTTPS URLs, SHA-256 hashes, publisher and
version information. SDK pins also retain Google's repository SHA-1 checksums
and package metadata. Redirects are limited to reviewed origins. Downloads have
a 1 GiB per-file limit, 3-second connection timeout, 3-second read inactivity
timeout, 180-second attempt deadline and two attempts. Extraction rejects unsafe
paths, special files, escaping links and duplicate entries, with a 5 GiB /
100,000-entry limit. Native storage is bounded to 12 GiB within the existing
24 GiB / 300,000-entry managed-profile boundary.

Offline mode uses verified cache entries only. Missing cache data explains which
component needs a connection. Cancellation and retry do not accept licenses,
relabel old installations or bypass integrity checks. The normal path requests
no credentials, signing changes, Gatekeeper exceptions or security-warning
bypasses.

The advanced Kotlin/debugger installers keep their existing pinned source and
compiler dependencies, private caches, supervised process-tree cancellation,
bounded output and strict Gradle verification. They may take several minutes.
Their tool/version manifests remain the stable runtime interface:
`managed::resolve(Tool::Kotlin/Debugger)` and `Tool::recipe`; consumers should not
hard-code checkout paths. Debugger semantics and the test runner remain separate
tasks. This change does not publish releases or alter signing/account settings.

## Acceptance

Focused tests cover reused/full JDK validation, pinned manifests, safe extraction,
synthetic SDK installation, explicit license binding, offline/cache failures,
cancel/retry, corruption, journal recovery, concurrent locks, saved-path restart,
rollback and profile ownership. UI tests cover setup navigation, individual
consent, chooser errors, cancellation cleanup, focus and fixed-footer layout.

Before claiming complete downloaded-app support, validate a fresh Apple Silicon
Nightly profile with no checkout or runtime environment exports: sync a real
project, configure Kotlin and test completion/generated symbols, run on an
authorized device, attach/step in Debug and render actual Compose previews.
Repeat cancellation, corruption/repair, restart/upgrade, offline and concurrent
window checks. Linux cloud checks and synthetic fixtures do not establish those
macOS/device flows or full Android Studio parity. The PR records the checks
actually performed and remaining runtime gaps.
