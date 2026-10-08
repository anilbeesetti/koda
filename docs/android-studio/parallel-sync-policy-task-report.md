# Parallel-sync eligibility policy: verified lead integration

Rust now evaluates the pinned `SUPPORTS_PARALLEL_SYNC` producer/AGP policy from
captured facts. It follows `[7.2.0, 7.3.0-alpha01)` and
`[7.3.0-alpha04, infinity)`, preserves signed model-version order and historical
AGP preview spellings, and reports missing, malformed and unsupported facts.
Ten added Rust tests include the complete original
`ModelVersionsTest.checkSupportsParallelSync` and nine supplemental boundary
groups. All nine original assertions preserve their order and named messages,
both signed `Int.MIN_VALUE` components, the empty description and the unused
null minimum consumer. Complete source fixtures retain Apache 2.0 attribution.

The policy computes eligibility. Gradle scheduling and UI publication remain
separate tasks. This change adds no new non-Rust runtime dependency.

Combined source `4fb7a80eacc598a786b4b98c809d1df24a1b02ee` contains the unchanged owner policy, the
captured-module display prerequisite and the reviewed V2 AGP correction. All
five owner reviewers passed static and source-bound runtime review. Earlier
owner evidence and pending statements are retained byte-exact as history.

| Lead check                         | Actual result                                                                                                                 |
| ---------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| Locked tooling suite, all features | 261 passed; zero failed, ignored or filtered; nine fresh current-worktree test targets                                        |
| Default UI library suite           | 125 passed; zero failed or filtered; one unchanged baseline native capture ignore                                             |
| Normal library build               | Passed; fresh current-worktree artifact                                                                                       |
| Repository Clippy                  | Passed; release, all targets/all features, warnings denied                                                                    |
| Workspace formatting               | Passed                                                                                                                        |
| Exact original reference captures  | 36 passed on this source, including all nine parallel assertions                                                              |
| Frozen parity validator            | Passed: 95 selected, 20 ported, 16 adapted, 59 unported; 36 passing                                                           |
| Normal full application build      | Passed in 623.54 seconds; ELF 1,359,451,600 bytes, SHA-256 `eac18f9a4c2ed6499406ba79b38a1ea6c290c903f7faa9c47472855aa3211e34` |
| Full workspace CI                  | 10,510 passed; zero failed; 23 existing skips; all three jobs succeeded                                                       |
| Native SDK/model/workflows/cleanup | Passed two SDK syncs, two complete current Rust model regressions, 51 paired exit-zero commands and owned-process cleanup     |
| Whole-source preservation          | All 4,730 tracked source and fixture bindings unchanged under the exact seven source exclusions                               |

[CI run 37703317253](https://github.com/anilbeesetti/koda/actions/runs/37703317253)
used synthetic merge `8b4f48cbe5905baef4610a4d52732bf152e565bc`. Its tree
`9659baeb98fcf7111435169eb04490ba3952989d` exactly equals the tested source tree.
The raw Linux workspace log retains 430,953 bytes with SHA-256
`15a2ad1d1fe1b9a388d9e05d9309640573de2c80c2657e247c2bae4ce1338fe4`.
The earlier CI receipt's pending local-gate field remains historical; the
[final lead summary](evidence/parallel-sync-policy-lead/final-lead-summary.json)
records the completed app and native checks separately.

The previous canonical 35 passing cases retain their statuses, reasons and
mapped target selections. Only the existing selected parallel-sync method gains
one original-test credit. The nine supplemental tests do not increase the
original-case count. The census remains partial and mirror coverage unverified.
No original test was deleted, weakened, skipped or reclassified as inapplicable.

The independent component/UI and exact36/retirement reviews passed. The completed
133-payload exact-capture package remains immutable. New lead evidence retains raw compiler and test logs, reversible original review text, source/fixture snapshots, actual CI logs and the source-tree equivalence proof. The initial
ENOSPC/SIGBUS attempt and preparation failures remain preserved. Scoped generated
unit/UI cache retirements followed successful execution, exact hashes/inodes,
accessible PID/FD checks and explicit root-service access limitations; frozen
policy probes and original sources/fixtures/logs remain retained.

Native evidence validates the existing full application and model transport.
The current Rust policy passed on both complete captured SDK models. Native
models equal both prior successful P1 full models after only their exact private
root-prefix substitution. Seven older records match the complete legacy
projection, excluding only the additive generated-artifacts envelope. The
supplementary driver uses JDK 21, AGP 9.4.0, Gradle 9.6.1 and SDK 37. All 17
captures were inspected; Root personally inspected three. The recorded 51
commands include inputs, screenshots, discovery, geometry and settle delays.
The 230-pixel stress width is below the declared 360-pixel minimum and supplies
only tab-overflow evidence. Only the second sync provides loading-state credit;
no current SDK-error state was induced or credited.

The full native manifest binds 677 retained files (54,689,888 bytes). The new lead
package copies a stated subset of proofs, scripts, raw models/logs, fixtures and
17 reversible PNG wrappers; generated Gradle build/cache, QA databases, libraries
and executable binaries remain external and are not claimed as portable copies.

This evidence does not observe concurrent Gradle scheduling or claim a logical project
tree, Rust language-server completion or performance measurements. Known physical
tree/raw Sync output/narrow captions and Gradle indentation limitations remain
tracked. Phase 1 and the complete IDE remain unfinished.

Actual scoped implementation/runtime gates and independent final metadata review passed. The [final independent review](evidence/parallel-sync-final-review/manifest.json) binds the portable evidence, source preservation and formatter classification. Root conflict/main verification and normal protected merge remain pending.

[Portable lead evidence](evidence/parallel-sync-policy-lead/manifest.json) preserves the new payloads. The [owner evidence](evidence/parallel-sync-policy/manifest.json) and [exact-capture evidence](evidence/parallel-sync-integration-exact/manifest.json) remain unchanged.
