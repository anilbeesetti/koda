//! Opt-in relocation of owned ELF copies. The built executable and system
//! libraries are read-only inputs. Embedded search paths never reach a loader:
//! dependencies are discovered statically in explicit system directories, then
//! the loader verifies the complete delivered closure through retained FDs.

use super::*;

#[derive(Clone, Copy, Debug, clap::ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum LoaderPolicy {
    StripEmbeddedSearchPaths,
}

#[derive(Serialize)]
pub(super) struct Transformation {
    policy: LoaderPolicy,
    source_path: PathBuf,
    original_digest: FileDigest,
    original_physical13: Physical13,
    copied_before_transform_digest: FileDigest,
    delivered_digest: FileDigest,
    dynamic_file_offset: u64,
    dynamic_bytes: u64,
    original_dynamic_sha256: String,
    delivered_dynamic_sha256: String,
    removed_rpath: Option<String>,
    removed_runpath: Option<String>,
    removed_entries: usize,
    all_other_file_bytes_equal: bool,
    all_other_elf_metadata_equal: bool,
}

#[derive(Serialize)]
pub(super) struct Evidence {
    pub(super) policy: LoaderPolicy,
    pub(super) main: Transformation,
    pub(super) libraries: Vec<Transformation>,
    pub(super) discovery: &'static str,
    pub(super) dependency_evidence_role: &'static str,
    pub(super) launch_requirement: &'static str,
    pub(super) original_input_loader_invoked: bool,
    pub(super) constructors_entered_during_packaging: bool,
    pub(super) runtime_acceptance_credit: u32,
}

pub(super) struct Copied {
    pub(super) file: File,
    pub(super) elf: elf::ElfEvidence,
    pub(super) transformation: Transformation,
}

pub(super) struct Delivered {
    pub(super) dependencies: Vec<Dependency>,
    pub(super) plans: Vec<PlannedDependency>,
    pub(super) transformations: Vec<Transformation>,
}

#[cfg(target_os = "linux")]
const SYSTEM_DIRECTORIES: [&str; 6] = [
    "/lib/x86_64-linux-gnu",
    "/usr/lib/x86_64-linux-gnu",
    "/lib64",
    "/usr/lib64",
    "/lib",
    "/usr/lib",
];
#[cfg(target_os = "linux")]
const MAX_DYNAMIC_BYTES: u64 = 1024 * 1024;
#[cfg(target_os = "linux")]
const MAX_SECTION_TABLE_BYTES: u64 = 4 * 1024 * 1024;
#[cfg(target_os = "linux")]
const MAX_POLICY_DEPENDENCIES: usize = 256;

#[cfg(target_os = "linux")]
fn word16(bytes: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        bytes
            .get(offset..offset + 2)
            .context("truncated policy u16")?
            .try_into()?,
    ))
}
#[cfg(target_os = "linux")]
fn word32(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .context("truncated policy u32")?
            .try_into()?,
    ))
}
#[cfg(target_os = "linux")]
fn word64(bytes: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .context("truncated policy u64")?
            .try_into()?,
    ))
}
#[cfg(target_os = "linux")]
fn end(offset: u64, bytes: u64) -> Result<u64> {
    offset
        .checked_add(bytes)
        .context("policy ELF range overflow")
}
#[cfg(target_os = "linux")]
fn overlaps(a: u64, a_bytes: u64, b: u64, b_bytes: u64) -> Result<bool> {
    Ok(a_bytes != 0 && b_bytes != 0 && a < end(b, b_bytes)? && b < end(a, a_bytes)?)
}

#[cfg(target_os = "linux")]
fn read_range(
    file: &File,
    offset: u64,
    bytes: u64,
    cap: u64,
    budget: &mut elf::MetadataBudget,
) -> Result<Vec<u8>> {
    use std::os::unix::fs::FileExt as _;
    ensure!(
        bytes <= cap && end(offset, bytes)? <= file.metadata()?.len(),
        "policy ELF range exceeds bounded file"
    );
    budget.charge(
        bytes
            .checked_mul(2)
            .and_then(|n| n.checked_add(256))
            .context("policy read budget overflow")?,
        "loader policy bounded read",
    )?;
    let mut result = vec![0; usize::try_from(bytes)?];
    file.read_exact_at(&mut result, offset)?;
    Ok(result)
}

#[cfg(target_os = "linux")]
struct Table {
    offset: u64,
    original: Vec<u8>,
    delivered: Vec<u8>,
    removed: usize,
}

#[cfg(target_os = "linux")]
fn compact_dynamic(original: &[u8], budget: &mut elf::MetadataBudget) -> Result<(Vec<u8>, usize)> {
    ensure!(
        !original.is_empty()
            && original.len() % 16 == 0
            && original.len() as u64 <= MAX_DYNAMIC_BYTES,
        "policy dynamic size differs"
    );
    budget.charge(
        original.len() as u64 * 2 + 256,
        "loader policy compacted table",
    )?;
    let mut delivered = vec![0; original.len()];
    let mut output = 0;
    let mut removed = 0;
    let mut terminated = false;
    for (index, entry) in original.chunks_exact(16).enumerate() {
        let tag = word64(entry, 0)?;
        if tag == 0 {
            // A nonzero tail cannot be silently reinterpreted as padding. Keeping
            // one terminator and the remaining zero slots preserves DT_NULL.
            ensure!(
                original[index * 16..].iter().all(|byte| *byte == 0),
                "policy dynamic terminator/tail is not zero padding"
            );
            terminated = true;
            break;
        }
        ensure!(
            !matches!(
                tag,
                0x6fff_fdfd | 0x6fff_fefb | 0x6fff_fefc | 0x7fff_fffd | 0x7fff_ffff
            ),
            "loader policy rejects positional, audit, auxiliary or filter dynamic tag {tag:#x}"
        );
        if matches!(tag, 15 | 29) {
            removed += 1;
        } else {
            delivered[output..output + 16].copy_from_slice(entry);
            output += 16;
        }
    }
    ensure!(terminated, "policy dynamic table lacks DT_NULL");
    Ok((delivered, removed))
}

#[cfg(target_os = "linux")]
fn table(file: &File, budget: &mut elf::MetadataBudget) -> Result<Table> {
    let before = physical(&file.metadata()?)?;
    let header = read_range(file, 0, 64, 64, budget)?;
    ensure!(
        header[..7] == *b"\x7fELF\x02\x01\x01"
            && word16(&header, 18)? == 62
            && word32(&header, 20)? == 1
            && matches!(word16(&header, 16)?, 2 | 3)
            && word16(&header, 52)? == 64
            && word16(&header, 54)? == 56,
        "unsupported policy ELF header"
    );
    let program_offset = word64(&header, 32)?;
    let program_count = word16(&header, 56)? as u64;
    ensure!(
        program_count > 0 && program_count <= 1024,
        "policy program header budget differs"
    );
    let program = read_range(file, program_offset, program_count * 56, 56 * 1024, budget)?;
    let mut dynamic = None;
    for entry in program.chunks_exact(56) {
        if word32(entry, 0)? == 2 {
            ensure!(dynamic.is_none(), "policy dynamic segment is ambiguous");
            dynamic = Some((
                word64(entry, 8)?,
                word64(entry, 32)?,
                word64(entry, 16)?,
                word64(entry, 40)?,
            ));
        }
    }
    let (offset, bytes, address, memory_bytes) =
        dynamic.context("policy dynamic segment absent")?;
    ensure!(
        bytes > 0
            && bytes % 16 == 0
            && bytes <= MAX_DYNAMIC_BYTES
            && end(offset, bytes)? <= before.bytes,
        "policy dynamic segment size differs"
    );
    ensure!(
        !overlaps(offset, bytes, 0, 64)?
            && !overlaps(offset, bytes, program_offset, program.len() as u64)?,
        "policy dynamic segment overlaps ELF headers"
    );
    let mut containing_loads = 0;
    for entry in program.chunks_exact(56) {
        let kind = word32(entry, 0)?;
        let start = word64(entry, 8)?;
        let length = word64(entry, 32)?;
        ensure!(
            end(start, length)? <= before.bytes,
            "policy program segment exceeds file"
        );
        if !overlaps(offset, bytes, start, length)? {
            continue;
        }
        match kind {
            1 => {
                let flags = word32(entry, 4)?;
                ensure!(
                    flags & 1 == 0
                        && flags & 2 != 0
                        && start <= offset
                        && end(start, length)? >= end(offset, bytes)?
                        && end(word64(entry, 16)?, offset - start)? == address
                        && length <= word64(entry, 40)?
                        && bytes <= memory_bytes
                        && end(address, memory_bytes)?
                            <= end(word64(entry, 16)?, word64(entry, 40)?)?,
                    "policy dynamic range is executable, read-only or ambiguously mapped"
                );
                containing_loads += 1;
            }
            2 | 0x6474_e552 => {} // PT_DYNAMIC and its GNU_RELRO container.
            _ => bail!("policy dynamic range overlaps another program segment {kind:#x}"),
        }
    }
    ensure!(
        containing_loads == 1,
        "policy dynamic range lacks one writable non-executable load mapping"
    );
    let section_offset = word64(&header, 40)?;
    let section_count = word16(&header, 60)? as u64;
    if section_offset != 0 || section_count != 0 {
        ensure!(
            section_offset != 0 && section_count > 0 && word16(&header, 58)? == 64,
            "extended/ambiguous policy section table"
        );
        let sections = read_range(
            file,
            section_offset,
            section_count * 64,
            MAX_SECTION_TABLE_BYTES,
            budget,
        )?;
        ensure!(
            !overlaps(offset, bytes, section_offset, sections.len() as u64)?,
            "policy dynamic range overlaps section headers"
        );
        let mut dynamic_sections = 0;
        for entry in sections.chunks_exact(64) {
            let kind = word32(entry, 4)?;
            let start = word64(entry, 24)?;
            let length = word64(entry, 32)?;
            if kind == 0 || kind == 8 {
                continue;
            } // NULL and NOBITS have no file body.
            ensure!(
                end(start, length)? <= before.bytes,
                "policy section exceeds file"
            );
            if overlaps(offset, bytes, start, length)? {
                ensure!(
                    kind == 6 && start == offset && length == bytes,
                    "policy dynamic range overlaps another section"
                );
                dynamic_sections += 1;
            }
        }
        ensure!(
            dynamic_sections == 1,
            "policy dynamic section mapping differs"
        );
    }
    let original = read_range(file, offset, bytes, MAX_DYNAMIC_BYTES, budget)?;
    let (delivered, removed) = compact_dynamic(&original, budget)?;
    ensure!(
        before == physical(&file.metadata()?)?,
        "policy table read full13 inverse differs"
    );
    Ok(Table {
        offset,
        original,
        delivered,
        removed,
    })
}

#[cfg(target_os = "linux")]
fn validate_soname(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 256
            && !matches!(name, "." | "..")
            && !name.contains('/')
            && !name.chars().any(char::is_control),
        "loader policy requires a bounded SONAME basename"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn system_library(
    name: &str,
    budget: &mut elf::MetadataBudget,
) -> Result<(PathBuf, PathBuf, File)> {
    validate_soname(name)?;
    for directory in SYSTEM_DIRECTORIES {
        let path = Path::new(directory).join(name);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("observe policy library {}", path.display()));
            }
            Ok(_) => {}
        }
        let resolved = resolve_noatime_with_budget(&path, budget)?;
        let file = open_noatime(&resolved, false)?;
        // The fixed system search route cannot be redirected through a directory
        // writable by an unprivileged actor. Same-UID/root interference is not a
        // claimed adversarial guarantee; retained FDs and final inverses still bind bytes.
        let mut ancestor = PathBuf::from("/");
        for component in resolved.components().skip(1) {
            ancestor.push(component);
            let metadata = fs::symlink_metadata(&ancestor)?;
            let identity = physical(&metadata)?;
            ensure!(
                identity.uid == 0
                    && identity.mode & 0o022 == 0
                    && !metadata.file_type().is_symlink(),
                "untrusted policy system library path {}",
                ancestor.display()
            );
        }
        ensure!(
            file.metadata()?.is_file(),
            "policy system library is not regular"
        );
        return Ok((path, resolved, file));
    }
    bail!(
        "loader policy cannot resolve {name} in explicit system directories {:?}; embedded build paths are not imported",
        SYSTEM_DIRECTORIES
    )
}

#[cfg(target_os = "linux")]
fn enqueue_dependencies<'a, T>(
    queue: &mut BTreeSet<String>,
    plans: &BTreeMap<String, T>,
    current: Option<&str>,
    needed: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    ensure!(
        queue.iter().all(|name| !plans.contains_key(name)),
        "policy pending and planned dependencies overlap"
    );
    let known = plans.len()
        + queue.len()
        + usize::from(
            current.is_some_and(|name| !plans.contains_key(name) && !queue.contains(name)),
        );
    ensure!(
        known <= MAX_POLICY_DEPENDENCIES,
        "policy dependency count exhausted"
    );
    // Borrow names during the preflight: neither an oversized initial list nor a
    // transitive batch can allocate an excess owned name or partially grow the queue.
    let mut additions = BTreeSet::new();
    for name in needed {
        validate_soname(name)?;
        if current == Some(name)
            || plans.contains_key(name)
            || queue.contains(name)
            || additions.contains(name)
        {
            continue;
        }
        ensure!(
            known + additions.len() < MAX_POLICY_DEPENDENCIES,
            "policy dependency count exhausted"
        );
        additions.insert(name);
    }
    for name in additions {
        queue.insert(name.to_owned());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn dependency_closure<T>(
    initial: &[String],
    budget: &mut elf::MetadataBudget,
    mut inspect: impl FnMut(&str, &mut elf::MetadataBudget) -> Result<T>,
    needed: impl Fn(&T) -> &[String],
) -> Result<Vec<T>> {
    // Sixteen slots per admitted name conservatively include B-tree slack for
    // plans, the queue and the borrowed preflight set, plus the returned vector.
    // The 4 KiB allowance covers two bounded SONAME byte allocations and node
    // headers/edges. Inspected ELF evidence and paths retain their separate charges.
    budget.charge(
        MAX_POLICY_DEPENDENCIES as u64
            * (16
                * (std::mem::size_of::<T>()
                    + std::mem::size_of::<String>()
                    + std::mem::size_of::<&str>()) as u64
                + 4096),
        "policy static closure, pending queue and borrowed preflight",
    )?;
    let mut queue = BTreeSet::new();
    let mut plans = BTreeMap::new();
    enqueue_dependencies(
        &mut queue,
        &plans,
        None,
        std::iter::once("ld-linux-x86-64.so.2").chain(initial.iter().map(String::as_str)),
    )?;
    while let Some(soname) = queue.pop_first() {
        let plan = inspect(&soname, budget)?;
        enqueue_dependencies(
            &mut queue,
            &plans,
            Some(&soname),
            needed(&plan).iter().map(String::as_str),
        )?;
        plans.insert(soname, plan);
    }
    Ok(plans.into_values().collect())
}

#[cfg(target_os = "linux")]
pub(super) fn plan_dependencies(
    binary: &elf::ElfEvidence,
    budget: &mut elf::MetadataBudget,
) -> Result<Vec<PlannedDependency>> {
    let interpreter = binary
        .interpreter
        .as_deref()
        .context("policy interpreter absent")?;
    ensure!(
        matches!(
            interpreter,
            "/lib64/ld-linux-x86-64.so.2" | "/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2"
        ),
        "unsupported policy platform loader"
    );
    dependency_closure(
        &binary.needed,
        budget,
        |soname, budget| {
            let (loader_path, resolved_path, file) = system_library(soname, budget)?;
            let (digest, identity) = descriptor_digest(&file, MAX_BINARY_BYTES)?;
            let facts = elf::inspect_file_with_budget(file.try_clone()?, budget)?;
            ensure!(
                facts.soname.as_deref() == Some(soname),
                "policy resolved SONAME differs for {}",
                resolved_path.display()
            );
            if host_family(soname) {
                ensure!(
                    facts.rpath.is_none() && facts.runpath.is_none(),
                    "unmodified host dependency {} contains RPATH {:?} or RUNPATH {:?}",
                    resolved_path.display(),
                    facts.rpath,
                    facts.runpath
                );
            }
            table(&file, budget)
                .with_context(|| format!("admit policy dependency {}", resolved_path.display()))?;
            ensure!(
                descriptor_digest(&file, MAX_BINARY_BYTES)? == (digest.clone(), identity.clone())
                    && identity == physical(&fs::symlink_metadata(&resolved_path)?)?,
                "policy source dependency inverse differs"
            );
            if soname == "ld-linux-x86-64.so.2" {
                ensure!(
                    resolved_path == resolve_noatime_with_budget(Path::new(interpreter), budget)?,
                    "policy loader dependency differs from interpreter"
                );
            }
            let preliminary_elf = elf::SharedEvidence::new(facts, budget)?;
            Ok(PlannedDependency {
                file,
                soname: soname.to_owned(),
                loader_path,
                resolved_path,
                digest,
                physical: identity,
                preliminary_elf,
            })
        },
        |plan| &plan.preliminary_elf.needed,
    )
}

#[cfg(not(target_os = "linux"))]
pub(super) fn plan_dependencies(
    _: &elf::ElfEvidence,
    _: &mut elf::MetadataBudget,
) -> Result<Vec<PlannedDependency>> {
    bail!("explicit ELF loader policy requires GNU/Linux x86-64")
}

#[cfg(target_os = "linux")]
pub(super) fn copy(
    source: &File,
    source_path: &Path,
    destination: &Path,
    expected: &FileDigest,
    facts: &elf::ElfEvidence,
    budget: &mut elf::MetadataBudget,
) -> Result<Copied> {
    use std::os::unix::fs::{FileExt as _, OpenOptionsExt as _, PermissionsExt as _};
    let (original_digest, original_physical13) = descriptor_digest(source, MAX_BINARY_BYTES)?;
    ensure!(
        original_digest == *expected
            && original_physical13 == physical(&fs::symlink_metadata(source_path)?)?,
        "policy copy original binding differs"
    );
    let table = table(source, budget).with_context(|| {
        format!(
            "admit owned-copy transformation for {}",
            source_path.display()
        )
    })?;
    let mut output = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NOATIME | libc::O_CLOEXEC)
        .open(destination)
        .context("policy destination must be new")?;
    // All writes, including dynamic-table compaction and chmod, use this creator
    // FD. No existing/source pathname is ever opened for writing.
    budget.charge(512 * 1024 + 512, "policy copy/unchanged-range buffers")?;
    budget.charge(
        source_path.as_os_str().len() as u64 * 2
            + std::mem::size_of::<Transformation>() as u64 * 2
            + 1024,
        "policy transformation provenance",
    )?;
    let mut input = source.try_clone()?;
    input.seek(SeekFrom::Start(0))?;
    let mut buffer = vec![0; 256 * 1024];
    let mut count = 0_u64;
    let mut hash = Sha256::new();
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        count = count
            .checked_add(read as u64)
            .context("policy copy size overflow")?;
        ensure!(count <= expected.bytes, "policy copy source grew");
        hash.update(&buffer[..read]);
        output.write_all(&buffer[..read])?;
    }
    ensure!(
        count == expected.bytes
            && format!("{:x}", hash.finalize()) == expected.sha256
            && original_physical13 == physical(&source.metadata()?)?,
        "policy copy input inverse differs"
    );
    output.sync_all()?;
    let copied_before_transform_digest = descriptor_digest(&output, MAX_BINARY_BYTES)?.0;
    ensure!(
        copied_before_transform_digest == *expected,
        "policy pre-transform copy digest differs"
    );
    output.write_all_at(&table.delivered, table.offset)?;
    output.sync_all()?;
    let mut comparison = vec![0; buffer.len()];
    let mut offset = 0_u64;
    while offset < expected.bytes {
        let count = usize::try_from((expected.bytes - offset).min(buffer.len() as u64))?;
        source.read_exact_at(&mut buffer[..count], offset)?;
        output.read_exact_at(&mut comparison[..count], offset)?;
        let prefix = usize::try_from(table.offset.saturating_sub(offset).min(count as u64))?;
        let dynamic_end = end(table.offset, table.delivered.len() as u64)?;
        let suffix = usize::try_from(dynamic_end.saturating_sub(offset).min(count as u64))?;
        ensure!(
            buffer[..prefix] == comparison[..prefix]
                && buffer[suffix..count] == comparison[suffix..count],
            "policy modified a byte outside its checked dynamic transformation"
        );
        let overlap_start = offset.max(table.offset);
        let overlap_end = end(offset, count as u64)?.min(dynamic_end);
        if overlap_start < overlap_end {
            let from = usize::try_from(overlap_start - offset)?;
            let to = usize::try_from(overlap_end - offset)?;
            let table_from = usize::try_from(overlap_start - table.offset)?;
            let table_to = usize::try_from(overlap_end - table.offset)?;
            ensure!(
                comparison[from..to] == table.delivered[table_from..table_to],
                "policy actual dynamic table differs from compacted entries"
            );
        }
        offset += count as u64;
    }
    output.set_permissions(fs::Permissions::from_mode(0o755))?;
    let (delivered_digest, identity) = descriptor_digest(&output, MAX_BINARY_BYTES)?;
    let mut delivered = elf::inspect_file_with_budget(output.try_clone()?, budget)?;
    ensure!(
        delivered.rpath.is_none() && delivered.runpath.is_none(),
        "policy left embedded search tags"
    );
    for value in [&facts.rpath, &facts.runpath].into_iter().flatten() {
        budget.charge(value.len() as u64 * 2 + 64, "policy removed-path receipt")?;
    }
    delivered.rpath = facts.rpath.clone();
    delivered.runpath = facts.runpath.clone();
    ensure!(
        delivered == *facts,
        "policy changed ELF metadata beyond RPATH/RUNPATH"
    );
    let removed_rpath = delivered.rpath.take();
    let removed_runpath = delivered.runpath.take();
    ensure!(
        descriptor_digest(&output, MAX_BINARY_BYTES)?
            == (delivered_digest.clone(), identity.clone())
            && identity == physical(&fs::symlink_metadata(destination)?)?
            && descriptor_digest(source, MAX_BINARY_BYTES)?
                == (original_digest.clone(), original_physical13.clone())
            && original_physical13 == physical(&fs::symlink_metadata(source_path)?)?,
        "policy copy complete original/delivered inverses differ"
    );
    // Release every writable open-file description before loader introspection.
    // Reopening this newly owned pathname is authenticated against its creator FD;
    // it is never used for writing and cannot substitute borrowed bytes unnoticed.
    let read_only = open_noatime(destination, false)?;
    ensure!(
        descriptor_digest(&read_only, MAX_BINARY_BYTES)?
            == (delivered_digest.clone(), identity.clone())
            && identity == physical(&output.metadata()?)?
            && identity == physical(&fs::symlink_metadata(destination)?)?,
        "policy read-only delivered descriptor differs from creator"
    );
    drop(output);
    Ok(Copied {
        file: read_only,
        elf: delivered,
        transformation: Transformation {
            policy: LoaderPolicy::StripEmbeddedSearchPaths,
            source_path: source_path.to_owned(),
            original_digest,
            original_physical13,
            copied_before_transform_digest,
            delivered_digest,
            dynamic_file_offset: table.offset,
            dynamic_bytes: table.original.len() as u64,
            original_dynamic_sha256: sha256(&table.original),
            delivered_dynamic_sha256: sha256(&table.delivered),
            removed_rpath,
            removed_runpath,
            removed_entries: table.removed,
            all_other_file_bytes_equal: true,
            all_other_elf_metadata_equal: true,
        },
    })
}

#[cfg(not(target_os = "linux"))]
pub(super) fn copy(
    _: &File,
    _: &Path,
    _: &Path,
    _: &FileDigest,
    _: &elf::ElfEvidence,
    _: &mut elf::MetadataBudget,
) -> Result<Copied> {
    bail!("explicit ELF loader policy requires GNU/Linux x86-64")
}

pub(super) fn deliver(
    planned: &[PlannedDependency],
    directory: &Path,
    budget: &mut elf::MetadataBudget,
) -> Result<Delivered> {
    budget.charge(
        planned.len() as u64
            * (std::mem::size_of::<Dependency>()
                + std::mem::size_of::<PlannedDependency>()
                + std::mem::size_of::<Transformation>()
                + 2048) as u64
            * 2,
        "policy delivered plans/receipts",
    )?;
    let mut dependencies = Vec::new();
    let mut delivered = Vec::new();
    let mut transformations = Vec::new();
    dependencies.try_reserve_exact(planned.len())?;
    delivered.try_reserve_exact(planned.len())?;
    transformations.try_reserve_exact(planned.len())?;
    for plan in planned {
        ensure!(
            descriptor_digest(&plan.file, MAX_BINARY_BYTES)?
                == (plan.digest.clone(), plan.physical.clone()),
            "policy dependency changed since admission"
        );
        let (file, delivered_elf, copied_identity) = if host_family(&plan.soname) {
            ensure!(
                plan.preliminary_elf.rpath.is_none() && plan.preliminary_elf.runpath.is_none(),
                "policy host dependency has embedded search paths"
            );
            (plan.file.try_clone()?, plan.preliminary_elf.clone(), None)
        } else {
            let path = directory.join(&plan.soname);
            let copied = copy(
                &plan.file,
                &plan.resolved_path,
                &path,
                &plan.digest,
                &plan.preliminary_elf,
                budget,
            )?;
            let identity = physical(&copied.file.metadata()?)?;
            let evidence = elf::SharedEvidence::new(copied.elf, budget)?;
            transformations.push(copied.transformation);
            (copied.file, evidence, Some(identity))
        };
        let (digest, identity) = descriptor_digest(&file, MAX_BINARY_BYTES)?;
        dependencies.push(Dependency {
            soname: plan.soname.clone(),
            loader_path: plan.loader_path.clone(),
            resolved_path: plan.resolved_path.clone(),
            digest: plan.digest.clone(),
            physical: plan.physical.clone(),
            delivery: if host_family(&plan.soname) {
                "host"
            } else {
                "bundled"
            },
            elf: plan.preliminary_elf.clone(),
            copied_identity,
        });
        delivered.push(PlannedDependency {
            file,
            soname: plan.soname.clone(),
            loader_path: plan.loader_path.clone(),
            resolved_path: plan.resolved_path.clone(),
            digest,
            physical: identity,
            preliminary_elf: delivered_elf,
        });
    }
    Ok(Delivered {
        dependencies,
        plans: delivered,
        transformations,
    })
}

pub(super) fn sources_after_loader(
    planned: &[PlannedDependency],
    probe: &BoundLoaderEvidence,
) -> Result<()> {
    for plan in planned {
        let (digest, after) = descriptor_digest(&plan.file, MAX_BINARY_BYTES)?;
        ensure!(
            digest == plan.digest
                && after == physical(&fs::symlink_metadata(&plan.resolved_path)?)?,
            "policy original dependency byte/path inverse differs"
        );
        if host_family(&plan.soname) {
            let observation = probe
                .dependencies
                .iter()
                .find(|entry| entry.soname == plan.soname)
                .context("policy host loader observation absent")?;
            ensure!(
                observation.before_loader == plan.physical && observation.after_loader == after,
                "policy observed host loader transition differs"
            );
            require_loader_inverse(&plan.physical, &after)?;
        } else {
            ensure!(
                after == plan.physical,
                "policy original bundled dependency full13 changed"
            );
        }
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn dynamic(entries: &[(u64, u64)]) -> Vec<u8> {
        entries
            .iter()
            .flat_map(|(tag, value)| tag.to_le_bytes().into_iter().chain(value.to_le_bytes()))
            .collect()
    }

    fn fixture() -> Vec<u8> {
        let mut bytes = vec![0_u8; 1024];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&3_u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&62_u16.to_le_bytes());
        bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
        bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&2_u16.to_le_bytes());
        for (offset, kind, start, size) in [(64, 1_u32, 0_u64, 1024_u64), (120, 2, 256, 112)] {
            bytes[offset..offset + 4].copy_from_slice(&kind.to_le_bytes());
            bytes[offset + 4..offset + 8].copy_from_slice(&6_u32.to_le_bytes());
            bytes[offset + 8..offset + 16].copy_from_slice(&start.to_le_bytes());
            bytes[offset + 16..offset + 24].copy_from_slice(&(0x400000 + start).to_le_bytes());
            bytes[offset + 32..offset + 40].copy_from_slice(&size.to_le_bytes());
            bytes[offset + 40..offset + 48].copy_from_slice(&size.to_le_bytes());
        }
        let strings = b"\0/private/a\0libfixture.so\0/private/b\0";
        bytes[512..512 + strings.len()].copy_from_slice(strings);
        let table = dynamic(&[
            (5, 0x400200),
            (10, strings.len() as u64),
            (15, 1),
            (1, 12),
            (29, 26),
            (14, 12),
            (0, 0),
        ]);
        bytes[256..256 + table.len()].copy_from_slice(&table);
        bytes[768..832].fill(0x91); // Distinct bytes outside PT_DYNAMIC remain exact.
        bytes
    }

    #[test]
    fn compaction_preserves_every_non_path_entry_order_and_null_padding() {
        let original = dynamic(&[
            (5, 700),
            (15, 3),
            (1, 9),
            (29, 11),
            (30, 8),
            (10, 32),
            (0, 0),
            (0, 0),
        ]);
        let before = original.clone();
        let (delivered, removed) =
            compact_dynamic(&original, &mut elf::MetadataBudget::new()).unwrap();
        assert_eq!(removed, 2);
        assert_eq!(
            delivered,
            dynamic(&[
                (5, 700),
                (1, 9),
                (30, 8),
                (10, 32),
                (0, 0),
                (0, 0),
                (0, 0),
                (0, 0)
            ])
        );
        assert_eq!(original, before);
        assert_eq!(delivered.len(), original.len());
    }

    #[test]
    fn malformed_dynamic_tables_and_implicit_loader_hooks_are_refused_without_mutation() {
        for entries in [
            dynamic(&[(15, 1)]),
            dynamic(&[(0, 0), (1, 3)]),
            dynamic(&[(0x6fff_fefc, 1), (0, 0)]),
            dynamic(&[(0x6fff_fefb, 1), (0, 0)]),
            dynamic(&[(0x6fff_fdfd, 1), (0, 0)]),
            dynamic(&[(0x7fff_fffd, 1), (0, 0)]),
            dynamic(&[(0x7fff_ffff, 1), (0, 0)]),
            vec![0; 17],
        ] {
            let before = entries.clone();
            assert!(compact_dynamic(&entries, &mut elf::MetadataBudget::new()).is_err());
            assert_eq!(entries, before);
        }
    }

    #[test]
    fn executable_read_only_header_and_virtual_mapping_overlaps_are_refused() {
        let temporary = tempfile::tempdir().unwrap();
        for (index, (offset, value)) in [(68, 7_u64), (68, 4), (128, 64), (136, 0x500100)]
            .into_iter()
            .enumerate()
        {
            let mut bytes = fixture();
            if offset == 68 {
                bytes[offset..offset + 4].copy_from_slice(&(value as u32).to_le_bytes());
            } else {
                bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            }
            let path = temporary.path().join(index.to_string());
            create_file(&path, &bytes).unwrap();
            let input = open_noatime(&path, false).unwrap();
            let before = descriptor_digest(&input, MAX_BINARY_BYTES).unwrap();
            assert!(table(&input, &mut elf::MetadataBudget::new()).is_err());
            assert_eq!(descriptor_digest(&input, MAX_BINARY_BYTES).unwrap(), before);
        }
    }

    #[test]
    fn section_aliases_and_oversized_dynamic_segments_are_rejected_before_copying() {
        let temporary = tempfile::tempdir().unwrap();
        let valid = temporary.path().join("valid-metadata");
        create_file(&valid, &fixture()).unwrap();
        let facts = elf::inspect(&valid).unwrap();
        for index in 0..2 {
            let mut bytes = fixture();
            if index == 0 {
                bytes[40..48].copy_from_slice(&896_u64.to_le_bytes());
                bytes[58..60].copy_from_slice(&64_u16.to_le_bytes());
                bytes[60..62].copy_from_slice(&2_u16.to_le_bytes());
                bytes[964..968].copy_from_slice(&1_u32.to_le_bytes());
                bytes[984..992].copy_from_slice(&256_u64.to_le_bytes());
                bytes[992..1000].copy_from_slice(&112_u64.to_le_bytes());
            } else {
                bytes[152..160].copy_from_slice(&(MAX_DYNAMIC_BYTES + 16).to_le_bytes());
            }
            let source = temporary.path().join(format!("source-{index}"));
            let destination = temporary.path().join(format!("destination-{index}"));
            create_file(&source, &bytes).unwrap();
            let input = open_noatime(&source, false).unwrap();
            let before = descriptor_digest(&input, MAX_BINARY_BYTES).unwrap();
            assert!(table(&input, &mut elf::MetadataBudget::new()).is_err());
            assert!(
                copy(
                    &input,
                    &source,
                    &destination,
                    &before.0,
                    &facts,
                    &mut elf::MetadataBudget::new()
                )
                .is_err()
            );
            assert!(!destination.exists());
            assert_eq!(descriptor_digest(&input, MAX_BINARY_BYTES).unwrap(), before);
        }
    }

    #[test]
    fn owned_copy_strips_paths_and_binds_all_original_bytes_full13_and_delivered_metadata() {
        use std::os::fd::AsRawFd as _;
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("built-library");
        let destination = temporary.path().join("owned-library");
        create_file(&source, &fixture()).unwrap();
        let input = open_noatime(&source, false).unwrap();
        let before = descriptor_digest(&input, MAX_BINARY_BYTES).unwrap();
        let facts = elf::inspect_file_with_budget(
            input.try_clone().unwrap(),
            &mut elf::MetadataBudget::new(),
        )
        .unwrap();
        let copied = copy(
            &input,
            &source,
            &destination,
            &before.0,
            &facts,
            &mut elf::MetadataBudget::new(),
        )
        .unwrap();
        assert_eq!(descriptor_digest(&input, MAX_BINARY_BYTES).unwrap(), before);
        assert_eq!(copied.transformation.original_digest, before.0);
        assert_eq!(copied.transformation.original_physical13, before.1);
        assert_eq!(
            copied.transformation.copied_before_transform_digest,
            before.0
        );
        assert_ne!(copied.transformation.delivered_digest, before.0);
        assert_eq!(
            copied.transformation.removed_rpath.as_deref(),
            Some("/private/a")
        );
        assert_eq!(
            copied.transformation.removed_runpath.as_deref(),
            Some("/private/b")
        );
        assert_eq!(copied.transformation.removed_entries, 2);
        assert!(copied.elf.rpath.is_none() && copied.elf.runpath.is_none());
        assert_eq!(copied.elf.needed, facts.needed);
        assert_eq!(copied.elf.soname, facts.soname);
        // SAFETY: F_GETFL queries this retained live descriptor without mutation.
        let flags = unsafe { libc::fcntl(copied.file.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(flags & libc::O_ACCMODE, libc::O_RDONLY);
        assert!(
            copied.transformation.all_other_file_bytes_equal
                && copied.transformation.all_other_elf_metadata_equal
        );
    }

    #[test]
    fn existing_destination_and_exhausted_policy_budget_cannot_modify_sources_or_outputs() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let existing = temporary.path().join("existing");
        let absent = temporary.path().join("absent");
        create_file(&source, &fixture()).unwrap();
        create_file(&existing, b"foreign bytes").unwrap();
        let input = open_noatime(&source, false).unwrap();
        let before = descriptor_digest(&input, MAX_BINARY_BYTES).unwrap();
        let foreign_before = digest_file(&existing, 1024).unwrap();
        let facts = elf::inspect_file_with_budget(
            input.try_clone().unwrap(),
            &mut elf::MetadataBudget::new(),
        )
        .unwrap();
        assert!(
            copy(
                &input,
                &source,
                &existing,
                &before.0,
                &facts,
                &mut elf::MetadataBudget::new()
            )
            .is_err()
        );
        assert_eq!(digest_file(&existing, 1024).unwrap(), foreign_before);
        assert!(
            copy(
                &input,
                &source,
                &absent,
                &before.0,
                &facts,
                &mut elf::MetadataBudget::limited(0)
            )
            .is_err()
        );
        assert!(!absent.exists());
        assert_eq!(descriptor_digest(&input, MAX_BINARY_BYTES).unwrap(), before);
    }

    #[test]
    fn bundled_delivery_binds_original_and_transformed_digests_and_never_rewrites_host_libraries() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("built-library");
        let bundled = temporary.path().join("bundled");
        let host = temporary.path().join("host");
        create_file(&source, &fixture()).unwrap();
        fs::create_dir(&bundled).unwrap();
        fs::create_dir(&host).unwrap();
        let input = open_noatime(&source, false).unwrap();
        let before = descriptor_digest(&input, MAX_BINARY_BYTES).unwrap();
        let mut budget = elf::MetadataBudget::new();
        let facts = elf::inspect_file_with_budget(input.try_clone().unwrap(), &mut budget).unwrap();
        let evidence = elf::SharedEvidence::new(facts, &mut budget).unwrap();
        let plan = |soname: &str| PlannedDependency {
            file: input.try_clone().unwrap(),
            soname: soname.to_owned(),
            loader_path: source.clone(),
            resolved_path: source.clone(),
            digest: before.0.clone(),
            physical: before.1.clone(),
            preliminary_elf: evidence.clone(),
        };
        let delivered = deliver(&[plan("libfixture.so")], &bundled, &mut budget).unwrap();
        assert_eq!(delivered.dependencies[0].digest, before.0);
        assert_eq!(delivered.transformations.len(), 1);
        assert_eq!(delivered.transformations[0].original_digest, before.0);
        assert_ne!(delivered.plans[0].digest, before.0);
        assert_eq!(
            delivered.plans[0].digest,
            delivered.transformations[0].delivered_digest
        );
        assert!(
            delivered.plans[0].preliminary_elf.rpath.is_none()
                && delivered.plans[0].preliminary_elf.runpath.is_none()
        );
        assert!(deliver(&[plan("libc.so.6")], &host, &mut budget).is_err());
        assert_eq!(fs::read_dir(&host).unwrap().count(), 0);
        assert_eq!(descriptor_digest(&input, MAX_BINARY_BYTES).unwrap(), before);
    }

    #[test]
    fn default_policy_still_refuses_paths_with_named_elf_and_actual_tag_diagnostics() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("built-koda");
        create_file(&source, &fixture()).unwrap();
        let facts = elf::inspect(&source).unwrap();
        let error = require_default_loader_policy(&source, &facts)
            .unwrap_err()
            .to_string();
        assert!(error.contains("requires an explicit loader policy"));
        assert!(error.contains(source.to_str().unwrap()));
        assert!(error.contains("/private/a") && error.contains("/private/b"));
        for unsafe_name in [
            "",
            ".",
            "..",
            "../private/lib.so",
            "/tmp/lib.so",
            "lib\n.so",
        ] {
            assert!(validate_soname(unsafe_name).is_err());
        }
        assert!(validate_soname("libfixture.so.1").is_ok());
    }

    #[test]
    fn oversized_initial_closure_is_rejected_before_resolver_or_digest_work() {
        use std::cell::Cell;
        // The mandatory interpreter counts toward the same closure limit.
        for count in [MAX_POLICY_DEPENDENCIES, MAX_POLICY_DEPENDENCIES + 1] {
            let initial = (0..count)
                .map(|index| format!("lib{index:03}.so"))
                .collect::<Vec<_>>();
            let resolver_calls = Cell::new(0);
            let digest_calls = Cell::new(0);
            let result = dependency_closure(
                &initial,
                &mut elf::MetadataBudget::new(),
                |_, _| {
                    resolver_calls.set(resolver_calls.get() + 1);
                    digest_calls.set(digest_calls.get() + 1);
                    Ok(Vec::<String>::new())
                },
                |needed| needed,
            );
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("dependency count exhausted")
            );
            assert_eq!(resolver_calls.get(), 0);
            assert_eq!(digest_calls.get(), 0);
        }
    }

    #[test]
    fn transitive_overflow_counts_processed_pending_and_current_before_excess_resolution() {
        use std::cell::RefCell;
        let initial = vec!["libexisting.so".to_owned()];
        let resolved = RefCell::new(Vec::<String>::new());
        let result = dependency_closure(
            &initial,
            &mut elf::MetadataBudget::new(),
            |soname, _| {
                resolved.borrow_mut().push(soname.to_owned());
                if soname == "ld-linux-x86-64.so.2" {
                    // One current loader + one pending initial library + 254 new
                    // names fills the union, even after the loader becomes planned.
                    Ok((0..MAX_POLICY_DEPENDENCIES - 2)
                        .map(|index| format!("lib{index:03}.so"))
                        .collect::<Vec<_>>())
                } else {
                    Ok(vec!["libexcess.so".to_owned()])
                }
            },
            |needed| needed,
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("dependency count exhausted")
        );
        assert_eq!(
            *resolved.borrow(),
            vec!["ld-linux-x86-64.so.2", "lib000.so"]
        );
        assert!(!resolved.borrow().iter().any(|name| name == "libexcess.so"));
    }

    #[test]
    fn rejected_transitive_batch_leaves_pending_names_unchanged() {
        let plans = (0..MAX_POLICY_DEPENDENCIES - 3)
            .map(|index| (format!("libplanned{index:03}.so"), ()))
            .collect::<BTreeMap<_, _>>();
        let mut queue = BTreeSet::from(["libpending.so".to_owned()]);
        let before = queue.clone();
        let result = enqueue_dependencies(
            &mut queue,
            &plans,
            Some("libcurrent.so"),
            ["libfirst.so", "libexcess.so"],
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("dependency count exhausted")
        );
        assert_eq!(queue, before);
        assert!(!queue.contains("libfirst.so") && !queue.contains("libexcess.so"));
    }

    #[test]
    fn exact_limit_closure_accepts_duplicates_and_cycles_without_duplicate_resolution() {
        use std::cell::RefCell;
        let mut initial = (0..MAX_POLICY_DEPENDENCIES - 1)
            .map(|index| format!("lib{index:03}.so"))
            .collect::<Vec<_>>();
        initial.extend(["lib000.so".to_owned(), "ld-linux-x86-64.so.2".to_owned()]);
        let resolved = RefCell::new(BTreeSet::new());
        let plans = dependency_closure(
            &initial,
            &mut elf::MetadataBudget::new(),
            |soname, _| {
                assert!(resolved.borrow_mut().insert(soname.to_owned()));
                Ok(vec![
                    soname.to_owned(),
                    "ld-linux-x86-64.so.2".to_owned(),
                    "lib000.so".to_owned(),
                ])
            },
            |needed| needed,
        )
        .unwrap();
        assert_eq!(plans.len(), MAX_POLICY_DEPENDENCIES);
        assert_eq!(resolved.borrow().len(), MAX_POLICY_DEPENDENCIES);
        assert!(resolved.borrow().contains("ld-linux-x86-64.so.2"));
        assert!(resolved.borrow().contains("lib254.so"));
    }
}
