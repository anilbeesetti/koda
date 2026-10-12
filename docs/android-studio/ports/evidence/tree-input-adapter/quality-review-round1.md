# Tree input adapter: independent code quality review, round 1

Verdict: **PASS for the frozen source quality scope**. No additional quality findings.

This is a read-only review, not an executed test, formatting, strict Clippy, build, full-app or native UI pass. Those checks remain pending under Root's Cargo lease. Existing independent UI/UX findings remain task blockers and are not waived by this verdict; the consolidated correction must receive affected reviews again.

Reviewed worktree: `/workspace/android-studio-tree-input-adapter`.

Frozen source hashes verified before and after review:

- `crates/android_tools/src/project_tree_adapter.rs`: `67dbb26617d07caee7db19047aa0cdb82c8572e34de305e97847b7af32549c09`.
- `crates/android_tools/tests/project_tree_adapter.rs`: `eeaaaa01809c3dabebbcdca3f84b3ad22b02db6060de54db4ffb858c4314133e`.
- `crates/android_tools/src/android_tools.rs`: `f4b033f8aa13b8141f706a1f24455c1d5500940ef5e4adffa46edd8baf1f2891`.

## Source quality

- The new file is a coherent boundary component, exported with one additive module declaration. It follows the existing crate layout and adds no Cargo dependency or lockfile change. `Cargo.toml`, `Cargo.lock` and the crate manifest are byte-identical to HEAD.
- Typed `AdapterUnavailableReason` / `AdapterUnavailable` preserve a reason, detail and optional physical path, implement `Display` and `Error`, and propagate the existing provider errors (`project_tree_adapter.rs:70-117`). Required presentation/provider/path inputs use fallible preparation (`:180-224`); source code contains no `unwrap`, `expect`, panic or silently discarded fallible result.
- Revision validation rejects cross-module, variant and model captures before projection (`:425-435`) and rejects stale Java bytes or bytes attached to a non-Java path (`:466-483`). Public capture types make the host producer contract explicit (`:359-394`). The module rustdoc explicitly leaves authoritative byte/inventory binding to the future producer (`:21-27`); it does not claim a live scanner or publication path.
- Java parser, duplicate-declaration and offset failures remain local diagnostics with ordinary file fallback, retaining real targets (`:495-545`). Offset validation uses checked addition and UTF-8-safe slicing (`:516-529`). The aggregate parsing limit has an explicit constant and a guarded accounting invariant (`:49-51,499-506`); this is a source inspection, not an RSS or latency measurement.
- Ancestor inference explains why observed descendants can prove a directory without proving absence or allocating a filesystem entry ID (`:552-580`). Explicit presence is validated through the existing checked constructor (`:586-590`), and projection errors are propagated with typed provider errors retained (`:615-625`).

## Test integrity and meaning

- All **97** paths in `immutable-inputs-before.json` were rehashed, with **zero mismatches**. This includes the original 21 component tests, 29 facts tests, Java parser source/original fixture, retained reference sources/sample data, Gradle exporter, canonical manifest and parity ledger. Canonical manifest and ledger also remain byte-identical to HEAD.
- The new integration file contains **23 additive tests**, with no `ignore` or `cfg` skip attributes. The tests exercise externally visible adapter contracts: missing presentation/providers (`project_tree_adapter.rs` tests `:272-358`), generation flags (`:361-424`), unsupported priorities (`:427-463`), retained host membership (`:466-497`), root encounter and duplicate physical target behavior (`:500-550`), actual Java names/UTF-8 targets and fallback (`:553-631`), stale revisions (`:634-673`), unknown versus missing presence and contradictions (`:676-718`), malformed inputs (`:720-764`), per-entry attribution and exact resource membership (`:767-846`), special targets (`:848-887`), parser/work budgets (`:944-1013`), and 20,000 distinct navigation targets and keys (`:1015-1040`). No existing test is weakened or replaced.
- Test-only finite-world presence synthesis is explicitly labeled synthetic (`tests/project_tree_adapter.rs:183-205`). The SDK test also labels its Kotlin capability synthetic and asserts that absent language capability cannot be inferred from SDK roots (`:1043-1107`). It adds zero canonical original test credit.

## Fixture provenance, attribution and documentation

- The **77,717-byte** SDK JSON fixture matches SHA `69dc1298842c340e278a48b898f3157d5c301fbdbb2a7aa292ed5481bd696c7f`. The recorded original capture log SHA, extracted model record SHA `dc9055879f22b42c0d12016894499151379c147bd9b4824ef1eb5497afbd42c9`, and exporter SHA `0be7902796daae305d671d70bda7f136386db57362e53187b4c3f4170bb239e3` were verified against actual bytes.
- Independently reconstructing the documented root-string normalization yields JSON identical to the fixture, with exactly **425** changed string values. No provider, root, generation flag or other value was added or changed. `test_data/project_tree_adapter/provenance.json` and `README.md` distinguish supplementary official SDK output from pinned original tree fixtures and identify the unchanged Apache-attributed sibling code/data.
- The new module and test file retain Apache 2.0 headers and AOSP attribution (`:1-18` in each). The task plan documents the supported subset, missing presentation/Kotlin/BuildConfig producers, producer-iterator limits, unresolved upstream generated-root filtering, and later scanning/publication/UI work. All five original tree methods and ten original assertions remain unported/not_run.

No Cargo invocation, GUI activity, Git mutation or source write occurred. The only output is this external review artifact. This verdict does not assert passing tests, full task completion, Android tree UI fidelity, full generated-source coverage or original reference parity.
