use crate::project_view_preferences::ProjectViewCapabilities;
use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

pub const CONTEXT_OUTPUT_PREFIX: &str = "KODA_PROJECT_CONTEXT=";
pub const CONTEXT_SCHEMA: u32 = 1;
pub const MAX_CONTEXT_RECORD_BYTES: usize = 1024 * 1024;
const MAX_MODULES: usize = 4096;
const MAX_TARGETS: usize = 256;
const MAX_TEXT_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
pub enum PluginId {
    #[serde(rename = "com.android.application")]
    AndroidApplication,
    #[serde(rename = "com.android.library")]
    AndroidLibrary,
    #[serde(rename = "com.android.dynamic-feature")]
    AndroidDynamicFeature,
    #[serde(rename = "com.android.test")]
    AndroidTest,
    #[serde(rename = "com.android.kotlin.multiplatform.library")]
    AndroidMultiplatformLibrary,
    #[serde(rename = "org.jetbrains.kotlin.multiplatform")]
    KotlinMultiplatform,
    #[serde(rename = "org.jetbrains.compose")]
    ComposeMultiplatform,
    #[serde(rename = "org.jetbrains.kotlin.plugin.compose")]
    ComposeCompiler,
    #[serde(rename = "org.jetbrains.kotlin.android")]
    KotlinAndroid,
}

impl PluginId {
    pub const ALL: [Self; 9] = [
        Self::AndroidApplication,
        Self::AndroidLibrary,
        Self::AndroidDynamicFeature,
        Self::AndroidTest,
        Self::AndroidMultiplatformLibrary,
        Self::KotlinMultiplatform,
        Self::ComposeMultiplatform,
        Self::ComposeCompiler,
        Self::KotlinAndroid,
    ];

    fn is_android(self) -> bool {
        matches!(self, Self::AndroidApplication | Self::AndroidLibrary
            | Self::AndroidDynamicFeature | Self::AndroidTest | Self::AndroidMultiplatformLibrary)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ObservationPhase {
    Partial,
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TargetPlatform {
    AndroidJvm,
    Jvm,
    Js,
    Native,
    Wasm,
    Common,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct TargetFact {
    pub name: String,
    pub platform: TargetPlatform,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct GetterUnavailable {
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "status", content = "value", rename_all = "camelCase", deny_unknown_fields)]
pub enum TargetCatalogue {
    Available(Vec<TargetFact>),
    Unavailable(GetterUnavailable),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(remote = "Self", deny_unknown_fields)]
struct AppliedPlugin {
    plugin: PluginId,
    applied: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(remote = "Self", deny_unknown_fields)]
struct RawModule {
    path: String,
    directory: PathBuf,
    plugins: Vec<AppliedPlugin>,
    targets: TargetCatalogue,
}

#[derive(Deserialize, Serialize)]
#[serde(remote = "Self", rename_all = "camelCase", deny_unknown_fields)]
struct RawContext {
    schema: u32,
    root: PathBuf,
    gradle_version: String,
    phase: ObservationPhase,
    modules: Vec<RawModule>,
}

struct ObjectOnly<D>(D);

impl<'de, D: serde::Deserializer<'de>> serde::Deserializer<'de> for ObjectOnly<D> {
    type Error = D::Error;

    fn deserialize_any<V: serde::de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.0.deserialize_map(visitor)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes
        byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct map
        struct enum identifier ignored_any
    }
}

macro_rules! object_serde {
    ($($name:ident),+ $(,)?) => {$(
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                $name::deserialize(ObjectOnly(deserializer))
            }
        }
        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                $name::serialize(self, serializer)
            }
        }
    )+};
}

object_serde!(TargetFact, GetterUnavailable, AppliedPlugin, RawModule, RawContext);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ecosystems {
    pub android: bool,
    pub kotlin_multiplatform: bool,
    pub compose_multiplatform: bool,
}

impl Ecosystems {
    pub fn qualifies(self) -> bool {
        self.android || self.kotlin_multiplatform || self.compose_multiplatform
    }
}

#[derive(Clone, Debug)]
pub struct ModuleContext {
    path: String,
    directory: PathBuf,
    applied_plugins: BTreeSet<PluginId>,
    targets: TargetCatalogue,
}

impl ModuleContext {
    pub fn path(&self) -> &str { &self.path }
    pub fn directory(&self) -> &Path { &self.directory }
    pub fn applied(&self, plugin: PluginId) -> bool { self.applied_plugins.contains(&plugin) }
    pub fn targets(&self) -> &TargetCatalogue { &self.targets }

    fn android_target(&self) -> bool {
        matches!(&self.targets, TargetCatalogue::Available(targets)
            if targets.iter().any(|target| target.platform == TargetPlatform::AndroidJvm))
    }
}

#[derive(Clone, Debug)]
pub struct ContextSnapshot {
    root: PathBuf,
    gradle_version: String,
    phase: ObservationPhase,
    modules: BTreeMap<String, ModuleContext>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleOwner<'a> {
    Module(&'a str),
    OutsideRoot,
    Ambiguous,
    Unresolved,
}

/// Readiness comes from the existing current selected model and renderer, not filenames.
#[derive(Clone, Copy, Debug, Default)]
pub struct OperationalReadiness<'a> {
    pub application_module: Option<&'a str>,
    pub model_current: bool,
    pub model_root: Option<&'a Path>,
    pub android_renderer_supported: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContextCapabilities {
    pub ecosystems: Ecosystems,
    pub android_sync: bool,
    pub automatic_android_sync: bool,
    pub android_devices: bool,
    pub android_run: bool,
    pub android_compose_preview: bool,
}

impl ContextSnapshot {
    pub fn root(&self) -> &Path { &self.root }
    pub fn gradle_version(&self) -> &str { &self.gradle_version }
    pub fn phase(&self) -> ObservationPhase { self.phase }
    pub fn modules(&self) -> impl Iterator<Item = &ModuleContext> { self.modules.values() }

    pub fn ecosystems(&self) -> Ecosystems {
        let mut ecosystems = Ecosystems::default();
        for module in self.modules.values() {
            ecosystems.android |= module.applied_plugins.iter().any(|plugin| plugin.is_android());
            ecosystems.kotlin_multiplatform |= module.applied(PluginId::KotlinMultiplatform)
                || module.applied(PluginId::AndroidMultiplatformLibrary);
            ecosystems.compose_multiplatform |= module.applied(PluginId::ComposeMultiplatform);
        }
        ecosystems
    }

    pub fn module_owner(&self, path: &Path) -> ModuleOwner<'_> {
        if validate_path(path).is_err() { return ModuleOwner::Unresolved; }
        if !path.starts_with(&self.root) { return ModuleOwner::OutsideRoot; }
        let mut owner = None;
        let mut depth = 0;
        let mut ambiguous = false;
        for module in self.modules.values().filter(|module| path.starts_with(&module.directory)) {
            let module_depth = module.directory.components().count();
            if module_depth > depth {
                owner = Some(module.path.as_str());
                depth = module_depth;
                ambiguous = false;
            } else if module_depth == depth { ambiguous = true; }
        }
        if ambiguous { ModuleOwner::Ambiguous }
        else { owner.map_or(ModuleOwner::Unresolved, ModuleOwner::Module) }
    }

    pub fn capabilities(&self, owner_path: Option<&Path>, readiness: OperationalReadiness<'_>) -> ContextCapabilities {
        if owner_path.is_some_and(|path| validate_path(path).is_err() || !path.starts_with(&self.root)) {
            return ContextCapabilities::default();
        }
        let ecosystems = self.ecosystems();
        let complete = self.phase == ObservationPhase::Complete;
        let android_devices = complete && self.modules.values().any(ModuleContext::android_target);
        let model_current = readiness.model_current && readiness.model_root == Some(self.root.as_path());
        let application = readiness.application_module.and_then(|path| self.modules.get(path));
        let android_run = android_devices && model_current
            && application.is_some_and(|module| module.applied(PluginId::AndroidApplication) && module.android_target());
        let owner = owner_path.and_then(|path| match self.module_owner(path) {
            ModuleOwner::Module(module) => self.modules.get(module),
            _ => None,
        });
        // The current bundled renderer consumes a selected application model. This does not rule out future library preview support.
        let android_compose_preview = android_run && readiness.android_renderer_supported
            && owner.is_some_and(|module| module.android_target()
                && (module.applied(PluginId::ComposeMultiplatform) || module.applied(PluginId::ComposeCompiler)));
        ContextCapabilities { ecosystems, android_sync: ecosystems.android || android_devices,
            automatic_android_sync: android_devices, android_devices, android_run, android_compose_preview }
    }

    pub fn project_view_capabilities(&self, supports_android_view: bool) -> ProjectViewCapabilities {
        ProjectViewCapabilities { is_android_project: self.ecosystems().android
            || (self.phase == ObservationPhase::Complete && self.modules.values().any(ModuleContext::android_target)),
            supports_android_view }
    }

    fn preserves(&self, previous: &Self) -> bool {
        previous.modules.values().all(|previous_module| {
            self.modules.get(&previous_module.path).is_some_and(|module| {
                module.directory == previous_module.directory
                    && previous_module.applied_plugins.is_subset(&module.applied_plugins)
                    && match (&previous_module.targets, &module.targets) {
                        (TargetCatalogue::Available(previous), TargetCatalogue::Available(current)) => previous.iter().all(|target| current.contains(target)),
                        (TargetCatalogue::Available(previous), TargetCatalogue::Unavailable(_)) => previous.is_empty(),
                        (TargetCatalogue::Unavailable(_), _) => true,
                    }
            })
        })
    }
}

pub fn decode_context_record(record: &[u8], expected_root: &Path) -> Result<ContextSnapshot> {
    ensure!(record.len() <= MAX_CONTEXT_RECORD_BYTES, "Project context record exceeds the byte limit");
    let raw: RawContext = serde_json::from_slice(record).context("Decode evaluated project context")?;
    ensure!(raw.schema == CONTEXT_SCHEMA, "Unsupported project context schema {}", raw.schema);
    validate_path(&raw.root)?;
    ensure!(raw.root == expected_root, "Project context belongs to a different root");
    validate_text(&raw.gradle_version)?;
    ensure!(raw.modules.len() <= MAX_MODULES, "Project context has too many modules");
    let mut modules = BTreeMap::new();
    for module in raw.modules {
        ensure!(valid_module_path(&module.path), "Invalid evaluated module identity {}", module.path);
        validate_path(&module.directory)?;
        ensure!(module.path != ":" || module.directory == raw.root, "Root module directory differs from the evaluated root");
        ensure!(module.plugins.len() <= PluginId::ALL.len(), "Too many applied-plugin observations");
        let mut seen = BTreeSet::new();
        let mut applied_plugins = BTreeSet::new();
        for plugin in module.plugins {
            ensure!(seen.insert(plugin.plugin), "Duplicate applied-plugin observation");
            ensure!(raw.phase == ObservationPhase::Complete || plugin.applied, "Partial context cannot prove plugin absence");
            if plugin.applied { applied_plugins.insert(plugin.plugin); }
        }
        ensure!(raw.phase != ObservationPhase::Complete || seen.len() == PluginId::ALL.len(), "Complete context lacks applied-plugin observations");
        ensure!(applied_plugins.iter().filter(|plugin| plugin.is_android()).count() <= 1, "Conflicting Android plugin kinds");
        match &module.targets {
            TargetCatalogue::Available(targets) => {
                ensure!(targets.len() <= MAX_TARGETS, "Too many evaluated targets");
                let mut names = BTreeSet::new();
                for target in targets {
                    validate_text(&target.name)?;
                    ensure!(names.insert(&target.name), "Duplicate evaluated target identity");
                    ensure!(target.platform != TargetPlatform::AndroidJvm
                        || applied_plugins.iter().any(|plugin| plugin.is_android() || *plugin == PluginId::KotlinMultiplatform),
                        "Android target has no evaluated Android or multiplatform plugin");
                }
            }
            TargetCatalogue::Unavailable(unavailable) => validate_text(&unavailable.detail)?,
        }
        let path = module.path.clone();
        ensure!(modules.insert(path, ModuleContext { path: module.path, directory: module.directory, applied_plugins, targets: module.targets }).is_none(), "Duplicate evaluated module identity");
    }
    ensure!(raw.phase != ObservationPhase::Complete || modules.contains_key(":"), "Complete context lacks the root module");
    Ok(ContextSnapshot { root: raw.root, gradle_version: raw.gradle_version, phase: raw.phase, modules })
}

fn validate_text(text: &str) -> Result<()> {
    ensure!(!text.is_empty() && text.len() <= MAX_TEXT_BYTES && !text.contains('\0'), "Invalid or oversized evaluated text");
    Ok(())
}

fn validate_path(path: &Path) -> Result<()> {
    ensure!(path.is_absolute() && path.as_os_str().len() <= 32768
        && !path.as_os_str().as_encoded_bytes().contains(&0)
        && !path.components().any(|component| matches!(component, Component::ParentDir)),
        "Evaluated path must be an absolute path without parent traversal");
    Ok(())
}

fn valid_module_path(path: &str) -> bool {
    path.len() <= MAX_TEXT_BYTES && (path == ":" || (path.starts_with(':') && path.get(1..).is_some_and(|path| {
        path.split(':').all(|part| !part.is_empty() && !part.chars().any(|character| character.is_control() || matches!(character, '/' | '\\')))
    })))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RootHandle {
    worktree: u64,
    incarnation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootToken {
    root: RootHandle,
    generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryToken(RootToken);

struct RootContext {
    handle: RootHandle,
    path: PathBuf,
    trusted: bool,
    generation: u64,
    snapshot: Option<ContextSnapshot>,
    pending: Option<DiscoveryToken>,
}

#[derive(Default)]
pub struct ContextStore {
    next_incarnation: u64,
    roots: BTreeMap<u64, RootContext>,
}

impl ContextStore {
    pub fn add_root(&mut self, worktree: u64, path: PathBuf, trusted: bool) -> Result<RootHandle> {
        validate_path(&path)?;
        ensure!(!self.roots.contains_key(&worktree), "Worktree context is already registered");
        let incarnation = self.next_incarnation.checked_add(1).context("Root incarnation space exhausted")?;
        self.next_incarnation = incarnation;
        let handle = RootHandle { worktree, incarnation };
        self.roots.insert(worktree, RootContext { handle, path, trusted, generation: 0, snapshot: None, pending: None });
        Ok(handle)
    }

    fn entry(&self, handle: RootHandle) -> Option<&RootContext> {
        self.roots.get(&handle.worktree).filter(|entry| entry.handle == handle)
    }

    fn entry_mut(&mut self, handle: RootHandle) -> Result<&mut RootContext> {
        self.roots.get_mut(&handle.worktree).filter(|entry| entry.handle == handle).context("Project root context was removed")
    }

    pub fn remove_root(&mut self, handle: RootHandle) -> bool {
        if self.entry(handle).is_none() { return false; }
        self.roots.remove(&handle.worktree).is_some()
    }

    pub fn invalidate(&mut self, handle: RootHandle) -> Result<()> {
        let entry = self.entry_mut(handle)?;
        entry.generation = entry.generation.checked_add(1).context("Context generation space exhausted")?;
        entry.snapshot = None;
        entry.pending = None;
        Ok(())
    }

    pub fn set_trusted(&mut self, handle: RootHandle, trusted: bool) -> Result<()> {
        if self.entry_mut(handle)?.trusted != trusted {
            self.invalidate(handle)?;
            self.entry_mut(handle)?.trusted = trusted;
        }
        Ok(())
    }

    pub fn token(&self, handle: RootHandle) -> Option<RootToken> {
        let entry = self.entry(handle).filter(|entry| entry.trusted)?;
        Some(RootToken { root: handle, generation: entry.generation })
    }

    pub fn is_current(&self, token: &RootToken) -> bool { self.token(token.root).as_ref() == Some(token) }

    pub fn snapshot(&self, handle: RootHandle) -> Option<&ContextSnapshot> {
        self.entry(handle).filter(|entry| entry.trusted)?.snapshot.as_ref()
    }

    pub fn root_path(&self, handle: RootHandle) -> Option<&Path> {
        self.entry(handle).map(|entry| entry.path.as_path())
    }

    pub fn begin_import(&mut self, handle: RootHandle) -> Result<DiscoveryToken> {
        ensure!(self.entry(handle).is_some_and(|entry| entry.trusted), "Trust the project before evaluating Gradle context");
        self.invalidate(handle)?;
        let token = DiscoveryToken(self.token(handle).context("Project context has no trusted root")?);
        self.entry_mut(handle)?.pending = Some(token.clone());
        Ok(token)
    }

    pub fn publish(&mut self, token: &DiscoveryToken, snapshot: ContextSnapshot) -> Result<()> {
        ensure!(self.is_current(&token.0), "Discarded an outdated project context result");
        let entry = self.entry_mut(token.0.root)?;
        ensure!(entry.pending.as_ref() == Some(token), "Project context import already finished");
        ensure!(entry.path == snapshot.root, "Project context belongs to a different root");
        if let Some(previous) = &entry.snapshot {
            ensure!(previous.gradle_version == snapshot.gradle_version && snapshot.preserves(previous), "Project context contradicts earlier evaluated observations");
        }
        if snapshot.phase == ObservationPhase::Complete { entry.pending = None; }
        entry.snapshot = Some(snapshot);
        Ok(())
    }

    pub fn finish_failed_import(&mut self, token: &DiscoveryToken) -> Result<()> {
        ensure!(self.is_current(&token.0), "Discarded an outdated project context failure");
        let entry = self.entry_mut(token.0.root)?;
        ensure!(entry.pending.as_ref() == Some(token), "Project context import already finished");
        // Affirmative plugin facts retain an explicit repair Sync after SDK failure; the partial phase denies automatic work and device/run/preview tools.
        entry.pending = None;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveContextToken {
    generation: u64,
    root: RootToken,
    owner_path: Option<PathBuf>,
}

#[derive(Default)]
pub struct ActiveContext {
    generation: u64,
    root: Option<RootHandle>,
    owner_path: Option<PathBuf>,
}

impl ActiveContext {
    pub fn select(&mut self, root: Option<RootHandle>, owner_path: Option<PathBuf>) -> Result<()> {
        ensure!(root.is_some() || owner_path.is_none(), "A pane owner requires a project root");
        if let Some(path) = &owner_path { validate_path(path)?; }
        if self.root != root || self.owner_path != owner_path {
            self.generation = self.generation.checked_add(1).context("Active context generation space exhausted")?;
            self.root = root;
            self.owner_path = owner_path;
        }
        Ok(())
    }

    pub fn token(&self, store: &ContextStore) -> Option<ActiveContextToken> {
        store.snapshot(self.root?)?;
        self.discovery_token(store)
    }

    pub fn discovery_token(&self, store: &ContextStore) -> Option<ActiveContextToken> {
        let root = self.root?;
        if self.owner_path.as_ref().is_some_and(|path| store.root_path(root).is_none_or(|root| !path.starts_with(root))) {
            return None;
        }
        Some(ActiveContextToken { generation: self.generation, root: store.token(root)?, owner_path: self.owner_path.clone() })
    }

    pub fn is_current(&self, token: &ActiveContextToken, store: &ContextStore) -> bool {
        self.discovery_token(store).as_ref() == Some(token)
    }

    pub fn publish(&self, store: &mut ContextStore, active: &ActiveContextToken, discovery: &DiscoveryToken, snapshot: ContextSnapshot) -> Result<()> {
        ensure!(self.is_current(active, store) && active.root == discovery.0,
            "Discarded a project context result for a different active pane or root");
        store.publish(discovery, snapshot)
    }

    pub fn capabilities(&self, store: &ContextStore, readiness: OperationalReadiness<'_>) -> ContextCapabilities {
        self.root.and_then(|root| store.snapshot(root)).map_or_else(ContextCapabilities::default,
            |snapshot| snapshot.capabilities(self.owner_path.as_deref(), readiness))
    }
}
