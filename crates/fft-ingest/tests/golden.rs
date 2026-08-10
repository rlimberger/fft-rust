//! Golden DBN → canonical vector for Wed 2026-07-29 file head
//! (`fixtures/ingest/glbx-mdp3-20260729-head.{dbn.zst,expect}`).
//! Paths resolve from `CARGO_MANIFEST_DIR` (docs/FIXTURES.md).

use std::fs;
use std::path::{Path, PathBuf};

use fft_ingest::decode::{canonical_line, instrument_meta, open_zstd_file};

fn fixtures_ingest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ingest")
}

#[test]
fn wed_head_first_50_match_expect() {
    let dir = fixtures_ingest();
    let dbn = dir.join("glbx-mdp3-20260729-head.dbn.zst");
    let expect_path = dir.join("glbx-mdp3-20260729-head.expect");
    assert!(
        dbn.is_file(),
        "missing golden DBN fixture at {}; regenerate with: fft-ingest slice \
         data/GLBX-20260803-4WJS899FNL/glbx-mdp3-20260729.mbo.dbn.zst \
         fixtures/ingest/glbx-mdp3-20260729-head.dbn.zst 100",
        dbn.display()
    );
    let expect = fs::read_to_string(&expect_path).unwrap_or_else(|err| {
        panic!("missing expect file {}: {err}", expect_path.display());
    });
    let expect_lines: Vec<&str> = expect.lines().collect();
    assert_eq!(
        expect_lines.len(),
        50,
        "expect file must pin exactly 50 events"
    );

    let mut decoder = open_zstd_file(&dbn).expect("open golden DBN");
    let mut got = Vec::with_capacity(50);
    while got.len() < 50 {
        let ev = decoder
            .next_event()
            .expect("decode")
            .unwrap_or_else(|| panic!("fixture ended after {} events, wanted 50", got.len()));
        got.push(canonical_line(&ev));
    }
    assert_eq!(
        got, expect_lines,
        "canonical decode of Wed head diverged from fixtures/ingest/glbx-mdp3-20260729-head.expect"
    );
    assert_eq!(
        decoder.gap_count(),
        0,
        "snapshot head must not synthesize gaps"
    );
}

#[test]
fn instrument_meta_is_loud_without_definition_schema() {
    let dbn = fixtures_ingest().join("glbx-mdp3-20260729-head.dbn.zst");
    let decoder = open_zstd_file(&dbn).expect("open golden DBN");
    let err = instrument_meta(decoder.metadata()).expect_err("must not invent tick metadata");
    let msg = err.to_string();
    assert!(msg.contains("definition"), "{msg}");
    assert!(msg.contains("min_price_increment"), "{msg}");
    assert!(msg.contains("unit_of_measure_qty"), "{msg}");
}
