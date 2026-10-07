# Foundation checkpoint

The foundation phase and IDE remain unfinished. In `anilbeesetti/koda`, the first
verified foundation slice was merged and pushed to `android-studio` at
`d2046e36b9448c69d8fe1b6f35efa87fb8714f78`. Its tree is identical to the tested
task source `5577738e0cca988b7d61925f203d03c0b3e48fbf`. The original checkout,
cached `origin/main` and remote `main` remain at
`30fca99a7a015168dfe4f394f576fbe2955fb4ea`. No local `main` branch was created
or modified. Authenticated shell and connector access work; pushes are authorized.

The staging branch is `android-studio-task/1-integration-check`, worktree
`/workspace/android-studio-integration`. Its tested source snapshot is
`f8114bfafa3562760a5e8b98a4b20acf98cce983`. It adds the reviewed Rust module-import
and project-view-preferences backends and model-sync inventory to the merged
plan, default-variant port, recovery fix and parity tooling. Later staging
commits only add catalog, evidence and reports. These additional backends are
awaiting their complete workspace test gate before a protected-branch merge.

## Local results

The default-variant owner passed all eight AOSP reference ports, 50 tooling tests,
105 Android UI tests, clippy, the full app build, real Gradle sync and live
variant recovery. One pre-existing opt-in native capture remains ignored; none
of the eight reference ports is ignored. Keyboard selection, resizing and panel
scrolling were checked in the full app. All five scoped reviewers passed after
three fix rounds. The final source is `c992461ee3`; its report and captured logs
are committed on the owner branch at `dd6ffa031f`.

The ledger and catalog expansion passed all 41 xtask tests, including all 21
parity regressions, repository clippy, formatting, and pinned archive/source/
fixture verification. All five scoped expansion reviewers passed. The original
eight-case validator inputs are now immutable fixture files; their bytes and
all original assertions are preserved.

Twenty-six fresh manifest-bound exact Cargo runs on the immutable combined
source passed and were imported into `test-parity.json`: eight default-variant,
nine import and nine adapted preference cases. The CLI validated their source,
commit, exact named result and log hashes. The reviewed catalog now has 92
cases, including 57 toolbar/workbench cases still unported.

Combined all-feature checks passed 52 Android-tool unit tests, 20 import tests,
19 preference tests, 105 Android UI tests and 41 xtask tests. The one
pre-existing opt-in native capture was separately executed and passed; none
of the 26 reference ports is ignored. Repository clippy passed with release,
all targets/features and warnings denied. The full app built successfully in
2 minutes 32 seconds with `cargo build --locked -p zed --bin koda`. Logs are
`/workspace/android-studio-artifacts/combined-backends-all-features-tests.log`,
`combined-backends-clippy.log` and `combined-backends-app-build.log` in the same
artifact directory.

A Rust API probe imported an external Android library, checked source/copy
bytes at import, persisted/reloaded preferences and exercised durable
legacy-property migration. The actual full app opened the resulting project
and completed Gradle sync, discovering two application variants. A debug APK
built with Gradle 9.6.1, AGP 9.4.0, JDK 21, Android platform 37.0 and build tools
37.0.0 contains both the application and imported library classes. The retained
APK is 874,020 bytes with SHA-256
`3c289375ebeddf789b870816041ed63229847e14a33f8fad45b91bf2dbcbd483`.
Artifacts, logs and the observed successful sync screenshot are under
`/workspace/android-studio-artifacts/combined-backends-live`. This synthetic
smoke is supplementary validation, not a port of pinned Gradle integration
fixtures. Import-wizard and preference UI wiring remain pending. No device
execution or hardware performance result is claimed.

| Catalog status         | Count |
| ---------------------- | ----: |
| Ported and passing     |    17 |
| Adapted and passing    |     9 |
| Not applicable         |     0 |
| Unported and not run   |    66 |
| Failing reference runs |     0 |

These counts cover the bounded inspected suites. Exhaustive source/runner census is
incomplete and its total remains unknown. JetBrains mirror omissions remain
unverified. The complete-parity gate correctly refuses completion. Additional
model-sync behavior and Gradle-backed Android tree assertions remain unported.

## Blocking gates and next work

The initial local `cargo test --locked --workspace --no-fail-fast` exited 101 during compilation
and linking when the 32 GB filesystem filled. Workspace tests had not begun
execution. Its log is
`/workspace/android-studio-artifacts/full-workspace-tests.log`. Cleaning only
generated output recovered room for focused checks; it does not establish a
full-suite pass. The complete CI run on the first merged source subsequently
passed all Linux/macOS jobs: [run 37609909130](https://github.com/anilbeesetti/koda/actions/runs/37609909130)
ran 10,206 workspace tests, all passing, with 23 pre-existing ignored tests.
No test was deleted, newly ignored, or weakened.

The next full CI run, [37615677970](https://github.com/anilbeesetti/koda/actions/runs/37615677970),
tests `cbf8c0ec26057913f62c8dc9f494cf2baf27945d`, which has the same Rust source
and fixtures as the immutable combined snapshot above. Formatting/scripts and
the macOS application check passed; Linux workspace tests are still running at
this checkpoint. The additional backend merge remains gated on that result.

The foundation still needs exhaustive census, module-import and preference UI,
Android tree rendering/navigation and refresh integration, complete model/sync behavior,
window fidelity and creation workflows. Editing, build/run, Android tooling and
polish remain planned. Rust language servers and the full emulator/device/Logcat
done flow are not complete. This environment has no ADB target or `/dev/kvm`;
device execution was not validated.

Parallel work runs as internal subagents. Project-tree projection, the actual
GPUI tool-window toolbar, and Gradle source-provider metadata are separate active
task branches. The tree component has five scoped review passes; its five
original Gradle-backed cases remain unported until real model/fixture integration
exists. Sidebar chat creation is unavailable
in this cloud session: the callable app tools have no `create_thread` or
`fork_thread`. Handoff briefs are saved beside this report. A new environment
can fetch the published task branches; SDK files, upstream source
archives and external build/UI evidence require separate workspace transfer.

A portable Git bundle is prepared at
`/workspace/android-studio-artifacts/android-studio-local-tasks.bundle`. It carries
the earlier preparation branches and requires the original base commit above
to exist in the receiving repository. It predates the latest commits; GitHub
contains the published branches. SDK files, upstream source archives and
external build/UI evidence are separate workspace artifacts.
