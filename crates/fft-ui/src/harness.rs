//! Frame pump instrumentation: warmup → measure phases, optional gap trace, and the
//! measured [`FrameResult`] handed to the gate report. Moved out of `main.rs` verbatim
//! (size rule); the timing logic is unchanged.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::frame_stats::FrameStats;
use crate::gate_report::{FrameResult, millis, round3};

/// Warmup is a time budget, never an event count (doctrine rule 5); the sample floor only
/// guards the median against a compositor that briefly withholds frame callbacks.
pub const WARMUP: Duration = Duration::from_millis(500);
pub const MIN_WARMUP_SAMPLES: usize = 8;

enum Phase {
    Warmup {
        started: Instant,
        gaps: Vec<Duration>,
    },
    Measuring {
        stats: FrameStats,
        gate_ends: Option<Instant>,
    },
}

/// What warmup actually observed (recorded for evidence, not used for control flow).
#[derive(Clone, Copy)]
struct Warmup {
    elapsed: Duration,
    samples: usize,
    refresh_interval: Duration,
}

pub struct Harness {
    gate: Option<Duration>,
    trace: Option<Trace>,
    last_frame: Option<Instant>,
    phase: Option<Phase>,
    warmup: Option<Warmup>,
}

impl Harness {
    pub fn new(gate: Option<Duration>, trace: Option<PathBuf>) -> Self {
        Self {
            gate,
            trace: trace.map(Trace::new),
            last_frame: None,
            phase: None,
            warmup: None,
        }
    }

    pub fn gating(&self) -> bool {
        self.gate.is_some()
    }

    /// Returns `false` once the gate window has elapsed and the redraw loop should stop.
    pub fn on_frame(&mut self, now: Instant) -> bool {
        crate::startup_trace::note_first_paint();
        let gap = self.last_frame.replace(now).map(|last| now - last);
        let phase = self.phase.get_or_insert(Phase::Warmup {
            started: now,
            gaps: Vec::new(),
        });
        let Some(gap) = gap else { return true };
        match phase {
            Phase::Warmup { started, gaps } => {
                gaps.push(gap);
                if now - *started >= WARMUP && gaps.len() >= MIN_WARMUP_SAMPLES {
                    gaps.sort_unstable();
                    let refresh_interval = gaps[gaps.len() / 2];
                    let deadline = refresh_interval * 3 / 2;
                    eprintln!(
                        "fft: refresh interval {:.3} ms ({:.1} Hz), deadline {:.3} ms",
                        refresh_interval.as_secs_f64() * 1e3,
                        1.0 / refresh_interval.as_secs_f64(),
                        deadline.as_secs_f64() * 1e3,
                    );
                    self.warmup = Some(Warmup {
                        elapsed: now - *started,
                        samples: gaps.len(),
                        refresh_interval,
                    });
                    self.phase = Some(Phase::Measuring {
                        stats: FrameStats::new(deadline),
                        gate_ends: self.gate.map(|g| now + g),
                    });
                }
                true
            }
            Phase::Measuring { stats, gate_ends } => {
                stats.record(gap);
                if let Some(trace) = &mut self.trace {
                    trace.samples.push(gap.as_nanos());
                }
                gate_ends.is_none_or(|ends| now < ends)
            }
        }
    }

    /// Prints the summary and returns the measured result, or `None` when warmup never
    /// completed (which the gate treats as a failure).
    pub fn finish(&mut self) -> Option<FrameResult> {
        if let Some(trace) = &self.trace {
            trace.write();
        }
        match &self.phase {
            Some(Phase::Measuring { stats, .. }) if stats.frames() > 0 => {
                let summary = stats.summary();
                println!("{summary}");
                let warmup = self
                    .warmup
                    .expect("measuring phase implies warmup completed");
                Some(FrameResult {
                    refresh_interval_ms: millis(warmup.refresh_interval),
                    refresh_hz: round3(1.0 / warmup.refresh_interval.as_secs_f64()),
                    deadline_ms: millis(summary.deadline),
                    warmup_ms: millis(warmup.elapsed),
                    warmup_samples: warmup.samples as u64,
                    frames: summary.frames,
                    missed: summary.missed,
                    p50_ms: millis(summary.p50),
                    p95_ms: millis(stats.quantile(0.95)),
                    p99_ms: millis(summary.p99),
                    max_ms: millis(summary.max),
                })
            }
            _ => {
                eprintln!("fft: no frames measured (warmup never completed)");
                None
            }
        }
    }
}

struct Trace {
    path: PathBuf,
    samples: Vec<u128>,
}

impl Trace {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            samples: Vec::new(),
        }
    }

    /// Trace I/O happens only after the window closes; the UI thread records in memory.
    fn write(&self) {
        let file = File::create(&self.path).unwrap_or_else(|err| {
            panic!("fft: cannot open trace file {}: {err}", self.path.display())
        });
        let mut writer = BufWriter::new(file);
        for sample in &self.samples {
            writeln!(writer, "{sample}")
                .unwrap_or_else(|err| panic!("fft: trace write failed: {err}"));
        }
        writer
            .flush()
            .unwrap_or_else(|err| panic!("fft: trace flush failed: {err}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Warmup completes only once both the time budget and the sample floor are met, and
    /// the measured warmup facts land in the reported result.
    #[test]
    fn warmup_facts_reach_the_frame_result() {
        let mut harness = Harness::new(None, None);
        let start = Instant::now();
        let interval = Duration::from_millis(10);
        // First call establishes `last_frame` (no gap yet), then 100 gaps of 10 ms.
        harness.on_frame(start);
        for i in 1..=100 {
            harness.on_frame(start + interval * i);
        }
        // Warmup ends at 500 ms / 50 samples; the rest are measured frames.
        let result = harness.finish().expect("frames measured");
        assert_eq!(result.warmup_samples, 50);
        assert_eq!(result.warmup_ms, 500.0);
        assert_eq!(result.refresh_interval_ms, 10.0);
        assert_eq!(result.refresh_hz, 100.0);
        assert_eq!(result.deadline_ms, 15.0);
        assert_eq!(result.frames, 50);
        assert_eq!(result.missed, 0);
    }

    #[test]
    fn no_frames_measured_yields_no_result() {
        let mut harness = Harness::new(Some(Duration::from_secs(1)), None);
        let start = Instant::now();
        harness.on_frame(start);
        harness.on_frame(start + Duration::from_millis(10));
        assert!(harness.finish().is_none());
        assert!(harness.gating());
    }
}
