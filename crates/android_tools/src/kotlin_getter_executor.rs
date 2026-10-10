//! Rust-owned requests over Gradle's official JVM getter runtime. The JVM bridge
//! reports observations; it never supplies the expected context for a packet.
//! Reflection boundaries follow the Apache-2.0 files in test_data/kotlin_import_facts.

use crate::{
    import_facts::ImportFactsSnapshot,
    kotlin_capture_budget::{BoundedJsonWriter, JsonUsage},
    kotlin_getter_lifetime::OwnedGradleRuntime,
    kotlin_import_facts::{
        CaptureBinding, CaptureContext, CaptureLimits, CaptureMode, CaptureObject, CaptureValue,
        Consumer, ContainerOrder, GetterArgument, GetterEvent, GetterOutcome, GetterPurpose,
        GetterRequest, ImportIdentity, KotlinFactsSnapshot, MethodCatalogue, MethodSelection,
        MissingMethod, ObjectKind, RequestParameter, ReturnShape, RuntimeArtifact, RuntimeClass,
        RuntimeIdentity, RuntimeLoader, ValueKind, parse_kotlin_facts,
    },
    project_model::{Module, ModuleKind, ProjectModel, Variant},
};
use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize, ser::SerializeSeq as _};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Component, Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const FACTS_ENVELOPE: &[u8] = b"{\"kotlinFacts\":";
const BRIDGE: &str = include_str!("kotlin_getter_capture.gradle");
pub const KOTLIN_PLUGIN_IDS: &[&str] = &[
    "kotlin",
    "kotlin2js",
    "kotlin-android",
    "kotlin-platform-jvm",
    "kotlin-platform-js",
    "kotlin-platform-common",
    "org.jetbrains.kotlin.multiplatform",
    "kotlin-multiplatform",
];
pub const ANDROID_PLUGIN_IDS: &[&str] = &[
    "com.android.application",
    "com.android.library",
    "com.android.dynamic-feature",
    "com.android.test",
    "com.android.kotlin.multiplatform.library",
];
const WRAPPER: &str = "org.jetbrains.kotlin.gradle.plugin.KotlinPluginWrapperKt";
const RESOLVER: &str = "org.jetbrains.kotlin.gradle.plugin.ide.IdeCompilerArgumentsResolver";
const KOTLIN_TASK_CLASSES: &[&str] = &[
    "org.jetbrains.kotlin.gradle.tasks.KotlinCompile_Decorated",
    "org.jetbrains.kotlin.gradle.tasks.KotlinCompileWithWorkers_Decorated",
    "org.jetbrains.kotlin.gradle.tasks.Kotlin2JsCompile_Decorated",
    "org.jetbrains.kotlin.gradle.tasks.KotlinCompileCommon_Decorated",
    "org.jetbrains.kotlin.gradle.tasks.Kotlin2JsCompileWithWorkers_Decorated",
    "org.jetbrains.kotlin.gradle.tasks.KotlinCompileCommonWithWorkers_Decorated",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureFile {
    pub relative_path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}

/// An explicit original/transformed fixture file inventory. Generated build/cache
/// files are outside this boundary; omission of an original file is a caller error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureBoundary {
    pub root: PathBuf,
    pub files: Vec<FixtureFile>,
    pub sha256: String,
}

impl FixtureBoundary {
    pub fn capture(root: &Path, relative_paths: &[PathBuf]) -> Result<Self> {
        Self::capture_monitored(root, relative_paths, &mut || Ok(()))
    }

    fn capture_monitored(
        root: &Path,
        relative_paths: &[PathBuf],
        monitor: &mut impl FnMut() -> Result<()>,
    ) -> Result<Self> {
        monitor()?;
        let root = root
            .canonicalize()
            .context("Resolve capture fixture root")?;
        let mut paths = BTreeSet::new();
        let mut files = Vec::new();
        let mut digest = Sha256::new();
        for relative_path in relative_paths {
            ensure!(
                !relative_path.as_os_str().is_empty()
                    && relative_path.components().collect::<PathBuf>().as_os_str()
                        == relative_path.as_os_str()
                    && relative_path
                        .components()
                        .all(|part| matches!(part, Component::Normal(_))),
                "Fixture paths must be unambiguous relative file paths"
            );
            ensure!(paths.insert(relative_path), "Duplicate fixture path");
        }
        for relative_path in paths {
            monitor()?;
            let mut path = root.clone();
            for part in relative_path.components() {
                path.push(part.as_os_str());
                ensure!(
                    !fs::symlink_metadata(&path)?.file_type().is_symlink(),
                    "Fixture path contains a symlink"
                );
            }
            let (bytes, sha256) = hash_regular_file_monitored(&path, monitor)?;
            let name = relative_path
                .to_str()
                .context("Fixture path is not UTF-8")?;
            digest.update((name.len() as u64).to_be_bytes());
            digest.update(name.as_bytes());
            digest.update(bytes.to_be_bytes());
            digest.update(sha256.as_bytes());
            files.push(FixtureFile {
                relative_path: relative_path.clone(),
                bytes,
                sha256,
            });
        }
        ensure!(
            !files.is_empty(),
            "Capture requires an explicit nonempty fixture inventory"
        );
        Ok(Self {
            root,
            files,
            sha256: format!("{:x}", digest.finalize()),
        })
    }

    pub fn ensure_unchanged(&self) -> Result<()> {
        self.ensure_unchanged_monitored(&mut || Ok(()))
    }

    fn ensure_unchanged_monitored(&self, monitor: &mut impl FnMut() -> Result<()>) -> Result<()> {
        let paths = self
            .files
            .iter()
            .map(|file| file.relative_path.clone())
            .collect::<Vec<_>>();
        ensure!(
            Self::capture_monitored(&self.root, &paths, monitor)? == *self,
            "Fixture changed during Kotlin getter capture"
        );
        Ok(())
    }
}

#[cfg(test)]
fn hash_regular_file(path: &Path) -> Result<(u64, String)> {
    hash_regular_file_monitored(path, &mut || Ok(()))
}

fn hash_regular_file_monitored(
    path: &Path,
    monitor: &mut impl FnMut() -> Result<()>,
) -> Result<(u64, String)> {
    monitor()?;
    let before = fs::symlink_metadata(path)?;
    ensure!(
        before.is_file() && !before.file_type().is_symlink(),
        "Runtime/fixture artifact must be a regular file"
    );
    let mut file = File::open(path)?;
    let opened = file.metadata()?;
    ensure!(
        same_file(&before, &opened),
        "Artifact identity changed before opening"
    );
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        monitor()?;
        let count = file.read(&mut buffer)?;
        monitor()?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .context("Artifact byte count overflow")?;
        ensure!(bytes <= before.len(), "Artifact grew while hashing");
        digest.update(&buffer[..count]);
    }
    let after = file.metadata()?;
    let path_after = fs::symlink_metadata(path)?;
    monitor()?;
    ensure!(
        same_file(&before, &after) && same_file(&before, &path_after) && bytes == before.len(),
        "Artifact changed while hashing"
    );
    Ok((bytes, format!("{:x}", digest.finalize())))
}

fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        left.dev() == right.dev()
            && left.ino() == right.ino()
            && left.mode() == right.mode()
            && left.len() == right.len()
            && left.mtime() == right.mtime()
            && left.mtime_nsec() == right.mtime_nsec()
            && left.ctime() == right.ctime()
            && left.ctime_nsec() == right.ctime_nsec()
    }
    #[cfg(not(unix))]
    {
        left.is_file() == right.is_file()
            && left.len() == right.len()
            && left.modified().ok() == right.modified().ok()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassLookupAttempt {
    pub loader: String,
    pub class: Option<String>,
    pub failures: Vec<crate::kotlin_import_facts::ExceptionCause>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassLookup {
    pub project: String,
    pub name: String,
    pub classes: Vec<String>,
    pub attempts: Vec<ClassLookupAttempt>,
    pub failures: Vec<crate::kotlin_import_facts::ExceptionCause>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscoveryInventory {
    pub runtime: RuntimeIdentity,
    pub objects: Vec<CaptureObject>,
    pub catalogues: Vec<MethodCatalogue>,
    pub project_objects: BTreeMap<String, String>,
    pub class_lookups: Vec<ClassLookup>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscoveryPosition {
    pub artifacts: usize,
    pub loaders: usize,
    pub classes: usize,
    pub objects: usize,
    pub catalogues: usize,
}

impl DiscoveryPosition {
    fn at(inventory: &DiscoveryInventory) -> Self {
        Self {
            artifacts: inventory.runtime.artifacts.len(),
            loaders: inventory.runtime.loaders.len(),
            classes: inventory.runtime.classes.len(),
            objects: inventory.objects.len(),
            catalogues: inventory.catalogues.len(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoaderArtifactsAddition {
    pub loader: String,
    pub previous: usize,
    pub artifacts: Vec<String>,
}

/// A frame can append observations, but cannot provide a replacement for an
/// accepted row. Session/request/sequence and predecessor positions are issued
/// independently by Rust, rather than inferred from the returned frame.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscoveryDelta {
    pub session: String,
    pub request: String,
    pub sequence: u64,
    pub predecessor: DiscoveryPosition,
    pub artifacts: Vec<RuntimeArtifact>,
    pub loaders: Vec<RuntimeLoader>,
    pub classes: Vec<RuntimeClass>,
    pub objects: Vec<CaptureObject>,
    pub catalogues: Vec<MethodCatalogue>,
    pub loader_artifacts: Vec<LoaderArtifactsAddition>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DiscoveryBootstrap {
    session: String,
    inventory: DiscoveryInventory,
}

impl DiscoveryInventory {
    #[cfg(test)]
    fn verify_artifacts(&self) -> Result<()> {
        self.verify_artifacts_monitored(&mut || Ok(()))
    }

    fn verify_artifacts_monitored(&self, monitor: &mut impl FnMut() -> Result<()>) -> Result<()> {
        let mut ids = BTreeSet::new();
        for artifact in &self.runtime.artifacts {
            monitor()?;
            ensure!(
                ids.insert(&artifact.id),
                "Duplicate runtime artifact identity"
            );
            ensure!(
                artifact.path.is_absolute() && artifact.path.canonicalize()? == artifact.path,
                "Runtime artifact is not canonical"
            );
            let (bytes, digest) = hash_regular_file_monitored(&artifact.path, monitor)?;
            ensure!(
                bytes == artifact.bytes && digest == artifact.sha256,
                "Runtime artifact bytes/digest disagree with owned discovery"
            );
        }
        ensure!(
            ids.contains(&self.runtime.java.artifact),
            "Java runtime artifact was not independently observed"
        );
        Ok(())
    }

    #[cfg(test)]
    fn validate_extension(&self, next: &Self) -> Result<()> {
        ensure!(
            self.runtime.gradle_version == next.runtime.gradle_version
                && self.runtime.java == next.runtime.java
                && self.runtime.locale == next.runtime.locale
                && self.project_objects == next.project_objects
                && self.class_lookups == next.class_lookups,
            "Owned runtime/discovery identity changed"
        );
        preserve_rows(&self.runtime.artifacts, &next.runtime.artifacts, |row| {
            &row.id
        })?;
        preserve_rows(&self.runtime.classes, &next.runtime.classes, |row| &row.id)?;
        preserve_rows(&self.objects, &next.objects, |row| &row.id)?;
        preserve_rows(&self.catalogues, &next.catalogues, |row| &row.id)?;
        let loaders = index_rows(&next.runtime.loaders, |row| &row.id)?;
        for (position, loader) in self.runtime.loaders.iter().enumerate() {
            let current = loaders
                .get(loader.id.as_str())
                .context("Runtime loader disappeared")?;
            ensure!(
                current.parent == loader.parent
                    && current.artifacts.starts_with(&loader.artifacts)
                    && next
                        .runtime
                        .loaders
                        .get(position)
                        .is_some_and(|row| row.id == loader.id),
                "Runtime loader provenance was rewritten"
            );
        }
        Ok(())
    }

    #[cfg(test)]
    fn extend(&mut self, next: Self) -> Result<()> {
        self.extend_monitored(next, &mut || Ok(()))
    }

    #[cfg(test)]
    fn extend_monitored(
        &mut self,
        next: Self,
        monitor: &mut impl FnMut() -> Result<()>,
    ) -> Result<()> {
        self.validate_extension(&next)?;
        self.accept_validated_extension(next, monitor)
    }

    #[cfg(test)]
    fn accept_validated_extension(
        &mut self,
        next: Self,
        monitor: &mut impl FnMut() -> Result<()>,
    ) -> Result<()> {
        // New return objects are discovered in a separate frame, before an event
        // can refer to them. Existing rows are never accepted from event metadata.
        for artifact in next
            .runtime
            .artifacts
            .iter()
            .skip(self.runtime.artifacts.len())
        {
            let (bytes, digest) = hash_regular_file_monitored(&artifact.path, monitor)?;
            ensure!(
                bytes == artifact.bytes && digest == artifact.sha256,
                "New runtime artifact differs from discovery"
            );
        }
        *self = next;
        Ok(())
    }
}

#[derive(Default)]
struct DiscoveryIndex {
    objects: BTreeMap<String, usize>,
    catalogues: BTreeMap<String, usize>,
    class_catalogues: BTreeMap<String, usize>,
    classes: BTreeMap<String, usize>,
    artifacts: BTreeMap<String, usize>,
    loaders: BTreeMap<String, usize>,
    loader_artifacts: BTreeMap<String, BTreeSet<String>>,
    methods: BTreeSet<String>,
    named_classes: BTreeMap<String, BTreeMap<String, usize>>,
}

impl DiscoveryIndex {
    #[cfg(test)]
    fn new(inventory: &DiscoveryInventory) -> Result<Self> {
        Self::new_monitored(inventory, &mut || Ok(()))
    }

    fn new_monitored(
        inventory: &DiscoveryInventory,
        monitor: &mut impl FnMut() -> Result<()>,
    ) -> Result<Self> {
        let mut index = Self::default();
        index.append_monitored(inventory, 0, 0, 0, monitor)?;
        for (position, row) in inventory.runtime.artifacts.iter().enumerate() {
            if position % 64 == 0 {
                monitor()?;
            }
            ensure!(
                index.artifacts.insert(row.id.clone(), position).is_none(),
                "Duplicate runtime artifact identity"
            );
        }
        for (position, row) in inventory.runtime.loaders.iter().enumerate() {
            if position % 64 == 0 {
                monitor()?;
            }
            ensure!(
                index.loaders.insert(row.id.clone(), position).is_none(),
                "Duplicate runtime loader identity"
            );
            let artifacts = row.artifacts.iter().cloned().collect::<BTreeSet<_>>();
            ensure!(
                artifacts.len() == row.artifacts.len(),
                "Duplicate runtime loader artifact"
            );
            index.loader_artifacts.insert(row.id.clone(), artifacts);
        }
        for (position, lookup) in inventory.class_lookups.iter().enumerate() {
            if position % 64 == 0 {
                monitor()?;
            }
            ensure!(
                index
                    .named_classes
                    .entry(lookup.project.clone())
                    .or_default()
                    .insert(lookup.name.clone(), position)
                    .is_none(),
                "Duplicate explicit class lookup"
            );
        }
        Ok(index)
    }

    #[cfg(test)]
    fn append(
        &mut self,
        inventory: &DiscoveryInventory,
        objects: usize,
        catalogues: usize,
        classes: usize,
    ) -> Result<()> {
        self.append_monitored(inventory, objects, catalogues, classes, &mut || Ok(()))
    }

    fn append_monitored(
        &mut self,
        inventory: &DiscoveryInventory,
        objects: usize,
        catalogues: usize,
        classes: usize,
        monitor: &mut impl FnMut() -> Result<()>,
    ) -> Result<()> {
        for (position, row) in inventory.objects.iter().enumerate().skip(objects) {
            if (position - objects) % 64 == 0 {
                monitor()?;
            }
            ensure!(
                self.objects.insert(row.id.clone(), position).is_none(),
                "Duplicate discovered object identity"
            );
        }
        for (position, row) in inventory.catalogues.iter().enumerate().skip(catalogues) {
            if (position - catalogues) % 64 == 0 {
                monitor()?;
            }
            for (position, method) in row.methods.iter().enumerate() {
                if position % 64 == 0 {
                    monitor()?;
                }
                ensure!(
                    self.methods.insert(method.id.clone()),
                    "Duplicate reflected method identity"
                );
            }
            ensure!(
                self.catalogues.insert(row.id.clone(), position).is_none(),
                "Duplicate discovered catalogue identity"
            );
            ensure!(
                self.class_catalogues
                    .insert(row.class_id.clone(), position)
                    .is_none(),
                "Several catalogues describe one runtime class"
            );
        }
        for (position, row) in inventory.runtime.classes.iter().enumerate().skip(classes) {
            if (position - classes) % 64 == 0 {
                monitor()?;
            }
            ensure!(
                self.classes.insert(row.id.clone(), position).is_none(),
                "Duplicate discovered class identity"
            );
        }
        Ok(())
    }

    fn validate_delta(
        &self,
        inventory: &DiscoveryInventory,
        delta: &DiscoveryDelta,
        session: &str,
        sequence: u64,
        request: &str,
        monitor: &mut impl FnMut() -> Result<()>,
    ) -> Result<usize> {
        monitor()?;
        ensure!(
            delta.session == session && delta.request == request,
            "Discovery delta belongs to a different session/request"
        );
        ensure!(
            sequence.checked_add(1) == Some(delta.sequence),
            "Discovery delta sequence was replayed or reordered"
        );
        ensure!(
            delta.predecessor == DiscoveryPosition::at(inventory),
            "Discovery predecessor was rewritten or truncated"
        );
        let mut examined = 0;
        let mut step = || -> Result<()> {
            examined += 1;
            if examined % 64 == 0 {
                monitor()?;
            }
            Ok(())
        };
        let artifacts =
            additional_ids(&delta.artifacts, &self.artifacts, |row| &row.id, &mut step)?;
        let loaders = additional_ids(&delta.loaders, &self.loaders, |row| &row.id, &mut step)?;
        let classes = additional_ids(&delta.classes, &self.classes, |row| &row.id, &mut step)?;
        additional_ids(&delta.objects, &self.objects, |row| &row.id, &mut step)?;
        additional_ids(
            &delta.catalogues,
            &self.catalogues,
            |row| &row.id,
            &mut step,
        )?;
        let has_artifact = |id: &str| self.artifacts.contains_key(id) || artifacts.contains(id);
        let has_loader = |id: &str| self.loaders.contains_key(id) || loaders.contains(id);
        let has_class = |id: &str| self.classes.contains_key(id) || classes.contains(id);
        let mut new_loader_artifacts = BTreeMap::new();
        for row in &delta.loaders {
            step()?;
            ensure!(
                row.parent.as_deref().is_none_or(&has_loader),
                "New loader has undiscovered parent provenance"
            );
            let mut seen = BTreeSet::new();
            for artifact in &row.artifacts {
                step()?;
                ensure!(
                    has_artifact(artifact) && seen.insert(artifact.as_str()),
                    "New loader has unknown/duplicate artifact provenance"
                );
            }
            new_loader_artifacts.insert(row.id.as_str(), seen);
        }
        let mut changed_loader_artifacts = BTreeMap::new();
        let mut changed_loaders = BTreeSet::new();
        for addition in &delta.loader_artifacts {
            step()?;
            ensure!(
                changed_loaders.insert(&addition.loader),
                "Several deltas rewrite one loader"
            );
            let position = self
                .loaders
                .get(&addition.loader)
                .context("Loader addition does not refer to a retained loader")?;
            let loader = inventory
                .runtime
                .loaders
                .get(*position)
                .context("Retained loader missing")?;
            ensure!(
                addition.previous == loader.artifacts.len() && !addition.artifacts.is_empty(),
                "Loader artifact predecessor was rewritten/truncated"
            );
            let old = self
                .loader_artifacts
                .get(&addition.loader)
                .context("Retained loader artifact index missing")?;
            let mut new = BTreeSet::new();
            for artifact in &addition.artifacts {
                step()?;
                ensure!(
                    has_artifact(artifact)
                        && !old.contains(artifact)
                        && new.insert(artifact.as_str()),
                    "Loader artifact was unknown, duplicated or rewritten"
                );
            }
            changed_loader_artifacts.insert(addition.loader.as_str(), new);
        }
        for row in &delta.classes {
            step()?;
            ensure!(
                has_loader(&row.loader),
                "New runtime class has unknown loader provenance"
            );
            if let crate::kotlin_import_facts::ClassOrigin::Artifact(artifact) = &row.origin {
                ensure!(
                    has_artifact(artifact),
                    "New runtime class has unknown artifact provenance"
                );
                ensure!(
                    self.loader_artifacts
                        .get(&row.loader)
                        .is_some_and(|artifacts| artifacts.contains(artifact))
                        || new_loader_artifacts
                            .get(row.loader.as_str())
                            .is_some_and(|artifacts| artifacts.contains(artifact.as_str()))
                        || changed_loader_artifacts
                            .get(row.loader.as_str())
                            .is_some_and(|artifacts| artifacts.contains(artifact.as_str())),
                    "New runtime class artifact does not belong to its observed loader"
                );
            }
            for parent in row.superclass.iter().chain(&row.interfaces) {
                step()?;
                ensure!(
                    has_class(parent),
                    "New runtime class ancestry is undiscovered"
                );
            }
        }
        for row in &delta.objects {
            step()?;
            ensure!(
                inventory.project_objects.contains_key(&row.project) && has_class(&row.class_id),
                "New object changed project/class provenance"
            );
        }
        let mut catalogue_classes = BTreeSet::new();
        let mut method_ids = BTreeSet::new();
        for row in &delta.catalogues {
            step()?;
            ensure!(
                has_class(&row.class_id)
                    && !self.class_catalogues.contains_key(&row.class_id)
                    && catalogue_classes.insert(&row.class_id),
                "Runtime class catalogue was rewritten or duplicated"
            );
            for method in &row.methods {
                step()?;
                ensure!(
                    !self.methods.contains(&method.id) && method_ids.insert(&method.id),
                    "Reflected method identity was reused"
                );
                ensure!(
                    has_class(&method.declaring_class),
                    "Reflected declaring class is undiscovered"
                );
                for class in method
                    .parameter_classes
                    .iter()
                    .filter_map(Option::as_deref)
                    .chain(method.return_class.as_deref())
                {
                    step()?;
                    ensure!(
                        has_class(class),
                        "Reflected parameter/return class is undiscovered"
                    );
                }
            }
        }
        for artifact in &delta.artifacts {
            ensure!(
                artifact.path.is_absolute() && artifact.path.canonicalize()? == artifact.path,
                "New runtime artifact is not canonical"
            );
            let (bytes, digest) = hash_regular_file_monitored(&artifact.path, monitor)?;
            ensure!(
                bytes == artifact.bytes && digest == artifact.sha256,
                "New runtime artifact differs from discovery"
            );
        }
        monitor()?;
        Ok(examined)
    }

    #[cfg(test)]
    fn apply_delta(
        &mut self,
        inventory: &mut DiscoveryInventory,
        delta: DiscoveryDelta,
    ) -> Result<()> {
        self.apply_delta_monitored(inventory, delta, &mut || Ok(()))
    }

    fn apply_delta_monitored(
        &mut self,
        inventory: &mut DiscoveryInventory,
        delta: DiscoveryDelta,
        monitor: &mut impl FnMut() -> Result<()>,
    ) -> Result<()> {
        let before = DiscoveryPosition::at(inventory);
        for (offset, artifact) in delta.artifacts.iter().enumerate() {
            if offset % 64 == 0 {
                monitor()?;
            }
            self.artifacts
                .insert(artifact.id.clone(), before.artifacts + offset);
        }
        for (offset, loader) in delta.loaders.iter().enumerate() {
            if offset % 64 == 0 {
                monitor()?;
            }
            self.loaders
                .insert(loader.id.clone(), before.loaders + offset);
            self.loader_artifacts.insert(
                loader.id.clone(),
                loader.artifacts.iter().cloned().collect(),
            );
        }
        for addition in delta.loader_artifacts {
            monitor()?;
            let position = self
                .loaders
                .get(&addition.loader)
                .context("Validated loader index missing")?;
            let loader = inventory
                .runtime
                .loaders
                .get_mut(*position)
                .context("Validated loader missing")?;
            self.loader_artifacts
                .get_mut(&addition.loader)
                .context("Validated artifact index missing")?
                .extend(addition.artifacts.iter().cloned());
            loader.artifacts.extend(addition.artifacts);
        }
        inventory.runtime.artifacts.extend(delta.artifacts);
        inventory.runtime.loaders.extend(delta.loaders);
        inventory.runtime.classes.extend(delta.classes);
        inventory.objects.extend(delta.objects);
        inventory.catalogues.extend(delta.catalogues);
        self.append_monitored(
            inventory,
            before.objects,
            before.catalogues,
            before.classes,
            monitor,
        )
    }

    fn object<'a>(&self, inventory: &'a DiscoveryInventory, id: &str) -> Result<&'a CaptureObject> {
        self.objects
            .get(id)
            .and_then(|position| inventory.objects.get(*position))
            .context("Getter receiver object was not discovered")
    }

    fn catalogue<'a>(
        &self,
        inventory: &'a DiscoveryInventory,
        id: &str,
    ) -> Result<&'a MethodCatalogue> {
        self.catalogues
            .get(id)
            .and_then(|position| inventory.catalogues.get(*position))
            .context("Requested catalogue has not been discovered")
    }

    fn object_catalogue<'a>(
        &self,
        inventory: &'a DiscoveryInventory,
        owner: &str,
    ) -> Result<&'a MethodCatalogue> {
        let object = self.object(inventory, owner)?;
        self.class_catalogues
            .get(&object.class_id)
            .and_then(|position| inventory.catalogues.get(*position))
            .context("Getter receiver lacks a reflected catalogue")
    }

    fn named_class_catalogue<'a>(
        &self,
        inventory: &'a DiscoveryInventory,
        project: &str,
        name: &str,
    ) -> Result<Option<&'a MethodCatalogue>> {
        let position = self
            .named_classes
            .get(project)
            .and_then(|names| names.get(name))
            .context("Class was not explicitly looked up")?;
        let lookup = inventory
            .class_lookups
            .get(*position)
            .context("Captured class lookup missing")?;
        ensure!(
            lookup.classes.len() <= 1,
            "Several plugin loaders resolve the requested class; choose a captured loader explicitly"
        );
        Ok(lookup
            .classes
            .first()
            .and_then(|class| self.class_catalogues.get(class))
            .and_then(|position| inventory.catalogues.get(*position)))
    }
}

fn additional_ids<'a, T>(
    rows: &'a [T],
    retained: &BTreeMap<String, usize>,
    id: impl Fn(&'a T) -> &'a str,
    monitor: &mut impl FnMut() -> Result<()>,
) -> Result<BTreeSet<&'a str>> {
    let mut result = BTreeSet::new();
    for row in rows {
        monitor()?;
        let id = id(row);
        ensure!(
            !id.is_empty() && !retained.contains_key(id) && result.insert(id),
            "Discovery identity was reused or rewritten"
        );
    }
    Ok(result)
}

impl DiscoveryDelta {
    #[cfg(test)]
    fn empty(
        inventory: &DiscoveryInventory,
        session: &str,
        request: &GetterRequest,
        sequence: u64,
    ) -> Self {
        Self {
            session: session.into(),
            request: request.id.clone(),
            sequence,
            predecessor: DiscoveryPosition::at(inventory),
            artifacts: vec![],
            loaders: vec![],
            classes: vec![],
            objects: vec![],
            catalogues: vec![],
            loader_artifacts: vec![],
        }
    }
}

#[cfg(test)]
fn preserve_rows<T: PartialEq>(before: &[T], after: &[T], id: impl Fn(&T) -> &str) -> Result<()> {
    let rows = index_rows(after, &id)?;
    for (position, row) in before.iter().enumerate() {
        ensure!(
            rows.get(id(row)).is_some_and(|current| *current == row)
                && after.get(position) == Some(row),
            "Discovery catalogue was overwritten or truncated"
        );
    }
    Ok(())
}

#[cfg(test)]
fn index_rows<T>(rows: &[T], id: impl Fn(&T) -> &str) -> Result<BTreeMap<&str, &T>> {
    let mut indexed = BTreeMap::new();
    for row in rows {
        ensure!(
            indexed.insert(id(row), row).is_none(),
            "Duplicate discovery identity"
        );
    }
    Ok(indexed)
}

#[derive(Deserialize, Serialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
enum Response {
    Hello(String),
    Discovery(DiscoveryBootstrap),
    DiscoveryDelta(DiscoveryDelta),
    Event(GetterEvent),
    Finished(()),
    Failure(String),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DiscoveryRequest<'a> {
    projects: &'a [String],
    session: &'a str,
    class_names: &'a [&'a str],
}

#[derive(Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
enum Request<'a> {
    Discover(DiscoveryRequest<'a>),
    Invoke(&'a GetterRequest),
    Finish(()),
}

/// A transport must deliver discovery independently of event responses. Tests
/// can supply a controlled transport without pretending to execute Gradle.
pub trait GetterTransport {
    fn discover(&mut self, projects: &[String], session: &str) -> Result<DiscoveryInventory>;
    fn invoke(&mut self, request: &GetterRequest) -> Result<(DiscoveryDelta, GetterEvent)>;
    fn finish(&mut self) -> Result<()>;
    fn abort(&mut self) -> Result<()>;
    fn check_progress(&mut self) -> Result<()>;
}

fn abort_capture<T>(transport: &mut impl GetterTransport, error: anyhow::Error) -> Result<T> {
    match transport.abort() {
        Ok(()) => Err(error),
        Err(closure) => Err(error.context(format!(
            "Owned getter runtime closure also failed: {closure:#}"
        ))),
    }
}

pub struct GradleTransport {
    child: OwnedGradleRuntime,
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    _directory: TempDir,
    logs_directory: PathBuf,
    diagnostic_bytes: u64,
    deadline: Instant,
    cancelled: std::sync::Arc<AtomicBool>,
    finished: bool,
}

#[derive(Clone, Debug)]
pub struct GradleCaptureOptions {
    pub wrapper: PathBuf,
    pub project_root: PathBuf,
    /// Runtime artifacts and fixture must be on this host, not a remote daemon.
    pub java_home: PathBuf,
    pub guardian_launcher: PathBuf,
    pub guardian_library: PathBuf,
    pub shutdown_timeout: Duration,
    pub timeout: Duration,
    /// Fresh caller-owned directory; logs survive successful or failed capture.
    pub logs_directory: PathBuf,
    /// Exceeding this combined stdout/stderr budget rejects the entire capture.
    pub diagnostic_bytes: u64,
    pub cancelled: std::sync::Arc<AtomicBool>,
}

impl GradleTransport {
    pub fn start(options: &GradleCaptureOptions) -> Result<Self> {
        ensure!(
            !options.timeout.is_zero() && options.timeout.as_millis() <= i32::MAX as u128,
            "Capture timeout must fit the JVM socket timeout"
        );
        ensure!(
            options.wrapper.is_absolute()
                && options.project_root.is_absolute()
                && options.java_home.is_absolute()
                && options.guardian_launcher.is_absolute()
                && options.guardian_library.is_absolute(),
            "Owned Gradle paths must be absolute"
        );
        ensure!(
            options.diagnostic_bytes > 0 && options.logs_directory.is_absolute(),
            "Capture requires an absolute log directory and diagnostic budget"
        );
        let deadline = Instant::now()
            .checked_add(options.timeout)
            .context("Capture deadline overflow")?;
        check_deadline(deadline, &options.cancelled)?;
        fs::create_dir(&options.logs_directory)
            .context("Create fresh owned Gradle log directory")?;
        let directory = tempfile::Builder::new()
            .prefix("koda-kotlin-getters-")
            .tempdir()?;
        let script = directory.path().join("capture.gradle");
        fs::write(&script, BRIDGE)?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let token: String = (0..4)
            .map(|_| format!("{:016x}", rand::random::<u64>()))
            .collect();
        let port = listener.local_addr()?.port();
        let output = File::create(options.logs_directory.join("stdout.log"))?;
        let errors = File::create(options.logs_directory.join("stderr.log"))?;
        let mut command = Command::new(&options.wrapper);
        let producer_limits = CaptureLimits::default();
        command
            .current_dir(&options.project_root)
            .env("JAVA_HOME", &options.java_home)
            .args(["--no-daemon", "--console=plain", "--init-script"])
            .arg(&script)
            .arg(format!("-Dkoda.kotlin.capture.port={port}"))
            .arg(format!("-Dkoda.kotlin.capture.token={token}"))
            .arg(format!(
                "-Dkoda.kotlin.capture.nativeLibrary={}",
                options.guardian_library.canonicalize()?.display()
            ))
            .arg(format!(
                "-Dkoda.kotlin.capture.timeoutMillis={}",
                options.timeout.as_millis()
            ))
            .arg(format!(
                "-Dkoda.kotlin.capture.maximumFrameBytes={}",
                producer_limits.record_bytes
            ))
            .arg(format!(
                "-Dkoda.kotlin.capture.maximumValueNodes={}",
                producer_limits.entries
            ))
            .arg(format!(
                "-Dkoda.kotlin.capture.maximumScalarBytes={}",
                producer_limits.string_bytes
            ))
            .arg("help");
        let mut child = OwnedGradleRuntime::spawn(
            command,
            &options.guardian_launcher,
            &options.guardian_library,
            Stdio::from(output),
            Stdio::from(errors),
            options.shutdown_timeout,
        )
        .context("Start owned Gradle getter runtime")?;
        let connection = (|| -> Result<_> {
            let stream = loop {
                check_deadline(deadline, &options.cancelled)?;
                check_diagnostics(&options.logs_directory, options.diagnostic_bytes)?;
                match listener.accept() {
                    Ok((stream, peer)) => {
                        ensure!(
                            peer.ip().is_loopback(),
                            "Getter transport accepted a nonlocal connection"
                        );
                        break stream;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if let Some(status) = child.try_wait()? {
                            bail!(
                                "Gradle getter runtime exited before discovery: {status}; logs in {}",
                                options.logs_directory.display()
                            );
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            stream.set_read_timeout(Some(Duration::from_millis(100)))?;
            stream.set_write_timeout(Some(Duration::from_millis(100)))?;
            let writer = stream.try_clone()?;
            Ok((stream, writer))
        })();
        let (stream, writer) = match connection {
            Ok(connection) => connection,
            Err(error) => return close_runtime_error(&mut child, error),
        };
        let mut transport = Self {
            child,
            reader: BufReader::new(stream),
            writer,
            _directory: directory,
            logs_directory: options.logs_directory.clone(),
            diagnostic_bytes: options.diagnostic_bytes,
            deadline,
            cancelled: options.cancelled.clone(),
            finished: false,
        };
        let authentication = match transport.receive() {
            Ok(Response::Hello(actual)) if actual == token => Ok(()),
            Ok(_) => Err(anyhow::anyhow!(
                "Getter runtime did not authenticate the owned session"
            )),
            Err(error) => Err(error),
        };
        match authentication {
            Ok(()) => Ok(transport),
            Err(error) => abort_capture(&mut transport, error),
        }
    }

    pub fn logs_directory(&self) -> &Path {
        &self.logs_directory
    }

    fn checked<T>(&mut self, operation: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        match operation(self) {
            Ok(value) => Ok(value),
            Err(error) => abort_capture(self, error),
        }
    }

    fn send(&mut self, request: &Request<'_>) -> Result<()> {
        check_deadline(self.deadline, &self.cancelled)?;
        check_diagnostics(&self.logs_directory, self.diagnostic_bytes)?;
        let mut writer = BoundedJsonWriter::new(Vec::new(), CaptureLimits::default(), 0);
        serde_json::to_writer(&mut writer, request)
            .context("Bounded Kotlin getter request frame")?;
        let mut bytes = writer.into_inner();
        bytes.push(b'\n');
        let child = &self.child;
        let logs_directory = &self.logs_directory;
        let diagnostic_bytes = self.diagnostic_bytes;
        write_frame_monitored(
            &mut self.writer,
            &bytes,
            self.deadline,
            &self.cancelled,
            || {
                child.health_check()?;
                check_diagnostics(logs_directory, diagnostic_bytes)
            },
        )
    }

    fn receive(&mut self) -> Result<Response> {
        let child = &self.child;
        let logs_directory = &self.logs_directory;
        let diagnostic_bytes = self.diagnostic_bytes;
        let bytes = read_frame_monitored(
            &mut self.reader,
            self.deadline,
            &self.cancelled,
            MAX_FRAME_BYTES,
            || {
                child.health_check()?;
                check_diagnostics(logs_directory, diagnostic_bytes)
            },
        )?;
        let response = decode_response_monitored(&bytes, &mut || self.check_progress())?;
        if let Response::Failure(detail) = response {
            bail!("Gradle getter discovery unavailable: {detail}");
        }
        Ok(response)
    }
}

fn decode_response_monitored(
    bytes: &[u8],
    monitor: &mut impl FnMut() -> Result<()>,
) -> Result<Response> {
    let mut writer = BoundedJsonWriter::new(std::io::sink(), CaptureLimits::default(), 0);
    for chunk in bytes.chunks(64 * 1024) {
        monitor()?;
        writer
            .write_all(chunk)
            .context("Bounded Kotlin getter response frame")?;
    }
    struct MonitoredBytes<'a, F> {
        bytes: &'a [u8],
        offset: usize,
        checked_at: usize,
        monitor: F,
        failure: Option<anyhow::Error>,
    }
    impl<F: FnMut() -> Result<()>> Read for MonitoredBytes<'_, F> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            // Deserializer error recovery can read again after a failed health check.
            if self.failure.is_some() {
                return Err(std::io::Error::other("Response decoder monitoring failed"));
            }
            if self.offset == 0 || self.offset - self.checked_at >= 64 * 1024 {
                if let Err(error) = (self.monitor)() {
                    self.failure = Some(error);
                    return Err(std::io::Error::other("Response decoder monitoring failed"));
                }
                self.checked_at = self.offset;
            }
            let remaining = self
                .bytes
                .get(self.offset..)
                .ok_or_else(|| std::io::Error::other("Response decoder position overflow"))?;
            let length = output.len().min(remaining.len());
            output
                .get_mut(..length)
                .ok_or_else(|| std::io::Error::other("Response decoder buffer overflow"))?
                .copy_from_slice(&remaining[..length]);
            self.offset += length;
            Ok(length)
        }
    }
    let mut reader = MonitoredBytes {
        bytes,
        offset: 0,
        checked_at: 0,
        monitor: &mut *monitor,
        failure: None,
    };
    let response = serde_json::from_reader(&mut reader);
    if let Some(error) = reader.failure {
        return Err(error).context("Decode owned getter response frame");
    }
    let response = response.context("Decode owned getter response frame")?;
    monitor()?;
    Ok(response)
}

impl GetterTransport for GradleTransport {
    fn check_progress(&mut self) -> Result<()> {
        check_deadline(self.deadline, &self.cancelled)?;
        self.child.health_check()?;
        check_diagnostics(&self.logs_directory, self.diagnostic_bytes)
    }
    fn discover(&mut self, projects: &[String], session: &str) -> Result<DiscoveryInventory> {
        self.checked(|this| {
            this.send(&Request::Discover(DiscoveryRequest {
                projects,
                session,
                class_names: &[WRAPPER, RESOLVER],
            }))?;
            match this.receive()? {
                Response::Discovery(bootstrap) => {
                    ensure!(
                        bootstrap.session == session,
                        "Discovery bootstrap belongs to a different session"
                    );
                    Ok(bootstrap.inventory)
                }
                _ => bail!("Expected separate discovery frame"),
            }
        })
    }

    fn invoke(&mut self, request: &GetterRequest) -> Result<(DiscoveryDelta, GetterEvent)> {
        self.checked(|this| {
            this.send(&Request::Invoke(request))?;
            let inventory = match this.receive()? {
                Response::DiscoveryDelta(delta) => delta,
                _ => bail!("Getter event arrived without discovery metadata"),
            };
            let event = match this.receive()? {
                Response::Event(event) => event,
                _ => bail!("Expected exact getter event"),
            };
            Ok((inventory, event))
        })
    }

    fn finish(&mut self) -> Result<()> {
        self.checked(|this| {
            this.send(&Request::Finish(()))?;
            ensure!(
                matches!(this.receive()?, Response::Finished(())),
                "Runtime did not acknowledge capture completion"
            );
            await_capture_completion(
                &mut this.child,
                this.deadline,
                &this.cancelled,
                &this.logs_directory,
                this.diagnostic_bytes,
            )?;
            this.finished = true;
            Ok(())
        })
    }

    fn abort(&mut self) -> Result<()> {
        let socket = self
            .writer
            .shutdown(std::net::Shutdown::Both)
            .or_else(|error| {
                if error.kind() == std::io::ErrorKind::NotConnected {
                    Ok(())
                } else {
                    Err(error)
                }
            });
        let closure = self.child.close();
        match (socket, closure) {
            (Ok(()), Ok(())) => {
                self.finished = true;
                Ok(())
            }
            (Err(socket), Err(closure)) => {
                Err(closure.context(format!("Owned getter socket close also failed: {socket}")))
            }
            (Err(socket), Ok(())) => Err(socket.into()),
            (Ok(()), Err(closure)) => Err(closure),
        }
    }
}

impl Drop for GradleTransport {
    fn drop(&mut self) {
        if !self.finished {
            if let Err(error) = self.abort() {
                log::error!("Unable to close owned Kotlin getter runtime: {error:#}");
            }
        }
    }
}

fn close_runtime_error<T>(runtime: &mut OwnedGradleRuntime, error: anyhow::Error) -> Result<T> {
    match runtime.close() {
        Ok(()) => Err(error),
        Err(closure) => Err(error.context(format!(
            "Owned getter runtime closure also failed: {closure:#}"
        ))),
    }
}

trait CaptureProcess {
    fn terminal_status(&mut self) -> Result<Option<ExitStatus>>;
    fn close_owned(&mut self) -> Result<()>;
}

impl CaptureProcess for OwnedGradleRuntime {
    fn terminal_status(&mut self) -> Result<Option<ExitStatus>> {
        self.try_wait()
    }
    fn close_owned(&mut self) -> Result<()> {
        self.close()
    }
}

fn await_capture_completion(
    process: &mut impl CaptureProcess,
    deadline: Instant,
    cancelled: &AtomicBool,
    logs_directory: &Path,
    diagnostic_bytes: u64,
) -> Result<()> {
    loop {
        check_deadline(deadline, cancelled)?;
        check_diagnostics(logs_directory, diagnostic_bytes)?;
        if let Some(status) = process.terminal_status()? {
            process.close_owned()?;
            check_deadline(deadline, cancelled)?;
            check_diagnostics(logs_directory, diagnostic_bytes)?;
            ensure!(
                status.success(),
                "Gradle runtime failed after getter capture: {status}"
            );
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn check_deadline(deadline: Instant, cancelled: &AtomicBool) -> Result<()> {
    check_deadline_at(deadline, cancelled, Instant::now())
}

fn check_deadline_at(deadline: Instant, cancelled: &AtomicBool, now: Instant) -> Result<()> {
    ensure!(
        !cancelled.load(Ordering::Acquire),
        "Kotlin getter capture was cancelled"
    );
    ensure!(now < deadline, "Kotlin getter capture deadline elapsed");
    Ok(())
}

fn read_frame_monitored(
    reader: &mut impl BufRead,
    deadline: Instant,
    cancelled: &AtomicBool,
    maximum: usize,
    mut monitor: impl FnMut() -> Result<()>,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        check_deadline(deadline, cancelled)?;
        monitor()?;
        let buffer = match reader.fill_buf() {
            Ok(buffer) => buffer,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        ensure!(
            !buffer.is_empty(),
            "Getter runtime closed an incomplete frame"
        );
        let count = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |offset| offset + 1);
        let complete = buffer.get(count - 1) == Some(&b'\n');
        ensure!(
            bytes.len().checked_add(count).is_some_and(|length| maximum
                .checked_add(1)
                .is_some_and(|maximum| length <= maximum)),
            "Getter response exceeds frame budget"
        );
        bytes.extend_from_slice(&buffer[..count]);
        reader.consume(count);
        if complete {
            bytes.pop();
            return Ok(bytes);
        }
    }
}

fn write_frame_monitored(
    writer: &mut impl Write,
    bytes: &[u8],
    deadline: Instant,
    cancelled: &AtomicBool,
    monitor: impl FnMut() -> Result<()>,
) -> Result<()> {
    write_frame_monitored_with_clock(writer, bytes, deadline, cancelled, monitor, Instant::now)
}

fn write_frame_monitored_with_clock(
    writer: &mut impl Write,
    bytes: &[u8],
    deadline: Instant,
    cancelled: &AtomicBool,
    mut monitor: impl FnMut() -> Result<()>,
    mut now: impl FnMut() -> Instant,
) -> Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        check_deadline_at(deadline, cancelled, now())?;
        monitor()?;
        let end = offset.saturating_add(64 * 1024).min(bytes.len());
        match writer.write(&bytes[offset..end]) {
            Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::WriteZero).into()),
            Ok(count) => offset += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error.into()),
        }
        check_deadline_at(deadline, cancelled, now())?;
        monitor()?;
    }
    check_deadline_at(deadline, cancelled, now())?;
    monitor()?;
    writer.flush()?;
    check_deadline_at(deadline, cancelled, now())?;
    monitor()
}

fn check_diagnostics(directory: &Path, maximum: u64) -> Result<()> {
    let stdout = fs::metadata(directory.join("stdout.log"))?.len();
    let stderr = fs::metadata(directory.join("stderr.log"))?.len();
    ensure!(
        stdout
            .checked_add(stderr)
            .is_some_and(|bytes| bytes <= maximum),
        "Gradle diagnostic output exceeded capture budget"
    );
    Ok(())
}

#[cfg(test)]
fn read_frame(
    reader: &mut impl BufRead,
    deadline: Instant,
    cancelled: &AtomicBool,
    maximum: usize,
) -> Result<Vec<u8>> {
    read_frame_monitored(reader, deadline, cancelled, maximum, || Ok(()))
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfficialProjectRequests {
    pub project: String,
    pub plugin_lookups: BTreeMap<String, String>,
    pub plugin_iteration: Option<String>,
    pub android_base_plugin: Option<String>,
    pub extension_lookup: Option<String>,
    pub compiler_version: Option<String>,
    pub task_iteration: Option<String>,
    pub source_set_names: BTreeMap<String, String>,
    pub compiler_arguments: BTreeMap<String, String>,
    pub unavailable: Vec<String>,
}

enum GetterReceiver<'a> {
    Object(&'a str),
    StaticCatalogue(&'a str),
}

struct GetterInvocation<'a> {
    project: &'a str,
    receiver: GetterReceiver<'a>,
    name: &'a str,
    descriptor: &'a str,
    shape: ReturnShape,
    arguments: Vec<GetterArgument>,
    purpose: GetterPurpose,
    after: Option<&'a str>,
}

struct AdditionalRow<'a, T> {
    rows: &'a [T],
    additional: Option<&'a T>,
}

impl<T: Serialize> Serialize for AdditionalRow<'_, T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(
            self.rows.len() + usize::from(self.additional.is_some()),
        ))?;
        for row in self.rows.iter().chain(self.additional) {
            sequence.serialize_element(row)?;
        }
        sequence.end()
    }
}

struct ModuleRows<'a>(&'a [Module]);
struct VariantNames<'a>(&'a [Variant]);

impl Serialize for VariantNames<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for variant in self.0 {
            sequence.serialize_element(&variant.name)?;
        }
        sequence.end()
    }
}

impl Serialize for ModuleRows<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Identity<'a> {
            module: &'a str,
            directory: &'a Path,
            kind: ModuleKind,
            variants: VariantNames<'a>,
        }
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for module in self.0 {
            sequence.serialize_element(&Identity {
                module: &module.path,
                directory: &module.directory,
                kind: module.kind,
                variants: VariantNames(&module.variants),
            })?;
        }
        sequence.end()
    }
}

struct CaptureRetention {
    root: PathBuf,
    modules: Box<serde_json::value::RawValue>,
    limits: CaptureLimits,
    usage: JsonUsage,
    #[cfg(test)]
    measured_bytes: std::cell::Cell<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BorrowedCaptureContext<'a> {
    binding: &'a CaptureBinding,
    imports: &'a ImportIdentity,
    mode: CaptureMode,
    runtime: &'a RuntimeIdentity,
    objects: &'a [CaptureObject],
    catalogues: &'a [MethodCatalogue],
    requests: AdditionalRow<'a, GetterRequest>,
}

#[derive(Serialize)]
struct BorrowedKotlinFacts<'a> {
    schema: u32,
    root: &'a Path,
    modules: &'a serde_json::value::RawValue,
    context: BorrowedCaptureContext<'a>,
    events: AdditionalRow<'a, GetterEvent>,
}

impl CaptureRetention {
    fn for_model(model: &ProjectModel) -> Result<Self> {
        let limits = CaptureLimits::default();
        let mut writer = BoundedJsonWriter::new(Vec::new(), limits, FACTS_ENVELOPE.len() + 1);
        serde_json::to_writer(&mut writer, &ModuleRows(&model.modules))?;
        let modules =
            serde_json::value::RawValue::from_string(String::from_utf8(writer.into_inner())?)?;
        Ok(Self {
            root: model.root.clone(),
            modules,
            limits,
            usage: JsonUsage::default(),
            #[cfg(test)]
            measured_bytes: std::cell::Cell::new(0),
        })
    }

    fn measure_monitored(
        &self,
        value: &impl Serialize,
        monitor: &mut impl FnMut() -> Result<()>,
    ) -> Result<JsonUsage> {
        struct MonitoredWriter<'a, W, F> {
            inner: &'a mut W,
            monitor: F,
            unchecked_bytes: usize,
        }
        impl<W: Write, F: FnMut() -> Result<()>> Write for MonitoredWriter<'_, W, F> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                let mut remaining = bytes;
                while !remaining.is_empty() {
                    if self.unchecked_bytes == 64 * 1024 {
                        (self.monitor)().map_err(std::io::Error::other)?;
                        self.unchecked_bytes = 0;
                    }
                    let length = remaining.len().min(64 * 1024 - self.unchecked_bytes);
                    self.inner.write_all(&remaining[..length])?;
                    self.unchecked_bytes += length;
                    remaining = &remaining[length..];
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.inner.flush()
            }
        }
        let mut writer = BoundedJsonWriter::new(std::io::sink(), self.limits, 0);
        serde_json::to_writer(
            MonitoredWriter {
                inner: &mut writer,
                monitor,
                unchecked_bytes: 0,
            },
            value,
        )?;
        let usage = writer.usage();
        #[cfg(test)]
        self.measured_bytes
            .set(self.measured_bytes.get() + usage.bytes);
        Ok(usage)
    }

    #[cfg(test)]
    fn array_addition<T: Serialize>(
        &self,
        old_length: usize,
        additional: &[T],
    ) -> Result<JsonUsage> {
        self.array_addition_monitored(old_length, additional, &mut || Ok(()))
    }

    fn array_addition_monitored<T: Serialize>(
        &self,
        old_length: usize,
        additional: &[T],
        monitor: &mut impl FnMut() -> Result<()>,
    ) -> Result<JsonUsage> {
        let mut usage = JsonUsage::default();
        for (offset, row) in additional.iter().enumerate() {
            if offset % 64 == 0 {
                monitor()?;
            }
            let mut row = self.measure_monitored(row, monitor)?;
            row.bytes += usize::from(old_length != 0 || offset != 0);
            usage = usage.checked_add(row, self.limits)?;
        }
        Ok(usage)
    }
}

pub struct KotlinGetterCapture<T: GetterTransport, H: Clone + PartialEq> {
    issued_host: H,
    transport: T,
    binding: CaptureBinding,
    fixture: FixtureBoundary,
    imports: ImportIdentity,
    inventory: DiscoveryInventory,
    requests: Vec<GetterRequest>,
    events: Vec<GetterEvent>,
    event_ids: BTreeSet<String>,
    retention: CaptureRetention,
    index: DiscoveryIndex,
    session: String,
    sequence: u64,
    ancestry_results: BTreeMap<(String, String), bool>,
    ancestry_work: usize,
    #[cfg(test)]
    discovery_rows_examined: usize,
    failed: bool,
}

pub struct CapturedKotlinGetters<H> {
    pub issued_host: H,
    pub snapshot: KotlinFactsSnapshot,
    pub expected: CaptureContext,
    pub projects: Vec<OfficialProjectRequests>,
    pub fixture: FixtureBoundary,
}

impl<T: GetterTransport, H: Clone + PartialEq> KotlinGetterCapture<T, H> {
    /// `issued_host` is the original host ImportRevision, including its context
    /// generation/import revision. The current-host callback must read the host;
    /// returning a freshly made matching token defeats the publication boundary.
    /// The adapter consumes this same original token after strict validation.
    pub fn discover(
        mut transport: T,
        issued_host: H,
        mut current_host: impl FnMut() -> Result<H>,
        binding: CaptureBinding,
        fixture: FixtureBoundary,
        model: &ProjectModel,
        imports: &ImportFactsSnapshot,
    ) -> Result<Self> {
        let session = format!(
            "{}:{:016x}{:016x}",
            binding.capture_id,
            rand::random::<u64>(),
            rand::random::<u64>()
        );
        let discovery = (|| -> Result<_> {
            ensure!(
                current_host()? == issued_host,
                "Host import revision changed before discovery"
            );
            fixture.ensure_unchanged_monitored(&mut || transport.check_progress())?;
            ensure!(
                model.root.canonicalize()? == fixture.root,
                "Fixture boundary belongs to a different project root"
            );
            ensure!(
                binding.fixture_before_sha256 == fixture.sha256
                    && binding.fixture_after_sha256 == fixture.sha256,
                "Host capture binding does not match fixture boundary"
            );
            ensure!(
                binding.model_revision == imports.binding().model_revision
                    && binding.selection_revision == imports.binding().selection_revision,
                "Host capture revision does not match import identity"
            );
            ensure!(
                binding
                    .selected_variants
                    .iter()
                    .map(|variant| (&variant.module, &variant.variant))
                    .eq(imports
                        .binding()
                        .selected_variants
                        .iter()
                        .map(|variant| (&variant.module, &variant.variant))),
                "Host capture selection differs from import identity"
            );
            imports
                .ensure_current(model, imports.binding())
                .map_err(|error| anyhow::anyhow!("{error:?}"))?;
            let projects = imports
                .projects()
                .map_err(|error| anyhow::anyhow!("{error:?}"))?
                .iter()
                .map(|project| {
                    project
                        .project_path
                        .as_ref()
                        .context("Imported project path was not captured")?
                        .available()
                        .cloned()
                        .map_err(|error| anyhow::anyhow!("{error:?}"))
                })
                .collect::<Result<Vec<_>>>()?;
            let inventory = transport.discover(&projects, &session)?;
            ensure!(
                inventory.runtime.gradle_version == imports.gradle_version(),
                "Runtime Gradle version differs from imported identity"
            );
            ensure!(
                inventory.project_objects.keys().collect::<BTreeSet<_>>()
                    == projects.iter().collect::<BTreeSet<_>>(),
                "Discovery omitted or invented imported projects"
            );
            let mut retention = CaptureRetention::for_model(model)?;
            let import_identity = ImportIdentity {
                build_identity: imports.build_identity().clone(),
                project_catalogue: imports.catalogue_observation().clone(),
            };
            let facts = BorrowedKotlinFacts {
                schema: 1,
                root: &retention.root,
                modules: &retention.modules,
                context: BorrowedCaptureContext {
                    binding: &binding,
                    imports: &import_identity,
                    mode: CaptureMode::Invocation,
                    runtime: &inventory.runtime,
                    objects: &inventory.objects,
                    catalogues: &inventory.catalogues,
                    requests: AdditionalRow {
                        rows: &[],
                        additional: None,
                    },
                },
                events: AdditionalRow {
                    rows: &[],
                    additional: None,
                },
            };
            let mut writer =
                BoundedJsonWriter::new(std::io::sink(), retention.limits, FACTS_ENVELOPE.len() + 1);
            serde_json::to_writer(&mut writer, &facts)
                .context("Kotlin getter aggregate discovery budget")?;
            retention.usage = writer.usage();
            inventory.verify_artifacts_monitored(&mut || transport.check_progress())?;
            ensure!(
                current_host()? == issued_host,
                "Host import revision changed during discovery"
            );
            Ok((inventory, retention, import_identity))
        })();
        let (inventory, retention, imports) = match discovery {
            Ok(discovery) => discovery,
            Err(error) => return abort_capture(&mut transport, error),
        };
        let index =
            match DiscoveryIndex::new_monitored(&inventory, &mut || transport.check_progress()) {
                Ok(index) => index,
                Err(error) => return abort_capture(&mut transport, error),
            };
        Ok(Self {
            issued_host,
            transport,
            binding,
            fixture,
            imports,
            inventory,
            requests: Vec::new(),
            events: Vec::new(),
            event_ids: BTreeSet::new(),
            retention,
            index,
            session,
            sequence: 0,
            ancestry_results: BTreeMap::new(),
            ancestry_work: 0,
            #[cfg(test)]
            discovery_rows_examined: 0,
            failed: false,
        })
    }

    pub fn inventory(&self) -> &DiscoveryInventory {
        &self.inventory
    }

    fn reject<TValue>(&mut self, error: anyhow::Error) -> Result<TValue> {
        if self.failed {
            return Err(error);
        }
        self.failed = true;
        abort_capture(&mut self.transport, error)
    }

    fn borrowed_facts<'a>(
        &'a self,
        inventory: &'a DiscoveryInventory,
        request: Option<&'a GetterRequest>,
        event: Option<&'a GetterEvent>,
    ) -> BorrowedKotlinFacts<'a> {
        BorrowedKotlinFacts {
            schema: 1,
            root: &self.retention.root,
            modules: &self.retention.modules,
            context: BorrowedCaptureContext {
                binding: &self.binding,
                imports: &self.imports,
                mode: CaptureMode::Invocation,
                runtime: &inventory.runtime,
                objects: &inventory.objects,
                catalogues: &inventory.catalogues,
                requests: AdditionalRow {
                    rows: &self.requests,
                    additional: request,
                },
            },
            events: AdditionalRow {
                rows: &self.events,
                additional: event,
            },
        }
    }

    #[cfg(test)]
    fn check_retention(
        &self,
        inventory: &DiscoveryInventory,
        request: Option<&GetterRequest>,
        event: Option<&GetterEvent>,
    ) -> Result<JsonUsage> {
        let mut usage = self.retention.usage;
        let limits = self.retention.limits;
        if let Some(request) = request {
            usage = usage.checked_add(
                self.retention
                    .array_addition(self.requests.len(), std::slice::from_ref(request))?,
                limits,
            )?;
        }
        if let Some(event) = event {
            usage = usage.checked_add(
                self.retention
                    .array_addition(self.events.len(), std::slice::from_ref(event))?,
                limits,
            )?;
        }
        if !std::ptr::eq(inventory, &self.inventory) {
            usage = usage.checked_add(
                self.retention.array_addition(
                    self.inventory.runtime.artifacts.len(),
                    inventory
                        .runtime
                        .artifacts
                        .get(self.inventory.runtime.artifacts.len()..)
                        .context("Runtime artifacts truncated")?,
                )?,
                limits,
            )?;
            usage = usage.checked_add(
                self.retention.array_addition(
                    self.inventory.runtime.classes.len(),
                    inventory
                        .runtime
                        .classes
                        .get(self.inventory.runtime.classes.len()..)
                        .context("Runtime classes truncated")?,
                )?,
                limits,
            )?;
            usage = usage.checked_add(
                self.retention.array_addition(
                    self.inventory.objects.len(),
                    inventory
                        .objects
                        .get(self.inventory.objects.len()..)
                        .context("Runtime objects truncated")?,
                )?,
                limits,
            )?;
            usage = usage.checked_add(
                self.retention.array_addition(
                    self.inventory.catalogues.len(),
                    inventory
                        .catalogues
                        .get(self.inventory.catalogues.len()..)
                        .context("Runtime catalogues truncated")?,
                )?,
                limits,
            )?;
            for (old, current) in self
                .inventory
                .runtime
                .loaders
                .iter()
                .zip(&inventory.runtime.loaders)
            {
                usage = usage.checked_add(
                    self.retention.array_addition(
                        old.artifacts.len(),
                        current
                            .artifacts
                            .get(old.artifacts.len()..)
                            .context("Runtime loader artifacts truncated")?,
                    )?,
                    limits,
                )?;
            }
            usage = usage.checked_add(
                self.retention.array_addition(
                    self.inventory.runtime.loaders.len(),
                    inventory
                        .runtime
                        .loaders
                        .get(self.inventory.runtime.loaders.len()..)
                        .context("Runtime loaders truncated")?,
                )?,
                limits,
            )?;
        }
        Ok(usage.checked_add(JsonUsage::default(), limits)?)
    }

    fn check_delta_retention(
        &mut self,
        delta: &DiscoveryDelta,
        event: &GetterEvent,
    ) -> Result<JsonUsage> {
        self.transport.check_progress()?;
        let transport = &mut self.transport;
        let monitor = &mut || transport.check_progress();
        let limits = self.retention.limits;
        let mut usage = self.retention.usage;
        usage = usage.checked_add(
            self.retention.array_addition_monitored(
                self.events.len(),
                std::slice::from_ref(event),
                monitor,
            )?,
            limits,
        )?;
        usage = usage.checked_add(
            self.retention.array_addition_monitored(
                self.inventory.runtime.artifacts.len(),
                &delta.artifacts,
                monitor,
            )?,
            limits,
        )?;
        usage = usage.checked_add(
            self.retention.array_addition_monitored(
                self.inventory.runtime.loaders.len(),
                &delta.loaders,
                monitor,
            )?,
            limits,
        )?;
        usage = usage.checked_add(
            self.retention.array_addition_monitored(
                self.inventory.runtime.classes.len(),
                &delta.classes,
                monitor,
            )?,
            limits,
        )?;
        usage = usage.checked_add(
            self.retention.array_addition_monitored(
                self.inventory.objects.len(),
                &delta.objects,
                monitor,
            )?,
            limits,
        )?;
        usage = usage.checked_add(
            self.retention.array_addition_monitored(
                self.inventory.catalogues.len(),
                &delta.catalogues,
                monitor,
            )?,
            limits,
        )?;
        for addition in &delta.loader_artifacts {
            usage = usage.checked_add(
                self.retention.array_addition_monitored(
                    addition.previous,
                    &addition.artifacts,
                    monitor,
                )?,
                limits,
            )?;
        }
        self.transport.check_progress()?;
        Ok(usage)
    }

    fn issue(&mut self, invocation: GetterInvocation<'_>) -> Result<GetterEvent> {
        ensure!(!self.failed, "Kotlin getter capture already failed");
        match self.issue_owned(invocation) {
            Ok(event) => Ok(event),
            Err(error) => self.reject(error),
        }
    }

    fn issue_owned(&mut self, invocation: GetterInvocation<'_>) -> Result<GetterEvent> {
        self.transport.check_progress()?;
        let GetterInvocation {
            project,
            receiver,
            name,
            descriptor,
            shape,
            arguments,
            purpose,
            after,
        } = invocation;
        let (owner, catalogue) = match receiver {
            GetterReceiver::Object(owner) => (
                Some(owner),
                self.index.object_catalogue(&self.inventory, owner)?,
            ),
            GetterReceiver::StaticCatalogue(id) => {
                (None, self.index.catalogue(&self.inventory, id)?)
            }
        };
        let method = select_exact_method(catalogue, name, descriptor, owner.is_none())?;
        let request = GetterRequest {
            id: format!(
                "{}:request:{}",
                self.binding.capture_id,
                self.requests.len()
            ),
            project: project.into(),
            consumer: Consumer::Kotlin,
            model_call: format!("{}:{project}:kotlin", self.binding.capture_id),
            parameter: RequestParameter::Absent(()),
            variant: None,
            owner: owner.map(str::to_owned),
            catalogue: catalogue.id.clone(),
            method,
            arguments,
            return_shape: shape,
            purpose,
            after: after.map(str::to_owned),
        };
        // Retain the request before handing it to any transport. Neither a result
        // nor a catalogue update can replace this issued request or its order.
        let usage = self.retention.usage.checked_add(
            self.retention.array_addition_monitored(
                self.requests.len(),
                std::slice::from_ref(&request),
                &mut || self.transport.check_progress(),
            )?,
            self.retention.limits,
        )?;
        self.requests.push(request.clone());
        self.retention.usage = usage;
        let (delta, event) = self.transport.invoke(&request)?;
        let examined = self.index.validate_delta(
            &self.inventory,
            &delta,
            &self.session,
            self.sequence,
            &request.id,
            &mut || self.transport.check_progress(),
        )?;
        #[cfg(test)]
        {
            self.discovery_rows_examined += examined;
        }
        #[cfg(not(test))]
        {
            let _examined = examined;
        }
        let usage = self.check_delta_retention(&delta, &event)?;
        self.index
            .apply_delta_monitored(&mut self.inventory, delta, &mut || {
                self.transport.check_progress()
            })?;
        self.sequence = self
            .sequence
            .checked_add(1)
            .context("Discovery sequence overflow")?;
        ensure!(
            event.request == request.id && self.event_ids.insert(event.id.clone()),
            "Getter event identity/order differs from issued request"
        );
        self.events.push(event.clone());
        self.retention.usage = usage;
        Ok(event)
    }

    fn capture_android_base_plugin(
        &mut self,
        project: &str,
        container: &str,
        plugins_request: &str,
    ) -> Result<String> {
        let event = self.issue(GetterInvocation {
            project,
            receiver: GetterReceiver::Object(container),
            name: "hasPlugin",
            descriptor: "(Ljava/lang/String;)Z",
            shape: scalar_shape(ValueKind::Boolean, false),
            arguments: vec![GetterArgument::String("com.android.base".into())],
            purpose: GetterPurpose::Raw,
            after: Some(plugins_request),
        })?;
        Ok(event.request)
    }

    pub fn capture_official_project(&mut self, project: &str) -> Result<OfficialProjectRequests> {
        ensure!(!self.failed, "Kotlin getter capture already failed");
        match self.capture_official_project_owned(project) {
            Ok(plan) => Ok(plan),
            Err(error) => self.reject(error),
        }
    }

    fn capture_official_project_owned(&mut self, project: &str) -> Result<OfficialProjectRequests> {
        self.transport.check_progress()?;
        let project_object = self
            .inventory
            .project_objects
            .get(project)
            .cloned()
            .context("Project object is missing from discovery")?;
        let mut plan = OfficialProjectRequests {
            project: project.into(),
            ..Default::default()
        };
        let plugins = self.issue(GetterInvocation {
            project,
            receiver: GetterReceiver::Object(&project_object),
            name: "getPlugins",
            descriptor: "()Lorg/gradle/api/plugins/PluginContainer;",
            shape: object_shape(ObjectKind::Container, false),
            arguments: vec![],
            purpose: GetterPurpose::Raw,
            after: None,
        })?;
        let iteration = self.issue(GetterInvocation {
            project,
            receiver: GetterReceiver::Object(&project_object),
            name: "getPlugins",
            descriptor: "()Lorg/gradle/api/plugins/PluginContainer;",
            shape: objects_shape(ObjectKind::Plugin, ContainerOrder::Iterable, false),
            arguments: vec![],
            purpose: GetterPurpose::Raw,
            after: None,
        })?;
        plan.plugin_iteration = Some(iteration.request.clone());
        if let Some(container) = object_result(&plugins) {
            for plugin in KOTLIN_PLUGIN_IDS.iter().chain(ANDROID_PLUGIN_IDS) {
                let event = self.issue(GetterInvocation {
                    project,
                    receiver: GetterReceiver::Object(&container),
                    name: "findPlugin",
                    descriptor: "(Ljava/lang/String;)Lorg/gradle/api/Plugin;",
                    shape: object_shape(ObjectKind::Plugin, true),
                    arguments: vec![GetterArgument::String((*plugin).into())],
                    purpose: GetterPurpose::Raw,
                    after: Some(&plugins.request),
                })?;
                plan.plugin_lookups.insert((*plugin).into(), event.request);
            }
            plan.android_base_plugin =
                Some(self.capture_android_base_plugin(project, &container, &plugins.request)?);
        } else {
            plan.unavailable
                .push("Project.getPlugins() unavailable".into());
        }
        let extensions = self.issue(GetterInvocation {
            project,
            receiver: GetterReceiver::Object(&project_object),
            name: "getExtensions",
            descriptor: "()Lorg/gradle/api/plugins/ExtensionContainer;",
            shape: object_shape(ObjectKind::Container, false),
            arguments: vec![],
            purpose: GetterPurpose::Raw,
            after: None,
        })?;
        if let Some(container) = object_result(&extensions) {
            let event = self.issue(GetterInvocation {
                project,
                receiver: GetterReceiver::Object(&container),
                name: "findByName",
                descriptor: "(Ljava/lang/String;)Ljava/lang/Object;",
                shape: object_shape(ObjectKind::Extension, true),
                arguments: vec![GetterArgument::String("kotlin".into())],
                purpose: GetterPurpose::Raw,
                after: Some(&extensions.request),
            })?;
            plan.extension_lookup = Some(event.request);
        } else {
            plan.unavailable
                .push("Project.getExtensions() unavailable".into());
        }
        if let Some(catalogue) = self
            .index
            .named_class_catalogue(&self.inventory, project, WRAPPER)?
            .map(|catalogue| catalogue.id.clone())
        {
            let event = self.issue(GetterInvocation {
                project,
                receiver: GetterReceiver::StaticCatalogue(&catalogue),
                name: "getKotlinPluginVersion",
                descriptor: "(Lorg/gradle/api/Project;)Ljava/lang/String;",
                shape: scalar_shape(ValueKind::String, true),
                arguments: vec![GetterArgument::Object(project_object.clone())],
                purpose: GetterPurpose::Raw,
                after: None,
            })?;
            plan.compiler_version = Some(event.request);
        } else {
            plan.unavailable
                .push(format!("Class lookup unavailable: {WRAPPER}"));
        }
        let task_map = self.issue(GetterInvocation {
            project,
            receiver: GetterReceiver::Object(&project_object),
            name: "getAllTasks",
            descriptor: "(Z)Ljava/util/Map;",
            shape: object_shape(ObjectKind::Container, false),
            arguments: vec![GetterArgument::Boolean(false)],
            purpose: GetterPurpose::Raw,
            after: None,
        })?;
        let Some(task_map_object) = object_result(&task_map) else {
            plan.unavailable
                .push("Project.getAllTasks(false) unavailable".into());
            return Ok(plan);
        };
        let tasks = self.issue(GetterInvocation {
            project,
            receiver: GetterReceiver::Object(&task_map_object),
            name: "get",
            descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;",
            shape: objects_shape(ObjectKind::Task, ContainerOrder::ProjectTaskMapValues, true),
            arguments: vec![GetterArgument::Object(project_object.clone())],
            purpose: GetterPurpose::ContainerIterate,
            after: Some(&task_map.request),
        })?;
        plan.task_iteration = Some(tasks.request.clone());
        let task_ids = match &tasks.outcome {
            GetterOutcome::Available(Some(CaptureValue::Objects(ids))) => ids.clone(),
            _ => {
                plan.unavailable
                    .push("Task-map project values unavailable".into());
                return Ok(plan);
            }
        };
        let resolver = if let Some(catalogue) = self
            .index
            .named_class_catalogue(&self.inventory, project, RESOLVER)?
            .map(|catalogue| catalogue.id.clone())
        {
            // The return descriptor comes from the exact reflected method, never
            // from a guessed plugin version or a synthetic resolver class.
            let method = self
                .inventory
                .catalogues
                .iter()
                .find(|value| value.id == catalogue)
                .and_then(|value| {
                    value.methods.iter().find(|method| {
                        method.name == "instance"
                            && method.is_static
                            && method.descriptor.starts_with("(Lorg/gradle/api/Project;)")
                    })
                })
                .cloned();
            if let Some(method) = method {
                Some(self.issue(GetterInvocation {
                    project,
                    receiver: GetterReceiver::StaticCatalogue(&catalogue),
                    name: &method.name,
                    descriptor: &method.descriptor,
                    shape: object_shape(ObjectKind::Resolver, true),
                    arguments: vec![GetterArgument::Object(project_object.clone())],
                    purpose: GetterPurpose::ResolverInstance,
                    after: None,
                })?)
            } else {
                plan.unavailable
                    .push("Compiler resolver instance method unavailable".into());
                None
            }
        } else {
            plan.unavailable
                .push(format!("Class lookup unavailable: {RESOLVER}"));
            None
        };
        for (position, task) in task_ids.into_iter().enumerate() {
            if position % 64 == 0 {
                self.transport.check_progress()?;
            }
            let object = self.index.object(&self.inventory, &task)?;
            let class = self
                .index
                .classes
                .get(&object.class_id)
                .and_then(|position| self.inventory.runtime.classes.get(*position))
                .context("Task runtime class is undiscovered")?;
            if !KOTLIN_TASK_CLASSES.contains(&class.name.as_str()) {
                continue;
            }
            let catalogue = self.index.object_catalogue(&self.inventory, &task)?;
            let source_method = catalogue
                .methods
                .iter()
                .find(|method| {
                    !method.is_static
                        && method.name.starts_with("getSourceSetName")
                        && method.descriptor.starts_with("()")
                })
                .cloned();
            if let Some(method) = source_method {
                let property = match method.return_class.as_deref() {
                    Some(class) => self.is_class(class, "org.gradle.api.provider.Property")?,
                    None => false,
                };
                let shape = if property {
                    object_shape(ObjectKind::Property, true)
                } else {
                    scalar_shape(ValueKind::String, true)
                };
                let source = self.issue(GetterInvocation {
                    project,
                    receiver: GetterReceiver::Object(&task),
                    name: &method.name,
                    descriptor: &method.descriptor,
                    shape,
                    arguments: vec![],
                    purpose: GetterPurpose::SourceSet,
                    after: None,
                })?;
                if property {
                    if let Some(owner) = object_result(&source) {
                        let terminal = self.issue(GetterInvocation {
                            project,
                            receiver: GetterReceiver::Object(&owner),
                            name: "get",
                            descriptor: "()Ljava/lang/Object;",
                            shape: scalar_shape(ValueKind::String, true),
                            arguments: vec![],
                            purpose: GetterPurpose::PropertyGet,
                            after: Some(&source.request),
                        })?;
                        plan.source_set_names.insert(task.clone(), terminal.request);
                    } else {
                        plan.source_set_names.insert(task.clone(), source.request);
                    }
                } else {
                    plan.source_set_names.insert(task.clone(), source.request);
                }
            } else {
                plan.unavailable
                    .push(format!("Source-set getter unavailable for {task}"));
            }
            if let Some(resolver_event) = &resolver
                && let Some(owner) = object_result(resolver_event)
            {
                let event = self.issue(GetterInvocation {
                    project,
                    receiver: GetterReceiver::Object(&owner),
                    name: "resolveCompilerArguments",
                    descriptor: "(Ljava/lang/Object;)Ljava/util/List;",
                    shape: strings_shape(true),
                    arguments: vec![GetterArgument::Object(task.clone())],
                    purpose: GetterPurpose::CompilerArguments,
                    after: Some(&resolver_event.request),
                })?;
                plan.compiler_arguments.insert(task, event.request);
            }
        }
        Ok(plan)
    }

    fn is_class(&mut self, class: &str, name: &str) -> Result<bool> {
        self.transport.check_progress()?;
        let key = (class.to_owned(), name.to_owned());
        if let Some(result) = self.ancestry_results.get(&key) {
            return Ok(*result);
        }
        let mut pending = vec![class.to_owned()];
        let mut seen = BTreeSet::new();
        let mut result = false;
        while let Some(id) = pending.pop() {
            self.charge_ancestry_work()?;
            if !seen.insert(id.clone()) {
                continue;
            }
            let position = *self
                .index
                .classes
                .get(&id)
                .context("Ancestry runtime class was not discovered")?;
            let row = self
                .inventory
                .runtime
                .classes
                .get(position)
                .context("Ancestry runtime class row missing")?;
            if row.name == name {
                result = true;
                break;
            }
            let superclass = row.superclass.is_some();
            let interfaces = row.interfaces.len();
            if superclass {
                self.charge_ancestry_work()?;
                pending.push(
                    self.inventory
                        .runtime
                        .classes
                        .get(position)
                        .and_then(|row| row.superclass.clone())
                        .context("Accepted superclass disappeared")?,
                );
            }
            for edge in 0..interfaces {
                self.charge_ancestry_work()?;
                pending.push(
                    self.inventory
                        .runtime
                        .classes
                        .get(position)
                        .and_then(|row| row.interfaces.get(edge))
                        .context("Accepted interface disappeared")?
                        .clone(),
                );
            }
        }
        self.ancestry_results.insert(key, result);
        Ok(result)
    }

    fn charge_ancestry_work(&mut self) -> Result<()> {
        self.ancestry_work = self
            .ancestry_work
            .checked_add(1)
            .filter(|work| *work <= self.retention.limits.ancestry_steps)
            .context("Kotlin getter cumulative ancestry work budget exceeded")?;
        if self.ancestry_work % 64 == 0 {
            self.transport.check_progress()?;
        }
        Ok(())
    }

    /// Compare the host's exact current ImportRevision with the originally
    /// retained token, including A→B→A context generation and import revision.
    pub fn finish(
        mut self,
        model: &ProjectModel,
        imports: &ImportFactsSnapshot,
        projects: Vec<OfficialProjectRequests>,
        mut current_host: impl FnMut() -> Result<H>,
    ) -> Result<CapturedKotlinGetters<H>> {
        let output = match (|| -> Result<String> {
            ensure!(!self.failed, "Kotlin getter capture already failed");
            ensure!(
                current_host()? == self.issued_host,
                "Host import revision changed during getter capture"
            );
            self.fixture
                .ensure_unchanged_monitored(&mut || self.transport.check_progress())?;
            self.inventory
                .verify_artifacts_monitored(&mut || self.transport.check_progress())?;
            self.transport.finish()?;
            ensure!(
                current_host()? == self.issued_host,
                "Host import revision changed during getter capture"
            );
            self.fixture
                .ensure_unchanged_monitored(&mut || self.transport.check_progress())?;
            self.inventory
                .verify_artifacts_monitored(&mut || self.transport.check_progress())?;
            let mut bytes = b"KODA_ANDROID_PROJECT_MODEL=".to_vec();
            bytes.extend_from_slice(FACTS_ENVELOPE);
            let mut writer =
                BoundedJsonWriter::new(bytes, self.retention.limits, FACTS_ENVELOPE.len() + 1);
            serde_json::to_writer(
                &mut writer,
                &self.borrowed_facts(&self.inventory, None, None),
            )
            .context("Serialize bounded Kotlin getter capture")?;
            let mut bytes = writer.into_inner();
            bytes.push(b'}');
            Ok(String::from_utf8(bytes)?)
        })() {
            Ok(output) => output,
            Err(error) => return self.reject(error),
        };
        let expected = CaptureContext {
            binding: self.binding,
            imports: self.imports,
            mode: CaptureMode::Invocation,
            runtime: self.inventory.runtime,
            objects: self.inventory.objects,
            catalogues: self.inventory.catalogues,
            requests: self.requests,
        };
        let snapshot =
            parse_kotlin_facts(&output, model, imports, &expected, self.retention.limits)
                .map_err(|error| anyhow::anyhow!("Strict Kotlin capture rejected: {error:?}"))?;
        Ok(CapturedKotlinGetters {
            issued_host: self.issued_host,
            snapshot,
            expected,
            projects,
            fixture: self.fixture,
        })
    }
}

pub fn select_exact_method(
    catalogue: &MethodCatalogue,
    name: &str,
    descriptor: &str,
    is_static: bool,
) -> Result<MethodSelection> {
    let methods = catalogue
        .methods
        .iter()
        .filter(|method| {
            method.name == name && method.descriptor == descriptor && method.is_static == is_static
        })
        .collect::<Vec<_>>();
    ensure!(
        methods.len() <= 1,
        "Ambiguous reflected getter; select a declaring method explicitly"
    );
    Ok(match methods.first() {
        Some(method) => MethodSelection::Selected(method.id.clone()),
        None => MethodSelection::Missing(MissingMethod {
            name: name.into(),
            descriptor: descriptor.into(),
            is_static,
        }),
    })
}

fn scalar_shape(kind: ValueKind, nullable: bool) -> ReturnShape {
    ReturnShape {
        kind,
        nullable,
        object_kind: None,
        order: None,
    }
}
fn object_shape(kind: ObjectKind, nullable: bool) -> ReturnShape {
    ReturnShape {
        kind: ValueKind::Object,
        nullable,
        object_kind: Some(kind),
        order: None,
    }
}
fn objects_shape(kind: ObjectKind, order: ContainerOrder, nullable: bool) -> ReturnShape {
    ReturnShape {
        kind: ValueKind::Objects,
        nullable,
        object_kind: Some(kind),
        order: Some(order),
    }
}
fn strings_shape(nullable: bool) -> ReturnShape {
    ReturnShape {
        kind: ValueKind::Strings,
        nullable,
        object_kind: None,
        order: Some(ContainerOrder::List),
    }
}
fn object_result(event: &GetterEvent) -> Option<String> {
    match &event.outcome {
        GetterOutcome::Available(Some(CaptureValue::Object(id))) => Some(id.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kotlin_import_facts::{GetterFailure, GetterFailureKind, GetterFailureStage};
    use std::io::Cursor;

    fn binding(digest: &str) -> CaptureBinding {
        CaptureBinding {
            capture_id: "synthetic-host-session".into(),
            source_epoch: "synthetic-host-epoch".into(),
            fixture_before_sha256: digest.into(),
            fixture_after_sha256: digest.into(),
            model_revision: 7,
            selection_revision: 3,
            selected_variants: vec![],
        }
    }

    fn inventory() -> Result<(TempDir, DiscoveryInventory, ImportIdentity)> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        let template = include_str!("../test_data/kotlin_import_facts/protocol-template.json")
            .replace(
                "$ROOT",
                root.to_str().context("Synthetic fixture path is UTF-8")?,
            );
        let packet: serde_json::Value = serde_json::from_str(&template)?;
        let mut context: CaptureContext = serde_json::from_value(packet["context"].clone())?;
        for artifact in &mut context.runtime.artifacts {
            fs::create_dir_all(
                artifact
                    .path
                    .parent()
                    .context("Synthetic artifact parent")?,
            )?;
            fs::write(
                &artifact.path,
                format!("synthetic artifact {}", artifact.id),
            )?;
            let (bytes, digest) = hash_regular_file(&artifact.path)?;
            artifact.bytes = bytes;
            artifact.sha256 = digest;
        }
        let project_objects = context
            .objects
            .iter()
            .filter(|object| object.kind == ObjectKind::Project)
            .map(|object| (object.project.clone(), object.id.clone()))
            .collect();
        Ok((
            directory,
            DiscoveryInventory {
                runtime: context.runtime,
                objects: context.objects,
                catalogues: context.catalogues,
                project_objects,
                class_lookups: vec![],
            },
            context.imports,
        ))
    }

    #[test]
    fn fixture_boundary_is_stable_for_inventory_order_and_detects_mutation() -> Result<()> {
        let directory = tempfile::tempdir()?;
        fs::write(
            directory.path().join("settings.gradle"),
            "rootProject.name='original'",
        )?;
        fs::write(directory.path().join("build.gradle"), "plugins {}")?;
        let first = FixtureBoundary::capture(
            directory.path(),
            &["settings.gradle".into(), "build.gradle".into()],
        )?;
        let reverse = FixtureBoundary::capture(
            directory.path(),
            &["build.gradle".into(), "settings.gradle".into()],
        )?;
        assert_eq!(first, reverse);
        first.ensure_unchanged()?;
        // Generated outputs do not change the explicit original fixture boundary.
        fs::write(directory.path().join("generated.log"), "output")?;
        first.ensure_unchanged()?;
        fs::write(
            directory.path().join("settings.gradle"),
            "rootProject.name='changed!'",
        )?;
        assert!(first.ensure_unchanged().is_err());
        fs::remove_file(directory.path().join("settings.gradle"))?;
        assert!(first.ensure_unchanged().is_err());
        Ok(())
    }

    #[test]
    fn fixture_boundary_rejects_empty_duplicate_and_escaping_paths() -> Result<()> {
        let directory = tempfile::tempdir()?;
        fs::write(directory.path().join("build.gradle"), "plugins {}")?;
        assert!(FixtureBoundary::capture(directory.path(), &[]).is_err());
        assert!(
            FixtureBoundary::capture(
                directory.path(),
                &["build.gradle".into(), "build.gradle".into()]
            )
            .is_err()
        );
        for path in [
            PathBuf::new(),
            PathBuf::from("../build.gradle"),
            PathBuf::from("/outside.gradle"),
        ] {
            assert!(FixtureBoundary::capture(directory.path(), &[path]).is_err());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn fixture_boundary_rejects_symlink_files_and_directory_ancestors() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        fs::write(outside.path().join("build.gradle"), "plugins {}")?;
        std::os::unix::fs::symlink(
            outside.path().join("build.gradle"),
            directory.path().join("file.gradle"),
        )?;
        std::os::unix::fs::symlink(outside.path(), directory.path().join("linked"))?;
        assert!(FixtureBoundary::capture(directory.path(), &["file.gradle".into()]).is_err());
        assert!(
            FixtureBoundary::capture(directory.path(), &["linked/build.gradle".into()]).is_err()
        );
        Ok(())
    }

    #[test]
    fn framed_transport_preserves_following_frame_and_rejects_limits_eof_and_cancel() -> Result<()>
    {
        let deadline = Instant::now() + Duration::from_secs(10);
        let cancelled = AtomicBool::new(false);
        let mut reader = BufReader::with_capacity(2, Cursor::new(b"first\nsecond\n"));
        assert_eq!(read_frame(&mut reader, deadline, &cancelled, 5)?, b"first");
        assert_eq!(read_frame(&mut reader, deadline, &cancelled, 6)?, b"second");
        assert!(read_frame(&mut Cursor::new(b"sixsix\n"), deadline, &cancelled, 5).is_err());
        assert!(read_frame(&mut Cursor::new(b"partial"), deadline, &cancelled, 20).is_err());
        cancelled.store(true, Ordering::Release);
        assert!(read_frame(&mut Cursor::new(b"ready\n"), deadline, &cancelled, 20).is_err());
        assert!(
            read_frame(
                &mut Cursor::new(b"ready\n"),
                Instant::now(),
                &AtomicBool::new(false),
                20
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn exact_method_plan_distinguishes_overloads_static_missing_and_ambiguity() -> Result<()> {
        let (_directory, inventory, _) = inventory()?;
        let mut catalogue = inventory
            .catalogues
            .first()
            .context("Synthetic catalogue")?
            .clone();
        let mut method = catalogue
            .methods
            .first()
            .context("Synthetic reflected method")?
            .clone();
        method.name = "getSourceSetName".into();
        method.descriptor = "()Ljava/lang/String;".into();
        method.is_static = false;
        catalogue.methods = vec![method.clone()];
        assert_eq!(
            select_exact_method(&catalogue, &method.name, &method.descriptor, false)?,
            MethodSelection::Selected(method.id.clone())
        );
        for (descriptor, static_call) in [
            ("(Ljava/lang/String;)Ljava/lang/String;", false),
            ("()Ljava/lang/String;", true),
        ] {
            assert!(matches!(
                select_exact_method(&catalogue, &method.name, descriptor, static_call)?,
                MethodSelection::Missing(_)
            ));
        }
        let mut duplicate = method.clone();
        duplicate.id = "different-declaring-method".into();
        catalogue.methods.push(duplicate);
        assert!(select_exact_method(&catalogue, &method.name, &method.descriptor, false).is_err());
        Ok(())
    }

    #[test]
    fn discovery_cannot_rewrite_prior_method_class_loader_or_locale() -> Result<()> {
        let (_directory, inventory, _) = inventory()?;
        let mut changed = inventory.clone();
        changed
            .catalogues
            .first_mut()
            .context("Catalogue")?
            .methods
            .clear();
        assert!(inventory.clone().extend(changed).is_err());
        let mut changed = inventory.clone();
        changed.runtime.classes.first_mut().context("Class")?.name = "counterfeit.Class".into();
        assert!(inventory.clone().extend(changed).is_err());
        let mut changed = inventory.clone();
        changed
            .runtime
            .loaders
            .first_mut()
            .context("Loader")?
            .parent = Some("counterfeit-loader".into());
        assert!(inventory.clone().extend(changed).is_err());
        let mut changed = inventory.clone();
        changed.runtime.locale.identifier = "en-US".into();
        assert!(inventory.clone().extend(changed).is_err());
        let mut changed = inventory.clone();
        changed.objects.clear();
        assert!(inventory.clone().extend(changed).is_err());
        let mut changed = inventory.clone();
        changed.runtime.artifacts.push(
            changed
                .runtime
                .artifacts
                .first()
                .context("Artifact")?
                .clone(),
        );
        assert!(inventory.clone().extend(changed).is_err());
        Ok(())
    }

    #[test]
    fn artifact_verification_uses_host_bytes_and_rejects_mutation() -> Result<()> {
        let (_directory, inventory, _) = inventory()?;
        inventory.verify_artifacts()?;
        let artifact = inventory.runtime.artifacts.first().context("Artifact")?;
        fs::write(&artifact.path, "counterfeit runtime")?;
        assert!(inventory.verify_artifacts().is_err());
        let mut self_authorized = inventory.clone();
        let (_, digest) = hash_regular_file(&artifact.path)?;
        self_authorized
            .runtime
            .artifacts
            .first_mut()
            .context("Artifact")?
            .sha256 = digest;
        // A follow-up discovery frame cannot bless changed bytes using its own digest.
        assert!(inventory.clone().extend(self_authorized).is_err());
        Ok(())
    }

    struct ScriptedTransport {
        inventory: DiscoveryInventory,
        wrong_request: bool,
        catalogue_changed: bool,
        observed: Vec<GetterRequest>,
        session: String,
    }
    impl GetterTransport for ScriptedTransport {
        fn check_progress(&mut self) -> Result<()> {
            Ok(())
        }
        fn discover(&mut self, _: &[String], session: &str) -> Result<DiscoveryInventory> {
            self.session = session.to_owned();
            Ok(self.inventory.clone())
        }
        fn invoke(&mut self, request: &GetterRequest) -> Result<(DiscoveryDelta, GetterEvent)> {
            self.observed.push(request.clone());
            let mut delta = DiscoveryDelta::empty(
                &self.inventory,
                &self.session,
                request,
                self.observed.len() as u64,
            );
            if self.catalogue_changed {
                delta.predecessor.catalogues = 0;
            }
            Ok((
                delta,
                GetterEvent {
                    id: format!("event:{}", self.observed.len()),
                    request: if self.wrong_request {
                        "counterfeit-request".into()
                    } else {
                        request.id.clone()
                    },
                    outcome: GetterOutcome::Unavailable(GetterFailure {
                        kind: GetterFailureKind::MissingMethod,
                        stage: GetterFailureStage::MethodDiscovery,
                        capability: request.id.clone(),
                        detail: "Synthetic missing capability".into(),
                        actual_class: None,
                        exceptions: vec![],
                    }),
                    container: None,
                },
            ))
        }
        fn finish(&mut self) -> Result<()> {
            Ok(())
        }
        fn abort(&mut self) -> Result<()> {
            Ok(())
        }
    }

    fn scripted_capture(
        wrong_request: bool,
        catalogue_changed: bool,
    ) -> Result<(TempDir, KotlinGetterCapture<ScriptedTransport, String>)> {
        let (directory, inventory, imports) = inventory()?;
        fs::write(directory.path().join("build.gradle"), "synthetic")?;
        let fixture = FixtureBoundary::capture(directory.path(), &["build.gradle".into()])?;
        let index = DiscoveryIndex::new(&inventory)?;
        let mut capture = KotlinGetterCapture {
            issued_host: "synthetic-exact-host-revision".to_string(),
            transport: ScriptedTransport {
                inventory: inventory.clone(),
                wrong_request,
                catalogue_changed,
                observed: vec![],
                session: "synthetic-session".into(),
            },
            binding: binding(&fixture.sha256),
            fixture,
            imports,
            inventory,
            requests: vec![],
            events: vec![],
            event_ids: BTreeSet::new(),
            retention: CaptureRetention {
                root: directory.path().canonicalize()?,
                modules: serde_json::value::RawValue::from_string("[]".into())?,
                limits: CaptureLimits::default(),
                usage: JsonUsage::default(),
                measured_bytes: std::cell::Cell::new(0),
            },
            index,
            session: "synthetic-session".into(),
            sequence: 0,
            ancestry_results: BTreeMap::new(),
            ancestry_work: 0,
            discovery_rows_examined: 0,
            failed: false,
        };
        refresh_synthetic_retention(&mut capture)?;
        Ok((directory, capture))
    }

    fn refresh_synthetic_retention<T: GetterTransport, H: Clone + PartialEq>(
        capture: &mut KotlinGetterCapture<T, H>,
    ) -> Result<()> {
        let mut writer = BoundedJsonWriter::new(
            std::io::sink(),
            capture.retention.limits,
            FACTS_ENVELOPE.len() + 1,
        );
        serde_json::to_writer(
            &mut writer,
            &capture.borrowed_facts(&capture.inventory, None, None),
        )?;
        capture.retention.usage = writer.usage();
        capture.index = DiscoveryIndex::new(&capture.inventory)?;
        Ok(())
    }

    #[test]
    fn immutable_issued_request_survives_bad_event_without_becoming_successful() -> Result<()> {
        for (wrong_request, catalogue_changed) in [(true, false), (false, true)] {
            let (_directory, mut capture) = scripted_capture(wrong_request, catalogue_changed)?;
            let catalogue = capture
                .inventory
                .catalogues
                .first()
                .context("Catalogue")?
                .id
                .clone();
            assert!(
                capture
                    .issue(GetterInvocation {
                        project: ":android",
                        receiver: GetterReceiver::StaticCatalogue(&catalogue),
                        name: "absentOfficialMethod",
                        descriptor: "()Ljava/lang/String;",
                        shape: scalar_shape(ValueKind::String, true),
                        arguments: vec![],
                        purpose: GetterPurpose::Raw,
                        after: None,
                    })
                    .is_err()
            );
            assert_eq!(capture.requests.len(), 1);
            assert!(capture.events.is_empty());
            assert_eq!(capture.requests, capture.transport.observed);
            assert!(matches!(
                capture.requests.first().context("Issued request")?.method,
                MethodSelection::Missing(_)
            ));
        }
        Ok(())
    }

    #[test]
    fn missing_capability_is_retained_as_an_event_and_not_an_empty_value() -> Result<()> {
        let (_directory, mut capture) = scripted_capture(false, false)?;
        let catalogue = capture
            .inventory
            .catalogues
            .first()
            .context("Catalogue")?
            .id
            .clone();
        let event = capture.issue(GetterInvocation {
            project: ":android",
            receiver: GetterReceiver::StaticCatalogue(&catalogue),
            name: "absentOfficialMethod",
            descriptor: "()Ljava/lang/String;",
            shape: scalar_shape(ValueKind::String, true),
            arguments: vec![],
            purpose: GetterPurpose::Raw,
            after: None,
        })?;
        assert!(matches!(event.outcome, GetterOutcome::Unavailable(_)));
        assert_eq!(capture.requests, capture.transport.observed);
        assert_eq!(capture.events.len(), 1);
        assert_eq!(
            capture.events.first().context("Getter event")?.request,
            capture.requests.first().context("Issued request")?.id
        );
        Ok(())
    }
    #[test]
    fn returning_to_a_with_a_new_host_generation_cannot_finish_old_capture() -> Result<()> {
        let (directory, capture) = scripted_capture(false, false)?;
        let root = directory.path().canonicalize()?;
        for name in ["android", "strange-parent", "shared-directory"] {
            fs::create_dir(root.join(name))?;
        }
        let wire = include_str!("../test_data/import_facts/wire-template.json")
            .replace("$ROOT", root.to_str().context("Synthetic root")?);
        let wire: serde_json::Value = serde_json::from_str(&wire)?;
        let output = format!(
            "KODA_ANDROID_PROJECT_MODEL={}",
            serde_json::to_string(&wire)?
        );
        let model = crate::project_model::parse_model(&output, &root)?;
        let imports = crate::import_facts::parse_import_facts(
            &output,
            &model,
            crate::import_facts::ImportFactsBinding {
                model_revision: 7,
                selection_revision: 3,
                selected_variants: vec![
                    crate::project_model::VariantId {
                        module: ":android".into(),
                        variant: "debug".into(),
                    },
                    crate::project_model::VariantId {
                        module: ":nested:library".into(),
                        variant: "jvm".into(),
                    },
                ],
            },
        )?;
        // Same A model/selection can return after B. A new host generation must
        // reject the old capture before any strict packet or publication exists.
        let error = capture
            .finish(&model, &imports, vec![], || {
                Ok("fresh-A-host-generation".into())
            })
            .err()
            .context("Old capture must not finish")?;
        assert!(error.to_string().contains("Host import revision changed"));
        Ok(())
    }
    #[test]
    fn diagnostic_budget_rejects_capture_instead_of_discarding_output() -> Result<()> {
        let directory = tempfile::tempdir()?;
        fs::write(directory.path().join("stdout.log"), b"12345")?;
        fs::write(directory.path().join("stderr.log"), b"67890")?;
        check_diagnostics(directory.path(), 10)?;
        assert!(check_diagnostics(directory.path(), 9).is_err());
        let error = read_frame_monitored(
            &mut Cursor::new(b"ready\n"),
            Instant::now() + Duration::from_secs(10),
            &AtomicBool::new(false),
            20,
            || check_diagnostics(directory.path(), 9),
        )
        .err()
        .context("Oversized runtime logs must stop capture")?;
        assert!(error.to_string().contains("diagnostic output exceeded"));
        assert_eq!(fs::read(directory.path().join("stdout.log"))?, b"12345");
        assert_eq!(fs::read(directory.path().join("stderr.log"))?, b"67890");
        Ok(())
    }

    fn missing_invocation(catalogue: &str) -> GetterInvocation<'_> {
        GetterInvocation {
            project: ":android",
            receiver: GetterReceiver::StaticCatalogue(catalogue),
            name: "absentOfficialMethod",
            descriptor: "()Ljava/lang/String;",
            shape: scalar_shape(ValueKind::String, true),
            arguments: vec![],
            purpose: GetterPurpose::Raw,
            after: None,
        }
    }

    fn retained_record_bytes<T: GetterTransport, H: Clone + PartialEq>(
        capture: &KotlinGetterCapture<T, H>,
    ) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&capture.borrowed_facts(
            &capture.inventory,
            None,
            None,
        ))?)
    }

    fn value_nodes(value: &serde_json::Value) -> usize {
        1 + match value {
            serde_json::Value::Array(values) => values.iter().map(value_nodes).sum(),
            serde_json::Value::Object(values) => values.values().map(value_nodes).sum(),
            _ => 0,
        }
    }

    #[test]
    fn individually_valid_events_exceed_aggregate_bytes_before_second_event_is_retained()
    -> Result<()> {
        let (_directory, mut capture) = scripted_capture(false, false)?;
        capture.binding.capture_id = "owned-session".repeat(100);
        refresh_synthetic_retention(&mut capture)?;
        let catalogue = capture
            .inventory
            .catalogues
            .first()
            .context("Catalogue")?
            .id
            .clone();
        let initial_bytes = retained_record_bytes(&capture)?.len();
        capture.issue(missing_invocation(&catalogue))?;
        let first_bytes = retained_record_bytes(&capture)?.len();
        let first_event = capture.events.first().context("First event")?.clone();
        assert!(serde_json::to_vec(&first_event)?.len() < MAX_FRAME_BYTES);
        capture.retention.limits.record_bytes =
            initial_bytes + 2 * (first_bytes - initial_bytes) + FACTS_ENVELOPE.len();
        let error = capture
            .issue(missing_invocation(&catalogue))
            .err()
            .context("Second event crosses aggregate bytes")?;
        assert!(format!("{error:#}").contains("aggregate byte budget"));
        assert_eq!(capture.transport.observed.len(), 2);
        assert_eq!(capture.requests.len(), 2);
        assert_eq!(capture.events.len(), 1);
        assert_eq!(
            capture.events.first().context("Retained event")?.id,
            first_event.id
        );
        Ok(())
    }

    #[test]
    fn individually_valid_events_exceed_aggregate_nodes_without_truncating_first_event()
    -> Result<()> {
        let (_directory, mut capture) = scripted_capture(false, false)?;
        let catalogue = capture
            .inventory
            .catalogues
            .first()
            .context("Catalogue")?
            .id
            .clone();
        let initial_nodes =
            value_nodes(&serde_json::from_slice(&retained_record_bytes(&capture)?)?);
        capture.issue(missing_invocation(&catalogue))?;
        let first_record = retained_record_bytes(&capture)?;
        let first_nodes = value_nodes(&serde_json::from_slice(&first_record)?);
        let first_event = capture.events.first().context("First event")?.clone();
        capture.retention.limits.entries = initial_nodes + 2 * (first_nodes - initial_nodes) - 1;
        assert!(
            value_nodes(&serde_json::to_value(&first_event)?) < capture.retention.limits.entries
        );
        let error = capture
            .issue(missing_invocation(&catalogue))
            .err()
            .context("Second event crosses aggregate nodes")?;
        assert!(format!("{error:#}").contains("aggregate entry budget"));
        assert_eq!(capture.transport.observed.len(), 2);
        assert_eq!(capture.requests.len(), 2);
        assert_eq!(capture.events, vec![first_event]);
        Ok(())
    }

    #[test]
    fn oversized_issued_request_is_rejected_before_transport_or_retention() -> Result<()> {
        let (_directory, mut capture) = scripted_capture(false, false)?;
        let catalogue = capture
            .inventory
            .catalogues
            .first()
            .context("Catalogue")?
            .id
            .clone();
        capture.retention.limits.record_bytes =
            retained_record_bytes(&capture)?.len() + FACTS_ENVELOPE.len() + 1;
        let mut invocation = missing_invocation(&catalogue);
        invocation.arguments = vec![GetterArgument::String("x".repeat(1024))];
        assert!(capture.issue(invocation).is_err());
        assert!(capture.requests.is_empty());
        assert!(capture.events.is_empty());
        assert!(capture.transport.observed.is_empty());
        Ok(())
    }

    #[test]
    fn retained_catalogue_validation_has_linear_identity_work_and_preserves_order() -> Result<()> {
        #[derive(Clone, PartialEq)]
        struct Row {
            id: String,
            value: usize,
        }
        for count in [100, 1_000, 10_000] {
            let before = (0..count)
                .map(|value| Row {
                    id: format!("owned:{value}"),
                    value,
                })
                .collect::<Vec<_>>();
            let mut after = before.clone();
            after.push(Row {
                id: "appended".into(),
                value: count,
            });
            let identities = std::cell::Cell::new(0);
            preserve_rows(&before, &after, |row| {
                identities.set(identities.get() + 1);
                &row.id
            })?;
            assert_eq!(identities.get(), before.len() + after.len());
            let mut rewritten = after.clone();
            rewritten.get_mut(count / 2).context("Middle row")?.value += 1;
            assert!(preserve_rows(&before, &rewritten, |row| &row.id).is_err());
            let mut missing = after.clone();
            missing.remove(count / 2);
            assert!(preserve_rows(&before, &missing, |row| &row.id).is_err());
            let mut duplicate = after.clone();
            duplicate.push(before.first().context("First row")?.clone());
            assert!(preserve_rows(&before, &duplicate, |row| &row.id).is_err());
            let mut reordered = after;
            reordered.swap(0, 1);
            assert!(preserve_rows(&before, &reordered, |row| &row.id).is_err());
        }
        Ok(())
    }

    struct TrackingTransport {
        scripted: ScriptedTransport,
        aborts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        closure_failure: bool,
        progress: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        fail_progress_at: Option<usize>,
    }

    impl GetterTransport for TrackingTransport {
        fn check_progress(&mut self) -> Result<()> {
            let progress = self.progress.fetch_add(1, Ordering::AcqRel) + 1;
            ensure!(
                self.fail_progress_at != Some(progress),
                "Injected capture health failure"
            );
            self.scripted.check_progress()
        }
        fn discover(&mut self, projects: &[String], session: &str) -> Result<DiscoveryInventory> {
            self.scripted.discover(projects, session)
        }
        fn invoke(&mut self, request: &GetterRequest) -> Result<(DiscoveryDelta, GetterEvent)> {
            self.scripted.invoke(request)
        }
        fn finish(&mut self) -> Result<()> {
            self.scripted.finish()
        }
        fn abort(&mut self) -> Result<()> {
            self.aborts.fetch_add(1, Ordering::AcqRel);
            ensure!(
                !self.closure_failure,
                "Injected owned runtime closure failure"
            );
            Ok(())
        }
    }

    fn tracking_capture(
        closure_failure: bool,
    ) -> Result<(TempDir, KotlinGetterCapture<TrackingTransport, String>)> {
        let (directory, capture) = scripted_capture(true, false)?;
        Ok((
            directory,
            KotlinGetterCapture {
                issued_host: capture.issued_host,
                transport: TrackingTransport {
                    scripted: capture.transport,
                    aborts: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                    closure_failure,
                    progress: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                    fail_progress_at: None,
                },
                binding: capture.binding,
                fixture: capture.fixture,
                imports: capture.imports,
                inventory: capture.inventory,
                requests: capture.requests,
                events: capture.events,
                event_ids: capture.event_ids,
                retention: capture.retention,
                index: capture.index,
                session: capture.session,
                sequence: capture.sequence,
                ancestry_results: capture.ancestry_results,
                ancestry_work: capture.ancestry_work,
                discovery_rows_examined: capture.discovery_rows_examined,
                failed: capture.failed,
            },
        ))
    }

    #[test]
    fn failed_event_closes_transport_and_retains_original_failure_if_closure_also_fails()
    -> Result<()> {
        for closure_failure in [false, true] {
            let (_directory, mut capture) = tracking_capture(closure_failure)?;
            let catalogue = capture
                .inventory
                .catalogues
                .first()
                .context("Catalogue")?
                .id
                .clone();
            let error = capture
                .issue(missing_invocation(&catalogue))
                .err()
                .context("Invalid event must fail capture")?;
            let detail = format!("{error:#}");
            assert!(detail.contains("Getter event identity/order differs"));
            assert_eq!(detail.contains("closure also failed"), closure_failure);
            assert_eq!(capture.transport.aborts.load(Ordering::Acquire), 1);
            assert_eq!(capture.requests.len(), 1);
            assert!(capture.events.is_empty());
        }
        Ok(())
    }

    #[test]
    fn aggregate_limit_failure_aborts_before_retaining_event_or_accepting_partial_success()
    -> Result<()> {
        let (_directory, mut capture) = tracking_capture(false)?;
        capture.transport.scripted.wrong_request = false;
        let catalogue = capture
            .inventory
            .catalogues
            .first()
            .context("Catalogue")?
            .id
            .clone();
        let initial_nodes =
            value_nodes(&serde_json::from_slice(&retained_record_bytes(&capture)?)?);
        capture.issue(missing_invocation(&catalogue))?;
        let first_nodes = value_nodes(&serde_json::from_slice(&retained_record_bytes(&capture)?)?);
        capture.retention.limits.entries = initial_nodes + 2 * (first_nodes - initial_nodes) - 1;
        assert!(capture.issue(missing_invocation(&catalogue)).is_err());
        assert_eq!(capture.transport.aborts.load(Ordering::Acquire), 1);
        assert_eq!(capture.transport.scripted.observed.len(), 2);
        assert_eq!(capture.requests.len(), 2);
        assert_eq!(capture.events.len(), 1);
        Ok(())
    }

    struct CompletionRace {
        logs: PathBuf,
        closed: bool,
        closure_failure: bool,
    }

    impl CaptureProcess for CompletionRace {
        fn terminal_status(&mut self) -> Result<Option<ExitStatus>> {
            ensure!(!self.closed, "Completion was already closed");
            fs::write(self.logs.join("stdout.log"), b"start-final-output")?;
            fs::write(self.logs.join("stderr.log"), b"final-warning")?;
            #[cfg(unix)]
            use std::os::unix::process::ExitStatusExt as _;
            #[cfg(windows)]
            use std::os::windows::process::ExitStatusExt as _;
            Ok(Some(ExitStatus::from_raw(0)))
        }
        fn close_owned(&mut self) -> Result<()> {
            ensure!(!self.closure_failure, "Injected completion closure failure");
            self.closed = true;
            Ok(())
        }
    }

    #[test]
    fn final_write_between_budget_check_and_exit_is_rejected_after_owned_closure() -> Result<()> {
        let directory = tempfile::tempdir()?;
        fs::write(directory.path().join("stdout.log"), b"start")?;
        fs::write(directory.path().join("stderr.log"), b"")?;
        check_diagnostics(directory.path(), 5)?;
        let mut process = CompletionRace {
            logs: directory.path().into(),
            closed: false,
            closure_failure: false,
        };
        let error = await_capture_completion(
            &mut process,
            Instant::now() + Duration::from_secs(10),
            &AtomicBool::new(false),
            directory.path(),
            5,
        )
        .err()
        .context("Final output must exceed budget")?;
        assert!(error.to_string().contains("diagnostic output exceeded"));
        assert!(process.closed);
        assert_eq!(
            fs::read(directory.path().join("stdout.log"))?,
            b"start-final-output"
        );
        assert_eq!(
            fs::read(directory.path().join("stderr.log"))?,
            b"final-warning"
        );
        Ok(())
    }

    #[test]
    fn successful_terminal_status_cannot_hide_owned_completion_closure_failure() -> Result<()> {
        let directory = tempfile::tempdir()?;
        fs::write(directory.path().join("stdout.log"), b"start")?;
        fs::write(directory.path().join("stderr.log"), b"")?;
        let mut process = CompletionRace {
            logs: directory.path().into(),
            closed: false,
            closure_failure: true,
        };
        let error = await_capture_completion(
            &mut process,
            Instant::now() + Duration::from_secs(10),
            &AtomicBool::new(false),
            directory.path(),
            100,
        )
        .err()
        .context("Closure failure must reject success")?;
        assert!(error.to_string().contains("completion closure failure"));
        assert!(!process.closed);
        assert_eq!(
            fs::read(directory.path().join("stdout.log"))?,
            b"start-final-output"
        );
        assert_eq!(
            fs::read(directory.path().join("stderr.log"))?,
            b"final-warning"
        );
        Ok(())
    }

    #[test]
    fn android_base_plugin_guard_has_its_own_exact_boolean_request_and_unavailable_result()
    -> Result<()> {
        let (_directory, mut capture) = scripted_capture(false, false)?;
        let container = capture
            .inventory
            .objects
            .iter()
            .find(|object| object.kind == ObjectKind::Container)
            .context("Synthetic container")?
            .clone();
        capture.inventory.catalogues.push(MethodCatalogue {
            id: "synthetic-plugin-container".into(),
            class_id: container.class_id,
            methods: vec![],
        });
        capture.transport.inventory = capture.inventory.clone();
        refresh_synthetic_retention(&mut capture)?;
        let request = capture.capture_android_base_plugin(
            ":android",
            &container.id,
            "synthetic-get-plugins-request",
        )?;
        let issued = capture
            .requests
            .first()
            .context("Issued base plugin request")?;
        assert_eq!(issued.id, request);
        assert_eq!(issued.owner.as_deref(), Some(container.id.as_str()));
        assert_eq!(
            issued.after.as_deref(),
            Some("synthetic-get-plugins-request")
        );
        assert_eq!(issued.purpose, GetterPurpose::Raw);
        assert_eq!(issued.return_shape, scalar_shape(ValueKind::Boolean, false));
        assert_eq!(
            issued.arguments,
            vec![GetterArgument::String("com.android.base".into())]
        );
        assert_eq!(
            issued.method,
            MethodSelection::Missing(MissingMethod {
                name: "hasPlugin".into(),
                descriptor: "(Ljava/lang/String;)Z".into(),
                is_static: false,
            })
        );
        assert_eq!(capture.requests, capture.transport.observed);
        assert!(matches!(
            capture.events.first().context("Base plugin event")?.outcome,
            GetterOutcome::Unavailable(_)
        ));
        let plan = OfficialProjectRequests {
            project: ":android".into(),
            android_base_plugin: Some(request.clone()),
            ..Default::default()
        };
        assert_eq!(serde_json::to_value(plan)?["androidBasePlugin"], request);
        Ok(())
    }

    #[test]
    fn project_plan_errors_abort_once_and_latch_failure_while_capture_is_retained() -> Result<()> {
        for ambiguous_lookup in [false, true] {
            for closure_failure in [false, true] {
                let (_directory, mut capture) = tracking_capture(closure_failure)?;
                capture.transport.scripted.wrong_request = false;
                if ambiguous_lookup {
                    capture.inventory.class_lookups.push(ClassLookup {
                        project: ":android".into(),
                        name: WRAPPER.into(),
                        classes: vec!["project".into(), "resolver".into()],
                        attempts: vec![],
                        failures: vec![],
                    });
                } else {
                    capture.inventory.project_objects.remove(":android");
                }
                capture.transport.scripted.inventory = capture.inventory.clone();
                refresh_synthetic_retention(&mut capture)?;
                let error = capture
                    .capture_official_project(":android")
                    .err()
                    .context("Invalid project plan must abort")?;
                let detail = format!("{error:#}");
                assert!(detail.contains(if ambiguous_lookup {
                    "Several plugin loaders"
                } else {
                    "Project object is missing"
                }));
                assert_eq!(detail.contains("closure also failed"), closure_failure);
                assert_eq!(capture.transport.aborts.load(Ordering::Acquire), 1);
                assert!(capture.failed);
                let observed = capture.transport.scripted.observed.len();
                assert_eq!(observed, if ambiguous_lookup { 3 } else { 0 });
                assert!(capture.capture_official_project(":android").is_err());
                assert_eq!(capture.transport.scripted.observed.len(), observed);
                assert_eq!(capture.transport.aborts.load(Ordering::Acquire), 1);
            }
        }
        Ok(())
    }

    struct PartialWriter<'a> {
        cancelled: &'a AtomicBool,
        writes: usize,
        cancel_on_write: bool,
        clock: Option<(&'a std::cell::Cell<Instant>, Instant)>,
        bytes: Vec<u8>,
        flushed: bool,
    }

    impl Write for PartialWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            let count = bytes.len().min(3);
            self.bytes.extend_from_slice(&bytes[..count]);
            if self.cancel_on_write {
                self.cancelled.store(true, Ordering::Release);
            }
            if let Some((clock, expired)) = self.clock {
                clock.set(expired);
            }
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.flushed = true;
            Ok(())
        }
    }

    #[test]
    fn partial_frame_writes_recheck_cancel_deadline_and_health_before_continuing() -> Result<()> {
        for (cancel, expire, health_failure) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            let cancelled = AtomicBool::new(false);
            let started = Instant::now();
            let deadline = started + Duration::from_secs(10);
            let clock = std::cell::Cell::new(started);
            let mut writer = PartialWriter {
                cancelled: &cancelled,
                writes: 0,
                cancel_on_write: cancel,
                clock: expire.then_some((&clock, deadline)),
                bytes: vec![],
                flushed: false,
            };
            let mut checks = 0;
            let error = write_frame_monitored_with_clock(
                &mut writer,
                b"a-whole-frame-that-cannot-be-accepted-after-failure",
                deadline,
                &cancelled,
                || {
                    checks += 1;
                    ensure!(!health_failure || checks != 2, "Injected guardian failure");
                    Ok(())
                },
                || clock.get(),
            )
            .err()
            .context("Partial write must stop on a capture-wide failure")?;
            let detail = format!("{error:#}");
            assert!(detail.contains(if cancel {
                "cancelled"
            } else if expire {
                "deadline"
            } else {
                "Injected guardian failure"
            }));
            assert_eq!(writer.writes, 1);
            assert_eq!(writer.bytes, b"a-w");
            assert!(!writer.flushed);
        }
        let cancelled = AtomicBool::new(false);
        let mut writer = PartialWriter {
            cancelled: &cancelled,
            writes: 0,
            cancel_on_write: false,
            clock: None,
            bytes: vec![],
            flushed: false,
        };
        write_frame_monitored(
            &mut writer,
            b"whole",
            Instant::now() + Duration::from_secs(10),
            &cancelled,
            || Ok(()),
        )?;
        assert_eq!(writer.bytes, b"whole");
        assert_eq!(writer.writes, 2);
        assert!(writer.flushed);
        Ok(())
    }

    #[test]
    fn hashing_checks_capture_health_between_chunks_and_rejects_growing_files() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("large-artifact");
        let bytes = vec![b'x'; 256 * 1024];
        fs::write(&path, &bytes)?;
        let mut checks = 0;
        let error = hash_regular_file_monitored(&path, &mut || {
            checks += 1;
            ensure!(checks != 3, "Injected hash cancellation");
            Ok(())
        })
        .err()
        .context("Hashing must observe cancellation during the first chunk")?;
        assert!(error.to_string().contains("Injected hash cancellation"));
        assert_eq!(checks, 3);
        assert_eq!(fs::read(&path)?, bytes);
        let mut checks = 0;
        let error = hash_regular_file_monitored(&path, &mut || {
            checks += 1;
            if checks == 2 {
                fs::OpenOptions::new()
                    .append(true)
                    .open(&path)?
                    .write_all(b"growth")?;
            }
            Ok(())
        })
        .err()
        .context("A growing file must not prolong hashing to a new EOF")?;
        assert!(error.to_string().contains("Artifact grew while hashing"));
        assert_eq!(fs::metadata(&path)?.len(), bytes.len() as u64 + 6);
        Ok(())
    }

    #[test]
    fn finish_health_failure_during_fixture_hashing_closes_runtime_before_returning() -> Result<()>
    {
        let (directory, mut capture) = tracking_capture(false)?;
        let root = directory.path().canonicalize()?;
        for name in ["android", "strange-parent", "shared-directory"] {
            fs::create_dir(root.join(name))?;
        }
        let fixture_bytes = vec![b'x'; 256 * 1024];
        fs::write(root.join("build.gradle"), &fixture_bytes)?;
        capture.fixture = FixtureBoundary::capture(&root, &["build.gradle".into()])?;
        capture.binding.fixture_before_sha256 = capture.fixture.sha256.clone();
        capture.binding.fixture_after_sha256 = capture.fixture.sha256.clone();
        refresh_synthetic_retention(&mut capture)?;
        let wire = include_str!("../test_data/import_facts/wire-template.json")
            .replace("$ROOT", root.to_str().context("Synthetic root")?);
        let wire: serde_json::Value = serde_json::from_str(&wire)?;
        let output = format!(
            "KODA_ANDROID_PROJECT_MODEL={}",
            serde_json::to_string(&wire)?
        );
        let model = crate::project_model::parse_model(&output, &root)?;
        let imports = crate::import_facts::parse_import_facts(
            &output,
            &model,
            crate::import_facts::ImportFactsBinding {
                model_revision: 7,
                selection_revision: 3,
                selected_variants: vec![
                    crate::project_model::VariantId {
                        module: ":android".into(),
                        variant: "debug".into(),
                    },
                    crate::project_model::VariantId {
                        module: ":nested:library".into(),
                        variant: "jvm".into(),
                    },
                ],
            },
        )?;
        capture.transport.fail_progress_at = Some(6);
        let progress = capture.transport.progress.clone();
        let aborts = capture.transport.aborts.clone();
        let issued_host = capture.issued_host.clone();
        let error = capture
            .finish(&model, &imports, vec![], || Ok(issued_host.clone()))
            .err()
            .context("Health failure while hashing must reject and close capture")?;
        assert!(
            error
                .to_string()
                .contains("Injected capture health failure")
        );
        assert_eq!(progress.load(Ordering::Acquire), 6);
        assert_eq!(aborts.load(Ordering::Acquire), 1);
        assert_eq!(fs::read(root.join("build.gradle"))?, fixture_bytes);
        Ok(())
    }

    #[test]
    fn incremental_retention_matches_full_json_and_measures_each_new_row_once() -> Result<()> {
        let (_directory, mut capture) = scripted_capture(false, false)?;
        let catalogue = capture
            .inventory
            .catalogues
            .first()
            .context("Catalogue")?
            .id
            .clone();
        let mut expected_measured_bytes = 0;
        for _ in 0..1000 {
            capture.issue(missing_invocation(&catalogue))?;
            expected_measured_bytes +=
                serde_json::to_vec(capture.requests.last().context("Last request")?)?.len();
            expected_measured_bytes +=
                serde_json::to_vec(capture.events.last().context("Last event")?)?.len();
            assert_eq!(
                capture.retention.measured_bytes.get(),
                expected_measured_bytes
            );
        }
        let bytes = retained_record_bytes(&capture)?;
        assert_eq!(
            capture.retention.usage.bytes,
            bytes.len() + FACTS_ENVELOPE.len() + 1
        );
        assert_eq!(
            capture.retention.usage.entries,
            value_nodes(&serde_json::from_slice(&bytes)?)
        );
        let mut next = capture.inventory.clone();
        let artifact = next
            .runtime
            .artifacts
            .last()
            .context("Artifact")?
            .id
            .clone();
        next.runtime
            .loaders
            .first_mut()
            .context("Loader")?
            .artifacts
            .push(artifact);
        let mut loader = next.runtime.loaders.last().context("Loader")?.clone();
        loader.id = "new-loader".into();
        next.runtime.loaders.push(loader);
        let mut object = next.objects.last().context("Object")?.clone();
        object.id = "new-object".into();
        next.objects.push(object);
        capture.inventory.validate_extension(&next)?;
        let usage = capture.check_retention(&next, None, None)?;
        let full = serde_json::to_vec(&capture.borrowed_facts(&next, None, None))?;
        assert_eq!(usage.bytes, full.len() + FACTS_ENVELOPE.len() + 1);
        assert_eq!(usage.entries, value_nodes(&serde_json::from_slice(&full)?));
        Ok(())
    }

    #[test]
    fn keyed_task_class_and_catalogue_indexes_preserve_identity_and_append_positions() -> Result<()>
    {
        for count in [100, 1000, 10000] {
            let (_directory, mut inventory, _) = inventory()?;
            let template = inventory
                .objects
                .iter()
                .find(|object| object.kind == ObjectKind::Task)
                .context("Task")?
                .clone();
            for position in 0..count {
                let mut task = template.clone();
                task.id = format!("ordered-task:{position}");
                inventory.objects.push(task);
            }
            let mut index = DiscoveryIndex::new(&inventory)?;
            for position in 0..count {
                let id = format!("ordered-task:{position}");
                let object = index.object(&inventory, &id)?;
                assert_eq!(object.id, id);
                assert_eq!(
                    index.object_catalogue(&inventory, &id)?.class_id,
                    object.class_id
                );
                assert_eq!(
                    inventory
                        .runtime
                        .classes
                        .get(*index.classes.get(&object.class_id).context("Class index")?)
                        .context("Class")?
                        .id,
                    object.class_id
                );
            }
            assert!(index.object(&inventory, "unknown-task").is_err());
            let old_objects = inventory.objects.len();
            let mut appended = template;
            appended.id = "appended-task".into();
            inventory.objects.push(appended);
            index.append(
                &inventory,
                old_objects,
                inventory.catalogues.len(),
                inventory.runtime.classes.len(),
            )?;
            assert_eq!(index.objects.len(), inventory.objects.len());
            assert_eq!(
                index.object(&inventory, "appended-task")?.id,
                "appended-task"
            );
            assert_eq!(
                index.objects.get("ordered-task:0"),
                Some(&(old_objects - count))
            );
        }
        Ok(())
    }
    fn wire_roundtrip(response: Response, bytes: &mut usize, rows: &mut usize) -> Result<Response> {
        let mut frame = serde_json::to_vec(&response)?;
        *bytes += frame.len();
        frame.push(b'\n');
        let mut reader = BufReader::new(std::io::Cursor::new(frame));
        let frame = read_frame_monitored(
            &mut reader,
            Instant::now() + Duration::from_secs(30),
            &AtomicBool::new(false),
            MAX_FRAME_BYTES,
            || Ok(()),
        )?;
        let response = decode_response_monitored(&frame, &mut || Ok(()))?;
        *rows += match &response {
            Response::Discovery(bootstrap) => {
                let inventory = &bootstrap.inventory;
                inventory.runtime.artifacts.len()
                    + inventory.runtime.loaders.len()
                    + inventory.runtime.classes.len()
                    + inventory.objects.len()
                    + inventory.catalogues.len()
                    + inventory.class_lookups.len()
            }
            Response::DiscoveryDelta(delta) => {
                delta.artifacts.len()
                    + delta.loaders.len()
                    + delta.classes.len()
                    + delta.objects.len()
                    + delta.catalogues.len()
                    + delta.loader_artifacts.len()
            }
            _ => 0,
        };
        Ok(response)
    }

    struct FramedPlanningTransport {
        inventory: DiscoveryInventory,
        session: String,
        sequence: u64,
        task_ids: Vec<String>,
        bootstrap_bytes: usize,
        delta_bytes: usize,
        parsed_rows: usize,
        observed: Vec<GetterRequest>,
        aborts: usize,
    }

    impl GetterTransport for FramedPlanningTransport {
        fn discover(&mut self, projects: &[String], session: &str) -> Result<DiscoveryInventory> {
            self.session = session.to_owned();
            let project = self
                .inventory
                .objects
                .iter()
                .find(|row| row.kind == ObjectKind::Project)
                .context("Project template")?
                .clone();
            for path in projects {
                if !self.inventory.project_objects.contains_key(path) {
                    let mut row = project.clone();
                    row.id = format!("project-for:{path}");
                    row.project = path.clone();
                    self.inventory
                        .project_objects
                        .insert(path.clone(), row.id.clone());
                    self.inventory.objects.push(row);
                }
            }
            match wire_roundtrip(
                Response::Discovery(DiscoveryBootstrap {
                    session: session.into(),
                    inventory: self.inventory.clone(),
                }),
                &mut self.bootstrap_bytes,
                &mut self.parsed_rows,
            )? {
                Response::Discovery(bootstrap) => Ok(bootstrap.inventory),
                _ => bail!("Controlled bootstrap changed frame kind"),
            }
        }
        fn invoke(&mut self, request: &GetterRequest) -> Result<(DiscoveryDelta, GetterEvent)> {
            self.sequence += 1;
            self.observed.push(request.clone());
            let delta =
                DiscoveryDelta::empty(&self.inventory, &self.session, request, self.sequence);
            let mut event = GetterEvent {
                id: format!("framed-event:{}", self.sequence),
                request: request.id.clone(),
                outcome: GetterOutcome::Unavailable(GetterFailure {
                    kind: GetterFailureKind::MissingMethod,
                    stage: GetterFailureStage::MethodDiscovery,
                    capability: request.id.clone(),
                    detail: "Controlled missing official capability".into(),
                    actual_class: None,
                    exceptions: vec![],
                }),
                container: None,
            };
            match request.purpose {
                GetterPurpose::ContainerIterate => {
                    event.outcome = GetterOutcome::Available(Some(CaptureValue::Objects(
                        self.task_ids.clone(),
                    )));
                    event.container = Some("framed-task-set".into());
                }
                GetterPurpose::SourceSet => {
                    event.outcome =
                        GetterOutcome::Available(Some(CaptureValue::String("main".into())));
                }
                _ => {
                    if matches!(&request.method, MethodSelection::Selected(method) if method == "framed-get-all-tasks")
                    {
                        event.outcome = GetterOutcome::Available(Some(CaptureValue::Object(
                            "framed-task-map".into(),
                        )));
                    }
                }
            }
            let delta = match wire_roundtrip(
                Response::DiscoveryDelta(delta),
                &mut self.delta_bytes,
                &mut self.parsed_rows,
            )? {
                Response::DiscoveryDelta(delta) => delta,
                _ => bail!("Controlled delta changed frame kind"),
            };
            let event = match wire_roundtrip(
                Response::Event(event),
                &mut self.delta_bytes,
                &mut self.parsed_rows,
            )? {
                Response::Event(event) => event,
                _ => bail!("Controlled event changed frame kind"),
            };
            Ok((delta, event))
        }
        fn check_progress(&mut self) -> Result<()> {
            Ok(())
        }
        fn finish(&mut self) -> Result<()> {
            Ok(())
        }
        fn abort(&mut self) -> Result<()> {
            self.aborts += 1;
            Ok(())
        }
    }

    fn framed_planning_capture(
        count: usize,
        applicable: bool,
    ) -> Result<(
        TempDir,
        ProjectModel,
        ImportFactsSnapshot,
        KotlinGetterCapture<FramedPlanningTransport, String>,
    )> {
        use crate::kotlin_import_facts::{ClassOrigin, GetterMethod, TaskIdentity};
        let (directory, mut inventory, _) = inventory()?;
        let root = directory.path().canonicalize()?;
        for name in ["android", "strange-parent", "shared-directory"] {
            fs::create_dir(root.join(name))?;
        }
        fs::write(
            root.join("build.gradle"),
            "controlled Rust transport fixture",
        )?;
        let wire = include_str!("../test_data/import_facts/wire-template.json")
            .replace("$ROOT", root.to_str().context("Fixture root")?);
        let wire: serde_json::Value = serde_json::from_str(&wire)?;
        let output = format!(
            "KODA_ANDROID_PROJECT_MODEL={}",
            serde_json::to_string(&wire)?
        );
        let model = crate::project_model::parse_model(&output, &root)?;
        let imports = crate::import_facts::parse_import_facts(
            &output,
            &model,
            crate::import_facts::ImportFactsBinding {
                model_revision: 7,
                selection_revision: 3,
                selected_variants: binding("unused")
                    .selected_variants
                    .into_iter()
                    .map(|variant| crate::project_model::VariantId {
                        module: variant.module,
                        variant: variant.variant,
                    })
                    .collect(),
            },
        )?;
        inventory
            .objects
            .retain(|row| row.kind == ObjectKind::Project);
        inventory
            .runtime
            .classes
            .iter_mut()
            .find(|row| row.id == "task")
            .context("Task class")?
            .name = if applicable {
            KOTLIN_TASK_CLASSES[0].into()
        } else {
            "controlled.NonKotlinTask".into()
        };
        for (id, name, interfaces) in [
            ("framed-map", "java.util.Map", vec![]),
            ("framed-set", "java.util.Set", vec!["iterable".into()]),
        ] {
            inventory.runtime.classes.push(RuntimeClass {
                id: id.into(),
                name: name.into(),
                loader: "bootstrap".into(),
                origin: ClassOrigin::Jdk("java.base".into()),
                superclass: None,
                interfaces,
            });
        }
        for (id, class_id) in [
            ("framed-task-map", "framed-map"),
            ("framed-task-set", "framed-set"),
        ] {
            inventory.objects.push(CaptureObject {
                id: id.into(),
                kind: ObjectKind::Container,
                project: ":android".into(),
                class_id: class_id.into(),
                task: None,
            });
        }
        inventory
            .catalogues
            .iter_mut()
            .find(|row| row.class_id == "project")
            .context("Project catalogue")?
            .methods
            .push(GetterMethod {
                id: "framed-get-all-tasks".into(),
                name: "getAllTasks".into(),
                descriptor: "(Z)Ljava/util/Map;".into(),
                declaring_class: "project".into(),
                is_static: false,
                parameter_classes: vec![None],
                return_class: Some("framed-map".into()),
            });
        inventory.catalogues.push(MethodCatalogue {
            id: "framed-maps".into(),
            class_id: "framed-map".into(),
            methods: vec![GetterMethod {
                id: "framed-map-get".into(),
                name: "get".into(),
                descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;".into(),
                declaring_class: "framed-map".into(),
                is_static: false,
                parameter_classes: vec![Some("object".into())],
                return_class: Some("object".into()),
            }],
        });
        inventory.class_lookups = [WRAPPER, RESOLVER]
            .into_iter()
            .map(|name| ClassLookup {
                project: ":android".into(),
                name: name.into(),
                classes: vec![],
                attempts: vec![],
                failures: vec![],
            })
            .collect();
        let task_ids = (0..count)
            .map(|position| format!("framed-task:{position}"))
            .collect::<Vec<_>>();
        for (position, id) in task_ids.iter().enumerate() {
            inventory.objects.push(CaptureObject {
                id: id.clone(),
                kind: ObjectKind::Task,
                project: ":android".into(),
                class_id: "task".into(),
                task: Some(TaskIdentity {
                    name: format!("compile{position}"),
                    path: format!(":android:compile{position}"),
                }),
            });
        }
        let fixture = FixtureBoundary::capture(&root, &["build.gradle".into()])?;
        let capture = KotlinGetterCapture::discover(
            FramedPlanningTransport {
                inventory,
                session: String::new(),
                sequence: 0,
                task_ids,
                bootstrap_bytes: 0,
                delta_bytes: 0,
                parsed_rows: 0,
                observed: vec![],
                aborts: 0,
            },
            "controlled-host-revision".to_owned(),
            || Ok("controlled-host-revision".into()),
            binding(&fixture.sha256),
            fixture,
            &model,
            &imports,
        )?;
        Ok((directory, model, imports, capture))
    }

    #[test]
    fn full_framed_task_planning_parses_each_discovery_row_once_and_passes_original_strict_decoder()
    -> Result<()> {
        let mut sizes = Vec::new();
        for (count, applicable) in [(100, true), (1000, true), (10000, false)] {
            let (_directory, model, imports, mut capture) =
                framed_planning_capture(count, applicable)?;
            let plan = capture.capture_official_project(":android")?;
            assert_eq!(
                plan.source_set_names.len(),
                if applicable { count } else { 0 }
            );
            assert!(plan.compiler_arguments.is_empty());
            assert_eq!(
                capture.transport.observed.len(),
                if applicable { count + 5 } else { 5 }
            );
            assert_eq!(
                capture.transport.parsed_rows,
                capture.inventory.runtime.artifacts.len()
                    + capture.inventory.runtime.loaders.len()
                    + capture.inventory.runtime.classes.len()
                    + capture.inventory.objects.len()
                    + capture.inventory.catalogues.len()
                    + capture.inventory.class_lookups.len()
            );
            assert_eq!(capture.discovery_rows_examined, 0);
            assert!(capture.retention.usage.entries <= CaptureLimits::default().entries);
            assert!(capture.retention.usage.bytes <= CaptureLimits::default().record_bytes);
            assert_eq!(capture.ancestry_work, if applicable { 3 } else { 0 });
            if applicable {
                sizes.push((
                    capture.transport.bootstrap_bytes,
                    capture.transport.delta_bytes,
                ));
            }
            let expected_requests = capture.requests.len();
            let completed = capture.finish(&model, &imports, vec![plan], || {
                Ok("controlled-host-revision".into())
            })?;
            assert_eq!(completed.expected.requests.len(), expected_requests);
            assert_eq!(completed.projects.len(), 1);
        }
        let first = sizes.first().context("100-task size")?;
        let second = sizes.get(1).context("1000-task size")?;
        assert!(second.0 < first.0 * 12);
        assert!(second.1 < first.1 * 12);
        Ok(())
    }

    #[test]
    fn deltas_reject_replay_foreign_session_rewrites_reorder_truncation_and_bad_loader_provenance()
    -> Result<()> {
        let (_directory, capture) = scripted_capture(false, false)?;
        let request = GetterRequest {
            id: "owned-delta-request".into(),
            project: ":android".into(),
            consumer: Consumer::Kotlin,
            model_call: "owned".into(),
            parameter: RequestParameter::Absent(()),
            variant: None,
            owner: None,
            catalogue: "tasks".into(),
            method: MethodSelection::Missing(MissingMethod {
                name: "absent".into(),
                descriptor: "()Ljava/lang/String;".into(),
                is_static: true,
            }),
            arguments: vec![],
            return_shape: scalar_shape(ValueKind::String, true),
            purpose: GetterPurpose::Raw,
            after: None,
        };
        let valid = DiscoveryDelta::empty(&capture.inventory, &capture.session, &request, 1);
        let verify = |delta: &DiscoveryDelta| {
            capture.index.validate_delta(
                &capture.inventory,
                delta,
                &capture.session,
                0,
                &request.id,
                &mut || Ok(()),
            )
        };
        assert_eq!(verify(&valid)?, 0);
        for change in 0..8 {
            let mut delta = valid.clone();
            match change {
                0 => delta.session = "foreign".into(),
                1 => delta.request = "foreign-request".into(),
                2 => delta.sequence = 0,
                3 => delta.predecessor.objects -= 1,
                4 => delta.classes.push(
                    capture
                        .inventory
                        .runtime
                        .classes
                        .first()
                        .context("Class")?
                        .clone(),
                ),
                5 => {
                    delta.objects = capture.inventory.objects.iter().rev().cloned().collect();
                }
                6 => delta.loader_artifacts.push(LoaderArtifactsAddition {
                    loader: "kgp".into(),
                    previous: 0,
                    artifacts: vec!["kgp-jar".into()],
                }),
                _ => {
                    let mut class = capture
                        .inventory
                        .runtime
                        .classes
                        .last()
                        .context("Class")?
                        .clone();
                    class.id = "new-class".into();
                    class.loader = "foreign-loader".into();
                    delta.classes.push(class);
                }
            }
            assert!(verify(&delta).is_err(), "Mutation {change} must fail");
        }
        assert_eq!(capture.sequence, 0);
        assert!(capture.requests.is_empty());
        Ok(())
    }

    #[test]
    fn append_only_loader_artifacts_and_objects_match_exact_retention_without_rechecking_the_prefix()
    -> Result<()> {
        let (_directory, mut capture) = scripted_capture(false, false)?;
        let catalogue = capture
            .inventory
            .catalogues
            .first()
            .context("Catalogue")?
            .id
            .clone();
        capture.issue(missing_invocation(&catalogue))?;
        let request = capture.requests.last().context("Issued request")?.clone();
        let mut delta = DiscoveryDelta::empty(&capture.inventory, &capture.session, &request, 2);
        let mut artifact = capture
            .inventory
            .runtime
            .artifacts
            .last()
            .context("Artifact")?
            .clone();
        artifact.id = "additional-runtime-artifact".into();
        delta.artifacts.push(artifact.clone());
        delta.loader_artifacts.push(LoaderArtifactsAddition {
            loader: "kgp".into(),
            previous: 1,
            artifacts: vec![artifact.id],
        });
        let mut object = capture.inventory.objects.last().context("Object")?.clone();
        object.id = "additional-object".into();
        delta.objects.push(object);
        let before = capture.inventory.clone();
        let examined = capture.index.validate_delta(
            &capture.inventory,
            &delta,
            &capture.session,
            1,
            &request.id,
            &mut || Ok(()),
        )?;
        assert!(examined <= 8);
        let event = GetterEvent {
            id: "additional-event".into(),
            request: request.id,
            outcome: GetterOutcome::Available(None),
            container: None,
        };
        let usage = capture.check_delta_retention(&delta, &event)?;
        capture.index.apply_delta(&mut capture.inventory, delta)?;
        capture.events.push(event);
        let bytes = retained_record_bytes(&capture)?;
        assert_eq!(usage.bytes, bytes.len() + FACTS_ENVELOPE.len() + 1);
        assert_eq!(usage.entries, value_nodes(&serde_json::from_slice(&bytes)?));
        assert_eq!(
            &capture.inventory.objects[..before.objects.len()],
            &before.objects
        );
        assert_eq!(capture.inventory.runtime.classes, before.runtime.classes);
        assert_eq!(capture.inventory.catalogues, before.catalogues);
        Ok(())
    }

    #[test]
    fn class_predicates_cache_both_results_by_exact_identity_and_share_checked_work_budget()
    -> Result<()> {
        let (_directory, mut capture) = scripted_capture(false, false)?;
        assert!(capture.is_class("property", "org.gradle.api.provider.Property")?);
        assert!(!capture.is_class("string", "org.gradle.api.provider.Property")?);
        let once = capture.ancestry_work;
        for _ in 0..1000 {
            assert!(capture.is_class("property", "org.gradle.api.provider.Property")?);
            assert!(!capture.is_class("string", "org.gradle.api.provider.Property")?);
        }
        assert_eq!(capture.ancestry_work, once);
        let mut other = capture
            .inventory
            .runtime
            .classes
            .iter()
            .find(|row| row.id == "property")
            .context("Property")?
            .clone();
        other.id = "same-name-different-loader".into();
        other.loader = "kgp".into();
        other.name = "controlled.NonProperty".into();
        capture.inventory.runtime.classes.push(other);
        refresh_synthetic_retention(&mut capture)?;
        assert!(!capture.is_class(
            "same-name-different-loader",
            "org.gradle.api.provider.Property"
        )?);
        assert_eq!(capture.ancestry_work, once + 1);
        capture.retention.limits.ancestry_steps = capture.ancestry_work;
        assert!(capture.is_class("task", "not-present").is_err());
        assert!(
            !capture
                .ancestry_results
                .contains_key(&("task".into(), "not-present".into()))
        );
        assert!(capture.events.is_empty());
        Ok(())
    }

    #[test]
    fn bounded_response_decoding_and_delta_validation_observe_cancellation_inside_large_frames()
    -> Result<()> {
        let (_directory, mut capture) = scripted_capture(false, false)?;
        let catalogue = capture
            .inventory
            .catalogues
            .first()
            .context("Catalogue")?
            .id
            .clone();
        capture.issue(missing_invocation(&catalogue))?;
        let request = capture.requests.last().context("Request")?;
        let mut delta = DiscoveryDelta::empty(&capture.inventory, &capture.session, request, 2);
        let template = capture.inventory.objects.last().context("Object")?;
        for position in 0..1000 {
            let mut row = template.clone();
            row.id = format!("cancel-object:{position}");
            delta.objects.push(row);
        }
        let bytes = serde_json::to_vec(&Response::DiscoveryDelta(delta.clone()))?;
        assert!(bytes.len() > 64 * 1024);
        let scanning_checks = bytes.len().div_ceil(64 * 1024);
        let mut checks = 0;
        let error = decode_response_monitored(&bytes, &mut || {
            checks += 1;
            ensure!(
                checks < scanning_checks + 2,
                "Injected parsing cancellation"
            );
            Ok(())
        })
        .err()
        .context("Decoding must cancel")?;
        assert!(format!("{error:#}").contains("Injected parsing cancellation"));
        assert!(format!("{error:#}").contains("Decode owned getter response frame"));
        assert_eq!(checks, scanning_checks + 2);
        let before = capture.inventory.clone();
        let mut checks = 0;
        let error = capture
            .index
            .validate_delta(
                &capture.inventory,
                &delta,
                &capture.session,
                1,
                &request.id,
                &mut || {
                    checks += 1;
                    ensure!(checks < 3, "Injected validation cancellation");
                    Ok(())
                },
            )
            .err()
            .context("Validation must cancel")?;
        assert!(format!("{error:#}").contains("Injected validation cancellation"));
        assert_eq!(capture.inventory, before);
        Ok(())
    }
    #[test]
    fn largest_legal_default_budget_planning_capture_passes_and_the_next_size_aborts_without_a_plan()
    -> Result<()> {
        let mut lower = 1000;
        let mut upper = 4000;
        while lower + 1 < upper {
            let count = lower + (upper - lower) / 2;
            let (_directory, model, imports, mut capture) = framed_planning_capture(count, true)?;
            match capture.capture_official_project(":android") {
                Ok(plan) => {
                    assert_eq!(plan.source_set_names.len(), count);
                    capture.finish(&model, &imports, vec![plan], || {
                        Ok("controlled-host-revision".into())
                    })?;
                    lower = count;
                }
                Err(error) => {
                    assert!(format!("{error:#}").contains("aggregate entry budget"));
                    assert!(capture.failed);
                    assert_eq!(capture.transport.aborts, 1);
                    assert!(capture.retention.usage.entries <= CaptureLimits::default().entries);
                    upper = count;
                }
            }
        }
        let (_directory, model, imports, mut capture) = framed_planning_capture(lower, true)?;
        let plan = capture.capture_official_project(":android")?;
        assert_eq!(plan.source_set_names.len(), lower);
        capture.finish(&model, &imports, vec![plan], || {
            Ok("controlled-host-revision".into())
        })?;
        let (_directory, _model, _imports, mut capture) = framed_planning_capture(upper, true)?;
        assert!(capture.capture_official_project(":android").is_err());
        assert!(capture.failed);
        assert_eq!(capture.transport.aborts, 1);
        let retained = capture.events.len();
        assert!(capture.capture_official_project(":android").is_err());
        assert_eq!(capture.events.len(), retained);
        assert_eq!(capture.transport.aborts, 1);
        Ok(())
    }

    #[test]
    fn cumulative_ancestry_exhaustion_aborts_the_full_project_plan_once_without_source_getters()
    -> Result<()> {
        let (_directory, _model, _imports, mut capture) = framed_planning_capture(100, true)?;
        capture.retention.limits.ancestry_steps = 1;
        let error = capture
            .capture_official_project(":android")
            .err()
            .context("Work budget must fail the plan")?;
        assert!(format!("{error:#}").contains("cumulative ancestry work budget"));
        assert!(capture.failed);
        assert_eq!(capture.transport.aborts, 1);
        assert!(
            capture
                .requests
                .iter()
                .all(|request| request.purpose != GetterPurpose::SourceSet)
        );
        assert!(capture.capture_official_project(":android").is_err());
        assert_eq!(capture.transport.aborts, 1);
        Ok(())
    }
    #[test]
    fn zero_frame_write_preserves_the_typed_io_failure_and_does_not_flush_or_accept_progress()
    -> Result<()> {
        #[derive(Default)]
        struct ZeroWriter {
            writes: usize,
            flushed: bool,
        }
        impl Write for ZeroWriter {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                self.writes += 1;
                Ok(0)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.flushed = true;
                Ok(())
            }
        }
        let mut writer = ZeroWriter::default();
        let error = write_frame_monitored(
            &mut writer,
            b"owned-frame\n",
            Instant::now() + Duration::from_secs(1),
            &AtomicBool::new(false),
            || Ok(()),
        )
        .err()
        .context("A zero write must fail")?;
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .context("Typed IO error")?
                .kind(),
            std::io::ErrorKind::WriteZero
        );
        assert_eq!(writer.writes, 1);
        assert!(!writer.flushed);
        Ok(())
    }

    #[test]
    fn ancestry_cache_separates_same_named_loader_classes_and_cancels_inside_shared_late_positive_graphs()
    -> Result<()> {
        let (_directory, mut capture) = tracking_capture(false)?;
        let template = capture
            .inventory
            .runtime
            .classes
            .iter()
            .find(|row| row.id == "property")
            .context("Class")?
            .clone();
        for position in 0..100 {
            let mut row = template.clone();
            row.id = format!("dag:{position}");
            row.name = format!("controlled.Dag{position}");
            row.interfaces = if position + 1 < 100 {
                vec![
                    format!("dag:{}", position + 1),
                    format!("dag:{}", position + 1),
                ]
            } else {
                vec![]
            };
            capture.inventory.runtime.classes.push(row);
        }
        for (id, loader, interfaces) in [
            (
                "positive-return",
                "gradle",
                vec!["property".into(), "dag:0".into()],
            ),
            ("negative-return", "kgp", vec![]),
        ] {
            let mut row = template.clone();
            row.id = id.into();
            row.name = "controlled.SameNamedReturn".into();
            row.loader = loader.into();
            row.interfaces = interfaces;
            capture.inventory.runtime.classes.push(row);
        }
        refresh_synthetic_retention(&mut capture)?;
        assert!(capture.is_class("positive-return", "org.gradle.api.provider.Property")?);
        assert!(!capture.is_class("negative-return", "org.gradle.api.provider.Property")?);
        let once = capture.ancestry_work;
        assert!(once > 100);
        for _ in 0..1000 {
            assert!(capture.is_class("positive-return", "org.gradle.api.provider.Property")?);
            assert!(!capture.is_class("negative-return", "org.gradle.api.provider.Property")?);
        }
        assert_eq!(capture.ancestry_work, once);
        let before = capture.transport.progress.load(Ordering::Acquire);
        capture.transport.fail_progress_at = Some(before + 2);
        let error = capture
            .is_class("positive-return", "controlled.AbsentTarget")
            .err()
            .context("Traversal must check health")?;
        assert!(format!("{error:#}").contains("Injected capture health failure"));
        assert!(capture.ancestry_work - once <= 64);
        assert!(
            !capture
                .ancestry_results
                .contains_key(&("positive-return".into(), "controlled.AbsentTarget".into()))
        );
        assert!(capture.events.is_empty());
        Ok(())
    }

    #[test]
    fn response_decoding_retains_one_shot_monitor_failure_without_retrying() -> Result<()> {
        let bytes = serde_json::to_vec(&Response::Hello("x".repeat(3 * 64 * 1024)))?;
        let scanning_checks = bytes.len().div_ceil(64 * 1024);
        for decoding_check in [1, 2] {
            let mut checks = 0;
            let fail_at = scanning_checks + decoding_check;
            let error = decode_response_monitored(&bytes, &mut || {
                checks += 1;
                if checks == fail_at {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "One-shot decoder health failure",
                    )
                    .into());
                }
                Ok(())
            })
            .err()
            .context("A one-shot monitoring failure must stop decoding")?;
            assert_eq!(checks, fail_at);
            assert!(format!("{error:#}").contains("One-shot decoder health failure"));
            assert!(format!("{error:#}").contains("Decode owned getter response frame"));
            assert_eq!(
                error
                    .downcast_ref::<std::io::Error>()
                    .context("Original typed monitor error")?
                    .kind(),
                std::io::ErrorKind::PermissionDenied
            );
        }
        Ok(())
    }
}
