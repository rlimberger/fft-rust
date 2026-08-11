//! Structural garbage and hand-crafted extreme frame headers.

use fft_log::{
    FrameHeader, KIND_CHECKPOINT, KIND_EVENTS, LogWriter, MAX_COMPRESSED_LEN, MAX_UNCOMPRESSED_LEN,
};
use xxhash_rust::xxh3::xxh3_64;

use crate::common::es_meta;
use crate::common::temp_path;
use crate::harness::{Finding, run_one};

/// Structural garbage: empty, random short blobs, magic-only, all-0xFF of various sizes.
pub(crate) fn mut_garbage(findings: &mut Vec<Finding>) -> u64 {
    let mut n = 0u64;
    let cases: Vec<(&[u8], &str)> = vec![
        (&[][..], "empty"),
        (b"FFTLOG2\0", "magic_only"),
        (b"FFTLOG2\0\x02\x00\x00\x00", "magic_plus_version"),
        (&[0u8; 64][..], "zeros_64"),
        (&[0xffu8; 128][..], "ff_128"),
        (b"NotALog!!!!!!!!", "bad_magic_ascii"),
    ];
    for (bytes, name) in cases {
        let recipe = format!("garbage kind={name}");
        if let Some(f) = run_one(bytes, &recipe) {
            findings.push(f);
        }
        n += 1;
    }
    // Growing zero-filled files (header-size range).
    for len in [1usize, 8, 20, 28, 64, 128, 256, 512, 1024] {
        let bytes = vec![0u8; len];
        let recipe = format!("garbage zeros len={len}");
        if let Some(f) = run_one(&bytes, &recipe) {
            findings.push(f);
        }
        n += 1;
    }
    n
}

/// Hand-crafted frame headers with valid checksums but extreme count/length fields
/// (extends the LOG-HARDEN / alloc_ceiling surface under the full reader path).
pub(crate) fn mut_crafted_extreme_headers(findings: &mut Vec<Finding>) -> u64 {
    let mut n = 0u64;
    let payload = zstd::bulk::compress(&[0u8; 32], 3).unwrap_or_else(|_| vec![0; 16]);

    let base = FrameHeader {
        kind: KIND_EVENTS,
        count: 1,
        compressed_len: payload.len() as u32,
        uncompressed_len: 32,
        first_ts: 0,
        last_ts: 0,
        first_seq: 0,
        last_seq: 0,
        payload_xxh3: xxh3_64(&payload),
    };

    let mut variants: Vec<(&str, FrameHeader)> = Vec::with_capacity(9);
    {
        let mut h = base;
        h.count = u32::MAX;
        variants.push(("count_max", h));
    }
    {
        let mut h = base;
        h.count = 0;
        variants.push(("count_zero_with_payload", h));
    }
    {
        let mut h = base;
        h.compressed_len = MAX_COMPRESSED_LEN;
        variants.push(("compressed_at_ceiling", h));
    }
    {
        let mut h = base;
        h.uncompressed_len = MAX_UNCOMPRESSED_LEN;
        variants.push(("uncompressed_at_ceiling", h));
    }
    {
        let mut h = base;
        h.compressed_len = MAX_COMPRESSED_LEN + 1;
        variants.push(("compressed_over_ceiling", h));
    }
    {
        let mut h = base;
        h.uncompressed_len = MAX_UNCOMPRESSED_LEN + 1;
        variants.push(("uncompressed_over_ceiling", h));
    }
    {
        let mut h = base;
        h.compressed_len = 0;
        h.uncompressed_len = 0;
        variants.push(("both_lens_zero", h));
    }
    {
        let mut h = base;
        h.kind = KIND_EVENTS;
        h.count = 1_000_000;
        h.uncompressed_len = 32; // mismatch vs count — BadPayload, not panic
        variants.push(("kind_events_count_huge", h));
    }
    {
        let mut h = base;
        h.kind = KIND_CHECKPOINT;
        h.count = 1_000_000;
        h.uncompressed_len = payload.len() as u32;
        variants.push(("kind_checkpoint_count_huge", h));
    }

    for (name, mut header) in variants {
        let tmp = temp_path("craft-hdr");
        // LIVE header only — recovery scan must decode our crafted frame.
        let w = LogWriter::create(tmp.path(), &es_meta()).expect("create");
        drop(w);
        // Recompute payload_xxh3 only when compressed_len still matches payload.
        if header.compressed_len as usize == payload.len() {
            header.payload_xxh3 = xxh3_64(&payload);
        }
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(tmp.path())
            .expect("open");
        use std::io::Write;
        file.write_all(&header.encode()).expect("hdr");
        // Write min(payload, compressed_len) so truncated-payload path is hit too.
        let write_n = (header.compressed_len as usize).min(payload.len());
        file.write_all(&payload[..write_n]).expect("payload");
        // Over-ceiling / at-ceiling compressed_len: do NOT allocate 16 MiB padding.
        // Leave file short → recovery sees truncated payload as uncommitted tail.
        drop(file);
        let bytes = std::fs::read(tmp.path()).expect("read");
        let recipe = format!("crafted_header name={name}");
        if let Some(f) = run_one(&bytes, &recipe) {
            findings.push(f);
        }
        n += 1;
    }
    n
}
