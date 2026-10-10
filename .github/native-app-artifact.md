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
