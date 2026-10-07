# Tree input adapter: independent CODE review, round 1

Verdict: **FAIL** — three P2 correctness findings, two sharing the root-classification ordering fix.

Read-only review of the pure adapter, its additive module export, supplemental tests, and unchanged model/provider/projection/Java-parser contracts. No Cargo, GUI, checkout edit, Git mutation, or test execution occurred. The externally saved review report is the only write. All three frozen files and all 97 protected existing inputs were SHA-256 checked unchanged before this report.

Frozen adapter SHA-256: `67dbb26617d07caee7db19047aa0cdb82c8572e34de305e97847b7af32549c09`.
Frozen supplemental tests SHA-256: `eeaaaa01809c3dabebbcdca3f84b3ad22b02db6060de54db4ffb858c4314133e`.
Additive export SHA-256: `f4b033f8aa13b8141f706a1f24455c1d5500940ef5e4adffa46edd8baf1f2891`.

## Required findings

### P2 — Apply the disabled-Kotlin move after built-in deduplication

Location: `crates/android_tools/src/project_tree_adapter.rs:249–260`, with exact-root deduplication at `:319–345`.

The adapter changes a Java/Kotlin intersection into Java before comparing physical roots across source types. Pinned `AndroidViewNodeDefaultProvider.kt:169–188` first deduplicates JAVA, KOTLIN, KOTLIN_AND_JAVA and subsequent groups in built-in order. Its Disabled move occurs only at `:198–202`, after deduplication.

Concrete counterexample: main contributes Kotlin-only `/shared`; debug contributes both Java and Kotlin `/shared`; capability is Disabled. Upstream's Java difference is empty, Kotlin wins `/shared`, and the common root is removed before the Disabled move. The correct retained group is Kotlin. This adapter makes the common occurrence Java and lets it beat Kotlin, returning the wrong group. All required inputs are present, so this is a supported-subset defect, not a deferred capability.

Keep common occurrences as KotlinAndJava during cross-provider/type deduplication. Move only surviving common occurrences to Java for Disabled, then normalize the final supported group order. Add a cross-provider Kotlin-only/shared Disabled regression. Preserve the existing same-provider expectations and immutable originals.

### P2 — Validate module-directory presence before returning a ready tree

Location: `crates/android_tools/src/project_tree_adapter.rs:552–625`; the missing check is immediately before building/projecting the immutable module input.

`required_presence_paths()` includes the module directory at `:155–156`, but adapting never requires its state to be Directory. `ProviderPresence::new` validates contradictions; the existing projector only checks a module inventory row if one exists (`project_tree.rs:445–450`). Neither checks a no-row module directory against explicit presence. With a zero-root plan and empty inventory, module presence Unknown, Missing, or File can all produce a ready module node. The same gap applies to external source-root inventories that do not prove the module directory by ancestry. This collapses unknown, absent, and malformed physical module states into successful tree publication.

Require positive Directory evidence for the module directory from an actual inventory row, explicit presence, or a real observed descendant. Keep Unknown, Missing, and File as distinct typed unavailable outcomes. Reject explicit contradictory states even when descendants would otherwise prove Directory. Do not invent filesystem IDs or infer Missing from an absent inventory row.

The newly written presentation-only test at `tests/project_tree_adapter.rs:301` currently uses an empty capture whose synthetic helper explicitly assigns Missing to the module directory (`:185–216`). That setup is a wrong new fixture assumption. Retain its identity assertions, record the correction reason/history, and give the test an actual synthetic module-directory entry. Add independent empty-capture Unknown/Missing/File and positive explicit/descendant Directory regressions. No original test needs changing.

### P2 — Request Kotlin capability only for a surviving common root

Location: `crates/android_tools/src/project_tree_adapter.rs:253–259`, before the root winner is established at `:319–345`.

An Unknown capability aborts preparation as soon as any provider reports a shared root, even if the shared occurrence is deterministically eliminated by an earlier built-in type. Example: the same main Kotlin-only/debug common `/shared` collision, now with Unknown capability. Kotlin wins before the upstream enablement-dependent common move; Enabled and Disabled both retain Kotlin. Capability is not required for this result, yet the adapter returns MissingKotlinCapability. A Java-only occurrence in another provider gives the corresponding deterministic Java case.

After built-in deduplication, return MissingKotlinCapability only if a KotlinAndJava occurrence survives and its presentation would depend on enablement. Add Unknown-with-shadowed-common and Unknown-with-surviving-common regressions. This can use the ordering correction required by finding 1.

## Contracts checked without additional findings

- Selected evaluated active providers are consumed separately from configured DSL candidates and native AGP precedence. Main/retained host/device/fixture/suite behavior delegates to the checked index; unsupported source roots stay explicit. No module, variant, provider, or language identity is inferred from directory names.
- Static roots preserve evaluated provider identity. Generated roots come from selected component entries whose actual generated flag is true; no build-path or filename heuristic replaces provenance. Built-in AIDL/RenderScript/JNI priorities participate in exact-root shadowing without advertising their renderer as implemented.
- Capture module, variant, model revision, and every supplied Java byte revision are checked before projection. Paths are absolute/normalized; duplicate inventory paths and Java bytes on non-Java files reject the capture. Physical file-versus-directory contradictions propagate through presence and projection.
- Observed descendants infer only ancestor Directory facts; the cached ancestor traversal does not infer Missing or fabricate entry identity. Exact supported-root encounters match the prepared root multiset, and nested roots remain separate occurrences.
- Per-entry provider lookup and exact resource-root membership use the existing immutable index/projection. Unknown relevant overlapping roots do not become unattributed or a later provider winner. Ordinary generated roots with a complete no-match remain explicitly unattributed.
- Java parsing uses actual captured bytes, never filenames or Kotlin declarations. Parser errors, absent bytes, duplicate top-level names, and per-file/aggregate budget exhaustion retain an ordinary file with a per-entry diagnostic. Original UTF-8 offsets are checked with fallible range slicing and checked addition. Aggregate subtraction/addition is guarded, and oversized per-file input is rejected by the parser before scanning.
- Special-file paths and navigation targets are preserved without invented filesystem IDs. Resource leaves retain individual physical targets; resource grouping uses the existing exact-root policy.

## Limits and remaining gates

This verdict covers source correctness of the supported pure producer boundary only. Authoritative host capture/presentation production, background scans and completeness, publication/request lifetimes, generated-artifact completeness, Gradle light-class filtering, sorted visible source folders, and actual project-panel UI remain explicit subsequent work. The adapter itself cannot prove that a host-assigned file revision matches the disk; that remains the documented host capture obligation.

No original five-case integration credit is granted. All five original methods and ten assertions remain unported/not_run. The source must be fixed and re-frozen before an affected code re-review. The owner still needs actual unfiltered tests, strict clippy/formatting, all five review passes, and the lead's combined full-app/live/workspace gates.
