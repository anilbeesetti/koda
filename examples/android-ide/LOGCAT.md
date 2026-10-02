# Logcat

Open **Android: Logcat** (`Cmd+6` on macOS, `Alt+6` on Linux/Windows), or use
**Open Logcat** in the Android panel. Logcat opens in the resizable bottom dock,
alongside the terminal pane, and can be opened before a device is connected. Its tabs and split views stay in that dock;
opening or hiding it preserves the editor tabs. The dock uses Android Studio’s
official Logcat icon, copied from [PR #33](https://github.com/anilbeesetti/zed/pull/33)
with its Apache license notice. The project must be local and trusted.

The device selector belongs to Logcat. It shows device/AVD names, serials,
Android versions, API levels, and connection states. Disconnected devices stay
in the selector; capture resumes when the selected device reconnects. Selecting
another device starts a separate source and clears the displayed logs. Process
metadata uses `ps -A -o PID,UID,ARGS`; package metadata uses `pm list packages -U`.
Older devices without these commands can still capture logs but report degraded
application/process filtering. Use **Refresh devices** after installing apps.

Each record puts its timestamp, PID/TID, tag, application, process, UID, severity,
and message on one row. Long lines share a horizontal scrollbar when wrapping
is off; vertical scrolling does not move the text horizontally. Multiline stack
traces remain a single message. Select a
message to see links to matching Kotlin/Java source files in the project.
Severity uses the IDE's theme colors. Stack traces can be folded from Options,
and selecting a folded message expands it. Process start/end separators are
detected from the process snapshots every two seconds. **Toggle compact metadata**
in Options hides detailed metadata;
**Wrap long lines** controls message wrapping and is off by default.
Capture actions are icon buttons in the left rail, with descriptive hover tooltips.

## Filters and search

The query starts with `package:mine`, showing the current application selected
in the Android project tools. It follows app/variant changes and uses the selected
variant's APK metadata; build the app first if that metadata is unavailable.
The rounded device selector sits beside the query. Use `package:`, `process:`,
and `level:` terms to narrow it further, or clear the query to show all messages.

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
| `package:mine` | Current application selected in this Android project |
| `pid:123 tid:456 uid:10123` | Exact numeric identities |
| `(tag:Network \| tag:Database) & level:ERROR` | Explicit boolean operators and parentheses |
| `NOT tag:Noisy` | Explicit negation |
| `name:"Network errors" level:ERROR` | A label for a saved query |

`process:`, `message:`, and `line:` also support substring, exact, regex, and
negative forms. Regexes use Rust regex syntax; lookaround and backreferences
are unsupported. Quote values containing spaces, parentheses, or boolean operators.
Plain words search the formatted message. The filter's **Aa** button controls
case sensitivity. Invalid queries show an error and preserve the last valid query.
Suggestions appear as you type, using supported fields and observed tags,
applications, and processes. Up/Down choose a suggestion, Enter/Tab accepts it,
and Escape dismisses it. Completion replaces the term at the cursor, preserves
following terms and groups, and handles quoted values and exclusions.
Use **Options → Save current filter** to retain queries across sessions.

Find stays hidden until `Cmd+F` (`Ctrl+F` on Linux/Windows) or the Find button
is invoked. Escape closes Find and returns focus to Logcat. It searches the
displayed messages, highlights matches,
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
current application** operates on the current Android application.

## Android Studio comparison

The reference was Android Studio's public [Logcat implementation](https://github.com/JetBrains/android/tree/master/logcat/src/com/android/tools/idea/logcat),
including its filter parser, device selector, formatting, file I/O, and actions,
See also the [Logcat documentation](https://developer.android.com/studio/debug/logcat).

This implementation covers the main capture, device selection, structured display,
filtering, search, copy, save/load, split-view, pause, restart, and app-termination
workflows. It does not yet provide R8/ProGuard retracing, bugreport ZIP/Firebase
file import, custom column/font presets, or character-level
text selection.

## Validation

Run `cargo test --locked -p android_tools --lib` and
`cargo test --locked -p android_ui --lib android_logcat` with the workspace
dependencies available. Run `./script/clippy --locked -p android_tools -p android_ui`.

For device validation:

1. Open this example, build `demoDebug`, and open Logcat before connecting a device.
2. Connect an emulator and a physical device. Verify names, versions, states,
   independent device selection, disconnect/reconnect, and unauthorized-device feedback.
3. Run the app, filter with `package:mine` and `process:`, then emit each severity and
   a multiline Java/Kotlin exception. Verify PID/TID, UID, tags, colors, wrapping,
   crash/stacktrace queries, and source navigation. Verify a long unwrapped message
   can be read to its end with the shared horizontal scrollbar.
4. Try repeated tag terms, exclusions, parentheses, regexes, case sensitivity,
   `package:mine`, and an invalid expression. Restart the app and check filtering.
5. Pause, generate enough logs to evict entries at 1 MiB, and confirm the snapshot
   stays readable. Resume, clear the view, disconnect/reconnect, and check that
   old cleared messages do not reappear. Close the tab and verify its ADB reader exits.
6. Invoke Find, search with literal and regex text, close Find with Escape,
   copy a selection, export JSONL and Studio
   JSON, and import each into the IDE. Open the Studio JSON in Android Studio too.
7. Open a split Logcat view, use a different source/filter, and check independence.
8. Revoke project trust and verify capture stops. Restore trust and refresh devices.

Native validation covers the Android tooling tests and the Android UI tests,
including bottom-dock tab/split isolation, current-app filtering, preference
migration, completion at the cursor, popup keyboard controls and dismissal,
Find visibility/focus, one-row formatting, independent scroll axes, wrapping,
paused snapshots, clearing, and reconnect behavior. Desktop/emulator visual
validation remains a manual check.
