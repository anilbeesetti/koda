#!/usr/bin/env bash
set -euo pipefail

script_directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "$script_directory/env.sh"
repository=$KODA_REPOSITORY
setup_directory="$KODA_ENVIRONMENT_ROOT/bootstrap"
case "${1:-}" in
    '') ;;
    --offline) export CARGO_NET_OFFLINE=true ;;
    *) printf 'Usage: %s [--offline]\n' "$0" >&2; exit 1 ;;
esac
mkdir -p "$setup_directory/cache" "$CARGO_HOME"
cd "$repository"
test -f Cargo.lock
test -f rust-toolchain.toml
for command in curl sha256sum tar git cc g++ make pkg-config python3; do
    command -v "$command" >/dev/null || {
        printf 'Missing prerequisite: %s\n' "$command" >&2
        exit 1
    }
done

if ! test -x "$CARGO_HOME/bin/rustup"; then
    test "${CARGO_NET_OFFLINE:-false}" != true || {
        printf 'Rust is not retained. Run setup without --offline.\n' >&2
        exit 1
    }
    case "$(uname -m)" in
        x86_64) bootstrap_target=x86_64-unknown-linux-gnu ;;
        aarch64) bootstrap_target=aarch64-unknown-linux-gnu ;;
        *) printf 'Unsupported architecture\n' >&2; exit 1 ;;
    esac
    bootstrap_url="https://static.rust-lang.org/rustup/dist/$bootstrap_target/rustup-init"
    curl --fail --location --retry 3 --proto '=https' --tlsv1.2 \
        "$bootstrap_url" -o "$setup_directory/cache/rustup-init"
    curl --fail --location --retry 3 --proto '=https' --tlsv1.2 \
        "$bootstrap_url.sha256" -o "$setup_directory/cache/rustup-init.sha256"
    (cd "$setup_directory/cache" && sha256sum --check rustup-init.sha256)
    chmod u+x "$setup_directory/cache/rustup-init"
    "$setup_directory/cache/rustup-init" -y --no-modify-path --default-toolchain none
fi

toolchain=$(python3 -c 'import tomllib; print(tomllib.load(open("rust-toolchain.toml", "rb"))["toolchain"]["channel"])')
if ! rustup run "$toolchain" rustc --version >/dev/null 2>&1; then
    test "${CARGO_NET_OFFLINE:-false}" != true || {
        printf 'The required Rust toolchain is missing. Run setup without --offline.\n' >&2
        exit 1
    }
    rustup toolchain install "$toolchain" --profile minimal
fi
rustup show active-toolchain
rustc --version
cargo --version
cargo test --locked -p collections -p sum_tree -p zlog --lib

test "$(uname -m)" = x86_64 || {
    printf 'The retained desktop/Android tools target Debian 13 x86_64.\n' >&2
    exit 1
}
(cd "$KODA_ENVIRONMENT_ROOT/packages" && sha256sum --check --quiet SHA256SUMS)
for component in native gui; do
    case "$component" in
        native) destination="$KODA_ENVIRONMENT_ROOT/sysroot" ;;
        gui) destination="$KODA_ENVIRONMENT_ROOT/gui-sysroot" ;;
    esac
    if ! test -d "$destination/usr"; then
        mkdir -p "$destination"
        for package in "$KODA_ENVIRONMENT_ROOT/packages/$component/"*.deb; do
            dpkg-deb --extract "$package" "$destination"
        done
    fi
done
source "$script_directory/android-env.sh"
pkg-config --exists alsa fontconfig freetype2 glib-2.0 sqlite3 wayland-client xkbcommon xkbcommon-x11
cmake --version
for executable in "$JAVA_HOME/bin/java" "$JAVA_HOME/bin/keytool" \
    "$KODA_ENVIRONMENT_ROOT/bin/android" "$KODA_ENVIRONMENT_ROOT/gui-sysroot/usr/bin/Xvfb" \
    "$KODA_ENVIRONMENT_ROOT/gui-sysroot/usr/bin/xdotool" "$ANDROID_HOME/build-tools/36.0.0/aapt2"; do
    test -x "$executable" || {
        printf 'Missing retained tool: %s. Restore the workspace cache before desktop/Android work.\n' "$executable" >&2
        exit 1
    }
done
test -f "$ANDROID_HOME/platforms/android-37.0/android.jar"
test -f "$KODA_ENVIRONMENT_ROOT/sysroot/usr/share/vulkan/icd.d/lvp_icd.json"

# Refresh Java trust when a restored session has a different injected public CA.
truststore=$(mktemp "$KODA_ENVIRONMENT_ROOT/java-cacerts.XXXXXX")
trap 'rm -f "$truststore"' EXIT
cp "$JAVA_HOME/lib/security/cacerts" "$truststore"
for certificate in /usr/local/share/ca-certificates/*.crt; do
    test -f "$certificate" || continue
    JAVA_TOOL_OPTIONS= "$JAVA_HOME/bin/keytool" -importcert -noprompt \
        -alias "cloud-$(basename "$certificate" .crt)" -file "$certificate" \
        -keystore "$truststore" -storepass changeit >/dev/null
done
mv "$truststore" "$KODA_ENVIRONMENT_ROOT/java-cacerts"
"$JAVA_HOME/bin/java" -version
printf 'Koda desktop/Android setup ready. Read %s/../SKILL.md for launch and test commands.\n' "$script_directory"
