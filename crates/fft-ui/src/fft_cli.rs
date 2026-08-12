//! CLI argument parsing for the `fft` binary (kept out of main for the ~500-line rule).

use std::path::PathBuf;
use std::time::Duration;

use fft_ui::datetime::parse_replay_at;
use fft_ui::shell::StartupSource;

pub struct Args {
    pub gate: Option<Duration>,
    pub trace: Option<PathBuf>,
    /// Replay / sim-live / blank — mutually exclusive feed sources.
    pub startup: StartupSource,
    /// Original `--replay-at` / `--head` argument text for gate provenance.
    pub anchor_arg: Option<String>,
    /// Prior-day fftlogs, oldest-first (CLI order preserved). Replay-only.
    pub prior: Vec<PathBuf>,
    /// Disable sibling/cache prior discovery and auto-ingest.
    pub no_prior_discovery: bool,
    /// Keep existing-log discovery but disable DBN auto-ingest.
    pub no_auto_ingest: bool,
    /// Override automatic `data/GLBX-*` DBN directory resolution.
    pub dbn_dir: Option<PathBuf>,
    pub gate_out: Option<PathBuf>,
    /// Perf-runner manifest path — validated at startup, recorded verbatim in evidence.
    pub manifest: Option<PathBuf>,
    /// Free-form run conditions from the runner — recorded verbatim when supplied.
    pub conditions: Option<String>,
    /// Emit cold-start first-paint / first-interactive marks (M5 boring gate).
    pub startup_trace: bool,
    /// Scripted scrub-release→rendered gate sample count.
    pub scrub_latency_gate: Option<u32>,
    /// Evidence JSON path for `--scrub-latency-gate`.
    pub scrub_latency_out: Option<PathBuf>,
    /// RNG seed for scripted scrub targets.
    pub scrub_latency_seed: u64,
}

pub fn parse_args() -> Args {
    let mut args = std::env::args().skip(1);
    let mut gate = None;
    let mut trace = None;
    let mut replay = None;
    let mut replay_at = None;
    let mut replay_at_arg = None;
    let mut sim_live = None;
    let mut head = None;
    let mut head_arg = None;
    let mut live_out = None;
    let mut prior = Vec::new();
    let mut no_prior_discovery = false;
    let mut no_auto_ingest = false;
    let mut dbn_dir = None;
    let mut gate_out = None;
    let mut manifest = None;
    let mut conditions = None;
    let mut startup_trace = false;
    let mut scrub_latency_gate = None;
    let mut scrub_latency_out = None;
    let mut scrub_latency_seed = fft_ui::scrub_latency::DEFAULT_SEED;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--gate" => {
                let secs = args
                    .next()
                    .unwrap_or_else(|| usage("--gate requires <seconds>"));
                let secs: f64 = secs
                    .parse()
                    .unwrap_or_else(|_| usage(&format!("invalid --gate value: {secs}")));
                if !(secs > 0.0 && secs.is_finite()) {
                    usage(&format!("--gate must be a positive number, got {secs}"));
                }
                gate = Some(Duration::from_secs_f64(secs));
            }
            "--trace" => {
                let path = args
                    .next()
                    .unwrap_or_else(|| usage("--trace requires <path>"));
                trace = Some(PathBuf::from(path));
            }
            "--replay" => {
                let path = args
                    .next()
                    .unwrap_or_else(|| usage("--replay requires <fftlog>"));
                replay = Some(PathBuf::from(path));
            }
            "--replay-at" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| usage("--replay-at requires <ts>"));
                let ts = parse_replay_at(&value).unwrap_or_else(|err| usage(&err));
                replay_at = Some(ts);
                replay_at_arg = Some(value);
            }
            "--sim-live" => {
                let path = args
                    .next()
                    .unwrap_or_else(|| usage("--sim-live requires <fftlog>"));
                sim_live = Some(PathBuf::from(path));
            }
            "--head" => {
                let value = args.next().unwrap_or_else(|| usage("--head requires <ts>"));
                // Same parser family as `--replay-at` (ns digits or YYYY-MM-DDTHH:MM:SSZ).
                let ts = parse_replay_at(&value)
                    .unwrap_or_else(|err| usage(&err.replace("--replay-at", "--head")));
                head = Some(ts);
                head_arg = Some(value);
            }
            "--live-out" => {
                let path = args
                    .next()
                    .unwrap_or_else(|| usage("--live-out requires <path>"));
                live_out = Some(PathBuf::from(path));
            }
            "--prior" => {
                let path = args
                    .next()
                    .unwrap_or_else(|| usage("--prior requires <fftlog>"));
                let path = PathBuf::from(path);
                // Existence-validated before the window opens (same rationale as --manifest).
                if !path.is_file() {
                    usage(&format!("--prior file does not exist: {}", path.display()));
                }
                prior.push(path);
            }
            "--no-prior-discovery" => {
                no_prior_discovery = true;
            }
            "--no-auto-ingest" => {
                no_auto_ingest = true;
            }
            "--dbn-dir" => {
                let path = args
                    .next()
                    .unwrap_or_else(|| usage("--dbn-dir requires <path>"));
                let path = PathBuf::from(path);
                if !path.is_dir() {
                    usage(&format!("--dbn-dir is not a directory: {}", path.display()));
                }
                dbn_dir = Some(path);
            }
            "--gate-out" => {
                let path = args
                    .next()
                    .unwrap_or_else(|| usage("--gate-out requires <path>"));
                gate_out = Some(PathBuf::from(path));
            }
            "--manifest" => {
                let path = args
                    .next()
                    .unwrap_or_else(|| usage("--manifest requires <path>"));
                let path = PathBuf::from(path);
                // Same rationale as GateOut::create: bad provenance must fail before a
                // measured run is spent, never after it.
                if !path.is_file() {
                    usage(&format!(
                        "--manifest file does not exist: {}",
                        path.display()
                    ));
                }
                manifest = Some(path);
            }
            "--conditions" => {
                let text = args
                    .next()
                    .unwrap_or_else(|| usage("--conditions requires <text>"));
                conditions = Some(text);
            }
            "--startup-trace" => {
                startup_trace = true;
            }
            "--scrub-latency-gate" => {
                let n = args
                    .next()
                    .unwrap_or_else(|| usage("--scrub-latency-gate requires <N>"));
                let n: u32 = n
                    .parse()
                    .unwrap_or_else(|_| usage(&format!("invalid --scrub-latency-gate: {n}")));
                scrub_latency_gate = Some(n);
            }
            "--scrub-latency-out" => {
                let p = args
                    .next()
                    .unwrap_or_else(|| usage("--scrub-latency-out requires <path>"));
                scrub_latency_out = Some(PathBuf::from(p));
            }
            "--scrub-latency-seed" => {
                let s = args
                    .next()
                    .unwrap_or_else(|| usage("--scrub-latency-seed requires <u64>"));
                scrub_latency_seed = s
                    .parse()
                    .unwrap_or_else(|_| usage(&format!("invalid --scrub-latency-seed: {s}")));
            }
            other => usage(&format!("unknown argument: {other}")),
        }
    }

    let startup = match (replay, sim_live, replay_at, head, live_out) {
        (Some(_), Some(_), ..) => {
            usage("--sim-live is mutually exclusive with --replay / --replay-at")
        }
        (Some(_), None, Some(_), _, Some(_)) | (Some(_), None, None, Some(_), _) => {
            usage("--head / --live-out require --sim-live")
        }
        (Some(_), None, Some(_), Some(_), None) => usage("--head / --live-out require --sim-live"),
        (Some(_), None, None, None, Some(_)) => usage("--live-out requires --sim-live"),
        (None, Some(_), Some(_), ..) => {
            usage("--sim-live is mutually exclusive with --replay / --replay-at")
        }
        (None, Some(path), None, Some(head_ts), Some(live_out)) => {
            if path == live_out {
                usage("--live-out must be distinct from the --sim-live source");
            }
            StartupSource::SimLive {
                path,
                head_ts,
                live_out,
            }
        }
        (None, Some(_), None, None, _) => usage("--sim-live requires --head and --live-out"),
        (None, Some(_), None, Some(_), None) => usage("--sim-live requires --head and --live-out"),
        (Some(path), None, replay_at, None, None) => StartupSource::Replay { path, replay_at },
        (None, None, Some(_), ..) => usage("--replay-at requires --replay"),
        (None, None, None, Some(_), _) | (None, None, None, None, Some(_)) => {
            usage("--head / --live-out require --sim-live")
        }
        (None, None, None, None, None) => StartupSource::None,
    };

    if !prior.is_empty() && !matches!(startup, StartupSource::Replay { .. }) {
        usage("--prior requires --replay");
    }
    if matches!(startup, StartupSource::SimLive { .. })
        && (no_prior_discovery || no_auto_ingest || dbn_dir.is_some())
    {
        usage(
            "--no-prior-discovery / --no-auto-ingest / --dbn-dir are replay-only (not with --sim-live)",
        );
    }
    if startup_trace && !startup.starts_engine() {
        usage(
            "--startup-trace requires --replay or --sim-live (interactive mark needs a snapshot)",
        );
    }
    if let Err(msg) = fft_ui::scrub_latency::validate_cli(
        scrub_latency_gate,
        scrub_latency_out.clone(),
        startup_trace,
        gate.is_some(),
        matches!(startup, StartupSource::Replay { .. }),
    ) {
        usage(&msg);
    }

    let anchor_arg = match &startup {
        StartupSource::Replay { .. } => replay_at_arg,
        StartupSource::SimLive { .. } => head_arg,
        StartupSource::None => None,
    };

    Args {
        gate,
        trace,
        startup,
        anchor_arg,
        prior,
        no_prior_discovery,
        no_auto_ingest,
        dbn_dir,
        gate_out,
        manifest,
        conditions,
        startup_trace,
        scrub_latency_gate,
        scrub_latency_out,
        scrub_latency_seed,
    }
}

fn usage(msg: &str) -> ! {
    eprintln!(
        "fft: {msg}\nusage: fft [--gate <seconds>] [--trace <path>] [--replay <fftlog>] \
         [--replay-at <ts>] [--sim-live <fftlog>] [--head <ts>] [--live-out <path>] \
         [--prior <fftlog>]... [--no-prior-discovery] \
         [--no-auto-ingest] [--dbn-dir <path>] [--gate-out <path>] [--manifest <path>] \
         [--conditions <text>] [--startup-trace] \
         [--scrub-latency-gate <N> --scrub-latency-out <path>] [--scrub-latency-seed <u64>]\n\
         --sim-live: join + wall-pin (requires --head and --live-out; exclusive with --replay)\n\
         --head: wall-clock head (ns digits or YYYY-MM-DDTHH:MM:SSZ); snapped to last event ≤ head\n\
         --live-out: LIVE-flagged append destination (must differ from the source)\n\
         --prior: earlier trade-date fftlog (repeatable, oldest first; requires --replay)\n\
         --no-prior-discovery: disable existing-log discovery and DBN auto-ingest\n\
         --no-auto-ingest: discover existing prior logs but do not ingest missing days\n\
         --dbn-dir: override automatic data/GLBX-* DBN source resolution\n\
         --startup-trace: emit first_paint_ms / first_interactive_ms then quit (M5 cold start)\n\
         --scrub-latency-gate: N scrub-release→rendered samples then quit (needs --replay + --out)"
    );
    std::process::exit(2);
}

/// Self-identifying description of what this run measured.
pub fn gate_description(args: &Args) -> String {
    let window = match args.gate {
        Some(gate) => format!("fft frame gate — {:.3} s", gate.as_secs_f64()),
        None => "fft frame harness (ungated)".to_string(),
    };
    let base = match (&args.startup, &args.anchor_arg) {
        (StartupSource::Replay { path, .. }, Some(at)) => {
            format!("{window}, replay {} @ {at}", path.display())
        }
        (StartupSource::Replay { path, .. }, None) => {
            format!("{window}, replay {}", path.display())
        }
        (StartupSource::SimLive { path, live_out, .. }, Some(head)) => {
            format!(
                "{window}, sim-live {} head {head} live-out {}",
                path.display(),
                live_out.display()
            )
        }
        (StartupSource::SimLive { path, live_out, .. }, None) => {
            format!(
                "{window}, sim-live {} live-out {}",
                path.display(),
                live_out.display()
            )
        }
        (StartupSource::None, _) => format!("{window}, blank window"),
    };
    let n = args.prior.len();
    if n > 0 {
        format!("{base} +{n} priors")
    } else {
        base
    }
}
