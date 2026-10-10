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

## Shutdown persistence candidate request

The current request selects shutdown persistence source commit
`432d846df21effebfb947edef949ecf4a2373000` and Git tree
`32fec6c2aefb365d48efdf48ed1cfc5d6d74b2da`. Its five independent static source
reviews, current builds/tests and real native quit/reopen checks remain pending.
All 50 original persistence tests and eight MultiWorkspace test helpers are
unchanged; two new background-only persistence regressions await execution.
The assertion-preserving old-production counterfactual must execute and fail
for the expected foreground-dispatch dependency before reproduction is credited.

This ordinary-forward control successor starts from
`c188d877c8be148abab44f3175807ee1cb77efb4`, tree
`c21b5c756ecb97f23fcee90de148a8545ba6f339`, and changes only the request's
`source_commit` and `source_tree` values and this documentation. The complete
native workflow remains byte-for-byte unchanged (SHA256
`001128024a31429eb6c16465df248e9789fe98b84d0d1f9e1226f984d10da09f`), including
normal full-app dev/default features, C++ WebRTC, toolchains, caches, identity
guards, time limit and failure evidence. No source, fixtures, assertions or
ordinary CI workflow changes belong in this control successor.

Root may publish DRAFT source/control PRs to obtain compiler/test evidence while
source static reviews remain pending. CI publication supplies no acceptance or
protected-branch merge credit. Acceptance still requires all five source review
passes, the normal complete app build, full workspace test suite, both new tests
and all unchanged persistence/shutdown tests. Strict Clippy must cover all nine
packages with all targets and `-D warnings`: `android_tools`, `android_ui`,
`command_palette`, `fs`, `gpui`, `project`, `title_bar`, `workspace`, `zed`.
Every failing check stays recorded and must be resolved; no skipped, deleted or
weakened test or increased timeout is permitted.

The previous software-adapter request selected `f279ec17c50d81e163769069d54fef7d0114edc2`
and tree `9190f72e87c9281cf19b54c72fe8c6589e6ba92d`. Preserve its existing executable
at `/dev/shm/koda-f279-native-artifact-c188-20261010-attempt1/koda`, its c188
provenance and every QA1 through QA13 receipt, assertion, budget and failure.
QA12/QA13 app_will_quit timeouts, the QA13 calibration failure and earlier QA9 CPU
failure retain their results. The CPU ceiling stays 50 percent; all original
five-cycle CPU/RSS and responsiveness requirements remain. QA14 is a new attempt
and has not run. A new artifact never relabels previous failed or unrun checks.

Root must admit the new artifact using actual workspace filesystem free bytes
(approximately 3.6 GB at planning time), actual downloaded ZIP size, provenance
binary size, checkout/evidence growth and at least 512 MiB headroom. Stream the
single verified `koda` tar member from the file-backed ZIP into a new matching
full-source checkout's `target/cloud-app/koda` on the workspace filesystem.
Retain one decoded executable inode; a same-filesystem hard link may reference
that inode when needed. Do not decode or copy the new ELF into `/tmp` or
`/dev/shm`, overwrite existing app links, or delete the previous artifact/cache.
If admission fails, preserve the evidence and choose another Root-owned
file-backed destination before download or extraction. A sparse review checkout
is not sufficient for full-source asset discovery or build validation.

Before a live check, verify the requested commit and tree against the exact
local checkout, the workflow/run identity against GitHub, and every artifact
digest. Inspect the tar member list before extracting into an isolated directory;
it must contain only `koda`. Verify its hash and ELF architecture and resolve its
dynamic dependencies against the local native sysroot. The artifact is built on
Ubuntu 24.04 x86_64; runtime compatibility with a different distribution must be
checked, rather than inferred from the filename. Match the recorded embedded
application commit from `--system-specs` to the requested source commit.

Activate the matching checkout's
`.agents/skills/koda-app-validation/SKILL.md` environment. Use the new workspace
file-backed checkout's verified ignored `target/cloud-app/koda` executable,
preserving every existing app link and profile first.
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
