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
Gradle/JVM bridge first returns one discovery bootstrap, then a separate
append-only delta before each corresponding event. Rust independently issues a
fresh session nonce; every delta must match that nonce, the retained request ID,
next sequence and exact predecessor row counts. Only new observations and
explicit append-only loader artifact entries are transmitted. The bridge cannot
replace, remove or reorder an accepted row through this protocol. Rust verifies
actual runtime artifact bytes and digests,
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
stale host revisions, retention overflow, and drop initiate owned lifetime closure.
The caller's cleanup wait is bounded by shutdownTimeoutMillis (at most 30
seconds), and cleanup failure is reported alongside the original capture error.
After a timeout, the existing supervisor retains the direct child, private
lifelines and reaping ownership; the launcher retains subreaper ownership until
its descendants are reaped. The original timeout remains a failure, and later
reaping does not turn it into a verified successful shutdown. Every intermediate
ancestry hop is pinned and rechecked before peer admission. Malformed owned-peer
messages close that connection at the first failure, and failure history remains
bounded with an explicit additional-failure count. Root must still run
the process regressions and a real official Gradle probe; source inspection is
not runtime closure evidence.

Windows Job containment, macOS kernel child tracking, and safe support for older
Linux kernels remain planned. Those platforms currently return a typed
capability-unavailable error before starting a process. They are unported, not
inapplicable. Bundling the two guardian artifacts into the final IDE distribution
and connecting the real getter producer to the project importer remain pending.

The complete encoded capture is bounded by the unchanged strict 16 MiB record
and 131,072 JSON value nodes. Streaming accounting checks incoming frames before
decoding. Initial context is measured once; immutable append-only row validation
then permits exact cached totals plus only new requests, events, discovery rows,
loader artifact entries and array separators. Every addition is checked before
retention or cloning. Final serialization uses a bounded writer over
borrowed context and events, without constructing a complete intermediate JSON
Value. Budget exhaustion rejects the capture and aborts its runtime; no values,
events, or raw diagnostics are truncated. The final diagnostic budget is checked
after verified owned process/writer closure. Bootstrap observations are indexed
once; delta validation, artifact hashing, indexing and retention accounting then
visit only new rows and loader additions. The transport does not retransmit or
reparse the complete discovery prefix per getter. Duplicate or reused identities,
foreign/replayed sessions, invalid predecessor counts, and changed class/loader
provenance reject the capture. Exact class-ID/target predicates retain positive
and negative results, with one checked cumulative vertex/edge work budget and
health checks during traversal. Separate class loaders retain distinct identities.
Object, class and catalogue lookups use retained keyed indexes rather than
scanning task vectors. A failure latches the capture
and closes its transport once, including errors during project-plan preparation.
Capture health is checked between bounded partial socket writes, while receiving,
and between every 64 KiB fixture/artifact read. A growing file rejects hashing
without following an expanding EOF. These source changes still require actual
tests, official JVM probes and large-project measurements.

Rust supplies the unchanged producer byte, scalar and value-node limits before
bridge construction. The JVM boundary admits new observations and mutable row
fields against exact cumulative encoded usage before retaining them. Getter
Lists and Iterables are visited once under checked per-item limits, without
`every`, `collect`, or an unbounded `toList` copy. Raw JSON is encoded into
bounded chunks of at most 64 KiB; value nodes, scalar UTF-8 bytes, escaped output,
depth, cycles, deadline and interruption are checked during encoding. A complete
JSON String and complete UTF-8 copy are never created. Both delta and event are
prepared successfully before either frame is published. Exceeding a limit
rejects the capture; values and diagnostics are never truncated. These limits
bound additional bridge observations and encoding, while official getter code
and JVM reflection remain the external runtime and require actual runtime
resource measurements.

Physical JVM object identities retain stable observed handles within each
project scope. A legal shared immutable List can therefore return to two projects
without copying its physical object or inventing a new class/loader origin.
Same-project repeats reuse the same handle; wrong-kind and foreign-receiver
requests still fail. The supplemental Root-run
`kotlin_bridge_fixture_probe` assembles the production bridge class with
`bridge_shared_collections.gradle`, invokes actual reflected ThreadLocal getters
for shared empty and ordered lists in real `:first` and `:second` Gradle projects,
and checks scoped handles, exact physical identity, class/loader provenance,
ordered values and both negative ownership cases. This controlled JVM fixture is
not Kotlin/KAPT reference parity. Root must execute it with the actual built
Rust launcher/native library and an independently prepared two-project fixture.
The same probe also assembles `bridge_producer_limits.gradle` with the production
bridge unchanged. Eleven actual JVM cases cover lazy oversized String Lists and
object Iterables, a giant scalar, escape-heavy allocation, cycles, exact byte and
node boundaries, Unicode and order, cumulative discovery admission, zero-byte
publication on a rejected event, and deadline/interruption during expansion.
The probe independently checks these observations in Rust and launches a second
owned JVM invocation with an uncaught producer rejection; it requires a failed
wrapper status, the original diagnostic cause, and verified owned closure.
The existing shared-list assertions remain intact; their project setup now uses
the same admission path as production. All of these checks await Root execution
and add no original reference-test credit.

The full framed Rust regression transports one bootstrap plus delta/event frames
through task planning at 100 and 1,000 applicable tasks and 10,000 non-Kotlin
tasks. It counts actual serialized bytes, parsed discovery rows and validation
work, checks the largest legal default-budget planning boundary, and passes the
unchanged final strict decoder. These are test declarations awaiting execution;
they do not establish official Gradle CPU/RSS or startup performance.

Root validation commands, with the normal repository toolchain and environment:

```text
cargo build --locked -p kotlin_jvm_guardian --lib --bin kotlin_jvm_guardian
cargo test --locked -p kotlin_jvm_guardian --lib
cargo test --locked -p kotlin_jvm_guardian --test process_lifetime
cargo test --locked -p android_tools --lib kotlin_getter_executor
cargo test --locked -p android_tools --lib kotlin_capture_budget
cargo test --locked -p android_tools --lib kotlin_getter_lifetime
cargo test --locked -p android_tools --test kotlin_import_facts
./script/clippy --locked -p android_tools -p kotlin_jvm_guardian
```

The binary and native library paths must come from the actual build output and
be pinned for each probe. The shared-list probe command is
`cargo run --locked -p android_tools --example kotlin_bridge_fixture_probe -- WRAPPER FIXTURE_ROOT JAVA_HOME GUARDIAN_LAUNCHER GUARDIAN_LIBRARY`.
`FIXTURE_ROOT` must contain evaluated `:first` and `:second` projects. Process-fixture tests exercise the real Rust supervisor
with controlled Rust children. They do not establish official JVM Agent_OnLoad,
Kotlin getter membership, KAPT preparation, or reference-version parity.

Original KOTLIN_KAPT preparation/version expansion and the exact members
`debugAndroidTest`, `debug`, `debugUnitTest` are still unported/not run. The
executor does not implement KAPT model synthesis or multiply synthetic fixtures
into reference credit. Real original/version Gradle probes, full strict Clippy,
full workspace tests, normal app import workflows and five final reviews remain
required. No Kotlin, KAPT, feature or parity completion is claimed here.
