//! Footer/index codec (`docs/FFTLOG-V2.md` §6): packed 25-byte index entries followed
//! by a 20-byte trailer `{index_len: u32, index_xxh3: u64, magic: "FFTIDX2\0"}`.

use xxhash_rust::xxh3::xxh3_64;

/// Trailer magic at the very end of a cleanly closed file.
pub const INDEX_MAGIC: [u8; 8] = *b"FFTIDX2\0";
/// Packed wire size of one index entry: offset(8) + kind(1) + first_ts(8) + first_seq(8).
pub const INDEX_ENTRY_LEN: usize = 25;
/// Trailer size: index_len(4) + index_xxh3(8) + magic(8).
pub const TRAILER_LEN: usize = 20;

/// One frame-index entry (§6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexEntry {
    /// File offset of the frame header.
    pub offset: u64,
    /// Frame kind ([`crate::KIND_EVENTS`] or [`crate::KIND_CHECKPOINT`]).
    pub kind: u8,
    /// Frame `first_ts`.
    pub first_ts: u64,
    /// Frame `first_seq`.
    pub first_seq: u64,
}

/// Encode the full footer: every index entry, then the trailer.
pub(crate) fn encode_footer(entries: &[IndexEntry]) -> Vec<u8> {
    let index_len = entries.len() * INDEX_ENTRY_LEN;
    let mut out = Vec::with_capacity(index_len + TRAILER_LEN);
    for e in entries {
        out.extend_from_slice(&e.offset.to_le_bytes());
        out.push(e.kind);
        out.extend_from_slice(&e.first_ts.to_le_bytes());
        out.extend_from_slice(&e.first_seq.to_le_bytes());
    }
    let index_xxh3 = xxh3_64(&out);
    out.extend_from_slice(
        &u32::try_from(index_len)
            .expect("index fits u32")
            .to_le_bytes(),
    );
    out.extend_from_slice(&index_xxh3.to_le_bytes());
    out.extend_from_slice(&INDEX_MAGIC);
    out
}

/// Result of probing the end of a file for a footer.
#[derive(Debug)]
pub(crate) enum FooterProbe {
    /// Valid trailer, checksum, and entries. `frames_end` is where the frame chain ends
    /// (= start of the index region).
    Valid {
        entries: Vec<IndexEntry>,
        frames_end: u64,
    },
    /// Trailer magic present but the index does not validate. When `frames_end` is
    /// `Some`, `index_len` was usable and the frame region is exactly delimited, so the
    /// caller may rebuild by scanning; `None` means the chain cannot be delimited.
    Corrupt {
        frames_end: Option<u64>,
        detail: String,
    },
    /// No trailer magic at the end of the file.
    Absent,
}

/// Probe `data` (the whole file) for a footer. `header_len` bounds the earliest
/// possible frame offset, used to sanity-check `index_len`.
pub(crate) fn probe_footer(data: &[u8], header_len: u64) -> FooterProbe {
    let file_len = data.len() as u64;
    if file_len < header_len + TRAILER_LEN as u64 {
        return FooterProbe::Absent;
    }
    let trailer = &data[data.len() - TRAILER_LEN..];
    if trailer[12..20] != INDEX_MAGIC {
        return FooterProbe::Absent;
    }
    let index_len = u64::from(u32::from_le_bytes(trailer[0..4].try_into().unwrap()));
    let stored_xxh3 = u64::from_le_bytes(trailer[4..12].try_into().unwrap());
    let index_start = match file_len
        .checked_sub(TRAILER_LEN as u64)
        .and_then(|e| e.checked_sub(index_len))
        .filter(|&s| s >= header_len)
    {
        Some(s) => s,
        None => {
            return FooterProbe::Corrupt {
                frames_end: None,
                detail: format!("trailer index_len {index_len} exceeds file layout"),
            };
        }
    };
    if index_len % INDEX_ENTRY_LEN as u64 != 0 {
        return FooterProbe::Corrupt {
            frames_end: Some(index_start),
            detail: format!("index_len {index_len} is not a multiple of {INDEX_ENTRY_LEN}"),
        };
    }
    let index_bytes = &data[index_start as usize..data.len() - TRAILER_LEN];
    if xxh3_64(index_bytes) != stored_xxh3 {
        return FooterProbe::Corrupt {
            frames_end: Some(index_start),
            detail: "index xxh3 mismatch".into(),
        };
    }
    let mut entries = Vec::with_capacity(index_bytes.len() / INDEX_ENTRY_LEN);
    let mut prev_offset: Option<u64> = None;
    for chunk in index_bytes.chunks_exact(INDEX_ENTRY_LEN) {
        let entry = IndexEntry {
            offset: u64::from_le_bytes(chunk[0..8].try_into().unwrap()),
            kind: chunk[8],
            first_ts: u64::from_le_bytes(chunk[9..17].try_into().unwrap()),
            first_seq: u64::from_le_bytes(chunk[17..25].try_into().unwrap()),
        };
        // Offsets must sit inside the frame region and strictly ascend.
        let in_range = entry.offset >= header_len && entry.offset < index_start;
        let ascending = prev_offset.is_none_or(|p| entry.offset > p);
        if !in_range || !ascending {
            return FooterProbe::Corrupt {
                frames_end: Some(index_start),
                detail: format!("index entry offset {} out of order or range", entry.offset),
            };
        }
        prev_offset = Some(entry.offset);
        entries.push(entry);
    }
    FooterProbe::Valid {
        entries,
        frames_end: index_start,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_round_trip() {
        // Stand-in layout: 64-byte header, frame region [64, 200), then the footer.
        let entries = vec![
            IndexEntry {
                offset: 64,
                kind: 1,
                first_ts: 5,
                first_seq: 1,
            },
            IndexEntry {
                offset: 120,
                kind: 2,
                first_ts: 9,
                first_seq: 7,
            },
        ];
        let mut file = vec![0u8; 200];
        file.extend_from_slice(&encode_footer(&entries));
        match probe_footer(&file, 64) {
            FooterProbe::Valid {
                entries: got,
                frames_end,
            } => {
                assert_eq!(got, entries);
                assert_eq!(frames_end, 200);
            }
            _ => panic!("expected valid footer"),
        }
    }

    #[test]
    fn corrupt_index_detected_with_frames_end() {
        let entries = vec![IndexEntry {
            offset: 64,
            kind: 1,
            first_ts: 0,
            first_seq: 0,
        }];
        let mut file = vec![0u8; 64];
        file.extend_from_slice(&encode_footer(&entries));
        let flip_at = 64 + 3; // inside the index entry bytes
        file[flip_at] ^= 0xff;
        match probe_footer(&file, 64) {
            FooterProbe::Corrupt {
                frames_end: Some(64),
                ..
            } => {}
            _ => panic!("expected corrupt-with-known-frames-end"),
        }
    }

    #[test]
    fn absent_when_magic_missing() {
        let file = vec![0u8; 200];
        assert!(matches!(probe_footer(&file, 64), FooterProbe::Absent));
    }

    /// Regression for the audit claim that trailer `index_len` can drive an unbounded
    /// allocation: `probe_footer` bounds `index_len` against the file layout via
    /// `checked_sub` **before** any `Vec` allocation (footer.rs:79–90, then :105 only
    /// preallocates from the already-sliced byte range). A hostile `index_len` must
    /// therefore error as `Corrupt { frames_end: None, … }` and never allocate from it.
    #[test]
    fn hostile_index_len_errors_before_allocation() {
        let entries = vec![IndexEntry {
            offset: 64,
            kind: 1,
            first_ts: 0,
            first_seq: 0,
        }];
        let mut file = vec![0u8; 64];
        file.extend_from_slice(&encode_footer(&entries));

        // Overwrite trailer index_len with u32::MAX while leaving magic intact.
        let index_len_at = file.len() - TRAILER_LEN;
        file[index_len_at..index_len_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());

        match probe_footer(&file, 64) {
            FooterProbe::Corrupt {
                frames_end: None,
                detail,
            } => {
                assert!(
                    detail.contains("index_len") && detail.contains("exceeds file layout"),
                    "detail={detail}"
                );
            }
            other => panic!("expected Corrupt with frames_end=None, got {other:?}"),
        }

        // Truncated layout: trailer claims a huge index but the file is short of it.
        let mut short = vec![0u8; 64 + TRAILER_LEN];
        let trailer_start = short.len() - TRAILER_LEN;
        short[trailer_start..trailer_start + 4].copy_from_slice(&(1_000_000u32).to_le_bytes());
        short[trailer_start + 12..].copy_from_slice(&INDEX_MAGIC);
        match probe_footer(&short, 64) {
            FooterProbe::Corrupt {
                frames_end: None, ..
            } => {}
            other => panic!("expected Corrupt with frames_end=None, got {other:?}"),
        }
    }
}
