//! Keyboard / pointer input path for the two-pane shell.
//!
//! Split from `shell.rs` so the shell module stays under ~500 lines.

use std::cell::RefCell;
use std::rc::Rc;

use fft_engine::{EngineCmd, EngineHandle, LiveTransportPhase, RenderSnapshot};
use gpui::{MouseMoveEvent, Window};

use crate::dom_input::DomInput;
use crate::mp_view::display_session;
use crate::pane_state::PaneState;
use crate::transport::{TransportCommand, TransportState, session_range_ns};

pub(crate) fn dispatch_transport(engine: &Option<EngineHandle>, commands: &[TransportCommand]) {
    let Some(handle) = engine.as_ref() else {
        return;
    };
    for cmd in commands {
        let engine_cmd = match cmd {
            TransportCommand::Play => EngineCmd::Play,
            TransportCommand::Pause => EngineCmd::Pause,
            TransportCommand::SetSpeed(s) => EngineCmd::SetSpeed(*s),
            TransportCommand::Seek { ts, generation } => EngineCmd::Seek {
                ts: *ts,
                generation: *generation,
            },
            TransportCommand::GoLive => EngineCmd::GoLive,
        };
        handle
            .send(engine_cmd)
            .unwrap_or_else(|err| panic!("fft: transport command failed: {err}"));
    }
}

pub(crate) struct KeyCtx<'a> {
    pub panes: &'a Rc<RefCell<PaneState>>,
    pub dom_input: &'a Rc<RefCell<DomInput>>,
    pub transport: &'a Rc<RefCell<TransportState>>,
    pub engine: &'a Rc<RefCell<Option<EngineHandle>>>,
    pub applied_ts: u64,
    pub first_ts: u64,
    pub last_ts: u64,
    pub live_phase: LiveTransportPhase,
}

/// Handle an unmodified keystroke. Returns `Some(refresh)` when the event was consumed.
pub(crate) fn handle_key(key: &str, ctx: &KeyCtx<'_>) -> Option<bool> {
    let (pane_handled, pane_refresh) = {
        let mut panes = ctx.panes.borrow_mut();
        match key {
            "1" => (true, panes.set_hovered_scale(1)),
            "2" => (true, panes.set_hovered_scale(2)),
            "4" => (true, panes.set_hovered_scale(4)),
            "t" => (true, panes.sync_scale_from_hovered()),
            "c" => (true, panes.recenter()),
            "d" => {
                let visible = panes.toggle_dom();
                if !visible {
                    ctx.dom_input.borrow_mut().end_drag();
                }
                (true, true)
            }
            _ => (false, false),
        }
    };
    if pane_handled {
        return Some(pane_refresh);
    }
    let action = {
        let mut t = ctx.transport.borrow_mut();
        match key {
            "r" => t.toggle_mode(),
            "space" => t.toggle_play(),
            "]" => t.speed_up(),
            "[" => t.speed_down(),
            "left" => t.step(ctx.applied_ts, ctx.first_ts, ctx.last_ts, false),
            "right" => t.step(ctx.applied_ts, ctx.first_ts, ctx.last_ts, true),
            "l" => t.go_live(ctx.live_phase),
            _ => return None,
        }
    };
    if let Some(hint) = action.status_hint {
        eprintln!("fft: {hint}");
    }
    dispatch_transport(&ctx.engine.borrow(), &action.commands);
    Some(action.refresh)
}

pub(crate) fn on_splitter_mouse_move(
    event: &MouseMoveEvent,
    panes: &Rc<RefCell<PaneState>>,
    window: &mut Window,
) {
    let mut panes = panes.borrow_mut();
    if panes.splitter.is_dragging() && event.dragging() {
        panes.splitter.queue(f32::from(event.position.x));
        drop(panes);
        window.refresh();
    }
}

pub(crate) fn end_splitter_drag(panes: &Rc<RefCell<PaneState>>) {
    panes.borrow_mut().splitter.end();
}

pub(crate) fn scrub_range_from_snapshot(snap: &RenderSnapshot) -> (u64, u64) {
    if let Some(session) = display_session(&snap.profile)
        && session.trade_date > 0
    {
        return session_range_ns(session.trade_date);
    }
    (0, 1)
}
