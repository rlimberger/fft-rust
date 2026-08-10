//! File header codec (`docs/FFTLOG-V2.md` §2): magic, version, flags, and the
//! fixed-order metadata block encoding [`fft_core::InstrumentMeta`].
//!
//! Ambiguity resolved here: the spec says strings are "len-prefixed UTF-8" without a
//! width — this implementation uses a **`u16` little-endian length prefix** for the
//! symbol, dataset, and schema-tag strings.

use fft_core::{InstrumentMeta, Price, Ts};
use xxhash_rust::xxh3::xxh3_64;

use crate::error::{LogError, Result};

/// File magic at offset 0.
pub const MAGIC: [u8; 8] = *b"FFTLOG2\0";
/// Wire major version. Readers reject anything else, loudly (§2).
pub const VERSION_MAJOR: u16 = 2;
/// Wire minor version. Minor bumps are additive-only.
pub const VERSION_MINOR: u16 = 0;
/// Header flag bit 0: set while a writer is appending, cleared on clean close.
pub const FLAG_LIVE: u32 = 1;
/// Source schema tag written by this version (§2 metadata block).
pub const SCHEMA_TAG: &str = "mbo";

/// Bytes before the metadata block: magic(8) + major(2) + minor(2) + flags(4) + meta_len(4).
const FIXED_PREFIX: usize = 20;
/// Byte offset of the `flags` field, needed to clear LIVE on close.
pub(crate) const FLAGS_OFFSET: usize = 12;
/// Sanity ceiling for `meta_len`; real metadata is well under 1 KiB.
const MAX_META_LEN: u32 = 64 * 1024;

/// Decoded file header.
#[derive(Debug, Clone)]
pub(crate) struct HeaderInfo {
    pub version_minor: u16,
    pub flags: u32,
    pub meta: InstrumentMeta,
    pub schema_tag: String,
    /// Total header length: `FIXED_PREFIX + meta_len + 8` (checksum).
    pub header_len: u64,
}

fn put_str(out: &mut Vec<u8>, field: &'static str, s: &str) -> Result<()> {
    let len = u16::try_from(s.len()).map_err(|_| LogError::BadMetadata {
        detail: format!("{field} length {} exceeds u16", s.len()),
    })?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

/// Encode the §2 metadata block in its fixed order.
fn encode_meta(meta: &InstrumentMeta) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(96);
    put_str(&mut out, "symbol", &meta.symbol)?;
    put_str(&mut out, "dataset", &meta.dataset)?;
    out.extend_from_slice(&meta.instrument_id.to_le_bytes());
    out.extend_from_slice(&meta.min_price_increment.0.to_le_bytes());
    out.extend_from_slice(&meta.unit_of_measure_qty.to_le_bytes());
    out.extend_from_slice(&meta.display_factor.to_le_bytes());
    out.extend_from_slice(&meta.trade_date.to_le_bytes());
    out.extend_from_slice(&meta.session_open.0.to_le_bytes());
    put_str(&mut out, "schema tag", SCHEMA_TAG)?;
    Ok(out)
}

/// Encode the complete file header (including trailing `header_xxh3`).
pub(crate) fn encode_header(meta: &InstrumentMeta, flags: u32) -> Result<Vec<u8>> {
    let meta_bytes = encode_meta(meta)?;
    let meta_len = u32::try_from(meta_bytes.len()).expect("metadata under u16-prefixed bound");
    let mut out = Vec::with_capacity(FIXED_PREFIX + meta_bytes.len() + 8);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION_MAJOR.to_le_bytes());
    out.extend_from_slice(&VERSION_MINOR.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&meta_len.to_le_bytes());
    out.extend_from_slice(&meta_bytes);
    let checksum = xxh3_64(&out);
    out.extend_from_slice(&checksum.to_le_bytes());
    Ok(out)
}

/// Little-endian cursor over the metadata block; every read is bounds-checked.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize, what: &str) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.buf.len())
            .ok_or_else(|| LogError::BadMetadata {
                detail: format!("truncated while reading {what}"),
            })?;
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn u16(&mut self, what: &str) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2, what)?.try_into().unwrap()))
    }

    fn u32(&mut self, what: &str) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4, what)?.try_into().unwrap()))
    }

    fn u64(&mut self, what: &str) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8, what)?.try_into().unwrap()))
    }

    fn i64(&mut self, what: &str) -> Result<i64> {
        Ok(i64::from_le_bytes(self.take(8, what)?.try_into().unwrap()))
    }

    fn string(&mut self, what: &str) -> Result<String> {
        let len = self.u16(what)? as usize;
        let bytes = self.take(len, what)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| LogError::BadMetadata {
            detail: format!("{what} is not valid UTF-8"),
        })
    }
}

/// Decode and validate the file header from the start of `data`.
///
/// Order of checks: length, magic, `meta_len` bounds, `header_xxh3`, `version_major`,
/// then the metadata fields. Readers stop at the fields they know; extra metadata bytes
/// (additive minor bumps) are ignored per §2.
pub(crate) fn decode_header(data: &[u8]) -> Result<HeaderInfo> {
    if data.len() < FIXED_PREFIX + 8 {
        return Err(LogError::HeaderTruncated {
            file_len: data.len() as u64,
        });
    }
    if data[..8] != MAGIC {
        return Err(LogError::BadMagic {
            found: data[..8].try_into().unwrap(),
        });
    }
    let version_major = u16::from_le_bytes(data[8..10].try_into().unwrap());
    let version_minor = u16::from_le_bytes(data[10..12].try_into().unwrap());
    let flags = u32::from_le_bytes(data[FLAGS_OFFSET..16].try_into().unwrap());
    let meta_len = u32::from_le_bytes(data[16..20].try_into().unwrap());
    if meta_len > MAX_META_LEN {
        return Err(LogError::BadMetadata {
            detail: format!("meta_len {meta_len} exceeds sanity ceiling {MAX_META_LEN}"),
        });
    }
    let header_len = FIXED_PREFIX + meta_len as usize + 8;
    if data.len() < header_len {
        return Err(LogError::HeaderTruncated {
            file_len: data.len() as u64,
        });
    }
    let checksum_at = FIXED_PREFIX + meta_len as usize;
    let stored = u64::from_le_bytes(data[checksum_at..header_len].try_into().unwrap());
    if xxh3_64(&data[..checksum_at]) != stored {
        return Err(LogError::HeaderChecksum);
    }
    if version_major != VERSION_MAJOR {
        return Err(LogError::UnsupportedMajor {
            found: version_major,
        });
    }

    let mut c = Cursor {
        buf: &data[FIXED_PREFIX..checksum_at],
        pos: 0,
    };
    let symbol = c.string("symbol")?;
    let dataset = c.string("dataset")?;
    let instrument_id = c.u32("instrument_id")?;
    let min_price_increment = Price(c.i64("min_price_increment")?);
    let unit_of_measure_qty = c.i64("unit_of_measure_qty")?;
    let display_factor = c.i64("display_factor")?;
    let trade_date = c.u32("trade_date")?;
    let session_open = Ts(c.u64("session_open")?);
    let schema_tag = c.string("schema tag")?;
    // Frozen v2.0 carries the constant `mbo` only (§2); anything else is a loud reject.
    if schema_tag != SCHEMA_TAG {
        return Err(LogError::UnsupportedSchemaTag { found: schema_tag });
    }

    Ok(HeaderInfo {
        version_minor,
        flags,
        meta: InstrumentMeta {
            symbol,
            instrument_id,
            dataset,
            min_price_increment,
            unit_of_measure_qty,
            display_factor,
            trade_date,
            session_open,
        },
        schema_tag,
        header_len: header_len as u64,
    })
}

/// Re-encode `header` with LIVE cleared, preserving every other field. Used on close.
pub(crate) fn with_live_cleared(header_bytes: &[u8]) -> Vec<u8> {
    let mut out = header_bytes.to_vec();
    let mut flags = u32::from_le_bytes(out[FLAGS_OFFSET..FLAGS_OFFSET + 4].try_into().unwrap());
    flags &= !FLAG_LIVE;
    out[FLAGS_OFFSET..FLAGS_OFFSET + 4].copy_from_slice(&flags.to_le_bytes());
    let checksum_at = out.len() - 8;
    let checksum = xxh3_64(&out[..checksum_at]);
    out[checksum_at..].copy_from_slice(&checksum.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fft_core::{InstrumentMeta, Price, Ts};

    fn sample_meta() -> InstrumentMeta {
        InstrumentMeta {
            symbol: "ESU6".into(),
            instrument_id: 42,
            dataset: "GLBX.MDP3".into(),
            min_price_increment: Price(250_000_000),
            unit_of_measure_qty: 50_000_000_000,
            display_factor: 1,
            trade_date: 20_662,
            session_open: Ts(1_785_000_000_000_000_000),
        }
    }

    #[test]
    fn accepts_mbo_schema_tag() {
        let hdr = encode_header(&sample_meta(), 0).unwrap();
        let info = decode_header(&hdr).unwrap();
        assert_eq!(info.schema_tag, SCHEMA_TAG);
    }

    #[test]
    fn rejects_non_mbo_schema_tag() {
        let mut hdr = encode_header(&sample_meta(), 0).unwrap();
        // Schema tag is the last metadata field: u16-le len + UTF-8, immediately
        // before the trailing header_xxh3. "mbo" is 3 bytes.
        let tag_bytes_at = hdr.len() - 8 - 3;
        assert_eq!(&hdr[tag_bytes_at..tag_bytes_at + 3], b"mbo");
        hdr[tag_bytes_at..tag_bytes_at + 3].copy_from_slice(b"mbx");
        let checksum_at = hdr.len() - 8;
        let checksum = xxh3_64(&hdr[..checksum_at]);
        hdr[checksum_at..].copy_from_slice(&checksum.to_le_bytes());

        match decode_header(&hdr) {
            Err(LogError::UnsupportedSchemaTag { found }) => assert_eq!(found, "mbx"),
            other => panic!("expected UnsupportedSchemaTag, got {other:?}"),
        }
    }
}
