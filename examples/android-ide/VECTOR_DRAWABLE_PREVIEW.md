# Vector drawable previews

Open `mobile/src/main/res/drawable/ic_vector_preview.xml` and click the eye in
its editor toolbar. Alt-click the eye to keep the XML and preview side by side.
The command palette also provides **Android: Open Vector Drawable Preview** and
**Android: Open Vector Drawable Preview To The Side**. No Android SDK, project
trust, Gradle sync, or build is needed.

The preview refreshes after unsaved XML edits and external file reloads. Opening
the same preview again reuses its tab. Use **Light** and **Dark** backgrounds to
inspect both dark and light artwork. The refresh button reloads resource values.

Supported features include Android's four hex color formats, root/path alpha,
fill rules, strokes and their cap/join styles, nested group transforms, scoped
clip paths, tint, linear/radial gradients, and trim paths. Android's stroke
scaling and first-contour trim semantics are preserved.

References to colors, dimensions, strings, and integers resolve from the same
source set's default `res/values/*.xml`. Aliases and open buffers' unsaved values
are supported; previews also refresh when those values change. Framework black,
white, and transparent colors work without an SDK.

This is a standalone vector preview in the default, left-to-right configuration.
It does not select device/variant resource qualifiers, merge dependency/source
set overlays, resolve application themes or arbitrary framework resources, or
animate `animated-vector` drawables. Unresolved references, unsupported complex
colors (including sweep gradients), malformed XML, and invalid path data show an
error in the preview instead of leaving stale artwork. Correct the XML and the
preview recovers automatically.

## Reference implementation

Android Studio and IntelliJ's Android plugin use
[`GutterIconFactory`](https://github.com/JetBrains/android/blob/master/android/src/com/android/tools/idea/rendering/GutterIconFactory.java)
to read unsaved XML, resolve resource references, and render vector drawables
with the lightweight
[`VdPreview` / `VdTree`](https://android.googlesource.com/platform/tools/base/+/studio-main/sdk-common/src/main/java/com/android/ide/common/vectordrawable/)
pipeline. Other drawable types use layoutlib. Koda follows the standalone vector
pipeline with a native Rust conversion to its existing SVG renderer, so previewing
vectors does not start Java or require downloading the Compose preview runtime.

## Manual checks

1. Alt-click the sample's eye. Confirm the gradient tile and white K appear.
2. Change `android:rotation` without saving. Confirm the preview rotates.
3. Switch Light/Dark, resize the split, switch tabs, and reopen the preview.
4. Set a path color to `@color/vector_color`; add that color to a sibling
   `res/values/colors.xml`. Confirm saved and unsaved color edits update it.
5. Enter invalid path data or an unknown reference. Confirm an error replaces
   the image, then undo and confirm automatic recovery.
6. Open an SVG and a non-vector XML file. Confirm SVG preview still works and
   ordinary XML has no vector preview button.
