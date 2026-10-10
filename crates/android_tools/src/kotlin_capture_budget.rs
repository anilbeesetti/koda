use crate::kotlin_import_facts::CaptureLimits;
use std::io::{self, Write};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct JsonUsage {
    pub(crate) bytes: usize,
    pub(crate) entries: usize,
}

impl JsonUsage {
    pub(crate) fn checked_add(self, additional: Self, limits: CaptureLimits) -> io::Result<Self> {
        let bytes = self
            .bytes
            .checked_add(additional.bytes)
            .filter(|bytes| *bytes <= limits.record_bytes.min(16 * 1024 * 1024))
            .ok_or_else(|| io::Error::other("Kotlin capture aggregate byte budget exceeded"))?;
        let entries = self
            .entries
            .checked_add(additional.entries)
            .filter(|entries| *entries <= limits.entries)
            .ok_or_else(|| io::Error::other("Kotlin capture aggregate entry budget exceeded"))?;
        Ok(Self { bytes, entries })
    }
}

// Count JSON values as serde_json writes them, before copying into the retained
// record. Object keys are excluded, matching the unchanged strict decoder.
pub(crate) struct BoundedJsonWriter<W> {
    inner: W,
    limits: CaptureLimits,
    bytes: usize,
    entries: usize,
    containers: Vec<Option<bool>>,
    string: bool,
    escaped: bool,
    scalar: bool,
}

impl<W: Write> BoundedJsonWriter<W> {
    pub(crate) fn new(inner: W, limits: CaptureLimits, envelope_bytes: usize) -> Self {
        Self {
            inner,
            limits,
            bytes: envelope_bytes,
            entries: 0,
            containers: Vec::new(),
            string: false,
            escaped: false,
            scalar: false,
        }
    }

    pub(crate) fn into_inner(self) -> W {
        self.inner
    }

    pub(crate) fn usage(&self) -> JsonUsage {
        JsonUsage {
            bytes: self.bytes,
            entries: self.entries,
        }
    }

    fn value(&mut self) -> io::Result<()> {
        self.entries = self
            .entries
            .checked_add(1)
            .filter(|entries| *entries <= self.limits.entries)
            .ok_or_else(|| io::Error::other("Kotlin capture aggregate entry budget exceeded"))?;
        Ok(())
    }

    fn count_byte(&mut self, byte: u8) -> io::Result<()> {
        if self.string {
            if self.escaped {
                self.escaped = false;
            } else if byte == b'\\' {
                self.escaped = true;
            } else if byte == b'"' {
                self.string = false;
            }
            return Ok(());
        }
        if self.scalar {
            if !matches!(byte, b',' | b'}' | b']') && !byte.is_ascii_whitespace() {
                return Ok(());
            }
            self.scalar = false;
        }
        match byte {
            b'{' => {
                self.value()?;
                self.containers.push(Some(true));
            }
            b'[' => {
                self.value()?;
                self.containers.push(None);
            }
            b'}' | b']' => {
                self.containers.pop();
            }
            b'"' => {
                if self.containers.last() != Some(&Some(true)) {
                    self.value()?;
                }
                self.string = true;
            }
            b':' => {
                if let Some(Some(key)) = self.containers.last_mut() {
                    *key = false;
                }
            }
            b',' => {
                if let Some(Some(key)) = self.containers.last_mut() {
                    *key = true;
                }
            }
            byte if byte.is_ascii_whitespace() => {}
            _ => {
                self.value()?;
                self.scalar = true;
            }
        }
        Ok(())
    }
}

impl<W: Write> Write for BoundedJsonWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .filter(|bytes| *bytes <= self.limits.record_bytes.min(16 * 1024 * 1024))
            .ok_or_else(|| io::Error::other("Kotlin capture aggregate byte budget exceeded"))?;
        for byte in bytes {
            self.count_byte(*byte)?;
        }
        self.inner.write_all(bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use serde_json::{Value, json};

    fn nodes(value: &Value) -> usize {
        1 + match value {
            Value::Object(values) => values.values().map(nodes).sum(),
            Value::Array(values) => values.iter().map(nodes).sum(),
            _ => 0,
        }
    }

    #[test]
    fn streaming_entries_match_strict_value_count_for_every_chunk_boundary() -> Result<()> {
        let value = json!({"quoted\\\"key": [null, true, false, -1.25e19, 0, "\\\"[]{},:", {"日本語": "é\n"}], "empty": []});
        let bytes = serde_json::to_vec(&value)?;
        let entries = nodes(&value);
        for chunk in 1..=bytes.len() {
            let limits = CaptureLimits {
                entries,
                ..CaptureLimits::default()
            };
            let mut writer = BoundedJsonWriter::new(Vec::new(), limits, 0);
            for part in bytes.chunks(chunk) {
                writer.write_all(part)?;
            }
            assert_eq!(writer.entries, entries);
            assert_eq!(writer.into_inner(), bytes);
            let mut writer = BoundedJsonWriter::new(
                io::sink(),
                CaptureLimits {
                    entries: entries - 1,
                    ..limits
                },
                0,
            );
            assert!(writer.write_all(&bytes).is_err());
        }
        Ok(())
    }

    #[test]
    fn byte_budget_includes_envelope_and_refuses_oversized_write_before_copying() -> Result<()> {
        let value = vec!["a".repeat(1024); 8];
        let bytes = serde_json::to_vec(&value)?;
        let limits = CaptureLimits {
            record_bytes: bytes.len() + 14,
            ..CaptureLimits::default()
        };
        let mut writer = BoundedJsonWriter::new(Vec::new(), limits, 14);
        serde_json::to_writer(&mut writer, &value)?;
        assert_eq!(writer.into_inner(), bytes);
        let mut writer = BoundedJsonWriter::new(
            Vec::new(),
            CaptureLimits {
                record_bytes: bytes.len() + 13,
                ..limits
            },
            14,
        );
        assert!(serde_json::to_writer(&mut writer, &value).is_err());
        assert!(writer.into_inner().len() < bytes.len());
        let mut writer = BoundedJsonWriter::new(
            Vec::new(),
            CaptureLimits {
                record_bytes: usize::MAX,
                ..limits
            },
            0,
        );
        assert!(writer.write_all(&vec![b'x'; 16 * 1024 * 1024 + 1]).is_err());
        assert!(writer.into_inner().is_empty());
        Ok(())
    }
}
