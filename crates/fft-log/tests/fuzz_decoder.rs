//! Adversarial corpus + structure-aware fuzz harness for the fftlog decoder (M7 LOG-FUZZ).
//!
//! **Security contract under test:** for arbitrary input bytes, `LogReader::open` + full
//! iteration (frames, events, checkpoints, footer/index paths) must never panic, hang,
//! overflow, or allocate unboundedly. Every malformed input becomes a typed `LogError`
//! (loud). Recovery follows frozen FFTLOG-V2 commit rules only.
//!
//! Default `cargo test -p fft-log` leaves this file's long run ignored. Full corpus:
//!
//! ```text
//! cargo test -p fft-log --test fuzz_decoder -- --ignored --nocapture
//! ```
//!
//! Budget: ~2–5 minutes wall time. Seeds ≤ 2 KiB get exhaustive bit-flips, truncations,
//! and length-field extremes; larger seeds stride (documented in run output). Every
//! finding prints a standalone mutation recipe for a one-shot repro test.
//!
//! `cargo-fuzz` / libFuzzer: not required. This harness is in-tree and dependency-free
//! beyond the crate's existing test surface. A `fuzz/` cargo-fuzz target was not added
//! because `cargo fuzz` is not installed on this host (see track report).

mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::time::Instant;

use common::{es_meta, mono_events, temp_path};
use fft_core::CanonicalEvent;
use fft_log::{
    FRAME_HEADER_LEN, IndexSource, KIND_CHECKPOINT, KIND_EVENTS, LogReader, LogWriter,
    SECTION_BOOK, SECTION_FLAG_OPTIONAL, SECTION_PROFILE, SectionRef, TRAILER_LEN,
};

// ─── Seed corpus ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
enum SeedKind {
    EventsOnly,
    WithCheckpoint,
    LiveTornTail,
    WithGaps,
}

impl SeedKind {
    fn name(self) -> &'static str {
        match self {
            SeedKind::EventsOnly => "events_only",
            SeedKind::WithCheckpoint => "with_checkpoint",
            SeedKind::LiveTornTail => "live_torn_tail",
            SeedKind::WithGaps => "with_gaps",
        }
    }
}

/// Build 4 small valid logs via `LogWriter` (events-only; checkpoint; LIVE torn-tail;
/// gap records). Returns `(kind, bytes)`.
///
/// Sizes are intentionally small (hundreds of bytes) so exhaustive bit-flips and
/// truncations finish well under the 2–5 min budget while still covering every
/// decoder path. Larger production logs are already exercised by M1/M2 gates.
fn build_seed_corpus() -> Vec<(SeedKind, Vec<u8>)> {
    let mut out = Vec::with_capacity(4);

    // 1. Events-only closed log: two small EVENTS frames.
    {
        let tmp = temp_path("seed-events");
        let bytes = common::write_closed(
            tmp.path(),
            &[mono_events(8, 1_000, 1), mono_events(8, 10_000, 9)],
        );
        out.push((SeedKind::EventsOnly, bytes));
    }

    // 2. Events + checkpoint + events, closed.
    {
        let tmp = temp_path("seed-ckpt");
        let batch = mono_events(6, 5_000, 1);
        let book = vec![0xB0u8; 48];
        let profile = vec![0x9Fu8; 24];
        let mut w = LogWriter::create(tmp.path(), &es_meta()).expect("create");
        w.append_events(&batch).expect("events");
        w.write_checkpoint([
            SectionRef {
                id: SECTION_BOOK,
                version: 1,
                flags: 0,
                bytes: &book,
            },
            SectionRef {
                id: SECTION_PROFILE,
                version: 1,
                flags: SECTION_FLAG_OPTIONAL,
                bytes: &profile,
            },
        ])
        .expect("checkpoint");
        w.append_events(&batch).expect("events2");
        w.close().expect("close");
        out.push((
            SeedKind::WithCheckpoint,
            std::fs::read(tmp.path()).expect("read"),
        ));
    }

    // 3. LIVE torn-tail: two committed frames, then a partial third (unclean crash).
    {
        let tmp = temp_path("seed-live");
        let batches = [
            mono_events(6, 1_000, 1),
            mono_events(6, 50_000, 7),
            mono_events(6, 100_000, 13),
        ];
        let full = common::write_live(tmp.path(), &batches);
        // Locate final frame start via a clean open of the full LIVE file, then cut mid-frame.
        let open_tmp = temp_path("seed-live-open");
        std::fs::write(open_tmp.path(), &full).expect("write");
        let (reader, _) = LogReader::open(open_tmp.path()).expect("open live");
        assert_eq!(reader.frame_count(), 3);
        let final_off = reader.index()[2].offset as usize;
        drop(reader);
        // Keep half of the final frame as uncommitted tail.
        let cut = final_off + (full.len() - final_off) / 2;
        out.push((SeedKind::LiveTornTail, full[..cut].to_vec()));
    }

    // 4. Closed log whose event stream includes Gap records (mono_events inserts one).
    {
        let tmp = temp_path("seed-gaps");
        // n > 4 guarantees a Gap at n/2 inside mono_events.
        let bytes = common::write_closed(
            tmp.path(),
            &[mono_events(12, 1_000, 1), mono_events(12, 200_000, 20)],
        );
        out.push((SeedKind::WithGaps, bytes));
    }

    out
}

// ─── Full read surface ───────────────────────────────────────────────────────

/// Exercise every public decode path on `path`. Returns `Ok(())` on typed success or
/// typed `LogError`; panics (caught by the harness) are the only findings.
fn exercise_full_surface(path: &Path) -> Result<(), String> {
    match LogReader::open(path) {
        Err(e) => {
            // Typed error is the contract. Display + Debug must not panic either.
            let _ = format!("{e}");
            let _ = format!("{e:?}");
            Ok(())
        }
        Ok((reader, report)) => {
            let _ = format!("{report:?}");
            let _ = format!("{reader:?}");
            let _ = reader.meta();
            let _ = reader.version();
            let _ = reader.schema_tag();
            let _ = reader.opened_live();
            let _ = reader.is_live();
            let n = reader.frame_count();
            let _ = reader.index();

            for i in 0..n {
                match reader.frame_header(i) {
                    Ok(fh) => {
                        let _ = format!("{fh:?}");
                        if fh.kind == KIND_CHECKPOINT {
                            match reader.read_checkpoint(i) {
                                Ok(sections) => {
                                    let _ = sections.len();
                                }
                                Err(e) => {
                                    let _ = format!("{e}");
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = format!("{e}");
                    }
                }
            }

            // Full event iteration (skips checkpoints; first error fuses).
            let mut count = 0usize;
            for item in reader.events(0..n) {
                match item {
                    Ok(_ev) => count += 1,
                    Err(e) => {
                        let _ = format!("{e}");
                        break;
                    }
                }
                // Hard ceiling against unbounded decode (malformed count / payload).
                if count > 10_000_000 {
                    return Err(format!(
                        "event iteration exceeded 10M events (possible unbounded decode); \
                         frames={n} index_source={:?}",
                        report.index_source
                    ));
                }
            }

            // refresh() on a just-opened path (no concurrent writer) must also be safe.
            // We cannot mutably refresh through the immutable path above without re-open;
            // re-open + refresh covers that surface.
            drop(reader);
            if let Ok((mut r2, _)) = LogReader::open(path) {
                match r2.refresh() {
                    Ok(rr) => {
                        let _ = format!("{rr:?}");
                    }
                    Err(e) => {
                        let _ = format!("{e}");
                    }
                }
            }
            Ok(())
        }
    }
}

/// Write `bytes` to a temp path, run the full surface inside `catch_unwind`, return a
/// finding description if the contract is violated.
fn run_one(bytes: &[u8], recipe: &str) -> Option<Finding> {
    let tmp = temp_path("fuzz-case");
    if let Err(e) = std::fs::write(tmp.path(), bytes) {
        return Some(Finding {
            recipe: recipe.to_string(),
            detail: format!("test harness failed to write temp file: {e}"),
            severity: Severity::Sev2,
        });
    }

    let path = tmp.path().to_path_buf();
    let result = catch_unwind(AssertUnwindSafe(|| exercise_full_surface(&path)));
    match result {
        Ok(Ok(())) => None,
        Ok(Err(msg)) => Some(Finding {
            recipe: recipe.to_string(),
            detail: msg,
            severity: Severity::Sev1,
        }),
        Err(payload) => {
            let panic_msg = panic_payload_to_string(payload);
            Some(Finding {
                recipe: recipe.to_string(),
                detail: format!("PANIC: {panic_msg}"),
                severity: Severity::Sev1,
            })
        }
    }
}

fn panic_payload_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".into()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    /// Decoder panic / hang / unbounded alloc on arbitrary bytes — M7 zero-defect gate.
    Sev1,
    /// Data-integrity or contract ambiguity short of panic.
    Sev2,
}

#[derive(Debug)]
struct Finding {
    recipe: String,
    detail: String,
    severity: Severity,
}

// ─── Mutation engines ────────────────────────────────────────────────────────

/// Bit-flip every byte (or every `stride`-th byte) of `seed`.
fn mut_bit_flips(seed: &[u8], seed_name: &str, stride: usize, findings: &mut Vec<Finding>) -> u64 {
    let mut n = 0u64;
    let mut i = 0usize;
    while i < seed.len() {
        for bit in 0..8u8 {
            let mut m = seed.to_vec();
            m[i] ^= 1u8 << bit;
            let recipe = format!("bit_flip seed={seed_name} offset={i} bit={bit}");
            if let Some(f) = run_one(&m, &recipe) {
                findings.push(f);
            }
            n += 1;
        }
        i = i.saturating_add(stride.max(1));
    }
    n
}

/// Truncate seed at every offset (or strided).
fn mut_truncations(
    seed: &[u8],
    seed_name: &str,
    stride: usize,
    findings: &mut Vec<Finding>,
) -> u64 {
    let mut n = 0u64;
    let mut cut = 0usize;
    while cut <= seed.len() {
        let recipe = format!("truncate seed={seed_name} cut={cut}");
        if let Some(f) = run_one(&seed[..cut], &recipe) {
            findings.push(f);
        }
        n += 1;
        if cut == seed.len() {
            break;
        }
        cut = (cut + stride.max(1)).min(seed.len());
        if cut < seed.len() && cut + stride > seed.len() {
            // Always hit the exact full length as the last step when striding.
            continue;
        }
    }
    n
}

/// Overwrite multi-byte length-like fields at known offsets with extremes.
/// Heuristic: every aligned u32/u64 window in the file (small seeds only).
fn mut_length_extremes(seed: &[u8], seed_name: &str, findings: &mut Vec<Finding>) -> u64 {
    let extremes_u32: &[u32] = &[
        0,
        1,
        u32::MAX,
        u32::MAX - 1,
        0x7FFF_FFFF,
        16 * 1024 * 1024 + 1,
    ];
    let extremes_u64: &[u64] = &[0, 1, u64::MAX, u64::MAX / 2];
    let mut n = 0u64;

    // u32 windows at every 4-byte alignment.
    let mut off = 0usize;
    while off + 4 <= seed.len() {
        for &v in extremes_u32 {
            let mut m = seed.to_vec();
            m[off..off + 4].copy_from_slice(&v.to_le_bytes());
            let recipe = format!("len_u32 seed={seed_name} offset={off} value={v:#x}");
            if let Some(f) = run_one(&m, &recipe) {
                findings.push(f);
            }
            n += 1;
        }
        off += 4;
    }

    // u64 windows at every 8-byte alignment (smaller set).
    let mut off = 0usize;
    while off + 8 <= seed.len() {
        for &v in extremes_u64 {
            let mut m = seed.to_vec();
            m[off..off + 8].copy_from_slice(&v.to_le_bytes());
            let recipe = format!("len_u64 seed={seed_name} offset={off} value={v:#x}");
            if let Some(f) = run_one(&m, &recipe) {
                findings.push(f);
            }
            n += 1;
        }
        off += 8;
    }
    n
}

/// Corrupt zstd payloads: flip bytes inside each frame's compressed region (using a
/// pristine open of the seed to locate payloads when possible).
fn mut_payload_corruption(seed: &[u8], seed_name: &str, findings: &mut Vec<Finding>) -> u64 {
    let mut n = 0u64;
    let tmp = temp_path("payload-locate");
    std::fs::write(tmp.path(), seed).expect("write");
    let Ok((reader, _)) = LogReader::open(tmp.path()) else {
        // Seed itself may be LIVE-torn; still try heuristic mid-file flips.
        if seed.len() > 64 {
            for off in [seed.len() / 4, seed.len() / 2, seed.len() * 3 / 4] {
                let mut m = seed.to_vec();
                m[off] ^= 0xff;
                let recipe = format!("payload_heuristic seed={seed_name} offset={off}");
                if let Some(f) = run_one(&m, &recipe) {
                    findings.push(f);
                }
                n += 1;
            }
        }
        return n;
    };

    let frame_count = reader.frame_count();
    for i in 0..frame_count {
        let Ok(fh) = reader.frame_header(i) else {
            continue;
        };
        let offset = reader.index()[i].offset as usize;
        let payload_start = offset + FRAME_HEADER_LEN;
        let payload_end = payload_start + fh.compressed_len as usize;
        if payload_end > seed.len() || payload_start >= seed.len() {
            continue;
        }
        // Flip first, middle, last payload byte; zero a run; fill with 0xFF.
        let targets: Vec<usize> = [
            payload_start,
            (payload_start + payload_end) / 2,
            payload_end.saturating_sub(1),
        ]
        .into_iter()
        .filter(|&p| p < seed.len() && p >= payload_start)
        .collect();
        for &p in &targets {
            let mut m = seed.to_vec();
            m[p] ^= 0xff;
            let recipe = format!("payload_flip seed={seed_name} frame={i} offset={p}");
            if let Some(f) = run_one(&m, &recipe) {
                findings.push(f);
            }
            n += 1;
        }
        // Zero entire compressed payload.
        {
            let mut m = seed.to_vec();
            for b in &mut m[payload_start..payload_end] {
                *b = 0;
            }
            let recipe = format!("payload_zero seed={seed_name} frame={i}");
            if let Some(f) = run_one(&m, &recipe) {
                findings.push(f);
            }
            n += 1;
        }
        // Fill with 0xFF.
        {
            let mut m = seed.to_vec();
            for b in &mut m[payload_start..payload_end] {
                *b = 0xff;
            }
            let recipe = format!("payload_ff seed={seed_name} frame={i}");
            if let Some(f) = run_one(&m, &recipe) {
                findings.push(f);
            }
            n += 1;
        }
    }
    drop(reader);
    n
}

/// Duplicate / reorder frame bytes when the seed has ≥ 2 frames (structure-aware).
fn mut_frame_reorder(seed: &[u8], seed_name: &str, findings: &mut Vec<Finding>) -> u64 {
    let mut n = 0u64;
    let tmp = temp_path("reorder-locate");
    std::fs::write(tmp.path(), seed).expect("write");
    let Ok((reader, report)) = LogReader::open(tmp.path()) else {
        return 0;
    };
    // Only closed footer-backed seeds give a clean frame region bound for splicing.
    if report.index_source != IndexSource::Footer && report.recovery.is_none() {
        // still try if we have frames
    }
    let n_frames = reader.frame_count();
    if n_frames < 2 {
        return 0;
    }

    // Slice each frame's bytes [offset, next_offset) or [offset, frames_end).
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for i in 0..n_frames {
        let start = reader.index()[i].offset as usize;
        let end = if i + 1 < n_frames {
            reader.index()[i + 1].offset as usize
        } else {
            // Best-effort: header + compressed payload.
            match reader.frame_header(i) {
                Ok(fh) => start + FRAME_HEADER_LEN + fh.compressed_len as usize,
                Err(_) => start + FRAME_HEADER_LEN,
            }
        };
        if end <= seed.len() && start < end {
            ranges.push((start, end));
        }
    }
    if ranges.len() < 2 {
        return 0;
    }

    let header_end = ranges[0].0;
    let prefix = &seed[..header_end];
    // Tail after last frame (footer etc.).
    let last_end = ranges.last().unwrap().1;
    let suffix = if last_end < seed.len() {
        &seed[last_end..]
    } else {
        &[][..]
    };

    let frame_slices: Vec<&[u8]> = ranges.iter().map(|&(s, e)| &seed[s..e]).collect();

    // Duplicate first frame.
    {
        let mut m = prefix.to_vec();
        m.extend_from_slice(frame_slices[0]);
        for s in &frame_slices {
            m.extend_from_slice(s);
        }
        m.extend_from_slice(suffix);
        let recipe = format!("dup_first_frame seed={seed_name}");
        if let Some(f) = run_one(&m, &recipe) {
            findings.push(f);
        }
        n += 1;
    }

    // Reverse frame order.
    {
        let mut m = prefix.to_vec();
        for s in frame_slices.iter().rev() {
            m.extend_from_slice(s);
        }
        m.extend_from_slice(suffix);
        let recipe = format!("reverse_frames seed={seed_name}");
        if let Some(f) = run_one(&m, &recipe) {
            findings.push(f);
        }
        n += 1;
    }

    // Swap first two frames.
    {
        let mut m = prefix.to_vec();
        m.extend_from_slice(frame_slices[1]);
        m.extend_from_slice(frame_slices[0]);
        for s in frame_slices.iter().skip(2) {
            m.extend_from_slice(s);
        }
        m.extend_from_slice(suffix);
        let recipe = format!("swap_frames_0_1 seed={seed_name}");
        if let Some(f) = run_one(&m, &recipe) {
            findings.push(f);
        }
        n += 1;
    }

    // Drop middle frame if ≥ 3.
    if frame_slices.len() >= 3 {
        let mut m = prefix.to_vec();
        for (i, s) in frame_slices.iter().enumerate() {
            if i != 1 {
                m.extend_from_slice(s);
            }
        }
        m.extend_from_slice(suffix);
        let recipe = format!("drop_middle_frame seed={seed_name}");
        if let Some(f) = run_one(&m, &recipe) {
            findings.push(f);
        }
        n += 1;
    }

    // Append a second copy of the entire footer (if present) after the file.
    if suffix.len() >= TRAILER_LEN {
        let mut m = seed.to_vec();
        m.extend_from_slice(suffix);
        let recipe = format!("dup_suffix seed={seed_name}");
        if let Some(f) = run_one(&m, &recipe) {
            findings.push(f);
        }
        n += 1;
    }

    drop(reader);
    n
}

/// Structural garbage: empty, random short blobs, magic-only, all-0xFF of various sizes.
fn mut_garbage(findings: &mut Vec<Finding>) -> u64 {
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
fn mut_crafted_extreme_headers(findings: &mut Vec<Finding>) -> u64 {
    use fft_log::{FrameHeader, MAX_COMPRESSED_LEN, MAX_UNCOMPRESSED_LEN};
    use xxhash_rust::xxh3::xxh3_64;

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

// ─── Tests ───────────────────────────────────────────────────────────────────

/// Smoke: seed corpus itself is valid under the full surface (always runs).
#[test]
fn seed_corpus_is_valid() {
    let corpus = build_seed_corpus();
    assert_eq!(corpus.len(), 4);
    for (kind, bytes) in &corpus {
        let tmp = temp_path(&format!("seed-valid-{}", kind.name()));
        std::fs::write(tmp.path(), bytes).unwrap();
        let (reader, report) = LogReader::open(tmp.path())
            .unwrap_or_else(|e| panic!("seed {} failed to open: {e}", kind.name()));
        match kind {
            SeedKind::EventsOnly => {
                assert!(!reader.opened_live());
                assert_eq!(report.index_source, IndexSource::Footer);
                assert!(reader.frame_count() >= 2);
                let events: Vec<CanonicalEvent> = reader
                    .events(0..reader.frame_count())
                    .collect::<Result<_, _>>()
                    .unwrap();
                assert!(!events.is_empty());
            }
            SeedKind::WithCheckpoint => {
                assert!(!reader.opened_live());
                assert!(reader.index().iter().any(|e| e.kind == KIND_CHECKPOINT));
                let ck = reader
                    .index()
                    .iter()
                    .position(|e| e.kind == KIND_CHECKPOINT)
                    .unwrap();
                let sections = reader.read_checkpoint(ck).unwrap();
                assert!(!sections.is_empty());
            }
            SeedKind::LiveTornTail => {
                assert!(reader.opened_live());
                assert_eq!(report.index_source, IndexSource::LiveRecovery);
                let recovery = report.recovery.expect("LIVE surfaces recovery");
                assert!(recovery.dropped_bytes > 0);
                assert_eq!(reader.frame_count(), 2);
            }
            SeedKind::WithGaps => {
                let events: Vec<CanonicalEvent> = reader
                    .events(0..reader.frame_count())
                    .collect::<Result<_, _>>()
                    .unwrap();
                assert!(
                    events.iter().any(|e| e.kind == fft_core::EventKind::Gap),
                    "gap seed must contain Gap events"
                );
            }
        }
        // Full surface must not panic on clean seeds.
        exercise_full_surface(tmp.path()).unwrap();
    }
}

/// Deterministic structure-aware adversarial run. Ignored in default CI; run with
/// `--ignored` for the M7 gate. Wall budget ~2–5 min; strides documented in stdout.
#[test]
#[ignore = "adversarial LOG-FUZZ corpus; run with --ignored for M7 (2–5 min)"]
fn adversarial_mutation_corpus() {
    let t0 = Instant::now();
    let corpus = build_seed_corpus();
    let mut findings: Vec<Finding> = Vec::new();
    let mut total_mutants: u64 = 0;

    // Garbage + crafted headers first (cheap, high-value).
    total_mutants += mut_garbage(&mut findings);
    total_mutants += mut_crafted_extreme_headers(&mut findings);

    // Identify the smallest seed for exhaustive flips/truncations.
    let (small_kind, small_bytes) = corpus
        .iter()
        .min_by_key(|(_, b)| b.len())
        .map(|(k, b)| (*k, b.as_slice()))
        .expect("corpus non-empty");

    eprintln!(
        "LOG-FUZZ: smallest seed = {} ({} bytes); exhaustive bit-flips + truncations",
        small_kind.name(),
        small_bytes.len()
    );

    // Exhaustive on smallest seed (stride 1).
    total_mutants += mut_bit_flips(small_bytes, small_kind.name(), 1, &mut findings);
    total_mutants += mut_truncations(small_bytes, small_kind.name(), 1, &mut findings);
    total_mutants += mut_length_extremes(small_bytes, small_kind.name(), &mut findings);
    total_mutants += mut_payload_corruption(small_bytes, small_kind.name(), &mut findings);
    total_mutants += mut_frame_reorder(small_bytes, small_kind.name(), &mut findings);

    // Other seeds: full exhaustive bit-flips/truncations when ≤ 2 KiB (current corpus
    // is well under that); otherwise stride so wall time stays ≤ ~5 min.
    const EXHAUSTIVE_MAX_BYTES: usize = 2048;
    for (kind, bytes) in &corpus {
        if kind.name() == small_kind.name() {
            continue;
        }
        let stride = if bytes.len() <= EXHAUSTIVE_MAX_BYTES {
            1
        } else {
            (bytes.len() / 256).max(1)
        };
        eprintln!(
            "LOG-FUZZ: seed {} ({} bytes) stride={stride} (bit-flip + truncate)",
            kind.name(),
            bytes.len()
        );
        total_mutants += mut_bit_flips(bytes, kind.name(), stride, &mut findings);
        total_mutants += mut_truncations(bytes, kind.name(), stride, &mut findings);
        if stride == 1 {
            total_mutants += mut_length_extremes(bytes, kind.name(), &mut findings);
        }
        total_mutants += mut_payload_corruption(bytes, kind.name(), &mut findings);
        total_mutants += mut_frame_reorder(bytes, kind.name(), &mut findings);

        // Insert a 0x00 and 0xFF byte at header boundary and mid-file (structural insert).
        for &pos in &[0usize, 8, bytes.len() / 2, bytes.len()] {
            for &b in &[0u8, 0xff] {
                let mut m = Vec::with_capacity(bytes.len() + 1);
                m.extend_from_slice(&bytes[..pos.min(bytes.len())]);
                m.push(b);
                m.extend_from_slice(&bytes[pos.min(bytes.len())..]);
                let recipe = format!("insert_byte seed={} pos={pos} byte={b:#x}", kind.name());
                if let Some(f) = run_one(&m, &recipe) {
                    findings.push(f);
                }
                total_mutants += 1;
            }
        }
    }

    // Extra: strip footer from closed seeds (forces rebuild path under mutations).
    for (kind, bytes) in &corpus {
        if matches!(kind, SeedKind::LiveTornTail) {
            continue;
        }
        if bytes.len() > TRAILER_LEN + 32 {
            let stripped = &bytes[..bytes.len() - TRAILER_LEN];
            // Magic may still be present in remaining bytes; strip full trailer only.
            let recipe = format!("strip_trailer seed={}", kind.name());
            if let Some(f) = run_one(stripped, &recipe) {
                findings.push(f);
            }
            total_mutants += 1;

            // Truncate the stripped form at mid-point (closed corrupt-tail path).
            let mid = stripped.len() / 2;
            let recipe = format!("strip_trailer_then_truncate seed={} cut={mid}", kind.name());
            if let Some(f) = run_one(&stripped[..mid], &recipe) {
                findings.push(f);
            }
            total_mutants += 1;
        }
    }

    let elapsed = t0.elapsed();
    eprintln!(
        "LOG-FUZZ done: mutants={total_mutants} findings={} wall={elapsed:?}",
        findings.len()
    );

    if !findings.is_empty() {
        eprintln!("\n======== LOG-FUZZ FINDINGS ({}) ========", findings.len());
        for (i, f) in findings.iter().enumerate() {
            eprintln!(
                "[{i}] {:?} | recipe: {}\n     detail: {}\n     repro: \
                 write mutant bytes per recipe, then LogReader::open + full surface \
                 inside catch_unwind — expect no panic",
                f.severity, f.recipe, f.detail
            );
        }
        // Fail the ignored test so CI/manual runs surface findings loudly.
        panic!(
            "LOG-FUZZ: {} finding(s) ({} SEV-1). See stderr recipes for standalone repros.",
            findings.len(),
            findings
                .iter()
                .filter(|f| f.severity == Severity::Sev1)
                .count()
        );
    }

    // Soft budget note (not a hard fail — machines vary).
    if elapsed.as_secs() > 360 {
        eprintln!(
            "LOG-FUZZ warning: wall time {:?} exceeded 5 min soft budget; \
             consider increasing strides",
            elapsed
        );
    }
    eprintln!("LOG-FUZZ: clean bill — {total_mutants} mutants, no panics, {elapsed:?}");
}

/// Always-on micro repro: a handful of high-value mutants must not panic.
/// Keeps a regression canary in the non-ignored suite without the full budget.
#[test]
fn fuzz_canary_high_value_mutants_do_not_panic() {
    let corpus = build_seed_corpus();
    let mut findings = Vec::new();
    let mut n = 0u64;

    n += mut_garbage(&mut findings);
    n += mut_crafted_extreme_headers(&mut findings);

    // One closed seed: truncate at 0, 1, mid, end-1; flip first/last byte.
    let (kind, bytes) = corpus
        .iter()
        .find(|(k, _)| matches!(k, SeedKind::EventsOnly))
        .expect("events_only seed");
    for cut in [0usize, 1, bytes.len() / 2, bytes.len().saturating_sub(1)] {
        let recipe = format!("canary_truncate seed={} cut={cut}", kind.name());
        if let Some(f) = run_one(&bytes[..cut.min(bytes.len())], &recipe) {
            findings.push(f);
        }
        n += 1;
    }
    for off in [0usize, bytes.len() - 1] {
        let mut m = bytes.clone();
        m[off] ^= 0xff;
        let recipe = format!("canary_flip seed={} offset={off}", kind.name());
        if let Some(f) = run_one(&m, &recipe) {
            findings.push(f);
        }
        n += 1;
    }
    // Hostile footer index_len on closed seed (LOG-HARDEN regression under full surface).
    if bytes.len() >= TRAILER_LEN {
        let mut m = bytes.clone();
        let index_len_at = m.len() - TRAILER_LEN;
        m[index_len_at..index_len_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let recipe = format!("canary_hostile_index_len seed={}", kind.name());
        if let Some(f) = run_one(&m, &recipe) {
            findings.push(f);
        }
        n += 1;
    }

    assert!(
        findings.is_empty(),
        "canary findings ({n} mutants): {:?}",
        findings
            .iter()
            .map(|f| (&f.recipe, &f.detail))
            .collect::<Vec<_>>()
    );
}
