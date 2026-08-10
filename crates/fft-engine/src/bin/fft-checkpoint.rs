//! Offline historical checkpoint pass — ENGINE.md §4 materialization item 2.
//!
//! ```text
//! fft-checkpoint <src.fftlog> <dst.fftlog>
//! ```

use fft_engine::{CheckpointSummary, write_checkpointed_copy};
use std::path::PathBuf;
use std::process::exit;

fn usage(msg: &str) -> ! {
    eprintln!("fft-checkpoint: {msg}\nusage: fft-checkpoint <src.fftlog> <dst.fftlog>");
    exit(2)
}

fn print_summary(summary: &CheckpointSummary, src: &str, dst: &str) {
    println!(
        "checkpointed {src} -> {dst}: events {} checkpoints {} src_bytes {} dst_bytes {} (+{})",
        summary.events,
        summary.checkpoints,
        summary.src_bytes,
        summary.dst_bytes,
        summary.dst_bytes.saturating_sub(summary.src_bytes),
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    let src = args.next().unwrap_or_else(|| usage("missing <src.fftlog>"));
    let dst = args.next().unwrap_or_else(|| usage("missing <dst.fftlog>"));
    if args.next().is_some() {
        usage("too many arguments");
    }

    let src_path = PathBuf::from(&src);
    let dst_path = PathBuf::from(&dst);
    match write_checkpointed_copy(&src_path, &dst_path) {
        Ok(summary) => print_summary(&summary, &src, &dst),
        Err(err) => {
            eprintln!("fft-checkpoint: {err}");
            // Apply-path panics already abort; typed failures exit 1 loudly.
            exit(1);
        }
    }
}
