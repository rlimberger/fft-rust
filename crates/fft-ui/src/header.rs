//! Always-visible top chrome: contract context, NY event clock, and frame cadence.

use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use fft_engine::RenderSnapshot;
use gpui::{AnyElement, div, prelude::*, px};
use jiff::Timestamp;

use crate::datetime::civil_from_days;
use crate::mp_view::display_session;
use crate::theme::Palette;
use crate::transport::TRANSPORT_H;

const NY: &str = "America/New_York";
const PAD_X: f32 = 8.0;
const WINDOW: Duration = Duration::from_secs(1);

/// Header height matches the transport strip at the current OS scale.
#[inline]
pub fn header_h(scale: f32) -> f32 {
    TRANSPORT_H * scale
}

/// Rolling count of frame callbacks in the trailing one-second window.
#[derive(Default)]
pub struct FrameCadence {
    frames: VecDeque<Instant>,
}

impl FrameCadence {
    pub fn record(&mut self, now: Instant) -> usize {
        rolling_frame_count(&mut self.frames, now, WINDOW)
    }
}

fn rolling_frame_count(frames: &mut VecDeque<Instant>, now: Instant, window: Duration) -> usize {
    frames.push_back(now);
    while frames
        .front()
        .is_some_and(|frame| now.saturating_duration_since(*frame) >= window)
    {
        frames.pop_front();
    }
    frames.len()
}

/// America/New_York event-time clock. Zero means no applied event.
pub fn format_ny_clock(ts_ns: u64) -> String {
    if ts_ns == 0 {
        return "--:--:--".to_string();
    }
    let ts = Timestamp::from_nanosecond(i128::from(ts_ns))
        .unwrap_or_else(|err| panic!("fft-ui: applied_ts {ts_ns} outside jiff range: {err}"));
    let zoned = ts
        .in_tz(NY)
        .unwrap_or_else(|err| panic!("fft-ui: tz database missing {NY}: {err}"));
    let time = zoned.time();
    format!(
        "{:02}:{:02}:{:02}",
        time.hour(),
        time.minute(),
        time.second()
    )
}

/// Contract slot: symbol from the current source header, else `--`, plus trade date.
pub fn contract_context(snapshot: &RenderSnapshot) -> String {
    let Some(session) = display_session(&snapshot.profile) else {
        return "-- ---- --".to_string();
    };
    if session.trade_date == 0 {
        return "-- ---- --".to_string();
    }
    let (year, month, day) = civil_from_days(i64::from(session.trade_date));
    let symbol = if snapshot.symbol.is_empty() {
        "--"
    } else {
        snapshot.symbol.as_ref()
    };
    format!("{symbol} {year:04}-{month:02}-{day:02}")
}

pub struct HeaderArgs {
    pub palette: Rc<Palette>,
    pub scale: f32,
    pub contract: String,
    pub applied_ts: u64,
    pub fps: usize,
}

/// Build the top header strip. The root font supplies the configured mono family.
pub fn header_strip(args: HeaderArgs) -> AnyElement {
    let h = header_h(args.scale);
    let text_size = px(11.0 * args.scale);
    let clock = format_ny_clock(args.applied_ts);
    let fps = format!("{} FPS", args.fps);

    div()
        .id("header-strip")
        .w_full()
        .h(px(h))
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .flex_none()
        .bg(args.palette.footer_bg)
        .border_b_1()
        .border_color(args.palette.divider)
        .px(px(PAD_X * args.scale))
        .text_size(text_size)
        .child(
            div()
                .id("header-contract")
                .text_color(args.palette.text)
                .child(args.contract),
        )
        .child(
            div()
                .id("header-clock")
                .text_color(args.palette.text)
                .child(clock),
        )
        .child(
            div()
                .id("header-fps")
                .text_color(args.palette.subtext)
                .child(fps),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fft_engine::{ProfileRenderState, ProfileSessionRender};
    use std::sync::Arc;

    #[test]
    fn ny_clock_pins_sim_live_anchor() {
        assert_eq!(format_ny_clock(1_785_333_000_000_000_000), "09:50:00");
    }

    #[test]
    fn ny_clock_crosses_spring_dst_boundary() {
        assert_eq!(format_ny_clock(1_772_953_199_000_000_000), "01:59:59");
        assert_eq!(format_ny_clock(1_772_953_200_000_000_000), "03:00:00");
    }

    #[test]
    fn ny_clock_is_empty_without_applied_event() {
        assert_eq!(format_ny_clock(0), "--:--:--");
    }

    #[test]
    fn rolling_fps_expires_frames_at_window_boundary() {
        let start = Instant::now();
        let mut frames = VecDeque::new();
        assert_eq!(rolling_frame_count(&mut frames, start, WINDOW), 1);
        assert_eq!(
            rolling_frame_count(&mut frames, start + Duration::from_millis(400), WINDOW),
            2
        );
        assert_eq!(
            rolling_frame_count(&mut frames, start + Duration::from_millis(999), WINDOW),
            3
        );
        assert_eq!(
            rolling_frame_count(&mut frames, start + Duration::from_secs(1), WINDOW),
            3
        );
        assert_eq!(
            rolling_frame_count(&mut frames, start + Duration::from_millis(1400), WINDOW),
            3
        );
        assert_eq!(
            rolling_frame_count(&mut frames, start + Duration::from_secs(2), WINDOW),
            2
        );
    }

    #[test]
    fn contract_context_is_placeholder_until_session_exists() {
        assert_eq!(contract_context(&RenderSnapshot::default()), "-- ---- --");
    }

    #[test]
    fn contract_context_pairs_placeholder_with_trade_date() {
        let snapshot = RenderSnapshot {
            profile: ProfileRenderState {
                sessions: vec![ProfileSessionRender {
                    trade_date: 20_663,
                    ..ProfileSessionRender::default()
                }],
            },
            ..RenderSnapshot::default()
        };
        assert_eq!(contract_context(&snapshot), "-- 2026-07-29");
    }

    #[test]
    fn contract_context_renders_symbol_with_trade_date() {
        let snapshot = RenderSnapshot {
            symbol: Arc::from("ESU6"),
            profile: ProfileRenderState {
                sessions: vec![ProfileSessionRender {
                    trade_date: 20_663,
                    ..ProfileSessionRender::default()
                }],
            },
            ..RenderSnapshot::default()
        };
        assert_eq!(contract_context(&snapshot), "ESU6 2026-07-29");
    }
}
