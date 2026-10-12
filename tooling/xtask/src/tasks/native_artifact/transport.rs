//! Strict, bounded transport for the official uploader's small ZIP files.
//! Accepts stored/deflated single-disk ZIP32 only. Unsupported formats fail as
//! explicit blockers; no network/file-reference conversion is attempted.

use anyhow::{Context as _, Result, ensure};
use clap::{Args, ValueEnum};
use flate2::read::DeflateDecoder;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

use super::{
    FileDigest, MAX_METADATA_BYTES, MAX_ZIP_BYTES, ORIGINAL_FLOOR, PIECE_BYTES, digest_file,
    filesystem_space, open_noatime, physical, require_hex, write_json,
};

#[derive(Clone, Copy, ValueEnum)]
pub(super) enum ZipKind {
    Metadata,
    Piece,
}

#[derive(Args)]
pub struct UnpackZipArgs {
    #[arg(long)]
    zip: PathBuf,
    /// Actual GitHub API artifact ZIP digest, already bound by Root to run/id/name.
    #[arg(long)]
    zip_sha256: String,
    #[arg(long)]
    zip_bytes: u64,
    #[arg(long, value_enum)]
    kind: ZipKind,
    #[arg(long)]
    piece_index: Option<usize>,
    #[arg(long)]
    output_dir: PathBuf,
    #[arg(long)]
    receipt: PathBuf,
    #[arg(long)]
    original_floor_bytes: u64,
}

const MAX_METADATA_UNPACKED: u64 = 32 * 1024 * 1024;
const METADATA_MEMBERS: &[&str] = &[
    "artifact-metadata.json",
    "payload-manifest.json",
    "piece-manifest.json",
    "runtime/evidence/native-build.json",
    "runtime/evidence/source-manifest.json",
];

struct ZipMember {
    name: String,
    method: u16,
    crc: u32,
    compressed: u64,
    unpacked: u64,
    offset: u64,
    data: u64,
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        bytes
            .get(offset..offset + 2)
            .context("truncated ZIP u16")?
            .try_into()?,
    ))
}
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .context("truncated ZIP u32")?
            .try_into()?,
    ))
}
fn at(file: &mut File, file_bytes: u64, offset: u64, length: u64, maximum: u64) -> Result<Vec<u8>> {
    ensure!(
        length <= maximum
            && offset
                .checked_add(length)
                .is_some_and(|end| end <= file_bytes),
        "ZIP range exceeds its budget"
    );
    let mut bytes = vec![0; usize::try_from(length)?];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn allowed_members(kind: ZipKind, index: Option<usize>) -> Result<BTreeSet<String>> {
    Ok(match kind {
        ZipKind::Metadata => {
            ensure!(index.is_none(), "metadata ZIP cannot select a piece index");
            METADATA_MEMBERS
                .iter()
                .map(|name| (*name).to_owned())
                .collect()
        }
        ZipKind::Piece => {
            let index = index.context("piece ZIP requires its exact index")?;
            ensure!(index < super::MAX_PIECES, "piece index exceeds its budget");
            [format!("{index:06}.part")].into_iter().collect()
        }
    })
}

fn members(file: &mut File, size: u64, allowed: &BTreeSet<String>) -> Result<Vec<ZipMember>> {
    ensure!(
        size >= 22 && size <= MAX_ZIP_BYTES,
        "ZIP size limit differs"
    );
    let tail_size = size.min(65_557);
    let tail_start = size - tail_size;
    let tail = at(file, size, tail_start, tail_size, 65_557)?;
    let end = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|offset| {
            tail[*offset..].starts_with(b"PK\x05\x06")
                && u16_at(&tail, *offset + 20)
                    .is_ok_and(|comment| *offset + 22 + comment as usize == tail.len())
        })
        .context("bounded ZIP end record absent")?;
    let header = &tail[end..];
    ensure!(
        u16_at(header, 4)? == 0
            && u16_at(header, 6)? == 0
            && u16_at(header, 8)? == u16_at(header, 10)?,
        "multidisk ZIP is unsupported"
    );
    let count = u16_at(header, 10)? as usize;
    let directory_bytes = u32_at(header, 12)? as u64;
    let directory_start = u32_at(header, 16)? as u64;
    ensure!(
        count == allowed.len()
            && count > 0
            && directory_bytes <= 64 * 1024
            && directory_start.checked_add(directory_bytes) == Some(tail_start + end as u64),
        "ZIP32 central directory/count differs; ZIP64 is unsupported"
    );
    let directory = at(file, size, directory_start, directory_bytes, 64 * 1024)?;
    let mut position = 0;
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    for _ in 0..count {
        let base = directory
            .get(position..)
            .context("central directory truncated")?;
        ensure!(
            base.starts_with(b"PK\x01\x02") && base.len() >= 46,
            "central directory signature/layout differs"
        );
        let flags = u16_at(base, 8)?;
        let method = u16_at(base, 10)?;
        ensure!(
            matches!(method, 0 | 8) && flags & !0x080e == 0 && (method == 8 || flags & 6 == 0),
            "encrypted/unsupported ZIP flags or method"
        );
        let name_bytes = u16_at(base, 28)? as usize;
        let extra = u16_at(base, 30)? as usize;
        let comment = u16_at(base, 32)? as usize;
        let length = 46_usize
            .checked_add(name_bytes)
            .and_then(|value| value.checked_add(extra))
            .and_then(|value| value.checked_add(comment))
            .context("ZIP directory length overflow")?;
        ensure!(
            length <= base.len()
                && name_bytes > 0
                && name_bytes <= 1024
                && extra <= 4096
                && comment <= 4096
                && u16_at(base, 34)? == 0,
            "ZIP central metadata budget differs"
        );
        let name = std::str::from_utf8(&base[46..46 + name_bytes])?.to_owned();
        ensure!(
            allowed.contains(&name) && seen.insert(name.clone()),
            "undeclared or duplicate ZIP member"
        );
        let attributes = u32_at(base, 38)?;
        let unix_type = (attributes >> 16) & 0o170000;
        ensure!(
            matches!(unix_type, 0 | 0o100000),
            "ZIP link/directory/device member rejected"
        );
        let mut extra_position = 46 + name_bytes;
        while extra_position < 46 + name_bytes + extra {
            let extra_header = base
                .get(extra_position..extra_position + 4)
                .context("truncated ZIP extra header")?;
            let kind = u16_at(extra_header, 0)?;
            let bytes = u16_at(extra_header, 2)? as usize;
            ensure!(kind != 1, "ZIP64 is not admitted");
            extra_position = extra_position
                .checked_add(4 + bytes)
                .context("ZIP extra overflow")?;
            ensure!(
                extra_position <= 46 + name_bytes + extra,
                "ZIP extra length differs"
            );
        }
        let compressed = u32_at(base, 20)? as u64;
        let unpacked = u32_at(base, 24)? as u64;
        let offset = u32_at(base, 42)? as u64;
        ensure!(
            compressed > 0
                && unpacked > 0
                && compressed <= MAX_ZIP_BYTES
                && unpacked <= MAX_METADATA_UNPACKED
                && offset < directory_start
                && (method != 0 || compressed == unpacked),
            "ZIP member size/type budget differs"
        );
        let local = at(file, size, offset, 30, 30)?;
        ensure!(
            local.starts_with(b"PK\x03\x04")
                && u16_at(&local, 6)? == flags
                && u16_at(&local, 8)? == method,
            "local/central ZIP header differs"
        );
        let local_name = u16_at(&local, 26)? as u64;
        let local_extra = u16_at(&local, 28)? as u64;
        ensure!(
            local_name == name_bytes as u64 && local_extra <= 4096,
            "local ZIP metadata budget differs"
        );
        let bytes = at(file, size, offset + 30, local_name, 1024)?;
        ensure!(bytes == name.as_bytes(), "local ZIP member name differs");
        if flags & 8 == 0 {
            ensure!(
                u32_at(&local, 14)? == u32_at(base, 16)?
                    && u32_at(&local, 18)? as u64 == compressed
                    && u32_at(&local, 22)? as u64 == unpacked,
                "local ZIP size/CRC differs"
            );
        }
        let data = offset
            .checked_add(30 + local_name + local_extra)
            .context("ZIP data offset overflow")?;
        ensure!(
            data.checked_add(compressed)
                .is_some_and(|end| end <= directory_start),
            "ZIP member overlaps central directory"
        );
        result.push(ZipMember {
            name,
            method,
            crc: u32_at(base, 16)?,
            compressed,
            unpacked,
            offset,
            data,
        });
        position += length;
    }
    ensure!(
        position == directory.len() && seen == *allowed,
        "ZIP central directory has extra bytes or members"
    );
    result.sort_by_key(|member| member.offset);
    for pair in result.windows(2) {
        ensure!(
            pair[0]
                .data
                .checked_add(pair[0].compressed)
                .is_some_and(|end| end <= pair[1].offset),
            "ZIP members overlap"
        );
    }
    Ok(result)
}

const fn crc_table() -> [u32; 256] {
    let mut table = [0; 256];
    let mut index = 0;
    while index < 256 {
        let mut crc = index as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[index] = crc;
        index += 1;
    }
    table
}
const CRC_TABLE: [u32; 256] = crc_table();

pub(super) fn unpack(args: UnpackZipArgs) -> Result<()> {
    require_hex(&args.zip_sha256, 64)?;
    ensure!(
        args.zip_bytes > 0
            && args.zip_bytes <= MAX_ZIP_BYTES
            && args.original_floor_bytes >= ORIGINAL_FLOOR,
        "original ZIP/filesystem limits cannot be reduced"
    );
    let (digest, before) = digest_file(&args.zip, MAX_ZIP_BYTES)?;
    ensure!(
        digest.bytes == args.zip_bytes && digest.sha256 == args.zip_sha256,
        "actual received ZIP identity differs"
    );
    let mut input = open_noatime(&args.zip, false)?;
    let allowed = allowed_members(args.kind, args.piece_index)?;
    let members = members(&mut input, digest.bytes, &allowed)?;
    let unpacked = members.iter().try_fold(0_u64, |bytes, member| {
        bytes
            .checked_add(member.unpacked)
            .context("ZIP total overflow")
    })?;
    ensure!(
        unpacked <= MAX_METADATA_UNPACKED
            && (matches!(args.kind, ZipKind::Metadata) || unpacked <= PIECE_BYTES),
        "ZIP expanded-content budget exhausted"
    );
    let parent = args
        .output_dir
        .parent()
        .context("ZIP output parent absent")?;
    let (block, available, inodes) = filesystem_space(parent)?;
    let padded = unpacked
        .checked_add(
            (allowed.len() as u64 + 8)
                .checked_mul(block)
                .context("ZIP padding overflow")?,
        )
        .and_then(|bytes| bytes.checked_add(64 * 1024 * 1024))
        .and_then(|bytes| bytes.checked_add(args.original_floor_bytes))
        .context("ZIP disk admission overflow")?;
    ensure!(
        available >= padded && inodes >= allowed.len() as u64 + 16,
        "ZIP metadata/piece extraction would violate the original filesystem floor"
    );
    fs::create_dir(&args.output_dir)?;
    #[derive(Serialize)]
    struct ReceivedMember {
        path: String,
        digest: FileDigest,
        crc32: u32,
    }
    let mut received = Vec::new();
    let mut buffer = vec![0; 256 * 1024];
    for member in members {
        input.seek(SeekFrom::Start(member.data))?;
        let mut compressed = (&mut input).take(member.compressed);
        let mut reader: Box<dyn Read + '_> = if member.method == 8 {
            Box::new(DeflateDecoder::new(&mut compressed))
        } else {
            Box::new(&mut compressed)
        };
        let destination = args.output_dir.join(&member.name);
        fs::create_dir_all(destination.parent().context("ZIP member parent absent")?)?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&destination)?;
        let mut digest = Sha256::new();
        let mut crc = u32::MAX;
        let mut bytes = 0_u64;
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            bytes = bytes
                .checked_add(read as u64)
                .context("ZIP member expansion overflow")?;
            ensure!(
                bytes <= member.unpacked,
                "ZIP expansion exceeds declared content"
            );
            digest.update(&buffer[..read]);
            for byte in &buffer[..read] {
                crc = (crc >> 8) ^ CRC_TABLE[((crc ^ *byte as u32) & 255) as usize];
            }
            output.write_all(&buffer[..read])?;
        }
        drop(reader);
        ensure!(
            bytes == member.unpacked && !crc == member.crc,
            "ZIP CRC or expanded size differs"
        );
        output.sync_all()?;
        received.push(ReceivedMember {
            path: member.name,
            digest: FileDigest {
                bytes,
                sha256: format!("{:x}", digest.finalize()),
            },
            crc32: !crc,
        });
    }
    ensure!(
        before == physical(&input.metadata()?)?
            && digest_file(&args.zip, MAX_ZIP_BYTES)?.0 == digest,
        "complete received ZIP inverse differs"
    );
    #[derive(Serialize)]
    struct Receipt {
        state: &'static str,
        zip: FileDigest,
        members: Vec<ReceivedMember>,
        downloads_and_application_launches: u32,
        native_resource_and_host_abi_admission_pending: bool,
    }
    write_json(
        &args.receipt,
        &Receipt {
            state: "ROOT_BOUND_GITHUB_ZIP_AUTHENTICATED_AND_CONFINED_NOT_NATIVE_ADMISSION",
            zip: digest,
            members: received,
            downloads_and_application_launches: 0,
            native_resource_and_host_abi_admission_pending: true,
        },
        MAX_METADATA_BYTES,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crc_is_ieee_and_members_are_literal() {
        let mut crc = u32::MAX;
        for byte in b"123456789" {
            crc = (crc >> 8) ^ CRC_TABLE[((crc ^ *byte as u32) & 255) as usize];
        }
        assert_eq!(!crc, 0xcbf4_3926);
        assert_eq!(
            allowed_members(ZipKind::Piece, Some(7)).unwrap(),
            ["000007.part".to_owned()]
                .into_iter()
                .collect::<BTreeSet<_>>()
        );
        assert!(allowed_members(ZipKind::Piece, Some(256)).is_err());
        assert!(allowed_members(ZipKind::Metadata, Some(0)).is_err());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn zip_member_names_are_confined_and_local_central_names_must_match() {
        fn fixture(name: &str) -> Vec<u8> {
            let data = b"x";
            let mut crc = u32::MAX;
            for byte in data {
                crc = (crc >> 8) ^ CRC_TABLE[((crc ^ *byte as u32) & 255) as usize];
            }
            let mut local = vec![0; 30];
            local[..4].copy_from_slice(b"PK\x03\x04");
            local[4..6].copy_from_slice(&20_u16.to_le_bytes());
            local[14..18].copy_from_slice(&(!crc).to_le_bytes());
            local[18..22].copy_from_slice(&1_u32.to_le_bytes());
            local[22..26].copy_from_slice(&1_u32.to_le_bytes());
            local[26..28].copy_from_slice(&(name.len() as u16).to_le_bytes());
            local.extend_from_slice(name.as_bytes());
            local.extend_from_slice(data);
            let directory_start = local.len() as u32;
            let mut directory = vec![0; 46];
            directory[..4].copy_from_slice(b"PK\x01\x02");
            directory[4..6].copy_from_slice(&20_u16.to_le_bytes());
            directory[6..8].copy_from_slice(&20_u16.to_le_bytes());
            directory[16..20].copy_from_slice(&(!crc).to_le_bytes());
            directory[20..24].copy_from_slice(&1_u32.to_le_bytes());
            directory[24..28].copy_from_slice(&1_u32.to_le_bytes());
            directory[28..30].copy_from_slice(&(name.len() as u16).to_le_bytes());
            directory.extend_from_slice(name.as_bytes());
            let mut end = vec![0; 22];
            end[..4].copy_from_slice(b"PK\x05\x06");
            end[8..10].copy_from_slice(&1_u16.to_le_bytes());
            end[10..12].copy_from_slice(&1_u16.to_le_bytes());
            end[12..16].copy_from_slice(&(directory.len() as u32).to_le_bytes());
            end[16..20].copy_from_slice(&directory_start.to_le_bytes());
            local.extend(directory);
            local.extend(end);
            local
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("piece.zip");
        let allowed = allowed_members(ZipKind::Piece, Some(0)).unwrap();
        let original = fixture("000000.part");
        fs::write(&path, &original).unwrap();
        assert_eq!(
            members(
                &mut open_noatime(&path, false).unwrap(),
                original.len() as u64,
                &allowed
            )
            .unwrap()
            .len(),
            1
        );
        let traversal = fixture("../escape");
        fs::write(&path, &traversal).unwrap();
        assert!(
            members(
                &mut open_noatime(&path, false).unwrap(),
                traversal.len() as u64,
                &allowed
            )
            .is_err()
        );
        let mut mismatched = original.clone();
        mismatched[30] = b'x';
        fs::write(&path, &mismatched).unwrap();
        assert!(
            members(
                &mut open_noatime(&path, false).unwrap(),
                mismatched.len() as u64,
                &allowed
            )
            .is_err()
        );
        assert!(
            members(
                &mut open_noatime(&path, false).unwrap(),
                MAX_ZIP_BYTES + 1,
                &allowed
            )
            .is_err()
        );
    }
}
