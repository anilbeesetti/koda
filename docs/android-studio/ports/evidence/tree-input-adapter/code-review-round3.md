# Tree input adapter: independent CODE review, round 3

Verdict: **PASS — scoped adapter correctness and saved component-check evidence**. The final synthetic fixture correction is source-backed, preserves the positive assertions, and adds a meaningful negative assertion. Production remains identical to the previously reviewed round-2 PASS. No new blocking finding requires another fix.

Reviewed current source hashes:

- Adapter: `d57ba8070321039cdd8b9749bc43df923e694a31c27299a1ec577b5974edfc3b`.
- Supplemental tests: `5e5927568f42cdc1247d88d16f3fdf1dcbb7b427e24b4d6240928378b6e5f79e`.
- Additive export: `f4b033f8aa13b8141f706a1f24455c1d5500940ef5e4adffa46edd8baf1f2891`.

This review read existing evidence and files only. No Cargo, Gradle, GUI, Git, source mutation, or executable test invocation was performed by the reviewer. The external review report is the sole write.

## Correctness of the fixture correction

The retained-host supplemental test now names its explicitly typed fixture provider `testFixtures` (`tests/project_tree_adapter.rs:480`), matching the pinned converter's `startsWith("testFixtures")` classification to `_test_fixtures_` at `ModelCacheV2Impl.kt:471`. The original `fixtures` string would instead be classified as `_unit_test_` by the converter's fallback at `:473`; assigning that provider to the model's explicit fixtures container is an unsupported shape. Existing provider validation correctly rejects it at `project_tree_facts.rs:903–909`. The original retained provider-facts test already uses `testFixtures` at `tests/project_tree_facts.rs:212`.

The positive case still expects exactly main plus the retained host-test Java roots and retains the original all-components-Main assertion (`tests/project_tree_adapter.rs:481–493`). No selected fixture artifact is supplied. Its omission follows the pinned `AndroidModelSourceProviderUtils.kt:76–77`, where fixtures are collected only if the selected variant has a fixture artifact. Device roots remain excluded by the corresponding selected-device-artifact gate at `:71–74`.

After the positive assertions, the test changes only this synthetic provider back to the invalid `fixtures` name and asserts the existing typed UnsupportedShape rejection (`tests/project_tree_adapter.rs:494–514`). It therefore strengthens the boundary coverage rather than accepting invalid metadata. No production workaround, weakened assertion, skipped test, golden replacement, or original fixture edit was introduced.

I independently removed only that added negative block and reversed only the provider-name replacement. The result exactly reconstructs the saved pre-correction test bytes with SHA-256 `98453f194d39b1d7129f35732c76f0718c5b5f072d5904d6004442b942efd047`. All prior positive assertions and all unrelated test code are preserved. The 97 protected original inputs remain SHA-256 identical. Round-1 FAIL and round-2 PASS reports remain byte-identical; fix-round3.json preserves the cumulative three-fix history and actual failure.

## Independently verified saved checks

Each successful receipt's 157-file android_tools source closure matches the current checkout exactly. Each raw log SHA-256 matches its receipt. The recorded commands preserve locked dependencies, unfiltered execution, and strict repository flags.

| Saved check | Verified result | Raw log SHA-256 |
| --- | --- | --- |
| `cargo test --locked -p android_tools --all-features --message-format=json` | Exit 0; 201 passed across six suites; 0 failed, ignored, measured, or filtered | `2d7354a883b7383d13c02a966b974cc9168f2e8492ed0a319d3570cdd854302b` |
| `./script/clippy --locked -p android_tools` | Exit 0; script supplies release, all targets/features, deny warnings | `87398404a7358c62c0b5eb854f7eded8408e098a056133592de17e60a8b16591` |
| `cargo fmt --all -- --check` | Exit 0; empty raw log | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| `cargo build --locked -p android_tools --all-features --message-format=json` | Exit 0; scoped all-feature library build | `89cb259e216d17dda7aaafcdf1cd1cba2a051fa14832d746c8bd29d717d66c5f` |

I independently parsed the successful test log into 85 library, 20 import, 27 adapter, 21 original tree-component, 29 provider-facts, and 19 preference tests. The sum is 201 and matches compiler-source-elf-proof.json; all six summaries are unfiltered with zero ignored/failed.

The preserved prior attempt genuinely failed the newly written retained-host case with “Fixture provider disagrees with the pinned Studio converter”: 26 adapter tests passed and one failed. Its raw log SHA-256 `7f4cd4c112e429f8d4fa6b5695b74fb190ca1539a00d14d5ed0bfef383c589cb` matches the failure receipt. Its source closure differs from current only in the corrected supplemental test file. The earlier capacity failure remains a separate historical failed attempt and is not credited as an executed pass.

All nine recorded compiler-artifact groups, their current emitted-file SHA-256/byte sizes, and their dependency-file SHA-256 values match the saved compiler proof. The library records declare the adapter module in their dependency files and were compiled with `fresh:false` during the first actual retry. Production did not change between that compilation and the successful retry. The adapter test target was freshly rebuilt (`fresh:false`) in the successful retry for the corrected test bytes; its direct dependency file appropriately describes the integration test, while production comes through the checked library. The separately frozen adapter test ELF is present, has ELF magic, and hashes to `766efb129744a5851c2b10e1db1d706ee2a9257b779d60ddb6447c89d6714de6` (6,480,040 bytes), matching the executed target proof. This avoids treating an unrelated shared-target Fresh result as source-bound evidence.

## Scope and remaining gates

The earlier root-precedence, shadowed Unknown capability, and effective module-directory presence fixes retain the reviewed round-2 behavior because the production bytes are unchanged. Stale capture/Java revisions, actual byte offsets, presence contradictions, unsupported roots, provider/resource lookup, and fallback/budget handling remain covered by that source review and the actual component run.

The saved runtime-status.json and review-source-round3.json predate this final fixture correction and retain the earlier historical state/test SHA; they are not used as current pass evidence. Current evidence is the final corrected source plus retry2/check receipts and compiler-source-elf-proof.json. The owner should distinguish those historical snapshots when preparing the final task report.

This pass grants no live UI, original five-case integration, generated-artifact completeness, or full-workspace completion credit. All five original tree methods and ten assertions remain unported/not_run. Current independent review roll-up and the lead's full-app/live/workspace/main/conflict checks remain separate gates. The pure adapter still depends on authoritative host capture/presentation, complete scans, publication lifetimes, generated light-class filtering, and sorted visible folders for subsequent product work.
