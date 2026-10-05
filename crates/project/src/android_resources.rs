use crate::{LocationLink, Project, ProjectPath};
use anyhow::Result;
use futures::StreamExt as _;
use gpui::{Context, Entity, Task};
use language::{Buffer, Location, PointUtf16, ToOffset};
use quick_xml::{Reader, events::Event};
use regex::Regex;
use std::{ops::Range, sync::LazyLock};
use util::ResultExt;

static RESOURCE: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(
        r"\b(?:(?<namespace>[a-zA-Z_]\w*(?:\.[a-zA-Z_]\w*)*)\.)?R\.(?<kind>\w+)\.(?<name>\w+)\b|@(?<xml_kind>\w+)/(?<xml_name>\w+)\b",
    )
});
static NAMESPACE: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r#"\bnamespace\s*(?:=\s*|\(\s*)?["']([^"']+)["']"#));
static IMPORT: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*import\s+([\w.]+)\.R\s*;?\s*$"));

impl Project {
    pub(crate) fn android_resource_model_unavailable(
        &self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        cx: &App,
    ) -> bool {
        let Some(root) = self.android_model.root() else {
            return false;
        };
        if self.android_model.selected.is_some() || !self.is_local() {
            return false;
        }
        let snapshot = buffer.read(cx).snapshot();
        let Some(file) = snapshot.file() else {
            return false;
        };
        let path = self.android_file_path(file.as_ref(), cx);
        if !path.starts_with(root) {
            return false;
        }
        let xml = path.extension().is_some_and(|extension| extension == "xml");
        if !xml
            && path
                .extension()
                .is_none_or(|extension| extension != "kt" && extension != "java")
        {
            return false;
        }
        let text = snapshot.text();
        let offset = position.to_offset(&snapshot);
        resources::completion_reference(&text, offset, xml).is_some()
            || resources::references(&text, xml).is_ok_and(|references| {
                references.iter().any(|reference| {
                    reference.range.start <= offset && offset <= reference.range.end
                })
            })
            || xml
                && resources::declarations(&text, "").is_ok_and(|declarations| {
                    declarations
                        .iter()
                        .any(|(_, _, _, range)| range.start <= offset && offset <= range.end)
                })
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
        let snapshot = buffer.read(cx).snapshot();
        let file = snapshot.file()?;
        let path = file.path().as_unix_str();
        if !path.ends_with(".kt") && !path.ends_with(".java") && !path.ends_with(".xml") {
            return None;
        }
        let offset = position.to_offset(&snapshot);
        let mut node = snapshot.syntax_ancestor(offset..offset);
        while let Some(ancestor) = node {
            if ancestor.kind().contains("comment")
                || (!path.ends_with(".xml") && ancestor.kind().contains("string"))
            {
                return None;
            }
            node = ancestor.parent();
        }
        let text = snapshot.text();
        let captures = RESOURCE
            .as_ref()
            .ok()?
            .captures_iter(&text)
            .find(|captures| {
                captures
                    .get(0)
                    .is_some_and(|found| found.range().contains(&offset))
            })?;
        let found = captures.get(0)?;
        let kind = captures
            .name("kind")
            .or_else(|| captures.name("xml_kind"))?
            .as_str()
            .to_owned();
        let name = captures
            .name("name")
            .or_else(|| captures.name("xml_name"))?
            .as_str()
            .to_owned();
        let namespace = captures
            .name("namespace")
            .map(|value| value.as_str().to_owned())
            .or_else(|| {
                IMPORT
                    .as_ref()
                    .ok()?
                    .captures(&text)?
                    .get(1)
                    .map(|value| value.as_str().to_owned())
            });
        if namespace.as_deref() == Some("android") {
            return None;
        }
        let worktree_id = file.worktree_id(cx);
        let worktree = self.worktree_for_id(worktree_id, cx)?.read(cx).snapshot();
        let token = self.android_model.token();
        if self.android_model.root().is_some() && self.android_model.selected.is_none() {
            return Some(Task::ready(Ok(Vec::new())));
        }
        let (mut paths, build, resource_roots) = if self.android_model.model.is_some() {
            let selected = self.android_model.selected.as_ref()?;
            let model_root = if self.android_model.root() == Some(worktree.abs_path().as_ref()) {
                selected.model.root.as_path()
            } else {
                worktree.abs_path().as_ref()
            };
            let absolute = model_root.join(file.path().as_std_path());
            let (owner, component) = selected.modules().find_map(|(module, variant)| {
                variant
                    .components
                    .iter()
                    .find(|component| {
                        component
                            .sources
                            .iter()
                            .any(|source| absolute.starts_with(&source.path))
                    })
                    .map(|component| (module, component))
            })?;
            let scope = component.scope;
            let namespace = namespace
                .as_deref()
                .or(component.namespace.as_deref())
                .or(owner.namespace.as_deref());
            let visible = selected.visible_modules(&owner.path, scope);
            let roots = selected
                .modules()
                .filter(|(module, _)| visible.contains(&module.path))
                .flat_map(|(module, variant)| {
                    variant.components.iter().filter(move |component| {
                        (component.scope == android_tools::project_model::SourceScope::Main
                            || (module.path == owner.path && component.scope == scope))
                            && component
                                .namespace
                                .as_deref()
                                .or(module.namespace.as_deref())
                                == namespace
                    })
                })
                .flat_map(|component| &component.sources)
                .filter(|source| source.kind == android_tools::project_model::SourceKind::Resources)
                .collect::<Vec<_>>();
            let roots = roots
                .iter()
                .map(|source| source.path.clone())
                .collect::<Vec<_>>();
            (Vec::new(), None, Some((model_root.to_path_buf(), roots)))
        } else {
            let module = path
                .split_once("/src/")
                .map(|(module, _)| module)
                .or_else(|| path.starts_with("src/").then_some(""))?;
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
            let build = ["build.gradle.kts", "build.gradle"]
                .into_iter()
                .find_map(|name| {
                    let path_text = format!("{build_prefix}{name}");
                    let path = util::rel_path::RelPath::from_unix_str(&path_text).ok()?;
                    worktree
                        .entry_for_path(path)
                        .map(|entry| entry.path.clone())
                })?;
            let paths = worktree
                .files(false, 0)
                .filter_map(|entry| {
                    let relative = entry.path.as_unix_str().strip_prefix(&prefix)?;
                    let (_, resource) = relative.split_once("/res/")?;
                    let (directory, filename) = resource.split_once('/')?;
                    if filename.contains('/') {
                        return None;
                    }
                    let folder_kind = directory.split('-').next()?;
                    ((folder_kind == "values" && filename.ends_with(".xml"))
                        || (folder_kind == kind
                            && filename.ends_with(".xml")
                            && filename.split('.').next() == Some(name.as_str())))
                    .then_some((entry.path.clone(), folder_kind == "values"))
                })
                .collect::<Vec<_>>();
            (paths, Some(build), None)
        };
        if paths.is_empty() && resource_roots.is_none() {
            return None;
        }
        let origin = Location {
            buffer: buffer.clone(),
            range: snapshot.anchor_before(found.start())..snapshot.anchor_after(found.end()),
        };
        let filesystem = self.fs.clone();
        let path_style = self.path_style(cx);
        Some(cx.spawn(async move |project, cx| {
            if let Some((model_root, roots)) = resource_roots {
                for root in roots {
                    if !filesystem.is_dir(&root).await {
                        continue;
                    }
                    let mut folders = filesystem.read_dir(&root).await?;
                    while let Some(folder) = folders.next().await {
                        let folder = folder?;
                        let Some(folder_kind) = folder
                            .file_name()
                            .and_then(|name| name.to_str())
                            .and_then(|name| name.split('-').next())
                        else {
                            continue;
                        };
                        if (folder_kind != "values" && folder_kind != kind)
                            || !filesystem.is_dir(&folder).await
                        {
                            continue;
                        }
                        let mut files = filesystem.read_dir(&folder).await?;
                        while let Some(file) = files.next().await {
                            let file = file?;
                            if file.extension().is_none_or(|extension| extension != "xml")
                                || (folder_kind != "values"
                                    && file.file_stem().is_none_or(|stem| stem != name.as_str()))
                                || !filesystem.is_file(&file).await
                            {
                                continue;
                            }
                            if let Ok(relative) = file.strip_prefix(&model_root) {
                                paths.push((
                                    util::rel_path::RelPath::new(relative, path_style)?.into_arc(),
                                    folder_kind == "values",
                                ));
                            }
                        }
                    }
                }
                paths.sort();
                paths.dedup();
            }
            if let Some(namespace) = namespace
                && let Some(build) = build
            {
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
                let matches = cx.update(|cx| {
                    let text = build.read(cx).text();
                    NAMESPACE
                        .as_ref()
                        .ok()
                        .and_then(|expression| expression.captures(&text))
                        .and_then(|captures| {
                            captures.get(1).map(|value| value.as_str() == namespace)
                        })
                        .unwrap_or(false)
                });
                if !matches {
                    return Ok(Vec::new());
                }
            }
            let mut locations = Vec::new();
            // Keep locale/qualifier alternatives; only the selected component roots participate.
            for (path, values) in paths {
                let buffer = project
                    .update(cx, |project, cx| {
                        project.open_buffer(ProjectPath { worktree_id, path }, cx)
                    })?
                    .await?;
                let snapshot = cx.update(|cx| buffer.read(cx).snapshot());
                let ranges = if values {
                    match value_ranges(&snapshot.text(), &kind, &name).log_err() {
                        Some(ranges) => ranges,
                        None => continue,
                    }
                } else {
                    vec![0..0]
                };
                locations.extend(ranges.into_iter().map(|range| LocationLink {
                    origin: Some(origin.clone()),
                    target: Location {
                        buffer: buffer.clone(),
                        range: snapshot.anchor_before(range.start)
                            ..snapshot.anchor_after(range.end),
                    },
                }));
            }
            if !project.read_with(cx, |project, _| project.android_model.is_current(&token))? {
                return Ok(Vec::new());
            }
            Ok(locations)
        }))
    }
}

fn value_ranges(text: &str, kind: &str, name: &str) -> Result<Vec<Range<usize>>> {
    let mut reader = Reader::from_str(text);
    let mut ranges = Vec::new();
    let mut depth = 0;
    let mut resources = false;
    loop {
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
                        ranges.push(end.saturating_sub(length)..end);
                    }
                }
                if matches!(event, Event::Start(_)) {
                    depth += 1;
                }
            }
            Event::End(_) => {
                anyhow::ensure!(depth > 0, "Unexpected Android resource closing tag");
                depth -= 1;
            }
            Event::Eof => {
                anyhow::ensure!(depth == 0, "Unclosed Android resource XML");
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

#[derive(Clone)]
struct ResourceRoot {
    path: PathBuf,
    namespace: String,
    priority: usize,
    generated: bool,
    external: bool,
    kind: SourceKind,
    domain: String,
}

struct ResourceDocument {
    buffer: Entity<Buffer>,
    snapshot: BufferSnapshot,
    path: PathBuf,
    namespace: String,
    generated: bool,
    disk_text: Option<String>,
    disk_metadata: Option<(u64, fs::MTime, u64)>,
}

struct ResourceIndex {
    declarations: Vec<Declaration>,
    documents: BTreeMap<PathBuf, ResourceDocument>,
    token: ModelToken,
    roots: Vec<ResourceRoot>,
    files: BTreeSet<PathBuf>,
    conflicts: BTreeSet<Symbol>,
    diagnostics: Vec<String>,
    public_symbols: BTreeMap<String, BTreeSet<(String, String)>>,
    supporting_metadata: BTreeMap<PathBuf, Option<(u64, fs::MTime, u64)>>,
}

struct ResourceQuery {
    selected: Arc<SelectedProject>,
    token: ModelToken,
    roots: Vec<ResourceRoot>,
    namespace: String,
    component_namespace: String,
    xml: bool,
    reference: Reference,
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
                    .unwrap_or_default()
                    .to_owned();
                let domain = format!("{}/{}", module.path, component.name);
                let model = selected.model.resource_models.get(&domain);
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
                            namespace: external.namespace.clone(),
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

    fn android_query(
        &self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        cx: &App,
    ) -> Option<ResourceQuery> {
        self.android_query_at(buffer, position, None, cx)
    }

    fn android_query_at(
        &self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        reference: Option<Reference>,
        cx: &App,
    ) -> Option<ResourceQuery> {
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
        let namespace = component
            .namespace
            .as_deref()
            .or(module.namespace.as_deref())
            .unwrap_or_default();
        let component_namespace = namespace.to_owned();
        let namespace = if xml {
            namespace.to_owned()
        } else {
            resources::code_namespace(&snapshot.text(), namespace)
        };
        let offset = position.to_offset(&snapshot);
        let reference = reference.or_else(|| {
            let mut references = resources::references(&snapshot.text(), xml).ok()?;
            if xml {
                for (kind, name, range, name_range) in
                    resources::declarations(&snapshot.text(), &namespace).ok()?
                {
                    references.push(Reference {
                        namespace: None,
                        kind,
                        name: name.replace('.', "_"),
                        range,
                        name_range,
                        declaration: true,
                    });
                }
            }
            references.into_iter().find(|reference| {
                (reference.name_range.start <= offset && offset <= reference.name_range.end)
                    || reference.range.contains(&offset)
            })
        })?;
        let origin = Location {
            buffer: buffer.clone(),
            range: snapshot.anchor_before(reference.range.start)
                ..snapshot.anchor_after(reference.range.end),
        };
        let transitive = selected
            .model
            .resource_models
            .get(&format!("{}/{}", module.path, component.name))
            .is_some_and(|model| !model.non_transitive_r);
        let mut roots = component_roots(&selected, false, &module.path, component.scope);
        // SDK XML exceeds 32 MiB on API 35. Keep that scan out of ordinary app
        // requests, which cannot refer to the framework without its namespace.
        if reference.namespace.as_deref() != Some("android") {
            roots.retain(|root| root.namespace != "android");
        }
        Some(ResourceQuery {
            roots,
            namespace,
            component_namespace,
            xml,
            reference,
            origin,
            source_snapshot: snapshot,
            transitive,
            source_path: path,
            selected,
            token: self.android_model.token(),
        })
    }

    pub(crate) fn android_resource_definitions(
        &mut self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<Vec<LocationLink>>>> {
        if self.android_model.root().is_none() {
            return self.legacy_android_resource_definitions(buffer, position, cx);
        }
        if self.android_resource_model_unavailable(buffer, position, cx) {
            return Some(Task::ready(Ok(Vec::new())));
        }
        let query = self.android_query(buffer, position, cx)?;
        Some(cx.spawn(async move |project, cx| {
            let index = build_index(
                &project,
                query.roots.clone(),
                query.token.clone(),
                false,
                cx,
            )
            .await?;
            let symbols = query_symbols(&query, &index);
            let mut locations = Vec::new();
            for symbol in symbols {
                for declaration in resources::resolve(&index.declarations, &symbol).winners {
                    if declaration
                        .path
                        .extension()
                        .is_none_or(|extension| extension != "xml")
                    {
                        continue;
                    }
                    let buffer = if let Some(document) = index.documents.get(&declaration.path) {
                        document.buffer.clone()
                    } else {
                        project
                            .update(cx, |project, cx| {
                                project.open_local_buffer(&declaration.path, cx)
                            })?
                            .await?
                    };
                    let snapshot = cx.update(|cx| buffer.read(cx).snapshot());
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
                return Ok(Vec::new());
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
            Ok(locations)
        }))
    }

    pub fn android_resource_file_definitions(
        &self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<Vec<PathBuf>>>> {
        let query = self.android_query(buffer, position, cx)?;
        if !matches!(query.reference.kind.as_str(), "drawable" | "mipmap") {
            return None;
        }
        Some(cx.spawn(async move |project, cx| {
            let index = build_index(
                &project,
                query.roots.clone(),
                query.token.clone(),
                false,
                cx,
            )
            .await?;
            let paths = query_symbols(&query, &index)
                .iter()
                .flat_map(|symbol| resources::resolve(&index.declarations, symbol).winners)
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
    ) -> Option<Task<Result<Vec<Hover>>>> {
        if self.android_resource_model_unavailable(buffer, position, cx) {
            return Some(Task::ready(Ok(Vec::new())));
        }
        let query = self.android_query(buffer, position, cx)?;
        Some(cx.spawn(async move |project, cx| {
            let index = build_index(
                &project,
                query.roots.clone(),
                query.token.clone(),
                false,
                cx,
            )
            .await?;
            let text = query_symbols(&query, &index)
                .iter()
                .map(|symbol| provenance(&index, symbol))
                .collect::<Vec<_>>()
                .join("\n");
            if text.is_empty() || !query_current(&project, &query, &index, cx).await? {
                return Ok(Vec::new());
            }
            Ok(vec![Hover {
                contents: vec![HoverBlock {
                    text,
                    kind: HoverBlockKind::Code {
                        language: "text".into(),
                    },
                }],
                range: Some(query.origin.range),
                language: None,
            }])
        }))
    }

    pub(crate) fn android_resource_references(
        &self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<Vec<Location>>>> {
        if self.android_resource_model_unavailable(buffer, position, cx) {
            return Some(Task::ready(Ok(Vec::new())));
        }
        let query = self.android_query(buffer, position, cx)?;
        let roots = component_roots(&query.selected, true, "", SourceScope::Main);
        Some(cx.spawn(async move |project, cx| {
            let selected_index = build_index(
                &project,
                query.roots.clone(),
                query.token.clone(),
                false,
                cx,
            )
            .await?;
            let symbols = query_symbols(&query, &selected_index);
            let index = build_index(&project, roots, query.token.clone(), true, cx).await?;
            let mut locations = Vec::new();
            for document in index.documents.values() {
                for reference in document_references(document)? {
                    if symbols
                        .iter()
                        .any(|symbol| reference_matches(document, &reference, &index, symbol))
                    {
                        locations.push(Location {
                            buffer: document.buffer.clone(),
                            range: document.snapshot.anchor_before(reference.name_range.start)
                                ..document.snapshot.anchor_after(reference.name_range.end),
                        });
                    }
                }
            }
            if !query_current(&project, &query, &index, cx).await? {
                return Ok(Vec::new());
            }
            Ok(locations)
        }))
    }

    pub(crate) fn android_resource_completions(
        &self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<CompletionResponse>>> {
        let snapshot = buffer.read(cx).snapshot();
        let xml = snapshot.file()?.path().as_unix_str().ends_with(".xml");
        let reference =
            resources::completion_reference(&snapshot.text(), position.to_offset(&snapshot), xml)?;
        let query = self.android_query_at(buffer, position, Some(reference), cx)?;
        Some(cx.spawn(async move |project, cx| {
            let index = build_index(
                &project,
                query.roots.clone(),
                query.token.clone(),
                false,
                cx,
            )
            .await?;
            let namespace = query
                .reference
                .namespace
                .as_deref()
                .unwrap_or(&query.namespace);
            let symbols = index
                .declarations
                .iter()
                .filter(|entry| {
                    entry.symbol.kind == query.reference.kind
                        && (entry.symbol.namespace == namespace
                            || query.xml
                                && query.reference.namespace.is_none()
                                && entry.symbol.namespace != "android")
                })
                .map(|entry| entry.symbol.clone())
                .collect::<BTreeSet<_>>();
            let mut completions = Vec::new();
            if query_current(&project, &query, &index, cx).await? {
                for symbol in symbols {
                    if symbol.namespace != query.namespace
                        && index
                            .public_symbols
                            .get(&symbol.namespace)
                            .is_some_and(|public| {
                                !public.contains(&(symbol.kind.clone(), symbol.name.clone()))
                            })
                    {
                        continue;
                    }
                    if !symbol.name.starts_with(&query.reference.name) {
                        continue;
                    }
                    completions.push(Completion {
                        replace_range: query
                            .source_snapshot
                            .anchor_before(query.reference.name_range.start)
                            ..query
                                .source_snapshot
                                .anchor_after(query.reference.name_range.end),
                        new_text: symbol.name.clone(),
                        label: CodeLabel::plain(symbol.name.clone(), None),
                        documentation: Some(
                            crate::lsp_store::CompletionDocumentation::MultiLinePlainText(
                                provenance(&index, &symbol).into(),
                            ),
                        ),
                        source: CompletionSource::Custom,
                        icon_path: None,
                        icon_color: None,
                        match_start: Some(
                            query
                                .source_snapshot
                                .anchor_before(query.reference.name_range.start),
                        ),
                        snippet_deduplication_key: None,
                        insert_text_mode: None,
                        confirm: None,
                        group: None,
                    });
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
        cx: &App,
    ) -> bool {
        self.android_resource_model_unavailable(buffer, position, cx)
            || self.android_query(buffer, position, cx).is_some()
    }

    pub(crate) fn android_prepare_resource_rename(
        &self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<PrepareRenameResponse>>> {
        if self.android_resource_model_unavailable(buffer, position, cx) {
            return Some(Task::ready(Err(anyhow::anyhow!(
                "Sync Android successfully and select a variant before renaming resources"
            ))));
        }
        let query = self.android_query(buffer, position, cx)?;
        let range = query
            .source_snapshot
            .anchor_before(query.reference.name_range.start)
            ..query
                .source_snapshot
                .anchor_after(query.reference.name_range.end);
        Some(Task::ready(Ok(PrepareRenameResponse::Success {
            range,
            language_server_id: None,
        })))
    }

    pub fn prepare_android_resource_rename(
        &self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        new_name: String,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<AndroidResourceRename>>> {
        if self.android_resource_model_unavailable(buffer, position, cx) {
            return Some(Task::ready(Err(anyhow::anyhow!(
                "Sync Android successfully and select a variant before renaming resources"
            ))));
        }
        let query = self.android_query(buffer, position, cx)?;
        let roots = component_roots(&query.selected, true, "", SourceScope::Main);
        Some(cx.spawn(async move |project, cx| {
            resources::valid_rename(&query.reference.kind, &query.reference.name, &new_name)?;
            let selected_index = build_index(&project, query.roots.clone(), query.token.clone(), false, cx).await?;
            let symbols = query_symbols(&query, &selected_index);
            ensure!(symbols.len() == 1, "Resource ownership is ambiguous or unavailable; sync and resolve the resource first");
            let symbol = symbols.first().context("Resource ownership is unavailable")?;
            ensure!(query.selected.model.resource_models.values().all(|model| model.non_transitive_r), "Resource rename with transitive R classes requires language-server support");
            let owner_modules = roots.iter().filter(|root| root.namespace == symbol.namespace && root.kind == SourceKind::Resources).filter_map(|root| root.domain.split('/').next()).collect::<BTreeSet<_>>();
            ensure!(owner_modules.len() == 1, "Multiple modules share this resource namespace; ownership cannot be proven safely");
            ensure!(query.roots.iter().filter(|root| root.namespace == symbol.namespace && !root.external).all(|root| query.selected.model.resource_models.contains_key(&root.domain)), "Sync Android resource overlay metadata before renaming");
            let resolution = resources::resolve(&selected_index.declarations, symbol);
            ensure!(selected_index.declarations.iter().filter(|entry| &entry.symbol == symbol).all(|entry| !entry.external), "A dependency also declares this resource identity; rename ownership cannot be proven safely");
            ensure!(resolution.conflicts.is_empty(), "Resource has equal-priority conflicts; resolve them before renaming");
            ensure!(resolution.winners.iter().all(|entry| !entry.generated && !entry.external), "Dependency, framework and generated resources are read-only for resource rename");
            let index = build_index(&project, roots, query.token.clone(), true, cx).await?;
            for path in &index.files {
                for resource_root in index.roots.iter().filter(|root| root.kind == SourceKind::Resources && root.namespace == symbol.namespace && path.strip_prefix(&root.path).is_ok_and(|relative| relative.components().count() == 2)) {
                    ensure!(!index.roots.iter().any(|root| path.starts_with(&root.path) && root.path.components().count() >= resource_root.path.components().count() && (root.kind != SourceKind::Resources || !path.strip_prefix(&root.path).is_ok_and(|relative| relative.components().count() == 2))), "Resource file in {} overlaps incompatible source roots; rename ownership cannot be proven safely", path.display());
                }
            }
            for declaration in selected_index.declarations.iter().chain(&index.declarations).filter(|entry| entry.symbol.kind == symbol.kind && entry.symbol.name == symbol.name) {
                let namespaces = index.roots.iter().filter(|root| {
                    root.kind == SourceKind::Resources
                        && declaration.path.strip_prefix(&root.path).is_ok_and(|relative| relative.components().count() == 2)
                }).map(|root| root.namespace.as_str()).collect::<BTreeSet<_>>();
                ensure!(!(namespaces.contains(symbol.namespace.as_str()) && namespaces.len() > 1), "Resource declaration in {} is shared by multiple namespaces; rename ownership cannot be proven safely", declaration.path.display());
            }
            ensure!(selected_index.declarations.iter().filter(|entry| &entry.symbol == symbol).all(|entry| index.declarations.iter().any(|candidate| &candidate.symbol == symbol && candidate.path == entry.path && candidate.name_range == entry.name_range)), "Resource declaration ownership changed across source roots; rename ownership cannot be proven safely");
            ensure!(!index.conflicts.contains(symbol), "Resource has conflicting declarations in a source set");
            ensure!(!index.declarations.iter().any(|entry| entry.symbol.namespace == symbol.namespace && entry.symbol.kind == symbol.kind && entry.symbol.name == new_name), "The new resource name already exists in a variant or qualifier");
            let declarations = index.declarations.iter().filter(|entry| &entry.symbol == symbol).collect::<Vec<_>>();
            ensure!(!declarations.is_empty(), "Resource declaration is not editable");
            ensure!(declarations.iter().all(|entry| entry.name_range.is_some() && !entry.generated && !entry.external && entry.xml_name == symbol.name), "File resources, generated resources, and normalized names cannot be safely renamed with a text transaction");
            let mut edits: BTreeMap<PathBuf, Vec<Range<usize>>> = BTreeMap::new();
            for declaration in declarations { if let Some(range) = &declaration.name_range { edits.entry(declaration.path.clone()).or_default().push(range.clone()); } }
            for document in index.documents.values() {
                let xml = document.path.extension().is_some_and(|extension| extension == "xml");
                let text = document.snapshot.text();
                if !xml {
                    ensure!(!resources::code_rename_has_unsupported_syntax(&text), "Unicode escapes, quoted identifiers or templates in {} require server-backed rename", document.path.display());
                }
                if !xml && (document.snapshot.text().contains(&symbol.name) || document.snapshot.text().contains("import")) {
                    ensure!(resources::rename_code_is_unambiguous(&document.snapshot.text()), "Ambiguous R binding or member import in {}; use the language server or remove the ambiguity before renaming", document.path.display());
                }
                ensure!(!resources::code_rename_has_unsupported_templates(&text, &symbol.kind, &symbol.name), "Kotlin interpolation in {} requires server-backed rename", document.path.display());
                if xml { ensure!(resources::xml_rename_is_unambiguous(&text, &symbol.kind, &symbol.name)?, "Data binding or escaped references in {} require server-backed rename", document.path.display()); }
                for reference in document_references(document)? {
                    if reference.kind == symbol.kind && reference.name == symbol.name {
                        if !xml && reference.namespace.is_none() {
                            let namespaces = index.roots.iter().filter(|root| matches!(root.kind, SourceKind::Java | SourceKind::Kotlin) && document.path.starts_with(&root.path)).map(|root| root.namespace.as_str()).collect::<BTreeSet<_>>();
                            ensure!(!(namespaces.contains(symbol.namespace.as_str()) && namespaces.len() > 1), "An implicit R reference in {} is shared by multiple namespaces; rename ownership cannot be proven safely", document.path.display());
                        }
                        if xml && reference.namespace.is_none() {
                            let namespaces = index.roots.iter().filter(|root| root.kind == SourceKind::Resources && document.path.strip_prefix(&root.path).is_ok_and(|relative| relative.components().count() == 2)).map(|root| root.namespace.as_str()).collect::<BTreeSet<_>>();
                            ensure!(!(namespaces.contains(symbol.namespace.as_str()) && namespaces.len() > 1), "An unqualified XML reference in {} is shared by multiple namespaces; rename ownership cannot be proven safely", document.path.display());
                        }
                        if xml && reference.namespace.is_none() && document.namespace != symbol.namespace {
                            anyhow::bail!("An unqualified cross-namespace XML reference in {} cannot be proven safe across all variants", document.path.display());
                        }
                        if reference_matches(document, &reference, &index, symbol) {
                            ensure!(!document.generated, "A generated reference in {} prevents a complete safe rename", document.path.display());
                            edits.entry(document.path.clone()).or_default().push(reference.name_range);
                        }
                    }
                }
            }
            ensure!(edits.len() <= 200 && edits.values().map(Vec::len).sum::<usize>() <= 2_000, "Resource rename exceeds review limit (200 files / 2,000 edits)");
            for ranges in edits.values_mut() { ranges.sort_by_key(|range| range.start); ranges.dedup(); }
            ensure!(query_current(&project, &query, &index, cx).await?, "Android model or source changed while preparing rename; try again");
            Ok(AndroidResourceRename { index, edits, new_name })
        }))
    }

    pub fn apply_android_resource_rename(
        &self,
        plan: AndroidResourceRename,
        cx: &mut Context<Self>,
    ) -> Task<Result<ProjectTransaction>> {
        let filesystem = self.fs.clone();
        cx.spawn(async move |project, cx| {
            let mut current_files = enumerate_files(&filesystem, &plan.index.roots, true).await?;
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
            for (path, metadata) in &plan.index.supporting_metadata {
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

fn query_symbols(query: &ResourceQuery, index: &ResourceIndex) -> Vec<Symbol> {
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
    if index
        .declarations
        .iter()
        .any(|entry| entry.symbol == symbol)
    {
        return vec![symbol];
    }
    let dependency_fallback = if query.xml {
        query.reference.namespace.is_none()
    } else {
        query.transitive
            && query
                .reference
                .namespace
                .as_deref()
                .is_none_or(|namespace| namespace == query.component_namespace)
    };
    // Transitive R adds dependency members to the owning component's R class. It
    // does not make an unrelated explicitly qualified R class an alias of it.
    if !dependency_fallback {
        return Vec::new();
    }
    index
        .declarations
        .iter()
        .filter(|entry| {
            entry.symbol.kind == symbol.kind
                && entry.symbol.name == symbol.name
                && entry.symbol.namespace != "android"
        })
        .map(|entry| entry.symbol.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn provenance(index: &ResourceIndex, symbol: &Symbol) -> String {
    let resolution = resources::resolve(&index.declarations, symbol);
    let mut text = format!("{}: {}/{}\n", symbol.namespace, symbol.kind, symbol.name);
    for diagnostic in &index.diagnostics {
        text.push_str(&format!("Incomplete index: {diagnostic}\n"));
    }
    for (label, entries) in [
        ("Selected", resolution.winners),
        ("Shadowed", resolution.shadowed),
        ("Conflict", resolution.conflicts),
    ] {
        for entry in entries {
            text.push_str(&format!(
                "{label}: {} [{}]{}\n",
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
            ));
        }
    }
    text
}

fn document_references(document: &ResourceDocument) -> Result<Vec<Reference>> {
    resources::references(
        &document.snapshot.text(),
        document
            .path
            .extension()
            .is_some_and(|extension| extension == "xml"),
    )
}

fn reference_matches(
    document: &ResourceDocument,
    reference: &Reference,
    index: &ResourceIndex,
    symbol: &Symbol,
) -> bool {
    if reference.kind != symbol.kind || reference.name != symbol.name {
        return false;
    }
    let namespace = reference
        .namespace
        .as_deref()
        .unwrap_or(&document.namespace);
    if namespace == symbol.namespace {
        return true;
    }
    if reference.namespace.is_none()
        && document
            .path
            .extension()
            .is_some_and(|extension| extension == "xml")
    {
        let local = Symbol {
            namespace: namespace.to_owned(),
            kind: reference.kind.clone(),
            name: reference.name.clone(),
        };
        return !index.declarations.iter().any(|entry| entry.symbol == local)
            && index
                .declarations
                .iter()
                .filter(|entry| {
                    entry.symbol.kind == reference.kind && entry.symbol.name == reference.name
                })
                .map(|entry| &entry.symbol.namespace)
                .collect::<BTreeSet<_>>()
                .len()
                == 1;
    }
    false
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
    let mut files = enumerate_files(&filesystem, &index.roots, sources).await?;
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
    for document in index.documents.values() {
        let metadata = filesystem
            .metadata(&document.path)
            .await?
            .map(|metadata| (metadata.inode, metadata.mtime, metadata.len));
        if metadata != document.disk_metadata {
            return Ok(false);
        }
    }
    for (path, metadata) in &index.supporting_metadata {
        if canonical_resource_metadata(&filesystem, path).await? != *metadata {
            return Ok(false);
        }
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

async fn build_index(
    project: &WeakEntity<Project>,
    roots: Vec<ResourceRoot>,
    token: ModelToken,
    sources: bool,
    cx: &mut AsyncApp,
) -> Result<ResourceIndex> {
    let filesystem = project.read_with(cx, |project, _| project.fs.clone())?;
    let mut files = enumerate_files(&filesystem, &roots, sources).await?;
    project.read_with(cx, |project, cx| {
        for buffer in project.opened_buffers(cx) {
            if let Some(file) = buffer.read(cx).file()
                && !file.disk_state().is_deleted()
            {
                let path = project.android_file_path(file.as_ref(), cx);
                if indexable_file(&path, &roots, sources) {
                    files.insert(path);
                }
            }
        }
    })?;
    let mut index = ResourceIndex {
        declarations: Vec::new(),
        documents: BTreeMap::new(),
        token,
        roots,
        files,
        conflicts: BTreeSet::new(),
        diagnostics: Vec::new(),
        public_symbols: BTreeMap::new(),
        supporting_metadata: BTreeMap::new(),
    };
    let mut bytes = 0;
    let mut conflict_groups: BTreeMap<(String, usize, String, Symbol), BTreeSet<(PathBuf, usize)>> =
        BTreeMap::new();
    for path in &index.files {
        let Some(root) = index
            .roots
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
        let snapshot = if xml || is_code {
            let metadata = filesystem.metadata(path).await?;
            bytes += metadata.map_or(0, |metadata| metadata.len);
            ensure!(
                bytes <= MAX_RESOURCE_BYTES,
                "Android resource scan exceeds text size limit"
            );
            let opened = project.read_with(cx, |project, cx| {
                project.opened_buffers(cx).into_iter().find(|buffer| {
                    buffer
                        .read(cx)
                        .file()
                        .is_some_and(|file| project.android_file_path(file.as_ref(), cx) == *path)
                })
            })?;
            let buffer = if let Some(buffer) = opened {
                buffer
            } else {
                project
                    .update(cx, |project, cx| project.open_local_buffer(path, cx))?
                    .await?
            };
            let snapshot = cx.update(|cx| buffer.read(cx).snapshot());
            bytes +=
                (snapshot.len() as u64).saturating_sub(metadata.map_or(0, |metadata| metadata.len));
            ensure!(
                bytes <= MAX_RESOURCE_BYTES,
                "Android resource buffer exceeds text size limit"
            );
            let namespace = if is_code {
                resources::code_namespace(&snapshot.text(), &root.namespace)
            } else {
                root.namespace.clone()
            };
            index.documents.insert(
                path.clone(),
                ResourceDocument {
                    buffer,
                    snapshot: snapshot.clone(),
                    path: path.clone(),
                    namespace,
                    generated: root.generated,
                    disk_metadata: metadata
                        .map(|metadata| (metadata.inode, metadata.mtime, metadata.len)),
                    disk_text: if sources && metadata.is_some() {
                        Some(filesystem.load(path).await?)
                    } else {
                        None
                    },
                },
            );
            Some(snapshot)
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
            if let Some(snapshot) = &snapshot {
                let declarations_in_file =
                    match resources::declarations(&snapshot.text(), &root.namespace) {
                        Ok(declarations) => declarations,
                        Err(error) if !sources => {
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
                for (kind, name, range, name_range) in declarations_in_file {
                    declarations.push(Declaration {
                        symbol: Symbol {
                            namespace: root.namespace.clone(),
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
            declarations.push(Declaration {
                symbol: Symbol {
                    namespace: root.namespace.clone(),
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
            if let Some(snapshot) = &snapshot {
                let references = match resources::references(&snapshot.text(), true) {
                    Ok(references) => references,
                    Err(error) if !sources => {
                        index
                            .diagnostics
                            .push(format!("{}: {error}", path.display()));
                        continue;
                    }
                    Err(error) => {
                        return Err(error.context(format!("Cannot safely scan {}", path.display())));
                    }
                };
                for reference in references
                    .into_iter()
                    .filter(|reference| reference.declaration)
                {
                    declarations.push(Declaration {
                        symbol: Symbol {
                            namespace: root.namespace.clone(),
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
            for matching_root in index
                .roots
                .iter()
                .filter(|candidate| candidate.path == root.path)
            {
                conflict_groups
                    .entry((
                        matching_root.domain.clone(),
                        matching_root.priority,
                        qualifier.into(),
                        declaration.symbol.clone(),
                    ))
                    .or_default()
                    .insert((path.clone(), declaration.range.start));
            }
            index.declarations.push(declaration);
        }
    }
    for document in index.documents.values().filter(|document| {
        document
            .path
            .extension()
            .is_some_and(|extension| extension == "xml")
            && index.roots.iter().any(|root| {
                root.kind == SourceKind::Resources
                    && document
                        .path
                        .strip_prefix(&root.path)
                        .is_ok_and(|relative| {
                            let mut parts = relative.components();
                            parts
                                .next()
                                .and_then(|part| part.as_os_str().to_str())
                                .is_some_and(|folder| folder.split('-').next() == Some("values"))
                                && parts.next().is_some()
                                && parts.next().is_none()
                        })
            })
    }) {
        let public = match resource_public_symbols(&document.snapshot.text()) {
            Ok(public) => public,
            Err(error) if !sources => {
                index
                    .diagnostics
                    .push(format!("{}: {error}", document.path.display()));
                continue;
            }
            Err(error) => {
                return Err(
                    error.context(format!("Cannot safely scan {}", document.path.display()))
                );
            }
        };
        if let Some(public) = public {
            index
                .public_symbols
                .entry(document.namespace.clone())
                .or_default()
                .extend(public);
        }
    }
    let selection = project.read_with(cx, |project, _| project.android_model.selected.clone())?;
    if let Some(selection) = selection {
        for (domain, model) in &selection.model.resource_models {
            if !index.roots.iter().any(|root| &root.domain == domain) {
                continue;
            }
            for dependency in model.dependencies.iter().filter(|dependency| {
                index.roots.iter().any(|root| {
                    root.external
                        && root.namespace == dependency.namespace
                        && root.path == dependency.path
                })
            }) {
                if let Some(path) = &dependency.public_resources {
                    let metadata = canonical_resource_metadata(&filesystem, path).await?;
                    index.supporting_metadata.insert(path.clone(), metadata);
                    let Some(metadata) = metadata else {
                        continue;
                    };
                    ensure!(
                        metadata.2 <= MAX_RESOURCE_BYTES,
                        "Public resource list exceeds size limit"
                    );
                    let text = filesystem.load(path).await?;
                    ensure!(
                        text.len() <= MAX_RESOURCE_BYTES as usize,
                        "Public resource list exceeds size limit"
                    );
                    ensure!(
                        canonical_resource_metadata(&filesystem, path).await? == Some(metadata),
                        "Public resource metadata changed while indexing: {}",
                        path.display()
                    );
                    let public = text
                        .lines()
                        .filter_map(|line| {
                            let mut words = line.split_whitespace();
                            Some((words.next()?.to_owned(), words.next()?.to_owned()))
                        })
                        .collect::<BTreeSet<_>>();
                    index
                        .public_symbols
                        .entry(dependency.namespace.clone())
                        .or_default()
                        .extend(public);
                }
            }
        }
    }
    for ((_, _, _, symbol), entries) in conflict_groups {
        if entries.len() > 1 && symbol.kind != "id" {
            index.conflicts.insert(symbol);
        }
    }
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
                    resources::public_symbols(text).map(Some)
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
