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

./target/release/fft --replay /tmp/esu6-wed-ckpt.fftlog \
  --replay-at 2026-07-29T13:50:00Z \
  --prior /tmp/esu6-mon.fftlog --prior /tmp/esu6-tue.fftlog
```

`--replay-at` anchors playback at any UTC instant (seeks need the checkpointed copy — a
checkpoint-less seek fails loudly by design). `--prior` (repeatable, oldest first) loads
earlier sessions asynchronously into the profile without ever blocking playback.

Theme and font follow the OS (Omarchy) live: colors from the active theme, size from
`[font] base-size`, family from fontconfig. No Omarchy → built-in Catppuccin Mocha with a
loud warning.

## Keys

`1/2/4` tick scale (pane under cursor) · `t` sync scales · `c` recenter · `r` transport
strip · `space` play/pause · `[`/`]` speed (0.25×–64×) · `←/→` step ±1 s · drag/wheel pan
· Ctrl+wheel MP zoom · hover a DOM row for queue/iceberg detail.

See `docs/OPERATOR.md` for the full runbook, `PRD.md` for the product contract, and
`docs/` for the frozen wire/engine specifications. Performance claims are numbers with
committed evidence, never adjectives: `perf-runner/results/`.
