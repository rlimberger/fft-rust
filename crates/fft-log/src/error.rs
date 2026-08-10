//! The one error type for the crate. Fail-loud: every variant carries enough context
//! (byte offsets where relevant) to locate the fault without re-running under a debugger.

use std::fmt;

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, LogError>;

/// Every way an fftlog v2 file can fail to be written or read. No variant is ever
/// swallowed internally; recoverable conditions (live tail, index rebuild) are reported
/// through [`crate::OpenReport`], not through `Ok`-with-degraded-data.
#[derive(Debug)]
pub enum LogError {
    /// Underlying I/O failure; `context` names the operation that failed.
    Io {
        /// Operation that failed (e.g. `"create log file"`).
        context: &'static str,
        /// The OS-level error.
        source: std::io::Error,
    },
    /// File too short to contain even a fixed-size file header.
    HeaderTruncated {
        /// Actual file length in bytes.
        file_len: u64,
    },
    /// File magic is not `"FFTLOG2\0"`.
    BadMagic {
        /// The eight bytes found at offset 0.
        found: [u8; 8],
    },
    /// `version_major` ≠ 2 — rejected loudly per the §2 compatibility policy.
    UnsupportedMajor {
        /// The major version found in the header.
        found: u16,
    },
    /// Source schema tag is not the frozen v2 value (`"mbo"`).
    UnsupportedSchemaTag {
        /// The schema tag found in the header metadata block.
        found: String,
    },
    /// File header checksum mismatch (`header_xxh3` over all preceding header bytes).
    HeaderChecksum,
    /// Malformed metadata block (§2 fixed-order encoding).
    BadMetadata {
        /// What was wrong.
        detail: String,
    },
    /// Frame header checksum mismatch on a frame the index or caller claims committed.
    FrameHeaderChecksum {
        /// File offset of the frame header.
        offset: u64,
    },
    /// Frame `kind` is neither EVENTS (1) nor CHECKPOINT (2).
    BadFrameKind {
        /// File offset of the frame header.
        offset: u64,
        /// The kind byte found.
        kind: u8,
    },
    /// Reserved frame-header bytes are non-zero.
    NonZeroReserved {
        /// File offset of the frame header.
        offset: u64,
    },
    /// A declared length exceeds its frozen ceiling (§3). Raised before any allocation.
    LimitExceeded {
        /// File offset of the frame header (0 on the write path).
        offset: u64,
        /// Which field violated its limit.
        field: &'static str,
        /// Declared value.
        value: u64,
        /// The frozen ceiling.
        max: u64,
    },
    /// Frame payload extends past the end of the committed frame region.
    FrameTruncated {
        /// File offset of the frame header.
        offset: u64,
        /// Bytes needed to complete the frame.
        needed: u64,
        /// Bytes actually available.
        available: u64,
    },
    /// Compressed payload checksum mismatch (`payload_xxh3`).
    PayloadChecksum {
        /// File offset of the frame header.
        offset: u64,
    },
    /// zstd decompression failed or produced a length ≠ `uncompressed_len`.
    BadPayload {
        /// File offset of the frame header.
        offset: u64,
        /// What was wrong.
        detail: String,
    },
    /// A 32-byte event record failed to decode (§4).
    BadEventRecord {
        /// File offset of the frame header containing the record.
        frame_offset: u64,
        /// Zero-based record index within the frame payload.
        index: u32,
        /// What was wrong.
        detail: String,
    },
    /// Checkpoint sections out of ascending-id order (§5).
    SectionOrder {
        /// File offset of the CHECKPOINT frame header (0 on the write path).
        frame_offset: u64,
        /// Previous section id.
        prev_id: u16,
        /// Offending section id.
        id: u16,
    },
    /// Checkpoint section header or bytes extend past the payload.
    SectionTruncated {
        /// File offset of the CHECKPOINT frame header.
        frame_offset: u64,
        /// Zero-based section index.
        index: u32,
    },
    /// Checkpoint section checksum mismatch (`section_xxh3`).
    SectionChecksum {
        /// File offset of the CHECKPOINT frame header.
        frame_offset: u64,
        /// Section id.
        id: u16,
    },
    /// Malformed checkpoint section (bad reserved field, trailing bytes, ...).
    BadSection {
        /// File offset of the CHECKPOINT frame header (0 on the write path).
        frame_offset: u64,
        /// What was wrong.
        detail: String,
    },
    /// A section id outside the frozen table (§5) without the OPTIONAL flag.
    UnknownRequiredSection {
        /// File offset of the CHECKPOINT frame header.
        frame_offset: u64,
        /// The unknown section id.
        id: u16,
    },
    /// Non-validating bytes at the tail of a **closed** file: corruption (§7).
    CorruptTail {
        /// File offset where validation first failed.
        offset: u64,
        /// What failed to validate.
        detail: String,
    },
    /// Footer/index damage where the frame chain cannot be proven intact, so a silent
    /// rebuild could drop committed frames (§6). Never downgraded to a warning.
    CorruptIndex {
        /// What was wrong.
        detail: String,
    },
    /// Frame index out of range for this log.
    FrameOutOfRange {
        /// Requested frame index.
        index: usize,
        /// Number of frames in the log.
        count: usize,
    },
    /// Frame at `index` has a different kind than the accessor requires.
    WrongFrameKind {
        /// Requested frame index.
        index: usize,
        /// Kind required by the accessor.
        expected: u8,
        /// Kind actually present.
        found: u8,
    },
    /// A single append would exceed a frozen frame limit (§3). Split the batch.
    OversizeAppend {
        /// Which limit would be exceeded.
        field: &'static str,
        /// The offending value.
        value: u64,
        /// The frozen ceiling.
        max: u64,
    },
}

impl fmt::Display for LogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use LogError::*;
        match self {
            Io { context, source } => write!(f, "I/O failure while trying to {context}: {source}"),
            HeaderTruncated { file_len } => {
                write!(
                    f,
                    "file too short for an fftlog v2 header ({file_len} bytes)"
                )
            }
            BadMagic { found } => write!(f, "bad file magic {found:02x?} (want \"FFTLOG2\\0\")"),
            UnsupportedMajor { found } => {
                write!(
                    f,
                    "unsupported fftlog major version {found} (reader supports 2)"
                )
            }
            UnsupportedSchemaTag { found } => write!(
                f,
                "unsupported source schema tag {found:?} (v2.0 requires \"mbo\")"
            ),
            HeaderChecksum => write!(f, "file header xxh3 checksum mismatch"),
            BadMetadata { detail } => write!(f, "malformed header metadata block: {detail}"),
            FrameHeaderChecksum { offset } => {
                write!(f, "frame header xxh3 mismatch at offset {offset}")
            }
            BadFrameKind { offset, kind } => {
                write!(f, "unknown frame kind {kind} at offset {offset}")
            }
            NonZeroReserved { offset } => {
                write!(f, "non-zero reserved frame-header bytes at offset {offset}")
            }
            LimitExceeded {
                offset,
                field,
                value,
                max,
            } => write!(
                f,
                "{field} = {value} exceeds frozen ceiling {max} (frame at offset {offset}); \
                 rejected before allocation"
            ),
            FrameTruncated {
                offset,
                needed,
                available,
            } => write!(
                f,
                "frame at offset {offset} needs {needed} bytes but only {available} remain"
            ),
            PayloadChecksum { offset } => {
                write!(f, "frame payload xxh3 mismatch at offset {offset}")
            }
            BadPayload { offset, detail } => {
                write!(f, "bad frame payload at offset {offset}: {detail}")
            }
            BadEventRecord {
                frame_offset,
                index,
                detail,
            } => write!(
                f,
                "bad event record {index} in frame at offset {frame_offset}: {detail}"
            ),
            SectionOrder {
                frame_offset,
                prev_id,
                id,
            } => write!(
                f,
                "checkpoint sections out of ascending-id order (… {prev_id}, {id}) \
                 in frame at offset {frame_offset}"
            ),
            SectionTruncated {
                frame_offset,
                index,
            } => write!(
                f,
                "checkpoint section {index} truncated in frame at offset {frame_offset}"
            ),
            SectionChecksum { frame_offset, id } => write!(
                f,
                "checkpoint section {id} xxh3 mismatch in frame at offset {frame_offset}"
            ),
            BadSection {
                frame_offset,
                detail,
            } => {
                write!(
                    f,
                    "bad checkpoint section in frame at offset {frame_offset}: {detail}"
                )
            }
            UnknownRequiredSection { frame_offset, id } => write!(
                f,
                "unknown REQUIRED checkpoint section id {id} in frame at offset {frame_offset}"
            ),
            CorruptTail { offset, detail } => write!(
                f,
                "corrupt tail on closed (non-LIVE) file at offset {offset}: {detail}"
            ),
            CorruptIndex { detail } => write!(
                f,
                "footer index corrupt and frame chain cannot be proven intact: {detail}"
            ),
            FrameOutOfRange { index, count } => {
                write!(
                    f,
                    "frame index {index} out of range (log has {count} frames)"
                )
            }
            WrongFrameKind {
                index,
                expected,
                found,
            } => write!(
                f,
                "frame {index} has kind {found}, accessor requires kind {expected}"
            ),
            OversizeAppend { field, value, max } => write!(
                f,
                "append would produce {field} = {value} > frozen ceiling {max}; split the batch"
            ),
        }
    }
}

impl std::error::Error for LogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LogError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl LogError {
    /// Wrap an I/O error with the operation that failed.
    pub(crate) fn io(context: &'static str) -> impl FnOnce(std::io::Error) -> LogError {
        move |source| LogError::Io { context, source }
    }
}
