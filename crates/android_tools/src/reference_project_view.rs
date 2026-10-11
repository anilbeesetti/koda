// Preparation behavior is adapted from the Android Open Source Project,
// tools/adt/idea revision a84efec3ba9542d9bfa1255103f0dc94833a3796:
// AndroidGradleTests.java (Copyright 2017), BuildEnvironment.java (Copyright 2016),
// and AgpVersionSoftwareEnvironment.kt (Copyright 2022), licensed under Apache 2.0.
// The original notices, fixtures and license remain in test_data/project_tree.
// SPDX-License-Identifier: Apache-2.0

//! Authenticated inputs and a preparation plan for the original project-view tests.
//!
//! This module performs no filesystem writes, import, Gradle execution or UI checks.
//! Artifact bindings are supplied by the caller; a plan does not establish that a
//! runtime artifact is installed or compatible. The six original workflow tests
//! remain separate obligations. Generated Java Properties and wrapper files are
//! typed operations rather than invented byte hashes for files not yet produced.

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;

const ORIGINAL_REVISION: &str = "a84efec3ba9542d9bfa1255103f0dc94833a3796";
const PROVENANCE_SHA256: &str = "4712e7ea212fc5ceead1d8abe7af35a0eeef2358152b5f8926970e0a60a0d67e";
const LICENSE_SHA256: &str = "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30";
const FIXTURE_PREFIX: &str = "simpleApplication/";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ReferencePath(String);

impl ReferencePath {
    pub fn parse(path: &str) -> Result<Self> {
        ensure!(
            !path.is_empty()
                && !path.contains(['\\', ':'])
                && !path.chars().any(char::is_control)
                && path.split('/').all(|part| !matches!(part, "" | "." | "..")),
            "Reference path must be a confined, portable relative path: {path:?}"
        );
        Ok(Self(path.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReferenceFileDigest {
    pub bytes: usize,
    pub sha256: String,
}

fn digest(bytes: &[u8]) -> ReferenceFileDigest {
    ReferenceFileDigest {
        bytes: bytes.len(),
        sha256: format!("{:x}", Sha256::digest(bytes)),
    }
}

#[derive(Deserialize)]
struct OriginalInventory {
    schema_version: u32,
    repository: String,
    revision: String,
    release: String,
    fixtures: Vec<OriginalEntry>,
    sources: Vec<OriginalEntry>,
}

#[derive(Deserialize)]
struct OriginalEntry {
    local_path: String,
    source_path: String,
    bytes: usize,
    sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OriginalVerificationReceipt {
    pub revision: String,
    pub fixture_count: usize,
    pub source_count: usize,
    pub inputs: BTreeMap<ReferencePath, ReferenceFileDigest>,
}

pub struct ReferenceFixtureManifest;

/// Constructed only after all 26 fixtures, 22 sources, provenance and license match.
pub struct VerifiedReferenceFixture {
    files: BTreeMap<ReferencePath, Vec<u8>>,
    receipt: OriginalVerificationReceipt,
}

impl ReferenceFixtureManifest {
    /// The reader must return the named original bytes. Filesystem readers remain
    /// responsible for rejecting symlinks and enforcing physical root ownership.
    pub fn verify_originals(
        mut read: impl FnMut(&ReferencePath) -> Result<Vec<u8>>,
    ) -> Result<VerifiedReferenceFixture> {
        let provenance_path = ReferencePath::parse("provenance.json")?;
        let provenance = read(&provenance_path)?;
        let provenance_digest = digest(&provenance);
        ensure!(
            provenance_digest.bytes == 35999 && provenance_digest.sha256 == PROVENANCE_SHA256,
            "Original provenance does not match the pinned reference"
        );
        let inventory: OriginalInventory = serde_json::from_slice(&provenance)?;
        ensure!(
            inventory.schema_version == 1
                && inventory.repository
                    == "https://android.googlesource.com/platform/tools/adt/idea"
                && inventory.revision == ORIGINAL_REVISION
                && inventory.release == "studio-2026.2.1"
                && inventory.fixtures.len() == 26
                && inventory.sources.len() == 22,
            "Unsupported original reference inventory"
        );
        let license_path = ReferencePath::parse("LICENSE-APACHE")?;
        let license = read(&license_path)?;
        let license_digest = digest(&license);
        ensure!(
            license_digest.bytes == 11358 && license_digest.sha256 == LICENSE_SHA256,
            "Original Apache license does not match the pinned reference"
        );
        let mut inputs = BTreeMap::from([
            (provenance_path, provenance_digest),
            (license_path, license_digest),
        ]);
        let mut files = BTreeMap::new();
        for (is_fixture, entries) in [(true, inventory.fixtures), (false, inventory.sources)] {
            for entry in entries {
                let path = ReferencePath::parse(&entry.local_path)?;
                ReferencePath::parse(&entry.source_path)?;
                ensure!(
                    path.as_str().starts_with(if is_fixture {
                        FIXTURE_PREFIX
                    } else {
                        "reference/"
                    }),
                    "Original entry is in the wrong inventory: {}",
                    path.as_str()
                );
                ensure!(
                    !inputs.contains_key(&path),
                    "Duplicate original reference path"
                );
                let bytes =
                    read(&path).with_context(|| format!("Read original {}", path.as_str()))?;
                let actual = digest(&bytes);
                ensure!(
                    actual.bytes == entry.bytes && actual.sha256 == entry.sha256,
                    "Original reference bytes changed: {}",
                    path.as_str()
                );
                inputs.insert(path.clone(), actual);
                if is_fixture {
                    files.insert(path, bytes);
                }
            }
        }
        Ok(VerifiedReferenceFixture {
            files,
            receipt: OriginalVerificationReceipt {
                revision: ORIGINAL_REVISION.to_owned(),
                fixture_count: 26,
                source_count: 22,
                inputs,
            },
        })
    }
}

impl VerifiedReferenceFixture {
    pub fn receipt(&self) -> &OriginalVerificationReceipt {
        &self.receipt
    }

    pub fn original(&self, path: &ReferencePath) -> Result<&[u8]> {
        self.files
            .get(path)
            .map(Vec::as_slice)
            .context("Unknown original fixture file")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReferenceEvidence {
    pub source_identity: String,
    pub sha256: String,
}

impl ReferenceEvidence {
    fn validate(&self) -> Result<()> {
        ensure!(
            !self.source_identity.trim().is_empty()
                && !self.source_identity.chars().any(char::is_control)
                && self.sha256.len() == 64
                && self
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                && self.sha256.bytes().any(|byte| byte != b'0'),
            "Missing or invalid resolved reference evidence"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReferenceResolvedValue<T> {
    pub value: T,
    pub evidence: ReferenceEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReferenceArtifactBinding {
    pub absolute_path: String,
    pub digest: ReferenceFileDigest,
    pub evidence: ReferenceEvidence,
}

impl ReferenceArtifactBinding {
    fn validate(&self) -> Result<()> {
        validate_absolute_path(&self.absolute_path)?;
        self.evidence.validate()?;
        ensure!(self.digest.bytes > 0, "Missing reference runtime artifact");
        ReferenceEvidence {
            source_identity: self.absolute_path.clone(),
            sha256: self.digest.sha256.clone(),
        }
        .validate()
    }
}

/// Every field is explicit; no SDK search, environment variable, compiled version
/// inference or substitution of the independent smoke-test tool tuple occurs here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReferenceEnvironmentInputs {
    pub agp: ReferenceResolvedValue<String>,
    pub latest_known_agp: ReferenceResolvedValue<String>,
    pub gradle: ReferenceResolvedValue<String>,
    pub kotlin: ReferenceResolvedValue<String>,
    pub embedded_jdk_major: ReferenceResolvedValue<u32>,
    pub embedded_jdk_path: ReferenceResolvedValue<String>,
    pub sdk_path: ReferenceResolvedValue<String>,
    pub compile_sdk: ReferenceResolvedValue<u32>,
    pub target_sdk: ReferenceResolvedValue<u32>,
    pub build_tools: ReferenceResolvedValue<String>,
    pub lowest_active_api: ReferenceResolvedValue<u32>,
    pub built_in_kotlin: ReferenceResolvedValue<bool>,
    pub parallel_sync: ReferenceResolvedValue<bool>,
    pub debug_inner_test_disabled: ReferenceResolvedValue<bool>,
    pub running_from_bazel: ReferenceResolvedValue<bool>,
    pub workspace_output_repository_exists: ReferenceResolvedValue<bool>,
    /// Java File.toURI() values, in the original helper's order. The caller must
    /// bind the actual directories and encode reserved path characters beforehand.
    pub offline_repository_uri: ReferenceResolvedValue<String>,
    pub workspace_output_repository_uri: Option<ReferenceResolvedValue<String>>,
    pub additional_repository_uris: Vec<ReferenceResolvedValue<String>>,
    pub gradle_distribution: ReferenceArtifactBinding,
    pub wrapper_jar: ReferenceArtifactBinding,
    pub wrapper_unix_script: ReferenceArtifactBinding,
    pub wrapper_windows_script: ReferenceArtifactBinding,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResolvedReferenceEnvironment {
    inputs: ReferenceEnvironmentInputs,
}

impl ResolvedReferenceEnvironment {
    /// Supports the pinned AGP_CURRENT descriptor with its default resolved
    /// Gradle/Kotlin versions. Other descriptors require a separately reviewed port.
    pub fn resolve(inputs: Option<ReferenceEnvironmentInputs>) -> Result<Self> {
        let inputs = inputs.context("The original reference environment is unresolved")?;
        for evidence in [
            &inputs.agp.evidence,
            &inputs.latest_known_agp.evidence,
            &inputs.gradle.evidence,
            &inputs.kotlin.evidence,
            &inputs.embedded_jdk_major.evidence,
            &inputs.embedded_jdk_path.evidence,
            &inputs.sdk_path.evidence,
            &inputs.compile_sdk.evidence,
            &inputs.target_sdk.evidence,
            &inputs.build_tools.evidence,
            &inputs.lowest_active_api.evidence,
            &inputs.built_in_kotlin.evidence,
            &inputs.parallel_sync.evidence,
            &inputs.debug_inner_test_disabled.evidence,
            &inputs.running_from_bazel.evidence,
            &inputs.workspace_output_repository_exists.evidence,
            &inputs.offline_repository_uri.evidence,
        ] {
            evidence.validate()?;
        }
        let agp_major = numeric_major(&inputs.agp.value)?;
        ensure!(
            agp_major >= 8
                && inputs.agp.value == inputs.latest_known_agp.value
                && inputs.gradle.value == "9.6.0"
                && inputs.kotlin.value == "2.4.20-RC3"
                && inputs.compile_sdk.value == 34
                && inputs.target_sdk.value == 34
                && inputs.build_tools.value == "36.0.0"
                && inputs.lowest_active_api.value == 16
                && inputs.embedded_jdk_major.value >= 17
                && inputs.debug_inner_test_disabled.value
                && inputs.built_in_kotlin.value == (agp_major >= 9),
            "Unsupported pinned reference environment or unresolved original flags"
        );
        validate_absolute_path(&inputs.embedded_jdk_path.value)?;
        validate_absolute_path(&inputs.sdk_path.value)?;
        validate_repository_uri(&inputs.offline_repository_uri)?;
        let use_output_repository =
            !inputs.running_from_bazel.value && inputs.workspace_output_repository_exists.value;
        ensure!(
            inputs.workspace_output_repository_uri.is_some() == use_output_repository,
            "Workspace output repository does not match the original Bazel/existence decision"
        );
        if let Some(repository) = &inputs.workspace_output_repository_uri {
            validate_repository_uri(repository)?;
        }
        for repository in &inputs.additional_repository_uris {
            validate_repository_uri(repository)?;
        }
        for artifact in [
            &inputs.gradle_distribution,
            &inputs.wrapper_jar,
            &inputs.wrapper_unix_script,
            &inputs.wrapper_windows_script,
        ] {
            artifact.validate()?;
        }
        Ok(Self { inputs })
    }

    pub fn inputs(&self) -> &ReferenceEnvironmentInputs {
        &self.inputs
    }
}

fn numeric_major(version: &str) -> Result<u32> {
    let pattern =
        regex::Regex::new(r"^([0-9]+)\.[0-9]+\.[0-9]+(?:-(?:alpha|beta|rc|dev)[0-9]*)?$")?;
    let matched = pattern
        .captures(version)
        .context("Unresolved or invalid original AGP version")?;
    matched
        .get(1)
        .context("Original AGP version has no numeric major")?
        .as_str()
        .parse()
        .context("Original AGP major version exceeds the supported range")
}

fn validate_absolute_path(path: &str) -> Result<()> {
    // This first support slice consumes Unix artifact paths. Windows path/URI
    // preparation is an explicit blocker rather than a host-dependent guess.
    ensure!(
        path.starts_with('/')
            && path.len() > 1
            && !path.contains('\\')
            && !path.chars().any(char::is_control)
            && path
                .split('/')
                .skip(1)
                .all(|part| !matches!(part, "" | "." | "..")),
        "Unsupported or unconfined reference artifact path"
    );
    Ok(())
}

fn validate_repository_uri(repository: &ReferenceResolvedValue<String>) -> Result<()> {
    repository.evidence.validate()?;
    let path = repository
        .value
        .strip_prefix("file:/")
        .context("Only original local Maven repository URIs are supported")?;
    ensure!(
        !path.is_empty()
            && !path.starts_with('/')
            && path.ends_with('/')
            && path.bytes().all(|byte| byte.is_ascii_alphanumeric()
                || matches!(byte, b'/' | b'%' | b'-' | b'_' | b'.' | b'~')),
        "Unsupported or unsafe original repository URI"
    );
    let mut characters = path.bytes();
    let mut decoded = Vec::with_capacity(path.len());
    while let Some(character) = characters.next() {
        if character == b'%' {
            let first = characters
                .next()
                .context("Incomplete encoded repository URI")?;
            let second = characters
                .next()
                .context("Incomplete encoded repository URI")?;
            let first = char::from(first)
                .to_digit(16)
                .context("Invalid encoded repository URI")?;
            let second = char::from(second)
                .to_digit(16)
                .context("Invalid encoded repository URI")?;
            let character = u8::try_from(first * 16 + second)?;
            ensure!(
                !matches!(character, b'/' | b'\\'),
                "Encoded repository path separator"
            );
            decoded.push(character);
        } else {
            decoded.push(character);
        }
    }
    let decoded =
        std::str::from_utf8(&decoded).context("Repository URI does not encode a UTF-8 path")?;
    ensure!(
        !decoded.chars().any(char::is_control)
            && decoded
                .trim_end_matches('/')
                .split('/')
                .all(|part| !matches!(part, "" | "." | "..")),
        "Unconfined repository URI"
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ReferenceRule {
    JavaGradleProjectRule,
    KotlinCoreProjectRule,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReferenceFilePatch {
    pub project_relative_path: ReferencePath,
    pub original: ReferenceFileDigest,
    pub prepared: ReferenceFileDigest,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum ReferencePreparationOperation {
    EnsureDirectory {
        project_relative_path: ReferencePath,
    },
    SetLocalProperties {
        sdk_path: String,
    },
    UpdateGradleProperties {
        original: ReferenceFileDigest,
        assignments: BTreeMap<String, String>,
    },
    CreateGradleWrapper {
        gradle_version: String,
        distribution: ReferenceArtifactBinding,
        jar: ReferenceArtifactBinding,
        unix_script: ReferenceArtifactBinding,
        windows_script: ReferenceArtifactBinding,
    },
    ReplaceFixtureFile(ReferenceFilePatch),
    /// Both original loaders migrate only when needed. This authenticated
    /// fixture's manifest already has no package attribute; no edit is warranted.
    ManifestPackageMigration {
        manifest: ReferenceFileDigest,
        already_migrated: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReferencePreparationPlan {
    pub rule: ReferenceRule,
    pub original: OriginalVerificationReceipt,
    pub environment: ResolvedReferenceEnvironment,
    pub helper_members: BTreeMap<String, ReferenceFileDigest>,
    pub operations: Vec<ReferencePreparationOperation>,
}

impl ReferencePreparationPlan {
    pub fn prepare_java_rule(
        fixture: &VerifiedReferenceFixture,
        environment: &ResolvedReferenceEnvironment,
    ) -> Result<Self> {
        Self::prepare(fixture, environment, ReferenceRule::JavaGradleProjectRule)
    }

    pub fn prepare_core_rule(
        fixture: &VerifiedReferenceFixture,
        environment: &ResolvedReferenceEnvironment,
    ) -> Result<Self> {
        Self::prepare(fixture, environment, ReferenceRule::KotlinCoreProjectRule)
    }

    fn prepare(
        fixture: &VerifiedReferenceFixture,
        environment: &ResolvedReferenceEnvironment,
        rule: ReferenceRule,
    ) -> Result<Self> {
        let inputs = environment.inputs();
        let mut assignments = BTreeMap::from([
            (
                "org.gradle.jvmargs".to_owned(),
                " -XX:MaxMetaspaceSize=768m ".to_owned(),
            ),
            ("org.gradle.vfs.watch".to_owned(), "false".to_owned()),
        ]);
        if inputs.parallel_sync.value {
            assignments.insert("org.gradle.tooling.parallel".to_owned(), "true".to_owned());
        }
        let mut operations = vec![
            ReferencePreparationOperation::EnsureDirectory {
                project_relative_path: ReferencePath::parse(".gradle")?,
            },
            ReferencePreparationOperation::SetLocalProperties {
                sdk_path: inputs.sdk_path.value.clone(),
            },
            ReferencePreparationOperation::UpdateGradleProperties {
                original: digest(fixture.original(&ReferencePath::parse(
                    "simpleApplication/gradle.properties",
                )?)?),
                assignments,
            },
            ReferencePreparationOperation::CreateGradleWrapper {
                gradle_version: inputs.gradle.value.clone(),
                distribution: inputs.gradle_distribution.clone(),
                jar: inputs.wrapper_jar.clone(),
                unix_script: inputs.wrapper_unix_script.clone(),
                windows_script: inputs.wrapper_windows_script.clone(),
            },
        ];
        let repositories = [
            Some(&inputs.offline_repository_uri),
            inputs.workspace_output_repository_uri.as_ref(),
        ]
        .into_iter()
        .flatten()
        .chain(inputs.additional_repository_uris.iter())
        .map(|repository| groovy_repository(&repository.value))
        .collect::<Vec<_>>()
        .join("\n");
        for (path, bytes) in &fixture.files {
            if path.as_str().ends_with(".gradle") {
                let original =
                    std::str::from_utf8(bytes).context("Original Gradle fixture is not UTF-8")?;
                let prepared = patch_original_gradle(original, inputs, &repositories);
                if prepared.as_bytes() != bytes {
                    operations.push(ReferencePreparationOperation::ReplaceFixtureFile(
                        ReferenceFilePatch {
                            project_relative_path: ReferencePath::parse(
                                path.as_str()
                                    .strip_prefix(FIXTURE_PREFIX)
                                    .context("Fixture path escaped its project")?,
                            )?,
                            original: digest(bytes),
                            prepared: digest(prepared.as_bytes()),
                            bytes: prepared.into_bytes(),
                        },
                    ));
                }
            }
        }
        operations.push(ReferencePreparationOperation::ManifestPackageMigration {
            manifest: digest(fixture.original(&ReferencePath::parse(
                "simpleApplication/app/src/main/AndroidManifest.xml",
            )?)?),
            already_migrated: true,
        });
        Ok(Self {
            rule,
            original: fixture.receipt.clone(),
            environment: environment.clone(),
            helper_members: helper_members(),
            operations,
        })
    }
}

fn patch_original_gradle(
    original: &str,
    inputs: &ReferenceEnvironmentInputs,
    repositories: &str,
) -> String {
    // Input authentication limits this to the exact Groovy SIMPLE_APPLICATION
    // files. These substitutions implement the matching groups in the original
    // helper; they are not a general Gradle parser or an alternative model decoder.
    let mut prepared = original.replace(
        "com.android.tools.build:gradle:1.5.0",
        &format!("com.android.tools.build:gradle:{}", inputs.agp.value),
    );
    prepared = prepared.replace(
        "buildToolsVersion \"23.0.2\"",
        &format!("buildToolsVersion \"{}\"", inputs.build_tools.value),
    );
    prepared = prepared.replace(
        "compileSdkVersion 23",
        &format!("compileSdkVersion {}", inputs.compile_sdk.value),
    );
    prepared = prepared.replace(
        "targetSdkVersion 23",
        &format!("targetSdkVersion {}", inputs.target_sdk.value),
    );
    prepared = prepared.replace(
        "repositories {",
        &format!("repositories {{\n{repositories}"),
    );
    prepared
}

fn groovy_repository(uri: &str) -> String {
    format!(
        "maven {{\n  url = \"{uri}\"\n  try {{\n    metadataSources() {{\n      mavenPom()\n      artifact()\n    }}\n  }} catch (Throwable ignored) {{ /* In case this Gradle version does not support this. */}}\n}}"
    )
}

fn helper_members() -> BTreeMap<String, ReferenceFileDigest> {
    [
        ("android-test-framework/testSrc/com/android/tools/idea/testing/AndroidGradleTests.java", 45131, "6f1a9666f8617b042ec873590571dc63fe8123d63017533ca5e4ac60ee648374"),
        ("android-test-framework/testSrc/com/android/tools/idea/testing/BuildEnvironment.java", 2274, "472dc55e5fa35e5ddb87b208f4dfb4ec76fac00df92411ee108566cbdddbb6a1"),
        ("android-test-framework/testSrc/com/android/tools/idea/testing/AgpVersionSoftwareEnvironment.kt", 7134, "2d5c34a4b2d5a913a6f2a4bcbe8bcf5b7aa866927f05b6508dd9cbc276e18fe8"),
    ].into_iter().map(|(path, bytes, sha256)| (path.to_owned(), ReferenceFileDigest { bytes, sha256: sha256.to_owned() })).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn original_inputs() -> Vec<(&'static str, &'static [u8])> {
        vec![
            (
                "simpleApplication/.gitignore",
                include_bytes!("../test_data/project_tree/simpleApplication/.gitignore") as &[u8],
            ),
            (
                "simpleApplication/app/.gitignore",
                include_bytes!("../test_data/project_tree/simpleApplication/app/.gitignore")
                    as &[u8],
            ),
            (
                "simpleApplication/app/build.gradle",
                include_bytes!("../test_data/project_tree/simpleApplication/app/build.gradle")
                    as &[u8],
            ),
            (
                "simpleApplication/app/proguard-rules.pro",
                include_bytes!("../test_data/project_tree/simpleApplication/app/proguard-rules.pro")
                    as &[u8],
            ),
            (
                "simpleApplication/app/src/androidTest/java/google/simpleapplication/ApplicationTest.java",
                include_bytes!(
                    "../test_data/project_tree/simpleApplication/app/src/androidTest/java/google/simpleapplication/ApplicationTest.java"
                ) as &[u8],
            ),
            (
                "simpleApplication/app/src/main/AndroidManifest.xml",
                include_bytes!(
                    "../test_data/project_tree/simpleApplication/app/src/main/AndroidManifest.xml"
                ) as &[u8],
            ),
            (
                "simpleApplication/app/src/main/java/google/simpleapplication/MyActivity.java",
                include_bytes!(
                    "../test_data/project_tree/simpleApplication/app/src/main/java/google/simpleapplication/MyActivity.java"
                ) as &[u8],
            ),
            (
                "simpleApplication/app/src/main/res/drawable/ic_launcher.xml",
                include_bytes!(
                    "../test_data/project_tree/simpleApplication/app/src/main/res/drawable/ic_launcher.xml"
                ) as &[u8],
            ),
            (
                "simpleApplication/app/src/main/res/layout/absolute.xml",
                include_bytes!(
                    "../test_data/project_tree/simpleApplication/app/src/main/res/layout/absolute.xml"
                ) as &[u8],
            ),
            (
                "simpleApplication/app/src/main/res/layout/activity_my.xml",
                include_bytes!(
                    "../test_data/project_tree/simpleApplication/app/src/main/res/layout/activity_my.xml"
                ) as &[u8],
            ),
            (
                "simpleApplication/app/src/main/res/layout/frames.xml",
                include_bytes!(
                    "../test_data/project_tree/simpleApplication/app/src/main/res/layout/frames.xml"
                ) as &[u8],
            ),
            (
                "simpleApplication/app/src/main/res/menu/my.xml",
                include_bytes!(
                    "../test_data/project_tree/simpleApplication/app/src/main/res/menu/my.xml"
                ) as &[u8],
            ),
            (
                "simpleApplication/app/src/main/res/values/dimens.xml",
                include_bytes!(
                    "../test_data/project_tree/simpleApplication/app/src/main/res/values/dimens.xml"
                ) as &[u8],
            ),
            (
                "simpleApplication/app/src/main/res/values/strings.xml",
                include_bytes!(
                    "../test_data/project_tree/simpleApplication/app/src/main/res/values/strings.xml"
                ) as &[u8],
            ),
            (
                "simpleApplication/app/src/main/res/values/styles.xml",
                include_bytes!(
                    "../test_data/project_tree/simpleApplication/app/src/main/res/values/styles.xml"
                ) as &[u8],
            ),
            (
                "simpleApplication/app/src/main/res/values-en/strings.xml",
                include_bytes!(
                    "../test_data/project_tree/simpleApplication/app/src/main/res/values-en/strings.xml"
                ) as &[u8],
            ),
            (
                "simpleApplication/app/src/main/res/values-en-rGB/strings.xml",
                include_bytes!(
                    "../test_data/project_tree/simpleApplication/app/src/main/res/values-en-rGB/strings.xml"
                ) as &[u8],
            ),
            (
                "simpleApplication/app/src/main/res/values-ta/strings.xml",
                include_bytes!(
                    "../test_data/project_tree/simpleApplication/app/src/main/res/values-ta/strings.xml"
                ) as &[u8],
            ),
            (
                "simpleApplication/app/src/main/res/values-v19/styles.xml",
                include_bytes!(
                    "../test_data/project_tree/simpleApplication/app/src/main/res/values-v19/styles.xml"
                ) as &[u8],
            ),
            (
                "simpleApplication/app/src/main/res/values-v20/styles.xml",
                include_bytes!(
                    "../test_data/project_tree/simpleApplication/app/src/main/res/values-v20/styles.xml"
                ) as &[u8],
            ),
            (
                "simpleApplication/app/src/main/res/values-w820dp/dimens.xml",
                include_bytes!(
                    "../test_data/project_tree/simpleApplication/app/src/main/res/values-w820dp/dimens.xml"
                ) as &[u8],
            ),
            (
                "simpleApplication/app/src/main/res/values-zh-rCN/strings.xml",
                include_bytes!(
                    "../test_data/project_tree/simpleApplication/app/src/main/res/values-zh-rCN/strings.xml"
                ) as &[u8],
            ),
            (
                "simpleApplication/app/src/test/java/google/simpleapplication/UnitTest.java",
                include_bytes!(
                    "../test_data/project_tree/simpleApplication/app/src/test/java/google/simpleapplication/UnitTest.java"
                ) as &[u8],
            ),
            (
                "simpleApplication/build.gradle",
                include_bytes!("../test_data/project_tree/simpleApplication/build.gradle")
                    as &[u8],
            ),
            (
                "simpleApplication/gradle.properties",
                include_bytes!("../test_data/project_tree/simpleApplication/gradle.properties")
                    as &[u8],
            ),
            (
                "simpleApplication/settings.gradle",
                include_bytes!("../test_data/project_tree/simpleApplication/settings.gradle")
                    as &[u8],
            ),
            (
                "reference/android/navigator/testSrc/com/android/tools/idea/navigator/AndroidProjectViewTest.java",
                include_bytes!(
                    "../test_data/project_tree/reference/android/navigator/testSrc/com/android/tools/idea/navigator/AndroidProjectViewTest.java"
                ) as &[u8],
            ),
            (
                "reference/android-navigator/src/com/android/tools/idea/navigator/nodes/AndroidViewProjectNode.java",
                include_bytes!(
                    "../test_data/project_tree/reference/android-navigator/src/com/android/tools/idea/navigator/nodes/AndroidViewProjectNode.java"
                ) as &[u8],
            ),
            (
                "reference/android-navigator/src/com/android/tools/idea/navigator/nodes/AndroidViewModuleNode.java",
                include_bytes!(
                    "../test_data/project_tree/reference/android-navigator/src/com/android/tools/idea/navigator/nodes/AndroidViewModuleNode.java"
                ) as &[u8],
            ),
            (
                "reference/android-navigator/src/com/android/tools/idea/navigator/nodes/AndroidViewNodeDefaultProvider.kt",
                include_bytes!(
                    "../test_data/project_tree/reference/android-navigator/src/com/android/tools/idea/navigator/nodes/AndroidViewNodeDefaultProvider.kt"
                ) as &[u8],
            ),
            (
                "reference/android-navigator/src/com/android/tools/idea/navigator/nodes/ModuleNodeUtils.kt",
                include_bytes!(
                    "../test_data/project_tree/reference/android-navigator/src/com/android/tools/idea/navigator/nodes/ModuleNodeUtils.kt"
                ) as &[u8],
            ),
            (
                "reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidModuleNode.kt",
                include_bytes!(
                    "../test_data/project_tree/reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidModuleNode.kt"
                ) as &[u8],
            ),
            (
                "reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidSourceTypeNode.kt",
                include_bytes!(
                    "../test_data/project_tree/reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidSourceTypeNode.kt"
                ) as &[u8],
            ),
            (
                "reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidPsiDirectoryNode.java",
                include_bytes!(
                    "../test_data/project_tree/reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidPsiDirectoryNode.java"
                ) as &[u8],
            ),
            (
                "reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidPsiFileNode.java",
                include_bytes!(
                    "../test_data/project_tree/reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidPsiFileNode.java"
                ) as &[u8],
            ),
            (
                "reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidResFolderNode.kt",
                include_bytes!(
                    "../test_data/project_tree/reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidResFolderNode.kt"
                ) as &[u8],
            ),
            (
                "reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidResFolderTypeNode.java",
                include_bytes!(
                    "../test_data/project_tree/reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidResFolderTypeNode.java"
                ) as &[u8],
            ),
            (
                "reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidResGroupNode.java",
                include_bytes!(
                    "../test_data/project_tree/reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidResGroupNode.java"
                ) as &[u8],
            ),
            (
                "reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidResFileNode.java",
                include_bytes!(
                    "../test_data/project_tree/reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidResFileNode.java"
                ) as &[u8],
            ),
            (
                "reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidResComparator.java",
                include_bytes!(
                    "../test_data/project_tree/reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidResComparator.java"
                ) as &[u8],
            ),
            (
                "reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidManifestsGroupNode.java",
                include_bytes!(
                    "../test_data/project_tree/reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidManifestsGroupNode.java"
                ) as &[u8],
            ),
            (
                "reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidManifestFileNode.java",
                include_bytes!(
                    "../test_data/project_tree/reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/AndroidManifestFileNode.java"
                ) as &[u8],
            ),
            (
                "reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/BuildScriptTreeStructureProvider.java",
                include_bytes!(
                    "../test_data/project_tree/reference/android-navigator/src/com/android/tools/idea/navigator/nodes/android/BuildScriptTreeStructureProvider.java"
                ) as &[u8],
            ),
            (
                "reference/android-navigator/src/org/jetbrains/android/facet/AndroidSourceType.kt",
                include_bytes!(
                    "../test_data/project_tree/reference/android-navigator/src/org/jetbrains/android/facet/AndroidSourceType.kt"
                ) as &[u8],
            ),
            (
                "reference/android-navigator/src/com/android/tools/idea/navigator/AndroidViewNodes.java",
                include_bytes!(
                    "../test_data/project_tree/reference/android-navigator/src/com/android/tools/idea/navigator/AndroidViewNodes.java"
                ) as &[u8],
            ),
            (
                "reference/android-test-framework/testSrc/com/android/tools/idea/testing/AndroidGradleProjectRule.kt",
                include_bytes!(
                    "../test_data/project_tree/reference/android-test-framework/testSrc/com/android/tools/idea/testing/AndroidGradleProjectRule.kt"
                ) as &[u8],
            ),
            (
                "reference/android-test-framework/testSrc/com/android/tools/idea/testing/AndroidGradleTestCase.java",
                include_bytes!(
                    "../test_data/project_tree/reference/android-test-framework/testSrc/com/android/tools/idea/testing/AndroidGradleTestCase.java"
                ) as &[u8],
            ),
            (
                "reference/android/navigator/testSrc/com/android/tools/idea/navigator/nodes/android/AndroidSourceTypeNodeTest.kt",
                include_bytes!(
                    "../test_data/project_tree/reference/android/navigator/testSrc/com/android/tools/idea/navigator/nodes/android/AndroidSourceTypeNodeTest.kt"
                ) as &[u8],
            ),
            (
                "provenance.json",
                include_bytes!("../test_data/project_tree/provenance.json") as &[u8],
            ),
            (
                "LICENSE-APACHE",
                include_bytes!("../test_data/project_tree/LICENSE-APACHE") as &[u8],
            ),
        ]
    }

    fn fixture() -> Result<VerifiedReferenceFixture> {
        ReferenceFixtureManifest::verify_originals(|path| {
            original_inputs()
                .into_iter()
                .find(|(name, _)| *name == path.as_str())
                .map(|(_, bytes)| bytes.to_vec())
                .context("Missing original test input")
        })
    }

    fn value<T>(value: T) -> ReferenceResolvedValue<T> {
        ReferenceResolvedValue {
            value,
            evidence: ReferenceEvidence {
                source_identity: "SUPPLEMENTAL_TEST_INPUT_ONLY_NOT_RUNTIME_EVIDENCE".to_owned(),
                sha256: "1111111111111111111111111111111111111111111111111111111111111111"
                    .to_owned(),
            },
        }
    }

    fn artifact(path: &str) -> ReferenceArtifactBinding {
        ReferenceArtifactBinding {
            absolute_path: path.to_owned(),
            digest: ReferenceFileDigest {
                bytes: 1,
                sha256: "2222222222222222222222222222222222222222222222222222222222222222"
                    .to_owned(),
            },
            evidence: value(()).evidence,
        }
    }

    fn inputs() -> ReferenceEnvironmentInputs {
        ReferenceEnvironmentInputs {
            agp: value("9.4.0-dev".to_owned()),
            latest_known_agp: value("9.4.0-dev".to_owned()),
            gradle: value("9.6.0".to_owned()),
            kotlin: value("2.4.20-RC3".to_owned()),
            embedded_jdk_major: value(21),
            embedded_jdk_path: value("/test/embedded-jdk".to_owned()),
            sdk_path: value("/test/android-sdk".to_owned()),
            compile_sdk: value(34),
            target_sdk: value(34),
            build_tools: value("36.0.0".to_owned()),
            lowest_active_api: value(16),
            built_in_kotlin: value(true),
            parallel_sync: value(false),
            debug_inner_test_disabled: value(true),
            running_from_bazel: value(false),
            workspace_output_repository_exists: value(false),
            offline_repository_uri: value("file:/test/offline-maven/".to_owned()),
            workspace_output_repository_uri: None,
            additional_repository_uris: Vec::new(),
            gradle_distribution: artifact("/test/gradle-9.6.0-bin.zip"),
            wrapper_jar: artifact("/test/gradle-wrapper.jar"),
            wrapper_unix_script: artifact("/test/gradlew"),
            wrapper_windows_script: artifact("/test/gradlew.bat"),
        }
    }

    fn environment() -> Result<ResolvedReferenceEnvironment> {
        ResolvedReferenceEnvironment::resolve(Some(inputs()))
    }

    #[test]
    fn verifies_every_original_fixture_source_provenance_and_license() -> Result<()> {
        let fixture = fixture()?;
        assert_eq!(fixture.receipt().fixture_count, 26);
        assert_eq!(fixture.receipt().source_count, 22);
        assert_eq!(fixture.receipt().inputs.len(), 50);
        assert_eq!(fixture.files.len(), 26);
        Ok(())
    }

    #[test]
    fn rejects_tampering_in_each_of_the_fifty_original_inputs() {
        for (target, _) in original_inputs() {
            let result = ReferenceFixtureManifest::verify_originals(|path| {
                let mut bytes = original_inputs()
                    .into_iter()
                    .find(|(name, _)| *name == path.as_str())
                    .map(|(_, bytes)| bytes.to_vec())
                    .context("Missing original test input")?;
                if path.as_str() == target {
                    bytes.push(0);
                }
                Ok(bytes)
            });
            assert!(result.is_err(), "Tampered input accepted: {target}");
        }
    }

    #[test]
    fn rejects_missing_original_bytes() {
        let result = ReferenceFixtureManifest::verify_originals(|path| {
            ensure!(
                path.as_str() != "simpleApplication/settings.gradle",
                "Missing original fixture"
            );
            original_inputs()
                .into_iter()
                .find(|(name, _)| *name == path.as_str())
                .map(|(_, bytes)| bytes.to_vec())
                .context("Missing original test input")
        });
        assert!(result.is_err());
    }

    #[test]
    fn confines_original_and_project_relative_paths() -> Result<()> {
        for path in [
            "",
            "/app/build.gradle",
            "../app",
            "app/../outside",
            "app/./file",
            "app//file",
            "app/",
            "C:/app/file",
            "app\\file",
            "app/\0file",
        ] {
            assert!(
                ReferencePath::parse(path).is_err(),
                "Unsafe path accepted: {path:?}"
            );
        }
        assert_eq!(
            ReferencePath::parse("app/src/main/res/resources.properties")?.as_str(),
            "app/src/main/res/resources.properties"
        );
        Ok(())
    }

    #[test]
    fn requires_resolved_environment_evidence_and_official_artifact_bindings() {
        assert!(ResolvedReferenceEnvironment::resolve(None).is_err());
        let mut missing = inputs();
        missing.agp.evidence.source_identity.clear();
        assert!(ResolvedReferenceEnvironment::resolve(Some(missing)).is_err());
        let mut missing = inputs();
        missing.gradle_distribution.digest.bytes = 0;
        assert!(ResolvedReferenceEnvironment::resolve(Some(missing)).is_err());
        let mut unresolved = inputs();
        unresolved.agp.value = "AGP_CURRENT".to_owned();
        assert!(ResolvedReferenceEnvironment::resolve(Some(unresolved)).is_err());
        for version in ["9.foo", "9.4evil", "9.4", "9.4.0;exec", "9.4.0\n"] {
            let mut invalid = inputs();
            invalid.agp.value = version.to_owned();
            invalid.latest_known_agp.value = version.to_owned();
            assert!(
                ResolvedReferenceEnvironment::resolve(Some(invalid)).is_err(),
                "Invalid AGP accepted: {version:?}"
            );
        }
        let mut mismatched = inputs();
        mismatched.latest_known_agp.value = "9.3.0".to_owned();
        assert!(ResolvedReferenceEnvironment::resolve(Some(mismatched)).is_err());
    }

    #[test]
    fn rejects_unsupported_descriptors_flags_and_artifact_paths() {
        let mut unsupported = inputs();
        unsupported.compile_sdk.value = 37;
        assert!(ResolvedReferenceEnvironment::resolve(Some(unsupported)).is_err());
        let mut unsupported = inputs();
        unsupported.gradle.value = "9.4.0".to_owned();
        assert!(ResolvedReferenceEnvironment::resolve(Some(unsupported)).is_err());
        let mut unsupported = inputs();
        unsupported.built_in_kotlin.value = false;
        assert!(ResolvedReferenceEnvironment::resolve(Some(unsupported)).is_err());
        let mut unsupported = inputs();
        unsupported.debug_inner_test_disabled.value = false;
        assert!(ResolvedReferenceEnvironment::resolve(Some(unsupported)).is_err());
        let mut unsupported = inputs();
        unsupported.sdk_path.value = "/test/../sdk".to_owned();
        assert!(ResolvedReferenceEnvironment::resolve(Some(unsupported)).is_err());
        let mut unsupported = inputs();
        unsupported.sdk_path.value = "C:/sdk".to_owned();
        assert!(ResolvedReferenceEnvironment::resolve(Some(unsupported)).is_err());
    }

    #[test]
    fn preserves_original_repository_order_duplicates_and_metadata_sources() -> Result<()> {
        let mut selected = inputs();
        selected.workspace_output_repository_exists.value = true;
        selected.workspace_output_repository_uri = Some(value("file:/test/out/repo/".to_owned()));
        selected.additional_repository_uris = vec![
            value("file:/test/extra%20repo/".to_owned()),
            value("file:/test/offline-maven/".to_owned()),
        ];
        let environment = ResolvedReferenceEnvironment::resolve(Some(selected))?;
        let plan = ReferencePreparationPlan::prepare_java_rule(&fixture()?, &environment)?;
        let root = plan
            .operations
            .iter()
            .find_map(|operation| match operation {
                ReferencePreparationOperation::ReplaceFixtureFile(patch)
                    if patch.project_relative_path.as_str() == "build.gradle" =>
                {
                    Some(patch)
                }
                _ => None,
            })
            .context("Root Gradle patch missing")?;
        let text = std::str::from_utf8(&root.bytes)?;
        let expected = [
            "file:/test/offline-maven/",
            "file:/test/out/repo/",
            "file:/test/extra%20repo/",
            "file:/test/offline-maven/",
        ];
        let uris: Vec<_> = text
            .lines()
            .filter_map(|line| line.strip_prefix("  url = \"")?.strip_suffix('"'))
            .collect();
        assert_eq!(uris, expected.repeat(2));
        assert_eq!(text.matches("metadataSources() {").count(), 8);
        assert_eq!(text.matches("mavenPom()").count(), 8);
        assert_eq!(text.matches("artifact()").count(), 8);
        Ok(())
    }

    #[test]
    fn rejects_invalid_repository_selection_and_uri_injection() {
        let mut selected = inputs();
        selected.workspace_output_repository_exists.value = true;
        assert!(ResolvedReferenceEnvironment::resolve(Some(selected)).is_err());
        let mut selected = inputs();
        selected.workspace_output_repository_uri = Some(value("file:/test/out/repo/".to_owned()));
        assert!(ResolvedReferenceEnvironment::resolve(Some(selected)).is_err());
        for uri in [
            "https://example.com/repo/",
            "file:/test/../repo/",
            "file:/test/%2e%2e/repo/",
            "file:/test/%2Foutside/",
            "file:/test/%00repo/",
            "file:/test/\";evil/",
            "file:/test/%ZZ/",
            "file:/test/repo",
        ] {
            let mut selected = inputs();
            selected.offline_repository_uri.value = uri.to_owned();
            assert!(
                ResolvedReferenceEnvironment::resolve(Some(selected)).is_err(),
                "Unsafe repository accepted: {uri}"
            );
        }
    }

    #[test]
    fn both_original_loader_paths_have_deterministic_common_preparation() -> Result<()> {
        let fixture = fixture()?;
        let environment = environment()?;
        let java = ReferencePreparationPlan::prepare_java_rule(&fixture, &environment)?;
        let core = ReferencePreparationPlan::prepare_core_rule(&fixture, &environment)?;
        assert_eq!(java.operations, core.operations);
        assert_eq!(java.original, core.original);
        assert_eq!(java.helper_members, core.helper_members);
        assert_ne!(java.rule, core.rule);
        assert_eq!(
            serde_json::to_vec(&java)?,
            serde_json::to_vec(&ReferencePreparationPlan::prepare_java_rule(
                &fixture,
                &environment
            )?)?
        );
        assert_eq!(java.operations.len(), 7);
        Ok(())
    }

    #[test]
    fn gradle_property_operations_retain_the_exact_upstream_assignments() -> Result<()> {
        let fixture = fixture()?;
        for parallel in [false, true] {
            let mut selected = inputs();
            selected.parallel_sync.value = parallel;
            let plan = ReferencePreparationPlan::prepare_java_rule(
                &fixture,
                &ResolvedReferenceEnvironment::resolve(Some(selected))?,
            )?;
            let assignments = plan
                .operations
                .iter()
                .find_map(|operation| match operation {
                    ReferencePreparationOperation::UpdateGradleProperties {
                        assignments, ..
                    } => Some(assignments),
                    _ => None,
                })
                .context("Properties operation missing")?;
            assert_eq!(
                assignments.get("org.gradle.jvmargs").map(String::as_str),
                Some(" -XX:MaxMetaspaceSize=768m ")
            );
            assert_eq!(
                assignments.get("org.gradle.vfs.watch").map(String::as_str),
                Some("false")
            );
            assert_eq!(
                assignments
                    .get("org.gradle.tooling.parallel")
                    .map(String::as_str),
                parallel.then_some("true")
            );
            assert!(!assignments.contains_key("org.gradle.parallel"));
            assert!(!assignments.contains_key("android.suppressUnsupportedCompileSdk"));
            assert!(!assignments.contains_key("android.builtInKotlin"));
            assert!(!assignments.contains_key("android.enableBuildConfigAsBytecode"));
        }
        Ok(())
    }

    #[test]
    fn exact_original_groovy_patch_preserves_min_sdk_build_config_and_failing_sample() -> Result<()>
    {
        let fixture = fixture()?;
        let plan = ReferencePreparationPlan::prepare_java_rule(&fixture, &environment()?)?;
        let app = plan
            .operations
            .iter()
            .find_map(|operation| match operation {
                ReferencePreparationOperation::ReplaceFixtureFile(patch)
                    if patch.project_relative_path.as_str() == "app/build.gradle" =>
                {
                    Some(patch)
                }
                _ => None,
            })
            .context("App Gradle patch missing")?;
        let expected = r###"apply plugin: 'com.android.application'

android {
    compileSdkVersion 34
    buildToolsVersion "36.0.0"
    useLibrary 'org.apache.http.legacy'
    namespace = "google.simpleapplication"

    defaultConfig {
        applicationId "google.simpleapplication"
        minSdkVersion 21
        targetSdkVersion 34
        versionCode 1
        versionName "1.0"
    }
    buildTypes {
        release {
            minifyEnabled false
            proguardFiles getDefaultProguardFile('proguard-android-optimize.txt'), 'proguard-rules.pro'
        }
    }
    lint {
        abortOnError = false
    }

    buildFeatures {
        buildConfig = true
    }
}

dependencies {
    api fileTree(dir: 'libs', include: ['*.jar'])
    api 'com.android.support:appcompat-v7:+'
    api 'com.google.guava:guava:19.0'
    api 'com.android.support.constraint:constraint-layout:1.0.2'
    testImplementation 'junit:junit:4.12'
    androidTestImplementation 'com.android.support.test:runner:+'
    androidTestImplementation 'com.android.support.test.espresso:espresso-core:+'
}"###;
        assert_eq!(app.bytes, expected.as_bytes());
        assert_eq!(app.prepared, digest(expected.as_bytes()));
        assert_eq!(
            app.original.sha256,
            "bd42bb1aa37765b1171205d610e8b31b2753b26bb2d7fe9ed7e1d0ed93434bde"
        );
        let sample = fixture.original(&ReferencePath::parse(
            "simpleApplication/app/src/test/java/google/simpleapplication/UnitTest.java",
        )?)?;
        assert_eq!(
            digest(sample).sha256,
            "a4ed37536b1e6385660a67b169deed000f51bb1e592073e0e6d576f3bde916c9"
        );
        assert!(std::str::from_utf8(sample)?.contains("failingTest"));
        assert!(std::str::from_utf8(sample)?.contains("assertEquals(5, 2 + 2)"));
        assert!(!plan.operations.iter().any(|operation| matches!(operation, ReferencePreparationOperation::ReplaceFixtureFile(patch) if patch.project_relative_path.as_str().ends_with("UnitTest.java"))));
        Ok(())
    }

    #[test]
    fn exact_root_patch_retains_original_comments_and_repository_insertion() -> Result<()> {
        let plan = ReferencePreparationPlan::prepare_java_rule(&fixture()?, &environment()?)?;
        let root = plan
            .operations
            .iter()
            .find_map(|operation| match operation {
                ReferencePreparationOperation::ReplaceFixtureFile(patch)
                    if patch.project_relative_path.as_str() == "build.gradle" =>
                {
                    Some(patch)
                }
                _ => None,
            })
            .context("Root Gradle patch missing")?;
        let expected = r###"// Top-level build file where you can add configuration options common to all sub-projects/modules.

buildscript {
    repositories {
maven {
  url = "file:/test/offline-maven/"
  try {
    metadataSources() {
      mavenPom()
      artifact()
    }
  } catch (Throwable ignored) { /* In case this Gradle version does not support this. */}
}
        // This will be populated by AndroidGradleTestCase
    }
    dependencies {
        classpath 'com.android.tools.build:gradle:9.4.0-dev'

        // NOTE: Do not place your application dependencies here; they belong
        // in the individual module build.gradle files
    }
}

allprojects {
    repositories {
maven {
  url = "file:/test/offline-maven/"
  try {
    metadataSources() {
      mavenPom()
      artifact()
    }
  } catch (Throwable ignored) { /* In case this Gradle version does not support this. */}
}
        // This will be populated by AndroidGradleTestCase
    }
}
"###;
        assert_eq!(root.bytes, expected.as_bytes());
        assert_eq!(root.prepared, digest(expected.as_bytes()));
        assert_eq!(
            root.original.sha256,
            "2c4facdd32bf7c5cb84c1b818940d37fcfc778acddb50d4a070c6cfb4650b51f"
        );
        Ok(())
    }
}
