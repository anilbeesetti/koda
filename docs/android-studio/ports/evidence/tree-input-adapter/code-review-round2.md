# Tree input adapter: independent CODE review, round 2

Verdict: **PASS — scoped static correctness review**. All three required round-1 findings are resolved in the frozen consolidated correction. No new blocking correctness finding was identified in the affected flow. This is not an executed-test, full-app, native-UI, workspace-build, or canonical-parity pass.

Adapter SHA-256: `d57ba8070321039cdd8b9749bc43df923e694a31c27299a1ec577b5974edfc3b`.
Supplemental tests SHA-256: `1fa2f0e7eedf5bf6cf2cbdbdcd5a1df8604325956cf6bfce78a9626872f6f69d`.
Additive export SHA-256: `f4b033f8aa13b8141f706a1f24455c1d5500940ef5e4adffa46edd8baf1f2891`.

The three frozen files and all 97 protected pre-existing inputs were independently SHA-256 checked unchanged at review completion. The round-1 FAIL report remains byte-identical (SHA-256 `a1d967c2c1a85a40c5399290b540400bd7a3bcc588752ae7cc6e6bbd00abb9f7`). No Cargo, GUI, source/Git mutation, or test execution occurred; this external report is the only write.

## Resolved findings

1. **Disabled Kotlin move follows original built-in root precedence.** `project_tree_adapter.rs:248–253` keeps per-provider intersections as KotlinAndJava. `:309–329` deduplicates exact physical roots in the original built-in group order, including unsupported AIDL/RenderScript/JNI shadowing. Only a surviving common root is migrated at `:330–346`. Stable final grouping at `:355–357` preserves this producer's encounters within each resulting group without claiming upstream HashMultimap or visible-folder ordering. The prior Kotlin-only/common collision now retains Kotlin for Enabled, Disabled, and Unknown. The additive regression at `tests/project_tree_adapter.rs:1110–1132` checks those three outcomes; the existing same-provider grouping expectations remain intact.

2. **Unknown Kotlin capability is required only for a surviving common occurrence.** The Unknown branch is reached after both unsupported-priority and seen-root checks (`project_tree_adapter.rs:323–342`). A preceding Java-only or Kotlin-only occurrence therefore shadows a common occurrence without requiring enablement. The additive Java-shadow regression at `tests/project_tree_adapter.rs:1134–1152` and the preceding three-capability Kotlin-shadow regression cover both earlier supported groups. The existing surviving-common Unknown test at `:254–268` continues to require MissingKotlinCapability.

3. **Effective module-directory presence must be Directory.** Explicit presence is validated first (`project_tree_adapter.rs:463–467`). All captured paths are normalized and unique before Java parsing. An actual module-directory inventory entry contributes its real kind, while only a strict observed descendant under that component-normalized module path may prove Directory (`:468–490`). External children do not contribute module proof. `:491–512` preserves UnknownPresence, MissingEntry, and Malformed as distinct typed errors carrying the actual module path. Positive explicit Directory may establish a known empty module without an invented filesystem entry ID. Conflicting explicit Missing/File versus a real module entry/descendant is rejected by `ProviderPresence::add_entry`; later projection still validates the complete inventory and ancestry. Tests `:1154–1219` cover the three negative module states, explicitly known empty Directory, and external child exclusion/positive explicit module proof. Existing observed-child and contradictory-presence cases remain in place.

## Preservation and affected-flow checks

I independently reconstructed the exact original round-1 supplemental test bytes by removing the four appended regressions and undoing only the two documented capture-setup changes. The resulting SHA-256 matches `eeaaaa01809c3dabebbcdca3f84b3ad22b02db6060de54db4ffb858c4314133e`. Thus every prior assertion, helper, test body, and unrelated input is preserved except those two explicit fixture-setup corrections. Their reasons are recorded in `fix-round1.json`: the identity test needs a real synthetic module directory; the unknown-assets test needs that same real module so it isolates assets presence. The prior invalid finite-world Missing assumption is not retained as a success expectation.

The new early validation does not bypass model/module/variant staleness at `:437–447`, supplied Java byte revision checks at `:520–527`, extension validation, duplicate declaration fallback, original-byte offset checks, aggregate parsing limits, provider lookup, resource-root resolution, or root-encounter coverage. Directory/File/Missing contradictions in captured entries and their ancestors still reach the existing strict presence/projection checks before ready publication. Inferred root/module directory rows continue to derive only from actual observed descendants (`:606–634`), and retained file/class navigation remains tied to the same captured file revision.

## Remaining task gates and scope

The 27 supplemental adapter tests have been read, not executed by this reviewer. The owner still owes actual unfiltered android_tools tests, strict repository clippy and formatting, and all five independent current-source review passes. Full-app/live/workspace checks remain lead gates. No runtime result is inferred from this static pass.

This remains the supported pure per-module producer boundary. Host revision-to-disk binding, authoritative Kotlin/module presentation production, complete targeted scans, stale request publication, visible project-panel rows, generated-artifact completeness, light-class filtering, and provider-sorted visible source folders remain explicit future prerequisites. All five original tree methods and ten assertions remain unported/not_run; this pass grants zero canonical reference-test credit.
