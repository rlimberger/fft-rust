# Performance Runner (M0 freeze 4)

**Status: FROZEN** (spec; the physical box is provisioned during M0).

## Split of duties

- **Shared CI (GitHub-hosted):** correctness only — fmt, clippy, tests, property tests,
  golden fixtures, deterministic replay-equivalence. No absolute timing gates: noisy
  shared runners make timing gates flaky, and flaky gates get disabled.
- **Perf runner (self-hosted, label `fft-perf`):** every timing gate, merge-blocking from
  M1 onward. One job at a time; a red frame budget is a red build.

## Pinned configuration

The runner publishes a machine manifest; a gate result without a matching manifest is
invalid. Pinned: CPU model + `performance` governor + SMT setting, core isolation for the
engine and UI threads, GPU + driver version, kernel version, compositor (Hyprland)
version, display mode (must match the committed manifest; 240 Hz hardware is mandatory
from the M3 gate onward), Rust toolchain, build profile (`release`, `lto = "thin"`,
`codegen-units = 1`), thermal precondition (60 s idle, package temp below the manifest
threshold, before any measured run).

Every result is stored with metadata JSON: full manifest + git SHA + timestamp + fixture
hashes. History is append-only.

## Evidence files (`--gate-out`)

The `fft` binary writes self-identifying JSON evidence via `--gate-out <path>`: gate
description + command line, `git_sha`/`git_dirty`, RFC 3339 timestamp, frame-time
distribution (p50/p95/p99/max, missed deadlines), coverage counters, and `null`
placeholders for manifest/`gpui_rev` until the runner manifest lands. Evidence is written
on **FAIL as well as PASS** — a failed gate must leave its numbers behind.

- Naming: `perf-runner/results/<YYYY-MM-DD>-<gate>.json` (e.g. `2026-08-10-m0-frame-gate.json`).
- CI (`perf.yml`, `workflow_dispatch` only): sets `GATE_OUT` before the run, uploads the
  file as an artifact (`retention-days: 90`, `if: always()`). **CI never commits results.**
- Repo-side `perf-runner/results/` history is committed only by the orchestrator when a
  gate run is accepted; that committed history is the append-only record the regression
  check reads.

## Gate evaluation

Two checks per metric, both must pass:

1. **Absolute budget** — the PRD/plan number (e.g. missed deadlines = 0, p99 frame time
   within budget, seek p95 ≤ 250 ms).
2. **Statistical regression** — against the last 20 runs on the same manifest: fail when
   the median worsens ≥ 3 % with Mann–Whitney U significance p < 0.01. This catches
   drift long before an absolute budget breaks (drift killed attempt one).

Physical 240 Hz presentation (photon-level validation) is a milestone/release activity on
this box, not a per-merge gate.
