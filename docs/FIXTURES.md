# Fixture Policy (M0 freeze 5)

**Status: FROZEN.**

## Storage

- **In Git:** only small deterministic fixtures under `fixtures/`, ≤ 1 MiB per file —
  golden DBN→canonical vectors, torn-tail logs, native-refresh scenario logs, corrupt
  frame/index/checkpoint cases.
- **Out of Git:** all large market data. `data/` at the repo root (raw zstd DBN week +
  legacy-format session logs, ≈ 5.7 GB) is gitignored and used read-only through M5. It
  also stands in for the live feed via the sim-live source (PRD §6) until Databento
  credentials exist. Generated `*.fftlog` files are gitignored everywhere. The dead prior
  attempt's source is parked at `~/Projects/fft-legacy` (reference only, outside this repo).
- The breadth CSVs and Schwab OAuth artifacts under `data/` are unrelated to FFT; they
  stay ignored and are never referenced by the new workspace.

## Acquisition and verification

- Source: Databento batch job **GLBX-20260803-4WJS899FNL** (GLBX.MDP3, `mbo` +
  `definition` + `status`, ESU6 sample week, 82 M events).
- M1 T2 (`fft-ingest`) commits `fixtures/MANIFEST.sha256`: relative path + SHA-256 for
  every large artifact the tests consume. Tests verify hashes before trusting data and
  fail loudly on mismatch or absence — with the acquisition instruction in the error.

## Test data resolution

- Tests resolve paths from `CARGO_MANIFEST_DIR` (small fixtures) or `FFT_DATA_DIR`
  (large data; default `<repo>/data`). Both are documented here — no other
  environment variables, and every test runs from any working directory.
- Tests requiring large data are `#[ignore]`-gated behind their data check so a bare
  `cargo test --workspace` is always green on a fresh clone.
