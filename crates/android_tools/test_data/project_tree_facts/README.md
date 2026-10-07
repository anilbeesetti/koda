# Evaluated provider source attribution

The unchanged source files under `reference` retain their original Apache 2.0
notices. `provenance.json` records the pinned archive members, commits, byte
counts and SHA-256 values. Rust selection and lookup are adapted from these
contracts. The AGP JVM bridge captures evaluated tooling-model data because
Gradle and its official model producers run in the JVM; ordering, lookup,
validation and tree projection remain Rust.

The accompanying tests are supplemental component regressions. They do not
port or credit any of the five original `AndroidProjectViewTest` Gradle tree
methods. Their original source, fixture bytes and assertions remain unchanged
under the existing `project_tree` test-data directory. GPUI selection, scanning,
refresh, navigation, settings and the complete original integration driver
remain separate pending work.

Native AGP membership, Studio current-provider order, the raw DSL catalog and
producer source-root encounters are distinct facts. No source-set directory
name establishes a role or a provider. The pinned converter's provider prefix
classification is checked for agreement with actual artifact keys; disagreement
remains unavailable instead of guessing. Model producers before 11 need the
unimplemented legacy asset filter. Nonempty test-suite conversion remains
unavailable in the current capture bridge.

Resource file annotations follow a distinct exact-resource-root lookup from
`AndroidResFileNode`; ancestry lookup remains in use for ordinary entries and
`resources.properties`. The new typed API preserves unknown/missing/file/stale
resource-root outcomes rather than returning an invented provider.
