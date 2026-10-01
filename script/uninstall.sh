#!/usr/bin/env sh
set -eu

# Uninstalls Koda that was installed using the install.sh script

check_remaining_installations() {
    platform="$(uname -s)"
    if [ "$platform" = "Darwin" ]; then
        # Check for any Koda variants in /Applications
        remaining=$(ls -d /Applications/Koda*.app 2>/dev/null | wc -l)
        [ "$remaining" -eq 0 ]
    else
        # Check for any Koda variants in ~/.local
        remaining=$(ls -d "$HOME/.local/koda"*.app 2>/dev/null | wc -l)
        [ "$remaining" -eq 0 ]
    fi
}

prompt_remove_preferences() {
    printf "Do you want to keep your Koda preferences? [Y/n] "
    read -r response
    case "$response" in
        [nN]|[nN][oO])
            rm -rf "$HOME/.config/koda"
            echo "Preferences removed."
            ;;
        *)
            echo "Preferences kept."
            ;;
    esac
}

main() {
    platform="$(uname -s)"
    channel="${KODA_CHANNEL:-stable}"

    if [ "$platform" = "Darwin" ]; then
        platform="macos"
    elif [ "$platform" = "Linux" ]; then
        platform="linux"
    else
        echo "Unsupported platform $platform"
        exit 1
    fi

    "$platform"

    echo "Koda has been uninstalled"
}

linux() {
    suffix=""
    if [ "$channel" != "stable" ]; then
        suffix="-$channel"
    fi

    appid=""
    db_suffix="stable"
    case "$channel" in
      stable)
        appid="dev.anilbeesetti.koda"
        db_suffix="stable"
        ;;
      nightly)
        appid="dev.anilbeesetti.koda-nightly"
        db_suffix="nightly"
        ;;
      preview)
        appid="dev.anilbeesetti.koda-preview"
        db_suffix="preview"
        ;;
      dev)
        appid="dev.anilbeesetti.koda-dev"
        db_suffix="dev"
        ;;
      *)
        echo "Unknown release channel: ${channel}. Using stable app ID."
        appid="dev.anilbeesetti.koda"
        db_suffix="stable"
        ;;
    esac

    # Remove the app directory
    rm -rf "$HOME/.local/koda$suffix.app"

    # Remove the binary symlink
    rm -f "$HOME/.local/bin/koda"

    # Remove the .desktop file
    rm -f "$HOME/.local/share/applications/${appid}.desktop"

    # Remove the database directory for this channel
    rm -rf "$HOME/.local/share/koda/db/0-$db_suffix"

    # Remove socket file
    rm -f "$HOME/.local/share/koda/koda-$db_suffix.sock"

    # Remove the entire Koda directory if no installations remain
    if check_remaining_installations; then
        rm -rf "$HOME/.local/share/koda"
        prompt_remove_preferences
    fi

    rm -rf $HOME/.koda_server
}

macos() {
    app="Koda.app"
    db_suffix="stable"
    app_id="dev.anilbeesetti.koda"
    case "$channel" in
      nightly)
        app="Koda Nightly.app"
        db_suffix="nightly"
        app_id="dev.anilbeesetti.koda-nightly"
        ;;
      preview)
        app="Koda Preview.app"
        db_suffix="preview"
        app_id="dev.anilbeesetti.koda-preview"
        ;;
      dev)
        app="Koda Dev.app"
        db_suffix="dev"
        app_id="dev.anilbeesetti.koda-dev"
        ;;
    esac

    # Remove the app bundle
    if [ -d "/Applications/$app" ]; then
        rm -rf "/Applications/$app"
    fi

    # Remove the binary symlink
    rm -f "$HOME/.local/bin/koda"

    # Remove the database directory for this channel
    rm -rf "$HOME/Library/Application Support/Koda/db/0-$db_suffix"

    # Remove app-specific files and directories
    rm -rf "$HOME/Library/Application Support/com.apple.sharedfilelist/com.apple.LSSharedFileList.ApplicationRecentDocuments/$app_id.sfl"*
    rm -rf "$HOME/Library/Caches/$app_id"
    rm -rf "$HOME/Library/HTTPStorages/$app_id"
    rm -rf "$HOME/Library/Preferences/$app_id.plist"
    rm -rf "$HOME/Library/Saved Application State/$app_id.savedState"

    # Remove the entire Koda directory if no installations remain
    if check_remaining_installations; then
        rm -rf "$HOME/Library/Application Support/Koda"
        rm -rf "$HOME/Library/Logs/Koda"

        prompt_remove_preferences
    fi

    rm -rf $HOME/.koda_server
}

main "$@"
