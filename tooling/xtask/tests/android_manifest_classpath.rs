// The two named TestGroupTest behavior ports and fixture construction are adapted
// from Copyright (C) 2026 The Android Open Source Project, Apache-2.0, tools/base
// 4a5d2ec9571e2021fc9a4621e24a195f966ee6dd. Every original assertion is preserved.

#[path = "../src/tasks/android_manifest_classpath.rs"]
pub mod android_manifest_classpath;

use android_manifest_classpath::{Diagnostic, Limits, add_manifest_class_path};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use url::Url;
use zip::write::FileOptions;
use zip::{CompressionMethod, ZipWriter};

fn jar_bytes(manifests: &[&[u8]], compression: CompressionMethod) -> Result<Vec<u8>> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = FileOptions::default().compression_method(compression);
    for manifest in manifests {
        writer.start_file("META-INF/MANIFEST.MF", options)?;
        writer.write_all(manifest)?;
    }
    writer.start_file("dummy.txt", options)?;
    writer.write_all(b"dummy")?;
    Ok(writer.finish()?.into_inner())
}

fn write_manifest_jar(path: &Path, manifest: &[u8], compression: CompressionMethod) -> Result<()> {
    fs::write(path, jar_bytes(&[manifest], compression)?)?;
    Ok(())
}

fn create_jar_with_class_path(path: &Path, class_path: &str) -> Result<()> {
    write_manifest_jar(
        path,
        format!("Manifest-Version: 1.0\r\nClass-Path: {class_path}\r\n\r\n").as_bytes(),
        CompressionMethod::Deflated,
    )
}

fn create_target(path: &Path) -> Result<()> {
    File::options().write(true).create_new(true).open(path)?;
    Ok(())
}

fn capture_error(path: &Path, limits: Limits) -> Result<String> {
    let result = add_manifest_class_path(path, &mut VecDeque::new(), limits);
    assert!(result.is_err(), "the adversarial fixture must be rejected");
    Ok(format!("{:#}", result.err().context("expected rejection")?))
}

fn little_u16(bytes: &[u8], offset: usize) -> Result<usize> {
    let value: [u8; 2] = bytes
        .get(offset..offset + 2)
        .context("fixture integer")?
        .try_into()?;
    Ok(usize::from(u16::from_le_bytes(value)))
}

fn central_offset(bytes: &[u8]) -> Result<usize> {
    bytes
        .windows(4)
        .position(|signature| signature == b"PK\x01\x02")
        .context("fixture central header")
}

fn end_offset(bytes: &[u8]) -> Result<usize> {
    bytes
        .windows(4)
        .rposition(|signature| signature == b"PK\x05\x06")
        .context("fixture end header")
}

fn mutate_u16(bytes: &mut [u8], offset: usize, value: u16) -> Result<()> {
    bytes
        .get_mut(offset..offset + 2)
        .context("fixture integer mutation")?
        .copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn mutate_u32(bytes: &mut [u8], offset: usize, value: u32) -> Result<()> {
    bytes
        .get_mut(offset..offset + 4)
        .context("fixture integer mutation")?
        .copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[test]
fn test_valid_absolute_path() -> Result<()> {
    let temporary = TempDir::new()?;
    let absolute_target = temporary.path().join("abs_target.jar");
    create_target(&absolute_target)?;
    let class_path = Url::from_file_path(&absolute_target)
        .map_err(|()| anyhow::anyhow!("absolute target URI"))?
        .to_string();
    // Original TestGroupTest.kt:53.
    assert!(class_path.starts_with("file:"));
    let wrapper = temporary.path().join("wrapper.jar");
    create_jar_with_class_path(&wrapper, &class_path)?;
    let mut existing_paths = VecDeque::new();

    add_manifest_class_path(&wrapper, &mut existing_paths, Limits::default())?;

    // Original TestGroupTest.kt:59; complete equality preserves containsExactly.
    assert_eq!(existing_paths, VecDeque::from([absolute_target]));
    Ok(())
}

#[test]
fn test_valid_relative_path() -> Result<()> {
    let temporary = TempDir::new()?;
    let subdirectory = temporary.path().join("sub");
    fs::create_dir(&subdirectory)?;
    let wrapper = subdirectory.join("wrapper.jar");
    let unique = temporary
        .path()
        .file_name()
        .context("unique temporary basename")?
        .to_string_lossy();
    let relative_name = format!("relative_target_{unique}.jar");
    let relative_target = subdirectory.join(&relative_name);
    let created = File::options()
        .write(true)
        .create_new(true)
        .open(&relative_target);
    // Original TestGroupTest.kt:68; this assertion remains explicit.
    assert!(created.is_ok());
    drop(created?);
    let in_current_directory = std::env::current_dir()?.join(&relative_name);
    // Original TestGroupTest.kt:71; test the actual process cwd, without changing it.
    assert!(!in_current_directory.exists());
    create_jar_with_class_path(&wrapper, &relative_name)?;
    let mut existing_paths = VecDeque::new();

    add_manifest_class_path(&wrapper, &mut existing_paths, Limits::default())?;

    // Original TestGroupTest.kt:87; complete equality preserves containsExactly.
    assert_eq!(existing_paths, VecDeque::from([relative_target]));
    Ok(())
}

#[test]
fn test_stored_manifest_preserves_queue_order_and_duplicates() -> Result<()> {
    let temporary = TempDir::new()?;
    let first = temporary.path().join("first.jar");
    let second = temporary.path().join("second.jar");
    create_target(&first)?;
    create_target(&second)?;
    let wrapper = temporary.path().join("wrapper.jar");
    write_manifest_jar(
        &wrapper,
        b"Manifest-Version: 1.0\nClass-Path: first.jar second.jar first.jar\n\n",
        CompressionMethod::Stored,
    )?;
    let seed = PathBuf::from("existing-seed.jar");
    let mut paths = VecDeque::from([seed.clone()]);
    let result = add_manifest_class_path(&wrapper, &mut paths, Limits::default())?;
    assert_eq!(paths, VecDeque::from([seed, first.clone(), second, first]));
    assert_eq!(result.appended, 3);
    assert!(result.diagnostics.is_empty());
    Ok(())
}

#[test]
fn test_missing_wrapper_directory_and_missing_manifest_are_noops() -> Result<()> {
    let temporary = TempDir::new()?;
    let seed = PathBuf::from("seed.jar");
    let mut paths = VecDeque::from([seed.clone()]);
    for wrapper in [
        temporary.path().join("missing.jar"),
        temporary.path().to_path_buf(),
    ] {
        let result = add_manifest_class_path(&wrapper, &mut paths, Limits::default())?;
        assert_eq!(result.appended, 0);
        assert!(result.diagnostics.is_empty());
        assert_eq!(paths, VecDeque::from([seed.clone()]));
    }
    let wrapper = temporary.path().join("no_manifest.jar");
    fs::write(&wrapper, jar_bytes(&[], CompressionMethod::Stored)?)?;
    let result = add_manifest_class_path(&wrapper, &mut paths, Limits::default())?;
    assert_eq!(result.appended, 0);
    assert_eq!(paths, VecDeque::from([seed]));
    Ok(())
}

#[test]
fn test_absent_and_empty_class_path_are_noops() -> Result<()> {
    let temporary = TempDir::new()?;
    let wrapper = temporary.path().join("wrapper.jar");
    for manifest in [
        b"Manifest-Version: 1.0\r\n\r\n".as_slice(),
        b"Manifest-Version: 1.0\r\nClass-Path: \r\n\r\n".as_slice(),
        b"Manifest-Version: 1.0\r\nClass-Path: \t \r\n\r\n".as_slice(),
    ] {
        write_manifest_jar(&wrapper, manifest, CompressionMethod::Deflated)?;
        let mut paths = VecDeque::new();
        let result = add_manifest_class_path(&wrapper, &mut paths, Limits::default())?;
        assert!(paths.is_empty());
        assert_eq!(result.appended, 0);
    }
    Ok(())
}

#[test]
fn test_missing_files_and_directories_are_diagnosed_without_stopping() -> Result<()> {
    let temporary = TempDir::new()?;
    let target = temporary.path().join("target.not_a_jar");
    create_target(&target)?;
    fs::create_dir(temporary.path().join("directory.jar"))?;
    let wrapper = temporary.path().join("wrapper.jar");
    create_jar_with_class_path(&wrapper, "missing.jar directory.jar target.not_a_jar")?;
    let mut paths = VecDeque::new();
    let result = add_manifest_class_path(&wrapper, &mut paths, Limits::default())?;
    assert_eq!(paths, VecDeque::from([target]));
    assert_eq!(
        result.diagnostics,
        vec![
            Diagnostic::MissingReferencedFile {
                reference: "missing.jar".to_owned(),
                wrapper_name: "wrapper.jar".to_owned()
            },
            Diagnostic::MissingReferencedFile {
                reference: "directory.jar".to_owned(),
                wrapper_name: "wrapper.jar".to_owned()
            },
        ]
    );
    Ok(())
}

#[test]
fn test_manifest_continuations_case_and_named_section_isolation() -> Result<()> {
    let temporary = TempDir::new()?;
    let target = temporary.path().join("relative_target.jar");
    create_target(&target)?;
    let unrelated = temporary.path().join("other.jar");
    create_target(&unrelated)?;
    let wrapper = temporary.path().join("wrapper.jar");
    write_manifest_jar(&wrapper, b"Manifest-Version: 1.0\r\ncLaSs-PaTh: relative_\r\n target.jar\r\n\r\nName: dummy.txt\r\nClass-Path: other.jar\r\n\r\n", CompressionMethod::Deflated)?;
    let mut paths = VecDeque::new();
    let result = add_manifest_class_path(&wrapper, &mut paths, Limits::default())?;
    assert_eq!(paths, VecDeque::from([target]));
    assert!(result.diagnostics.is_empty());
    Ok(())
}

#[test]
fn test_ascii_separators_do_not_split_nonbreaking_space_filename() -> Result<()> {
    let temporary = TempDir::new()?;
    let target = temporary.path().join("one\u{00a0}file.jar");
    create_target(&target)?;
    let second = temporary.path().join("second.jar");
    create_target(&second)?;
    let wrapper = temporary.path().join("wrapper.jar");
    create_jar_with_class_path(
        &wrapper,
        " \tone\u{00a0}file.jar\u{000b}\u{000c}second.jar ",
    )?;
    let mut paths = VecDeque::new();
    add_manifest_class_path(&wrapper, &mut paths, Limits::default())?;
    assert_eq!(paths, VecDeque::from([target, second]));
    Ok(())
}

#[test]
fn test_file_uri_percent_encoding_and_parent_resolution() -> Result<()> {
    let temporary = TempDir::new()?;
    let subdirectory = temporary.path().join("sub");
    fs::create_dir(&subdirectory)?;
    let target = temporary.path().join("space and λ.jar");
    create_target(&target)?;
    let nested = subdirectory.join("nested");
    fs::create_dir(&nested)?;
    let nested_target = nested.join("child.jar");
    create_target(&nested_target)?;
    let absolute_uri = Url::from_file_path(&target).map_err(|()| anyhow::anyhow!("fixture URI"))?;
    let wrapper = subdirectory.join("wrapper.jar");
    create_jar_with_class_path(
        &wrapper,
        &format!("../space%20and%20%CE%BB.jar ./nested/child.jar {absolute_uri}"),
    )?;
    let mut paths = VecDeque::new();
    add_manifest_class_path(&wrapper, &mut paths, Limits::default())?;
    assert_eq!(
        paths,
        VecDeque::from([target.clone(), nested_target, target])
    );
    Ok(())
}

#[test]
fn test_duplicate_main_class_path_uses_last_value_with_diagnostic() -> Result<()> {
    let temporary = TempDir::new()?;
    let target = temporary.path().join("last.jar");
    create_target(&target)?;
    let wrapper = temporary.path().join("wrapper.jar");
    write_manifest_jar(
        &wrapper,
        b"Class-Path: missing-first.jar\nCLASS-PATH: last.jar\n\n",
        CompressionMethod::Stored,
    )?;
    let mut paths = VecDeque::new();
    let result = add_manifest_class_path(&wrapper, &mut paths, Limits::default())?;
    assert_eq!(paths, VecDeque::from([target]));
    assert_eq!(
        result.diagnostics,
        vec![Diagnostic::DuplicateMainAttribute {
            name: "class-path".to_owned()
        }]
    );
    Ok(())
}

#[test]
fn test_invalid_later_uri_keeps_previous_queue_appends() -> Result<()> {
    let temporary = TempDir::new()?;
    let target = temporary.path().join("target.jar");
    create_target(&target)?;
    let wrapper = temporary.path().join("wrapper.jar");
    let seed = PathBuf::from("seed.jar");
    for invalid in [
        "https://example.invalid/test.jar",
        "file:relative.jar",
        ":bad.jar",
        "file://localhost/tmp/test.jar",
        "//host/test.jar",
        "target.jar?query",
        "target.jar#fragment",
        "%GG.jar",
        "%2.jar",
        "back\\slash.jar",
        "bad[bracket].jar",
        "%FF.jar",
        "%00.jar",
        "%2e%2e/target.jar",
    ] {
        create_jar_with_class_path(&wrapper, &format!("target.jar {invalid}"))?;
        let mut paths = VecDeque::from([seed.clone()]);
        let result = add_manifest_class_path(&wrapper, &mut paths, Limits::default());
        assert!(
            result.is_err(),
            "invalid or explicitly unsupported URI: {invalid}"
        );
        assert_eq!(paths, VecDeque::from([seed.clone(), target.clone()]));
    }
    Ok(())
}

#[test]
fn test_malformed_manifest_is_rejected_before_appends() -> Result<()> {
    let temporary = TempDir::new()?;
    let wrapper = temporary.path().join("wrapper.jar");
    for manifest in [
        b" continuation\n\n".as_slice(),
        b"Class-Path:missing.jar\n\n".as_slice(),
        b"Bad Name: value\n\n".as_slice(),
        b"Class-Path missing.jar\n\n".as_slice(),
        b"Class-Path: missing.jar".as_slice(),
        b"Class-Path: first.jar\n\nClass-Path: named.jar\n\n".as_slice(),
    ] {
        write_manifest_jar(&wrapper, manifest, CompressionMethod::Stored)?;
        let seed = PathBuf::from("seed.jar");
        let mut paths = VecDeque::from([seed.clone()]);
        assert!(add_manifest_class_path(&wrapper, &mut paths, Limits::default()).is_err());
        assert_eq!(paths, VecDeque::from([seed]));
    }
    Ok(())
}

#[test]
fn test_utf8_continuation_is_decoded_after_folding() -> Result<()> {
    let temporary = TempDir::new()?;
    let target = temporary.path().join("λ.jar");
    create_target(&target)?;
    let wrapper = temporary.path().join("wrapper.jar");
    write_manifest_jar(
        &wrapper,
        b"Class-Path: \xce\r\n \xbb.jar\r\n\r\n",
        CompressionMethod::Deflated,
    )?;
    let mut paths = VecDeque::new();
    add_manifest_class_path(&wrapper, &mut paths, Limits::default())?;
    assert_eq!(paths, VecDeque::from([target]));
    Ok(())
}

#[test]
fn test_invalid_utf8_manifest_value_uses_replacement_decoder() -> Result<()> {
    let temporary = TempDir::new()?;
    let target = temporary.path().join("�.jar");
    create_target(&target)?;
    let wrapper = temporary.path().join("wrapper.jar");
    write_manifest_jar(
        &wrapper,
        b"Class-Path: \xff.jar\n\n",
        CompressionMethod::Stored,
    )?;
    let mut paths = VecDeque::new();
    add_manifest_class_path(&wrapper, &mut paths, Limits::default())?;
    assert_eq!(paths, VecDeque::from([target]));
    Ok(())
}

#[test]
fn test_archive_central_entry_and_manifest_limits_are_enforced() -> Result<()> {
    let temporary = TempDir::new()?;
    let wrapper = temporary.path().join("wrapper.jar");
    create_jar_with_class_path(&wrapper, "missing.jar")?;
    for (limits, expected) in [
        (
            Limits {
                archive_bytes: 1,
                ..Limits::default()
            },
            "archive byte limit",
        ),
        (
            Limits {
                central_directory_bytes: 1,
                ..Limits::default()
            },
            "central directory exceeds",
        ),
        (
            Limits {
                zip_entries: 1,
                ..Limits::default()
            },
            "entry count exceeds",
        ),
        (
            Limits {
                manifest_bytes: 8,
                ..Limits::default()
            },
            "expanded byte limit",
        ),
        (
            Limits {
                manifest_line_bytes: 8,
                ..Limits::default()
            },
            "physical line exceeds",
        ),
        (
            Limits {
                uri_token_bytes: 3,
                ..Limits::default()
            },
            "URI token exceeds",
        ),
    ] {
        assert!(capture_error(&wrapper, limits)?.contains(expected));
    }
    Ok(())
}

#[test]
fn test_token_and_queue_limits_preserve_prior_appends() -> Result<()> {
    let temporary = TempDir::new()?;
    let first = temporary.path().join("first.jar");
    let second = temporary.path().join("second.jar");
    create_target(&first)?;
    create_target(&second)?;
    let wrapper = temporary.path().join("wrapper.jar");
    create_jar_with_class_path(&wrapper, "first.jar second.jar")?;
    for limits in [
        Limits {
            class_path_tokens: 1,
            ..Limits::default()
        },
        Limits {
            queue_entries: 1,
            ..Limits::default()
        },
    ] {
        let mut paths = VecDeque::new();
        assert!(add_manifest_class_path(&wrapper, &mut paths, limits).is_err());
        assert_eq!(paths, VecDeque::from([first.clone()]));
    }
    let mut paths = VecDeque::from([first.clone(), second.clone()]);
    assert!(
        add_manifest_class_path(
            &wrapper,
            &mut paths,
            Limits {
                queue_entries: 1,
                ..Limits::default()
            }
        )
        .is_err()
    );
    assert_eq!(paths, VecDeque::from([first, second]));
    Ok(())
}

#[test]
fn test_compressed_manifest_expansion_limit_is_enforced() -> Result<()> {
    let temporary = TempDir::new()?;
    let wrapper = temporary.path().join("wrapper.jar");
    let manifest = format!("Class-Path: {}\r\n\r\n", "a".repeat(4000));
    write_manifest_jar(&wrapper, manifest.as_bytes(), CompressionMethod::Deflated)?;
    let error = capture_error(
        &wrapper,
        Limits {
            manifest_bytes: 512,
            ..Limits::default()
        },
    )?;
    assert!(error.contains("expanded byte limit"));
    Ok(())
}

#[test]
fn test_nonzip_truncated_and_inconsistent_zip_records_are_rejected() -> Result<()> {
    let temporary = TempDir::new()?;
    let wrapper = temporary.path().join("wrapper.jar");
    fs::write(&wrapper, b"not a zip archive")?;
    assert!(capture_error(&wrapper, Limits::default())?.contains("end-of-central-directory"));
    let original = jar_bytes(
        &[b"Class-Path: missing.jar\r\n\r\n"],
        CompressionMethod::Stored,
    )?;
    fs::write(
        &wrapper,
        original
            .get(..original.len() - 5)
            .context("truncated fixture")?,
    )?;
    assert!(capture_error(&wrapper, Limits::default())?.contains("end-of-central-directory"));
    let end = end_offset(&original)?;
    let mut invalid = original.clone();
    mutate_u16(&mut invalid, end + 8, 3)?;
    fs::write(&wrapper, invalid)?;
    assert!(capture_error(&wrapper, Limits::default())?.contains("inconsistent ZIP entry count"));
    let mut invalid = original.clone();
    mutate_u16(&mut invalid, end + 8, 3)?;
    mutate_u16(&mut invalid, end + 10, 3)?;
    fs::write(&wrapper, invalid)?;
    assert!(capture_error(&wrapper, Limits::default())?.contains("truncated ZIP central header"));
    let mut invalid = original;
    mutate_u32(&mut invalid, end + 16, 0)?;
    fs::write(&wrapper, invalid)?;
    assert!(capture_error(&wrapper, Limits::default())?.contains("central directory bounds"));
    Ok(())
}

#[test]
fn test_zip64_multidisk_and_encrypted_entries_are_explicit_errors() -> Result<()> {
    let temporary = TempDir::new()?;
    let wrapper = temporary.path().join("wrapper.jar");
    let original = jar_bytes(&[b"Class-Path: missing.jar\n\n"], CompressionMethod::Stored)?;
    let end = end_offset(&original)?;
    let central = central_offset(&original)?;
    for (offset, value, expected) in [
        (end + 10, u16::MAX, "ZIP64"),
        (end + 4, 1, "multidisk"),
        (central + 8, 1, "encrypted"),
        (central + 6, 45, "ZIP64"),
        (central + 34, 1, "multidisk"),
    ] {
        let mut invalid = original.clone();
        mutate_u16(&mut invalid, offset, value)?;
        fs::write(&wrapper, invalid)?;
        assert!(capture_error(&wrapper, Limits::default())?.contains(expected));
    }
    Ok(())
}

#[test]
fn test_duplicate_manifest_member_is_rejected() -> Result<()> {
    let temporary = TempDir::new()?;
    let wrapper = temporary.path().join("wrapper.jar");
    fs::write(
        &wrapper,
        jar_bytes(
            &[b"Class-Path: first.jar\n\n", b"Class-Path: second.jar\n\n"],
            CompressionMethod::Stored,
        )?,
    )?;
    assert!(capture_error(&wrapper, Limits::default())?.contains("duplicate wrapper manifest"));
    Ok(())
}

#[test]
fn test_manifest_crc_corruption_is_rejected() -> Result<()> {
    let temporary = TempDir::new()?;
    let wrapper = temporary.path().join("wrapper.jar");
    let mut corrupted = jar_bytes(&[b"Class-Path: missing.jar\n\n"], CompressionMethod::Stored)?;
    let content = 30 + little_u16(&corrupted, 26)? + little_u16(&corrupted, 28)?;
    let byte = corrupted.get_mut(content).context("manifest payload")?;
    *byte ^= 1;
    fs::write(&wrapper, corrupted)?;
    assert!(capture_error(&wrapper, Limits::default())?.contains("CRC"));
    Ok(())
}

#[test]
fn test_cli_reports_direct_entries_without_runtime_case_claims() -> Result<()> {
    let temporary = TempDir::new()?;
    let target = temporary.path().join("target.jar");
    create_target(&target)?;
    let wrapper = temporary.path().join("wrapper.jar");
    create_jar_with_class_path(&wrapper, "target.jar missing.jar")?;
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("android-manifest-class-path")
        .arg("--jar")
        .arg(&wrapper)
        .arg("--existing-path")
        .arg("seed.jar")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        report.get("direct_manifest_only"),
        Some(&serde_json::Value::Bool(true))
    );
    assert_eq!(
        report.get("paths"),
        Some(&serde_json::json!(["seed.jar", target]))
    );
    assert_eq!(
        report
            .get("resolution")
            .and_then(|value| value.get("appended")),
        Some(&serde_json::json!(1))
    );
    assert_eq!(
        report
            .get("resolution")
            .and_then(|value| value.get("diagnostics")),
        Some(
            &serde_json::json!([{"kind":"missing_referenced_file", "reference":"missing.jar", "wrapper_name":"wrapper.jar"}])
        )
    );
    assert!(report.get("effective_runtime_cases").is_none());
    assert!(report.get("tests").is_none());
    Ok(())
}

#[test]
fn test_cli_invalid_zip_fails_with_context() -> Result<()> {
    let temporary = TempDir::new()?;
    let wrapper = temporary.path().join("wrapper.jar");
    fs::write(&wrapper, b"invalid zip")?;
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("android-manifest-class-path")
        .arg("--jar")
        .arg(&wrapper)
        .output()?;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("end-of-central-directory"));
    Ok(())
}

#[test]
fn test_complete_original_source_pins_and_assertion_provenance() -> Result<()> {
    for (bytes, expected) in [
        (
            include_bytes!("../test_data/android_manifest_classpath/original/TestGroup.java")
                .as_slice(),
            "6d384e55511e1208a6a1c2e950f02acfef4f12f6b059d4e0f76c149db7cc415a",
        ),
        (
            include_bytes!("../test_data/android_manifest_classpath/original/TestGroupTest.kt")
                .as_slice(),
            "0f6a26754d7afa899e37e82936450f33fbad2339aa907924468c8c31dddf4743",
        ),
        (
            include_bytes!(
                "../test_data/android_manifest_classpath/original/JarTestSuiteRunner.java"
            )
            .as_slice(),
            "da503cfd8096ae4be970e29f5a96d882bbc6eeb31f0c1ec8a47bed9a7d2abc3a",
        ),
        (
            include_bytes!("../test_data/android_manifest_classpath/original/BUILD").as_slice(),
            "c1c78be6ae712f5c346525a5cb4318d2fd55d8da99b4fad7c7271f17fc5617f1",
        ),
    ] {
        assert_eq!(format!("{:x}", Sha256::digest(bytes)), expected);
    }
    let provenance: serde_json::Value = serde_json::from_str(include_str!(
        "../test_data/android_manifest_classpath/provenance.json"
    ))?;
    let rows = provenance
        .get("behavior_ports")
        .and_then(|value| value.as_array())
        .context("named original rows")?;
    assert_eq!(rows.len(), 2);
    let methods: Vec<_> = rows
        .iter()
        .map(|row| row.get("method").and_then(|value| value.as_str()))
        .collect();
    assert_eq!(
        methods,
        vec![Some("testValidAbsolutePath"), Some("testValidRelativePath")]
    );
    let assertions: Vec<_> = rows
        .iter()
        .map(|row| row.get("original_assertion_lines"))
        .collect();
    assert_eq!(
        assertions,
        vec![
            Some(&serde_json::json!([53, 59])),
            Some(&serde_json::json!([68, 71, 87]))
        ]
    );
    Ok(())
}

#[test]
fn test_central_signature_and_filename_bounds_are_checked() -> Result<()> {
    let temporary = TempDir::new()?;
    let wrapper = temporary.path().join("wrapper.jar");
    let original = jar_bytes(&[b"Class-Path: missing.jar\n\n"], CompressionMethod::Stored)?;
    let central = central_offset(&original)?;
    let mut invalid = original.clone();
    mutate_u32(&mut invalid, central, 0)?;
    fs::write(&wrapper, invalid)?;
    assert!(capture_error(&wrapper, Limits::default())?.contains("central header signature"));
    let mut invalid = original;
    mutate_u16(&mut invalid, central + 28, u16::MAX)?;
    fs::write(&wrapper, invalid)?;
    assert!(capture_error(&wrapper, Limits::default())?.contains("truncated ZIP filename"));
    Ok(())
}

#[test]
fn test_underreported_manifest_size_cannot_hide_expanded_bytes() -> Result<()> {
    let temporary = TempDir::new()?;
    let wrapper = temporary.path().join("wrapper.jar");
    let manifest = format!("Class-Path: {}\n\n", "a".repeat(4096));
    let mut invalid = jar_bytes(&[manifest.as_bytes()], CompressionMethod::Deflated)?;
    let central = central_offset(&invalid)?;
    mutate_u32(&mut invalid, central + 24, 8)?;
    mutate_u32(&mut invalid, 22, 8)?;
    fs::write(&wrapper, invalid)?;
    let error = capture_error(
        &wrapper,
        Limits {
            manifest_bytes: 512,
            ..Limits::default()
        },
    )?;
    assert!(error.contains("expanded byte limit") || error.contains("CRC"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn test_existing_symlink_target_keeps_lexical_path() -> Result<()> {
    let temporary = TempDir::new()?;
    let physical = temporary.path().join("physical.jar");
    let lexical = temporary.path().join("linked.jar");
    create_target(&physical)?;
    std::os::unix::fs::symlink(&physical, &lexical)?;
    let wrapper = temporary.path().join("wrapper.jar");
    create_jar_with_class_path(&wrapper, "linked.jar")?;
    let mut paths = VecDeque::new();
    add_manifest_class_path(&wrapper, &mut paths, Limits::default())?;
    assert_eq!(paths, VecDeque::from([lexical]));
    Ok(())
}

#[test]
fn test_absolute_file_uri_preserves_dot_segments_and_percent_decoding() -> Result<()> {
    let temporary = TempDir::new()?;
    fs::create_dir(temporary.path().join("sub"))?;
    let target = temporary.path().join("space and λ.jar");
    create_target(&target)?;
    let lexical = temporary
        .path()
        .join("sub")
        .join("..")
        .join(".")
        .join("space and λ.jar");
    let directory_uri = Url::from_directory_path(temporary.path())
        .map_err(|()| anyhow::anyhow!("absolute fixture directory URI"))?;
    let remainder = directory_uri
        .as_str()
        .strip_prefix("file:")
        .context("fixture file URI scheme")?;
    let without_authority = remainder
        .strip_prefix("//")
        .context("fixture empty file URI authority")?;
    let wrapper = temporary.path().join("wrapper.jar");
    for reference in [
        format!("{directory_uri}sub/.././space%20and%20%CE%BB.jar"),
        format!("FILE:{remainder}sub/.././space%20and%20%CE%BB.jar"),
        format!("file:{without_authority}sub/.././space%20and%20%CE%BB.jar"),
        format!("{without_authority}sub/.././space%20and%20%CE%BB.jar"),
    ] {
        create_jar_with_class_path(&wrapper, &reference)?;
        let mut paths = VecDeque::new();
        let result = add_manifest_class_path(&wrapper, &mut paths, Limits::default())?;
        assert_eq!(result.appended, 1);
        assert!(result.diagnostics.is_empty());
        assert_eq!(paths, VecDeque::from([lexical.clone()]));
        // Path equality ignores ordinary '.' components, so compare OS text too.
        assert_eq!(
            paths.front().context("queued absolute target")?.as_os_str(),
            lexical.as_os_str()
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn test_absolute_symlink_parent_existing_target_keeps_lexical_queue_path() -> Result<()> {
    let temporary = TempDir::new()?;
    let physical_parent = temporary.path().join("physical");
    let physical_directory = physical_parent.join("nested");
    fs::create_dir_all(&physical_directory)?;
    let link = temporary.path().join("linked");
    std::os::unix::fs::symlink(&physical_directory, &link)?;
    let physical_target = physical_parent.join("target.jar");
    create_target(&physical_target)?;
    let lexical = link.join("..").join("target.jar");
    let normalized = temporary.path().join("target.jar");
    assert!(fs::metadata(&lexical)?.is_file());
    assert!(!normalized.exists());
    let directory_uri = Url::from_directory_path(temporary.path())
        .map_err(|()| anyhow::anyhow!("absolute fixture directory URI"))?;
    let reference = format!("{directory_uri}linked/../target.jar");
    let wrapper = temporary.path().join("wrapper.jar");
    create_jar_with_class_path(&wrapper, &reference)?;
    let mut paths = VecDeque::new();
    let result = add_manifest_class_path(&wrapper, &mut paths, Limits::default())?;
    assert_eq!(result.appended, 1);
    assert!(result.diagnostics.is_empty());
    assert_eq!(paths, VecDeque::from([lexical.clone()]));
    let queued = paths.front().context("queued symlink-parent target")?;
    assert_eq!(queued.as_os_str(), lexical.as_os_str());
    assert_eq!(
        fs::canonicalize(queued)?,
        fs::canonicalize(physical_target)?
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn test_absolute_symlink_parent_missing_target_ignores_normalized_file() -> Result<()> {
    let temporary = TempDir::new()?;
    let physical_directory = temporary.path().join("physical").join("nested");
    fs::create_dir_all(&physical_directory)?;
    let link = temporary.path().join("linked");
    std::os::unix::fs::symlink(&physical_directory, &link)?;
    let lexical = link.join("..").join("target.jar");
    let normalized = temporary.path().join("target.jar");
    create_target(&normalized)?;
    assert!(!lexical.exists());
    assert!(fs::metadata(&normalized)?.is_file());
    let directory_uri = Url::from_directory_path(temporary.path())
        .map_err(|()| anyhow::anyhow!("absolute fixture directory URI"))?;
    let reference = format!("{directory_uri}linked/../target.jar");
    let wrapper = temporary.path().join("wrapper.jar");
    create_jar_with_class_path(&wrapper, &reference)?;
    let seed = PathBuf::from("seed.jar");
    let mut paths = VecDeque::from([seed.clone()]);
    let result = add_manifest_class_path(&wrapper, &mut paths, Limits::default())?;
    assert_eq!(result.appended, 0);
    assert_eq!(paths, VecDeque::from([seed]));
    assert_eq!(
        result.diagnostics,
        vec![Diagnostic::MissingReferencedFile {
            reference,
            wrapper_name: "wrapper.jar".to_owned(),
        }]
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn test_relative_symlink_parent_reference_still_normalizes_before_file_lookup() -> Result<()> {
    let temporary = TempDir::new()?;
    let physical_directory = temporary.path().join("physical").join("nested");
    fs::create_dir_all(&physical_directory)?;
    let link = temporary.path().join("linked");
    std::os::unix::fs::symlink(&physical_directory, &link)?;
    let lexical = link.join("..").join("target.jar");
    let normalized = temporary.path().join("target.jar");
    create_target(&normalized)?;
    assert!(!lexical.exists());
    let wrapper = temporary.path().join("wrapper.jar");
    create_jar_with_class_path(&wrapper, "linked/../target.jar")?;
    let mut paths = VecDeque::new();
    let result = add_manifest_class_path(&wrapper, &mut paths, Limits::default())?;
    assert_eq!(result.appended, 1);
    assert!(result.diagnostics.is_empty());
    assert_eq!(paths, VecDeque::from([normalized]));
    Ok(())
}

#[test]
fn test_absolute_file_uri_fast_path_retains_syntax_rejections() -> Result<()> {
    let temporary = TempDir::new()?;
    let target = temporary.path().join("target.jar");
    create_target(&target)?;
    let directory_uri = Url::from_directory_path(temporary.path())
        .map_err(|()| anyhow::anyhow!("absolute fixture directory URI"))?;
    let wrapper = temporary.path().join("wrapper.jar");
    let seed = PathBuf::from("seed.jar");
    for suffix in [
        "target.jar?query",
        "target.jar#fragment",
        "%FF.jar",
        "%00.jar",
        "%GG.jar",
        "%2.jar",
        "%2e/target.jar",
        "%2e%2e/target.jar",
        "bad[bracket].jar",
        "back\\slash.jar",
    ] {
        let reference = format!("{directory_uri}{suffix}");
        create_jar_with_class_path(&wrapper, &reference)?;
        let mut paths = VecDeque::from([seed.clone()]);
        assert!(
            add_manifest_class_path(&wrapper, &mut paths, Limits::default()).is_err(),
            "absolute URI must be rejected: {reference}"
        );
        assert_eq!(paths, VecDeque::from([seed.clone()]));
    }
    Ok(())
}
