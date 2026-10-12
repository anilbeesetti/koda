//! Bounded ELF64 little-endian x86-64 parsing. The platform loader still resolves
//! its own dependencies; no C readelf/ldd inspection tool is needed here.

use anyhow::{Context as _, Result, ensure};
use serde::Serialize;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use super::super::{open_noatime, physical};

const MAX_DYNAMIC_BYTES: u64 = 1024 * 1024;
const MAX_STRING_BYTES: u64 = 4 * 1024 * 1024;
const MAX_VERSIONS: usize = 4096;

// Conservative cumulative accounting includes released temporary buffers as well
// as retained evidence. It cannot undercount simultaneous plans/copies and fits
// inside the independently admitted192MiB tool reserve without assuming reuse.
const MAX_AGGREGATE_ELF_ALLOCATION_BYTES: u64 = 32 * 1024 * 1024;
#[derive(Debug)]
pub(super) struct MetadataBudget {
    limit: u64,
    charged: u64,
    allocations: u64,
    last_rejected_phase: Option<&'static str>,
}
impl MetadataBudget {
    pub(super) fn new() -> Self {
        Self::with_limit(MAX_AGGREGATE_ELF_ALLOCATION_BYTES)
    }
    #[cfg(test)]
    pub(super) fn limited(limit: u64) -> Self {
        Self::with_limit(limit.min(MAX_AGGREGATE_ELF_ALLOCATION_BYTES))
    }
    #[cfg(test)]
    pub(super) fn charged(&self) -> u64 {
        self.charged
    }
    #[cfg(test)]
    pub(super) fn rejected_phase(&self) -> Option<&'static str> {
        self.last_rejected_phase
    }
    fn with_limit(limit: u64) -> Self {
        Self {
            limit,
            charged: 0,
            allocations: 0,
            last_rejected_phase: None,
        }
    }
    pub(super) fn charge(&mut self, bytes: u64, phase: &'static str) -> Result<()> {
        let next = self
            .charged
            .checked_add(bytes)
            .context("aggregate ELF allocation arithmetic overflow")?;
        if next > self.limit {
            self.last_rejected_phase = Some(phase);
            anyhow::bail!(
                "aggregate ELF allocation budget exhausted BEFORE {phase}: {} + {bytes} > {}",
                self.charged,
                self.limit
            );
        }
        self.charged = next;
        self.allocations = self
            .allocations
            .checked_add(1)
            .context("ELF allocation counter overflow")?;
        Ok(())
    }
    fn collection<T>(&mut self, count: usize, phase: &'static str) -> Result<()> {
        let bytes = (count as u64)
            .checked_mul(std::mem::size_of::<T>() as u64)
            .and_then(|n| n.checked_add(128))
            .and_then(|n| n.checked_mul(2))
            .context("ELF collection charge overflow")?;
        self.charge(bytes, phase)
    }
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        wanted: usize,
        phase: &'static str,
    ) -> Result<()> {
        if wanted > values.capacity() {
            self.collection::<T>(wanted, phase)?;
            values.try_reserve_exact(
                wanted
                    .checked_sub(values.len())
                    .context("ELF capacity subtraction")?,
            )?;
        }
        Ok(())
    }
    fn push<T>(&mut self, values: &mut Vec<T>, value: T, phase: &'static str) -> Result<()> {
        let wanted = values
            .len()
            .checked_add(1)
            .and_then(usize::checked_next_power_of_two)
            .context("ELF vector capacity overflow")?;
        self.reserve(values, wanted, phase)?;
        values.push(value);
        Ok(())
    }
    fn owned(&mut self, value: &str, phase: &'static str) -> Result<String> {
        self.charge(
            (value.len() as u64)
                .checked_add(64)
                .and_then(|n| n.checked_mul(2))
                .context("ELF string charge overflow")?,
            phase,
        )?;
        Ok(value.to_owned())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SharedEvidence(std::sync::Arc<ElfEvidence>);
impl SharedEvidence {
    pub(super) fn new(value: ElfEvidence, budget: &mut MetadataBudget) -> Result<Self> {
        budget.collection::<ElfEvidence>(1, "immutable shared ELF evidence")?;
        Ok(Self(std::sync::Arc::new(value)))
    }
}
impl std::ops::Deref for SharedEvidence {
    type Target = ElfEvidence;
    fn deref(&self) -> &ElfEvidence {
        &self.0
    }
}
impl serde::Serialize for SharedEvidence {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.0.as_ref().serialize(serializer)
    }
}
fn address_seen(
    set: &mut std::collections::HashSet<u64>,
    value: u64,
    budget: &mut MetadataBudget,
    phase: &'static str,
) -> Result<()> {
    ensure!(!set.contains(&value), "cyclic ELF version-address chain");
    if set.len() == set.capacity() {
        let wanted = set
            .len()
            .checked_add(1)
            .context("ELF address count overflow")?;
        // Bounds rounded hash-table growth, control bytes and allocator overhead.
        budget.charge(
            (wanted as u64)
                .checked_mul(128)
                .and_then(|n| n.checked_add(1024))
                .context("ELF address charge overflow")?,
            phase,
        )?;
        set.try_reserve(1)?;
    }
    set.insert(value);
    Ok(())
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(super) struct ElfEvidence {
    pub(super) class: &'static str,
    pub(super) endian: &'static str,
    pub(super) machine: u16,
    pub(super) file_type: u16,
    pub(super) interpreter: Option<String>,
    pub(super) soname: Option<String>,
    pub(super) needed: Vec<String>,
    pub(super) rpath: Option<String>,
    pub(super) runpath: Option<String>,
    pub(super) version_requirements: Vec<VersionRequirement>,
    pub(super) version_definitions: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(super) struct VersionRequirement {
    library: String,
    version: String,
    flags: u16,
}

#[derive(Clone, Copy)]
struct Segment {
    kind: u32,
    offset: u64,
    address: u64,
    file_bytes: u64,
}

struct Reader<'a> {
    file: File,
    bytes: u64,
    budget: &'a mut MetadataBudget,
}

impl Reader<'_> {
    fn at(&mut self, offset: u64, length: u64, cap: u64) -> Result<Vec<u8>> {
        ensure!(
            length <= cap
                && offset
                    .checked_add(length)
                    .is_some_and(|end| end <= self.bytes),
            "ELF range outside the bounded file"
        );
        self.budget
            .collection::<u8>(usize::try_from(length)?, "ELF read buffer")?;
        let mut bytes = vec![0; usize::try_from(length)?];
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut bytes)?;
        Ok(bytes)
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        bytes
            .get(offset..offset + 2)
            .context("truncated ELF u16")?
            .try_into()?,
    ))
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .context("truncated ELF u32")?
            .try_into()?,
    ))
}

fn u64_at(bytes: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .context("truncated ELF u64")?
            .try_into()?,
    ))
}

fn mapped(segments: &[Segment], address: u64, length: u64) -> Result<u64> {
    let mut matches = segments.iter().filter_map(|segment| {
        if segment.kind != 1 {
            return None;
        }
        let delta = address.checked_sub(segment.address)?;
        (delta.checked_add(length)? <= segment.file_bytes)
            .then(|| segment.offset.checked_add(delta))
            .flatten()
    });
    let result = matches
        .next()
        .context("ELF virtual range is not file-backed")?;
    ensure!(matches.next().is_none(), "ambiguous ELF virtual range");
    Ok(result)
}

fn string_budget(strings: &[u8], offset: u64, budget: &mut MetadataBudget) -> Result<String> {
    let bytes = strings
        .get(usize::try_from(offset)?..)
        .context("ELF string offset outside table")?;
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .context("ELF string has no NUL terminator")?;
    ensure!(end > 0 && end <= 4096, "empty or oversized ELF string");
    let string = std::str::from_utf8(&bytes[..end])?;
    ensure!(
        !string.chars().any(char::is_control),
        "ELF string contains control characters"
    );
    budget.owned(string, "owned ELF string")
}

#[cfg(test)]
fn string(strings: &[u8], offset: u64) -> Result<String> {
    string_budget(strings, offset, &mut MetadataBudget::new())
}
fn unique(tags: &[(u64, u64)], key: u64) -> Result<Option<u64>> {
    let mut values = tags
        .iter()
        .filter(|(tag, _)| *tag == key)
        .map(|(_, value)| *value);
    let first = values.next();
    ensure!(
        values.next().is_none(),
        "duplicate singleton ELF dynamic tag"
    );
    Ok(first)
}

pub(super) fn inspect(path: &Path) -> Result<ElfEvidence> {
    let file = open_noatime(path, false)?;
    let before = physical(&file.metadata()?)?;
    let evidence = inspect_file(file)?;
    ensure!(
        before == physical(&std::fs::symlink_metadata(path)?)?,
        "ELF pathname inverse differs"
    );
    Ok(evidence)
}

pub(super) fn inspect_file(file: File) -> Result<ElfEvidence> {
    inspect_file_with_budget(file, &mut MetadataBudget::new())
}
pub(super) fn inspect_file_with_budget(
    file: File,
    budget: &mut MetadataBudget,
) -> Result<ElfEvidence> {
    let before = physical(&file.metadata()?)?;
    ensure!(
        file.metadata()?.is_file() && before.bytes >= 64 && before.bytes <= super::MAX_BINARY_BYTES,
        "ELF is not a bounded regular file"
    );
    let mut reader = Reader {
        file,
        bytes: before.bytes,
        budget,
    };
    let header = reader.at(0, 64, 64)?;
    ensure!(
        header[..4] == *b"\x7fELF"
            && header[4] == 2
            && header[5] == 1
            && header[6] == 1
            && u16_at(&header, 18)? == 62
            && u32_at(&header, 20)? == 1,
        "only current ELF64 little-endian x86-64 is admitted"
    );
    let file_type = u16_at(&header, 16)?;
    ensure!(
        matches!(file_type, 2 | 3) && u16_at(&header, 52)? == 64 && u16_at(&header, 54)? == 56,
        "unsupported ELF type/header layout"
    );
    let count = u16_at(&header, 56)? as u64;
    ensure!(
        count > 0 && count <= 1024,
        "ELF program-header budget differs"
    );
    let program = reader.at(u64_at(&header, 32)?, count * 56, 56 * 1024)?;
    let mut segments = Vec::new();
    reader
        .budget
        .reserve(&mut segments, usize::try_from(count)?, "ELF segment vector")?;
    for entry in program.chunks_exact(56) {
        let segment = Segment {
            kind: u32_at(entry, 0)?,
            offset: u64_at(entry, 8)?,
            address: u64_at(entry, 16)?,
            file_bytes: u64_at(entry, 32)?,
        };
        ensure!(
            segment
                .offset
                .checked_add(segment.file_bytes)
                .is_some_and(|end| end <= reader.bytes),
            "ELF segment outside file"
        );
        segments.push(segment);
    }
    let mut interpreter = None;
    let mut tags: Vec<(u64, u64)> = Vec::new();
    let mut dynamic_seen = false;
    for segment in &segments {
        if segment.kind == 3 {
            ensure!(
                interpreter.is_none() && segment.file_bytes <= 4096,
                "interpreter layout differs"
            );
            let bytes = reader.at(segment.offset, segment.file_bytes, 4096)?;
            ensure!(
                bytes.last() == Some(&0) && !bytes[..bytes.len().saturating_sub(1)].contains(&0),
                "interpreter is not exactly NUL terminated"
            );
            let value = std::str::from_utf8(&bytes[..bytes.len() - 1])?;
            ensure!(
                value.starts_with('/') && !value.chars().any(char::is_control),
                "unsafe interpreter path"
            );
            interpreter = Some(reader.budget.owned(value, "ELF interpreter")?);
        }
        if segment.kind == 2 {
            ensure!(
                !dynamic_seen && segment.file_bytes % 16 == 0,
                "dynamic segment layout differs"
            );
            dynamic_seen = true;
            let bytes = reader.at(segment.offset, segment.file_bytes, MAX_DYNAMIC_BYTES)?;
            let tag_capacity = tags
                .len()
                .checked_add(bytes.len() / 16)
                .context("ELF tag count overflow")?;
            reader
                .budget
                .reserve(&mut tags, tag_capacity, "ELF dynamic-tag vector")?;
            let mut terminated = false;
            for entry in bytes.chunks_exact(16) {
                let tag = u64_at(entry, 0)?;
                if tag == 0 {
                    terminated = true;
                    break;
                }
                tags.push((tag, u64_at(entry, 8)?));
            }
            ensure!(terminated, "dynamic segment lacks DT_NULL");
        }
    }
    ensure!(dynamic_seen, "expected dynamic ELF");
    let string_address = unique(&tags, 5)?.context("dynamic string table absent")?;
    let string_bytes = unique(&tags, 10)?.context("dynamic string-table size absent")?;
    ensure!(
        string_bytes > 0 && string_bytes <= MAX_STRING_BYTES,
        "dynamic string-table budget differs"
    );
    let strings = reader.at(
        mapped(&segments, string_address, string_bytes)?,
        string_bytes,
        MAX_STRING_BYTES,
    )?;
    let mut needed = Vec::new();
    for value in tags
        .iter()
        .filter(|(tag, _)| *tag == 1)
        .map(|(_, value)| *value)
    {
        ensure!(needed.len() < 256, "ELF needed count budget exhausted");
        let value = string_budget(&strings, value, reader.budget)?;
        ensure!(
            !value.contains('/') && !needed.iter().any(|prior| prior == &value),
            "unsafe or duplicate ELF NEEDED"
        );
        reader
            .budget
            .push(&mut needed, value, "ELF needed vector")?;
    }
    let soname = unique(&tags, 14)?
        .map(|offset| string_budget(&strings, offset, reader.budget))
        .transpose()?;
    if let Some(soname) = &soname {
        ensure!(!soname.contains('/'), "SONAME is not a basename");
    }
    let mut requirements = Vec::new();
    if let Some(address) = unique(&tags, 0x6fff_fffe)? {
        let count = unique(&tags, 0x6fff_ffff)?.context("VERNEED count absent")?;
        ensure!(count > 0 && count <= 256, "VERNEED count budget differs");
        let mut current = address;
        let mut seen = std::collections::HashSet::new();
        for index in 0..count {
            address_seen(&mut seen, current, reader.budget, "VERNEED seen addresses")?;
            let need = reader.at(mapped(&segments, current, 16)?, 16, 16)?;
            ensure!(u16_at(&need, 0)? == 1, "unsupported VERNEED version");
            let library = string_budget(&strings, u32_at(&need, 4)? as u64, reader.budget)?;
            ensure!(!library.contains('/'), "unsafe VERNEED library name");
            let auxiliaries = u16_at(&need, 2)? as usize;
            ensure!(
                auxiliaries > 0
                    && requirements
                        .len()
                        .checked_add(auxiliaries)
                        .is_some_and(|len| len <= MAX_VERSIONS),
                "VERNEED auxiliary budget exhausted"
            );
            let mut auxiliary = current
                .checked_add(u32_at(&need, 8)? as u64)
                .context("VERNEED auxiliary overflow")?;
            let mut auxiliary_seen = std::collections::HashSet::new();
            for auxiliary_index in 0..auxiliaries {
                address_seen(
                    &mut auxiliary_seen,
                    auxiliary,
                    reader.budget,
                    "VERNEED auxiliary seen addresses",
                )?;
                let entry = reader.at(mapped(&segments, auxiliary, 16)?, 16, 16)?;
                let requirement = VersionRequirement {
                    library: reader
                        .budget
                        .owned(&library, "repeated VERNEED library string")?,
                    version: string_budget(&strings, u32_at(&entry, 8)? as u64, reader.budget)?,
                    flags: u16_at(&entry, 4)?,
                };
                reader
                    .budget
                    .push(&mut requirements, requirement, "VERNEED requirement vector")?;
                let next = u32_at(&entry, 12)? as u64;
                ensure!(
                    (auxiliary_index + 1 == auxiliaries) == (next == 0),
                    "VERNEED auxiliary count/link differs"
                );
                auxiliary = auxiliary
                    .checked_add(next)
                    .context("VERNEED chain overflow")?;
            }
            let next = u32_at(&need, 12)? as u64;
            ensure!(
                (index + 1 == count) == (next == 0),
                "VERNEED count/link differs"
            );
            current = current
                .checked_add(next)
                .context("VERNEED chain overflow")?;
        }
    } else {
        ensure!(
            unique(&tags, 0x6fff_ffff)?.unwrap_or(0) == 0,
            "VERNEED count without table"
        );
    }
    let mut definitions = Vec::new();
    if let Some(address) = unique(&tags, 0x6fff_fffc)? {
        let count = unique(&tags, 0x6fff_fffd)?.context("VERDEF count absent")?;
        ensure!(
            count > 0 && count <= MAX_VERSIONS as u64,
            "VERDEF count budget differs"
        );
        let mut current = address;
        let mut seen = std::collections::HashSet::new();
        for index in 0..count {
            address_seen(&mut seen, current, reader.budget, "VERDEF seen addresses")?;
            let definition = reader.at(mapped(&segments, current, 20)?, 20, 20)?;
            ensure!(
                u16_at(&definition, 0)? == 1 && u16_at(&definition, 6)? > 0,
                "unsupported VERDEF layout"
            );
            let auxiliary = current
                .checked_add(u32_at(&definition, 12)? as u64)
                .context("VERDEF auxiliary overflow")?;
            let entry = reader.at(mapped(&segments, auxiliary, 8)?, 8, 8)?;
            let value = string_budget(&strings, u32_at(&entry, 0)? as u64, reader.budget)?;
            reader
                .budget
                .push(&mut definitions, value, "VERDEF definition vector")?;
            let next = u32_at(&definition, 16)? as u64;
            ensure!(
                (index + 1 == count) == (next == 0),
                "VERDEF count/link differs"
            );
            current = current.checked_add(next).context("VERDEF chain overflow")?;
        }
    } else {
        ensure!(
            unique(&tags, 0x6fff_fffd)?.unwrap_or(0) == 0,
            "VERDEF count without table"
        );
    }
    ensure!(
        before == physical(&reader.file.metadata()?)?,
        "ELF physical inverse differs"
    );
    Ok(ElfEvidence {
        class: "ELF64",
        endian: "little",
        machine: 62,
        file_type,
        interpreter,
        soname,
        needed,
        rpath: unique(&tags, 15)?
            .map(|offset| string_budget(&strings, offset, reader.budget))
            .transpose()?,
        runpath: unique(&tags, 29)?
            .map(|offset| string_budget(&strings, offset, reader.budget))
            .transpose()?,
        version_requirements: requirements,
        version_definitions: definitions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_require_bounded_nul_and_safe_utf8() {
        assert_eq!(string(b"\0GLIBC_2.39\0", 1).unwrap(), "GLIBC_2.39");
        assert!(string(b"no-terminator", 0).is_err());
        assert!(string(b"\xff\0", 0).is_err());
        assert!(string(b"\n\0", 0).is_err());
        assert!(string(b"\0", 0).is_err());
    }

    #[test]
    fn virtual_ranges_reject_overflow_bss_and_ambiguous_maps() {
        let segment = Segment {
            kind: 1,
            offset: 64,
            address: 4096,
            file_bytes: 32,
        };
        assert_eq!(mapped(&[segment], 4100, 4).unwrap(), 68);
        assert!(mapped(&[segment], 4120, 16).is_err());
        assert!(mapped(&[segment], u64::MAX, 32).is_err());
        assert!(mapped(&[segment, segment], 4100, 4).is_err());
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
    fn inspect_actual_rust_test_elf_preserves_its_physical_identity() {
        let executable = std::env::current_exe().unwrap();
        let before = physical(
            &open_noatime(&executable, false)
                .unwrap()
                .metadata()
                .unwrap(),
        )
        .unwrap();
        let facts = inspect(&executable).unwrap();
        assert_eq!(facts.machine, 62);
        assert_eq!(facts.class, "ELF64");
        assert!(facts.interpreter.as_ref().unwrap().starts_with('/'));
        assert!(facts.needed.iter().any(|needed| needed == "libc.so.6"));
        assert!(!facts.version_requirements.is_empty());
        let after = physical(
            &open_noatime(&executable, false)
                .unwrap()
                .metadata()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(before, after);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn malformed_elf_is_rejected_without_mapping_or_running_it() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("not-an-elf");
        std::fs::write(&path, [0; 64]).unwrap();
        let before = physical(&open_noatime(&path, false).unwrap().metadata().unwrap()).unwrap();
        assert!(inspect(&path).is_err());
        assert_eq!(
            before,
            physical(&open_noatime(&path, false).unwrap().metadata().unwrap()).unwrap()
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
pub(super) fn valid_repeated_version_fixture(count: usize, string_bytes: usize) -> Vec<u8> {
    assert!(count > 0 && count <= MAX_VERSIONS && string_bytes > 0 && string_bytes <= 4096);
    let strings_offset = 512_usize;
    let strings_len = string_bytes + 2;
    let versions_offset = (strings_offset + strings_len + 7) / 8 * 8;
    let mut bytes = vec![0_u8; versions_offset + count * 28];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&3_u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&62_u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
    bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&2_u16.to_le_bytes());
    let size = bytes.len() as u64;
    for (offset, kind, file_offset, length) in [(64_usize, 1_u32, 0_u64, size), (120, 2, 256, 80)] {
        bytes[offset..offset + 4].copy_from_slice(&kind.to_le_bytes());
        bytes[offset + 8..offset + 16].copy_from_slice(&file_offset.to_le_bytes());
        bytes[offset + 16..offset + 24].copy_from_slice(&(0x400000 + file_offset).to_le_bytes());
        bytes[offset + 32..offset + 40].copy_from_slice(&length.to_le_bytes());
    }
    for (index, (tag, value)) in [
        (5_u64, 0x400000 + strings_offset as u64),
        (10, strings_len as u64),
        (0x6fff_fffc, 0x400000 + versions_offset as u64),
        (0x6fff_fffd, count as u64),
        (0, 0),
    ]
    .into_iter()
    .enumerate()
    {
        let offset = 256 + index * 16;
        bytes[offset..offset + 8].copy_from_slice(&tag.to_le_bytes());
        bytes[offset + 8..offset + 16].copy_from_slice(&value.to_le_bytes());
    }
    bytes[strings_offset + 1..strings_offset + 1 + string_bytes].fill(b'v');
    for index in 0..count {
        let offset = versions_offset + index * 28;
        bytes[offset..offset + 2].copy_from_slice(&1_u16.to_le_bytes());
        bytes[offset + 6..offset + 8].copy_from_slice(&1_u16.to_le_bytes());
        bytes[offset + 12..offset + 16].copy_from_slice(&20_u32.to_le_bytes());
        let next = if index + 1 == count { 0 } else { 28 };
        bytes[offset + 16..offset + 20].copy_from_slice(&(next as u32).to_le_bytes());
        bytes[offset + 20..offset + 24].copy_from_slice(&1_u32.to_le_bytes());
    }
    bytes
}
#[cfg(test)]
mod aggregate_budget_tests {
    use super::*;
    #[test]
    fn owned_strings_repeated_libraries_and_collection_growth_reject_before_allocation() {
        let input = format!("{}\0", "v".repeat(4096));
        let mut budget = MetadataBudget::limited(8192);
        let before = budget.allocations;
        assert!(string_budget(input.as_bytes(), 0, &mut budget).is_err());
        assert_eq!(budget.charged, 0);
        assert_eq!(budget.allocations, before);
        assert_eq!(budget.rejected_phase(), Some("owned ELF string"));
        let mut names = Vec::<String>::new();
        let mut budget = MetadataBudget::limited(128);
        let capacity = names.capacity();
        assert!(
            budget
                .reserve(&mut names, 4096, "fixture collection growth")
                .is_err()
        );
        assert_eq!(names.capacity(), capacity);
        assert!(names.is_empty());
        let mut budget = MetadataBudget::limited(17000);
        let long = "v".repeat(4096);
        let first = budget
            .owned(&long, "repeated VERNEED library string")
            .unwrap();
        let second = budget
            .owned(&long, "repeated VERNEED library string")
            .unwrap();
        let before = budget.allocations;
        let charged = budget.charged;
        assert!(
            budget
                .owned(&long, "repeated VERNEED library string")
                .is_err()
        );
        assert_eq!(budget.allocations, before);
        assert_eq!(budget.charged, charged);
        assert_eq!(first, second);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn valid_maximum_length_version_records_share_budget_across_original_and_copied_parsers() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("valid-version-fixture.so");
        std::fs::write(&path, valid_repeated_version_fixture(64, 4096)).unwrap();
        let mut measured = MetadataBudget::new();
        let facts =
            inspect_file_with_budget(open_noatime(&path, false).unwrap(), &mut measured).unwrap();
        assert_eq!(facts.version_definitions.len(), 64);
        assert!(
            facts
                .version_definitions
                .iter()
                .all(|value| value.len() == 4096)
        );
        let one = measured.charged();
        let mut shared = MetadataBudget::limited(one + one / 2);
        let original =
            inspect_file_with_budget(open_noatime(&path, false).unwrap(), &mut shared).unwrap();
        assert_eq!(original, facts);
        let copy = temporary.path().join("copied-fixture.so");
        std::fs::copy(&path, &copy).unwrap();
        assert!(
            inspect_file_with_budget(open_noatime(&copy, false).unwrap(), &mut shared).is_err()
        );
        assert!(shared.charged() <= one + one / 2);
        assert!(shared.rejected_phase().is_some());
        assert_eq!(std::fs::read(path).unwrap(), std::fs::read(copy).unwrap());
    }
}
