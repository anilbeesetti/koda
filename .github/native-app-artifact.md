# Full app artifacts for native QA

The **Native app artifact** workflow builds the complete `koda` application from
the immutable commit and Git tree in `native-app-artifact-request.json`. Changing
that request in a pull request triggers a new build without requiring a workflow
on `main`. Manual dispatch uses the request from the selected workflow revision.
The workflow checks both object IDs before installing dependencies or building.
It uses the existing Linux CI prerequisites, repository Rust toolchain, default
features, dev profile, and retained C++ WebRTC implementation. Cargo source and
build caches are restored read-only with the existing fork CI keys and paths.
The existing **Fork CI** workflow continues to run its usual checks and tests.

The successful artifact contains `koda.tar.gz` with the full executable and its
original executable mode, `provenance.json`, the request and control-workflow
identity, the normal Cargo build log, Rust and native toolchain versions, ELF
headers and dynamic dependencies, and their SHA-256 digests. A separate logs
artifact retains available evidence if the build or packaging fails. Actions are
pinned to commit IDs; the workflow needs only repository read access and exposes
no deployment, signing, or publication credentials. This YAML and the standard
shell, Git, jq, tar, ELF inspection, and GitHub Actions commands are CI
infrastructure; IDE functionality remains Rust.

## Software adapter candidate request

The current request selects software-adapter cached presentation source commit
`f279ec17c50d81e163769069d54fef7d0114edc2` and Git tree
`9190f72e87c9281cf19b54c72fe8c6589e6ba92d`. All five independent static source
reviews passed after the frame-demand fixture correction. The earlier `d1088df`
fixture failure and QA9 CPU failure remain recorded; current tests, fresh native
checks, and measured CPU improvement are pending.

This task changes only the request's `source_commit` and `source_tree` values and
this documentation, based on artifact controls commit
`049c1a547e534ac417fb9a186dcc712762ca753e`. The complete native artifact workflow
remains byte-for-byte unchanged, including its normal dev/default-feature build,
C++ WebRTC, bootstrap, caches, identity guards, time limit, and failure evidence.
No production source, reference tests, fixtures, or ordinary CI checks change.

The control task requires five independent code, UI, UX, performance, and code
quality reviews of these exact files before publication. Root must publish the
requested source commit first, then run the existing artifact workflow and verify
its executable, source/tree identity, and provenance. Source acceptance separately
requires current strict Clippy, the full test suite, the affected GPUI tests, live
redraw/cursor/animation/recovery checks, and the unchanged QA9 five-cycle CPU/RSS
and responsiveness checks. The CPU ceiling remains 50 percent. A prepared request
or static review does not establish any of these runtime results, reference-test
parity, or readiness to merge.

Before a live check, verify the requested commit and tree against the exact
local checkout, the workflow/run identity against GitHub, and every artifact
digest. Inspect the tar member list before extracting into an isolated directory;
it must contain only `koda`. Verify its hash and ELF architecture and resolve its
dynamic dependencies against the local native sysroot. The artifact is built on
Ubuntu 24.04 x86_64; runtime compatibility with a different distribution must be
checked, rather than inferred from the filename. Match the recorded embedded
application commit from `--system-specs` to the requested source commit.

Activate the matching checkout's
`.agents/skills/koda-app-validation/SKILL.md` environment. Place a verified hard
link or copy of the extracted executable at that checkout's ignored
`target/cloud-app/koda`, preserving any existing app links and profiles first.
Launch that explicit absolute executable path in a PTY with stdout attached and
an isolated `--user-data-dir`; follow the skill's SDK, Xvfb/software Vulkan,
profile, screenshot, and owned-process cleanup requirements. This location
provides debug asset discovery from the matching checkout. Verify the launched
process's executable identity and embedded commit again before UI checks.

`run-app.sh` selects the standard build-cache executable: it sources `env.sh`,
which resets `CARGO_TARGET_DIR`, and replaces `target/cloud-app/koda`. It cannot
select this downloaded artifact through an isolated `CARGO_TARGET_DIR` override.
Use the explicit artifact launch above without replacing the retained cache app.
Inspect the live app and affected flows, capture screenshots, and verify process
cleanup as required by the skill. A build artifact does not establish native UI
success, reference-test parity, or hardware GPU performance.
