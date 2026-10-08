# Android Setup

Koda opens Android Setup on first launch, including an empty workspace. You can
choose **Not now** and reopen setup with **Android: Setup** in the command
palette. Missing project dependencies also lead back to setup. The Android sidebar
and Android toolbar menu have been removed; project, variant and device selection
remain in the title bar, with Build Output and Logcat in their existing docks.

## Setup flow

1. **Review downloads** detects compatible full JDK 21 and SDK installations,
   then shows missing components, download sizes and exact locations. Publisher
   details and verified source URLs are available in **Show download details**.
   Java downloads use Eclipse Temurin JDK 21 in Koda's application storage. SDK
   downloads default to Android Studio's usual SDK directory; existing complete
   packages are reused. **Change settings** exposes folder choices, Android API,
   offline mode and advanced repair. Changing a choice requires refreshing the
   summary before proceeding. SDK discovery also reads the project's existing
   `local.properties` `sdk.dir`; a JRE without `javac` is rejected with guidance.
   A literal project `compileSdk` takes precedence; an empty workspace reuses the
   saved API or a complete installed SDK API before suggesting the default API.
2. **SDK license** shows the full applicable SDK terms when downloads need them.
   Consent starts unchecked and is tied to the displayed text and installation
   plan. Declining or cancelling prevents the corresponding installation. Koda
   records acceptance only when the user starts installation.
3. **Downloading** shows progress and error details, with cancellation. Errors
   return to the review with their precise reason and retry guidance visible.
   Verified cached downloads can be reused. Cancellation preserves the previous
   Koda selection; publication already in progress finishes or recovers it.
   Complete packages already added to a shared SDK remain available to other IDEs.
4. **Ready** lists selected paths and remaining prerequisites. **Finish** completes
   onboarding; **Not now** defers it. Java and SDK setup alone does not install the
   Kotlin extension, configure language servers, create an AVD or establish
   working Run/Debug/Preview for a project.

The SDK defaults match Android Studio: `~/Library/Android/sdk` on macOS,
`~/Android/Sdk` on Linux and `%LOCALAPPDATA%/Android/Sdk` on Windows. You can change
the destination before installing. Koda only adds missing packages; it refuses to
overwrite conflicting SDK contents. SDK locations can be shared with Android
Studio. Java, settings and caches remain private to each Koda data profile.
See [Android Studio's SDK location source](https://android.googlesource.com/platform/tools/adt/idea/+/refs/heads/mirror-goog-studio-main/android/src/org/jetbrains/android/sdk/AndroidSdkUtils.java).

The automatic SDK catalog currently contains platform-tools and platform/build
tools for API **36** and **37.0**, pinned to reviewed releases. An existing SDK
can supply other APIs when the requested platform, `android.jar`, `adb`, `aapt2`
and D8 are present. A complete existing SDK is reused read-only. Missing packages
can be added to the chosen shared destination without replacing existing
packages. `compileSdk` detection
is a bounded literal hint, not evaluation of Gradle: computed values and unusual
project layouts may need manual selection. Selecting an SDK does not rewrite
`local.properties`; fix an obsolete project SDK path there if Gradle overrides
the saved selection.

SDK/AVD browsing, emulator images, NDK/CMake and arbitrary package management are
outside this setup flow. Use Android Studio or Google's supported tools for those
components. Koda uses SDK platform-tools (`adb`) directly for device discovery,
Run and Debug.
Device access must be authorized. Starting an existing emulator also requires the
SDK emulator package and an existing AVD; stopping it uses `adb emu kill`.

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

Native Java and SDK downloads have pins for **macOS Apple Silicon, macOS Intel,
Linux x86_64 and Windows x86_64**. Other hosts can select existing compatible
tools but cannot request unavailable automatic downloads. Cross-platform pins
and compilation do not establish full IDE runtime support on every platform.

Native Java and SDK setup needs **no Python**. The existing advanced managed
Kotlin/debugger source installers currently support **Apple Silicon macOS** and
still require Python **3.12+** and Apple's Command Line Tools. Their UI lists
missing prerequisites and streams failures into Build Output. Configure Kotlin
explicitly after installing its extension and runtime. Custom/disabled language
server preferences are preserved until that action is selected.

## Persistence, repair and updates

Java, download caches and managed tool records belong to `android-tools` under
the app's data profile. SDK packages use the selected shared SDK folder. On macOS
the data profiles are
`~/Library/Application Support/Koda`, `Koda Nightly` and `Koda Dev`; a custom
`--user-data-dir` owns its own state. Stable, Nightly, Dev and Zed settings, Java
installations and caches remain isolated; SDK packages can be shared.
Saved paths live in `environment.json`, outside project settings, and are applied
to Android command and language-server environments without shell exports.

Advanced setup exposes **Validate**, **Repair / update**, **Restore previous**,
offline mode and managed storage. Validation checks saved dependencies and full
managed inventories. Missing/corrupt files, incompatible recipes and another
profile's managed paths fail with actionable guidance. Repair uses the current
embedded manifest and new immutable installation slots. Restore requires a
validated previous selection compatible with the running app's recipe. Restoring
a selection does not delete packages from a shared SDK.

An app update changes embedded runtime identities when their recipes change.
It does not silently download or replace tools on startup. Return to setup to
review and install the new plan, then configure affected projects again. Old
slots remain because another window or persisted project path may use them.
Close all Koda windows before manually reclaiming obsolete storage.

Native installs use the same nonblocking, profile-wide kernel lock as advanced
installers. Concurrent windows receive a retryable busy error. Downloads and
extraction happen in staging; complete files, permissions and link targets are
inventoried before publication. Shared SDK packages are published individually
without replacement, followed by an atomic journalled Koda path selection.
Restart recovers interrupted publication. Corrupt regular settings/selectors are
preserved for diagnosis; unsafe links, unknown formats or an ambiguous corrupt
journal produce repair guidance instead of being overwritten.

## Download boundaries

Native manifests contain immutable HTTPS URLs, SHA-256 hashes, publisher and
version information. SDK pins also retain Google's repository SHA-1 checksums
and package metadata. Redirects are limited to reviewed origins. Downloads have
a 1 GiB per-file limit, 3-second connection timeout, 3-second read inactivity
timeout, 30-minute attempt deadline and two attempts. Extraction rejects unsafe
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
