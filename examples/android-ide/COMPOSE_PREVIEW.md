# Compose previews

Run `cargo run --locked -p zed --bin koda`, open a Kotlin file, and choose
**Compose preview**. Cargo automatically fetches checksum-pinned preview build
inputs on the first build. The renderer, native layoutlib, compiled bridge, and
Eclipse Temurin Java 21 runtime are embedded in the executable and included in
packaged apps. Previews require no installer, launcher, Java override, or runtime
download. The app unpacks its bundle into a versioned application cache on first
use; subsequent refreshes reuse it. Missing or truncated cached files are repaired
from the app's copy, and concurrent app instances share an extraction lock. The
gallery appears in a separate pane beside the editor. It follows the active
Kotlin file and displays every discovered annotation, including multipreview
annotations and each value from a preview parameter provider.

The preview surface uses collapsible headers for each composable function and
compact variant titles with per-preview menus. Device sizes stay proportional at
one scale, and variants wrap into rows when the pane resizes. A blue border marks
the selected preview. Refresh preserves selection, collapsed groups, and the
visible composable. Only visible rows create image views.

The toolbar's **Build and refresh** button rebuilds manually. The lightning
button toggles **Auto refresh**, which refreshes after edits stop for 700 ms.
Unsaved Kotlin buffers are compiled from temporary copies; the editor does not
save them. Changes during a build coalesce into one subsequent refresh. **Stop**
cancels the current request. Inactive preview tabs pause automatic work.
Switching files, projects, or variants cancels the previous request. The toolbar
shows whether previews are up to date, refreshing, or failed.

Click a preview or select the eye button to show layout outlines. Hover
highlights the deepest component with a project source location. Click it again
to open its source line in the code pane. Each preview's three-dot menu provides
keyboard access to component sources and render diagnostics. **Choose a preview**
(the tree button) reveals any variant, including those in collapsed groups.
Navigation is disabled while the displayed render is out of date.

The controls at the bottom right provide drag-to-pan, zoom in/out, **1:1**, and
**Fit**. Fit adjusts the common scale as the pane changes size; manual zoom keeps
the chosen scale and wraps the gallery instead. Scrollbars accommodate previews
larger than the pane. A failing composable keeps its scrollable diagnostic beside
successful previews; its menu can copy the full render error.

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
| `PreviewDesignSurface.kt` | Display multiple scenes with selection and zoom | Virtualized grouped gallery, selection, zoom and panning, individual diagnostics |
| [`SurfaceLayoutManagerOption.kt`](https://github.com/JetBrains/android/blob/master/preview-designer/src/com/android/tools/idea/preview/modes/SurfaceLayoutManagerOption.kt), [`GridLayoutManager.kt`](https://github.com/JetBrains/android/blob/master/designer/src/com/android/tools/idea/uibuilder/layout/option/GridLayoutManager.kt), [`GridLayoutGroup.kt`](https://github.com/JetBrains/android/blob/master/designer/src/com/android/tools/idea/uibuilder/layout/positionable/GridLayoutGroup.kt), [`SceneViewHeader.kt`](https://github.com/JetBrains/android/blob/master/designer/src/com/android/tools/idea/common/surface/organization/SceneViewHeader.kt) | Group by composable, pack proportionally scaled views into wrapping rows, and label each variant | Function headers, collapsible groups, adaptive row packing and compact variant menus |
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
version 2 is packaged together with the renderer. The build uses a pinned Eclipse
compiler and a host Java runtime, so bridge compilation also needs no local JDK.
Cross-compiles select separate host and target Java distributions. Pinned inputs
are cached under Cargo's home (or `KODA_COMPOSE_PREVIEW_ARTIFACT_CACHE`) and checked
against `crates/android_tools/preview-bundle.json`; binaries carry no build-machine
paths. Bundled license notices and Java's `legal` directory are preserved.
For offline builds, keep the artifact cache and set `CARGO_NET_OFFLINE=true`.
Build scripts cannot see Cargo's `--offline` flag; this environment setting also
prevents preview artifact downloads and fails immediately if an input is missing.

Only visible gallery rows create UI elements. Duplicate layout rectangles are
collapsed once per render. Image assets are released when the gallery changes or
closes. The decoded gallery is limited to 256 MiB; dimensions and hierarchy sizes
are bounded. Source resolution reads only filenames present in the render metadata.

## Native UI captures

On Linux with a working headless Vulkan adapter, run
`cargo test --locked -p android_ui capture_compose_preview_surface -- --ignored --nocapture`.
This renders the actual GPUI preview pane beside an editor and saves wide,
narrow, and skeleton-inspection screenshots to `target/compose-preview-visuals`.
`COMPOSE_PREVIEW_VISUAL_OUTPUT` changes the output directory.
`COMPOSE_PREVIEW_VISUAL_FIXTURE` can point to a renderer output directory containing
`previews.json`, `results.json`, and the corresponding PNGs to capture real
Compose images instead of the default sizing fixture. The normal interaction
suite runs without a GPU and covers resizing, collapse, keyboard navigation,
panning, source navigation in a second column, rebuild continuity, and gallery
virtualization.

## Compatibility boundaries

This implements the file gallery, rebuild, automatic refresh, skeleton inspection,
and source navigation workflow. It does not implement Android Studio's animation
timeline, interactive device input, UI Check mode, or its in-process Fast Preview
compiler. Refresh uses incremental Gradle builds and a renderer JVM, so build
latency depends on the project and is longer than Studio's Fast Preview path.
Preview configuration support follows the pinned standalone renderer; it does not
offer every Studio display mode (for example, system-bar decorations).

Bundled previews support Apple Silicon and Intel macOS, Linux x86-64, and Windows
x86-64, matching Google's available native layoutlib distributions. Android
projects still need their normal SDK and Gradle-compatible project JDK. Navigation requires Compose
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
SDK 37, and a preview runtime extracted from the app's application cache:

```sh
fixture="$(mktemp -d)"
mkdir "$fixture/project"
tar --exclude=build --exclude=.gradle --exclude=.kotlin --exclude=.zed \
  --exclude=local.properties --exclude=workspace.json \
  -C examples/android-ide -cf - . | tar -C "$fixture/project" -xf -
script/test-android-preview --project "$fixture/project" \
  --installation /absolute/path/to/compose-preview \
  --output "$fixture/report"
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

Rust checks are `cargo test -p android_tools --features bundled-preview`, `cargo test -p android_ui`, and
`./script/clippy -p android_tools -p android_ui`. Editor tests cover scaled overlay
clicks, navigation, request coalescing, variant cancellation, ignored generated
outputs, and preserving unrelated tabs and unsaved edits when closing previews.
