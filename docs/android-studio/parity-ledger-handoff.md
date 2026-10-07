# Android foundation: reference parity ledger

Continue the reference catalog and parity tooling for `anilbeesetti/koda` in
the Koda cloud environment. Read `AGENTS.md` and the implementation plan first.

The owner worktree is `/workspace/android-studio-parity-ledger`, branch
`android-studio-task/1-parity-ledger`. Commit
`529a1d9453cdb50ed8c0a79fe2b7538aedf020c9` adds Rust
`cargo xtask android-parity`, the immutable source manifest, the canonical
ledger, and `docs/android-studio/ports/parity-ledger-task-report.json`.
All 41 xtask tests, including 21 ledger invariants, clippy, provenance validation
and five scoped reviews passed. Nothing has been merged or pushed.

The seed catalog has eight individually inspected reference methods, still
unported in this standalone tooling branch. The integration-check branch will
map the default-variant implementation with fresh manifest-bound Cargo evidence.
Do not confuse this partial catalog with exhaustive upstream test discovery.

Next, reconcile inherited, parameterized, generated and non-JVM runner discovery
against every pinned source. Keep each newly identified test unported until its
Rust behavior tests run. Deferred features remain unported. Do not claim mirror
completeness: the JetBrains GitHub mirror omits files and canonical comparison
is unresolved. Review and import the selected foundation declarations and
fixture provenance under `/workspace/android-studio-references/samples/`.

Source archives remain external at `/workspace/android-studio-references`.
Coordinate Cargo with other owners, preserve licenses and assertions, and
leave protected branches unchanged. Full-workspace validation is blocked by
disk capacity, and the user holds GitHub writes.
