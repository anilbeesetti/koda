#![allow(clippy::disallowed_methods, reason = "tooling is exempt")]

use std::{collections::BTreeMap, fs, path::{Path, PathBuf}, process::Command};
use anyhow::{Context as _, Result, ensure};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const LIMIT: u64 = 8 * 1024 * 1024;
const PACKAGE_MANIFEST: &str = "crates/android_tools/Cargo.toml";
const REFERENCE_MANIFEST: &str = "crates/android_tools/test_data/string_helper/reference-manifest.json";

#[derive(Parser)]
pub struct Args {
    #[command(subcommand)]
    command: Operation,
}

#[derive(Subcommand)]
enum Operation {
    /// Validate a completed workflow's exact named Cargo log; do not edit the ledger.
    Record {
        #[arg(long, default_value = ".")]
        repository: PathBuf,
        #[arg(long)]
        log: PathBuf,
        #[arg(long)]
        test_name: String,
        #[arg(long)]
        tested_commit: String,
        #[arg(long, allow_hyphen_values = true)]
        exit_code: i32,
        #[arg(long)]
        output: PathBuf,
    },
    /// Reproduce the attributed tables from SHA-bound original Unicode files.
    GenerateCaseData {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
}

pub fn run(args: Args) -> Result<()> {
    match args.command {
        Operation::Record { repository, log, test_name, tested_commit, exit_code, output } => {
            record(&repository, &log, &test_name, &tested_commit, exit_code, &output)
        }
        Operation::GenerateCaseData { data_dir, output } => {
            let generated = generate_case_data(&data_dir)?;
            create_new(&output, generated.as_bytes())
        }
    }
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).context("stat recorder input")?;
    ensure!(metadata.is_file() && metadata.len() <= LIMIT, "Recorder input is not a bounded regular file: {}", path.display());
    // take() caps the allocation even if an input grows after its metadata check.
    use std::io::Read as _;
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(LIMIT + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= LIMIT, "Recorder input grew beyond its byte limit");
    Ok(bytes)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn create_new(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn git(repository: &Path) -> Command {
    let mut command = Command::new("git");
    command.args(["-c", "core.preloadIndex=false", "-c", "index.threads=1"])
        .env("GIT_OPTIONAL_LOCKS", "0").current_dir(repository);
    command
}

fn verify_revision(repository: &Path, tested_commit: &str) -> Result<()> {
    ensure!(tested_commit.len() == 40 && tested_commit.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)), "Expected an immutable lowercase Git commit");
    let head = git(repository).args(["rev-parse", "HEAD"]).output()?;
    ensure!(head.status.success() && std::str::from_utf8(&head.stdout)?.trim() == tested_commit, "Recorder checkout differs from the tested commit");
    let paths = ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "crates/android_tools", "tooling/xtask"];
    for cached in [false, true] {
        let mut command = git(repository);
        command.args(["diff", "--quiet"]);
        if cached { command.arg("--cached"); }
        ensure!(command.args([tested_commit, "--"]).args(paths).status()?.success(), "Recorder inputs changed since the tested commit");
    }
    let untracked = git(repository).args(["ls-files", "--others", "--exclude-standard", "--"]).args(paths).output()?;
    ensure!(untracked.status.success() && untracked.stdout.is_empty(), "Recorder cannot credit untracked implementation inputs");
    Ok(())
}

fn cargo_arguments(target: &str, test_name: &str) -> Vec<String> {
    ["test", "--locked", "--manifest-path", PACKAGE_MANIFEST, "-p", "android_tools", "--test", target, test_name, "--", "--exact", "--show-output"]
        .into_iter().map(str::to_owned).collect()
}

fn record(repository: &Path, log: &Path, name: &str, commit: &str, exit_code: i32, output: &Path) -> Result<()> {
    ensure!(exit_code == 0 || exit_code == 101, "Only a completed Cargo test outcome (0 or 101) can be recorded");
    let repository = repository.canonicalize()?;
    verify_revision(&repository, commit)?;
    let manifest = read_bounded(&repository.join(PACKAGE_MANIFEST))?;
    let reference = read_bounded(&repository.join(REFERENCE_MANIFEST))?;
    let matrix: serde_json::Value = serde_json::from_slice(&reference)?;
    let entry = matrix["tests"].as_array().context("Reference suite mapping is missing")?
        .iter().find(|entry| entry["rust_test_name"].as_str() == Some(name))
        .context("Test name is not an individually mapped StringHelper original")?;
    let target = entry["rust_integration_target"].as_str().context("Missing integration test target")?;
    ensure!(matches!(target, "string_helper" | "string_helper_tokenize"), "Unexpected StringHelper integration target");
    let source_path = format!("crates/android_tools/tests/{target}.rs");
    let source = read_bounded(&repository.join(&source_path))?;
    let log_bytes = read_bounded(log)?;
    let text = std::str::from_utf8(&log_bytes).context("Cargo log is not UTF-8")?;
    super::android_parity::verify_test_output(text, name, exit_code == 0)?;
    ensure!(read_bounded(&repository.join(&source_path))? == source
        && read_bounded(&repository.join(PACKAGE_MANIFEST))? == manifest
        && read_bounded(&repository.join(REFERENCE_MANIFEST))? == reference, "Recorder inputs changed while recording");
    verify_revision(&repository, commit)?;
    let mut command = vec!["cargo".to_owned()];
    command.extend(cargo_arguments(target, name));
    let evidence = serde_json::json!({
        "schema_version": 1,
        "evidence_kind": "workflow_supplied_completed_cargo_log",
        "reference_id": entry["reference_id"],
        "test_name": name, "tested_commit": commit, "command": command,
        "exit_code": exit_code,
        "log": {"path": log, "sha256": digest(&log_bytes), "bytes": log_bytes.len()},
        "source": {"path": source_path, "sha256": digest(&source)},
        "manifest": {"path": PACKAGE_MANIFEST, "sha256": digest(&manifest)},
        "reference_manifest_sha256": digest(&reference),
        "canonical_ledger_modified": false,
        "limitations": "Recorder validates supplied log structure and clean input identity; workflow provenance must be reviewed separately. This does not establish baseline JBR compatibility, IDE consumer execution or global census completeness."
    });
    let mut bytes = serde_json::to_vec_pretty(&evidence)?;
    bytes.push(b'\n');
    create_new(output, &bytes)
}

#[derive(Deserialize)]
struct UnicodeInput {
    version: String,
    file: String,
    bytes: usize,
    sha256: String,
}

fn generate_case_data(data_dir: &Path) -> Result<String> {
    let inputs: Vec<UnicodeInput> = serde_json::from_str(include_str!("../../../../crates/android_tools/test_data/string_helper/unicode/manifest.json"))?;
    let mut upper = BTreeMap::<(u32, u32), u8>::new();
    let mut lower = BTreeMap::<(u32, u32), u8>::new();
    for (column, version) in ["13.0.0", "14.0.0", "15.0.0", "15.1.0", "16.0.0"].iter().enumerate() {
        for name in ["UnicodeData.txt", "SpecialCasing.txt"] {
            let expected = inputs.iter().find(|input| input.version == *version && input.file == name).context("Missing Unicode input binding")?;
            let bytes = read_bounded(&data_dir.join(format!("{version}-{name}")))?;
            ensure!(bytes.len() == expected.bytes && digest(&bytes) == expected.sha256, "Unicode input does not match the attributed version");
            let text = std::str::from_utf8(&bytes)?;
            for line in text.lines() {
                if name == "UnicodeData.txt" {
                    let fields = line.split(';').collect::<Vec<_>>();
                    ensure!(fields.len() == 15, "Malformed UnicodeData row");
                    let source = u32::from_str_radix(fields[0], 16)?;
                    for (field, table) in [(fields[12], &mut upper), (fields[13], &mut lower)] {
                        if !field.is_empty() {
                            let target = u32::from_str_radix(field, 16)?;
                            ensure!(source > 0xffff || target <= 0xffff, "Simple BMP mapping exceeds a Java char");
                            if source != target { *table.entry((source, target)).or_default() |= 1 << column; }
                        }
                    }
                } else {
                    let line = line.split('#').next().unwrap_or_default().trim();
                    if line.is_empty() { continue; }
                    let fields = line.split(';').map(str::trim).collect::<Vec<_>>();
                    ensure!(fields.len() >= 5, "Malformed SpecialCasing row");
                    if !fields[4].is_empty() { continue; }
                    ensure!(u32::from_str_radix(fields[0], 16)? <= 0xffff, "Supplementary full casing requires an additional mapping");
                    let lowercase = fields[1].split_whitespace().collect::<Vec<_>>();
                    ensure!(lowercase.len() <= 1 || fields[0] == "0130" && lowercase == ["0069", "0307"], "Unimplemented unconditional full-lowercase expansion");
                }
            }
        }
    }
    let mut result = String::from("// Generated from UnicodeData.txt 13.0.0, 14.0.0, 15.0.0, 15.1.0 and 16.0.0.\n// Unicode License V3; see test_data/string_helper/unicode/NOTICE.\n// Reproduce with: cargo xtask android-string-helper generate-case-data --data-dir <data-dir> --output <new-file>\n\n");
    use std::fmt::Write as _;
    for (name, table) in [("UPPERCASE", upper), ("LOWERCASE", lower)] {
        writeln!(result, "pub(super) const {name}: &[(u32, u32, u8)] = &[")?;
        for ((source, target), versions) in table { writeln!(result, "    (0x{source:x}, 0x{target:x}, 0x{versions:x}),")?; }
        result.push_str("];\n\n");
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_recording_requires_one_executed_case_and_no_ignored_tests() {
        let name = "string_helper_as_separated_list_contains";
        let valid = format!("running 1 test\ntest {name} ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 32 filtered out; finished in 0.00s\n");
        assert!(super::super::android_parity::verify_test_output(&valid, name, true).is_ok());
        assert!(super::super::android_parity::verify_test_output(&valid.replace("1 passed", "0 passed"), name, true).is_err());
        assert!(super::super::android_parity::verify_test_output(&valid.replace("0 ignored", "1 ignored"), name, true).is_err());
        assert!(super::super::android_parity::verify_test_output(&valid, "other_test", true).is_err());
        assert!(super::super::android_parity::verify_test_output(&(valid.clone() + &valid), name, true).is_err());
    }

    #[test]
    fn named_command_keeps_exact_filter_and_locked_inputs() {
        assert_eq!(cargo_arguments("string_helper", "string_helper_as_separated_list_contains"), ["test", "--locked", "--manifest-path", PACKAGE_MANIFEST, "-p", "android_tools", "--test", "string_helper", "string_helper_as_separated_list_contains", "--", "--exact", "--show-output"]);
    }
}
