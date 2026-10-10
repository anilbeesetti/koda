# Exact source-specific Android reference results

Forty named reference tests pass in [GitHub run 38033202999](https://github.com/anilbeesetti/koda/actions/runs/38033202999): 36 on source `594bf18526b3d7197315c82f5c6c3860cfb7bf5a` and four constructor adaptations on source `0d155a5fb869df3f10837db92e252e946687f7ae`. Each command selected exactly one test, passed once, failed zero times and ignored zero tests.

This task merges the existing table implementation with the reference runner and adds parity evidence. The integration combines inherited Rust code and tests; the authored reconciliation changes only evidence and ledger files.

These results cover those two tested revisions. The combined evidence task tree and Source17 have no named execution credit from this run. The active combined ledger has zero passing runs, 40 blocked mappings and 61 unexecuted rows. Its 40 targets have `run: null` and a concrete blocker: the current implementation/dependency snapshot differs from both executed revisions. The unchanged parity validator requires a matching whole snapshot. Build, full-suite, strict Clippy, matching named tests, native UI/UX/performance and final merge gates remain pending for the combined tree.

The [evidence document](../../ports/current40-reference-evidence.json) records each exact command, tested manifest/source/fixture hashes, compiler artifact identity, full executable digest, case stdout/stderr digest and immutable GitHub run, job and artifact IDs. It also records the current static ledger mapping separately from actual tested inputs. The merged `android_tools` manifest includes the table integration target; its static hash differs from the primary source's tested manifest. The separate source-specific evidence records the executed revision. The active combined ledger keeps `run: null` until its own whole snapshot is tested.

The matrix has 101 catalogued rows: 20 ported, 20 adapted, 61 unported and zero not applicable. Four constructor cases move from unported to adapted because explicit Rust inputs replace IntelliJ modules and Gradle builders while preserving all 19 original outcomes. `withAbi` stays unported under the approved NDK deferral. Remaining cases and the exhaustive upstream census are unfinished.

The exact pre-execution ledger and constructor plan are retained as `.json.raw` files. Every previous passing run claim and existing historical log is preserved in that history; no source-specific pass is selected as a combined run. Eight stale static `project_model.rs` whole-file bindings now point to the source whose identical named declarations were tested on source594. Rust declarations and assertions are unchanged; the current combined run remains blocked.

Each source folder contains the unmodified uploaded receipt, worker log and small per-case stdout/stderr logs. `.json.raw` files preserve original JSON bytes and hashes rather than formatter output. The selected compiler JSON identity records are reproduced in the evidence document. Full compiler and job logs are identified by whole-file hashes and GitHub artifact/job IDs. Original GitHub artifact retention is seven days. The repository retains the small evidence after artifact expiry.

The independent audit verifies ZIP sizes/hashes/CRCs, safe unique regular-file paths, all case summaries, compiler/runtime identities, source before/after records, 78 source bindings and declaration hashes. ELF bytes are absent from the uploads; their recorded full digests cannot be independently rehashed from these artifacts.

Retained AOSP/IntelliJ source, helper, fixtures, Apache licenses and notices remain unchanged. Constructor fixture JSON documents inline input transformations; the Rust tests construct those inputs directly.
