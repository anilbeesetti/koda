# Android/Kotlin debugging

Install `script/install-android-debugger` and launch Koda using `script/android-ide`.
The managed runtime must have revision
`7f05669b642d21afa46ac7b75307fa5d523a7263+android-2` in its `.revision` file.
The executable path remains `target/android-ide/dependencies/kotlin-debug-adapter/bin/kotlin-debug-adapter`.
The installer builds and verifies the pinned source and Android patch. An explicit
user-installed debugger override remains available through the existing debugger settings.

**Debug** still saves, builds and launches the selected app before attaching.
Selected model roots supply Java/Kotlin sources, including selected dependency
variants. Changing the root, variant or device, or invalidating Gradle/model/resource inputs,
disconnects the owned session.
Disconnect removes its JDWP forward and leaves the app alive; disconnect before
redeploying. The inherited debugger provides breakpoints, exception filters,
stack frames, stepping, watches, variables and evaluation.

## Attach to a running process

In the Android tools panel, choose a connected device and use **Refresh debug processes**,
then the process picker. Only JDWP-enabled processes belonging to the selected
application are listed, including `application:secondary` processes. The PID is
rechecked before attachment. Refresh after a process exits or restarts. Application
identity comes from selected-variant APK metadata; attachment does not require
universal APKs or existing APK files. Metadata must already have been generated.

Attaching to an existing process refuses unsaved selected Java/Kotlin sources or
Gradle/manifest/model inputs, since they may disagree with installed bytecode.
Unrelated unsaved notes do not block attachment. Run/Debug saves the workspace
before building, refuses remaining dirty launch inputs if saving fails or is
cancelled, and rereads the selected run configuration after saving.
A timed-out or cancelled deferred start removes its forward immediately and
checks for a late session for another 30 seconds, without shutting down unrelated
debugger sessions. A second attach or deployment requires disconnecting
the current Android session first.

## Reusable launch configurations

Create `.zed/android-run.json` in the Android project root, then use the panel's
configuration reload button and picker. **Default launcher** retains the existing
launch behavior. The named selection applies to both Run and Debug; it is kept in
the current panel rather than persisted as a separate workspace setting.

```json
[
  {
    "name": "Main activity",
    "activity": ".MainActivity",
    "forceStop": true,
    "waitForDebugger": true
  },
  {
    "name": "Open profile",
    "deepLink": "sample://profile/42?tab=details",
    "flags": 268435456,
    "waitForDebugger": true
  },
  {
    "name": "Worker activity",
    "activity": ".WorkerActivity",
    "process": ":worker",
    "waitForDebugger": true
  }
]
```

Use actual activities, intent filters and process names from your app. `activity`
may be a relative class, fully qualified class, or `application/class` component.
A deep link uses VIEW and is restricted to the installed application. Activity and
process ownership are checked against the selected variant's application ID,
including suffixes. `process` chooses the debugger's process; it does not change
the activity's manifest process. `flags` is an unsigned Android intent flag bitmask.
`forceStop` requests `am start -S`; `waitForDebugger` requests `-D` only for Debug.
Without it, early startup breakpoints may be missed. Run uses normal launch waiting.
Android's textual launch errors are surfaced even when `am` exits successfully.
Unknown fields and malformed, oversized or duplicate-name configurations fail
validation. Deep links are shell quoted, including apostrophes and shell metacharacters.
Arbitrary extras, shell commands and launch-before tasks are not supported.

## Supported JVM inspection and limits

* Inline breakpoints match all loaded/future Kotlin SMAP locations. Next from a
  callsite skips the inline implementation; Next inside an inline body advances
  its source lines. Step Out from an inline body returns to the callsite. Source
  identities include paths, so equal filenames in different packages stay distinct.
* Mapping uses Kotlin/KotlinDebug strata when the VM exposes them. Without these
  strata, Java line metadata is the fallback. Missing or ambiguous files are not
  guessed. Android/D8 can discard metadata; JVM probe results do not establish
  equivalent mapping on every Android runtime.
* Evaluation supports read-only local/`this`/field paths and literal nonnegative
  array indices, for example `user.name` or `values[2000]`. Watches and hover share
  this behavior. Unloaded field types and large-array indices are supported.
  Calls, Kotlin properties/getters, arithmetic, assignments and arbitrary Kotlin
  expression compilation are not implemented and return errors. Array child
  enumeration is bounded to 1,000 elements. Conditional/hit-count breakpoints and
  logpoints are explicitly rejected rather than treated as unconditional stops.
* Exception details read Throwable fields without invoking debuggee methods.
  Stale frame/variable handles are invalidated on resume and step.
* A stopped continuation exposing `label` and `completion` gets a **Coroutine
  state (raw)** scope with spilled fields. Newly resumed worker threads are
  discovered. This does not enumerate suspended coroutines, reconstruct logical
  coroutine stacks, or follow a suspend step across threads. Coroutine runtime
  trampolines are skipped during ordinary physical-thread line stepping.
* Nested inline/suspend semantics, optimized release bytecode, arbitrary expression
  evaluation and NDK/LLDB integration remain gaps. This is not Android Studio parity.

## Validation

The device-free protocol fixture uses an actual Kotlin compiler, JDWP VM and
adapter. Supply a compiler classpath containing its dependencies:

```sh
script/test-android-debugger-jvm \
  --adapter /path/to/adapter/bin/kotlin-debug-adapter \
  --java-home /path/to/jdk \
  --compiler-classpath '/path/to/kotlin/compiler/lib/*' \
  --stdlib /path/to/kotlin/compiler/lib/kotlin-stdlib.jar
```

It checks Kotlin/Java breakpoints, cross-package inline Next/Step Out, Java Step
In/Out, stale frame rejection, watches/hover capability, completion, unsupported
expressions/breakpoints, unloaded field types, large arrays, caught exceptions,
resumed continuation state, detach/reconnect and abrupt process death. Logs are
written under `target/android-ide/validation/jvm-debugger`.
`script/test-android-debugger --device emulator-5554` retains the explicit
emulator Java/Kotlin breakpoint, evaluation and detach smoke test.

For Android acceptance, build `demoDebug` and `fullDebug`, then check both default
and configured launches, deep links and an app-declared secondary process. Test
exception stops, stepping through inline/suspend code, unsaved source/model edits,
variant/device changes during attachment, repeated attach/detach, redeployment,
ADB disconnect/reconnect and process death. Confirm `adb forward --list` has no
owned forward after each terminal path. These require a real emulator/device;
the JVM fixture and GPUI tests cannot substitute for ART or rendered UI checks.
