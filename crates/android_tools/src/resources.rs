use anyhow::{Context as _, Result, ensure};
use quick_xml::{Reader, events::Event};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ops::Range, path::PathBuf, sync::LazyLock};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceModel {
    /// AGP's outer list is highest priority first; inner directories have equal priority.
    pub layers: Vec<Vec<PathBuf>>,
    pub dependencies: Vec<ExternalResources>,
    pub framework: Option<PathBuf>,
    pub merged_manifest: Option<PathBuf>,
    #[serde(default = "non_transitive_default")]
    pub non_transitive_r: bool,
}

fn non_transitive_default() -> bool {
    true
}

#[cfg(test)]
mod exporter_contract_tests {
    use super::*;

    #[test]
    fn external_resources_preserve_exporter_public_resources_and_legacy_alias() -> Result<()> {
        let dependency: ExternalResources = serde_json::from_value(serde_json::json!({
            "namespace": "external.library", "path": "/aar/res",
            "publicResources": "/aar/public.txt"
        }))?;
        assert_eq!(
            dependency.public_resources,
            Some(PathBuf::from("/aar/public.txt"))
        );
        let serialized = serde_json::to_value(&dependency)?;
        assert_eq!(serialized["publicResources"], "/aar/public.txt");
        assert!(serialized.get("public_resources").is_none());
        let legacy: ExternalResources = serde_json::from_value(serde_json::json!({
            "namespace": "external.library", "path": "/aar/res",
            "public_resources": "/aar/legacy-public.txt"
        }))?;
        assert_eq!(
            legacy.public_resources,
            Some(PathBuf::from("/aar/legacy-public.txt"))
        );
        let absent: ExternalResources = serde_json::from_value(serde_json::json!({
            "namespace": "external.library", "path": "/aar/res"
        }))?;
        assert!(absent.public_resources.is_none());
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExternalResources {
    pub namespace: String,
    pub path: PathBuf,
    #[serde(default)]
    pub manifest: Option<PathBuf>,
    #[serde(default, rename = "publicResources", alias = "public_resources")]
    pub public_resources: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Symbol {
    pub namespace: String,
    pub kind: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct Reference {
    pub namespace: Option<String>,
    pub kind: String,
    pub name: String,
    pub range: Range<usize>,
    pub name_range: Range<usize>,
    pub declaration: bool,
}

#[derive(Clone, Debug)]
pub struct Declaration {
    pub symbol: Symbol,
    pub path: PathBuf,
    pub qualifier: String,
    pub priority: usize,
    pub generated: bool,
    pub external: bool,
    pub range: Range<usize>,
    pub name_range: Option<Range<usize>>,
    pub xml_name: String,
}

#[derive(Default, Debug)]
pub struct Resolution<'a> {
    pub winners: Vec<&'a Declaration>,
    pub shadowed: Vec<&'a Declaration>,
    pub conflicts: Vec<&'a Declaration>,
}

pub fn resolve<'a>(declarations: &'a [Declaration], symbol: &Symbol) -> Resolution<'a> {
    resolve_entries(declarations, symbol)
}

/// Resolve the already-indexed entries for a symbol using the same overlay rules.
pub fn resolve_entries<'a>(
    declarations: impl IntoIterator<Item = &'a Declaration>,
    symbol: &Symbol,
) -> Resolution<'a> {
    let mut groups: BTreeMap<&str, Vec<&Declaration>> = BTreeMap::new();
    for declaration in declarations
        .into_iter()
        .filter(|entry| &entry.symbol == symbol)
    {
        groups
            .entry(&declaration.qualifier)
            .or_default()
            .push(declaration);
    }
    let mut resolution = Resolution::default();
    for entries in groups.values_mut() {
        entries.sort_by_key(|entry| (&entry.priority, &entry.path, entry.range.start));
        let Some(first) = entries.first() else {
            continue;
        };
        let priority = first.priority;
        let (winners, shadowed): (Vec<_>, Vec<_>) = entries
            .iter()
            .copied()
            .partition(|entry| entry.priority == priority);
        if winners.len() > 1 && symbol.kind != "id" {
            resolution.conflicts.extend(winners.iter().copied());
        }
        resolution.winners.extend(winners);
        resolution.shadowed.extend(shadowed);
    }
    resolution
}

static REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
    r"\b(?:(?<namespace>[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)*)\.)?(?<class>R)\s*\.\s*(?<kind>\w+)\s*\.\s*(?<name>\w+)\b|[@?](?<create>\+)?(?:(?<xml_namespace>[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)*):)?(?<xml_kind>\w+)/(?<xml_name>[\w.]+)"
).expect("constant resource reference expression")
});
static IMPORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*import\s+(?<namespace>[\w.]+)\.R(?:\s+as\s+(?<alias>\w+))?\s*;?\s*$")
        .expect("constant R import expression")
});
static PACKAGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*package\s+([\w.]+)").expect("constant package expression")
});
static SHADOW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
    r"\b(?:class|object|interface|typealias|val|var)\s+R\b|\b[A-Za-z_$][\w$<>\[\]?]*\s+R\s*(?:[=;,:)]|$)|\bR\s*:|\bimport\s+(?:static\s+)?[\w.]+\.R[.*]"
).expect("constant R ambiguity expression")
});

// Preserve byte positions while hiding comments and strings, including Kotlin nested comments.
pub fn code_mask(text: &str) -> String {
    let mut checkpoint = || Ok(());
    code_mask_controlled(
        text,
        &mut ResourceParserControl::new(&mut checkpoint, usize::MAX),
    )
    .expect("unlimited resource code mask")
}

/// Cooperative cancellation and a separate bound for repeated whole-document
/// passes. The record limit alone cannot bound alias or binding analysis.
pub struct ResourceParserControl<'a> {
    checkpoint: &'a mut dyn FnMut() -> Result<()>,
    remaining_work: usize,
    owned: ResourceOwnedBudget,
    pub binding_checks: usize,
}

impl<'a> ResourceParserControl<'a> {
    pub fn new(checkpoint: &'a mut dyn FnMut() -> Result<()>, work: usize) -> Self {
        Self {
            checkpoint,
            remaining_work: work,
            owned: ResourceOwnedBudget::new(128 * 1024 * 1024),
            binding_checks: 0,
        }
    }

    pub fn checkpoint(&mut self) -> Result<()> {
        (self.checkpoint)()
    }

    pub fn with_owned_byte_limit(mut self, bytes: usize) -> Self {
        self.owned = ResourceOwnedBudget::new(bytes);
        self
    }

    pub fn charge_owned(&mut self, bytes: impl IntoIterator<Item = usize>) -> Result<()> {
        self.checkpoint()?;
        self.owned.charge(bytes)
    }

    pub fn charge_work(&mut self, bytes: usize) -> Result<()> {
        self.checkpoint()?;
        self.remaining_work = self
            .remaining_work
            .checked_sub(bytes)
            .ok_or_else(|| anyhow::anyhow!("Android resource parser exceeds work limit"))?;
        Ok(())
    }

    fn charge(&mut self, bytes: usize) -> Result<()> {
        self.charge_work(bytes)
    }
}

/// Preflight duplicated owned payload before allocation. Record/count limits
/// separately bound container overhead; this is not a total process RSS claim.
pub struct ResourceOwnedBudget {
    remaining: usize,
}

impl ResourceOwnedBudget {
    pub fn new(bytes: usize) -> Self {
        Self { remaining: bytes }
    }

    pub fn charge(&mut self, bytes: impl IntoIterator<Item = usize>) -> Result<()> {
        let failure = || {
            anyhow::Error::new(ResourceLimitExceeded)
                .context("Android resource scan exceeds owned byte limit")
        };
        let bytes = bytes.into_iter().try_fold(0_usize, |sum, bytes| {
            sum.checked_add(bytes).ok_or_else(failure)
        })?;
        self.remaining = self.remaining.checked_sub(bytes).ok_or_else(failure)?;
        Ok(())
    }
}

pub fn code_mask_controlled(text: &str, control: &mut ResourceParserControl<'_>) -> Result<String> {
    control.charge(text.len())?;
    control.charge_owned([text.len()])?;
    let input = text.as_bytes();
    let mut output = Vec::new();
    output.try_reserve_exact(input.len())?;
    for chunk in input.chunks(4096) {
        control.checkpoint()?;
        output.extend_from_slice(chunk);
    }
    let mut offset = 0;
    let mut next_checkpoint = 0;
    while offset < input.len() {
        if offset >= next_checkpoint {
            control.checkpoint()?;
            next_checkpoint = offset.saturating_add(4096);
        }
        let start = offset;
        if input.get(offset..offset + 2) == Some(b"//") {
            offset += 2;
            while offset < input.len() && input[offset] != b'\n' {
                if offset >= next_checkpoint {
                    control.checkpoint()?;
                    next_checkpoint = offset.saturating_add(4096);
                }
                offset += 1;
            }
        } else if input.get(offset..offset + 2) == Some(b"/*") {
            offset += 2;
            let mut depth = 1;
            while offset < input.len() && depth > 0 {
                if offset >= next_checkpoint {
                    control.checkpoint()?;
                    next_checkpoint = offset.saturating_add(4096);
                }
                if input.get(offset..offset + 2) == Some(b"/*") {
                    depth += 1;
                    offset += 2;
                } else if input.get(offset..offset + 2) == Some(b"*/") {
                    depth -= 1;
                    offset += 2;
                } else {
                    offset += 1;
                }
            }
        } else if matches!(input[offset], b'"' | b'\'') {
            let quote = input[offset];
            let triple = input.get(offset..offset + 3) == Some(b"\"\"\"");
            offset += if triple { 3 } else { 1 };
            while offset < input.len() {
                if offset >= next_checkpoint {
                    control.checkpoint()?;
                    next_checkpoint = offset.saturating_add(4096);
                }
                if triple && input.get(offset..offset + 3) == Some(b"\"\"\"") {
                    offset += 3;
                    break;
                }
                if !triple && input[offset] == quote {
                    offset += 1;
                    break;
                }
                if !triple && input[offset] == b'\\' {
                    offset = (offset + 2).min(input.len());
                } else {
                    offset += 1;
                }
            }
        } else {
            offset += 1;
            continue;
        }
        for chunk in output[start..offset].chunks_mut(4096) {
            control.checkpoint()?;
            for byte in chunk {
                if *byte != b'\n' && *byte != b'\r' {
                    *byte = b' ';
                }
            }
        }
    }
    // Every non-ASCII byte removed from a string/comment was replaced, and code was left intact.
    Ok(String::from_utf8(output).expect("mask preserves UTF-8"))
}

/// A bounded parser fails rather than returning incomplete navigation results.
#[derive(Debug)]
pub struct ResourceLimitExceeded;

impl std::fmt::Display for ResourceLimitExceeded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Android resource scan exceeds symbol limit")
    }
}

impl std::error::Error for ResourceLimitExceeded {}

fn check_resource_limit(count: usize, limit: usize) -> Result<()> {
    if count >= limit {
        return Err(ResourceLimitExceeded.into());
    }
    Ok(())
}

pub fn code_references(text: &str) -> Vec<Reference> {
    code_references_limited(text, usize::MAX).expect("unlimited resource reference parser")
}

fn code_references_limited(text: &str, limit: usize) -> Result<Vec<Reference>> {
    let mut checkpoint = || Ok(());
    code_references_controlled(
        text,
        limit,
        &mut ResourceParserControl::new(&mut checkpoint, usize::MAX),
    )
}

pub fn code_references_controlled(
    text: &str,
    limit: usize,
    control: &mut ResourceParserControl<'_>,
) -> Result<Vec<Reference>> {
    let mask = code_mask_controlled(text, control)?;
    code_references_from_mask(&mask, limit, control, &mut BTreeMap::new())
}

fn code_references_from_mask(
    mask: &str,
    limit: usize,
    control: &mut ResourceParserControl<'_>,
    bindings: &mut BTreeMap<String, bool>,
) -> Result<Vec<Reference>> {
    control.charge(mask.len())?;
    let mut imports = BTreeMap::new();
    for capture in IMPORT.captures_iter(&mask) {
        control.checkpoint()?;
        let Some(namespace) = capture.name("namespace") else {
            continue;
        };
        let alias = capture.name("alias").map_or("R", |alias| alias.as_str());
        if !imports.contains_key(alias) {
            check_resource_limit(imports.len(), limit)?;
        }
        control.charge_owned([alias.len(), namespace.as_str().len()])?;
        imports.insert(alias.to_owned(), namespace.as_str().to_owned());
    }
    control.charge_owned([mask.len()])?;
    let stripped = IMPORT.replace_all(&mask, "");
    control.charge(stripped.len())?;
    let ambiguous =
        SHADOW.is_match(&stripped) || binding_shadow_cached(&stripped, "R", control, bindings)?;
    let mut references = Vec::new();
    for capture in REFERENCE.captures_iter(&mask) {
        control.checkpoint()?;
        let (Some(found), Some(kind), Some(name)) =
            (capture.get(0), capture.name("kind"), capture.name("name"))
        else {
            continue;
        };
        let namespace = capture
            .name("namespace")
            .map(|value| value.as_str())
            .or_else(|| imports.get("R").map(String::as_str));
        let shadowed =
            if let Some(first) = namespace.and_then(|namespace| namespace.split('.').next()) {
                binding_shadow_cached(&stripped, first, control, bindings)?
            } else {
                false
            };
        if ambiguous || shadowed {
            continue;
        }
        check_resource_limit(references.len(), limit)?;
        control.charge_owned([
            namespace.map_or(0, str::len),
            kind.as_str().len(),
            name.as_str().len(),
            std::mem::size_of::<Reference>() * 2,
        ])?;
        references.push(Reference {
            namespace: namespace.map(str::to_owned),
            kind: kind.as_str().into(),
            name: name.as_str().into(),
            range: found.range(),
            name_range: name.range(),
            declaration: false,
        });
    }
    for (alias, namespace) in imports.iter().filter(|(alias, _)| *alias != "R") {
        if binding_shadow_cached(&stripped, alias, control, bindings)? {
            continue;
        }
        let expression = Regex::new(&format!(
            r"\b{}\s*\.\s*(\w+)\s*\.\s*(\w+)\b",
            regex::escape(alias)
        ));
        let Ok(expression) = expression else { continue };
        control.charge(mask.len())?;
        for capture in expression.captures_iter(&mask) {
            control.checkpoint()?;
            let (Some(found), Some(kind), Some(name)) =
                (capture.get(0), capture.get(1), capture.get(2))
            else {
                continue;
            };
            check_resource_limit(references.len(), limit)?;
            control.charge_owned([
                namespace.len(),
                kind.as_str().len(),
                name.as_str().len(),
                std::mem::size_of::<Reference>() * 2,
            ])?;
            references.push(Reference {
                namespace: Some(namespace.clone()),
                kind: kind.as_str().into(),
                name: name.as_str().into(),
                range: found.range(),
                name_range: name.range(),
                declaration: false,
            });
        }
    }
    references.sort_by_key(|reference| reference.range.start);
    Ok(references)
}

pub fn code_namespace(text: &str, fallback: &str) -> String {
    PACKAGE
        .captures(&code_mask(text))
        .and_then(|capture| capture.get(1))
        .map_or_else(|| fallback.into(), |value| value.as_str().into())
}

/// Return only an explicitly declared package; callers can retain a shared
/// fallback namespace instead of allocating it once per source document.
pub fn code_declared_namespace_controlled(
    text: &str,
    control: &mut ResourceParserControl<'_>,
) -> Result<Option<String>> {
    let mask = code_mask_controlled(text, control)?;
    let Some(namespace) = PACKAGE.captures(&mask).and_then(|capture| capture.get(1)) else {
        return Ok(None);
    };
    control.charge_owned([namespace.as_str().len()])?;
    Ok(Some(namespace.as_str().to_owned()))
}

fn binding_shadow(mask: &str, identifier: &str) -> bool {
    let identifier = regex::escape(identifier);
    Regex::new(&format!(r"\b(?:enum|class|object|interface|typealias|val|var|fun|record)\s+{identifier}\b|\b{identifier}\s*(?::|->|\bin\b|[,);}}{{=>(])|\b[A-Za-z_$][\w$<>\[\]?]*\s+{identifier}\s*(?:[=;,:)]|$)"))
        .is_ok_and(|expression| expression.is_match(mask))
}

fn binding_shadow_cached(
    mask: &str,
    identifier: &str,
    control: &mut ResourceParserControl<'_>,
    bindings: &mut BTreeMap<String, bool>,
) -> Result<bool> {
    control.checkpoint()?;
    if let Some(value) = bindings.get(identifier) {
        return Ok(*value);
    }
    control.charge(mask.len())?;
    control.binding_checks += 1;
    let value = binding_shadow(mask, identifier);
    bindings.insert(identifier.to_owned(), value);
    Ok(value)
}

pub fn rename_code_is_unambiguous(text: &str) -> bool {
    let mut checkpoint = || Ok(());
    rename_code_is_unambiguous_controlled(
        text,
        usize::MAX,
        &mut ResourceParserControl::new(&mut checkpoint, usize::MAX),
    )
    .expect("unlimited resource rename parser")
}

pub fn rename_code_is_unambiguous_controlled(
    text: &str,
    limit: usize,
    control: &mut ResourceParserControl<'_>,
) -> Result<bool> {
    code_rename_analysis_controlled(text, limit, control).map(|(safe, _)| safe)
}

pub fn code_rename_analysis_controlled(
    text: &str,
    limit: usize,
    control: &mut ResourceParserControl<'_>,
) -> Result<(bool, Vec<Reference>)> {
    let mask = code_mask_controlled(text, control)?;
    control.charge(mask.len())?;
    let stripped = IMPORT.replace_all(&mask, "");
    let mut names = BTreeMap::new();
    let mut bindings = BTreeMap::new();
    for import in IMPORT.captures_iter(&mask) {
        control.checkpoint()?;
        check_resource_limit(names.len(), limit)?;
        let alias = import.name("alias").map_or("R", |alias| alias.as_str());
        let Some(namespace) = import.name("namespace") else {
            return Ok((false, Vec::new()));
        };
        if names.insert(alias, namespace.as_str()).is_some()
            || binding_shadow_cached(&stripped, alias, control, &mut bindings)?
        {
            return Ok((false, Vec::new()));
        }
    }
    control.charge(stripped.len())?;
    if binding_shadow_cached(&stripped, "R", control, &mut bindings)? || SHADOW.is_match(&stripped)
    {
        return Ok((false, Vec::new()));
    }
    control.charge(stripped.len())?;
    if Regex::new(r"\b(?:extends|implements)\b|\b(?:class|object|interface)\b[^{};]*:")
        .is_ok_and(|expression| expression.is_match(&stripped))
    {
        return Ok((false, Vec::new()));
    }
    if !rename_scopes_are_unambiguous_controlled(&stripped, control)? {
        return Ok((false, Vec::new()));
    }
    control.charge(stripped.len())?;
    if Regex::new(r"(?m)^\s*import[^\n;]*(?:\bR\b|\*)")
        .is_ok_and(|expression| expression.is_match(&stripped))
    {
        return Ok((false, Vec::new()));
    }
    let references = code_references_from_mask(&mask, limit, control, &mut bindings)?;
    for reference in &references {
        if let Some(namespace) = &reference.namespace
            && let Some(first) = namespace.split('.').next()
            && binding_shadow_cached(&stripped, first, control, &mut bindings)?
        {
            return Ok((false, Vec::new()));
        }
    }
    control.charge(text.len())?;
    Ok((!code_rename_has_unsupported_syntax(text), references))
}

pub fn code_rename_has_unsupported_syntax(text: &str) -> bool {
    text.contains("\\u") || text.contains('`') || text.contains("\"\"\"") || text.contains("${")
}

fn rename_scopes_are_unambiguous_controlled(
    mask: &str,
    control: &mut ResourceParserControl<'_>,
) -> Result<bool> {
    // A lexer cannot prove bindings supplied by implicit receivers, extension
    // receivers or anonymous superclasses. Accept only recognizable declaration
    // and control-flow blocks; other scopes need language-server verification.
    let unsupported = Regex::new(
        r"\bnew\b[^{};]*\{|\bcontext\s*\(|\b(?:fun|val|var)\s+(?:<[^>{}]*>\s*)?[\w<>?]+\s*\.",
    );
    control.charge(mask.len())?;
    if unsupported.is_ok_and(|expression| expression.is_match(mask)) {
        return Ok(false);
    }
    let Ok(block) = Regex::new(
        r"(?:\b(?:class|interface|enum|object)\s+\w+[^{};\n]*|\bfun\s+\w+\s*\([^{};=]*\)(?:\s*:\s*[^{};=]+)?|\b(?:void|int|long|boolean|byte|short|float|double|char|[A-Z]\w*(?:<[^{};]*>)?(?:\[\])?)\s+\w+\s*\([^{};]*\)|\b(?:if|for|while|when|switch|catch|synchronized)\s*\([^{};]*\)|\b(?:else|try|finally|do))\s*$",
    ) else {
        return Ok(false);
    };
    for (offset, _) in mask.match_indices('{') {
        control.checkpoint()?;
        let start = mask[..offset]
            .char_indices()
            .rev()
            .nth(512)
            .map_or(0, |(offset, _)| offset);
        if !block.is_match(&mask[start..offset]) {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn xml_rename_is_unambiguous(text: &str, kind: &str, name: &str) -> Result<bool> {
    let mut checkpoint = || Ok(());
    xml_rename_is_unambiguous_controlled(
        text,
        kind,
        name,
        &mut ResourceParserControl::new(&mut checkpoint, usize::MAX),
    )
}

pub fn xml_rename_is_unambiguous_controlled(
    text: &str,
    kind: &str,
    name: &str,
    control: &mut ResourceParserControl<'_>,
) -> Result<bool> {
    control.charge(text.len())?;
    let expression = Regex::new(&format!(
        r"[@?]\+?(?:[\w.]+:)?{}/{}\b|\b\w*R\s*\.\s*{}\s*\.\s*{}\b",
        regex::escape(kind),
        regex::escape(name),
        regex::escape(kind),
        regex::escape(name)
    ))?;
    let mut reader = Reader::from_str(text);
    let mut encoded_reference = None;
    let check = |value: &[u8]| -> Result<bool> {
        let raw = std::str::from_utf8(value)?;
        let decoded = quick_xml::escape::unescape(raw)?;
        Ok(!decoded.contains("@{")
            && !decoded.contains("@={")
            && !(expression.is_match(&decoded) && decoded != raw))
    };
    loop {
        control.checkpoint()?;
        match reader.read_event()? {
            Event::Start(element) | Event::Empty(element) => {
                for attribute in element.attributes() {
                    let attribute = attribute?;
                    if element.name().as_ref() == b"public" && attribute.value.contains(&b'&') {
                        return Ok(false);
                    }
                    if !check(attribute.value.as_ref())? {
                        return Ok(false);
                    }
                }
            }
            Event::Text(text) => {
                if !check(text.as_ref())? {
                    return Ok(false);
                }
            }
            Event::GeneralRef(_) => {
                // Numeric/entity references split text events in quick-xml.
                let matches = if let Some(matches) = encoded_reference {
                    matches
                } else {
                    control.charge(text.len())?;
                    let decoded = quick_xml::escape::unescape(text)?;
                    let matches = expression.is_match(&decoded);
                    encoded_reference = Some(matches);
                    matches
                };
                if matches {
                    return Ok(false);
                }
            }
            Event::Eof => return Ok(true),
            _ => {}
        }
    }
}

pub fn references(text: &str, xml: bool) -> Result<Vec<Reference>> {
    references_limited(text, xml, usize::MAX)
}

/// Parse references without allocating more than the supplied record limit.
pub fn references_limited(text: &str, xml: bool, limit: usize) -> Result<Vec<Reference>> {
    let mut checkpoint = || Ok(());
    references_controlled(
        text,
        xml,
        limit,
        &mut ResourceParserControl::new(&mut checkpoint, usize::MAX),
    )
}

pub fn references_controlled(
    text: &str,
    xml: bool,
    limit: usize,
    control: &mut ResourceParserControl<'_>,
) -> Result<Vec<Reference>> {
    if !xml {
        return code_references_controlled(text, limit, control);
    }
    control.charge(text.len())?;
    let mut reader = Reader::from_str(text);
    let mut references = Vec::new();
    let mut named_usages = Vec::new();
    let reference_count = std::cell::Cell::new(0_usize);
    let mut append = |value: &[u8], control: &mut ResourceParserControl<'_>| -> Result<()> {
        let start = (value.as_ptr() as usize)
            .checked_sub(text.as_ptr() as usize)
            .filter(|offset| offset + value.len() <= text.len())
            .ok_or_else(|| anyhow::anyhow!("Resource XML value is outside its input"))?;
        let value = std::str::from_utf8(value)?;
        for capture in REFERENCE.captures_iter(value) {
            control.checkpoint()?;
            let (Some(found), Some(kind), Some(name)) = (
                capture.get(0),
                capture.name("xml_kind"),
                capture.name("xml_name"),
            ) else {
                continue;
            };
            if value.trim() != found.as_str() {
                continue;
            }
            check_resource_limit(reference_count.get(), limit)?;
            reference_count.set(reference_count.get() + 1);
            references.push(Reference {
                namespace: capture
                    .name("xml_namespace")
                    .map(|value| value.as_str().into()),
                kind: kind.as_str().into(),
                name: name.as_str().replace('.', "_"),
                range: start + found.start()..start + found.end(),
                name_range: start + name.start()..start + name.end(),
                declaration: capture.name("create").is_some() && kind.as_str() == "id",
            });
        }
        Ok(())
    };
    loop {
        control.checkpoint()?;
        match reader.read_event()? {
            Event::Start(element) | Event::Empty(element) => {
                if element.name().as_ref() == b"public" {
                    let attributes = element
                        .attributes()
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    let kind = attributes
                        .iter()
                        .find(|entry| entry.key.as_ref() == b"type");
                    let name = attributes
                        .iter()
                        .find(|entry| entry.key.as_ref() == b"name");
                    if let (Some(kind), Some(name)) = (kind, name) {
                        let start = (name.value.as_ref().as_ptr() as usize)
                            .checked_sub(text.as_ptr() as usize)
                            .context("Public resource attribute outside input")?;
                        check_resource_limit(reference_count.get(), limit)?;
                        reference_count.set(reference_count.get() + 1);
                        named_usages.push(Reference {
                            namespace: None,
                            kind: String::from_utf8(kind.value.to_vec())?,
                            name: String::from_utf8(name.value.to_vec())?,
                            range: start..start + name.value.len(),
                            name_range: start..start + name.value.len(),
                            declaration: false,
                        });
                    }
                }
                for attribute in element.attributes() {
                    append(attribute?.value.as_ref(), control)?;
                }
            }
            Event::Text(value) => append(value.as_ref(), control)?,
            Event::Eof => break,
            _ => {}
        }
    }
    if references.len().saturating_add(named_usages.len()) > limit {
        return Err(ResourceLimitExceeded.into());
    }
    references.extend(named_usages);
    Ok(references)
}

pub fn declarations(
    text: &str,
    namespace: &str,
) -> Result<Vec<(String, String, Range<usize>, Range<usize>)>> {
    declarations_limited(text, namespace, usize::MAX)
}

/// Parse declarations without allocating more than the supplied record limit.
pub fn declarations_limited(
    text: &str,
    namespace: &str,
    limit: usize,
) -> Result<Vec<(String, String, Range<usize>, Range<usize>)>> {
    let mut checkpoint = || Ok(());
    declarations_controlled(
        text,
        namespace,
        limit,
        &mut ResourceParserControl::new(&mut checkpoint, usize::MAX),
    )
}

pub fn declarations_controlled(
    text: &str,
    namespace: &str,
    limit: usize,
    control: &mut ResourceParserControl<'_>,
) -> Result<Vec<(String, String, Range<usize>, Range<usize>)>> {
    control.charge(text.len())?;
    let mut reader = Reader::from_str(text);
    let mut result = Vec::new();
    let mut stack = Vec::new();
    loop {
        control.checkpoint()?;
        let start = reader.buffer_position() as usize;
        let event = reader.read_event()?;
        match event {
            Event::Start(ref element) | Event::Empty(ref element) => {
                let tag = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                let mut name = None;
                let mut kind = match tag.as_str() {
                    "string-array" | "integer-array" => "array".into(),
                    "declare-styleable" => "styleable".into(),
                    _ => tag.clone(),
                };
                for attribute in element.attributes() {
                    let attribute = attribute?;
                    if attribute.key.as_ref() == b"name" {
                        let value = attribute.value.as_ref();
                        let offset = (value.as_ptr() as usize)
                            .checked_sub(text.as_ptr() as usize)
                            .filter(|offset| offset + value.len() <= text.len())
                            .ok_or_else(|| {
                                anyhow::anyhow!("Resource XML attribute is outside its input")
                            })?;
                        name = Some((
                            String::from_utf8(value.to_vec())?,
                            offset..offset + value.len(),
                        ));
                    } else if tag == "item" && attribute.key.as_ref() == b"type" {
                        kind = String::from_utf8(attribute.value.to_vec())?;
                    }
                }
                let top_level = stack.as_slice() == ["resources"];
                let nested_attribute =
                    stack.as_slice() == ["resources", "declare-styleable"] && tag == "attr";
                if (top_level && !matches!(tag.as_str(), "public" | "eat-comment" | "skip"))
                    || nested_attribute
                {
                    if let Some((name, range)) = name
                        && !name.contains(':')
                        && !kind.is_empty()
                    {
                        check_resource_limit(result.len(), limit)?;
                        result.push((kind, name, start..reader.buffer_position() as usize, range));
                    }
                }
                if matches!(event, Event::Start(_)) {
                    stack.push(tag);
                }
            }
            Event::End(_) => {
                ensure!(stack.pop().is_some(), "Unexpected resource closing tag");
            }
            Event::Eof => {
                ensure!(stack.is_empty(), "Unclosed resource XML in {namespace}");
                break;
            }
            _ => {}
        }
    }
    Ok(result)
}

pub fn valid_rename(kind: &str, old_name: &str, new_name: &str) -> Result<()> {
    ensure!(
        !matches!(kind, "attr" | "styleable" | "style"),
        "Renaming styles, attributes and generated styleable members is unsupported"
    );
    ensure!(
        matches!(
            kind,
            "anim"
                | "animator"
                | "array"
                | "bool"
                | "color"
                | "dimen"
                | "drawable"
                | "font"
                | "fraction"
                | "id"
                | "integer"
                | "interpolator"
                | "layout"
                | "menu"
                | "mipmap"
                | "navigation"
                | "plurals"
                | "raw"
                | "string"
                | "transition"
                | "xml"
        ),
        "This resource kind is unsupported for reviewed rename"
    );
    ensure!(
        old_name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'),
        "Renaming normalized or dotted resource names is unsupported"
    );
    ensure!(
        new_name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_lowercase)
            && new_name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'),
        "Resource names must start with a lowercase letter and contain only a–z, 0–9 and underscores"
    );
    ensure!(
        ![
            "abstract",
            "assert",
            "boolean",
            "break",
            "byte",
            "case",
            "catch",
            "char",
            "class",
            "const",
            "continue",
            "default",
            "do",
            "double",
            "else",
            "enum",
            "extends",
            "final",
            "finally",
            "float",
            "for",
            "fun",
            "goto",
            "if",
            "implements",
            "import",
            "in",
            "instanceof",
            "int",
            "interface",
            "is",
            "long",
            "native",
            "new",
            "null",
            "object",
            "package",
            "private",
            "protected",
            "public",
            "return",
            "short",
            "static",
            "strictfp",
            "super",
            "switch",
            "synchronized",
            "this",
            "throw",
            "throws",
            "transient",
            "true",
            "false",
            "try",
            "typealias",
            "val",
            "var",
            "void",
            "volatile",
            "when",
            "while"
        ]
        .contains(&new_name),
        "Resource name is a Java or Kotlin keyword"
    );
    ensure!(old_name != new_name, "The resource name has not changed");
    Ok(())
}

pub fn resource_file_name(filename: &str) -> Option<&str> {
    let name = filename
        .strip_suffix(".9.png")
        .or_else(|| filename.rsplit_once('.').map(|(name, _)| name))?;
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_strings_comments_supports_aliases_qualified_xml_and_ids() -> Result<()> {
        let text = "import lib.R as LibR\nval a = LibR.string.title\nval b = android.R.string.ok\n/* R.string.fake /* nested */ */ val c = \"R.string.fake\"\nval d = R.string.real";
        let references = code_references(text);
        assert_eq!(references.len(), 3);
        assert_eq!(references[0].namespace.as_deref(), Some("lib"));
        assert_eq!(references[1].namespace.as_deref(), Some("android"));
        let references = super::references(
            "<view a=\"@+id/title\" b=\"?android:attr/colorAccent\" c=\"@lib:string/title\"/><!-- @string/fake -->",
            true,
        )?;
        assert_eq!(references.len(), 3);
        assert!(references[0].declaration);
        assert_eq!(references[1].namespace.as_deref(), Some("android"));
        Ok(())
    }

    #[test]
    fn finds_values_attributes_normalized_names_and_precise_ranges() -> Result<()> {
        let text = "<resources><!--ignore--><style name='Theme.App'/><string-array name=\"planets\"/><declare-styleable name='Widget'><attr name='local'/><attr name='android:text'/></declare-styleable><item type='id' name='label'/><public type='string' name='public_only'/></resources>";
        let declarations = declarations(text, "app")?;
        assert_eq!(declarations.len(), 5);
        assert_eq!(declarations[1].0, "array");
        assert_eq!(declarations[2].0, "styleable");
        for (_, name, _, range) in declarations {
            assert_eq!(&text[range], name);
        }
        assert!(super::declarations("<resources><string name='broken'>", "app").is_err());
        Ok(())
    }

    #[test]
    fn resolves_each_qualifier_and_preserves_equal_priority_conflicts() {
        let symbol = Symbol {
            namespace: "app".into(),
            kind: "string".into(),
            name: "title".into(),
        };
        let declarations =
            [("", 3), ("", 0), ("fr", 3), ("fr", 3)].map(|(qualifier, priority)| Declaration {
                symbol: symbol.clone(),
                path: PathBuf::from(format!("{qualifier}{priority}")),
                qualifier: qualifier.into(),
                priority,
                generated: false,
                external: false,
                range: 0..1,
                name_range: Some(0..1),
                xml_name: "title".into(),
            });
        let resolution = resolve(&declarations, &symbol);
        assert_eq!(resolution.winners.len(), 3);
        assert_eq!(resolution.shadowed.len(), 1);
        assert_eq!(resolution.conflicts.len(), 2);
    }

    #[test]
    fn blocks_unsafe_rename_forms() {
        assert!(!rename_code_is_unambiguous(
            "class R {}\nval x = R.string.title"
        ));
        assert!(!rename_code_is_unambiguous(
            "import static app.R.string.title;"
        ));
        assert!(rename_code_is_unambiguous(
            "import app.R\nval x = R.string.title"
        ));
        assert!(valid_rename("string", "title", "new_title").is_ok());
        for name in ["Title", "../../x", "", "title.name", "1title"] {
            assert!(valid_rename("string", "title", name).is_err());
        }
        assert!(valid_rename("style", "Theme_App", "theme").is_err());
        assert!(valid_rename("overlayable", "policy", "renamed_policy").is_err());
        assert!(valid_rename("macro", "macro_name", "renamed_macro").is_err());
        assert_eq!(resource_file_name("image.9.png"), Some("image"));
        assert_eq!(resource_file_name("image.webp"), Some("image"));
    }

    #[test]
    fn public_resource_metadata_is_a_precise_rename_usage() -> Result<()> {
        let text = "<resources><string name='title'>Title</string><public type='string' name='title'/><public name=\"title\" type=\"id\"/></resources>";
        let declarations = declarations(text, "app")?;
        assert_eq!(declarations.len(), 1);
        let references = references(text, true)?;
        assert_eq!(references.len(), 2);
        for (reference, kind) in references.iter().zip(["string", "id"]) {
            assert_eq!(reference.kind, kind);
            assert_eq!(reference.name, "title");
            assert!(!reference.declaration);
            assert_eq!(&text[reference.name_range.clone()], "title");
        }
        let mut renamed = text.to_owned();
        let mut ranges = references
            .into_iter()
            .filter(|reference| reference.kind == "string")
            .map(|reference| reference.name_range)
            .chain(declarations.into_iter().map(|(_, _, _, range)| range))
            .collect::<Vec<_>>();
        ranges.sort_by_key(|range| std::cmp::Reverse(range.start));
        for range in ranges {
            renamed.replace_range(range, "heading");
        }
        assert_eq!(
            renamed,
            "<resources><string name='heading'>Title</string><public type='string' name='heading'/><public name=\"title\" type=\"id\"/></resources>"
        );
        Ok(())
    }

    #[test]
    fn repeated_layout_ids_resolve_without_value_conflicts() -> Result<()> {
        let symbol = Symbol {
            namespace: "app".into(),
            kind: "id".into(),
            name: "title".into(),
        };
        let mut declarations = Vec::new();
        for (path, qualifier, priority) in [
            ("main/layout/first.xml", "", 0),
            ("main/layout/second.xml", "", 0),
            ("main/layout-land/first.xml", "land", 0),
            ("fallback/layout/first.xml", "", 1),
        ] {
            let text = "<view id='@+id/title'/>";
            let reference = references(text, true)?
                .into_iter()
                .next()
                .context("ID declaration reference")?;
            assert!(reference.declaration);
            declarations.push(Declaration {
                symbol: symbol.clone(),
                path: path.into(),
                qualifier: qualifier.into(),
                priority,
                generated: false,
                external: false,
                range: reference.range,
                name_range: Some(reference.name_range),
                xml_name: "title".into(),
            });
        }
        let resolution = resolve(&declarations, &symbol);
        assert_eq!(resolution.winners.len(), 3);
        assert_eq!(resolution.shadowed.len(), 1);
        assert!(resolution.conflicts.is_empty());
        Ok(())
    }

    #[test]
    fn refuses_shadowed_resource_classes_aliases_and_member_imports() {
        for text in [
            "import app.R as AppR\nfun title(AppR: Local) = AppR.string.title",
            "import app.R as AppR\nval AppR = Local()\nval title = AppR.string.title",
            "package app; enum R { VALUE }; class Screen { int title = R.string.title; }",
            "package app; class Screen { int title(Local R) { return R.string.title; } }",
            "import app.R\nfun title(R: Local) = R.string.title",
        ] {
            assert!(!rename_code_is_unambiguous(text), "{text}");
            assert!(code_references(text).is_empty(), "{text}");
        }
        for text in [
            "import static app.R . string . title;\nclass Screen { int value = title; }",
            "import app . R.string.title\nval value = title",
            "import app.R.string.title as heading\nval value = heading",
        ] {
            assert!(!rename_code_is_unambiguous(text), "{text}");
        }
        let text = "import app.R as AppR\nval title = AppR.string.title";
        assert!(rename_code_is_unambiguous(text));
        let references = code_references(text);
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].namespace.as_deref(), Some("app"));
    }

    #[test]
    fn refuses_destructured_lambda_and_enum_resource_bindings() {
        for receiver in ["R", "AppR"] {
            let import = if receiver == "R" {
                "import app.R"
            } else {
                "import app.R as AppR"
            };
            for declaration in [
                format!("val ({receiver}, other) = pair"),
                format!("val (other, {receiver}) = pair"),
                format!(
                    "val values = pairs.map {{ {receiver}, other -> {receiver}.string.title }}"
                ),
                format!(
                    "val values = pairs.map {{ (other, {receiver}) -> {receiver}.string.title }}"
                ),
                format!("for (({receiver}, other) in pairs) {{ use({receiver}.string.title) }}"),
            ] {
                let text = format!("{import}\n{declaration}\nval title = {receiver}.string.title");
                assert!(!rename_code_is_unambiguous(&text), "{text}");
                assert!(code_references(&text).is_empty(), "{text}");
            }
        }
        for text in [
            "enum Holder { R; int title = R.string.title; }",
            "enum Holder { R, OTHER; int title = R.string.title; }",
            "enum Holder { R(1); int title = R.string.title; }",
            "import app.R as AppR\nenum Holder { AppR; int title = AppR.string.title; }",
        ] {
            assert!(!rename_code_is_unambiguous(text), "{text}");
            assert!(code_references(text).is_empty(), "{text}");
        }
    }

    #[test]
    fn refuses_jvm_supertype_bindings_without_server_verification() {
        for text in [
            "package app; class Screen extends Base { int title = R.string.title; }",
            "import app.R;\nclass Screen implements Base { int title = R.string.title; }",
            "import app.R\nclass Screen : Base() { val title = R.string.title }",
            "import app.R as AppR\nclass Screen\n : Base() { val title = AppR.string.title }",
            "import app.R as AppR\nval screen = object : Base() { val title = AppR.string.title }",
            "import app.R\ninterface Screen : Base { val title get() = R.string.title }",
        ] {
            assert!(!rename_code_is_unambiguous(text), "{text}");
        }
        for text in [
            "package app; class Screen { int title = R.string.title; }",
            "import app.R as AppR\nclass Screen { val title = AppR.string.title }",
        ] {
            assert!(rename_code_is_unambiguous(text), "{text}");
        }
    }

    #[test]
    fn refuses_kotlin_interpolated_and_escaped_resource_usages() {
        for text in [
            "val title = \"${R . string . title}\"",
            "import app.R as AppR\nval title = \"${AppR . string . title}\"",
            "val title = \"\"\"${R.string.title}\"\"\"",
        ] {
            assert!(
                code_rename_has_unsupported_templates(text, "string", "title"),
                "{text}"
            );
            assert!(
                !code_rename_has_unsupported_templates(text, "string", "other"),
                "{text}"
            );
        }
        for text in [
            "val title = R.string.`title`",
            "val title = \\u0052.string.title;",
            "import app.R as Resources\nval title = \"${Resources . string . title}\"",
        ] {
            assert!(!rename_code_is_unambiguous(text), "{text}");
        }
        assert!(rename_code_is_unambiguous(
            "val literal = \"R.string.title\"\n// R.string.title\nval title = R . string . title"
        ));
        assert!(!code_rename_has_unsupported_templates(
            "val literal = \"R.string.title\"\nval title = R . string . title",
            "string",
            "title"
        ));
    }

    #[test]
    fn refuses_implicit_receivers_and_anonymous_subclasses() {
        for text in [
            "class Screen { Object value = new Base() { int title = R.string.title; }; }",
            "val title = with(model) { R.string.title }",
            "val title = model.apply { R.string.title }",
            "fun Screen() { ResourceScope { val title = R.string.title } }",
            "fun Model.render() = R.string.title",
            "val Model.title get() = R.string.title",
            "context(Model) fun render() = R.string.title",
            "class Screen<R> { int title = R.string.title; }",
            "fun render() = ResourceScope { R.string.title }",
            "fun render() = ResourceScope(model) { R.string.title }",
            "fun <T> T.render() = R.string.title",
            "fun Model . render() = R.string.title",
            "class Screen { Object value = new @Marker Base() { int title = R.string.title; }; }",
        ] {
            assert!(!rename_code_is_unambiguous(text), "{text}");
        }
        for text in [
            "fun render() { val title = stringResource(R.string.title) }",
            "class Screen { int title = R.string.title; void render() { show(R.string.title); } }",
        ] {
            assert!(rename_code_is_unambiguous(text), "{text}");
        }
    }

    #[test]
    fn refuses_binding_and_entity_encoded_xml_resource_usages() -> Result<()> {
        for text in [
            "<view title='@{@string/title}'/>",
            "<view title='@={@string/title}'/>",
            "<view title='@{app.R . string . title}'/>",
            "<view title='&#64;string/title'/>",
            "<view title='@string/ti&#116;le'/>",
            "<resources><string name='alias'>&#64;string/title</string></resources>",
            "<resources><public type='string' name='ti&#116;le'/></resources>",
        ] {
            assert!(
                !xml_rename_is_unambiguous(text, "string", "title")?,
                "{text}"
            );
        }
        for text in [
            "<view title='@string/title'/>",
            "<view title='&#64;string/other'/>",
            "<!-- @{@string/title} --><view title='@string/title'/>",
        ] {
            assert!(
                xml_rename_is_unambiguous(text, "string", "title")?,
                "{text}"
            );
        }
        Ok(())
    }

    #[test]
    fn completion_respects_resource_aliases_and_literal_r_imports() -> Result<()> {
        for (text, namespace) in [
            (
                "import lib.R as LibR\nval value = LibR.string.ti",
                Some("lib"),
            ),
            ("import lib.R as LibR\nval value = R.string.ti", None),
            (
                "import lib.R as LibR\nimport app.R\nval value = R.string.ti",
                Some("app"),
            ),
            (
                "import lib.R as LibR\nval value = android.R.string.ti",
                Some("android"),
            ),
        ] {
            let reference = completion_reference(text, text.len(), false)
                .with_context(|| format!("Resource completion for {text}"))?;
            assert_eq!(reference.namespace.as_deref(), namespace, "{text}");
            assert_eq!(reference.kind, "string");
            assert_eq!(reference.name, "ti");
            assert_eq!(&text[reference.name_range], "ti");
        }
        for text in [
            "import lib.R as LibR\nfun value(LibR: Local) = LibR.string.ti",
            "val literal = \"R.string.ti",
            "// R.string.ti",
        ] {
            assert!(
                completion_reference(text, text.len(), false).is_none(),
                "{text}"
            );
        }
        Ok(())
    }
}

pub fn completion_reference(text: &str, offset: usize, xml: bool) -> Option<Reference> {
    let mut checkpoint = || Ok(());
    completion_context_controlled(
        text,
        offset,
        xml,
        "",
        &mut ResourceParserControl::new(&mut checkpoint, usize::MAX),
    )
    .ok()
    .flatten()
    .map(|(reference, _)| reference)
}

/// Both completion and the document package are derived from one code mask.
pub fn completion_context_controlled(
    text: &str,
    offset: usize,
    xml: bool,
    fallback: &str,
    control: &mut ResourceParserControl<'_>,
) -> Result<Option<(Reference, String)>> {
    if text.get(..offset).is_none() {
        return Ok(None);
    }
    let mask = if xml {
        let text = &text[..offset];
        control.charge(text.len())?;
        if text
            .rfind("<!--")
            .is_some_and(|start| text.rfind("-->").is_none_or(|end| start > end))
        {
            return Ok(None);
        }
        text.to_owned()
    } else {
        code_mask_controlled(text, control)?
    };
    control.charge(mask.len())?;
    let namespace = if xml {
        fallback.to_owned()
    } else {
        PACKAGE
            .captures(&mask)
            .and_then(|capture| capture.get(1))
            .map_or_else(|| fallback.to_owned(), |value| value.as_str().to_owned())
    };
    control.charge(offset)?;
    Ok(
        completion_reference_from_mask(&mask[..offset], xml)
            .map(|reference| (reference, namespace)),
    )
}

fn completion_reference_from_mask(mask: &str, xml: bool) -> Option<Reference> {
    let expression = if xml {
        r"[@?](?:\+)?(?:(?<namespace>[\w.]+):)?(?<kind>\w+)/(?<name>[\w.]*)$"
    } else {
        r"\b(?<receiver>[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)*)\s*\.\s*(?<kind>\w+)\s*\.\s*(?<name>\w*)$"
    };
    let expression = Regex::new(expression).ok()?;
    let capture = expression.captures(&mask)?;
    let found = capture.get(0)?;
    let name = capture.name("name")?;
    let namespace = if xml {
        capture
            .name("namespace")
            .map(|value| value.as_str().to_owned())
    } else {
        let receiver = capture.name("receiver")?.as_str();
        let stripped = IMPORT.replace_all(&mask, "");
        if binding_shadow(&stripped, receiver.split('.').next()?) {
            return None;
        }
        if let Some(namespace) = receiver.strip_suffix(".R") {
            Some(namespace.to_owned())
        } else {
            let import = IMPORT.captures_iter(&mask).find(|import| {
                import.name("alias").map_or("R", |alias| alias.as_str()) == receiver
            });
            match import {
                Some(import) => Some(import.name("namespace")?.as_str().to_owned()),
                None if receiver == "R" => None,
                None => return None,
            }
        }
    };
    Some(Reference {
        namespace,
        kind: capture.name("kind")?.as_str().to_owned(),
        name: name.as_str().into(),
        range: found.range(),
        name_range: name.range(),
        declaration: false,
    })
}

pub fn reference_context_controlled(
    text: &str,
    offset: usize,
    xml: bool,
    fallback: &str,
    limit: usize,
    control: &mut ResourceParserControl<'_>,
) -> Result<Option<(Reference, String)>> {
    let (mut references, namespace) = if xml {
        (
            references_controlled(text, true, limit, control)?,
            fallback.to_owned(),
        )
    } else {
        let mask = code_mask_controlled(text, control)?;
        control.charge(mask.len())?;
        let namespace = PACKAGE
            .captures(&mask)
            .and_then(|capture| capture.get(1))
            .map_or_else(|| fallback.to_owned(), |value| value.as_str().to_owned());
        (
            code_references_from_mask(&mask, limit, control, &mut BTreeMap::new())?,
            namespace,
        )
    };
    if xml {
        for (kind, name, range, name_range) in
            declarations_controlled(text, &namespace, limit, control)?
        {
            control.checkpoint()?;
            check_resource_limit(references.len(), limit)?;
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
    Ok(references
        .into_iter()
        .find(|reference| {
            (reference.name_range.start <= offset && offset <= reference.name_range.end)
                || reference.range.contains(&offset)
        })
        .map(|reference| (reference, namespace)))
}

#[derive(Clone, Debug)]
pub struct ManifestAttribute {
    pub identity: Vec<(String, Option<String>)>,
    pub name: String,
    pub value: String,
    pub range: Range<usize>,
}

pub fn manifest_attributes(text: &str) -> Result<Vec<ManifestAttribute>> {
    let mut reader = Reader::from_str(text);
    let mut identity = Vec::new();
    let mut attributes = Vec::new();
    loop {
        let event = reader.read_event()?;
        match event {
            Event::Start(ref element) | Event::Empty(ref element) => {
                let name = String::from_utf8(element.name().as_ref().to_vec())?;
                let entries = element
                    .attributes()
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let key = entries
                    .iter()
                    .find(|entry| entry.key.as_ref() == b"android:name")
                    .map(|entry| String::from_utf8_lossy(entry.value.as_ref()).into_owned());
                identity.push((name, key));
                for entry in entries {
                    if entry.key.as_ref().starts_with(b"xmlns") {
                        continue;
                    }
                    let start = (entry.key.as_ref().as_ptr() as usize)
                        .checked_sub(text.as_ptr() as usize)
                        .context("Manifest attribute outside its input")?;
                    let end = (entry.value.as_ref().as_ptr() as usize)
                        .checked_sub(text.as_ptr() as usize)
                        .context("Manifest attribute outside its input")?
                        + entry.value.len();
                    ensure!(end <= text.len(), "Manifest attribute outside its input");
                    attributes.push(ManifestAttribute {
                        identity: identity.clone(),
                        name: String::from_utf8(entry.key.as_ref().to_vec())?,
                        value: String::from_utf8(entry.value.to_vec())?,
                        range: start..end,
                    });
                }
                if matches!(event, Event::Empty(_)) {
                    identity.pop();
                }
            }
            Event::End(_) => {
                ensure!(identity.pop().is_some(), "Unexpected manifest closing tag");
            }
            Event::Eof => {
                ensure!(identity.is_empty(), "Unclosed manifest XML");
                break;
            }
            _ => {}
        }
    }
    Ok(attributes)
}

pub fn public_symbols(text: &str) -> Result<std::collections::BTreeSet<(String, String)>> {
    public_symbols_limited(text, usize::MAX)
}

/// Bound public metadata independently from resource declarations.
pub fn public_symbols_limited(
    text: &str,
    limit: usize,
) -> Result<std::collections::BTreeSet<(String, String)>> {
    let mut reader = Reader::from_str(text);
    let mut symbols = std::collections::BTreeSet::new();
    loop {
        match reader.read_event()? {
            Event::Empty(element) | Event::Start(element)
                if element.name().as_ref() == b"public" =>
            {
                let entries = element
                    .attributes()
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let kind = entries.iter().find(|entry| entry.key.as_ref() == b"type");
                let name = entries.iter().find(|entry| entry.key.as_ref() == b"name");
                if let (Some(kind), Some(name)) = (kind, name) {
                    let symbol = (
                        String::from_utf8(kind.value.to_vec())?,
                        String::from_utf8(name.value.to_vec())?.replace('.', "_"),
                    );
                    if !symbols.contains(&symbol) {
                        check_resource_limit(symbols.len(), limit)?;
                        symbols.insert(symbol);
                    }
                }
            }
            Event::Eof => return Ok(symbols),
            _ => {}
        }
    }
}

pub fn code_rename_has_unsupported_templates(text: &str, kind: &str, name: &str) -> bool {
    let expression = Regex::new(&format!(
        r"\b\w*R\s*\.\s*{}\s*\.\s*{}\b",
        regex::escape(kind),
        regex::escape(name)
    ));
    text.contains("${") && expression.is_ok_and(|expression| expression.is_match(text))
}

#[cfg(test)]
mod resource_parser_worker_tests {
    use super::*;

    #[test]
    fn repeated_qualified_references_check_each_binding_once() -> Result<()> {
        let text = "val value = library.R.string.title\n".repeat(2_000);
        let mut checkpoint = || Ok(());
        let mut control = ResourceParserControl::new(&mut checkpoint, 16 * text.len());
        let references = code_references_controlled(&text, 2_000, &mut control)?;
        assert_eq!(references.len(), 2_000);
        assert_eq!(control.binding_checks, 2); // R and library, independently of usage count.
        assert!(
            references
                .iter()
                .all(|reference| reference.namespace.as_deref() == Some("library"))
        );
        let shadowed = format!("fun use(library: Local) = Unit\n{text}");
        let mut checkpoint = || Ok(());
        let mut control = ResourceParserControl::new(&mut checkpoint, 16 * shadowed.len());
        assert!(code_references_controlled(&shadowed, 2_000, &mut control)?.is_empty());
        assert_eq!(control.binding_checks, 2);
        Ok(())
    }

    #[test]
    fn rename_analysis_reuses_the_same_binding_cache_and_references() -> Result<()> {
        let text = format!(
            "fun read() {{\n{}\n}}",
            "val value = library.R.string.title\n".repeat(1_000)
        );
        let mut checkpoint = || Ok(());
        let mut control = ResourceParserControl::new(&mut checkpoint, 32 * text.len());
        let (safe, references) = code_rename_analysis_controlled(&text, 1_000, &mut control)?;
        assert!(safe);
        assert_eq!(references.len(), 1_000);
        assert_eq!(control.binding_checks, 2);
        let unsafe_text = "fun read(R: Local) = R.string.title";
        let mut checkpoint = || Ok(());
        let mut control = ResourceParserControl::new(&mut checkpoint, 32 * unsafe_text.len());
        assert!(!code_rename_analysis_controlled(unsafe_text, 1_000, &mut control)?.0);
        Ok(())
    }

    #[test]
    fn distinct_aliases_have_an_independent_whole_document_work_budget() {
        let text = (0..100)
            .map(|index| format!("import library{index}.R as Alias{index}\n"))
            .collect::<String>();
        let mut checkpoint = || Ok(());
        let mut control = ResourceParserControl::new(&mut checkpoint, 8 * text.len());
        let error = code_references_controlled(&text, 1_000, &mut control).unwrap_err();
        assert!(error.to_string().contains("parser exceeds work limit"));
        assert!(control.binding_checks < 100);
    }

    #[test]
    fn cancellation_interrupts_a_single_large_comment_before_reference_or_rename_results() {
        let text = format!(
            "/*{}*/\nval value = library.R.string.title",
            "x".repeat(1_000_000)
        );
        for rename in [false, true] {
            let mut calls = 0;
            let mut checkpoint = || {
                calls += 1;
                anyhow::ensure!(calls < 8, "request cancelled");
                Ok(())
            };
            let mut control = ResourceParserControl::new(&mut checkpoint, 32 * text.len());
            let result = if rename {
                code_rename_analysis_controlled(&text, 1_000, &mut control)
                    .map(|(_, references)| references)
            } else {
                code_references_controlled(&text, 1_000, &mut control)
            };
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("request cancelled")
            );
            assert_eq!(calls, 8);
        }
    }

    #[test]
    fn completion_context_masks_once_and_cancels_before_returning_a_cursor_result() -> Result<()> {
        let text = "package example.app\nval value = library.R.string.ti";
        let mut checkpoint = || Ok(());
        let mut control = ResourceParserControl::new(&mut checkpoint, 3 * text.len());
        let (reference, namespace) =
            completion_context_controlled(text, text.len(), false, "fallback", &mut control)?
                .expect("Resource cursor");
        assert_eq!(namespace, "example.app");
        assert_eq!(reference.namespace.as_deref(), Some("library"));
        assert_eq!(reference.name, "ti");
        let text = format!("/*{}*/\n{text}", "x".repeat(1_000_000));
        let mut calls = 0;
        let mut checkpoint = || {
            calls += 1;
            anyhow::ensure!(calls < 8, "request cancelled");
            Ok(())
        };
        let mut control = ResourceParserControl::new(&mut checkpoint, 4 * text.len());
        assert!(
            completion_context_controlled(&text, text.len(), false, "fallback", &mut control)
                .unwrap_err()
                .to_string()
                .contains("request cancelled")
        );
        Ok(())
    }
}

#[cfg(test)]
mod resource_scan_limit_tests {
    use super::*;

    #[test]
    fn bounded_resource_parsers_keep_exact_results_or_fail_without_truncation() -> Result<()> {
        let values = "<resources><string name='first'>First</string><string name='second'>Second</string></resources>";
        let full = declarations(values, "app")?;
        assert_eq!(declarations_limited(values, "app", 2)?, full);
        assert!(
            declarations_limited(values, "app", 1)
                .unwrap_err()
                .is::<ResourceLimitExceeded>()
        );
        let layout = "<view first='@+id/first' second='@+id/second'/>";
        assert_eq!(references_limited(layout, true, 2)?.len(), 2);
        assert!(
            references_limited(layout, true, 1)
                .unwrap_err()
                .is::<ResourceLimitExceeded>()
        );
        let code = "val first = R.string.first; val second = R.string.second";
        assert_eq!(references_limited(code, false, 2)?.len(), 2);
        assert!(
            references_limited(code, false, 1)
                .unwrap_err()
                .is::<ResourceLimitExceeded>()
        );
        Ok(())
    }

    #[test]
    fn bounded_public_metadata_keeps_duplicates_and_combined_xml_reference_limit() -> Result<()> {
        let values = "<resources><public type='string' name='first'/><public type='string' name='first'/><public type='string' name='second'/></resources>";
        assert_eq!(public_symbols_limited(values, 2)?, public_symbols(values)?);
        assert!(
            public_symbols_limited(values, 1)
                .unwrap_err()
                .is::<ResourceLimitExceeded>()
        );
        let mixed = "<resources><public type='string' name='first'/><string name='second'>@string/first</string></resources>";
        assert!(
            references_limited(mixed, true, 1)
                .unwrap_err()
                .is::<ResourceLimitExceeded>()
        );
        assert_eq!(references_limited(mixed, true, 2)?.len(), 2);
        assert!(declarations_limited("<resources/>", "app", 0)?.is_empty());
        assert!(references_limited("<view/>", true, 0)?.is_empty());
        Ok(())
    }
}

#[cfg(test)]
mod resource_owned_payload_tests {
    use super::*;

    #[test]
    fn imported_and_aliased_namespaces_fail_the_owned_budget_without_partial_results() -> Result<()>
    {
        let namespace = "long_namespace".repeat(32);
        for alias in ["R", "LibraryResources"] {
            let import = if alias == "R" {
                format!("import {namespace}.R\n")
            } else {
                format!("import {namespace}.R as {alias}\n")
            };
            let text = format!(
                "{import}{}",
                format!("val value = {alias}.string.title\n").repeat(12)
            );
            let mut checkpoint = || Ok(());
            let mut control = ResourceParserControl::new(&mut checkpoint, 32 * text.len())
                .with_owned_byte_limit(64 * 1024);
            let full = code_references_controlled(&text, 12, &mut control)?;
            assert_eq!(full.len(), 12, "{alias}");
            assert!(
                full.iter()
                    .all(|reference| reference.namespace.as_deref() == Some(namespace.as_str())),
                "{alias}"
            );
            let mut checkpoint = || Ok(());
            let mut control = ResourceParserControl::new(&mut checkpoint, 32 * text.len())
                .with_owned_byte_limit(4 * 1024);
            let error = code_references_controlled(&text, 12, &mut control).unwrap_err();
            assert!(error.is::<ResourceLimitExceeded>(), "{alias}: {error:#}");
            assert!(
                error.to_string().contains("owned byte limit"),
                "{alias}: {error:#}"
            );
        }
        Ok(())
    }
}
