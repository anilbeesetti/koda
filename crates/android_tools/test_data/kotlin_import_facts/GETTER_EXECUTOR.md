# Owned getter capture executor — actual validation pending

The eight original strict capture/schema/tests/reference fixture files remain
byte-identical to `c1bd317ca39b923ed19dad51937369443cb66c62`. Attribution and the
Apache-2.0 references remain in NOTICE.md and reference/. New executor tests are
supplemental protocol regressions, not original Android Studio parity results.

`KotlinGetterCapture<T, H>` retains the independently issued host revision H.
Use the current `module_import::ImportRevision` including context generation and
import revision. Its current-host callback must read current host state; finish
returns the original H for the strict adapter, never a newly made token. The
host must reject conversion to a complete import plan when any required request
ID is absent; missing classes/capabilities preserve Unknown, never prove absence.

Rust issues each exact immutable reflected request before execution. The owned
Gradle/JVM bridge first returns a separate append-only discovery frame, then the
corresponding event. Rust verifies actual runtime artifact bytes and digests,
retains class/loader/method identity, event order, null/unavailable states and
iterable order, and validates the final observations with the unchanged strict
schema. Runtime classes with unavailable/non-file origins or ambiguous plugin
loaders fail the capture; they are not synthesized. Return kinds and official
getter membership remain Rust policies. The JVM bridge only performs reflection
and raw serialization because those APIs and live objects exist in Gradle's JVM.
This is the approved official Gradle/JVM exception, not an IntelliJ importer.

The explicit fixture file inventory is sorted and hashes each relative path,
byte length and digest. Root must supply every original prepared fixture file,
including all 39 KOTLIN_KAPT files and all preparation transformations. Generated
build outputs are excluded intentionally; file mutation, disappearance, ambiguous
paths and symlink ancestors reject the capture. Record original and transformed
fixture inventories separately. Snapshots with version names do not establish
an exhaustive effective version runner.

The Root-run example accepts a JSON configuration with projectRoot, wrapper,
javaHome, modelOutput, hostRevisionFile, fixtureFiles, selectedVariants,
captureId, sourceEpoch, timeoutMillis and outputDirectory. modelOutput contains
an independently captured current Basic/importFacts record. hostRevisionFile
contains contextGeneration, modelRevision, selectionRevision, importRevision and
root. The example reads this file before/after capture, retains it separately,
and writes separate owned expected context, events, request plans, fixture
boundary and original host revision to a fresh output directory. Gradle stdout
and stderr survive failure in gradle-logs; exceeding the combined diagnostic
budget rejects the entire capture. There is no output truncation or partial
successful receipt. Cancellation closes the owned socket and launcher; Root's
runtime wrapper must independently verify descendant/JVM shutdown before any
successful runtime receipt.

Original KOTLIN_KAPT preparation/version expansion and the exact members
`debugAndroidTest`, `debug`, `debugUnitTest` are still unported/not run. The
executor does not implement KAPT model synthesis or multiply synthetic fixtures
into reference credit. Real original/version Gradle probes, full strict Clippy,
full workspace tests, normal app import workflows and five final reviews remain
required. No Kotlin, KAPT, feature or parity completion is claimed here.
