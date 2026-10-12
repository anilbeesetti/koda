//! Producer disk/inode preflight. Retained payload, archive and raw pieces coexist;
//! admission never assumes favorable compression or deletes failure evidence.

use super::{
    FileDigest, GitBatch, MAX_ARCHIVE_BYTES, MAX_BUILD_EVIDENCE_BYTES, MAX_MANIFEST_BYTES,
    MAX_PAYLOAD_BYTES, MAX_PAYLOAD_ENTRIES, PIECE_BYTES, SourceArgs, SourceManifest, command,
};
use anyhow::{Context as _, Result, ensure};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
    path::Path,
};

pub(super) fn compression_bound(bytes: u64) -> Result<u64> {
    // Deliberately exceeds zlib's compressBound for every supported input size.
    bytes
        .checked_add(bytes / 1000)
        .and_then(|n| n.checked_add(1024))
        .context("conservative compression bound overflow")
}

pub(super) struct LimitedWriter<W> {
    inner: W,
    limit: u64,
    bytes: u64,
}
impl<W> LimitedWriter<W> {
    pub(super) fn new(inner: W, limit: u64) -> Self {
        Self {
            inner,
            limit,
            bytes: 0,
        }
    }
    pub(super) fn into_inner(self) -> W {
        self.inner
    }
}
impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self
            .bytes
            .checked_add(bytes.len() as u64)
            .filter(|n| *n <= self.limit)
            .ok_or_else(|| io::Error::other("compressed output budget exhausted before write"))?;
        let written = self.inner.write(bytes)?;
        self.bytes = count - bytes.len() as u64 + written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub(super) struct GitPlan {
    pub(super) objects: BTreeMap<String, String>,
    pub(super) compressed_upper_bytes: u64,
}

pub(super) fn git_plan(source: &SourceArgs) -> Result<GitPlan> {
    let listing = command(
        Some(&source.source),
        "git",
        &["ls-tree", "-rtz", "--full-tree", &source.sha],
        super::super::MAX_TREE_BYTES,
    )?;
    let mut objects = BTreeMap::new();
    objects.insert(source.sha.clone(), "commit".to_owned());
    objects.insert(source.tree.clone(), "tree".to_owned());
    for record in listing
        .split(|byte| *byte == 0)
        .filter(|row| !row.is_empty())
    {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .context("malformed reconstruction tree")?;
        let fields: Vec<_> = std::str::from_utf8(&record[..tab])?
            .split_whitespace()
            .collect();
        ensure!(
            fields.len() == 3 && matches!(fields[1], "blob" | "tree"),
            "unsupported Git reconstruction member"
        );
        super::require_hex(fields[2], 40)?;
        objects.insert(fields[2].to_owned(), fields[1].to_owned());
        ensure!(
            objects.len() <= MAX_PAYLOAD_ENTRIES,
            "reconstruction object count exhausted"
        );
    }
    let mut batch = GitBatch::new(&source.source)?;
    let mut logical = 0_u64;
    let mut compressed = 0_u64;
    for (oid, kind) in &objects {
        let bytes = batch.object(oid, kind)?;
        logical = logical
            .checked_add(bytes.len() as u64)
            .context("Git planning size overflow")?;
        ensure!(
            logical <= super::super::MAX_SOURCE_BYTES + super::super::MAX_TREE_BYTES,
            "Git planning total budget exhausted before writes"
        );
        let header = format!("{kind} {}\0", bytes.len()).len() as u64;
        compressed = compressed
            .checked_add(compression_bound(bytes.len() as u64 + header)?)
            .context("Git compression total overflow")?;
    }
    batch.finish()?;
    Ok(GitPlan {
        objects,
        compressed_upper_bytes: compressed,
    })
}

#[derive(Debug, Serialize)]
pub(super) struct Plan {
    pub(super) state: &'static str,
    pub(super) payload_upper_bytes: u64,
    pub(super) archive_upper_bytes: u64,
    pub(super) retained_piece_upper_bytes: u64,
    pub(super) required_bytes: u64,
    pub(super) required_inodes: u64,
    pub(super) available_bytes: u64,
    pub(super) available_inodes: u64,
    pub(super) original_floor_bytes: u64,
    pub(super) scratch_bytes: u64,
    pub(super) fits: bool,
    pub(super) payload_writes_before_admission: u32,
}

fn add_parents(path: &str, directories: &mut BTreeSet<String>) {
    let mut parent = Path::new(path).parent();
    while let Some(value) = parent.filter(|value| !value.as_os_str().is_empty()) {
        directories.insert(value.to_string_lossy().into_owned());
        parent = value.parent();
    }
}

pub(super) fn plan(
    manifest: &SourceManifest,
    git: &GitPlan,
    binary: &FileDigest,
    dependencies: impl Iterator<Item = (String, u64)>,
    evidence_bytes: u64,
    block: u64,
    available: u64,
    available_inodes: u64,
) -> Result<Plan> {
    ensure!(
        block > 0
            && block <= 1024 * 1024
            && binary.bytes > 0
            && binary.bytes <= super::MAX_BINARY_BYTES,
        "invalid producer filesystem/binary facts"
    );
    let mut paths = BTreeSet::new();
    let mut directories = BTreeSet::from([
        "runtime".to_owned(),
        "runtime/lib".to_owned(),
        "runtime/evidence".to_owned(),
    ]);
    let mut payload = 0_u64;
    for entry in &manifest.entries {
        let path = format!("runtime/source/{}", entry.path);
        super::payload_relative(&path)?;
        add_parents(&path, &mut directories);
        paths.insert(path);
        if entry.mode != "120000" {
            payload = payload
                .checked_add(entry.bytes)
                .context("source copy total overflow")?;
        }
    }
    for oid in git.objects.keys() {
        let path = format!("runtime/source/.git/objects/{}/{}", &oid[..2], &oid[2..]);
        add_parents(&path, &mut directories);
        paths.insert(path);
    }
    directories.insert("runtime/source/.git/refs".to_owned());
    for name in ["HEAD", "shallow", "config", "index"] {
        let path = format!("runtime/source/.git/{name}");
        add_parents(&path, &mut directories);
        paths.insert(path);
    }
    let index_upper = manifest
        .entries
        .iter()
        .try_fold(64 * 1024_u64, |bytes, entry| {
            bytes
                .checked_add(entry.path.len() as u64 + 256)
                .context("Git index upper bound overflow")
        })?;
    payload = payload
        .checked_add(git.compressed_upper_bytes)
        .and_then(|n| n.checked_add(index_upper))
        .and_then(|n| n.checked_add(binary.bytes))
        .and_then(|n| n.checked_add(evidence_bytes))
        .context("producer payload bound overflow")?;
    paths.insert("runtime/source/target/debug/koda".to_owned());
    add_parents("runtime/source/target/debug/koda", &mut directories);
    let mut dependency_count = 0;
    for (soname, bytes) in dependencies {
        ensure!(
            bytes > 0 && bytes <= super::MAX_BINARY_BYTES,
            "dependency copy bound differs"
        );
        dependency_count += 1;
        ensure!(
            dependency_count <= 256 && !soname.contains('/'),
            "dependency count/name differs"
        );
        paths.insert(format!("runtime/lib/{soname}"));
        payload = payload
            .checked_add(bytes)
            .context("dependency aggregate overflow")?;
        ensure!(
            payload <= MAX_PAYLOAD_BYTES,
            "dependency aggregate exceeds payload budget before copy"
        );
    }
    // All generated evidence files and a bound for the payload/metadata manifests
    // outside runtime are reserved before their creation.
    for name in [
        "source-manifest.json",
        "source-before-build-observation.json",
        "source-after-build-observation.json",
        "cargo-build.jsonl",
        "package-atime-transitions.json",
        "native-build.json",
        "source-package-observation.json",
        "producer-admission.json",
        "cargo-process-closure.json",
    ] {
        paths.insert(format!("runtime/evidence/{name}"));
    }
    ensure!(
        payload > 0 && payload <= MAX_PAYLOAD_BYTES,
        "aggregate payload exceeds budget before copy"
    );
    let entries = paths
        .len()
        .checked_add(directories.len())
        .context("producer entry overflow")?;
    ensure!(
        entries <= MAX_PAYLOAD_ENTRIES,
        "producer payload entry budget exhausted before copy"
    );
    // Includes every body padding block, real header, two GNU extension records,
    // their padding and the two terminal blocks. No compressed-size optimism.
    let raw_tar = payload
        .checked_add(
            (entries as u64)
                .checked_mul(5 * 1024)
                .context("tar overhead overflow")?,
        )
        .and_then(|n| n.checked_add(1024))
        .context("tar upper bound overflow")?;
    let archive = compression_bound(raw_tar)?;
    ensure!(
        archive <= MAX_ARCHIVE_BYTES,
        "incompressible archive upper bound exceeds transport budget before copy"
    );
    let pieces = archive.div_ceil(PIECE_BYTES);
    let inodes = (entries as u64)
        .checked_add(pieces)
        .and_then(|n| n.checked_add(384))
        .context("producer inode bound overflow")?;
    let padding = inodes
        .checked_mul(block)
        .context("producer block padding overflow")?;
    let scratch = 64 * 1024 * 1024;
    let floor = super::super::ORIGINAL_FLOOR;
    let outside_evidence = 4 * MAX_MANIFEST_BYTES + MAX_BUILD_EVIDENCE_BYTES;
    let required = payload
        .checked_add(archive)
        .and_then(|n| n.checked_add(archive))
        .and_then(|n| n.checked_add(padding))
        .and_then(|n| n.checked_add(outside_evidence))
        .and_then(|n| n.checked_add(scratch))
        .and_then(|n| n.checked_add(floor))
        .context("producer total admission overflow")?;
    Ok(Plan {
        state: "PRODUCER_DISK_INODE_PREFLIGHT_BEFORE_PAYLOAD_WRITES",
        payload_upper_bytes: payload,
        archive_upper_bytes: archive,
        retained_piece_upper_bytes: archive,
        required_bytes: required,
        required_inodes: inodes,
        available_bytes: available,
        available_inodes,
        original_floor_bytes: floor,
        scratch_bytes: scratch,
        fits: available >= required && available_inodes >= inodes,
        payload_writes_before_admission: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    struct CountWrites(Rc<Cell<usize>>);
    impl Write for CountWrites {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.set(self.0.get() + bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn compressed_output_rejects_the_first_over_budget_write_without_forwarding_it() {
        let bytes = Rc::new(Cell::new(0));
        let mut writer = LimitedWriter::new(CountWrites(bytes.clone()), 4);
        writer.write_all(b"abc").unwrap();
        assert!(writer.write_all(b"de").is_err());
        assert_eq!(bytes.get(), 3);
        writer.write_all(b"d").unwrap();
        assert_eq!(bytes.get(), 4);
        assert!(compression_bound(u64::MAX).is_err());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn producer_preflight_rejects_disk_inodes_and_dependency_aggregate_before_any_payload_write() {
        let (_temporary, source) = super::super::super::tests::fixture();
        let (manifest, _) =
            super::super::observe(&source, super::super::Authentication::Materialized).unwrap();
        let git = git_plan(&source).unwrap();
        let binary = FileDigest {
            bytes: 64,
            sha256: "a".repeat(64),
        };
        let plan = plan(
            &manifest,
            &git,
            &binary,
            std::iter::empty(),
            1024,
            4096,
            u64::MAX,
            u64::MAX,
        )
        .unwrap();
        assert!(plan.fits);
        assert_eq!(plan.payload_writes_before_admission, 0);
        assert!(
            !self::plan(
                &manifest,
                &git,
                &binary,
                std::iter::empty(),
                1024,
                4096,
                plan.required_bytes - 1,
                u64::MAX
            )
            .unwrap()
            .fits
        );
        assert!(
            !self::plan(
                &manifest,
                &git,
                &binary,
                std::iter::empty(),
                1024,
                4096,
                u64::MAX,
                plan.required_inodes - 1
            )
            .unwrap()
            .fits
        );
        assert!(
            self::plan(
                &manifest,
                &git,
                &binary,
                (0..4).map(|n| (format!("lib{n}.so"), super::super::MAX_BINARY_BYTES)),
                1024,
                4096,
                u64::MAX,
                u64::MAX
            )
            .is_err()
        );
        assert!(!source.source.join("runtime").exists());
        assert!(source.source.join("README.md").is_file());
    }
}
