# Android resource semantics

This feature consumes the selected project and component graph from the shared
Android project model (PR #50). The additive `resourceModels` metadata carries
AGP resource layers, transformed AAR resources/public lists, the selected SDK
resources and the merged-manifest output path. There is no second variant picker
or independently inferred Gradle graph. Older exports retain navigation, but
resource rename requires overlay metadata from a successful sync.

## Supported matrix

| Operation | Supported behavior | Boundary |
| --- | --- | --- |
| Resolution | Selected application/library/feature and component dependency roots; AGP outer layers in highest-priority order, equal-priority inner directories; custom resource directories; build types/flavors; one winner per qualifier, shadowed declarations and equal-priority conflicts | Qualifiers are separate candidates, not a simulated device configuration. Dependency namespace ambiguity is exposed as multiple candidates; library merge precedence is not inferred. |
| References | Kotlin/Java `R.type.name`, explicit namespace R, ordinary R imports and Kotlin R aliases; XML whole attribute/text `@type/name`, `@namespace:type/name`, `?attr/name`, and `@+id/name`; comments and ordinary strings are excluded | This is a conservative resource lexer, not JVM symbol binding. Static/member imports, interpolated strings, escaped syntax and data-binding expressions need language-server support. Uses across all catalogued variants are namespace-based candidates. |
| Definitions and hover | XML value resources, resource-file names (including `.9.png`, WebP and other non-XML files), layout-created IDs, selected project dependencies, transformed AARs and SDK resources; provenance/conflict hover | Text definition links cover XML. Ordinary Go to Definition opens PNG/JPEG/WebP/GIF through the workspace image viewer under both Picker and Multibuffer settings; cmd-click, split and hover image links are not implemented. Resource-file hover indexes other binary formats without opening them as text. Composite `R.styleable.Widget_attr` members and complete attr/style inheritance are not modeled. |
| Completions | Matching kind/prefix in XML and Kotlin/Java R expressions, including ordinary R imports/aliases; namespace/dependency candidates with provenance; public metadata filters imported/unqualified and own-R dependency members and XML dependency suggestions; explicitly qualified foreign R also offers private members, matching Studio | Incomplete results merge with existing server completions. Uncommon framework public-group forms remain incomplete. |
| Rename | Preview before applying; simple lowercase XML value names and layout/values IDs across catalogued source sets, qualifiers, Kotlin, Java, XML and `<public>` metadata; unsaved buffers; unsaved multi-buffer transactions using the editor's project-transaction/undo flow | Rejects file-resource moves, style/attr/styleable/macro or non-R metadata kinds and normalized names, external/framework/generated declarations or references, collisions, equal-priority conflicts, shared namespaces, physical resource/code roots with ambiguous ownership, shared main/test declarations or implicit references with different namespaces, transitive R mode, ambiguous JVM bindings/imports, inheritance, anonymous subclasses, implicit/extension/context receivers and unrecognized callback scopes, templates/escapes, data binding and unqualified cross-namespace XML references. Refusal is intentional when complete ownership cannot be proved. |
| Compose integration | Selected source-root observers retain main ownership when main/test roots share a path; deeper generated/test roots remain excluded. Dirty selected main XML resources pause previews with a save-required message; saving queues automatic refresh while unsaved Kotlin overlays remain supported | Resource rename does not autosave. Gradle reads saved XML resources; this feature does not provide unsaved XML overlays to the resource compiler. |
| Manifest provenance | From an existing merged-manifest attribute, navigate to matching source declarations in selected manifest roots/project dependencies/AAR manifests; hover distinguishes equal and different source values | Does not run the merger or establish the merger winner. Placeholder expansion, tools directives, class-name normalization and merger-blame reports are not interpreted. The merged output must already exist. |
| Lifecycle | Model generation and trust checks, including a final trust/locality check before applying a reviewed plan; source/document snapshots; file membership, deletion and disk metadata; public-list metadata; failed-sync invalidation/recovery and suppression of stale server resource results while the model is unavailable; edited-path canonical identity, symlink and writability checks; bounded scans/previews | Local trusted projects only. Scans are on demand, bounded to 20,000 files / 128 MiB text; SDK roots are scanned only for explicit `android` references. Rename review is bounded to 200 files / 2,000 edits. Unsaved external `public.txt` changes are not a supported visibility input. |

Repeated `@+id` declarations are legal and are not treated as duplicate-resource
conflicts. Dependency, SDK and generated resources are read-only for this rename
operation; that restriction does not globally lock ordinary editor buffers.
External XML resources share hidden directory worktrees instead of creating one
worktree per file. Newly created resource worktrees ignore initial and later
project settings, tasks, debug configurations and EditorConfig; existing
containing project worktrees retain their usual configuration and trust policy.
SDK/AAR lookups scan and parse files on a kept background task without opening
editor buffers for unopened files. Navigation opens only its target buffers;
existing unsaved buffers remain authoritative. Each file's text is discarded
after parsing. Traversal is limited to 20,000 files, 80,000 visited/pending paths
and 128 MiB text; declarations and public metadata each have a 262,144-record
limit, and ownership entries have a 1,048,576-record limit. Overflow fails rather
than returning a truncated index. One progress slot limits work ahead to two
32-file chunks, with model/trust checks between deliveries; dropping a request
cancels its kept tasks. This remains an on-demand metadata index, without a
persistent SDK cache or a measured latency/resident-memory guarantee. Reference
and rename plans retain their editable buffer snapshots and disk text so preview,
stale-plan checks and undo preserve their existing behavior.

The resource model uses compile-classpath AAR transforms and the compile SDK's
`data/res`. It does not parse binary-only APK tables or a dependency R JAR as a
replacement for source resources. Generated/test R references resolve against
component namespaces and source scopes; generated files are never rewritten by
resource rename.

## Generic refactoring

Existing Kotlin/Java language-server rename and server-provided code actions
remain available for ordinary language symbols. No verified server contract in
this repository implements general move/package, change-signature or safe-delete
operations with Android resource ownership. This change adds none of those
operations and never substitutes project-wide text replacement for them. It is
not Android Studio refactoring parity or a visual layout editor.

## Verification

- `cargo test --locked -p android_tools`: parser, masking, aliases, metadata,
  legal repeated IDs, overlay conflicts and conservative rename guards.
- Project integration tests filtered by `test_android_resource`: selected model,
  custom/flavor/qualifier/dependency/framework/non-XML roots, completion and
  manifest candidates, preview/unsaved edits/undo, collisions/generated files,
  stale source/model/disk state, file addition/deletion, failed sync/recovery,
  metadata edits and safe refusals; shared external directory worktrees and
  suppression of initial and later dependency configuration with a working
  visible-project configuration control.
- Editor tests filtered by `resource_rename::tests`: read-only preview,
  apply/cancel/Escape/dismissal, keyboard focus and viewport bounds.
- `lsp_locations` tests: default action dispatch opens a real PNG image under
  Picker and Multibuffer settings, while XML qualifier candidates retain the
  definitions picker; ordinary LSP navigation and references fallback still work.
- Android UI tests cover shared main/test source-root ownership and save-required
  preview behavior for dirty custom resource XML, with generated/test exclusions.
- `script/test-android-resources --gradle <path> --sdk <path> --agp 8.9.1|9.4.0`:
  parallel AGP collectors, custom/equal/variant/generated resource layers,
  transformed AndroidX AAR/public metadata, SDK paths, test resources and
  shared-model validation.

AGP probes need the matching Gradle version, a full JDK 21, API 35 and matching
build tools. They export resource metadata and do not run an emulator or assert
runtime device resource selection.

The current-target integration also retains Android KMP dependency model imports.
KMP resource layers and compile-SDK framework paths use the Android library
Kotlin target; they do not require an ordinary `android` extension. Merged-manifest
resource navigation is unavailable for KMP modules and is reported in model
diagnostics. The retained multiplatform exporter probe exercises this boundary.

SDK completion labels and provenance are also prepared on the background task,
using a symbol-to-declaration index rather than rescanning every declaration per
item. One progress slot bounds completion work ahead to two 128-item batches;
combined completion text is limited to 128 MiB. Model, trust and source-version
changes discard the result before delivery.

Cursor context for completion, hover, references and rename is captured only
after local/trusted Android model, file-type and selected-source ownership gates.
The snapshot is materialized and parsed on a retained background task. Python,
HTML, unowned source files and ordinary projects take the existing LSP path.
Hover and references also return to LSP when asynchronous parsing finds no
resource at the cursor. Unavailable-model cursor classification and both rename
routing stages also run on retained background tasks, with the same byte,
record, logical-work, trust, source-version and cancellation limits. A normal
Kotlin/Java symbol returns an explicit nonresource result and continues the
existing language-server rename, including editor confirmation. Unavailable,
unsafe or stale resource rename requests remain errors. Cursor results also
distinguish a stale request from a proven nonresource:
stale hover, reference and definition requests are withheld rather than sent to
LSP, and completions check the captured model/source/trust binding before
delivering either Android or concurrent server candidates. A fake-LSP regression
exercises these actual Project entry points across model invalidation.
The compatibility navigation path checks Gradle/module/type metadata before
parsing, and moves its
cursor, namespace, project-file and XML work off the foreground thread too.

All 339 tests and 1,447 ordered assertions in the prior corrected proposal and
the seven additional tests in the first routing correction are retained
unchanged. New regressions call actual Project completion, hover,
references, prepare-rename and perform-rename entry points, and the editor's
Rename → Confirm workflow for ordinary Kotlin and Java symbols. These regressions
are authored but not yet executed; compiler, full CI and all five reviews remain
acceptance gates. The prior performance and ordinary-rename failures remain
archived evidence rather than receiving passing credit from this proposal.

The forward integration uses PR42's `2c914` project-context foundation
(tree `da8f5c180ea10b5ce2c2e716d325b0005069eb27`). Its context controller,
evaluated module ownership, chooser, Android gating and every unrelated tree
entry are retained. The single preview conflict keeps that foundation's
all-variant, deepest-root implementation: shared main roots stay active at equal
depth, while deeper generated, test or inactive-variant roots exclude preview
refresh. Resource features do not introduce another UI context controller.

Two shared test fixtures require explicit integration adaptations. All 347
resource-proposal test bodies remain unchanged. The PR42 versions below are
behaviorally subsumed rather than claimed byte-identical; exact fixture diffs
are supplied with the integration packet.

| PR42 test | Integration status | Scenarios and expected predicates retained |
| --- | --- | --- |
| `selected_source_scopes_control_automatic_preview_refresh` | Adapted fixture, no passing credit yet | Existing `Main.kt`, `code/Content.kt` and `app/build.gradle.kts` inputs remain true; existing custom-generated and test inputs remain false. The resource fixture adds shared main/test roots and a deeper generated root, with additional positive and negative inputs. Every PR42 input is still visited with its original predicate. |
| `test_android_resource_definitions_without_language_server` | Adapted fixture, no passing credit yet | String and manifest lookups retain both localized declarations, foreign/framework lookups remain empty, and every `R.array`/`@array` lookup still executes the original one-location, same-buffer and exact-tag assertions. The resource fixture's additional `R.drawable` guard is never entered by this array-only loop and bypasses none of those predicates. |

Qualified compatibility navigation now also retains the Gradle namespace
buffer's snapshot, path and worktree ownership. Cooperative progress and final
delivery revalidate that input alongside the source. An unsaved namespace edit
cancels the lookup with a diagnostic and does not release it to LSP. A regression
edits the Gradle buffer when the real Project lookup opens its resource target,
after the namespace snapshot was captured. This test is authored but unexecuted.

Each opened compatibility resource input also retains a change observer until
the request finishes. A shared invalidation flag stops background parser work
at a cooperative checkpoint and prevents delivery after any earlier validated
input changes during later batches. Observer count is bounded by the existing
20,000-file limit; paths share the worktree's relative-path allocation. The
Gradle namespace snapshot remains checked directly at progress and delivery.
The 150-file regression edits the first resource after both validation batches
have passed, through a test-only GPUI notification barrier. It verifies the
aggregate guard catches the edit without repeating an unbroken foreground sweep.

Pure reference parsing and rename planning run on retained background tasks.
One coalesced progress slot allows foreground model/trust/source checks; closing
the receiver cancels the parser at its next cooperative checkpoint. Source-mask
copying and masking check every 4 KiB, and reference/alias/declaration loops check
each entry. Regex searches and bounded overlay sorting are checked around the
individual library operation, not interruptible inside it. An independent 1 GiB
logical work allowance bounds repeated whole-document passes and root/namespace
analysis. Binding checks are memoized once per identifier per document, including
the shared rename analysis; many distinct aliases fail the work allowance
instead of causing unbounded repeated scanning. The existing final transaction,
snapshot, membership, metadata, trust and ownership checks remain required.

Documentation uses a checked writer with bounded geometric capacity growth,
checking the requested length before reservation/append. The same writer caps
combined hover at 128 MiB and contributes to the aggregate 128 MiB completion
budget. Overflow returns an error, never a successful truncated result. File,
source, record, metadata, work and output allowances are logical limits; they
are not measured RSS, startup or hardware responsiveness claims. Regression
tests for generic bypass, stale large-source requests, receiver closure, repeated
bindings and tiny documentation budgets are authored and await CI execution.

## Authored visibility regression correction

The newly authored
`test_android_resource_exporter_public_metadata_restricts_completion_and_definition`
had two incorrect expectations. This is a correction of our regression, not a
declaration that an original reference test is wrong or inapplicable. The
historical 329-test packet remains unchanged at
`/tmp/koda-PR53-resource-review-fixes-private-20261011-attempt1/FINAL-PR53-OWNER-19-PATH-THREE-REVIEW-FIXES-READY-CLOSED0-ACTUAL.json`
(4012 bytes; SHA-256
`e48e4f652d5e5d655ed3391be34179b64fc2d802b71001c1693e238b257bf203`).
All 325 tests preceding that packet remain byte-for-byte conserved. The other
valid expectations in this authored regression remain, with additional imported
private navigation and XML public-completion/private-navigation cases.

| Authored regression operand | Archived expectation | Corrected expectation | Status/reason |
| --- | --- | --- | --- |
| `names` for `external.library.R.string.external_` | `vec!["external_public"]` | `vec!["external_private", "external_public"]` | Authored expectation wrong: explicit foreign Java/Kotlin R qualification intentionally disables private-completion filtering. |
| `definitions` for `external.library.R.string.external_private` | `is_none_or` / empty | One existing declaration containing `>Private<` | Authored expectation wrong: available private resource fields/declarations remain navigable; accessibility diagnostics are separate. |
| Imported `R.string.external_` and XML dependency completion | Public-only suggestions | Public-only suggestions | Retained and expanded; private completion is filtered for these syntactic forms. |

This decision uses the Android Studio AOSP Android plugin at
`a84efec3ba9542d9bfa1255103f0dc94833a3796`, copyright its original authors,
Apache-2.0. No source/test fixture is copied by this correction. Source evidence:

- [ResourceCompletionContributor.kt](https://android.googlesource.com/platform/tools/adt/idea/+/a84efec3ba9542d9bfa1255103f0dc94833a3796/android/src/com/android/tools/idea/res/completion/ResourceCompletionContributor.kt), lines 76–90, 96–110 and 116–121, SHA-256 `e4276c530082f7e28227ed61e7c60bc8df5465be2f172de47908cf0429622d56`: unqualified/own-package filtering; explicit foreign exemption.
- [InnerRClassBase.kt](https://android.googlesource.com/platform/tools/adt/idea/+/a84efec3ba9542d9bfa1255103f0dc94833a3796/android/src/org/jetbrains/android/augment/InnerRClassBase.kt), lines 73–95 and 171–184, SHA-256 `8857974fda0def78d6c13aeab6cc54081c3f4799a502e05b0438f0d9ac6f2b05`: private visibility metadata retains fields.
- [ResourceDefinitionSearch.kt](https://android.googlesource.com/platform/tools/adt/idea/+/a84efec3ba9542d9bfa1255103f0dc94833a3796/android/src/com/android/tools/idea/res/psi/ResourceDefinitionSearch.kt), lines 27–45, SHA-256 `57591ed5d9f0c970ca40d589c0713ca58c8fd8900c6cb1eba2e868afeda8bec0`: navigation resolves declarations without a public-visibility gate.
- [ResourceRepositoryToPsiResolver.kt](https://android.googlesource.com/platform/tools/adt/idea/+/a84efec3ba9542d9bfa1255103f0dc94833a3796/android/src/com/android/tools/idea/res/psi/ResourceRepositoryToPsiResolver.kt), lines 105–118, 182–187 and 246–264, SHA-256 `7f2e440d0a50c3400832e1fb6081ad7c68e37697aaf5e0ee9dcd9af16196c4a4`: resource existence/ID preference is distinct from visibility.
- [ResourceReferenceConverter.java](https://android.googlesource.com/platform/tools/adt/idea/+/a84efec3ba9542d9bfa1255103f0dc94833a3796/android/src/org/jetbrains/android/dom/converters/ResourceReferenceConverter.java), lines 430–438 and 478–499, SHA-256 `c73e1d6918ca44d437484fc595ebfe1fe64877b08b1ea7a42cbeff284ef19526`: XML dependency completion requests PUBLIC resources.

The adjudication receipt is
`/tmp/koda-PR53-private-visibility-pinned-reference-assessment-20261011-attempt1/FINAL-INDEPENDENT-PR53-PINNED-VISIBILITY-REFERENCE-ASSESSMENT-CLOSED0-ACTUAL.json`
(32364 bytes; SHA-256
`50cc483e04a0104599d7c58d715da5dedbbc142da78e0172f8b041fbffe36923`).
Exact canonical reference test names and their fixture census have not been
established for these branches. These authored checks are pending Rust CI and
grant no reference-test port, runtime or completed-parity credit.

## Resource355 review corrections

The resource355 proposal remains archived unchanged at
`/tmp/koda-PR53-resource347-forward-integrated-with-PR42-2c914-private-20261011-attempt1/FINAL-PR53-CURRENT42-19-PATH-FORWARD-INTEGRATION-355-TESTS-FORMATTED-UNAPPLIED-REVIEW-READY-ACTUAL.json`
(9551 bytes; SHA-256
`75197129c4dcb4e4dd42540cebf7466b82cfdc87392a76d7168aa4f1daffb5a2`).
Its 355 Rust tests and 1545 ordered assertion macros are preserved in the new
proposal; the following regressions add coverage rather than replacing them.

Unsaved preview resources use evaluated module ownership, including sibling
worktrees outside the Gradle root. Refresh eligibility and dirty-resource
checks share the all-variant deepest-source classifier: a deeper inactive,
generated or test root excludes an enclosing selected main root. The added
GPUI workflow dirties real sibling XML buffers, checks that refresh refuses
disk rendering with a save diagnostic, saves the main resource through Project,
and checks that a refresh is queued while excluded buffers remain dirty. It
does not start a renderer or claim rendered-pixel validation.

Namespace payload has an independent 128 MiB owned-byte allowance. Imported and
aliased R references, scanner declarations, conflict/index keys and public-input
tuples check aggregate duplicated payload before cloning. Overflow remains a
typed resource-limit error, including non-strict scans; it cannot become a
successful partial index. Component source roots and captured documents share
namespace allocations through `Arc<str>`; source-root domain handles also share
one allocation per component. Temporary namespace lookup keys are charged before
creation. Small-budget tests exercise implicit/aliased imports, real resource
scanning, lookup construction and shared-root identity without allocating large
buffers. This allowance bounds charged payload; file/record/work limits still
apply separately. It is not a bound on total allocator overhead or measured RSS.

The newly authored Python resource probe is ported to
`crates/android_tools/examples/resource_model_probe.rs`.
`script/test-android-resources` is now a thin shell launcher for this Rust
example. Shell is retained solely to find the repository and invoke Cargo;
fixture creation, CLI validation, subprocess orchestration, model parsing and
assertions are Rust. Gradle/JDK, the Android SDK and Cargo remain the required
external build tools. Existing non-resource project-model probes are unchanged.

The archived Python resource probe has SHA-256
`e8588eaef12ee4a2450e7bfbfa1bbc031fe867aee26cb5405a633feae4590b91`;
its shared fixture source has SHA-256
`f7e333977baf9a80b3da118df14836944734fb0315274bd385b46332da4f380f`.
The AGP 8.9.1 and 9.4.0 JSON assets preserve each version's 27 original fixture
files byte for byte. SDK-specific `local.properties` is still generated at run
time. Kotlin plugin differences, custom/equal overlays, qualifiers, generated
roots, Android tests, public XML and AAR dependencies remain present. CLI flags,
parallel Gradle arguments, selected-project probe build/invocation and the
success message retain their original behavior; every child-process failure
propagates. Log parsing requires exactly one model record and checks the line
budget before buffer growth.

| Original resource-probe predicate | Rust check | Status |
| --- | --- | --- |
| Exactly one model record | `model_from_log` | Adapted authored probe; duplicate/missing/malformed/oversized cases added. |
| Variant/debug/flavor/main overlay order | P01 | Adapted authored predicate. |
| Equal custom-root priority | P02 | Adapted authored predicate. |
| Every layer belongs to main resource sources | P03 | Adapted authored predicate. |
| Variant resource source is not generated | P04 | Adapted authored predicate. |
| Generated resource source exists | P05 | Adapted authored predicate. |
| Non-transitive R metadata | P06 | Adapted authored predicate. |
| Framework resource directory exists | P07 | Adapted authored predicate. |
| Merged manifest path | P08 | Adapted authored predicate. |
| AAR resources and manifest exist | P09 | Adapted authored predicate. |
| AAR public metadata exists | P10 | Adapted authored predicate. |
| Android test resource root | P11 | Adapted authored predicate. |

The Rust example includes both fixture checks, all-predicate acceptance,
individual rejection of every original predicate, bounded log parsing and CLI/
Gradle flag regressions. These are authored tests awaiting compiler and CI
execution, not ports of reference Android Studio tests. No runtime, performance
measurement or reference-parity completion is inferred from static preparation.
