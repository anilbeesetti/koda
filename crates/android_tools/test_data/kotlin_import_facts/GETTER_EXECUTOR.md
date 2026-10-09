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

The official request plan separately records
`PluginContainer.hasPlugin("com.android.base")` as a raw, non-null Boolean getter
with descriptor `(Ljava/lang/String;)Z`. Its receiver and preceding request come
from the same `Project.getPlugins()` observation. Android application/library
plugin lookups do not substitute for this predicate; missing or unavailable
capabilities preserve Unknown in the importer.

The explicit fixture file inventory is sorted and hashes each relative path,
byte length and digest. Root must supply every original prepared fixture file,
including all 39 KOTLIN_KAPT files and all preparation transformations. Generated
build outputs are excluded intentionally; file mutation, disappearance, ambiguous
paths and symlink ancestors reject the capture. Record original and transformed
fixture inventories separately. Snapshots with version names do not establish
an exhaustive effective version runner.

The Root-run example accepts a JSON configuration with projectRoot, wrapper,
javaHome, modelOutput, hostRevisionFile, fixtureFiles, selectedVariants,
captureId, sourceEpoch, timeoutMillis, guardianLauncher, guardianLibrary,
shutdownTimeoutMillis and outputDirectory. modelOutput contains
an independently captured current Basic/importFacts record. hostRevisionFile
contains contextGeneration, modelRevision, selectionRevision, importRevision and
root. The example reads this file before/after capture, retains it separately,
and writes separate owned expected context, events, request plans, fixture
boundary and original host revision to a fresh output directory. Gradle stdout
and stderr survive failure in gradle-logs; exceeding the combined diagnostic
budget rejects the entire capture. There is no output truncation or partial
successful receipt. The required guardianLauncher is the Rust
`kotlin_jvm_guardian` binary; guardianLibrary is its native cdylib. The public
transport executes this launcher and injects the Rust native JVM agent through
JAVA_TOOL_OPTIONS into the owned invocation. The original JAVA_TOOL_OPTIONS
are retained. No existing shared Gradle daemon is reused or signalled.

On Linux 6.5 or newer, OS-authenticated socket peer identities provide kernel
pidfds. The Rust launcher installs itself as a subreaper before launching the
wrapper. It remains the ancestor of double-forked or detached children, including
JVMs which have not loaded their native agent when cancellation arrives. Shutdown
requires the launcher's authenticated ECHILD acknowledgement, stable process
handle exit, and direct launcher reaping. The agent exits its own JVM immediately
when its lifeline closes; it does not wait for blocked project configuration or
getter code. Startup failure, cancellation, deadline expiry, malformed responses,
stale host revisions, retention overflow, and drop all close the owned lifetime.
Cleanup is bounded by shutdownTimeoutMillis (at most 30 seconds), and cleanup
failure is reported alongside the original capture error. Root must still run
the process regressions and a real official Gradle probe; source inspection is
not runtime closure evidence.

Windows Job containment, macOS kernel child tracking, and safe support for older
Linux kernels remain planned. Those platforms currently return a typed
capability-unavailable error before starting a process. They are unported, not
inapplicable. Bundling the two guardian artifacts into the final IDE distribution
and connecting the real getter producer to the project importer remain pending.

The whole capture retains at most the unchanged strict 16 MiB record and 131,072
JSON value nodes. Streaming accounting checks incoming frames before decoding,
issued requests before handing them to a transport, and incoming discovery/events
before retention or cloning. Final serialization uses a bounded writer over
borrowed context and events, without constructing a complete intermediate JSON
Value. Budget exhaustion rejects the capture and aborts its runtime; no values,
events, or raw diagnostics are truncated. The final diagnostic budget is checked
after verified owned process/writer closure. Incoming immutable discovery rows
are indexed once per update; duplicate, rewritten, removed, or reordered prior
identities reject the capture.

Root validation commands, with the normal repository toolchain and environment:

```text
cargo build --locked -p kotlin_jvm_guardian --lib --bin kotlin_jvm_guardian
cargo test --locked -p kotlin_jvm_guardian --test process_lifetime
cargo test --locked -p android_tools --lib kotlin_getter_executor
cargo test --locked -p android_tools --lib kotlin_capture_budget
./script/clippy --locked -p android_tools -p kotlin_jvm_guardian
```

The binary and native library paths must come from the actual build output and
be pinned for each probe. Process-fixture tests exercise the real Rust supervisor
with controlled Rust children. They do not establish official JVM Agent_OnLoad,
Kotlin getter membership, KAPT preparation, or reference-version parity.

Original KOTLIN_KAPT preparation/version expansion and the exact members
`debugAndroidTest`, `debug`, `debugUnitTest` are still unported/not run. The
executor does not implement KAPT model synthesis or multiply synthetic fixtures
into reference credit. Real original/version Gradle probes, full strict Clippy,
full workspace tests, normal app import workflows and five final reviews remain
required. No Kotlin, KAPT, feature or parity completion is claimed here.
