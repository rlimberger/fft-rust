//! CLI for m7-soak.

use std::path::PathBuf;
use std::process::exit;

const DEFAULT_SPEED: f64 = 64.0;
const DEFAULT_SCRUB_SEEKS: u32 = 24;
const DEFAULT_LEAK_WINDOW: usize = 10;
const DEFAULT_LEAK_PCT: f64 = 10.0;

pub struct Args {
    pub replay: PathBuf,
    pub priors: Vec<PathBuf>,
    pub out: PathBuf,
    pub speed: f64,
    pub cycle_secs: u64,
    pub max_cycles: u64,
    pub max_hours: f64,
    pub scrub_seeks: u32,
    pub leak_window: usize,
    pub leak_pct: f64,
    pub label: Option<String>,
}

fn usage(msg: &str) -> ! {
    eprintln!(
        "m7-soak: {msg}\n\
         usage: m7-soak --replay <ckpt.fftlog> --prior <fftlog>... --out <metrics.jsonl> \
         [--speed F] [--cycle-secs N] [--max-cycles N] [--max-hours F] \
         [--scrub-seeks N] [--leak-window N] [--leak-pct F] [--label TEXT]\n\
         0 for --max-cycles / --max-hours / --cycle-secs = unbounded (EOF for cycle)\n\
         --prior: other-day logs oldest-first (Wed current → Mon,Tue,Thu,Fri; later dates skip)"
    );
    exit(2)
}

pub fn parse_args() -> Args {
    let mut args = std::env::args().skip(1);
    let mut replay = None;
    let mut priors = Vec::new();
    let mut out = None;
    let mut speed = DEFAULT_SPEED;
    let mut cycle_secs = 0u64;
    let mut max_cycles = 0u64;
    let mut max_hours = 0.0f64;
    let mut scrub_seeks = DEFAULT_SCRUB_SEEKS;
    let mut leak_window = DEFAULT_LEAK_WINDOW;
    let mut leak_pct = DEFAULT_LEAK_PCT;
    let mut label = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--replay" => {
                replay = Some(PathBuf::from(
                    args.next()
                        .unwrap_or_else(|| usage("missing value for --replay")),
                ));
            }
            "--prior" => {
                let p = PathBuf::from(
                    args.next()
                        .unwrap_or_else(|| usage("missing value for --prior")),
                );
                if !p.is_file() {
                    usage(&format!("--prior not a file: {}", p.display()));
                }
                priors.push(p);
            }
            "--out" => {
                out = Some(PathBuf::from(
                    args.next()
                        .unwrap_or_else(|| usage("missing value for --out")),
                ));
            }
            "--speed" => {
                speed = args
                    .next()
                    .unwrap_or_else(|| usage("missing --speed value"))
                    .parse()
                    .unwrap_or_else(|_| usage("--speed must be a finite f64 > 0"));
                if !(speed.is_finite() && speed > 0.0) {
                    usage("--speed must be finite and > 0");
                }
            }
            "--cycle-secs" => cycle_secs = parse_u64(&mut args, "--cycle-secs"),
            "--max-cycles" => max_cycles = parse_u64(&mut args, "--max-cycles"),
            "--max-hours" => {
                max_hours = args
                    .next()
                    .unwrap_or_else(|| usage("missing --max-hours value"))
                    .parse()
                    .unwrap_or_else(|_| usage("--max-hours must be a non-negative f64"));
                if max_hours < 0.0 {
                    usage("--max-hours must be >= 0");
                }
            }
            "--scrub-seeks" => {
                scrub_seeks = parse_u64(&mut args, "--scrub-seeks") as u32;
                if scrub_seeks == 0 {
                    usage("--scrub-seeks must be > 0");
                }
            }
            "--leak-window" => {
                leak_window = parse_u64(&mut args, "--leak-window") as usize;
                if leak_window < 2 {
                    usage("--leak-window must be >= 2");
                }
            }
            "--leak-pct" => {
                leak_pct = args
                    .next()
                    .unwrap_or_else(|| usage("missing --leak-pct value"))
                    .parse()
                    .unwrap_or_else(|_| usage("--leak-pct must be a non-negative f64"));
                if leak_pct < 0.0 {
                    usage("--leak-pct must be >= 0");
                }
            }
            "--label" => {
                label = Some(
                    args.next()
                        .unwrap_or_else(|| usage("missing --label value")),
                );
            }
            "-h" | "--help" => usage("help"),
            other => usage(&format!("unknown argument {other}")),
        }
    }
    let replay = replay.unwrap_or_else(|| usage("missing --replay"));
    let out = out.unwrap_or_else(|| usage("missing --out"));
    if !replay.is_file() {
        usage(&format!("replay log not found: {}", replay.display()));
    }
    if priors.is_empty() {
        usage("need at least one --prior (full week: four other days)");
    }
    Args {
        replay,
        priors,
        out,
        speed,
        cycle_secs,
        max_cycles,
        max_hours,
        scrub_seeks,
        leak_window,
        leak_pct,
        label,
    }
}

fn parse_u64(args: &mut impl Iterator<Item = String>, flag: &str) -> u64 {
    args.next()
        .unwrap_or_else(|| usage(&format!("missing {flag} value")))
        .parse()
        .unwrap_or_else(|_| usage(&format!("{flag} must be a non-negative integer")))
}
