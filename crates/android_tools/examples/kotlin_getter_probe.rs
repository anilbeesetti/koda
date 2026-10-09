//! Root-run probe for attributed fixture/version captures; it does not claim parity.

use android_tools::{
    import_facts::{ImportFactsBinding, parse_import_facts},
    kotlin_getter_executor::{
        FixtureBoundary, GradleCaptureOptions, GradleTransport, KotlinGetterCapture,
    },
    kotlin_import_facts::{CaptureBinding, SelectedVariant},
    project_model::{VariantId, parse_model},
};
use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostRevision {
    context_generation: u64,
    model_revision: u64,
    selection_revision: u64,
    import_revision: u64,
    root: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProbeConfiguration {
    project_root: PathBuf,
    wrapper: PathBuf,
    java_home: PathBuf,
    guardian_launcher: PathBuf,
    guardian_library: PathBuf,
    shutdown_timeout_millis: u64,
    model_output: PathBuf,
    host_revision_file: PathBuf,
    fixture_files: Vec<PathBuf>,
    selected_variants: Vec<VariantId>,
    capture_id: String,
    source_epoch: String,
    timeout_millis: u64,
    output_directory: PathBuf,
}

fn main() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let configuration = PathBuf::from(
        arguments
            .next()
            .context("Usage: kotlin_getter_probe CONFIGURATION.json")?,
    );
    ensure!(
        arguments.next().is_none(),
        "Expected exactly one configuration path"
    );
    let configuration: ProbeConfiguration = serde_json::from_slice(&fs::read(configuration)?)?;
    let current_host = || -> Result<HostRevision> {
        Ok(serde_json::from_slice(&fs::read(
            &configuration.host_revision_file,
        )?)?)
    };
    let issued_host = current_host()?;
    ensure!(
        issued_host.root == configuration.project_root,
        "Host revision belongs to another project root"
    );
    let output = fs::read_to_string(&configuration.model_output)?;
    let model = parse_model(&output, &configuration.project_root)?;
    let imports = parse_import_facts(
        &output,
        &model,
        ImportFactsBinding {
            model_revision: issued_host.model_revision,
            selection_revision: issued_host.selection_revision,
            selected_variants: configuration.selected_variants.clone(),
        },
    )?;
    let fixture =
        FixtureBoundary::capture(&configuration.project_root, &configuration.fixture_files)?;
    let binding = CaptureBinding {
        capture_id: configuration.capture_id,
        source_epoch: configuration.source_epoch,
        fixture_before_sha256: fixture.sha256.clone(),
        fixture_after_sha256: fixture.sha256.clone(),
        model_revision: issued_host.model_revision,
        selection_revision: issued_host.selection_revision,
        selected_variants: configuration
            .selected_variants
            .iter()
            .map(|variant| SelectedVariant {
                module: variant.module.clone(),
                variant: variant.variant.clone(),
            })
            .collect(),
    };
    ensure!(
        !configuration.output_directory.exists(),
        "Probe output directory must be fresh"
    );
    fs::create_dir_all(&configuration.output_directory)?;
    let transport = GradleTransport::start(&GradleCaptureOptions {
        wrapper: configuration.wrapper,
        project_root: configuration.project_root,
        java_home: configuration.java_home,
        guardian_launcher: configuration.guardian_launcher,
        guardian_library: configuration.guardian_library,
        shutdown_timeout: Duration::from_millis(configuration.shutdown_timeout_millis),
        timeout: Duration::from_millis(configuration.timeout_millis),
        logs_directory: configuration.output_directory.join("gradle-logs"),
        diagnostic_bytes: 16 * 1024 * 1024,
        cancelled: Arc::new(AtomicBool::new(false)),
    })?;
    let mut capture = KotlinGetterCapture::discover(
        transport,
        issued_host,
        current_host,
        binding,
        fixture,
        &model,
        &imports,
    )?;
    let projects = capture
        .inventory()
        .project_objects
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let plans = projects
        .iter()
        .map(|project| capture.capture_official_project(project))
        .collect::<Result<Vec<_>>>()?;
    let capture = capture.finish(&model, &imports, plans, current_host)?;
    fs::write(
        configuration
            .output_directory
            .join("owned-expected-context.json"),
        serde_json::to_vec_pretty(&capture.expected)?,
    )?;
    fs::write(
        configuration.output_directory.join("getter-events.json"),
        serde_json::to_vec_pretty(capture.snapshot.raw_events())?,
    )?;
    fs::write(
        configuration
            .output_directory
            .join("issued-host-revision.json"),
        serde_json::to_vec_pretty(&capture.issued_host)?,
    )?;
    fs::write(
        configuration.output_directory.join("fixture-boundary.json"),
        serde_json::to_vec_pretty(&capture.fixture)?,
    )?;
    fs::write(
        configuration
            .output_directory
            .join("official-project-request-plans.json"),
        serde_json::to_vec_pretty(&capture.projects)?,
    )?;
    println!("Captured strict Kotlin getter events; original reference parity remains pending");
    Ok(())
}
