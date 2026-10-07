# Android project tree reference material

`simpleApplication/` contains all 26 original `SIMPLE_APPLICATION` files from
AOSP `tools/adt/idea` at `a84efec3ba9542d9bfa1255103f0dc94833a3796`
(`studio-2026.2.1`). `reference/` preserves the complete test suite and selected
production nodes and test-fixture loader sources at that same revision. All
copied bytes, including individual Apache notices and files without individual
headers, remain unchanged. `LICENSE-APACHE` and `provenance.json` provide the
repository attribution, paths, hashes, and license audit notes.

The Rust component tests read the original file inventory and supply explicit
typed source/provider/class facts. Additional generated folders and files are
immutable test inputs. No Gradle import, generated-root discovery, Java parser,
file watcher, GPUI tree, or sample application test execution occurs here.
The five Gradle-backed `AndroidProjectViewTest` methods remain unported; matching
a tree path from supplied facts does not satisfy their model assertions.

The original sample `UnitTest.java` intentionally expects `5` for `2 + 2`.
Its bytes are preserved. The raw project uses AGP 1.5; the upstream test loader
patches a temporary copy for `AGP_CURRENT` before importing it. A later Rust
integration task must preserve and document that preparation rather than edit
these originals.

Projection scope includes supported source/generated groups, compacted package
labels, literal assets, resource type/name grouping, provider annotations,
manifest files, `resources.properties`, and module `google-services.json`.
Resource qualifier ordering and selecting the best qualified resource need
`FolderConfiguration` semantics in later resource tooling. Resource group
navigation is therefore absent; individual variants retain their file targets.
Multiple Java class facts are explicit component inputs; this component does
not establish IntelliJ PSI behavior for multi-class files.

The supported source groups are a subset of `AndroidSourceType.BUILT_IN_TYPES`.
Build-script groups, dependencies, non-Android/included-build modules, and the
remaining source kinds need later model and view integration.

The original `app/src/main/res/layout/absolute.xml` ends in a blank line.
Default Git whitespace checking reports that retained line. Owned Rust/docs
pass strict checking; the copied originals are checked by exact byte hashes.
