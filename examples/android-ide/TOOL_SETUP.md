# Managed Android tool setup

The downloaded app includes Koda's installer recipes, patches and preview bridge
source. A source checkout and `script/android-ide` are no longer required to find
or provision Koda's runtimes. Managed Kotlin language support, Android debugging
and Compose rendering currently support **Apple Silicon macOS only**. SDK/JDK
path selection also compiles on other platforms; it does not establish working
Kotlin, debugging or rendering support there.

## First project

1. Install an existing Android SDK with Android Studio, a JDK 21, Python 3.12 or
   newer from python.org/Homebrew, Apple's Command Line Tools, and Google's
   Android CLI. Koda detects standard SDK/JDK/Homebrew locations. It neither
   installs SDK packages or AVDs nor accepts SDK licenses. Project-specific SDK
   platform/build-tool/Gradle requirements remain the project's responsibility.
2. Open the project's Gradle root and trust it. The Android panel reports missing
   dependencies when initial sync cannot proceed. Expand **Tool setup**, use
   **Detect dependencies**, and select **Choose SDK**, **Choose JDK 21** or
   **Choose Android CLI** for nonstandard locations. Choose the SDK directory
   containing `platform-tools/adb`, the JDK home containing `release` and
   `bin/java`/`bin/javac`, and the CLI executable, respectively.
3. Choose **Install / repair** for Kotlin. Setup streams progress and errors to
   Build Output. Install the Kotlin extension if necessary, sync the project,
   select its module/variant and choose **Configure official Kotlin**. Existing
   custom language-server preferences still require that explicit configuration.
4. Provision the debugger or Compose preview when needed, then use the existing
   Run/Debug/Preview actions. The debugger builds pinned fwcd sources; its
   temporary checksum-pinned JDK 11 is a build dependency, while Android setup
   and debugger execution use the discovered/chosen JDK 21. Source-based builds
   can take several minutes and need network access on their first installation.

Saved dependencies are applied to Android CLI/Gradle commands, Kotlin resource
generation, Java model generation and language-server imports. An existing
`JAVA_HOME` remains the general Gradle JVM unless the user selects a JDK; Kotlin
setup still requires JDK 21. Android Studio's JDK 21 is discovered when available.
Setting a selected SDK does not rewrite `local.properties`; Gradle's project
configuration may still override the SDK environment.

## Persistence, validation and updates

Runtime state belongs to `android-tools` beneath the app's existing data profile:
`~/Library/Application Support/Koda` or `Koda Nightly` on macOS. A custom
`--user-data-dir` profile also owns its tool state. Stable, Nightly, development
profiles and Zed do not share managed paths, Gradle caches or download archives.
SDK/JDK selections are stored in `environment.json`, outside project settings.

Each tool's schema-1 JSON manifest records its embedded recipe SHA-256,
installation slot, canonical entrypoint, every file/link digest and permission
mode, plus one previous installation. Runtime versions, upstream origins and
licenses remain in the installed `.revision`, `zed-native-importer.json`,
`SOURCE.txt` and bundled notices. `managed::Tool::{recipe, resolve}` provide the
stable interface for runtime consumers. The recipe identity includes the
installer, patch and relevant embedded source bytes; changing them in a Koda app
update makes an older active runtime unavailable until **Install / repair** is
run. The app updater continues to update the app; it never silently replaces
tools or downloads them during startup. Configure Kotlin again after a repair
or update so a project's persisted server path uses the new installation.

**Validate** checks the complete inventory. Normal runtime resolution also checks
the inventory before use. A process-local validation cache uses the exact file
set, expected digests, lengths, modification times, inode/device/change times,
permission modes and link targets; changes invalidate it. Persisted managed Kotlin
paths are checked before language-server startup and must belong to the current
profile and active recipe. Initial hashing runs
on a background executor. Invalid/missing files produce repair guidance instead
of silently selecting an older runtime. Explicit legacy `ANDROID_IDE_*` runtime
overrides remain supported and take precedence; clear an obsolete override to
use the managed runtime.

Installation builds in a private staging directory under a profile-wide
nonblocking kernel lock. Other windows receive a retryable busy error. A complete
distribution is moved to an immutable slot, validated, and selected by an atomic,
fsynced manifest replacement. Failed/cancelled builds leave the previous active
manifest intact. An interrupted process releases the kernel lock; the next
installation removes abandoned staging and named partial downloads. Malformed
regular manifests/settings are preserved under `.corrupt-*` backup names and
can be repaired; symlinks and unsupported future manifest schemas are preserved
and rejected. The active manifest retains a previous installation for **Roll
back** when that installation passes validation and matches this app's recipe.
An older incompatible recipe cannot be rolled back into a newer app.

Older complete slots remain because open processes and persisted project settings
may refer to them. They are not automatically deleted. **Reveal managed storage**
opens the profile location. To reclaim old slots or archive/Gradle caches, close
all Koda windows first, remove unused versions using Finder, restart and configure
affected projects again. Never remove an installation used by another window.

## Download and failure boundaries

Direct upstream archives, source files, compiler dependencies and distributions
use embedded SHA-256 pins and HTTPS, including redirect validation. Transfers
have a 60-second read timeout, a 15-minute deadline and a 2 GiB per-file cap.
Installed inventories are limited to 10 GiB/100,000 files. A watchdog checks the
whole managed profile every two seconds during installation and rejects growth
past 24 GiB/300,000 entries, including Gradle caches and older runtime slots. The
supervised process also has a 30-minute deadline and bounded Build Output.

Debugger builds use a checksum-pinned Gradle distribution, private Gradle cache,
HTTPS repository guard and no persistent daemon. The client JVM matches the
build JVM and uses Gradle's required module opens; the Kotlin compiler runs in
process. This prevents detached single-use/compile daemons escaping cancellation.
Successful setup also terminates remaining children in its owned process group. The embedded, SHA-256-pinned
Gradle verification metadata covers 217 components and 398 artifacts, including
plugin and dependency metadata. Strict verification runs for the adapter, shared
included build and buildSrc; unknown artifacts or checksum mismatches fail closed.
The installed distribution preserves that metadata for provenance. Changing the
adapter's dependency graph requires reviewing and updating this pinned metadata;
provisioning does not trust newly generated checksums automatically. Managed
installation builds the distribution only. The upstream adapter tests are run
separately during development validation because their sample-project fixture
starts another Gradle wrapper outside the managed verification boundary.

**Cancel tool setup** or the Build Output Stop control terminates the supervised
process tree. Tool setup also cancels when its project closes or loses trust.
**Rerun** retries the failed tool operation, rather than an earlier Android build.
**Offline: on** permits only verified direct-download cache entries and uses
Gradle's offline mode; missing artifacts explain that reconnection is needed.
An existing valid installation works offline. Offline repair is possible only
when all required archives and Gradle artifacts are cached. Cache corruption is
discarded and re-fetched on retry; there is no automatic endless retry.

No SDK license acceptance, credentials, signing changes, Gatekeeper exceptions
or security-warning bypasses are performed. macOS execution restrictions should
be handled through normal trusted installation guidance, rather than disabling
the platform's protections.

## Acceptance checks

Automated backend and headless UI tests cover atomic fixture installation,
restart, corruption/repair, failed/checksum-invalid/oversized transfers, offline
cache behavior, interrupted process recovery, concurrent profile locking,
manifest update/rollback compatibility, safe paths and permissions, saved
dependency recovery, tool-specific retry and project-close cancellation.

Before claiming downloaded-app runtime support is validated, test an actual
Apple Silicon macOS installation with a fresh Nightly profile and no source
checkout or `ANDROID_IDE_*`, `ANDROID_HOME` or `JAVA_HOME` exports:

1. Start the downloaded app; choose existing SDK/JDK/CLI paths and provision tools.
2. Open this fixture, sync, configure Kotlin, confirm completion and generated R
   symbols, run on an authorized device, attach/debug/step and render a preview.
3. Cancel an installation and interrupt a download; restart and retry. Check the
   prior runtime still resolves and partial files are recovered.
4. Corrupt a managed library/manifest, confirm actionable validation errors and
   repair. Check offline launch, cached repair and missing-cache guidance.
5. Open two windows and attempt concurrent provisioning. Update app recipe,
   confirm update guidance, then provision and reconfigure. Check compatible
   rollback, isolated stable/Nightly profiles and coexistence with Zed.

A separate fresh-cache Linux/JDK 11 build passed the pinned debugger's Gradle
`:adapter:test` and `:adapter:installDist` tasks with strict verification. This
checks the adapter dependency graph, not the macOS provisioner or device flow.
These interactive macOS/device checks have not been run in the Linux cloud
environment. Headless tests do not establish downloaded-app startup, completion,
device execution, debugger behavior, rendered preview success or Android Studio
feature parity.

## Companion task integration

This work starts on main `c17272361e`, after the Nightly bundle metadata fix in
PR #48. It does not alter release packaging or publish a release.

PR #50's shared variant model is separate and unmerged. Its Kotlin importer
revision changes to `263.4702.0+android-6`; retain that revision and patch when
integrating. Embedded recipe identities automatically change, so the managed UI
will require reinstallation. PR #49's test runner remains separate; it can use
`managed::command_environment` for its Gradle commands without another installer
or discovery model. Reconcile Android panel and README changes when stacking.

The debugger task owns adapter semantics and patch revisions. The provisioner
now incorporates PR #51's exact adapter patch from `84e1f6def2` and revision
`7f05669b642d21afa46ac7b75307fa5d523a7263+android-2`, with unchanged upstream/source/JDK
checksums. This is the runtime dependency; PR #51's debugger UI and session
changes remain separate. The entrypoint and installation-root `.revision` layout
are compatible. Repair builds a new immutable slot and preserves the old runtime;
it never merely relabels an existing installation. Preserve
`install-android-debugger`'s default repository behavior and optional
`main(install_kotlin=False)` interface. Adapter revisions/patches automatically
change `Tool::Debugger.recipe`; consume `managed::resolve(Tool::Debugger)` and
the existing explicit override, not a hard-coded checkout path. Base Kotlin
distribution-version changes must update the canonical entrypoint in both the
Rust `Tool` and Python `ENTRYPOINTS` definitions. Do not accept a manifest with
another tool, recipe, entrypoint or escaped installation slot.
