# Android test-runner acceptance probe

The Rust CLI exercises the test runner's actual Gradle init script against the
repository's Android fixture:

```sh
cargo build --locked -p android_debugger --bin android-debugger
target/debug/android-debugger probe-test-runner --root "$PWD"
```

Use the actual Cargo target directory when `CARGO_TARGET_DIR` is set. This command
executes trusted fixture Gradle code and needs a compatible JDK, Android SDK, and
access to the fixture's Gradle dependencies. It runs directly in
`examples/android-ide`, so use a disposable checkout or copy of the repository.
Gradle may write build outputs there. Probe result/log files use temporary
directories and are removed on exit; capture CLI output when retaining evidence.

Assertions cover selected-variant discovery, class/method/suite selectors,
pass/fail/skipped results, failure source information, fresh reruns, and
parameterized classes. Process-supervision unit tests cover timeout/cancellation,
descendants, escaped sessions, and incomplete-cleanup diagnostics. These CLI
checks do not exercise the interactive Tests panel.

Add `--device SERIAL` for the Compose instrumentation assertion on a connected
Android target. Without it, the probe explicitly reports device validation as
not run. A unit-test or local-JVM pass must not be reported as Android/device
acceptance. No prior branch's test counts or CI results apply to this port.
