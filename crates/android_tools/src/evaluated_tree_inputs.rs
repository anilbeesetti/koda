/*
 * Copyright (C) 2017 The Android Open Source Project
 * Copyright (C) 2021 The Android Open Source Project
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Evaluated sync facts retained under their original immutable model revision.
//! Light-class filtering adapts AOSP idea's GradleProjectSystemUtil.java131–181
//! and FilenameConstants.kt at a84efec3ba9542d9bfa1255103f0dc94833a3796.

use crate::{
    generated_artifacts::{
        ArtifactSlot, GeneratedArtifactSnapshot, ModelConsumerVersion, parse_generated_artifacts,
    },
    import_facts::{ImportFactsBinding, ImportFactsSnapshot, parse_import_facts},
    project_model::{
        EvaluatedModelPaths, MODEL_OUTPUT_PREFIX, ModelState, ModelToken, ProjectModel, VariantId,
        parse_model, parse_model_with_context,
    },
    project_tree_facts::{FactsUnavailable, FactsUnavailableReason},
};
use anyhow::{Context as _, Result, ensure};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

type FactsResult<T> = std::result::Result<T, FactsUnavailable>;
const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;

fn unavailable(reason: FactsUnavailableReason, detail: impl Into<String>) -> FactsUnavailable {
    FactsUnavailable {
        reason,
        detail: detail.into(),
        path: None,
    }
}

#[derive(Clone, Debug)]
pub struct EvaluatedTreeInputs {
    token: ModelToken,
    model: Arc<ProjectModel>,
    record: Arc<str>,
    import_facts: FactsResult<ImportFactsSnapshot>,
    raw_generated_artifacts: FactsResult<serde_json::Value>,
}

impl EvaluatedTreeInputs {
    /// This is the production sync decoder. All sidecars refer to this one record.
    pub fn decode_sync(
        output: &str,
        root: &Path,
        paths: Option<&EvaluatedModelPaths>,
        token: &ModelToken,
    ) -> Result<Self> {
        let mut records = output
            .lines()
            .filter(|line| line.starts_with(MODEL_OUTPUT_PREFIX));
        let record = records
            .next()
            .context("Gradle returned no Android project model")?;
        ensure!(
            records.next().is_none(),
            "Gradle returned multiple Android project models"
        );
        ensure!(
            record.len() <= MAX_RECORD_BYTES,
            "Android project model exceeds the 16 MiB decoding limit"
        );
        let model = Arc::new(match paths {
            Some(paths) => parse_model_with_context(record, root, paths)?,
            None => parse_model(record, root)?,
        });
        let import_facts = parse_import_facts(
            record,
            &model,
            ImportFactsBinding {
                model_revision: token.revision(),
                selection_revision: token.revision(),
                selected_variants: Vec::new(),
            },
        );
        let wire: serde_json::Value = serde_json::from_str(
            record
                .strip_prefix(MODEL_OUTPUT_PREFIX)
                .context("Android model record prefix is missing")?,
        )?;
        let raw_generated_artifacts = match wire.get("generatedArtifacts") {
            Some(value) if value.is_object() => Ok(value.clone()),
            Some(_) => Err(unavailable(
                FactsUnavailableReason::Malformed,
                "generatedArtifacts must be an object",
            )),
            None => Err(unavailable(
                FactsUnavailableReason::MissingMetadata,
                "generatedArtifacts is absent from the legacy model",
            )),
        };
        Ok(Self {
            token: token.clone(),
            model,
            record: Arc::from(record),
            import_facts,
            raw_generated_artifacts,
        })
    }

    pub fn token(&self) -> &ModelToken {
        &self.token
    }
    pub fn model(&self) -> &Arc<ProjectModel> {
        &self.model
    }
    pub fn raw_record(&self) -> &str {
        &self.record
    }
    pub fn import_facts(&self) -> FactsResult<&ImportFactsSnapshot> {
        self.import_facts.as_ref().map_err(Clone::clone)
    }
    pub fn raw_generated_artifacts(&self) -> FactsResult<&serde_json::Value> {
        self.raw_generated_artifacts.as_ref().map_err(Clone::clone)
    }

    /// Live sync has not declared a supported Rust consumer schema. Retain raw
    /// facts rather than treating the producer's minimum as our capability.
    pub fn generated_artifacts(
        &self,
        consumer: Option<&ModelConsumerVersion>,
    ) -> FactsResult<GeneratedArtifactSnapshot> {
        self.raw_generated_artifacts()?;
        let consumer = consumer.ok_or_else(|| unavailable(FactsUnavailableReason::Capability,
            "Rust generated-artifact model consumer identity is unavailable; evaluated raw record retained"))?;
        parse_generated_artifacts(&self.record, &self.model, self.token.revision(), consumer)
    }
}

/// Independent kind outcomes retain resources/assets when source filtering is
/// unavailable. A complete tree plan must require every relevant kind to succeed.
#[derive(Clone, Debug)]
pub struct SelectedMainGeneratedRoots {
    capture: Arc<EvaluatedTreeInputs>,
    selection: ModelToken,
    selected: VariantId,
    java: FactsResult<Vec<PathBuf>>,
    resources: FactsResult<Vec<PathBuf>>,
    assets: FactsResult<Vec<PathBuf>>,
}

impl SelectedMainGeneratedRoots {
    pub fn selected(&self) -> &VariantId {
        &self.selected
    }
    pub fn model_revision(&self) -> u64 {
        self.capture.token.revision()
    }
    pub fn selection_revision(&self) -> u64 {
        self.selection.revision()
    }
    pub fn model(&self) -> &ProjectModel {
        &self.capture.model
    }
    pub fn java(&self) -> FactsResult<&[PathBuf]> {
        self.java.as_deref().map_err(Clone::clone)
    }
    pub fn resources(&self) -> FactsResult<&[PathBuf]> {
        self.resources.as_deref().map_err(Clone::clone)
    }
    pub fn assets(&self) -> FactsResult<&[PathBuf]> {
        self.assets.as_deref().map_err(Clone::clone)
    }

    pub fn ensure_current(&self, state: &ModelState) -> FactsResult<()> {
        let current = state
            .evaluated_inputs()
            .is_some_and(|capture| Arc::ptr_eq(capture, &self.capture));
        let model = state
            .model
            .as_ref()
            .is_some_and(|model| Arc::ptr_eq(model, &self.capture.model));
        let selected = state.selected.as_ref().is_some_and(|selected| {
            Arc::ptr_eq(&selected.model, &self.capture.model)
                && selected.variants.get(&self.selected.module) == Some(&self.selected.variant)
        });
        if !current
            || !model
            || !selected
            || !state.is_current(&self.selection)
            || state.model_revision() != Some(self.model_revision())
        {
            return Err(unavailable(
                FactsUnavailableReason::Stale,
                "Selected generated roots belong to an outdated model/root/selection",
            ));
        }
        Ok(())
    }
}

pub fn prepare_selected_main_generated_roots(
    state: &ModelState,
    selection: &ModelToken,
    selected: VariantId,
    consumer: &ModelConsumerVersion,
) -> FactsResult<SelectedMainGeneratedRoots> {
    let capture = state
        .evaluated_inputs()
        .ok_or_else(|| {
            unavailable(
                FactsUnavailableReason::MissingMetadata,
                "Evaluated Android tree inputs are unavailable",
            )
        })?
        .clone();
    let mut roots = SelectedMainGeneratedRoots {
        capture,
        selection: selection.clone(),
        selected,
        java: Ok(Vec::new()),
        resources: Ok(Vec::new()),
        assets: Ok(Vec::new()),
    };
    roots.ensure_current(state)?;
    let snapshot = roots.capture.generated_artifacts(Some(consumer))?;
    snapshot.ensure_current(&roots.capture.model, roots.model_revision())?;
    let module = snapshot
        .modules()
        .iter()
        .find(|module| module.module == roots.selected.module)
        .ok_or_else(|| {
            unavailable(
                FactsUnavailableReason::MissingMetadata,
                "Selected generated module is unavailable",
            )
        })?;
    let versions = module.versions.available()?;
    let variant = snapshot.variant(&roots.selected.module, &roots.selected.variant)?;
    let ArtifactSlot::Present(artifact) = variant.main.available()? else {
        return Err(unavailable(
            FactsUnavailableReason::UnsupportedShape,
            "Selected V2 main artifact is absent",
        ));
    };
    roots.java = module.build_folder.available().and_then(|build_folder| {
        artifact
            .generated_source_folders
            .available()
            .map(|folders| filter_generated_java(folders, build_folder))
    });
    roots.resources = artifact.generated_resource_folders.available().cloned();
    roots.assets = artifact.generated_assets(versions).map(<[PathBuf]>::to_vec);
    Ok(roots)
}

fn filter_generated_java(folders: &[PathBuf], build_folder: &Path) -> Vec<PathBuf> {
    let generated = build_folder.join("generated");
    let excluded = [
        generated.join("source/r"),
        generated.join("not_namespaced_r_class_sources"),
        generated.join("data_binding_base_class_source_out"),
        generated.join("source/navigation-args"),
    ];
    folders
        .iter()
        .filter(|folder| !excluded.iter().any(|excluded| folder.starts_with(excluded)))
        .cloned()
        .collect()
}
