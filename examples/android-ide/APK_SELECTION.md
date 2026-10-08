# APK selection for Run and Debug

## Next Player failure

Next Player enables ABI outputs for `armeabi-v7a`, `arm64-v8a`, `x86`, and
`x86_64`, with `isUniversalApk = true`. An assemble task therefore describes
five alternative, independently installable APKs in AGP's `output-metadata.json`.
Koda previously required exactly one element with no filters, rejecting the
whole listing even though a universal APK was present.

The fix chooses one output. It does not change the application's Gradle setup,
install every ABI alternative, or assume a filename such as `app-debug.apk`.

## Android Studio and IntelliJ IDEA research

Sources inspected on 2026-10-02:

- [Next Player application build configuration](https://github.com/anilbeesetti/nextplayer/blob/e217bf9d38093429ce06db350331e34fec6ba8ca/app/build.gradle.kts).
- [Android's multiple APK build documentation](https://developer.android.com/build/configure-apk-splits).
  `isUniversalApk` adds a universal output alongside per-ABI outputs. Active ABI
  in the IDE editor does not determine which APK is deployed.
- [Android Studio GradleApkProvider](https://android.googlesource.com/platform/tools/adt/idea/+/7db9010947773ee7309f268ded237698fa38f2ef/project-system-gradle/src/com/android/tools/idea/run/GradleApkProvider.java).
  `getApks` gets the device's ABIs, loads the AGP output listing/redirect, and
  delegates selection to `GenericBuiltArtifactsSplitOutputMatcher`.
- [AGP GenericBuiltArtifactsSplitOutputMatcher](https://android.googlesource.com/platform/tools/base/+/4f5db9aac562aeb95065de85317e45cec77e40be/sdk-common/src/main/java/com/android/ide/common/build/GenericBuiltArtifactsSplitOutputMatcher.kt).
  Filters incompatible ABI outputs, compares version codes, then ABI preference.
  Missing version codes default to 1. The result is one standalone APK.
- [IntelliJ Android plugin GradleApkProvider](https://github.com/JetBrains/android/blob/b75170bd5e9653995ee5e0d91fdabef3220d4d29/project-system-gradle/src/com/android/tools/idea/run/GradleApkProvider.java),
  [legacy SplitOutputMatcher](https://github.com/JetBrains/android/blob/b75170bd5e9653995ee5e0d91fdabef3220d4d29/project-system-gradle/src/com/android/tools/idea/run/SplitOutputMatcher.java), and
  [its tests](https://github.com/JetBrains/android/blob/b75170bd5e9653995ee5e0d91fdabef3220d4d29/project-system-gradle/testSrc/com/android/tools/idea/run/SplitOutputMatcherTest.java).
  The Gradle provider delegates modern metadata selection to the same AGP
  matcher; its older model-based matcher uses the same ABI/version ordering.
  IntelliJ's non-Gradle provider follows a separate module/artifact path and is
  not the relevant path for Next Player.

The ordering is **highest version code first**, among compatible outputs. With
identical version codes, the primary ABI beats universal; universal beats
secondary ABIs; secondary ABIs follow the device's order. A higher-version
secondary ABI output can therefore beat a lower-version primary output.

ABI outputs from `splits.abi` must be distinguished from an app bundle's
base/configuration/feature split set. Studio has additional paths for bundles
and dependent features. That requires installing a coherent set together, not
choosing a single ABI alternative. This change covers standalone AGP outputs.

## Koda behavior

Run and Debug query `adb -s <selected serial> shell getprop` with a 15-second
timeout. `ro.product.cpu.abilist` supplies ordered ABIs; older devices fall back
to `ro.product.cpu.abi` and `ro.product.cpu.abi2`. Duplicate ABIs are removed
without changing order. Empty or malformed properties stop deployment.

`AndroidTarget::apk_for_device` applies the version/ABI ordering above and
passes exactly one canonical APK path to the native Android SDK `adb` deployment
flow used by Run and Debug. Debug uses the application ID from the selected
variant's metadata.
A missing winning APK fails rather than silently falling back to a stale output.
Files for unselected ABI alternatives need not exist.

| Outputs (equal version codes) | Device ABIs | Selected output |
| --- | --- | --- |
| Next Player's four ABIs + universal | x86_64, x86 | x86_64 |
| Next Player's four ABIs + universal | arm64-v8a, armeabi-v7a | arm64-v8a |
| x86 + universal | x86_64, x86 | universal |
| x86 + armeabi-v7a | x86_64, x86, armeabi-v7a | x86 |
| x86 only | arm64-v8a | error with device ABIs and rebuild guidance |

Device-independent `apk()` / `apk_paths()` choose a universal APK even when
ABI alternatives exist, keeping Compose preview usable without a connected
device. An ABI-only build needs a universal output for those tools.

Existing metadata version, artifact type, variant identity, relative path,
canonical directory containment, regular-file, extension, and comma checks
remain in place for the selected APK. Equal winning ranks are rejected rather
than relying on metadata order. Unsupported output types reject deployment;
density, unknown, or multiple filters do not become universal candidates.

The CLI target adapter does not expose variant-level native-library ABI filters.
Consequently, an unfiltered APK is a universal candidate based on its output
metadata; native-library compatibility is ultimately checked by installation,
as it was before this change. Full bundle/configuration split installation and
density selection remain outside this implementation.

Selection takes O(outputs × device ABIs) work and stores only the current best
candidate. Typical Next Player input is five outputs and a handful of ABIs.
There is one asynchronous ADB property request per deployment, no additional
Gradle build, and no polling. Metadata and filesystem work stays on the
background executor.

## Validation

`cargo test --locked -p android_tools` passes 21 tests, including Next Player's
five-output layout, ARM/x86 selection, version priority, universal preference,
legacy properties, ambiguity, unsupported outputs, missing selected/unselected
files, traversal, variant mismatch, and symlink escape. APK contents are test
fixtures, not executable application binaries.

Additional checks passed:

- `cargo check --locked -p android_ui` (isolated CMake was required in this environment).
- `cargo clippy --locked -p android_tools --all-targets -- -D warnings`.
- `cargo fmt --all -- --check` and `git diff --check`.

Three independent reviews covered correctness and path handling, runtime and
performance, and IDE behavior/test coverage. They found no blocking issues.
The runtime review's suggestion to keep the entire property query off the UI
executor was applied and re-reviewed.

Live Next Player build/install/launch has not been verified in this cloud
session: it has no Android SDK, ADB, emulator, or `/dev/kvm`. To verify locally,
open Next Player, sync, select `:app · debug` and an x86_64 or arm64 emulator,
and use Run. Confirm the appropriate single ABI APK is deployed. Repeat with
Debug and verify attachment to `dev.anilbeesetti.nextplayer.debug`. Compose
preview should still select the universal output without a device.
