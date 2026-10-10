# Evaluated project-context fixtures

These are new supplemental fixtures for the requested general-purpose project
context behavior. They are not ports of Android Studio or IntelliJ tests and
grant no original reference-test credit. Their Gradle project logic is test
data; all IDE classification, ownership, command and UI policy is Rust.

Stage each fixture outside the repository before evaluation. Use a verified
official Gradle distribution/wrapper and an isolated Gradle user directory;
do not put wrapper JARs, SDKs, caches, generated outputs or downloaded plugins
in Source. Run the production context adapter through the explicit import
path, retaining raw records, diagnostic output, process exit and command logs.

| Fixture | Candidate runtime | Required result |
| --- | --- | --- |
| plain-jvm | Gradle 9.6.1, JDK 21 | One evaluation; complete non-Android facts; no SDK/ADB/Android bootstrap |
| java-android | Gradle 9.6.1, AGP 9.4.0, SDK 37, JDK 21 | Actual alias-applied application/library AGP facts and Java sources; unavailable Kotlin target API; devices only from complete public AGP observation; Run only with current application model |
| sdk-failure | Gradle 9.6.1, AGP 9.4.0, JDK 21, private missing SDK override | Partial affirmative Android facts remain repairable; no automatic device/run/preview capability |
| convention | Gradle 9.6.1, JDK 21 | Actual applied java-library; raw custom buildSrc output directory; selected-root observer starts before evaluation; generated writes remain current; source edit rejects publication |
| included-convention/root | Gradle 9.6.1, AGP 9.4.0, SDK 37, JDK 21 | Actual convention-applied Android library; external included-build/custom source observers established before bounded second evaluation; external custom generated output remains current; genuine input edits reject publication |
| kmp-jvm | Gradle 8.14, Kotlin 2.2.10, JDK 21 | Actual JVM/common target getters; KMP ecosystem with no Android capability |
| kmp-android | Gradle 8.14, Kotlin 2.2.10, AGP 8.10.0, SDK 36, Build Tools 35.0.0, JDK 21 | Actual Android/JVM targets; Android library has no application Run |
| cmp-desktop | Gradle 8.14, Kotlin/compiler 2.2.10, Compose 1.8.2, JDK 21 | Actual Compose and KMP plugins/desktop target; no Android devices/Run/renderer |

Gradle 9.6.1/AGP 9.4.0 and Kotlin compiler 2.2.10 were found in the prepared
environment. KMP markers, Compose plugin, Gradle 8.14 and AGP 8.10.0 are not
confirmed retained. The official KGP/KMP compatibility tables support Gradle
8.14 and AGP 8.10.0 for Kotlin 2.2.10; Compose 1.8.2 requires Kotlin 2.1.0 or
later and a matching compiler plugin. AGP 8.10.0 supports API 36 at most and
defaults to Build Tools 35.0.0. Platform 36 and Build Tools 35.0.0 are missing
prerequisites, separately from the retained API 37 environment used by AGP
9.4.0. A separate scoped runtime/download grant is required before evaluation.
The exact historical Compose/Kotlin pair remains unverified in execution.
Merely listing versions or running pure Rust JSON tests is not a
successful fixture evaluation. The pinned Android Studio behavioral reference
remains idea a84efec3ba9542d9bfa1255103f0dc94833a3796.

The selected root is directly observed before the first evaluation, so an
initially missing buildSrc directory does not require a second pass or reliance
on Worktree ignores. Newly discovered external included-build/source observer
roots require exactly one additional verification evaluation inside the same
explicit import. No first-pass operational context is published during that gap.
If the final snapshot introduces unwatched inputs, import fails explicitly;
there is no third pass or detached retry. The SDK-failure harness creates private
`local.properties` pointing at a missing private SDK path and never changes the
shared SDK.

Also evaluate SDK/configuration failure, convention/applied-plugin aliases,
new external included-build observers, custom source/output directories,
HEAD changes, input edits during both passes, trust revocation, root removal,
window closure and A-B-A activation. A legitimate sibling `projectDir` module
requires production ownership integration and its own fixture; it remains
unverified until that integration is complete. Preserve unrelated ordinary
projects, configured language servers and the top project/workspace/branch
changer throughout these checks.
