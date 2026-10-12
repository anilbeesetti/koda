# Official V2 generated artifact transport

The task exports official Android SDK generated-artifact facts beside the existing Basic project model. Rust decodes and validates the independent sidecar, binds it to the Basic model revision and identities, and evaluates the pinned model-version feature rules. Existing `ProjectModel`, `Module`, `Variant`, and `Component` fields and all prior tests and fixtures are preserved.

Source commit: `830ff53de76dbecd5fc3761f1c5f5bae164763c2`. Branch: `android-studio-task/1-v2-artifact-facts`, based on `ac58815bb268d338bea40a31fff630734cd719f4`. The owner checks below passed. The lead's combined application build, native checks, full workspace CI, exact parity captures, and protected-branch merge remain separate gates.

## API and behavior

`android_tools::generated_artifacts::parse_generated_artifacts` returns a `GeneratedArtifactSnapshot` independently from Basic parsing. The snapshot retains official module, variant, main/host/device/test-fixture artifact identities; producer, minimum-consumer, AGP and model-schema versions; Basic build-folder facts; and raw generated source, resource, asset and classpath collections. Encounter order and duplicates are retained. Generated classpaths remain separate from source folders.

`CapturedField` distinguishes available values, unavailable getters, and fields that do not apply. `ArtifactSlot` distinguishes an absent artifact from an available empty collection. Null or wrongly typed collections/maps become unavailable rather than empty. `ensure_current` rejects stale revisions or changed Basic identities. Object-only wire decoding rejects positional arrays; validation rejects malformed schemas, versions, identities and paths, and enforces a 16 MiB model-record limit.

The Rust feature policy uses model producer >= 11.0 for generated assets and model producer >= 8.9 or AGP >= 8.2.0-alpha07 for generated classpaths. Unproved older fallback behavior produces typed capability failure while retaining raw facts. Signed version comparison ignores descriptions for ordering, while record equality preserves descriptions. The existing unsigned Basic-model version contract is unchanged.

## Validation

- `cargo test --locked -p android_tools --all-features --message-format=json`: 202 passed, 0 failed, 0 ignored, 0 filtered; includes all 28 new tests.
- `./script/clippy --locked -p android_tools`: passed with the repository's release, all-target, all-feature and deny-warning settings.
- `cargo fmt --all -- --check`: passed.
- `cargo build --locked -p android_tools --all-features --lib --message-format=json`: passed. Compiler JSON binds the current source to the freshly compiled normal Cargo library and test executables.
- Eleven extracted production-getter boundary cases were serialized through the current bridge and asserted by a Rust probe: passed.
- Five actual SDK modes passed: Java BuildConfig, bytecode BuildConfig, a custom DSL field forcing Java despite a bytecode request, disabled BuildConfig, and disabled host/device test artifacts. The probes also checked official resources/assets outside the build folder, absent test fixtures, actual artifact identities and bytecode remaining a classpath.
- The complete 4,612-file source guard, 184 immutable originals, and 13 original/private fixture inputs matched. All five owned single-use SDK daemons exited; unowned processes were left untouched.

All five independent code, UI, UX, performance and quality reviews passed for the current source and these recorded owner gates. Their final addenda and earlier failure/fix history are retained in the evidence package. These verdicts preserve the separate lead integration requirements.

SDK observations used AGP 9.4.0, Gradle 9.6.1, JDK 21 and Android SDK/build tools 37. They are supplementary compatibility evidence, not equivalent execution of the pinned Studio helper or original generated-project-view fixtures. Rust timing/RSS figures describe the entire isolated probe process, including I/O, parsing, assertions and JSON output. They do not establish IDE startup, decoder-only or large-project budgets. Exact measurement-runner source was not retained; the method and executed commands are disclosed in the evidence addendum.

The disabled-artifact configuration confirms explicit empty release test maps alongside enabled debug artifact keys. The baseline already lacked release test artifacts, so this probe does not establish a before/after artifact-removal workflow.

## Reference parity and attribution

The complete pinned `ModelVersionsTest.checkModelVersionOrdering` and `checkModelConsumerVersionOrdering` behaviors have Rust tests. Original signed minimum/maximum values, ordered tuples, descriptions, reversed inputs and full-record order assertions remain intact. The original test source, pinned `BasicModules.kt`, Apache 2.0 license and notice are retained under `crates/android_tools/test_data/generated_artifacts`.

The two rows in `ports/v2-artifact-facts.json` are proposals with passing owner execution evidence. Canonical ledger promotion requires the lead's exact captures and combined gates. `checkSupportsParallelSync` and all five original generated-project-view flows remain unported. This task does not establish an exhaustive suite/runtime census or mark Phase 1 complete.

## Exception and remaining work

The existing Gradle/Android SDK JVM serialization bridge is the sole non-Rust component changed by this task: official tooling model objects and getters live inside the isolated Gradle JVM. The bridge exports those values; Rust owns decoding, version policy, validation and IDE behavior. No new non-Rust exception was introduced.

Next consumers must separately implement the pinned generated-root filtering/light-class policy, language/facet equivalence, and imported module display identity before integrating these facts into the tree. AAPT, data-binding base, SafeArgs and external KAPT filtering remain unported. The current tree adapter continues to consume its existing supported subset. There are no tree UI, controller or language-server changes here.

Portable raw results, compiler identities, source guards, reviewer reports and preserved failure history are under `evidence/v2-artifact-facts`. Full fixture copies, probe source, frozen binaries and prior source snapshots remain in the external task artifact directory; their hashes are recorded for handoff.
