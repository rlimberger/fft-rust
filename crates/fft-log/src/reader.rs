//! Log reader (`docs/FFTLOG-V2.md` §6–§8): memmap2 single-cursor access, footer index
//! with rebuild-by-scan fallback, and crash recovery for `LIVE` files.
//!
//! Recovery/rebuild policy (fail-loud, §7–§8):
//! - `LIVE` flag set ⇒ unclean shutdown. Scan to the last committed frame, logically
//!   truncate, and surface a [`TailRecovery`] in the [`OpenReport`] — always, even when
//!   nothing was dropped.
//! - Closed file, valid footer ⇒ use it.
//! - Closed file, footer magic present but index invalid ⇒ rebuild by scanning, but
//!   only when `index_len` still delimits the frame region exactly and every frame
//!   validates; surfaced as a warning. Otherwise a loud [`LogError::CorruptIndex`] —
//!   never a rebuild that could silently drop committed frames.
//! - Closed file, no footer ⇒ rebuild by scanning; any non-validating byte is a loud
//!   [`LogError::CorruptTail`] (only `LIVE` files get tail forgiveness).

use std::fs::File;
use std::ops::Range;
use std::path::{Path, PathBuf};

use fft_core::{CanonicalEvent, InstrumentMeta, Ts};
use memmap2::Mmap;
use xxhash_rust::xxh3::xxh3_64;

use crate::checkpoint::{self, Section};
use crate::error::{LogError, Result};
use crate::events;
use crate::footer::{self, FooterProbe, IndexEntry};
use crate::frame::{FRAME_HEADER_LEN, FrameHeader, KIND_CHECKPOINT};
use crate::header::{self, FLAG_LIVE, HeaderInfo, VERSION_MAJOR};

/// Crash-recovery result for a `LIVE` file (§8). Surfaced through [`OpenReport`]; the
/// caller must see it — recovery is never silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TailRecovery {
    /// Bytes after the last committed frame that were logically truncated.
    pub dropped_bytes: u64,
    /// `(last_ts, last_seq)` of the last committed frame; `None` if no frame committed.
    pub last_good: Option<(Ts, u64)>,
}

/// Where the frame index came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexSource {
    /// Valid footer on a cleanly closed file.
    Footer,
    /// Closed file without a footer; index rebuilt by walking frame headers (§6).
    RebuiltMissingFooter,
    /// Footer present but its index failed validation; frame chain proved intact and
    /// the index was rebuilt by scanning (§6, with a visible warning).
    RebuiltCorruptIndex,
    /// `LIVE` file; index built by the §8 crash-recovery scan.
    LiveRecovery,
}

/// Everything the caller must know about how the file was opened. Warnings are
/// human-readable and always paired with a machine-readable field here.
#[derive(Debug)]
pub struct OpenReport {
    /// Provenance of the frame index.
    pub index_source: IndexSource,
    /// Present iff the file was `LIVE` (crash recovery ran).
    pub recovery: Option<TailRecovery>,
    /// Visible warnings (index rebuilds, recovery details). Never empty when the open
    /// deviated from the clean fast path.
    pub warnings: Vec<String>,
}

/// Result of [`LogReader::refresh`] for a concurrently growing (or just-closed) log (§7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefreshReport {
    /// Fully committed frames discovered since the previous open/refresh.
    pub new_frames: usize,
    /// Whether the on-disk header still has the `LIVE` flag after this refresh.
    pub live: bool,
}

/// Memory-mapped fftlog v2 reader. One `Mmap` is the single cursor; all frame and
/// payload access is bounds-checked slicing into it. Hold the path so
/// [`LogReader::refresh`] can remmap a growing LIVE file (§7 concurrent tail).
pub struct LogReader {
    path: PathBuf,
    mmap: Mmap,
    header: HeaderInfo,
    index: Vec<IndexEntry>,
    /// End of the committed frame region (start of footer, or recovery truncation point).
    frames_end: u64,
}

impl std::fmt::Debug for LogReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogReader")
            .field("path", &self.path)
            .field("symbol", &self.header.meta.symbol)
            .field("frames", &self.index.len())
            .field("frames_end", &self.frames_end)
            .finish_non_exhaustive()
    }
}

/// Read-only mapping of the log file. The **only** unsafe block in the crate.
#[allow(unsafe_code)]
fn map_file(file: &File) -> Result<Mmap> {
    // SAFETY: standard memmap2 read-only mapping. The safety contract is that the file
    // is not truncated or mutated in place while mapped; the fftlog format is
    // append-only (§7 — committed bytes are never overwritten) and the writer's only
    // in-place write is the header LIVE flag on close, which readers of a live file
    // re-validate by checksum. External truncation of a mapped file is undefined
    // behaviour we accept for same-host tailing, as specified.
    unsafe { Mmap::map(file) }.map_err(LogError::io("mmap log file"))
}

/// How a frame-chain scan ended.
enum ScanEnd {
    /// Consumed exactly to the region end.
    Clean,
    /// Non-validating bytes from `offset` (torn header, torn payload, checksum miss).
    Tail { offset: u64, detail: String },
}

/// Result of walking the frame chain over `[start, end)`.
struct Scan {
    entries: Vec<IndexEntry>,
    /// `(last_ts, last_seq)` of the last committed frame.
    last_good: Option<(u64, u64)>,
    end: ScanEnd,
}

/// Walk frame headers over `data[start..end)`, validating both checksums of every
/// frame (§7 commit rule). Returns `Err` only for *hard* corruption: a frame header
/// whose checksum validates but whose contents are illegal (limit breach, bad kind,
/// non-zero reserved) can never be a torn write and is loud everywhere.
fn scan_frames(data: &[u8], start: u64, end: u64) -> Result<Scan> {
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
fn resolve_closed_index(
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

impl LogReader {
    /// Open a log and report how (§6–§8). See the module docs for the full policy.
    pub fn open(path: &Path) -> Result<(LogReader, OpenReport)> {
        let file = File::open(path).map_err(LogError::io("open log file"))?;
        let file_len = file
            .metadata()
            .map_err(LogError::io("stat log file"))?
            .len();
        if file_len == 0 {
            return Err(LogError::HeaderTruncated { file_len });
        }
        let mmap = map_file(&file)?;
        let header = header::decode_header(&mmap)?;
        let header_len = header.header_len;
        let mut warnings = Vec::new();

        let (index, frames_end, index_source, recovery) = if header.flags & FLAG_LIVE != 0 {
            // §8 crash recovery: LIVE on open means unclean shutdown.
            let scan = scan_frames(&mmap, header_len, file_len)?;
            let (frames_end, dropped_bytes) = match scan.end {
                ScanEnd::Clean => (file_len, 0),
                ScanEnd::Tail { offset, detail } => {
                    warnings.push(format!(
                        "live file: uncommitted tail at offset {offset} ({detail}); \
                         logically truncated {} bytes",
                        file_len - offset
                    ));
                    (offset, file_len - offset)
                }
            };
            let recovery = TailRecovery {
                dropped_bytes,
                last_good: scan.last_good.map(|(ts, seq)| (Ts(ts), seq)),
            };
            (
                scan.entries,
                frames_end,
                IndexSource::LiveRecovery,
                Some(recovery),
            )
        } else {
            let (index, frames_end, index_source) =
                resolve_closed_index(&mmap, header_len, file_len, &mut warnings)?;
            (index, frames_end, index_source, None)
        };

        let reader = LogReader {
            path: path.to_path_buf(),
            mmap,
            header,
            index,
            frames_end,
        };
        Ok((
            reader,
            OpenReport {
                index_source,
                recovery,
                warnings,
            },
        ))
    }

    /// Remap the file and pick up newly committed frames (§7 concurrent LIVE tail).
    ///
    /// While the header still has `LIVE` set, a non-validating tail is **not** fatal:
    /// those bytes are treated as not-yet-committed and left for a later refresh. Hard
    /// corruption (checksum-valid illegal frame headers) still fails loudly.
    ///
    /// After a clean close (`LIVE` cleared, footer written) this switches to the closed
    /// index path so the reader sees the full footer index.
    ///
    /// Wire bytes are untouched; this is remap + rescan only.
    pub fn refresh(&mut self) -> Result<RefreshReport> {
        let file = File::open(&self.path).map_err(LogError::io("open log file for refresh"))?;
        let file_len = file
            .metadata()
            .map_err(LogError::io("stat log file for refresh"))?
            .len();
        if file_len == 0 {
            return Err(LogError::HeaderTruncated { file_len });
        }
        let mmap = map_file(&file)?;
        let header = header::decode_header(&mmap)?;
        if header.header_len != self.header.header_len {
            return Err(LogError::BadMetadata {
                detail: format!(
                    "header length changed on refresh (was {}, now {})",
                    self.header.header_len, header.header_len
                ),
            });
        }

        let prev_count = self.index.len();
        let live = header.flags & FLAG_LIVE != 0;

        if live {
            // Append-only growth: never accept a shrink past the committed region.
            if file_len < self.frames_end {
                return Err(LogError::CorruptTail {
                    offset: file_len,
                    detail: format!(
                        "LIVE file shrank below committed frames_end {} (now {file_len})",
                        self.frames_end
                    ),
                });
            }
            let scan = scan_frames(&mmap, self.frames_end, file_len)?;
            // Partial/torn final frame → stop at last committed; retry later (§7).
            let frames_end = match scan.end {
                ScanEnd::Clean => file_len,
                ScanEnd::Tail { offset, .. } => offset,
            };
            self.index.extend(scan.entries);
            self.frames_end = frames_end;
            self.mmap = mmap;
            self.header = header;
            Ok(RefreshReport {
                new_frames: self.index.len() - prev_count,
                live: true,
            })
        } else {
            // Clean close: footer (or rebuild) is authoritative for the full index.
            let mut warnings = Vec::new();
            let (index, frames_end, _) =
                resolve_closed_index(&mmap, header.header_len, file_len, &mut warnings)?;
            self.index = index;
            self.frames_end = frames_end;
            self.mmap = mmap;
            self.header = header;
            Ok(RefreshReport {
                new_frames: self.index.len().saturating_sub(prev_count),
                live: false,
            })
        }
    }

    /// Instrument metadata from the file header (§2).
    pub fn meta(&self) -> &InstrumentMeta {
        &self.header.meta
    }

    /// `(version_major, version_minor)` from the file header.
    pub fn version(&self) -> (u16, u16) {
        (VERSION_MAJOR, self.header.version_minor)
    }

    /// Source schema tag from the header metadata block (`"mbo"`).
    pub fn schema_tag(&self) -> &str {
        &self.header.schema_tag
    }

    /// Whether the on-disk header currently has the `LIVE` flag (updated by refresh).
    pub fn is_live(&self) -> bool {
        self.header.flags & FLAG_LIVE != 0
    }

    /// Whether the on-disk header had the `LIVE` flag set when last opened/refreshed.
    pub fn was_live(&self) -> bool {
        self.is_live()
    }

    /// The frame index: one entry per committed frame, in file order.
    pub fn index(&self) -> &[IndexEntry] {
        &self.index
    }

    /// Number of committed frames.
    pub fn frame_count(&self) -> usize {
        self.index.len()
    }

    /// Decode and validate the frame header of frame `index`.
    pub fn frame_header(&self, index: usize) -> Result<FrameHeader> {
        let entry = *self.index.get(index).ok_or(LogError::FrameOutOfRange {
            index,
            count: self.index.len(),
        })?;
        let start = entry.offset;
        let end = start + FRAME_HEADER_LEN as u64;
        if end > self.frames_end {
            return Err(LogError::FrameTruncated {
                offset: start,
                needed: FRAME_HEADER_LEN as u64,
                available: self.frames_end.saturating_sub(start),
            });
        }
        let bytes: &[u8; FRAME_HEADER_LEN] =
            self.mmap[start as usize..end as usize].try_into().unwrap();
        FrameHeader::decode(bytes, start)
    }

    /// Validate checksums, then decompress the payload of frame `index`. Never
    /// allocates more than the (already ceiling-checked) declared `uncompressed_len`.
    fn frame_payload(&self, index: usize) -> Result<(FrameHeader, u64, Vec<u8>)> {
        let fh = self.frame_header(index)?;
        let offset = self.index[index].offset;
        let start = offset + FRAME_HEADER_LEN as u64;
        let end = start + u64::from(fh.compressed_len);
        if end > self.frames_end {
            return Err(LogError::FrameTruncated {
                offset,
                needed: FRAME_HEADER_LEN as u64 + u64::from(fh.compressed_len),
                available: self.frames_end.saturating_sub(offset),
            });
        }
        let compressed = &self.mmap[start as usize..end as usize];
        if xxh3_64(compressed) != fh.payload_xxh3 {
            return Err(LogError::PayloadChecksum { offset });
        }
        let payload =
            zstd::bulk::decompress(compressed, fh.uncompressed_len as usize).map_err(|e| {
                LogError::BadPayload {
                    offset,
                    detail: e.to_string(),
                }
            })?;
        if payload.len() != fh.uncompressed_len as usize {
            return Err(LogError::BadPayload {
                offset,
                detail: format!(
                    "decompressed to {} bytes, header declares {}",
                    payload.len(),
                    fh.uncompressed_len
                ),
            });
        }
        Ok((fh, offset, payload))
    }

    /// Read the sections of CHECKPOINT frame `index` (§5). Unknown OPTIONAL sections
    /// are skipped; unknown REQUIRED sections are a loud error.
    pub fn read_checkpoint(&self, index: usize) -> Result<Vec<Section>> {
        let (fh, offset, payload) = self.frame_payload(index)?;
        if fh.kind != KIND_CHECKPOINT {
            return Err(LogError::WrongFrameKind {
                index,
                expected: KIND_CHECKPOINT,
                found: fh.kind,
            });
        }
        checkpoint::decode_sections(&payload, fh.count, offset)
    }

    /// Iterate the events of a frame range, decoded to absolute-timestamp
    /// [`CanonicalEvent`]s. CHECKPOINT frames inside the range are skipped. The
    /// iterator yields the first error it hits and then fuses.
    pub fn events(&self, frames: Range<usize>) -> EventsIter<'_> {
        EventsIter {
            reader: self,
            frames,
            current: Vec::new().into_iter(),
            failed: false,
        }
    }

    /// Decode EVENTS frame `index`, or `None` if it is a CHECKPOINT frame.
    fn decode_events_frame(&self, index: usize) -> Result<Option<Vec<CanonicalEvent>>> {
        let (fh, offset, payload) = self.frame_payload(index)?;
        if fh.kind == KIND_CHECKPOINT {
            return Ok(None);
        }
        events::decode_events(&payload, fh.count, fh.first_ts, offset).map(Some)
    }
}

/// Iterator over decoded events in a frame range; see [`LogReader::events`].
pub struct EventsIter<'a> {
    reader: &'a LogReader,
    frames: Range<usize>,
    current: std::vec::IntoIter<CanonicalEvent>,
    failed: bool,
}

impl Iterator for EventsIter<'_> {
    type Item = Result<CanonicalEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        loop {
            if let Some(e) = self.current.next() {
                return Some(Ok(e));
            }
            let frame = self.frames.next()?;
            match self.reader.decode_events_frame(frame) {
                Ok(Some(events)) => self.current = events.into_iter(),
                Ok(None) => continue, // checkpoint frame inside the range
                Err(e) => {
                    self.failed = true;
                    return Some(Err(e));
                }
            }
        }
    }
}
