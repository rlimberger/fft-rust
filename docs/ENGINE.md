# fft-engine — Command Protocol & Render Snapshot (M0 freezes 2 + 3)

**Status: FROZEN.** Agent tracks implement against this contract; they do not reinterpret
it. Changes are orchestrator-approved spec edits, same commit as the code.

## 1. Ownership map

`fft-engine` runs one **dedicated OS thread** and is the **sole writer** of all mutable
market state:

- L3 book, profile, flow windows, CVD, native-refresh state machine
- Source lifecycle: file-replay cursor (the engine owns its mmap cursor directly) and the
  live canonical-event inlet
- Seek service and seek generations
- Sequence watermarks: `received_seq`, `decoded_seq`, `applied_seq`, `logged_seq`,
  `published_seq`
- Gap state, live-log append, shutdown

Tokio (vendored `gpui_tokio` bridge, never on GPUI threads) owns **networking only**:
Databento connect, intraday-replay join, reconnect, DBN decode. It hands the engine
**bounded batches of canonical events** — batch ≤ 4,096 events, channel capacity 16,
backpressure blocks the network task, nothing is ever dropped. A Tokio callback never
mutates engine state. The UI thread never blocks on I/O or seeks.

## 2. Command protocol

```rust
enum EngineCmd {
    SetSource(Source),          // Replay { path } | Live { config }
    Play,
    Pause,
    SetSpeed(f64),              // replay only; 0.0 < speed, go-live cancels it
    Seek { ts: u64, generation: u64 },
    GoLive,                     // live source only; jump to head
    Shutdown,                   // engine flushes + closes the log, then exits
}
```

- Commands travel UI → engine on a **bounded** channel (capacity 64). Only `EngineCmd`
  crosses; no state ever flows UI → engine.
- **Seek coalescing, latest-wins:** the UI stamps each `Seek` with a monotonically
  increasing `generation`. The engine drains all pending commands before working and
  executes only the highest seek generation; a completed seek whose generation is below
  the latest requested is **discarded before publication**. Scrub-drag therefore costs
  one seek resolution, not one per mouse event.
- Every seek resolves as checkpoint-restore + tail-replay (see `docs/FFTLOG-V2.md` §5),
  runs `check_invariants()` after restore, and is followed by exactly one publication.

## 3. Render snapshot contract

```rust
struct RenderSnapshot {
    generation: u64,       // monotonic, +1 per publication
    applied_seq: u64,      // last source sequence reflected in this state
    applied_ts: u64,       // event time of that sequence, ns UTC
    seek_generation: u64,  // the seek this state answers (0 = live/forward flow)
    dom: DomRenderState,       // ladder window: per-price aggregates, flow counters,
                               // refresh badges, selected-order queue rank
    profile: ProfileRenderState, // per-session TPO/volume arrays, VA/IB/VPOC, CVD
    coverage: CoverageCounters,  // event-coverage accounting (M3 gate surface)
}

struct CoverageCounters {
    events_read: u64,    // events decoded from the source since SetSource
    events_applied: u64, // events applied to book+profile exactly once
    gap_records: u64,    // gap events encountered (downstream state = unavailable)
}
```

`events_read == events_applied` is an invariant (debug-asserted in the engine); the UI
renders `events_read - events_applied` as the dropped-event counter and the M3 gate
requires it to read zero for the whole run. `gap_records` is informational (a gap is loud
data, not a drop). Counters reset on `SetSource`; a seek neither resets nor advances them —
seek resolution is accounted by its own bit-identical gate, the counters cover forward/live
flow only.

Semantics (each is a gate, not a hint):

1. The engine publishes `Arc<RenderSnapshot>` into a **latest-value slot** (atomic swap);
   the accompanying wake signal is **payloadless** and means "newer state exists", never
   "N things happened".
2. The UI loads **exactly one** `Arc` at frame start and renders both panes from it —
   same generation, always. Pane-generation skew is a bug by definition (PRD claim 5
   depends on this).
3. ≤ 1 `entity.update` per frame on the UI side, regardless of publication rate.
4. Snapshot construction **never clones the L3 book**. `DomRenderState` carries
   aggregates for the visible ladder window plus the state the DOM actually draws;
   per-order data enters only for explicitly selected/hovered orders.
5. Budgets (frozen numbers, enforced by criterion benches from M2 on the perf runner):
   construction p99 ≤ **300 µs**; steady-state heap per snapshot ≤ **8 MiB**; publication
   allocates at most one new `RenderSnapshot` (no chained clones).
6. Stale completed seeks are never published (§2). Integrity accounting (every accepted
   event applied exactly once) is tracked by the watermarks in §1 — latest-value
   coalescing of *renders* is intentional and is not event dropping.

## 4. Checkpoint integration (frozen 2026-08-10)

No production checkpoint bytes exist yet, so this freeze costs zero wire compatibility.
The implementation moves to the FFTLOG-V2 §5 six-section layout; the doc does not move
to the code.

Section ownership and API (crate → sections):

- **fft-book** owns 1 BOOK, 2 FLOW, 5 REFRESH:
  ```rust
  Book::serialize_book() -> Vec<u8>     // resting orders + levels + book meta
  Book::serialize_flow() -> Vec<u8>     // 5 s flow window, cB/cA counters
  Book::serialize_refresh() -> Vec<u8>  // native-refresh state machine + tombstones
  Book::restore(book: &[u8], flow: &[u8], refresh: &[u8]) -> Result<Book, RestoreError>
  ```
  All three payloads are REQUIRED for restore. Each payload's first two bytes are its own
  section version (self-identifying, as BOOK is today). `RestoreError` mirrors
  fft-profile's (loud, typed); panicking restore is retired. A future change to the
  pending 1 ms refresh-window decision bumps REFRESH *semantics/version*, never BOOK
  layout — that containment is why the sections are separate.
- **fft-profile** owns 3 PROFILE, 4 CVD, 6 SESSION:
  ```rust
  MultiProfile::serialize() -> ProfileSections { profile: Vec<u8>, cvd: Vec<u8>, session: Vec<u8> }
  MultiProfile::restore(profile: &[u8], cvd: &[u8], session: &[u8]) -> Result<MultiProfile, RestoreError>
  ```
  SESSION carries the session-boundary state currently embedded in PROFILE (trade dates,
  period cursors, gap markers); PROFILE keeps the TPO/VA/IB/VPOC arrays.
- **fft-log** stays the neutral §5 frame/section codec — no changes.
- **fft-replay** composes: restore requires all six sections in ascending id order; a
  missing or version-mismatched section is a loud `ReplayError`, never a partial restore.

Materialization (who writes CHECKPOINT frames):

1. **Live (M6):** the engine's single writer emits a CHECKPOINT frame every 60 s
   wall-clock while appending the live log (FFTLOG-V2 §5 cadence).
2. **Historical:** `fft-checkpoint` (bin target in fft-engine) reads an ingest-produced
   log and writes a checkpointed copy at 60 s **event-time** cadence through the same
   apply-then-serialize path. Ingest never writes checkpoints (it has no book/profile
   state).
3. Gate inputs are checkpointed logs. `EngineCmd::Seek` against a source whose frame
   index holds zero CHECKPOINT frames (`ReplaySource::checkpoint_count()`) panics with
   the fft-checkpoint remediation — replaying from frame zero is a forbidden
   silent-degraded path (doctrine rule 7).

Command-batch semantics (closes the stale-seek/source-switch defect): commands in one
drained batch apply **in order**; a `SetSource` discards any seek selected earlier in the
same batch (it targeted the old source) and resets `latest_seek` to 0; a `Seek` after the
`SetSource` executes against the new source.

Restore equivalence stays the M2 bar: checkpoint-restore + tail-replay bit-identical to
forward replay, compared order-exact (FFTLOG-V2 §5), `check_invariants()` after every
restore.
