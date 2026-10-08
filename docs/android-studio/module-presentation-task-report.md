# Captured module display policy: verified integration

The Rust policy preserves captured imported, internal, holder, display and sort
identities when deriving Android module labels. Root projects retain their
reported external IDs; nonroot projects use the final component; source-set
suffixes take precedence. Null-only fallback, raw path equality, empty suffixes,
Unicode and borrowed names follow the retained reference behavior. Twenty
supplemental tests cover these rules. Eight pinned source files retain their
exact bytes and Apache 2.0 attribution.

Combined source `905012f11885a86c9e8e18710ead07897d941d40` passes the scoped owner
reviews and the lead implementation/runtime gates. All five owner reviewers
passed static and source-bound runtime review. Their original reports and
receipts remain unchanged in [owner evidence](evidence/module-presentation/).
The previous component report is retained byte-exact in
[historical report](evidence/module-presentation-lead/history/owner-task-report.json).
There were no policy source fixes or weakened tests. Protected merge remains
pending Root's normal PR61 dependency merge and final metadata, conflict and
`main` checks.

| Lead check on combined source              | Actual result                                                                                       |
| ------------------------------------------ | --------------------------------------------------------------------------------------------------- |
| Locked `android_tools` tests, all features | 251 passed; zero failed, ignored or filtered; includes all 20 new supplemental cases                |
| Default `android_ui` library suite         | 125 passed; zero failed or filtered; one pre-existing ignored native capture retained               |
| Exact existing reference captures          | 35 actual passing captures bound to this source                                                     |
| Frozen Rust parity validator               | Passed: 95 selected, 19 ported, 16 adapted, 60 unported; 35 passing, zero failing                   |
| Normal all-features library build          | Passed                                                                                              |
| Repository strict Clippy                   | Passed; release, all targets and features, warnings denied                                          |
| Full workspace Rust formatting             | Passed                                                                                              |
| Normal full Koda app build                 | Passed in 205.18 seconds; actual executable hash and whole-source binding retained                  |
| Full workspace CI                          | 10,500 passed, zero failed, 23 existing skips retained; execution 1,099.69 seconds                  |
| CI formatting/Linux/macOS jobs             | All three succeeded; synthetic merge tree exactly equals the tested source tree                     |
| Native SDK and model regression            | Two syncs succeeded; both complete exports passed the current Rust decoder and model comparisons    |
| Native keyboard and editor regression      | 51 paired recorded commands exited zero; 17 full-app captures inspected                             |
| Source and cleanup guards                  | All 4,725 source bindings and 12 original/private fixture files unchanged; owned app/display closed |

[CI run 37696260846](https://github.com/anilbeesetti/koda/actions/runs/37696260846)
uses synthetic merge `6ce178a20c755cdce04774bc56281c8bf647c7c2`, whose tree
`ca7e3a82b6dcd83c756a40e6e74c57dcf9cdb796` exactly equals combined source.
The retained raw Linux job log has 423,527 bytes and SHA-256
`c024922293e081b70fc05fc69dcfee16313c667ed53f310657451dd76f0a3a83`.
The earlier CI summary's pending local-gate field remains historical; the
[final lead summary](evidence/module-presentation-lead/final-lead-summary.json)
adds the completed native and parity gates without rewriting that receipt.

The normal full-app ELF is 1,359,451,424 bytes with SHA-256
`9c018912f5622c4b991276a311a1f86bd3544de612537904d299cdc7f0b40891`.
The linker helper changes only the final output path, preserving all 1,205 other
arguments. Its independent static, probe and actual-app reviews are retained as
reversible JSON wrappers. This is actual app compilation and native execution;
it does not measure startup or hardware graphics performance.

The fresh native profile trusts only the private fixture and uses the unchanged
official wrapper with explicit JDK 21, AGP 9.4.0, Gradle 9.6.1 and SDK 37. This
is supplementary runtime evidence, not the pinned original reference driver.
Recorded keyboard checks cover wide and narrow forward Tab/Space, reverse
Shift+Tab/Space, Hide by Return, sidebar restore, explicit resync and Gradle/XML
tabs. The second sync's screenshot shows loading; the first attempted loading
capture still shows the trust prompt and receives no loading credit. No current
SDK error was induced or credited. The 230-pixel stress width clips unrelated
content and provides only toolbar-overflow evidence.

Both complete current models, including generated-artifact facts, equal the two
retained prior V2 models after replacing only each known private-root prefix in
JSON strings. Seven older complete legacy projections match every other field,
nested value and list order. Full raw exports remain intact. The current Rust
decoder validates schema, root, module/variant identity, consumer/version gates,
revision binding and generated assets/classpaths, then reproduces every sidecar
module value and list order. The native
[report](evidence/module-presentation-lead/native/review-report.md.json) and
[model proof](evidence/module-presentation-lead/native/native-model-proof.json)
retain exact commands, raw hashes and these comparison limits.

No original module-name or generated-tree test becomes ported here. The retained
[bounded preflight inventory](evidence/module-presentation-lead/preflight/bounded-test-inventory.json)
contains 53 inspected method declarations: nine existing adapted declarations
and 44 unported declarations. Its 22 source-provider definitions remain
unported. Runtime parameter and runner expansion are incomplete, so these
bounded source counts do not establish an executed runtime-case census. The
[original preflight report](evidence/module-presentation-lead/preflight/preflight-report.md.json)
retains the exact source inventory and planning limits. These counts are
separate from the canonical matrix of 95 identified tests and 35 passing ports.
The 20 policy assertions add supplemental source-derived coverage and zero
original-test credit. All five original generated-tree flows and their ten
assertions remain unported. The global census and mirror coverage remain
incomplete.

Authoritative getter transport, internal/qualified-name construction and
collisions, holder resolution, Kotlin facet/capability import, revision-bound
publication, the adapter's empty-label extension and logical Project UI remain
separate tasks. The visible tree remains physical, the Sync console still shows
raw model JSON, and the known Gradle indent-query diagnostic remains an editing
issue. Language servers are disabled for native isolation; no Rust language-server
credit is implied. No CPU, allocation count, app startup, large-project or
hardware GPU performance claim is added. No new non-Rust runtime exception is
introduced by this Rust policy.

[Portable lead evidence](evidence/module-presentation-lead/manifest.json) retains
byte-exact logs, current source snapshots, build/test/CI/native receipts and
reversible original review text. Earlier component reports, failures and artifact
retirement receipts are preserved. Metadata packaging keeps the exact seven
source exclusions, including all nested port evidence in the source guard.
This scoped integration is ready for Root's final metadata review and protected
merge; Phase 1 and the complete IDE remain unfinished.
