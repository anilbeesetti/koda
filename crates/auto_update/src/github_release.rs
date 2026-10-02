//! GitHub release metadata for timestamp-versioned Apple Silicon fork builds.

use anyhow::{Context as _, Result, ensure};
use release_channel::ReleaseChannel;
use semver::Version;
use serde::Deserialize;

use crate::ReleaseAsset;

const TIMESTAMP_METADATA_PREFIX: &str = "github-release.";

#[derive(Clone)]
pub(crate) struct GitHubReleaseSource {
    pub(crate) repository: String,
    pub(crate) installed_tag: String,
    pub(crate) channel: ReleaseChannel,
}

impl GitHubReleaseSource {
    pub(crate) fn from_env() -> Option<Self> {
        option_env!("ZED_GITHUB_REPOSITORY").map(|repository| Self {
            repository: repository.to_string(),
            installed_tag: option_env!("ZED_RELEASE_VERSION")
                .unwrap_or_default()
                .to_string(),
            channel: *release_channel::RELEASE_CHANNEL,
        })
    }

    pub(crate) fn api_url(&self) -> Result<String> {
        let parts: Vec<_> = self.repository.split('/').collect();
        ensure!(
            parts.len() == 2
                && parts.iter().all(|part| {
                    !part.is_empty()
                        && *part != "."
                        && *part != ".."
                        && part.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                        })
                }),
            "invalid GitHub update repository"
        );
        self.timestamp(&self.installed_tag)?;
        let endpoint = match self.channel {
            ReleaseChannel::Stable => "releases/latest",
            ReleaseChannel::Nightly => "releases?per_page=100&page=1",
            _ => anyhow::bail!("unsupported Koda release channel"),
        };
        Ok(format!(
            "https://api.github.com/repos/{}/{endpoint}",
            self.repository
        ))
    }

    fn timestamp(&self, tag: &str) -> Result<TimestampVersion> {
        let version = match self.channel {
            ReleaseChannel::Nightly => tag
                .strip_prefix("nightly-")
                .context("not a Koda Nightly release tag")?,
            ReleaseChannel::Stable => tag,
            _ => anyhow::bail!("unsupported Koda release channel"),
        };
        TimestampVersion::parse(version)
    }

    pub(crate) fn latest_prerelease_asset(
        &self,
        releases: Vec<GitHubRelease>,
    ) -> Result<Option<ReleaseAsset>> {
        ensure!(
            self.channel == ReleaseChannel::Nightly,
            "not a Koda Nightly update source"
        );
        releases
            .into_iter()
            .filter(|release| {
                !release.draft && release.prerelease && release.published_at.is_some()
            })
            .filter_map(|release| {
                self.timestamp(&release.tag_name)
                    .ok()
                    .map(|version| (version, release))
            })
            .max_by_key(|(version, _)| *version)
            .map(|(_, release)| self.release_asset(release))
            .transpose()
    }

    pub(crate) fn release_notes_url(&self) -> String {
        format!(
            "https://github.com/{}/releases/tag/{}",
            self.repository, self.installed_tag
        )
    }

    pub(crate) fn newer_version(
        &self,
        fetched_tag: &str,
        cached: Option<&Version>,
    ) -> Result<Option<Version>> {
        let installed = self.timestamp(&self.installed_tag)?;
        let current = match cached {
            Some(version) => TimestampVersion::from_status_version(version)
                .context("invalid cached GitHub release version")?,
            None => installed,
        };
        let fetched = self.timestamp(fetched_tag)?;
        Ok((fetched > current).then(|| fetched.status_version()))
    }

    pub(crate) fn release_asset(&self, release: GitHubRelease) -> Result<ReleaseAsset> {
        let (name, version) = if self.channel == ReleaseChannel::Nightly {
            (
                "Koda-Nightly",
                release
                    .tag_name
                    .strip_prefix("nightly-")
                    .context("not a Koda Nightly release tag")?,
            )
        } else {
            ("Koda", release.tag_name.as_str())
        };
        let name = format!("{name}-{version}-macos-aarch64.dmg");
        self.named_asset(release, name)
    }

    pub(crate) fn remote_server_asset(
        &self,
        release: GitHubRelease,
        os: &str,
        arch: &str,
    ) -> Result<ReleaseAsset> {
        ensure!(
            release.tag_name == self.installed_tag,
            "remote server release does not match the installed Koda build"
        );
        ensure!(
            matches!(os, "macos" | "linux" | "windows" | "freebsd")
                && matches!(arch, "aarch64" | "x86_64"),
            "unsupported Koda remote server platform"
        );
        let name = format!("koda-remote-server-{}-{os}-{arch}.gz", self.installed_tag);
        self.named_asset(release, name)
    }

    fn named_asset(&self, release: GitHubRelease, name: String) -> Result<ReleaseAsset> {
        ensure!(
            !release.draft
                && release.prerelease == (self.channel == ReleaseChannel::Nightly)
                && release.published_at.is_some(),
            "GitHub update is not a published release for this Koda channel"
        );
        self.timestamp(&release.tag_name)?;
        let asset = release
            .assets
            .into_iter()
            .find(|asset| asset.name == name)
            .with_context(|| format!("GitHub release {} has no {name}", release.tag_name))?;
        let expected_url = format!(
            "https://github.com/{}/releases/download/{}/{}",
            self.repository, release.tag_name, name
        );
        ensure!(
            asset.browser_download_url == expected_url,
            "GitHub release asset has an unexpected download URL"
        );
        ensure!(
            asset.state == "uploaded" && asset.size > 0,
            "GitHub release asset is not fully uploaded"
        );
        let digest = asset
            .digest
            .context("GitHub release asset has no SHA-256 digest")?;
        let checksum = digest
            .strip_prefix("sha256:")
            .context("unsupported GitHub release asset digest")?;
        ensure!(
            checksum.len() == 64 && checksum.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "invalid GitHub release asset SHA-256 digest"
        );
        Ok(ReleaseAsset {
            version: release.tag_name,
            url: asset.browser_download_url,
            sha256: Some(checksum.to_ascii_lowercase()),
            size: Some(asset.size),
        })
    }
}

#[derive(Deserialize)]
pub(crate) struct GitHubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    published_at: Option<String>,
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    state: String,
    size: u64,
    digest: Option<String>,
}

/// Ordered numeric components, including actual calendar validation. Semver's
/// build metadata cannot order hours/minutes, so never use semver to compare these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TimestampVersion([u64; 5]);

impl TimestampVersion {
    fn parse(tag: &str) -> Result<Self> {
        let parts: Vec<_> = tag.split('.').collect();
        ensure!(
            parts.len() == 5
                && parts
                    .iter()
                    .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())),
            "invalid timestamp release tag: {tag}"
        );
        let mut values = [0; 5];
        for (value, part) in values.iter_mut().zip(parts) {
            *value = part
                .parse()
                .context("timestamp component is out of range")?;
        }
        let [year, month, day, hour, minute] = values;
        ensure!(
            (1..=9999).contains(&year) && (1..=12).contains(&month) && hour < 24 && minute < 60,
            "invalid timestamp release tag: {tag}"
        );
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days = match month {
            2 if leap => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        ensure!(
            (1..=days).contains(&day),
            "invalid timestamp release tag: {tag}"
        );
        Ok(Self(values))
    }

    fn status_version(self) -> Version {
        let [year, month, day, hour, minute] = self.0;
        let mut version = Version::new(year, month, day);
        version.build =
            semver::BuildMetadata::new(&format!("{TIMESTAMP_METADATA_PREFIX}{hour}.{minute}"))
                .expect("numeric timestamp build metadata is valid");
        version
    }

    fn from_status_version(version: &Version) -> Option<Self> {
        let time = version
            .build
            .as_str()
            .strip_prefix(TIMESTAMP_METADATA_PREFIX)?;
        Self::parse(&format!(
            "{}.{}.{}.{}",
            version.major, version.minor, version.patch, time
        ))
        .ok()
    }

    fn display(self) -> String {
        let [year, month, day, hour, minute] = self.0;
        format!("{year:04}.{month:02}.{day:02}.{hour:02}.{minute:02}")
    }
}

pub(crate) fn display_status_version(version: &Version) -> String {
    TimestampVersion::from_status_version(version)
        .map_or_else(|| version.to_string(), TimestampVersion::display)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(tag: &str) -> GitHubReleaseSource {
        GitHubReleaseSource {
            repository: "anilbeesetti/koda".into(),
            installed_tag: tag.into(),
            channel: ReleaseChannel::Stable,
        }
    }

    fn release() -> GitHubRelease {
        serde_json::from_value(serde_json::json!({
            "tag_name": "2026.09.30.14.05", "draft": false, "prerelease": false,
            "published_at": "2026-09-30T08:35:00Z",
            "assets": [{ "name": "Koda-2026.09.30.14.05-macos-aarch64.dmg",
                "browser_download_url": "https://github.com/anilbeesetti/koda/releases/download/2026.09.30.14.05/Koda-2026.09.30.14.05-macos-aarch64.dmg",
                "state": "uploaded", "size": 123, "digest": format!("sha256:{}", "a".repeat(64))
            }]
        })).unwrap()
    }

    fn nightly_source() -> GitHubReleaseSource {
        GitHubReleaseSource {
            repository: "anilbeesetti/koda".into(),
            installed_tag: "nightly-2026.09.30.14.05".into(),
            channel: ReleaseChannel::Nightly,
        }
    }

    fn nightly_release(version: &str) -> GitHubRelease {
        let mut release = release();
        release.tag_name = format!("nightly-{version}");
        release.prerelease = true;
        release.assets[0].name = format!("Koda-Nightly-{version}-macos-aarch64.dmg");
        release.assets[0].browser_download_url = format!(
            "https://github.com/anilbeesetti/koda/releases/download/nightly-{version}/Koda-Nightly-{version}-macos-aarch64.dmg"
        );
        release
    }

    #[test]
    fn nightly_selects_only_the_newest_published_nightly_prerelease() {
        let source = nightly_source();
        assert_eq!(
            source.api_url().unwrap(),
            "https://api.github.com/repos/anilbeesetti/koda/releases?per_page=100&page=1"
        );
        let mut draft = nightly_release("2026.10.01.14.05");
        draft.draft = true;
        let mut unpublished = nightly_release("2026.10.02.14.05");
        unpublished.published_at = None;
        let mut preview = nightly_release("2026.10.03.14.05");
        preview.tag_name = "preview-2026.10.03.14.05".into();
        let mut dev = nightly_release("2026.10.04.14.05");
        dev.tag_name = "dev-2026.10.04.14.05".into();
        let asset = source
            .latest_prerelease_asset(vec![
                nightly_release("2026.09.30.14.06"),
                release(),
                draft,
                unpublished,
                preview,
                dev,
                nightly_release("2026.09.29.14.05"),
            ])
            .unwrap()
            .unwrap();
        assert_eq!(asset.version, "nightly-2026.09.30.14.06");
        assert!(
            source
                .latest_prerelease_asset(vec![release()])
                .unwrap()
                .is_none()
        );
        let newer = source.newer_version(&asset.version, None).unwrap().unwrap();
        assert_eq!(display_status_version(&newer), "2026.09.30.14.06");
        assert!(
            source
                .newer_version(&asset.version, Some(&newer))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn stable_and_nightly_releases_cannot_cross_channels() {
        let stable = source("2026.09.30.14.05");
        let nightly = nightly_source();
        assert!(
            stable
                .release_asset(nightly_release("2026.09.30.14.06"))
                .is_err()
        );
        assert!(nightly.release_asset(release()).is_err());
        assert!(
            stable
                .newer_version("nightly-2026.09.30.14.06", None)
                .is_err()
        );
        assert!(nightly.newer_version("2026.09.30.14.06", None).is_err());
        assert!(nightly.newer_version("dev-2026.09.30.14.06", None).is_err());
        let mut wrong_asset = nightly_release("2026.09.30.14.06");
        wrong_asset.assets[0].name = "Koda-2026.09.30.14.06-macos-aarch64.dmg".into();
        assert!(nightly.release_asset(wrong_asset).is_err());
    }

    #[test]
    fn nightly_remote_servers_are_pinned_to_the_installed_prerelease() {
        let source = nightly_source();
        let mut remote = nightly_release("2026.09.30.14.05");
        let name = "koda-remote-server-nightly-2026.09.30.14.05-macos-aarch64.gz";
        remote.assets[0].name = name.into();
        remote.assets[0].browser_download_url = format!(
            "https://github.com/anilbeesetti/koda/releases/download/nightly-2026.09.30.14.05/{name}"
        );
        assert!(
            source
                .remote_server_asset(remote, "macos", "aarch64")
                .is_ok()
        );
        assert!(
            source
                .remote_server_asset(nightly_release("2026.09.30.14.06"), "macos", "aarch64")
                .is_err()
        );
    }

    #[test]
    fn compares_every_numeric_component() {
        for (old, new) in [
            ("2026.9.30.14.5", "2026.9.30.14.6"),
            ("2026.9.30.9.59", "2026.9.30.10.0"),
            ("2026.9.30.23.59", "2026.10.1.0.0"),
            ("2026.12.31.23.59", "2027.1.1.0.0"),
        ] {
            let newer = source(old).newer_version(new, None).unwrap().unwrap();
            assert_eq!(
                display_status_version(&newer),
                TimestampVersion::parse(new).unwrap().display()
            );
            assert!(source(new).newer_version(old, None).unwrap().is_none());
            assert!(source(old).newer_version(old, None).unwrap().is_none());
            assert!(
                source(old)
                    .newer_version(new, Some(&newer))
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn rejects_malformed_dates_and_validates_leap_years() {
        for tag in [
            "v2026.09.30.14.05",
            "0.09.30.14.05",
            "2026.09.30.14",
            "2026.13.1.0.0",
            "2026.2.29.0.0",
            "2026.4.31.0.0",
            "2026.9.0.0.0",
            "2026.9.30.24.0",
            "2026.9.30.0.60",
            "2026.9.30.-1.0",
            "2026.9.30.1.0+build",
            "2026..30.1.0",
        ] {
            assert!(TimestampVersion::parse(tag).is_err(), "{tag}");
        }
        assert!(TimestampVersion::parse("2028.2.29.0.0").is_ok());
        assert!(TimestampVersion::parse("2100.2.29.0.0").is_err());
        assert!(TimestampVersion::parse("2000.2.29.0.0").is_ok());
    }

    #[test]
    fn selects_only_published_apple_silicon_asset() {
        let source = source("2026.09.29.14.05");
        let asset = source.release_asset(release()).unwrap();
        assert_eq!(asset.version, "2026.09.30.14.05");
        assert_eq!(asset.size, Some(123));
        assert_eq!(asset.sha256, Some("a".repeat(64)));
        for change in 0..8 {
            let mut release = release();
            match change {
                0 => release.draft = true,
                1 => release.prerelease = true,
                2 => release.published_at = None,
                3 => release.assets[0].name = "Koda-2026.09.30.14.05-macos-x86_64.dmg".into(),
                4 => {
                    release.assets[0].browser_download_url = "https://example.com/update.dmg".into()
                }
                5 => release.assets[0].state = "new".into(),
                6 => release.assets[0].digest = None,
                _ => release.assets[0].digest = Some("sha256:bad".into()),
            }
            assert!(source.release_asset(release).is_err());
        }
    }

    #[test]
    fn remote_servers_require_the_installed_koda_release() {
        let source = source("2026.09.30.14.05");
        let remote = || {
            let mut release = release();
            let name = "koda-remote-server-2026.09.30.14.05-macos-aarch64.gz";
            release.assets[0].name = name.into();
            release.assets[0].browser_download_url = format!(
                "https://github.com/anilbeesetti/koda/releases/download/2026.09.30.14.05/{name}"
            );
            release
        };
        assert!(
            source
                .remote_server_asset(remote(), "macos", "aarch64")
                .is_ok()
        );
        assert!(
            source
                .remote_server_asset(remote(), "linux", "x86_64")
                .is_err()
        );
        let mut mismatched = remote();
        mismatched.tag_name = "2026.09.30.14.06".into();
        assert!(
            source
                .remote_server_asset(mismatched, "macos", "aarch64")
                .is_err()
        );
        let mut upstream = release();
        upstream.assets[0].name = "Zed-2026.09.30.14.05-macos-aarch64.dmg".into();
        assert!(source.release_asset(upstream).is_err());
    }

    #[test]
    fn validates_source_and_preserves_upstream_status_display() {
        assert_eq!(
            source("2026.09.30.14.05").api_url().unwrap(),
            "https://api.github.com/repos/anilbeesetti/koda/releases/latest"
        );
        assert_eq!(
            source("2026.09.30.14.05").release_notes_url(),
            "https://github.com/anilbeesetti/koda/releases/tag/2026.09.30.14.05"
        );
        for repository in ["", "owner", "../zed", "owner/repo/other", "owner/repo?x=1"] {
            let mut source = source("2026.09.30.14.05");
            source.repository = repository.into();
            assert!(source.api_url().is_err());
        }
        assert!(source("").api_url().is_err());
        let version: Version = "0.225.0+nightly.abcdef".parse().unwrap();
        assert_eq!(display_status_version(&version), version.to_string());
    }
}
