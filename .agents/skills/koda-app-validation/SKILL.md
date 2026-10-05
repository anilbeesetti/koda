---
name: koda-app-validation
description: >-
  Use when building, launching, or validating Koda implementation and UI changes,
  including live app screenshots, Android project sync, Compose previews, focused
  Rust tests, and the prepared Linux cloud environment.
---

# Koda app validation

Read the checkout's `.rules` and applicable `AGENTS.md`, preserve existing changes,
and use the checkout being reviewed. For every implementation change, build and
run the full app and exercise the affected behavior before claiming completion.
Inspect live screenshots for UI changes; a unit test or native surface capture
alone does not establish that the full app works.

## Normal developer environment

Follow the repository's platform setup instructions for native prerequisites
(`script/linux` on supported Linux distributions). With Rust from
`rust-toolchain.toml`, a graphical session, and Vulkan support on Linux, run:

```bash
cargo run --locked -p zed --bin koda -- /absolute/path/to/project
```

Android projects also need their usual SDK and a Gradle-compatible JDK. When the
checkout supports bundled Compose previews, use the normal app build; do not run
`script/install-android-preview` or select an external preview installation as a
workaround. Verify the checkout's capabilities before claiming preview support.

## Prepared Linux cloud environment

The adjacent [scripts](scripts) retain the verified cloud workflow in Git. They
resolve their location and repository root, so they do not require
`/workspace/cloud-environment`. Generated files, downloaded tools, SDKs, Gradle
state, app profiles, and screenshots stay outside tracked files.

From the checkout root, activate the environment in each new shell:

```bash
source .agents/skills/koda-app-validation/scripts/env.sh
rustup show active-toolchain
bash .agents/skills/koda-app-validation/scripts/install.sh --offline
```

The scripts target the prepared Debian 13 x86_64 environment. Rust and Cargo's
build cache use `/workspace/.cache/zed`; other tools default to
`/workspace/.cache/koda-environment` (`KODA_ENVIRONMENT_ROOT` overrides that
location). `KODA_REPOSITORY` can explicitly select another checkout. The native
multiarch library directory is also added to `LIBRARY_PATH` for GCC link steps
that don't use pkg-config's search paths. Keep these native paths identical
across builds, tests, and linting to avoid rebuilds.
Four jobs, disabled incremental compilation, and no dev debug symbols control
disk use. Check free disk space before a large build, especially when old debug
binaries are still hard-linked into the checkout's ignored `target` directory.

The retained cache includes native libraries/CMake, Xvfb/xdotool, Mesa software
Vulkan, Temurin JDK 21, Android platform 37.0, build tools 36.0.0 and 37.0.0,
Google's Android CLI and its runtime, and Gradle caches. It does not include adb,
an emulator, or sdkmanager. The scripts validate this prepared cache; they do not
provision a bare machine. Setup verifies `packages/SHA256SUMS`, can re-extract
retained native debs, refreshes Java's truststore with the session's public CA
certificates, and runs the library baseline. Missing JDK/SDK/CLI directories
require restoring the retained cache. Without `--offline`, Rust can download
missing dependencies. Binaries and public certificate bundles do not belong in
this skill's Git directory.

For Android tooling, activate the additional environment:

```bash
source .agents/skills/koda-app-validation/scripts/android-env.sh
```

This selects the retained JDK, SDK and CLI, preserves explicit Java properties,
derives Java proxy defaults from the public cloud network policy, and uses
writable Java preferences/user directories without changing HOME. Preserve
injected credentials, proxy and CA trust; do not dump credentials or disable TLS,
signature, or checksum verification. Refresh trust after a restored session
before Gradle downloads. No app login is needed for local editor checks.
Minimal cloud sessions can log D-Bus/keyring errors from optional credential
providers. These messages alone do not establish an editor failure; verify the
affected local flow rather than attempting to configure an account for the test.

## Run the full app and capture the UI

Use a **PTY with stdout attached** (`exec_command` with `tty:true`):

```bash
.agents/skills/koda-app-validation/scripts/run-app.sh --build --headless \
  /absolute/path/to/project /absolute/path/to/source.kt
```

`--build` builds the current checkout with `cargo build --locked -p zed --bin koda`.
Omit it only when the binary already matches that checkout. The launcher uses a
hard link under ignored `target/cloud-app` for debug asset discovery with Cargo's
external cache. It creates an isolated profile with telemetry, updates and
language servers disabled, and preserves any existing profile settings. It
never enables global project trust. Set `KODA_QA_DIRECTORY` to isolate a different
test profile; by default profiles/artifacts use the retained cache's `qa` directory.

`--headless` starts Xvfb at `:98`, waits for readiness, and uses Mesa software
Vulkan with `ZED_ALLOW_EMULATED_GPU=1`. `KODA_TEST_DISPLAY` selects a free display.
Omit `--headless` for an existing graphical session. Software rendering supports
functional checks; it does not establish hardware GPU performance.

In sandboxed tools, grant network access for X11's local socket as well as HTTP
downloads. Keep stdout attached: a non-PTY app launch reloads the login-shell
environment and may lose prepared SDK/JDK settings. Do not use `--foreground`;
the full Linux app does not accept that flag. Restart processes after restoration.
Cargo's `--offline` only controls Cargo downloads: native build scripts such as
LiveKit's WebRTC setup may still fetch their pinned release artifacts. A clean
desktop build therefore also needs network access through the session proxy.

From another activated shell:

```bash
export DISPLAY=:98
"$KODA_ENVIRONMENT_ROOT/gui-sysroot/usr/bin/xdotool" search --onlyvisible --class 'koda|Zed'
mkdir -p "$KODA_ENVIRONMENT_ROOT/qa/screenshots"
import -display "$DISPLAY" -window root "$KODA_ENVIRONMENT_ROOT/qa/screenshots/koda.png"
```

Wait for the changed flow to settle before capturing. Inspect the actual image.
Without a window manager, use `xdotool windowfocus`, not `windowactivate`. Keep
`xdotool type` last in its invocation; following arguments are typed as text.

Check affected alignment, resizing, scrolling, focus/keyboard navigation,
loading, empty and error states. Exercise relevant save/reload and tab switching
flows. Inspect logs for crashes, repeated failures and unexpected work; check
resource cleanup and visible stutters when the change can affect them. Report
actual observations and distinguish measured performance from a visual check.

Close the app normally to release active preview artifacts. To stop from another
tool call, send TERM to the launcher PID it prints; its signal handler stops only
its own app and Xvfb. A tool's PTY interrupt may detach without signaling the
launcher, so verify its processes exited. Never kill unrelated GUI sessions.

## Android and Compose checks

Copy `examples/android-ide` to a writable fixture outside the checkout. Exclude
generated builds, `.gradle`, `.kotlin`, `.koda`, `.zed`, `local.properties` and
workspace state. Set the copy's `local.properties` to the retained SDK; do not
change a user's project to suit this environment. Trust only the fixture in the
app, sync it, and choose its `:mobile` / `demoDebug` variant.

The isolated profile binds Ctrl+Alt+R to Android sync and Ctrl+Alt+P to Compose
previews. Use `MainActivity.kt` in `mobile/src/main/java/dev/zed/androidsample`.
When `PreviewGallery.kt` is present, it exercises the expanded gallery: ten cards,
nine successful and one intentional render failure. Verify manual refresh,
automatic unsaved rebuilds, skeleton selection/source navigation, tab switching,
divider resizing and zoom controls when the checkout provides those features.

For bundled previews, runtime extraction uses
the selected Koda data profile's `android-tools/compose-preview`, with per-request directories under
`renders`. Verify gallery replacement and closing release request artifacts and
do not create a project-local `.koda/android-preview` directory. If present,
follow `examples/android-ide/COMPOSE_PREVIEW.md` and `script/test-android-preview`
for saved/unsaved/restored renderer probes using an extracted bundled runtime.

## Focused tests

```bash
cargo test --locked -p collections -p sum_tree -p zlog --lib
.agents/skills/koda-app-validation/scripts/test-android.sh
cargo fmt --all -- --check
./script/clippy --locked -p android_tools -p android_ui
```

Use only checks relevant to changed crates. The Android helper detects whether
`android_tools` exposes `bundled-preview` before enabling it, so it also works on
checkouts predating that feature. It resolves test executables from Cargo JSON
and hard-links them into ignored `target/cloud-tests` for GPUI debug assets.
Arguments are forwarded to both suites; use a filter or `--nocapture` as needed.
Require actual passing results; `--no-run` is not test success.

When the checkout contains `capture_compose_preview_surface`, opt in to native
GPUI screenshots explicitly:

```bash
export VK_ICD_FILENAMES="$KODA_ENVIRONMENT_ROOT/sysroot/usr/share/vulkan/icd.d/lvp_icd.json"
export XDG_RUNTIME_DIR="$KODA_ENVIRONMENT_ROOT/qa/runtime"
export MESA_SHADER_CACHE_DIR="$KODA_ENVIRONMENT_ROOT/qa/runtime/mesa-cache"
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
export COMPOSE_PREVIEW_VISUAL_OUTPUT="$KODA_ENVIRONMENT_ROOT/qa/native-captures"
.agents/skills/koda-app-validation/scripts/test-android.sh \
  capture_compose_preview_surface --ignored --nocapture
```

Verify the test exists and actually ran; zero matching tests do not validate a
capture. Label these native surface screenshots separately from full-app captures.
`COMPOSE_PREVIEW_VISUAL_FIXTURE` can supply real rendered previews. The normal
Android suites need no GPU. `./script/clippy` uses a release profile and all
targets/features; budget time/disk accordingly. Do not test the whole workspace
or start the collaboration backend for ordinary editor work.

In the completion report and PR, give the checkout/build, affected live flow,
focused test results, screenshot locations, and any concrete unverified behavior.
If a check is blocked, explain the attempted command and actual blocker instead
of claiming it passed.
