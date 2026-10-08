# Raw import identity getter transport: source draft

The additive `importFacts` sidecar preserves the existing Basic model and V2
artifact fields. It captures official Gradle project names, local paths, project
directories, root names/directories and parent paths, plus public build-tree and
internal reference identity paths independently. Idea plugin lookup is observed
without applying a plugin, and nullable module names stay distinct from failed
or missing getters. The complete six API/helper sources and Apache attribution
are retained under `crates/android_tools/test_data/import_facts`.

Rust checks object-only decoding, explicit nullable result payloads, getter
provenance, duplicate identities and contradictory available observations. The
raw ordered catalogue retains root holders beyond compiled Basic modules, with
an index for lookups. Captures bind exact Basic module/directory/kind/variant
identity and caller model/selection revisions. Missing required observations
return typed errors; they do not replace or erase the caller's Basic model.

Twenty-eight authored supplemental tests cover transport, identity, nullable
observations and stale bindings. They are not yet executed. The current lease
authorizes source-only work; formatting, library/test/Clippy checks and real
Gradle/SDK captures await an exclusive runtime slot. All five independent owner
review areas and Root's full-app/full-CI/native checks remain pending. No scoped
completion or original-test credit is claimed.

Included builds and standalone Java-only Basic imports remain unsupported. TAPI
IdeaModule names, imported-name collision resolution, Kotlin/facet capability
import and tree publication are dependent tasks. Original naming/facet/Kotlin
and generated-tree methods remain applicable/unported; the canonical parity
ledger is unchanged and the global census remains incomplete.

The official Gradle JVM and minimal getter serialization are existing necessary
non-Rust exceptions. IDE identity validation and import policy stay in Rust; no
IntelliJ importer or facet runtime is hosted. No startup, memory, large-project,
hardware rendering or native workflow performance claim is made by this draft.
