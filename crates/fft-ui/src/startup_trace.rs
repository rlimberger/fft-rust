//! Optional cold-start timing for the M5 boring gates (PRD §4).
//!
//! Enabled only by `--startup-trace`. Records wall time from process entry to:
//! (a) first GPUI frame presented (`Harness::on_frame` first call),
//! (b) first non-empty engine snapshot rendered (`RenderSnapshot.generation > 0`).
//!
//! Flag (not env): matches `--gate`/`--trace` style; normal runs stay zero-overhead
//! aside from two atomic loads per frame when disabled.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static PROCESS_START: OnceLock<Instant> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);
static FIRST_PAINT: AtomicBool = AtomicBool::new(false);
static FIRST_INTERACTIVE: AtomicBool = AtomicBool::new(false);

/// Call at the absolute top of `main` before arg parse / GPUI init.
pub fn mark_process_start() {
    let _ = PROCESS_START.set(Instant::now());
}

/// Arm emission (CLI `--startup-trace`).
pub fn enable() {
    ENABLED.store(true, Ordering::Release);
}

/// Whether `--startup-trace` is active.
#[inline]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

fn elapsed_ms() -> f64 {
    PROCESS_START
        .get()
        .map(|t| t.elapsed().as_secs_f64() * 1e3)
        .unwrap_or(0.0)
}

/// First frame presented (harness `on_frame` first entry).
#[inline]
pub fn note_first_paint() {
    if !enabled() {
        return;
    }
    if FIRST_PAINT.swap(true, Ordering::AcqRel) {
        return;
    }
    eprintln!("fft: startup-trace first_paint_ms={:.3}", elapsed_ms());
}

/// First render that adopted a non-empty `RenderSnapshot` (generation > 0).
#[inline]
pub fn note_first_interactive() {
    if !enabled() {
        return;
    }
    if FIRST_INTERACTIVE.swap(true, Ordering::AcqRel) {
        return;
    }
    eprintln!(
        "fft: startup-trace first_interactive_ms={:.3}",
        elapsed_ms()
    );
}

/// Both marks emitted — shell may quit so measurement runs terminate.
#[inline]
pub fn complete() -> bool {
    enabled() && FIRST_PAINT.load(Ordering::Acquire) && FIRST_INTERACTIVE.load(Ordering::Acquire)
}
