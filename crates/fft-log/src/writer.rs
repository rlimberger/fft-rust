//! Append-only log writer (`docs/FFTLOG-V2.md` §2, §3, §7). Buffered appends — never
//! per-record syscalls — with one flush per frame, which is the §7 commit point.

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fft_core::{CanonicalEvent, InstrumentMeta};
use xxhash_rust::xxh3::xxh3_64;

use crate::checkpoint::{self, SectionRef};
use crate::error::{LogError, Result};
use crate::events::{self, EVENT_RECORD_LEN};
use crate::footer::{self, IndexEntry};
use crate::frame::{
    FRAME_HEADER_LEN, FrameHeader, KIND_CHECKPOINT, KIND_EVENTS, MAX_COMPRESSED_LEN,
    MAX_UNCOMPRESSED_LEN,
};
use crate::header::{self, FLAG_LIVE};

/// zstd compression level for all frame payloads (zstd's default).
const ZSTD_LEVEL: i32 = 3;

/// Single-writer, append-only fftlog v2 writer.
///
/// [`create`](LogWriter::create) writes the file header with the `LIVE` flag set; every
/// append writes one complete frame and flushes it (the commit point). Dropping the
/// writer without [`close`](LogWriter::close) leaves a `LIVE` file — exactly the state
/// crash recovery (§8) handles on the next open. [`close`](LogWriter::close) appends the
/// footer index and clears `LIVE`.
pub struct LogWriter {
    file: File,
    path: PathBuf,
    header_bytes: Vec<u8>,
    offset: u64,
    index: Vec<IndexEntry>,
    last_ts: u64,
    last_seq: u64,
}

impl LogWriter {
    /// Create a new log at `path` (refusing to overwrite an existing file) and write
    /// the §2 header with `LIVE` set.
    pub fn create(path: &Path, meta: &InstrumentMeta) -> Result<LogWriter> {
        let header_bytes = header::encode_header(meta, FLAG_LIVE)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(LogError::io("create log file"))?;
        file.write_all(&header_bytes)
            .map_err(LogError::io("write file header"))?;
        file.flush().map_err(LogError::io("flush file header"))?;
        let offset = header_bytes.len() as u64;
        Ok(LogWriter {
            file,
            path: path.to_path_buf(),
            header_bytes,
            offset,
            index: Vec::new(),
            last_ts: 0,
            last_seq: 0,
        })
    }

    /// Path this writer was created with.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Compress `payload`, then write and flush one committed frame. All appends funnel
    /// through here: header + payload in buffered writes, one flush at the end (§7).
    /// Parameters mirror the frozen §3 frame-header table one-to-one.
    #[allow(clippy::too_many_arguments)]
    fn write_frame(
        &mut self,
        kind: u8,
        count: u32,
        payload: &[u8],
        first_ts: u64,
        last_ts: u64,
        first_seq: u64,
        last_seq: u64,
    ) -> Result<()> {
        if payload.len() as u64 > u64::from(MAX_UNCOMPRESSED_LEN) {
            return Err(LogError::OversizeAppend {
                field: "uncompressed_len",
                value: payload.len() as u64,
                max: u64::from(MAX_UNCOMPRESSED_LEN),
            });
        }
        let compressed = zstd::bulk::compress(payload, ZSTD_LEVEL)
            .map_err(LogError::io("zstd-compress frame payload"))?;
        if compressed.len() as u64 > u64::from(MAX_COMPRESSED_LEN) {
            return Err(LogError::OversizeAppend {
                field: "compressed_len",
                value: compressed.len() as u64,
                max: u64::from(MAX_COMPRESSED_LEN),
            });
        }
        let frame_header = FrameHeader {
            kind,
            count,
            compressed_len: compressed.len() as u32,
            uncompressed_len: payload.len() as u32,
            first_ts,
            last_ts,
            first_seq,
            last_seq,
            payload_xxh3: xxh3_64(&compressed),
        };
        // One buffered write per frame — header and payload assembled in memory,
        // never per-record syscalls.
        let mut frame_bytes = Vec::with_capacity(FRAME_HEADER_LEN + compressed.len());
        frame_bytes.extend_from_slice(&frame_header.encode());
        frame_bytes.extend_from_slice(&compressed);
        self.file
            .write_all(&frame_bytes)
            .map_err(LogError::io("append frame"))?;
        // §7 commit point: header + payload written, then flushed.
        self.file.flush().map_err(LogError::io("flush frame"))?;
        self.index.push(IndexEntry {
            offset: self.offset,
            kind,
            first_ts,
            first_seq,
        });
        self.offset += frame_bytes.len() as u64;
        Ok(())
    }

    /// Append one EVENTS frame holding `events` (§3, §4). Empty batches are legal and
    /// produce an empty frame. Batches whose worst-case encoding (every event preceded
    /// by a TsReset) would exceed the 64 MiB uncompressed ceiling are refused before
    /// encoding — split them.
    pub fn append_events(&mut self, events: &[CanonicalEvent]) -> Result<()> {
        let worst_case = events.len() as u64 * 2 * EVENT_RECORD_LEN as u64;
        if worst_case > u64::from(MAX_UNCOMPRESSED_LEN) {
            return Err(LogError::OversizeAppend {
                field: "uncompressed_len (worst case)",
                value: worst_case,
                max: u64::from(MAX_UNCOMPRESSED_LEN),
            });
        }
        let enc = events::encode_events(events);
        self.write_frame(
            KIND_EVENTS,
            enc.count,
            &enc.payload,
            enc.first_ts,
            enc.last_ts,
            enc.first_seq,
            enc.last_seq,
        )?;
        if !events.is_empty() {
            self.last_ts = enc.last_ts;
            self.last_seq = enc.last_seq;
        }
        Ok(())
    }

    /// Append one CHECKPOINT frame (§5). Sections must be in strictly ascending id
    /// order; contents are opaque to `fft-log`. The frame's ts/seq bounds are stamped
    /// with the last appended event's ts/seq (zero before any event) so the index can
    /// seek checkpoints in event time.
    pub fn write_checkpoint<'a>(
        &mut self,
        sections: impl IntoIterator<Item = SectionRef<'a>>,
    ) -> Result<()> {
        let (payload, count) = checkpoint::encode_sections(sections)?;
        let (ts, seq) = (self.last_ts, self.last_seq);
        self.write_frame(KIND_CHECKPOINT, count, &payload, ts, ts, seq, seq)
    }

    /// Cleanly close the log (§6): append the footer index and trailer, then clear the
    /// header `LIVE` flag and sync to disk. Consumes the writer.
    pub fn close(mut self) -> Result<()> {
        let footer_bytes = footer::encode_footer(&self.index);
        self.file
            .write_all(&footer_bytes)
            .map_err(LogError::io("append footer"))?;
        self.file.flush().map_err(LogError::io("flush footer"))?;
        // Clear LIVE by rewriting the header with a recomputed checksum. This is the
        // only in-place write in the format and is mandated by §2/§6; frame bytes are
        // never overwritten.
        let closed_header = header::with_live_cleared(&self.header_bytes);
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(LogError::io("seek to header"))?;
        self.file
            .write_all(&closed_header)
            .map_err(LogError::io("clear LIVE flag"))?;
        self.file
            .sync_all()
            .map_err(LogError::io("sync closed log"))?;
        Ok(())
    }
}
