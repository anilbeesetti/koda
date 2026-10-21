# Android tests and debugging

Sync this project and select `:mobile` / `demoDebug`. Use **Android: Discover
Tests** to inspect the selected unit-test sources, **Android: Test** to run the
suite, or **Android: Instrumentation Test** to run device tests on the selected
connected device. **Android: Toggle Tests** opens the Tests panel.

The panel groups results by class and shows pass, failure, skipped, duration and
failure details. Select a method or class to run it, rerun failures, or follow its
source location. Parameterized/display-name cases rerun their owning class when
they cannot be represented as a Gradle method filter. Discovery reads source
declarations; Gradle execution and its reports determine the actual test cases.
Generated/dynamic cases can therefore appear only after execution.

Each run uses a fresh report directory. Stop retains completed results while the
process and scanner finish cancellation. Gradle acknowledges cancellation through
its build cancellation service before a replacement run is admitted (validated
with Gradle 8.11.1 and 9.6.1). Unsupported cancellation services fail explicitly. If completion cannot be
confirmed, reruns stay blocked: confirm the old test process has stopped, then
reopen the workspace.
Saving, selected-model refresh, variant/device changes and edits invalidate old
results; interrupted runs and incomplete reports remain visible as such.

The Java fixtures cover passing, optional failing, skipped, suite and
parameterized tests. Failure is opt-in with `-Pkoda.fixture.fail=true`; the normal
sample suite passes. `RunnerComposeTest` requires an Android target. The optional
Rust probe exercises the actual runner script:

```sh
cargo run --locked -p android_debugger --bin android-debugger -- \
  probe-test-runner --root "$PWD"
# Add --device SERIAL to include Compose instrumentation acceptance.
```

This probe needs the sample's Android SDK, a compatible JDK and Gradle dependency
access. It runs trusted Gradle code. JVM unit tests do not establish device test
acceptance.

The toolbar's launch menu can reload named configurations from
`.zed/android-run.json`. For example:

```json
[
  {
    "name": "Demo activity",
    "activity": "dev.zed.androidsample.MainActivity",
    "waitForDebugger": true,
    "forceStop": true
  },
  {
    "name": "Open an item",
    "deepLink": "https://example.test/items/42",
    "flags": 268435456
  }
]
```

Activities and optional `process` names must belong to the installed application.
Use a fully qualified activity class when namespace and application ID differ,
as with the sample's flavor suffixes. A secondary process can be written as
`:worker`. Unsupported fields are rejected. Save configuration and selected
source/model edits before launching; launch inputs are rechecked after save.

The process menu refreshes JDWP-enabled processes belonging to the selected
application and can attach without redeploying. Disconnect leaves the application
running and releases the owned session/ADB forward. Variant, project or device
changes invalidate attachment ownership. Requests cancelled before session
creation cannot take ownership of a later request; late-session cleanup is
bounded to 30 seconds. Forward removal is best effort if ADB is unavailable.

Koda retains its managed Kotlin adapter as the default. The optional standalone
Rust adapter and real-JVM probe are documented in
[`crates/android_debugger/TEST_PARITY.md`](../../crates/android_debugger/TEST_PARITY.md).
Read-only watches, supported inline mappings and raw continuation fields are
bounded debugger features, not arbitrary Kotlin expression execution or logical
coroutine-stack support. Android/ART breakpoints, D8 mappings, actual secondary
processes, deep-link launches and device reconnection require device acceptance.
