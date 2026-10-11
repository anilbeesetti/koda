# Pinned Android target and runner discovery fixtures

These 16 complete source files are retained from Android Studio's AOSP
`tools/adt/idea`, `tools/base`, and JetBrains Android plugin sources under Apache
2.0. Original bytes and license headers remain intact. `selection.json` records
each source revision, retained archive identity, path, byte count, and SHA-256.
`provenance.json` preserves the previous verified archive-member inventory and
the subsequent byte-identical selection. This command rechecks complete source
bytes, but does not reopen or reverify compressed archives.

The Rust regressions use these files to test source evidence discovery. They do
not port their original behavior tests, execute Bazel or JUnit, infer an IntelliJ
class hierarchy, or receive original test parity credit. Runtime case counts
remain null, applicability remains unresolved, and the global census remains
incomplete.

The AOSP pins are `a84efec3ba9542d9bfa1255103f0dc94833a3796` for `idea` and
`4a5d2ec9571e2021fc9a4621e24a195f966ee6dd` for `base`. The JetBrains Android
plugin pin is `132bc7c3cf52598117590637d00e81b929444bde`; matching names are
retained as separate source identities, including files whose bytes differ.

Related license text: [Apache License 2.0](../../../../LICENSE-APACHE).
