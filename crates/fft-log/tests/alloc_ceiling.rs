//! §3 length ceilings are enforced **before allocation**: a frame header declaring
//! `uncompressed_len` > 64 MiB (with a valid header checksum, so it survives the
//! checksum gate) must be rejected without ever asking the allocator for it. A
//! watching global allocator proves no oversized allocation was attempted.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{es_meta, temp_path};
use fft_log::{
    FrameHeader, KIND_EVENTS, LogError, LogReader, LogWriter, MAX_COMPRESSED_LEN,
    MAX_UNCOMPRESSED_LEN,
};
use xxhash_rust::xxh3::xxh3_64;

/// Records the largest single allocation request seen.
struct WatchAlloc;

static MAX_REQUEST: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for WatchAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        MAX_REQUEST.fetch_max(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: WatchAlloc = WatchAlloc;

/// Write a LIVE header plus one hand-crafted frame with valid checksums but an
/// over-ceiling declared length.
fn craft(name: &str, mutate: impl FnOnce(&mut FrameHeader)) -> common::TempPath {
    let tmp = temp_path(name);
    let w = LogWriter::create(tmp.path(), &es_meta()).unwrap();
    drop(w); // stays LIVE; recovery scan will decode our crafted frame header
    let payload = zstd::bulk::compress(&[0u8; 64], 3).unwrap();
    let mut header = FrameHeader {
        kind: KIND_EVENTS,
        count: 2,
        compressed_len: payload.len() as u32,
        uncompressed_len: 64,
        first_ts: 0,
        last_ts: 0,
        first_seq: 0,
        last_seq: 0,
        payload_xxh3: xxh3_64(&payload),
    };
    mutate(&mut header);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(tmp.path())
        .unwrap();
    file.write_all(&header.encode()).unwrap(); // encode() computes a valid header_xxh3
    file.write_all(&payload).unwrap();
    tmp
}

#[test]
fn oversized_uncompressed_len_rejected_before_allocation() {
    let declared = u64::from(MAX_UNCOMPRESSED_LEN) + 1;
    let tmp = craft("alloc-uncompressed", |h| {
        h.uncompressed_len = declared as u32
    });

    MAX_REQUEST.store(0, Ordering::Relaxed);
    let err = LogReader::open(tmp.path()).unwrap_err();
    assert!(
        matches!(
            err,
            LogError::LimitExceeded { field: "uncompressed_len", value, .. } if value == declared
        ),
        "got {err}"
    );
    let max = MAX_REQUEST.load(Ordering::Relaxed);
    assert!(
        (max as u64) < declared,
        "an allocation of {max} bytes was attempted for a rejected frame"
    );
}

#[test]
fn oversized_compressed_len_rejected_before_allocation() {
    let declared = u64::from(MAX_COMPRESSED_LEN) + 1;
    let tmp = craft("alloc-compressed", |h| h.compressed_len = declared as u32);

    MAX_REQUEST.store(0, Ordering::Relaxed);
    let err = LogReader::open(tmp.path()).unwrap_err();
    assert!(
        matches!(
            err,
            LogError::LimitExceeded { field: "compressed_len", value, .. } if value == declared
        ),
        "got {err}"
    );
    let max = MAX_REQUEST.load(Ordering::Relaxed);
    assert!(
        (max as u64) < declared,
        "an allocation of {max} bytes was attempted"
    );
}
