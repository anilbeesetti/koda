# Parallel-sync eligibility policy: verified component

Source commit `63c88dcf0eb7364521c5ea79b8cd67e63e4990b0` adds a pure Rust policy
for the pinned `SUPPORTS_PARALLEL_SYNC` feature. It evaluates captured producer
versions and the exact AGP intervals `[7.2.0, 7.3.0-alpha01)` and
`[7.3.0-alpha04, infinity)`, preserving signed model ordering, historical preview
spellings and typed missing, malformed and unsupported facts. The function does
not schedule Gradle work or publish a UI capability.

The ten added Rust tests include the complete original
`ModelVersionsTest.checkSupportsParallelSync` method and nine supplemental
boundary groups. All nine original AGP assertions retain their order and named
messages, both `Int.MIN_VALUE` components and the empty model description. The
original unused null minimum consumer is represented by an unavailable captured
field and is not consulted. Complete original fixtures and Apache 2.0
attribution remain byte-identical.

All five independent code, UI, UX, performance and quality reviewers passed
static and current-source owner runtime review, with no blocking findings or
policy product fix rounds.

| Actual bounded check                                   | Result                                                                                      |
| ------------------------------------------------------ | ------------------------------------------------------------------------------------------- |
| Normal locked `android_tools` tests, all features      | 241 passed; 0 failed, ignored or filtered; ten new policy tests                             |
| Current-worktree compiler proof                        | Eight fresh test artifacts with exact source paths and at-execution hashes                  |
| Normal locked all-features library build               | Passed with a fresh current-worktree compiler artifact                                      |
| Repository `./script/clippy --locked -p android_tools` | Passed; release, all targets and features, warnings denied                                  |
| `cargo fmt --all -- --check`                           | Passed                                                                                      |
| Exact original-method owner capture                    | One passed, nine deliberately filtered, zero ignored; all nine original assertions executed |
| Source and original preservation                       | All 4,717 whole-source bindings, Git index/status and 148 rebounded originals preserved     |
| Process cleanup                                        | All bounded check processes exited; exclusive execution lease released to Root              |

The fixed seven lead source-guard patterns include all nested port evidence,
fixtures, code and assets. Ten before/after snapshots are byte-identical; their
individual observations refer to one exact portable copy without rewriting the
original external receipts. Raw logs retain their bytes. All ten independent
review texts are preserved as exact UTF-8 content in audit JSON, whose decoded
content hashes match the original reports. The portable manifest is
`evidence/parallel-sync-policy/manifest.json`.

The frozen ten-test executable is 1,762,376 bytes with SHA-256
`5867af45da2c75a32f26f9a70453ceaa3efb7d983b04eed6ccb3c1482b1d7ab6`.
Shared-cache compiler artifact identities are historical observations at
execution time; later normal builds may replace those files. No owner-generated
artifact, source, fixture, raw log or frozen executable was removed.

The ordinary import of Root correction
`9b9cbbc78e2ee035fe3f48f447f0072bd3aa17e2` preserves the six policy files and
includes the separately reviewed inherited V2 AGP grammar and dev-ordering fix.
The historical pre-import preservation and discovered V2 finding remain
recorded. The current 148-original guard includes the explicitly approved four
imported correction files rather than claiming their earlier hashes never
changed.

The executed long-input regression rejects a 100,000-byte malformed AGP value
with bounded error output. Recorded build/test durations include compilation
and test process work; they are not an app startup, allocation count, hardware,
large-project, parallel Gradle or before/after performance measurement.

Canonical parity credit remains zero for this task handoff. The scoped metadata
proposes the original method for promotion after lead capture, while the global
matrix remains unchanged and the reference census remains incomplete. No test
was deleted, ignored, weakened or classified not applicable.

Root still owns the combined matching-source full-app build and native
regression, full-workspace CI, exact canonical reference captures, conflict and
main verification, and protected merge. No new dependency or non-Rust exception
is added. This component is ready for lead integration; parallel sync workflow,
Phase 1 and the IDE remain incomplete.
