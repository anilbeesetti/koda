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

    pub(crate) fn android_resource_definitions(
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
