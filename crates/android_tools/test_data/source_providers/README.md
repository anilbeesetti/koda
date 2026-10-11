# Evaluated source-provider smoke

`gradle-smoke` is a live model-export fixture, not a port of an Android Studio
integration test. The evaluated names, custom resource and manifest directories,
overlapping `main`/`debug` asset directories, and task-generated assets exercise
the additive Rust source-provider catalog. It uses AGP 9.4.0, Gradle 9.6.1,
JDK 21, Android platform 37 and build tools 37.0.0; these versions are not the
pinned Android Studio test harness.

Copy the fixture outside the checkout, supply the local Android SDK and invoke
Gradle 9.6.1 with the checkout's `src/project_model.gradle` as an init script:

```sh
gradle --no-daemon --console=plain --rerun-tasks \
  --init-script /absolute/path/to/project_model.gradle \
  :app:createAssets kodaAndroidProjectModel
```

The producer is requested explicitly. The model collector also retains the
source provider's Gradle producer dependencies, without requesting an APK build.
Decode the complete Gradle output with the Rust `project_model_probe` example:

```sh
cargo run --locked -p android_tools --example project_model_probe -- \
  /absolute/path/to/copied-fixture /absolute/path/to/gradle-output.log --model-only
```

The catalog reports Gradle's evaluated source-set container iteration order. It
does not identify active providers, reproduce Android Studio's overlay order, or
claim that configured roots exist in the virtual filesystem. Legacy/KMP catalog
absence remains unknown; an empty known catalog is a distinct value. Missing
directories are retained and overlapping provider candidates remain separate.
`Resources` denotes Android `res`, not JVM classpath resources. Generatedness
uses evaluated component `all`/`static` providers and configured asset roots;
it is not a provider-name guess or a claim that directory contents were rebuilt.

The `AssetGenerator` registration is adapted from AOSP
`android/navigator/testSrc/com/android/tools/idea/navigator/AndroidProjectViewTest.java`,
`testGeneratedAssets` (lines 186–243), at immutable idea revision
`a84efec3ba9542d9bfa1255103f0dc94833a3796`. The adapted task writes a nested asset
directly, while the original test writes `foo.txt` and adds the nested asset
after model sync. This fixture does not reproduce that test's complete project
rule, model assertion, refresh, and tree assertion. The original Apache 2.0
source/license notice is retained externally in the reference archive and source
extract, with its SHA-256 recorded in
`docs/android-studio/ports/source-providers.json`. See the repository's Apache
license text in `test_data/default_variants/LICENSE-APACHE`.

The task report identifies the frozen exporter, input files, actual named producer
execution, raw model, generated file, and validation evidence already captured.
Rust decoding, compilation, tests, and live-app checks are separate gates; consult
their recorded status and artifacts in the report. No canonical reference-test
status is changed by this smoke.
