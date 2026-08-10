//! Checkpoint section framing (`docs/FFTLOG-V2.md` §5). `fft-log` owns the section
//! table and this primitive reader/writer **only** — section contents (BOOK, PROFILE,
//! ...) are opaque byte payloads encoded and versioned by their owning crates.

use xxhash_rust::xxh3::xxh3_64;

use crate::error::{LogError, Result};

/// Section id 1: order book state.
pub const SECTION_BOOK: u16 = 1;
/// Section id 2: flow-window state.
pub const SECTION_FLOW: u16 = 2;
/// Section id 3: profile state.
pub const SECTION_PROFILE: u16 = 3;
/// Section id 4: cumulative-volume-delta state.
pub const SECTION_CVD: u16 = 4;
/// Section id 5: native-refresh classifier state.
pub const SECTION_REFRESH: u16 = 5;
/// Section id 6: session state.
pub const SECTION_SESSION: u16 = 6;
/// Section flag bit 0: a reader that does not know the section id may skip it.
pub const SECTION_FLAG_OPTIONAL: u16 = 1;

/// Per-section header size: id(2) + version(2) + flags(2) + reserved(2) + len(4) + xxh3(8).
const SECTION_HEADER_LEN: usize = 20;

/// Ids in the frozen §5 section table. Anything else is *unknown*: skipped when
/// OPTIONAL, a loud error otherwise.
fn is_known(id: u16) -> bool {
    (SECTION_BOOK..=SECTION_SESSION).contains(&id)
}

/// A borrowed checkpoint section for writing.
#[derive(Debug, Clone, Copy)]
pub struct SectionRef<'a> {
    /// Section id (§5 table for known sections).
    pub id: u16,
    /// Per-section version, owned by the encoding crate.
    pub version: u16,
    /// Bit 0 = [`SECTION_FLAG_OPTIONAL`].
    pub flags: u16,
    /// Opaque section payload, owned by the encoding crate.
    pub bytes: &'a [u8],
}

/// An owned checkpoint section, as returned by the reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Section id (§5 table).
    pub id: u16,
    /// Per-section version, owned by the encoding crate.
    pub version: u16,
    /// Bit 0 = [`SECTION_FLAG_OPTIONAL`].
    pub flags: u16,
    /// Opaque section payload.
    pub bytes: Vec<u8>,
}

/// Encode sections into one uncompressed CHECKPOINT payload, enforcing strictly
/// ascending section-id order. Returns `(payload, section_count)`.
pub(crate) fn encode_sections<'a>(
    sections: impl IntoIterator<Item = SectionRef<'a>>,
) -> Result<(Vec<u8>, u32)> {
    let mut out = Vec::new();
    let mut count: u32 = 0;
    let mut prev_id: Option<u16> = None;
    for s in sections {
        if let Some(prev) = prev_id
            && s.id <= prev
        {
            return Err(LogError::SectionOrder {
                frame_offset: 0,
                prev_id: prev,
                id: s.id,
            });
        }
        prev_id = Some(s.id);
        let len = u32::try_from(s.bytes.len()).map_err(|_| LogError::BadSection {
            frame_offset: 0,
            detail: format!("section {} length {} exceeds u32", s.id, s.bytes.len()),
        })?;
        out.extend_from_slice(&s.id.to_le_bytes());
        out.extend_from_slice(&s.version.to_le_bytes());
        out.extend_from_slice(&s.flags.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // reserved
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&xxh3_64(s.bytes).to_le_bytes());
        out.extend_from_slice(s.bytes);
        count += 1;
    }
    Ok((out, count))
}

/// Decode an uncompressed CHECKPOINT payload into sections. Enforces strictly ascending
/// id order and per-section checksums. Unknown ids: OPTIONAL flag → skipped (not
/// returned), otherwise → [`LogError::UnknownRequiredSection`]. Trailing bytes after the
/// last section are a loud error.
pub(crate) fn decode_sections(
    payload: &[u8],
    count: u32,
    frame_offset: u64,
) -> Result<Vec<Section>> {
    let mut sections = Vec::new();
    let mut pos = 0usize;
    let mut prev_id: Option<u16> = None;
    for index in 0..count {
        let header_end = pos
            .checked_add(SECTION_HEADER_LEN)
            .filter(|&e| e <= payload.len())
            .ok_or(LogError::SectionTruncated {
                frame_offset,
                index,
            })?;
        let h = &payload[pos..header_end];
        let id = u16::from_le_bytes(h[0..2].try_into().unwrap());
        let version = u16::from_le_bytes(h[2..4].try_into().unwrap());
        let flags = u16::from_le_bytes(h[4..6].try_into().unwrap());
        let reserved = u16::from_le_bytes(h[6..8].try_into().unwrap());
        let len = u32::from_le_bytes(h[8..12].try_into().unwrap());
        let stored_xxh3 = u64::from_le_bytes(h[12..20].try_into().unwrap());
        if reserved != 0 {
            return Err(LogError::BadSection {
                frame_offset,
                detail: format!("section {id} has non-zero reserved field {reserved}"),
            });
        }
        if let Some(prev) = prev_id
            && id <= prev
        {
            return Err(LogError::SectionOrder {
                frame_offset,
                prev_id: prev,
                id,
            });
        }
        prev_id = Some(id);
        let end = header_end
            .checked_add(len as usize)
            .filter(|&e| e <= payload.len())
            .ok_or(LogError::SectionTruncated {
                frame_offset,
                index,
            })?;
        let bytes = &payload[header_end..end];
        if xxh3_64(bytes) != stored_xxh3 {
            return Err(LogError::SectionChecksum { frame_offset, id });
        }
        pos = end;
        if !is_known(id) {
            if flags & SECTION_FLAG_OPTIONAL != 0 {
                continue; // unknown OPTIONAL: skip per §5.
            }
            return Err(LogError::UnknownRequiredSection { frame_offset, id });
        }
        sections.push(Section {
            id,
            version,
            flags,
            bytes: bytes.to_vec(),
        });
    }
    if pos != payload.len() {
        return Err(LogError::BadSection {
            frame_offset,
            detail: format!("{} trailing bytes after last section", payload.len() - pos),
        });
    }
    Ok(sections)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_order_enforcement() {
        let book = vec![1u8, 2, 3];
        let profile = vec![9u8; 100];
        let (payload, count) = encode_sections([
            SectionRef {
                id: SECTION_BOOK,
                version: 1,
                flags: 0,
                bytes: &book,
            },
            SectionRef {
                id: SECTION_PROFILE,
                version: 2,
                flags: 0,
                bytes: &profile,
            },
        ])
        .unwrap();
        assert_eq!(count, 2);
        let sections = decode_sections(&payload, count, 0).unwrap();
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].bytes, book);
        assert_eq!(
            sections[1],
            Section {
                id: SECTION_PROFILE,
                version: 2,
                flags: 0,
                bytes: profile
            }
        );

        // Descending order on write is refused.
        let err = encode_sections([
            SectionRef {
                id: SECTION_PROFILE,
                version: 1,
                flags: 0,
                bytes: &[],
            },
            SectionRef {
                id: SECTION_BOOK,
                version: 1,
                flags: 0,
                bytes: &[],
            },
        ])
        .unwrap_err();
        assert!(matches!(err, LogError::SectionOrder { .. }));
    }

    #[test]
    fn unknown_sections() {
        let (payload, count) = encode_sections([
            SectionRef {
                id: SECTION_BOOK,
                version: 1,
                flags: 0,
                bytes: b"b",
            },
            SectionRef {
                id: 99,
                version: 1,
                flags: SECTION_FLAG_OPTIONAL,
                bytes: b"x",
            },
        ])
        .unwrap();
        // Unknown OPTIONAL is skipped.
        let sections = decode_sections(&payload, count, 0).unwrap();
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].id, SECTION_BOOK);

        // Unknown REQUIRED is a loud error.
        let (payload, count) = encode_sections([SectionRef {
            id: 99,
            version: 1,
            flags: 0,
            bytes: b"x",
        }])
        .unwrap();
        assert!(matches!(
            decode_sections(&payload, count, 5),
            Err(LogError::UnknownRequiredSection {
                frame_offset: 5,
                id: 99
            })
        ));
    }

    #[test]
    fn section_corruption_is_loud() {
        let (mut payload, count) = encode_sections([SectionRef {
            id: SECTION_BOOK,
            version: 1,
            flags: 0,
            bytes: b"abc",
        }])
        .unwrap();
        let last = payload.len() - 1;
        payload[last] ^= 0xff;
        assert!(matches!(
            decode_sections(&payload, count, 0),
            Err(LogError::SectionChecksum {
                id: SECTION_BOOK,
                ..
            })
        ));
    }
}
