# Compose previews

Run `cargo run --locked -p zed --bin koda`, open a Kotlin file, and choose
**Compose preview**. On first use, Koda downloads the checksum-pinned renderer,
resources, and native layoutlib for this platform. Cargo builds and packaged apps
contain only the small bridge source and release manifest. Select Java in **Android Setup**:
use an existing full JDK 21 or download the managed Temurin JDK 21 into application
storage. Preview uses that selection without a shell-exported Java path and compiles
its bridge once during installation. Subsequent refreshes reuse the verified cache,
including offline. Missing or corrupt files are repaired from verified downloads;
uncached libraries require a connection. The toolbar reports installation progress,
and **Stop** cancels downloads and compilation. Completed downloads survive retry.
Concurrent app instances share an installation lock. The
gallery appears beside the code inside the selected Kotlin source tab, sharing
that tab's navigation and file identity. It displays every discovered annotation,
including multipreview annotations and each value from a preview parameter
provider. Drag the divider to resize the code and preview areas; double-click it
to restore equal widths. Switching tabs preserves each file's gallery, selection,
zoom and collapsed groups. Closing a source tab releases its preview.

The preview surface uses collapsible headers for each composable function and
compact variant titles with per-preview menus. Device sizes stay proportional at
one scale, and variants wrap into rows when the pane resizes. A blue border marks
the selected preview. Refresh preserves selection, collapsed groups, and the
visible composable. Only visible rows create image views.

The toolbar's **Build and refresh** button rebuilds manually. Previews always
refresh automatically after edits stop for 700 ms.
Unsaved Kotlin buffers are compiled from temporary copies; the editor does not
save them. Changes during a build coalesce into one subsequent refresh. **Stop**
cancels the current request. Inactive source tabs and panes hidden by zoom pause
automatic work, cancel running requests, and unload decoded images while keeping
their gallery state.
Switching files, projects, or variants cancels the previous request. The toolbar
shows whether previews are up to date, refreshing, or failed.

Render output, Gradle exporter scripts, and unsaved source copies live in isolated
temporary directories under the selected Koda data profile's
`android-tools/compose-preview/renders` directory, beside the downloaded preview runtime.
Stable, Nightly and custom `--user-data-dir` profiles have separate caches.
Each request gets its own directory, so projects and app instances cannot overwrite
one another's inputs. The directory is removed when its render fails or is
cancelled, or when the resulting gallery is replaced or closed. Previews no longer
create `.koda/android-preview` in the project. Older project-local preview
directories are unused; normal Gradle build outputs remain in the project.

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
| [`SourceCodeEditorProvider.kt`](https://github.com/JetBrains/android/blob/master/designer/src/com/android/tools/idea/uibuilder/editor/multirepresentation/sourcecode/SourceCodeEditorProvider.kt), [`TextEditorWithMultiRepresentationPreview.kt`](https://github.com/JetBrains/android/blob/master/designer/src/com/android/tools/idea/uibuilder/editor/multirepresentation/TextEditorWithMultiRepresentationPreview.kt), [`SplitEditor.kt`](https://github.com/JetBrains/android/blob/master/designer/src/com/android/tools/idea/common/editor/SplitEditor.kt) | Wrap code and design as one file editor, retaining the source tab and navigation identity | An editor addon wraps the source editor content with a resizable gallery; no separate preview workspace item or pane |
| `AnnotationFilePreviewElementFinder.kt` | Find and expand previews belonging to a file | Google's bytecode `PreviewMethodFinder`; ASM `SourceFile` and package metadata identify the file even with `@file:JvmName` |
| `ComposePreviewRepresentation.kt`, `ComposePreviewRefreshRequest.kt` | Maintain per-file models, serialize and coalesce refreshes, react to visibility | One preview view per source editor, a debounced request queue, request revisions, cancellable processes, stale-result rejection |
| `PreviewDesignSurface.kt` | Display multiple scenes with selection and zoom | Virtualized grouped gallery, selection, zoom and panning, individual diagnostics |
| [`SurfaceLayoutManagerOption.kt`](https://github.com/JetBrains/android/blob/master/preview-designer/src/com/android/tools/idea/preview/modes/SurfaceLayoutManagerOption.kt), [`GridLayoutManager.kt`](https://github.com/JetBrains/android/blob/master/designer/src/com/android/tools/idea/uibuilder/layout/option/GridLayoutManager.kt), [`GridLayoutGroup.kt`](https://github.com/JetBrains/android/blob/master/designer/src/com/android/tools/idea/uibuilder/layout/positionable/GridLayoutGroup.kt), [`SceneViewHeader.kt`](https://github.com/JetBrains/android/blob/master/designer/src/com/android/tools/idea/common/surface/organization/SceneViewHeader.kt) | Group by composable, pack proportionally scaled views into wrapping rows, and label each variant | Function headers, collapsible groups, adaptive row packing and compact variant menus |
| `ComposeViewInfoParser.kt`, `ComposeViewInfo.kt` | Read `ComposeViewAdapter.getViewInfos*` before disposing the scene | Java bridge reflects the adapter into bounds, hierarchy depth, filename, package hash, and source line |
| `PreviewNavigation.kt`, `SourceLocationWithVirtualFile.kt` | Resolve source locations and choose the deepest hit | Index relevant project filenames by Compose's UTF-16 package hash; deepest hit then descending source line; open the editor at that line |

Studio's [`RenderResult.java`](https://github.com/JetBrains/android/blob/master/rendering/src/com/android/tools/rendering/RenderResult.java)
holds the rendered image in a disposable in-memory image pool.
[`ClassBinaryCacheManager.kt`](https://github.com/JetBrains/android/blob/master/rendering/src/com/android/tools/rendering/classloading/ClassBinaryCacheManager.kt)
uses a bounded in-memory class cache, and
[`FastPreviewManager.kt`](https://github.com/JetBrains/android/blob/master/android/src/com/android/tools/idea/editors/fast/FastPreviewManager.kt)
creates compilation overlays with `Files.createTempDirectory("overlay")`.
There is no project-local preview-image cache in this path. Koda follows the
temporary request lifecycle while keeping its standalone renderer's disk protocol
files inside the application cache.

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
version 2 is tested together with the renderer. The selected JDK 21 compiles the
bridge during first use; building Koda needs no preview libraries or local JDK.
`crates/android_tools/preview-manifest.json` pins URLs, sizes, and SHA-256 checksums.
Update that manifest and validate rendering before shipping a new Koda release;
the app never fetches an untested "latest" library version.

Cache identities include the manifest, bridge source, and host platform. Each
release selects only its own complete, verified generation, publishing a new
selector atomically after extraction and compilation succeed. Updating Koda
downloads new libraries on next preview use, reusing unchanged artifact downloads.
If the required version is unavailable offline, connect and retry **Build and refresh**;
an incompatible older generation is never substituted. Older generations are
retained for rollback and other running Koda versions. Reveal managed storage from
Android Setup, then close all Koda windows before manually removing unused generations;
the existing managed storage budget bounds growth. Google tooling
license notices are preserved; managed Java retains its own legal files.

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
suite runs without a GPU and covers embedded divider resizing, control alignment,
tab switching and moving, tab-close cleanup, collapse, keyboard navigation,
panning, source navigation, rebuild continuity, and gallery virtualization.

The full Koda application was also built and run on Linux X11 with a software
Vulkan adapter. Live checks exercised both sample source tabs, the bundled
renderer, ten-card discovery, manual refresh, unsaved automatic rebuilds and
restoration, skeleton selection, source cursor navigation, zoom controls, and
divider dragging. These full-app captures are separate from the opt-in native
surface test.

Live cache checks confirmed that saved and unsaved renders use the application
cache, source files stay unchanged, replacing a gallery removes its old request
directory, and closing previews empties the render cache without creating a
project-local preview directory.

## Compatibility boundaries

This implements the file gallery, rebuild, automatic refresh, skeleton inspection,
and source navigation workflow. It does not implement Android Studio's animation
timeline, interactive device input, UI Check mode, or its in-process Fast Preview
compiler. Refresh uses incremental Gradle builds and a renderer JVM, so build
latency depends on the project and is longer than Studio's Fast Preview path.
Preview configuration support follows the pinned standalone renderer; it does not
offer every Studio display mode (for example, system-bar decorations).

Previews support Apple Silicon and Intel macOS, Linux x86-64, and Windows
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
SDK 37, and a preview runtime installed in the app's application cache:

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

Rust checks are `cargo test -p android_tools`, `cargo test -p android_ui`, and
`./script/clippy -p android_tools -p android_ui`. Editor tests cover scaled overlay
clicks, navigation, request coalescing, variant cancellation, ignored generated
outputs, and preserving unrelated tabs and unsaved edits when closing previews.

To validate the real pinned libraries and first-use bridge compilation without an
Android project, run
`PREVIEW_TEST_JAVA=/absolute/path/to/jdk21/bin/java cargo test --locked -p android_tools real_preview_libraries_compile_and_run_the_bridge -- --ignored --nocapture`.
This downloads the libraries into isolated temporary storage, exercises discovery,
and verifies that the installed generation is reused without network work.
