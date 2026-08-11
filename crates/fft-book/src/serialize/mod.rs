//! Deterministic, separately versioned BOOK/FLOW/REFRESH checkpoint payloads.

mod book_section;
mod codec;
mod flow_section;
mod refresh_section;

use crate::book::Book;
use crate::level::{NIL, OrderOrigin};
use fft_core::Side;

/// Loud checkpoint restore failure with section-local context.
#[derive(Debug, PartialEq, Eq)]
pub enum RestoreError {
    UnsupportedVersion {
        section: &'static str,
        version: u16,
    },
    Truncated {
        section: &'static str,
    },
    Corrupt {
        section: &'static str,
        what: &'static str,
    },
}

impl std::fmt::Display for RestoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedVersion { section, version } => {
                write!(f, "{section} section version {version} unsupported")
            }
            Self::Truncated { section } => write!(f, "{section} section truncated"),
            Self::Corrupt { section, what } => {
                write!(f, "{section} section corrupt: {what}")
            }
        }
    }
}

impl std::error::Error for RestoreError {}

impl Book {
    /// Resting orders, FIFO topology, and book metadata only.
    pub fn serialize_book(&self) -> Vec<u8> {
        book_section::serialize(self)
    }

    /// Five-second per-price flow windows and Grady cB/cA counters only.
    pub fn serialize_flow(&self) -> Vec<u8> {
        flow_section::serialize(self)
    }

    /// Native-refresh state, candidates, and per-price aggregates only.
    pub fn serialize_refresh(&self) -> Vec<u8> {
        refresh_section::serialize(self)
    }

    /// Restore complete state from the three required book-owned sections.
    pub fn restore(book: &[u8], flow: &[u8], refresh: &[u8]) -> Result<Book, RestoreError> {
        let mut out = book_section::restore(book)?;
        flow_section::restore_into(flow, &mut out)?;
        out.refresh = refresh_section::restore(refresh)?;
        // Checkpoints written under the old interval GC may still carry tombstones
        // past [`crate::REFRESH_WINDOW_NS`]. Drop them against restored `now` so
        // restore+tail matches a forward book that already swept them.
        let now = out.now;
        out.refresh.gc(now);
        validate_cross_section(&out)?;
        out.check_invariants();
        Ok(out)
    }
}

fn validate_cross_section(book: &Book) -> Result<(), RestoreError> {
    // Bid/ask overlap is wire-legal: measured locked+crossed states are
    // transient intra-event-group (and post-close maintenance can persist a
    // crossed book into a checkpoint). See HANDOFF Wave 5 LOCKED-MARKET-EVIDENCE.
    // Structural topology only below — never reject on bb/ba relation.
    if book.gap_pending && (book.last_gap.is_none() || book.last_seq.is_some()) {
        return Err(RestoreError::Corrupt {
            section: "BOOK",
            what: "inconsistent pending-gap state",
        });
    }
    if book
        .last_trade
        .is_some_and(|(price, _, _)| price.checked_mul(book.tick).is_none())
    {
        return Err(RestoreError::Corrupt {
            section: "BOOK",
            what: "last-trade price exceeds fixed-point range",
        });
    }
    if book
        .tai
        .bid_price
        .is_some_and(|price| price.checked_mul(book.tick).is_none())
        || book
            .tai
            .ask_price
            .is_some_and(|price| price.checked_mul(book.tick).is_none())
    {
        return Err(RestoreError::Corrupt {
            section: "FLOW",
            what: "cB/cA price exceeds fixed-point range",
        });
    }
    for (_, order) in &book.orders {
        if order.epoch > book.refresh.gap_epoch {
            return Err(RestoreError::Corrupt {
                section: "REFRESH",
                what: "order epoch exceeds gap epoch",
            });
        }
        if order.origin == OrderOrigin::Snapshot
            && !book
                .refresh
                .live
                .get(&order.id)
                .is_some_and(|state| state.unavailable)
        {
            return Err(RestoreError::Corrupt {
                section: "REFRESH",
                what: "snapshot order lacks unavailable refresh state",
            });
        }
    }
    for id in book.refresh.live.keys() {
        if !book.index.contains_key(id) {
            return Err(RestoreError::Corrupt {
                section: "REFRESH",
                what: "live refresh entry has no resting order",
            });
        }
    }
    for (id, tombstone) in &book.refresh.tombstones {
        if book.index.contains_key(id) {
            return Err(RestoreError::Corrupt {
                section: "REFRESH",
                what: "tombstoned order is resting",
            });
        }
        if tombstone.epoch > book.refresh.gap_epoch {
            return Err(RestoreError::Corrupt {
                section: "REFRESH",
                what: "tombstone epoch exceeds gap epoch",
            });
        }
        if tombstone.depleted_ts > book.now {
            return Err(RestoreError::Corrupt {
                section: "REFRESH",
                what: "tombstone time exceeds book time",
            });
        }
        if tombstone.price.checked_mul(book.tick).is_none() {
            return Err(RestoreError::Corrupt {
                section: "REFRESH",
                what: "tombstone price exceeds fixed-point range",
            });
        }
    }
    for (id, progress) in &book.refresh.fills {
        let Some(&slot) = book.index.get(id) else {
            return Err(RestoreError::Corrupt {
                section: "REFRESH",
                what: "Fill progress has no resting order",
            });
        };
        if book.refresh.tombstones.contains_key(id) || progress.epoch > book.refresh.gap_epoch {
            return Err(RestoreError::Corrupt {
                section: "REFRESH",
                what: "inconsistent Fill progress ownership",
            });
        }
        let order = &book.orders[slot as usize];
        if progress.depleted_ts.is_none() && progress.epoch != book.refresh.gap_epoch {
            return Err(RestoreError::Corrupt {
                section: "REFRESH",
                what: "partial Fill progress crosses a gap",
            });
        }
        if progress.depleted_ts.is_some() != (progress.qty >= u64::from(order.size)) {
            return Err(RestoreError::Corrupt {
                section: "REFRESH",
                what: "Fill progress disagrees with displayed size",
            });
        }
        if progress.depleted_ts.is_some_and(|ts| ts > book.now) {
            return Err(RestoreError::Corrupt {
                section: "REFRESH",
                what: "Fill depletion time exceeds book time",
            });
        }
    }
    for &(side, price) in book.refresh.per_price.keys() {
        if Side::from_u8(side).is_none() || price.checked_mul(book.tick).is_none() {
            return Err(RestoreError::Corrupt {
                section: "REFRESH",
                what: "aggregate price exceeds fixed-point range",
            });
        }
    }
    validate_side(book, Side::Bid)?;
    validate_side(book, Side::Ask)?;
    Ok(())
}

fn validate_side(book: &Book, side: Side) -> Result<(), RestoreError> {
    let side_book = if side == Side::Bid {
        &book.bids
    } else {
        &book.asks
    };
    let mut valid = true;
    side_book.try_for_each_populated(|_, level| {
        let mut cur = level.head;
        let mut walked = 0u32;
        while cur != NIL {
            walked += 1;
            if walked > level.order_count {
                valid = false;
                return false;
            }
            cur = book.orders[cur as usize].next;
        }
        if walked != level.order_count {
            valid = false;
            return false;
        }
        true
    });
    if valid {
        Ok(())
    } else {
        Err(RestoreError::Corrupt {
            section: "BOOK",
            what: "invalid FIFO linkage",
        })
    }
}
