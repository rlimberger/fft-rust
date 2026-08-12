//! Prior-session profile builds (`docs/ENGINE.md` §2).

use fft_book::Book;
use fft_core::InstrumentMeta;
use fft_profile::{MultiProfile, SessionProfile};
use fft_replay::{ReplayError, ReplaySource};
use std::path::PathBuf;
use std::time::Duration;

/// Time-budgeted slice for one prior-session build step.
pub(crate) const PRIOR_BUILD_BUDGET: Duration = Duration::from_millis(2);

/// In-progress profile-only build of an earlier trade date.
pub(crate) struct PriorBuild {
    pub path: PathBuf,
    pub source: ReplaySource,
    pub book: Book,
    pub profile: MultiProfile,
}

pub(crate) fn start_prior_build(
    path: PathBuf,
    source_meta: Option<&InstrumentMeta>,
    profile: Option<&MultiProfile>,
    source_warnings: &mut Vec<String>,
    prior_skips: &mut u64,
) -> Option<PriorBuild> {
    let Some(current_meta) = source_meta else {
        skip_prior(
            &path,
            "no current source (LoadPriorSession requires SetSource first)",
            prior_skips,
        );
        return None;
    };
    let mut source = match ReplaySource::open(&path) {
        Ok(source) => source,
        Err(err) => {
            skip_prior(&path, &format!("open failed: {err}"), prior_skips);
            return None;
        }
    };
    source_warnings.extend(source.open_report().warnings.iter().cloned());
    let prior_meta = source.meta().clone();
    if prior_meta.instrument_id != current_meta.instrument_id
        || prior_meta.symbol != current_meta.symbol
    {
        skip_prior(
            &path,
            &format!(
                "instrument mismatch: prior {}/{} vs current {}/{}",
                prior_meta.symbol,
                prior_meta.instrument_id,
                current_meta.symbol,
                current_meta.instrument_id
            ),
            prior_skips,
        );
        return None;
    }
    if prior_meta.trade_date >= current_meta.trade_date {
        skip_prior(
            &path,
            &format!(
                "trade date {} is not earlier than current {}",
                prior_meta.trade_date, current_meta.trade_date
            ),
            prior_skips,
        );
        return None;
    }
    if profile.is_some_and(|p| p.session(prior_meta.trade_date).is_some()) {
        skip_prior(
            &path,
            &format!(
                "trade date {} already present in profile",
                prior_meta.trade_date
            ),
            prior_skips,
        );
        return None;
    }
    let (book, profile) = match source.prepare_prior_build() {
        Ok(state) => state,
        Err(err) => {
            skip_prior(
                &path,
                &format!("checkpoint restore failed: {err}"),
                prior_skips,
            );
            return None;
        }
    };
    Some(PriorBuild {
        path,
        source,
        book,
        profile,
    })
}

pub(crate) fn advance_prior_build(build: &mut PriorBuild) -> Result<bool, ReplayError> {
    let progress = build.source.apply_forward(
        &mut build.book,
        &mut build.profile,
        usize::MAX,
        PRIOR_BUILD_BUDGET,
    )?;
    Ok(progress.eof)
}

/// Insert the completed prior session. Returns `true` when a publication is needed.
pub(crate) fn finish_prior_build(
    build: PriorBuild,
    profile: &mut Option<MultiProfile>,
    prior_skips: &mut u64,
    priors_completed: &mut u64,
) -> bool {
    let path = build.path;
    let mut sessions = build.profile.sessions().to_vec();
    if sessions.is_empty() {
        skip_prior(&path, "prior log produced zero sessions", prior_skips);
        return false;
    }
    if sessions.len() > 1 {
        eprintln!(
            "fft-engine LoadPriorSession {}: prior log has {} sessions; using last only",
            path.display(),
            sessions.len()
        );
    }
    let session: SessionProfile = sessions.pop().expect("non-empty");
    let date = session.trade_date();
    let reject = match profile.as_ref() {
        None => Some("no current profile at completion".to_string()),
        Some(profile) if profile.session(date).is_some() => {
            Some(format!("trade date {date} already present at completion"))
        }
        Some(profile) => match profile.current() {
            None => Some("no current session at completion".to_string()),
            Some(current) if date >= current.trade_date() => Some(format!(
                "trade date {date} is not earlier than current {} at completion",
                current.trade_date()
            )),
            Some(_) => None,
        },
    };
    if let Some(reason) = reject {
        skip_prior(&path, &reason, prior_skips);
        return false;
    }
    profile
        .as_mut()
        .expect("validated present")
        .insert_prior_session(session);
    *priors_completed += 1;
    true
}

pub(crate) fn skip_prior(path: &std::path::Path, reason: &str, prior_skips: &mut u64) {
    *prior_skips += 1;
    eprintln!(
        "fft-engine LoadPriorSession skipped {}: {reason}",
        path.display()
    );
}
