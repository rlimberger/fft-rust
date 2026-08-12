//! Live-append coverage and deterministic replay identity checks.

use crate::report::{AppendCheck, IdentityCheck, LiveLifecycle, WatermarkEvidence};
use fft_book::Book;
use fft_engine::EngineExit;
use fft_log::{IndexSource, LogReader};
use fft_profile::MultiProfile;
use fft_replay::ReplaySource;
use std::path::Path;

pub struct Sections {
    book: Vec<u8>,
    flow: Vec<u8>,
    refresh: Vec<u8>,
    profile: Vec<u8>,
    cvd: Vec<u8>,
    session: Vec<u8>,
}

pub fn sections_from_exit(exit: &EngineExit) -> Result<Sections, String> {
    let profile = exit
        .profile_bytes
        .as_ref()
        .ok_or_else(|| "missing profile sections at shutdown".to_string())?;
    let book = exit
        .book_bytes
        .clone()
        .ok_or_else(|| "missing book section at shutdown".to_string())?;
    let flow = exit
        .flow_bytes
        .clone()
        .ok_or_else(|| "missing flow section at shutdown".to_string())?;
    let refresh = exit
        .refresh_bytes
        .clone()
        .ok_or_else(|| "missing refresh section at shutdown".to_string())?;
    Ok(Sections {
        book,
        flow,
        refresh,
        profile: profile.profile.clone(),
        cvd: profile.cvd.clone(),
        session: profile.session.clone(),
    })
}

pub fn replay_live_identity(live_out: &Path, pre: &Sections) -> Result<IdentityCheck, String> {
    let mut source = ReplaySource::open(live_out)
        .map_err(|error| format!("open live_out {}: {error}", live_out.display()))?;
    let meta = source.meta().clone();
    let mut book = Book::new(meta.min_price_increment);
    let mut profile = MultiProfile::new(meta.min_price_increment);
    profile.begin_session(meta.trade_date);
    let mut replayed_events = 0;
    while source
        .apply_next(&mut book, &mut profile)
        .map_err(|error| format!("live replay apply: {error}"))?
        .is_some()
    {
        replayed_events += 1;
    }
    book.check_invariants();
    let secs = profile.serialize();
    let book_bytes = book.serialize_book();
    let flow_bytes = book.serialize_flow();
    let refresh_bytes = book.serialize_refresh();
    let pairs = [
        ("BOOK", pre.book.as_slice(), book_bytes.as_slice()),
        ("FLOW", pre.flow.as_slice(), flow_bytes.as_slice()),
        ("REFRESH", pre.refresh.as_slice(), refresh_bytes.as_slice()),
        ("PROFILE", pre.profile.as_slice(), secs.profile.as_slice()),
        ("CVD", pre.cvd.as_slice(), secs.cvd.as_slice()),
        ("SESSION", pre.session.as_slice(), secs.session.as_slice()),
    ];
    let first_mismatch = pairs
        .iter()
        .find_map(|(name, before, replayed)| (before != replayed).then(|| (*name).to_string()));
    Ok(IdentityCheck {
        replayed_events,
        replayed_applied_seq: source.applied_seq(),
        replayed_applied_ts: source.applied_ts(),
        compared_sections: pairs
            .iter()
            .map(|(name, _, _)| (*name).to_string())
            .collect(),
        ok: replayed_events > 0 && first_mismatch.is_none(),
        first_mismatch,
    })
}

/// Probe LIVE-flag state while the engine still owns the append destination.
pub fn probe_live_during(live_out: &Path) -> (bool, bool) {
    match LogReader::open(live_out) {
        Ok((reader, report)) => (
            reader.is_live(),
            report.index_source == IndexSource::LiveRecovery,
        ),
        Err(_) => (false, false),
    }
}

pub fn append_check(
    live_out: &Path,
    exit: &EngineExit,
    during_is_live: bool,
    during_index_source_live_recovery: bool,
) -> AppendCheck {
    let watermarks = WatermarkEvidence {
        received_seq: exit.watermarks.received_seq,
        decoded_seq: exit.watermarks.decoded_seq,
        applied_seq: exit.watermarks.applied_seq,
        logged_seq: exit.watermarks.logged_seq,
        published_seq: exit.watermarks.published_seq,
    };
    let clean_coverage = exit.coverage.events_read == exit.coverage.events_applied;
    let logged_through_applied = exit.watermarks.logged_seq == exit.watermarks.applied_seq;
    let live_out_bytes = std::fs::metadata(live_out).map_or(0, |meta| meta.len());
    let live_lifecycle =
        probe_live_after(live_out, during_is_live, during_index_source_live_recovery);
    let source_warnings_empty = exit.source_warnings.is_empty();
    let lifecycle_ok = live_lifecycle.during_is_live
        && live_lifecycle.during_index_source_live_recovery
        && live_lifecycle.after_not_live
        && live_lifecycle.after_index_source_footer
        && live_lifecycle.after_recovery_none
        && live_lifecycle.after_warnings_empty;
    AppendCheck {
        live_out_bytes,
        events_read: exit.coverage.events_read,
        events_applied: exit.coverage.events_applied,
        gap_records: exit.coverage.gap_records,
        watermarks,
        source_warnings: exit.source_warnings.clone(),
        live_lifecycle,
        clean_coverage,
        logged_through_applied,
        ok: live_out_bytes > 0
            && exit.coverage.events_applied > 0
            && clean_coverage
            && logged_through_applied
            && source_warnings_empty
            && lifecycle_ok,
    }
}

fn probe_live_after(
    live_out: &Path,
    during_is_live: bool,
    during_index_source_live_recovery: bool,
) -> LiveLifecycle {
    match LogReader::open(live_out) {
        Ok((reader, report)) => LiveLifecycle {
            during_is_live,
            during_index_source_live_recovery,
            after_not_live: !reader.is_live(),
            after_index_source_footer: report.index_source == IndexSource::Footer,
            after_recovery_none: report.recovery.is_none(),
            after_warnings_empty: report.warnings.is_empty(),
        },
        Err(_) => LiveLifecycle {
            during_is_live,
            during_index_source_live_recovery,
            after_not_live: false,
            after_index_source_footer: false,
            after_recovery_none: false,
            after_warnings_empty: false,
        },
    }
}
