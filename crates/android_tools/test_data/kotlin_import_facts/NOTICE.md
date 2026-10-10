The complete files under `reference/` retain their original JetBrains copyright
headers and Apache-2.0 licenses. They define the reflection boundaries this raw
capture protocol supports. Kotlin/Gradle model building, filtering, fallback,
compiler argument policy and facet creation are not implemented by this slice.

- `KotlinGradleModelBuilder.kt`: JetBrains/intellij-community `b75ab523e6adbe1d26112219729eacbcfd24daa0`, SHA-256 `658a5bb4099cafec6058517737a2434a04f8f88a507510608f9c5ab800e2dcbd`.
- `modelBuilderUtils.kt`: JetBrains/intellij-community `b75ab523e6adbe1d26112219729eacbcfd24daa0`, SHA-256 `7883ba5c53e322ab418b793d65870613a98b60099349d52272c92b52126335af`.
- `KotlinCompilerArgumentsResolverReflection.kt`: JetBrains/intellij-community `b75ab523e6adbe1d26112219729eacbcfd24daa0`, SHA-256 `f7b005592588e012af94228ce823879e07a9c298c782e8928e0500cbe88ff6e7`.
- `KaptModelBuilderService.kt`: JetBrains/intellij-community `b75ab523e6adbe1d26112219729eacbcfd24daa0`, SHA-256 `8b52017bfeecef2f5a917cee66ccf75da8897c22487ab715b9bccf9c78956c88`.

`protocol-template.json` is synthetic supplemental protocol data. Its classes,
loaders, task names, artifact bytes/digests, versions and observations do not
represent an executed Gradle build or an original Android Studio test. The Rust
tests use it to reject malformed, rebound or stale packets and preserve raw
states. No original Kotlin/facet/KAPT/StringHelper test receives parity credit.

The existing P2a import fixture is reused byte for byte by the Rust tests; its
original attribution remains in `../import_facts/NOTICE`. The selected
reference pins are supplementary Community sources, not an exhaustive Android
Studio reference census.

The copied files use the repository [Apache-2.0 license](../../../../LICENSE-APACHE).
