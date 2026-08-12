# FFT

A Market Profile + order-flow workstation for CME futures, in Rust on GPUI. WindoTrader
(Dalton) and Jigsaw Daytradr (Grady) had a baby — it looks like its parents, only
prettier, and it is **always faster than the monitor**. A missed frame deadline is a bug,
not a tuning issue.

One nanosecond CME Market-by-Order stream drives both panes — the profile (where value
is) and the DOM (what is happening at the touch) can never disagree; the pane-agreement
assertion is a merge gate, not a hope. v1 scope: historical replay + sim-live of recorded
data. Linux/Wayland first. No order entry.

## What makes it different

- **Every event applied exactly once, every frame fresh.** No coalescing-then-guessing:
  the per-frame event-coverage counter must read zero dropped, and does (gate evidence in
  `perf-runner/results/`).
- **Seek anywhere, bit-identical.** Any nanosecond of a session restores via checkpoint +
  tail-replay and is byte-equal to forward replay from open — order IDs, FIFO ranks,
  iceberg state, all of it. Measured: cold p95 6.9 ms against a 250 ms budget.
- **Exact queue standing.** Contracts and orders ahead for any resting order — CME
  FIFO/modify semantics, not an estimate.
- **Deterministic icebergs.** Native-refresh classification on the CME same-`order_id`
  signature within a 1 ms event-time window; across a sequence gap it reads
  *unavailable*, never false.

## Build & run

Rust stable, Wayland compositor (Hyprland is the reference). GPUI is pinned to a fork rev
(see `Cargo.toml`) — never repoint it to upstream; `.cargo/config.toml` forces git-CLI
fetching for it.

```bash
cargo build --release -p fft-ui

# Ingest a trade date from Databento DBN, checkpoint it, replay it:
cargo run --release -p fft-ingest -- write /tmp/esu6-wed.fftlog \
  data/GLBX-20260803-4WJS899FNL/*.mbo.dbn.zst --trade-date 2026-07-29 \
  --tick 250000000 --uom-qty 50000000000 --display-factor 1
cargo run --release -p fft-engine --bin fft-checkpoint -- \
  /tmp/esu6-wed.fftlog /tmp/esu6-wed-ckpt.fftlog

# Replay (seek anchor; priors are replay-only):
./target/release/fft --replay /tmp/esu6-wed-ckpt.fftlog \
  --replay-at 2026-07-29T13:50:00Z \
  --prior /tmp/esu6-mon.fftlog --prior /tmp/esu6-tue.fftlog

# Sim-live (join open → wall-pin at head; exclusive with --replay / --replay-at):
./target/release/fft --sim-live /tmp/esu6-wed-ckpt.fftlog \
  --head 2026-07-29T13:50:00Z \
  --live-out /tmp/esu6-wed-live.fftlog
```

`--replay-at` anchors playback at any UTC instant (seeks need the checkpointed copy — a
checkpoint-less seek fails loudly by design). `--prior` (repeatable, oldest first) loads
earlier sessions asynchronously into the profile without ever blocking playback — replay
only.

`--sim-live` joins at session open and wall-pins at `--head` (requires `--live-out`,
exclusive with `--replay` / `--replay-at`). Wall-clock `--head` snaps to the last in-log
event ≤ head (engine needs an exact event ts). `--live-out` is the LIVE-flagged append
destination and must differ from the source. Transport is armed at spawn; `l` = GoLive
(needs sim-live; else loud hint). Headless gate: `cargo run --release -p fft-engine --bin m15-gate -- --help`.

Theme and font follow the OS (Omarchy) live: colors from the active theme, size from
`[font] base-size`, family from fontconfig. No Omarchy → built-in Catppuccin Mocha with a
loud warning.

## Keys

MP is full-width by default; DOM is hidden every launch (`d` toggles it, MP nav preserved).
`1/2/4` tick scale of the pane under the cursor · `t` sync scales · `c` price-only recenter
· `r` arms transport (already on for `--sim-live`) · with transport on: `space` play/pause
· `[`/`]` speed (0.25×–64×) · `←/→` step ±1 s · `l` = GoLive · MP left-drag:
vertical price / horizontal strips · MP plain or Ctrl+wheel zoom (never pan) · DOM
drag/wheel pan when shown · hover a DOM row for orders/size/hidden/reload per side. `e`
unbound.

See `docs/OPERATOR.md` for the full runbook, `PRD.md` for the product contract, and
`docs/` for the frozen wire/engine specifications. Performance claims are numbers with
committed evidence, never adjectives: `perf-runner/results/`.
