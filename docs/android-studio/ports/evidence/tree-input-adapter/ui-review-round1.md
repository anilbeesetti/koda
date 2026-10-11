# Tree input adapter: independent UI-contract review, round 1

Verdict: **FAIL** — one P2 presentation-contract mismatch.

Scope: read-only review of the pure per-module adapter, its additive export and supplemental integration tests. Bound to `review-source-round1.json`; all three frozen source hashes were verified unchanged. Pinned reference: AOSP `tools/adt/idea` at `a84efec3ba9542d9bfa1255103f0dc94833a3796` (`studio-2026.2.1`). No Cargo, source edits, Git mutation, GUI launch or display activity occurred. This verdict grants no native UI or canonical reference-test parity credit.

## Required finding

**P2 — Preserve built-in source-type precedence before applying the disabled-Kotlin presentation move.**

`crates/android_tools/src/project_tree_adapter.rs:250-252` immediately maps each per-provider Java/Kotlin intersection to `SourceGroup::Java` when Kotlin is disabled. The later cross-provider deduplication at `:319-338` therefore treats these intersections as the highest-priority Java roots.

The pinned `AndroidViewNodeDefaultProvider.kt:169-188` collects and deduplicates the original JAVA, KOTLIN and KOTLIN_AND_JAVA types in built-in order. Only afterward, at `:198-202`, it moves surviving KOTLIN_AND_JAVA roots into JAVA when Kotlin is disabled. `AndroidSourceType.kt:49-67` computes the differences/intersection per provider.

Concrete supported input: active provider `main` reports `/shared` as Kotlin-only; active provider `debug` reports `/shared` as both Java and Kotlin; captured Kotlin capability is Disabled. Upstream JAVA is empty, KOTLIN claims `/shared`, KOTLIN_AND_JAVA loses its duplicate, and the subsequent disabled-Kotlin move has nothing left to move: the visible group remains **kotlin**. This adapter maps the `debug` intersection to Java before deduplication and returns **java** instead. No missing SDK or language-capability fact is involved.

Collect intersections as KotlinAndJava, deduplicate against all built-in priorities, then move surviving common roots into Java for Disabled and normalize final group order. Add a supplemental regression for the cross-provider Kotlin-only/shared collision with explicit Disabled capability. The existing same-provider test at `crates/android_tools/tests/project_tree_adapter.rs:225-268` does not cover that collision. Preserve existing tests and original fixtures.

## Checked contracts without additional findings

- Per-provider Java-only, Kotlin-only and shared membership uses the correct difference/intersection logic for Enabled (`project_tree_adapter.rs:227-280`; `AndroidSourceType.kt:49-67`). Kotlin capability and module display identity remain explicit captured facts, rather than guesses from paths or filenames.
- Supported group labels and generated suffixes delegate to `SourceGroup::label` / `is_generated` and the facts-aware projection (`project_tree.rs:52-68,524-542`): manifests, java, kotlin, kotlin+java, assets, res and ` (generated)` match the pinned group nodes.
- Exact-root deduplication includes AIDL, RenderScript and JNI-library priority without rendering them as supported groups (`project_tree_adapter.rs:302-338`; `AndroidSourceType.kt:163-183`). Unsupported roots are returned explicitly.
- Actual Java bytes, duplicate-name/parser/budget fallback and original UTF-8 declaration offsets feed the existing class/file presentation, preserving real paths (`project_tree_adapter.rs:465-545`; supplemental tests `:553-631,944-1012`). Kotlin bytes cannot be substituted as Java facts.
- Resource annotations delegate to exact resource-root membership and retain physical filenames and individual navigation paths (`project_tree.rs:876-1014`; supplemental tests `:813-887`). Multiple qualified resources retain individual targets while best-resource selection stays deferred. Special files do not acquire fabricated filesystem IDs.
- Source-provider annotations distinguish display labels from reference labels (`project_tree.rs:719-737`; pinned AndroidPsiFileNode / AndroidManifestFileNode). Main-source display annotations remain suppressed while reference labels retain upstream source attribution.

## Scope limits retained

The plan explicitly leaves AndroidSourceTypeNode's provider-sorted visible folder order and the Gradle light-class generated-root filter as pending prerequisites. The current root encounter is only this producer's iterator, and raw generated component roots do not establish complete generated-group visibility. This review does not waive those future requirements. It does not install or inspect a project-panel renderer, icons, resizing, scrolling or focus. All five original tree methods and ten original assertions remain unported/not_run; supplemental tests grant zero canonical parity credit.
