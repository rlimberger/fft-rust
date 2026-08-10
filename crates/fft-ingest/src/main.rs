//! `fft-ingest` binary — the M1 T2 evidence tool.
//!
//! - `stats <path>...` — stream-decode `.dbn.zst` files in the order given; print
//!   per-trade-date event counts by kind, distinct instrument ids, sequence-gap count,
//!   and first/last timestamps. Never loads a file into memory. SNAPSHOT-flagged
//!   records (each day file opens with a book-state replay whose `ts_event` is the
//!   snapshot creation time, not an event time) are counted per file, not bucketed
//!   into trade dates.
//! - `dump <path> [--limit N]` — print canonical events, one per line
//!   (`decode::canonical_line` format), including synthesized gap events.
//! - `slice <src> <dst> <count>` — copy the first `count` records into a small
//!   zstd-compressed DBN file with the source metadata verbatim (fixture generator).
//! - `write <out.fftlog> <path.dbn.zst>...` — filter one instrument + CT trade date,
//!   append canonical EVENTS frames via `fft_log::LogWriter`, clean close. Requires
//!   explicit `--tick` / `--uom-qty` / `--display-factor` (no silent ES defaults).

use std::collections::BTreeMap;
use std::path::Path;
use std::process::exit;

use dbn::decode::{DbnMetadata, DecodeRecordRef};
use dbn::encode::EncodeRecordRef;
use dbn::encode::dbn::Encoder;
use fft_core::{EventKind, Ts};
use fft_ingest::decode::{self, DecodedEvent, IngestError, open_zstd_file};
use fft_ingest::session::{TradeDateBucketer, session_open};
use fft_ingest::write::{parse_write_args, write_fftlog, write_usage};
use jiff::Timestamp;
use jiff::civil::Date;

const KINDS: [EventKind; 8] = [
    EventKind::Add,
    EventKind::Cancel,
    EventKind::Modify,
    EventKind::Trade,
    EventKind::Fill,
    EventKind::Clear,
    EventKind::Status,
    EventKind::Gap,
];

#[derive(Default)]
struct DateStats {
    by_kind: [u64; 8],
    first_ts: Option<Ts>,
    last_ts: Option<Ts>,
}

#[derive(Default)]
struct Stats {
    dates: BTreeMap<Date, DateStats>,
    instruments: BTreeMap<u32, u64>,
    gap_count: u64,
    total: u64,
}

fn utc(ts: Ts) -> String {
    Timestamp::from_nanosecond(i128::from(ts.0)).map_or_else(
        |_| format!("<out of range: {} ns>", ts.0),
        |t| t.to_string(),
    )
}

fn record(stats: &mut Stats, bucketer: &mut TradeDateBucketer, ev: &DecodedEvent) {
    let date = bucketer.bucket(ev.event.ts);
    let ds = stats.dates.entry(date).or_default();
    ds.by_kind[ev.event.kind as usize - 1] += 1;
    ds.first_ts.get_or_insert(ev.event.ts);
    ds.last_ts = Some(ev.event.ts);
    stats.total += 1;
    if ev.event.kind == EventKind::Gap {
        stats.gap_count += 1;
    } else {
        *stats.instruments.entry(ev.instrument_id).or_default() += 1;
    }
}

/// Instrument id → raw contract symbol from the DBN metadata symbol mappings. Measured
/// fact: although the batch queried `stype_in=parent` (`ES.FUT`), the file metadata
/// carries per-contract mappings (`ESU6`, `ESZ6-ESH7`, …) → instrument-id intervals.
fn raw_symbols(metadata: &dbn::Metadata, table: &mut BTreeMap<u32, String>) {
    for mapping in &metadata.mappings {
        for interval in &mapping.intervals {
            if let Ok(id) = interval.symbol.parse::<u32>() {
                table
                    .entry(id)
                    .or_insert_with(|| mapping.raw_symbol.clone());
            }
        }
    }
}

fn cmd_stats(paths: &[String]) -> Result<(), IngestError> {
    let mut stats = Stats::default();
    let mut bucketer = TradeDateBucketer::default();
    let mut symbols = BTreeMap::new();
    // (channel, first seq, last seq) per file, for cross-file continuity reporting.
    type SeqBound = (u8, u32, u32);
    let mut file_bounds: Vec<(String, Vec<SeqBound>)> = Vec::new();

    for path in paths {
        let mut decoder = open_zstd_file(Path::new(path))?;
        let md = decoder.metadata();
        println!(
            "file {path}: dataset {} schema {} dbn-version {} start {} end {}",
            md.dataset,
            md.schema.map_or_else(|| "mixed".into(), |s| s.to_string()),
            md.version,
            utc(Ts(md.start)),
            md.end.map_or_else(|| "<none>".into(), |e| utc(Ts(e.get()))),
        );
        raw_symbols(md, &mut symbols);

        let mut bounds: Vec<(u8, u32, u32)> = Vec::new();
        let mut file_events = 0u64;
        let mut snapshot_events = 0u64;
        let gaps_before = decoder.gap_count();
        let snapshot_flag = u16::from(dbn::flags::SNAPSHOT);
        while let Some(ev) = decoder.next_event()? {
            file_events += 1;
            if ev.event.kind != EventKind::Gap && ev.event.flags & snapshot_flag != 0 {
                snapshot_events += 1;
                continue; // state replay, not a session event — reported per file
            }
            record(&mut stats, &mut bucketer, &ev);
            if ev.event.kind != EventKind::Gap && ev.event.seq.0 != 0 {
                // Live sequence bounds for boundary reporting. The channel id is not on
                // DecodedEvent; events arrive in file order, so fold over the stream.
                match bounds.last_mut() {
                    Some((_, _, last)) => *last = ev.event.seq.0,
                    None => bounds.push((0, ev.event.seq.0, ev.event.seq.0)),
                }
            }
        }
        let channels: Vec<u8> = decoder.channels().collect();
        if let Some(first) = bounds.first_mut() {
            first.0 = *channels.first().unwrap_or(&0);
        }
        println!(
            "  events {file_events} (snapshot {snapshot_events}, gaps {}) channels {channels:?} live seq {:?}",
            decoder.gap_count() - gaps_before,
            bounds
                .iter()
                .map(|(ch, a, b)| format!("ch{ch}: {a}..={b}"))
                .collect::<Vec<_>>(),
        );
        file_bounds.push((path.clone(), bounds));
    }

    for pair in file_bounds.windows(2) {
        let (prev_path, prev) = &pair[0];
        let (next_path, next) = &pair[1];
        if let (Some((_, _, last)), Some((_, first, _))) = (prev.last(), next.first())
            && *first != *last
            && u64::from(*first) != u64::from(*last) + 1
        {
            println!(
                "note: file boundary discontinuity {prev_path} (last seq {last}) -> \
                 {next_path} (first seq {first}) — files decode with independent gap state"
            );
        }
    }

    println!("\nper-trade-date counts (America/Chicago trade dates):");
    for (date, ds) in &stats.dates {
        println!(
            "trade date {date} (session open {}):",
            utc(session_open(*date))
        );
        let kinds = KINDS
            .iter()
            .zip(ds.by_kind)
            .map(|(k, n)| format!("{k:?} {n}"))
            .collect::<Vec<_>>()
            .join("  ");
        println!("  {kinds}");
        println!(
            "  total {}  first ts {}  last ts {}",
            ds.by_kind.iter().sum::<u64>(),
            ds.first_ts.map_or_else(|| "-".into(), utc),
            ds.last_ts.map_or_else(|| "-".into(), utc),
        );
    }

    println!("\ninstruments ({} distinct):", stats.instruments.len());
    for (id, count) in &stats.instruments {
        let symbol = symbols
            .get(id)
            .map_or("<no symbology mapping in DBN metadata>", String::as_str);
        println!("  {id}: {count} events  {symbol}");
    }

    println!(
        "\ntotal events {}  sequence gaps {}",
        stats.total, stats.gap_count
    );
    Ok(())
}

fn cmd_dump(path: &str, limit: Option<u64>) -> Result<(), IngestError> {
    let mut decoder = open_zstd_file(Path::new(path))?;
    let mut printed = 0u64;
    while let Some(ev) = decoder.next_event()? {
        println!("{}", decode::canonical_line(&ev));
        printed += 1;
        if limit.is_some_and(|l| printed >= l) {
            break;
        }
    }
    Ok(())
}

fn cmd_slice(src: &str, dst: &str, count: u64, skip: u64) -> Result<(), IngestError> {
    let mut decoder = dbn::decode::dbn::Decoder::from_zstd_file(src)?;
    let out = std::fs::File::create(dst)
        .unwrap_or_else(|err| panic!("fft-ingest: cannot create {dst}: {err}"));
    let mut encoder = Encoder::with_zstd(out, decoder.metadata())?;
    let mut position = 0u64;
    while position < skip + count {
        let Some(rec) = decoder.decode_record_ref()? else {
            panic!(
                "fft-ingest: {src} ended after {position} records, wanted {}",
                skip + count
            );
        };
        if position >= skip {
            encoder.encode_record_ref(rec)?;
        }
        position += 1;
    }
    println!("wrote {count} records (skipped {skip}) from {src} to {dst}");
    Ok(())
}

fn cmd_write(args: &[String]) -> Result<(), IngestError> {
    let config = parse_write_args(args)?;
    let stats = write_fftlog(&config)?;
    println!(
        "wrote {} events ({} frames, {} snapshots, {} gaps) -> {} \
         instrument_id {} trade_date {}",
        stats.events_written,
        stats.frames,
        stats.snapshots_kept,
        stats.gaps_kept,
        config.output.display(),
        config.instrument_id,
        config.trade_date,
    );
    Ok(())
}

fn usage(msg: &str) -> ! {
    eprintln!(
        "fft-ingest: {msg}\nusage:\n  fft-ingest stats <path>...\n  \
         fft-ingest dump <path> [--limit N]\n  \
         fft-ingest slice <src> <dst> <count> [skip]\n  \
         fft-ingest {write_usage}",
        write_usage = write_usage()
    );
    exit(2)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("stats") => {
            if args.len() < 2 {
                usage("stats requires at least one path");
            }
            cmd_stats(&args[1..])
        }
        Some("dump") => {
            let path = args.get(1).unwrap_or_else(|| usage("dump requires <path>"));
            let limit = match args.get(2).map(String::as_str) {
                Some("--limit") => Some(
                    args.get(3)
                        .unwrap_or_else(|| usage("--limit requires N"))
                        .parse::<u64>()
                        .unwrap_or_else(|_| usage("invalid --limit value")),
                ),
                Some(other) => usage(&format!("unknown argument: {other}")),
                None => None,
            };
            cmd_dump(path, limit)
        }
        Some("slice") => {
            if args.len() != 4 && args.len() != 5 {
                usage("slice requires <src> <dst> <count> [skip]");
            }
            let count = args[3]
                .parse::<u64>()
                .unwrap_or_else(|_| usage("invalid slice count"));
            let skip = args.get(4).map_or(0, |s| {
                s.parse::<u64>()
                    .unwrap_or_else(|_| usage("invalid slice skip"))
            });
            cmd_slice(&args[1], &args[2], count, skip)
        }
        Some("write") => {
            if args.len() < 2 {
                usage("write requires <out.fftlog> and inputs");
            }
            cmd_write(&args[1..])
        }
        Some(other) => usage(&format!("unknown subcommand: {other}")),
        None => usage("missing subcommand"),
    };
    if let Err(err) = result {
        eprintln!("fft-ingest: {err}");
        exit(1);
    }
}
