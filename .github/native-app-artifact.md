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

Activate the checkout's `.agents/skills/koda-app-validation/SKILL.md` environment.
Use an isolated Cargo target directory containing the verified executable as
`debug/koda`, and launch the matching checkout with `run-app.sh` in a PTY without
`--build`. Its `target/cloud-app` link provides debug asset discovery from that
checkout. Preserve any existing app links and profiles before using that path.
Inspect the live app and affected flows, capture screenshots, and verify process
cleanup as required by the skill. A build artifact does not establish native UI
success, reference-test parity, or hardware GPU performance.
