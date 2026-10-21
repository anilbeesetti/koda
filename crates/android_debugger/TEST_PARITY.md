# Optional Rust Android debugger

`android-debugger` is a standalone DAP/JDWP adapter and validation CLI. The
application continues to resolve its managed Kotlin debugger by default. Building
this crate does not replace that runtime or change provisioning. To explicitly
try the Rust adapter, set `ANDROID_IDE_KOTLIN_DEBUGGER` to its absolute executable
path before launching Koda.

Build and test from the repository root:

```sh
cargo build --locked -p android_debugger --bin android-debugger
cargo test --locked -p android_debugger
./script/clippy --locked -p android_debugger
```

Run the JVM acceptance probe with an installed JDK (including `javac`), a Kotlin
compiler classpath including its dependencies, and the matching Kotlin stdlib:

```sh
target/debug/android-debugger probe-jvm \
  --adapter "$PWD/target/debug/android-debugger" \
  --java-home "$JAVA_HOME" \
  --compiler-classpath "$KOTLIN_COMPILER_CLASSPATH" \
  --stdlib "$KOTLIN_STDLIB" \
  --repository "$PWD"
```

When `CARGO_TARGET_DIR` is set, use the binary under that directory instead.
The probe compiles the Java/Kotlin fixtures, starts local JVMs, and records bounded
protocol diagnostics under `target/android-ide/validation/jvm-debugger`.
It fails on an unmet assertion. A build or unit-test pass is not a JVM probe pass.

The probe covers source identity, Java/Kotlin breakpoints, inline stepping,
read-only fields/arrays, unsupported-expression rejection, stale handles,
exceptions, raw continuation fields, detach/reconnect, and abrupt VM death.
Mock protocol regressions additionally cover paging bounds, malformed replies,
operation deadlines, cancellation, event ownership, and cleanup failures.

Inspection never invokes application methods. Arbitrary Kotlin expressions,
logical coroutine stacks, cross-thread suspend stepping, hot swap, optimized-code
parity, and NDK debugging are unsupported. JVM results do not establish Android
ART behavior, D8/SMAP fidelity, device process selection, ADB forwarding cleanup,
or live application UI acceptance. Those require separate device/UI checks.
Results obtained with this Rust executable do not validate the managed Kotlin
adapter; pass its executable as `--adapter` to probe that runtime separately.
