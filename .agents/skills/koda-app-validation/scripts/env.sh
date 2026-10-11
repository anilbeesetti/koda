_koda_environment_prepend_path() {
    local variable="$1" entry="$2" existing="${!1-}"
    case ":$existing:" in
        *":$entry:"*) ;;
        *) printf -v "$variable" '%s' "$entry${existing:+:$existing}"; export "$variable" ;;
    esac
}

export CARGO_HOME=/workspace/.cache/zed/cargo
export RUSTUP_HOME=/workspace/.cache/zed/rustup
export CARGO_TARGET_DIR=/workspace/.cache/zed/target
_koda_environment_prepend_path PATH "$CARGO_HOME/bin"
export CARGO_BUILD_JOBS=4
export CARGO_NET_GIT_FETCH_WITH_CLI=true
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_DEV_BUILD_OVERRIDE_DEBUG=0

export KODA_REPOSITORY="${KODA_REPOSITORY:-$(git -C "$(dirname -- "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)}"
export KODA_ENVIRONMENT_ROOT="${KODA_ENVIRONMENT_ROOT:-/workspace/.cache/koda-environment}"

# Keep the native build environment identical for builds, tests, and linting.
if test -d "$KODA_ENVIRONMENT_ROOT/sysroot/usr"; then
    export PKG_CONFIG_SYSROOT_DIR="$KODA_ENVIRONMENT_ROOT/sysroot"
    _koda_environment_prepend_path PKG_CONFIG_PATH "$PKG_CONFIG_SYSROOT_DIR/usr/share/pkgconfig"
    _koda_environment_prepend_path PKG_CONFIG_PATH "$PKG_CONFIG_SYSROOT_DIR/usr/lib/x86_64-linux-gnu/pkgconfig"
    _koda_environment_prepend_path PATH "$PKG_CONFIG_SYSROOT_DIR/usr/bin"
    _koda_environment_prepend_path LIBRARY_PATH "$PKG_CONFIG_SYSROOT_DIR/usr/lib/x86_64-linux-gnu"
    _koda_environment_prepend_path LD_LIBRARY_PATH "$PKG_CONFIG_SYSROOT_DIR/usr/lib/x86_64-linux-gnu"
    _koda_environment_prepend_path LD_LIBRARY_PATH "$KODA_ENVIRONMENT_ROOT/gui-sysroot/usr/lib/x86_64-linux-gnu"
fi
unset -f _koda_environment_prepend_path
