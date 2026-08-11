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

#[path = "../common/mod.rs"]
mod common;

mod crafted;
mod harness;
mod mutations;
mod seed;

use std::time::Instant;

use common::temp_path;
use fft_core::CanonicalEvent;
use fft_log::{IndexSource, KIND_CHECKPOINT, LogReader, TRAILER_LEN};

use crafted::{mut_crafted_extreme_headers, mut_garbage};
use harness::{Finding, Severity, exercise_full_surface, run_one};
use mutations::{
    mut_bit_flips, mut_frame_reorder, mut_length_extremes, mut_payload_corruption, mut_truncations,
};
use seed::{SeedKind, build_seed_corpus};

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
