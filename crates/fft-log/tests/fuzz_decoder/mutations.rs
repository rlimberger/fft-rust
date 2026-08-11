//! Structure-aware mutation engines over seed bytes.

use super::harness::{Finding, run_one};
use crate::common::temp_path;
use fft_log::{FRAME_HEADER_LEN, IndexSource, LogReader, TRAILER_LEN};

/// Bit-flip every byte (or every `stride`-th byte) of `seed`.
pub(super) fn mut_bit_flips(
    seed: &[u8],
    seed_name: &str,
    stride: usize,
    findings: &mut Vec<Finding>,
) -> u64 {
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
pub(super) fn mut_truncations(
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
pub(super) fn mut_length_extremes(
    seed: &[u8],
    seed_name: &str,
    findings: &mut Vec<Finding>,
) -> u64 {
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
pub(super) fn mut_payload_corruption(
    seed: &[u8],
    seed_name: &str,
    findings: &mut Vec<Finding>,
) -> u64 {
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
pub(super) fn mut_frame_reorder(seed: &[u8], seed_name: &str, findings: &mut Vec<Finding>) -> u64 {
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
