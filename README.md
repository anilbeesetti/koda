# Koda

Koda is an Android-focused IDE fork of [Zed](https://github.com/zed-industries/zed).
It uses its own app identity (`dev.anilbeesetti.koda`), `koda` CLI, settings,
extensions, databases, caches, logs, keychain entries, and `koda://` URL handler.
The app uses an angular K monogram inspired by Zed’s outlined logo.
Icon sources: `assets/branding/koda-icon.png` and `koda-icon-nightly.png`; regenerate
platform assets with `script/generate-koda-icons` (requires ImageMagick).
Koda Nightly uses a blue NIGHTLY icon and the bundle identifier
`dev.anilbeesetti.koda-nightly`, so both apps can be installed together.

Koda stores configuration in `~/.config/koda` on macOS and Linux, app data in
`~/Library/Application Support/Koda` on macOS or `$XDG_DATA_HOME/koda` on Linux,
and configuration/data in `%APPDATA%\Koda` / `%LOCALAPPDATA%\Koda` on Windows.
Nightly uses `~/.config/koda-nightly`, `~/Library/Application Support/Koda Nightly`
on macOS, `$XDG_DATA_HOME/koda-nightly` on Linux, and
`%APPDATA%\Koda Nightly` / `%LOCALAPPDATA%\Koda Nightly` on Windows. Its caches
and logs are also separate from stable.
Project settings, tasks, and Android tooling caches live in `.koda/`. Existing
Zed settings and `.zed/` project files are left in place; copy selected settings
into Koda's directories if you want to reuse them.

### Installation

Download the Apple Silicon DMG from [this fork's releases](https://github.com/anilbeesetti/koda/releases)
and drag `Koda.app` into Applications. Install the `koda` command using the
app's **Install CLI** action. Stable builds update from this fork's latest stable
release. For Nightly, download the `Koda-Nightly` DMG from a prerelease and install
`Koda Nightly.app`; its updater follows only published `nightly-` prereleases.
Unconfigured local development builds require manual installation.
Remote servers also come from the matching fork release and require an asset for
the target platform, or a custom server build during development.

For the isolated macOS Android Nightly app, run `script/android-ide`. Its local
bundle and profile live under `target/android-ide/nightly/`.
For a direct debug build, run `cargo build --locked -p zed --bin koda` and
launch `target/debug/koda`. Rust crate names and action namespaces remain
compatible with upstream Zed.
Signed macOS builds require this fork's certificate and `MACOS_SIGNING_IDENTITY`
(also configurable as a GitHub Actions secret); Koda bundles do not embed Zed's
provisioning profiles.

### Releases

Stable releases require manually running **Release Koda (stable)** on `main`.
**Release Koda Nightly (prerelease)** runs daily at midnight in Asia/Calcutta
(18:30 UTC) and can also be run manually on `main`. GitHub may delay scheduled
runs. It skips the build when `main` matches the commit behind the latest
published nightly tag; drafts and failed builds do not count as a published
nightly. Merging to `main` does not immediately publish a release.

Both workflows currently build Apple Silicon macOS downloads. Stable tags use
`year.month.day.hour.minute` in Asia/Calcutta; Nightly tags use
`nightly-year.month.day.hour.minute`. Nightlies are marked as prereleases and
leave GitHub's latest stable release unchanged. Failed runs can be retried with
their reserved tag.

### Upstream development documentation

- [Building Zed for macOS](./docs/src/development/macos.md)
- [Building Zed for Linux](./docs/src/development/linux.md)
- [Building Zed for Windows](./docs/src/development/windows.md)

### Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for ways you can contribute to Zed.

### Licensing

Zed source code is licensed primarily under GPL-3.0-or-later, with Apache-2.0 components where marked.

License information for third party dependencies must be correctly provided for CI to pass.

We use [`cargo-about`](https://github.com/EmbarkStudios/cargo-about) to automatically comply with open source licenses. If CI is failing, check the following:

- Is it showing a `no license specified` error for a crate you've created? If so, add `publish = false` under `[package]` in your crate's Cargo.toml.
- Is the error `failed to satisfy license requirements` for a dependency? If so, first determine what license the project has and whether this system is sufficient to comply with this license's requirements. If you're unsure, ask a lawyer. Once you've verified that this system is acceptable add the license's SPDX identifier to the `accepted` array in `script/licenses/zed-licenses.toml`.
- Is `cargo-about` unable to find the license for a dependency? If so, add a clarification field at the end of `script/licenses/zed-licenses.toml`, as specified in the [cargo-about book](https://embarkstudios.github.io/cargo-about/cli/generate/config.html#crate-configuration).

## Sponsorship

Zed is developed by **Zed Industries, Inc.**, a for-profit company.

If you’d like to financially support the project, you can do so via GitHub Sponsors.
Sponsorships go directly to Zed Industries and are used as general company revenue.
There are no perks or entitlements associated with sponsorship.
