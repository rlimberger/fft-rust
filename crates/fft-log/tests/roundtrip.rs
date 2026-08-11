//! Encode-then-decode identity: deterministic cases plus a property test over
//! arbitrary event vectors, including >u32::MAX ns timestamp jumps (forcing TsReset in
//! both directions) and Gap events. Also covers header metadata and checkpoint
//! round-trips through the public API.

mod common;

use common::{es_meta, mono_events, read_all_events, temp_path};
use fft_core::{CanonicalEvent, EventKind, OrderId, Price, Seq, Side, Ts};
use fft_log::{
    IndexSource, KIND_CHECKPOINT, KIND_EVENTS, LogReader, LogWriter, SECTION_BOOK,
    SECTION_FLAG_OPTIONAL, SECTION_PROFILE, SectionRef, VERSION_MAJOR, VERSION_MINOR,
};
use proptest::prelude::*;

#[test]
fn header_meta_round_trips() {
    let tmp = temp_path("meta");
    common::write_closed(tmp.path(), &[mono_events(10, 1_000, 1)]);
    let (reader, report) = LogReader::open(tmp.path()).unwrap();
    assert_eq!(report.index_source, IndexSource::Footer);
    assert!(report.recovery.is_none());
    assert!(report.warnings.is_empty());
    assert_eq!(reader.meta(), &es_meta());
    assert_eq!(reader.version(), (VERSION_MAJOR, VERSION_MINOR));
    assert_eq!(reader.schema_tag(), "mbo");
    assert!(!reader.opened_live());
}

#[test]
fn events_round_trip_with_ts_jumps_and_gap() {
    let mut events = mono_events(100, 1_000_000, 1);
    // Forward jump past u32::MAX ns, then a backwards step: both force TsReset.
    events.push(common::ev(
        EventKind::Trade,
        Side::None,
        u64::from(u32::MAX) * 3,
        200,
    ));
    events.push(common::ev(EventKind::Add, Side::Bid, 500, 201));
    events.push(CanonicalEvent::gap(Ts(600), 202, 210));

    let tmp = temp_path("jumps");
    common::write_closed(tmp.path(), &[events.clone()]);
    let (decoded, _) = read_all_events(tmp.path());
    assert_eq!(decoded, events);
}

#[test]
fn multi_frame_and_empty_batches_round_trip() {
    let batches = vec![
        mono_events(50, 1_000, 1),
        Vec::new(),
        mono_events(50, 1_000_000, 100),
    ];
    let tmp = temp_path("multi");
    common::write_closed(tmp.path(), &batches);
    let (decoded, _) = read_all_events(tmp.path());
    let expected: Vec<_> = batches.into_iter().flatten().collect();
    assert_eq!(decoded, expected);

    let (reader, _) = LogReader::open(tmp.path()).unwrap();
    assert_eq!(reader.frame_count(), 3);
    assert!(reader.index().iter().all(|e| e.kind == KIND_EVENTS));
    // Frame-range access decodes just the requested frames.
    let last: Vec<_> = reader.events(2..3).collect::<Result<_, _>>().unwrap();
    assert_eq!(last, mono_events(50, 1_000_000, 100));
}

#[test]
fn checkpoint_sections_round_trip_in_event_stream() {
    let tmp = temp_path("checkpoint");
    let batch = mono_events(20, 5_000, 1);
    let book = vec![0xB0u8; 400];
    let profile = vec![0x9Fu8; 100];
    let mut w = LogWriter::create(tmp.path(), &es_meta()).unwrap();
    w.append_events(&batch).unwrap();
    w.write_checkpoint([
        SectionRef {
            id: SECTION_BOOK,
            version: 1,
            flags: 0,
            bytes: &book,
        },
        SectionRef {
            id: SECTION_PROFILE,
            version: 3,
            flags: SECTION_FLAG_OPTIONAL,
            bytes: &profile,
        },
    ])
    .unwrap();
    w.append_events(&batch).unwrap();
    w.close().unwrap();

    let (reader, _) = LogReader::open(tmp.path()).unwrap();
    assert_eq!(reader.frame_count(), 3);
    assert_eq!(reader.index()[1].kind, KIND_CHECKPOINT);
    // Checkpoint index entry is stamped with the last appended event's ts.
    assert_eq!(reader.index()[1].first_ts, batch.last().unwrap().ts.0);

    let sections = reader.read_checkpoint(1).unwrap();
    assert_eq!(sections.len(), 2);
    assert_eq!(
        (sections[0].id, sections[0].version, &sections[0].bytes),
        (SECTION_BOOK, 1, &book)
    );
    assert_eq!(sections[1].flags, SECTION_FLAG_OPTIONAL);
    assert_eq!(sections[1].bytes, profile);

    // Events iteration over the full range skips the checkpoint frame.
    let events: Vec<_> = reader
        .events(0..reader.frame_count())
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(events.len(), 2 * batch.len());
}

fn arb_event() -> impl Strategy<Value = CanonicalEvent> {
    let arb_ts = prop_oneof![
        3 => 0u64..10_000_000,
        2 => any::<u64>(), // arbitrary: forces TsReset forward jumps and backwards steps
    ];
    (
        (1u8..=8).prop_map(|k| EventKind::from_u8(k).unwrap()),
        (0u8..=2).prop_map(|s| Side::from_u8(s).unwrap()),
        any::<u16>(),
        any::<u32>(),
        arb_ts,
        any::<u32>(),
        any::<i64>(),
        any::<u64>(),
    )
        .prop_map(
            |(kind, side, flags, size, ts, seq, price, order_id)| CanonicalEvent {
                kind,
                side,
                flags,
                size,
                ts: Ts(ts),
                seq: Seq(seq),
                price: Price(price),
                order_id: OrderId(order_id),
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn encode_then_decode_is_identity(
        events in proptest::collection::vec(arb_event(), 0..300),
        frame_size in 1usize..64,
    ) {
        let tmp = temp_path("prop");
        let mut w = LogWriter::create(tmp.path(), &es_meta()).unwrap();
        for chunk in events.chunks(frame_size) {
            w.append_events(chunk).unwrap();
        }
        w.close().unwrap();
        let (decoded, _) = read_all_events(tmp.path());
        prop_assert_eq!(decoded, events);
    }
}
