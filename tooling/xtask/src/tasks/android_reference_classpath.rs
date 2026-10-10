// Behavior adapted from AOSP TestGroup.java and TestGroupTest.kt at
// tools/base@4a5d2ec9571e2021fc9a4621e24a195f966ee6dd, Apache-2.0.
// Complete originals and attribution: ../../test_data/reference_classpath/.

use anyhow::{Context, Result, ensure};
use clap::Args;
use serde::Serialize;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct AndroidReferenceClassPathArgs {
    /// Wrapper JAR whose main manifest Class-Path is inspected.
    #[arg(long)]
    jar: PathBuf,
    #[arg(long, default_value_t = 32 * 1024 * 1024)]
    max_archive_bytes: u64,
    #[arg(long, default_value_t = 1024 * 1024)]
    max_manifest_bytes: u64,
    #[arg(long, default_value_t = 4096)]
    max_archive_entries: u64,
    #[arg(long, default_value_t = 4096)]
    max_references: usize,
}

pub fn run(args: AndroidReferenceClassPathArgs) -> Result<()> {
    let limits = ManifestLimits {
        archive_bytes: args.max_archive_bytes,
        manifest_bytes: args.max_manifest_bytes,
        compressed_manifest_bytes: args.max_manifest_bytes,
        archive_entries: args.max_archive_entries,
        references: args.max_references,
        ..ManifestLimits::default()
    };
    let result = read_manifest_classpath(&args.jar, &limits)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

#[derive(Clone, Copy)]
pub struct ManifestLimits {
    pub archive_bytes: u64,
    pub archive_entries: u64,
    pub compressed_manifest_bytes: u64,
    pub manifest_bytes: u64,
    pub physical_line_bytes: usize,
    pub attribute_value_bytes: usize,
    pub references: usize,
}

impl Default for ManifestLimits {
    fn default() -> Self {
        Self {
            archive_bytes: 32 * 1024 * 1024,
            archive_entries: 4096,
            compressed_manifest_bytes: 1024 * 1024,
            manifest_bytes: 1024 * 1024,
            physical_line_bytes: 512,
            attribute_value_bytes: 1024 * 1024,
            references: 4096,
        }
    }
}

impl ManifestLimits {
    fn validate(&self) -> Result<()> {
        ensure!(
            (1..=128 * 1024 * 1024).contains(&self.archive_bytes)
                && (1..=65536).contains(&self.archive_entries)
                && (1..=8 * 1024 * 1024).contains(&self.compressed_manifest_bytes)
                && (1..=8 * 1024 * 1024).contains(&self.manifest_bytes)
                && (1..=8 * 1024 * 1024).contains(&self.physical_line_bytes)
                && (1..=8 * 1024 * 1024).contains(&self.attribute_value_bytes)
                && (1..=65536).contains(&self.references),
            "Manifest limits must be positive and within the supported resource bounds"
        );
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct MissingReference {
    pub reference: String,
    pub resolved_path: PathBuf,
    pub diagnostic: String,
}

#[derive(Debug, Default, Serialize)]
pub struct ManifestClassPath {
    pub paths: Vec<PathBuf>,
    pub missing: Vec<MissingReference>,
    pub configured_suite_membership_verified: bool,
}

pub fn read_manifest_classpath(
    wrapper: &Path,
    limits: &ManifestLimits,
) -> Result<ManifestClassPath> {
    limits.validate()?;
    let mut result = ManifestClassPath::default();
    if !wrapper.is_file() {
        return Ok(result);
    }
    let mut file = File::open(wrapper).with_context(|| format!("Open {}", wrapper.display()))?;
    let file_size = file.metadata()?.len();
    ensure!(
        file_size <= limits.archive_bytes,
        "Wrapper JAR exceeds archive byte limit"
    );
    let entry_count = bounded_directory_count(&mut file, file_size, limits.archive_entries)?;
    let mut archive = zip::ZipArchive::new(file).context("Read wrapper JAR directory")?;
    ensure!(
        archive.len() as u64 <= entry_count,
        "ZIP directory entry count mismatch"
    );
    let mut manifest = match archive.by_name("META-INF/MANIFEST.MF") {
        Ok(manifest) => manifest,
        Err(zip::result::ZipError::FileNotFound) => return Ok(result),
        Err(error) => return Err(error).context("Open wrapper manifest"),
    };
    ensure!(!manifest.is_dir(), "Manifest entry is a directory");
    ensure!(
        manifest.compressed_size() <= limits.compressed_manifest_bytes,
        "Manifest exceeds compressed byte limit"
    );
    ensure!(
        manifest.size() <= limits.manifest_bytes,
        "Manifest exceeds decoded byte limit"
    );
    let mut bytes = Vec::new();
    manifest
        .by_ref()
        .take(limits.manifest_bytes + 1)
        .read_to_end(&mut bytes)
        .context("Read complete wrapper manifest")?;
    ensure!(
        bytes.len() as u64 <= limits.manifest_bytes,
        "Manifest exceeds decoded byte limit"
    );
    let Some(class_path) = manifest_class_path(&bytes, limits)? else {
        return Ok(result);
    };
    let absolute_wrapper = if wrapper.is_absolute() {
        wrapper.to_path_buf()
    } else {
        std::env::current_dir()?.join(wrapper)
    };
    let base_path = file_uri_path(&absolute_wrapper)?;
    for (index, reference) in class_path
        .split(|character| matches!(character, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c'))
        .filter(|reference| !reference.is_empty())
        .enumerate()
    {
        ensure!(
            index < limits.references,
            "Manifest exceeds reference count limit"
        );
        let resolved_path = resolve_file_reference(&base_path, reference)?;
        if resolved_path.is_file() {
            result.paths.push(resolved_path);
        } else {
            result.missing.push(MissingReference {
                reference: reference.to_owned(),
                resolved_path,
                diagnostic: format!(
                    "Cannot find class-path jar: {reference} referenced from {}",
                    wrapper
                        .file_name()
                        .context("Wrapper has no filename")?
                        .to_string_lossy()
                ),
            });
        }
    }
    Ok(result)
}

fn integer<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N]> {
    bytes
        .get(offset..offset.checked_add(N).context("ZIP field offset overflow")?)
        .context("Truncated ZIP directory field")?
        .try_into()
        .context("Invalid ZIP directory field width")
}

fn bounded_directory_count(file: &mut File, size: u64, maximum: u64) -> Result<u64> {
    // Check the declared count before ZipArchive allocates its directory table.
    let tail_size = size.min(65557);
    file.seek(SeekFrom::Start(size - tail_size))?;
    let mut tail = vec![0; usize::try_from(tail_size)?];
    file.read_exact(&mut tail)?;
    let end = tail
        .windows(4)
        .enumerate()
        .rev()
        .find_map(|(offset, signature)| {
            if signature != b"PK\x05\x06" {
                return None;
            }
            let comment = u16::from_le_bytes(integer::<2>(&tail, offset + 20).ok()?);
            (offset + 22 + usize::from(comment) == tail.len()).then_some(offset)
        })
        .context("Missing or truncated ZIP end directory")?;
    let disk = u16::from_le_bytes(integer(&tail, end + 4)?);
    let directory_disk = u16::from_le_bytes(integer(&tail, end + 6)?);
    let disk_entries = u16::from_le_bytes(integer(&tail, end + 8)?);
    let entries = u16::from_le_bytes(integer(&tail, end + 10)?);
    ensure!(
        disk == 0 && directory_disk == 0 && disk_entries == entries,
        "Spanned ZIP is unsupported"
    );
    let end_offset = size - tail_size + end as u64;
    let mut locator = [0; 20];
    let has_zip64 = if let Some(offset) = end_offset.checked_sub(20) {
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut locator)?;
        locator.starts_with(b"PK\x06\x07")
    } else {
        false
    };
    let count = if has_zip64 {
        ensure!(
            u32::from_le_bytes(integer(&locator, 4)?) == 0
                && u32::from_le_bytes(integer(&locator, 16)?) == 1,
            "Spanned ZIP64 is unsupported"
        );
        let offset = u64::from_le_bytes(integer(&locator, 8)?);
        ensure!(
            offset
                .checked_add(56)
                .is_some_and(|end| end <= end_offset - 20),
            "Invalid ZIP64 directory offset"
        );
        file.seek(SeekFrom::Start(offset))?;
        let mut record = [0; 56];
        file.read_exact(&mut record)?;
        ensure!(record.starts_with(b"PK\x06\x06"), "Missing ZIP64 directory");
        let record_size = u64::from_le_bytes(integer(&record, 4)?);
        ensure!(
            record_size >= 44
                && offset
                    .checked_add(12)
                    .and_then(|offset| offset.checked_add(record_size))
                    .is_some_and(|end| end <= end_offset - 20),
            "Invalid ZIP64 directory size"
        );
        ensure!(
            u32::from_le_bytes(integer(&record, 16)?) == 0
                && u32::from_le_bytes(integer(&record, 20)?) == 0,
            "Spanned ZIP64 is unsupported"
        );
        let disk_count = u64::from_le_bytes(integer(&record, 24)?);
        let count = u64::from_le_bytes(integer(&record, 32)?);
        ensure!(disk_count == count, "Spanned ZIP64 is unsupported");
        count
    } else {
        ensure!(entries != u16::MAX, "Missing ZIP64 directory locator");
        u64::from(entries)
    };
    ensure!(
        count <= maximum,
        "Wrapper JAR exceeds directory entry limit"
    );
    file.seek(SeekFrom::Start(0))?;
    Ok(count)
}

fn manifest_class_path(bytes: &[u8], limits: &ManifestLimits) -> Result<Option<String>> {
    let mut position = 0;
    let mut main = true;
    let mut needs_name = false;
    let mut current: Option<(String, Vec<u8>)> = None;
    let mut class_path = None;
    while position < bytes.len() {
        let remaining = bytes.get(position..).context("Invalid manifest offset")?;
        let length = remaining
            .iter()
            .position(|byte| matches!(byte, b'\r' | b'\n'))
            .context("Manifest physical line has no terminator")?;
        let line = remaining.get(..length).context("Invalid manifest line")?;
        let newline =
            if remaining.get(length) == Some(&b'\r') && remaining.get(length + 1) == Some(&b'\n') {
                2
            } else {
                1
            };
        ensure!(
            length + newline <= limits.physical_line_bytes,
            "Manifest exceeds physical line limit"
        );
        position += length + newline;
        if line.first() == Some(&b' ') {
            let (_, value) = current
                .as_mut()
                .context("Manifest continuation without an attribute")?;
            let continuation = line.get(1..).context("Invalid manifest continuation")?;
            ensure!(
                value.len().saturating_add(continuation.len()) <= limits.attribute_value_bytes,
                "Manifest exceeds attribute value limit"
            );
            value.extend_from_slice(continuation);
            continue;
        }
        if let Some((name, value)) = current.take() {
            if main && name.eq_ignore_ascii_case("Class-Path") {
                // Java Manifest decodes UTF-8 after joining continuation bytes.
                class_path = Some(String::from_utf8_lossy(&value).into_owned());
            }
        }
        if line.is_empty() {
            main = false;
            needs_name = true;
            continue;
        }
        let colon = line
            .iter()
            .position(|byte| *byte == b':')
            .context("Manifest attribute has no colon")?;
        let name = line
            .get(..colon)
            .context("Invalid manifest attribute name")?;
        ensure!(
            !name.is_empty()
                && name.len() <= 70
                && name
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                && line.get(colon + 1) == Some(&b' '),
            "Invalid manifest attribute header"
        );
        let name = std::str::from_utf8(name)?.to_owned();
        if needs_name {
            ensure!(
                name.eq_ignore_ascii_case("Name"),
                "Manifest named section must start with Name"
            );
            needs_name = false;
        }
        let value = line
            .get(colon + 2..)
            .context("Invalid manifest attribute value")?;
        ensure!(
            value.len() <= limits.attribute_value_bytes,
            "Manifest exceeds attribute value limit"
        );
        current = Some((name, value.to_vec()));
    }
    if let Some((name, value)) = current {
        if main && name.eq_ignore_ascii_case("Class-Path") {
            class_path = Some(String::from_utf8_lossy(&value).into_owned());
        }
    }
    Ok(class_path)
}

fn file_uri_path(path: &Path) -> Result<String> {
    let path = path
        .to_str()
        .context("Wrapper path cannot be represented by a Java file URI")?;
    #[cfg(windows)]
    let path = format!("/{}", path.replace('\\', "/"));
    let mut encoded = String::new();
    for byte in path.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            encoded.push(char::from(*byte));
        } else {
            use std::fmt::Write;
            write!(&mut encoded, "%{byte:02X}")?;
        }
    }
    Ok(encoded)
}

fn resolve_file_reference(base_path: &str, reference: &str) -> Result<PathBuf> {
    ensure!(
        !reference.chars().any(|character| character.is_control()
            || character.is_whitespace()
            || matches!(
                character,
                ' ' | '<' | '>' | '"' | '{' | '}' | '|' | '^' | '`' | '[' | ']' | '\\'
            )),
        "Invalid Java URI reference: {reference}"
    );
    let mut escapes = reference.as_bytes().iter();
    while let Some(byte) = escapes.next() {
        if *byte == b'%' {
            ensure!(
                escapes.next().is_some_and(u8::is_ascii_hexdigit)
                    && escapes.next().is_some_and(u8::is_ascii_hexdigit),
                "Invalid URI escape: {reference}"
            );
        }
    }
    ensure!(
        !reference.contains(['?', '#']),
        "Java file URI cannot have query or fragment: {reference}"
    );
    let first_segment = reference.split('/').next().context("Empty URI reference")?;
    let (path, absolute_uri) = if let Some((scheme, _)) = first_segment.split_once(':') {
        ensure!(
            scheme.eq_ignore_ascii_case("file"),
            "Reference is not a file URI: {reference}"
        );
        (
            reference
                .get(scheme.len() + 1..)
                .context("Invalid file URI")?,
            true,
        )
    } else {
        (reference, false)
    };
    ensure!(
        !absolute_uri || path.starts_with('/'),
        "Opaque file URI: {reference}"
    );
    if let Some(authority_path) = path.strip_prefix("//") {
        ensure!(
            authority_path.starts_with('/'),
            "Java file URI cannot have authority: {reference}"
        );
    }
    let encoded = if absolute_uri || path.starts_with('/') {
        path.to_owned()
    } else {
        let (directory, _) = base_path
            .rsplit_once('/')
            .context("Wrapper URI has no parent")?;
        // Java URI.resolve normalizes literal dot segments, before percent decoding.
        let joined = format!("{directory}/{path}");
        let mut segments = Vec::new();
        for segment in joined.split('/') {
            match segment {
                "" | "." => {}
                ".." if segments.last().is_some_and(|last| *last != "..") => {
                    segments.pop();
                }
                _ => segments.push(segment),
            }
        }
        format!("/{}", segments.join("/"))
    };
    let mut decoded = Vec::new();
    let bytes = encoded.as_bytes();
    let mut position = 0;
    while let Some(byte) = bytes.get(position) {
        if *byte == b'%' {
            let escape = bytes
                .get(position + 1..position + 3)
                .context("Truncated URI escape")?;
            let escape = std::str::from_utf8(escape).context("Invalid URI escape")?;
            decoded.push(u8::from_str_radix(escape, 16).context("Invalid URI escape")?);
            position += 3;
        } else {
            decoded.push(*byte);
            position += 1;
        }
    }
    let decoded = String::from_utf8_lossy(&decoded);
    #[cfg(not(windows))]
    let path = format!(
        "/{}",
        decoded
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("/")
    );
    #[cfg(windows)]
    let path = decoded
        .strip_prefix('/')
        .context("File URI is not an absolute Windows path")?
        .replace('/', "\\");
    let path = PathBuf::from(path);
    ensure!(
        path.is_absolute(),
        "Resolved file URI is not absolute: {reference}"
    );
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;
    use zip::write::FileOptions;

    fn create_jar(path: &Path, manifest: &[u8], compression: zip::CompressionMethod) -> Result<()> {
        let mut archive = zip::ZipWriter::new(File::create(path)?);
        let options = FileOptions::default().compression_method(compression);
        archive.start_file("META-INF/MANIFEST.MF", options)?;
        archive.write_all(manifest)?;
        archive.start_file("dummy.txt", options)?;
        archive.write_all(b"dummy")?;
        archive.finish()?;
        Ok(())
    }

    fn wrapper(directory: &Path, class_path: &str) -> Result<PathBuf> {
        let path = directory.join("wrapper.jar");
        create_jar(
            &path,
            format!("Manifest-Version: 1.0\r\nClass-Path: {class_path}\r\n\r\n").as_bytes(),
            zip::CompressionMethod::Stored,
        )?;
        Ok(path)
    }

    #[test]
    fn test_valid_absolute_path() -> Result<()> {
        let directory = TempDir::new()?;
        let target = directory.path().join("abs_target.jar");
        File::create(&target)?;
        let class_path = url::Url::from_file_path(&target)
            .map_err(|()| anyhow::anyhow!("Invalid target file URI"))?
            .to_string();
        assert!(class_path.starts_with("file:"));
        let wrapper = wrapper(directory.path(), &class_path)?;
        let result = read_manifest_classpath(&wrapper, &ManifestLimits::default())?;
        assert_eq!(result.paths, vec![target]);
        Ok(())
    }

    #[test]
    fn test_valid_relative_path() -> Result<()> {
        let directory = TempDir::new()?;
        let subdirectory = directory.path().join("sub");
        fs::create_dir(&subdirectory)?;
        let unique = directory
            .path()
            .file_name()
            .context("Temporary directory has no name")?
            .to_string_lossy();
        let relative_name = format!("relative_target_{unique}.jar");
        let target = subdirectory.join(&relative_name);
        let created = File::options()
            .write(true)
            .create_new(true)
            .open(&target)
            .is_ok();
        assert!(created);
        assert!(!Path::new(&relative_name).exists());
        let wrapper = wrapper(&subdirectory, &relative_name)?;
        let result = read_manifest_classpath(&wrapper, &ManifestLimits::default())?;
        assert_eq!(result.paths, vec![target]);
        Ok(())
    }

    #[test]
    fn continuation_escapes_parent_paths_order_and_duplicates() -> Result<()> {
        let directory = TempDir::new()?;
        let subdirectory = directory.path().join("sub");
        fs::create_dir(&subdirectory)?;
        let spaced = subdirectory.join("space é.jar");
        let parent = directory.path().join("parent.jar");
        File::create(&spaced)?;
        File::create(&parent)?;
        let path = subdirectory.join("wrapper.jar");
        create_jar(&path, b"Manifest-Version: 1.0\r\nClass-Path: space%20%C3\r\n %A9.jar ../parent.jar space%20%C3%A9.jar\r\n\r\n", zip::CompressionMethod::Deflated)?;
        let result = read_manifest_classpath(&path, &ManifestLimits::default())?;
        assert_eq!(result.paths, vec![spaced.clone(), parent, spaced]);
        assert!(result.missing.is_empty());
        assert!(!result.configured_suite_membership_verified);
        Ok(())
    }

    #[test]
    fn missing_files_keep_order_and_upstream_diagnostics() -> Result<()> {
        let directory = TempDir::new()?;
        let path = wrapper(directory.path(), "one.jar two.jar one.jar")?;
        let result = read_manifest_classpath(&path, &ManifestLimits::default())?;
        assert!(result.paths.is_empty());
        assert_eq!(
            result
                .missing
                .iter()
                .map(|missing| missing.reference.as_str())
                .collect::<Vec<_>>(),
            vec!["one.jar", "two.jar", "one.jar"]
        );
        assert_eq!(
            result.missing[0].resolved_path,
            directory.path().join("one.jar")
        );
        assert_eq!(
            result.missing[0].diagnostic,
            "Cannot find class-path jar: one.jar referenced from wrapper.jar"
        );
        Ok(())
    }

    #[test]
    fn missing_wrapper_and_missing_manifest_return_empty() -> Result<()> {
        let directory = TempDir::new()?;
        assert!(
            read_manifest_classpath(
                &directory.path().join("absent.jar"),
                &ManifestLimits::default()
            )?
            .paths
            .is_empty()
        );
        assert!(
            read_manifest_classpath(directory.path(), &ManifestLimits::default())?
                .paths
                .is_empty()
        );
        let path = directory.path().join("empty.jar");
        zip::ZipWriter::new(File::create(&path)?).finish()?;
        let result = read_manifest_classpath(&path, &ManifestLimits::default())?;
        assert!(result.paths.is_empty() && result.missing.is_empty());
        Ok(())
    }

    #[test]
    fn main_attributes_ignore_named_class_path_and_last_duplicate_wins() -> Result<()> {
        let directory = TempDir::new()?;
        let target = directory.path().join("target.jar");
        File::create(&target)?;
        let path = directory.path().join("wrapper.jar");
        create_jar(&path, b"Manifest-Version: 1.0\nClass-Path: missing.jar\nclass-path: target.jar\n\nName: example\nClass-Path: named.jar\n\n", zip::CompressionMethod::Stored)?;
        assert_eq!(
            read_manifest_classpath(&path, &ManifestLimits::default())?.paths,
            vec![target]
        );
        Ok(())
    }

    #[test]
    fn malformed_manifest_and_uri_references_fail_explicitly() -> Result<()> {
        let directory = TempDir::new()?;
        let path = directory.path().join("wrapper.jar");
        for manifest in [
            b" orphan\n".as_slice(),
            b"Bad:header\n",
            b"Class-Path: target.jar",
            b"Manifest-Version: 1.0\n\nClass-Path: named.jar\n",
        ] {
            create_jar(&path, manifest, zip::CompressionMethod::Stored)?;
            assert!(read_manifest_classpath(&path, &ManifestLimits::default()).is_err());
        }
        for reference in [
            "https://example.test/a.jar",
            "file:relative.jar",
            "file://localhost/a.jar",
            "//host/a.jar",
            "target.jar?q",
            "target.jar#f",
            "%zz.jar",
            "%",
            "%zz/../target.jar",
            "back\\slash.jar",
            "non\u{00a0}breaking.jar",
        ] {
            let path = wrapper(directory.path(), reference)?;
            assert!(
                read_manifest_classpath(&path, &ManifestLimits::default()).is_err(),
                "{reference}"
            );
        }
        Ok(())
    }

    #[test]
    fn malformed_zip_and_archive_entry_limits_fail_before_manifest_intake() -> Result<()> {
        let directory = TempDir::new()?;
        let path = directory.path().join("wrapper.jar");
        fs::write(&path, b"not a zip")?;
        assert!(read_manifest_classpath(&path, &ManifestLimits::default()).is_err());
        let path = wrapper(directory.path(), "missing.jar")?;
        let limits = ManifestLimits {
            archive_entries: 1,
            ..ManifestLimits::default()
        };
        assert!(read_manifest_classpath(&path, &limits).is_err());
        let limits = ManifestLimits {
            archive_bytes: 1,
            ..ManifestLimits::default()
        };
        assert!(read_manifest_classpath(&path, &limits).is_err());
        Ok(())
    }

    #[test]
    fn decoded_compressed_line_attribute_and_reference_limits_fail() -> Result<()> {
        let directory = TempDir::new()?;
        let path = wrapper(directory.path(), "a.jar b.jar")?;
        for limits in [
            ManifestLimits {
                manifest_bytes: 8,
                ..ManifestLimits::default()
            },
            ManifestLimits {
                compressed_manifest_bytes: 8,
                ..ManifestLimits::default()
            },
            ManifestLimits {
                physical_line_bytes: 8,
                ..ManifestLimits::default()
            },
            ManifestLimits {
                attribute_value_bytes: 8,
                ..ManifestLimits::default()
            },
            ManifestLimits {
                references: 1,
                ..ManifestLimits::default()
            },
            ManifestLimits {
                archive_entries: 0,
                ..ManifestLimits::default()
            },
        ] {
            assert!(read_manifest_classpath(&path, &limits).is_err());
        }
        Ok(())
    }

    #[test]
    fn huge_classic_and_zip64_counts_are_rejected_before_directory_allocation() -> Result<()> {
        let directory = TempDir::new()?;
        let path = wrapper(directory.path(), "a.jar")?;
        let original = fs::read(&path)?;
        let end = original
            .len()
            .checked_sub(22)
            .context("Missing fixture ZIP end")?;
        let mut classic = original.clone();
        for offset in [end + 8, end + 10] {
            classic
                .get_mut(offset..offset + 2)
                .context("Missing fixture count")?
                .copy_from_slice(&5000u16.to_le_bytes());
        }
        fs::write(&path, &classic)?;
        let error = read_manifest_classpath(&path, &ManifestLimits::default())
            .expect_err("Classic count must fail");
        assert!(error.to_string().contains("directory entry limit"));

        let mut zip64 = original
            .get(..end)
            .context("Missing fixture ZIP prefix")?
            .to_vec();
        let mut record = b"PK\x06\x06".to_vec();
        record.extend_from_slice(&44u64.to_le_bytes());
        record.extend_from_slice(&45u16.to_le_bytes());
        record.extend_from_slice(&45u16.to_le_bytes());
        record.extend_from_slice(&0u32.to_le_bytes());
        record.extend_from_slice(&0u32.to_le_bytes());
        record.extend_from_slice(&5000u64.to_le_bytes());
        record.extend_from_slice(&5000u64.to_le_bytes());
        record.extend_from_slice(&0u64.to_le_bytes());
        record.extend_from_slice(&0u64.to_le_bytes());
        zip64.extend_from_slice(&record);
        zip64.extend_from_slice(b"PK\x06\x07");
        zip64.extend_from_slice(&0u32.to_le_bytes());
        zip64.extend_from_slice(&(end as u64).to_le_bytes());
        zip64.extend_from_slice(&1u32.to_le_bytes());
        zip64.extend_from_slice(original.get(end..).context("Missing fixture ZIP end")?);
        fs::write(&path, zip64)?;
        let error = read_manifest_classpath(&path, &ManifestLimits::default())
            .expect_err("ZIP64 count must fail");
        assert!(error.to_string().contains("directory entry limit"));
        Ok(())
    }

    #[test]
    fn oversized_declared_manifest_is_rejected_before_decoding() -> Result<()> {
        let directory = TempDir::new()?;
        let path = wrapper(directory.path(), "a.jar")?;
        let mut bytes = fs::read(&path)?;
        let central = bytes
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .context("Missing fixture directory")?;
        bytes
            .get_mut(central + 24..central + 28)
            .context("Missing manifest size field")?
            .copy_from_slice(&(2u32 * 1024 * 1024).to_le_bytes());
        fs::write(&path, bytes)?;
        let error = read_manifest_classpath(&path, &ManifestLimits::default())
            .expect_err("Manifest size must fail");
        assert!(error.to_string().contains("decoded byte limit"));
        Ok(())
    }

    #[test]
    fn utf8_continuation_bytes_are_joined_before_decoding() -> Result<()> {
        let directory = TempDir::new()?;
        let target = directory.path().join("unié.jar");
        File::create(&target)?;
        let path = directory.path().join("wrapper.jar");
        create_jar(
            &path,
            b"Manifest-Version: 1.0\r\nClass-Path: uni\xc3\r\n \xa9.jar\r\n\r\n",
            zip::CompressionMethod::Stored,
        )?;
        assert_eq!(
            read_manifest_classpath(&path, &ManifestLimits::default())?.paths,
            vec![target]
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn percent_encoded_dot_segments_preserve_java_uri_resolution() -> Result<()> {
        let directory = TempDir::new()?;
        let external = directory.path().join("external");
        fs::create_dir(&external)?;
        let nested = external.join("nested");
        fs::create_dir(&nested)?;
        std::os::unix::fs::symlink(&nested, directory.path().join("link"))?;
        let expected = external.join("target.jar");
        File::create(&expected)?;
        let path = wrapper(directory.path(), "link/%2e%2e/target.jar")?;
        let result = read_manifest_classpath(&path, &ManifestLimits::default())?;
        assert_eq!(
            result.paths,
            vec![directory.path().join("link/../target.jar")]
        );
        assert_eq!(fs::canonicalize(&result.paths[0])?, expected);
        Ok(())
    }
}
