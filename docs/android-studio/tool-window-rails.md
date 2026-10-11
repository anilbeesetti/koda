# Bounded tool-window rail geometry

Task: `1-window-rails`, branch `android-studio-task/1-window-rails`, [PR 67](https://github.com/anilbeesetti/koda/pull/67).

Existing tool-window buttons now form contiguous 40px logical slots with 20px
icon boxes, 30px inset themed state surfaces and a separate 1px rail border.
The complete padded slot responds to clicks, including its corners. Existing
panel actions, menus, focus and dock resizing continue through their normal
Rust/GPUI routes. The implementation touches `crates/workspace/src/dock.rs` and
`crates/workspace/src/workspace.rs`.

This is an incremental geometry change. Project, editor, Android and other
panels already existed. The full Android Studio shell redesign, visible Android
project-tree adapter integration, Rust language servers and later build/run and
Android-tooling phases remain unfinished.

## Reference and test scope

Geometry follows IntelliJ Community revision
`b75ab523e6adbe1d26112219729eacbcfd24daa0`, matching the pinned Android Studio
build family. `JBUI.java` supplies stripe size/icon/padding; `SquareStripeButtonLook.kt`
supplies state-surface/icon placement; left/right toolbar sources define the
separate border. These are Apache 2.0 sources with original JetBrains notices;
full original bytes and hashes were retained in the task's external reference
evidence. The [task record](ports/tool-window-rails.json) includes exact source
paths, URLs and checksums. Matching build-family sources are not proof of
byte-identical packaged AOSP platform sources or pixel-identical global themes.

Selected complete platform-impl/platform-tests trees did not reveal an original
global-stripe geometry/input suite. The AOSP `AttachedToolWindowTest` cases cover
designer workbench controls and retain their existing unported status; they do
not become global-stripe ports or blanket not-applicable cases. The global test
census remains incomplete.

Two supplementary Rust GPUI cases validate actual layout bounds and interaction:

- `workspace::tests::test_tool_window_rail_reference_geometry`: 40px slots,
  contiguous ordering, 30px inset surfaces and 20px icons at normal/narrow/2× scale.
- `workspace::tests::test_tool_window_rail_full_slot_activates_and_hides_panel`:
  padded 1px/39px clicks, press/drag cancellation, real focused-handle evidence,
  raw Space/Enter, panel/editor focus and visibility in all three dock positions.

All original predicates and simulated inputs remain. Three prior semantic fix
rounds and the user's exactly one additional visible-worktree fixture correction
are preserved with their failures. No semantic budget reset occurred. These
supplements earn **zero original-reference test credit**. The seeded project
matrix remains partial: **95 cases: 20 ported, 16 adapted, 59 unported; 36 passing**.

## Current verification

Tested implementation source: `c5cf3ef9629d62ad05b43ec7d5a0a4929c31b001`, tree
`87d766d884651119a86358d6fd627405ef5616c1`, incorporating protected dependency
`404ea3faad6e6b4946206560685411d6be5c5721`. This documentation update leaves the
fixed seven-exclusion set of 4,748 implementation Source paths identical.

- [Full unfiltered CI](https://github.com/anilbeesetti/koda/actions/runs/37789793506)
  passed all three jobs: **10,559 passed, zero failures, 23 reported existing
  skips, 212 binaries**. Skip identity equivalence is not claimed. The two
  unignored GPUI supplements are included by the full Source suite; its
  failure-only reporter does not print individual passing names.
- The normal current full app build passed with the true c5 Git stamp and
  current rebuilt package roots. Strict repository Clippy passed using
  `./script/clippy --locked -p workspace -p android_tools -p android_ui`, with
  diagnostic JSON output only: release/all targets/all features/warnings denied,
  zero diagnostics and rebuilt lib/test units for all three packages. Absent
  optional local tools receive no extra check credit.
- Native current-app checks covered every rail at 1×/2×, padded corners,
  press/drag cancellation, actual Space/Enter via existing Build toolbar Tab
  traversal, menus/moves/fixed sizing, three splitters, light/dark states,
  font-size20, count badges and a 360×720 viewport. Original PNG pixel reads
  independently verify 40/5/30/1 logical geometry at both scales. Owned app and
  display processes closed; source/app/keeper guards passed.
- Five distinct final reviewers passed Code, UI, UX, Performance and Quality
  on that source and actual retained evidence. Final metadata-head full CI and
  lead merge approval remain pending.

Default JetBrains editor Ctrl+B/Alt+B resolve definition/implementation commands;
the native checks do not claim they toggle docks from the editor. Existing F6
region navigation is retained. The actual rail focus route is Build Output →
Tab to Hide → Tab to Agent, then raw Space/Enter, without direct focus injection
or a custom keymap. No new global Tab/arrow or assistive-focus parity is claimed.

Basic sync showed `:mobile`/`demoDebug` alongside evaluated-provider and SDK
build-tools36.0.0/license warnings; no installed emulator was available. Language
servers were disabled by the isolated validation profile. The narrow viewport
retains inherited whole-shell clipping while rail inputs work. Software Vulkan,
small-fixture inputs and owner-recorded RSS snapshots do not establish hardware,
startup, controlled memory, input-latency or large-project performance. These
checks earn no build/run, LSP, preview or complete-IDE credit.
