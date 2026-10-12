# Rust Android debugger parity

The debugger replaces the custom Kotlin/JDI engine with direct Rust JDWP and DAP.
All existing Android model-token, forward/session ownership, test reservation,
deferred attach, cancellation and runner tests are retained in the IDE crates.
The earlier performance failures remain recorded; their corrected regressions
are retained. Source preparation does not establish that any test passed.

| Reference suite/test | Rust behavior | Status |
| --- | --- | --- |
| fwcd `SampleWorkspaceTest.testBreakpointsAndVariables` | `fixture::sample_workspace_fixture`: identical sample source, VM `-Dtest=testVmArgs`, breakpoint line 8, local `123`, non-null `this`, exact member `"testVmArgs"`. Launch is outside DAP, as in Android deployment. | Adapted; live execution required |
| fwcd `PathUtilsTest.testFilePathToJVMClassNames` | `source::tests::original_class_and_kotlin_facade_share_exact_source_identity`: both original class-name outputs identify `com/abc/MyClass.kt` through actual JDWP signature/SourceFile metadata. | Adapted; tests unrun |
| fwcd `ObjectPoolTest.testObjectPool` | `handles::tests::object_pool_ownership`: zero/six/three/two/zero counts, both owner sets, all six ID lookups and remaining owner set. Additional resume regression rejects stale IDs. | Adapted; tests unrun |
| fwcd `DebugAdapterTestFixture` | Compiler/JVM lifecycle and DAP client in Rust `fixture.rs`; this is a fixture helper, with no independent `@Test` method. | Adapted fixture; live execution required |
| Existing `script/test-android-debugger-jvm` | Entire acceptance sequence now in Rust `fixture::run`: Kotlin/Java source lines, array/field/null inspection, large direct index, all rejected expressions, completion, same-name package identity, inline next/out, Java in/out, exception message, raw coroutine fields, stale handles, live detach, reconnect and VM death. No assertions removed. | Ported behavior; live execution required |
| New wire/source/expression/ownership regressions | Bounded framing, dynamic IDs, event ordering without replies, strict target error codes, partial polling, SMAP arithmetic/ranges/strata/ambiguity, exact paths, grammar rejection, readonly JDWP command trace, array bounds and detach without exit. | New; tests unrun |
| AOSP/IntelliJ Android debugger suites | The existing project-wide reference census remains the authority. This adapter's three upstream tests do not substitute for an exhaustive AOSP/IntelliJ debugger census. | Planned/unported; no completion credit |

The fwcd baseline is `7f05669b642d21afa46ac7b75307fa5d523a7263`. Adapted
test behavior and its original fixture retain MIT attribution in `NOTICE` and
`LICENSE.fwcd`. The four Kotlin/Java Android fixture files remain byte-identical.

Required validation before merging:

1. `cargo test --locked -p android_debugger` and clippy/format checks.
2. `script/test-android-debugger-jvm --adapter ABSOLUTE_RUST_ADAPTER
   --java-home JDK --compiler-classpath KOTLIN_COMPILER_CLASSPATH --stdlib KOTLIN_STDLIB`.
   The Rust probe fails on missing toolchains, unexpected behavior, timeouts or
   missing assertions. It has no skip/ignore path. It records compiler output and
   the complete bounded DAP protocol under the local validation directory.
3. All preserved IDE crate tests, the full IDE build/test suite and five
   independent review areas.
4. Actual emulator/device JDWP and debugger UI acceptance. A JVM protocol probe
   proves those fixture behaviors only and does not prove ART/UI acceptance.

Inspection is intentionally read-only: locals, `this`, explicit fields and
nonnegative array indexing. Calls, property getters, arithmetic, assignment,
conditional evaluation and logpoints are rejected. Existing ASCII identifier
limitations, raw physical-thread coroutine state and unsupported deeply nested
inline/optimized-code behavior remain tracked; none is claimed as Android Studio
debugger parity.

Non-Rust runtime exceptions for this feature: Android SDK/ADB/ART and Gradle own
device deployment and runtime; the official JDK and Kotlin compiler compile/run
test programs. Small shell entry points only dispatch Cargo/the Rust executable
to preserve the existing commands. No Java/JDI debugger engine, Gradle debugger
build, Python installer or Python JVM probe remains in the delivered path.

## Current context integration and reviewer corrections

This proposal depends on the complete PR42 context candidate at commit
`2c91419ab6fcbd5cd82ef13f0b8ed330d3303809` (tree
`da8f5c180ea10b5ce2c2e716d325b0005069eb27`). Android test-panel metadata and
actions use that evaluated Android/KMP/CMP controller. Loading, generic, stale
and untrusted contexts retain the existing readiness and ownership rules;
Python/HTML editors, extension support and the top workspace/project/branch
chooser are preserved. Context loss cancels owned tests without releasing the
Gradle reservation before child cleanup. Existing library Gradle Test routing
remains available; the structured unit/device test panel currently selects an
Android application variant. Library/KMP/CMP structured test execution remains
planned, rather than being marked inapplicable.

Every earlier 123 scenario and all 757 ordered literal assertions are retained.
The raw earlier integration recorded 99 identical whole functions; the other 24 adapt
only fixture setup to the affirmative evaluated controller, its model token and
operation owner. Every test function in the touched PR42 dependency files is
whole-identical except one existing artifact-metadata test, which preserves all
old statements and adds two application-id assertions. No test was deleted,
skipped or weakened. The current scoped
source census is 218 tests/1157 body assertions, including
117 standalone debugger/runner tests. The prior formatter records its explicit
whole-byte whitespace changes; this text does not claim those raw whole-byte
counts describe every later formatted source. The historical 50-test standalone pass
only covers its earlier source. The corrected source and GPUI regressions have
not executed locally because the unchanged resource admission gate was not met.

Supplemental corrections cover page selection before JDWP value reads, valid
start offsets above 1000, bounded multi-command deadlines and response/cache
allocation, irrelevant prepared-class handling, transactional breakpoint
rollback, immediate retirement of individually acknowledged Clear requests,
exception-request ownership after a failed Clear, idempotent pause, cancellation
and disconnect checkpoints, and deadline-controlled DAP output. Live JVM and
Android device fixtures remain required; supplemental mocks are not upstream
execution credit.

The acceptance runner is now Rust and retains all 29 original predicates and
five selectors. Its Linux-only dedicated worker holds unreaped process leaders,
closes owned descendants, and reports bounded cleanup failures. The original
20 runner tests/69 assertions remain whole-identical, with seven additional
supervisor functions. Non-Linux acceptance-worker supervision is unsupported
until a platform backend exists. This is an unported limitation, not an
inapplicability waiver or a silent successful skip. The IDE adapter itself does
not gain a host-JDK prerequisite for attaching to an existing debuggable process.

## Common correction regressions awaiting execution

All 194 earlier scenarios remain. Of their current formatted functions, 191 are
whole-identical. Three supplemental mock fixtures adapt to the new JDWP
protocol: no ClassPrepare watcher is installed at attach, an active watcher uses
EVENT_THREAD and captures an AllClasses snapshot, malformed metadata still
clears the exact created breakpoint ID and the watcher, and the deadline fixture
expects five attach commands plus the same two attempted user commands instead
of six attach commands. These are explicit fixture adaptations, not deleted
scenarios or reduced error/deadline expectations. The 8193 unrelated classes
still exist in the large-VM scenario. Its metadata queries are now deferred and
indexed rather than required before the direct project breakpoint can verify.

The prepared-class index includes cross-package SMAP owners, retains bounded
negative class IDs, and resolves unrelated classes in cancellable batches.
Future class preparation deliberately uses a broad EVENT_THREAD fallback only
while breakpoints exist: guessed filename/package filters would exclude future
Kotlin inline callers. Each event shares one metadata lookup across breakpoints.
ThreadReference.Resume releases that event's owned thread count; a composite
with a real ALL-suspended breakpoint stop stays stopped. Unknown metadata and
failed Clear/Resume operations remain errors with exact request ownership.

The acceptance worker uses a bounded procstat PPID/start-time census, verified
pidfd ownership and a final empty census. Missing task/children files are never
interpreted as proof that adopted Gradle descendants do not exist. Selected
evaluated sibling Java/Kotlin test roots participate in post-save freshness;
unselected sibling roots and unrelated Python/HTML edits do not invalidate that
snapshot. The owner's existing save-all admission remains intact.

The following 24 regressions are authored and formatted, but NOT_BUILT/NOT_RUN.
They add no upstream execution or performance credit. The historical standalone
50-test pass covers only its recorded source; all 117 current standalone tests,
GPUI cases, live JVM predicates and Android/ART UI acceptance remain required.

| Rust regression | Source | Status |
|---|---|---|
| `escaped_session_descendant_is_adopted_and_reaped_by_isolated_worker` (5 assertions) | `crates/android_debugger/src/runner_process.rs` | Authored; not executed |
| `isolated_escaped_census_helper_process` (6 assertions) | `crates/android_debugger/src/runner_process.rs` | Authored; not executed |
| `stat_census_enforces_record_entry_task_child_and_total_byte_limits` (3 assertions) | `crates/android_debugger/src/runner_process.rs` | Authored; not executed |
| `stat_census_finds_adopted_child_without_task_children_interface` (5 assertions) | `crates/android_debugger/src/runner_process.rs` | Authored; not executed |
| `stat_census_ignores_only_a_vanished_process_directory` (2 assertions) | `crates/android_debugger/src/runner_process.rs` | Authored; not executed |
| `stat_census_rejects_malformed_identity_and_truncated_records` (1 assertions) | `crates/android_debugger/src/runner_process.rs` | Authored; not executed |
| `stat_census_rejects_missing_live_stat_or_task_telemetry` (1 assertions) | `crates/android_debugger/src/runner_process.rs` | Authored; not executed |
| `stat_parser_accepts_parenthesized_and_non_utf8_commands` (1 assertions) | `crates/android_debugger/src/runner_process.rs` | Authored; not executed |
| `a_background_class_slice_is_bounded_and_reuses_negative_results` (5 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `a_real_breakpoint_in_an_all_suspended_prepare_composite_is_never_resumed` (5 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `cancelled_configuration_is_retryable_without_a_premature_resume` (5 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `cancelled_step_clear_retains_ownership_without_network_output` (5 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `class_prepare_request_is_event_thread_only_and_absent_without_breakpoints` (6 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `cross_package_inline_prepare_resolves_once_for_all_breakpoints_and_resumes_only_its_thread` (10 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `delayed_large_vm_prioritizes_the_source_and_reuses_metadata_on_edits` (7 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `failed_configuration_resume_leaves_configuration_retryable` (7 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `failed_prepare_thread_resume_retains_the_exact_suspension_for_retry` (6 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `failed_step_clear_during_continue_retains_the_exact_id_until_retry` (8 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `failed_step_event_clear_preserves_the_old_request_until_acknowledged` (6 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `malformed_prepare_metadata_still_releases_only_the_owned_event_thread` (6 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `negative_source_index_reuses_absent_metadata_and_new_sources_invalidate_it` (7 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `primitive_and_null_field_types_charge_one_string_at_the_page_boundary` (5 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `dirty_evaluated_sibling_test_sources_reject_post_save_launch` (10 assertions) | `crates/android_ui/src/android_tests.rs` | Authored; not executed |
| `sibling_test_sources_share_freshness_while_unselected_and_generic_files_do_not` (3 assertions) | `crates/android_ui/src/android_tests.rs` | Authored; not executed |

## Background retry and notification correction awaiting execution

All 218 preceding test functions and their 1,157 body assertion macros remain
whole-identical in this follow-up. Earlier conservation is stated separately:
191 of the earlier 194 functions were whole-identical; three supplemental mocks
explicitly adapted their protocol transcripts and literal predicates. The
rollback fixture changes the original final Clear check to the penultimate
Clear and adds the watcher Clear check; the deadline fixture changes the expected
command count from eight to seven after removing the attach-time watcher.
The original 123 scenarios/757 assertions are preserved through the reviewed
current42 integration chain, not relabelled as newly executed.

A new full-drain regression now actually processes all 8,193 unrelated classes
in bounded background slices, retains the single owned source-cache entry and
limits, and verifies negative-index reuse without additional metadata reads.
The earlier direct-owner prioritization and latency regressions remain unchanged.

Successful stop/thread/verification notifications commit to a bounded outbox
before later indexing or cleanup can fail. Failed EventRequest.Set does not
commit its location key. Consumed step packets report their real stop and retain
only the Clear retry; a client never needs to replay the packet. Actual Unix DAP
loop fixtures verify visible recoverable diagnostics, continued client requests,
acknowledged thread cleanup and real-stop delivery before a later indexing
error. A poisoned framing/transport failure remains fatal and emits a diagnostic
and terminated event. Repeated identical retry diagnostics are suppressed and
background retries back off 100 ms while client requests remain serviced.

Retirement of an unused class watcher uses a separate two-second cancellation-
independent cleanup budget, keeps its exact ID until acknowledged Clear, and
retries rejected cleanup from poll. Original network, operation, cache and
response limits remain unchanged; the notification outbox additionally admits
each fixed-schema JSON event under an explicit 4 KiB ownership reservation and
an 8 MiB retained-event budget before a breakpoint target request can succeed.
All current compiler, unit, GPUI, live JVM/ART and app validation remains pending.

| Rust regression | Source | Status |
|---|---|---|
| `rejected_background_set_does_not_commit_a_location_or_consume_its_retry` (11 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `a_partially_installed_breakpoint_commits_one_notification_before_a_later_set_error` (9 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `prepare_partial_success_preserves_verified_events_and_queues_the_unfinished_class` (12 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `a_real_stop_survives_failure_in_later_background_source_indexing` (8 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `cancellation_after_prepare_set_acknowledgement_cannot_veto_watcher_cleanup` (6 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `failed_unused_watcher_cleanup_retains_its_id_and_retries_from_actual_poll` (5 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `retained_notifications_are_bounded_and_failed_admission_preserves_the_outbox` (5 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `all_8193_foreign_classes_are_indexed_in_background_without_exhausting_owned_cache` (15 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `an_actual_consumed_step_packet_reports_the_stop_and_retries_only_its_clear` (18 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `actual_dap_loop_reports_recoverable_resume_and_keeps_client_requests_alive` (16 assertions) | `crates/android_debugger/src/dap.rs` | Authored; not executed |
| `actual_dap_delivers_a_real_stop_before_the_later_indexing_error` (15 assertions) | `crates/android_debugger/src/dap.rs` | Authored; not executed |
| `actual_dap_reports_a_poisoned_transport_as_fatal_and_does_not_retry_it` (6 assertions) | `crates/android_debugger/src/dap.rs` | Authored; not executed |
| `background_error_diagnostics_are_bounded_at_a_utf8_boundary` (4 assertions) | `crates/android_debugger/src/dap.rs` | Authored; not executed |

## Protocol fairness and consumed-event correction awaiting execution

This successor preserves all 231 previous test functions whole-identically,
their 1,287 body assertion macros, and all 1,484 previous whole-source assertion
macros in their original order. It adds ten tests with 102 body assertions.
The complete current scope has 241 tests/1,389 body assertions/1,604 whole-source
assertion macros; the standalone debugger subset has 140 tests. These bytes are
**unformatted, unapplied, not compiled and not executed**. The historical
50-test passing result covers its older exact source, not this successor.

The behavioral reference for these supplemental protocol regressions is the
[Oracle JDK 21 JDWP specification](https://docs.oracle.com/en/java/javase/21/docs/specs/jdwp/jdwp-protocol.html).
INVALID_CLASS (21) retires that loaded class identity from source-index work.
Other recoverable class failures move to the back of the bounded queue, so a
later valid cross-package inline owner is still examined. Acknowledged target
request IDs and committed notifications retain their existing ownership.
VM_DEAD (112) is terminal even on a still-open connection; the adapter emits
its terminal outcome once and does not retry target commands.

The complete bounded composite packet is decoded before fallible target work.
A real SingleStep stop is reported after a successful Clear followed by a
rejected replacement Set, and a conforming same-thread, same-location
SingleStep+Breakpoint tail is still delivered. The actual DAP fixtures exercise
the adapter loop, not manual replay of an already-consumed event packet.

The specification promotes a system-thread ClassPrepare event (threadID 0)
from EVENT_THREAD to ALL. The adapter owns one separately tracked VM suspend
count per such event set and retires only acknowledged Resume counts. Failure
produces a visible temporary stop; successful retry reports global continuation
with threadId 0, because the event has no Java thread. Explicit user/attach
suspension remains separate. A user Pause acquires its own VM count before an
internal ALL count can be released; Continue does not issue an extra unowned
Resume. Thread cleanup failures do not prevent cleanup of another owned thread
or internal ALL count.

The unchanged `a_real_breakpoint_in_an_all_suspended_prepare_composite_is_never_resumed`
and `actual_dap_delivers_a_real_stop_before_the_later_indexing_error` fixtures
include a ClassPrepare+Breakpoint event mix excluded by JDWP EventSet grouping.
They remain supplemental defensive coverage only, not VM-valid reference tests
or substitutes for the new conforming SingleStep+Breakpoint regression. The
unchanged `failed_step_event_clear_preserves_the_old_request_until_acknowledged`
manually replays its synthetic packet and is ownership-unit coverage only;
actual consumed-wire/DAP delivery is covered by the newer fixtures. No original
test is deleted, skipped or weakened, and no reference execution credit is
claimed for any authored mock.

| Rust regression | Source | Status |
|---|---|---|
| `an_unloaded_front_class_cannot_starve_a_later_cross_package_inline_owner` (10 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `transient_class_errors_rotate_without_losing_later_verification_or_request_ownership` (12 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `promoted_system_thread_prepare_failure_releases_only_its_all_count` (7 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `failed_promoted_all_resume_retains_its_count_and_reports_then_recovers_the_stop` (12 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `user_pause_gains_its_own_count_while_promoted_prepare_cleanup_is_pending` (9 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `continue_from_an_internal_all_stop_does_not_resume_an_unowned_extra_count` (6 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `a_failed_thread_resume_does_not_starve_another_thread_or_a_promoted_all_count` (9 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `actual_dap_delivers_a_consumed_step_after_clear_ack_and_replacement_set_failure` (8 assertions) | `crates/android_debugger/src/dap.rs` | Authored; not executed |
| `actual_dap_preserves_the_same_location_breakpoint_tail_after_replacement_step_set_failure` (10 assertions) | `crates/android_debugger/src/dap.rs` | Authored; not executed |
| `actual_dap_treats_vm_dead_as_terminal_even_while_the_target_socket_remains_open` (19 assertions) | `crates/android_debugger/src/dap.rs` | Authored; not executed |

## Suspension-state acknowledgement correction awaiting execution

This successor preserves all 241 prior functions whole-identically, their 1,389
body assertions, and all 1,604 whole-source assertion macros in their original
order. Three added tests contain 68 assertions: current scope 244 tests/1,457
body assertions/1,672 whole-source assertion macros, standalone subset 143 tests.
The current bytes remain **unformatted, unapplied, not compiled and not run**.

An acknowledged configuration/user Resume releases that explicit count. If an
internal promoted ALL count remains, it still stops the VM, but subsequent
cleanup cannot restore the released attach/user ownership. A failed Resume
retains both ownership and stopped state. An acknowledged automatic filtered-
step Resume instead releases only that consumed location-event count; it
invalidates stale frames and restores any independently acknowledged user Pause
count. These transitions prevent a later promoted prepare cleanup from restoring
a false stopped state and prevent Pause from reporting success without Suspend.

The actual DAP fixture uses a legal SingleStep ALL event followed by a separate
system-thread ClassPrepare ALL event. A normal thread-start packet provides a
deterministic delivery barrier before the next client Pause, avoiding a timed
sleep. The strict target transcript requires a real VM.Suspend and precisely
four Resume commands. The actual consumed-wire configuration fixture retains a
promoted count across a rejected configuration Resume and its acknowledged
retry, verifies a recovery continuation after internal cleanup, then requires a
real user Suspend. The supplemental Session fixture registers and acknowledges
both the step request and separate user Pause before processing its valid
synthetic SingleStep body; the automatic event Resume must preserve that user
count. No prior mock is changed, replayed as runtime evidence or relabelled as
reference execution.

| Rust regression | Source | Status |
|---|---|---|
| `actual_dap_filtered_step_then_promoted_prepare_still_acquires_a_real_user_pause` (30 assertions) | `crates/android_debugger/src/dap.rs` | Authored; not executed |
| `acknowledged_configuration_releases_attach_ownership_while_an_internal_all_count_is_pending` (28 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |
| `filtered_event_resume_preserves_a_separately_acknowledged_user_pause` (10 assertions) | `crates/android_debugger/src/session.rs` | Authored; not executed |


## Private formatting-only validation

All19 Rust source files were formatted and Rustfmt --check passed on owned copies
with the repository configuration and skip_children=true. This formatted proposal
preserves all244 test registrations,1457 body assertions and1672 whole-source
assertion macros. Of244 test functions,218 remain whole-byte exact;26 are explicit
formatter adaptations with only twenty explicitly enumerated trailing-comma
insertions immediately before closing delimiters. Every old test-function/assertion
token, operator, string, number and interior separator remains exact and ordered;
no token is deleted/replaced and no block wrapper is added. The earlier raw
packets and unformatted-status sections are retained historical evidence.

The separate micro workload used128MiB initial/96MiB routine admission,64MiB child
RSS,60-second/4MiB streams and at least32MiB explicitly charged scratch. FORMAT0 and
CHECK0 are syntax/formatting evidence only. All143 standalone and244 scoped tests,
Rust compilation, live JVM/Gradle fixtures, full IDE/app and exhaustive reference
parity still await execution. Global runtime-library metadata was not independently
observed. No compiler/JVM/app gates were changed and no raw143 source was edited.


## CI345 inherited installer fixture adaptation

The active `KotlinBootstrapTests.test_debugger_bootstraps_its_own_shared_sources_and_jdk`
case is adapted to execute the compatibility shell through a controlled Cargo
transport, rather than loading that shell as Python. The other three whole Kotlin
installer tests and all four registrations remain unchanged. The original debugger
case and its exact fixtures/assertions are retained in the before-source packet.
This is a test API adaptation, not deletion, skipping, or full-case exclusion.

| Original predicate/sub-behavior | Status | Rust replacement or precise reason |
| --- | --- | --- |
| Build includes the external shared Kotlin Gradle project | Not applicable sub-behavior | The custom Java/JDI engine and fwcd shared Kotlin project were replaced by the Rust `android_debugger` crate. The adapted active case checks Cargo package/binary/target arguments and repository bootstrap. |
| Included shared-source directory exists | Not applicable sub-behavior | There is no external shared Kotlin source archive; the Rust crate lives in the repository. The active case verifies a repository path containing spaces is selected even when invoked elsewhere. |
| Downloaded Java11 HOME contains java | Not applicable sub-behavior | The Rust installer/engine needs no host JDK. The active transport case succeeds without JAVA_HOME/JAVA11_HOME. Official JVM/compiler runtimes remain required only by the separate real JVM fixture and Android builds. |
| Official Kotlin LSP installation is called before debugger build | Not applicable dependency | The Rust debugger is independent of the Kotlin language server. Its installation must not install or modify that server; both old server-location absence and independence remain active checks. |
| Installed `.revision` matches current debugger version | Ported | Existing Rust `installation_contains_only_rust_binary_and_revision_without_jvm_downloads` plus the new matching-repeat test authenticate `INSTALL_REVISION`. |
| fwcd shared Kotlin library license copied into Java adapter distribution | Not applicable artifact | No fwcd Kotlin library is linked/copied by the Rust adapter. Its old license artifact would misattribute an unused component; Rust dependency/fixture attribution remains retained. |
| Community Kotlin language-server directory is not created | Adapted | Active transport case retains the absence check; Rust binary installation contains only its binary and revision. |
| Shared-source download uses pinned SHA256 and revision URL | Not applicable download | No external fwcd source download occurs; the Rust source/package is bound to the reviewed Git tree and Cargo.lock. |
| Java11 archive uses pinned SHA256 | Not applicable download | Rust installation does not download a JDK; official JDK/Kotlin compiler acquisition in the separate fixture workflow remains pinned and unrun until that CI succeeds. |
| Second install succeeds without repeated downloads | Ported | Exact matching revision/binary/files now produce a read-only repeat success. Unknown/changed installations remain refused; new Rust tests cover repeat, changed binary, extra user content, substituted revision symlink and filesystem failures. |
| Build/dependency failure propagates to caller | Adapted/strengthened | Active transport fixture injects Cargo exit29 and requires exact return29; new Rust filesystem failure test requires user data preserved. |

Matching-install verification is read-only, bounded to the two expected root entries
and one binary entry, a fixed marker length and64KiB binary chunks. It refuses
static symlink components, uses no-follow/nonblocking Unix leaf opens, and checks
opened file identity/size/permissions/modification before and after comparison.
It does not claim arbitrary hostile same-user directory interference is impossible.
All old143 standalone Rust test functions/assertions remain unchanged; five new
Rust tests and a caller-PATH/restricted-sudo transport regression are added.

The genuine first CI failures remain retained: parity run38195767831 did not compile
Rust because the sudo/systemd-run client could not locate cargo; Fork345 failed
Python runpy parsing of the shell entry point. Neither failure is a passing test or
reference-parity claim. New compiler, unit, installer and live fixture execution
remains required. Current local check status is reported by the accompanying
actual receipt rather than inferred from these authored tests.

The unchanged example compiles against API37. The dedicated workflow already
requires its android.jar and build-tools36 aapt2 after sdkmanager returns; these
postconditions are preserved. Earlier unrelated SDK35/build-tools35/36 setup does
not establish API37 availability. Parity1 never reached SDK installation, so all
SDK37-dependent Gradle probes remain unrun and the SDK37 prerequisite is unresolved.
