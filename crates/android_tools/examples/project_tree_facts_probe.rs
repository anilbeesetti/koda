use android_tools::{
    project_model::{self, EvaluatedProviderMetadata, SourceProviderRootKind},
    project_tree_facts::{ActiveProviderIndex, ProviderPresence, ProviderRole, RootPresence},
};
use anyhow::{Context as _, Result, ensure};
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

fn presence(path: &Path) -> Result<RootPresence> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(RootPresence::Directory),
        Ok(metadata) if metadata.is_file() => Ok(RootPresence::File),
        Ok(_) => anyhow::bail!("Unsupported filesystem entry {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(RootPresence::Missing),
        Err(error) => Err(error).with_context(|| format!("Cannot inspect {}", path.display())),
    }
}

fn main() -> Result<()> {
    let mut arguments = env::args().skip(1);
    let root =
        PathBuf::from(arguments.next().context("Expected smoke project root")?).canonicalize()?;
    let output = fs::read_to_string(arguments.next().context("Expected Gradle output")?)?;
    ensure!(arguments.next().is_none(), "Unexpected argument");
    let model = project_model::parse_model(&output, &root)?;
    let module = model
        .modules
        .iter()
        .find(|module| module.path == ":app")
        .context("App module missing")?;
    let raw = match &module.evaluated_providers {
        Some(EvaluatedProviderMetadata::Available(model)) => model,
        other => anyhow::bail!("Actual evaluated metadata is unavailable: {other:?}"),
    };
    // The pinned ModelBuilder filters dimensions by created variants. A fully
    // disabled flavor remains a DSL declaration, but has no Basic container.
    ensure!(
        raw.product_flavors
            .iter()
            .all(|flavor| flavor.name.as_deref() != Some("orphan")
                && flavor
                    .container
                    .main
                    .as_ref()
                    .is_none_or(|provider| provider.name != "orphan")),
        "Disabled orphan flavor retained an authoritative Basic source set"
    );
    ensure!(
        raw.variants.iter().all(|variant| !variant
            .product_flavors
            .iter()
            .any(|flavor| flavor == "orphan")),
        "Disabled orphan flavor retained an evaluated variant"
    );
    let orphan = module
        .source_providers
        .as_ref()
        .context("Evaluated DSL catalog missing")?
        .providers
        .iter()
        .find(|provider| provider.name == "orphan")
        .context("Disabled orphan flavor disappeared from the evaluated DSL catalog")?;
    ensure!(
        orphan
            .roots
            .iter()
            .any(|source| source.kind == SourceProviderRootKind::Assets
                && source.path == module.directory.join("inactive-assets")),
        "Disabled orphan DSL provider lost its configured asset data"
    );
    let mut captures = Vec::new();
    for (variant, build_type) in [("redWideDebug", "debug"), ("redWideRelease", "release")] {
        let index = ActiveProviderIndex::from_module(module, variant, 1)?;
        let main = index
            .providers()
            .iter()
            .filter(|active| active.role == ProviderRole::Main)
            .map(|active| active.provider.name.as_str())
            .collect::<Vec<_>>();
        ensure!(
            main == ["main", "red", "wide", "redWide", build_type, variant],
            "Unexpected actual main provider order: {main:?}"
        );
        ensure!(
            !index
                .providers()
                .iter()
                .any(|active| active.provider.name == "orphan"),
            "Inactive DSL provider became active"
        );
        let native = index
            .native_membership()
            .context("Native membership capture unavailable")?
            .iter()
            .find(|component| component.artifact == "_main_")
            .context("Main native membership missing")?;
        ensure!(
            native
                .providers
                .as_ref()
                .context("Native provider names unavailable")?
                == &["main", "wide", "red", "redWide", build_type, variant],
            "Native membership order was relabeled"
        );
        let selected = raw
            .variants
            .iter()
            .find(|selected| selected.name == variant)
            .context("Raw variant missing")?;
        if build_type == "release" {
            ensure!(
                selected
                    .host_tests
                    .iter()
                    .all(|artifact| artifact.artifact != "_unit_test_"),
                "Release unit artifact was not disabled"
            );
            ensure!(
                index
                    .providers()
                    .iter()
                    .any(|active| active.role == ProviderRole::UnitTest
                        && active.provider.name == "test"),
                "All-variant default host container was dropped with the selected artifact"
            );
        }
        let mut paths = BTreeMap::new();
        for active in index.providers() {
            for source in &active.provider.roots {
                paths.insert(source.path.clone(), presence(&source.path)?);
                if let Some(parent) = source.path.parent() {
                    paths.insert(parent.into(), presence(parent)?);
                }
            }
        }
        let entry = module
            .directory
            .join("unrelated-assets/nested/shared.txt")
            .components()
            .collect::<PathBuf>();
        paths.insert(entry.clone(), presence(&entry)?);
        let facts = ProviderPresence::new(1, 1, paths)?;
        let resolved = index.resolve(&entry, &facts)?;
        ensure!(
            resolved
                .winner()
                .is_some_and(|winner| winner.name == "main"),
            "Actual shared file did not use first provider"
        );
        captures.push(serde_json::json!({
            "variant": variant, "main": main, "native": native,
            "providers": index.providers().iter().map(|active| serde_json::json!({
                "role": format!("{:?}", active.role), "provider": active.provider,
            })).collect::<Vec<_>>(),
            "sharedFile": entry, "candidates": resolved.candidates.iter().map(|candidate| serde_json::json!({
                "encounter": candidate.encounter, "role": format!("{:?}", candidate.role), "name": candidate.name, "root": candidate.root,
            })).collect::<Vec<_>>(),
        }));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "scope": "supplemental evaluated-provider capture; original five integration methods unported",
            "project": root, "captures": captures, "model": model,
        }))?
    );
    Ok(())
}
