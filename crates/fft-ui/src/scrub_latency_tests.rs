use super::*;
use std::sync::Mutex;

static TEST_LOCK: Mutex<()> = Mutex::new(());

fn reset_for_test(n: u32, out: PathBuf) {
    enable(
        n,
        out,
        DEFAULT_SEED,
        BUDGET_P95_MS,
        PathBuf::from("/tmp/scrub-latency-test.fftlog"),
    );
    with_state(|s| {
        s.pending_t0 = None;
        s.bound = None;
        s.samples_ms.clear();
        s.evidence_written = false;
        s.verdict = None;
        s.quit = false;
        s.first_ts = None;
        s.last_ts = None;
        s.rng = if s.seed == 0 { 1 } else { s.seed };
    });
}

#[test]
fn note_release_bind_rendered_records_one_sample() {
    let _g = TEST_LOCK.lock().unwrap();
    let out = std::env::temp_dir().join("scrub-latency-unit-one.json");
    reset_for_test(1, out);
    note_release();
    bind_generation(7);
    note_rendered(7);
    assert!(complete());
    assert_eq!(with_state(|s| s.samples_ms.len()), 1);
}

#[test]
fn mismatched_generation_does_not_complete() {
    let _g = TEST_LOCK.lock().unwrap();
    let out = std::env::temp_dir().join("scrub-latency-unit-mismatch.json");
    reset_for_test(1, out);
    note_release();
    bind_generation(7);
    note_rendered(8);
    assert!(!complete());
    assert!(with_state(|s| s.samples_ms.is_empty()));
    note_rendered(7);
    assert!(complete());
}

#[test]
fn p95_math_on_known_vector() {
    let samples: Vec<f64> = (1..=20).map(|i| i as f64).collect();
    assert_eq!(percentile_nearest_rank(&samples, 0.95), 19.0);
    assert_eq!(percentile_nearest_rank(&samples, 0.50), 10.0);
    assert_eq!(percentile_nearest_rank(&samples, 0.99), 20.0);
}

#[test]
fn next_script_target_maps_into_range() {
    let _g = TEST_LOCK.lock().unwrap();
    let out = std::env::temp_dir().join("scrub-latency-unit-range.json");
    reset_for_test(3, out);
    let first = 1_000u64;
    let last = 2_000u64;
    for i in 0..3 {
        let ts = next_script_target(first, last).expect("target");
        assert!((first..=last).contains(&ts));
        note_release();
        bind_generation(10 + i);
        note_rendered(10 + i);
    }
    assert!(complete());
    assert!(next_script_target(first, last).is_none());
}
