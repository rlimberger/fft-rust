//! Frame-chain scan and closed-file index resolution (§6–§7). Internal to the
//! reader module — not part of the public crate surface.

use std::fs::File;

use memmap2::Mmap;
use xxhash_rust::xxh3::xxh3_64;

use crate::error::{LogError, Result};
use crate::footer::{self, FooterProbe, IndexEntry};
use crate::frame::{FRAME_HEADER_LEN, FrameHeader};
use crate::reader::IndexSource;

/// Read-only mapping of the log file. The **only** unsafe block in the crate.
#[allow(unsafe_code)]
pub(super) fn map_file(file: &File) -> Result<Mmap> {
    // SAFETY: standard memmap2 read-only mapping. The safety contract is that the file
    // is not truncated or mutated in place while mapped; the fftlog format is
    // append-only (§7 — committed bytes are never overwritten) and the writer's only
    // in-place write is the header LIVE flag on close, which readers of a live file
    // re-validate by checksum. External truncation of a mapped file is undefined
    // behaviour we accept for same-host tailing, as specified.
    unsafe { Mmap::map(file) }.map_err(LogError::io("mmap log file"))
}

/// How a frame-chain scan ended.
pub(super) enum ScanEnd {
    /// Consumed exactly to the region end.
    Clean,
    /// Non-validating bytes from `offset` (torn header, torn payload, checksum miss).
    Tail { offset: u64, detail: String },
}

/// Result of walking the frame chain over `[start, end)`.
pub(super) struct Scan {
    pub entries: Vec<IndexEntry>,
    /// `(last_ts, last_seq)` of the last committed frame.
    pub last_good: Option<(u64, u64)>,
    pub end: ScanEnd,
}

/// Walk frame headers over `data[start..end)`, validating both checksums of every
/// frame (§7 commit rule). Returns `Err` only for *hard* corruption: a frame header
/// whose checksum validates but whose contents are illegal (limit breach, bad kind,
/// non-zero reserved) can never be a torn write and is loud everywhere.
pub(super) fn scan_frames(data: &[u8], start: u64, end: u64) -> Result<Scan> {
    let mut entries = Vec::new();
    let mut last_good: Option<(u64, u64)> = None;
    let mut offset = start;
    let scan_end = loop {
        if offset == end {
            break ScanEnd::Clean;
        }
        if end - offset < FRAME_HEADER_LEN as u64 {
            break ScanEnd::Tail {
                offset,
                detail: "truncated frame header".into(),
            };
        }
        let header_bytes: &[u8; FRAME_HEADER_LEN] = data
            [offset as usize..offset as usize + FRAME_HEADER_LEN]
            .try_into()
            .unwrap();
        let fh = match FrameHeader::decode(header_bytes, offset) {
            Ok(fh) => fh,
            Err(LogError::FrameHeaderChecksum { .. }) => {
                break ScanEnd::Tail {
                    offset,
                    detail: "frame header checksum mismatch".into(),
                };
            }
            // Checksum-valid but illegal contents: hard corruption, loud everywhere.
            Err(e) => return Err(e),
        };
        let payload_start = offset + FRAME_HEADER_LEN as u64;
        let payload_end = payload_start + u64::from(fh.compressed_len);
        if payload_end > end {
            break ScanEnd::Tail {
                offset,
                detail: "truncated frame payload".into(),
            };
        }
        if xxh3_64(&data[payload_start as usize..payload_end as usize]) != fh.payload_xxh3 {
            break ScanEnd::Tail {
                offset,
                detail: "frame payload checksum mismatch".into(),
            };
        }
        entries.push(IndexEntry {
            offset,
            kind: fh.kind,
            first_ts: fh.first_ts,
            first_seq: fh.first_seq,
        });
        last_good = Some((fh.last_ts, fh.last_seq));
        offset = payload_end;
    };
    Ok(Scan {
        entries,
        last_good,
        end: scan_end,
    })
}

/// Resolve the frame index for a closed (non-LIVE) file — same policy as open (§6).
pub(super) fn resolve_closed_index(
    mmap: &Mmap,
    header_len: u64,
    file_len: u64,
    warnings: &mut Vec<String>,
) -> Result<(Vec<IndexEntry>, u64, IndexSource)> {
    match footer::probe_footer(mmap, header_len) {
        FooterProbe::Valid {
            entries,
            frames_end,
        } => Ok((entries, frames_end, IndexSource::Footer)),
        FooterProbe::Corrupt {
            frames_end: Some(frames_end),
            detail,
        } => {
            let scan = scan_frames(mmap, header_len, frames_end)?;
            match scan.end {
                ScanEnd::Clean => {
                    warnings.push(format!(
                        "footer index corrupt ({detail}); frame chain intact — \
                         index rebuilt by scan"
                    ));
                    Ok((scan.entries, frames_end, IndexSource::RebuiltCorruptIndex))
                }
                ScanEnd::Tail { offset, detail } => Err(LogError::CorruptTail { offset, detail }),
            }
        }
        FooterProbe::Corrupt {
            frames_end: None,
            detail,
        } => Err(LogError::CorruptIndex { detail }),
        FooterProbe::Absent => {
            let scan = scan_frames(mmap, header_len, file_len)?;
            match scan.end {
                ScanEnd::Clean => {
                    warnings.push("closed file has no footer; index rebuilt by scan".into());
                    Ok((scan.entries, file_len, IndexSource::RebuiltMissingFooter))
                }
                ScanEnd::Tail { offset, detail } => Err(LogError::CorruptTail { offset, detail }),
            }
        }
    }
}
