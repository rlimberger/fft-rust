//! Unit tests for `transport` (kept separate so the module stays under ~500 lines).

use super::*;

#[test]
fn toggle_mode_does_not_emit_commands_or_pause() {
    let mut t = TransportState::default();
    assert!(!t.mode_on);
    assert!(t.playing);
    let a = t.toggle_mode();
    assert!(t.mode_on);
    assert!(t.playing, "entering replay mode must not pause");
    assert!(a.commands.is_empty());
    assert!(a.refresh);
    let a = t.toggle_mode();
    assert!(!t.mode_on);
    assert!(a.refresh);
}

#[test]
fn play_pause_only_when_mode_on() {
    let mut t = TransportState::default();
    assert!(t.toggle_play().commands.is_empty());
    t.toggle_mode();
    let a = t.toggle_play();
    assert!(!t.playing);
    assert_eq!(a.commands, vec![TransportCommand::Pause]);
    let a = t.toggle_play();
    assert!(t.playing);
    assert_eq!(a.commands, vec![TransportCommand::Play]);
}

#[test]
fn speed_clamps_at_ladder_ends() {
    let mut t = TransportState::default();
    t.toggle_mode();
    assert_eq!(t.speed(), 1.0);
    // Down to floor.
    for _ in 0..8 {
        t.speed_down();
    }
    assert_eq!(t.speed_index, 0);
    assert_eq!(t.speed(), 0.25);
    let a = t.speed_down();
    assert!(a.commands.is_empty(), "floor is a no-op");
    // Up to ceiling.
    for _ in 0..16 {
        t.speed_up();
    }
    assert_eq!(t.speed_index, SPEED_LADDER.len() - 1);
    assert_eq!(t.speed(), 64.0);
    let a = t.speed_up();
    assert!(a.commands.is_empty(), "ceiling is a no-op");
    // One step down emits SetSpeed.
    let a = t.speed_down();
    assert_eq!(a.commands, vec![TransportCommand::SetSpeed(16.0)]);
}

#[test]
fn speed_ignored_when_mode_off() {
    let mut t = TransportState::default();
    assert!(t.speed_up().commands.is_empty());
    assert!(t.speed_down().commands.is_empty());
    assert_eq!(t.speed_index, DEFAULT_SPEED_INDEX);
}

#[test]
fn scrub_coalesces_to_one_seek_with_last_position() {
    let mut t = TransportState::default();
    t.toggle_mode();
    let first = 1_000u64;
    let last = 2_000u64;
    t.begin_scrub(0.0, 0.0, 100.0, first, last);
    t.queue_scrub(25.0, 0.0, 100.0, first, last);
    t.queue_scrub(50.0, 0.0, 100.0, first, last);
    t.queue_scrub(90.0, 0.0, 100.0, first, last);
    t.end_scrub();
    let cmd = t.take_coalesced_seek().expect("one pending seek");
    match cmd {
        TransportCommand::Seek { ts, generation } => {
            assert_eq!(generation, FIRST_UI_SEEK_GENERATION);
            assert_eq!(ts, scrub_ts_from_x(90.0, 0.0, 100.0, first, last));
        }
        other => panic!("expected Seek, got {other:?}"),
    }
    assert!(t.take_coalesced_seek().is_none(), "second drain is empty");
}

#[test]
fn seek_generation_is_monotonic_from_two() {
    let mut t = TransportState::default();
    t.toggle_mode();
    assert_eq!(t.next_seek_generation(), FIRST_UI_SEEK_GENERATION);
    let a = t.step(5_000_000_000, 0, 10_000_000_000, true);
    match &a.commands[..] {
        [TransportCommand::Seek { generation: 2, .. }] => {}
        other => panic!("expected gen 2, got {other:?}"),
    }
    let a = t.step(6_000_000_000, 0, 10_000_000_000, true);
    match &a.commands[..] {
        [TransportCommand::Seek { generation: 3, .. }] => {}
        other => panic!("expected gen 3, got {other:?}"),
    }
    assert_eq!(t.next_seek_generation(), 4);
}

#[test]
fn step_arithmetic_clamps_to_range() {
    let mut t = TransportState::default();
    t.toggle_mode();
    let first = 10 * STEP_NS;
    let last = 20 * STEP_NS;
    // Backward from first is no-op.
    let a = t.step(first, first, last, false);
    assert!(a.commands.is_empty());
    // Forward one second.
    let a = t.step(first, first, last, true);
    match &a.commands[..] {
        [TransportCommand::Seek { ts, .. }] => assert_eq!(*ts, first + STEP_NS),
        other => panic!("expected step seek, got {other:?}"),
    }
    // Forward past last clamps.
    let a = t.step(last, first, last, true);
    assert!(a.commands.is_empty());
    let a = t.step(last - STEP_NS / 2, first, last, true);
    match &a.commands[..] {
        [TransportCommand::Seek { ts, .. }] => assert_eq!(*ts, last),
        other => panic!("expected clamp to last, got {other:?}"),
    }
}

#[test]
fn go_live_is_hint_only() {
    let mut t = TransportState::default();
    assert!(t.go_live_placeholder().status_hint.is_none());
    t.toggle_mode();
    let a = t.go_live_placeholder();
    assert!(a.commands.is_empty());
    assert_eq!(a.status_hint, Some(GO_LIVE_HINT));
    assert_eq!(t.status_hint, Some(GO_LIVE_HINT));
}

#[test]
fn scrub_ts_from_x_endpoints() {
    let first = 100u64;
    let last = 200u64;
    assert_eq!(scrub_ts_from_x(0.0, 0.0, 100.0, first, last), first);
    assert_eq!(scrub_ts_from_x(100.0, 0.0, 100.0, first, last), last);
    assert_eq!(scrub_ts_from_x(50.0, 0.0, 100.0, first, last), 150);
    // Out of track clamps.
    assert_eq!(scrub_ts_from_x(-10.0, 0.0, 100.0, first, last), first);
    assert_eq!(scrub_ts_from_x(999.0, 0.0, 100.0, first, last), last);
}

#[test]
fn session_range_wed_sample_is_cdt_open() {
    // Trade date 2026-07-29 = day 20_663; open = 2026-07-28 17:00 CDT = 22:00 UTC.
    let (open, close) = session_range_ns(20_663);
    assert_eq!(open, 1_785_276_000_000_000_000);
    assert_eq!(close, open + 86_400 * 1_000_000_000);
    // PRD §6 anchor 13:50Z sits inside the range.
    let anchor = 1_785_333_000_000_000_000u64;
    assert!(anchor > open && anchor < close);
}

#[test]
fn format_ct_clock_anchor() {
    // 2026-07-29T13:50:00Z = 08:50:00 CDT.
    assert_eq!(format_ct_clock(1_785_333_000_000_000_000), "08:50:00");
    assert_eq!(format_ct_clock(0), "--:--:--");
}

#[test]
fn format_speed_labels() {
    assert_eq!(format_speed(1.0), "×1");
    assert_eq!(format_speed(0.25), "×0.25");
    assert_eq!(format_speed(64.0), "×64");
}

#[test]
fn queue_without_begin_is_ignored() {
    let mut t = TransportState::default();
    t.toggle_mode();
    t.queue_scrub(50.0, 0.0, 100.0, 0, 100);
    assert!(t.take_coalesced_seek().is_none());
}

#[test]
fn format_ct_clock_out_of_range_soft_fails() {
    assert_eq!(
        format_zone_clock_ns(i128::MAX, "America/Chicago"),
        "--:--:--"
    );
}

#[test]
fn session_open_far_future_trade_date_soft_fails() {
    // u32::MAX civil year overflows jiff i16; soft-fail to (0,1), not panic.
    assert_eq!(session_open_ns(u32::MAX), 0);
    assert_eq!(session_range_ns(u32::MAX), (0, 1));
}
