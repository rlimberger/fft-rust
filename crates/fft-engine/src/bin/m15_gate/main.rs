//! M1.5 sim-live gate (`docs/ENGINE.md` §5).
//!
//! ```text
//! m15-gate --replay <log> --head <ts|RFC3339Z> --live-out <path> \
//!   --gate-secs <n> --out <json>
//! ```

mod args;
#[cfg(test)]
mod evidence_tests;
mod fixture;
mod identity;
mod report;
mod run;

use args::parse_args;
use report::Evidence;
use run::run as run_gate;
use std::path::Path;
use std::process::exit;

fn main() {
    let args = parse_args();
    let out = args.out.clone();
    let evidence = run_gate(args);
    if let Err(error) = write_evidence(&out, &evidence) {
        eprintln!("m15-gate: {error}");
        exit(1);
    }
    if evidence.verdict != "PASS" {
        exit(1);
    }
}

fn write_evidence(path: &Path, evidence: &Evidence) -> Result<(), String> {
    let json = serde_json::to_string_pretty(evidence)
        .map_err(|error| format!("serialize evidence: {error}"))?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, format!("{json}\n"))
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    println!("{json}");
    Ok(())
}
