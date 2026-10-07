# Captured module display policy

This task starts from `0e15a3d2b6eff51451b5f30d4c62ab0edde608ab` on
`android-studio-task/1-module-presentation`. It adds a pure Rust policy to
`android_tools` for captured, imported module identities.

`CapturedModuleIdentity` retains internal and holder names separately from
external-system awareness, imported Gradle module type, external project ID,
and exact reported project/root paths. `resolve_module_presentation` returns
separate internal, display, holder-group display, and sort names by borrowing
captured strings. An unavailable external-system capture returns a typed error;
it never becomes a non-Gradle or null getter result.

The policy follows the pinned `GradleModuleSystem.getDisplayNameForModule` and
its default imported-name fallback. Source-set modules use the final suffix of
the external ID when a delimiter exists. Project modules retain the whole ID
when reported project/root paths compare equal, including two reported nulls;
otherwise they use the last colon-delimited component. Missing IDs and source-set
IDs without a colon fall back to the imported internal name. Empty suffixes,
empty IDs, Unicode, whitespace, and case-sensitive raw paths remain exact.
Holder-group display and sort identity use the supplied imported holder/internal
names, independently of the display label.

Acceptance:

- All 20 supplemental source-derived edge tests pass without skipping, weakening,
  or replacing any existing test. Gradle awareness, unknown capture, root/nonroot,
  null/empty distinction, source-set precedence, raw path equality, distinct
  names, Unicode and borrowed long-name results remain covered.
- All 159 original tracked crate/matrix paths remain byte-identical except the
  one additive `android_tools.rs` module export. Existing public model and adapter
  DTOs, source-provider fixtures, matrix, app and UI are unchanged.
- Eight full pinned reference sources retain exact bytes, headers, URLs and
  hashes in `test_data/module_presentation`. The supplemental tests are traced
  to source behavior and grant zero original-test or canonical parity credit.
- Run normal locked unfiltered `android_tools` tests with all features, library
  build, repository `./script/clippy`, and formatting under the shared runtime
  lease. Record actual compiler source paths and executable identities.
- Obtain independent code, UI, UX, performance and quality review passes. Preserve
  every finding and execution failure; escalate after three unsuccessful product
  fix rounds. Root owns subsequent combined app/live/full-workspace checks before
  protected integration merge.

Owned files are `src/module_presentation.rs`, one export in `src/android_tools.rs`,
`tests/module_presentation.rs`, new attributed source/provenance fixtures, and
this task's plan/evidence. No dependency or non-Rust runtime is introduced.

Reference provenance and exact line ranges are in
`crates/android_tools/test_data/module_presentation/selection.json`. AOSP Rabbit1
is `a84efec3ba9542d9bfa1255103f0dc94833a3796`; matching IntelliJ Community is
`b75ab523e6adbe1d26112219729eacbcfd24daa0` (`idea/262.9437.185`). Community remains
supplementary evidence rather than a new canonical manifest source entry.
Original Apache 2.0 attribution is retained.

This task does not construct qualified/internal names or collision suffixes,
resolve holder pointers, capture authoritative Gradle getters, import Kotlin
facets, detect Kotlin capability, bind captures to revisions, publish tree
state, append Android node test suffixes, or wire the project panel. Those are
separate importer/controller/UI tasks. Import mode, root/build/included-build
identity, and existing name collisions must be captured before such policies
are implemented. Filesystem names or Kotlin support flags cannot substitute.

The current tree adapter rejects empty display labels. This policy preserves
the specifically applicable reference empty-label behavior; it does not
silently normalize it or extend the adapter. The adapter extension and its
edge tests remain unported. All five original generated-tree methods and their
assertions remain unported/not_run until actual import, file refresh, projection
and UI flows are implemented. The original Kotlin import/fixture/snapshot tests
are not ported by these supplemental display cases.

Local runtime gates and all five scoped reviews are initially pending. Passing
this bounded policy cannot establish full IDE, Android tree UI, or phase
completion. The final evidence report must state actual results and blockers.
