//! Schema lock for the `--gate-out` JSON document. The perf runner parses this file, so a
//! renamed, dropped, or silently added field is a breaking change and fails here.

use std::path::PathBuf;

use fft_ui::gate_report::{CoverageReport, FrameResult, GateReport, GitInfo, RunMeta};

fn sample_result() -> FrameResult {
    FrameResult {
        refresh_interval_ms: 4.167,
        refresh_hz: 240.0,
        deadline_ms: 6.25,
        warmup_ms: 501.2,
        warmup_samples: 120,
        frames: 14_400,
        missed: 0,
        p50_ms: 4.17,
        p95_ms: 4.2,
        p99_ms: 4.31,
        max_ms: 5.02,
    }
}

fn sample_meta() -> RunMeta {
    RunMeta {
        gate: "fft frame gate — 60.000 s, replay fixtures/esu6.fftlog".to_string(),
        binary: "fft --gate 60 --replay fixtures/esu6.fftlog".to_string(),
        git: GitInfo {
            sha: "59639e2aa0b1c2d3e4f5060708090a0b0c0d0e0f".to_string(),
            dirty: Some(false),
        },
        replay: Some(PathBuf::from("fixtures/esu6.fftlog")),
        trace: None,
        manifest: None,
        conditions: None,
    }
}

fn as_json(report: &GateReport) -> serde_json::Value {
    serde_json::from_str(&serde_json::to_string(report).expect("gate report serializes"))
        .expect("gate report is valid JSON")
}

#[test]
fn report_json_carries_every_documented_field() {
    let report = GateReport::new(
        &sample_meta(),
        "2026-08-10T10:11:00Z".to_string(),
        Some(sample_result()),
        Some(CoverageReport::new(82_000_000, 82_000_000, 3)),
    );
    let json = as_json(&report);

    let top = json.as_object().expect("report is a JSON object");
    for field in [
        "gate",
        "date",
        "binary",
        "git_sha",
        "git_dirty",
        "replay",
        "trace",
        "manifest",
        "gpui_rev",
        "conditions",
        "notes",
        "result",
        "coverage",
        "verdict",
    ] {
        assert!(top.contains_key(field), "missing top-level field {field}");
    }
    assert_eq!(top.len(), 14, "unexpected extra top-level fields: {top:?}");

    let result = top["result"].as_object().expect("result object");
    for field in [
        "refresh_interval_ms",
        "refresh_hz",
        "deadline_ms",
        "warmup_ms",
        "warmup_samples",
        "frames",
        "missed",
        "p50_ms",
        "p95_ms",
        "p99_ms",
        "max_ms",
    ] {
        assert!(result.contains_key(field), "missing result field {field}");
    }
    assert_eq!(
        result.len(),
        11,
        "unexpected extra result fields: {result:?}"
    );

    let coverage = top["coverage"].as_object().expect("coverage object");
    for field in ["events_read", "events_applied", "dropped", "gap_records"] {
        assert!(coverage.contains_key(field), "missing coverage {field}");
    }
    assert_eq!(coverage.len(), 4);

    // M0 artifact field names and value shapes are preserved.
    assert_eq!(top["gate"], sample_meta().gate.as_str());
    assert_eq!(top["date"], "2026-08-10T10:11:00Z");
    assert_eq!(top["binary"], sample_meta().binary.as_str());
    assert_eq!(top["verdict"], "PASS");
    assert_eq!(result["frames"], 14_400);
    assert_eq!(result["missed"], 0);
    assert_eq!(result["deadline_ms"], 6.25);
    assert_eq!(coverage["dropped"], 0);
    assert_eq!(coverage["gap_records"], 3);
    assert_eq!(top["git_dirty"], false);
    assert_eq!(top["replay"], "fixtures/esu6.fftlog");
    assert!(top["trace"].is_null());
    assert!(top["manifest"].is_null());
    assert!(top["conditions"].is_null());
    let gpui_rev = top["gpui_rev"].as_str().expect("gpui_rev is a string");
    assert_eq!(gpui_rev.len(), 40, "gpui_rev must be a 40-hex git rev");
    assert!(
        gpui_rev.chars().all(|c| c.is_ascii_hexdigit()),
        "gpui_rev must be hex: {gpui_rev}"
    );
}

#[test]
fn report_json_nulls_are_explicit_when_evidence_is_absent() {
    let meta = RunMeta {
        replay: None,
        git: GitInfo {
            sha: "unknown".to_string(),
            dirty: None,
        },
        ..sample_meta()
    };
    let json = as_json(&GateReport::new(
        &meta,
        "2026-08-10T10:11:00Z".to_string(),
        None,
        None,
    ));
    assert!(json["coverage"].is_null(), "no replay => coverage null");
    assert!(json["result"].is_null(), "no frames => result null");
    assert!(json["replay"].is_null());
    assert_eq!(json["git_sha"], "unknown");
    assert!(json["git_dirty"].is_null(), "unknown sha => dirty null");
    assert!(json["manifest"].is_null(), "unsupplied manifest => null");
    assert!(
        json["conditions"].is_null(),
        "unsupplied conditions => null"
    );
    let gpui_rev = json["gpui_rev"].as_str().expect("gpui_rev always present");
    assert_eq!(gpui_rev.len(), 40);
    assert!(gpui_rev.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(json["verdict"], "FAIL", "no frames measured is a failure");
}

#[test]
fn report_carries_supplied_manifest_and_conditions_verbatim() {
    let meta = RunMeta {
        manifest: Some("perf-runner/manifests/box-a.toml".to_string()),
        conditions: Some("governor=performance SMT=off idle=60s".to_string()),
        ..sample_meta()
    };
    let report = GateReport::new(
        &meta,
        "2026-08-10T10:11:00Z".to_string(),
        Some(sample_result()),
        Some(CoverageReport::new(1_000, 1_000, 0)),
    );
    assert_eq!(
        report.manifest.as_deref(),
        Some("perf-runner/manifests/box-a.toml")
    );
    assert_eq!(
        report.conditions.as_deref(),
        Some("governor=performance SMT=off idle=60s")
    );
    let json = as_json(&report);
    assert_eq!(json["manifest"], "perf-runner/manifests/box-a.toml");
    assert_eq!(json["conditions"], "governor=performance SMT=off idle=60s");
}

#[test]
fn dropped_events_fail_the_report_verdict() {
    let json = as_json(&GateReport::new(
        &sample_meta(),
        "2026-08-10T10:11:00Z".to_string(),
        Some(sample_result()),
        Some(CoverageReport::new(82_000_000, 81_999_999, 0)),
    ));
    assert_eq!(json["result"]["missed"], 0);
    assert_eq!(json["coverage"]["dropped"], 1);
    assert_eq!(json["verdict"], "FAIL", "one dropped event fails the gate");
}
