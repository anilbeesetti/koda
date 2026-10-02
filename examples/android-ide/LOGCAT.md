# Logcat

Open **Android: Logcat** (`Cmd+6` on macOS, `Alt+6` on Linux/Windows), or use
**Open Logcat** in the Android panel. Logcat opens as a workspace tab and can be
opened before a device is connected. The project must be local and trusted.

The device selector belongs to Logcat. It shows device/AVD names, serials,
Android versions, API levels, and connection states. Disconnected devices stay
in the selector; capture resumes when the selected device reconnects. Selecting
another device starts a separate source and clears the displayed logs. Process
metadata uses `ps -A -o PID,UID,ARGS`; package metadata uses `pm list packages -U`.
Older devices without these commands can still capture logs but report degraded
application/process filtering. Use **Refresh devices** after installing apps.

Each message shows its timestamp, severity, PID/TID, application, process, tag,
and complete message. Multiline stack traces remain a single message. Select a
message to see links to matching Kotlin/Java source files in the project.
Severity uses the IDE's theme colors. Stack traces can be folded from Options,
and selecting a folded message expands it. Process start/end separators are
detected from the process snapshots every two seconds. **Compact** hides the detailed header;
**Wrap** controls message wrapping.

## Filters and search

Application, process, and minimum-severity selectors combine with the query.
**Project applications** and `package:mine` use application IDs from the synced
variants' APK metadata; build the app first if that metadata is unavailable.
Process selections use process names so they survive PID changes after a restart.

The query supports:

| Syntax | Behavior |
| --- | --- |
| `tag:Network package:dev.example` | Substring matches; different fields combine with AND |
| `tag:Network tag:Database` | Repeated positive fields combine with OR |
| `-tag:Noisy -package:other.app` | Exclusions combine with AND |
| `tag=:Network` | Exact match |
| `message~:"timeout\|failed"` | Regular expression |
| `level:WARN` | Warning and higher |
| `is:error` | Exactly error severity |
| `is:crash`, `is:stacktrace`, `is:firebase` | Android Studio's specialized filters |
| `age:30s`, `age:5m`, `age:2h`, `age:1d` | Messages newer than the given age |
| `package:mine` | Applications from this Android project |
| `pid:123 tid:456 uid:10123` | Exact numeric identities |
| `(tag:Network \| tag:Database) & level:ERROR` | Explicit boolean operators and parentheses |
| `NOT tag:Noisy` | Explicit negation |
| `name:"Network errors" level:ERROR` | A label for a saved query |

`process:`, `message:`, and `line:` also support substring, exact, regex, and
negative forms. Regexes use Rust regex syntax; lookaround and backreferences
are unsupported. Quote values containing spaces, parentheses, or boolean operators.
Plain words search the formatted message. The filter's **Aa** button controls
case sensitivity. Invalid queries show an error and preserve the last valid query.
Use **Suggestions** to complete the trailing filter term from supported fields
and observed tags/applications/processes. Use **Options → Save current filter** to
retain queries across sessions.

The separate Find field searches the displayed messages, highlights matches,
and supports case-sensitive and regex searches. **Next** and **Previous** wrap
through matching messages. Select a row, Shift-click a range, or Cmd/Ctrl-click
individual rows, then use **Copy selected**. Each row also offers **Copy message**
and tag/application include/exclude shortcuts.

## Capture and files

**Pause** freezes the displayed snapshot while capture continues. **Resume**
shows the retained live messages. **Clear view** clears the local display;
**Options → Clear device log buffers** separately clears ADB's buffers.
**Restart** restarts capture from the device's retained history. Scrolling away
from the end suspends automatic scrolling; **Scroll to end** restores it.

The default buffers are main, system, and crash. Options also offers all buffers
and individual main/system/crash/radio/events buffers. Event tags use the device's `/system/etc/event-log-tags` names when available;
unknown tags retain their numeric IDs. Unsupported event payloads are shown as
diagnostic hex.
Retention defaults to 16 MiB and can be set to 1–64 MiB. Eviction counts are
visible, and paused snapshots remain readable even when live entries are evicted.

**Export** saves the currently displayed messages. The default `.jsonl` format
preserves all structured fields. Choose a `.json` filename for Android Studio's
`logcatMessages` format. **Open logs** accepts either format, Android Studio JSON
exports, and threadtime text logs with or without a year. Import stops live
capture; select a device to return to capture. File imports are limited to
64 MiB and 100,000 messages.

**Options → New Logcat view in split** creates an independent viewer. Each view
has its own source, filters, pause state, and retention buffer. **Terminate
selected application** operates on a specific selected application.

## Android Studio comparison

The reference was Android Studio's public [Logcat implementation](https://github.com/JetBrains/android/tree/master/logcat/src/com/android/tools/idea/logcat),
including its filter parser, device selector, formatting, file I/O, and actions,
See also the [Logcat documentation](https://developer.android.com/studio/debug/logcat).

This implementation covers the main capture, device selection, structured display,
filtering, search, copy, save/load, split-view, pause, restart, and app-termination
workflows. It does not yet provide R8/ProGuard retracing, bugreport ZIP/Firebase
file import, automatic completion popups, custom column/font presets, or character-level
text selection.

## Validation

Run `cargo test --locked -p android_tools --lib` and
`cargo test --locked -p android_ui --lib android_logcat` with the workspace
dependencies available. Run `./script/clippy --locked -p android_tools -p android_ui`.

For device validation:

1. Open this example, build `demoDebug`, and open Logcat before connecting a device.
2. Connect an emulator and a physical device. Verify names, versions, states,
   independent device selection, disconnect/reconnect, and unauthorized-device feedback.
3. Run the app, select its application and process, then emit each severity and
   a multiline Java/Kotlin exception. Verify PID/TID, UID, tags, colors, wrapping,
   crash/stacktrace queries, and source navigation.
4. Try repeated tag terms, exclusions, parentheses, regexes, case sensitivity,
   `package:mine`, and an invalid expression. Restart the app and check filtering.
5. Pause, generate enough logs to evict entries at 1 MiB, and confirm the snapshot
   stays readable. Resume, clear the view, disconnect/reconnect, and check that
   old cleared messages do not reappear. Close the tab and verify its ADB reader exits.
6. Search with literal and regex text, copy a selection, export JSONL and Studio
   JSON, and import each into the IDE. Open the Studio JSON in Android Studio too.
7. Open a split Logcat view, use a different source/filter, and check independence.
8. Revoke project trust and verify capture stops. Restore trust and refresh devices.

The cloud session passed 16 Android tooling tests against the actual source in an
isolated manifest using cached dependencies, plus Clippy for the new Logcat module,
formatting, and manifest validation. The full workspace build and GPUI test were
blocked by uncached crate archives; desktop/emulator validation remains pending.
