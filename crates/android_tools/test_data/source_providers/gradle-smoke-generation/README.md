# SDK source provenance regression

This supplemental fixture extends `../gradle-smoke`; that original fixture is unchanged. The adapted `AssetGenerator` retains the Apache 2.0 notice in `app/build.gradle` and its upstream source attribution in `../README.md`. It is not a complete port of `AndroidProjectViewTest.testGeneratedAssets`.

AGP's public `static` provider can omit variant-layer configured directories. The exporter reads `DirectoryEntry.isGenerated` through the same SDK model conversion method, excluding its synthetic JVM bridge. Rust still owns model decoding and IDE behavior. This minimal JVM Gradle bridge is required because these SDK objects exist inside Gradle's isolated JVM. If that capability is unavailable, sync reports an explicit error instead of guessing provenance from a path.

`model.json` was captured with AGP 9.4.0, Gradle 9.6.1, JDK 21, Android platform 37, and build tools 37.0.0. Only the exact isolated project prefix in JSON string values was replaced by `${project_root}`. This is a public runtime smoke; it does not establish the pinned Studio test harness or earn canonical parity credit.

The fixture disables the `inactive` flavor, retains its configured provider, registers generated outputs outside `build`, and adds missing static asset directories both inside and outside `build`. It also exercises variant-only Java, Kotlin, resource, and asset folders. The Rust regression verifies missing-directory retention, generation flags, active source ordering, and disabled-flavor exclusion without requiring an SDK during unit tests.

For live verification, copy the fixture outside the checkout, supply `local.properties` for the installed SDK, and run Gradle with the exporter's init script:

```sh
gradle --no-daemon --console=plain --rerun-tasks \
  --init-script /absolute/path/to/project_model.gradle \
  :app:createAssetsDemoDebug :app:createAssetsDemoRelease kodaAndroidProjectModel
```

Both named producers and both model tasks executed in the fresh validation run. The captured JSON matches the unit fixture after the single documented normalization. Full-app/native and workspace validation remain separate gates.
