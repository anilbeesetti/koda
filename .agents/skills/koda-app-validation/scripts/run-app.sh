#!/usr/bin/env bash
set -euo pipefail
source "$(dirname -- "${BASH_SOURCE[0]}")/android-env.sh"
cd "$KODA_REPOSITORY"

if ! test -t 1; then
    printf 'Run with a PTY (exec_command tty:true) and keep stdout attached. Koda otherwise reloads the login-shell environment.\n' >&2
    exit 1
fi

if [[ "${1:-}" == --build ]]; then
    shift
    cargo build --locked -p zed --bin koda
fi
test -x "$CARGO_TARGET_DIR/debug/koda" || {
    printf 'Build first: cargo build --locked -p zed --bin koda\n' >&2
    exit 1
}

qa_directory="${KODA_QA_DIRECTORY:-$KODA_ENVIRONMENT_ROOT/qa}"
mkdir -p "$qa_directory/runtime" "$qa_directory/data" "$qa_directory/cache" "$qa_directory/user-data"
chmod 700 "$qa_directory/runtime"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-$qa_directory/runtime}"
export XDG_CACHE_HOME="$qa_directory/cache"
export XDG_DATA_HOME="$qa_directory/data"

# A restored cache may not contain a QA profile. Preserve any existing settings.
python3 - "$qa_directory/user-data" <<'PY'
from pathlib import Path
import json, sys
config = Path(sys.argv[1]) / 'config'
config.mkdir(parents=True, exist_ok=True)
defaults = {
    'settings.json': {
        'telemetry': {'diagnostics': False, 'metrics': False},
        'auto_update': False,
        'enable_language_server': False,
        'restore_on_startup': 'none',
        'theme': 'One Dark',
    },
    'keymap.json': [{'context': 'Workspace', 'bindings': {
        'ctrl-alt-p': 'android::ToggleComposePreview',
        'ctrl-alt-r': 'android::SyncProject',
    }}],
}
for name, value in defaults.items():
    path = config / name
    if not path.exists():
        path.write_text(json.dumps(value, indent=2) + '\n')
PY

cleanup() {
    if test -n "${app_process:-}"; then
        kill "$app_process" 2>/dev/null || true
        wait "$app_process" 2>/dev/null || true
    fi
    if test -n "${xvfb_process:-}"; then
        kill "$xvfb_process" 2>/dev/null || true
        wait "$xvfb_process" 2>/dev/null || true
    fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
printf 'Koda QA launcher PID: %s\n' "$$"

if [[ "${1:-}" == --headless ]]; then
    shift
    export DISPLAY="${KODA_TEST_DISPLAY:-:98}"
    unset WAYLAND_DISPLAY
    export XDG_RUNTIME_DIR="$qa_directory/runtime"
    export VK_ICD_FILENAMES="$KODA_ENVIRONMENT_ROOT/sysroot/usr/share/vulkan/icd.d/lvp_icd.json"
    export MESA_SHADER_CACHE_DIR="$qa_directory/runtime/mesa-cache"
    export ZED_ALLOW_EMULATED_GPU=1
    if "$KODA_ENVIRONMENT_ROOT/gui-sysroot/usr/bin/xdotool" getdisplaygeometry >/dev/null 2>&1; then
        printf 'Display %s is already active; choose a free KODA_TEST_DISPLAY.\n' "$DISPLAY" >&2
        exit 1
    fi
    "$KODA_ENVIRONMENT_ROOT/gui-sysroot/usr/bin/Xvfb" "$DISPLAY" -screen 0 1600x1000x24 -nolisten tcp -nolisten local -ac -noreset > "$qa_directory/xvfb.log" 2>&1 &
    xvfb_process=$!
    for ((attempt=0; attempt<50; attempt++)); do
        if "$KODA_ENVIRONMENT_ROOT/gui-sysroot/usr/bin/xdotool" getdisplaygeometry >/dev/null 2>&1; then
            break
        fi
        kill -0 "$xvfb_process" 2>/dev/null || { cat "$qa_directory/xvfb.log" >&2; exit 1; }
        sleep 0.1
    done
    "$KODA_ENVIRONMENT_ROOT/gui-sysroot/usr/bin/xdotool" getdisplaygeometry >/dev/null
elif test -z "${DISPLAY:-}${WAYLAND_DISPLAY:-}"; then
    printf 'No graphical display. Use --headless for the isolated Xvfb/software Vulkan session.\n' >&2
    exit 1
fi

launch_directory="$KODA_REPOSITORY/target/cloud-app"
mkdir -p "$launch_directory"
rm -f "$launch_directory/koda"
ln "$CARGO_TARGET_DIR/debug/koda" "$launch_directory/koda" || cp "$CARGO_TARGET_DIR/debug/koda" "$launch_directory/koda"
"$launch_directory/koda" --user-data-dir "$qa_directory/user-data" "$@" &
app_process=$!
wait "$app_process"
