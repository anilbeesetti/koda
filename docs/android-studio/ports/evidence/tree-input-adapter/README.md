# Tree input adapter verification evidence

The final source is bound by `source-commit.json`, `review-source-final.json` and
`compiler-source-elf-proof.json`. `runtime-status-final.json` supersedes the
historical pre-fixture `runtime-status.json` and `review-source-round3.json`.
The artifact catalog maps external paths to exact portable bytes.

All 201 unfiltered Android tooling tests passed, including 27 new supplemental
adapter tests, with zero failures, ignores or filtering. Strict repository
Clippy, final formatting and the all-features library build passed. Final code,
quality and performance addenda apply to the final test fixture correction;
UI and UX passes apply to the unchanged frozen production boundary.

The first bundle build failed from ENOSPC before tests ran. The next attempt
compiled fresh and failed one new synthetic fixture whose producer name did not
match the pinned Studio converter. Both raw attempts remain. Three source/test
fix rounds are retained. No original assertion, fixture, matrix row or source
was changed to resolve them. The fixture correction retains all prior positive
assertions and adds rejection of the invalid producer name.

Precise stress receipts use direct `os.posix_spawn` and per-PID `os.wait4`.
They measure one debug test process, including its fixture setup and projection.
The Java case shares one 8 MiB input buffer across five entries; its RSS is not a
bound for captures containing distinct buffers. The 32 MiB limit bounds parsing
work. It does not bound total capture or tree memory. The runner hashes the ELF
before launch and source before/after; `post-stress-elf-proof.json` and the
performance review rehash the same frozen ELF afterward. Earlier aggregate
RUSAGE_CHILDREN receipts and unavailable `/usr/bin/time` attempts remain.

An earlier archive basename collision and its exact-hash reconstruction are
recorded in `round1-source-restoration.json`; separately named historical
snapshots and the colliding artifact remain. This evidence handling correction
is separate from the three implementation/test fix rounds.

These are captured-input component checks. Combined full-app, workspace, live
UI and CI gates belong to the lead integration. Original five tree methods and
ten assertions remain unported/not_run, with zero new canonical credit. The
external helpers and historical source snapshots are inert text records and
add no product runtime or build dependency.

The local `.gitattributes` preserves raw log whitespace, including terminal
blank lines. Source and test formatting checks retain their normal rules.
