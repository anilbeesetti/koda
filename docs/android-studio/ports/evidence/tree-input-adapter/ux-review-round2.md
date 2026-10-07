# Tree input adapter: independent UX review, round 2

Verdict: **PASS** for the scoped source contracts after consolidated fix round 1. No new required UX findings.

Bound production SHA-256: `d57ba8070321039cdd8b9749bc43df923e694a31c27299a1ec577b5974edfc3b`; additive export: `f4b033f8aa13b8141f706a1f24455c1d5500940ef5e4adffa46edd8baf1f2891`; 27 supplemental tests: `1fa2f0e7eedf5bf6cf2cbdbdcd5a1df8604325956cf6bfce78a9626872f6f69d`. All match `review-source-round2.json`. All 97 immutable inputs were rehashed and remain unchanged.

## Round-one finding resolved

The earlier UX **FAIL** remains intact in `ux-review-round1.md` (SHA-256 `e2aa04e2bb393c75f7f3c46426a1c55b80f6a0a4612f87931b7199cfca135962`). This is a new review of the corrected source, not a replacement or revision of the earlier verdict.

`crates/android_tools/src/project_tree_adapter.rs:463-490` now builds checked explicit presence and merges direct module inventory facts or legitimate observed descendants before deciding readiness. Paths are normalized and duplicate inventory entries rejected before this proof is used. A child under the module directory establishes Directory; an unrelated external generated child does not. `ProviderPresence::add_entry` retains contradiction detection.

At `:491-511`, only effective Directory can proceed. Unknown returns `Provider(UnknownPresence)`, Missing returns `Provider(MissingEntry)`, and File returns `Provider(Malformed)`, each with the module directory path. This handles a module with no supported roots or wholly missing roots as well as one with external sources. An explicit Directory probe can correctly establish a known empty module without inventing an inventory entry or navigation target. Direct Directory observations and observed descendants remain accepted.

The added regression `tests/project_tree_adapter.rs:1154-1180` distinguishes Unknown, Missing and File, checks the path, and contrasts them with a genuinely known empty Directory. The external generated child regression at `:1182-1219` covers omitted module proof and acceptance after an actual Directory fact. Existing observed-child/contradiction assertions at `:698-717` remain present. Their execution is pending; these are inspected test contracts, not claimed passing runs.

The invalid ready-case setup identified in round one is explicitly recorded in `fix-round1.json`: the presentation test now supplies `directory("")` at `tests/project_tree_adapter.rs:297`. Its module binding and display-name assertions are unchanged. The separate unknown-assets test at `:679` similarly supplies an actual module directory so that it continues testing the intended unknown source-root fact. I verified the complete prior 23-test file is an exact prefix of the current test file after only those two documented setup substitutions; four new tests follow. No prior assertion or original fixture was removed or weakened, and the 97-input guard includes the original tree/facts/reference data.

## Related affected presentation contract

I also checked the disabled-Kotlin precedence correction and the associated Unknown capability boundary. `project_tree_adapter.rs:248-250` collects common roots as KotlinAndJava; `:311-329` applies the original built-in precedence and exact-root deduplication before `:330-346` changes surviving common roots according to capability. Shadowed common roots require no Kotlin guess. Stable final grouping at `:355-357` retains this producer's within-group encounter order.

This matches the pinned collector's ordering at `AndroidViewNodeDefaultProvider.kt:169-188,198-202`. The new cross-provider collision tests at `tests/project_tree_adapter.rs:1110-1152` retain Kotlin-only or Java-only winners and cover Enabled/Disabled/Unknown. The existing same-provider test still rejects Unknown when a common root actually survives. The independent UI reviewer owns the UI round-two verdict; this source check does not substitute for that review or erase its round-one failure.

## Retained contracts and scope limits

The other reviewed UX contracts remain intact: typed missing presentation/provider/variant failures; active provider membership and explicit unsupported roots; actual static/generated provenance; exact module/variant/model/Java-byte revision checks; per-file Java parser/missing-byte/duplicate/budget fallback; actual physical navigation paths and unchanged UTF-8 declaration offsets; and distinct source-root presence/contradiction handling. None acquires fabricated filesystem EntryIds, Kotlin-as-Java facts, filename-based BuildConfig facts or runtime-completeness claims.

The plan still requires authoritative display/Kotlin producers, provider-sorted visible folder order, generated light-class filtering, BuildConfig artifact export, targeted scanning and Java-byte capture, stale request publication, project-panel consumers and the original Gradle fixture driver. No visible Android tree, keyboard navigation, GUI, app build or native capture was exercised by this review. All five original tree methods and ten assertions remain **unported/not_run**; canonical and native UI credit remain zero. Cargo, Gradle, source, tests, fixtures, Git state and display processes were untouched. Only this external report was written.

## Evidence feedback communicated to the owner

The external `round1-source/project_tree_adapter.rs` snapshot contains the prior **test** file (hash `eeaaaa01809c3dabebbcdca3f84b3ad22b02db6060de54db4ffb858c4314133e`), because source and tests share a basename. Both original hashes remain in the round-one review snapshot and `fix-round1.json`, and the retained test snapshot allowed exact assertion-retention verification above. I notified the owner, who retained the ambiguous artifact and recorded the collision and recovery in `round1-source-restoration.json`. I independently rehashed the recovered distinct `round1-source/src-project_tree_adapter.rs` and `round1-source/tests-project_tree_adapter.rs`: they exactly match the original frozen production hash `67dbb26617d07caee7db19047aa0cdb82c8572e34de305e97847b7af32549c09` and test hash `eeaaaa01809c3dabebbcdca3f84b3ad22b02db6060de54db4ffb858c4314133e`. I then inspected the actual source diff and confirmed its scope is the consolidated corrections described above. This evidence issue is resolved with its history preserved. I did not modify the snapshots or checkout.
