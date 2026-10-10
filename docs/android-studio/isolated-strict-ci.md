# Isolated strict Clippy

Fork CI has an **Isolated Android IDE strict Clippy** job on a separate Ubuntu
24.04 worker. It runs automatically only for pull requests whose head branch is
`android-studio-task/1-source15-isolated-strict-ci`, or through the optional manual
inputs below. A small eligibility job checks that branch spelling in Bash because
GitHub expression comparisons ignore case. A differently cased branch can start
only that gate; it skips the strict checkout, bootstrap and compiler. Unrelated
branch names keep their existing workload. The existing
formatting, Linux workspace test/build, and macOS check jobs keep their existing
commands and selection rules.

To request strict Clippy, dispatch Fork CI from the workflow revision containing
this job with both `strict_source_sha` and `strict_source_tree`. Each value must be
an exact 40-character lowercase hexadecimal Git object ID. Leaving both inputs
empty preserves the usual manual run; supplying just one fails the strict job.
The checkout must match both IDs and have no tracked changes. These checks and
the frozen `script/clippy` SHA-256 run before bootstrap and immediately before:

```sh
./script/clippy --locked -p android_tools -p android_ui -p command_palette -p fs -p gpui -p project -p title_bar -p workspace -p zed
```

The unchanged script adds `--release --all-targets --all-features -- --deny warnings`.
The job copies the existing Linux job's concurrency policy, native dependencies,
repository Rust toolchain, Node 24 and WASI SDK bootstrap. It starts without a
restored build cache. Its workflow commit and checked-out source commit/tree are
recorded separately because the strict target can differ from the dispatch ref.

The task-branch PR route always checks out commit
`594bf18526b3d7197315c82f5c6c3860cfb7bf5a`, tree
`dd2bb2ae16a43fd0a0dbee35bae3615a5fbe26f1`, independently of the workflow-only PR
changes. The workflow's ordinary jobs validate their PR merge or dispatch
revision, while the strict job validates its explicit source. Each result must
be attributed to its actual checkout. Creating the task's draft PR can trigger
this route without assuming workflow-dispatch API access or changing `main`.

The local strict attempt for this source stopped at its shared-memory resource
guard before a compiler verdict. Its failure remains recorded; an isolated CI
result does not turn that attempt into a pass or alter its caps. This job runs
Clippy and does not execute tests, establish native/UI validation, or prove
individual reference-test parity. The separate exact reference-test manifest
must be supplied and validated before claiming those assertions passed.

The workflow change is prepared for review. Completion requires the actual
isolated strict run through the task-branch PR route or manual route, all existing
Fork CI jobs on the task head, five independent reviews and lead validation.
No CI success is inferred from this document.
