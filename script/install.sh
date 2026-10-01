#!/usr/bin/env sh
set -eu

# Installs Koda without changing an upstream Zed installation.

main() {
    platform="$(uname -s)"
    arch="$(uname -m)"
    channel="${KODA_CHANNEL:-stable}"
    KODA_VERSION="${KODA_VERSION:-latest}"
    # Use TMPDIR if available (for environments with non-standard temp directories)
    if [ -n "${TMPDIR:-}" ] && [ -d "${TMPDIR}" ]; then
        temp="$(mktemp -d "$TMPDIR/koda-XXXXXX")"
    else
        temp="$(mktemp -d "/tmp/koda-XXXXXX")"
    fi

    if [ "$platform" = "Darwin" ]; then
        platform="macos"
    elif [ "$platform" = "Linux" ]; then
        platform="linux"
    else
        echo "Unsupported platform $platform"
        exit 1
    fi

    case "$platform-$arch" in
        macos-arm64* | linux-arm64* | linux-aarch64)
            arch="aarch64"
            ;;
        macos-x86* | linux-x86*)
            arch="x86_64"
            ;;
        *)
            echo "Unsupported platform or architecture"
            exit 1
            ;;
    esac

    if command -v curl >/dev/null 2>&1; then
        curl () {
            command curl -fL "$@"
        }
    elif command -v wget >/dev/null 2>&1; then
        curl () {
            wget -O- "$@"
        }
    else
        echo "Could not find 'curl' or 'wget' in your path"
        exit 1
    fi

    "$platform" "$@"

    if [ "$(command -v koda)" = "$HOME/.local/bin/koda" ]; then
        echo "Koda has been installed. Run with 'koda'"
    else
        echo "To run Koda from your terminal, you must add ~/.local/bin to your PATH"
        echo "Run:"

        case "$SHELL" in
            *zsh)
                echo "   echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.zshrc"
                echo "   source ~/.zshrc"
                ;;
            *fish)
                echo "   fish_add_path -U $HOME/.local/bin"
                ;;
            *)
                echo "   echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.bashrc"
                echo "   source ~/.bashrc"
                ;;
        esac

        echo "To run Koda now, '~/.local/bin/koda'"
    fi
}

linux() {
    if [ -n "${KODA_BUNDLE_PATH:-}" ]; then
        cp "$KODA_BUNDLE_PATH" "$temp/koda-linux-$arch.tar.gz"
    else
        echo "Set KODA_BUNDLE_PATH to a tarball produced by script/bundle-linux." >&2
        exit 1
    fi

    suffix=""
    if [ "$channel" != "stable" ]; then
        suffix="-$channel"
    fi

    appid=""
    case "$channel" in
      stable)
        appid="dev.anilbeesetti.koda"
        ;;
      nightly)
        appid="dev.anilbeesetti.koda-nightly"
        ;;
      preview)
        appid="dev.anilbeesetti.koda-preview"
        ;;
      dev)
        appid="dev.anilbeesetti.koda-dev"
        ;;
      *)
        echo "Unknown release channel: ${channel}. Using stable app ID."
        appid="dev.anilbeesetti.koda"
        ;;
    esac

    # Unpack
    rm -rf "$HOME/.local/koda$suffix.app"
    mkdir -p "$HOME/.local/koda$suffix.app"
    tar -xzf "$temp/koda-linux-$arch.tar.gz" -C "$HOME/.local/"

    zed_editor="$HOME/.local/koda$suffix.app/libexec/koda-editor"
    if [ -f "$zed_editor" ] && command -v ldd >/dev/null 2>&1; then
        missing="$(ldd "$zed_editor" 2>/dev/null | sed -n 's/^[[:space:]]*\(.*\) => not found$/\1/p')"
        if [ -n "$missing" ]; then
            echo "Warning: your system is missing libraries that Koda needs:"
            echo "$missing" | sed 's/^/    /'
            echo "Install them with your package manager, or Koda will fail to start."
        fi
    fi

    # Setup ~/.local directories
    mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications"

    # Link the binary
    if [ -f "$HOME/.local/koda$suffix.app/bin/koda" ]; then
        ln -sf "$HOME/.local/koda$suffix.app/bin/koda" "$HOME/.local/bin/koda"
    else
        # support for versions before 0.139.x.
        ln -sf "$HOME/.local/koda$suffix.app/bin/cli" "$HOME/.local/bin/koda"
    fi

    # Copy .desktop file
    desktop_file_path="$HOME/.local/share/applications/${appid}.desktop"
    src_dir="$HOME/.local/koda$suffix.app/share/applications"
    if [ -f "$src_dir/${appid}.desktop" ]; then
        cp "$src_dir/${appid}.desktop" "${desktop_file_path}"
    else
        # Fallback for older tarballs
        cp "$src_dir/koda$suffix.desktop" "${desktop_file_path}"
    fi
    sed -i "s|Icon=koda|Icon=$HOME/.local/koda$suffix.app/share/icons/hicolor/512x512/apps/koda.png|g" "${desktop_file_path}"
    sed -i "s|Exec=koda|Exec=$HOME/.local/koda$suffix.app/bin/koda|g" "${desktop_file_path}"
}

macos() {
    if [ -n "${KODA_BUNDLE_PATH:-}" ]; then
        cp "$KODA_BUNDLE_PATH" "$temp/Koda-$arch.dmg"
    else
        if [ "$channel" != stable ] || [ "$arch" != aarch64 ]; then
            echo "Published Koda downloads currently support stable Apple Silicon builds. Set KODA_BUNDLE_PATH for other builds." >&2
            exit 1
        fi
        repository="${KODA_GITHUB_REPOSITORY:-anilbeesetti/zed}"
        if [ "$KODA_VERSION" = latest ]; then
            KODA_VERSION=$(curl "https://api.github.com/repos/$repository/releases/latest" | python3 -c 'import json, sys; print(json.load(sys.stdin)["tag_name"])')
        fi
        echo "Downloading Koda version: $KODA_VERSION"
        curl "https://github.com/$repository/releases/download/$KODA_VERSION/Koda-$KODA_VERSION-macos-$arch.dmg" > "$temp/Koda-$arch.dmg"
    fi
    hdiutil attach -quiet "$temp/Koda-$arch.dmg" -mountpoint "$temp/mount"
    app="$(cd "$temp/mount/"; echo *.app)"
    case "$app" in
        Koda.app | "Koda Dev.app" | "Koda Nightly.app" | "Koda Preview.app") ;;
        *) echo "The disk image does not contain a Koda app." >&2; hdiutil detach -quiet "$temp/mount"; exit 1 ;;
    esac
    echo "Installing $app"
    if [ -d "/Applications/$app" ]; then
        echo "Removing existing $app"
        rm -rf "/Applications/$app"
    fi
    ditto "$temp/mount/$app" "/Applications/$app"
    hdiutil detach -quiet "$temp/mount"

    mkdir -p "$HOME/.local/bin"
    # Link the binary
    ln -sf "/Applications/$app/Contents/MacOS/cli" "$HOME/.local/bin/koda"
}

main "$@"
