# Foundation checkpoint

The foundation phase and IDE remain unfinished. In `anilbeesetti/koda`, the first
verified foundation slice was merged and pushed to `android-studio` at
`d2046e36b9448c69d8fe1b6f35efa87fb8714f78`. Its tree is identical to the tested
task source `5577738e0cca988b7d61925f203d03c0b3e48fbf`. The second verified
foundation slice was merged and pushed at
`c19021e1827e347b54b597b53f6f27ea61bce0d0`; its tree matches the verified
backend/catalog task head `92f6a9e1f0291f4911fc6568e3decb2b0a0ff4be`.
The original checkout,
cached `origin/main` and remote `main` remain at
`30fca99a7a015168dfe4f394f576fbe2955fb4ea`. No local `main` branch was created
or modified. Authenticated shell and connector access work; pushes are authorized.

The staging branch is `android-studio-task/1-integration-check`, worktree
`/workspace/android-studio-integration`. Its tested source snapshot is
`f8114bfafa3562760a5e8b98a4b20acf98cce983`. It adds the reviewed Rust module-import
and project-view-preferences backends and model-sync inventory to the merged
plan, default-variant port, recovery fix and parity tooling. Later staging
commits only add catalog, evidence and reports. The complete workspace gate for
these additional backends passed, and this bounded slice is now merged.

The current combined source is staged and pushed outside the protected branch at
`371e79d3dd29f94db1b1cc2d01aff85327630065`. It adds the project-tree projection,
evaluated source/provider membership and per-entry facts, SDK source-generation
provenance, Rust Java declaration facts, the GPUI tool-window toolbar and its
keyboard focus reveal correction, icon registrations, and bounded reference
archive discovery. [Draft PR 58](https://github.com/anilbeesetti/koda/pull/58)
tracks this candidate. The protected branch remains at `c19021e182`.

On this exact source, 174 Android tooling tests, 125 UI tests and both unchanged
icon integrity tests pass. The one existing opt-in UI capture remains ignored;
no new test is ignored. Required release/all-target/all-feature Clippy and full
workspace formatting pass. Two actual Gradle producer/model exports and current
Rust probes verify ordered provider/tree facts and generation flags for Java,
Kotlin, resources and assets. The documented minimal Gradle JVM bridge exports
official SDK objects; Rust owns model decoding and IDE logic. This supplemental
AGP 9.4/Gradle 9.6.1/JDK 21/SDK 37 smoke is not the exact pinned Studio helper.

The complete app built with locked Cargo in 215.5 seconds. Source and index
checks cover 4,606 product/fixture files, and the exact executable hash is
`e5bec9ab384701f21e8af5a9e6a353e24ff1e7c31f677794c878bc7b8aa65521`.
The prior SIGBUS link failure remains retained. An independently reviewed
external Rust helper moved only the owned linker output destination, preserving
all inputs and flags, and its actual compiler/linker probe passed. Fresh native checks pass after restarting the cloud session: narrow and wide
focus/activation, scrolling, hide/restore, successful and intentionally failed
Sync, and actual emitted SDK metadata. Interrupted checks remain uncredited.
All owned app/display processes closed and final source/executable/fixture guards
pass. Screenshots stay in external evidence and do not change the frozen source
closure. Clean software-GPU idle samples were 3.20% wide, 8.09% initially narrow,
and 2.20% on the settled narrow recheck; all three are retained, with no hardware
or large-project performance claim.

All 33 selected manifest-bound exact reference tests were freshly captured on
this source, and the provenance CLI validated sources, commits, declarations,
fixtures and log hashes. [Full CI 37658403968](https://github.com/anilbeesetti/koda/actions/runs/37658403968)
passed every job: formatting/scripts, the macOS app check, the unfiltered
Linux workspace suite and the Linux app build. It ran 10,383 tests, all passing,
with 23 pre-existing ignored tests. The byte-exact completed log and summary
are retained. Native checks now pass; the final lead protected/main/conflict
checks and merge remain pending.

The prior `7b2bd3e7ff` full CI
[37643890651](https://github.com/anilbeesetti/koda/actions/runs/37643890651)
ran 10,349 tests: 10,348 passed and the unchanged icon integrity test failed,
with 23 pre-existing ignored tests. The missing two enum registrations are
corrected in this candidate; both original tests pass locally. Earlier runs
37630209951 and 37632833705 were canceled for revised census source/fixtures and
cannot establish the final gate. All failures and original assertions remain
retained. The independent JUnit source-declaration checkpoint is outside this
candidate and has no new behavioral parity credit.

## Local results

The default-variant owner passed all eight AOSP reference ports, 50 tooling tests,
105 Android UI tests, clippy, the full app build, real Gradle sync and live
variant recovery. One pre-existing opt-in native capture remains ignored; none
of the eight reference ports is ignored. Keyboard selection, resizing and panel
scrolling were checked in the full app. All five scoped reviewers passed after
three fix rounds. The final source is `c992461ee3`; its report and captured logs
are committed on the owner branch at `dd6ffa031f`.

The ledger and catalog expansion passed all 41 xtask tests, including all 21
parity regressions, repository clippy, formatting, and pinned archive/source/
fixture verification. All five scoped expansion reviewers passed. The original
eight-case validator inputs are now immutable fixture files; their bytes and
all original assertions are preserved.

The earlier merged slice captured 26 exact reference cases: eight default-variant,
nine import and nine adapted preference cases. The current source refreshes all
of them and adds seven adapted original toolbar cases. The provenance CLI
validates all 33 source/commit/result/log bindings. The reviewed bounded catalog
contains 92 cases; 59 remain unported.

Combined all-feature checks passed 52 Android-tool unit tests, 20 import tests,
19 preference tests, 105 Android UI tests and 41 xtask tests. The one
pre-existing opt-in native capture was separately executed and passed; none
of the 26 reference ports is ignored. Repository clippy passed with release,
all targets/features and warnings denied. The full app built successfully in
2 minutes 32 seconds with `cargo build --locked -p zed --bin koda`. Logs are
`/workspace/android-studio-artifacts/combined-backends-all-features-tests.log`,
`combined-backends-clippy.log` and `combined-backends-app-build.log` in the same
artifact directory.

A Rust API probe imported an external Android library, checked source/copy
bytes at import, persisted/reloaded preferences and exercised durable
legacy-property migration. The actual full app opened the resulting project
and completed Gradle sync, discovering two application variants. A debug APK
built with Gradle 9.6.1, AGP 9.4.0, JDK 21, Android platform 37.0 and build tools
37.0.0 contains both the application and imported library classes. The retained
APK is 874,020 bytes with SHA-256
`3c289375ebeddf789b870816041ed63229847e14a33f8fad45b91bf2dbcbd483`.
Artifacts, logs and the observed successful sync screenshot are under
`/workspace/android-studio-artifacts/combined-backends-live`. This synthetic
smoke is supplementary validation, not a port of pinned Gradle integration
fixtures. Import-wizard and preference UI wiring remain pending. No device
execution or hardware performance result is claimed.

| Catalog status         | Count |
| ---------------------- | ----: |
| Ported and passing     |    17 |
| Adapted and passing    |    16 |
| Not applicable         |     0 |
| Unported and not run   |    59 |
| Failing reference runs |     0 |

These counts cover the bounded inspected suites. Exhaustive source/runner census is
incomplete and its total remains unknown. JetBrains mirror omissions remain
unverified. The complete-parity gate correctly refuses completion. Additional
model-sync behavior and Gradle-backed Android tree assertions remain unported.

## Blocking gates and next work

The initial local `cargo test --locked --workspace --no-fail-fast` exited 101 during compilation
and linking when the 32 GB filesystem filled. Workspace tests had not begun
execution. Its log is
`/workspace/android-studio-artifacts/full-workspace-tests.log`. Cleaning only
generated output recovered room for focused checks; it does not establish a
full-suite pass. The complete CI run on the first merged source subsequently
passed all Linux/macOS jobs: [run 37609909130](https://github.com/anilbeesetti/koda/actions/runs/37609909130)
ran 10,206 workspace tests, all passing, with 23 pre-existing ignored tests.
No test was deleted, newly ignored, or weakened.

The next full CI run, [37615677970](https://github.com/anilbeesetti/koda/actions/runs/37615677970),
tests `cbf8c0ec26057913f62c8dc9f494cf2baf27945d`, which has the same Rust source
and fixtures as the immutable combined snapshot above. All jobs passed: formatting/scripts, macOS application check and Linux full
workspace tests/application build. It ran 10,247 workspace tests, all passing,
with 23 pre-existing ignored tests. The compile/execution step took 3,048.1
seconds and the Linux application build took 324.1 seconds. The reviewed bounded
backend/catalog slice satisfied its merge gate and was merged at `c19021e182`;
product wiring remains pending.

The earlier tree component snapshot passed 112 all-feature tests, including 21 meaningful
component tests, and five scoped reviews. Its five original Gradle-backed cases
and all ten original assertions remain unported. The source-provider task
passed 94 all-feature tests and actual Gradle export/Rust decoding comparisons;
its matching full-app build passed on combined source and its actual native
Sync passed. The current combined workspace gate is recorded above and remains pending as passed above; the final lead branch/conflict guard remains pending.

The toolbar owner escalated after three failing runtime fix rounds. Lead
investigation retained those failures, fixed signed-zero arrow comparisons and
post-layout reveal/coalescing, and appended three regressions without changing
the original eleven tests or driver. The 14 focused tests and 119 broader UI
tests passed. Actual native a7ef checks then exposed missing plain Tab traversal.
The follow-up preserves those 14 tests, appends two actual key-dispatch
regressions, and opts the explicit focus handles into GPUI traversal. Its final
16 focused tests and 121 all-feature UI tests pass, with the one existing
opt-in capture retained. Required strict repository Clippy and all five scoped
static reviews pass. Those earlier keyboard results are historical. The current focus-reveal correction
passes 20 focused and 125 broader UI tests; 33 exact captures now pass. New-app native validation, clean idle samples and final combined CI now pass
on371; the final lead branch guard and merge remain pending.
The failed initial keyboard tests, app/UI disk-link attempts, and the earlier
39.34% one-core wide-window CPU sample remain recorded.

Bounded archive discovery now passes 74 xtask tests, strict Clippy and all five
scoped reviews on frozen `1a6b161d0e`. The initial real traversal failed after
411.12 seconds because the mirror's Git global PAX metadata header was treated
as a filesystem path; the exact reproducer superseded the earlier directory-only
diagnosis. The narrow typed-metadata correction preserves all existing
assertions and rejects unsupported metadata. Full three-archive generation
passed in 465.506 seconds with peak RSS 32,456 KiB; deterministic `--check`
passed in 517.695 seconds with peak RSS 32,604 KiB. Ten retained gzip inventories
have verified decompressed hashes, preserving the 450 MB raw output in 46.9 MB.
They contain 108,311 source/build/runner candidates and 342,071 lexical method
candidates. These are not executable-test counts. Inheritance, parameters,
custom runners, 934 unresolved source files and mirror coverage keep the
exhaustive effective census incomplete; no parity credit was added.

The bounded Rust Java declaration-facts producer is integrated into the staged
candidate. Its 29 focused regressions, 144 all-feature tooling tests, strict
Clippy and five scoped reviews pass. It is not a full Java parser or language
server, and UI wiring remains pending.

Evaluated active-provider and per-entry facts are now integrated in candidate371.
The owner first escalated a private-helper lifetime failure after three fix
rounds, followed by a redundant test clone and mistaken new smoke expectation.
The narrow lead corrections preserve every original assertion and fixture; the
29 new fact tests, complete component tests and five affected reviews pass.
Their exact histories and wrong-test reasons remain in the task report. The
subsequent SDK provenance correction resolves the static/generated conflict
without guessing from path names. Root's current 174 tooling tests and real
Gradle/Rust probes validate the combination. The five original Gradle-backed
tree cases and ten original assertions remain unported pending their real
model/refresh/UI integration.

The foundation still needs exhaustive census, module-import and preference UI,
Android tree rendering/navigation and refresh integration, complete model/sync behavior,
window fidelity and creation workflows. Editing, build/run, Android tooling and
polish remain planned. Rust language servers and the full emulator/device/Logcat
done flow are not complete. This environment has no ADB target or `/dev/kvm`;
device execution was not validated.

Parallel work runs as internal subagents. Published task branches carry the
tree component, source-provider metadata, retained failing toolbar snapshot,
lead toolbar correction and bounded census candidate. Combined native validation and the dependent tree-to-project snapshot adapter
are the next foundation work; the evaluated facts are included in this candidate. The final Java
and census implementations are included in the pushed integration candidate. The tree's five original Gradle-backed cases remain
unported until real model/fixture/refresh/UI integration exists.
Sidebar chat creation is unavailable
in this cloud session: the callable app tools have no `create_thread` or
`fork_thread`. Handoff briefs are saved beside this report. A new environment
can fetch the published task branches; SDK files, upstream source
archives and external build/UI evidence require separate workspace transfer.

A portable Git bundle is prepared at
`/workspace/android-studio-artifacts/android-studio-local-tasks.bundle`. It carries
the earlier preparation branches and requires the original base commit above
to exist in the receiving repository. It predates the latest commits; GitHub
contains the published branches. SDK files, upstream source archives and
external build/UI evidence are separate workspace artifacts.
