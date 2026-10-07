# Android reference-test ledger

`cargo xtask android-parity` validates the checked-in catalog and prints an honest
progress summary. This initial slice contains the eight manually inspected methods
in AOSP `DefaultVariantsTest`, all unported and not run. It is not a test census,
and archive file totals must never be presented as reference-test totals.

Run from the checkout root:

```sh
cargo xtask android-parity
cargo xtask android-parity --reference-root /workspace/android-studio-references
cargo xtask android-parity --reference-root /workspace/android-studio-references --complete
```

The external root contains the three archived sources and selected unmodified
source extractions at `samples/<source-id>/<upstream-relative-path>`. Its
`samples/archive-manifest.json` records archive sizes, hashes and extraction
provenance; `jetbrains-android-provenance.json` records the mirror omission
warning. The checked-in manifest pins the hashes independently. No downloaded
archive belongs in the product repository. External byte verification is optional
for progress reports and required for the completion gate.

The canonical release manifest is
[studio-2026.2.1](https://android.googlesource.com/platform/manifest/+/refs/tags/studio-2026.2.1/default.xml).
The pinned AOSP [studio/version.bzl](https://android.googlesource.com/platform/tools/adt/idea/+/a84efec3ba9542d9bfa1255103f0dc94833a3796/studio/version.bzl)
identifies Rabbit1 and stable configuration. The JetBrains tag matches build
family 262.9437.185 without claiming that its mirror equals either AOSP source
tree. Its DefaultVariantsTest has different bytes from AOSP.
The mirror remains UNVERIFIED and cannot satisfy completion until a reviewed
canonical comparison, identified by immutable revision, resolves its omissions.

## Adding a matrix slice

Inspect the actual source and runner before adding tests. Add one manifest record
per method and explicit parameterized/generated instance, with pinned source ID,
relative path, SHA-256, declaration line, full suite name, and copyright. The
validator checks each citation against the unmodified extraction when the external
root is supplied. Helpers and source-file totals are not test records. Preserve
Apache 2.0 notices and attribution when copying source or fixtures. Add a ledger
entry for every catalog record; unmapped entries, duplicates and omissions fail.

The canonical ID is
`<source>@<revision>:<path>#<suite>.<method>[<instance>]`; omit the final bracketed
instance for nonparameterized methods. Keep newly identified applicable tests
`unported/not_run` until a Rust target exists. User-deferred features remain
unported, with their scope reason; deferral is not inapplicability. Use
`not_applicable/not_run` only with a specific explanation of the behavior that
has no equivalent in this IDE. Reviewers must judge that explanation.

A ported entry needs at least one Rust target with package name, optional
integration-target name (`null` selects `--lib`), exact test name, hashed Cargo
manifest, and hashed Rust source. Adapted ports additionally explain how the
changed test preserves the reference behavior. A blocked run has its own
`run_blocker`, independent of any adaptation reason. List every upstream fixture
in the manifest and map each to a hashed local destination. Reused fixtures keep
the original hash; adapted fixtures carry a specific explanation. If no external
fixture exists, explain that in `fixture_reason`.

Run evidence records `tested_commit` (the full 40-character Git commit), `command`,
`exit_code`, and a hashed UTF-8 `.log` artifact under
`docs/android-studio/evidence/`. Use precisely:

```text
cargo test --locked --manifest-path <manifest> -p <package> --lib <fully-qualified-test> -- --exact --show-output
cargo test --locked --manifest-path <manifest> -p <package> --test <target> <fully-qualified-test> -- --exact --show-output
```

The log must contain exactly one named test result and one libtest summary with
one executed test, no ignored tests and the recorded outcome. A successful
process with zero matched tests is rejected. Passing entries require successful
evidence for every target; failing entries require a named failing run. No run
evidence is allowed for not-run or blocked entries. File hashes detect changed
sources or artifacts. The tested commit must be an ancestor of the checkout, and
implementation/dependency files must match that commit, including the index and
working tree. Untracked implementation files invalidate evidence. Only the two
ledger JSON files, prose directly in `docs/android-studio/`, task reports in
`ports/`, and `.json`/`.log` evidence artifacts are excluded from this source-tree
comparison because they are committed after the run. Rust sources, manifests and
implementation fixtures must live outside `docs/android-studio/`; fixture and
implementation changes elsewhere inside that documentation tree remain checked.
Individual artifact hashes still check current evidence bytes. Git performs these source-control checks once
per distinct tested commit. Captured test output prevents a test's unterminated
print from corrupting the harness result line. Logs are reviewable records, not proof of semantic parity
or cryptographic proof that a command was executed.

Task-local port reports are provenance inputs, not a second authoritative matrix.
On integration, match each report's `reference_id` to the existing manifest and
update that entry in `test-parity.json`. Use these canonical statuses:
`unported`, `ported`, `adapted`, `not_applicable` and independently `not_run`,
`passing`, `failing`, `blocked`. An older task report's `unrun` means `not_run`;
never infer `passing` from a ported label or an aggregate test count. Record the
final combined source and manifest hashes, tested commit, and named Cargo-run
evidence before promoting the entry to passing. Direct test-executable runs are
useful supplemental checks; they cannot substitute for the manifest-bound Cargo
command required here. Preserve task reports as supplementary provenance only.

Every canonical target has this shape (angle-bracket values are placeholders):

```json
{
  "package": "android_tools",
  "integration_target": null,
  "test_name": "project_model::tests::<ported_test>",
  "manifest": { "path": "crates/android_tools/Cargo.toml", "sha256": "<sha256>" },
  "source": { "path": "crates/android_tools/src/project_model.rs", "sha256": "<sha256>" },
  "run": {
    "tested_commit": "<40-character-tested-commit>",
    "command": ["cargo", "test", "--locked", "--manifest-path", "crates/android_tools/Cargo.toml", "-p", "android_tools", "--lib", "project_model::tests::<ported_test>", "--", "--exact", "--show-output"],
    "exit_code": 0,
    "log": { "path": "docs/android-studio/evidence/<ported_test>.log", "sha256": "<sha256>" }
  }
}
```

## Completion gate

The complete gate first requires every source inventory and the global census to
be complete, no unverified mirror coverage, no unported tests, and every applicable
entry passing. Each complete inventory needs a hashed JSON attestation with
`source`, `revision`, `all_suites_and_expansions_reviewed: true`, `reviewed_by`,
and the full `reference_ids` list. The inventory must match the catalog exactly,
including inherited, parameterized and generated tests. This tool deliberately
does not infer exhaustive discovery from regex matches. Reviewers establish
census correctness and behavior preservation; an attestation alone cannot prove
that all upstream tests were discovered.

A verified mirror also needs a hashed JSON comparison with `source`, `revision`,
`canonical_repository: "https://git.jetbrains.org/idea/android.git"`, an immutable
`canonical_revision`, `all_omissions_resolved: true`, and `reviewed_by`. Reviewers
must establish that comparison, including obtaining omitted files; changing a
status or signing an unsupported claim does not establish complete coverage.

After those prerequisites and external-byte checks, the complete gate reruns every
unique mapped Rust test in the current checkout and rejects missing, ignored or
failing tests. Recorded logs cannot bypass this rerun. This focused gate supplements
the user's full-project build, full test suite and five reviewer verdicts; it does
not replace them. Source hashes alone do not cover transitive dependency changes,
which is another reason completion reruns the tests.
