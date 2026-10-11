use crate::{LocationLink, Project, ProjectPath};
use anyhow::Result;
use futures::StreamExt as _;
use gpui::{Context, Entity, Task};
use language::{Buffer, Location, PointUtf16, ToOffset};
use quick_xml::{Reader, events::Event};
use regex::Regex;
use std::{ops::Range, sync::LazyLock};
use util::ResultExt;

static NAMESPACE: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r#"\bnamespace\s*(?:=\s*|\(\s*)?["']([^"']+)["']"#));

impl Project {
    pub(crate) fn android_resource_model_unavailable(
        &self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<bool>>> {
        let root = self.android_model.root()?;
        if self.android_model.selected.is_some()
            || !self.is_local()
            || crate::trusted_worktrees::TrustedWorktrees::has_restricted_worktrees(
                &self.worktree_store(),
                cx,
            )
        {
            return None;
        }
        let snapshot = buffer.read(cx).snapshot();
        let file = snapshot.file()?;
        let path = self.android_file_path(file.as_ref(), cx);
        if !path.starts_with(root) {
            return None;
        }
        let xml = path.extension().is_some_and(|extension| extension == "xml");
        if !xml
            && path
                .extension()
                .is_none_or(|extension| extension != "kt" && extension != "java")
        {
            return None;
        }
        let offset = position.to_offset(&snapshot);
        let token = self.android_model.token();
        let buffer = buffer.clone();
        Some(cx.spawn(async move |project, cx| {
            if !source_request_current(&project, &token, &buffer, &snapshot, &path, cx).await? {
                return Ok(true);
            }
            let worker_snapshot = snapshot.clone();
            let worker_path = path.clone();
            let (sender, receiver) = async_channel::bounded(1);
            let worker = cx.background_executor().spawn(async move {
                let mut checkpoint = || resource_worker_checkpoint(&sender);
                let mut control = resources::ResourceParserControl::new(
                    &mut checkpoint,
                    MAX_RESOURCE_PARSER_WORK,
                );
                if parse_seed_source(
                    &worker_snapshot,
                    &worker_path,
                    offset,
                    xml,
                    "",
                    true,
                    &mut control,
                )?
                .is_some()
                {
                    return anyhow::Ok(true);
                }
                anyhow::Ok(
                    parse_seed_source(
                        &worker_snapshot,
                        &worker_path,
                        offset,
                        xml,
                        "",
                        false,
                        &mut control,
                    )?
                    .is_some(),
                )
            });
            while receiver.recv().await.is_ok() {
                if !source_request_current(&project, &token, &buffer, &snapshot, &path, cx).await? {
                    // A stale classification must not release an unverified
                    // resource position to the language server.
                    return Ok(true);
                }
            }
            let unavailable = worker.await?;
            if !source_request_current(&project, &token, &buffer, &snapshot, &path, cx).await? {
                return Ok(true);
            }
            Ok(unavailable)
        }))
    }

    pub fn android_model(&self) -> &android_tools::project_model::ModelState {
        &self.android_model
    }

    pub fn invalidate_android_model(
        &mut self,
        root: Option<std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) -> android_tools::project_model::ModelToken {
        let token = self.android_model.invalidate(root);
        cx.notify();
        token
    }

    pub fn publish_android_model(
        &mut self,
        token: &android_tools::project_model::ModelToken,
        model: android_tools::project_model::ProjectModel,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.android_model.publish(token, model)?;
        cx.notify();
        Ok(())
    }

    pub fn select_android_variant(
        &mut self,
        id: Option<android_tools::project_model::VariantId>,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let result = self.android_model.select(id);
        cx.notify();
        result
    }

    fn legacy_android_resource_definitions(
        &mut self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<Vec<LocationLink>>>> {
        if !self.is_local()
            || crate::trusted_worktrees::TrustedWorktrees::has_restricted_worktrees(
                &self.worktree_store(),
                cx,
            )
        {
            return None;
        }
        let snapshot = buffer.read(cx).snapshot();
        let file = snapshot.file()?;
        let relative_path = file.path().as_unix_str().to_owned();
        let source_path = self.android_file_path(file.as_ref(), cx);
        let xml = source_path
            .extension()
            .is_some_and(|extension| extension == "xml");
        if !xml
            && source_path
                .extension()
                .is_none_or(|extension| extension != "kt" && extension != "java")
        {
            return None;
        }
        // Before materializing source text, require a Gradle module and the
        // historical src/<source-set> layout used by this compatibility lookup.
        let module = relative_path
            .split_once("/src/")
            .map(|(module, _)| module)
            .or_else(|| relative_path.starts_with("src/").then_some(""))?;
        let prefix = if module.is_empty() {
            "src/".to_owned()
        } else {
            format!("{module}/src/")
        };
        let build_prefix = if module.is_empty() {
            String::new()
        } else {
            format!("{module}/")
        };
        let worktree_id = file.worktree_id(cx);
        let worktree = self.worktree_for_id(worktree_id, cx)?.read(cx).snapshot();
        let build = ["build.gradle.kts", "build.gradle"]
            .into_iter()
            .find_map(|name| {
                let path_text = format!("{build_prefix}{name}");
                let path = util::rel_path::RelPath::from_unix_str(&path_text).ok()?;
                worktree
                    .entry_for_path(path)
                    .map(|entry| entry.path.clone())
            })?;
        let offset = position.to_offset(&snapshot);
        let token = self.android_model.token();
        let source_buffer = buffer.clone();
        Some(cx.spawn(async move |project, cx| {
            let source_input = LegacySourceInput {
                buffer: source_buffer.clone(),
                snapshot: snapshot.clone(),
                path: source_path.clone(),
            };
            let mut namespace_input = None;
            let input_invalidated = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let mut input_observers = Vec::new();
            let worker_snapshot = snapshot.clone();
            let worker_path = source_path.clone();
            let lookup = source_cursor_worker(
                &project,
                &token,
                &source_input,
                None,
                input_invalidated.clone(),
                cx,
                move |control| {
                    let Some((reference, _, _)) = parse_seed_source(
                        &worker_snapshot,
                        &worker_path,
                        offset,
                        xml,
                        "",
                        false,
                        control,
                    )?
                    else {
                        return Ok(None);
                    };
                    if reference.namespace.as_deref() == Some("android") {
                        return Ok(None);
                    }
                    let mut paths = Vec::new();
                    let mut files = 0_usize;
                    let mut bytes = 0_usize;
                    for entry in worktree.files(false, 0) {
                        control.checkpoint()?;
                        files += 1;
                        ensure!(
                            files <= MAX_RESOURCE_FILES,
                            "Legacy Android lookup exceeds file limit"
                        );
                        let Some(relative) = entry.path.as_unix_str().strip_prefix(&prefix) else {
                            continue;
                        };
                        let Some((_, resource)) = relative.split_once("/res/") else {
                            continue;
                        };
                        let Some((directory, filename)) = resource.split_once('/') else {
                            continue;
                        };
                        if filename.contains('/') {
                            continue;
                        }
                        let folder_kind = directory.split('-').next().unwrap_or_default();
                        if (folder_kind == "values" && filename.ends_with(".xml"))
                            || (folder_kind == reference.kind
                                && filename.ends_with(".xml")
                                && filename.split('.').next() == Some(reference.name.as_str()))
                        {
                            bytes = bytes
                                .checked_add(entry.path.as_unix_str().len())
                                .context("Legacy Android lookup path size overflow")?;
                            ensure!(
                                bytes as u64 <= MAX_RESOURCE_BYTES,
                                "Legacy Android lookup exceeds path size limit"
                            );
                            paths.push((entry.path.clone(), folder_kind == "values"));
                        }
                    }
                    Ok(Some((reference, paths)))
                },
            )
            .await?;
            let Some(Some((reference, paths))) = lookup else {
                return Ok(Vec::new());
            };
            let origin = Location {
                buffer: source_buffer.clone(),
                range: snapshot.anchor_before(reference.range.start)
                    ..snapshot.anchor_after(reference.range.end),
            };
            if let Some(namespace) = reference.namespace.as_ref() {
                let build = project
                    .update(cx, |project, cx| {
                        project.open_buffer(
                            ProjectPath {
                                worktree_id,
                                path: build,
                            },
                            cx,
                        )
                    })?
                    .await?;
                let build_snapshot = cx.update(|cx| build.read(cx).snapshot());
                let build_path = cx.update(|cx| {
                    build_snapshot
                        .file()
                        .map(|file| file.full_path(cx))
                        .context("The Gradle namespace source has no file")
                })?;
                namespace_input = Some(LegacySourceInput {
                    buffer: build,
                    snapshot: build_snapshot.clone(),
                    path: build_path,
                });
                let namespace = namespace.clone();
                let matches = source_cursor_worker(
                    &project,
                    &token,
                    &source_input,
                    namespace_input.as_ref(),
                    input_invalidated.clone(),
                    cx,
                    move |control| {
                        ensure!(
                            build_snapshot.len() as u64 <= MAX_RESOURCE_BYTES,
                            "Legacy Android Gradle source exceeds text size limit"
                        );
                        control.checkpoint()?;
                        let text = build_snapshot.text();
                        let matches = NAMESPACE
                            .as_ref()
                            .ok()
                            .and_then(|expression| expression.captures(&text))
                            .and_then(|captures| {
                                captures.get(1).map(|value| value.as_str() == namespace)
                            })
                            .unwrap_or(false);
                        control.checkpoint()?;
                        Ok(matches)
                    },
                )
                .await?;
                if matches != Some(true) {
                    return Ok(Vec::new());
                }
            }
            let mut locations = Vec::new();
            let mut text_bytes = 0_usize;
            let mut targets = Vec::new();
            for (path, values) in paths {
                if !legacy_cursor_inputs_current(
                    &project,
                    &token,
                    &source_input,
                    namespace_input.as_ref(),
                    &input_invalidated,
                    cx,
                )
                .await?
                {
                    return Ok(Vec::new());
                }
                let buffer = project
                    .update(cx, |project, cx| {
                        project.open_buffer(
                            ProjectPath {
                                worktree_id,
                                path: path.clone(),
                            },
                            cx,
                        )
                    })?
                    .await?;
                let target_snapshot = cx.update(|cx| buffer.read(cx).snapshot());
                input_observers.push(observe_legacy_input(
                    &project,
                    &buffer,
                    &target_snapshot,
                    worktree_id,
                    path.clone(),
                    input_invalidated.clone(),
                    cx,
                )?);
                text_bytes = text_bytes
                    .checked_add(target_snapshot.len())
                    .context("Legacy resource text size overflow")?;
                ensure!(
                    text_bytes as u64 <= MAX_RESOURCE_BYTES,
                    "Legacy resource text exceeds size limit"
                );
                let worker_snapshot = target_snapshot.clone();
                let kind = reference.kind.clone();
                let name = reference.name.clone();
                let ranges = source_cursor_worker(
                    &project,
                    &token,
                    &source_input,
                    namespace_input.as_ref(),
                    input_invalidated.clone(),
                    cx,
                    move |control| {
                        control.checkpoint()?;
                        if values {
                            value_ranges_controlled(&worker_snapshot.text(), &kind, &name, control)
                        } else {
                            Ok(vec![0..0])
                        }
                    },
                )
                .await;
                let ranges = match ranges {
                    Ok(Some(ranges)) => ranges,
                    Ok(None) => return Ok(Vec::new()),
                    // Malformed resource XML remains an ignored compatibility
                    // candidate. Budget/cancellation failures must not publish
                    // a successful partial definition result.
                    Err(error)
                        if error.downcast_ref::<quick_xml::Error>().is_some()
                            || error
                                .downcast_ref::<quick_xml::events::attributes::AttrError>()
                                .is_some()
                            || error.downcast_ref::<LegacyXmlStructureError>().is_some() =>
                    {
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                for batch in ranges.chunks(128) {
                    resource_foreground_yield().await;
                    ensure!(
                        locations.len() + batch.len() <= MAX_RESOURCE_SYMBOLS,
                        "Legacy resource definitions exceed result limit"
                    );
                    for range in batch {
                        locations.push(LocationLink {
                            origin: Some(origin.clone()),
                            target: Location {
                                buffer: buffer.clone(),
                                range: target_snapshot.anchor_before(range.start)
                                    ..target_snapshot.anchor_after(range.end),
                            },
                        });
                    }
                }
                targets.push((buffer, target_snapshot, path));
            }
            for batch in targets.chunks(128) {
                resource_foreground_yield().await;
                if !legacy_cursor_inputs_current(
                    &project,
                    &token,
                    &source_input,
                    namespace_input.as_ref(),
                    &input_invalidated,
                    cx,
                )
                .await?
                {
                    return Ok(Vec::new());
                }
                if !project.read_with(cx, |project, cx| {
                    batch.iter().all(|(buffer, snapshot, path)| {
                        let buffer = buffer.read(cx);
                        buffer.version() == *snapshot.version()
                            && buffer.file().is_some_and(|file| {
                                !file.disk_state().is_deleted()
                                    && file.worktree_id(cx) == worktree_id
                                    && file.path() == path.as_ref()
                                    && project.worktree_for_id(worktree_id, cx).is_some()
                            })
                    })
                })? {
                    return Ok(Vec::new());
                }
                #[cfg(any(test, feature = "test-support"))]
                project.update(cx, |_, cx| {
                    *LEGACY_RESOURCE_VALIDATION_BATCH_COUNTS
                        .lock()
                        .expect("Legacy validation test counters")
                        .entry(source_path.clone())
                        .or_default() += 1;
                    // A deterministic GPUI test barrier after this actual batch.
                    cx.notify();
                })?;
            }
            if !legacy_cursor_inputs_current(
                &project,
                &token,
                &source_input,
                namespace_input.as_ref(),
                &input_invalidated,
                cx,
            )
            .await?
            {
                return Ok(Vec::new());
            }
            Ok(locations)
        }))
    }
}

#[cfg(test)]
fn value_ranges(text: &str, kind: &str, name: &str) -> Result<Vec<Range<usize>>> {
    let mut checkpoint = || Ok(());
    value_ranges_controlled(
        text,
        kind,
        name,
        &mut resources::ResourceParserControl::new(&mut checkpoint, usize::MAX),
    )
}

#[derive(Debug)]
struct LegacyXmlStructureError(&'static str);

impl std::fmt::Display for LegacyXmlStructureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for LegacyXmlStructureError {}

fn value_ranges_controlled(
    text: &str,
    kind: &str,
    name: &str,
    control: &mut resources::ResourceParserControl<'_>,
) -> Result<Vec<Range<usize>>> {
    control.charge_work(text.len())?;
    let mut reader = Reader::from_str(text);
    let mut ranges = Vec::new();
    let mut depth = 0;
    let mut resources = false;
    loop {
        control.checkpoint()?;
        control.charge_work(64)?;
        let event = reader.read_event()?;
        match event {
            Event::Start(ref element) | Event::Empty(ref element) => {
                if depth == 0 {
                    resources = element.name().as_ref() == b"resources";
                }
                if resources && depth == 1 {
                    let mut matches_name = false;
                    let mut matches_type = match element.name().as_ref() {
                        b"string-array" | b"integer-array" => kind == "array",
                        tag => tag == kind.as_bytes(),
                    };
                    for attribute in element.attributes() {
                        control.checkpoint()?;
                        let attribute = attribute?;
                        let value =
                            attribute.normalized_value(quick_xml::XmlVersion::Implicit1_0)?;
                        if attribute.key.as_ref() == b"name" {
                            matches_name = value.replace('.', "_") == name;
                        }
                        if element.name().as_ref() == b"item" && attribute.key.as_ref() == b"type" {
                            matches_type = value == kind;
                        }
                    }
                    if matches_name && matches_type {
                        let end = reader.buffer_position() as usize;
                        let length = element.len()
                            + if matches!(event, Event::Empty(_)) {
                                3
                            } else {
                                2
                            };
                        ensure!(
                            ranges.len() < MAX_RESOURCE_SYMBOLS,
                            "Legacy resource definitions exceed result limit"
                        );
                        ranges.push(end.saturating_sub(length)..end);
                    }
                }
                if matches!(event, Event::Start(_)) {
                    depth += 1;
                }
            }
            Event::End(_) => {
                if depth == 0 {
                    return Err(
                        LegacyXmlStructureError("Unexpected Android resource closing tag").into(),
                    );
                }
                depth -= 1;
            }
            Event::Eof => {
                if depth != 0 {
                    return Err(LegacyXmlStructureError("Unclosed Android resource XML").into());
                }
                break;
            }
            _ => {}
        }
    }
    Ok(ranges)
}

use crate::{
    Completion, CompletionResponse, CompletionSource, Hover, HoverBlock, HoverBlockKind,
    PrepareRenameResponse, ProjectTransaction,
};
use android_tools::{
    project_model::{ModelToken, SelectedProject, SourceKind, SourceScope},
    resources::{self, Declaration, Reference, Symbol},
};
use anyhow::{Context as _, ensure};
use gpui::{App, AsyncApp, WeakEntity};
use language::{BufferSnapshot, CodeLabel, DiskState};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};

const MAX_RESOURCE_FILES: usize = 20_000;
const MAX_RESOURCE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_RESOURCE_SYMBOLS: usize = 262_144;
const RESOURCE_SCAN_PROGRESS_FILES: usize = 32;
const MAX_RESOURCE_PARSER_WORK: usize = 1024 * 1024 * 1024;

#[cfg(any(test, feature = "test-support"))]
static RESOURCE_CONTEXT_PARSE_COUNTS: LazyLock<std::sync::Mutex<BTreeMap<PathBuf, usize>>> =
    LazyLock::new(|| std::sync::Mutex::new(BTreeMap::new()));

#[cfg(any(test, feature = "test-support"))]
static LEGACY_RESOURCE_VALIDATION_BATCH_COUNTS: LazyLock<
    std::sync::Mutex<BTreeMap<PathBuf, usize>>,
> = LazyLock::new(|| std::sync::Mutex::new(BTreeMap::new()));

// Capturing this seed only reads project metadata and a persistent buffer
// snapshot. No source text is materialized before Android/trust/type gating.
struct ResourceQuerySeed {
    selected: Arc<SelectedProject>,
    token: ModelToken,
    roots: Vec<ResourceRoot>,
    component_namespace: String,
    xml: bool,
    buffer: Entity<Buffer>,
    snapshot: BufferSnapshot,
    offset: usize,
    transitive: bool,
    path: PathBuf,
}

enum ResourceCursorResult {
    Resource(Box<ResourceQuery>),
    NotResource,
    Stale,
}

impl ResourceQuerySeed {
    fn finish(
        mut self,
        reference: Reference,
        namespace: String,
        explicit_namespace: bool,
    ) -> ResourceQuery {
        if reference.namespace.as_deref() != Some("android") {
            self.roots
                .retain(|root| root.namespace.as_ref() != "android");
        }
        let origin = Location {
            buffer: self.buffer,
            range: self.snapshot.anchor_before(reference.range.start)
                ..self.snapshot.anchor_after(reference.range.end),
        };
        ResourceQuery {
            selected: self.selected,
            token: self.token,
            roots: self.roots,
            namespace,
            component_namespace: self.component_namespace,
            xml: self.xml,
            reference,
            explicit_namespace,
            origin,
            source_snapshot: self.snapshot,
            transitive: self.transitive,
            source_path: self.path,
        }
    }
}

#[derive(Clone)]
struct ResourceRoot {
    path: PathBuf,
    namespace: Arc<str>,
    priority: usize,
    generated: bool,
    external: bool,
    kind: SourceKind,
    domain: Arc<str>,
}

struct ResourceDocument {
    buffer: Entity<Buffer>,
    snapshot: BufferSnapshot,
    path: PathBuf,
    namespace: Arc<str>,
    generated: bool,
    disk_text: Option<String>,
    disk_metadata: Option<(u64, fs::MTime, u64)>,
}

struct ResourceIndex {
    scan: Arc<ResourceScan>,
    documents: BTreeMap<PathBuf, ResourceDocument>,
    token: ModelToken,
    roots: Vec<ResourceRoot>,
    files: BTreeSet<PathBuf>,
    // Keep directory worktrees alive until lazy navigation opens its targets.
    _external_worktrees: Vec<Entity<worktree::Worktree>>,
}

struct ResourceQuery {
    selected: Arc<SelectedProject>,
    token: ModelToken,
    roots: Vec<ResourceRoot>,
    namespace: String,
    component_namespace: String,
    xml: bool,
    reference: Reference,
    explicit_namespace: bool,
    origin: Location,
    source_snapshot: BufferSnapshot,
    transitive: bool,
    source_path: PathBuf,
}

pub struct AndroidResourceRename {
    index: ResourceIndex,
    edits: BTreeMap<PathBuf, Vec<Range<usize>>>,
    new_name: String,
}

impl AndroidResourceRename {
    pub fn preview(&self) -> String {
        let mut result = String::new();
        for (path, ranges) in &self.edits {
            result.push_str(&format!("{} ({} edits)\n", path.display(), ranges.len()));
            if let Some(document) = self.index.documents.get(path) {
                let text = document.snapshot.text();
                let mut line = 1;
                let mut previous_offset = 0;
                for range in ranges {
                    let before_start = text[..range.start]
                        .char_indices()
                        .rev()
                        .nth(80)
                        .map_or(0, |(offset, _)| offset);
                    let after_end = text[range.end..]
                        .char_indices()
                        .nth(80)
                        .map_or(text.len(), |(offset, _)| range.end + offset);
                    let before = text[before_start..range.start]
                        .rsplit('\n')
                        .next()
                        .unwrap_or("");
                    let after = text[range.end..after_end].split('\n').next().unwrap_or("");
                    line += text[previous_offset..range.start]
                        .bytes()
                        .filter(|byte| *byte == b'\n')
                        .count();
                    previous_offset = range.start;
                    result.push_str(&format!(
                        "  {line}: {before}{}{} → {before}{}{}\n",
                        &text[range.clone()],
                        after,
                        self.new_name,
                        after
                    ));
                }
            }
        }
        result
    }
}

fn component_roots(
    selected: &SelectedProject,
    all_variants: bool,
    owner: &str,
    scope: SourceScope,
) -> Vec<ResourceRoot> {
    let visible = selected.visible_modules(owner, scope);
    let modules = if all_variants {
        selected.model.modules.iter().collect::<Vec<_>>()
    } else {
        selected
            .modules()
            .filter(|(module, _)| visible.contains(&module.path))
            .map(|(module, _)| module)
            .collect()
    };
    let mut roots = Vec::new();
    let mut namespaces: BTreeMap<&str, Arc<str>> = BTreeMap::new();
    for module in modules {
        for variant in &module.variants {
            if !all_variants && selected.variants.get(&module.path) != Some(&variant.name) {
                continue;
            }
            for component in &variant.components {
                if !all_variants
                    && component.scope != SourceScope::Main
                    && !(module.path == owner && component.scope == scope)
                {
                    continue;
                }
                let namespace = component
                    .namespace
                    .as_deref()
                    .or(module.namespace.as_deref())
                    .unwrap_or_default();
                let namespace = namespaces
                    .entry(namespace)
                    .or_insert_with(|| Arc::from(namespace))
                    .clone();
                let domain: Arc<str> = format!("{}/{}", module.path, component.name).into();
                let model = selected.model.resource_models.get(domain.as_ref());
                for source in &component.sources {
                    if !all_variants && source.kind != SourceKind::Resources {
                        continue;
                    }
                    let priority = model
                        .and_then(|model| {
                            model
                                .layers
                                .iter()
                                .position(|layer| layer.contains(&source.path))
                        })
                        .unwrap_or(usize::MAX / 4);
                    roots.push(ResourceRoot {
                        path: source.path.clone(),
                        namespace: namespace.clone(),
                        priority,
                        generated: source.generated,
                        external: false,
                        kind: source.kind,
                        domain: domain.clone(),
                    });
                }
                if !all_variants && let Some(model) = model {
                    for external in &model.dependencies {
                        roots.push(ResourceRoot {
                            path: external.path.clone(),
                            namespace: namespaces
                                .entry(external.namespace.as_str())
                                .or_insert_with(|| Arc::from(external.namespace.as_str()))
                                .clone(),
                            priority: usize::MAX / 2,
                            generated: true,
                            external: true,
                            kind: SourceKind::Resources,
                            domain: domain.clone(),
                        });
                    }
                    if let Some(framework) = &model.framework {
                        roots.push(ResourceRoot {
                            path: framework.clone(),
                            namespace: "android".into(),
                            priority: usize::MAX / 2,
                            generated: true,
                            external: true,
                            kind: SourceKind::Resources,
                            domain: domain.clone(),
                        });
                    }
                }
            }
        }
    }
    roots
}

impl Project {
    fn android_file_path(&self, file: &dyn language::File, cx: &App) -> PathBuf {
        if let Some(root) = self.android_model.root()
            && let Some(worktree) = self.worktree_for_id(file.worktree_id(cx), cx)
            && root == worktree.read(cx).abs_path().as_ref()
        {
            let root = self
                .android_model
                .selected
                .as_ref()
                .map_or(root, |selected| selected.model.root.as_path());
            return root.join(file.path().as_std_path());
        }
        file.full_path(cx)
    }

    fn android_query_seed(
        &self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        cx: &App,
    ) -> Option<ResourceQuerySeed> {
        if !self.is_local()
            || crate::trusted_worktrees::TrustedWorktrees::has_restricted_worktrees(
                &self.worktree_store(),
                cx,
            )
        {
            return None;
        }
        let selected = self.android_model.selected.clone()?;
        let snapshot = buffer.read(cx).snapshot();
        let file = snapshot.file()?;
        let path = self.android_file_path(file.as_ref(), cx);
        let xml = path.extension().is_some_and(|extension| extension == "xml");
        if !xml
            && path
                .extension()
                .is_none_or(|extension| extension != "kt" && extension != "java")
        {
            return None;
        }
        let (module, component) = selected.modules().find_map(|(module, variant)| {
            variant
                .components
                .iter()
                .find(|component| {
                    component
                        .sources
                        .iter()
                        .any(|source| path.starts_with(&source.path))
                })
                .map(|component| (module, component))
        })?;
        let component_namespace = component
            .namespace
            .as_deref()
            .or(module.namespace.as_deref())
            .unwrap_or_default()
            .to_owned();
        let transitive = selected
            .model
            .resource_models
            .get(&format!("{}/{}", module.path, component.name))
            .is_some_and(|model| !model.non_transitive_r);
        let roots = component_roots(&selected, false, &module.path, component.scope);
        Some(ResourceQuerySeed {
            offset: position.to_offset(&snapshot),
            selected,
            token: self.android_model.token(),
            roots,
            component_namespace,
            xml,
            buffer: buffer.clone(),
            snapshot,
            transitive,
            path,
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn android_resource_context_parse_count_for_test(&self, path: &std::path::Path) -> usize {
        RESOURCE_CONTEXT_PARSE_COUNTS
            .lock()
            .expect("Resource parser test counters")
            .get(path)
            .copied()
            .unwrap_or(0)
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn android_resource_legacy_validation_batch_count_for_test(
        &self,
        path: &std::path::Path,
    ) -> usize {
        LEGACY_RESOURCE_VALIDATION_BATCH_COUNTS
            .lock()
            .expect("Legacy validation test counters")
            .get(path)
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn android_resource_definitions(
        &mut self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<Option<Vec<LocationLink>>>>> {
        if self.android_model.root().is_none() {
            let legacy = self.legacy_android_resource_definitions(buffer, position, cx)?;
            let snapshot = buffer.read(cx).snapshot();
            let path = self.android_file_path(snapshot.file()?.as_ref(), cx);
            let token = self.android_model.token();
            let buffer = buffer.clone();
            return Some(cx.spawn(async move |project, cx| {
                let locations = legacy.await?;
                if !source_request_current(&project, &token, &buffer, &snapshot, &path, cx).await? {
                    return Ok(Some(Vec::new()));
                }
                Ok((!locations.is_empty()).then_some(locations))
            }));
        }
        let unavailable = self.android_resource_model_unavailable(buffer, position, cx);
        let seed = self.android_query_seed(buffer, position, cx);
        if unavailable.is_none() && seed.is_none() {
            return None;
        }
        Some(cx.spawn(async move |project, cx| {
            if let Some(unavailable) = unavailable
                && unavailable.await?
            {
                return Ok(Some(Vec::new()));
            }
            let Some(seed) = seed else {
                return Ok(None);
            };
            let query = match parse_query_seed(&project, seed, false, cx).await? {
                ResourceCursorResult::Resource(query) => *query,
                ResourceCursorResult::NotResource => return Ok(None),
                ResourceCursorResult::Stale => return Ok(Some(Vec::new())),
            };
            let mut index = build_index(
                &project,
                query.roots.clone(),
                query.token.clone(),
                false,
                cx,
            )
            .await?;
            let symbols = query_symbols_background(&project, &query, &index, cx).await?;
            let mut locations = Vec::new();
            for symbol in symbols {
                for declaration in resources::resolve(&index.scan.declarations, &symbol).winners {
                    if declaration
                        .path
                        .extension()
                        .is_none_or(|extension| extension != "xml")
                    {
                        continue;
                    }
                    let (buffer, snapshot) = if let Some(document) =
                        index.documents.get(&declaration.path)
                    {
                        (document.buffer.clone(), document.snapshot.clone())
                    } else {
                        if !resource_scan_current(&project, &query.token, cx)? {
                            return Ok(Some(Vec::new()));
                        }
                        let buffer = project
                            .update(cx, |project, cx| {
                                project.open_local_buffer(&declaration.path, cx)
                            })?
                            .await?;
                        let (snapshot, dirty) = cx.update(|cx| {
                            let buffer = buffer.read(cx);
                            (buffer.snapshot(), buffer.is_dirty())
                        });
                        let filesystem = project.read_with(cx, |project, _| project.fs.clone())?;
                        let path = declaration.path.clone();
                        let metadata =
                            cx.background_executor()
                                .spawn(async move {
                                    canonical_resource_metadata(&filesystem, &path).await
                                })
                                .await?;
                        if dirty
                            || index.scan.supporting_metadata.get(&declaration.path)
                                != Some(&metadata)
                            || declaration.range.end > snapshot.len()
                        {
                            return Ok(Some(Vec::new()));
                        }
                        index.documents.insert(
                            declaration.path.clone(),
                            ResourceDocument {
                                buffer: buffer.clone(),
                                snapshot: snapshot.clone(),
                                path: declaration.path.clone(),
                                namespace: declaration.symbol.namespace.clone(),
                                generated: declaration.generated,
                                disk_text: None,
                                disk_metadata: metadata,
                            },
                        );
                        (buffer, snapshot)
                    };
                    locations.push(LocationLink {
                        origin: Some(query.origin.clone()),
                        target: Location {
                            buffer,
                            range: snapshot.anchor_before(declaration.range.start)
                                ..snapshot.anchor_after(declaration.range.end),
                        },
                    });
                }
            }
            if !query_current(&project, &query, &index, cx).await? {
                return Ok(Some(Vec::new()));
            }
            // Binary resources participate in resolution but text definition links cannot display images.
            locations.retain(|location| {
                cx.update(|cx| {
                    location
                        .target
                        .buffer
                        .read(cx)
                        .file()
                        .is_some_and(|file| !file.disk_state().is_deleted())
                })
            });
            Ok(Some(locations))
        }))
    }

    pub fn android_resource_file_definitions(
        &self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<Vec<PathBuf>>>> {
        let seed = self.android_query_seed(buffer, position, cx)?;
        Some(cx.spawn(async move |project, cx| {
            let ResourceCursorResult::Resource(query) =
                parse_query_seed(&project, seed, false, cx).await?
            else {
                return Ok(Vec::new());
            };
            if !matches!(query.reference.kind.as_str(), "drawable" | "mipmap") {
                return Ok(Vec::new());
            }
            let index = build_index(
                &project,
                query.roots.clone(),
                query.token.clone(),
                false,
                cx,
            )
            .await?;
            let paths = query_symbols_background(&project, &query, &index, cx)
                .await?
                .iter()
                .flat_map(|symbol| resources::resolve(&index.scan.declarations, symbol).winners)
                .filter(|entry| {
                    entry.path.extension().is_some_and(|extension| {
                        ["png", "jpg", "jpeg", "webp", "gif"]
                            .iter()
                            .any(|allowed| extension == *allowed)
                    })
                })
                .map(|entry| entry.path.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            if !query_current(&project, &query, &index, cx).await? {
                return Ok(Vec::new());
            }
            Ok(paths)
        }))
    }

    pub(crate) fn android_resource_hover(
        &self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<Option<Vec<Hover>>>>> {
        let unavailable = self.android_resource_model_unavailable(buffer, position, cx);
        let seed = self.android_query_seed(buffer, position, cx);
        if unavailable.is_none() && seed.is_none() {
            return None;
        }
        Some(cx.spawn(async move |project, cx| {
            if let Some(unavailable) = unavailable
                && unavailable.await?
            {
                return Ok(Some(Vec::new()));
            }
            let Some(seed) = seed else {
                return Ok(None);
            };
            let query = match parse_query_seed(&project, seed, false, cx).await? {
                ResourceCursorResult::Resource(query) => *query,
                ResourceCursorResult::NotResource => return Ok(None),
                ResourceCursorResult::Stale => return Ok(Some(Vec::new())),
            };
            let index = build_index(
                &project,
                query.roots.clone(),
                query.token.clone(),
                false,
                cx,
            )
            .await?;
            let symbols = query_symbols_background(&project, &query, &index, cx).await?;
            let scan = index.scan.clone();
            let text = resource_worker(&project, &query, cx, move |control| {
                resource_documentation(&scan, &symbols, MAX_RESOURCE_BYTES as usize, control)
            })
            .await?;
            if text.is_empty() || !query_current(&project, &query, &index, cx).await? {
                return Ok(Some(Vec::new()));
            }
            Ok(Some(vec![Hover {
                contents: vec![HoverBlock {
                    text,
                    kind: HoverBlockKind::Code {
                        language: "text".into(),
                    },
                }],
                range: Some(query.origin.range),
                language: None,
            }]))
        }))
    }

    pub(crate) fn android_resource_references(
        &self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<Option<Vec<Location>>>>> {
        let unavailable = self.android_resource_model_unavailable(buffer, position, cx);
        let seed = self.android_query_seed(buffer, position, cx);
        if unavailable.is_none() && seed.is_none() {
            return None;
        }
        Some(cx.spawn(async move |project, cx| {
            if let Some(unavailable) = unavailable
                && unavailable.await?
            {
                return Ok(Some(Vec::new()));
            }
            let Some(seed) = seed else {
                return Ok(None);
            };
            let query = match parse_query_seed(&project, seed, false, cx).await? {
                ResourceCursorResult::Resource(query) => *query,
                ResourceCursorResult::NotResource => return Ok(None),
                ResourceCursorResult::Stale => return Ok(Some(Vec::new())),
            };
            let roots = component_roots(&query.selected, true, "", SourceScope::Main);
            let selected_index = build_index(
                &project,
                query.roots.clone(),
                query.token.clone(),
                false,
                cx,
            )
            .await?;
            let symbols = query_symbols_background(&project, &query, &selected_index, cx).await?;
            let index = build_index(&project, roots, query.token.clone(), true, cx).await?;
            let mut source_index = ResourceSourceIndex::capture(&index);
            let matches = resource_worker(&project, &query, cx, move |control| {
                source_index.index_namespaces(control)?;
                let mut matches = Vec::new();
                let mut result_bytes = 0_usize;
                for document in &source_index.documents {
                    control.checkpoint()?;
                    let text = document.snapshot.text();
                    let xml = document
                        .path
                        .extension()
                        .is_some_and(|extension| extension == "xml");
                    for reference in
                        resources::references_controlled(&text, xml, MAX_RESOURCE_SYMBOLS, control)?
                    {
                        control.checkpoint()?;
                        control.charge_work(symbols.len().saturating_mul(64))?;
                        let mut matching = false;
                        for symbol in &symbols {
                            if reference_matches(
                                document,
                                &reference,
                                &source_index,
                                symbol,
                                control,
                            )? {
                                matching = true;
                                break;
                            }
                        }
                        if matching {
                            ensure!(
                                matches.len() < MAX_RESOURCE_SYMBOLS,
                                "Android resource references exceed result limit"
                            );
                            result_bytes = result_bytes
                                .checked_add(document.path.as_os_str().len())
                                .and_then(|bytes| {
                                    bytes.checked_add(std::mem::size_of::<Range<usize>>())
                                })
                                .context("Android resource references exceed text size limit")?;
                            ensure!(
                                result_bytes as u64 <= MAX_RESOURCE_BYTES,
                                "Android resource references exceed text size limit"
                            );
                            matches.push((document.path.clone(), reference.name_range));
                        }
                    }
                }
                anyhow::Ok(matches)
            })
            .await?;
            if !query_current(&project, &query, &index, cx).await? {
                return Ok(Some(Vec::new()));
            }
            let mut locations = Vec::new();
            for (number, (path, range)) in matches.into_iter().enumerate() {
                // Parsing stays on the worker. Only GPUI entities and anchors are
                // assembled here, with a yield between bounded batches.
                if number % 128 == 0 {
                    resource_foreground_yield().await;
                    if !source_request_current(
                        &project,
                        &query.token,
                        &query.origin.buffer,
                        &query.source_snapshot,
                        &query.source_path,
                        cx,
                    )
                    .await?
                    {
                        return Ok(Some(Vec::new()));
                    }
                }
                let document = &index.documents[&path];
                locations.push(Location {
                    buffer: document.buffer.clone(),
                    range: document.snapshot.anchor_before(range.start)
                        ..document.snapshot.anchor_after(range.end),
                });
            }
            if !query_current(&project, &query, &index, cx).await? {
                return Ok(Some(Vec::new()));
            }
            Ok(Some(locations))
        }))
    }

    pub(crate) fn android_resource_completions(
        &self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<CompletionResponse>>> {
        let seed = self.android_query_seed(buffer, position, cx)?;
        Some(cx.spawn(async move |project, cx| {
            let ResourceCursorResult::Resource(query) =
                parse_query_seed(&project, seed, true, cx).await?
            else {
                return Ok(CompletionResponse {
                    completions: Vec::new(),
                    display_options: Default::default(),
                    is_incomplete: true,
                });
            };
            let index = build_index(
                &project,
                query.roots.clone(),
                query.token.clone(),
                false,
                cx,
            )
            .await?;
            let mut completions = Vec::new();
            if query_current(&project, &query, &index, cx).await? {
                let scan = index.scan.clone();
                let source_snapshot = query.source_snapshot.clone();
                let reference = query.reference.clone();
                let namespace = reference
                    .namespace
                    .as_deref()
                    .unwrap_or(&query.namespace)
                    .to_owned();
                let origin_namespace = query.component_namespace.clone();
                let query_explicit_namespace =
                    query.explicit_namespace && namespace != query.component_namespace;
                let xml = query.xml;
                // Transitive R contributes dependency members only to the owning
                // component's R class. Imported or qualified foreign R stays scoped.
                let dependency_fallback = if xml {
                    reference.namespace.is_none()
                } else {
                    query.transitive && namespace == query.component_namespace
                };
                let (completion_progress, progress_rx) = async_channel::bounded(1);
                let worker = cx.background_executor().spawn(async move {
                    let local_names = scan
                        .symbol_declarations
                        .keys()
                        .filter(|symbol| {
                            symbol.namespace == namespace && symbol.kind == reference.kind
                        })
                        .map(|symbol| symbol.name.as_str())
                        .collect::<BTreeSet<_>>();
                    let mut symbols = BTreeMap::<String, Vec<Symbol>>::new();
                    let mut completion_bytes = 0_u64;
                    for (number, symbol) in scan.symbol_declarations.keys().enumerate() {
                        if number % 128 == 0 {
                            completion_progress.send(()).await.map_err(|_| {
                                anyhow::anyhow!("Android resource completion cancelled")
                            })?;
                        }
                        ensure!(
                            !completion_progress.is_closed(),
                            "Android resource completion cancelled"
                        );
                        if symbol.kind != reference.kind
                            || !(symbol.namespace == namespace
                                || dependency_fallback
                                    && symbol.namespace != "android"
                                    && !local_names.contains(symbol.name.as_str()))
                        {
                            continue;
                        }
                        if !query_explicit_namespace
                            && symbol.namespace != origin_namespace
                            && scan
                                .public_symbols
                                .get(&symbol.namespace)
                                .is_some_and(|public| {
                                    !public.contains(&(symbol.kind.clone(), symbol.name.clone()))
                                })
                        {
                            continue;
                        }
                        if !symbol.name.starts_with(&reference.name) {
                            continue;
                        }
                        completion_bytes = completion_bytes
                            .saturating_add(symbol.namespace.len() as u64)
                            .saturating_add(symbol.kind.len() as u64)
                            .saturating_add(2 * symbol.name.len() as u64);
                        ensure!(
                            completion_bytes <= MAX_RESOURCE_BYTES,
                            "Android resource completion exceeds text size limit"
                        );
                        symbols
                            .entry(symbol.name.clone())
                            .or_default()
                            .push(symbol.clone());
                    }
                    let mut completions = Vec::new();
                    for (number, (name, candidates)) in symbols.into_iter().enumerate() {
                        if number % 128 == 0 {
                            completion_progress.send(()).await.map_err(|_| {
                                anyhow::anyhow!("Android resource completion cancelled")
                            })?;
                        }
                        ensure!(
                            !completion_progress.is_closed(),
                            "Android resource completion cancelled"
                        );
                        // One insertion per name; preserve all contributing namespaces
                        // in its provenance instead of choosing an arbitrary dependency.
                        let remaining = MAX_RESOURCE_BYTES
                            .checked_sub(completion_bytes)
                            .and_then(|remaining| remaining.checked_sub(2 * name.len() as u64))
                            .context("Android resource completion exceeds text size limit")?;
                        let mut documentation = ResourceText::new(remaining as usize);
                        for (candidate_number, symbol) in candidates.iter().enumerate() {
                            if candidate_number > 0 && candidate_number % 128 == 0 {
                                completion_progress.send(()).await.map_err(|_| {
                                    anyhow::anyhow!("Android resource completion cancelled")
                                })?;
                            }
                            ensure!(
                                !completion_progress.is_closed(),
                                "Android resource completion cancelled"
                            );
                            use std::fmt::Write as _;
                            let mut checkpoint =
                                || resource_worker_checkpoint(&completion_progress);
                            let mut control = resources::ResourceParserControl::new(
                                &mut checkpoint,
                                MAX_RESOURCE_PARSER_WORK,
                            );
                            if !documentation.text.is_empty() {
                                documentation.write_str("\n\n").context(
                                    "Android resource completion exceeds text size limit",
                                )?;
                            }
                            append_provenance(&mut documentation, &scan, symbol, &mut control)?;
                        }
                        completion_bytes += documentation.text.len() as u64 + 2 * name.len() as u64;
                        completions.push(Completion {
                            replace_range: source_snapshot.anchor_before(reference.name_range.start)
                                ..source_snapshot.anchor_after(reference.name_range.end),
                            new_text: name.clone(),
                            label: CodeLabel::plain(name, None),
                            documentation: Some(
                                crate::lsp_store::CompletionDocumentation::MultiLinePlainText(
                                    documentation.text.into(),
                                ),
                            ),
                            source: CompletionSource::Custom,
                            icon_path: None,
                            icon_color: None,
                            match_start: Some(
                                source_snapshot.anchor_before(reference.name_range.start),
                            ),
                            snippet_deduplication_key: None,
                            insert_text_mode: None,
                            confirm: None,
                            group: None,
                        });
                    }
                    anyhow::Ok(completions)
                });
                while progress_rx.recv().await.is_ok() {
                    if !resource_scan_current(&project, &query.token, cx)?
                        || !project.read_with(cx, |_, cx| {
                            query.origin.buffer.read(cx).version()
                                == *query.source_snapshot.version()
                        })?
                    {
                        return Ok(CompletionResponse {
                            completions: Vec::new(),
                            display_options: Default::default(),
                            is_incomplete: true,
                        });
                    }
                }
                completions = worker.await?;
                if !query_current(&project, &query, &index, cx).await? {
                    completions.clear();
                }
            }
            Ok(CompletionResponse {
                completions,
                display_options: Default::default(),
                is_incomplete: true,
            })
        }))
    }

    pub(crate) fn android_query_is_resource(
        &self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<bool>>> {
        let unavailable = self.android_resource_model_unavailable(buffer, position, cx);
        let seed = self.android_query_seed(buffer, position, cx);
        if unavailable.is_none() && seed.is_none() {
            return None;
        }
        Some(cx.spawn(async move |project, cx| {
            if let Some(unavailable) = unavailable
                && unavailable.await?
            {
                return Ok(true);
            }
            let Some(seed) = seed else {
                return Ok(false);
            };
            Ok(parse_rename_query_seed(&project, seed, cx).await?.is_some())
        }))
    }

    pub(crate) fn android_prepare_resource_rename(
        &self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<Option<PrepareRenameResponse>>>> {
        let unavailable = self.android_resource_model_unavailable(buffer, position, cx);
        let seed = self.android_query_seed(buffer, position, cx);
        if unavailable.is_none() && seed.is_none() {
            return None;
        }
        Some(cx.spawn(async move |project, cx| {
            if let Some(unavailable) = unavailable {
                ensure!(
                    !unavailable.await?,
                    "Sync Android successfully and select a variant before renaming resources"
                );
            }
            let Some(seed) = seed else {
                return Ok(None);
            };
            let Some(query) = parse_rename_query_seed(&project, seed, cx).await? else {
                return Ok(None);
            };
            let range = query
                .source_snapshot
                .anchor_before(query.reference.name_range.start)
                ..query
                    .source_snapshot
                    .anchor_after(query.reference.name_range.end);
            Ok(Some(PrepareRenameResponse::Success {
                range,
                language_server_id: None,
            }))
        }))
    }

    pub fn prepare_android_resource_rename(
        &self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        new_name: String,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<AndroidResourceRename>>> {
        let task =
            self.prepare_android_resource_rename_if_applicable(buffer, position, new_name, cx)?;
        Some(cx.background_spawn(async move {
            task.await?
                .context("No Android resource at the requested position")
        }))
    }

    /// Resolve cursor applicability before replacing ordinary language-server rename.
    /// `Ok(None)` means the caller must continue its existing rename workflow;
    /// malformed, unavailable, stale, or unsafe resource requests remain errors.
    pub fn prepare_android_resource_rename_if_applicable(
        &self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        new_name: String,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<Option<AndroidResourceRename>>>> {
        let unavailable = self.android_resource_model_unavailable(buffer, position, cx);
        let seed = self.android_query_seed(buffer, position, cx);
        if unavailable.is_none() && seed.is_none() {
            return None;
        }
        Some(cx.spawn(async move |project, cx| {
            if let Some(unavailable) = unavailable {
                ensure!(!unavailable.await?, "Sync Android successfully and select a variant before renaming resources");
            }
            let Some(seed) = seed else {
                return Ok(None);
            };
            let Some(query) = parse_rename_query_seed(&project, seed, cx).await? else {
                return Ok(None);
            };
            let roots = component_roots(&query.selected, true, "", SourceScope::Main);
            resources::valid_rename(&query.reference.kind, &query.reference.name, &new_name)?;
            let selected_index = build_index(&project, query.roots.clone(), query.token.clone(), false, cx).await?;
            let selection = ResourceSymbolQuery::capture(&query);
            let selected = query.selected.clone();
            let query_roots = query.roots.clone();
            let selected_scan = selected_index.scan.clone();
            let worker_roots = roots.clone();
            let symbol = resource_worker(&project, &query, cx, move |control| {
                let roots = worker_roots;
                control.checkpoint()?;
                let symbols = query_symbols_for(&selection, &selected_scan, control)?;
            ensure!(symbols.len() == 1, "Resource ownership is ambiguous or unavailable; sync and resolve the resource first");
            let symbol = symbols.first().context("Resource ownership is unavailable")?;
            ensure!(selected.model.resource_models.values().all(|model| model.non_transitive_r), "Resource rename with transitive R classes requires language-server support");
            let owner_modules = roots.iter().filter(|root| root.namespace.as_ref() == symbol.namespace.as_str() && root.kind == SourceKind::Resources).filter_map(|root| root.domain.split('/').next()).collect::<BTreeSet<_>>();
            ensure!(owner_modules.len() == 1, "Multiple modules share this resource namespace; ownership cannot be proven safely");
            ensure!(query_roots.iter().filter(|root| root.namespace.as_ref() == symbol.namespace.as_str() && !root.external).all(|root| selected.model.resource_models.contains_key(root.domain.as_ref())), "Sync Android resource overlay metadata before renaming");
            control.checkpoint()?;
            let resolution = resources::resolve(&selected_scan.declarations, symbol);
            ensure!(selected_scan.declarations.iter().filter(|entry| &entry.symbol == symbol).all(|entry| !entry.external), "A dependency also declares this resource identity; rename ownership cannot be proven safely");
            ensure!(resolution.conflicts.is_empty(), "Resource has equal-priority conflicts; resolve them before renaming");
            ensure!(resolution.winners.iter().all(|entry| !entry.generated && !entry.external), "Dependency, framework and generated resources are read-only for resource rename");
                control.checkpoint()?;
                anyhow::Ok(symbol.clone())
            }).await?;
            let index = build_index(&project, roots, query.token.clone(), true, cx).await?;
            let mut source_index = ResourceSourceIndex::capture(&index);
            let selected_scan = selected_index.scan.clone();
            let worker_new_name = new_name.clone();
            let edits = resource_worker(&project, &query, cx, move |control| {
                let new_name = worker_new_name;
                source_index.index_namespaces(control)?;
            for path in &source_index.files {
                control.checkpoint()?;
                control.charge_work(source_index.roots.len().saturating_mul(source_index.roots.len()).saturating_mul(64))?;
                for resource_root in source_index.roots.iter().filter(|root| root.kind == SourceKind::Resources && root.namespace.as_ref() == symbol.namespace.as_str() && path.strip_prefix(&root.path).is_ok_and(|relative| relative.components().count() == 2)) {
                    ensure!(!source_index.roots.iter().any(|root| path.starts_with(&root.path) && root.path.components().count() >= resource_root.path.components().count() && (root.kind != SourceKind::Resources || !path.strip_prefix(&root.path).is_ok_and(|relative| relative.components().count() == 2))), "Resource file in {} overlaps incompatible source roots; rename ownership cannot be proven safely", path.display());
                }
            }
            for declaration in selected_scan.declarations.iter().chain(&source_index.scan.declarations).filter(|entry| entry.symbol.kind == symbol.kind && entry.symbol.name == symbol.name) {
                control.charge_work(source_index.roots.len().saturating_mul(64))?;
                let namespaces = source_index.roots.iter().filter(|root| {
                    root.kind == SourceKind::Resources
                        && declaration.path.strip_prefix(&root.path).is_ok_and(|relative| relative.components().count() == 2)
                }).map(|root| root.namespace.as_ref()).collect::<BTreeSet<_>>();
                ensure!(!(namespaces.contains(symbol.namespace.as_str()) && namespaces.len() > 1), "Resource declaration in {} is shared by multiple namespaces; rename ownership cannot be proven safely", declaration.path.display());
            }
            let mut declaration_locations = BTreeSet::new();
            for candidate in source_index.scan.declarations.iter().filter(|entry| entry.symbol == symbol) {
                control.checkpoint()?;
                declaration_locations.insert((&candidate.path, candidate.name_range.as_ref().map(|range| (range.start, range.end))));
            }
            for entry in selected_scan.declarations.iter().filter(|entry| entry.symbol == symbol) {
                control.checkpoint()?;
                ensure!(declaration_locations.contains(&(&entry.path, entry.name_range.as_ref().map(|range| (range.start, range.end)))), "Resource declaration ownership changed across source roots; rename ownership cannot be proven safely");
            }
            ensure!(!source_index.scan.conflicts.contains(&symbol), "Resource has conflicting declarations in a source set");
            ensure!(!source_index.scan.declarations.iter().any(|entry| entry.symbol.namespace == symbol.namespace && entry.symbol.kind == symbol.kind && entry.symbol.name == new_name), "The new resource name already exists in a variant or qualifier");
            let declarations = source_index.scan.declarations.iter().filter(|entry| &entry.symbol == &symbol).collect::<Vec<_>>();
            ensure!(!declarations.is_empty(), "Resource declaration is not editable");
            ensure!(declarations.iter().all(|entry| entry.name_range.is_some() && !entry.generated && !entry.external && entry.xml_name == symbol.name), "File resources, generated resources, and normalized names cannot be safely renamed with a text transaction");
            let mut edits: BTreeMap<PathBuf, Vec<Range<usize>>> = BTreeMap::new();
            let mut edit_count = 0_usize;
            for declaration in declarations { if let Some(range) = &declaration.name_range { append_rename_edit(&mut edits, &mut edit_count, &declaration.path, range.clone(), control)?; } }
            for document in &source_index.documents {
                control.checkpoint()?;
                let xml = document.path.extension().is_some_and(|extension| extension == "xml");
                let text = document.snapshot.text();
                if !xml {
                    ensure!(!resources::code_rename_has_unsupported_syntax(&text), "Unicode escapes, quoted identifiers or templates in {} require server-backed rename", document.path.display());
                }
                let mut code_references = None;
                if !xml && (text.contains(&symbol.name) || text.contains("import")) {
                    let (unambiguous, references) = resources::code_rename_analysis_controlled(&text, MAX_RESOURCE_SYMBOLS, control)?;
                    ensure!(unambiguous, "Ambiguous R binding or member import in {}; use the language server or remove the ambiguity before renaming", document.path.display());
                    code_references = Some(references);
                }
                ensure!(!resources::code_rename_has_unsupported_templates(&text, &symbol.kind, &symbol.name), "Kotlin interpolation in {} requires server-backed rename", document.path.display());
                if xml { ensure!(resources::xml_rename_is_unambiguous_controlled(&text, &symbol.kind, &symbol.name, control)?, "Data binding or escaped references in {} require server-backed rename", document.path.display()); }
                let references = if let Some(references) = code_references { references } else { resources::references_controlled(&text, xml, MAX_RESOURCE_SYMBOLS, control)? };
                for reference in references {
                    control.checkpoint()?;
                    control.charge_work(source_index.roots.len().saturating_mul(64))?;
                    if reference.kind == symbol.kind && reference.name == symbol.name {
                        if !xml && reference.namespace.is_none() {
                            let namespaces = source_index.roots.iter().filter(|root| matches!(root.kind, SourceKind::Java | SourceKind::Kotlin) && document.path.starts_with(&root.path)).map(|root| root.namespace.as_ref()).collect::<BTreeSet<_>>();
                            ensure!(!(namespaces.contains(symbol.namespace.as_str()) && namespaces.len() > 1), "An implicit R reference in {} is shared by multiple namespaces; rename ownership cannot be proven safely", document.path.display());
                        }
                        if xml && reference.namespace.is_none() {
                            let namespaces = source_index.roots.iter().filter(|root| root.kind == SourceKind::Resources && document.path.strip_prefix(&root.path).is_ok_and(|relative| relative.components().count() == 2)).map(|root| root.namespace.as_ref()).collect::<BTreeSet<_>>();
                            ensure!(!(namespaces.contains(symbol.namespace.as_str()) && namespaces.len() > 1), "An unqualified XML reference in {} is shared by multiple namespaces; rename ownership cannot be proven safely", document.path.display());
                        }
                        if xml && reference.namespace.is_none() && document.namespace.as_ref() != symbol.namespace.as_str() {
                            anyhow::bail!("An unqualified cross-namespace XML reference in {} cannot be proven safe across all variants", document.path.display());
                        }
                        if reference_matches(document, &reference, &source_index, &symbol, control)? {
                            ensure!(!document.generated, "A generated reference in {} prevents a complete safe rename", document.path.display());
                            append_rename_edit(&mut edits, &mut edit_count, &document.path, reference.name_range, control)?;
                        }
                    }
                }
            }
            ensure!(edits.len() <= 200 && edits.values().map(Vec::len).sum::<usize>() <= 2_000, "Resource rename exceeds review limit (200 files / 2,000 edits)");
            for ranges in edits.values_mut() { control.checkpoint()?; ranges.sort_by_key(|range| range.start); ranges.dedup(); }
                anyhow::Ok(edits)
            }).await?;
            ensure!(query_current(&project, &query, &index, cx).await?, "Android model or source changed while preparing rename; try again");
            Ok(Some(AndroidResourceRename { index, edits, new_name }))
        }))
    }

    pub fn apply_android_resource_rename(
        &self,
        plan: AndroidResourceRename,
        cx: &mut Context<Self>,
    ) -> Task<Result<ProjectTransaction>> {
        let filesystem = self.fs.clone();
        cx.spawn(async move |project, cx| {
            let mut current_files = enumerate_files_in_background(
                filesystem.clone(),
                plan.index.roots.clone(),
                true,
                cx,
            )
            .await?;
            project.read_with(cx, |project, cx| {
                for buffer in project.opened_buffers(cx) {
                    if let Some(file) = buffer.read(cx).file()
                        && file.disk_state() == DiskState::New
                    {
                        let path = project.android_file_path(file.as_ref(), cx);
                        if indexable_file(&path, &plan.index.roots, true) {
                            current_files.insert(path);
                        }
                    }
                }
            })?;
            ensure!(
                current_files == plan.index.files,
                "Files changed after resource rename preview; prepare a new preview"
            );
            for document in plan.index.documents.values() {
                let current = if filesystem.is_file(&document.path).await {
                    Some(filesystem.load(&document.path).await?)
                } else {
                    None
                };
                ensure!(
                    current == document.disk_text,
                    "File changed on disk after resource rename preview: {}",
                    document.path.display()
                );
            }
            for (path, metadata) in &plan.index.scan.supporting_metadata {
                ensure!(
                    canonical_resource_metadata(&filesystem, path).await? == *metadata,
                    "Resource metadata changed after rename preview: {}",
                    path.display()
                );
            }
            for path in plan.edits.keys() {
                if plan.index.documents[path].disk_text.is_none() {
                    let mut ancestor = path.parent();
                    while let Some(candidate) = ancestor {
                        if filesystem.is_dir(candidate).await {
                            ensure!(
                                filesystem.canonicalize(candidate).await? == candidate,
                                "New resource path changed through a symlink"
                            );
                            break;
                        }
                        ancestor = candidate.parent();
                    }
                    continue;
                }
                let canonical = filesystem.canonicalize(path).await?;
                ensure!(
                    canonical == *path,
                    "Resource rename path changed through a symlink"
                );
                let metadata = filesystem
                    .metadata(path)
                    .await?
                    .context("A resource rename file was deleted")?;
                ensure!(
                    metadata.is_writable,
                    "Resource rename file is read-only: {}",
                    path.display()
                );
            }
            project.update(cx, |project, cx| {
                ensure!(
                    project.android_model.is_current(&plan.index.token)
                        && !project.is_read_only(cx)
                        && project.is_local()
                        && !crate::trusted_worktrees::TrustedWorktrees::has_restricted_worktrees(
                            &project.worktree_store(),
                            cx,
                        ),
                    "Android project changed after resource rename preview"
                );
                for document in plan.index.documents.values() {
                    let buffer = document.buffer.read(cx);
                    ensure!(
                        buffer.version() == *document.snapshot.version()
                            && buffer
                                .file()
                                .is_some_and(|file| !file.disk_state().is_deleted()
                                    && project.android_file_path(file.as_ref(), cx)
                                        == document.path),
                        "File changed after resource rename preview: {}",
                        document.path.display()
                    );
                }
                for path in plan.edits.keys() {
                    ensure!(
                        !plan.index.documents[path].buffer.read(cx).read_only(),
                        "Resource buffer is read-only"
                    );
                }
                let mut transaction = ProjectTransaction::default();
                for (path, ranges) in plan.edits {
                    let handle = &plan.index.documents[&path].buffer;
                    handle.update(cx, |buffer, cx| {
                        buffer.finalize_last_transaction();
                        buffer.start_transaction();
                        buffer.edit(
                            ranges
                                .into_iter()
                                .map(|range| (range, plan.new_name.clone())),
                            None,
                            cx,
                        );
                        if buffer.end_transaction(cx).is_some()
                            && let Some(edit) = buffer.finalize_last_transaction()
                        {
                            transaction.0.insert(handle.clone(), edit.clone());
                        }
                    });
                }
                Ok(transaction)
            })?
        })
    }
}

fn append_rename_edit(
    edits: &mut BTreeMap<PathBuf, Vec<Range<usize>>>,
    count: &mut usize,
    path: &std::path::Path,
    range: Range<usize>,
    control: &mut resources::ResourceParserControl<'_>,
) -> Result<()> {
    control.checkpoint()?;
    ensure!(
        *count < 2_000 && (edits.contains_key(path) || edits.len() < 200),
        "Resource rename exceeds review limit (200 files / 2,000 edits)"
    );
    edits.entry(path.to_owned()).or_default().push(range);
    *count += 1;
    Ok(())
}

fn parse_seed_source(
    snapshot: &BufferSnapshot,
    _path: &std::path::Path,
    offset: usize,
    xml: bool,
    namespace: &str,
    completion: bool,
    control: &mut resources::ResourceParserControl<'_>,
) -> Result<Option<(Reference, String, bool)>> {
    #[cfg(any(test, feature = "test-support"))]
    {
        *RESOURCE_CONTEXT_PARSE_COUNTS
            .lock()
            .expect("Resource parser test counters")
            .entry(_path.to_owned())
            .or_default() += 1;
    }
    ensure!(
        snapshot.len() as u64 <= MAX_RESOURCE_BYTES,
        "Android resource source exceeds text size limit"
    );
    control.checkpoint()?;
    let text = snapshot.text();
    control.checkpoint()?;
    let context = if completion {
        resources::completion_context_controlled(&text, offset, xml, namespace, control)?
    } else {
        resources::reference_context_controlled(
            &text,
            offset,
            xml,
            namespace,
            MAX_RESOURCE_SYMBOLS,
            control,
        )?
    };
    Ok(context.map(|(reference, namespace)| {
        let explicit = !xml
            && reference.namespace.as_deref().is_some_and(|namespace| {
                text.get(reference.range.clone())
                    .and_then(|text| text.strip_prefix(namespace))
                    .is_some_and(|text| text.starts_with(".R"))
            });
        (reference, namespace, explicit)
    }))
}

fn resource_worker_checkpoint(sender: &async_channel::Sender<()>) -> Result<()> {
    match sender.try_send(()) {
        Ok(()) | Err(async_channel::TrySendError::Full(_)) => Ok(()),
        Err(async_channel::TrySendError::Closed(_)) => {
            anyhow::bail!("Android resource request cancelled")
        }
    }
}

async fn resource_foreground_yield() {
    let mut yielded = false;
    futures::future::poll_fn(|cx| {
        if yielded {
            std::task::Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    })
    .await;
}

async fn source_request_current(
    project: &WeakEntity<Project>,
    token: &ModelToken,
    buffer: &Entity<Buffer>,
    snapshot: &BufferSnapshot,
    path: &std::path::Path,
    cx: &mut AsyncApp,
) -> Result<bool> {
    Ok(resource_scan_current(project, token, cx)?
        && project.read_with(cx, |project, cx| {
            let buffer = buffer.read(cx);
            buffer.version() == *snapshot.version()
                && buffer.file().is_some_and(|file| {
                    !file.disk_state().is_deleted()
                        && project.worktree_for_id(file.worktree_id(cx), cx).is_some()
                        && project.android_file_path(file.as_ref(), cx) == path
                })
        })?)
}

async fn parse_query_seed(
    project: &WeakEntity<Project>,
    seed: ResourceQuerySeed,
    completion: bool,
    cx: &mut AsyncApp,
) -> Result<ResourceCursorResult> {
    if !source_request_current(
        project,
        &seed.token,
        &seed.buffer,
        &seed.snapshot,
        &seed.path,
        cx,
    )
    .await?
    {
        return Ok(ResourceCursorResult::Stale);
    }
    let snapshot = seed.snapshot.clone();
    let namespace = seed.component_namespace.clone();
    let offset = seed.offset;
    let xml = seed.xml;
    let path = seed.path.clone();
    let (sender, receiver) = async_channel::bounded(1);
    let worker = cx.background_executor().spawn(async move {
        let mut checkpoint = || resource_worker_checkpoint(&sender);
        parse_seed_source(
            &snapshot,
            &path,
            offset,
            xml,
            &namespace,
            completion,
            &mut resources::ResourceParserControl::new(&mut checkpoint, MAX_RESOURCE_PARSER_WORK),
        )
    });
    while receiver.recv().await.is_ok() {
        if !source_request_current(
            project,
            &seed.token,
            &seed.buffer,
            &seed.snapshot,
            &seed.path,
            cx,
        )
        .await?
        {
            return Ok(ResourceCursorResult::Stale);
        }
    }
    let parsed = worker.await?;
    if !source_request_current(
        project,
        &seed.token,
        &seed.buffer,
        &seed.snapshot,
        &seed.path,
        cx,
    )
    .await?
    {
        return Ok(ResourceCursorResult::Stale);
    }
    Ok(match parsed {
        Some((reference, namespace, explicit)) => {
            ResourceCursorResult::Resource(Box::new(seed.finish(reference, namespace, explicit)))
        }
        None => ResourceCursorResult::NotResource,
    })
}

struct LegacySourceInput {
    buffer: Entity<Buffer>,
    snapshot: BufferSnapshot,
    path: PathBuf,
}

fn observe_legacy_input(
    project: &WeakEntity<Project>,
    buffer: &Entity<Buffer>,
    snapshot: &BufferSnapshot,
    worktree_id: worktree::WorktreeId,
    path: Arc<util::rel_path::RelPath>,
    invalidated: Arc<std::sync::atomic::AtomicBool>,
    cx: &mut AsyncApp,
) -> Result<gpui::Subscription> {
    let snapshot = snapshot.clone();
    project.update(cx, |_, cx| {
        cx.observe(buffer, move |project, buffer, cx| {
            let buffer = buffer.read(cx);
            let current = buffer.version() == *snapshot.version()
                && buffer.file().is_some_and(|file| {
                    !file.disk_state().is_deleted()
                        && file.worktree_id(cx) == worktree_id
                        && file.path() == path.as_ref()
                        && project.worktree_for_id(worktree_id, cx).is_some()
                });
            if !current {
                invalidated.store(true, std::sync::atomic::Ordering::Release);
            }
        })
    })
}

async fn legacy_cursor_inputs_current(
    project: &WeakEntity<Project>,
    token: &ModelToken,
    source: &LegacySourceInput,
    namespace: Option<&LegacySourceInput>,
    invalidated: &std::sync::atomic::AtomicBool,
    cx: &mut AsyncApp,
) -> Result<bool> {
    if !source_request_current(
        project,
        token,
        &source.buffer,
        &source.snapshot,
        &source.path,
        cx,
    )
    .await?
    {
        return Ok(false);
    }
    if let Some(namespace) = namespace {
        ensure!(
            source_request_current(
                project,
                token,
                &namespace.buffer,
                &namespace.snapshot,
                &namespace.path,
                cx,
            )
            .await?,
            "Legacy Android Gradle namespace changed during resource lookup; try again"
        );
    }
    ensure!(
        !invalidated.load(std::sync::atomic::Ordering::Acquire),
        "Legacy Android resource input changed during lookup; try again"
    );
    Ok(true)
}

async fn source_cursor_worker<T: Send + 'static>(
    project: &WeakEntity<Project>,
    token: &ModelToken,
    source: &LegacySourceInput,
    namespace: Option<&LegacySourceInput>,
    invalidated: Arc<std::sync::atomic::AtomicBool>,
    cx: &mut AsyncApp,
    operation: impl FnOnce(&mut resources::ResourceParserControl<'_>) -> Result<T> + Send + 'static,
) -> Result<Option<T>> {
    if !legacy_cursor_inputs_current(project, token, source, namespace, &invalidated, cx).await? {
        return Ok(None);
    }
    let (sender, receiver) = async_channel::bounded(1);
    let worker_invalidated = invalidated.clone();
    let worker = cx.background_executor().spawn(async move {
        let mut checkpoint = || {
            ensure!(
                !worker_invalidated.load(std::sync::atomic::Ordering::Acquire),
                "Legacy Android resource input changed during lookup; try again"
            );
            resource_worker_checkpoint(&sender)
        };
        operation(&mut resources::ResourceParserControl::new(
            &mut checkpoint,
            MAX_RESOURCE_PARSER_WORK,
        ))
    });
    while receiver.recv().await.is_ok() {
        if !legacy_cursor_inputs_current(project, token, source, namespace, &invalidated, cx)
            .await?
        {
            return Ok(None);
        }
    }
    let result = worker.await?;
    if !legacy_cursor_inputs_current(project, token, source, namespace, &invalidated, cx).await? {
        return Ok(None);
    }
    Ok(Some(result))
}

async fn parse_rename_query_seed(
    project: &WeakEntity<Project>,
    seed: ResourceQuerySeed,
    cx: &mut AsyncApp,
) -> Result<Option<ResourceQuery>> {
    let token = seed.token.clone();
    let buffer = seed.buffer.clone();
    let snapshot = seed.snapshot.clone();
    let path = seed.path.clone();
    let query = parse_query_seed(project, seed, false, cx).await?;
    ensure!(
        source_request_current(project, &token, &buffer, &snapshot, &path, cx).await?,
        "Android model or source changed while classifying rename; try again"
    );
    match query {
        ResourceCursorResult::Resource(query) => Ok(Some(*query)),
        ResourceCursorResult::NotResource => Ok(None),
        ResourceCursorResult::Stale => {
            anyhow::bail!("Android model or source changed while classifying rename; try again")
        }
    }
}

async fn resource_worker<T: Send + 'static>(
    project: &WeakEntity<Project>,
    query: &ResourceQuery,
    cx: &mut AsyncApp,
    operation: impl FnOnce(&mut resources::ResourceParserControl<'_>) -> Result<T> + Send + 'static,
) -> Result<T> {
    let (sender, receiver) = async_channel::bounded(1);
    let worker = cx.background_executor().spawn(async move {
        let mut checkpoint = || resource_worker_checkpoint(&sender);
        operation(&mut resources::ResourceParserControl::new(
            &mut checkpoint,
            MAX_RESOURCE_PARSER_WORK,
        ))
    });
    while receiver.recv().await.is_ok() {
        ensure!(
            source_request_current(
                project,
                &query.token,
                &query.origin.buffer,
                &query.source_snapshot,
                &query.source_path,
                cx
            )
            .await?,
            "Android model or source changed while preparing resource result"
        );
    }
    let result = worker.await?;
    ensure!(
        source_request_current(
            project,
            &query.token,
            &query.origin.buffer,
            &query.source_snapshot,
            &query.source_path,
            cx
        )
        .await?,
        "Android model or source changed while preparing resource result"
    );
    Ok(result)
}

struct ResourceSymbolQuery {
    reference: Reference,
    namespace: String,
    component_namespace: String,
    xml: bool,
    transitive: bool,
}

impl ResourceSymbolQuery {
    fn capture(query: &ResourceQuery) -> Self {
        Self {
            reference: query.reference.clone(),
            namespace: query.namespace.clone(),
            component_namespace: query.component_namespace.clone(),
            xml: query.xml,
            transitive: query.transitive,
        }
    }
}

async fn query_symbols_background(
    project: &WeakEntity<Project>,
    query: &ResourceQuery,
    index: &ResourceIndex,
    cx: &mut AsyncApp,
) -> Result<Vec<Symbol>> {
    let selection = ResourceSymbolQuery::capture(query);
    let scan = index.scan.clone();
    resource_worker(project, query, cx, move |control| {
        query_symbols_for(&selection, &scan, control)
    })
    .await
}

fn query_symbols_for(
    query: &ResourceSymbolQuery,
    scan: &ResourceScan,
    control: &mut resources::ResourceParserControl<'_>,
) -> Result<Vec<Symbol>> {
    control.checkpoint()?;
    let symbol = Symbol {
        namespace: query
            .reference
            .namespace
            .as_deref()
            .unwrap_or(&query.namespace)
            .to_owned(),
        kind: query.reference.kind.clone(),
        name: query.reference.name.clone(),
    };
    if scan.symbol_declarations.contains_key(&symbol) {
        return Ok(vec![symbol]);
    }
    let dependency_fallback = if query.xml {
        query.reference.namespace.is_none()
    } else {
        query.transitive && symbol.namespace == query.component_namespace
    };
    // Transitive R adds dependency members to the owning component's R class. It
    // does not make an unrelated explicitly qualified R class an alias of it.
    if !dependency_fallback {
        return Ok(Vec::new());
    }
    let mut symbols = Vec::new();
    for entry in scan.symbol_declarations.keys() {
        control.checkpoint()?;
        if entry.kind == symbol.kind && entry.name == symbol.name && entry.namespace != "android" {
            control.charge_work(
                entry
                    .namespace
                    .len()
                    .saturating_add(entry.kind.len())
                    .saturating_add(entry.name.len()),
            )?;
            symbols.push(entry.clone());
        }
    }
    Ok(symbols)
}

struct ResourceText {
    text: String,
    limit: usize,
}

impl ResourceText {
    fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            limit,
        }
    }
}

impl std::fmt::Write for ResourceText {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        let length = self
            .text
            .len()
            .checked_add(value.len())
            .ok_or(std::fmt::Error)?;
        if length > self.limit {
            return Err(std::fmt::Error);
        }
        if length > self.text.capacity() {
            // Grow geometrically within the limit; repeated ID declarations must
            // not turn bounded documentation into quadratic reallocations.
            let capacity = length
                .max(self.text.capacity().saturating_mul(2))
                .min(self.limit);
            self.text
                .try_reserve_exact(capacity - self.text.len())
                .map_err(|_| std::fmt::Error)?;
        }
        self.text.push_str(value);
        Ok(())
    }
}

fn append_provenance(
    text: &mut ResourceText,
    scan: &ResourceScan,
    symbol: &Symbol,
    control: &mut resources::ResourceParserControl<'_>,
) -> Result<()> {
    use std::fmt::Write as _;
    control.checkpoint()?;
    let declarations = scan
        .symbol_declarations
        .get(symbol)
        .into_iter()
        .flatten()
        .map(|index| &scan.declarations[*index]);
    let resolution = resources::resolve_entries(declarations, symbol);
    control.checkpoint()?;
    writeln!(
        text,
        "{}: {}/{}",
        symbol.namespace, symbol.kind, symbol.name
    )
    .context("Android resource documentation exceeds text size limit")?;
    for diagnostic in &scan.diagnostics {
        control.checkpoint()?;
        writeln!(text, "Incomplete index: {diagnostic}")
            .context("Android resource documentation exceeds text size limit")?;
    }
    for (label, entries) in [
        ("Selected", resolution.winners),
        ("Shadowed", resolution.shadowed),
        ("Conflict", resolution.conflicts),
    ] {
        for entry in entries {
            control.checkpoint()?;
            writeln!(
                text,
                "{label}: {} [{}]{}",
                entry.path.display(),
                if entry.qualifier.is_empty() {
                    "default"
                } else {
                    &entry.qualifier
                },
                if entry.generated || entry.external {
                    " (read-only for resource rename)"
                } else {
                    ""
                }
            )
            .context("Android resource documentation exceeds text size limit")?;
        }
    }
    Ok(())
}

fn resource_documentation(
    scan: &ResourceScan,
    symbols: &[Symbol],
    limit: usize,
    control: &mut resources::ResourceParserControl<'_>,
) -> Result<String> {
    use std::fmt::Write as _;
    let mut text = ResourceText::new(limit);
    for (index, symbol) in symbols.iter().enumerate() {
        control.checkpoint()?;
        if index > 0 {
            text.write_str("\n")
                .context("Android resource documentation exceeds text size limit")?;
        }
        append_provenance(&mut text, scan, symbol, control)?;
    }
    Ok(text.text)
}

struct ResourceSourceDocument {
    snapshot: BufferSnapshot,
    path: PathBuf,
    namespace: Arc<str>,
    generated: bool,
}

struct ResourceSourceIndex {
    scan: Arc<ResourceScan>,
    roots: Vec<ResourceRoot>,
    files: BTreeSet<PathBuf>,
    documents: Vec<ResourceSourceDocument>,
    namespace_counts: BTreeMap<(String, String), usize>,
}

impl ResourceSourceIndex {
    fn capture(index: &ResourceIndex) -> Self {
        Self {
            scan: index.scan.clone(),
            roots: index.roots.clone(),
            files: index.files.clone(),
            namespace_counts: BTreeMap::new(),
            documents: index
                .documents
                .values()
                .map(|document| ResourceSourceDocument {
                    snapshot: document.snapshot.clone(),
                    path: document.path.clone(),
                    namespace: document.namespace.clone(),
                    generated: document.generated,
                })
                .collect(),
        }
    }

    fn index_namespaces(
        &mut self,
        control: &mut resources::ResourceParserControl<'_>,
    ) -> Result<()> {
        let mut bytes = 0_usize;
        for symbol in self.scan.symbol_declarations.keys() {
            control.checkpoint()?;
            control.charge_owned([symbol.kind.len(), symbol.name.len()])?;
            let key = (symbol.kind.clone(), symbol.name.clone());
            if !self.namespace_counts.contains_key(&key) {
                bytes = bytes
                    .checked_add(symbol.kind.len())
                    .and_then(|bytes| bytes.checked_add(symbol.name.len()))
                    .context("Android resource reference index exceeds text size limit")?;
                ensure!(
                    bytes as u64 <= MAX_RESOURCE_BYTES,
                    "Android resource reference index exceeds text size limit"
                );
            }
            *self.namespace_counts.entry(key).or_default() += 1;
        }
        Ok(())
    }
}

fn reference_matches(
    document: &ResourceSourceDocument,
    reference: &Reference,
    index: &ResourceSourceIndex,
    symbol: &Symbol,
    control: &mut resources::ResourceParserControl<'_>,
) -> Result<bool> {
    if reference.kind != symbol.kind || reference.name != symbol.name {
        return Ok(false);
    }
    let namespace = reference
        .namespace
        .as_deref()
        .unwrap_or(&document.namespace);
    control.charge_work(namespace.len().saturating_add(symbol.namespace.len()))?;
    if namespace == symbol.namespace {
        return Ok(true);
    }
    if reference.namespace.is_none()
        && document
            .path
            .extension()
            .is_some_and(|extension| extension == "xml")
    {
        control.charge_owned([
            namespace.len(),
            reference.kind.len(),
            reference.name.len(),
            reference.kind.len(),
            reference.name.len(),
        ])?;
        let local = Symbol {
            namespace: namespace.to_owned(),
            kind: reference.kind.clone(),
            name: reference.name.clone(),
        };
        return Ok(!index.scan.symbol_declarations.contains_key(&local)
            && index
                .namespace_counts
                .get(&(reference.kind.clone(), reference.name.clone()))
                == Some(&1));
    }
    Ok(false)
}

async fn query_current(
    project: &WeakEntity<Project>,
    query: &ResourceQuery,
    index: &ResourceIndex,
    cx: &mut AsyncApp,
) -> Result<bool> {
    let foreground_current = |project: &Project, cx: &App| {
        project.android_model.is_current(&query.token)
            && !crate::trusted_worktrees::TrustedWorktrees::has_restricted_worktrees(
                &project.worktree_store(),
                cx,
            )
            && query.origin.buffer.read(cx).version() == *query.source_snapshot.version()
            && query.origin.buffer.read(cx).file().is_some_and(|file| {
                !file.disk_state().is_deleted()
                    && project.android_file_path(file.as_ref(), cx) == query.source_path
            })
            && project.opened_buffers(cx).into_iter().all(|buffer| {
                let buffer = buffer.read(cx);
                let Some(file) = buffer.file() else {
                    return true;
                };
                let path = project.android_file_path(file.as_ref(), cx);
                !index.files.contains(&path)
                    || index.documents.contains_key(&path)
                    || !buffer.is_dirty() && !file.disk_state().is_deleted()
            })
            && index.documents.values().all(|document| {
                document.buffer.read(cx).version() == *document.snapshot.version()
                    && document.buffer.read(cx).file().is_some_and(|file| {
                        !file.disk_state().is_deleted()
                            && project.android_file_path(file.as_ref(), cx) == document.path
                    })
            })
    };
    if !project.read_with(cx, foreground_current)? {
        return Ok(false);
    }
    let filesystem = project.read_with(cx, |project, _| project.fs.clone())?;
    let sources = index
        .roots
        .iter()
        .any(|root| root.kind != SourceKind::Resources);
    let mut files =
        enumerate_files_in_background(filesystem.clone(), index.roots.clone(), sources, cx).await?;
    project.read_with(cx, |project, cx| {
        for buffer in project.opened_buffers(cx) {
            if let Some(file) = buffer.read(cx).file()
                && !file.disk_state().is_deleted()
            {
                let path = project.android_file_path(file.as_ref(), cx);
                if indexable_file(&path, &index.roots, sources) {
                    files.insert(path);
                }
            }
        }
    })?;
    if files != index.files {
        return Ok(false);
    }
    let documents = index
        .documents
        .values()
        .map(|document| (document.path.clone(), document.disk_metadata))
        .collect::<Vec<_>>();
    let scan = index.scan.clone();
    let metadata_current = cx
        .background_executor()
        .spawn(async move {
            for (path, expected) in documents {
                let actual = filesystem
                    .metadata(&path)
                    .await?
                    .map(|metadata| (metadata.inode, metadata.mtime, metadata.len));
                if actual != expected {
                    return anyhow::Ok(false);
                }
            }
            for (path, expected) in &scan.supporting_metadata {
                if canonical_resource_metadata(&filesystem, path).await? != *expected {
                    return Ok(false);
                }
            }
            Ok(true)
        })
        .await?;
    if !metadata_current {
        return Ok(false);
    }
    project.read_with(cx, foreground_current)
}

async fn enumerate_files(
    filesystem: &Arc<dyn fs::Fs>,
    roots: &[ResourceRoot],
    sources: bool,
) -> Result<BTreeSet<PathBuf>> {
    let mut paths = BTreeSet::new();
    let mut pending = roots
        .iter()
        .filter(|root| sources || root.kind == SourceKind::Resources)
        .map(|root| root.path.clone())
        .collect::<Vec<_>>();
    ensure!(
        pending.len() <= MAX_RESOURCE_FILES * 4,
        "Android resource scan exceeds root limit"
    );
    let mut visited = BTreeSet::new();
    while let Some(path) = pending.pop() {
        if !visited.insert(path.clone()) {
            continue;
        }
        ensure!(
            visited.len() <= MAX_RESOURCE_FILES * 4,
            "Android resource scan exceeds directory limit"
        );
        let Some(metadata) = filesystem.metadata(&path).await? else {
            continue;
        };
        ensure!(
            filesystem.canonicalize(&path).await? == path,
            "Android resource path changed through a symlink: {}",
            path.display()
        );
        ensure!(
            !metadata.is_symlink,
            "Android resource scan does not follow symlinks: {}",
            path.display()
        );
        if metadata.is_dir {
            let mut children = filesystem.read_dir(&path).await?;
            while let Some(child) = children.next().await {
                pending.push(child?);
                ensure!(
                    pending.len() <= MAX_RESOURCE_FILES * 4,
                    "Android resource scan exceeds pending directory limit"
                );
            }
        } else if indexable_file(&path, roots, sources) {
            paths.insert(path);
            ensure!(
                paths.len() <= MAX_RESOURCE_FILES,
                "Android resource scan exceeds file limit"
            );
        }
    }
    Ok(paths)
}

#[derive(Default)]
struct ResourceScan {
    declarations: Vec<Declaration>,
    symbol_declarations: BTreeMap<Symbol, Vec<usize>>,
    documents: BTreeMap<PathBuf, ResourceScanDocument>,
    conflicts: BTreeSet<Symbol>,
    diagnostics: Vec<String>,
    public_symbols: BTreeMap<String, BTreeSet<(String, String)>>,
    supporting_metadata: BTreeMap<PathBuf, Option<(u64, fs::MTime, u64)>>,
    public_symbol_count: usize,
}

struct ResourceScanDocument {
    namespace: Arc<str>,
    generated: bool,
    disk_metadata: Option<(u64, fs::MTime, u64)>,
    disk_text: Option<String>,
}

impl ResourceScan {
    fn merge_public_symbols(
        &mut self,
        namespace: &str,
        symbols: BTreeSet<(String, String)>,
        owned: &mut resources::ResourceOwnedBudget,
    ) -> Result<()> {
        owned.charge([namespace.len()])?;
        let public = self.public_symbols.entry(namespace.to_owned()).or_default();
        for symbol in symbols {
            if public.insert(symbol) {
                self.public_symbol_count += 1;
                ensure!(
                    self.public_symbol_count <= MAX_RESOURCE_SYMBOLS,
                    "Public resource list exceeds symbol limit"
                );
            }
        }
        Ok(())
    }
}

async fn enumerate_files_in_background(
    filesystem: Arc<dyn fs::Fs>,
    roots: Vec<ResourceRoot>,
    sources: bool,
    cx: &AsyncApp,
) -> Result<BTreeSet<PathBuf>> {
    // Keeping this Task owned by the caller cancels traversal when the request is dropped.
    cx.background_executor()
        .spawn(async move { enumerate_files(&filesystem, &roots, sources).await })
        .await
}

fn resource_scan_current(
    project: &WeakEntity<Project>,
    token: &ModelToken,
    cx: &AsyncApp,
) -> Result<bool> {
    Ok(project.read_with(cx, |project, cx| {
        project.android_model.is_current(token)
            && project.is_local()
            && !crate::trusted_worktrees::TrustedWorktrees::has_restricted_worktrees(
                &project.worktree_store(),
                cx,
            )
    })?)
}

async fn build_index(
    project: &WeakEntity<Project>,
    roots: Vec<ResourceRoot>,
    token: ModelToken,
    sources: bool,
    cx: &mut AsyncApp,
) -> Result<ResourceIndex> {
    let filesystem = project.read_with(cx, |project, _| project.fs.clone())?;
    let mut index = ResourceIndex {
        scan: Arc::new(ResourceScan::default()),
        documents: BTreeMap::new(),
        token: token.clone(),
        roots: roots.clone(),
        files: BTreeSet::new(),
        _external_worktrees: Vec::new(),
    };
    if !resource_scan_current(project, &token, cx)? {
        return Ok(index);
    }
    let mut files =
        enumerate_files_in_background(filesystem.clone(), roots.clone(), sources, cx).await?;
    if !resource_scan_current(project, &token, cx)? {
        return Ok(index);
    }
    for root in roots.iter().filter(|root| root.external) {
        if files.iter().any(|path| path.starts_with(&root.path)) {
            let (worktree, _) = project
                .update(cx, |project, cx| {
                    if project.find_worktree(&root.path, cx).is_none() {
                        project.settings_observer.update(cx, |observer, _| {
                            observer.exclude_resource_root_settings(root.path.clone());
                        });
                    }
                    project.find_or_create_worktree(&root.path, false, cx)
                })?
                .await?;
            index._external_worktrees.push(worktree);
        }
    }
    let mut opened_buffers = project.read_with(cx, |project, cx| {
        let mut opened = BTreeMap::new();
        for buffer in project.opened_buffers(cx) {
            if let Some(file) = buffer.read(cx).file()
                && !file.disk_state().is_deleted()
            {
                let path = project.android_file_path(file.as_ref(), cx);
                if indexable_file(&path, &roots, sources) {
                    files.insert(path.clone());
                    opened.entry(path).or_insert(buffer);
                }
            }
        }
        opened
    })?;
    ensure!(
        files.len() <= MAX_RESOURCE_FILES,
        "Android resource scan exceeds file limit"
    );
    let mut snapshots = BTreeMap::new();
    let mut bytes = 0_u64;
    for path in &files {
        if !resource_scan_current(project, &token, cx)? {
            return Ok(index);
        }
        let textual = path.extension().is_some_and(|extension| {
            extension == "xml" || extension == "kt" || extension == "java"
        });
        if !textual {
            continue;
        }
        if sources && !opened_buffers.contains_key(path) {
            let metadata = filesystem.metadata(path).await?;
            ensure!(
                bytes.saturating_add(metadata.map_or(0, |metadata| metadata.len))
                    <= MAX_RESOURCE_BYTES,
                "Android resource scan exceeds text size limit"
            );
            let buffer = project
                .update(cx, |project, cx| project.open_local_buffer(path, cx))?
                .await?;
            opened_buffers.insert(path.clone(), buffer);
        }
        if let Some(buffer) = opened_buffers.get(path) {
            let snapshot = cx.update(|cx| buffer.read(cx).snapshot());
            bytes = bytes.saturating_add(snapshot.len() as u64);
            ensure!(
                bytes <= MAX_RESOURCE_BYTES,
                "Android resource buffer exceeds text size limit"
            );
            snapshots.insert(path.clone(), snapshot);
        }
    }
    index.files = files.clone();
    if !resource_scan_current(project, &token, cx)? {
        return Ok(index);
    }
    let selection = project.read_with(cx, |project, _| project.android_model.selected.clone())?;
    let (progress_tx, progress_rx) = async_channel::bounded(1);
    let worker = cx.background_executor().spawn({
        let filesystem = filesystem.clone();
        let roots = roots.clone();
        let snapshots = snapshots.clone();
        async move {
            scan_resource_files(
                filesystem,
                roots,
                files,
                snapshots,
                selection,
                sources,
                progress_tx,
            )
            .await
        }
    });
    // One progress slot provides backpressure. At most two 32-file chunks can
    // advance before this task rechecks model/trust ownership. No worker is detached.
    while progress_rx.recv().await.is_ok() {
        if !resource_scan_current(project, &token, cx)? {
            return Ok(index);
        }
    }
    let mut scan = worker.await?;
    if !resource_scan_current(project, &token, cx)? {
        return Ok(index);
    }
    for (path, document) in std::mem::take(&mut scan.documents) {
        if let Some(buffer) = opened_buffers.remove(&path) {
            let snapshot = snapshots
                .remove(&path)
                .context("Resource snapshot missing")?;
            index.documents.insert(
                path.clone(),
                ResourceDocument {
                    buffer,
                    snapshot,
                    path,
                    namespace: document.namespace,
                    generated: document.generated,
                    disk_text: document.disk_text,
                    disk_metadata: document.disk_metadata,
                },
            );
        }
    }
    index.scan = Arc::new(scan);
    Ok(index)
}

struct ResourceScanDelivery {
    progress: async_channel::Sender<()>,
    owned_byte_limit: usize,
}

async fn scan_resource_files(
    filesystem: Arc<dyn fs::Fs>,
    roots: Vec<ResourceRoot>,
    files: BTreeSet<PathBuf>,
    snapshots: BTreeMap<PathBuf, BufferSnapshot>,
    selection: Option<Arc<SelectedProject>>,
    sources: bool,
    progress: async_channel::Sender<()>,
) -> Result<ResourceScan> {
    scan_resource_files_bounded(
        filesystem,
        roots,
        files,
        snapshots,
        selection,
        sources,
        ResourceScanDelivery {
            progress,
            owned_byte_limit: MAX_RESOURCE_BYTES as usize,
        },
    )
    .await
}

async fn scan_resource_files_bounded(
    filesystem: Arc<dyn fs::Fs>,
    roots: Vec<ResourceRoot>,
    files: BTreeSet<PathBuf>,
    snapshots: BTreeMap<PathBuf, BufferSnapshot>,
    selection: Option<Arc<SelectedProject>>,
    sources: bool,
    delivery: ResourceScanDelivery,
) -> Result<ResourceScan> {
    let ResourceScanDelivery {
        progress,
        owned_byte_limit,
    } = delivery;
    let mut owned = resources::ResourceOwnedBudget::new(owned_byte_limit);
    let mut namespaces: BTreeMap<Arc<str>, Arc<str>> = BTreeMap::new();
    let mut index = ResourceScan::default();
    let mut bytes = 0_u64;
    let mut conflict_entries = 0;
    let mut conflict_groups: BTreeMap<
        (Arc<str>, usize, String, Symbol),
        BTreeSet<(PathBuf, usize)>,
    > = BTreeMap::new();
    for (file_number, path) in files.iter().enumerate() {
        ensure!(!progress.is_closed(), "Android resource scan cancelled");
        if file_number % RESOURCE_SCAN_PROGRESS_FILES == 0 {
            progress
                .send(())
                .await
                .map_err(|_| anyhow::anyhow!("Android resource scan cancelled"))?;
        }
        let Some(root) = roots
            .iter()
            .filter(|root| path.starts_with(&root.path))
            .max_by_key(|root| root.path.components().count())
        else {
            continue;
        };
        let xml = path.extension().is_some_and(|extension| extension == "xml");
        let is_code = path
            .extension()
            .is_some_and(|extension| extension == "kt" || extension == "java");
        let metadata = canonical_resource_metadata(&filesystem, path).await?;
        owned.charge([path.as_os_str().len()])?;
        index.supporting_metadata.insert(path.clone(), metadata);
        let text = if xml || is_code {
            ensure!(
                bytes.saturating_add(metadata.map_or(0, |metadata| metadata.2))
                    <= MAX_RESOURCE_BYTES,
                "Android resource scan exceeds text size limit"
            );
            let text = if let Some(snapshot) = snapshots.get(path) {
                snapshot.text()
            } else {
                filesystem.load(path).await?
            };
            bytes = bytes
                .saturating_add((text.len() as u64).max(metadata.map_or(0, |metadata| metadata.2)));
            ensure!(
                bytes <= MAX_RESOURCE_BYTES,
                "Android resource buffer exceeds text size limit"
            );
            let namespace: Arc<str> = if is_code {
                let declared = {
                    let mut checkpoint = || {
                        ensure!(!progress.is_closed(), "Android resource scan cancelled");
                        Ok(())
                    };
                    let mut control = resources::ResourceParserControl::new(
                        &mut checkpoint,
                        MAX_RESOURCE_PARSER_WORK,
                    );
                    resources::code_declared_namespace_controlled(&text, &mut control)?
                };
                if let Some(declared) = declared {
                    if let Some(namespace) = namespaces.get(declared.as_str()) {
                        namespace.clone()
                    } else {
                        owned.charge([declared.len()])?;
                        let namespace: Arc<str> = declared.into();
                        namespaces.insert(namespace.clone(), namespace.clone());
                        namespace
                    }
                } else {
                    root.namespace.clone()
                }
            } else {
                root.namespace.clone()
            };
            let disk_text = if sources && metadata.is_some() {
                Some(filesystem.load(path).await?)
            } else {
                None
            };
            ensure!(
                canonical_resource_metadata(&filesystem, path).await? == metadata,
                "Android resource metadata changed while indexing: {}",
                path.display()
            );
            if snapshots.contains_key(path) {
                owned.charge([path.as_os_str().len()])?;
                index.documents.insert(
                    path.clone(),
                    ResourceScanDocument {
                        namespace,
                        generated: root.generated,
                        disk_metadata: metadata,
                        disk_text,
                    },
                );
            }
            Some(text)
        } else {
            None
        };
        if root.kind != SourceKind::Resources {
            continue;
        }
        let relative = path.strip_prefix(&root.path)?;
        let mut parts = relative.components();
        let Some(folder) = parts.next().and_then(|part| part.as_os_str().to_str()) else {
            continue;
        };
        let Some(filename) = parts.next().and_then(|part| part.as_os_str().to_str()) else {
            continue;
        };
        if parts.next().is_some() {
            continue;
        }
        let (kind, qualifier) = folder.split_once('-').unwrap_or((folder, ""));
        let Some(name) = resources::resource_file_name(filename) else {
            continue;
        };
        let mut declarations = Vec::new();
        if kind == "values" {
            if let Some(text) = &text {
                match resource_public_symbols_limited(text, MAX_RESOURCE_SYMBOLS) {
                    Ok(Some(public)) => {
                        index.merge_public_symbols(&root.namespace, public, &mut owned)?
                    }
                    Ok(None) => {}
                    Err(error) if !sources && !error.is::<resources::ResourceLimitExceeded>() => {
                        index
                            .diagnostics
                            .push(format!("{}: {error}", path.display()));
                    }
                    Err(error) => {
                        return Err(error.context(format!("Cannot safely scan {}", path.display())));
                    }
                }
            }
            if let Some(text) = &text {
                let declarations_in_file = match resources::declarations_limited(
                    text,
                    &root.namespace,
                    MAX_RESOURCE_SYMBOLS - index.declarations.len(),
                ) {
                    Ok(declarations) => declarations,
                    Err(error) if !sources && !error.is::<resources::ResourceLimitExceeded>() => {
                        index
                            .diagnostics
                            .push(format!("{}: {error}", path.display()));
                        continue;
                    }
                    Err(error) => {
                        return Err(error.context(format!("Cannot safely scan {}", path.display())));
                    }
                };
                for (kind, name, range, name_range) in declarations_in_file {
                    ensure!(!progress.is_closed(), "Android resource scan cancelled");
                    owned.charge([
                        root.namespace.len(),
                        kind.len(),
                        name.len(),
                        name.len(),
                        path.as_os_str().len(),
                        qualifier.len(),
                    ])?;
                    declarations.push(Declaration {
                        symbol: Symbol {
                            namespace: root.namespace.to_string(),
                            kind,
                            name: name.replace('.', "_"),
                        },
                        xml_name: name,
                        path: path.clone(),
                        qualifier: qualifier.into(),
                        priority: root.priority,
                        generated: root.generated,
                        external: root.external,
                        range,
                        name_range: Some(name_range),
                    });
                }
            }
        } else {
            owned.charge([
                root.namespace.len(),
                kind.len(),
                name.len(),
                name.len(),
                path.as_os_str().len(),
                qualifier.len(),
            ])?;
            declarations.push(Declaration {
                symbol: Symbol {
                    namespace: root.namespace.to_string(),
                    kind: kind.into(),
                    name: name.into(),
                },
                path: path.clone(),
                qualifier: qualifier.into(),
                priority: root.priority,
                generated: root.generated,
                external: root.external,
                range: 0..0,
                name_range: None,
                xml_name: name.into(),
            });
            if let Some(text) = &text {
                let references =
                    match resources::references_limited(text, true, MAX_RESOURCE_SYMBOLS) {
                        Ok(references) => references,
                        Err(error)
                            if !sources && !error.is::<resources::ResourceLimitExceeded>() =>
                        {
                            index
                                .diagnostics
                                .push(format!("{}: {error}", path.display()));
                            continue;
                        }
                        Err(error) => {
                            return Err(
                                error.context(format!("Cannot safely scan {}", path.display()))
                            );
                        }
                    };
                for reference in references
                    .into_iter()
                    .filter(|reference| reference.declaration)
                {
                    ensure!(!progress.is_closed(), "Android resource scan cancelled");
                    owned.charge([
                        root.namespace.len(),
                        reference.name.len(),
                        path.as_os_str().len(),
                        qualifier.len(),
                    ])?;
                    declarations.push(Declaration {
                        symbol: Symbol {
                            namespace: root.namespace.to_string(),
                            kind: reference.kind,
                            name: reference.name.clone(),
                        },
                        path: path.clone(),
                        qualifier: qualifier.into(),
                        priority: root.priority,
                        generated: root.generated,
                        external: root.external,
                        range: reference.range,
                        name_range: Some(reference.name_range),
                        xml_name: reference.name,
                    });
                }
            }
        }
        for declaration in declarations {
            for matching_root in roots.iter().filter(|candidate| candidate.path == root.path) {
                ensure!(!progress.is_closed(), "Android resource scan cancelled");
                owned.charge([
                    qualifier.len(),
                    declaration.symbol.namespace.len(),
                    declaration.symbol.kind.len(),
                    declaration.symbol.name.len(),
                    path.as_os_str().len(),
                ])?;
                let entries = conflict_groups
                    .entry((
                        matching_root.domain.clone(),
                        matching_root.priority,
                        qualifier.into(),
                        declaration.symbol.clone(),
                    ))
                    .or_default();
                if entries.insert((path.clone(), declaration.range.start)) {
                    conflict_entries += 1;
                    ensure!(
                        conflict_entries <= MAX_RESOURCE_SYMBOLS * 4,
                        "Android resource scan exceeds ownership limit"
                    );
                }
            }
            ensure!(
                index.declarations.len() < MAX_RESOURCE_SYMBOLS,
                "Android resource scan exceeds symbol limit"
            );
            owned.charge([
                declaration.symbol.namespace.len(),
                declaration.symbol.kind.len(),
                declaration.symbol.name.len(),
            ])?;
            index
                .symbol_declarations
                .entry(declaration.symbol.clone())
                .or_default()
                .push(index.declarations.len());
            index.declarations.push(declaration);
        }
    }
    let mut public_inputs = BTreeSet::new();
    if let Some(selection) = selection {
        for (domain, model) in &selection.model.resource_models {
            if !roots
                .iter()
                .any(|root| root.domain.as_ref() == domain.as_str())
            {
                continue;
            }
            for dependency in model.dependencies.iter().filter(|dependency| {
                roots.iter().any(|root| {
                    root.external
                        && root.namespace.as_ref() == dependency.namespace.as_str()
                        && root.path == dependency.path
                })
            }) {
                if let Some(path) = &dependency.public_resources {
                    ensure!(!progress.is_closed(), "Android resource scan cancelled");
                    owned.charge([dependency.namespace.len(), path.as_os_str().len()])?;
                    if !public_inputs.insert((dependency.namespace.clone(), path.clone())) {
                        continue;
                    }
                    ensure!(!progress.is_closed(), "Android resource scan cancelled");
                    if public_inputs.len() % RESOURCE_SCAN_PROGRESS_FILES == 1 {
                        progress
                            .send(())
                            .await
                            .map_err(|_| anyhow::anyhow!("Android resource scan cancelled"))?;
                    }
                    let metadata = canonical_resource_metadata(&filesystem, path).await?;
                    owned.charge([path.as_os_str().len()])?;
                    index.supporting_metadata.insert(path.clone(), metadata);
                    ensure!(
                        index.supporting_metadata.len() <= MAX_RESOURCE_FILES,
                        "Android resource scan exceeds metadata file limit"
                    );
                    let Some(metadata) = metadata else {
                        continue;
                    };
                    ensure!(
                        metadata.2 <= MAX_RESOURCE_BYTES,
                        "Public resource list exceeds size limit"
                    );
                    ensure!(
                        bytes.saturating_add(metadata.2) <= MAX_RESOURCE_BYTES,
                        "Android resource scan exceeds text size limit"
                    );
                    let text = filesystem.load(path).await?;
                    ensure!(
                        text.len() <= MAX_RESOURCE_BYTES as usize,
                        "Public resource list exceeds size limit"
                    );
                    bytes = bytes.saturating_add((text.len() as u64).max(metadata.2));
                    ensure!(
                        bytes <= MAX_RESOURCE_BYTES,
                        "Android resource scan exceeds text size limit"
                    );
                    ensure!(
                        canonical_resource_metadata(&filesystem, path).await? == Some(metadata),
                        "Public resource metadata changed while indexing: {}",
                        path.display()
                    );
                    let mut public = BTreeSet::new();
                    for line in text.lines() {
                        let mut words = line.split_whitespace();
                        if let (Some(kind), Some(name)) = (words.next(), words.next()) {
                            ensure!(!progress.is_closed(), "Android resource scan cancelled");
                            owned.charge([kind.len(), name.len()])?;
                            public.insert((kind.to_owned(), name.to_owned()));
                            ensure!(
                                public.len() <= MAX_RESOURCE_SYMBOLS,
                                "Public resource list exceeds symbol limit"
                            );
                        }
                    }
                    index.merge_public_symbols(&dependency.namespace, public, &mut owned)?;
                }
            }
        }
    }
    for ((_, _, _, symbol), entries) in conflict_groups {
        ensure!(!progress.is_closed(), "Android resource scan cancelled");
        if entries.len() > 1 && symbol.kind != "id" {
            index.conflicts.insert(symbol);
        }
    }
    ensure!(!progress.is_closed(), "Android resource scan cancelled");
    Ok(index)
}

impl Project {
    pub(crate) fn android_manifest_provenance(
        &self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<(Vec<LocationLink>, Vec<Hover>)>>> {
        if !self.is_local()
            || crate::trusted_worktrees::TrustedWorktrees::has_restricted_worktrees(
                &self.worktree_store(),
                cx,
            )
        {
            return None;
        }
        let selected = self.android_model.selected.as_ref()?;
        let snapshot = buffer.read(cx).snapshot();
        if snapshot.len() > 1024 * 1024 {
            return None;
        }
        let path = self.android_file_path(snapshot.file()?.as_ref(), cx);
        let (module, component, resources) = selected.modules().find_map(|(module, variant)| {
            variant.components.iter().find_map(|component| {
                let model = selected
                    .model
                    .resource_models
                    .get(&format!("{}/{}", module.path, component.name))?;
                (model.merged_manifest.as_ref() == Some(&path))
                    .then_some((module, component, model))
            })
        })?;
        let attribute = resources::manifest_attributes(&snapshot.text())
            .ok()?
            .into_iter()
            .find(|attribute| attribute.range.contains(&position.to_offset(&snapshot)))?;
        let mut sources = component
            .sources
            .iter()
            .filter(|source| source.kind == SourceKind::Manifest)
            .map(|source| source.path.clone())
            .collect::<BTreeSet<_>>();
        for (_, visible_variant) in selected.modules().filter(|(candidate, _)| {
            selected
                .visible_modules(&module.path, component.scope)
                .contains(&candidate.path)
        }) {
            for visible_component in visible_variant
                .components
                .iter()
                .filter(|component| component.scope == SourceScope::Main)
            {
                sources.extend(
                    visible_component
                        .sources
                        .iter()
                        .filter(|source| source.kind == SourceKind::Manifest)
                        .map(|source| source.path.clone()),
                );
            }
        }
        sources.extend(
            resources
                .dependencies
                .iter()
                .filter_map(|dependency| dependency.manifest.clone()),
        );
        let token = self.android_model.token();
        let filesystem = self.fs.clone();
        let origin = Location {
            buffer: buffer.clone(),
            range: snapshot.anchor_before(attribute.range.start)
                ..snapshot.anchor_after(attribute.range.end),
        };
        Some(cx.spawn(async move |project, cx| {
            let merged_metadata = canonical_resource_metadata(&filesystem, &path).await?;
            let mut links = Vec::new();
            let mut observed = Vec::new();
            let mut text = "Manifest source declarations. Matching a merged value does not establish the merger winner; tools directives and placeholders need the AGP merge report.\n".to_owned();
            for path in sources {
                let Some(metadata) = canonical_resource_metadata(&filesystem, &path).await? else { continue; };
                ensure!(metadata.2 <= 1024 * 1024, "Manifest source exceeds size limit");
                let source = project.update(cx, |project, cx| project.open_local_buffer(&path, cx))?.await?;
                let source_snapshot = cx.update(|cx| source.read(cx).snapshot());
                ensure!(source_snapshot.len() <= 1024 * 1024, "Manifest source buffer exceeds size limit");
                for candidate in resources::manifest_attributes(&source_snapshot.text())? {
                    if candidate.identity != attribute.identity || candidate.name != attribute.name { continue; }
                    text.push_str(&format!("{}: {} = {} ({})\n", path.display(), candidate.name, candidate.value, if candidate.value == attribute.value { "matches merged value" } else { "different source value" }));
                    links.push(LocationLink { origin: Some(origin.clone()), target: Location { buffer: source.clone(), range: source_snapshot.anchor_before(candidate.range.start)..source_snapshot.anchor_after(candidate.range.end) } });
                }
                observed.push((source, source_snapshot, path, metadata));
            }
            if canonical_resource_metadata(&filesystem, &path).await? != merged_metadata {
                return Ok((Vec::new(), Vec::new()));
            }
            for (_, _, source_path, metadata) in &observed {
                if canonical_resource_metadata(&filesystem, source_path).await?.as_ref() != Some(metadata) {
                    return Ok((Vec::new(), Vec::new()));
                }
            }
            let current = project.read_with(cx, |project, cx| {
                project.android_model.is_current(&token)
                    && !crate::trusted_worktrees::TrustedWorktrees::has_restricted_worktrees(&project.worktree_store(), cx)
                    && origin.buffer.read(cx).version() == *snapshot.version()
                    && origin.buffer.read(cx).file().is_some_and(|file| !file.disk_state().is_deleted() && project.android_file_path(file.as_ref(), cx) == path)
                    && observed.iter().all(|(buffer, snapshot, path, _)| buffer.read(cx).version() == *snapshot.version()
                        && buffer.read(cx).file().is_some_and(|file| !file.disk_state().is_deleted() && project.android_file_path(file.as_ref(), cx) == *path))
            })?;
            if !current { return Ok((Vec::new(), Vec::new())); }
            Ok((links, vec![Hover { contents: vec![HoverBlock { text, kind: HoverBlockKind::Code { language: "text".into() } }], range: Some(origin.range), language: None }]))
        }))
    }
}

async fn canonical_resource_metadata(
    filesystem: &Arc<dyn fs::Fs>,
    path: &std::path::Path,
) -> Result<Option<(u64, fs::MTime, u64)>> {
    let Some(metadata) = filesystem.metadata(path).await? else {
        return Ok(None);
    };
    ensure!(
        !metadata.is_symlink && !metadata.is_dir,
        "Resource metadata is not a regular file: {}",
        path.display()
    );
    ensure!(
        filesystem.canonicalize(path).await? == path,
        "Resource metadata path changed through a symlink: {}",
        path.display()
    );
    Ok(Some((metadata.inode, metadata.mtime, metadata.len)))
}

fn resource_public_symbols(text: &str) -> Result<Option<BTreeSet<(String, String)>>> {
    resource_public_symbols_limited(text, usize::MAX)
}

fn resource_public_symbols_limited(
    text: &str,
    limit: usize,
) -> Result<Option<BTreeSet<(String, String)>>> {
    let mut reader = Reader::from_str(text);
    let mut depth = 0;
    let mut resources = false;
    let mut restricted = false;
    loop {
        let event = reader.read_event()?;
        match event {
            Event::Start(ref element) | Event::Empty(ref element) => {
                if depth == 0 {
                    resources = element.name().as_ref() == b"resources";
                }
                restricted |= resources && depth == 1 && element.name().as_ref() == b"public";
                if matches!(event, Event::Start(_)) {
                    depth += 1;
                }
            }
            Event::End(_) => {
                ensure!(depth > 0, "Unexpected public resource closing tag");
                depth -= 1;
            }
            Event::Eof => {
                ensure!(depth == 0, "Unclosed public resource XML");
                return if restricted {
                    resources::public_symbols_limited(text, limit).map(Some)
                } else {
                    Ok(None)
                };
            }
            _ => {}
        }
    }
}

fn indexable_file(path: &std::path::Path, roots: &[ResourceRoot], sources: bool) -> bool {
    roots.iter().any(|root| {
        path.starts_with(&root.path)
            && (root.kind == SourceKind::Resources
                || path.extension().is_some_and(|extension| {
                    extension == "xml" || sources && (extension == "kt" || extension == "java")
                }))
    })
}

#[cfg(test)]
mod resource_worker_budget_tests {
    use super::*;
    use std::fmt::Write as _;

    fn repeated_ids(count: usize) -> (ResourceScan, Symbol) {
        let symbol = Symbol {
            namespace: "app".into(),
            kind: "id".into(),
            name: "view".into(),
        };
        let mut scan = ResourceScan::default();
        for index in 0..count {
            scan.declarations.push(Declaration {
                symbol: symbol.clone(),
                path: PathBuf::from(format!("/app/res/layout/{index}.xml")),
                qualifier: String::new(),
                priority: 0,
                generated: false,
                external: false,
                range: 0..1,
                name_range: Some(0..1),
                xml_name: "view".into(),
            });
        }
        scan.symbol_declarations
            .insert(symbol.clone(), (0..count).collect());
        (scan, symbol)
    }

    #[test]
    fn checked_documentation_rejects_growth_before_allocating_an_oversized_value() {
        let mut text = ResourceText::new(8);
        text.write_str("abc").expect("Initial text");
        let capacity = text.text.capacity();
        assert!(text.write_str(&"x".repeat(1_000_000)).is_err());
        assert_eq!(text.text, "abc");
        assert_eq!(text.text.capacity(), capacity);
        assert!(capacity <= 8);
    }

    #[test]
    fn repeated_id_provenance_and_combined_hover_share_one_output_budget() -> Result<()> {
        let (scan, symbol) = repeated_ids(1_000);
        let mut checkpoint = || Ok(());
        let mut control =
            resources::ResourceParserControl::new(&mut checkpoint, MAX_RESOURCE_PARSER_WORK);
        let mut text = ResourceText::new(128);
        let error = append_provenance(&mut text, &scan, &symbol, &mut control).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("documentation exceeds text size limit")
        );
        assert!(text.text.len() <= 128);
        assert!(text.text.capacity() <= 128);
        assert!(resource_documentation(&scan, &[symbol.clone()], 128, &mut control).is_err());
        let (scan, symbol) = repeated_ids(1);
        let single = resource_documentation(&scan, &[symbol.clone()], 1_024, &mut control)?;
        assert_eq!(
            single,
            "app: id/view\nSelected: /app/res/layout/0.xml [default]\n"
        );
        assert!(
            resource_documentation(&scan, &[symbol.clone(), symbol], single.len(), &mut control)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn receiver_drop_interrupts_provenance_and_single_document_parsers() {
        for operation in ["hover", "references", "rename"] {
            let (sender, receiver) = async_channel::bounded(1);
            let mut receiver = Some(receiver);
            let mut checkpoints = 0;
            let mut checkpoint = || {
                checkpoints += 1;
                if checkpoints == 5 {
                    receiver.take();
                }
                resource_worker_checkpoint(&sender)
            };
            let mut control =
                resources::ResourceParserControl::new(&mut checkpoint, MAX_RESOURCE_PARSER_WORK);
            let error = if operation == "hover" {
                let (scan, symbol) = repeated_ids(1_000);
                resource_documentation(&scan, &[symbol], MAX_RESOURCE_BYTES as usize, &mut control)
                    .unwrap_err()
            } else {
                let text = format!(
                    "/*{}*/\nval value = library.R.string.title",
                    "x".repeat(1_000_000)
                );
                if operation == "rename" {
                    resources::code_rename_analysis_controlled(
                        &text,
                        MAX_RESOURCE_SYMBOLS,
                        &mut control,
                    )
                    .unwrap_err()
                } else {
                    resources::references_controlled(
                        &text,
                        false,
                        MAX_RESOURCE_SYMBOLS,
                        &mut control,
                    )
                    .unwrap_err()
                }
            };
            assert!(
                error.to_string().contains("resource request cancelled"),
                "{operation}"
            );
            assert_eq!(checkpoints, 5, "{operation}");
            assert!(receiver.is_none(), "{operation}");
        }
    }
}

#[cfg(test)]
mod resource_visibility_tests {
    use super::resource_public_symbols;

    #[test]
    fn distinguishes_unrestricted_resources_from_an_empty_public_declaration() -> anyhow::Result<()>
    {
        assert!(
            resource_public_symbols("<resources><string name='title'>Title</string></resources>")?
                .is_none()
        );
        assert!(
            resource_public_symbols("<resources><public/></resources>")?
                .is_some_and(|public| public.is_empty())
        );
        assert!(resource_public_symbols("<resources><!-- <public/> --></resources>")?.is_none());
        let public = resource_public_symbols(
            "<resources><public type='style' name='Theme.App'/></resources>",
        )?
        .expect("An explicit public declaration restricts visibility");
        assert!(public.contains(&("style".into(), "Theme_App".into())));
        assert!(resource_public_symbols("<resources><public/>").is_err());
        Ok(())
    }
}

#[cfg(test)]
mod resource_scan_delivery_tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    async fn closed_progress_receiver_cancels_a_backpressured_resource_scan(
        cx: &mut TestAppContext,
    ) {
        let filesystem: Arc<dyn fs::Fs> = fs::FakeFs::new(cx.executor());
        let (sender, receiver) = async_channel::bounded(1);
        sender.try_send(()).expect("Fill the only progress slot");
        let task = cx.executor().spawn(scan_resource_files(
            filesystem,
            Vec::new(),
            BTreeSet::from([PathBuf::from("/unreadable-resource.xml")]),
            BTreeMap::new(),
            None,
            false,
            sender,
        ));
        cx.run_until_parked();
        assert_eq!(receiver.len(), 1);
        drop(receiver);
        let error = task
            .await
            .err()
            .expect("Receiver closure must cancel the scan");
        assert!(
            error
                .to_string()
                .contains("Android resource scan cancelled")
        );
    }

    #[gpui::test]
    async fn scanner_preflights_model_namespace_copies_before_non_strict_success(
        cx: &mut TestAppContext,
    ) {
        let filesystem = fs::FakeFs::new(cx.executor());
        filesystem.insert_tree("/bounded-res", serde_json::json!({
            "values": {"strings.xml": "<resources><string name='first'>First</string><string name='second'>Second</string></resources>"},
            "layout": {"screen.xml": "<view first='@+id/first' second='@+id/second'/>"}
        })).await;
        let namespace: Arc<str> = "long_namespace".repeat(512).into();
        let root = ResourceRoot {
            path: "/bounded-res".into(),
            namespace: namespace.clone(),
            priority: 0,
            generated: false,
            external: false,
            kind: SourceKind::Resources,
            domain: ":app/debug".into(),
        };
        for (path, expected) in [
            ("/bounded-res/values/strings.xml", 2),
            ("/bounded-res/layout/screen.xml", 3),
        ] {
            let (sender, receiver) = async_channel::bounded(1);
            let error = scan_resource_files_bounded(
                filesystem.clone(),
                vec![root.clone()],
                BTreeSet::from([PathBuf::from(path)]),
                BTreeMap::new(),
                None,
                false,
                ResourceScanDelivery {
                    progress: sender,
                    owned_byte_limit: 1024,
                },
            )
            .await
            .err()
            .expect("A small owned budget must reject namespace amplification");
            assert!(
                error.is::<resources::ResourceLimitExceeded>(),
                "{path}: {error:#}"
            );
            assert!(
                error.to_string().contains("owned byte limit"),
                "{path}: {error:#}"
            );
            assert_eq!(receiver.len(), 1);
            let (sender, receiver) = async_channel::bounded(1);
            let full = scan_resource_files_bounded(
                filesystem.clone(),
                vec![root.clone()],
                BTreeSet::from([PathBuf::from(path)]),
                BTreeMap::new(),
                None,
                false,
                ResourceScanDelivery {
                    progress: sender,
                    owned_byte_limit: 256 * 1024,
                },
            )
            .await
            .expect("A sufficient owned budget must retain every declaration");
            assert_eq!(full.declarations.len(), expected, "{path}");
            assert!(
                full.declarations
                    .iter()
                    .all(|declaration| declaration.symbol.namespace == namespace.as_ref()),
                "{path}"
            );
            assert_eq!(receiver.len(), 1);
        }
    }
}

#[cfg(test)]
mod resource_namespace_owned_tests {
    use super::*;

    #[test]
    fn component_source_roots_share_one_namespace_allocation() -> Result<()> {
        let namespace = "long_namespace".repeat(512);
        let sources = (0..128)
            .map(|index| {
                serde_json::json!({
                    "path":format!("/shared/app/res/{index}"), "kind":"resources", "generated":false
                })
            })
            .collect::<Vec<_>>();
        let model: android_tools::project_model::ProjectModel = serde_json::from_value(
            serde_json::json!({
                "version":1,"root":"/shared","diagnostics":[],"modules":[{
                    "path":":app","directory":"/shared/app","kind":"application","namespace":namespace,
                    "variants":[{"name":"debug","components":[
                        {"name":"debug","scope":"main","dependencies":[],"sources":sources}
                    ]}]
                }]
            }),
        )?;
        let selected = Arc::new(model).select(android_tools::project_model::VariantId {
            module: ":app".into(),
            variant: "debug".into(),
        })?;
        let roots = component_roots(&selected, true, ":app", SourceScope::Main);
        assert_eq!(roots.len(), 128);
        assert_eq!(roots[0].namespace.as_ref(), namespace);
        assert!(
            roots
                .iter()
                .all(|root| Arc::ptr_eq(&root.namespace, &roots[0].namespace))
        );
        assert!(
            roots
                .iter()
                .all(|root| Arc::ptr_eq(&root.domain, &roots[0].domain))
        );
        Ok(())
    }

    #[test]
    fn namespace_lookup_checks_owned_keys_before_inserting_them() -> Result<()> {
        let symbol = Symbol {
            namespace: "app".into(),
            kind: "string".into(),
            name: "large_name".repeat(128),
        };
        let mut scan = ResourceScan::default();
        scan.symbol_declarations.insert(symbol.clone(), Vec::new());
        let mut index = ResourceSourceIndex {
            scan: Arc::new(scan),
            roots: Vec::new(),
            files: BTreeSet::new(),
            documents: Vec::new(),
            namespace_counts: BTreeMap::new(),
        };
        let mut checkpoint = || Ok(());
        let mut control =
            resources::ResourceParserControl::new(&mut checkpoint, MAX_RESOURCE_PARSER_WORK)
                .with_owned_byte_limit(1);
        let error = index.index_namespaces(&mut control).unwrap_err();
        assert!(error.is::<resources::ResourceLimitExceeded>());
        assert!(index.namespace_counts.is_empty());
        let mut checkpoint = || Ok(());
        let mut control =
            resources::ResourceParserControl::new(&mut checkpoint, MAX_RESOURCE_PARSER_WORK)
                .with_owned_byte_limit(4096);
        index.index_namespaces(&mut control)?;
        assert_eq!(
            index.namespace_counts.get(&(symbol.kind, symbol.name)),
            Some(&1)
        );
        Ok(())
    }
}
