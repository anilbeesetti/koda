# Raw import identity getter transport: source draft

The additive `importFacts` sidecar preserves the existing Basic model and V2
artifact fields. It captures official Gradle project names, local paths, project
directories, root names/directories and parent paths, plus public build-tree and
internal reference identity paths independently. Idea plugin lookup is observed
without applying a plugin, and nullable module names stay distinct from failed
or missing getters. The complete six API/helper sources and Apache attribution
are retained under `crates/android_tools/test_data/import_facts`.

The bridge captures at task-graph readiness after all project-evaluation
callbacks, preserving names changed by late build/plugin configuration. An
authored Java+Idea fixture records an independent final getter oracle; its real
Gradle execution is still pending.

Rust checks object-only decoding, explicit nullable result payloads, getter
provenance, duplicate identities and contradictory available observations. The
raw ordered catalogue retains root holders beyond compiled Basic modules, with
an index for lookups. Captures bind exact Basic module/directory/kind/variant
identity and caller model/selection revisions. Missing required observations
return typed errors; they do not replace or erase the caller's Basic model.

Thirty authored supplemental tests cover transport, identity, nullable
observations and stale bindings. All 30 pass under normal default-feature Cargo execution, with zero failed,
ignored or filtered tests. The default library build and full workspace Rust
formatting pass. Compiler JSON binds freshly compiled library/tests to this
checkout; all 4,738 source hashes, index and untracked set stayed unchanged
through the gates. The initial 27-pass/one-failure output and its exact positional
wrapper boundary remain preserved. The third fix qualifies object-only trait
decoding; all earlier expectations remain, with two added regressions.

All-feature tooling tests/build, strict repository Clippy and actual Gradle/SDK
probes remain pending. Owner static reviews and Root full-app/full-CI/native
gates do not become runtime passes from these default component checks. No
scoped completion or original-test credit is claimed.

Included builds and standalone Java-only Basic imports remain unsupported. TAPI
IdeaModule names, imported-name collision resolution, Kotlin/facet capability
import and tree publication are dependent tasks. Original naming/facet/Kotlin
and generated-tree methods remain applicable/unported; the canonical parity
ledger is unchanged and the global census remains incomplete.

The official Gradle JVM and minimal getter serialization are existing necessary
non-Rust exceptions. IDE identity validation and import policy stay in Rust; no
IntelliJ importer or facet runtime is hosted. No startup, memory, large-project,
hardware rendering or native workflow performance claim is made by this draft.
