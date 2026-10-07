# Tree input adapter: independent performance review, round 3

Verdict: **PASS for the pure per-module adapter, with actual component-test evidence.** No additional blocking performance finding or product fix is required. This is the actual-evidence addendum to the preserved round 2 static PASS, not an application-performance verdict.

## Source and executable binding

Reviewed `/workspace/android-studio-tree-input-adapter`; production remains SHA-256 `d57ba8070321039cdd8b9749bc43df923e694a31c27299a1ec577b5974edfc3b`, export `f4b033f8aa13b8141f706a1f24455c1d5500940ef5e4adffa46edd8baf1f2891`, current supplemental tests `5e5927568f42cdc1247d88d16f3fdf1dcbb7b427e24b4d6240928378b6e5f79e`. All 97 original guarded paths were independently rehashed and remain unchanged. The source/test fix history remains at three rounds; the latest synthetic fixture-name correction and negative validation assertion leave production unchanged.

`compiler-source-elf-proof.json` and the raw Cargo JSON bind the worktree package/source paths to the actual artifacts. The production library and unit-test executable were compiled fresh in retry 1; retry 2 reused that unchanged production artifact and freshly compiled the corrected adapter integration test. Retry 2 ran the entire component suite: **201 passed, 0 failed, 0 ignored, 0 filtered**, including all 27 adapter tests. No exact stress run substitutes for that unfiltered gate. The build, repository Clippy and formatting receipts also have exit code 0 and unchanged source; their receipt/log hashes match `actual-checks-summary.json`.

The frozen adapter executable is `/tmp/tree-input-adapter-verified-binaries/project_tree_adapter-tests27`, SHA-256 `766efb129744a5851c2b10e1db1d706ee2a9257b779d60ddb6447c89d6714de6`. Its current bytes independently match the compiler receipt and both measurement receipts. The actual integration executable is 6,480,040 bytes.

## Actual measured cases

| Exact test case | Test PID | Wall time | Process peak RSS | Result |
| --- | ---: | ---: | ---: | --- |
| 20,000 flat asset files: real targets and unique stable keys | 251838 | 0.5487 s | 27,176 KiB | 1 passed, 0 failed, 0 ignored |
| Aggregate Java budget: four eligible parses plus one fallback | 251840 | 0.1455 s | 20,072 KiB | 1 passed, 0 failed, 0 ignored |

Both logs independently show one exact test executed and 26 other cases filtered solely for measurement. Their raw-byte SHA-256 values match their respective JSON receipts. These are debug integration-test process measurements, including process/test-harness overhead, fixture preparation, projection and assertions; they are not isolated function timers or allocator accounting.

`measure-exact-test.py` launches the exact executable directly with `os.posix_spawn`, redirects stdout/stderr to a newly created log, then obtains resource usage with `os.wait4` for that specific PID. It checks the returned PID/status, records process user/system time and Linux peak RSS in KiB, and verifies source hashes before/after. This avoids the earlier aggregate `RUSAGE_CHILDREN` receipt's potential Git-helper baseline. Earlier aggregate receipts and unavailable `/usr/bin/time` attempts remain preserved; neither is used for the peak-RSS values above. The unavailable timed attempts launched neither test.

Precision: the measurement script records the ELF hash **before** spawning and source hashes before/after; it does not record a separate post-run ELF hash. This reviewer independently rehashed the frozen ELF after reading both measurements and confirmed the same hash. The report therefore does not describe the runner as checking ELF hashes before and after each run. The measurement script itself hashes to `46ba260b79aaf7a45b9243ab626e8e5f8e15f4c4b4f71f981b7bb08d1953f7e4`.

## Interpretation and limits

These observations support the static assessment: a flat 20,000-entry inventory is processed without a pairwise inventory loop or per-file provider-index rebuild; the Java work cap produces a preserved ordinary-file fallback and retains the unaffected declarations. The parser limit remains 8 MiB per eligible file and 32 MiB of accounted parse input per module.

The Java fixture uses **one shared 8 MiB `Arc<[u8]>` buffer referenced by five files**. Four inputs consume the 32 MiB work allowance and the fifth falls back. Its observed RSS is not evidence that independently retained source buffers, declaration-heavy Java inputs or a whole Android project fit within that amount. Likewise, the flat inventory case does not measure deep ancestry, overlapping provider/root occurrences, resource qualifier grouping or many modules. Both are single runs with no statistical variability or cold/warm comparison established. Existing repeated root-subtree traversal and owned logical-node/path storage retain the scope limits from round 2.

No full-app startup, UI latency, native rendering, scan/publication performance, project memory ceiling or complete generated-group behavior is claimed. Later host integration still needs background execution/cancellation/stale publication and representative project measurements. The five original tree methods and ten original assertions remain **unported/not_run**; these supplemental tests grant zero canonical reference-test credit.

This reviewer launched no Cargo, Gradle, benchmark, test executable or GUI, and mutated no source or Git state. Only evidence files were inspected and this external report written. The round 2 report remains byte-identical.
