# FFT — Operator Runbook

Everything an operator needs to run, measure, and diagnose FFT. Facts verified against
source 2026-08-11 (fact-collection pass; citations in the session record). Budgets and
gate law live in `PRD.md` / `IMPLEMENTATION-PLAN.md` / `docs/PERF-RUNNER.md`.

## 1. The `fft` binary

```
fft [--gate <seconds>] [--trace <path>] [--replay <fftlog>] [--replay-at <ts>]
    [--sim-live <fftlog>] [--head <ts>] [--live-out <path>]
    [--prior <fftlog>]... [--no-prior-discovery] [--no-auto-ingest] [--dbn-dir <path>]
    [--gate-out <path>] [--manifest <path>] [--conditions <text>] [--startup-trace]
```

| Flag | Meaning | Rules |
|---|---|---|
| `--gate <secs>` | Measured frame-gate window; process fails on a missed deadline or dropped event | positive finite number |
| `--trace <path>` | Per-frame gap trace (ns per line), written after the window closes | |
| `--replay <fftlog>` | Spawn the engine, play the log | exclusive with `--sim-live` |
| `--replay-at <ts>` | Start anchored at a UTC instant (Seek before Play) | needs `--replay`; digits = ns UTC or `YYYY-MM-DDTHH:MM:SSZ`; needs a checkpointed log |
| `--sim-live <fftlog>` | Join at session open; wall-pin at `--head` | requires `--head` + `--live-out`; exclusive with `--replay` / `--replay-at` |
| `--head <ts>` | Wall-clock sim-live head | needs `--sim-live`; same forms as `--replay-at`; **snapped** to last in-log event ≤ head (engine needs an exact event ts) |
| `--live-out <path>` | LIVE-flagged append destination for sim-live | needs `--sim-live`; must differ from the source path |
| `--prior <fftlog>` | Async prior-session load into the MP; repeatable, **oldest first** | **replay-only**; file must exist; wrong dates are loud counted skips |
| `--no-prior-discovery` / `--no-auto-ingest` / `--dbn-dir` | Prior discovery / DBN ingest controls | **replay-only** (rejected with `--sim-live`) |
| `--gate-out <path>` | Self-identifying JSON evidence (written on FAIL too) | unwritable path fails before the run, not after |
| `--manifest <path>` | Perf-runner manifest recorded in evidence | must exist |
| `--conditions <text>` | Free-form run conditions recorded verbatim | |
| `--startup-trace` | Print first-paint / first-interactive ms, then exit | needs `--replay` or `--sim-live` |

Bad flags/values → usage on stderr, exit 2. Evidence always carries git SHA+dirty and the
pinned gpui rev (baked at build time from Cargo.lock).

## 2. Keys (as implemented)

Launch: MP full-width; DOM hidden every start. `d` toggles the DOM without resetting MP
navigation (pan/zoom/center). When hidden: no splitter, no DOM hit targets.

| Key | Action |
|---|---|
| `1` `2` `4` | Tick scale of the pane under the cursor (no hover → no-op) |
| `t` | Copy hovered pane's scale to the other pane |
| `c` | Price-only recenter (clear locked center; MP pan/zoom untouched) |
| `d` | Toggle DOM surface (launch-local; MP nav preserved) |
| `r` | Toggle the transport strip (chrome only; playback state untouched). Arms transport keys below. **On at spawn for `--sim-live`.** |
| `space` | Play / pause — requires transport on; silent no-op otherwise |
| `]` / `[` | Speed up / down the ladder 0.25×…64× — requires transport on; silent no-op otherwise |
| `←` / `→` | Step ±1 s (Seek, clamped to session) — requires transport on; silent no-op otherwise |
| `l` | GoLive — requires transport on + active sim-live; else loud hint (`go-live: needs sim-live`); silent no-op if transport off |
| MP left-drag | Vertical → price pan; horizontal → strip pan |
| MP wheel | Plain or Ctrl+wheel: horizontal zoom 0.5×–3× at cursor (never pan) |
| DOM drag / wheel | Vertical price pan (only when DOM shown) |
| hover (DOM row) | Per-price readout: orders, size, hidden volume, reload count per side |

Modified keys (Ctrl/Alt/Shift chords other than MP Ctrl+wheel) are ignored. `e` is unbound.
When the DOM is shown and the linked center cannot sit mid-window in engine depth, the ladder
synthesizes a zero-filled scaled-tick lattice centered on that price (source overlap kept;
no fabricated sizes/inside).

## 3. Fixtures & recipes

Volatile fixtures live in `/tmp` and die on reboot. Regeneration (expected counts are a
defect check — any deviation: stop and investigate):

```bash
cargo run --release -p fft-ingest -- write /tmp/esu6-wed-v3.fftlog \
  data/GLBX-20260803-4WJS899FNL/*.mbo.dbn.zst --trade-date 2026-07-29 \
  --tick 250000000 --uom-qty 50000000000 --display-factor 1
# Expect: 21,401,139 events · 0 gaps · 1880 seq_holes_ignored ·
#         7561 snapshots kept / 30,200 dropped (six-file run)

cargo run --release -p fft-engine --bin fft-checkpoint -- \
  /tmp/esu6-wed-v3.fftlog /tmp/esu6-wed-v3-ckpt.fftlog
# Expect: 1393 checkpoints
```

Other trade dates: same command with `--trade-date 2026-07-27 … 2026-07-31` →
`/tmp/esu6-<date>.fftlog` (per-day counts in the m1 evidence JSON).

**Canonical anchored replay** (Seek at the PRD §6 head; priors replay-only):

```bash
./target/release/fft --replay /tmp/esu6-wed-v3-ckpt.fftlog \
  --replay-at 2026-07-29T13:50:00Z \
  --prior /tmp/esu6-2026-07-27.fftlog --prior /tmp/esu6-2026-07-28.fftlog
```

**Canonical sim-live** (join open → wall-pin; LIVE append to a distinct path):

```bash
./target/release/fft --sim-live /tmp/esu6-wed-v3-ckpt.fftlog \
  --head 2026-07-29T13:50:00Z \
  --live-out /tmp/esu6-wed-live.fftlog
```

`--head` is wall-clock; the CLI snaps it to the last in-log event ≤ head before
`SetSource` (exact event timestamp required by the engine). Transport strip/keys are
armed at spawn. Headless M1.5 gate:

```bash
cargo run --release -p fft-engine --bin m15-gate -- --help
```

**Prefs** persist at `$XDG_CONFIG_HOME/fft/prefs.toml` (else `~/.config/fft/prefs.toml`):
`mp_scale`/`dom_scale` ∈ {1,2,4}, `splitter_ratio` ∈ [0.1,0.9], `mp_zoom` ∈ [0.5,3.0],
`transport_speed_index` ∈ 0..=7. Out-of-range numerics clamp loudly; unparseable values and
illegal scales fall back to defaults loudly. Missing file → defaults, no warning. Saves are
atomic.

**Theme/font** follow Omarchy live (≤500 ms): colors from
`~/.local/state/omarchy/current/theme/colors.toml`; size from `[font] base-size` (user
`~/.config/omarchy/shell.toml` wins over the themed default 12; UI scales by base/12);
family from `fc-match monospace`, resolved at startup. No Omarchy state → loud warning +
built-in Mocha.

## 4. Gate runs (quiet-box protocol)

No dedicated perf hardware (ruling 2026-08-11). Gate runs are valid only on an
otherwise-idle machine — concurrent builds measurably inject ~33 ms two-vsync spikes
(blank-window control evidence committed). Check `pgrep -c rustc` reads 0 first.

| Gate | Command |
|---|---|
| Frame (M3/M4) | `fft --gate 60 --replay <ckpt> --replay-at 2026-07-29T13:50:00Z --gate-out perf-runner/results/<date>-<gate>.json` |
| M1 data plane | `m1-gate --out <json> --legacy-dir data/sessions --diff-trials 7 <day.fftlog>...` |
| M1.5 sim-live | `m15-gate --replay <ckpt> --head 2026-07-29T13:50:00Z --live-out <path> --gate-secs <n> --out <json>` (`cargo run --release -p fft-engine --bin m15-gate -- --help`) |
| M2 seek | `m2-gate --log <ckpt> --seeks 1000 --verify 25 --out <json>` |
| M4 agreement | `m4-agreement --replay <ckpt> --out <json>` |
| M5 scrub | `m5-scrub-burst --replay <ckpt> --out <json>` |
| M5 RSS | `m5-rss-week --current <fri> --prior <mon>.. --prior <thu> --out <json>` |
| Cold start | `fft --replay <ckpt> --replay-at <ts> --startup-trace` ×5 |

Evidence files are committed by the orchestrator only when a run is accepted;
history is append-only.

## 5. Failure modes you can trigger

| Symptom | Cause | Remedy |
|---|---|---|
| `usage:` + exit 2 | Bad flag/value; `--sim-live` with `--replay`/`--replay-at`; missing `--head`/`--live-out`; `--live-out` == source; `--prior` without `--replay`; prior-discovery flags with `--sim-live`; missing `--prior`/`--manifest` file | Fix the invocation |
| `fft-engine Seek against a log with zero checkpoints: <path>...` | `--replay-at`/scrub on an un-checkpointed log | Run `fft-checkpoint <src> <dst>`, replay the copy |
| Engine panic on `SetSource(SimLive)` | Head not an in-log event ts, empty source, before open, or past EOF | CLI snaps wall-clock heads; if you bypass the snap, pass an exact event ts |
| `fft: cannot open gate result file` (before the window) | Unwritable `--gate-out` | By design: never spend a 60 s run to discover the result can't be recorded |
| `fft: ENGINE THREAD PANICKED ...` + FAIL exit | Engine died (e.g. missing log file mid-open, corrupt log) | Evidence JSON is still written first with the panic in `notes`; read it |
| `LoadPriorSession skipped <path>: ...` | Missing prior file, wrong/duplicate trade date | Loud counted skip; playback continues (replay path only) |
| Window at ~30 fps | Only possible on non-fft GPUI builds — `fft` sets `GPUI_DISABLE_INACTIVE_THROTTLE=1` unconditionally on the pinned fork | Verify you run the workspace binary |

## 6. Environment pins

- **GPUI**: `rlimberger/zed` fork, rev `34ba175b…` (unfocused-throttle opt-out). Never
  repoint to upstream.
- **`.cargo/config.toml`**: `git-fetch-with-cli = true` — this machine's insteadOf
  rewrite breaks cargo's built-in fetcher.
- **Wayland/Hyprland** reference stack; `gpui_platform` builds with the `wayland` feature.
- **Font**: fontconfig-resolved; the reference box has "JetBrainsMono Nerd Font".
- **Data**: `data/GLBX-20260803-4WJS899FNL/` (MBO-only DBN, hash-verified via the ignored
  manifest test), legacy v1 logs in `data/sessions/` (size-comparison reference only).
