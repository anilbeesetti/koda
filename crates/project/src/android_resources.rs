use crate::{LocationLink, Project, ProjectPath};
use anyhow::Result;
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
        if paths.is_empty() {
            return None;
        }
        let origin = Location {
            buffer: buffer.clone(),
            range: snapshot.anchor_before(found.start())..snapshot.anchor_after(found.end()),
        };
        Some(cx.spawn(async move |project, cx| {
            if let Some(namespace) = namespace {
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
            // ponytail: show all module source-set/locale declarations; use the Gradle
            // resource overlay model when selecting one active resource becomes necessary.
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
                    let mut matches_type = element.name().as_ref() == kind.as_bytes();
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
