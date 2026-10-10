# Isolated strict Clippy

Fork CI has an **Isolated Android IDE strict Clippy** job on a separate Ubuntu
24.04 worker. It runs automatically only for pull requests whose head branch is
`android-studio-task/1-evaluated-tree-isolated-strict-ci`, or through the optional manual
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
`d7b80774528b895829a18963e61761d73205febe`, tree
`54689ee1530f0dfbccdf8cdd7f4731fcfb4b536c`, independently of the workflow-only PR
changes. The workflow's ordinary jobs validate their PR merge or dispatch
revision, while the strict job validates its explicit source. Each result must
be attributed to its actual checkout. Creating the task's draft PR can trigger
this route without assuming workflow-dispatch API access or changing `main`.

The earlier local strict attempt for Source `594bf185` stopped at its shared-memory
resource guard before a compiler verdict. Its isolated CI229 run then failed
`clippy::cloned_ref_to_slice_refs` in `crates/android_tools/tests/project_model.rs:184`.
Source `28d91557` changed that assertion's expected singleton to a borrowed slice.
Its isolated CI231 run then failed six `clippy::redundant_clone` diagnostics in
`crates/android_tools/tests/project_context.rs`. Source `16c39c11` retains
that singleton fix, moves four final-use fixture values, and preserves both
observer snapshot clone checks through earlier bindings. Its isolated CI235 run
`38036495477` passed the exact nine-crate strict command. The same run's ordinary
jobs separately passed formatting/scripts, the Linux build with 10,653 passing
tests and 23 inherited skips, and the macOS check for workflow task
`2f29b86d52b8a2f4c2bea4487cc6cc2095164b3d` and PR merge
`b8a927e8a2e104d2e5294e0f5f0119ce90d85d3d`. Those results remain bound to their
actual checkouts. All previous failures remain recorded; the evaluated-tree
source's new strict result is pending.
An isolated CI result does not turn the local attempt into a pass or alter its
caps. This job runs
Clippy and does not execute tests, establish native/UI validation, or prove
individual reference-test parity. The separate exact reference-test manifest
must be supplied and validated before claiming those assertions passed.

The workflow change is prepared for review. Completion requires the actual
isolated strict run through the task-branch PR route or manual route, all existing
Fork CI jobs on the task head, five independent reviews and lead validation.
No CI success is inferred from this document.

## Evaluated-tree validation task

This task changes only the exact PR head-branch selector and its strict source
commit/tree binding in `.github/workflows/fork_ci.yml`, plus this document. It
uses the reviewed workflow revision `2f29b86d52b8a2f4c2bea4487cc6cc2095164b3d` as
its base. Its dependency is normal publication of the evaluated-tree source
`d7b80774528b895829a18963e61761d73205febe`; its five source reviews are static
passes, with runtime validation pending.

Acceptance requires the branch selector and both source IDs to match this
worktree's task and the frozen evaluated-tree source, with every other workflow
byte unchanged, followed by all five independent infrastructure reviews. Root
then publishes this task branch normally and runs the exact isolated strict
command against the pinned source. The ordinary jobs retain every test, filter,
cache, bootstrap, guard, timeout, and command and validate the infrastructure
PR's own checkout. The evaluated-tree source's full build and tests require
results from its own checkout. Neither the prepared pin nor the prior source's
successful CI supplies those results or reference-test parity. The five original
tree workflows and ten original assertions remain unported.
