#![allow(clippy::disallowed_methods, reason = "tooling is exempt")]

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};
use clap::Parser;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_SELECTION_BYTES: u64 = 256 * 1024;
const MAX_FILES: usize = 64;
const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TOKENS: usize = 100_000;
const MAX_DECLARATIONS: usize = 512;
const MAX_CLASSES: usize = 128;
const MAX_HEADER_TOKENS: usize = 1024;
const MAX_HEADER_BYTES: usize = 16 * 1024;

struct MetadataBudget {
    used: usize,
    limit: usize,
}

impl MetadataBudget {
    fn new(limit: usize) -> Self {
        Self { used: 0, limit }
    }

    fn charge(&mut self, additional: usize) -> Result<()> {
        let next = self.used.checked_add(additional).context(
            "metadata byte count overflow; declaration evidence unresolved and not published",
        )?;
        ensure!(
            next <= self.limit,
            "selected declaration metadata budget exhausted; declaration evidence unresolved and not published"
        );
        self.used = next;
        Ok(())
    }

    fn owned(&mut self, value: &str) -> Result<String> {
        self.charge(value.len())?;
        Ok(value.to_owned())
    }

    fn annotation(&mut self, fact: &AnnotationFact) -> Result<AnnotationFact> {
        Ok(AnnotationFact {
            spelling: self.owned(&fact.spelling)?,
            use_site_target: fact
                .use_site_target
                .as_deref()
                .map(|target| self.owned(target))
                .transpose()?,
            span: fact.span,
        })
    }

    fn joined(&mut self, prefix: &str, separator: char, name: &str) -> Result<String> {
        let length = prefix
            .len()
            .checked_add(separator.len_utf8())
            .and_then(|length| length.checked_add(name.len()))
            .context("qualified metadata byte count overflow")?;
        self.charge(length)?;
        let mut value = String::with_capacity(length);
        value.push_str(prefix);
        value.push(separator);
        value.push_str(name);
        Ok(value)
    }
}

struct BoundedJsonWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedJsonWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn finish(mut self) -> Result<Vec<u8>> {
        self.write_all(b"\n")?;
        Ok(self.bytes)
    }
}

impl Write for BoundedJsonWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let required = self
            .bytes
            .len()
            .checked_add(buffer.len())
            .filter(|required| *required <= self.limit)
            .ok_or_else(|| std::io::Error::other("evidence exceeds the bounded output budget"))?;
        if required > self.bytes.capacity() {
            let capacity = required
                .max(self.bytes.capacity().saturating_mul(2))
                .min(self.limit);
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(std::io::Error::other)?;
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Parser)]
pub struct AndroidJunit4DeclarationsArgs {
    #[arg(long, default_value = ".")]
    repository: PathBuf,
    /// Directory containing the selected original sources, with source-id/path layout.
    #[arg(long)]
    source_root: PathBuf,
    /// Explicit, hash-bound source selection. This is not exhaustive reference discovery.
    #[arg(long)]
    selection: PathBuf,
    /// External evidence file to create. Existing output is never overwritten.
    #[arg(long)]
    output: PathBuf,
    /// Require byte-identical existing evidence; never update it.
    #[arg(long)]
    check: bool,
}

#[derive(Deserialize)]
struct Manifest {
    schema_version: u32,
    baseline: String,
    sources: Vec<ReferenceSource>,
}

#[derive(Deserialize)]
struct ReferenceSource {
    id: String,
    revision: String,
    repository: String,
    coverage: String,
    archive: ReferenceArchive,
}

#[derive(Deserialize, Serialize)]
struct ReferenceArchive {
    path: String,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    schema_version: u32,
    scope: String,
    files: Vec<SelectedFile>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SelectedFile {
    source: String,
    revision: String,
    path: String,
    sha256: String,
    bytes: u64,
    fixture: String,
    license: String,
}

#[derive(Serialize)]
struct Evidence {
    schema_version: u32,
    baseline: String,
    scope: String,
    discovery: &'static str,
    selection_sha256: String,
    reference_manifest_sha256: String,
    selected_archive_membership_reverified: bool,
    effective_test_census_complete: bool,
    declaration_coverage_complete: bool,
    new_behavioral_parity_credit: usize,
    effective_runtime_cases: Option<usize>,
    files: Vec<FileEvidence>,
    unresolved: Vec<&'static str>,
}

#[derive(Serialize)]
struct FileEvidence {
    selected: SelectedFile,
    repository: String,
    source_coverage: String,
    archive_sha256: String,
    source_facts: SourceFacts,
}

#[derive(Debug, Default, Serialize)]
struct SourceFacts {
    package: Option<String>,
    imports: Vec<ImportFact>,
    annotations: Vec<AnnotationFact>,
    classes: Vec<ClassFact>,
    method_candidates: Vec<MethodFact>,
    lexical_coverage_complete: bool,
    unresolved: BTreeSet<String>,
}

#[derive(Debug, Serialize)]
struct ImportFact {
    spelling: String,
    alias: Option<String>,
    wildcard: bool,
    static_import: bool,
    span: Span,
}

#[derive(Debug, Serialize)]
struct AnnotationFact {
    spelling: String,
    use_site_target: Option<String>,
    span: Span,
}

#[derive(Debug, Serialize)]
struct ClassFact {
    name: String,
    declaration_kind: String,
    owner: Option<String>,
    abstract_declaration: bool,
    nested: bool,
    declared_supertype_scope_unresolved: bool,
    header: String,
    span: Span,
    annotations: Vec<AnnotationFact>,
}

#[derive(Debug, Serialize)]
struct MethodFact {
    name: String,
    owner: Option<String>,
    name_span: Span,
    declaration_span: Span,
    parameter_span: Span,
    signature_sha256: String,
    annotations: Vec<AnnotationFact>,
    junit4_annotation_declared: bool,
    declaration_status: &'static str,
    applicability: &'static str,
    effective_runtime_cases: Option<usize>,
    unresolved: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct Span {
    start_byte: usize,
    end_byte: usize,
    start_line: usize,
    end_line: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum TokenKind {
    Identifier,
    EscapedIdentifier,
    Symbol,
    Literal,
}

#[derive(Clone, Copy)]
struct Token<'a> {
    text: &'a str,
    kind: TokenKind,
    start: usize,
    end: usize,
    line: usize,
    end_line: usize,
}

impl Token<'_> {
    fn is(&self, spelling: &str) -> bool {
        self.kind != TokenKind::EscapedIdentifier
            && self.kind != TokenKind::Literal
            && self.text == spelling
    }

    fn identifier(&self) -> bool {
        matches!(
            self.kind,
            TokenKind::Identifier | TokenKind::EscapedIdentifier
        )
    }
}

pub fn run(args: AndroidJunit4DeclarationsArgs) -> Result<()> {
    let repository = args.repository.canonicalize()?;
    let output_parent = args
        .output
        .parent()
        .context("output requires a parent")?
        .canonicalize()?;
    let output = output_parent.join(
        args.output
            .file_name()
            .context("output requires a file name")?,
    );
    ensure!(
        !output.starts_with(&repository),
        "declaration evidence must stay outside the checkout"
    );
    ensure!(
        args.check == fs::symlink_metadata(&output).is_ok(),
        "create a new output file, or use --check for existing evidence"
    );
    let bytes = generate(&repository, &args.source_root, &args.selection)?;
    if args.check {
        ensure!(
            fs::symlink_metadata(&output)?.file_type().is_file(),
            "existing evidence must be a regular file"
        );
        ensure!(
            read_bounded(&output, MAX_TOTAL_BYTES)? == bytes,
            "declaration evidence changed"
        );
    } else {
        let staging = output_parent.join(format!(
            ".{}-staging-{}",
            output
                .file_name()
                .context("output requires a file name")?
                .to_string_lossy(),
            std::process::id()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)?;
        let publication = file
            .write_all(&bytes)
            .and_then(|()| file.flush())
            .and_then(|()| fs::hard_link(&staging, &output));
        drop(file);
        fs::remove_file(&staging).context("cannot retire declaration staging file")?;
        publication.context("cannot publish declaration evidence without replacement")?;
    }
    println!(
        "Selected source declaration evidence verified; effective test census remains incomplete. Output: {}",
        output.display()
    );
    Ok(())
}

fn generate(repository: &Path, source_root: &Path, selection_path: &Path) -> Result<Vec<u8>> {
    let mut metadata = MetadataBudget::new(MAX_TOTAL_BYTES as usize);
    generate_with_budget(
        repository,
        source_root,
        selection_path,
        &mut metadata,
        MAX_TOTAL_BYTES as usize,
    )
}

fn generate_with_budget(
    repository: &Path,
    source_root: &Path,
    selection_path: &Path,
    metadata: &mut MetadataBudget,
    output_limit: usize,
) -> Result<Vec<u8>> {
    let manifest_bytes = read_bounded(
        &repository.join("docs/android-studio/reference-manifest.json"),
        MAX_SELECTION_BYTES,
    )?;
    metadata.charge(manifest_bytes.len())?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    ensure!(
        manifest.schema_version == 1,
        "unsupported reference manifest version"
    );
    let selection_bytes = read_bounded(selection_path, MAX_SELECTION_BYTES)?;
    metadata.charge(selection_bytes.len())?;
    let mut selection: Selection = serde_json::from_slice(&selection_bytes)?;
    ensure!(
        selection.schema_version == 1,
        "unsupported selection version"
    );
    ensure!(
        !selection.scope.trim().is_empty(),
        "selection needs an explicit scope"
    );
    ensure!(
        !selection.files.is_empty() && selection.files.len() <= MAX_FILES,
        "selection file count exceeds the bounded scope"
    );
    let source_root = source_root.canonicalize()?;
    let mut references = BTreeMap::new();
    for source in manifest.sources {
        ensure!(valid_identity(&source.id), "invalid reference source id");
        ensure!(
            valid_hash(&source.revision, 40) && valid_hash(&source.archive.sha256, 64),
            "invalid pinned source identity"
        );
        ensure!(
            references
                .insert(metadata.owned(&source.id)?, source)
                .is_none(),
            "duplicate reference source id"
        );
    }
    selection
        .files
        .sort_by(|left, right| (&left.source, &left.path).cmp(&(&right.source, &right.path)));
    let mut identities = BTreeSet::new();
    let mut total_bytes = 0_u64;
    let mut files = Vec::new();
    for selected in selection.files {
        ensure!(
            identities.insert((
                metadata.owned(&selected.source)?,
                metadata.owned(&selected.path)?,
            )),
            "duplicate selected source identity"
        );
        let reference = references
            .get(&selected.source)
            .context("selected source is not pinned in the manifest")?;
        ensure!(
            selected.revision == reference.revision,
            "selected revision differs from the pinned source"
        );
        ensure!(
            selected.license == "Apache-2.0",
            "selected fixture license must retain Apache-2.0 attribution"
        );
        ensure!(
            valid_hash(&selected.sha256, 64),
            "invalid selected source hash"
        );
        safe_relative(&selected.path)?;
        safe_relative(&selected.fixture)?;
        ensure!(
            selected.fixture == metadata.joined(&selected.source, '/', &selected.path)?,
            "fixture path must preserve source identity and original path"
        );
        ensure!(
            selected.path.ends_with(".java") || selected.path.ends_with(".kt"),
            "selection supports Java/Kotlin sources only"
        );
        ensure!(
            selected.bytes <= MAX_SOURCE_BYTES,
            "selected source exceeds the byte budget"
        );
        total_bytes = total_bytes
            .checked_add(selected.bytes)
            .context("source byte count overflow")?;
        ensure!(
            total_bytes <= MAX_TOTAL_BYTES,
            "selection exceeds the total source budget"
        );
        let fixture = source_root.join(&selected.fixture).canonicalize()?;
        ensure!(
            fixture.starts_with(&source_root),
            "fixture escapes the source root"
        );
        ensure!(fixture.is_file(), "fixture must be a regular file");
        let bytes = read_bounded(&fixture, MAX_SOURCE_BYTES)?;
        metadata.charge(64)?;
        ensure!(
            bytes.len() as u64 == selected.bytes && sha256(&bytes) == selected.sha256,
            "selected source bytes/hash differ from provenance: {}:{}",
            selected.source,
            selected.path
        );
        let source_facts = match std::str::from_utf8(&bytes) {
            Ok(text) => source_facts_with_budget(text, selected.path.ends_with(".kt"), metadata)?,
            Err(_) => SourceFacts {
                unresolved: BTreeSet::from([
                    metadata.owned("source is not UTF-8; declaration evidence unresolved")?
                ]),
                ..SourceFacts::default()
            },
        };
        let file_evidence = FileEvidence {
            selected,
            repository: metadata.owned(&reference.repository)?,
            source_coverage: metadata.owned(&reference.coverage)?,
            archive_sha256: metadata.owned(&reference.archive.sha256)?,
            source_facts,
        };
        files.push(file_evidence);
    }
    metadata.charge(128)?;
    let evidence = Evidence {
        schema_version: 1,
        baseline: manifest.baseline,
        scope: selection.scope,
        discovery: "selected direct Java/Kotlin annotation declarations; not classpath binding, build membership or runner-expanded test instances",
        selection_sha256: sha256(&selection_bytes),
        reference_manifest_sha256: sha256(&manifest_bytes),
        selected_archive_membership_reverified: false,
        effective_test_census_complete: false,
        declaration_coverage_complete: false,
        new_behavioral_parity_credit: 0,
        effective_runtime_cases: None,
        files,
        unresolved: vec![
            "selection is bounded; retained lexical candidates outside it remain unported",
            "source hashes match selected provenance; archive member origin relies on reviewed capture and is not reverified by this command",
            "JUnit3 inheritance/suite factories and JUnit5/TestNG/meta-annotation bindings unresolved",
            "nested/local/aliased declarations and unsupported method syntax require semantic review",
            "abstract/ignored/disabled declarations are retained; applicability remains unreviewed",
            "custom runners/rules, parameter providers and generated or repeated instances unresolved",
            "build-target membership, classpath symbols and compiled runner descriptions unresolved",
            "source identities are separate; unverified mirrors cannot establish canonical coverage",
        ],
    };
    let mut writer = BoundedJsonWriter::new(output_limit);
    serde_json::to_writer_pretty(&mut writer, &evidence)?;
    writer.finish()
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = File::open(path)?;
    ensure!(
        file.metadata()?.len() <= limit,
        "input exceeds the byte budget: {}",
        path.display()
    );
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "input grew beyond the byte budget"
    );
    Ok(bytes)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn valid_hash(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn safe_relative(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && !path.starts_with('/')
            && !path.contains('\\')
            && path.split('/').all(|part| !part.is_empty()
                && part != "."
                && part != ".."
                && !part.contains(':')
                && !part.chars().any(char::is_control)),
        "unsafe source path"
    );
    Ok(())
}

fn tokenize<'a>(
    text: &'a str,
    kotlin: bool,
    metadata: &mut MetadataBudget,
) -> Result<(Vec<Token<'a>>, BTreeSet<String>)> {
    let mut tokens = Vec::new();
    let mut unresolved = BTreeSet::new();
    let mut position = 0;
    let mut line = 1;
    while position < text.len() {
        if tokens.len() == MAX_TOKENS {
            unresolved.insert(
                metadata.owned("token budget exhausted; remaining declarations unresolved")?,
            );
            break;
        }
        let remainder = &text[position..];
        if remainder.starts_with("//") {
            position += remainder.find('\n').unwrap_or(remainder.len());
            continue;
        }
        if remainder.starts_with("/*") {
            position += 2;
            let mut depth = 1;
            while position < text.len() && depth > 0 {
                let remainder = &text[position..];
                if kotlin && remainder.starts_with("/*") {
                    depth += 1;
                    position += 2;
                } else if remainder.starts_with("*/") {
                    depth -= 1;
                    position += 2;
                } else if let Some(character) = remainder.chars().next() {
                    line += usize::from(character == '\n');
                    position += character.len_utf8();
                }
            }
            if depth != 0 {
                unresolved.insert(metadata.owned("unterminated block comment")?);
            }
            continue;
        }
        let Some(character) = remainder.chars().next() else {
            break;
        };
        if character.is_whitespace() {
            line += usize::from(character == '\n');
            position += character.len_utf8();
            continue;
        }
        let start = position;
        let start_line = line;
        let mut kind = TokenKind::Symbol;
        let mut value_start = start;
        let value_end;
        if character == '"' || character == '\'' {
            kind = TokenKind::Literal;
            let delimiter = if remainder.starts_with("\"\"\"") {
                "\"\"\""
            } else if character == '"' {
                "\""
            } else {
                "'"
            };
            position += delimiter.len();
            let mut closed = false;
            while position < text.len() {
                if text[position..].starts_with(delimiter) {
                    position += delimiter.len();
                    closed = true;
                    break;
                }
                let Some(character) = text[position..].chars().next() else {
                    break;
                };
                position += character.len_utf8();
                line += usize::from(character == '\n');
                if (delimiter.len() == 1 || !kotlin)
                    && character == '\\'
                    && let Some(escaped) = text[position..].chars().next()
                {
                    position += escaped.len_utf8();
                    line += usize::from(escaped == '\n');
                }
            }
            if !closed {
                unresolved.insert(metadata.owned("unterminated string/character literal")?);
            }
            value_end = position;
        } else if character == '`' {
            kind = TokenKind::EscapedIdentifier;
            position += 1;
            value_start = position;
            if let Some(length) = text[position..].find('`') {
                value_end = position + length;
                line += text[position..value_end]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count();
                position = value_end + 1;
                if !kotlin || start_line != line {
                    unresolved.insert(metadata.owned("unsupported escaped identifier syntax")?);
                }
            } else {
                unresolved.insert(metadata.owned("unterminated escaped identifier")?);
                break;
            }
        } else {
            position += character.len_utf8();
            if character.is_alphabetic() || character == '_' || character == '$' {
                kind = TokenKind::Identifier;
                while let Some(following) = text[position..].chars().next() {
                    if following.is_alphanumeric() || following == '_' || following == '$' {
                        position += following.len_utf8();
                    } else {
                        break;
                    }
                }
            }
            value_end = position;
        }
        tokens.push(Token {
            text: &text[value_start..value_end],
            kind,
            start,
            end: position,
            line: start_line,
            end_line: line,
        });
    }
    Ok((tokens, unresolved))
}

struct Structure {
    closing: Vec<Option<usize>>,
    brace_depth: Vec<usize>,
    valid: bool,
}

impl Structure {
    fn new(tokens: &[Token<'_>]) -> Self {
        let mut result = Self {
            closing: vec![None; tokens.len()],
            brace_depth: Vec::with_capacity(tokens.len()),
            valid: true,
        };
        let mut stack: Vec<(usize, &str)> = Vec::new();
        let mut depth = 0_usize;
        for (index, token) in tokens.iter().enumerate() {
            result.brace_depth.push(depth);
            if ["{", "(", "["].iter().any(|spelling| token.is(spelling)) {
                stack.push((index, token.text));
                depth += usize::from(token.is("{"));
            } else if ["}", ")", "]"].iter().any(|spelling| token.is(spelling)) {
                depth = depth.saturating_sub(usize::from(token.is("}")));
                if let Some((open, spelling)) = stack.pop() {
                    let matches =
                        matches!((spelling, token.text), ("{", "}") | ("(", ")") | ("[", "]"));
                    if matches {
                        result.closing[open] = Some(index);
                    } else {
                        result.valid = false;
                    }
                } else {
                    result.valid = false;
                }
            }
        }
        result.valid &= stack.is_empty();
        result
    }
}

fn span(tokens: &[Token<'_>], first: usize, last: usize) -> Span {
    Span {
        start_byte: tokens[first].start,
        end_byte: tokens[last].end,
        start_line: tokens[first].line,
        end_line: tokens[last].end_line,
    }
}

fn qualified_name(
    tokens: &[Token<'_>],
    start: usize,
    metadata: &mut MetadataBudget,
) -> Result<(String, usize)> {
    let mut index = start;
    let mut length = 0_usize;
    if let Some(token) = tokens
        .get(index)
        .filter(|token| token.kind == TokenKind::Identifier)
    {
        length = token.text.len();
        index += 1;
        while tokens.get(index).is_some_and(|token| token.is("."))
            && tokens
                .get(index + 1)
                .is_some_and(|token| token.kind == TokenKind::Identifier || token.is("*"))
        {
            length = length
                .checked_add(1)
                .and_then(|length| length.checked_add(tokens[index + 1].text.len()))
                .context("qualified metadata byte count overflow")?;
            index += 2;
            if tokens[index - 1].is("*") {
                break;
            }
        }
    }
    if index == start {
        return Ok((String::new(), index));
    }
    metadata.charge(length)?;
    let mut spelling = String::with_capacity(length);
    for token in &tokens[start..index] {
        spelling.push_str(token.text);
    }
    Ok((spelling, index))
}

struct ParsedAnnotation {
    fact: AnnotationFact,
    start: usize,
    end: usize,
}

fn annotations(
    tokens: &[Token<'_>],
    structure: &Structure,
    metadata: &mut MetadataBudget,
) -> Result<Vec<ParsedAnnotation>> {
    let mut result = Vec::new();
    for (index, _) in tokens.iter().enumerate().filter(|(_, token)| token.is("@")) {
        if result.len() == MAX_DECLARATIONS {
            break;
        }
        let (first, after_first) = qualified_name(tokens, index + 1, metadata)?;
        let (target, spelling, mut after) =
            if tokens.get(after_first).is_some_and(|token| token.is(":")) {
                let (name, after) = qualified_name(tokens, after_first + 1, metadata)?;
                (Some(first), name, after)
            } else {
                (None, first, after_first)
            };
        if spelling.is_empty() {
            continue;
        }
        if tokens.get(after).is_some_and(|token| token.is("(")) {
            if let Some(close) = structure.closing[after] {
                after = close + 1;
            } else {
                continue;
            }
        }
        result.push(ParsedAnnotation {
            fact: AnnotationFact {
                spelling,
                use_site_target: target,
                span: span(tokens, index, after - 1),
            },
            start: index,
            end: after,
        });
    }
    Ok(result)
}

fn modifier(token: &Token<'_>) -> bool {
    [
        "public",
        "private",
        "protected",
        "internal",
        "abstract",
        "open",
        "final",
        "override",
        "static",
        "suspend",
        "inline",
        "external",
        "synchronized",
        "native",
        "strictfp",
        "default",
        "tailrec",
        "infix",
        "operator",
        "enum",
        "annotation",
        "data",
        "sealed",
        "value",
        "expect",
        "actual",
    ]
    .iter()
    .any(|name| token.is(name))
}

fn kotlin_alias_identifier(token: &Token<'_>) -> bool {
    (token.kind == TokenKind::EscapedIdentifier && !token.text.is_empty())
        || (token.kind == TokenKind::Identifier
            && !token.text.contains('$')
            && ![
                "as",
                "break",
                "class",
                "continue",
                "do",
                "else",
                "false",
                "for",
                "fun",
                "if",
                "in",
                "interface",
                "is",
                "null",
                "object",
                "package",
                "return",
                "super",
                "this",
                "throw",
                "true",
                "try",
                "typealias",
                "typeof",
                "val",
                "var",
                "when",
                "while",
            ]
            .contains(&token.text))
}

fn declaration_prefix<'a>(
    tokens: &[Token<'_>],
    index: usize,
    parsed: &'a [ParsedAnnotation],
    ends: &BTreeMap<usize, usize>,
) -> (usize, Vec<&'a AnnotationFact>) {
    let mut start = index;
    let mut facts = Vec::new();
    let limit = index.saturating_sub(MAX_HEADER_TOKENS);
    while start > limit {
        if let Some(annotation) = ends.get(&start) {
            facts.push(&parsed[*annotation].fact);
            start = parsed[*annotation].start;
        } else if tokens.get(start - 1).is_some_and(modifier) {
            start -= 1;
        } else {
            break;
        }
    }
    facts.reverse();
    (start, facts)
}

struct ParsedClass {
    fact: ClassFact,
    open: usize,
    close: usize,
}

fn class_body(tokens: &[Token<'_>], index: usize, structure: &Structure) -> Option<usize> {
    let mut cursor = index;
    while cursor < tokens.len() && cursor < index.saturating_add(MAX_HEADER_TOKENS) {
        if tokens[cursor].is("{") {
            return Some(cursor);
        }
        if tokens[cursor].is(";") || tokens[cursor].is("}") {
            return None;
        }
        if [
            "class",
            "interface",
            "object",
            "record",
            "enum",
            "fun",
            "val",
            "var",
            "typealias",
            "import",
            "package",
        ]
        .iter()
        .any(|name| tokens[cursor].is(name))
        {
            return None;
        }
        if tokens[cursor].is("(") || tokens[cursor].is("[") {
            cursor = structure.closing[cursor]? + 1;
        } else {
            cursor += 1;
        }
    }
    None
}

fn declared_supertype(
    tokens: &[Token<'_>],
    start: usize,
    end: usize,
    structure: &Structure,
    kotlin: bool,
) -> bool {
    let mut index = start;
    let mut generic_depth = 0_usize;
    while index < end {
        let token = &tokens[index];
        if token.is("(") || token.is("[") {
            if let Some(close) = structure.closing[index] {
                index = close + 1;
                continue;
            }
        }
        if token.is("<") {
            generic_depth += 1;
        } else if token.is(">") {
            generic_depth = generic_depth.saturating_sub(1);
        } else if generic_depth == 0
            && ((kotlin && token.is(":"))
                || (!kotlin && (token.is("extends") || token.is("implements"))))
        {
            return true;
        }
        index += 1;
    }
    false
}

#[cfg(test)]
fn source_facts(text: &str, kotlin: bool) -> SourceFacts {
    source_facts_with_budget(
        text,
        kotlin,
        &mut MetadataBudget::new(MAX_TOTAL_BYTES as usize),
    )
    .expect("existing scanner input stays within the normal metadata budget")
}

fn source_facts_with_budget(
    text: &str,
    kotlin: bool,
    metadata: &mut MetadataBudget,
) -> Result<SourceFacts> {
    let (tokens, mut unresolved) = tokenize(text, kotlin, metadata)?;
    let structure = Structure::new(&tokens);
    if !structure.valid {
        unresolved.insert(
            metadata
                .owned("unbalanced/mismatched delimiters; annotation declarations unresolved")?,
        );
    }
    if !kotlin && text.contains("\\u") {
        unresolved.insert(metadata.owned("Java Unicode-escape preprocessing unresolved")?);
    }
    let lexically_complete = unresolved.is_empty();
    let mut supported_headers = true;
    let parsed = annotations(&tokens, &structure, metadata)?;
    let ends = parsed
        .iter()
        .enumerate()
        .map(|(index, annotation)| (annotation.end, index))
        .collect::<BTreeMap<_, _>>();
    let mut facts = SourceFacts {
        annotations: parsed
            .iter()
            .map(|annotation| metadata.annotation(&annotation.fact))
            .collect::<Result<Vec<_>>>()?,
        lexical_coverage_complete: lexically_complete,
        unresolved,
        ..SourceFacts::default()
    };
    if tokens.iter().any(|token| token.is("@")) && parsed.is_empty() {
        facts
            .unresolved
            .insert(metadata.owned("unsupported annotation syntax remains unresolved")?);
    }
    if parsed.len() == MAX_DECLARATIONS
        && tokens.iter().filter(|token| token.is("@")).count() > parsed.len()
    {
        facts.unresolved.insert(
            metadata.owned("annotation budget exhausted; remaining annotations unresolved")?,
        );
    }
    for (index, token) in tokens.iter().enumerate() {
        if structure.brace_depth[index] != 0 {
            continue;
        }
        if token.is("package") {
            let (name, _) = qualified_name(&tokens, index + 1, metadata)?;
            if !name.is_empty() {
                facts.package = Some(name);
            }
        } else if token.is("import") {
            let static_import = tokens
                .get(index + 1)
                .is_some_and(|token| token.is("static"));
            let (spelling, mut after) =
                qualified_name(&tokens, index + 1 + usize::from(static_import), metadata)?;
            if spelling.is_empty() {
                supported_headers = false;
                facts
                    .unresolved
                    .insert(metadata.owned("unsupported import syntax")?);
                continue;
            }
            let alias = if tokens.get(after).is_some_and(|token| token.is("as")) {
                after += 1;
                let alias = tokens
                    .get(after)
                    .filter(|token| kotlin_alias_identifier(token))
                    .map(|token| metadata.owned(token.text))
                    .transpose()?;
                if !kotlin || alias.is_none() {
                    supported_headers = false;
                    facts.unresolved.insert(metadata.owned("unsupported alias syntax; a Kotlin alias identifier is required and Java aliases are unsupported")?);
                }
                after += usize::from(alias.is_some());
                facts.unresolved.insert(
                    metadata.owned("aliased imports are retained without annotation binding")?,
                );
                alias
            } else {
                None
            };
            let wildcard = spelling.ends_with(".*");
            if !tokens.get(after).is_some_and(|token| {
                token.is(";") || (kotlin && token.line > tokens[after - 1].end_line)
            }) && after != tokens.len()
            {
                supported_headers = false;
                facts.unresolved.insert(
                    metadata
                        .owned("unsupported import terminator; annotation binding unresolved")?,
                );
            }
            if !kotlin && !tokens.get(after).is_some_and(|token| token.is(";")) {
                supported_headers = false;
                facts
                    .unresolved
                    .insert(metadata.owned("Java import lacks its required terminator")?);
            }
            if wildcard {
                facts.unresolved.insert(metadata.owned(
                    "wildcard imports require semantic binding for unresolved annotations",
                )?);
            }
            facts.imports.push(ImportFact {
                spelling,
                alias,
                wildcard,
                static_import,
                span: span(&tokens, index, after - 1),
            });
        }
    }
    let mut classes = Vec::new();
    let mut shadowed_test = false;
    let mut shadowed_org = false;
    for (index, token) in tokens.iter().enumerate() {
        if token.is("typealias") {
            if let Some(name) = tokens.get(index + 1) {
                shadowed_test |= name.text == "Test";
                shadowed_org |= name.text == "org";
            }
            facts
                .unresolved
                .insert(metadata.owned("type aliases require semantic annotation binding")?);
        }
        if !(["class", "interface", "object", "record"]
            .iter()
            .any(|name| token.is(name))
            || (!kotlin && token.is("enum")))
            || tokens
                .get(index.wrapping_sub(1))
                .is_some_and(|token| token.is(".") || token.is(":"))
        {
            continue;
        }
        let Some(name) = tokens.get(index + 1).filter(|token| token.identifier()) else {
            continue;
        };
        shadowed_test |= name.text == "Test";
        shadowed_org |= name.text == "org";
        let Some(open) = class_body(&tokens, index + 2, &structure) else {
            facts
                .unresolved
                .insert(metadata.owned("class header/body exceeds supported declaration scope")?);
            continue;
        };
        let Some(close) = structure.closing[open] else {
            continue;
        };
        if classes.len() == MAX_CLASSES
            || tokens[open].start.saturating_sub(tokens[index].start) > MAX_HEADER_BYTES
        {
            facts.unresolved.insert(
                metadata
                    .owned("class declaration/header budget exhausted; ownership unresolved")?,
            );
            continue;
        }
        shadowed_test |= tokens[index + 2..open].iter().any(|token| token.is("Test"));
        shadowed_org |= tokens[index + 2..open].iter().any(|token| token.is("org"));
        let nested = structure.brace_depth[index] != 0;
        if nested {
            facts.unresolved.insert(metadata.owned(
                "nested/local classes are retained without direct annotation confirmation",
            )?);
        }
        let (start, annotations) = declaration_prefix(&tokens, index, &parsed, &ends);
        if tokens[open].start.saturating_sub(tokens[start].start) > MAX_HEADER_BYTES {
            facts.unresolved.insert(
                metadata
                    .owned("class declaration/header budget exhausted; ownership unresolved")?,
            );
            continue;
        }
        let annotations = annotations
            .into_iter()
            .map(|annotation| metadata.annotation(annotation))
            .collect::<Result<Vec<_>>>()?;
        let owner = if nested {
            None
        } else {
            Some(match &facts.package {
                Some(package) => metadata.joined(package, '.', name.text)?,
                None => metadata.owned(name.text)?,
            })
        };
        classes.push(ParsedClass {
            fact: ClassFact {
                name: metadata.owned(name.text)?,
                declaration_kind: if kotlin
                    && token.is("class")
                    && tokens
                        .get(index.wrapping_sub(1))
                        .is_some_and(|token| token.is("enum"))
                {
                    metadata.owned("enum")?
                } else {
                    metadata.owned(token.text)?
                },
                owner,
                abstract_declaration: tokens[start..index]
                    .iter()
                    .any(|token| token.is("abstract")),
                nested,
                declared_supertype_scope_unresolved: declared_supertype(
                    &tokens,
                    index + 2,
                    open,
                    &structure,
                    kotlin,
                ),
                header: metadata.owned(&text[tokens[start].start..tokens[open].start])?,
                span: span(&tokens, start, open),
                annotations,
            },
            open,
            close,
        });
    }
    let test_imports = facts
        .imports
        .iter()
        .filter(|import| {
            !import.wildcard
                && import
                    .alias
                    .as_deref()
                    .or_else(|| import.spelling.rsplit('.').next())
                    == Some("Test")
        })
        .collect::<Vec<_>>();
    let explicit_test = test_imports.len() == 1
        && test_imports[0].spelling == "org.junit.Test"
        && test_imports[0].alias.is_none()
        && !test_imports[0].static_import
        && !shadowed_test;
    shadowed_org |= facts.imports.iter().any(|import| {
        !import.wildcard
            && import
                .alias
                .as_deref()
                .or_else(|| import.spelling.rsplit('.').next())
                == Some("org")
    });
    let qualified_prefix_unresolved =
        shadowed_org || facts.imports.iter().any(|import| import.wildcard);
    let alias_names = facts
        .imports
        .iter()
        .filter(|import| import.spelling == "org.junit.Test")
        .filter_map(|import| import.alias.as_deref())
        .collect::<BTreeSet<_>>();
    for (index, token) in tokens.iter().enumerate() {
        if facts.method_candidates.len() == MAX_DECLARATIONS {
            facts.unresolved.insert(
                metadata.owned(
                    "method declaration budget exhausted; remaining candidates unresolved",
                )?,
            );
            break;
        }
        let (name_index, return_index) = if kotlin && token.is("fun") {
            let Some(name) = tokens.get(index + 1).filter(|name| name.identifier()) else {
                continue;
            };
            if !tokens.get(index + 2).is_some_and(|token| token.is("(")) {
                facts.unresolved.insert(metadata.owned(
                    "generic/receiver Kotlin functions require semantic declaration discovery",
                )?);
                continue;
            }
            if name.text.is_empty() {
                continue;
            }
            (index + 1, index)
        } else if !kotlin
            && token.identifier()
            && tokens.get(index + 1).is_some_and(|token| token.is("("))
        {
            let Some(previous) = tokens.get(index.wrapping_sub(1)).filter(|previous| {
                previous.identifier()
                    && !modifier(previous)
                    && !previous.is("new")
                    && !previous.is("return")
            }) else {
                continue;
            };
            if tokens
                .get(index.wrapping_sub(2))
                .is_some_and(|token| token.is(".") || token.is("new"))
            {
                continue;
            }
            if !classes.iter().any(|class| {
                index > class.open
                    && index < class.close
                    && structure.brace_depth[index] == structure.brace_depth[class.open] + 1
            }) {
                continue;
            }
            if previous.kind != TokenKind::Identifier {
                continue;
            }
            (index, index - 1)
        } else {
            continue;
        };
        let open = name_index + 1;
        let Some(close) = structure.closing.get(open).copied().flatten() else {
            continue;
        };
        let (start, annotations) = declaration_prefix(&tokens, return_index, &parsed, &ends);
        let annotations = annotations
            .into_iter()
            .map(|annotation| metadata.annotation(annotation))
            .collect::<Result<Vec<_>>>()?;
        let unsupported_generic_method_prefix = !kotlin
            && tokens
                .get(start.wrapping_sub(1))
                .is_some_and(|token| token.is(">"));
        let name = &tokens[name_index];
        if !name.text.starts_with("test") && annotations.is_empty() {
            continue;
        }
        let mut method_unresolved = BTreeSet::from([
            metadata.owned("build-target membership and classpath symbol identity unresolved")?,
            metadata
                .owned("runner/rule dispatch, inheritance and effective instances unresolved")?,
        ]);
        let owner = classes.iter().find(|class| {
            !class.fact.nested
                && name_index > class.open
                && name_index < class.close
                && structure.brace_depth[name_index] == structure.brace_depth[class.open] + 1
        });
        if owner.is_none() {
            method_unresolved
                .insert(metadata.owned("nested/local/top-level function ownership unsupported")?);
        }
        let inherited_scope =
            owner.is_some_and(|owner| owner.fact.declared_supertype_scope_unresolved);
        if inherited_scope {
            method_unresolved.insert(metadata.owned(
                "declared supertypes may shadow annotation names; inherited type scope unresolved",
            )?);
        }
        if unsupported_generic_method_prefix {
            method_unresolved.insert(
                metadata.owned("generic Java method/type-use annotation scope unresolved")?,
            );
        }
        let mut confirmed = false;
        for annotation in &annotations {
            if annotation.use_site_target.is_some() {
                method_unresolved
                    .insert(metadata.owned(
                        "annotation use-site targets are retained without method binding",
                    )?);
                continue;
            }
            if annotation.spelling == "org.junit.Test"
                && !qualified_prefix_unresolved
                && !shadowed_test
            {
                confirmed = true;
            } else if annotation.spelling == "Test" && explicit_test {
                confirmed = true;
            } else if annotation.spelling.rsplit('.').next() == Some("Test")
                || alias_names.contains(annotation.spelling.as_str())
            {
                method_unresolved.insert(metadata.owned(
                    "test annotation import/alias/shadowing/framework binding unresolved",
                )?);
            }
            if annotation
                .spelling
                .rsplit('.')
                .next()
                .is_some_and(|name| matches!(name, "Ignore" | "Disabled"))
            {
                method_unresolved.insert(metadata.owned(
                    "ignored/disabled declaration retained; runner eligibility unresolved",
                )?);
            }
        }
        if name.text.starts_with("test") && !confirmed {
            method_unresolved.insert(metadata.owned(
                "test-prefixed declaration retained; JUnit3 inheritance/custom dispatch unresolved",
            )?);
        }
        if owner.is_some_and(|owner| owner.fact.abstract_declaration) {
            method_unresolved.insert(
                metadata.owned("abstract owner retained; concrete subclass dispatch unresolved")?,
            );
        }
        if owner.is_some_and(|owner| {
            owner.fact.annotations.iter().any(|annotation| {
                annotation
                    .spelling
                    .rsplit('.')
                    .next()
                    .is_some_and(|name| matches!(name, "Ignore" | "Disabled"))
            })
        }) {
            method_unresolved.insert(
                metadata.owned("ignored/disabled owner retained; runner eligibility unresolved")?,
            );
        }
        if !lexically_complete {
            method_unresolved.insert(metadata.owned(
                "incomplete/unsupported lexical preprocessing prevents annotation confirmation",
            )?);
        }
        confirmed &= lexically_complete
            && supported_headers
            && owner.is_some()
            && !inherited_scope
            && !unsupported_generic_method_prefix;
        let signature = &text[tokens[return_index].start..tokens[close].end];
        metadata.charge(64)?;
        facts.method_candidates.push(MethodFact {
            name: metadata.owned(name.text)?,
            owner: owner
                .and_then(|owner| owner.fact.owner.as_deref())
                .map(|owner| metadata.owned(owner))
                .transpose()?,
            name_span: span(&tokens, name_index, name_index),
            declaration_span: span(&tokens, start, close),
            parameter_span: span(&tokens, open, close),
            signature_sha256: sha256(signature.as_bytes()),
            annotations,
            junit4_annotation_declared: confirmed,
            declaration_status: if confirmed {
                "confirmed_source_junit4_annotation_declaration"
            } else {
                "unresolved_method_candidate"
            },
            applicability: "unreviewed",
            effective_runtime_cases: None,
            unresolved: method_unresolved,
        });
    }
    facts.classes = classes.into_iter().map(|class| class.fact).collect();
    if facts.annotations.iter().any(|annotation| {
        (annotation.spelling.rsplit('.').next() == Some("Test")
            || alias_names.contains(annotation.spelling.as_str()))
            && !facts.method_candidates.iter().any(|method| {
                method
                    .annotations
                    .iter()
                    .any(|bound| bound.span.start_byte == annotation.span.start_byte)
            })
    }) {
        facts.unresolved.insert(metadata.owned(
            "unbound test annotation retained; unsupported declaration syntax/ownership unresolved",
        )?);
    }
    facts.unresolved.insert(metadata.owned("annotation declarations do not establish runnable test membership or effective case counts")?);
    Ok(facts)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_VARIANTS: &str = "project-system-gradle-sync/testSrc/com/android/tools/idea/gradle/project/sync/DefaultVariantsTest.kt";
    const TABBED_TOOLBAR: &str =
        "adt-ui/src/test/java/com/android/tools/adtui/TabbedToolbarTest.kt";
    const GRADLE_IMPORT: &str =
        "android/gradle/testSrc/com/android/tools/idea/gradle/project/GradleModuleImportTest.java";

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/reference_declarations")
    }

    fn repository() -> Result<PathBuf> {
        Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()?)
    }

    fn original(source: &str, path: &str) -> Result<String> {
        Ok(fs::read_to_string(
            fixture_root().join("sources").join(source).join(path),
        )?)
    }

    fn confirmed_names(facts: &SourceFacts) -> Vec<&str> {
        facts
            .method_candidates
            .iter()
            .filter(|method| method.junit4_annotation_declared)
            .map(|method| method.name.as_str())
            .collect()
    }

    #[test]
    fn aosp_default_variants_preserves_all_eight_direct_declarations() -> Result<()> {
        let text = original("idea", DEFAULT_VARIANTS)?;
        assert_eq!(
            sha256(text.as_bytes()),
            "c21c3dbc6fe94377ae7e82b2028d357c0ce0285793eecafa65140df63cc84b15"
        );
        let facts = source_facts(&text, true);
        assert_eq!(
            confirmed_names(&facts),
            [
                "oneVariant",
                "debugPreferred",
                "preferredBuildType",
                "preferredProductFlavorOverDebugBuildType",
                "preferredFlavorsInTwoDimensions",
                "preferredFlavorInSecondDimensionOnly",
                "mismatchedProductFlavourLength",
                "onEmpty"
            ]
        );
        assert!(
            facts
                .method_candidates
                .iter()
                .all(|method| method.effective_runtime_cases.is_none()
                    && method.applicability == "unreviewed")
        );
        let first = &facts.method_candidates[0];
        assert_eq!(
            first.owner.as_deref(),
            Some("com.android.tools.idea.gradle.project.sync.DefaultVariantsTest")
        );
        assert_eq!(first.name_span.start_line, 26);
        assert_eq!(
            &text[first.name_span.start_byte..first.name_span.end_byte],
            "oneVariant"
        );
        assert_eq!(
            &text[first.annotations[0].span.start_byte..first.annotations[0].span.end_byte],
            "@Test"
        );
        Ok(())
    }

    #[test]
    fn aosp_toolbar_preserves_backtick_names_and_runner_rule_context() -> Result<()> {
        let text = original("idea", TABBED_TOOLBAR)?;
        assert_eq!(
            sha256(text.as_bytes()),
            "7e72bbc46fb4f28872c48cfdce7c90a8c5de8e40c5136943790aa9ab6f8f963b"
        );
        let facts = source_facts(&text, true);
        assert_eq!(
            confirmed_names(&facts),
            [
                "componentIsAddedElement",
                "tabIsAdded",
                "closedIsCalledWhenClicked",
                "noCloseButtonWhenNoListener",
                "iconButtonsCallbackWhenClicked",
                "can select tab by index",
                "adding tab should not trigger select listener"
            ]
        );
        assert_eq!(facts.classes[0].annotations[0].spelling, "RunsInEdt");
        assert!(
            facts
                .annotations
                .iter()
                .any(|annotation| annotation.spelling == "Rule"
                    && annotation.use_site_target.as_deref() == Some("get"))
        );
        let named = facts
            .method_candidates
            .iter()
            .find(|method| method.name == "can select tab by index")
            .context("missing backtick declaration")?;
        assert_eq!(
            &text[named.name_span.start_byte..named.name_span.end_byte],
            "`can select tab by index`"
        );
        assert!(named.effective_runtime_cases.is_none());
        Ok(())
    }

    #[test]
    fn mirror_paths_keep_distinct_hash_bound_source_identity() -> Result<()> {
        for (path, aosp_hash, mirror_hash, count, aosp_line, mirror_line) in [
            (
                DEFAULT_VARIANTS,
                "c21c3dbc6fe94377ae7e82b2028d357c0ce0285793eecafa65140df63cc84b15",
                "e79c781a5e7f96a24a1f55278351cb708d8f2a5417ad676f78b925b47430544a",
                8,
                33,
                38,
            ),
            (
                TABBED_TOOLBAR,
                "7e72bbc46fb4f28872c48cfdce7c90a8c5de8e40c5136943790aa9ab6f8f963b",
                "f524898eb42526b57242d0c79db1ecf88ead8105a623eb7c64d57cea45d47bb6",
                7,
                41,
                41,
            ),
        ] {
            let aosp = original("idea", path)?;
            let mirror = original("jetbrains-android", path)?;
            assert_eq!(sha256(aosp.as_bytes()), aosp_hash);
            assert_eq!(sha256(mirror.as_bytes()), mirror_hash);
            assert_ne!(aosp, mirror);
            let left = source_facts(&aosp, true);
            let right = source_facts(&mirror, true);
            assert_eq!(confirmed_names(&left).len(), count);
            assert_eq!(confirmed_names(&right).len(), count);
            assert_eq!(confirmed_names(&left), confirmed_names(&right));
            assert_eq!(left.method_candidates[1].name_span.start_line, aosp_line);
            assert_eq!(right.method_candidates[1].name_span.start_line, mirror_line);
        }
        Ok(())
    }

    #[test]
    fn junit3_style_original_retains_nine_methods_without_guessing_inheritance() -> Result<()> {
        let aosp = original("idea", GRADLE_IMPORT)?;
        let mirror = original("jetbrains-android", GRADLE_IMPORT)?;
        assert_eq!(aosp, mirror);
        assert_eq!(
            sha256(aosp.as_bytes()),
            "e63aa8bcd300922c731ba1c27be470dcafd808021c8906082d62f8f0e7e6c762"
        );
        let facts = source_facts(&aosp, false);
        assert!(confirmed_names(&facts).is_empty());
        let methods = facts
            .method_candidates
            .iter()
            .filter(|method| method.name.starts_with("test"))
            .collect::<Vec<_>>();
        assert_eq!(methods.len(), 9);
        assert!(methods.iter().all(|method| {
            method
                .unresolved
                .iter()
                .any(|reason| reason.contains("JUnit3 inheritance"))
                && method.effective_runtime_cases.is_none()
        }));
        assert!(
            facts.classes[0]
                .header
                .contains("extends HeavyPlatformTestCase")
        );
        Ok(())
    }

    #[test]
    fn original_java_junit4_methods_have_direct_binding_but_unknown_runtime() -> Result<()> {
        let text = original(
            "base",
            "apkparser/analyzer/src/test/java/com/android/tools/apk/analyzer/PathUtilsTest.java",
        )?;
        let facts = source_facts(&text, false);
        assert_eq!(
            confirmed_names(&facts),
            ["testLocalFileSystemPath", "testZipPath"]
        );
        assert!(facts.classes[0].header.contains("@RunWith(JUnit4.class)"));
        assert!(
            facts
                .method_candidates
                .iter()
                .all(|method| method.effective_runtime_cases.is_none())
        );
        Ok(())
    }

    #[test]
    fn original_ignored_method_is_retained_without_applicability_credit() -> Result<()> {
        let text = original(
            "base",
            "build-system/aaptcompiler/src/test/java/com/android/aaptcompiler/FloatParsingTest.kt",
        )?;
        let facts = source_facts(&text, true);
        assert_eq!(confirmed_names(&facts).len(), 9);
        let ignored = facts
            .method_candidates
            .iter()
            .find(|method| method.name == "roundTrip")
            .context("missing ignored original method")?;
        assert!(ignored.junit4_annotation_declared);
        assert_eq!(
            ignored
                .annotations
                .iter()
                .map(|annotation| annotation.spelling.as_str())
                .collect::<Vec<_>>(),
            ["Test", "Ignore"]
        );
        assert!(
            ignored
                .unresolved
                .iter()
                .any(|reason| reason.contains("ignored/disabled declaration retained"))
        );
        assert_eq!(ignored.applicability, "unreviewed");
        assert!(ignored.effective_runtime_cases.is_none());
        Ok(())
    }

    #[test]
    fn original_parameterized_runner_does_not_expand_source_method_counts() -> Result<()> {
        let text = original(
            "idea",
            "adt-ui/src/test/java/com/android/tools/adtui/common/ColorPaletteManagerTest.kt",
        )?;
        let facts = source_facts(&text, true);
        assert_eq!(confirmed_names(&facts).len(), 10);
        assert!(
            facts.classes[0]
                .header
                .contains("@RunWith(Parameterized::class)")
        );
        assert!(
            facts
                .annotations
                .iter()
                .any(|annotation| annotation.spelling == "Parameterized.Parameters")
        );
        assert!(
            facts
                .method_candidates
                .iter()
                .all(|method| method.effective_runtime_cases.is_none())
        );
        Ok(())
    }

    #[test]
    fn original_abstract_owner_and_nested_helpers_keep_inheritance_gaps() -> Result<()> {
        let text = original(
            "base",
            "build-system/gradle-core/src/test/java/com/android/build/api/artifact/impl/AbstractMultipleArtifactTest.kt",
        )?;
        let facts = source_facts(&text, true);
        assert_eq!(confirmed_names(&facts).len(), 8);
        assert!(facts.classes[0].abstract_declaration);
        assert!(facts.classes.iter().any(|class| class.nested));
        assert!(
            facts
                .method_candidates
                .iter()
                .filter(|method| method.junit4_annotation_declared)
                .all(|method| method
                    .unresolved
                    .iter()
                    .any(|reason| reason.contains("abstract owner retained"))
                    && method.effective_runtime_cases.is_none())
        );
        Ok(())
    }

    #[test]
    fn misleading_nested_annotation_and_visual_harness_are_not_jupiter_tests() -> Result<()> {
        let gradle = original(
            "base",
            "build-system/gradle-api/src/main/java/com/android/build/api/instrumentation/AsmClassVisitorFactory.kt",
        )?;
        let facts = source_facts(&gradle, true);
        assert!(confirmed_names(&facts).is_empty());
        assert!(
            facts
                .imports
                .iter()
                .any(|import| import.spelling == "org.gradle.api.tasks.Nested")
        );
        let visual = original(
            "idea",
            "adt-ui/src/test/java/com/android/tools/adtui/visualtests/VisualTest.java",
        )?;
        let facts = source_facts(&visual, false);
        assert!(facts.classes[0].abstract_declaration);
        assert!(confirmed_names(&facts).is_empty());
        Ok(())
    }

    #[test]
    fn original_custom_suite_runner_is_not_an_effective_leaf_count() -> Result<()> {
        let text = original(
            "idea",
            "adt-ui/src/test/java/com/android/tools/adtui/AdtUiTestSuite.java",
        )?;
        let facts = source_facts(&text, false);
        assert!(
            facts.classes[0]
                .header
                .contains("@RunWith(JarTestSuiteRunner.class)")
        );
        assert!(
            facts.classes[0]
                .header
                .contains("extends IdeaTestSuiteBase")
        );
        assert!(confirmed_names(&facts).is_empty());
        Ok(())
    }

    #[test]
    fn qualified_junit4_annotation_is_confirmed_without_import_guessing() {
        for (text, kotlin) in [
            (
                "package demo; class Suite { @org.junit.Test public void testA() {} }",
                false,
            ),
            (
                "package demo\nclass Suite { @org.junit.Test fun testA() {} }",
                true,
            ),
        ] {
            let facts = source_facts(text, kotlin);
            assert_eq!(confirmed_names(&facts), ["testA"]);
            assert!(facts.method_candidates[0].effective_runtime_cases.is_none());
        }
    }

    #[test]
    fn aliases_wildcards_conflicting_imports_and_other_frameworks_stay_unknown() {
        for (text, kotlin) in [
            (
                "import org.junit.Test as Check\nclass Suite { @Check fun testA() {} }",
                true,
            ),
            (
                "import org.junit.*; class Suite { @Test public void testA() {} }",
                false,
            ),
            (
                "import org.junit.Test; import other.Test; class Suite { @Test public void testA() {} }",
                false,
            ),
            (
                "import org.junit.jupiter.api.Test; class Suite { @Test public void testA() {} }",
                false,
            ),
            (
                "import org.testng.annotations.Test\nclass Suite { @Test fun testA() {} }",
                true,
            ),
            (
                "class Suite { @org.junit.jupiter.api.Test public void testA() {} }",
                false,
            ),
        ] {
            let facts = source_facts(text, kotlin);
            assert!(
                confirmed_names(&facts).is_empty(),
                "unexpected binding: {text}"
            );
            assert_eq!(facts.method_candidates.len(), 1);
            assert!(facts.method_candidates[0].effective_runtime_cases.is_none());
            assert!(
                facts.method_candidates[0]
                    .unresolved
                    .iter()
                    .any(|reason| reason.contains("binding unresolved"))
            );
        }
    }

    #[test]
    fn name_shadowing_type_aliases_and_generic_types_prevent_binding_guesses() {
        for text in [
            "import org.junit.Test\nannotation class Test {}\nclass Suite { @Test fun testA() {} }",
            "import org.junit.Test\ntypealias Test = Other\nclass Suite { @Test fun testA() {} }",
            "import org.junit.Test\nclass Suite<Test> { @Test fun testA() {} }",
            "object org {}\nclass Suite { @org.junit.Test fun testA() {} }",
        ] {
            let facts = source_facts(text, true);
            assert!(
                confirmed_names(&facts).is_empty(),
                "unexpected shadowed binding: {text}"
            );
            assert!(
                facts
                    .method_candidates
                    .iter()
                    .any(|method| method.name == "testA")
            );
        }
    }

    #[test]
    fn nested_local_and_top_level_functions_are_retained_without_confirmation() {
        let text = "import org.junit.Test\n@Test fun testTop() {}\nclass Suite { @Test fun testOuter() { @Test fun testLocal() {} } class Nested { @Test fun testNested() {} } }";
        let facts = source_facts(text, true);
        assert_eq!(confirmed_names(&facts), ["testOuter"]);
        for name in ["testTop", "testLocal", "testNested"] {
            let method = facts
                .method_candidates
                .iter()
                .find(|method| method.name == name)
                .expect("unresolved method retained");
            assert!(!method.junit4_annotation_declared);
            assert!(method.owner.is_none());
        }
        let java = source_facts(
            "import org.junit.Test; class Suite { class Nested { @Test public void testNested() {} } }",
            false,
        );
        assert!(confirmed_names(&java).is_empty());
        assert_eq!(java.method_candidates[0].name, "testNested");
    }

    #[test]
    fn bodyless_neighbor_class_does_not_steal_the_following_class_owner() {
        let facts = source_facts(
            "import org.junit.Test\nclass Empty\nclass Suite { @Test fun testA() {} }",
            true,
        );
        assert_eq!(confirmed_names(&facts), ["testA"]);
        assert_eq!(facts.method_candidates[0].owner.as_deref(), Some("Suite"));
        assert!(
            facts
                .unresolved
                .iter()
                .any(|reason| reason.contains("class header/body"))
        );
    }

    #[test]
    fn use_site_target_and_unbound_generic_function_keep_source_annotation_facts() {
        let facts = source_facts(
            "import org.junit.Test\nclass Suite { @get:Test fun testA() {} @Test fun <T> generic() {} }",
            true,
        );
        assert!(confirmed_names(&facts).is_empty());
        assert_eq!(facts.annotations.len(), 2);
        assert!(
            facts
                .unresolved
                .iter()
                .any(|reason| reason.contains("unbound test annotation"))
        );
    }

    #[test]
    fn multiline_comments_literals_and_utf8_spans_preserve_the_real_declaration() {
        let text = "import org.junit.Test\n/* @Test fun fake() {} /* nested */ */\nclass Suite { val text = \"@Test fun fake() {}\"\n@Test\nfun `café works`(value: String) {} }";
        let facts = source_facts(text, true);
        assert_eq!(confirmed_names(&facts), ["café works"]);
        let method = &facts.method_candidates[0];
        assert_eq!(method.name_span.start_line, 5);
        assert_eq!(
            &text[method.name_span.start_byte..method.name_span.end_byte],
            "`café works`"
        );
        assert_eq!(
            &text[method.parameter_span.start_byte..method.parameter_span.end_byte],
            "(value: String)"
        );
        assert_eq!(method.declaration_span.start_line, 4);
    }

    #[test]
    fn overloads_have_distinct_parameter_header_hashes_and_byte_positions() {
        let facts = source_facts(
            "import org.junit.Test\nclass Suite { @Test fun choose(value: Int) {} @Test fun choose(value: String) {} }",
            true,
        );
        assert_eq!(confirmed_names(&facts), ["choose", "choose"]);
        assert_ne!(
            facts.method_candidates[0].signature_sha256,
            facts.method_candidates[1].signature_sha256
        );
        assert_ne!(
            facts.method_candidates[0].name_span.start_byte,
            facts.method_candidates[1].name_span.start_byte
        );
    }

    #[test]
    fn ignored_class_and_abstract_method_are_retained_with_unknown_eligibility() {
        let facts = source_facts(
            "import org.junit.Test; import org.junit.Ignore; @Ignore public abstract class Suite { @Test public abstract void testA(); }",
            false,
        );
        assert_eq!(confirmed_names(&facts), ["testA"]);
        let method = &facts.method_candidates[0];
        assert!(
            method
                .unresolved
                .iter()
                .any(|reason| reason.contains("abstract owner"))
        );
        assert!(
            method
                .unresolved
                .iter()
                .any(|reason| reason.contains("ignored/disabled owner"))
        );
        assert!(method.effective_runtime_cases.is_none());
    }

    #[test]
    fn declared_supertypes_keep_potential_inherited_annotation_shadowing_unknown() {
        for (text, kotlin) in [
            (
                "import org.junit.Test; class Suite extends Parent { @Test public void testA() {} }",
                false,
            ),
            (
                "class Suite extends Parent { @org.junit.Test public void testA() {} }",
                false,
            ),
            (
                "import org.junit.Test\nclass Suite : Parent() { @Test fun testA() {} }",
                true,
            ),
        ] {
            let facts = source_facts(text, kotlin);
            assert!(confirmed_names(&facts).is_empty());
            assert_eq!(facts.method_candidates.len(), 1);
            assert!(facts.classes[0].declared_supertype_scope_unresolved);
            assert!(
                facts.method_candidates[0]
                    .unresolved
                    .iter()
                    .any(|reason| reason.contains("inherited type scope unresolved"))
            );
        }
    }

    #[test]
    fn escaped_java_text_block_does_not_expose_phantom_declarations() {
        let text = r#"import org.junit.Test;
class Suite {
  String text = """
    \"""
    @Test public void phantom() {}
    \"""
    """;
}"#;
        let facts = source_facts(text, false);
        assert!(facts.lexical_coverage_complete);
        assert!(facts.annotations.is_empty());
        assert!(facts.method_candidates.is_empty());
        assert!(confirmed_names(&facts).is_empty());
    }

    #[test]
    fn kotlin_raw_triples_keep_backslash_literal_and_close_at_three_quotes() {
        let text = r#"import org.junit.Test
class Suite {
  val text = """\""";
  @Test fun actual() {}
}"#;
        let facts = source_facts(text, true);
        assert!(facts.lexical_coverage_complete);
        assert_eq!(confirmed_names(&facts), ["actual"]);
        assert_eq!(facts.annotations.len(), 1);
    }

    #[test]
    fn enum_type_shadowing_is_retained_for_simple_and_qualified_annotations() {
        for (text, kotlin, shadow) in [
            (
                "import org.junit.Test; class Suite { enum Test { Value } @Test public void actual() {} }",
                false,
                "Test",
            ),
            (
                "class Suite { enum org { Value } @org.junit.Test public void actual() {} }",
                false,
                "org",
            ),
            (
                "import org.junit.Test\nclass Suite { enum class Test { Value } @Test fun actual() {} }",
                true,
                "Test",
            ),
        ] {
            let facts = source_facts(text, kotlin);
            assert!(confirmed_names(&facts).is_empty());
            assert!(
                facts
                    .classes
                    .iter()
                    .any(|class| class.name == shadow && class.declaration_kind == "enum")
            );
            assert!(
                facts
                    .method_candidates
                    .iter()
                    .any(|method| method.name == "actual")
            );
        }
        let facts = source_facts(
            "import org.junit.Test\nenum class Suite { Value; @Test fun actual() {} }",
            true,
        );
        assert_eq!(facts.method_candidates[0].owner.as_deref(), Some("Suite"));
        assert_eq!(facts.classes[0].declaration_kind, "enum");
        assert!(facts.method_candidates[0].effective_runtime_cases.is_none());
    }

    #[test]
    fn imported_alias_static_and_prefix_names_participate_in_binding_conflicts() {
        for (text, kotlin) in [
            (
                "import org.junit.Test\nimport other.Annotation as Test\nclass Suite { @Test fun actual() {} }",
                true,
            ),
            (
                "import other.Container as org\nclass Suite { @org.junit.Test fun actual() {} }",
                true,
            ),
            (
                "import org.junit.Test; import static other.Container.Test; class Suite { @Test public void actual() {} }",
                false,
            ),
            (
                "import static other.Container.org; class Suite { @org.junit.Test public void actual() {} }",
                false,
            ),
            (
                "import other.*; class Suite { @org.junit.Test public void actual() {} }",
                false,
            ),
        ] {
            let facts = source_facts(text, kotlin);
            assert!(
                confirmed_names(&facts).is_empty(),
                "unexpected import binding: {text}"
            );
            assert_eq!(facts.method_candidates.len(), 1);
            assert!(
                facts.method_candidates[0]
                    .unresolved
                    .iter()
                    .any(|reason| reason.contains("binding unresolved"))
            );
            assert!(facts.method_candidates[0].effective_runtime_cases.is_none());
        }
    }

    #[test]
    fn missing_reserved_and_java_alias_syntax_is_retained_without_confirmation() {
        for (text, kotlin) in [
            (
                "import org.junit.Test as ;\nclass Suite { @Test fun actual() {} }",
                true,
            ),
            (
                "import org.junit.Test as class;\nclass Suite { @org.junit.Test fun actual() {} }",
                true,
            ),
            (
                "import org.junit.Test as $Check;\nclass Suite { @org.junit.Test fun actual() {} }",
                true,
            ),
            (
                "import org.junit.Test as ``;\nclass Suite { @org.junit.Test fun actual() {} }",
                true,
            ),
            (
                "import other.Annotation as Check; class Suite { @org.junit.Test public void actual() {} }",
                false,
            ),
        ] {
            let facts = source_facts(text, kotlin);
            assert!(
                confirmed_names(&facts).is_empty(),
                "unexpected malformed alias binding: {text}"
            );
            assert_eq!(facts.method_candidates.len(), 1);
            assert!(!facts.imports.is_empty());
            assert!(
                facts
                    .unresolved
                    .iter()
                    .any(|reason| reason.contains("unsupported alias syntax"))
            );
        }
    }

    #[test]
    fn generic_org_type_scope_is_retained_without_qualified_binding_guesses() {
        for (text, kotlin) in [
            (
                "class Suite<org> { @org.junit.Test public void actual() {} }",
                false,
            ),
            ("class Suite<org> { @org.junit.Test fun actual() {} }", true),
            (
                "class Suite { public <org> @org.junit.Test void actual() {} }",
                false,
            ),
        ] {
            let facts = source_facts(text, kotlin);
            assert!(
                confirmed_names(&facts).is_empty(),
                "unexpected generic prefix binding: {text}"
            );
            assert_eq!(facts.method_candidates.len(), 1);
            assert!(
                facts.method_candidates[0]
                    .annotations
                    .iter()
                    .any(|annotation| annotation.spelling == "org.junit.Test")
            );
            assert!(facts.method_candidates[0].effective_runtime_cases.is_none());
        }
    }

    #[test]
    fn malformed_delimiters_literals_imports_and_java_unicode_never_confirm() {
        for text in [
            "import org.junit.Test; class Suite { @Test public void testA() {}",
            "import org.junit.Test; class Suite { @Test public void testA(] {} }",
            "import org.junit.Test; class Suite { @Test public void testA() { String value = \"unterminated; } }",
            "import org.junit.Test; class Suite { @Test public void testA() {} /* unclosed",
            r"import org.junit.Test; class Suite { @Test public void testA() {} } // \u007d",
            "import org.junit.Test class Suite { @Test public void testA() {} }",
        ] {
            let facts = source_facts(text, false);
            assert!(
                confirmed_names(&facts).is_empty(),
                "unexpected incomplete declaration: {text}"
            );
            assert!(!facts.unresolved.is_empty());
        }
    }

    #[test]
    fn scanner_budgets_are_explicit_and_never_claim_remaining_coverage() {
        let tokens = format!(
            "{} import org.junit.Test; class Suite {{ @Test public void testLate() {{}} }}",
            ";".repeat(MAX_TOKENS)
        );
        let facts = source_facts(&tokens, false);
        assert!(!facts.lexical_coverage_complete);
        assert!(
            facts
                .unresolved
                .iter()
                .any(|reason| reason.contains("token budget"))
        );
        assert!(confirmed_names(&facts).is_empty());
        let methods = (0..MAX_DECLARATIONS + 1)
            .map(|index| format!("@Test public void test{index}() {{}} "))
            .collect::<String>();
        let facts = source_facts(
            &format!("import org.junit.Test; class Suite {{ {methods} }}"),
            false,
        );
        assert_eq!(facts.method_candidates.len(), MAX_DECLARATIONS);
        assert!(
            facts
                .unresolved
                .iter()
                .any(|reason| reason.contains("method declaration budget"))
        );
        let oversized_header = format!(
            "import org.junit.Test; class Suite extends {} {{ @Test public void testA() {{}} }}",
            "A".repeat(MAX_HEADER_BYTES + 1)
        );
        let facts = source_facts(&oversized_header, false);
        assert!(confirmed_names(&facts).is_empty());
        assert!(
            facts
                .unresolved
                .iter()
                .any(|reason| reason.contains("header budget"))
        );
    }

    struct FixtureHarness {
        directory: tempfile::TempDir,
        repository: PathBuf,
        source_root: PathBuf,
        selection: PathBuf,
    }

    impl FixtureHarness {
        fn new() -> Result<Self> {
            let directory = tempfile::tempdir()?;
            let repository = directory.path().join("repository");
            let source_root = directory.path().join("sources");
            fs::create_dir_all(repository.join("docs/android-studio"))?;
            fs::copy(
                super::tests::repository()?.join("docs/android-studio/reference-manifest.json"),
                repository.join("docs/android-studio/reference-manifest.json"),
            )?;
            let selection = directory.path().join("selection.json");
            let mut original: serde_json::Value =
                serde_json::from_slice(&fs::read(fixture_root().join("selection.json"))?)?;
            let selected = original["files"]
                .as_array()
                .context("fixture selection array")?
                .iter()
                .find(|file| file["source"] == "idea" && file["path"] == DEFAULT_VARIANTS)
                .context("fixture selection item")?
                .clone();
            original["files"] = serde_json::json!([selected]);
            fs::write(&selection, serde_json::to_vec_pretty(&original)?)?;
            let fixture = selected["fixture"].as_str().context("fixture path")?;
            let target = source_root.join(fixture);
            fs::create_dir_all(target.parent().context("fixture parent")?)?;
            fs::copy(fixture_root().join("sources").join(fixture), target)?;
            Ok(Self {
                directory,
                repository,
                source_root,
                selection,
            })
        }

        fn args(&self, output: PathBuf, check: bool) -> AndroidJunit4DeclarationsArgs {
            AndroidJunit4DeclarationsArgs {
                repository: self.repository.clone(),
                source_root: self.source_root.clone(),
                selection: self.selection.clone(),
                output,
                check,
            }
        }

        fn mutate_selection(&self, mutation: impl FnOnce(&mut serde_json::Value)) -> Result<()> {
            let mut selection: serde_json::Value =
                serde_json::from_slice(&fs::read(&self.selection)?)?;
            mutation(&mut selection);
            fs::write(&self.selection, serde_json::to_vec_pretty(&selection)?)?;
            Ok(())
        }
    }

    #[test]
    fn complete_original_selection_is_deterministic_and_earns_no_new_parity_credit() -> Result<()> {
        let selection = fixture_root().join("selection.json");
        let first = generate(&repository()?, &fixture_root().join("sources"), &selection)?;
        let second = generate(&repository()?, &fixture_root().join("sources"), &selection)?;
        assert_eq!(first, second);
        let evidence: serde_json::Value = serde_json::from_slice(&first)?;
        assert_eq!(
            evidence["files"].as_array().context("file evidence")?.len(),
            16
        );
        assert_eq!(evidence["effective_test_census_complete"], false);
        assert_eq!(evidence["declaration_coverage_complete"], false);
        assert_eq!(evidence["selected_archive_membership_reverified"], false);
        assert_eq!(evidence["new_behavioral_parity_credit"], 0);
        assert!(evidence["effective_runtime_cases"].is_null());
        let same_file = evidence["files"]
            .as_array()
            .context("file evidence")?
            .iter()
            .filter(|file| file["selected"]["path"] == GRADLE_IMPORT)
            .collect::<Vec<_>>();
        assert_eq!(same_file.len(), 2);
        assert_ne!(
            same_file[0]["selected"]["source"],
            same_file[1]["selected"]["source"]
        );
        assert_eq!(
            same_file[0]["selected"]["sha256"],
            same_file[1]["selected"]["sha256"]
        );
        Ok(())
    }

    #[test]
    fn external_publication_check_and_changed_evidence_never_overwrite() -> Result<()> {
        let harness = FixtureHarness::new()?;
        let output = harness.directory.path().join("evidence.json");
        run(harness.args(output.clone(), false))?;
        let original = fs::read(&output)?;
        run(harness.args(output.clone(), true))?;
        assert_eq!(fs::read(&output)?, original);
        assert!(run(harness.args(output.clone(), false)).is_err());
        assert_eq!(fs::read(&output)?, original);
        fs::write(&output, b"changed evidence\n")?;
        assert!(run(harness.args(output.clone(), true)).is_err());
        assert_eq!(fs::read(output)?, b"changed evidence\n");
        Ok(())
    }

    #[test]
    fn rejected_checkout_output_has_no_filesystem_side_effects() -> Result<()> {
        let harness = FixtureHarness::new()?;
        let output = harness.repository.join("evidence.json");
        assert!(run(harness.args(output.clone(), false)).is_err());
        assert!(!output.exists());
        assert_eq!(fs::read_dir(&harness.repository)?.count(), 1);
        Ok(())
    }

    #[test]
    fn provenance_hash_revision_and_duplicate_identity_fail_before_publication() -> Result<()> {
        for field in [
            "sha256",
            "revision",
            "duplicate",
            "license",
            "bytes",
            "fixture",
            "unknown",
        ] {
            let harness = FixtureHarness::new()?;
            harness.mutate_selection(|selection| match field {
                "sha256" => selection["files"][0]["sha256"] = serde_json::json!("0".repeat(64)),
                "revision" => selection["files"][0]["revision"] = serde_json::json!("0".repeat(40)),
                "duplicate" => {
                    selection["files"] = serde_json::json!([
                        selection["files"][0].clone(),
                        selection["files"][0].clone()
                    ])
                }
                "license" => selection["files"][0]["license"] = serde_json::json!("unknown"),
                "bytes" => selection["files"][0]["bytes"] = serde_json::json!(1),
                "fixture" => selection["files"][0]["fixture"] = serde_json::json!("../escape.kt"),
                "unknown" => selection["unsupported"] = serde_json::json!(true),
                _ => unreachable!("fixed corruption fields"),
            })?;
            let output = harness.directory.path().join("evidence.json");
            assert!(
                run(harness.args(output.clone(), false)).is_err(),
                "accepted invalid provenance {field}"
            );
            assert!(!output.exists());
        }
        Ok(())
    }

    #[test]
    fn oversized_selection_and_source_inputs_are_rejected_before_publication() -> Result<()> {
        let harness = FixtureHarness::new()?;
        fs::write(
            &harness.selection,
            vec![b' '; MAX_SELECTION_BYTES as usize + 1],
        )?;
        let output = harness.directory.path().join("evidence.json");
        assert!(run(harness.args(output.clone(), false)).is_err());
        assert!(!output.exists());
        let harness = FixtureHarness::new()?;
        harness.mutate_selection(|selection| {
            selection["files"][0]["bytes"] = serde_json::json!(MAX_SOURCE_BYTES + 1)
        })?;
        let output = harness.directory.path().join("evidence.json");
        assert!(run(harness.args(output.clone(), false)).is_err());
        assert!(!output.exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn source_and_existing_output_symlinks_cannot_escape_guards() -> Result<()> {
        let harness = FixtureHarness::new()?;
        let original = harness.source_root.join("idea").join(DEFAULT_VARIANTS);
        let outside = harness.directory.path().join("outside.kt");
        fs::rename(&original, &outside)?;
        std::os::unix::fs::symlink(&outside, &original)?;
        let output = harness.directory.path().join("evidence.json");
        assert!(run(harness.args(output.clone(), false)).is_err());
        assert!(!output.exists());
        let harness = FixtureHarness::new()?;
        let output = harness.directory.path().join("evidence.json");
        let outside = harness.directory.path().join("outside.json");
        fs::write(&outside, b"untouched\n")?;
        std::os::unix::fs::symlink(&outside, &output)?;
        assert!(run(harness.args(output, true)).is_err());
        assert_eq!(fs::read(outside)?, b"untouched\n");
        Ok(())
    }
    fn synthetic_java(package_bytes: usize, methods: usize) -> String {
        let methods = (0..methods)
            .map(|index| format!("@Test void test{index}() {{}} "))
            .collect::<String>();
        format!(
            "package {}; import org.junit.Test; class Suite {{ {methods} }}",
            "a".repeat(package_bytes)
        )
    }

    fn synthetic_selection(harness: &FixtureHarness, sources: &[(&str, &str)]) -> Result<()> {
        let mut selection: serde_json::Value =
            serde_json::from_slice(&fs::read(&harness.selection)?)?;
        let template = selection["files"]
            .as_array()
            .context("synthetic selection array")?
            .first()
            .context("synthetic selection template")?
            .clone();
        let mut files = Vec::new();
        for (path, text) in sources {
            let mut selected = template.clone();
            let fixture = format!("idea/{path}");
            selected["path"] = serde_json::json!(path);
            selected["fixture"] = serde_json::json!(fixture);
            selected["bytes"] = serde_json::json!(text.len());
            selected["sha256"] = serde_json::json!(sha256(text.as_bytes()));
            let target = harness.source_root.join(&fixture);
            fs::create_dir_all(target.parent().context("synthetic source parent")?)?;
            fs::write(target, text)?;
            files.push(selected);
        }
        selection["files"] = serde_json::json!(files);
        fs::write(&harness.selection, serde_json::to_vec_pretty(&selection)?)?;
        Ok(())
    }

    #[test]
    fn metadata_budget_rejects_owned_copies_and_overflow_before_charging() -> Result<()> {
        let mut metadata = MetadataBudget::new(5);
        assert_eq!(metadata.owned("12345")?, "12345");
        assert_eq!(metadata.used, 5);
        assert!(metadata.owned("6").is_err());
        assert_eq!(metadata.used, 5);
        let mut overflow = MetadataBudget::new(usize::MAX);
        overflow.used = usize::MAX;
        assert!(overflow.charge(1).is_err());
        assert_eq!(overflow.used, usize::MAX);
        let annotation = AnnotationFact {
            spelling: "Test".to_owned(),
            use_site_target: Some("get".to_owned()),
            span: Span {
                start_byte: 0,
                end_byte: 4,
                start_line: 1,
                end_line: 1,
            },
        };
        let mut metadata = MetadataBudget::new(7);
        let copied = metadata.annotation(&annotation)?;
        assert_eq!(copied.spelling, "Test");
        assert_eq!(copied.use_site_target.as_deref(), Some("get"));
        assert_eq!(metadata.used, 7);
        assert!(metadata.annotation(&annotation).is_err());
        assert_eq!(metadata.used, 7);
        Ok(())
    }

    #[test]
    fn actual_large_package_owner_amplification_is_rejected_before_retention() -> Result<()> {
        let source = synthetic_java(1024 * 1024, MAX_DECLARATIONS);
        assert!(source.len() as u64 <= MAX_SOURCE_BYTES);
        let (tokens, lexical_gaps) = tokenize(
            &source,
            false,
            &mut MetadataBudget::new(MAX_TOTAL_BYTES as usize),
        )?;
        assert!(tokens.len() < MAX_TOKENS);
        assert!(lexical_gaps.is_empty());
        assert_eq!(tokens.iter().filter(|token| token.is("@")).count(), 512);
        let mut metadata = MetadataBudget::new(MAX_TOTAL_BYTES as usize);
        let error = source_facts_with_budget(&source, false, &mut metadata)
            .expect_err("owner amplification must not retain all owners");
        assert!(error.to_string().contains("metadata budget"));
        assert!(metadata.used <= metadata.limit);
        let small = source_facts(&synthetic_java(16, 2), false);
        assert_eq!(small.classes.len(), 1);
        assert_eq!(small.method_candidates.len(), 2);
        assert!(small.method_candidates.iter().all(|method| {
            method.junit4_annotation_declared
                && method.effective_runtime_cases.is_none()
                && method.applicability == "unreviewed"
        }));
        Ok(())
    }

    #[test]
    fn one_metadata_budget_is_shared_across_sources_and_the_generation_loop() -> Result<()> {
        let source = synthetic_java(4096, 8);
        let mut measured = MetadataBudget::new(MAX_TOTAL_BYTES as usize);
        source_facts_with_budget(&source, false, &mut measured)?;
        let mut shared = MetadataBudget::new(measured.used);
        source_facts_with_budget(&source, false, &mut shared)?;
        assert!(source_facts_with_budget(&source, false, &mut shared).is_err());
        assert!(shared.used <= shared.limit);

        let harness = FixtureHarness::new()?;
        synthetic_selection(&harness, &[("synthetic/Suite.java", &source)])?;
        let single_selection_bytes = fs::read(&harness.selection)?.len();
        let mut single = MetadataBudget::new(MAX_TOTAL_BYTES as usize);
        generate_with_budget(
            &harness.repository,
            &harness.source_root,
            &harness.selection,
            &mut single,
            MAX_TOTAL_BYTES as usize,
        )?;
        synthetic_selection(
            &harness,
            &[
                ("synthetic/Suite.java", &source),
                ("synthetic/Other.java", &source),
            ],
        )?;
        let additional_selection_bytes =
            fs::read(&harness.selection)?.len() - single_selection_bytes;
        let mut cumulative = MetadataBudget::new(single.used + additional_selection_bytes);
        let error = generate_with_budget(
            &harness.repository,
            &harness.source_root,
            &harness.selection,
            &mut cumulative,
            MAX_TOTAL_BYTES as usize,
        )
        .expect_err("each source must consume the same selection-wide budget");
        assert!(error.to_string().contains("metadata budget"));
        assert!(cumulative.used <= cumulative.limit);
        Ok(())
    }

    #[test]
    fn expanded_annotation_headers_obey_the_original_full_span_boundary() -> Result<()> {
        let header = |bytes: usize| {
            let before = "@Context(\"";
            let after = "\") class Suite ";
            format!(
                "{before}{}{after}{{ @org.junit.Test void testA() {{}} }}",
                "a".repeat(bytes - before.len() - after.len())
            )
        };
        let exact = source_facts(&header(MAX_HEADER_BYTES), false);
        assert_eq!(exact.classes.len(), 1);
        assert_eq!(exact.classes[0].header.len(), MAX_HEADER_BYTES);
        assert_eq!(confirmed_names(&exact), vec!["testA"]);
        let beyond = source_facts(&header(MAX_HEADER_BYTES + 1), false);
        assert!(beyond.classes.is_empty());
        assert!(confirmed_names(&beyond).is_empty());
        assert!(
            beyond
                .unresolved
                .iter()
                .any(|reason| reason.contains("header budget"))
        );
        let source = header(1024 * 1024);
        let large = source_facts(&source, false);
        assert!(large.classes.is_empty());
        assert!(confirmed_names(&large).is_empty());
        assert_eq!(large.annotations[0].spelling, "Context");
        assert_eq!(large.annotations[0].span.start_byte, 0);
        let harness = FixtureHarness::new()?;
        synthetic_selection(&harness, &[("synthetic/Suite.java", &source)])?;
        let bytes = generate(
            &harness.repository,
            &harness.source_root,
            &harness.selection,
        )?;
        let evidence: serde_json::Value = serde_json::from_slice(&bytes)?;
        assert_eq!(
            evidence["files"][0]["selected"]["sha256"],
            sha256(source.as_bytes())
        );
        assert_eq!(evidence["files"][0]["selected"]["bytes"], source.len());
        assert_eq!(
            evidence["files"][0]["source_facts"]["annotations"][0]["spelling"],
            "Context"
        );
        assert_eq!(evidence["effective_test_census_complete"], false);
        assert_eq!(evidence["new_behavioral_parity_credit"], 0);
        assert!(evidence["effective_runtime_cases"].is_null());
        Ok(())
    }

    #[test]
    fn bounded_json_writer_checks_growth_escaping_and_the_final_newline() -> Result<()> {
        let mut writer = BoundedJsonWriter::new(8);
        writer.write_all(b"12345678")?;
        let capacity = writer.bytes.capacity();
        assert!(capacity <= 8);
        assert!(writer.write_all(b"9").is_err());
        assert_eq!(writer.bytes, b"12345678");
        assert_eq!(writer.bytes.capacity(), capacity);
        let mut empty = BoundedJsonWriter::new(8);
        assert!(empty.write_all(&vec![b'a'; 1024 * 1024]).is_err());
        assert!(empty.bytes.is_empty());
        assert_eq!(empty.bytes.capacity(), 0);

        let value = serde_json::json!({"line": "\u{0}\n\""});
        let mut expected = serde_json::to_vec_pretty(&value)?;
        let mut exact = BoundedJsonWriter::new(expected.len() + 1);
        serde_json::to_writer_pretty(&mut exact, &value)?;
        expected.push(b'\n');
        assert_eq!(exact.finish()?, expected);
        let mut short = BoundedJsonWriter::new(expected.len() - 1);
        serde_json::to_writer_pretty(&mut short, &value)?;
        assert!(short.finish().is_err());
        let mut escaping = BoundedJsonWriter::new(8);
        assert!(serde_json::to_writer_pretty(&mut escaping, "\u{0}\u{0}\u{0}").is_err());
        assert!(escaping.bytes.len() <= 8);
        assert!(escaping.bytes.capacity() <= 8);
        let mut small_writes = BoundedJsonWriter::new(1024);
        for _ in 0..511 {
            small_writes.write_all(b"a")?;
            assert!(small_writes.bytes.capacity() <= 1024);
        }
        assert_eq!(small_writes.finish()?.len(), 512);
        Ok(())
    }

    #[test]
    fn metadata_failure_preserves_publication_and_existing_checked_evidence() -> Result<()> {
        let harness = FixtureHarness::new()?;
        let small = synthetic_java(16, 2);
        synthetic_selection(&harness, &[("synthetic/Suite.java", &small)])?;
        let output = harness.directory.path().join("evidence.json");
        run(harness.args(output.clone(), false))?;
        let original = fs::read(&output)?;
        let mut metadata = MetadataBudget::new(MAX_TOTAL_BYTES as usize);
        let error = generate_with_budget(
            &harness.repository,
            &harness.source_root,
            &harness.selection,
            &mut metadata,
            1,
        )
        .expect_err("oversized serialization must be rejected");
        assert!(error.to_string().contains("output budget"));
        assert_eq!(fs::read(&output)?, original);
        let large = synthetic_java(1024 * 1024, MAX_DECLARATIONS);
        synthetic_selection(&harness, &[("synthetic/Suite.java", &large)])?;
        let rejected = harness.directory.path().join("rejected.json");
        let error = run(harness.args(rejected.clone(), false))
            .expect_err("metadata failure must precede publication");
        assert!(error.to_string().contains("metadata budget"));
        assert!(!rejected.exists());
        assert!(run(harness.args(output.clone(), true)).is_err());
        assert_eq!(fs::read(&output)?, original);
        for entry in fs::read_dir(harness.directory.path())? {
            assert!(!entry?.file_name().to_string_lossy().contains("staging"));
        }
        Ok(())
    }
}
