# FFT — Tech Stack

Every choice here was made after reading Zed's source at HEAD (2026-08-10), a full autopsy of
the legacy attempt, and current Databento/CME docs. Facts below are verified, not vibes.

## 1. Core

| Layer | Choice | Why |
|---|---|---|
| Language | **Rust, edition 2024**, pinned stable toolchain | Fearless concurrency for a wire-speed book + zero-GC frame loop. Both prior attempts died on process, not language. |
| UI platform | **GPUI**, pinned git rev of `zed-industries/zed` | crates.io `gpui` is frozen at 0.2.2 (Oct 2025) — main is the real project. Linux backend renders through **wgpu/Vulkan** with true Wayland `wl_surface::frame` vsync. World-class glyph atlas + shaping — the hardest 40 % of a custom renderer, already solved and tuned on exactly our workload (dense monospace grids at 120 fps). |
| Rendering doctrine | **One custom `Element` per pane** | The MP pane and the DOM pane are each a single element that computes its own grid and paints quads + cached glyph runs directly. **The legacy div-per-cell tree (≈1,600 elements/frame) is forbidden.** This is custom-wgpu performance with GPUI's platform for free. |
| Data | **`databento` + `dbn` crates** (pinned) | Official Rust client; zero-copy DBN decode; live gateway with intraday replay-join. |
| Async | GPUI executors + **vendored `gpui_tokio` bridge** (~100 lines, Apache-2.0) | The databento client needs tokio. Tokio stays off GPUI threads — ever. |
| Time | **`jiff`** | Real tz database. Legacy hand-rolled NY DST math — never again. All trade-date bucketing in `America/Chicago`. |
| Storage | **`memmap2`** + `zstd` | Legacy read the log 32 B per syscall: 2.0 M rec/s vs 264 M rec/s buffered — a measured **130×** loss. mmap gives one cursor and kills per-record read syscalls; zstd still writes decompressed output, so mmap vs. large buffered reads is benchmarked in M1 and decided on evidence. |

**Plan B seam:** only the two pane elements and the frame pump touch GPUI. If GPUI ever blocks
us, they re-target raw `winit` + `wgpu` without touching engine crates.

## 2. Thread topology

```
┌────────────────────────┐   events    ┌──────────────────────────────┐
│ feed thread (dedicated │──applied──▶│  Engine state (single writer) │
│ OS thread, blocking OK)│             │  L3 book · profile · flow    │
│ live: databento client │             └──────────┬───────────────────┘
│ replay: fftlog cursor  │                        │ snapshot publish
└────────────────────────┘             ┌──────────▼───────────────────┐
                                       │ watch channel (latest-value) │
                                       └──────────┬───────────────────┘
        wl_surface::frame vsync        ┌──────────▼───────────────────┐
        ─────────────────────────────▶ │ UI thread: ONE batch loop    │
                                       │ ≤1 entity.update per frame   │
                                       │ panes snapshot at prepaint   │
                                       └──────────────────────────────┘
```

The engine box is a crate, **`fft-engine`** — sole owner of the single-writer state, the
engine command protocol (play/pause/speed/seek/source-switch/shutdown), seek generations,
source-sequence and gap accounting, and snapshot publication. Tokio owns networking only
(connect, intraday-replay join, reconnect, DBN decode) and hands the engine bounded batches
of canonical events; a Tokio callback never mutates engine state. For file replay the engine
owns its mmap cursor directly.

**Render snapshot contract (frozen in M0, before any M1 fan-out):** the engine publishes
`Arc<RenderSnapshot>` — `generation`, `applied_seq`, `applied_ts`, `seek_generation`, DOM +
profile render state — into a latest-value slot. The UI loads exactly one `Arc` at frame
start; both panes render the same generation; completed-but-stale seeks are discarded before
publication; snapshot construction never clones the L3 book and carries an explicit memory
and construction-time budget.

Non-negotiable rules (each one is a documented production failure in GPUI apps):

1. **Feed on a dedicated OS thread** (`spawn_dedicated` — blocking reads are sanctioned there).
   Never on the GPUI executor pool.
2. **Signals are payloadless; state moves via latest-value snapshot.** Unbounded channels only
   for empty wakeups; payload-bearing channels are bounded.
3. **Budget `entity.update` calls, not `notify` calls.** Every update runs a full effect flush;
   notify is coalesced but update is not. One update per frame, max, regardless of input rate.
4. **Never update an entity from inside an update** (`BorrowMutError` is GPUI's #1 crash class).
   Anything "after this update" goes through `cx.defer`.
5. **Frame budgets in time, never event counts.** Legacy's 80 k-events-per-frame warmup was
   40 ms — 25 fps by design. Loop on a deadline.
6. **The UI thread never blocks on I/O or seeks.** Scrub targets are coalesced
   (latest-wins) and resolved on the engine thread; the UI renders last-good until the new
   frame arrives.
7. **Fail loudly.** No silent fallback paths (legacy silently degraded to a 16 s full-file
   scan on index corruption). Corrupt data = visible error.

**Known platform constraint (verified 2026-08-10, M0 gate investigation):** GPUI at the
pinned rev throttles animation-driven redraw of windows without keyboard focus to ~30 fps
(`min_frame_interval` in `gpui/src/window.rs:1626-1663`; throttled cycles commit without
drawing). Acceptable for the M0 gate (run focused); **not acceptable for the product** — a
DOM must hold full rate while the trader types elsewhere. Resolution owed by the M3 gate:
patch the pinned rev (we control it) or an upstream opt-out; measured, not assumed.

## 3. Engine (kept from legacy autopsy — the parts that were right)

- **Book:** sliding 512-tick dense window + BTreeMap far-map per side, slab-allocated orders
  with intrusive FIFO level lists, exact CME modify semantics (size-down keeps queue position;
  size-up/price-change goes to back). Verified over 82 M events, zero invariant failures.
  `check_invariants()` runs after every seek.
- **Profile:** dense per-price arrays; dual ETH/RTH TPO lettering (RTH restarts at A);
  volume VA (70 %), IB from first two RTH periods, VPOC, CVD candles, cB/cA counters.
- **Book restore is a first-class operation** — never replayed through the event path
  (legacy's replay-as-Adds poisoned flow counters and forced a 6 s pre-roll hack).

## 4. `fftlog` v2 (replay log)

Legacy format lessons, applied: self-identifying header (symbol, instrument id, tick size,
display factor, session boundaries — a log is readable alone); zstd-compressed event frames
with a frame index (legacy was uncompressed and 2.5× larger than its zstd source); delta-coded
timestamps; snapshots every 60 s wall-clock (cadence proven: median 5.7 k events to replay)
carrying **book + flow window + profile + CVD checkpoints** so any seek is restore-plus-tail,
never a rescan (legacy checkpointed only the book — scrubbing rescanned millions of events on
the UI thread); checksummed everything; single-cursor mmap reader.

The full wire specification is **frozen in M0**: magic/version + compatibility policy,
endianness, integer widths and units, timestamp basis and delta-reset rules, canonical event
schema, source-sequence and gap fields, checksum algorithm and coverage, frame-index and
checkpoint-section formats (versioned; queue state serialized strictly head-to-tail per
level — never by `HashMap` iteration), append-commit protocol, concurrent-reader behavior,
truncated-tail recovery, and decoder allocation ceilings for corrupt input. M6 writes live
files continuously, so crash recovery shapes the framing — it is built in M1 (see plan),
never bolted on.

The **ingest tool is part of v1** (DBN → fftlog v2; front-month resolution, Globex stitching,
CT trade-date bucketing). The legacy one only ever existed in the dead C++ tree.

## 5. Live data plane

- Subscribe GLBX.MDP3, schema `mbo`, `parent` symbology (`ES.FUT`) → instrument id mapping.
- Join: intraday replay from session open → stream to head → go live; identical event path as
  file replay, so live and historical are one code path writing the same log.
- Tick size/point value from the `definition` schema (`min_price_increment`,
  `unit_of_measure_qty`; **not** `contract_multiplier` or `min_price_increment_amount` — both
  are documented traps). Session state from the `status` schema.
- Disconnect/gap handling: resubscribe with replay from last applied sequence; the log records
  the gap loudly.

## 6. Portability (asked and answered)

Our code never touches a graphics API — panes emit GPUI primitives, and GPUI picks the backend:
**Metal on macOS** (CVDisplayLink pacing; ProMotion 120 Hz works), **DirectX on Windows**
(DwmFlush pacing), **wgpu/Vulkan on Linux/Wayland**. Zed ships on all three. "Linux-first"
means *tested and perf-gated only on Linux in v1*, not Linux-only code. Known items for the
port milestone: a Windows paint-starvation issue under input floods (fixed upstream —
re-verify), coarser Windows vsync granularity, X11 fallback uses a timer at the RandR rate
rather than true vsync.

## 7. Verification (how we keep the three killers dead)

| Killer | Defense |
|---|---|
| Performance drift | Frame-time harness + criterion benches **with hard, merge-blocking gates on a dedicated perf runner** (provisioned in M0: pinned CPU/governor, core affinity, GPU/driver, kernel, compositor, display mode, build profile, thermal precondition; machine metadata stored with every result; statistical regression thresholds). Ordinary shared CI gates correctness only — absolute GPU timing on noisy runners becomes flaky and gets disabled. Per-frame event-coverage counter proves zero drops. |
| Silent corruption | Differential tests (chunked replay ≡ one-shot replay — the legacy off-by-one dropped one event per frame and no test caught it); seek ≡ forward-replay bit-identity; book invariants on every seek. |
| Agent-generated tangle | Small crates with one owner each (`fft-log`, `fft-book`, `fft-profile`, `fft-engine`, `fft-replay`, `fft-ingest`, `fft-feed`, `fft-ui`); no file over ~500 lines; every public item documented; orchestrator reviews every diff. |
