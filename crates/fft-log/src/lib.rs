//! fftlog v2 codec — the frozen wire format of `docs/FFTLOG-V2.md` (M0 freeze 1).
//!
//! This crate owns framing and primitive codecs **only**: the file header
//! ([`fft_core::InstrumentMeta`] metadata block), 64-byte frame headers over zstd
//! payloads, the 32-byte event records with frame-relative timestamp deltas, the
//! checkpoint *section table and primitive section reader/writer* (section contents are
//! opaque bytes owned by other crates), the footer index, and §7/§8 commit + crash
//! recovery. Book/profile/engine internals never leak in here.
//!
//! Entry points: [`LogWriter`] (buffered appends, one flush per frame — the commit
//! point) and [`LogReader`] (memmap2 single cursor). Everything fails loudly with
//! byte offsets: recoverable deviations (live-tail recovery, index rebuilds) are
//! surfaced through [`OpenReport`], never silently absorbed.

#![deny(unsafe_code)] // one documented exception: the mmap call in `reader`.
#![warn(missing_docs)]

mod checkpoint;
mod error;
mod events;
mod footer;
mod frame;
mod header;
mod reader;
mod writer;

pub use checkpoint::{
    SECTION_BOOK, SECTION_CVD, SECTION_FLAG_OPTIONAL, SECTION_FLOW, SECTION_PROFILE,
    SECTION_REFRESH, SECTION_SESSION, Section, SectionRef,
};
pub use error::{LogError, Result};
pub use events::EVENT_RECORD_LEN;
pub use footer::{INDEX_ENTRY_LEN, INDEX_MAGIC, IndexEntry, TRAILER_LEN};
pub use frame::{
    FRAME_HEADER_LEN, FrameHeader, KIND_CHECKPOINT, KIND_EVENTS, MAX_COMPRESSED_LEN,
    MAX_UNCOMPRESSED_LEN,
};
pub use header::{FLAG_LIVE, MAGIC, SCHEMA_TAG, VERSION_MAJOR, VERSION_MINOR};
pub use reader::{EventsIter, IndexSource, LogReader, OpenReport, RefreshReport, TailRecovery};
pub use writer::LogWriter;
