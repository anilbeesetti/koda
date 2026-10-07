#![allow(clippy::disallowed_methods, reason = "tooling is exempt")]

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufReader, Read},
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result, bail, ensure};
use clap::Parser;
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Parser)]
pub struct AndroidParityArgs {
    /// Checkout containing the ledger and Rust test targets.
    #[arg(long, default_value = ".")]
    repository: PathBuf,
    /// Directory containing pinned archives and selected source extractions.
    #[arg(long)]
    reference_root: Option<PathBuf>,
    /// Require complete reviewed inventories and rerun every mapped Rust test.
    #[arg(long)]
    complete: bool,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    baseline: String,
    census_complete: bool,
    sources: Vec<Source>,
    tests: Vec<ReferenceTest>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    id: String,
    repository: String,
    revision: String,
    tag: Option<String>,
    license: String,
    archive: Artifact,
    coverage: Coverage,
    coverage_reason: String,
    coverage_evidence: Option<Artifact>,
    census_complete: bool,
    census_evidence: Option<Artifact>,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Coverage {
    Canonical,
    UnverifiedMirror,
    VerifiedMirror,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    path: String,
    sha256: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferenceTest {
    id: String,
    source: String,
    suite: String,
    method: String,
    /// Explicit instance identifier for parameterized or dynamically generated cases.
    case: Option<String>,
    file: Artifact,
    line: usize,
    declaration: String,
    copyright: String,
    fixtures: Vec<Artifact>,
    fixture_reason: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    schema_version: u32,
    entries: Vec<Entry>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    reference_id: String,
    port_status: PortStatus,
    run_status: RunStatus,
    reason: Option<String>,
    run_blocker: Option<String>,
    rust_targets: Vec<RustTarget>,
    fixtures: Vec<FixturePort>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum PortStatus {
    Unported,
    Ported,
    Adapted,
    NotApplicable,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum RunStatus {
    NotRun,
    Passing,
    Failing,
    Blocked,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RustTarget {
    package: String,
    /// None selects --lib; otherwise selects --test <integration target>.
    integration_target: Option<String>,
    test_name: String,
    manifest: Artifact,
    source: Artifact,
    run: Option<RunEvidence>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunEvidence {
    tested_commit: String,
    command: Vec<String>,
    exit_code: i32,
    log: Artifact,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixturePort {
    reference_path: String,
    destination: Artifact,
    adapted: bool,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CensusEvidence {
    source: String,
    revision: String,
    /// A human-reviewed inventory, including inherited and generated instances.
    all_suites_and_expansions_reviewed: bool,
    reviewed_by: String,
    reference_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MirrorEvidence {
    source: String,
    revision: String,
    canonical_repository: String,
    canonical_revision: String,
    all_omissions_resolved: bool,
    reviewed_by: String,
}

const PINS: [(&str, &str, &str, &str); 3] = [
    (
        "idea",
        "https://android.googlesource.com/platform/tools/adt/idea",
        "a84efec3ba9542d9bfa1255103f0dc94833a3796",
        "f15d0ee82d4719da82aa7d549dc4c3c14c180dac8112c4a1a73a65474419cf84",
    ),
    (
        "base",
        "https://android.googlesource.com/platform/tools/base",
        "4a5d2ec9571e2021fc9a4621e24a195f966ee6dd",
        "0cbac492c1e2b47273fc9a1e2929caa6460660cf475cbd7773256f443cb30bb4",
    ),
    (
        "jetbrains-android",
        "https://github.com/JetBrains/android",
        "132bc7c3cf52598117590637d00e81b929444bde",
        "d7bb6a155c077166a6aed54ddaaa7d1629cbee8c200ceced0bddc0bcedfd92e2",
    ),
];

pub fn run(args: AndroidParityArgs) -> Result<()> {
    let repository = args
        .repository
        .canonicalize()
        .context("locating checkout")?;
    let manifest: Manifest =
        read_json(&repository.join("docs/android-studio/reference-manifest.json"))?;
    let ledger: Ledger = read_json(&repository.join("docs/android-studio/test-parity.json"))?;
    validate(
        &manifest,
        &ledger,
        &repository,
        args.reference_root.as_deref(),
    )?;
    println!("{}", summary(&manifest, &ledger));
    if args.reference_root.is_none() {
        println!(
            "External reference bytes not checked; use --reference-root to verify archives and citations."
        );
    }
    if args.complete {
        check_complete(&manifest, &ledger)?;
        ensure!(
            args.reference_root.is_some(),
            "completion requires --reference-root"
        );
        rerun_targets(&ledger, &repository, |arguments, repository| {
            let output = Command::new("cargo")
                .args(arguments)
                .current_dir(repository)
                .output()
                .context("running exact Rust test")?;
            Ok(TestRun {
                success: output.status.success(),
                stdout: String::from_utf8(output.stdout).context("reading Rust test output")?,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        })?;
        println!(
            "Completion gate passed; every mapped Rust test was rerun. Semantic parity and census accuracy still require review."
        );
    }
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_reader(BufReader::new(
        File::open(path).with_context(|| format!("opening {}", path.display()))?,
    ))
    .with_context(|| format!("parsing {}", path.display()))
}

fn validate(
    manifest: &Manifest,
    ledger: &Ledger,
    repository: &Path,
    references: Option<&Path>,
) -> Result<()> {
    ensure!(
        manifest.schema_version == 1 && ledger.schema_version == 1,
        "unsupported ledger schema"
    );
    require_text(&manifest.baseline, "baseline")?;
    ensure!(
        manifest.sources.len() == PINS.len(),
        "all three pinned reference sources are required"
    );
    let mut sources = BTreeMap::new();
    for source in &manifest.sources {
        ensure!(
            sources.insert(source.id.as_str(), source).is_none(),
            "duplicate source {}",
            source.id
        );
        let pin = PINS
            .iter()
            .find(|pin| pin.0 == source.id)
            .context("unknown reference source")?;
        ensure!(
            source.repository == pin.1
                && source.revision == pin.2
                && source.archive.sha256 == pin.3,
            "immutable source pin changed: {}",
            source.id
        );
        ensure!(
            source.license == "Apache-2.0",
            "source license must be retained: {}",
            source.id
        );
        require_text(&source.coverage_reason, "source coverage reason")?;
        if source.id == "jetbrains-android" {
            ensure!(
                source.tag.as_deref() == Some("idea/262.9437.185"),
                "JetBrains tag changed"
            );
            ensure!(
                source.coverage != Coverage::Canonical,
                "JetBrains mirror cannot claim canonical coverage"
            );
        } else {
            ensure!(
                source.tag.is_none() && source.coverage == Coverage::Canonical,
                "invalid AOSP source coverage"
            );
        }
        validate_artifact(&source.archive)?;
        ensure!(
            (source.coverage == Coverage::VerifiedMirror) == source.coverage_evidence.is_some(),
            "verified mirror requires canonical comparison evidence"
        );
        if let Some(artifact) = &source.coverage_evidence {
            let path = verify_artifact(repository, artifact)?;
            let evidence: MirrorEvidence = read_json(&path)?;
            ensure!(
                evidence.source == source.id
                    && evidence.revision == source.revision
                    && evidence.canonical_repository
                        == "https://git.jetbrains.org/idea/android.git"
                    && evidence.all_omissions_resolved,
                "mirror coverage lacks a complete canonical comparison"
            );
            ensure!(
                evidence.canonical_revision.len() == 40
                    && evidence
                        .canonical_revision
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                "canonical comparison needs an immutable revision"
            );
            require_text(&evidence.reviewed_by, "mirror reviewer")?;
        }
        if let Some(root) = references {
            verify_artifact(root, &source.archive)?;
        }
        ensure!(
            source.census_complete == source.census_evidence.is_some(),
            "complete census requires evidence, incomplete census cannot claim evidence: {}",
            source.id
        );
    }
    ensure!(
        manifest.census_complete == manifest.sources.iter().all(|source| source.census_complete),
        "global census status disagrees with source inventories"
    );
    ensure!(
        !manifest.tests.is_empty(),
        "reference catalog cannot be empty"
    );
    let mut tests = BTreeMap::new();
    for test in &manifest.tests {
        let source = sources
            .get(test.source.as_str())
            .context("test refers to unknown source")?;
        require_text(&test.suite, "reference suite")?;
        require_text(&test.method, "reference method")?;
        require_text(&test.copyright, "reference copyright")?;
        require_text(&test.declaration, "reference declaration")?;
        ensure!(test.line > 0, "reference line must be one-based");
        validate_artifact(&test.file)?;
        if let Some(case) = &test.case {
            require_text(case, "parameterized case")?;
        }
        let case = test
            .case
            .as_ref()
            .map(|case| format!("[{case}]"))
            .unwrap_or_default();
        let expected_id = format!(
            "{}@{}:{}#{}.{}{}",
            source.id, source.revision, test.file.path, test.suite, test.method, case
        );
        ensure!(
            test.id == expected_id,
            "noncanonical reference ID: {}",
            test.id
        );
        ensure!(
            tests.insert(test.id.as_str(), test).is_none(),
            "duplicate reference ID: {}",
            test.id
        );
        ensure!(
            test.fixtures.is_empty() == test.fixture_reason.is_some(),
            "empty fixtures require a specific explanation; fixtures must not claim no fixture"
        );
        if let Some(reason) = &test.fixture_reason {
            require_reason(reason, "fixture reason")?;
        }
        let mut fixture_paths = BTreeSet::new();
        for fixture in &test.fixtures {
            validate_artifact(fixture)?;
            ensure!(
                fixture_paths.insert(&fixture.path),
                "duplicate reference fixture"
            );
        }
        if let Some(root) = references {
            let source_root = root.join("samples").join(&test.source);
            let path = verify_artifact(&source_root, &test.file)?;
            let text = fs::read_to_string(path).context("reading reference citation")?;
            ensure!(
                text.lines().nth(test.line - 1).map(str::trim) == Some(test.declaration.trim()),
                "reference declaration disagrees with pinned source: {}",
                test.id
            );
            for fixture in &test.fixtures {
                verify_artifact(&source_root, fixture)?;
            }
        }
    }
    for source in &manifest.sources {
        if let Some(artifact) = &source.census_evidence {
            let path = verify_artifact(repository, artifact)?;
            let evidence: CensusEvidence = read_json(&path)?;
            ensure!(
                evidence.source == source.id
                    && evidence.revision == source.revision
                    && evidence.all_suites_and_expansions_reviewed,
                "invalid complete census attestation: {}",
                source.id
            );
            require_text(&evidence.reviewed_by, "census reviewer")?;
            let actual = evidence.reference_ids.iter().collect::<BTreeSet<_>>();
            let expected = manifest
                .tests
                .iter()
                .filter(|test| test.source == source.id)
                .map(|test| &test.id)
                .collect::<BTreeSet<_>>();
            ensure!(
                !expected.is_empty()
                    && actual.len() == evidence.reference_ids.len()
                    && actual == expected,
                "census and catalog disagree: {}",
                source.id
            );
        }
    }
    let mut entries = BTreeSet::new();
    for entry in &ledger.entries {
        ensure!(
            entries.insert(entry.reference_id.as_str()),
            "duplicate ledger entry: {}",
            entry.reference_id
        );
        let test = tests
            .get(entry.reference_id.as_str())
            .context("ledger entry has no reference test")?;
        validate_entry(entry, test, repository).with_context(|| entry.reference_id.clone())?;
    }
    ensure!(
        entries.len() == tests.len(),
        "reference tests are missing ledger entries"
    );
    let revisions = ledger
        .entries
        .iter()
        .flat_map(|entry| &entry.rust_targets)
        .filter_map(|target| target.run.as_ref())
        .map(|run| run.tested_commit.as_str())
        .collect::<BTreeSet<_>>();
    for revision in revisions {
        verify_tested_commit(repository, revision)?;
    }
    Ok(())
}

struct TestRun {
    success: bool,
    stdout: String,
    stderr: String,
}

fn rerun_targets(
    ledger: &Ledger,
    repository: &Path,
    mut execute: impl FnMut(&[String], &Path) -> Result<TestRun>,
) -> Result<usize> {
    let mut executed = BTreeSet::new();
    for target in ledger.entries.iter().flat_map(|entry| &entry.rust_targets) {
        let arguments = target.arguments();
        if executed.insert(arguments.clone()) {
            let output = execute(&arguments, repository)?;
            ensure!(
                output.success,
                "{} failed: {}\n{}",
                target.test_name,
                output.stdout,
                output.stderr
            );
            verify_test_output(&output.stdout, &target.test_name, true)?;
        }
    }
    Ok(executed.len())
}

fn verify_tested_commit(repository: &Path, revision: &str) -> Result<()> {
    ensure!(
        revision.len() == 40
            && revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "run evidence requires an immutable tested Git commit"
    );
    let status = Command::new("git")
        .args(["merge-base", "--is-ancestor", revision, "HEAD"])
        .current_dir(repository)
        .status()
        .context("checking tested commit ancestry")?;
    ensure!(
        status.success(),
        "tested commit is absent or is not an ancestor of the checkout"
    );
    // Ledger and captured logs are committed after the tested code; including them would make the evidence self-referential.
    let paths = [
        ".",
        ":(glob,exclude)docs/android-studio/*.md",
        ":(exclude)docs/android-studio/reference-manifest.json",
        ":(exclude)docs/android-studio/test-parity.json",
        ":(glob,exclude)docs/android-studio/ports/*.json",
        ":(glob,exclude)docs/android-studio/evidence/**/*.log",
        ":(glob,exclude)docs/android-studio/evidence/**/*.json",
    ];
    for cached in [false, true] {
        let mut command = Command::new("git");
        command.arg("diff").arg("--quiet");
        if cached {
            command.arg("--cached");
        }
        let status = command
            .args([revision, "--"])
            .args(paths)
            .current_dir(repository)
            .status()
            .context("checking tested source snapshot")?;
        ensure!(
            status.success(),
            "run evidence is stale: implementation or dependency files changed in {} since {revision}",
            if cached {
                "the Git index"
            } else {
                "the working tree"
            }
        );
    }
    let output = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard", "--"])
        .args(paths)
        .current_dir(repository)
        .output()
        .context("checking untracked implementation files")?;
    ensure!(
        output.status.success() && output.stdout.is_empty(),
        "run evidence cannot credit untracked implementation files"
    );
    Ok(())
}

fn validate_entry(entry: &Entry, test: &ReferenceTest, repository: &Path) -> Result<()> {
    ensure!(
        (entry.run_status == RunStatus::Blocked) == entry.run_blocker.is_some(),
        "only blocked runs require a separate concrete blocker"
    );
    match entry.port_status {
        PortStatus::Unported | PortStatus::NotApplicable => {
            ensure!(
                entry.run_status == RunStatus::NotRun
                    && entry.rust_targets.is_empty()
                    && entry.fixtures.is_empty(),
                "unported or inapplicable tests cannot claim execution or Rust ports"
            );
            if entry.port_status == PortStatus::NotApplicable {
                require_reason(
                    entry.reason.as_deref().context(
                        "inapplicable tests need the absent equivalent behavior explained",
                    )?,
                    "inapplicability reason",
                )?;
            }
        }
        PortStatus::Ported | PortStatus::Adapted => {
            ensure!(
                !entry.rust_targets.is_empty(),
                "ported tests need Rust targets"
            );
            if entry.port_status == PortStatus::Adapted {
                require_reason(
                    entry
                        .reason
                        .as_deref()
                        .context("adapted tests need a behavior-preservation reason")?,
                    "adaptation reason",
                )?;
            } else {
                ensure!(
                    entry.reason.is_none(),
                    "ported tests cannot hide adaptation in a reason"
                );
            }
            if entry.run_status == RunStatus::Blocked {
                require_reason(
                    entry
                        .run_blocker
                        .as_deref()
                        .context("blocked tests need a concrete blocker")?,
                    "run blocker",
                )?;
            }
            let mut targets = BTreeSet::new();
            let mut failure_found = false;
            for target in &entry.rust_targets {
                ensure!(
                    !target.manifest.path.starts_with("docs/android-studio/")
                        && !target.source.path.starts_with("docs/android-studio/"),
                    "Rust targets cannot live under ledger metadata or evidence paths"
                );
                require_identifier(&target.package, "Rust package")?;
                if let Some(name) = &target.integration_target {
                    require_identifier(name, "integration target")?;
                }
                ensure!(
                    target.test_name.split("::").all(|part| {
                        !part.is_empty()
                            && part.chars().all(|character| {
                                character.is_ascii_alphanumeric() || character == '_'
                            })
                    }),
                    "invalid exact Rust test name"
                );
                ensure!(
                    target.source.path.ends_with(".rs"),
                    "Rust target source must be a Rust file"
                );
                let manifest_path = verify_artifact(repository, &target.manifest)?;
                let cargo_manifest = cargo_toml::Manifest::from_path(&manifest_path)
                    .context("reading Rust target package manifest")?;
                ensure!(
                    cargo_manifest
                        .package
                        .as_ref()
                        .is_some_and(|package| package.name == target.package),
                    "Rust target package disagrees with its manifest"
                );
                let product = if let Some(integration) = &target.integration_target {
                    cargo_manifest
                        .test
                        .iter()
                        .find(|product| product.name.as_deref() == Some(integration))
                        .context("mapped integration target is absent")?
                } else {
                    cargo_manifest
                        .lib
                        .as_ref()
                        .context("mapped library target is absent")?
                };
                ensure!(
                    product.test && product.harness,
                    "mapped Rust target must use an enabled libtest harness"
                );
                let package_root = manifest_path
                    .parent()
                    .context("locating Rust target package root")?;
                verify_artifact(repository, &target.source)?;
                ensure!(
                    repository
                        .join(&target.source.path)
                        .canonicalize()?
                        .starts_with(package_root),
                    "Rust test source is outside its package"
                );
                ensure!(targets.insert(target.arguments()), "duplicate Rust target");
                match (&target.run, entry.run_status) {
                    (None, RunStatus::Passing) => bail!("passing target has no run evidence"),
                    (Some(_), RunStatus::NotRun | RunStatus::Blocked) => {
                        bail!("unexecuted target has run evidence")
                    }
                    (Some(run), status) => {
                        let mut command = vec!["cargo".to_owned()];
                        command.extend(target.arguments());
                        ensure!(
                            run.command == command,
                            "run command must select exactly the mapped test without skipping"
                        );
                        let path = verify_artifact(repository, &run.log)?;
                        ensure!(
                            run.log.path.starts_with("docs/android-studio/evidence/")
                                && run.log.path.ends_with(".log"),
                            "run logs must be stored under docs/android-studio/evidence as .log artifacts"
                        );
                        let output =
                            fs::read_to_string(path).context("reading test run evidence")?;
                        let passing = run.exit_code == 0;
                        verify_test_output(&output, &target.test_name, passing)?;
                        ensure!(
                            status != RunStatus::Passing || passing,
                            "passing target has failed run evidence"
                        );
                        failure_found |= !passing;
                    }
                    (None, _) => {}
                }
            }
            ensure!(
                entry.run_status != RunStatus::Failing || failure_found,
                "failing entry needs at least one executed failing test"
            );
            let expected = test
                .fixtures
                .iter()
                .map(|fixture| (&fixture.path, &fixture.sha256))
                .collect::<BTreeMap<_, _>>();
            let mut mapped = BTreeSet::new();
            for fixture in &entry.fixtures {
                ensure!(
                    !fixture.destination.path.starts_with("docs/android-studio/"),
                    "implementation fixtures cannot live under ledger metadata or evidence paths"
                );
                ensure!(
                    mapped.insert(&fixture.reference_path),
                    "duplicate fixture mapping"
                );
                let reference_hash = expected
                    .get(&fixture.reference_path)
                    .context("fixture port is not mapped to a reference fixture")?;
                verify_artifact(repository, &fixture.destination)?;
                if fixture.adapted {
                    require_reason(
                        fixture
                            .reason
                            .as_deref()
                            .context("adapted fixture needs a reason")?,
                        "fixture adaptation",
                    )?;
                } else {
                    ensure!(
                        fixture.reason.is_none() && &fixture.destination.sha256 == *reference_hash,
                        "reused fixture must preserve original bytes"
                    );
                }
            }
            ensure!(
                mapped.len() == expected.len(),
                "reference fixture is missing its Rust-side mapping"
            );
        }
    }
    if let Some(reason) = &entry.reason {
        require_reason(reason, "entry reason")?;
    }
    Ok(())
}

impl RustTarget {
    fn arguments(&self) -> Vec<String> {
        let mut arguments = vec![
            "test".into(),
            "--locked".into(),
            "--manifest-path".into(),
            self.manifest.path.clone(),
            "-p".into(),
            self.package.clone(),
        ];
        if let Some(target) = &self.integration_target {
            arguments.extend(["--test".into(), target.clone()]);
        } else {
            arguments.push("--lib".into());
        }
        arguments.extend([
            self.test_name.clone(),
            "--".into(),
            "--exact".into(),
            "--show-output".into(),
        ]);
        arguments
    }
}

fn verify_test_output(output: &str, name: &str, passing: bool) -> Result<()> {
    let outcome = if passing { "ok" } else { "FAILED" };
    let result = if passing {
        "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;"
    } else {
        "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured;"
    };
    ensure!(
        output
            .lines()
            .filter(|line| *line == format!("test {name} ... {outcome}"))
            .count()
            == 1,
        "run did not execute the named test exactly once"
    );
    let summaries = output
        .lines()
        .filter(|line| line.starts_with("test result:"))
        .collect::<Vec<_>>();
    ensure!(
        summaries.len() == 1
            && summaries
                .first()
                .is_some_and(|line| line.starts_with(result)),
        "run evidence must show exactly one executed test, no ignored tests, and the recorded outcome"
    );
    Ok(())
}

fn check_complete(manifest: &Manifest, ledger: &Ledger) -> Result<()> {
    ensure!(
        manifest.census_complete && manifest.sources.iter().all(|source| source.census_complete),
        "completion blocked: reference test census is incomplete"
    );
    ensure!(
        manifest
            .sources
            .iter()
            .all(|source| source.coverage != Coverage::UnverifiedMirror),
        "completion blocked: mirror omissions are unverified"
    );
    for entry in &ledger.entries {
        ensure!(
            entry.port_status != PortStatus::Unported,
            "completion blocked: unported reference test {}",
            entry.reference_id
        );
        ensure!(
            entry.port_status == PortStatus::NotApplicable
                || entry.run_status == RunStatus::Passing,
            "completion blocked: {:?} reference test {}",
            entry.run_status,
            entry.reference_id
        );
    }
    Ok(())
}

fn summary(manifest: &Manifest, ledger: &Ledger) -> String {
    let mut ports = BTreeMap::new();
    let mut runs = BTreeMap::new();
    for entry in &ledger.entries {
        *ports.entry(entry.port_status).or_insert(0) += 1;
        *runs.entry(entry.run_status).or_insert(0) += 1;
    }
    format!(
        "Baseline: {}\nCensus: {} ({} individually identified reference tests; exhaustive total unknown unless complete)\nPorts: {:?}\nRuns: {:?}\nMirror omissions: {}",
        manifest.baseline,
        if manifest.census_complete {
            "complete"
        } else {
            "PARTIAL"
        },
        manifest.tests.len(),
        ports,
        runs,
        if manifest
            .sources
            .iter()
            .any(|source| source.coverage == Coverage::UnverifiedMirror)
        {
            "UNVERIFIED"
        } else {
            "verified"
        }
    )
}

fn require_text(text: &str, label: &str) -> Result<()> {
    ensure!(
        !text.trim().is_empty() && text == text.trim(),
        "{label} must be nonempty without surrounding whitespace"
    );
    Ok(())
}

fn require_reason(text: &str, label: &str) -> Result<()> {
    require_text(text, label)?;
    ensure!(
        text.len() >= 20,
        "{label} must explain the specific behavior, adaptation, or blocker"
    );
    Ok(())
}

fn require_identifier(text: &str, label: &str) -> Result<()> {
    ensure!(
        !text.is_empty()
            && text
                .chars()
                .all(|character| character.is_ascii_alphanumeric()
                    || character == '_'
                    || character == '-'),
        "invalid {label}"
    );
    Ok(())
}

fn validate_artifact(artifact: &Artifact) -> Result<()> {
    let path = Path::new(&artifact.path);
    ensure!(
        !artifact.path.is_empty()
            && path
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
        "artifact paths must be relative and cannot traverse directories"
    );
    ensure!(
        artifact.sha256.len() == 64
            && artifact
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "artifact needs a lowercase SHA-256"
    );
    Ok(())
}

fn verify_artifact(root: &Path, artifact: &Artifact) -> Result<PathBuf> {
    validate_artifact(artifact)?;
    let root = root.canonicalize().context("locating artifact root")?;
    let path = root
        .join(&artifact.path)
        .canonicalize()
        .with_context(|| format!("locating artifact {}", artifact.path))?;
    ensure!(
        path.starts_with(root) && path.is_file(),
        "artifact escapes its root or is not a file"
    );
    let mut reader = BufReader::new(File::open(&path)?);
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    ensure!(
        format!("{:x}", digest.finalize()) == artifact.sha256,
        "artifact SHA-256 mismatch: {}",
        artifact.path
    );
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn seed() -> Result<(Manifest, Ledger)> {
        Ok((
            serde_json::from_str(include_str!(
                "../../../../docs/android-studio/reference-manifest.json"
            ))?,
            serde_json::from_str(include_str!(
                "../../../../docs/android-studio/test-parity.json"
            ))?,
        ))
    }

    fn artifact(root: &Path, path: &str, contents: &str) -> Result<Artifact> {
        let destination = root.join(path);
        fs::create_dir_all(destination.parent().context("artifact parent")?)?;
        fs::write(destination, contents)?;
        Ok(Artifact {
            path: path.into(),
            sha256: format!("{:x}", Sha256::digest(contents.as_bytes())),
        })
    }

    fn commit_fixture(root: &Path) -> Result<String> {
        for arguments in [
            vec!["init", "--quiet"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=ParityTest",
                "-c",
                "user.email=parity-test@example.invalid",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "Record test fixture",
            ],
        ] {
            let output = Command::new("git")
                .args(arguments)
                .current_dir(root)
                .output()?;
            ensure!(
                output.status.success(),
                "fixture Git operation failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root)
            .output()?;
        ensure!(output.status.success(), "reading fixture commit failed");
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }

    fn target(root: &Path, output: Option<&str>, exit_code: i32) -> Result<RustTarget> {
        let mut target = RustTarget {
            package: "parity_fixture".into(),
            integration_target: None,
            test_name: "tests::reference_behavior".into(),
            manifest: artifact(
                root,
                "crate/Cargo.toml",
                "[package]\nname = 'parity_fixture'\nversion = '0.1.0'\n[lib]\npath = 'source.rs'\n",
            )?,
            source: artifact(
                root,
                "crate/source.rs",
                "#[cfg(test)] mod tests { #[test] fn reference_behavior() { assert_eq!(2 + 2, 4); } }\n",
            )?,
            run: None,
        };
        if let Some(output) = output {
            let mut command = vec!["cargo".into()];
            command.extend(target.arguments());
            target.run = Some(RunEvidence {
                tested_commit: commit_fixture(root)?,
                command,
                exit_code,
                log: artifact(root, "docs/android-studio/evidence/run.log", output)?,
            });
        }
        Ok(target)
    }

    const PASS: &str = "running 1 test\ntest tests::reference_behavior ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out; finished in 0.00s\n";
    const FAIL: &str = "running 1 test\ntest tests::reference_behavior ... FAILED\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 5 filtered out; finished in 0.00s\n";

    fn port_first(ledger: &mut Ledger, target: RustTarget, status: RunStatus) -> Result<()> {
        let entry = ledger.entries.first_mut().context("seed entry")?;
        entry.port_status = PortStatus::Ported;
        entry.reason = None;
        entry.run_status = status;
        entry.rust_targets = vec![target];
        Ok(())
    }

    #[test]
    fn partial_seed_is_valid_but_never_complete() -> Result<()> {
        let (manifest, ledger) = seed()?;
        validate(&manifest, &ledger, Path::new("."), None)?;
        ensure!(manifest.tests.len() == 8);
        ensure!(summary(&manifest, &ledger).contains("PARTIAL"));
        ensure!(summary(&manifest, &ledger).contains("UNVERIFIED"));
        ensure!(check_complete(&manifest, &ledger).is_err());
        Ok(())
    }

    #[test]
    fn immutable_pin_changes_and_missing_sources_fail() -> Result<()> {
        let (mut manifest, ledger) = seed()?;
        manifest
            .sources
            .first_mut()
            .context("seed source")?
            .revision = "0".repeat(40);
        ensure!(validate(&manifest, &ledger, Path::new("."), None).is_err());
        let (mut manifest, ledger) = seed()?;
        manifest.sources.pop().context("seed source")?;
        ensure!(validate(&manifest, &ledger, Path::new("."), None).is_err());
        Ok(())
    }

    #[test]
    fn every_reference_is_unique_and_mapped_exactly_once() -> Result<()> {
        let (manifest, ledger) = seed()?;
        let mut duplicate = manifest.clone();
        duplicate
            .tests
            .push(manifest.tests.first().context("seed test")?.clone());
        ensure!(validate(&duplicate, &ledger, Path::new("."), None).is_err());
        let mut duplicate = ledger.clone();
        duplicate
            .entries
            .push(ledger.entries.first().context("seed entry")?.clone());
        ensure!(validate(&manifest, &duplicate, Path::new("."), None).is_err());
        let mut missing = ledger.clone();
        missing.entries.pop().context("seed entry")?;
        ensure!(validate(&manifest, &missing, Path::new("."), None).is_err());
        let mut invented = ledger;
        invented
            .entries
            .first_mut()
            .context("seed entry")?
            .reference_id = "invented.test".into();
        ensure!(validate(&manifest, &invented, Path::new("."), None).is_err());
        Ok(())
    }

    #[test]
    fn inapplicability_requires_a_specific_reason_and_no_run() -> Result<()> {
        let (manifest, mut ledger) = seed()?;
        let entry = ledger.entries.first_mut().context("seed entry")?;
        entry.port_status = PortStatus::NotApplicable;
        entry.reason = Some("N/A".into());
        ensure!(validate(&manifest, &ledger, Path::new("."), None).is_err());
        let entry = ledger.entries.first_mut().context("seed entry")?;
        entry.reason = Some(
            "The reference tests IntelliJ PSI lifecycle internals with no equivalent IDE behavior."
                .into(),
        );
        validate(&manifest, &ledger, Path::new("."), None)?;
        ledger.entries.first_mut().context("seed entry")?.run_status = RunStatus::Passing;
        ensure!(validate(&manifest, &ledger, Path::new("."), None).is_err());
        Ok(())
    }

    #[test]
    fn passing_requires_named_execution_and_current_target_bytes() -> Result<()> {
        let directory = TempDir::new()?;
        let (manifest, mut ledger) = seed()?;
        port_first(
            &mut ledger,
            target(directory.path(), Some(PASS), 0)?,
            RunStatus::Passing,
        )?;
        validate(&manifest, &ledger, directory.path(), None)?;
        fs::write(
            directory.path().join("crate/source.rs"),
            "modified Rust test",
        )?;
        ensure!(validate(&manifest, &ledger, directory.path(), None).is_err());
        Ok(())
    }

    #[test]
    fn passing_without_evidence_and_skip_commands_fail() -> Result<()> {
        let directory = TempDir::new()?;
        let (manifest, mut ledger) = seed()?;
        port_first(
            &mut ledger,
            target(directory.path(), None, 0)?,
            RunStatus::Passing,
        )?;
        ensure!(validate(&manifest, &ledger, directory.path(), None).is_err());
        let mut skipped = target(directory.path(), Some(PASS), 0)?;
        skipped
            .run
            .as_mut()
            .context("run evidence")?
            .command
            .push("--skip=reference_behavior".into());
        port_first(&mut ledger, skipped, RunStatus::Passing)?;
        ensure!(validate(&manifest, &ledger, directory.path(), None).is_err());
        Ok(())
    }

    #[test]
    fn zero_ignored_wrong_or_multiple_executed_tests_cannot_pass() -> Result<()> {
        for output in [
            "running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out;",
            "test tests::reference_behavior ... ignored\ntest result: ok. 0 passed; 0 failed; 1 ignored; 0 measured;",
            "test tests::different_test ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;",
            "test tests::reference_behavior ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured;",
        ] {
            ensure!(verify_test_output(output, "tests::reference_behavior", true).is_err());
        }
        ensure!(
            verify_test_output(&format!("{PASS}{PASS}"), "tests::reference_behavior", true)
                .is_err()
        );
        ensure!(verify_test_output(FAIL, "tests::reference_behavior", true).is_err());
        Ok(())
    }

    #[test]
    fn failing_needs_failure_evidence_and_not_run_has_none() -> Result<()> {
        let directory = TempDir::new()?;
        let (manifest, mut ledger) = seed()?;
        port_first(
            &mut ledger,
            target(directory.path(), Some(FAIL), 101)?,
            RunStatus::Failing,
        )?;
        validate(&manifest, &ledger, directory.path(), None)?;
        port_first(
            &mut ledger,
            target(directory.path(), Some(PASS), 0)?,
            RunStatus::Failing,
        )?;
        ensure!(validate(&manifest, &ledger, directory.path(), None).is_err());
        port_first(
            &mut ledger,
            target(directory.path(), Some(PASS), 0)?,
            RunStatus::NotRun,
        )?;
        ensure!(validate(&manifest, &ledger, directory.path(), None).is_err());
        Ok(())
    }

    #[test]
    fn adaptation_and_blocker_are_independent_explanations() -> Result<()> {
        let directory = TempDir::new()?;
        let (manifest, mut ledger) = seed()?;
        port_first(
            &mut ledger,
            target(directory.path(), None, 0)?,
            RunStatus::Blocked,
        )?;
        let entry = ledger.entries.first_mut().context("seed entry")?;
        entry.port_status = PortStatus::Adapted;
        entry.run_blocker =
            Some("The fixture's required Android SDK platform is not installed.".into());
        ensure!(validate(&manifest, &ledger, directory.path(), None).is_err());
        ledger.entries.first_mut().context("seed entry")?.reason = Some(
            "The GPUI harness exercises the same visible selection in place of IntelliJ Swing."
                .into(),
        );
        validate(&manifest, &ledger, directory.path(), None)?;
        Ok(())
    }

    #[test]
    fn fixture_reuse_requires_original_hash_and_all_mappings() -> Result<()> {
        let directory = TempDir::new()?;
        let (mut manifest, mut ledger) = seed()?;
        let fixture = artifact(directory.path(), "fixtures/original.xml", "<resources/>\n")?;
        let test = manifest.tests.first_mut().context("seed test")?;
        test.fixtures.push(fixture.clone());
        test.fixture_reason = None;
        port_first(
            &mut ledger,
            target(directory.path(), Some(PASS), 0)?,
            RunStatus::Passing,
        )?;
        ensure!(validate(&manifest, &ledger, directory.path(), None).is_err());
        ledger
            .entries
            .first_mut()
            .context("seed entry")?
            .fixtures
            .push(FixturePort {
                reference_path: fixture.path,
                destination: artifact(directory.path(), "fixtures/local.xml", "<resources/>\n")?,
                adapted: false,
                reason: None,
            });
        let commit = commit_fixture(directory.path())?;
        for target in &mut ledger
            .entries
            .first_mut()
            .context("seed entry")?
            .rust_targets
        {
            target.run.as_mut().context("run evidence")?.tested_commit = commit.clone();
        }
        validate(&manifest, &ledger, directory.path(), None)?;
        let fixture = ledger
            .entries
            .first_mut()
            .context("seed entry")?
            .fixtures
            .first_mut()
            .context("fixture mapping")?;
        fixture.destination = artifact(
            directory.path(),
            "fixtures/local.xml",
            "<resources><string/></resources>\n",
        )?;
        ensure!(validate(&manifest, &ledger, directory.path(), None).is_err());
        Ok(())
    }

    #[test]
    fn complete_gate_rejects_unported_not_run_failing_blocked_and_mirror() -> Result<()> {
        let (mut manifest, mut ledger) = seed()?;
        manifest.census_complete = true;
        for source in &mut manifest.sources {
            source.census_complete = true;
        }
        ensure!(check_complete(&manifest, &ledger).is_err());
        for source in &mut manifest.sources {
            if source.coverage == Coverage::UnverifiedMirror {
                source.coverage = Coverage::VerifiedMirror;
            }
        }
        ensure!(check_complete(&manifest, &ledger).is_err());
        for entry in &mut ledger.entries {
            entry.port_status = PortStatus::Ported;
            entry.run_status = RunStatus::Passing;
        }
        check_complete(&manifest, &ledger)?;
        for status in [RunStatus::NotRun, RunStatus::Failing, RunStatus::Blocked] {
            ledger.entries.first_mut().context("seed entry")?.run_status = status;
            ensure!(check_complete(&manifest, &ledger).is_err());
        }
        Ok(())
    }

    #[test]
    fn claimed_census_or_mirror_completeness_without_evidence_fails() -> Result<()> {
        let (mut manifest, ledger) = seed()?;
        manifest
            .sources
            .first_mut()
            .context("seed source")?
            .census_complete = true;
        ensure!(validate(&manifest, &ledger, Path::new("."), None).is_err());
        let (mut manifest, ledger) = seed()?;
        manifest
            .sources
            .iter_mut()
            .find(|source| source.id == "jetbrains-android")
            .context("mirror")?
            .coverage = Coverage::VerifiedMirror;
        ensure!(validate(&manifest, &ledger, Path::new("."), None).is_err());
        Ok(())
    }

    fn attested_fixture(root: &Path) -> Result<(Manifest, Ledger)> {
        let (mut manifest, mut ledger) = seed()?;
        for source in manifest.sources.iter().skip(1) {
            let mut test = manifest.tests.first().context("seed test")?.clone();
            test.source = source.id.clone();
            test.id = format!(
                "{}@{}:{}#{}.{}",
                source.id, source.revision, test.file.path, test.suite, test.method
            );
            ledger.entries.push(Entry {
                reference_id: test.id.clone(),
                port_status: PortStatus::NotApplicable,
                run_status: RunStatus::NotRun,
                reason: Some("Synthetic validator fixture has no product behavior to port.".into()),
                run_blocker: None,
                rust_targets: Vec::new(),
                fixtures: Vec::new(),
            });
            manifest.tests.push(test);
        }
        for entry in &mut ledger.entries {
            entry.port_status = PortStatus::NotApplicable;
            entry.reason =
                Some("Synthetic validator fixture has no product behavior to port.".into());
        }
        manifest.census_complete = true;
        for source in &mut manifest.sources {
            source.census_complete = true;
            let report = serde_json::json!({"source":source.id,"revision":source.revision,"all_suites_and_expansions_reviewed":true,"reviewed_by":"Fixture reviewer","reference_ids":manifest.tests.iter().filter(|test| test.source == source.id).map(|test| &test.id).collect::<Vec<_>>()});
            source.census_evidence = Some(artifact(
                root,
                &format!("docs/android-studio/evidence/{}-census.json", source.id),
                &serde_json::to_string(&report)?,
            )?);
            if source.id == "jetbrains-android" {
                source.coverage = Coverage::VerifiedMirror;
                let report = serde_json::json!({"source":source.id,"revision":source.revision,"canonical_repository":"https://git.jetbrains.org/idea/android.git","canonical_revision":source.revision,"all_omissions_resolved":true,"reviewed_by":"Fixture reviewer"});
                source.coverage_evidence = Some(artifact(
                    root,
                    "docs/android-studio/evidence/mirror.json",
                    &serde_json::to_string(&report)?,
                )?);
            }
        }
        Ok((manifest, ledger))
    }

    #[test]
    fn census_attestation_requires_exact_inventory_revision_and_reviewer() -> Result<()> {
        let directory = TempDir::new()?;
        let (manifest, ledger) = attested_fixture(directory.path())?;
        validate(&manifest, &ledger, directory.path(), None)?;
        check_complete(&manifest, &ledger)?;
        for (field, value) in [
            ("source", serde_json::json!("base")),
            ("revision", serde_json::json!("0".repeat(40))),
            ("reviewed_by", serde_json::json!("")),
            (
                "all_suites_and_expansions_reviewed",
                serde_json::json!(false),
            ),
            ("reference_ids", serde_json::json!([])),
            (
                "reference_ids",
                serde_json::json!([
                    manifest.tests.first().context("seed test")?.id,
                    manifest.tests.first().context("seed test")?.id
                ]),
            ),
        ] {
            let mut changed = manifest.clone();
            let source = changed.sources.first_mut().context("seed source")?;
            let evidence = source.census_evidence.as_ref().context("census artifact")?;
            let mut report: serde_json::Value = read_json(&directory.path().join(&evidence.path))?;
            report[field] = value;
            source.census_evidence = Some(artifact(
                directory.path(),
                "docs/android-studio/evidence/changed-census.json",
                &serde_json::to_string(&report)?,
            )?);
            ensure!(validate(&changed, &ledger, directory.path(), None).is_err());
        }
        Ok(())
    }

    #[test]
    fn mirror_attestation_requires_resolved_omissions_and_canonical_pin() -> Result<()> {
        let directory = TempDir::new()?;
        let (manifest, ledger) = attested_fixture(directory.path())?;
        for (field, value) in [
            ("source", serde_json::json!("idea")),
            ("revision", serde_json::json!("0".repeat(40))),
            (
                "canonical_repository",
                serde_json::json!("https://example.invalid/mirror"),
            ),
            ("canonical_revision", serde_json::json!("moving-tag")),
            ("reviewed_by", serde_json::json!("")),
            ("all_omissions_resolved", serde_json::json!(false)),
        ] {
            let mut changed = manifest.clone();
            let source = changed
                .sources
                .iter_mut()
                .find(|source| source.id == "jetbrains-android")
                .context("mirror source")?;
            let evidence = source
                .coverage_evidence
                .as_ref()
                .context("mirror artifact")?;
            let mut report: serde_json::Value = read_json(&directory.path().join(&evidence.path))?;
            report[field] = value;
            source.coverage_evidence = Some(artifact(
                directory.path(),
                "docs/android-studio/evidence/changed-mirror.json",
                &serde_json::to_string(&report)?,
            )?);
            ensure!(validate(&changed, &ledger, directory.path(), None).is_err());
        }
        Ok(())
    }

    #[test]
    fn completion_runner_deduplicates_exact_manifest_bound_targets() -> Result<()> {
        let directory = TempDir::new()?;
        let (_, mut ledger) = seed()?;
        port_first(
            &mut ledger,
            target(directory.path(), Some(PASS), 0)?,
            RunStatus::Passing,
        )?;
        let first = ledger
            .entries
            .first()
            .context("seed entry")?
            .rust_targets
            .clone();
        ledger
            .entries
            .get_mut(1)
            .context("second entry")?
            .rust_targets = first;
        let mut calls = 0;
        let count = rerun_targets(&ledger, directory.path(), |arguments, working_directory| {
            calls += 1;
            ensure!(working_directory == directory.path());
            ensure!(
                arguments
                    .windows(2)
                    .any(|pair| pair == ["--manifest-path", "crate/Cargo.toml"])
            );
            Ok(TestRun {
                success: true,
                stdout: PASS.into(),
                stderr: String::new(),
            })
        })?;
        ensure!(calls == 1 && count == 1);
        Ok(())
    }

    #[test]
    fn completion_runner_rejects_command_failure_zero_ignored_and_wrong_test() -> Result<()> {
        let directory = TempDir::new()?;
        let (_, mut ledger) = seed()?;
        port_first(
            &mut ledger,
            target(directory.path(), Some(PASS), 0)?,
            RunStatus::Passing,
        )?;
        ensure!(
            rerun_targets(&ledger, directory.path(), |_, _| bail!(
                "command could not start"
            ))
            .is_err()
        );
        ensure!(
            rerun_targets(&ledger, directory.path(), |_, _| Ok(TestRun {
                success: false,
                stdout: PASS.into(),
                stderr: "compiler error".into()
            }))
            .is_err()
        );
        for output in [
            "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured;",
            "test tests::reference_behavior ... ignored\ntest result: ok. 0 passed; 0 failed; 1 ignored; 0 measured;",
            "test tests::other ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;",
        ] {
            ensure!(
                rerun_targets(&ledger, directory.path(), |_, _| Ok(TestRun {
                    success: true,
                    stdout: output.into(),
                    stderr: String::new()
                }))
                .is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn duplicate_package_names_cannot_redirect_the_recorded_manifest() -> Result<()> {
        let directory = TempDir::new()?;
        let first = target(directory.path(), None, 0)?;
        let mut excluded = first.clone();
        excluded.manifest = artifact(
            directory.path(),
            "excluded/Cargo.toml",
            "[package]\nname = 'parity_fixture'\nversion = '0.1.0'\n[workspace]\n[lib]\npath = 'source.rs'\n",
        )?;
        excluded.source = artifact(
            directory.path(),
            "excluded/source.rs",
            "#[test] fn different_behavior() {}\n",
        )?;
        ensure!(first.arguments() != excluded.arguments());
        ensure!(
            excluded
                .arguments()
                .windows(2)
                .any(|pair| pair == ["--manifest-path", "excluded/Cargo.toml"])
        );
        let (manifest, mut ledger) = seed()?;
        excluded.integration_target = Some("nonexistent".into());
        port_first(&mut ledger, excluded, RunStatus::NotRun)?;
        ensure!(validate(&manifest, &ledger, directory.path(), None).is_err());
        Ok(())
    }

    #[test]
    fn passing_evidence_becomes_stale_when_implementation_or_dependencies_change() -> Result<()> {
        let directory = TempDir::new()?;
        let (manifest, mut ledger) = seed()?;
        artifact(
            directory.path(),
            "crate/implementation.rs",
            "pub const VALUE: i32 = 1;\n",
        )?;
        artifact(directory.path(), "Cargo.lock", "version = 4\n")?;
        artifact(
            directory.path(),
            "docs/android-studio/reference-fixtures/behavior.json",
            "{\"expected\":1}\n",
        )?;
        artifact(
            directory.path(),
            "docs/android-studio/reference-fixtures/behavior.md",
            "expected: 1\n",
        )?;
        port_first(
            &mut ledger,
            target(directory.path(), Some(PASS), 0)?,
            RunStatus::Passing,
        )?;
        validate(&manifest, &ledger, directory.path(), None)?;
        fs::write(
            directory.path().join("crate/implementation.rs"),
            "pub const VALUE: i32 = 2;\n",
        )?;
        let status = Command::new("git")
            .args(["add", "crate/implementation.rs"])
            .current_dir(directory.path())
            .status()?;
        ensure!(status.success(), "staging fixture change failed");
        ensure!(validate(&manifest, &ledger, directory.path(), None).is_err());
        fs::write(
            directory.path().join("crate/implementation.rs"),
            "pub const VALUE: i32 = 1;\n",
        )?;
        ensure!(validate(&manifest, &ledger, directory.path(), None).is_err());
        let status = Command::new("git")
            .args(["add", "crate/implementation.rs"])
            .current_dir(directory.path())
            .status()?;
        ensure!(status.success(), "restoring fixture index failed");
        validate(&manifest, &ledger, directory.path(), None)?;
        fs::write(directory.path().join("Cargo.lock"), "version = 3\n")?;
        ensure!(validate(&manifest, &ledger, directory.path(), None).is_err());
        fs::write(directory.path().join("Cargo.lock"), "version = 4\n")?;
        fs::write(
            directory
                .path()
                .join("docs/android-studio/reference-fixtures/behavior.json"),
            "{\"expected\":2}\n",
        )?;
        ensure!(validate(&manifest, &ledger, directory.path(), None).is_err());
        fs::write(
            directory
                .path()
                .join("docs/android-studio/reference-fixtures/behavior.json"),
            "{\"expected\":1}\n",
        )?;
        fs::write(
            directory
                .path()
                .join("docs/android-studio/reference-fixtures/behavior.md"),
            "expected: 2\n",
        )?;
        ensure!(validate(&manifest, &ledger, directory.path(), None).is_err());
        fs::write(
            directory
                .path()
                .join("docs/android-studio/reference-fixtures/behavior.md"),
            "expected: 1\n",
        )?;
        fs::write(
            directory.path().join("crate/untracked.rs"),
            "new implementation",
        )?;
        ensure!(validate(&manifest, &ledger, directory.path(), None).is_err());
        Ok(())
    }

    #[test]
    fn rust_targets_and_fixture_ports_cannot_hide_in_ledger_evidence() -> Result<()> {
        let directory = TempDir::new()?;
        let (mut manifest, mut ledger) = seed()?;
        let mut mapped = target(directory.path(), None, 0)?;
        mapped.manifest = artifact(
            directory.path(),
            "docs/android-studio/evidence/Cargo.toml",
            "[package]\nname = 'parity_fixture'\nversion = '0.1.0'\n[lib]\npath = 'source.rs'\n",
        )?;
        mapped.source = artifact(
            directory.path(),
            "docs/android-studio/evidence/source.rs",
            "#[test] fn reference_behavior() {}\n",
        )?;
        port_first(&mut ledger, mapped, RunStatus::NotRun)?;
        ensure!(validate(&manifest, &ledger, directory.path(), None).is_err());
        port_first(
            &mut ledger,
            target(directory.path(), None, 0)?,
            RunStatus::NotRun,
        )?;
        let original = Artifact {
            path: "upstream/fixture.json".into(),
            sha256: format!("{:x}", Sha256::digest(b"{}")),
        };
        let reference = manifest.tests.first_mut().context("seed test")?;
        reference.fixture_reason = None;
        reference.fixtures.push(original.clone());
        ledger
            .entries
            .first_mut()
            .context("seed entry")?
            .fixtures
            .push(FixturePort {
                reference_path: original.path,
                destination: artifact(
                    directory.path(),
                    "docs/android-studio/evidence/fixture.json",
                    "{}",
                )?,
                adapted: false,
                reason: None,
            });
        ensure!(validate(&manifest, &ledger, directory.path(), None).is_err());
        Ok(())
    }

    #[test]
    fn paths_hashes_and_unknown_schema_fields_are_rejected() -> Result<()> {
        for path in ["../escape.rs", "/absolute.rs", "a/../../escape", ""] {
            ensure!(
                validate_artifact(&Artifact {
                    path: path.into(),
                    sha256: "a".repeat(64)
                })
                .is_err()
            );
        }
        ensure!(
            validate_artifact(&Artifact {
                path: "file.rs".into(),
                sha256: "A".repeat(64)
            })
            .is_err()
        );
        let mut value = serde_json::to_value(serde_json::from_str::<serde_json::Value>(
            include_str!("../../../../docs/android-studio/test-parity.json"),
        )?)?;
        value["silently_ignored_override"] = true.into();
        ensure!(serde_json::from_value::<Ledger>(value).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlink_cannot_escape_artifact_root() -> Result<()> {
        let root = TempDir::new()?;
        let outside = TempDir::new()?;
        let file = artifact(outside.path(), "outside.rs", "external")?;
        std::os::unix::fs::symlink(
            outside.path().join("outside.rs"),
            root.path().join("escape.rs"),
        )?;
        ensure!(
            verify_artifact(
                root.path(),
                &Artifact {
                    path: "escape.rs".into(),
                    sha256: file.sha256
                }
            )
            .is_err()
        );
        Ok(())
    }
}
