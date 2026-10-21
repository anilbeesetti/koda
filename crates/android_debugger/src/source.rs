//! Bounded JSR-45 source maps and exact source-path resolution.
//!
//! Kotlin and KotlinDebug describe different locations for an inlined instruction.
//! Callers select a stratum explicitly; this module never substitutes one for another.
//! The mapping format is defined by JSR-45, not copied from IntelliJ implementations.

use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

pub const MAX_SMAP_BYTES: usize = 1_048_576;
pub const MAX_SMAP_TEXT_LINES: usize = 65_536;
pub const MAX_SMAP_FILES: usize = 4_096;
pub const MAX_SMAP_STRATA: usize = 16;
pub const MAX_SMAP_MAPPINGS: usize = 65_536;
pub const MAX_SMAP_OUTPUT_LINES: usize = 262_144;
pub const MAX_SOURCE_PATH_BYTES: usize = 4_096;
pub const MAX_SOURCE_ROOTS: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFile {
    pub id: u32,
    pub name: String,
    /// The exact declared SMAP path, before Kotlin compiler facade conversion.
    pub declared_path: PathBuf,
    /// Source path used for exact lookup. Kotlin's package/facade entry is
    /// converted to its declared source file within that same package.
    pub path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLocation {
    pub path: PathBuf,
    pub line: u32,
    pub file_id: u32,
    pub stratum: String,
}

/// Downcast this error from anyhow to distinguish unresolved source from malformed
/// source-map syntax or invalid caller input. No ambiguous candidate is selected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UnresolvedSource {
    Unavailable(String),
    Missing(PathBuf),
    Ambiguous {
        declared_path: PathBuf,
        candidates: Vec<PathBuf>,
    },
    MissingMapping {
        stratum: String,
        output_line: u32,
    },
    AmbiguousMapping {
        stratum: String,
        output_line: u32,
    },
}

impl std::fmt::Display for UnresolvedSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(reason) => formatter.write_str(reason),
            Self::Missing(path) => write!(formatter, "source file not found: {}", path.display()),
            Self::Ambiguous { declared_path, .. } => {
                write!(
                    formatter,
                    "ambiguous source file: {}",
                    declared_path.display()
                )
            }
            Self::MissingMapping {
                stratum,
                output_line,
            } => {
                write!(
                    formatter,
                    "no {stratum} source mapping for runtime line {output_line}"
                )
            }
            Self::AmbiguousMapping {
                stratum,
                output_line,
            } => {
                write!(
                    formatter,
                    "ambiguous {stratum} source mapping for runtime line {output_line}"
                )
            }
        }
    }
}

impl std::error::Error for UnresolvedSource {}

impl UnresolvedSource {
    pub fn new(reason: impl Into<String>) -> Self {
        Self::Unavailable(reason.into())
    }
}

#[derive(Clone, Debug)]
struct LineMapping {
    input_start: u32,
    file_id: u32,
    repeat: u32,
    output_start: u32,
    output_increment: u32,
    output_end: u32,
}

#[derive(Clone, Debug, Default)]
struct Stratum {
    files: BTreeMap<u32, SourceFile>,
    mappings: Vec<LineMapping>,
    has_files: bool,
    has_lines: bool,
    last_file_id: u32,
}

#[derive(Clone, Debug)]
pub struct Smap {
    pub output_file: String,
    pub default_stratum: String,
    strata: BTreeMap<String, Stratum>,
}

#[derive(Clone, Copy)]
enum Section {
    None,
    Files,
    Lines,
    Vendor,
}

impl Smap {
    pub fn parse(text: &str) -> Result<Self> {
        ensure!(text.len() <= MAX_SMAP_BYTES, "SMAP byte limit exceeded");
        let mut lines = Vec::new();
        for line in text.lines() {
            ensure!(
                lines.len() < MAX_SMAP_TEXT_LINES,
                "SMAP text-line limit exceeded"
            );
            lines.push(line);
        }
        ensure!(
            lines.len() >= 4 && lines[0] == "SMAP",
            "invalid SMAP header"
        );
        checked_text(lines[1], MAX_SOURCE_PATH_BYTES, "SMAP output file")?;
        checked_text(lines[2], 128, "default stratum")?;
        let mut smap = Self {
            output_file: lines[1].into(),
            default_stratum: lines[2].into(),
            strata: BTreeMap::new(),
        };
        let mut current_name: Option<String> = None;
        let mut section = Section::None;
        let mut total_files = 0usize;
        let mut total_mappings = 0usize;
        let mut total_output_lines = 0usize;
        let mut cursor = 3usize;
        let mut ended = false;
        while cursor < lines.len() {
            let line = lines[cursor];
            cursor += 1;
            if line == "*E" {
                ensure!(
                    lines[cursor..].iter().all(|line| line.is_empty()),
                    "content after SMAP end"
                );
                ended = true;
                break;
            }
            if let Some(name) = line.strip_prefix("*S ") {
                checked_text(name, 128, "stratum name")?;
                ensure!(
                    smap.strata.len() < MAX_SMAP_STRATA,
                    "SMAP stratum limit exceeded"
                );
                ensure!(!smap.strata.contains_key(name), "duplicate SMAP stratum");
                smap.strata.insert(
                    name.into(),
                    Stratum {
                        last_file_id: 1,
                        ..Stratum::default()
                    },
                );
                current_name = Some(name.into());
                section = Section::None;
                continue;
            }
            if line == "*V" || line.starts_with("*V ") {
                // Vendor payloads are bounded by the outer byte and line limits.
                section = Section::Vendor;
                continue;
            }
            if matches!(section, Section::Vendor) && !line.starts_with('*') {
                continue;
            }
            let name = current_name
                .as_deref()
                .context("section before SMAP stratum")?;
            let stratum = smap.strata.get_mut(name).context("missing SMAP stratum")?;
            match line {
                "*F" => {
                    ensure!(!stratum.has_files, "duplicate SMAP file section");
                    ensure!(!stratum.has_lines, "SMAP file section after line section");
                    stratum.has_files = true;
                    section = Section::Files;
                    continue;
                }
                "*L" => {
                    ensure!(stratum.has_files, "SMAP line section before file section");
                    ensure!(!stratum.has_lines, "duplicate SMAP line section");
                    stratum.has_lines = true;
                    section = Section::Lines;
                    continue;
                }
                _ if line.starts_with('*') => bail!("unsupported SMAP section: {line}"),
                _ => {}
            }
            match section {
                Section::Files => {
                    ensure!(total_files < MAX_SMAP_FILES, "SMAP file limit exceeded");
                    let (entry, has_path) = match line.strip_prefix("+ ") {
                        Some(entry) => (entry, true),
                        None => (line, false),
                    };
                    let (id, file_name) =
                        entry.split_once(' ').context("malformed SMAP file entry")?;
                    let id = positive_number(id, "file id")?;
                    checked_text(file_name, MAX_SOURCE_PATH_BYTES, "SMAP file name")?;
                    ensure!(!stratum.files.contains_key(&id), "duplicate SMAP file id");
                    let path = if has_path {
                        let path = lines.get(cursor).context("missing SMAP file path")?;
                        ensure!(!path.starts_with('*'), "missing SMAP file path");
                        checked_text(path, MAX_SOURCE_PATH_BYTES, "SMAP file path")?;
                        cursor += 1;
                        PathBuf::from(path)
                    } else {
                        PathBuf::from(file_name)
                    };
                    let source_path = source_path_for_file(name, file_name, &path)?;
                    stratum.files.insert(
                        id,
                        SourceFile {
                            id,
                            name: file_name.into(),
                            declared_path: path,
                            path: source_path,
                        },
                    );
                    total_files += 1;
                }
                Section::Lines => {
                    ensure!(
                        total_mappings < MAX_SMAP_MAPPINGS,
                        "SMAP mapping limit exceeded"
                    );
                    let mapping = parse_line_mapping(line, stratum.last_file_id)?;
                    ensure!(
                        stratum.files.contains_key(&mapping.file_id),
                        "SMAP mapping references unknown file"
                    );
                    let cardinality = usize::try_from(
                        mapping
                            .repeat
                            .checked_mul(mapping.output_increment)
                            .context("SMAP output cardinality overflow")?,
                    )
                    .context("SMAP output cardinality cannot fit usize")?;
                    total_output_lines = total_output_lines
                        .checked_add(cardinality)
                        .context("SMAP output cardinality overflow")?;
                    ensure!(
                        total_output_lines <= MAX_SMAP_OUTPUT_LINES,
                        "SMAP output-line limit exceeded"
                    );
                    stratum.last_file_id = mapping.file_id;
                    stratum.mappings.push(mapping);
                    total_mappings += 1;
                }
                Section::Vendor => {}
                Section::None => bail!("SMAP payload outside a section"),
            }
        }
        ensure!(ended, "missing SMAP end");
        ensure!(
            smap.strata.contains_key(&smap.default_stratum),
            "default SMAP stratum is missing"
        );
        Ok(smap)
    }

    /// Conservative accounting for retained collection/string capacity, intended
    /// for a cache budget alongside an entry cap. BTree allocation overhead uses
    /// a deliberately generous per-entry charge; this is not an RSS measurement
    /// or an allocator-independent upper bound.
    pub fn estimated_heap_bytes(&self) -> usize {
        let mut bytes = self
            .output_file
            .capacity()
            .saturating_add(self.default_stratum.capacity());
        for (name, stratum) in &self.strata {
            bytes = bytes.saturating_add(name.capacity()).saturating_add(512);
            bytes = bytes.saturating_add(
                stratum
                    .mappings
                    .capacity()
                    .saturating_mul(std::mem::size_of::<LineMapping>()),
            );
            for file in stratum.files.values() {
                bytes = bytes
                    .saturating_add(512)
                    .saturating_add(file.name.capacity())
                    .saturating_add(file.declared_path.capacity())
                    .saturating_add(file.path.capacity());
            }
        }
        bytes
    }

    pub fn has_stratum(&self, name: &str) -> bool {
        self.strata.contains_key(name)
    }

    pub fn resolve_output(
        &self,
        stratum: &str,
        output_line: u32,
        roots: &[PathBuf],
    ) -> Result<(PathBuf, u32)> {
        let location = self.map_output_line(stratum, output_line)?.ok_or_else(|| {
            UnresolvedSource::MissingMapping {
                stratum: stratum.into(),
                output_line,
            }
        })?;
        let path = resolve_source_path(roots, &location.path)?
            .ok_or_else(|| UnresolvedSource::Missing(location.path.clone()))?;
        Ok((path, location.line))
    }

    pub fn stratum_names(&self) -> impl Iterator<Item = &str> {
        self.strata.keys().map(String::as_str)
    }

    pub fn files(&self, stratum: &str) -> Result<impl Iterator<Item = &SourceFile>> {
        Ok(self.stratum(stratum)?.files.values())
    }

    /// Map an actual runtime output line. Increment-sized blocks map to one input
    /// line. Conflicting mappings fail explicitly instead of guessing a location.
    pub fn map_output_line(
        &self,
        stratum: &str,
        output_line: u32,
    ) -> Result<Option<SourceLocation>> {
        ensure!(output_line > 0, "runtime line must be positive");
        let selected = self.stratum(stratum)?;
        let mut result: Option<SourceLocation> = None;
        for mapping in &selected.mappings {
            if output_line < mapping.output_start || output_line > mapping.output_end {
                continue;
            }
            let offset = (output_line - mapping.output_start) / mapping.output_increment;
            let line = mapping
                .input_start
                .checked_add(offset)
                .context("SMAP input line overflow")?;
            let source = selected
                .files
                .get(&mapping.file_id)
                .context("missing SMAP source file")?;
            if let Some(existing) = &result {
                if existing.path != source.path || existing.line != line {
                    return Err(UnresolvedSource::AmbiguousMapping {
                        stratum: stratum.into(),
                        output_line,
                    }
                    .into());
                }
            } else {
                result = Some(SourceLocation {
                    path: source.path.clone(),
                    line,
                    file_id: source.id,
                    stratum: stratum.into(),
                });
            }
        }
        Ok(result)
    }

    /// Exact path lookup only. A basename never matches a different qualified path.
    /// Returned lines are sorted and unique; one input line can map to several
    /// runtime lines, including every output line in an increment-sized block.
    pub fn output_lines_for_source(
        &self,
        stratum: &str,
        exact_path: &Path,
        source_line: u32,
    ) -> Result<Vec<u32>> {
        ensure!(source_line > 0, "source line must be positive");
        let selected = self.stratum(stratum)?;
        let mut output_lines = BTreeSet::new();
        for mapping in &selected.mappings {
            let source = selected
                .files
                .get(&mapping.file_id)
                .context("missing SMAP source file")?;
            if source.path != exact_path || source_line < mapping.input_start {
                continue;
            }
            let input_offset = source_line - mapping.input_start;
            if input_offset >= mapping.repeat {
                continue;
            }
            let output_offset = input_offset
                .checked_mul(mapping.output_increment)
                .context("SMAP output offset overflow")?;
            let first = mapping
                .output_start
                .checked_add(output_offset)
                .context("SMAP output line overflow")?;
            for offset in 0..mapping.output_increment {
                let output = first
                    .checked_add(offset)
                    .context("SMAP output line overflow")?;
                output_lines.insert(output);
                ensure!(
                    output_lines.len() <= MAX_SMAP_OUTPUT_LINES,
                    "SMAP reverse mapping limit exceeded"
                );
            }
        }
        Ok(output_lines.into_iter().collect())
    }

    /// Resolve the requested exact file once, then collect its runtime lines
    /// before the caller asks the VM for method line tables. Unrelated missing
    /// files are ignored. If an ambiguous declared path includes the requested
    /// file, fail explicitly; an ambiguity among unrelated files cannot match it.
    pub fn output_lines_for_resolved_source(
        &self,
        stratum: &str,
        exact_path: &Path,
        source_line: u32,
        roots: &[PathBuf],
    ) -> Result<Vec<u32>> {
        ensure!(source_line > 0, "source line must be positive");
        let selected = self.stratum(stratum)?;
        let requested = resolve_source_path(roots, exact_path)?
            .ok_or_else(|| UnresolvedSource::Missing(exact_path.to_path_buf()))?;
        let mut matched_files = BTreeSet::new();
        for file in selected.files.values() {
            match resolve_source_path(roots, &file.path) {
                Ok(Some(candidate)) if candidate == requested => {
                    matched_files.insert(file.id);
                }
                Ok(_) => {}
                Err(error) => match error.downcast_ref::<UnresolvedSource>() {
                    Some(UnresolvedSource::Ambiguous { candidates, .. })
                        if !candidates.contains(&requested) => {}
                    _ => return Err(error),
                },
            }
        }
        let mut output_lines = BTreeSet::new();
        for mapping in &selected.mappings {
            if matched_files.contains(&mapping.file_id) {
                collect_output_lines(mapping, source_line, &mut output_lines)?;
            }
        }
        Ok(output_lines.into_iter().collect())
    }

    fn stratum(&self, name: &str) -> Result<&Stratum> {
        self.strata
            .get(name)
            .with_context(|| format!("SMAP stratum is missing: {name}"))
    }
}

/// Kotlin's compiler writes a JVM facade (app/InlineKt) in a + file entry,
/// alongside the separate source name (Inline.kt). Preserve its complete package;
/// never search by basename. Actual paths with an extension remain unchanged.
fn source_path_for_file(stratum: &str, file_name: &str, declared_path: &Path) -> Result<PathBuf> {
    if matches!(stratum, "Kotlin" | "KotlinDebug")
        && !declared_path.is_absolute()
        && declared_path.extension().is_none()
        && matches!(
            Path::new(file_name)
                .extension()
                .and_then(|extension| extension.to_str()),
            Some("kt" | "kts")
        )
    {
        ensure!(
            Path::new(file_name).components().count() == 1
                && !file_name.chars().any(|ch| matches!(ch, '/' | '\\' | ':'))
                && declared_path
                    .components()
                    .all(|component| matches!(component, Component::Normal(_)))
                && !declared_path.as_os_str().to_string_lossy().contains('\\'),
            "invalid Kotlin facade source path"
        );
        let source_path = declared_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(file_name);
        ensure!(
            source_path.as_os_str().len() <= MAX_SOURCE_PATH_BYTES,
            "Kotlin source path limit exceeded"
        );
        return Ok(source_path);
    }
    Ok(declared_path.to_path_buf())
}

fn checked_text(text: &str, max_bytes: usize, name: &str) -> Result<()> {
    ensure!(
        !text.is_empty() && text.len() <= max_bytes,
        "invalid {name} length"
    );
    ensure!(
        !text.chars().any(char::is_control),
        "control character in {name}"
    );
    Ok(())
}

fn collect_output_lines(
    mapping: &LineMapping,
    source_line: u32,
    output_lines: &mut BTreeSet<u32>,
) -> Result<()> {
    if source_line < mapping.input_start {
        return Ok(());
    }
    let input_offset = source_line - mapping.input_start;
    if input_offset >= mapping.repeat {
        return Ok(());
    }
    let output_offset = input_offset
        .checked_mul(mapping.output_increment)
        .context("SMAP output offset overflow")?;
    let first = mapping
        .output_start
        .checked_add(output_offset)
        .context("SMAP output line overflow")?;
    for offset in 0..mapping.output_increment {
        output_lines.insert(
            first
                .checked_add(offset)
                .context("SMAP output line overflow")?,
        );
        ensure!(
            output_lines.len() <= MAX_SMAP_OUTPUT_LINES,
            "SMAP reverse mapping limit exceeded"
        );
    }
    Ok(())
}

fn positive_number(text: &str, name: &str) -> Result<u32> {
    ensure!(
        !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()),
        "invalid SMAP {name}"
    );
    let value = text
        .parse::<u32>()
        .with_context(|| format!("SMAP {name} overflow"))?;
    ensure!(value > 0, "SMAP {name} must be positive");
    Ok(value)
}

fn parse_line_mapping(text: &str, default_file_id: u32) -> Result<LineMapping> {
    let (input, output) = text
        .split_once(':')
        .context("malformed SMAP line mapping")?;
    let (input, repeat) = match input.split_once(',') {
        Some((input, repeat)) => (input, positive_number(repeat, "repeat")?),
        None => (input, 1),
    };
    let (input, file_id) = match input.split_once('#') {
        Some((input, file_id)) => (input, positive_number(file_id, "file id")?),
        None => (input, default_file_id),
    };
    let (output, increment) = match output.split_once(',') {
        Some((output, increment)) => (output, positive_number(increment, "increment")?),
        None => (output, 1),
    };
    let input_start = positive_number(input, "input line")?;
    let output_start = positive_number(output, "output line")?;
    input_start
        .checked_add(repeat - 1)
        .context("SMAP input range overflow")?;
    let cardinality = repeat
        .checked_mul(increment)
        .context("SMAP output cardinality overflow")?;
    let output_end = output_start
        .checked_add(cardinality - 1)
        .context("SMAP output range overflow")?;
    Ok(LineMapping {
        input_start,
        file_id,
        repeat,
        output_start,
        output_increment: increment,
        output_end,
    })
}

/// Resolve only the declared path against source roots; no recursive search and no
/// basename/suffix fallback. Multiple existing candidates are an ambiguity error.
/// Relative parent traversal is rejected. Absolute SMAP paths can resolve directly.
pub fn resolve_source_path(roots: &[PathBuf], declared_path: &Path) -> Result<Option<PathBuf>> {
    ensure!(
        roots.len() <= MAX_SOURCE_ROOTS,
        "source-root limit exceeded"
    );
    ensure!(
        !declared_path.as_os_str().is_empty()
            && declared_path.as_os_str().len() <= MAX_SOURCE_PATH_BYTES,
        "invalid source path length"
    );
    if declared_path.is_absolute() {
        return if declared_path.is_file() {
            Ok(Some(
                declared_path
                    .canonicalize()
                    .context("cannot resolve absolute source candidate")?,
            ))
        } else {
            Ok(None)
        };
    }
    ensure!(
        declared_path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir)),
        "source path must not escape its root"
    );
    let mut candidates = BTreeSet::new();
    for root in roots {
        let candidate = root.join(declared_path);
        if candidate.is_file() {
            // Canonicalization deduplicates roots that alias the same file. It is
            // bounded to candidate files, never an entire source-tree traversal.
            candidates.insert(
                candidate
                    .canonicalize()
                    .context("cannot resolve source candidate")?,
            );
        }
    }
    if candidates.len() > 1 {
        return Err(UnresolvedSource::Ambiguous {
            declared_path: declared_path.to_path_buf(),
            candidates: candidates.into_iter().collect(),
        }
        .into());
    }
    Ok(candidates.into_iter().next())
}

/// Java fallback when SourceDebugExtension is absent. Use the JVM class package
/// plus its SourceFile attribute; Foo$Inner and Foo share the declared SourceFile.
/// A present but malformed SMAP must be reported by the caller, not bypassed here.
pub fn java_source_relative_path(signature: &str, source_file: &str) -> Result<PathBuf> {
    checked_text(signature, MAX_SOURCE_PATH_BYTES, "JVM class signature")?;
    checked_text(source_file, MAX_SOURCE_PATH_BYTES, "Java source file")?;
    let class_name = signature
        .strip_prefix('L')
        .and_then(|value| value.strip_suffix(';'))
        .context("expected a JVM reference-type class signature")?;
    ensure!(
        !class_name.is_empty()
            && class_name
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..")
            && !class_name
                .chars()
                .any(|ch| matches!(ch, '\\' | ':' | '.' | '[' | ';')),
        "invalid JVM class signature"
    );
    ensure!(
        source_file != "."
            && source_file != ".."
            && !source_file.chars().any(|ch| matches!(ch, '/' | '\\' | ':')),
        "SourceFile must contain one file name"
    );
    let relative = match class_name.rsplit_once('/') {
        Some((package, _)) => Path::new(package).join(source_file),
        None => PathBuf::from(source_file),
    };
    ensure!(
        relative.as_os_str().len() <= MAX_SOURCE_PATH_BYTES,
        "Java source path limit exceeded"
    );
    Ok(relative)
}

pub fn try_resolve_java_source(
    roots: &[PathBuf],
    signature: &str,
    source_file: &str,
) -> Result<Option<PathBuf>> {
    resolve_source_path(roots, &java_source_relative_path(signature, source_file)?)
}

pub fn resolve_java_source(
    roots: &[PathBuf],
    signature: &str,
    source_file: &str,
) -> Result<PathBuf> {
    let relative = java_source_relative_path(signature, source_file)?;
    resolve_source_path(roots, &relative)?.ok_or_else(|| UnresolvedSource::Missing(relative).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    const INLINE_MAP: &str = "SMAP\nMain.kt\nKotlin\n*S Kotlin\n*F\n+ 1 Main.kt\napp/Main.kt\n+ 2 Inline.kt\nother/Inline.kt\n+ 3 Inline.kt\napp/Inline.kt\n*L\n1#1,10:1\n20#2,2:11\n40#3:13\n*S KotlinDebug\n*F\n+ 1 Main.kt\napp/Main.kt\n*L\n7#1,3:11\n*E\n";

    #[test]
    fn kotlin_and_kotlin_debug_keep_distinct_inline_locations() {
        let map = Smap::parse(INLINE_MAP).unwrap();
        let body = map.map_output_line("Kotlin", 11).unwrap().unwrap();
        let call_site = map.map_output_line("KotlinDebug", 11).unwrap().unwrap();
        assert_eq!(body.path, Path::new("other/Inline.kt"));
        assert_eq!(body.line, 20);
        assert_eq!(call_site.path, Path::new("app/Main.kt"));
        assert_eq!(call_site.line, 7);
        assert!(map.map_output_line("KotlinDebug", 1).unwrap().is_none());
        assert!(map.map_output_line("Missing", 11).is_err());
        assert_eq!(map.default_stratum, "Kotlin");
    }

    #[test]
    fn equal_basenames_use_exact_paths_in_both_directions() {
        let map = Smap::parse(INLINE_MAP).unwrap();
        assert_eq!(
            map.output_lines_for_source("Kotlin", Path::new("other/Inline.kt"), 20)
                .unwrap(),
            vec![11]
        );
        assert_eq!(
            map.output_lines_for_source("Kotlin", Path::new("app/Inline.kt"), 40)
                .unwrap(),
            vec![13]
        );
        assert!(
            map.output_lines_for_source("Kotlin", Path::new("Inline.kt"), 20)
                .unwrap()
                .is_empty()
        );
        assert!(
            map.output_lines_for_source("Kotlin", Path::new("app/Inline.kt"), 20)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn simple_file_entries_and_remembered_file_ids_are_supported() {
        let map = Smap::parse(
            "SMAP\nOut.kt\nKotlin\n*S Kotlin\n*F\n1 Main.kt\n2 Inline.kt\n*L\n8#2:100\n9:101\n*E\n",
        )
        .unwrap();
        let location = map.map_output_line("Kotlin", 101).unwrap().unwrap();
        assert_eq!(location.path, Path::new("Inline.kt"));
        assert_eq!(location.file_id, 2);
        assert_eq!(location.line, 9);
        assert_eq!(map.files("Kotlin").unwrap().count(), 2);
    }

    #[test]
    fn repeat_and_increment_cover_every_output_line_without_expanding_at_parse() {
        let map = one_mapping("10#1,3:100,2").unwrap();
        for (output, input) in [
            (100, 10),
            (101, 10),
            (102, 11),
            (103, 11),
            (104, 12),
            (105, 12),
        ] {
            assert_eq!(
                map.map_output_line("Kotlin", output).unwrap().unwrap().line,
                input
            );
        }
        assert!(map.map_output_line("Kotlin", 99).unwrap().is_none());
        assert!(map.map_output_line("Kotlin", 106).unwrap().is_none());
        assert_eq!(
            map.output_lines_for_source("Kotlin", Path::new("File.kt"), 11)
                .unwrap(),
            vec![102, 103]
        );
        assert!(
            map.output_lines_for_source("Kotlin", Path::new("File.kt"), 13)
                .unwrap()
                .is_empty()
        );
        assert!(map.map_output_line("Kotlin", 0).is_err());
        assert!(
            map.output_lines_for_source("Kotlin", Path::new("File.kt"), 0)
                .is_err()
        );
    }

    #[test]
    fn conflicting_runtime_locations_are_errors_and_identical_duplicates_deduplicate() {
        let map = one_mapping("1#1:10\n2#1:10").unwrap();
        assert!(map.map_output_line("Kotlin", 10).is_err());
        let map = one_mapping("1#1:10\n1#1:10").unwrap();
        assert_eq!(map.map_output_line("Kotlin", 10).unwrap().unwrap().line, 1);
        assert_eq!(
            map.output_lines_for_source("Kotlin", Path::new("File.kt"), 1)
                .unwrap(),
            vec![10]
        );
    }

    #[test]
    fn checked_arithmetic_rejects_input_output_and_cardinality_overflow() {
        for mapping in [
            "4294967295#1,2:1",
            "1#1,2:4294967295",
            "1#1,4294967295:1,2",
            "4294967296#1:1",
            "1#1:4294967296",
        ] {
            assert!(one_mapping(mapping).is_err(), "{mapping}");
        }
        let map = one_mapping("4294967295#1:4294967295").unwrap();
        assert_eq!(
            map.map_output_line("Kotlin", u32::MAX)
                .unwrap()
                .unwrap()
                .line,
            u32::MAX
        );
        assert_eq!(
            map.output_lines_for_source("Kotlin", Path::new("File.kt"), u32::MAX)
                .unwrap(),
            vec![u32::MAX]
        );
    }

    #[test]
    fn malformed_records_are_reported_instead_of_silently_falling_back() {
        for mapping in [
            "", "1", "1:1:1", "0#1:1", "1#0:1", "1#9:1", "1#1,0:1", "1#1:0", "1#1:1,0", "1#1,-1:1",
            "1##1:1", "1#1: 1",
        ] {
            assert!(one_mapping(mapping).is_err(), "{mapping:?}");
        }
        for text in [
            "SMAP\nOut.kt\nKotlin\n*F\n1 File.kt\n*E\n",
            "SMAP\nOut.kt\nKotlin\n*S Kotlin\n*L\n1:1\n*E\n",
            "SMAP\nOut.kt\nKotlin\n*S Kotlin\n*F\n+ 1 File.kt\n*L\n1:1\n*E\n",
            "SMAP\nOut.kt\nKotlin\n*S Kotlin\n*F\n1 File.kt\n1 Other.kt\n*E\n",
            "SMAP\nOut.kt\nKotlin\n*S Kotlin\n*F\n1 File.kt\n*L\n1:1\n",
            "SMAP\nOut.kt\nKotlin\n*S Java\n*F\n1 File.java\n*E\n",
            "SMAP\nOut.kt\nKotlin\n*S Kotlin\n*F\n1 File.kt\n*E\nunexpected\n",
            "SMAP\nOut.kt\nKotlin\n*S Kotlin\n*F\n1 File.kt\n*S Kotlin\n*E\n",
            "SMAP\nOut.kt\nKotlin\n*S Kotlin\n*F\n1 File.kt\n*O nested\n*E\n",
        ] {
            assert!(Smap::parse(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn explicit_caps_bound_file_mapping_line_and_byte_work() {
        assert!(one_mapping(&format!("1#1,{}:1", MAX_SMAP_OUTPUT_LINES + 1)).is_err());
        assert!(one_mapping(&format!("1#1,{}:1", MAX_SMAP_OUTPUT_LINES)).is_ok());
        let mut file_map = String::from("SMAP\nOut.kt\nKotlin\n*S Kotlin\n*F\n");
        for id in 1..=MAX_SMAP_FILES + 1 {
            file_map.push_str(&format!("{id} File.kt\n"));
        }
        file_map.push_str("*E\n");
        assert!(Smap::parse(&file_map).is_err());
        let mut mapping_map = String::from("SMAP\nOut.kt\nKotlin\n*S Kotlin\n*F\n1 File.kt\n*L\n");
        for _ in 0..=MAX_SMAP_MAPPINGS {
            mapping_map.push_str("1:1\n");
        }
        mapping_map.push_str("*E\n");
        assert!(Smap::parse(&mapping_map).is_err());
        let text_lines = format!(
            "SMAP\nOut.kt\nKotlin\n*S Kotlin\n*V\n{}*E\n",
            "\n".repeat(MAX_SMAP_TEXT_LINES)
        );
        assert!(Smap::parse(&text_lines).is_err());
        assert!(Smap::parse(&"x".repeat(MAX_SMAP_BYTES + 1)).is_err());
        let long_path = format!(
            "SMAP\nOut.kt\nKotlin\n*S Kotlin\n*F\n+ 1 File.kt\n{}\n*E\n",
            "x".repeat(MAX_SOURCE_PATH_BYTES + 1)
        );
        assert!(Smap::parse(&long_path).is_err());
        let mut strata = String::from("SMAP\nOut.kt\nKotlin\n*S Kotlin\n");
        for index in 0..MAX_SMAP_STRATA {
            strata.push_str(&format!("*S Other{index}\n"));
        }
        strata.push_str("*E\n");
        assert!(Smap::parse(&strata).is_err());
    }

    #[test]
    fn crlf_and_bounded_vendor_sections_are_accepted() {
        let map = Smap::parse("SMAP\r\nOut.kt\r\nKotlin\r\n*S Kotlin\r\n*V vendor\r\nopaque\r\n*F\r\n1 File.kt\r\n*L\r\n1:1\r\n*E\r\n").unwrap();
        assert_eq!(map.map_output_line("Kotlin", 1).unwrap().unwrap().line, 1);
    }

    #[test]
    fn java_fallback_uses_signature_package_including_nested_classes() {
        assert_eq!(
            java_source_relative_path("Lapp/Inline$Inner;", "Inline.java").unwrap(),
            Path::new("app/Inline.java")
        );
        assert_eq!(
            java_source_relative_path("Lother/Inline;", "Inline.java").unwrap(),
            Path::new("other/Inline.java")
        );
        assert_eq!(
            java_source_relative_path("LDefault;", "Default.java").unwrap(),
            Path::new("Default.java")
        );
        for (signature, file) in [
            ("app.Inline", "Inline.java"),
            ("[Lapp/Inline;", "Inline.java"),
            ("Lapp//Inline;", "Inline.java"),
            ("L../Inline;", "Inline.java"),
            ("Lapp/Inline;", "../Inline.java"),
            ("Lapp/Inline;", "other/Inline.java"),
            ("Lapp/Inline;", r"other\Inline.java"),
        ] {
            assert!(java_source_relative_path(signature, file).is_err());
        }
    }

    // Adapted from PathUtilsTest.testFilePathToJVMClassNames (fwcd, MIT).
    // Direct JDWP identifies loaded classes, so both original utility outputs
    // are verified against the same exact source identity rather than guessing
    // class names from a pathname.
    #[test]
    fn original_class_and_kotlin_facade_share_exact_source_identity() {
        assert_eq!(
            java_source_relative_path("Lcom/abc/MyClass;", "MyClass.kt").unwrap(),
            Path::new("com/abc/MyClass.kt")
        );
        assert_eq!(
            java_source_relative_path("Lcom/abc/MyClassKt;", "MyClass.kt").unwrap(),
            Path::new("com/abc/MyClass.kt")
        );
    }

    #[test]
    fn roots_resolve_exact_paths_and_report_equal_basename_ambiguity() {
        let temp = TempDirectory::new();
        fs::create_dir_all(temp.0.join("app")).unwrap();
        fs::create_dir_all(temp.0.join("other")).unwrap();
        fs::write(temp.0.join("app/Inline.java"), "class Inline {}").unwrap();
        fs::write(temp.0.join("other/Inline.java"), "class Inline {}").unwrap();
        let roots = vec![temp.0.clone()];
        assert_eq!(
            try_resolve_java_source(&roots, "Lapp/Inline;", "Inline.java").unwrap(),
            Some(temp.0.join("app/Inline.java").canonicalize().unwrap())
        );
        assert_eq!(
            try_resolve_java_source(&roots, "Lother/Inline;", "Inline.java").unwrap(),
            Some(temp.0.join("other/Inline.java").canonicalize().unwrap())
        );
        assert!(
            resolve_source_path(&roots, Path::new("Inline.java"))
                .unwrap()
                .is_none()
        );
        let package_roots = vec![temp.0.join("app"), temp.0.join("other")];
        assert!(resolve_source_path(&package_roots, Path::new("Inline.java")).is_err());
        assert!(resolve_source_path(&roots, Path::new("../Inline.java")).is_err());
        let duplicate_roots = vec![temp.0.clone(), temp.0.clone()];
        assert!(
            resolve_source_path(&duplicate_roots, Path::new("app/Inline.java"))
                .unwrap()
                .is_some()
        );
        assert!(
            resolve_source_path(
                &vec![temp.0.clone(); MAX_SOURCE_ROOTS + 1],
                Path::new("app/Inline.java")
            )
            .is_err()
        );
    }

    #[test]
    fn kotlin_facades_use_declared_filename_and_exact_package_without_basename_guessing() {
        let map = Smap::parse("SMAP\nMain.kt\nKotlin\n*S Kotlin\n*F\n+ 1 Inline.kt\napp/InlineKt\n+ 2 Inline.kt\nother/InlineKt\n*L\n3#1:10\n8#2:11\n*S KotlinDebug\n*F\n+ 1 DebugFixture.kt\napp/DebugFixtureKt\n*L\n20#1:10\n*E\n").unwrap();
        assert!(map.has_stratum("Kotlin"));
        assert!(!map.has_stratum("Java"));
        assert_eq!(
            map.map_output_line("Kotlin", 10).unwrap().unwrap().path,
            Path::new("app/Inline.kt")
        );
        assert_eq!(
            map.map_output_line("Kotlin", 11).unwrap().unwrap().path,
            Path::new("other/Inline.kt")
        );
        assert_eq!(
            map.map_output_line("KotlinDebug", 10)
                .unwrap()
                .unwrap()
                .path,
            Path::new("app/DebugFixture.kt")
        );
        assert_eq!(
            map.output_lines_for_source("Kotlin", Path::new("app/Inline.kt"), 3)
                .unwrap(),
            vec![10]
        );
        assert!(
            map.output_lines_for_source("Kotlin", Path::new("Inline.kt"), 3)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            map.files("Kotlin").unwrap().next().unwrap().declared_path,
            Path::new("app/InlineKt")
        );
        let temp = TempDirectory::new();
        fs::create_dir_all(temp.0.join("app")).unwrap();
        fs::create_dir_all(temp.0.join("other")).unwrap();
        fs::write(temp.0.join("app/Inline.kt"), "fun app() = Unit").unwrap();
        fs::write(temp.0.join("other/Inline.kt"), "fun other() = Unit").unwrap();
        assert_eq!(
            map.resolve_output("Kotlin", 10, std::slice::from_ref(&temp.0))
                .unwrap(),
            (temp.0.join("app/Inline.kt").canonicalize().unwrap(), 3)
        );
        assert_eq!(
            map.resolve_output("Kotlin", 11, std::slice::from_ref(&temp.0))
                .unwrap(),
            (temp.0.join("other/Inline.kt").canonicalize().unwrap(), 8)
        );
    }

    #[test]
    fn unresolved_source_is_typed_and_never_falls_back_after_malformed_smap() {
        let map = one_mapping("1#1:10").unwrap();
        let missing = map.resolve_output("Kotlin", 10, &[]).unwrap_err();
        assert!(matches!(
            missing.downcast_ref::<UnresolvedSource>(),
            Some(UnresolvedSource::Missing(_))
        ));
        let unmapped = map.resolve_output("Kotlin", 11, &[]).unwrap_err();
        assert!(matches!(
            unmapped.downcast_ref::<UnresolvedSource>(),
            Some(UnresolvedSource::MissingMapping { .. })
        ));
        let ambiguous = one_mapping("1#1:10\n2#1:10")
            .unwrap()
            .map_output_line("Kotlin", 10)
            .unwrap_err();
        assert!(matches!(
            ambiguous.downcast_ref::<UnresolvedSource>(),
            Some(UnresolvedSource::AmbiguousMapping { .. })
        ));
        let missing_java = resolve_java_source(&[], "Lapp/Foo;", "Foo.java").unwrap_err();
        assert!(matches!(
            missing_java.downcast_ref::<UnresolvedSource>(),
            Some(UnresolvedSource::Missing(_))
        ));
        let malformed = Smap::parse("not a SMAP").unwrap_err();
        assert!(malformed.downcast_ref::<UnresolvedSource>().is_none());
    }

    #[test]
    fn resolved_reverse_lookup_preserves_packages_and_handles_only_relevant_ambiguity() {
        let map = Smap::parse("SMAP\nMain.kt\nKotlin\n*S Kotlin\n*F\n+ 1 Inline.kt\napp/InlineKt\n+ 2 Inline.kt\nother/InlineKt\n+ 3 Missing.kt\nunused/MissingKt\n*L\n3#1:10\n3#2:11\n3#3:12\n*E\n").unwrap();
        let temp = TempDirectory::new();
        for root in ["first", "second"] {
            fs::create_dir_all(temp.0.join(root).join("other")).unwrap();
            fs::write(
                temp.0.join(root).join("other/Inline.kt"),
                "fun other() = Unit",
            )
            .unwrap();
        }
        fs::create_dir_all(temp.0.join("first/app")).unwrap();
        fs::write(temp.0.join("first/app/Inline.kt"), "fun app() = Unit").unwrap();
        let roots = vec![temp.0.join("first"), temp.0.join("second")];
        let requested = temp.0.join("first/app/Inline.kt");
        assert_eq!(
            map.output_lines_for_resolved_source("Kotlin", &requested, 3, &roots)
                .unwrap(),
            vec![10]
        );
        assert!(
            map.output_lines_for_resolved_source("Kotlin", &requested, 4, &roots)
                .unwrap()
                .is_empty()
        );
        let other = map
            .output_lines_for_resolved_source(
                "Kotlin",
                &temp.0.join("first/other/Inline.kt"),
                3,
                &roots,
            )
            .unwrap_err();
        assert!(matches!(
            other.downcast_ref::<UnresolvedSource>(),
            Some(UnresolvedSource::Ambiguous { .. })
        ));
        fs::create_dir_all(temp.0.join("second/app")).unwrap();
        fs::write(
            temp.0.join("second/app/Inline.kt"),
            "fun duplicate() = Unit",
        )
        .unwrap();
        let ambiguous = map
            .output_lines_for_resolved_source("Kotlin", &requested, 3, &roots)
            .unwrap_err();
        assert!(matches!(
            ambiguous.downcast_ref::<UnresolvedSource>(),
            Some(UnresolvedSource::Ambiguous { .. })
        ));
    }

    #[test]
    fn heap_accounting_includes_parsed_file_and_mapping_storage() {
        let map = one_mapping("1#1:10").unwrap();
        let larger = one_mapping("1#1:10\n2#1:11").unwrap();
        assert!(map.estimated_heap_bytes() >= 512 + std::mem::size_of::<LineMapping>());
        assert!(larger.estimated_heap_bytes() >= map.estimated_heap_bytes());
        assert_eq!(
            map.clone().map_output_line("Kotlin", 10).unwrap(),
            map.map_output_line("Kotlin", 10).unwrap()
        );
        assert!(
            UnresolvedSource::new("class has no source metadata")
                .to_string()
                .contains("no source metadata")
        );
    }

    fn one_mapping(mapping: &str) -> Result<Smap> {
        Smap::parse(&format!(
            "SMAP\nOut.kt\nKotlin\n*S Kotlin\n*F\n1 File.kt\n*L\n{mapping}\n*E\n"
        ))
    }

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let directory = std::env::temp_dir().join(format!(
                "koda-debugger-source-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir(&directory).unwrap();
            Self(directory)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
