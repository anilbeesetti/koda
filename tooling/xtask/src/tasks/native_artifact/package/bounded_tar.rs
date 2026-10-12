//! Raw tar validation precedes extension allocation. The producer emits GNU tar;
//! PAX, sparse files and concatenated archives require explicit future support.

use anyhow::{Context as _, Result, ensure};
use std::io::{BufRead, Read, Seek, SeekFrom};

use super::{PayloadKind, PayloadManifest};

const BLOCK: u64 = 512;
const LONG_BYTES: u64 = super::super::MAX_PATH_BYTES as u64 + 1;

pub(super) fn decoder<R: BufRead + Seek>(mut input: R) -> Result<flate2::bufread::GzDecoder<R>> {
    let start = input.stream_position()?;
    let mut header = [0; 10];
    input.read_exact(&mut header)?;
    // The producer has no gzip filename/comment/extra records. Reject these
    // before GzDecoder can buffer an unbounded NUL-terminated header field.
    ensure!(
        header[..3] == [0x1f, 0x8b, 8] && header[3] == 0,
        "only bounded producer gzip headers without optional metadata are supported"
    );
    input.seek(SeekFrom::Start(start))?;
    Ok(flate2::bufread::GzDecoder::new(input))
}

pub(super) fn expanded_limit(manifest: &PayloadManifest) -> Result<u64> {
    manifest.entries.iter().try_fold(2 * BLOCK, |total, entry| {
        let body = if entry.kind == PayloadKind::File {
            entry.bytes
        } else {
            0
        };
        let padded = body
            .checked_add(BLOCK - 1)
            .map(|n| n / BLOCK * BLOCK)
            .context("tar body padding overflow")?;
        // Two bounded GNU name/link records may precede each real member.
        total
            .checked_add(BLOCK + padded + 2 * (BLOCK + 3 * BLOCK))
            .context("expanded tar budget overflow")
    })
}

pub(super) struct Member {
    pub(super) header: tar::Header,
    pub(super) name: String,
    pub(super) link: Option<String>,
}

pub(super) struct RawTar<R> {
    reader: R,
    limit: u64,
    consumed: u64,
    maximum_members: usize,
    members: usize,
    remaining: u64,
    padding: usize,
    ended: bool,
}

impl<R: Read> RawTar<R> {
    pub(super) fn new(reader: R, limit: u64, maximum_members: usize) -> Self {
        Self {
            reader,
            limit,
            consumed: 0,
            maximum_members,
            members: 0,
            remaining: 0,
            padding: 0,
            ended: false,
        }
    }

    fn exact(&mut self, bytes: &mut [u8]) -> Result<()> {
        let total = self
            .consumed
            .checked_add(bytes.len() as u64)
            .context("expanded tar byte overflow")?;
        ensure!(
            total <= self.limit,
            "expanded tar header/body/padding budget exceeded"
        );
        self.reader.read_exact(bytes)?;
        self.consumed = total;
        Ok(())
    }

    fn zero_padding(&mut self) -> Result<()> {
        let mut padding = [0; 511];
        let count = self.padding;
        self.exact(&mut padding[..count])?;
        ensure!(
            padding[..count].iter().all(|byte| *byte == 0),
            "nonzero tar block padding"
        );
        self.padding = 0;
        Ok(())
    }

    pub(super) fn next(&mut self) -> Result<Option<Member>> {
        ensure!(
            self.remaining == 0 && !self.ended,
            "tar member body not consumed or archive already closed"
        );
        self.zero_padding()?;
        let mut long_name = None;
        let mut long_link = None;
        loop {
            let mut block = [0; BLOCK as usize];
            self.exact(&mut block)?;
            if block.iter().all(|byte| *byte == 0) {
                ensure!(
                    long_name.is_none() && long_link.is_none(),
                    "orphan GNU tar extension"
                );
                let mut second = [0; BLOCK as usize];
                self.exact(&mut second)?;
                ensure!(
                    second.iter().all(|byte| *byte == 0),
                    "tar requires exactly two zero terminal blocks"
                );
                self.ended = true;
                return Ok(None);
            }
            self.members = self
                .members
                .checked_add(1)
                .context("raw tar member overflow")?;
            ensure!(
                self.members <= self.maximum_members,
                "raw tar extension/member count exhausted"
            );
            let mut header = tar::Header::new_gnu();
            header.as_mut_bytes().copy_from_slice(&block);
            let checksum: u32 = block
                .iter()
                .enumerate()
                .map(|(index, byte)| {
                    if (148..156).contains(&index) {
                        b' ' as u32
                    } else {
                        *byte as u32
                    }
                })
                .sum();
            ensure!(
                header.cksum()? == checksum,
                "raw tar header checksum differs"
            );
            let kind = header.entry_type().as_byte();
            let size = header.size()?;
            if matches!(kind, b'L' | b'K') {
                // Crucially, this rejects before Vec allocation or body reading.
                ensure!(
                    size > 0 && size <= LONG_BYTES,
                    "GNU long-name/link exceeds path budget"
                );
                ensure!(
                    header.as_gnu().is_some(),
                    "GNU extension requires a GNU header"
                );
                let slot = if kind == b'L' {
                    &mut long_name
                } else {
                    &mut long_link
                };
                ensure!(slot.is_none(), "duplicate GNU extension before a member");
                let mut bytes = vec![0; usize::try_from(size)?];
                self.exact(&mut bytes)?;
                ensure!(
                    bytes.last() == Some(&0) && !bytes[..bytes.len() - 1].contains(&0),
                    "GNU name/link must have one final NUL"
                );
                let value = std::str::from_utf8(&bytes[..bytes.len() - 1])?.to_owned();
                ensure!(!value.is_empty(), "empty GNU name/link");
                // Release the option borrow before reading padding through self.
                if kind == b'L' {
                    long_name = Some(value);
                } else {
                    long_link = Some(value);
                }
                self.padding = ((BLOCK - size % BLOCK) % BLOCK) as usize;
                self.zero_padding()?;
                continue;
            }
            ensure!(
                !matches!(kind, b'x' | b'g'),
                "PAX extension is unsupported and rejected before its body"
            );
            ensure!(
                matches!(kind, 0 | b'0' | b'2' | b'5'),
                "unsupported raw tar member type"
            );
            let name = match long_name {
                Some(name) => name,
                None => std::str::from_utf8(header.path_bytes().as_ref())?.to_owned(),
            };
            ensure!(
                name.len() <= super::super::MAX_PATH_BYTES,
                "tar member path exceeds budget"
            );
            let link = match long_link {
                Some(link) => Some(link),
                None => header
                    .link_name_bytes()
                    .map(|bytes| std::str::from_utf8(bytes.as_ref()).map(str::to_owned))
                    .transpose()?,
            };
            ensure!(
                link.as_ref()
                    .is_none_or(|link| link.len() <= super::super::MAX_PATH_BYTES),
                "tar link target exceeds budget"
            );
            self.remaining = size;
            self.padding = ((BLOCK - size % BLOCK) % BLOCK) as usize;
            return Ok(Some(Member { header, name, link }));
        }
    }

    pub(super) fn body(&mut self, buffer: &mut [u8]) -> Result<usize> {
        let count = usize::try_from(self.remaining.min(buffer.len() as u64))?;
        self.exact(&mut buffer[..count])?;
        self.remaining -= count as u64;
        Ok(count)
    }

    pub(super) fn finish(mut self) -> Result<R> {
        ensure!(
            self.ended && self.remaining == 0,
            "tar terminal blocks not observed"
        );
        let mut extra = [0];
        // Reading the decoder to its actual EOF validates its CRC and ISIZE footer.
        // Even one expanded trailing byte is rejected, rather than drained forever.
        ensure!(
            self.reader.read(&mut extra)? == 0,
            "data follows the tar terminal blocks"
        );
        Ok(self.reader)
    }
}

impl<R: BufRead> RawTar<flate2::bufread::GzDecoder<R>> {
    pub(super) fn finish_gzip(self) -> Result<R> {
        let decoder = self.finish()?;
        let mut input = decoder.into_inner();
        ensure!(
            input.fill_buf()?.is_empty(),
            "concatenated gzip member or trailing compressed bytes"
        );
        Ok(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        io::{self, Cursor, Write},
        rc::Rc,
    };

    struct Counting {
        bytes: Cursor<Vec<u8>>,
        reads: Rc<Cell<u64>>,
    }
    impl Read for Counting {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let n = self.bytes.read(buffer)?;
            self.reads.set(self.reads.get() + n as u64);
            Ok(n)
        }
    }
    fn extension(kind: u8, bytes: u64) -> Vec<u8> {
        let mut header = tar::Header::new_gnu();
        header.set_path("././@LongLink").unwrap();
        header.set_entry_type(tar::EntryType::new(kind));
        header.set_size(bytes);
        header.set_cksum();
        header.as_bytes().to_vec()
    }

    #[test]
    fn giant_gnu_and_pax_headers_fail_before_any_extension_body_read() {
        for kind in [b'L', b'K', b'x', b'g'] {
            let reads = Rc::new(Cell::new(0));
            let input = Counting {
                bytes: Cursor::new(extension(kind, 1 << 40)),
                reads: reads.clone(),
            };
            let mut tar = RawTar::new(input, 4096, 3);
            assert!(tar.next().is_err());
            assert_eq!(reads.get(), 512);
        }
    }

    #[test]
    fn extension_count_padding_and_expansion_fail_at_the_first_over_budget_block() {
        let mut bytes = extension(b'L', 2);
        bytes.extend_from_slice(b"a\0");
        bytes.extend_from_slice(&[0; 510]);
        bytes.extend_from_slice(&extension(b'K', 2));
        let reads = Rc::new(Cell::new(0));
        let mut tar = RawTar::new(
            Counting {
                bytes: Cursor::new(bytes.clone()),
                reads: reads.clone(),
            },
            1024,
            3,
        );
        assert!(tar.next().is_err());
        assert_eq!(reads.get(), 1024);
        let mut tar = RawTar::new(Cursor::new(bytes.clone()), 4096, 1);
        assert!(tar.next().is_err());
        bytes[515] = 1;
        assert!(RawTar::new(Cursor::new(bytes), 4096, 3).next().is_err());
    }

    #[test]
    fn genuine_long_gnu_paths_roundtrip_and_terminal_trailing_data_is_rejected() {
        let name = format!("runtime/source/{}/file.rs", "long-component/".repeat(12));
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(3);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, &name, &b"abc"[..])
            .unwrap();
        let bytes = builder.into_inner().unwrap();
        let mut raw = RawTar::new(Cursor::new(bytes.clone()), 8192, 3);
        let member = raw.next().unwrap().unwrap();
        assert_eq!(member.name, name);
        let mut body = [0; 3];
        assert_eq!(raw.body(&mut body).unwrap(), 3);
        assert_eq!(&body, b"abc");
        assert!(raw.next().unwrap().is_none());
        assert!(raw.finish().is_ok());
        let mut bad = bytes;
        bad.push(1);
        let mut raw = RawTar::new(Cursor::new(bad), 8192, 3);
        raw.next().unwrap();
        raw.body(&mut body).unwrap();
        raw.next().unwrap();
        assert!(raw.finish().is_err());
    }

    #[test]
    fn gzip_optional_headers_fail_before_metadata_read_and_crc_footer_and_members_are_checked() {
        let mut bad_header = vec![0x1f, 0x8b, 8, 8, 0, 0, 0, 0, 0, 255];
        bad_header.extend(std::iter::repeat_n(b'x', 100_000));
        let mut input = Cursor::new(bad_header);
        assert!(decoder(&mut input).is_err());
        assert_eq!(input.position(), 10);
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
        encoder.write_all(&[0; 1024]).unwrap();
        let valid = encoder.finish().unwrap();
        let check = |bytes: Vec<u8>| -> Result<()> {
            let mut raw = RawTar::new(decoder(Cursor::new(bytes))?, 1024, 0);
            ensure!(raw.next()?.is_none(), "expected terminal blocks");
            raw.finish_gzip()?;
            Ok(())
        };
        assert!(check(valid.clone()).is_ok());
        let mut crc = valid.clone();
        let len = crc.len();
        crc[len - 8] ^= 1;
        assert!(check(crc).is_err());
        let mut truncated = valid.clone();
        truncated.truncate(truncated.len() - 4);
        assert!(check(truncated).is_err());
        let mut concatenated = valid.clone();
        concatenated.extend_from_slice(&valid);
        assert!(check(concatenated).is_err());
        let mut trailing = valid;
        trailing.push(1);
        assert!(check(trailing).is_err());
    }
}
