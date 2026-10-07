# Selected JUnit4 source declaration evidence

This independent checkpoint adds a Rust xtask evidence command for an explicit,
hash-bound selection of original Java and Kotlin sources. It does not establish
runtime test instances, complete reference discovery, applicability or behavior
ports. The canonical 92-case ledger, prior passing mappings and census flags
remain unchanged. Unsupported candidates remain unported and unresolved.

The branch is `android-studio-task/1-junit4-declarations`, based on
`7b2bd3e7ffc07156c7193baf156c5041bef256c1`. Initial source preparation uses a sparse
worktree with `tooling/xtask`, `docs/android-studio`, the app-validation skill,
`script`, `.cargo` and cone-mode root files. Normal locked checks require the
complete workspace dependency closure and the lead's exclusive Cargo lease;
this task does not change workspace manifests to accommodate sparse checkout.
The normal full checkout was subsequently restored under the lead's Cargo
lease. Workspace manifests and the lockfile remained byte-identical to the
base; source-root freshness touches preserved their bytes.

## Acceptance and boundaries

- Verify each selected full file against source id, pinned revision, original
  path, SHA-256, byte count and Apache attribution. Keep AOSP/JetBrains identities
  separate, including identical-byte files.
- Recognize supported direct Java/Kotlin annotation declarations only for a
  unique explicit `org.junit.Test` import or exact qualified annotation spelling.
  Aliases, wildcard-only bindings, conflicts, shadowing, use-site targets,
  nested/local ownership and declared supertype scopes stay unresolved.
  Imported aliases/static types and enum types participate in shadowing;
  qualified prefix names with wildcard imports stay unresolved.
- Preserve byte spans, one-based lines, actual owner/name and a SHA-256 of the
  source header through closing parameters. This header hash excludes return
  clauses, throws clauses and the method body; file identity and byte location
  distinguish declarations, including overloads and backtick names.
- Retain annotation facts, ignored/abstract source declarations, custom-runner
  context and JUnit3-shaped candidates. Confirmation describes supported source
  syntax and annotation reference only; classpath identities and compiler/runner
  eligibility remain unresolved. Effective case counts stay `null` everywhere.
- Preserve unknown JUnit5/TestNG/meta-annotation/generator/provider/build-target
  gaps. Never infer N/A, a complete zero or canonical mirror coverage from this
  selected output. Original discovery candidates and output are unchanged.
- Enforce bounded inputs/tokens/classes/annotations/metadata and safe paths.
  Publish only outside the checkout without replacement; `--check` compares
  deterministic evidence and never updates it.
- Preserve every existing discovery assertion and fixture. Independent original
  fixtures and regression tests live under
  `tooling/xtask/test_data/reference_declarations` with complete provenance.

The command is intentionally selection-limited:

```sh
cargo xtask android-junit4-declarations \
  --repository /absolute/path/to/checkout \
  --source-root /absolute/path/to/reference_declarations/sources \
  --selection /absolute/path/to/reference_declarations/selection.json \
  --output /absolute/path/outside/checkout/declarations.json
```

Run the same arguments with `--check` to compare existing evidence. The explicit
selection contains 16 complete files (100,711 source bytes), including direct
Java/Kotlin tests, ignored and abstract tests, parameterized/custom runners,
JUnit3-shaped Gradle tests, misleading test/annotation names and distinct
AOSP/JetBrains originals. Discovery regression tests grant no behavioral parity
credit.

## Dependencies and validation status

The source manifest, frozen discovery, verified retained gzip inventories and
pinned archives are inputs. The original fixture capture verified all three
archive hashes and each selected member hash/length before copying. Inventory
and initial planning evidence are external under
`/workspace/android-studio-artifacts/reference-census-next-task/`.

The command checks supplied selection hashes against source bytes and pinned
manifest identities. It does not reread archives to authenticate arbitrary
supplied member hashes. Output explicitly records
`selected_archive_membership_reverified: false`; archive origin relies on the
reviewed capture proof and committed fixture provenance.

Code review round 1 found enum/imported-name shadowing and escaped Java
text-block false declarations. Owner fix round 1 adds dedicated regressions.
It also flagged a wrong new regression expectation: different fixture bytes do
not require different method lines. The pinned second method lines are 33/38
for DefaultVariantsTest and 41/41 for TabbedToolbarTest. Exact expected line
pairs replace that mistaken blanket inequality while retaining every source
hash, unequal-byte, method-name and declaration-count assertion. Original
fixtures and prior discovery assertions remain unchanged. Runtime checks had
not run at that review checkpoint.

Code review round 2 verified the four initial fixes and found malformed alias
syntax that could re-enter normal binding. Owner fix round 2 requires a Kotlin
alias identifier and blocks Java aliases; dedicated negatives retain the import
and method evidence with no annotation confirmation. Tests had not run at that
review checkpoint.

Code review round 3 verified malformed aliases and found a missing qualified
prefix shadow from a class type parameter named `org`. Owner fix round 3 retains
that header scope and blocks confirmation, with Java/Kotlin negatives. Generic
Java method/type-use annotation prefixes also stay unresolved. Subsequent
failures after this third fix round require escalation before further changes.

The first complete xtask run executed 108 tests: 107 passed and one new Rust
regression failed. Its expected superclass, `GradleImportingTestCase`, was wrong:
both pinned original GradleModuleImportTest files declare `HeavyPlatformTestCase`
on line 59, with SHA-256
`e63aa8bcd300922c731ba1c27be470dcafd808021c8906082d62f8f0e7e6c762`.
The test is explicitly flagged as wrong in retained review history. The lead
authorized correcting only the new expected literal; the assertion, original
fixtures, nine-method checks and null-runtime checks remain intact. Strict
Clippy separately rejected one redundant clone in the new fixture helper. The
lead authorized removing only that clone. These are lead corrections 1 and 2
after three owner fix rounds, with both failed commands retained.

Corrected module SHA-256
`739d29d71996845ac671ea870d266383cf3cba73b4c9b910537d515267ac3aec`
passed all 108 xtask tests (34 new and 74 prior), with zero ignored or filtered
tests, and required repository Clippy. Full workspace formatting passed on the
preceding snapshot. Production bytes are unchanged by those two test/helper
corrections. Static code, output/UI and workflow/UX reviews passed their recorded
snapshots; these verdicts grant no runtime census or full IDE completion.

The first performance review failed two allocation checks. A long package name
can be copied into many class/method owners before the output budget is checked,
and an expanded annotation prefix can bypass the class-header byte check.
Both findings were escalated before production edits. A
preliminary import-suffix amplification suggestion was withdrawn by its
reviewer and is not a finding. CLI build/execution, final affected reviews and
lead full-app/workspace validation remained pending at that checkpoint. The
Cargo lease was released before the lead's combined build.

An independent performance design review passed the shared-budget/header/writer
proposal, and the lead authorized that exact correction. The new implementation
charges owned metadata strings before copying against one 16 MiB budget shared
across selected files. Exhaustion rejects generation with an explicit unresolved
evidence error before staging. Class headers retain their 16 KiB limit over the
full annotation/modifier prefix, and JSON writes check the 16 MiB output limit
before buffer growth, including the final newline. Publication and canonical
parity data remain unchanged.

Six appended stress groups cover the actual 1 MiB package/512-method shape,
selection-wide accounting, arithmetic and full-header boundaries, escaping,
newline/capacity limits and publication failure. The existing 34 new regressions
and helpers remain byte-identical to the corrected 108-test snapshot. There are
now 40 authored component tests. At that correction checkpoint, the resulting
114-test suite, affected reviews, Clippy, formatting and CLI checks had not yet
run. This is a new lead production
correction after three owner rounds and two lead test/helper corrections;
the previous performance FAIL and design PASS remain separate historical facts.

The external history retains all measurements, exact hashes, wrong-test reasons
and reviewer findings. Freeze the final source/executable only after passing
checks, then preserve output and deterministic change detection. Reviews run
sequentially within the lead's concurrency budget. Subsequent findings after
three owner rounds require escalation before further corrections; the preceding
census task's failures and escalations remain separate retained evidence.

Follow-up tasks must resolve JUnit3 inheritance/suite factories, JUnit5/TestNG
symbol and meta-annotation binding, custom/parameter runner expansion, exact
build target membership, compiled runner descriptions and canonical mirror
coverage before exhaustive parity can be claimed.

## Current validated checkpoint

Module SHA-256
`68bfab04874226583230716ecbb59b7291ba73a4fc54ae62ee1ebc6f19f4700f`
now passes all 114 xtask tests (40 new and 74 prior), with zero failed, ignored or
filtered tests. Required `./script/clippy --locked -p xtask` and full workspace
formatting pass. All five independent scoped source reviews pass on this exact
module: code round 5, output/UI round 2, workflow/UX round 2, performance round 2
and quality round 1. No new source corrections followed those verdicts.

The normal locked release CLI build passes. Its frozen executable is 77,379,264
bytes, SHA-256
`0f7ced36c8eccbc11b6bc4e64850b87b6d6cac9d3a54ce75e6aac5c4f854232f`.
Actual help, original-source generation, byte-identical check, refusal to replace
existing output, changed-evidence rejection, changed-hash rejection, metadata
exhaustion and existing-evidence preservation checks pass. The original 16-file
output is 301,972 bytes, SHA-256
`81df6c8d6eb0d167d89410f898d7de8d9279ee175fbd637a1ca68bb2dc0ed88f`.
It retains 111 method candidates and 71 confirmed direct annotation declarations;
these are selected source facts, not runnable test counts.

The actual 1 MiB package/512-method amplification input is rejected before
staging in approximately 11.6 ms, with an observed per-process `wait4` peak of
21,596 KiB, including child launch. Existing checked evidence remains identical
and no staging files remain. This measurement covers this one synthetic failure
shape, not corpus-wide performance or a process memory ceiling. The metadata
and output limits each bound their own representation.

Freshness touches preserve source bytes. The fixed-exclusion whole-source and
index guards remain identical before and after tests, build and CLI execution;
original fixture bytes, manifests, discovery and canonical parity are unchanged.
Raw successes, failures, measurements and independent reviews are retained.
The historical inherited `RUSAGE_CHILDREN` formatting peak is not used as a
formatting memory measurement; CLI measurements use per-process `wait4`.

This checkpoint is ready for the lead's combined validation. The task is not
merged and does not yet have a combined full-app build/full-workspace pass on
its new source. It grants zero behavioral parity credit, leaves completeness
false and runtime counts null, and does not complete a phase or the IDE.


## Lead combined validation on 93155342

The combined candidate retains all owner source bytes and passes the complete
114-test xtask suite, strict repository Clippy, workspace formatting, a normal
full application build, a normal CLI build, and eight actual CLI flows. All 33
previously ported/adapted reference cases were recaptured against this commit
with the unchanged whole-product exclusions and all 4626 source/fixture hashes.
The completion gate still rejects the incomplete reference census.

The rebuilt full app opened the private Android fixture, completed official SDK
sync with two variants, and passed editor/tab/focus/Hide-restore checks. Native
screenshots stay external; byte-exact JSON and logs are retained under
`evidence/junit4-integration`. Installed Java 6.8.27 reports an existing Gradle
indent query diagnostic; Rust language support and indentation fidelity remain
required in the editing phase.

PR #59 full CI passed: 10423 workspace tests passed, zero failed, and 23
pre-existing tests remain skipped. The 40 new cases introduce no skips.
Linux and macOS application builds and formatting/scripts all pass. CI checked
the same complete Git tree as tested source 93155342. The lead verified every
source and fixture hash, prior parity log and protected/main reference again.
This scoped checkpoint is ready for authorized integration; the merge receipt
is a subsequent event. No behavioral parity credit was added by this tooling
checkpoint, and the phase and IDE remain incomplete.
