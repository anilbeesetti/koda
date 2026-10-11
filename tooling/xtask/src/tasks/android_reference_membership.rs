#![allow(clippy::disallowed_methods, reason = "tooling is exempt")]

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail, ensure};
use clap::Parser;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CATALOG: &str = include_str!("../../test_data/reference_membership/selection.json");
const MAX_SELECTION_BYTES: u64 = 256 * 1024;
const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOKENS: usize = 50_000;
const MAX_DEPTH: usize = 32;
const MAX_CALLS: usize = 64;
const MACRO_PATH: &str = "bazel/bazel.bzl";
const MACRO_LABEL: &str = "//tools/base/bazel:bazel.bzl";
const ATTRIBUTES: &[&str] = &[
    "name",
    "srcs",
    "package_prefixes",
    "test_srcs",
    "exclude",
    "resources",
    "res_zips",
    "test_resources",
    "deps",
    "runtime_deps",
    "test_friends",
    "visibility",
    "module_visibility",
    "exports",
    "jvm_target",
    "javacopts",
    "javacopts_from_jps",
    "kotlinc_opts",
    "enable_tests",
    "test_data",
    "test_deps",
    "test_flaky",
    "test_jvm_flags",
    "test_timeout",
    "test_class",
    "test_shard_count",
    "split_test_targets",
    "tags",
    "compatible_intellij_platforms",
    "test_tags",
    "test_agents",
    "iml_files",
    "data",
    "test_main_class",
    "lint_baseline",
    "lint_enabled",
    "lint_partial_analysis",
    "lint_timeout",
    "exec_properties",
    "kotlin_use_compose",
    "kotlin_use_serialization",
    "generate_coverage_baseline",
];

#[derive(Parser)]
pub struct AndroidReferenceMembershipArgs {
    /// Complete original sources in source-id/upstream-path layout.
    #[arg(long)]
    source_root: PathBuf,
    /// Exact pinned file selection; independent of the growing parity ledger.
    #[arg(long)]
    selection: PathBuf,
    /// Create external evidence without replacing an existing file.
    #[arg(long)]
    output: PathBuf,
    /// Require identical existing evidence without modifying it.
    #[arg(long)]
    check: bool,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Selection {
    schema_version: u32,
    scope: String,
    files: Vec<SelectedFile>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SelectedFile {
    source: String,
    revision: String,
    archive_sha256: String,
    path: String,
    sha256: String,
    bytes: u64,
    license: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct Span {
    start: usize,
    end: usize,
}

#[derive(Debug, Serialize)]
struct Evidence {
    schema_version: u32,
    scope: String,
    selection_sha256: String,
    immutable_catalog_sha256: String,
    retained_member_provenance_sha256: String,
    retained_member_provenance: &'static str,
    archive_membership_reverified_by_this_command: bool,
    global_census_complete: bool,
    new_original_behavior_credit: usize,
    effective_runtime_cases: Option<usize>,
    applicability: &'static str,
    files: Vec<SelectedFile>,
    build_files: Vec<BuildEvidence>,
    protocol_facts: Vec<ProtocolFact>,
    unresolved: Vec<&'static str>,
}

#[derive(Debug, Default, Serialize)]
struct BuildEvidence {
    source: String,
    path: String,
    literal_coverage_complete: bool,
    loads: Vec<LoadFact>,
    modules: Vec<ModuleFact>,
    unresolved: Vec<Unresolved>,
}

#[derive(Debug, Serialize)]
struct Unresolved {
    reason: String,
    span: Span,
}

#[derive(Debug, Serialize)]
struct LoadFact {
    label: Option<String>,
    bindings: Vec<(String, String)>,
    span: Span,
}

#[derive(Debug, Serialize)]
struct ModuleFact {
    call_binding: String,
    span: Span,
    attributes: Vec<Attribute>,
    producer_source: &'static str,
    producer_path: &'static str,
    producer_verified_load: bool,
    source_derived_relationships: Vec<Relationship>,
    membership: &'static str,
    compiled_membership: Option<bool>,
    runtime_cases: Option<usize>,
    unresolved: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Attribute {
    name: String,
    span: Span,
    value: Literal,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
enum Literal {
    String(String),
    Boolean(bool),
    Integer(u64),
    None,
    List(Vec<Literal>),
    Dictionary(Vec<(String, Literal)>),
    Unsupported,
}

#[derive(Debug, Serialize)]
struct Relationship {
    label: String,
    kind: &'static str,
    suite_class: Option<String>,
    suite_jar_property: Option<String>,
    aggregate_membership: Option<bool>,
    split_configuration: Option<String>,
    provenance: &'static str,
}

#[derive(Debug, Serialize)]
struct ProtocolFact {
    id: &'static str,
    source: &'static str,
    path: &'static str,
    meaning: &'static str,
    observations: Vec<Observation>,
    runtime_verified: bool,
}

#[derive(Debug, Serialize)]
struct Observation {
    span: Span,
    excerpt: String,
}

#[derive(Debug)]
struct Token<'a> {
    text: &'a str,
    string: Option<String>,
    span: Span,
    indentation: usize,
}

pub fn run(args: AndroidReferenceMembershipArgs) -> Result<()> {
    let selection_bytes = read_bounded(&args.selection, MAX_SELECTION_BYTES)?;
    let selection: Selection = serde_json::from_slice(&selection_bytes)?;
    let originals = load_originals(&args.source_root, &selection)?;
    let evidence = discover(selection, &selection_bytes, &originals)?;
    publish(&args.output, args.check, &encode(&evidence)?)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    ensure!(
        fs::symlink_metadata(path)?.is_file(),
        "input must be a regular file: {}",
        path.display()
    );
    let mut bytes = Vec::new();
    File::open(path)
        .with_context(|| format!("open {}", path.display()))?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "input budget exhausted: {}",
        path.display()
    );
    Ok(bytes)
}

fn safe_path(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && !path.contains('\\')
            && !path.contains(':')
            && !path.contains('\0')
            && path.split('/').all(|part| !matches!(part, "" | "." | "..")),
        "unsafe relative source path: {path}"
    );
    Ok(())
}

fn regular_source(root: &Path, source: &str, path: &str) -> Result<PathBuf> {
    safe_path(source)?;
    safe_path(path)?;
    ensure!(
        fs::symlink_metadata(root)?.is_dir(),
        "source root must be a real directory"
    );
    let mut current = root.to_owned();
    let components: Vec<_> = source.split('/').chain(path.split('/')).collect();
    for (index, component) in components.iter().enumerate() {
        current.push(component);
        let metadata = fs::symlink_metadata(&current)?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "source symlink rejected: {}",
            current.display()
        );
        ensure!(
            if index + 1 == components.len() {
                metadata.is_file()
            } else {
                metadata.is_dir()
            },
            "source must use regular files and directories: {}",
            current.display()
        );
    }
    Ok(current)
}

fn validate_selection(selection: &Selection) -> Result<()> {
    let catalog: Selection = serde_json::from_str(CATALOG)?;
    ensure!(
        selection.schema_version == 1,
        "unsupported selection schema"
    );
    ensure!(
        selection.scope == catalog.scope,
        "wrong source selection scope"
    );
    ensure!(
        selection.files.len() == catalog.files.len(),
        "complete pinned input set required"
    );
    let expected: BTreeMap<_, _> = catalog
        .files
        .iter()
        .map(|file| ((&file.source, &file.path), file))
        .collect();
    let mut identities = BTreeSet::new();
    let mut bytes = 0_u64;
    for file in &selection.files {
        safe_path(&file.source)?;
        safe_path(&file.path)?;
        ensure!(
            identities.insert((&file.source, &file.path)),
            "duplicate source identity"
        );
        ensure!(
            expected.get(&(&file.source, &file.path)).copied() == Some(file),
            "wrong pinned source revision/path/hash/length/license: {}/{}",
            file.source,
            file.path
        );
        ensure!(
            file.bytes <= MAX_SOURCE_BYTES,
            "source byte budget exhausted"
        );
        bytes = bytes
            .checked_add(file.bytes)
            .context("source byte count overflow")?;
        ensure!(
            bytes <= MAX_TOTAL_BYTES,
            "total source byte budget exhausted"
        );
    }
    Ok(())
}

fn load_originals(
    root: &Path,
    selection: &Selection,
) -> Result<BTreeMap<(String, String), String>> {
    validate_selection(selection)?;
    let mut originals = BTreeMap::new();
    for file in &selection.files {
        let path = regular_source(root, &file.source, &file.path)?;
        let bytes = read_bounded(&path, MAX_SOURCE_BYTES)?;
        ensure!(
            bytes.len() as u64 == file.bytes && sha256(&bytes) == file.sha256,
            "changed complete original: {}/{}",
            file.source,
            file.path
        );
        originals.insert(
            (file.source.clone(), file.path.clone()),
            String::from_utf8(bytes).context("original source is not UTF-8")?,
        );
    }
    Ok(originals)
}

fn discover(
    selection: Selection,
    selection_bytes: &[u8],
    originals: &BTreeMap<(String, String), String>,
) -> Result<Evidence> {
    let mut build_files = Vec::new();
    for path in ["adt-ui/BUILD", "project-system-gradle-sync/BUILD"] {
        let source = original(originals, "idea", path)?;
        let mut evidence = parse_build(source)?;
        evidence.source = "idea".into();
        evidence.path = path.into();
        let package = path
            .strip_suffix("/BUILD")
            .context("BUILD package path missing")?;
        for module in &mut evidence.modules {
            derive_relationships(module, package)?;
        }
        build_files.push(evidence);
    }
    Ok(Evidence {
        schema_version: 1,
        scope: selection.scope,
        selection_sha256: sha256(selection_bytes),
        immutable_catalog_sha256: sha256(CATALOG.as_bytes()),
        retained_member_provenance_sha256: sha256(include_bytes!(
            "../../test_data/reference_membership/provenance.json"
        )),
        retained_member_provenance: "reference_membership/provenance.json; exact previously verified archive-member identities, complete original bytes reverified here",
        archive_membership_reverified_by_this_command: false,
        global_census_complete: false,
        new_original_behavior_credit: 0,
        effective_runtime_cases: None,
        applicability: "unresolved",
        files: selection.files,
        build_files,
        protocol_facts: protocol_facts(originals)?,
        unresolved: vec![
            "Directory inputs require upstream glob, generated-source and configured target evaluation",
            "Matching Bazel/JVM/IntelliJ/JUnit closure and compiled classpath are unverified",
            "Inherited hierarchy, suite factories, ignored/filter/shard cases and runner-expanded instances remain unresolved",
            "This bounded source evidence does not enumerate all reference suites or port original behavior",
        ],
    })
}

fn original<'a>(
    originals: &'a BTreeMap<(String, String), String>,
    source: &str,
    path: &str,
) -> Result<&'a str> {
    originals
        .get(&(source.into(), path.into()))
        .map(String::as_str)
        .with_context(|| format!("missing required original {source}/{path}"))
}

fn lex(source: &str, max_tokens: usize) -> Result<Vec<Token<'_>>> {
    ensure!(
        source.len() as u64 <= MAX_SOURCE_BYTES,
        "source byte budget exhausted"
    );
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut position = 0;
    let mut line_start = 0;
    let mut delimiters = Vec::new();
    while position < bytes.len() {
        let start = position;
        let byte = bytes[position];
        if matches!(byte, b' ' | b'\t' | b'\r') {
            position += 1;
            continue;
        }
        if byte == b'#' {
            while position < bytes.len() && bytes[position] != b'\n' {
                position += 1;
            }
            continue;
        }
        let indentation = source[line_start..start]
            .bytes()
            .take_while(|byte| matches!(byte, b' ' | b'\t' | b'\r'))
            .count();
        let mut string = None;
        if matches!(byte, b'\'' | b'"') {
            let triple = bytes
                .get(position..position + 3)
                .is_some_and(|slice| slice == [byte, byte, byte]);
            let width = if triple { 3 } else { 1 };
            position += width;
            let mut value = String::new();
            let mut closed = false;
            while position < bytes.len() {
                if bytes
                    .get(position..position + width)
                    .is_some_and(|slice| slice.iter().all(|item| *item == byte))
                {
                    position += width;
                    closed = true;
                    break;
                }
                if bytes[position] == b'\\' {
                    position += 1;
                    let escaped = *bytes.get(position).context("unterminated string escape")?;
                    value.push(match escaped {
                        b'\\' => '\\',
                        b'\'' => '\'',
                        b'"' => '"',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        _ => bail!("unsupported string escape; source evidence unresolved"),
                    });
                    position += 1;
                } else {
                    let character = source[position..]
                        .chars()
                        .next()
                        .context("invalid string position")?;
                    ensure!(
                        triple || character != '\n',
                        "unterminated single-line string"
                    );
                    if character == '\n' {
                        line_start = position + 1;
                    }
                    value.push(character);
                    position += character.len_utf8();
                }
            }
            ensure!(closed, "unterminated string");
            string = Some(value);
        } else if byte.is_ascii_alphanumeric() || byte == b'_' {
            position += 1;
            while bytes
                .get(position)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            {
                position += 1;
            }
        } else {
            position += source[position..]
                .chars()
                .next()
                .context("invalid token position")?
                .len_utf8();
            if matches!(byte, b'(' | b'[' | b'{') {
                ensure!(
                    delimiters.len() < MAX_DEPTH,
                    "delimiter depth budget exhausted"
                );
                delimiters.push(byte);
            } else if matches!(byte, b')' | b']' | b'}') {
                let expected = match byte {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                ensure!(delimiters.pop() == Some(expected), "mismatched delimiter");
            }
            if byte == b'\n' {
                line_start = position;
            }
        }
        ensure!(
            tokens.len() < max_tokens,
            "token budget exhausted; source membership unresolved"
        );
        tokens.push(Token {
            text: &source[start..position],
            string,
            span: Span {
                start,
                end: position,
            },
            indentation,
        });
    }
    ensure!(delimiters.is_empty(), "unclosed delimiter");
    Ok(tokens)
}

fn significant<'a, 'b>(tokens: &'b [Token<'a>]) -> Vec<&'b Token<'a>> {
    tokens.iter().filter(|token| token.text != "\n").collect()
}

fn separated<'a, 'b>(tokens: &[&'b Token<'a>], separator: &str) -> Vec<Vec<&'b Token<'a>>> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0_usize;
    for (index, token) in tokens.iter().enumerate() {
        match token.text {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth = depth.saturating_sub(1),
            text if text == separator && depth == 0 => {
                parts.push(tokens[start..index].to_vec());
                start = index + 1;
            }
            _ => {}
        }
    }
    if start < tokens.len() {
        parts.push(tokens[start..].to_vec());
    }
    parts
}

fn literal(tokens: &[&Token<'_>]) -> Literal {
    if tokens.len() == 1 {
        let token = tokens[0];
        if let Some(value) = &token.string {
            return Literal::String(value.clone());
        }
        return match token.text {
            "True" => Literal::Boolean(true),
            "False" => Literal::Boolean(false),
            "None" => Literal::None,
            text if text.bytes().all(|byte| byte.is_ascii_digit()) => text
                .parse()
                .map(Literal::Integer)
                .unwrap_or(Literal::Unsupported),
            _ => Literal::Unsupported,
        };
    }
    if tokens.len() < 2 {
        return Literal::Unsupported;
    }
    let first = tokens[0].text;
    let last = tokens[tokens.len() - 1].text;
    if !matches!((first, last), ("[", "]") | ("{", "}")) {
        return Literal::Unsupported;
    }
    let parts = separated(&tokens[1..tokens.len() - 1], ",");
    if first == "[" {
        return Literal::List(parts.iter().map(|part| literal(part)).collect());
    }
    let mut entries = Vec::new();
    let mut keys = BTreeSet::new();
    for part in parts {
        let pair = separated(&part, ":");
        if pair.len() != 2 {
            return Literal::Unsupported;
        }
        let Literal::String(key) = literal(&pair[0]) else {
            return Literal::Unsupported;
        };
        if !keys.insert(key.clone()) {
            return Literal::Unsupported;
        }
        entries.push((key, literal(&pair[1])));
    }
    Literal::Dictionary(entries)
}

fn fully_literal(value: &Literal) -> bool {
    match value {
        Literal::Unsupported => false,
        Literal::List(values) => values.iter().all(fully_literal),
        Literal::Dictionary(entries) => entries.iter().all(|(_, value)| fully_literal(value)),
        _ => true,
    }
}

fn parse_build(source: &str) -> Result<BuildEvidence> {
    let tokens = lex(source, MAX_TOKENS)?;
    let mut evidence = BuildEvidence::default();
    let mut bindings = BTreeMap::new();
    let mut position = 0;
    let mut calls = 0;
    let mut binding_resolution_complete = true;
    while position < tokens.len() {
        let token = &tokens[position];
        if token.text == "\n" {
            position += 1;
            continue;
        }
        if token.indentation != 0
            || tokens
                .get(position + 1)
                .is_none_or(|token| token.text != "(")
        {
            binding_resolution_complete = false;
            evidence.unresolved.push(Unresolved { reason: "Unsupported statement or conditional/indented build code; target absence cannot be inferred".into(), span: token.span });
            while position < tokens.len() && tokens[position].text != "\n" {
                position += 1;
            }
            continue;
        }
        calls += 1;
        ensure!(
            calls <= MAX_CALLS,
            "call budget exhausted; source membership unresolved"
        );
        let mut end = position + 2;
        let mut depth = 1;
        while end < tokens.len() {
            match tokens[end].text {
                "(" => depth += 1,
                ")" => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                break;
            }
            end += 1;
        }
        ensure!(end < tokens.len(), "unterminated build call");
        let span = Span {
            start: token.span.start,
            end: tokens[end].span.end,
        };
        let standalone = tokens.get(end + 1).is_none_or(|token| token.text == "\n");
        let inner = significant(&tokens[position + 2..end]);
        let arguments = separated(&inner, ",");
        if token.text == "load" {
            if !standalone {
                binding_resolution_complete = false;
            }
            let label = arguments
                .first()
                .and_then(|argument| match literal(argument) {
                    Literal::String(value) => Some(value),
                    _ => None,
                });
            let mut loaded = Vec::new();
            for argument in arguments.iter().skip(1) {
                let pair = separated(argument, "=");
                let binding = if pair.len() == 1 {
                    match literal(argument) {
                        Literal::String(name) => Some((name.clone(), name)),
                        _ => None,
                    }
                } else if pair.len() == 2 && pair[0].len() == 1 && identifier(pair[0][0].text) {
                    match literal(&pair[1]) {
                        Literal::String(name) => Some((pair[0][0].text.into(), name)),
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some((alias, name)) = binding {
                    ensure!(identifier(&alias), "invalid load alias");
                    ensure!(
                        !bindings.contains_key(&alias),
                        "duplicate/rebound load alias"
                    );
                    bindings.insert(alias.clone(), (label.clone(), name.clone()));
                    loaded.push((alias, name));
                } else {
                    binding_resolution_complete = false;
                    evidence.unresolved.push(Unresolved {
                        reason: "Unsupported computed load binding".into(),
                        span,
                    });
                }
            }
            if label.is_none() {
                binding_resolution_complete = false;
                evidence.unresolved.push(Unresolved {
                    reason: "Unsupported computed load label".into(),
                    span,
                });
            }
            evidence.loads.push(LoadFact {
                label,
                bindings: loaded,
                span,
            });
        } else if bindings.get(token.text).is_some_and(|(label, name)| {
            label.as_deref() == Some(MACRO_LABEL) && name == "iml_module"
        }) {
            let mut module = ModuleFact {
                call_binding: token.text.into(),
                span,
                attributes: Vec::new(),
                producer_source: "base",
                producer_path: MACRO_PATH,
                producer_verified_load: binding_resolution_complete && standalone,
                source_derived_relationships: Vec::new(),
                membership: "declared source inputs only",
                compiled_membership: None,
                runtime_cases: None,
                unresolved: Vec::new(),
            };
            let mut names = BTreeSet::new();
            for argument in arguments {
                let pair = separated(&argument, "=");
                if pair.len() != 2 || pair[0].len() != 1 || !identifier(pair[0][0].text) {
                    module
                        .unresolved
                        .push("Unsupported positional/computed attribute".into());
                    continue;
                }
                let name = pair[0][0].text.to_owned();
                ensure!(
                    names.insert(name.clone()),
                    "duplicate module attribute: {name}"
                );
                if !ATTRIBUTES.contains(&name.as_str()) {
                    module
                        .unresolved
                        .push(format!("Unknown pinned macro attribute: {name}"));
                }
                let value = literal(&pair[1]);
                if !fully_literal(&value) {
                    module
                        .unresolved
                        .push(format!("Unsupported/computed/glob attribute: {name}"));
                }
                module.attributes.push(Attribute {
                    name,
                    span: Span {
                        start: argument[0].span.start,
                        end: argument[argument.len() - 1].span.end,
                    },
                    value,
                });
            }
            evidence.modules.push(module);
        } else {
            evidence.unresolved.push(Unresolved {
                reason: format!("Unsupported/unverified macro: {}", token.text),
                span,
            });
        }
        position = end + 1;
        if tokens.get(position).is_some_and(|token| token.text != "\n") {
            evidence.unresolved.push(Unresolved {
                reason: "Computed expression after top-level call".into(),
                span: tokens[position].span,
            });
        }
    }
    evidence.literal_coverage_complete = evidence.unresolved.is_empty()
        && evidence
            .modules
            .iter()
            .all(|module| module.unresolved.is_empty());
    Ok(evidence)
}

fn identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn attribute<'a>(module: &'a ModuleFact, name: &str) -> Option<&'a Literal> {
    module
        .attributes
        .iter()
        .find(|attribute| attribute.name == name)
        .map(|attribute| &attribute.value)
}

fn string(value: Option<&Literal>) -> Option<String> {
    match value {
        Some(Literal::String(value)) => Some(value.clone()),
        _ => None,
    }
}

fn strings(value: Option<&Literal>) -> Option<Vec<&str>> {
    match value {
        None | Some(Literal::None) => Some(Vec::new()),
        Some(Literal::List(values)) => values
            .iter()
            .map(|value| match value {
                Literal::String(value) => Some(value.as_str()),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

fn supported_producer_input(name: &str, value: &Literal) -> bool {
    match name {
        "srcs" | "test_srcs" | "exclude" | "resources" | "res_zips" | "test_resources" | "deps"
        | "runtime_deps" | "test_friends" | "visibility" | "module_visibility" | "exports"
        | "javacopts" | "javacopts_from_jps" | "kotlinc_opts" | "test_data" | "test_deps"
        | "test_jvm_flags" | "test_agents" | "data" => {
            matches!(value, Literal::List(_)) && strings(Some(value)).is_some()
        }
        "tags" | "test_tags" | "iml_files" | "compatible_intellij_platforms" => {
            strings(Some(value)).is_some()
        }
        "name" | "test_class" => matches!(value, Literal::String(_)),
        "jvm_target" | "test_timeout" | "test_main_class" | "lint_baseline" | "lint_timeout" => {
            matches!(value, Literal::String(_) | Literal::None)
        }
        "enable_tests"
        | "test_flaky"
        | "lint_enabled"
        | "lint_partial_analysis"
        | "kotlin_use_compose"
        | "kotlin_use_serialization"
        | "generate_coverage_baseline" => matches!(value, Literal::Boolean(_)),
        "test_shard_count" => matches!(value, Literal::Integer(_) | Literal::None),
        "split_test_targets" => matches!(value, Literal::Dictionary(_) | Literal::None),
        "package_prefixes" | "exec_properties" => match value {
            Literal::Dictionary(entries) => entries
                .iter()
                .all(|(_, value)| matches!(value, Literal::String(_))),
            Literal::None if name == "exec_properties" => true,
            _ => false,
        },
        _ => false,
    }
}

fn supported_split_filter(value: &str) -> bool {
    let value = value.strip_prefix('.').unwrap_or(value);
    let value = value.strip_suffix("\\.").unwrap_or(value);
    value
        .split('.')
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_alphanumeric()))
}

fn derive_relationships(module: &mut ModuleFact, package: &str) -> Result<()> {
    if !module.producer_verified_load {
        module.unresolved.push("Unsupported preceding statements may shadow the load binding; producer relationships unresolved".into());
        return Ok(());
    }
    if !module.unresolved.is_empty() {
        module
            .unresolved
            .push("Unsupported arguments prevent producer target derivation".into());
        return Ok(());
    }
    for attribute in &module.attributes {
        if !supported_producer_input(&attribute.name, &attribute.value) {
            module.unresolved.push(format!(
                "Unsupported pinned producer input type: {}",
                attribute.name
            ));
            return Ok(());
        }
    }
    let Some(name) = string(attribute(module, "name")) else {
        module
            .unresolved
            .push("Literal module name required for target relationships".into());
        return Ok(());
    };
    ensure!(
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')),
        "unsafe module label name"
    );
    let prefix = format!("//tools/adt/idea/{package}:{name}");
    module.source_derived_relationships.push(Relationship {
        label: format!("{prefix}_testlib"),
        kind: "test_library",
        suite_class: None,
        suite_jar_property: None,
        aggregate_membership: None,
        split_configuration: None,
        provenance: "pinned iml_module unconditional _iml_test_module_ declaration",
    });
    if attribute(module, "lint_baseline").is_some_and(truthy)
        && strings(attribute(module, "srcs")).is_some_and(|values| values.is_empty())
        && strings(attribute(module, "resources")).is_some_and(|values| values.is_empty())
    {
        module.unresolved.push("Pinned macro rejects a nonempty lint_baseline with literal empty production sources and resources".into());
        return Ok(());
    }
    let sources = strings(attribute(module, "test_srcs"));
    if sources.as_ref().is_some_and(Vec::is_empty) {
        return Ok(());
    }
    let enabled = match attribute(module, "enable_tests") {
        None => Some(true),
        Some(Literal::Boolean(value)) => Some(*value),
        _ => None,
    };
    if enabled == Some(false) {
        for field in [
            "test_tags",
            "test_data",
            "test_jvm_flags",
            "test_agents",
            "test_shard_count",
            "split_test_targets",
            "test_flaky",
        ] {
            if attribute(module, field).is_some_and(truthy) {
                module.unresolved.push(format!(
                    "Pinned macro rejects enable_tests=False with nonempty {field}"
                ));
            }
        }
        return Ok(());
    }
    if sources.is_none() || enabled.is_none() || !module.unresolved.is_empty() {
        module.unresolved.push("Generated test target relationships unresolved because module arguments are unsupported".into());
        return Ok(());
    }
    module.unresolved.push("Declared source directories still require upstream glob/configuration; compiled and runnable membership unverified".into());
    let suite_class = match attribute(module, "test_class") {
        None => Some("com.android.testutils.JarTestSuite".into()),
        value => string(value),
    };
    let jar = Some(format!("-Dtest.suite.jar={name}_test.jar"));
    let splits = attribute(module, "split_test_targets");
    let has_splits = splits.is_some_and(truthy);
    if has_splits && attribute(module, "test_shard_count").is_some_and(truthy) {
        module.unresolved.push(
            "Pinned macro rejects simultaneous nonempty split_test_targets and test_shard_count"
                .into(),
        );
        return Ok(());
    }
    if has_splits && attribute(module, "test_flaky").is_some_and(truthy) {
        module
            .unresolved
            .push("Pinned macro rejects split_test_targets with target-level test_flaky".into());
        return Ok(());
    }
    let mut split_descriptors = Vec::new();
    let mut split_issues = Vec::new();
    let mut catch_all_filters = 0;
    if has_splits {
        let Some(Literal::Dictionary(entries)) = splits else {
            module
                .unresolved
                .push("Unsupported split target dictionary".into());
            return Ok(());
        };
        for (split_name, configuration) in entries {
            ensure!(
                !split_name.is_empty()
                    && split_name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric()
                            || matches!(byte, b'_' | b'-' | b'.')),
                "unsafe split target label"
            );
            if split_name == "all" {
                split_issues.push("Pinned split name all collides with the producer's reserved manual _tests__all target".into());
            }
            let Literal::Dictionary(fields) = configuration else {
                split_issues.push(format!("Unsupported split configuration: {split_name}"));
                continue;
            };
            for (name, value) in fields {
                let supported = match name.as_str() {
                    "tags" | "data" | "additional_jvm_args" => {
                        matches!(value, Literal::List(_)) && strings(Some(value)).is_some()
                    }
                    "shard_count" => matches!(value, Literal::Integer(_) | Literal::None),
                    "flaky" => matches!(value, Literal::Boolean(_) | Literal::None),
                    "timeout" | "test_filter" => {
                        matches!(value, Literal::String(_) | Literal::None)
                    }
                    "exec_properties" => match value {
                        Literal::None => true,
                        Literal::Dictionary(entries) => entries
                            .iter()
                            .all(|(_, value)| matches!(value, Literal::String(_))),
                        _ => false,
                    },
                    _ => true,
                };
                if !supported {
                    split_issues.push(format!(
                        "Unsupported pinned split input type: {split_name}.{name}"
                    ));
                }
            }
            match fields
                .iter()
                .find(|(name, _)| name == "test_filter")
                .map(|(_, value)| value)
            {
                None | Some(Literal::None) => catch_all_filters += 1,
                Some(Literal::String(value)) if value.is_empty() => catch_all_filters += 1,
                Some(Literal::String(value)) if !supported_split_filter(value) => split_issues
                    .push(format!(
                        "Unsupported or producer-rejected split filter: {split_name}"
                    )),
                _ => {}
            }
            let tags = fields
                .iter()
                .find(|(name, _)| name == "tags")
                .map(|(_, value)| value);
            let membership = strings(tags).map(|tags| !tags.contains(&"manual"));
            split_descriptors.push((split_name.clone(), membership));
        }
    }
    if catch_all_filters > 1 {
        split_issues.push("Pinned producer rejects multiple catch-all split filters".into());
    }
    if !split_issues.is_empty() {
        module.unresolved.extend(split_issues);
        return Ok(());
    }
    let mut generated = Vec::new();
    let make = |label, kind, membership, split| Relationship {
        label,
        kind,
        suite_class: suite_class.clone(),
        suite_jar_property: jar.clone(),
        aggregate_membership: membership,
        split_configuration: split,
        provenance: "pinned iml_module/_gen_tests/_gen_split_tests source; not an observed configured target",
    };
    if has_splits {
        generated.push(make(
            format!("{prefix}_tests__all"),
            "manual_unsplit_test",
            Some(false),
            None,
        ));
        for (split_name, membership) in split_descriptors {
            generated.push(make(
                format!("{prefix}_tests__{split_name}"),
                "split_test",
                membership,
                Some(split_name),
            ));
        }
        generated.push(make(
            format!("{prefix}_tests"),
            "aggregate_test_suite",
            None,
            None,
        ));
    } else {
        generated.push(make(format!("{prefix}_tests"), "unsplit_test", None, None));
    }
    module.source_derived_relationships.extend(generated);
    Ok(())
}

fn truthy(value: &Literal) -> bool {
    match value {
        Literal::None | Literal::Boolean(false) | Literal::Integer(0) => false,
        Literal::List(values) => !values.is_empty(),
        Literal::Dictionary(entries) => !entries.is_empty(),
        Literal::String(value) => !value.is_empty(),
        _ => true,
    }
}

fn protocol_facts(originals: &BTreeMap<(String, String), String>) -> Result<Vec<ProtocolFact>> {
    let runner = "testutils/src/main/java/com/android/testutils/JarTestSuiteRunner.java";
    let group = "testutils/src/main/java/com/android/testutils/TestGroup.java";
    type ProtocolSpec = (
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        &'static [&'static str],
    );
    let specs: Vec<ProtocolSpec> = vec![
        (
            "producer_testlib",
            "base",
            MACRO_PATH,
            "The pinned macro declares a test library even when test_srcs is empty",
            &[
                "name = name + \"_testlib\"",
                "if not test_srcs:\n        return",
            ],
        ),
        (
            "producer_defaults",
            "base",
            MACRO_PATH,
            "Literal source defaults; no configured Bazel target evaluation",
            &[
                "enable_tests = True,",
                "test_class = \"com.android.testutils.JarTestSuite\",",
                "test_srcs = [],",
                "exclude = [],",
            ],
        ),
        (
            "producer_inputs",
            "base",
            MACRO_PATH,
            "Sources/resources/exclusions feed the upstream splitter; dependency order remains meaningful",
            &[
                "split_test_srcs = split_srcs(test_srcs, test_resources, exclude)",
                "test_deps = deps + test_deps,",
            ],
        ),
        (
            "producer_jar_binding",
            "base",
            MACRO_PATH,
            "Configured suite property binds the generated test jar suffix; boot classpath and agents remain producer facts",
            &[
                "\"test_jar\": \"%{name}_test.jar\"",
                "jvm_flags = test_jvm_flags + [\"-Dtest.suite.jar=\" + name + \"_test.jar\"]",
                "jvm_flags += get_xbootclasspath_jvm_flags(intellij_platform)",
                "for test_agent in test_agents:",
            ],
        ),
        (
            "producer_split_manual",
            "base",
            MACRO_PATH,
            "Manual __all and manual split children are excluded from aggregate children; target-level manual tags are applied afterward",
            &[
                "name = name + \"_tests__all\"",
                "if \"manual\" not in tags:\n            split_tests.append(test_name)",
                "if test_tags:\n            tags += test_tags",
                "tests = split_tests,",
            ],
        ),
        (
            "producer_split_filters",
            "base",
            MACRO_PATH,
            "Split filter/exclusion flags depend on all split definitions and their additional JVM arguments",
            &[
                "def _gen_split_test_jvm_flags(",
                "def _gen_split_test_excludes(",
                "other_jvm_args = other.get(\"additional_jvm_args\", default = [])",
            ],
        ),
        (
            "producer_shard_conflict",
            "base",
            MACRO_PATH,
            "Simultaneous split and shard configuration is rejected",
            &[
                "if split_test_targets and test_shard_count:",
                "test_shard_count and split_test_targets should not both be specified",
            ],
        ),
        (
            "producer_literal_constraints",
            "base",
            MACRO_PATH,
            "Literal-empty lint inputs and multiple catch-all split filters have explicit producer rejection paths; unsupported input shapes remain unresolved",
            &[
                "if lint_baseline and not lint_srcs:",
                "lint_baseline set for iml_module that has no sources",
                "def _validate_split_test_filter(test_filter):",
                "Cannot have more than one split_test_targets without a 'test_filter'.",
            ],
        ),
        (
            "runner_suite_jar",
            "base",
            runner,
            "Jar suffix missing under Bazel is an error; outside Bazel it returns no suite children",
            &[
                "System.getProperty(\"test.suite.jar\")",
                "if (TestUtils.runningFromBazel())",
                "return new Class<?>[0];",
            ],
        ),
        (
            "runner_filters",
            "base",
            runner,
            "Class-name include/exclude regexes filter discovered classes; no runtime case expansion is observed here",
            &[
                "System.getProperty(\"test_filter\")",
                "System.getProperty(\"test_exclude_filter\")",
                "No tests found in class path using suffix:",
            ],
        ),
        (
            "runner_finalizer",
            "base",
            runner,
            "Inherited finalizer is in descriptions, exempt from ordinary child filtering/sharding/sorting, and runs after delegate success when no failure was reported",
            &[
                "@Inherited",
                "suiteClass.getAnnotation(FinalizerTest.class)",
                "delegate.evaluate();",
                "if (finalizerTest != null && succeeded[0])",
                "description.addChild(finalizerTest.getDescription());",
            ],
        ),
        (
            "runner_classpath",
            "base",
            group,
            "Compiled .class loading and manifest Class-Path scanning require the exact external classpath",
            &[
                "addManifestClassPath(path, paths);",
                "getValue(\"Class-Path\")",
                "Class<?> aClass = loader.loadClass(className);",
            ],
        ),
        (
            "runner_junit3",
            "base",
            group,
            "Nonabstract TestCase/TestSuite assignability is checked; public inherited methods carrying JUnit4 Test/Ignore annotations cause rejection",
            &[
                "TestCase.class.isAssignableFrom(aClass)",
                "TestSuite.class.isAssignableFrom(aClass)",
                "!Modifier.isAbstract(aClass.getModifiers())",
                "for (Method method : aClass.getMethods())",
                "method.getAnnotation(Ignore.class)",
                "method.getAnnotation(Test.class)",
            ],
        ),
        (
            "runner_junit4",
            "base",
            group,
            "Nonabstract classes qualify by RunWith or Test annotations on public inherited methods; this does not resolve an input hierarchy",
            &[
                "aClass.isAnnotationPresent(RunWith.class)",
                "Arrays.stream(aClass.getMethods()).anyMatch(hasTestAnnotation)",
            ],
        ),
        (
            "runner_ignored_dispatch",
            "base",
            "testutils/src/main/java/com/android/testutils/DelegatingRunnerBuilder.java",
            "ignored.tests.only chooses an external ignored-test runner builder",
            &[
                "Boolean.getBoolean(\"ignored.tests.only\")",
                "ignoredTestsBuilder.runnerForClass(testClass)",
            ],
        ),
        (
            "suite_inherited_finalizer",
            "idea",
            "adt-testutils/src/main/java/com/android/tools/tests/IdeaTestSuiteBase.java",
            "Suite infrastructure declares a finalizer; effective inherited runtime dispatch is unverified",
            &["@JarTestSuiteRunner.FinalizerTest(LastInIdeaTestSuite.class)"],
        ),
        (
            "adt_suite",
            "idea",
            "adt-ui/src/test/java/com/android/tools/adtui/AdtUiTestSuite.java",
            "ADT suite declares the custom runner and base class, distinct from Gradle-sync's configured suite",
            &[
                "@RunWith(JarTestSuiteRunner.class)",
                "AdtUiTestSuite extends IdeaTestSuiteBase",
            ],
        ),
        (
            "parameter_provider",
            "idea",
            "adt-ui/src/test/java/com/android/tools/adtui/common/ColorPaletteManagerTest.kt",
            "Provider values and conditional assumptions are source evidence, not expanded or successful runtime cases",
            &[
                "@JvmStatic @Parameterized.Parameters fun data() = listOf(false, true)",
                "assumeFalse(isDarkMode)",
            ],
        ),
        (
            "junit3_shaped_source",
            "idea",
            "android/gradle/testSrc/com/android/tools/idea/gradle/project/GradleModuleImportTest.java",
            "HeavyPlatformTestCase ancestry requires matching IntelliJ hierarchy; names alone cannot establish membership",
            &[
                "GradleModuleImportTest extends HeavyPlatformTestCase",
                "public void test",
            ],
        ),
    ];
    let mut facts = Vec::new();
    for (id, source, path, meaning, needles) in specs {
        let content = original(originals, source, path)?;
        let mut observations = Vec::new();
        for needle in needles {
            let matches: Vec<_> = content.match_indices(needle).collect();
            ensure!(
                !matches.is_empty(),
                "pinned producer/runner witness missing: {id}: {needle}"
            );
            for (start, excerpt) in matches {
                observations.push(Observation {
                    span: Span {
                        start,
                        end: start + excerpt.len(),
                    },
                    excerpt: excerpt.into(),
                });
            }
        }
        facts.push(ProtocolFact {
            id,
            source,
            path,
            meaning,
            observations,
            runtime_verified: false,
        });
    }
    Ok(facts)
}

struct BoundedWriter(Vec<u8>);

impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let required = self
            .0
            .len()
            .checked_add(bytes.len())
            .filter(|count| *count <= MAX_OUTPUT_BYTES)
            .ok_or_else(|| std::io::Error::other("evidence output budget exhausted"))?;
        self.0
            .try_reserve(required - self.0.len())
            .map_err(std::io::Error::other)?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn encode(evidence: &Evidence) -> Result<Vec<u8>> {
    let mut writer = BoundedWriter(Vec::new());
    serde_json::to_writer_pretty(&mut writer, evidence)?;
    writer.write_all(b"\n")?;
    Ok(writer.0)
}

fn publish(path: &Path, check: bool, bytes: &[u8]) -> Result<()> {
    if check {
        ensure!(
            read_bounded(path, MAX_OUTPUT_BYTES as u64)? == bytes,
            "existing evidence differs; check never modifies it"
        );
    } else {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .with_context(|| format!("create new evidence {}", path.display()))?
            .write_all(bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/reference_membership/sources")
    }

    fn selection() -> Result<Selection> {
        Ok(serde_json::from_str(CATALOG)?)
    }

    fn module(arguments: &str) -> Result<BuildEvidence> {
        let mut evidence = parse_build(&format!(
            "load(\"{MACRO_LABEL}\", module = \"iml_module\")\nmodule({arguments})\n"
        ))?;
        for module in &mut evidence.modules {
            derive_relationships(module, "synthetic")?;
        }
        Ok(evidence)
    }

    #[test]
    fn complete_originals_keep_distinct_module_targets_and_unknown_runtime() -> Result<()> {
        let selection = selection()?;
        let originals = load_originals(&fixtures(), &selection)?;
        let evidence = discover(selection, CATALOG.as_bytes(), &originals)?;
        assert_eq!(evidence.files.len(), 16);
        assert!(!evidence.global_census_complete);
        assert_eq!(evidence.effective_runtime_cases, None);
        assert_eq!(evidence.new_original_behavior_credit, 0);
        assert_eq!(evidence.applicability, "unresolved");
        assert!(!evidence.archive_membership_reverified_by_this_command);
        assert_eq!(evidence.build_files.len(), 2);
        let adt = &evidence.build_files[0];
        assert_eq!(adt.modules.len(), 1);
        assert!(adt.literal_coverage_complete);
        assert_eq!(adt.modules[0].source_derived_relationships.len(), 2);
        assert_eq!(
            adt.modules[0].source_derived_relationships[1]
                .suite_class
                .as_deref(),
            Some("com.android.tools.adtui.AdtUiTestSuite")
        );
        let sync = &evidence.build_files[1];
        assert_eq!(sync.modules.len(), 2);
        assert!(!sync.literal_coverage_complete);
        assert!(
            sync.unresolved
                .iter()
                .any(|issue| issue.reason.contains("maven_repository"))
        );
        assert_eq!(sync.modules[0].source_derived_relationships.len(), 1);
        assert_eq!(sync.modules[1].source_derived_relationships.len(), 2);
        assert_eq!(
            sync.modules[1].source_derived_relationships[1]
                .suite_class
                .as_deref(),
            Some(
                "com.android.tools.idea.projectsystem.gradle.sync.GradleProjectSystemSyncTestSuite"
            )
        );
        for build in &evidence.build_files {
            for module in &build.modules {
                assert_eq!(module.compiled_membership, None);
                assert_eq!(module.runtime_cases, None);
                assert!(module.producer_verified_load);
            }
        }
        Ok(())
    }

    #[test]
    fn complete_original_attributes_preserve_order_and_exact_byte_spans() -> Result<()> {
        let originals = load_originals(&fixtures(), &selection()?)?;
        let source = original(&originals, "idea", "adt-ui/BUILD")?;
        let evidence = parse_build(source)?;
        let module = &evidence.modules[0];
        assert!(
            source
                .get(module.span.start..module.span.end)
                .is_some_and(|span| span.starts_with("iml_module("))
        );
        let dependencies =
            strings(attribute(module, "test_deps")).context("literal dependencies")?;
        assert_eq!(
            dependencies.first().copied(),
            Some("//tools/adt/idea/.idea/libraries:junit4")
        );
        assert_eq!(
            dependencies.last().copied(),
            Some("//tools/adt/idea/adt-testutils:intellij.android.adt.testutils")
        );
        assert_eq!(
            strings(attribute(module, "test_resources")),
            Some(vec!["src/test/resources"])
        );
        assert_eq!(
            strings(attribute(module, "resources")),
            Some(vec!["resources"])
        );
        assert_eq!(
            strings(attribute(module, "test_srcs")),
            Some(vec!["src/test/java"])
        );
        for attribute in &module.attributes {
            let span = source
                .get(attribute.span.start..attribute.span.end)
                .context("attribute byte span")?;
            assert!(span.starts_with(&attribute.name));
            assert!(span.contains('='));
        }
        Ok(())
    }

    #[test]
    fn complete_runner_witnesses_are_hash_bound_source_facts() -> Result<()> {
        let originals = load_originals(&fixtures(), &selection()?)?;
        let facts = protocol_facts(&originals)?;
        for id in [
            "producer_testlib",
            "producer_split_filters",
            "producer_shard_conflict",
            "runner_classpath",
            "runner_junit3",
            "runner_junit4",
            "runner_finalizer",
            "suite_inherited_finalizer",
            "parameter_provider",
            "junit3_shaped_source",
        ] {
            assert!(facts.iter().any(|fact| fact.id == id));
        }
        for fact in &facts {
            assert!(!fact.runtime_verified);
            let source = original(&originals, fact.source, fact.path)?;
            for observation in &fact.observations {
                assert_eq!(
                    source.get(observation.span.start..observation.span.end),
                    Some(observation.excerpt.as_str())
                );
            }
        }
        let mut missing_witness = originals;
        missing_witness.insert(("base".into(), MACRO_PATH.into()), String::new());
        assert!(protocol_facts(&missing_witness).is_err());
        Ok(())
    }

    #[test]
    fn selection_rejects_each_pinned_identity_change() -> Result<()> {
        for field in [
            "source",
            "revision",
            "archive_sha256",
            "path",
            "sha256",
            "license",
        ] {
            let mut value: serde_json::Value = serde_json::from_str(CATALOG)?;
            value["files"][0][field] = "wrong".into();
            let selection: Selection = serde_json::from_value(value)?;
            assert!(validate_selection(&selection).is_err(), "changed {field}");
        }
        let mut selection = selection()?;
        selection.files[0].bytes += 1;
        assert!(validate_selection(&selection).is_err());
        Ok(())
    }

    #[test]
    fn selection_rejects_duplicates_missing_members_and_unknown_fields() -> Result<()> {
        let mut value: serde_json::Value = serde_json::from_str(CATALOG)?;
        value["files"][1] = value["files"][0].clone();
        assert!(validate_selection(&serde_json::from_value(value)?).is_err());
        let mut selection = selection()?;
        assert!(selection.files.pop().is_some());
        assert!(validate_selection(&selection).is_err());
        assert!(
            serde_json::from_str::<Selection>(
                r#"{"schema_version":1,"scope":"x","files":[],"ignored":true}"#
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn paths_and_nonregular_inputs_are_rejected() -> Result<()> {
        for path in [
            "",
            "/absolute",
            "a//b",
            "a/../b",
            "./file",
            "a/",
            "a\\b",
            "C:drive",
            "a\0b",
        ] {
            assert!(safe_path(path).is_err(), "{path:?}");
        }
        safe_path("adt-ui/src/test/java/Test.kt")?;
        let temporary = tempfile::tempdir()?;
        assert!(read_bounded(temporary.path(), 100).is_err());
        let selection = selection()?;
        assert!(load_originals(temporary.path(), &selection).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn source_root_intermediate_and_leaf_symlinks_are_rejected() -> Result<()> {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("root");
        fs::create_dir_all(root.join("idea/adt-ui"))?;
        let source = root.join("idea/adt-ui/BUILD");
        fs::write(&source, b"retained")?;
        regular_source(&root, "idea", "adt-ui/BUILD")?;
        let alias = temporary.path().join("alias");
        symlink(&root, &alias)?;
        assert!(regular_source(&alias, "idea", "adt-ui/BUILD").is_err());
        fs::remove_file(&alias)?;
        symlink(root.join("idea"), &alias)?;
        assert!(regular_source(temporary.path(), "alias", "adt-ui/BUILD").is_err());
        let leaf = root.join("idea/adt-ui/alias");
        symlink(&source, &leaf)?;
        assert!(regular_source(&root, "idea", "adt-ui/alias").is_err());
        assert!(read_bounded(&leaf, 100).is_err());
        Ok(())
    }

    #[test]
    fn changed_original_is_rejected_without_rewriting_it() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let selection = selection()?;
        for file in &selection.files {
            let source = fixtures().join(&file.source).join(&file.path);
            let destination = temporary.path().join(&file.source).join(&file.path);
            fs::create_dir_all(destination.parent().context("fixture parent")?)?;
            fs::copy(source, destination)?;
        }
        let changed = temporary.path().join("idea/adt-ui/BUILD");
        fs::write(&changed, b"changed")?;
        assert!(load_originals(temporary.path(), &selection).is_err());
        assert_eq!(fs::read(changed)?, b"changed");
        Ok(())
    }

    #[test]
    fn computed_glob_conditional_and_shadowing_never_claim_absence() -> Result<()> {
        let computed = module("name = \"sample\", test_srcs = glob([\"**/*.kt\"])")?;
        assert!(!computed.literal_coverage_complete);
        assert!(computed.modules[0].source_derived_relationships.is_empty());
        assert!(
            computed.modules[0]
                .unresolved
                .iter()
                .any(|issue| issue.contains("glob"))
        );
        let conditional = parse_build(&format!(
            "load(\"{MACRO_LABEL}\", \"iml_module\")\nif True:\n    iml_module(name = \"conditional\")\n"
        ))?;
        assert!(!conditional.literal_coverage_complete);
        assert!(conditional.modules.is_empty());
        let mut shadow = parse_build(&format!(
            "load(\"{MACRO_LABEL}\", \"iml_module\")\niml_module = replacement\niml_module(name = \"shadow\")\n"
        ))?;
        derive_relationships(&mut shadow.modules[0], "synthetic")?;
        assert!(!shadow.modules[0].producer_verified_load);
        assert!(shadow.modules[0].source_derived_relationships.is_empty());
        Ok(())
    }

    #[test]
    fn unverified_macro_load_and_inline_expression_remain_unresolved() -> Result<()> {
        let wrong = parse_build(
            "load(\"//other:macro.bzl\", \"iml_module\")\niml_module(name = \"wrong\")\n",
        )?;
        assert!(wrong.modules.is_empty());
        assert!(!wrong.literal_coverage_complete);
        let mut inline = parse_build(&format!(
            "load(\"{MACRO_LABEL}\", \"iml_module\"); iml_module(name = \"inline\")\n"
        ))?;
        assert!(!inline.literal_coverage_complete);
        if let Some(module) = inline.modules.first_mut() {
            derive_relationships(module, "synthetic")?;
            assert!(!module.producer_verified_load);
            assert!(module.source_derived_relationships.is_empty());
        }
        Ok(())
    }

    #[test]
    fn invalid_duplicates_and_unsafe_labels_fail_explicitly() -> Result<()> {
        assert!(module("name = \"a\", name = \"b\"").is_err());
        assert!(
            parse_build(&format!(
                "load(\"{MACRO_LABEL}\", \"iml_module\", \"iml_module\")\n"
            ))
            .is_err()
        );
        assert!(module("name = \"../unsafe\"").is_err());
        assert!(
            module(
                "name = \"safe\", test_srcs = [\"testSrc\"], split_test_targets = {\"../bad\": {}}"
            )
            .is_err()
        );
        for arguments in [
            "name = \"safe\", unexpected = True",
            "name = \"safe\", test_srcs = None",
            "name = \"safe\", test_srcs = [\"testSrc\"], test_shard_count = \"wrong\"",
            "name = \"safe\", test_class = 2",
        ] {
            let evidence = module(arguments)?;
            assert!(evidence.modules[0].source_derived_relationships.is_empty());
            assert!(!evidence.modules[0].unresolved.is_empty());
        }
        Ok(())
    }

    #[test]
    fn literal_unicode_comments_trailing_commas_and_container_order() -> Result<()> {
        let source = "[\"é # ( , \", True, False, None, 42, {\"second\": [\"x\"], \"first\": []},] # ignored\n";
        let tokens = lex(source, MAX_TOKENS)?;
        let value = literal(&significant(&tokens));
        assert!(fully_literal(&value));
        let Literal::List(values) = value else {
            bail!("list expected")
        };
        assert!(matches!(&values[0], Literal::String(value) if value == "é # ( , "));
        let Literal::Dictionary(entries) = &values[5] else {
            bail!("dictionary expected")
        };
        assert_eq!(
            entries
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["second", "first"]
        );
        for source in [
            "{\"a\": 1, \"a\": 2}",
            "[value for value in values]",
            "[\"one\"] + [\"two\"]",
            "-1",
        ] {
            assert!(!fully_literal(&literal(&significant(&lex(
                source, MAX_TOKENS
            )?))));
        }
        Ok(())
    }

    #[test]
    fn split_manual_membership_precedes_target_level_tags() -> Result<()> {
        let evidence = module(
            "name = \"split\", test_srcs = [\"tests\"], test_tags = [\"manual\"], split_test_targets = {\"automatic\": {\"test_filter\": \"com.sample\", \"shard_count\": 3}, \"manual\": {\"tags\": [\"manual\"], \"test_filter\": \"com.other\"}}",
        )?;
        let relationships = &evidence.modules[0].source_derived_relationships;
        assert_eq!(relationships.len(), 5);
        assert_eq!(relationships[1].kind, "manual_unsplit_test");
        assert_eq!(relationships[1].aggregate_membership, Some(false));
        assert_eq!(relationships[2].aggregate_membership, Some(true));
        assert_eq!(relationships[3].aggregate_membership, Some(false));
        assert_eq!(relationships[4].kind, "aggregate_test_suite");
        assert_eq!(
            relationships[2].split_configuration.as_deref(),
            Some("automatic")
        );
        assert_eq!(
            relationships[2].suite_jar_property.as_deref(),
            Some("-Dtest.suite.jar=split_test.jar")
        );
        Ok(())
    }

    #[test]
    fn unsupported_split_shapes_never_claim_automatic_membership() -> Result<()> {
        for configuration in [
            "{\"tags\": None}",
            "{\"tags\": 2}",
            "{\"data\": None}",
            "{\"data\": [1]}",
            "{\"additional_jvm_args\": None}",
            "{\"additional_jvm_args\": \"-ea\"}",
            "{\"test_filter\": True}",
            "{\"shard_count\": \"two\"}",
            "{\"exec_properties\": {\"key\": 1}}",
            "None",
        ] {
            let evidence = module(&format!(
                "name = \"invalid\", test_srcs = [\"tests\"], split_test_targets = {{\"child\": {configuration}}}"
            ))?;
            let module = &evidence.modules[0];
            assert_eq!(
                module.source_derived_relationships.len(),
                1,
                "{configuration}"
            );
            assert!(
                module
                    .unresolved
                    .iter()
                    .any(|issue| issue.contains("Unsupported") && issue.contains("child")),
                "{configuration}"
            );
            assert!(
                !module
                    .source_derived_relationships
                    .iter()
                    .any(|relationship| relationship.aggregate_membership == Some(true))
            );
        }
        Ok(())
    }

    #[test]
    fn empty_disabled_unsplit_and_split_conflicts_match_producer_gates() -> Result<()> {
        let empty = module("name = \"empty\"")?;
        assert_eq!(empty.modules[0].source_derived_relationships.len(), 1);
        let disabled =
            module("name = \"disabled\", test_srcs = [\"tests\"], enable_tests = False")?;
        assert_eq!(disabled.modules[0].source_derived_relationships.len(), 1);
        for arguments in [
            "name = \"bad\", test_srcs = [\"tests\"], enable_tests = False, test_data = [\"data\"]",
            "name = \"bad\", test_srcs = [\"tests\"], split_test_targets = {\"a\": {}}, test_shard_count = 2",
            "name = \"bad\", test_srcs = [\"tests\"], split_test_targets = {\"a\": {}}, test_flaky = True",
        ] {
            let evidence = module(arguments)?;
            assert_eq!(evidence.modules[0].source_derived_relationships.len(), 1);
            assert!(
                evidence.modules[0]
                    .unresolved
                    .iter()
                    .any(|issue| issue.contains("rejects"))
            );
        }
        let unsplit = module(
            "name = \"single\", test_srcs = [\"tests\"], test_shard_count = 4, test_jvm_flags = [\"-ea\", \"-Dmode=test\"]",
        )?;
        assert_eq!(
            unsplit.modules[0].source_derived_relationships[1].kind,
            "unsplit_test"
        );
        assert_eq!(
            strings(attribute(&unsplit.modules[0], "test_jvm_flags")),
            Some(vec!["-ea", "-Dmode=test"])
        );
        Ok(())
    }

    #[test]
    fn literal_split_filter_rejections_and_reserved_names_are_unresolved() -> Result<()> {
        for splits in [
            r#"{"invalid": {"test_filter": "("}}"#,
            r#"{"one": {}, "two": {}}"#,
            r#"{"one": {"test_filter": None}, "two": {"test_filter": ""}}"#,
            r#"{"all": {"test_filter": "com.sample"}}"#,
            r#"{"unicode": {"test_filter": "é.sample"}}"#,
        ] {
            let evidence = module(&format!(
                "name = \"invalid\", test_srcs = [\"tests\"], split_test_targets = {splits}"
            ))?;
            let module = &evidence.modules[0];
            assert_eq!(module.source_derived_relationships.len(), 1, "{splits}");
            assert!(
                module
                    .unresolved
                    .iter()
                    .any(|issue| issue.contains("filter") || issue.contains("reserved")),
                "{splits}"
            );
        }
        assert!(supported_split_filter("com.sample"));
        assert!(supported_split_filter(".com.sample\\."));
        assert!(!supported_split_filter("com.sample_name"));
        assert!(!supported_split_filter("com..sample"));
        let valid = module(
            r#"name = "valid", test_srcs = ["tests"], split_test_targets = {"fallback": {}, "specific": {"test_filter": "com.sample"}}"#,
        )?;
        assert_eq!(valid.modules[0].source_derived_relationships.len(), 5);
        Ok(())
    }

    #[test]
    fn unsupported_consumed_production_shapes_withhold_relationships() -> Result<()> {
        for argument in [
            "srcs = 1",
            "resources = None",
            "javacopts = None",
            "runtime_deps = 2",
            "res_zips = [1]",
            "test_friends = None",
            "exports = True",
            "package_prefixes = {\"src\": 1}",
            "enable_tests = None",
            "lint_enabled = 2",
            "kotlin_use_compose = \"yes\"",
            "compatible_intellij_platforms = 3",
        ] {
            let evidence = module(&format!(
                "name = \"invalid\", test_srcs = [\"tests\"], {argument}"
            ))?;
            let module = &evidence.modules[0];
            assert!(module.source_derived_relationships.is_empty(), "{argument}");
            assert!(
                module
                    .unresolved
                    .iter()
                    .any(|issue| issue.contains("Unsupported pinned producer input type")),
                "{argument}"
            );
        }
        Ok(())
    }

    #[test]
    fn lint_baseline_empty_production_gate_precedes_test_children() -> Result<()> {
        let rejected = module(
            "name = \"invalid\", test_srcs = [\"tests\"], lint_baseline = \"baseline.xml\"",
        )?;
        assert_eq!(rejected.modules[0].source_derived_relationships.len(), 1);
        assert!(
            rejected.modules[0]
                .unresolved
                .iter()
                .any(|issue| issue.contains("lint_baseline"))
        );
        let potential = module(
            "name = \"potential\", srcs = [\"src\"], test_srcs = [\"tests\"], lint_baseline = \"baseline.xml\"",
        )?;
        assert_eq!(potential.modules[0].source_derived_relationships.len(), 2);
        assert_eq!(potential.modules[0].compiled_membership, None);
        let disabled =
            module("name = \"disabled\", test_srcs = [\"tests\"], lint_baseline = \"\"")?;
        assert_eq!(disabled.modules[0].source_derived_relationships.len(), 2);
        Ok(())
    }

    #[test]
    fn lexical_depth_token_call_and_source_bounds_fail_explicitly() -> Result<()> {
        assert!(lex("one two three", 2).is_err());
        assert!(
            lex(
                &format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1)),
                MAX_TOKENS
            )
            .is_err()
        );
        assert!(lex(&"x".repeat(MAX_SOURCE_BYTES as usize + 1), MAX_TOKENS).is_err());
        assert!(parse_build(&"unsupported()\n".repeat(MAX_CALLS + 1)).is_err());
        for source in ["([)]", "[", "\"unterminated", "\"unsupported\\xescape\""] {
            assert!(lex(source, MAX_TOKENS).is_err());
        }
        Ok(())
    }

    #[test]
    fn input_and_output_budgets_are_enforced_at_boundary() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let input = temporary.path().join("input");
        fs::write(&input, b"12345")?;
        assert_eq!(read_bounded(&input, 5)?, b"12345");
        assert!(read_bounded(&input, 4).is_err());
        let mut writer = BoundedWriter(Vec::new());
        assert!(writer.write_all(&vec![0; MAX_OUTPUT_BYTES + 1]).is_err());
        assert!(writer.0.is_empty());
        Ok(())
    }

    #[test]
    fn deterministic_create_check_never_overwrites_original_or_existing_output() -> Result<()> {
        let selection = selection()?;
        let originals = load_originals(&fixtures(), &selection)?;
        let evidence = discover(selection, CATALOG.as_bytes(), &originals)?;
        let bytes = encode(&evidence)?;
        assert_eq!(bytes, encode(&evidence)?);
        let temporary = tempfile::tempdir()?;
        let output = temporary.path().join("membership.json");
        publish(&output, false, &bytes)?;
        publish(&output, true, &bytes)?;
        assert!(publish(&output, false, b"replacement").is_err());
        assert!(publish(&output, true, b"different").is_err());
        assert_eq!(fs::read(output)?, bytes);
        assert!(publish(&temporary.path().join("missing"), true, &bytes).is_err());
        Ok(())
    }

    #[test]
    fn cli_requires_all_paths_and_preserves_explicit_check_mode() -> Result<()> {
        assert!(AndroidReferenceMembershipArgs::try_parse_from(["membership"]).is_err());
        let args = AndroidReferenceMembershipArgs::try_parse_from([
            "membership",
            "--source-root",
            "sources",
            "--selection",
            "selection.json",
            "--output",
            "evidence.json",
            "--check",
        ])?;
        assert!(args.check);
        assert_eq!(args.source_root, PathBuf::from("sources"));
        Ok(())
    }
}
