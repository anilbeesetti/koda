// Manifest Class-Path behavior adapted from AOSP TestGroup.java (Copyright 2021
// The Android Open Source Project), Apache-2.0, tools/base revision
// 4a5d2ec9571e2021fc9a4621e24a195f966ee6dd. Complete originals and attribution
// are retained in test_data/android_manifest_classpath.

use anyhow::{Context, Result, ensure};
use clap::Args;
use serde::Serialize;
use std::collections::{BTreeSet, VecDeque};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use url::Url;

const MANIFEST_PATH: &str = "META-INF/MANIFEST.MF";
const END_RECORD_BYTES: usize = 22;
const MAX_END_SEARCH_BYTES: usize = END_RECORD_BYTES + u16::MAX as usize;

#[derive(Args)]
pub struct AndroidManifestClassPathArgs {
    /// Inspect only this wrapper JAR's direct main-manifest Class-Path entries.
    #[arg(long)]
    pub jar: PathBuf,
    /// Preserve these queue entries before appending resolved manifest entries.
    #[arg(long)]
    pub existing_path: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub archive_bytes: u64,
    pub central_directory_bytes: u64,
    pub zip_entries: usize,
    pub manifest_bytes: usize,
    pub manifest_line_bytes: usize,
    pub class_path_tokens: usize,
    pub queue_entries: usize,
    pub uri_token_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            archive_bytes: 16 * 1024 * 1024,
            central_directory_bytes: 2 * 1024 * 1024,
            zip_entries: 4096,
            manifest_bytes: 256 * 1024,
            manifest_line_bytes: 512,
            class_path_tokens: 4096,
            queue_entries: 4096,
            uri_token_bytes: 8 * 1024,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Diagnostic {
    MissingReferencedFile {
        reference: String,
        wrapper_name: String,
    },
    DuplicateMainAttribute {
        name: String,
    },
}

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct Resolution {
    pub appended: usize,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Serialize)]
struct Output {
    direct_manifest_only: bool,
    paths: VecDeque<PathBuf>,
    resolution: Resolution,
}

pub fn run(args: AndroidManifestClassPathArgs) -> Result<()> {
    let mut paths = args.existing_path.into();
    let resolution = add_manifest_class_path(&args.jar, &mut paths, Limits::default())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&Output {
            direct_manifest_only: true,
            paths,
            resolution,
        })?
    );
    Ok(())
}

pub fn add_manifest_class_path(
    jar_path: &Path,
    existing_paths: &mut VecDeque<PathBuf>,
    limits: Limits,
) -> Result<Resolution> {
    ensure!(
        existing_paths.len() <= limits.queue_entries,
        "existing queue exceeds entry limit"
    );
    for path in existing_paths.iter() {
        ensure!(
            path.as_os_str().len() <= limits.uri_token_bytes,
            "existing queue path exceeds byte limit"
        );
    }
    match fs::metadata(jar_path) {
        Ok(metadata) if !metadata.is_file() => return Ok(Resolution::default()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Resolution::default());
        }
        Err(error) => return Err(error).context("checking wrapper JAR"),
        Ok(_) => {}
    }
    let mut file = File::open(jar_path).context("opening wrapper JAR")?;
    let archive_bytes = file
        .metadata()
        .context("checking opened wrapper JAR")?
        .len();
    ensure!(
        archive_bytes <= limits.archive_bytes,
        "wrapper JAR exceeds archive byte limit"
    );
    let entry_count = preflight_zip(&mut file, archive_bytes, limits)?;
    file.seek(SeekFrom::Start(0))?;
    let mut archive = zip::ZipArchive::new(file).context("reading wrapper ZIP")?;
    ensure!(
        archive.len() == entry_count,
        "ZIP entry count differs from preflight"
    );
    let mut manifest = match archive.by_name(MANIFEST_PATH) {
        Ok(manifest) => manifest,
        Err(zip::result::ZipError::FileNotFound) => return Ok(Resolution::default()),
        Err(error) => return Err(error).context("opening wrapper manifest"),
    };
    ensure!(
        manifest.size() <= u64::try_from(limits.manifest_bytes)?,
        "manifest exceeds expanded byte limit"
    );
    let read_limit = limits
        .manifest_bytes
        .checked_add(1)
        .context("manifest read limit overflow")?;
    let mut bytes = Vec::new();
    manifest
        .by_ref()
        .take(u64::try_from(read_limit)?)
        .read_to_end(&mut bytes)
        .context("reading manifest bytes and CRC")?;
    ensure!(
        bytes.len() <= limits.manifest_bytes,
        "manifest exceeds expanded byte limit"
    );
    let (class_path, diagnostics) = manifest_class_path(&bytes, limits)?;
    let mut resolution = Resolution {
        diagnostics,
        ..Resolution::default()
    };
    let Some(class_path) = class_path else {
        return Ok(resolution);
    };
    let absolute = if jar_path.is_absolute() {
        jar_path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("locating wrapper relative to current directory")?
            .join(jar_path)
    };
    ensure!(
        absolute.to_str().is_some(),
        "non-UTF-8 wrapper path is unsupported"
    );
    let base_uri = Url::from_file_path(&absolute)
        .map_err(|()| anyhow::anyhow!("cannot construct absolute wrapper file URI"))?;
    let wrapper_name = jar_path
        .file_name()
        .context("wrapper JAR has no filename")?
        .to_string_lossy()
        .into_owned();
    // Java's default regex \s is ASCII; split_whitespace would split a filename
    // containing a nonbreaking space and change the reference behavior.
    for (ordinal, token) in class_path
        .split([' ', '\t', '\n', '\r', '\u{000b}', '\u{000c}'])
        .filter(|token| !token.is_empty())
        .enumerate()
    {
        ensure!(
            ordinal < limits.class_path_tokens,
            "Class-Path token count exceeds limit"
        );
        ensure!(
            token.len() <= limits.uri_token_bytes,
            "Class-Path URI token exceeds byte limit"
        );
        let path = resolve_file_reference(&base_uri, token)?;
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => {
                ensure!(
                    existing_paths.len() < limits.queue_entries,
                    "resolved queue exceeds entry limit"
                );
                ensure!(
                    path.as_os_str().len() <= limits.uri_token_bytes,
                    "resolved path exceeds byte limit"
                );
                existing_paths.push_back(path);
                resolution.appended += 1;
            }
            Ok(_) => resolution
                .diagnostics
                .push(Diagnostic::MissingReferencedFile {
                    reference: token.to_owned(),
                    wrapper_name: wrapper_name.clone(),
                }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                resolution
                    .diagnostics
                    .push(Diagnostic::MissingReferencedFile {
                        reference: token.to_owned(),
                        wrapper_name: wrapper_name.clone(),
                    });
            }
            Err(error) => return Err(error).context("checking manifest Class-Path target"),
        }
    }
    Ok(resolution)
}

fn preflight_zip(file: &mut File, archive_bytes: u64, limits: Limits) -> Result<usize> {
    let tail_bytes = usize::try_from(archive_bytes.min(u64::try_from(MAX_END_SEARCH_BYTES)?))?;
    file.seek(SeekFrom::End(-i64::try_from(tail_bytes)?))?;
    let mut tail = vec![0; tail_bytes];
    file.read_exact(&mut tail)
        .context("reading ZIP end records")?;
    let end_offset = tail
        .windows(END_RECORD_BYTES)
        .enumerate()
        .rev()
        .find_map(|(offset, record)| {
            if record.get(..4) != Some(b"PK\x05\x06") {
                return None;
            }
            let comment_bytes = usize::from(little_u16(record, 20).ok()?);
            (offset
                .checked_add(END_RECORD_BYTES)?
                .checked_add(comment_bytes)?
                == tail.len())
            .then_some(offset)
        })
        .context("ZIP end-of-central-directory record is missing or truncated")?;
    let record = tail
        .get(end_offset..)
        .context("ZIP end record is out of bounds")?;
    ensure!(
        little_u16(record, 4)? == 0 && little_u16(record, 6)? == 0,
        "multidisk ZIP is unsupported"
    );
    let entries = little_u16(record, 10)?;
    ensure!(entries != u16::MAX, "ZIP64 is unsupported");
    ensure!(
        little_u16(record, 8)? == entries,
        "inconsistent ZIP entry count"
    );
    let entries = usize::from(entries);
    ensure!(
        entries <= limits.zip_entries,
        "ZIP entry count exceeds limit"
    );
    let central_bytes = little_u32(record, 12)?;
    let central_offset = little_u32(record, 16)?;
    ensure!(
        central_bytes != u32::MAX && central_offset != u32::MAX,
        "ZIP64 is unsupported"
    );
    let central_bytes = u64::from(central_bytes);
    let central_offset = u64::from(central_offset);
    ensure!(
        central_bytes <= limits.central_directory_bytes,
        "ZIP central directory exceeds byte limit"
    );
    let absolute_end_offset = archive_bytes
        .checked_sub(u64::try_from(tail_bytes)?)
        .and_then(|offset| offset.checked_add(u64::try_from(end_offset).ok()?))
        .context("ZIP end offset overflow")?;
    ensure!(
        central_offset.checked_add(central_bytes) == Some(absolute_end_offset),
        "ZIP central directory bounds are inconsistent"
    );
    let mut central = vec![0; usize::try_from(central_bytes)?];
    file.seek(SeekFrom::Start(central_offset))?;
    file.read_exact(&mut central)
        .context("reading bounded ZIP central directory")?;
    let mut cursor = 0usize;
    let mut manifest_count = 0usize;
    for _ in 0..entries {
        let header_end = cursor
            .checked_add(46)
            .context("ZIP central header overflow")?;
        let header = central
            .get(cursor..header_end)
            .context("truncated ZIP central header")?;
        ensure!(
            header.get(..4) == Some(b"PK\x01\x02"),
            "invalid ZIP central header signature"
        );
        ensure!(
            little_u16(header, 6)? < 45,
            "ZIP64 or newer ZIP format is unsupported"
        );
        ensure!(
            little_u16(header, 8)? & 1 == 0,
            "encrypted ZIP is unsupported"
        );
        ensure!(
            little_u16(header, 34)? == 0,
            "multidisk ZIP entry is unsupported"
        );
        ensure!(
            little_u32(header, 20)? != u32::MAX
                && little_u32(header, 24)? != u32::MAX
                && little_u32(header, 42)? != u32::MAX,
            "ZIP64 entry is unsupported"
        );
        let filename_bytes = usize::from(little_u16(header, 28)?);
        let extra_bytes = usize::from(little_u16(header, 30)?);
        let comment_bytes = usize::from(little_u16(header, 32)?);
        let name_end = header_end
            .checked_add(filename_bytes)
            .context("ZIP filename offset overflow")?;
        let name = central
            .get(header_end..name_end)
            .context("truncated ZIP filename")?;
        if name == MANIFEST_PATH.as_bytes() {
            manifest_count += 1;
            ensure!(
                manifest_count == 1,
                "duplicate wrapper manifest is unsupported"
            );
            ensure!(
                [0, 8].contains(&little_u16(header, 10)?),
                "manifest ZIP compression is unsupported"
            );
        }
        cursor = name_end
            .checked_add(extra_bytes)
            .and_then(|offset| offset.checked_add(comment_bytes))
            .context("ZIP central entry offset overflow")?;
        ensure!(cursor <= central.len(), "truncated ZIP central entry");
    }
    ensure!(
        cursor == central.len(),
        "ZIP central directory entry count mismatch"
    );
    Ok(entries)
}

fn little_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let end = offset
        .checked_add(2)
        .context("ZIP integer offset overflow")?;
    let bytes: [u8; 2] = bytes
        .get(offset..end)
        .context("truncated ZIP integer")?
        .try_into()?;
    Ok(u16::from_le_bytes(bytes))
}

fn little_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let end = offset
        .checked_add(4)
        .context("ZIP integer offset overflow")?;
    let bytes: [u8; 4] = bytes
        .get(offset..end)
        .context("truncated ZIP integer")?
        .try_into()?;
    Ok(u32::from_le_bytes(bytes))
}

fn manifest_class_path(bytes: &[u8], limits: Limits) -> Result<(Option<String>, Vec<Diagnostic>)> {
    let mut cursor = 0usize;
    let mut pending: Option<Vec<u8>> = None;
    let mut main_section = true;
    let mut section_has_attribute = false;
    let mut main_names = BTreeSet::new();
    let mut class_path = None;
    let mut diagnostics = Vec::new();
    while cursor < bytes.len() {
        let remaining = bytes
            .get(cursor..)
            .context("manifest cursor out of bounds")?;
        let line_bytes = remaining
            .iter()
            .position(|byte| matches!(byte, b'\r' | b'\n'))
            .context("unterminated manifest line")?;
        let mut physical_bytes = line_bytes
            .checked_add(1)
            .context("manifest line overflow")?;
        if remaining.get(line_bytes) == Some(&b'\r')
            && remaining.get(physical_bytes) == Some(&b'\n')
        {
            physical_bytes += 1;
        }
        ensure!(
            physical_bytes <= limits.manifest_line_bytes,
            "manifest physical line exceeds limit"
        );
        let line = remaining
            .get(..line_bytes)
            .context("manifest line out of bounds")?;
        cursor = cursor
            .checked_add(physical_bytes)
            .context("manifest cursor overflow")?;
        if line.first() == Some(&b' ') {
            pending
                .as_mut()
                .context("manifest continuation has no preceding attribute")?
                .extend_from_slice(line.get(1..).context("invalid manifest continuation")?);
            continue;
        }
        if let Some(attribute) = pending.take() {
            accept_attribute(
                &attribute,
                main_section,
                &mut section_has_attribute,
                &mut main_names,
                &mut class_path,
                &mut diagnostics,
            )?;
        }
        if line.is_empty() {
            main_section = false;
            section_has_attribute = false;
        } else {
            pending = Some(line.to_vec());
        }
    }
    if let Some(attribute) = pending {
        accept_attribute(
            &attribute,
            main_section,
            &mut section_has_attribute,
            &mut main_names,
            &mut class_path,
            &mut diagnostics,
        )?;
    }
    Ok((class_path, diagnostics))
}

fn accept_attribute(
    attribute: &[u8],
    main_section: bool,
    section_has_attribute: &mut bool,
    main_names: &mut BTreeSet<String>,
    class_path: &mut Option<String>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<()> {
    let colon = attribute
        .iter()
        .position(|byte| *byte == b':')
        .context("manifest attribute has no colon")?;
    ensure!(
        colon > 0 && colon <= 70,
        "manifest attribute name length is invalid"
    );
    let name = attribute
        .get(..colon)
        .context("manifest name out of bounds")?;
    ensure!(
        name.iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
        "manifest attribute name is invalid"
    );
    ensure!(
        attribute.get(colon + 1) == Some(&b' '),
        "manifest attribute separator must be colon-space"
    );
    let name = std::str::from_utf8(name)?.to_ascii_lowercase();
    let value = attribute
        .get(colon + 2..)
        .context("manifest value out of bounds")?;
    if !main_section && !*section_has_attribute {
        ensure!(
            name == "name",
            "manifest entry section must begin with Name"
        );
    }
    *section_has_attribute = true;
    if main_section {
        if !main_names.insert(name.clone()) {
            diagnostics.push(Diagnostic::DuplicateMainAttribute { name: name.clone() });
        }
        if name == "class-path" {
            // java.util.jar.Manifest decodes attribute values with the UTF-8
            // replacement decoder; continuation folding happens before decoding.
            *class_path = Some(String::from_utf8_lossy(value).into_owned());
        }
    }
    Ok(())
}

fn resolve_file_reference(base_uri: &Url, reference: &str) -> Result<PathBuf> {
    ensure!(
        reference.chars().all(|character| {
            !character.is_ascii_control()
                && !matches!(
                    character,
                    ' ' | '"' | '<' | '>' | '[' | ']' | '\\' | '^' | '`' | '{' | '|' | '}'
                )
        }),
        "invalid URI character in Class-Path reference"
    );
    validate_percent_encoding(reference.as_bytes())?;
    ensure!(
        !reference.contains(['?', '#']),
        "file URI query or fragment is unsupported"
    );
    let absolute_file_path = if let Some((scheme, remainder)) = reference.split_once(':') {
        if !scheme.contains('/') {
            ensure!(
                scheme
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphabetic)
                    && scheme.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.')
                    }),
                "invalid URI scheme in Class-Path reference"
            );
            ensure!(
                scheme.eq_ignore_ascii_case("file"),
                "Class-Path reference is not a file URI"
            );
            ensure!(remainder.starts_with('/'), "opaque file URI is unsupported");
            Some(remainder)
        } else {
            reference.starts_with('/').then_some(reference)
        }
    } else {
        reference.starts_with('/').then_some(reference)
    };
    if let Some(authority) = absolute_file_path.unwrap_or(reference).strip_prefix("//") {
        ensure!(
            authority.starts_with('/'),
            "file URI authority is unsupported"
        );
    }
    // WHATWG URL resolution normalizes encoded dot segments. Java URI.resolve
    // preserves them, so this profile rejects them rather than changing a path.
    for segment in reference.split('/') {
        let decoded = decode_percent_encoding(segment.as_bytes())?;
        ensure!(
            !(segment.contains('%') && (decoded == b"." || decoded == b"..")),
            "encoded dot segment is unsupported"
        );
    }
    if let Some(path) = absolute_file_path {
        // URI.resolve returns an absolute URI or absolute-path reference without
        // normalizing its dot segments. URL parsing/joining would change both
        // the queue text and which file a symlink/../ path addresses.
        return absolute_file_uri_path(path.strip_prefix("//").unwrap_or(path));
    }
    let resolved = base_uri
        .join(reference)
        .context("resolving manifest URI reference")?;
    ensure!(
        resolved.scheme() == "file",
        "Class-Path reference is not a file URI"
    );
    ensure!(
        resolved.host_str().is_none(),
        "file URI authority is unsupported"
    );
    ensure!(
        resolved.query().is_none() && resolved.fragment().is_none(),
        "file URI query or fragment is unsupported"
    );
    let path_bytes = decode_percent_encoding(resolved.path().as_bytes())?;
    ensure!(
        std::str::from_utf8(&path_bytes).is_ok(),
        "non-UTF-8 URI path is unsupported"
    );
    ensure!(!path_bytes.contains(&0), "file URI path contains NUL");
    resolved
        .to_file_path()
        .map_err(|()| anyhow::anyhow!("file URI has no absolute filesystem path"))
}

fn absolute_file_uri_path(raw_path: &str) -> Result<PathBuf> {
    let path_bytes = decode_percent_encoding(raw_path.as_bytes())?;
    let decoded_path =
        std::str::from_utf8(&path_bytes).context("non-UTF-8 URI path is unsupported")?;
    ensure!(!path_bytes.contains(&0), "file URI path contains NUL");
    #[cfg(windows)]
    let path = {
        let mut segments = decoded_path
            .strip_prefix('/')
            .context("file URI has no absolute filesystem path")?
            .split('/');
        let drive = segments.next().context("file URI has no drive letter")?;
        ensure!(
            drive.len() == 2
                && drive
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphabetic)
                && drive.as_bytes().get(1) == Some(&b':'),
            "file URI has no absolute filesystem path"
        );
        let mut path = drive.to_owned();
        for segment in segments {
            path.push('\\');
            path.push_str(segment);
        }
        if path.len() == 2 {
            path.push('\\');
        }
        PathBuf::from(path)
    };
    #[cfg(not(windows))]
    let path = PathBuf::from(decoded_path);
    ensure!(
        path.is_absolute(),
        "file URI has no absolute filesystem path"
    );
    Ok(path)
}

fn validate_percent_encoding(bytes: &[u8]) -> Result<()> {
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes.get(cursor) == Some(&b'%') {
            let high = bytes
                .get(cursor + 1)
                .and_then(|byte| (*byte as char).to_digit(16));
            let low = bytes
                .get(cursor + 2)
                .and_then(|byte| (*byte as char).to_digit(16));
            ensure!(
                high.is_some() && low.is_some(),
                "invalid percent escape in URI reference"
            );
            cursor += 3;
        } else {
            cursor += 1;
        }
    }
    Ok(())
}

fn decode_percent_encoding(bytes: &[u8]) -> Result<Vec<u8>> {
    validate_percent_encoding(bytes)?;
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let byte = *bytes.get(cursor).context("URI byte cursor out of bounds")?;
        if byte == b'%' {
            let high = bytes
                .get(cursor + 1)
                .and_then(|byte| (*byte as char).to_digit(16))
                .context("invalid URI escape")?;
            let low = bytes
                .get(cursor + 2)
                .and_then(|byte| (*byte as char).to_digit(16))
                .context("invalid URI escape")?;
            decoded.push(u8::try_from(high * 16 + low)?);
            cursor += 3;
        } else {
            decoded.push(byte);
            cursor += 1;
        }
    }
    Ok(decoded)
}
