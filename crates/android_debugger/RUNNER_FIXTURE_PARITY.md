# Rust test-runner fixture probe

`android-debugger probe-test-runner` ports the existing authored
`script/test-android-test-runner` acceptance probe. This is supplemental fixture
coverage, not an Android Studio/AOSP test-suite port. The historical Python
proposal remains retained; only the active entry point changes to Rust.

Build `cargo build -p android_debugger --bin android-debugger`, then run
`script/test-android-test-runner`. For a private Cargo target, set
`KODA_ANDROID_DEBUGGER` to its compiled `android-debugger` executable. The shell
file only resolves the repository and execs that binary. The Rust CLI also
accepts `probe-test-runner --root REPOSITORY [--device SERIAL]` directly.

The probe requires a full JDK, Android SDK, and normal Gradle repository access.
`--device SERIAL` selects an already connected Android device; it does not start
an emulator or claim device coverage when omitted. Gradle, its wrapper, the JVM,
Android SDK, and the optional device are existing platform/test-runtime
exceptions. Model parsing, command construction, report parsing, event checking,
timeout control, acceptance predicates, and supplemental tests are Rust.

## Original acceptance predicates

Python line numbers below refer to the retained 6,072-byte source with SHA256
`93e7b8264b6bb3c5ba06157c5d8883fb2c4aff385649a471fb948fa0980ddffb`.
Every row is authored in Rust but **NOT RUN** at source handoff. The nine original
assert sites expand to 27 assert invocations, or 29 atomic predicates when the
device conjunction is counted separately. No case is skipped or weakened.

| ID | Python line | Original predicate | Rust implementation | Regression |
|---|---:|---|---|---|
| M1 | 68 | Exactly one `KODA_ANDROID_PROJECT_MODEL=` record | `validate_model_output` | `model_record_and_component_selection_are_exact` |
| M2 | 76 | Unit component Java/Kotlin sources contain `src/testDemo/java` | `validate_model_output` | `selected_variant_source_predicates_are_preserved` |
| M3 | 77 | Unit component Java/Kotlin sources contain no `testFull` | `validate_model_output` | `selected_variant_source_predicates_are_preserved` |
| M4 | 76 | Instrumentation component Java/Kotlin sources contain `src/androidTest/java` | `validate_model_output` | `selected_variant_source_predicates_are_preserved` |
| M5 | 77 | Instrumentation component Java/Kotlin sources contain no `testFull` | `validate_model_output` | `selected_variant_source_predicates_are_preserved` |
| F1 | 94 | A `Failed` event detail contains `RunnerTest.java:` | `validate_unit_probe`, failing class case only | `failed_event_must_include_original_java_source_location` |
| D1 | 100 | Exactly one Compose instrumentation testcase | `validate_device_probe` | `device_keeps_all_three_original_predicates` |
| D2 | 100 | Its direct `failure` child is absent | `validate_device_probe` | `device_keeps_all_three_original_predicates` |
| D3 | 100 | Its direct `error` child is absent | `validate_device_probe` | `device_keeps_all_three_original_predicates` |

The remaining 20 predicates are explicit below. Each case uses
`testDemoDebugUnitTest`; each column is independently checked by
`validate_unit_probe` and independently rejected when perturbed by
`every_original_unit_case_checks_all_four_counts`. A testcase containing both
`failure` and `error` still contributes **one** failed testcase, as before.

| IDs | Selector | Expected Gradle failure | Testcase count, line 88 | Failure/error count, line 89 | Skipped count, line 90 | Event count, line 92 |
|---|---|---|---:|---:|---:|---:|
| U1a/U1b/U1c/U1d | `dev.zed.androidsample.RunnerTest.passes` | false | 1 | 0 | 0 | 1 |
| U2a/U2b/U2c/U2d | `dev.zed.androidsample.RunnerSuite` | false | 5 | 0 | 1 | 5 |
| U3a/U3b/U3c/U3d | `dev.zed.androidsample.RunnerTest` | true | 3 | 1 | 1 | 3 |
| U4a/U4b/U4c/U4d | `dev.zed.androidsample.RunnerTest.optionalFailure` | false | 1 | 0 | 0 | 1 |
| U5a/U5b/U5c/U5d | `dev.zed.androidsample.ParameterizedRunnerTest` | false | 2 | 0 | 0 | 2 |

Model lookup still requires module `:mobile`, variant `demoDebug`, and components
`demoDebugUnitTest`/`unitTest` and `demoDebugAndroidTest`/`androidTest`. Missing
required fields remain errors. The failure-event search retains the original
short circuit and missing-key errors before the first matching source location.

## Execution and output contracts

| Contract | Rust implementation and regression |
|---|---|
| Each Gradle exit is nonzero exactly when its case expects failure | `validate_exit`; `gradle_exit_must_match_expected_failure` |
| Unexpected exit diagnostic includes the original numeric exit code | `exit_code`; `unexpected_gradle_result_keeps_the_original_numeric_message` |
| Runner init-script bytes are extracted unchanged from `testing.rs` | `extract_init_script`; `script_extraction_preserves_bytes_and_rejects_missing_delimiters` |
| Project-model script is read from `crates/android_tools/src/project_model.gradle` | `run_fixture`; fixture source remains unchanged |
| Unit selectors use `--tests`; failure uses `-Pkoda.fixture.fail=true` | `probe_arguments`; `gradle_command_flags_and_selectors_match_original` |
| Model/runner variant flags, output directory, no-daemon and no-configuration-cache flags remain exact | `model_arguments`/`probe_arguments`; full argument-vector regression |
| Device task is `connectedDemoDebugAndroidTest`, selector `dev.zed.androidsample.RunnerComposeTest#showsContent`, kind `device`, and `--rerun-tasks` | `run_fixture`/`probe_arguments`; full argument-vector and device predicate regressions |
| Only selected device probe overrides `ANDROID_SERIAL`; normal inherited environment remains | `prepare_probe_command`; `device_serial_is_only_added_to_the_selected_probe_environment` |
| Every Gradle invocation retains a 600-second deadline | `TIMEOUT`/`wait_for_command`; `gradle_exit_must_match_expected_failure` |
| Each probe gets a fresh temporary directory and recursive `TEST-*.xml` discovery | `run_probe`/`read_reports`; `nested_reports_events_and_fresh_temp_directories_are_used` |
| Unqualified testcase descendants and direct status children match ElementTree | `parse_report`; descendant/direct-child and namespace regressions |
| JSONL is parsed line by line; empty interior lines remain errors | `read_events`; nested report/event regression |
| Model combined stdout/stderr is printed before predicates | `run_fixture`, one duplicated log handle, `println!("{output}")` |
| Missing `--device` prints the original explicit NOT RUN result | `DEVICE_NOT_RUN`; argument/output regression |

Original success/output messages remain exactly:

```text
PASS: selected variant unit/instrumentation discovery, method/class/suite, pass/fail/skipped, failure source, fresh rerun, parameterized class
PASS: Compose instrumentation method on selected device
NOT RUN: device/instrumentation validation (--device required)
```

The selected device success line is printed only after its original predicates
pass; otherwise the NOT RUN line applies. Existing Gradle/JUnit/Compose fixtures
are reused at their original paths. No fixture source is rewritten by this port.

Rust adds explicit error limits: 32 MiB per input, 128 MiB aggregate reports,
100,000 directory entries, 65,536 testcases/events, and XML nesting 128. These
limits reject excess or malformed input rather than truncate accepted results.
DTD-bearing reports and symlink reports are rejected explicitly. Original
Gradle fixture reports use UTF-8 XML without DTDs; namespace resolution preserves
unqualified ElementTree matching. On Windows, model and test probes both use the
existing Gradle batch wrapper instead of assuming `/bin/sh`. On Unix, timeout
cleanup kills the probe's owned wrapper/JVM process group and reaps the wrapper.

Source formatting is the only tool validation at handoff. Compilation, all Rust
tests, Gradle execution, device/emulator execution, IDE builds, full CI, and
reference parity are pending. This probe does not add a custom JVM debugger.

## Bounded cleanup follow-up

The public probe hands off by Unix exec to a dedicated Linux worker. Only that
worker enables child subreaping. It keeps each owned Gradle leader unreaped until
its process family is closed, uses pidfds for observed descendants, and reports
inspection, signalling, deadline and incomplete-cleanup errors. Both successful
wrapper exit and failure clean up lingering owned descendants. Seven added
supervisor functions include injected error/deadline cases and an isolated real
lingering-child regression. They remain unrun in this corrected proposal.

Non-Linux fixture-worker supervision is not implemented and the opt-in CLI
refuses explicitly. Original acceptance predicates remain required and unported
on those platforms; this limitation does not justify marking them not applicable.
