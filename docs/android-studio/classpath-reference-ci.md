# Classpath reference validation

This independent Foundation task executes the two adapted AOSP `TestGroupTest`
behaviors against exact source `51fcb1b53b244cf35b914676de6c283fe310b2bb`, tree
`7c1a872c64a32c576d9f580b01ed6f223cf82435`. The complete Java/Kotlin originals,
Apache 2.0 license, notice, Rust implementation, declarations, manifests, and
build configuration are bound in `reference-classpath-named-tests.json`. Neither
these receipts nor the source task establish a complete configured reference
suite census or execute the original JVM runner.

## Task and dependencies

The task branch combines the reviewed classpath source with the reviewed Rust
exact-reference runner. It adds an explicit `--bin xtask` selector, a separate
two-case manifest, the isolated workflow, and protocol regression tests. The
existing forty-case manifest and ordinary CI remain unchanged. It depends on
the source task and exact-runner baseline; it touches
`tooling/xtask/src/android_reference_runs.rs`, the new manifest, this document,
and `.github/workflows/android_reference_classpath_exact.yml`.

The selected test compiler artifact must be an ELF `bin` test for `xtask`, with
the pinned `tooling/xtask/src/main.rs` source, normal dev test profile, and default
feature set. The actual Cargo runtime executable must have the same absolute
path, size, and SHA-256 before and after each exact run. A library or integration
target cannot stand in for the binary.

## Acceptance criteria

- All five independent reviewers pass on one frozen packet and close their read
  holds before mutation or execution.
- The runner passes repository formatting, strict `./script/clippy --locked -p
xtask`, and every unfiltered xtask test in isolated CI. New fixtures reject
  mixed or absent selectors, wrong compiler source/kind/features, partial
  original manifests, and missing, ignored, filtered, or duplicated test output.
- The pinned tested checkout passes full formatting, the same repository strict
  xtask invocation, every unfiltered xtask test, and callable classpath CLI help
  with its JAR argument and bounded options. Every one of the seventeen Linux
  classpath cases must appear as passing in the whole-test raw output.
- Each original behavior runs separately using literal Cargo `--bin xtask`
  commands with the full name, `--exact`, and `--show-output`: exactly one
  selected, one passed, zero failed, zero ignored, and zero measured. Zero-test
  successes receive no credit. The five original assertions remain unchanged.
- Source checks use their own fresh target so the subsequent named compiler
  artifact cannot inherit a previous build. Full checkout/index and fixture
  bindings match before and after execution.
- The normal full-app build, full workspace test suite, lead integration checks,
  and any required app validation still pass before a protected-branch merge.

## Bounds and evidence

The existing Linux Rust supervisor retains both complete output streams, waits
for the owned process group and pipe EOF, and records actual exit status. It
fails explicitly on deadlines, incomplete pipes, source mismatch, or output
limits. Each process allows 16 MiB of successful output, each archive allows
128 MiB, and a further 8 MiB is a failure drain allowance; hitting a ceiling
never produces a truncated PASS.

Pinned source checks have deadlines of 300 seconds for formatting, 1,800 seconds
for strict Clippy, 1,200 seconds for whole xtask tests, and 600 seconds for CLI
help. Named compilation retains the existing 3,600-second deadline and each
exact test retains 600 seconds. The isolated job has a 180-minute limit and
always uploads any receipts, complete retained raw logs, and worker output,
including failures. Receipts explicitly record that no parity ledger was
changed.

Local compilation was not admitted because the new ZIP dependency cache reuse
and resource fit were unproven. Local formatting is a Root-owned scoped check;
actual source and runner compilation occur in isolated CI. Static review does
not establish runtime success. Windows native path behavior remains not run;
Linux spelling assertions do not establish Windows execution. Deferred features
remain unported, and the parity matrix is unchanged by this task.
