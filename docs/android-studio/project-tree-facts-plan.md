# Evaluated provider facts for Android tree projection

This isolated task starts at `959992d03c4cfc928aaad01fda6f83cfc3e34994`,
which combines reviewed tree source/evidence with the frozen source-provider
catalog. Its branch is `android-studio-task/1-project-tree-facts`. Protected
branches remain untouched. The five original Gradle tree methods and their ten
assertions remain unported until real fixture preparation, import, generated
model facts, refresh, class processing and visible tree integration pass.

## Source proof and scope

The pinned `AndroidViewNodes` concatenates current main, host tests, device
tests, test suites and fixtures. `AndroidModelSourceProviderUtils` selects
actual model containers in default, forward product-flavor, multi-flavor,
build-type and variant order. Its flavor-order TODO is retained: native AGP
`VariantSources.getSortedSourceProviders` reverses flavors instead. A DSL
catalog or native component membership cannot be relabeled as Studio order.

The pinned AGP V2 `ModelBuilder` provides a non-parameterized
`BasicAndroidProject` model with actual source-set containers and basic variant
artifact providers. Its containers include providers used by artifacts across
all variants, filter disabled artifact dimensions, and incorporate late static
folders. Those facts cannot be reconstructed from only the selected native
component. The bridge will capture this registered tooling model and native
component membership separately, leaving selection and order logic in Rust.
The Versions model supplies the actual model-producer version. Producers
before 11 require the unimplemented legacy asset filter and remain unavailable.
Missing model capabilities produce explicit unavailable metadata without a
fabricated provider or fallback to source-set-name heuristics.

Nonempty test suites require their complete source-provider definitions and
encounter order. They remain explicitly unavailable if the captured basic
model cannot prove those inputs. The model's fixture/screenshot flavor filter
predicates differ in the pinned source; this task consumes model facts and does
not repair those predicates. The raw evaluated DSL catalog remains unchanged
and continues to include inactive providers. `ModelCacheV2Impl` joins actual
DSL dimension identities to each typed container's `sourceProvider.name`. Its
extra-provider prefix classification is checked only for agreement with actual
artifact keys; disagreement is an unsupported shape, never a guessed role.
`AndroidResFileNode` resolves resource annotations through exact `resDirectories`
membership. This is a distinct typed lookup; generic ancestry remains in use for
ordinary files/directories and `resources.properties`. Unknown resource-root
presence remains unavailable, with missing/file/stale outcomes kept separate.
Root encounters are labeled as the producer's captured iterator order. This
contract does not itself prove AndroidSourceType group assignment or the full
Studio generated-source model order.

## Bounded tasks and acceptance

| Task | Owned files | Dependencies | Acceptance |
| --- | --- | --- | --- |
| Capture evaluated model inputs | `android_tools/src/project_model.gradle`, additive typed fields in `project_model.rs`, attributed source/provenance | Registered AGP V2 basic tooling-model capability; existing owner module task locks | Capture actual default/build-type/flavor containers, selected variant identifiers and artifact-specific providers. Preserve their encounter order and native membership independently. Unsupported versions/shapes have typed unavailable outcomes and diagnostics; ordinary project import remains usable. No invented provider identities or roles. |
| Resolve providers per entry | New `android_tools/src/project_tree_facts.rs` and crate export | Evaluated model inputs; complete model-bound filesystem presence facts | Mirror the pinned Studio order, including the forward flavor TODO and host/device/fixture conditions. First matching active provider wins per file/directory; shared/nested paths retain ordered candidates. Existing manifest parents can match even when their manifest is absent. Missing roots do not match; unknown presence and obsolete revisions remain unavailable. No disk I/O or foreground work. |
| Adapt immutable projection | Additive `project_tree_with_facts` in `project_tree.rs`, typed encounter inputs | Checked provider resolution and explicit supported root encounter provenance | Resolve annotations separately for every directory/file. Preserve root occurrence identities and reference-sensitive encounter order without path-derived provider guesses. Reject unknown root order instead of calling it reference-equivalent. Preserve the existing entry point and every assertion in all 21 legacy component tests. |
| Verify evaluated behavior | New supplemental integration tests and external Gradle smoke inputs/evidence | Cargo/Gradle lease; frozen source and exact fixture inputs | Exercise overlapping roots, changing winners, shared/inactive providers, forward/native flavor differences, artifact conditions, missing/unknown presence, malformed/stale facts and encounter order. Use an actual Gradle model capture to verify the bridge and Rust decoding; label this smoke as distinct from the original five methods. Full crate tests, repository clippy and formatting pass on immutable source. |

The independent Java class producer belongs to another owner. Its proposed
boundary supplies actual top-level names and UTF-8 identifier offsets, plus
typed malformed/unsupported outcomes, converting into the unchanged
`project_tree::JavaClassFact`. A later caller binds those results to the exact
source revision; filenames cannot substitute for parsed declarations.

Each of the five review areas runs independently and sequentially for this
owner. Findings receive fixes and affected reruns, with escalation after three
fix rounds. Full-app/live and full-workspace validation remain lead merge
gates. This task installs no GPUI selector, renderer or navigation workflow.


## Escalated review correction

Owner review history is preserved: three fix rounds completed the initial CODE
review, then UI found that generic ancestry was incorrectly reused for resource
annotations. The owner escalated this after the limit. The lead reported the
escalation and authorized one bounded correction: implement exact resource-root
membership from the pinned `AndroidResFileNode`, retain generic lookup elsewhere,
and add overlap/capability regressions. All five review areas must recheck the
frozen correction source. Further findings require another specific escalation.
Runtime checks, app/live checks and original integration parity remain pending.


## Escalated scaling correction

Performance review rejected the frozen first correction because per-entry lookup
scanned all configured roots, annotation collection reinserted complete ancestor
chains, and encounter remapping searched the root list quadratically. The owner
reported this further finding before editing source; the lead authorized bounded
correction 2. The owner three-round history and first correction remain intact.

The design received a performance PASS before implementation. The immutable index now stores directory roots by normalized path, exact
manifest file/parent matches separately, and exact resource roots separately.
Rules retain provider encounter, category rank and original root encounter.
Lookup examines candidate ancestors/exact keys and preserves first-provider and
first-category/root semantics, including earlier unknown matches. Annotation
ancestor coverage is cached per root boundary so overlapping roots cannot hide
coverage gaps. Typed group/path root keys preserve the current first duplicate
when remapping explicit encounters. Additional many-root, overlap/order and deep
shared-ancestor regressions assert results and navigation without timing limits.
Supplemental regressions cover 2,048 disjoint roots, duplicate/shared resource
roots, 4,096 sibling files under 96 shared ancestor levels and 2,048 explicit
root encounters. They also preserve successful earlier-rule short circuits and
earlier unknown-root failures. All five areas recheck frozen source. Runtime
gates remain pending.

The independent Java producer intentionally preserves actual duplicate
top-level names in invalid unsaved source. The future adapter must detect
duplicate declarations and malformed/unsupported results before publication,
retaining ordinary file presentation for that entry rather than inventing or
merging classes or failing the whole snapshot.

At correction 2, all five reviewers passed source/backend contracts before
execution. Later runtime checks exposed an explicit module lifetime omission,
a redundant test clone, an unsupported supplemental source-set declaration,
and an incorrect newly introduced Basic-catalog expectation. Those failures
and lead-directed corrections 3–6 remain recorded; owner history is not reset.

## Executed validation and remaining gates

The final reviewed snapshot passed all 144 all-feature crate tests (55 unit,
20 import, 21 unchanged tree component, 29 provider facts and 19 preferences),
strict repository clippy and workspace formatting. The actual public-toolchain
Gradle capture and Rust probe pass. They preserve Studio forward-flavor order
and native reverse-flavor order as separate facts, actual shared-file first
provider semantics, and all-variant host-container behavior.

The new supplemental expectation that a fully disabled flavor remains in the
Basic catalog was wrong: the pinned builder filters by created variants. It is
explicitly flagged with source lines/hashes and its actual failed probe, then
corrected to independent Basic/variant absence while retaining DSL identity,
assets and the original inactive-provider assertion. No canonical test is
reclassified or credited.

The actual sample also exposes an existing component-source classification
conflict: API-added static assets are flagged generated while authoritative V2
provider data includes them as static. A separate source-model correction is
required before AndroidSourceType group/UI wiring. This provider component
does not infer or claim that grouping.

The shared lease is returned to the lead. Source-commit captures (if required),
full app/live checks, full workspace/combined CI and product UI wiring remain
lead integration gates. All five original integration cases remain unported.
