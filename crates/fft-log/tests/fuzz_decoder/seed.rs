//! Seed corpus builders for the LOG-FUZZ harness.

use fft_log::{
    LogReader, LogWriter, SECTION_BOOK, SECTION_FLAG_OPTIONAL, SECTION_PROFILE, SectionRef,
};

use crate::common::{self, es_meta, mono_events, temp_path};

#[derive(Clone, Copy, Debug)]
pub(crate) enum SeedKind {
    EventsOnly,
    WithCheckpoint,
    LiveTornTail,
    WithGaps,
}

impl SeedKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            SeedKind::EventsOnly => "events_only",
            SeedKind::WithCheckpoint => "with_checkpoint",
            SeedKind::LiveTornTail => "live_torn_tail",
            SeedKind::WithGaps => "with_gaps",
        }
    }
}

/// Build 4 small valid logs via `LogWriter` (events-only; checkpoint; LIVE torn-tail;
/// gap records). Returns `(kind, bytes)`.
///
/// Sizes are intentionally small (hundreds of bytes) so exhaustive bit-flips and
/// truncations finish well under the 2–5 min budget while still covering every
/// decoder path. Larger production logs are already exercised by M1/M2 gates.
pub(crate) fn build_seed_corpus() -> Vec<(SeedKind, Vec<u8>)> {
    let mut out = Vec::with_capacity(4);

    // 1. Events-only closed log: two small EVENTS frames.
    {
        let tmp = temp_path("seed-events");
        let bytes = common::write_closed(
            tmp.path(),
            &[mono_events(8, 1_000, 1), mono_events(8, 10_000, 9)],
        );
        out.push((SeedKind::EventsOnly, bytes));
    }

    // 2. Events + checkpoint + events, closed.
    {
        let tmp = temp_path("seed-ckpt");
        let batch = mono_events(6, 5_000, 1);
        let book = vec![0xB0u8; 48];
        let profile = vec![0x9Fu8; 24];
        let mut w = LogWriter::create(tmp.path(), &es_meta()).expect("create");
        w.append_events(&batch).expect("events");
        w.write_checkpoint([
            SectionRef {
                id: SECTION_BOOK,
                version: 1,
                flags: 0,
                bytes: &book,
            },
            SectionRef {
                id: SECTION_PROFILE,
                version: 1,
                flags: SECTION_FLAG_OPTIONAL,
                bytes: &profile,
            },
        ])
        .expect("checkpoint");
        w.append_events(&batch).expect("events2");
        w.close().expect("close");
        out.push((
            SeedKind::WithCheckpoint,
            std::fs::read(tmp.path()).expect("read"),
        ));
    }

    // 3. LIVE torn-tail: two committed frames, then a partial third (unclean crash).
    {
        let tmp = temp_path("seed-live");
        let batches = [
            mono_events(6, 1_000, 1),
            mono_events(6, 50_000, 7),
            mono_events(6, 100_000, 13),
        ];
        let full = common::write_live(tmp.path(), &batches);
        // Locate final frame start via a clean open of the full LIVE file, then cut mid-frame.
        let open_tmp = temp_path("seed-live-open");
        std::fs::write(open_tmp.path(), &full).expect("write");
        let (reader, _) = LogReader::open(open_tmp.path()).expect("open live");
        assert_eq!(reader.frame_count(), 3);
        let final_off = reader.index()[2].offset as usize;
        drop(reader);
        // Keep half of the final frame as uncommitted tail.
        let cut = final_off + (full.len() - final_off) / 2;
        out.push((SeedKind::LiveTornTail, full[..cut].to_vec()));
    }

    // 4. Closed log whose event stream includes Gap records (mono_events inserts one).
    {
        let tmp = temp_path("seed-gaps");
        // n > 4 guarantees a Gap at n/2 inside mono_events.
        let bytes = common::write_closed(
            tmp.path(),
            &[mono_events(12, 1_000, 1), mono_events(12, 200_000, 20)],
        );
        out.push((SeedKind::WithGaps, bytes));
    }

    out
}
