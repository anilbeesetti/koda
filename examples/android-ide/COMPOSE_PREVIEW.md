# Compose previews

Install or update the renderer with `script/install-android-preview`, relaunch
`script/android-ide`, open a Kotlin file, and choose **Compose preview**. The
gallery appears in a separate pane beside the editor. It follows the active
Kotlin file and displays every discovered annotation, including multipreview
annotations and each value from a preview parameter provider.

**Build & Refresh** rebuilds manually. **Auto** refreshes after edits stop for
700 ms. Unsaved Kotlin buffers are compiled from temporary copies; the editor
does not save them. Changes during a build coalesce into one subsequent refresh.
**Stop** cancels the current request. Inactive preview tabs pause automatic work.
Switching files, projects, or variants cancels the previous request.

Click a preview or select **Inspect** to show layout outlines. Hover highlights
the deepest component with a project source location. Click it again to open its
source line in the code pane. The **Components** menu provides keyboard access to
source locations. The **Previews** menu scrolls to any card using the keyboard.
Navigation is disabled while the displayed render is out of date.
**Fit**, **−**, **+**, and scrollbars accommodate larger previews. A failing
composable displays its diagnostic without hiding successful previews.

## How Android Studio implements this

Android Studio and the IntelliJ Android plugin share the `compose-designer`
implementation. The relevant sources are available in
[AOSP](https://android.googlesource.com/platform/tools/adt/idea/+/refs/heads/mirror-goog-studio-main/compose-designer/src/com/android/tools/idea/compose/preview/)
and [JetBrains/android](https://github.com/JetBrains/android/tree/master/compose-designer/src/com/android/tools/idea/compose/preview).
The investigation focused on these responsibilities:

| Upstream source | Responsibility | Koda implementation |
| --- | --- | --- |
| `AnnotationFilePreviewElementFinder.kt` | Find and expand previews belonging to a file | Google's bytecode `PreviewMethodFinder`; ASM `SourceFile` and package metadata identify the file even with `@file:JvmName` |
| `ComposePreviewRepresentation.kt`, `ComposePreviewRefreshRequest.kt` | Maintain per-file models, serialize and coalesce refreshes, react to visibility | One preview view, a debounced request queue, request revisions, cancellable processes, stale-result rejection |
| `PreviewDesignSurface.kt` | Display multiple scenes with selection and zoom | Virtualized gallery, zoom and scrolling, individual diagnostics |
| `ComposeViewInfoParser.kt`, `ComposeViewInfo.kt` | Read `ComposeViewAdapter.getViewInfos*` before disposing the scene | Java bridge reflects the adapter into bounds, hierarchy depth, filename, package hash, and source line |
| `PreviewNavigation.kt`, `SourceLocationWithVirtualFile.kt` | Resolve source locations and choose the deepest hit | Index relevant project filenames by Compose's UTF-16 package hash; deepest hit then descending source line; open the editor at that line |

Koda uses the checksum-pinned
[Google standalone renderer](https://android.googlesource.com/platform/tools/base/+/refs/heads/mirror-goog-studio-main/standalone-render/lib/src/com/android/tools/render/)
and layoutlib rather than loading Android Studio's Swing editor and PSI services
into the Rust application. One JVM renders the entire file. Google's configuration,
device masking, annotation expansion, parameter providers and diagnostics are
retained. The bridge copies the Compose hierarchy before each render scene is
disposed; the original PNG-only CLI discards it.

Gradle exports evaluated class outputs and runtime artifacts, including project
library previews and the exact source inputs of executed compile tasks. This
excludes inactive source variants when resolving component locations.
Unsaved sources replace files in the Kotlin
compile task's source collection without modifying source files or build scripts.
The collection's roots are copied before replacement, retaining generated-source
task dependencies. Membership checks run lazily during file-tree traversal so
Gradle does not resolve generated directories before their producing tasks finish.
Older Kotlin plugins append in `setSource`, so the bridge uses
the underlying configurable collection's `setFrom` instead. Unsupported source
collection APIs produce an actionable build error.

The alpha15 renderer reads only known JSON fields; source metadata stays outside
its screenshot objects. Its relocated coroutine classes require the regular
service loader, so the bridge disables the coroutine fast loader. Bridge protocol
version 2 makes an older installation fail with an update instruction; rerunning
the installer upgrades an intact managed installation without redownloading it.

Only visible gallery rows create UI elements. Duplicate layout rectangles are
collapsed once per render. Image assets are released when the gallery changes or
closes. The decoded gallery is limited to 256 MiB; dimensions and hierarchy sizes
are bounded. Source resolution reads only filenames present in the render metadata.

## Compatibility boundaries

This implements the file gallery, rebuild, automatic refresh, skeleton inspection,
and source navigation workflow. It does not implement Android Studio's animation
timeline, interactive device input, UI Check mode, or its in-process Fast Preview
compiler. Refresh uses incremental Gradle builds and a renderer JVM, so build
latency depends on the project and is longer than Studio's Fast Preview path.
Preview configuration support follows the pinned standalone renderer; it does not
offer every Studio display mode (for example, system-bar decorations).

The installer currently supports Apple Silicon macOS. Integration checks also run
with matching Linux layoutlib runtime artifacts. Navigation requires Compose
compiler source information and project sources; framework and unavailable library
sources are outlined but cannot be opened. Ambiguous source locations are skipped.
Unsaved source overlays currently cover Kotlin; save resource or Java edits before
refreshing. The bridge is compiled against alpha15's APIs and must be updated
together with the renderer when changing that pin.

## Validation

`PreviewGallery.kt` exercises a custom file facade, private previews, repeated and
multipreview annotations, two parameter values, font scaling, and an intentional
render failure. It should show ten cards, nine successful and one diagnostic.

Use an isolated copy of the sample, a JDK accepted by its Gradle version, Android
SDK 37, and the installed preview tools:

```sh
fixture="$(mktemp -d)"
mkdir "$fixture/project"
tar --exclude=build --exclude=.gradle --exclude=.kotlin --exclude=.zed \
  --exclude=local.properties --exclude=workspace.json \
  -C examples/android-ide -cf - . | tar -C "$fixture/project" -xf -
script/test-android-preview --project "$fixture/project" \
  --installation /absolute/path/to/compose-preview \
  --java-home /absolute/path/to/jdk-21 --output "$fixture/report"
```

The probe assembles and renders the saved file, renders an unsaved overlay, and
builds the saved file again. It checks annotation and parameter expansion, failure
isolation, PNG output, source metadata, changed pixels for the unsaved edit, and
restoration of the original render. It verifies the original source bytes remain
unchanged. The report retains models, manifests, PNGs, command logs and durations.
These durations describe the command-line renderer, not editor GPU latency.

The real renderer probe has been exercised with AGP 8.9.1 / Kotlin 2.1.20 /
Compose 1.7.0 / Gradle 8.11.1 and the current sample's AGP 9.4.0 / Kotlin 2.2.10 /
Compose BOM 2026.02.01 / Gradle 9.6.1. Separate Compose compiler fixtures verify
multifile Kotlin facades, overloaded previews and coroutine rendering.

Rust checks are `cargo test -p android_tools`, `cargo test -p android_ui`, and
`./script/clippy -p android_tools -p android_ui`. Editor tests cover scaled overlay
clicks, navigation, request coalescing, variant cancellation, ignored generated
outputs, and preserving unrelated tabs and unsaved edits when closing previews.
