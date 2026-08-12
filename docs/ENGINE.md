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
    SetSource(Source),          // Replay { path } | SimLive { path, head_ts, live_out } | Live { config }
    LoadPriorSession { path },  // async profile-only build of an earlier trade date
    Play,
    Pause,
    SetSpeed(f64),              // replay / sim-live transport; 0.0 < speed, go-live cancels it
    Seek { ts: u64, generation: u64 },
    GoLive,                     // SimLive / Live only; jump to wall-pinned head
    Shutdown,                   // engine flushes + closes the log, then exits
}
```

**Prior sessions (2026-08-11, René: "all prior sessions we have, async"):**
`LoadPriorSession` builds the PROFILE-side state of an earlier trade date on the engine
thread in **time-budgeted slices interleaved with forward work** — playback and input
latency are never blocked (budgets in time, doctrine rule 4). Rules:

1. Profile-only: prior days build no book, no flow, no refresh state. Fast path: when the
   log carries CHECKPOINT frames, restore PROFILE/CVD/SESSION from the **last** checkpoint
   and tail-apply; otherwise stream-apply trades under the slice budget.
2. The command carries one path; the UI issues one command per prior day,
   **oldest-first** (M4 spec). The engine inserts each completed session into
   `ProfileRenderState.sessions` keeping ascending trade-date order; **the current
   (replay/live) session is always `sessions.last()`**. `display_session`-style consumers
   must select by recency, never `.first()`.
3. A prior session becomes visible only when **complete** (no partial-day publications);
   completion triggers one normal publication. Errors (missing file, wrong instrument,
   trade date ≥ the current session's) are loud stderr + a counted skip, never a panic of
   the forward path.
4. `SetSource` drops any in-progress prior build but **keeps** completed prior sessions
   when the new source's trade date is unchanged; otherwise it clears them.
   Completed priors are likewise **engine-owned across Seek** (2026-08-11, defect found
   by the m7-soak rig): a seek's checkpoint restore rebuilds only the current session;
   the engine drains its completed priors before `ReplaySource::seek` (which replaces
   the profile before its first cancellation poll) and re-inserts them after it returns
   — on every exit path, including cancelled and superseded seeks. An in-progress prior
   build is source-independent and unaffected by seeks; its completion re-validates
   against the then-current profile.
5. Snapshot budgets (§3.5) are unchanged and now include prior sessions: the 8 MiB heap
   assert is the guard — a full week of ES sessions measures well under it.

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
    symbol: Arc<str>,      // contract from the source's header meta (2026-08-11);
                           // captured once at SetSource, refcount-cloned per publish,
                           // empty before any source exists
    dom: DomRenderState,       // ladder window: per-price aggregates, flow counters,
                               // refresh badges, selected-order queue rank
    profile: ProfileRenderState, // per-session TPO/volume arrays, VA/IB/VPOC, CVD
    coverage: CoverageCounters,  // event-coverage accounting (M3 gate surface)
}

struct CoverageCounters {
    events_read: u64,    // events decoded from the source since SetSource
    events_applied: u64, // events applied to book+profile exactly once
    gap_records: u64,    // gap events encountered (downstream state = unavailable)
    head_lag_ns: i64,    // sim-live wall-pin lag (coverage note); 0 for replay / steady state
}
```

`events_read == events_applied` is an invariant (debug-asserted in the engine); the UI
renders `events_read - events_applied` as the dropped-event counter and the M3 gate
requires it to read zero for the whole run. `gap_records` is informational (a gap is loud
data, not a drop). `head_lag_ns` is the §5 wall-pin observability surface:
`(applied_ts - head_ts) - (now - wall_at_head)` while pinned or catching to the wall,
computed in signed saturating wide arithmetic (backward timestamps legal, never wraps);
zero before pin, while scrubbed back, and on plain replay. Counters reset on `SetSource`; a seek neither resets nor advances them —
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

**Post-seek intent (strict, batch-local — 2026-08-12, same-commit with the
implementation).** Each drained batch tracks the coalesced `Seek` (highest generation)
plus one `after_seek ∈ {Paused, Play, GoLive}`:

- Every `Seek` selects/replaces the coalesced target, forces pause, and **resets**
  `after_seek = Paused` — a later `Seek` clears any earlier `Play`/`GoLive` intent in
  the same batch.
- `Play` / `Pause` / `GoLive` arriving after a selected `Seek` set only `after_seek`;
  they run when that seek completes, and only if its generation is still `latest_seek`.
- Consequences, each protocol-tested: `[Seek, Play]` resumes from the anchor;
  `[Seek, Pause]` stays paused; `[Seek, Play, Seek]` ends paused at the final seek;
  `[Seek, GoLive]` ends live; `[GoLive, Seek]` ends scrubbed.
- `SetSource` discards the selected seek **and** clears `after_seek` (resume intent
  dies with the old source).

Restore equivalence stays the M2 bar: checkpoint-restore + tail-replay bit-identical to
forward replay, compared order-exact (FFTLOG-V2 §5), `check_invariants()` after every
restore.

## 5. Sim-live source (M1.5 freeze, 2026-08-11)

The recorded week stands in for Databento live (PRD §6): identical engine path, so M6
swaps the inlet, never the path. Frozen interface:

```rust
Source::SimLive {
    path: PathBuf,
    head_ts: u64,          // ns-UTC, an EXACT in-log event timestamp
    live_out: PathBuf,     // LIVE-flagged append destination (§5.4 / gate --live-out)
}
```

`live_out` is additive to the original freeze listing (same-commit): required by the
live-append contract and the M1.5 gate's `--live-out`; without it the engine has no
single-writer destination for clause 4.

`head_ts` must be an **exact event timestamp present in the log** — not merely within
its range. `SetSource(SimLive)` validates the whole source for that exact timestamp
**before** creating `live_out`; an invalid head (empty source, before open, past EOF,
or between events) is a loud panic that never truncates an existing `live_out`.
(The `m15-gate` CLI accepts a wall-clock `--head` and snaps it to the last event
at-or-before that instant before issuing `SetSource` — a harness convenience; the
engine contract stays exact.)

Semantics (each clause is gate-tested, not advisory):

1. **Join = catch-up.** `SetSource(SimLive)` opens the log and applies from session open
   to `head_ts` unpaced in **time-budgeted slices** (same doctrine as prior builds —
   forward publication cadence and command latency are never blocked; budgets in time).
   Catch-up progress publishes normally: the UI shows the book racing to the head, which
   is exactly the M6 intraday-replay-join UX. `--replay-at`'s Seek-based anchor is NOT
   the join path — a sim-live join replays through the open, it never checkpoint-skips.
2. **Wall pin at head.** When `applied_ts` reaches `head_ts`, the engine records
   `wall_at_head = Instant::now()` and thereafter paces so that
   `applied_ts - head_ts` tracks `now - wall_at_head` **absolutely** — the pin is to the
   origin, never relative re-anchoring, so a multi-hour session cannot drift. Falling
   behind (slice exhaustion) is caught up on the next slice; the lag is observable as
   `head_lag_ns` in the snapshot's coverage notes (0 in steady state).
3. **Speed/GoLive.** `SetSpeed` is legal while paused-behind or scrubbed-back
   (transport still works over the already-streamed range); `GoLive` jumps to the
   current wall-pinned head (unpaced catch-up of the interim), cancels any `SetSpeed`
   to 1×, and resumes wall-pinned streaming. `GoLive` on a plain `Replay` source stays
   a panic.
4. **Live-log append.** The engine (single writer, §1) appends every applied canonical
   event to a LIVE-flagged fftlog v2 (`FFTLOG-V2` §7 commit protocol) and emits a
   CHECKPOINT frame every 60 s **wall-clock** (§4.1). The append frontier is an
   **event ordinal**, not a timestamp: each consumed source event advances a cursor
   ordinal, and a successful append records it as the tip ordinal — so a slice that
   stops mid same-timestamp burst cannot double-append or skip the burst's tail.
   `Seek`/`GoLive` seal the tip; scrubbed replay behind the tip never appends, and a
   GoLive re-catch suppresses appends until the sealed tip ordinal is crossed.
   `logged_seq` advances only on a committed append and means the **last non-snapshot
   channel seq in committed wire order** (a committed Gap re-anchors it exactly once,
   mirroring the applied-side re-anchor); it is no longer an alias of `applied_seq`
   on this source. `Shutdown` closes the log cleanly (LIVE flag cleared).
5. **Watermarks.** `received_seq`/`decoded_seq` advance at the sim-live inlet as events
   are drawn from the cursor, before apply — the five stages become real, in
   preparation for M6's network inlet.
6. **Gap injection is harness-side.** The M1.5 gate bin wraps the cursor and injects
   synthetic Gap records mid-stream; the engine treats them as wire truth (loud
   `gap_records`, book/refresh → unavailable). No injection hooks live in the engine.

**Gate bin** (`m15-gate`, bin target in fft-engine — headless): inventories the source
and validates the head, joins at session open (exact join-prefix event count, zero
seeks), streams wall-pinned (measured drift bound over the gate window: |head_lag_ns|
p99 ≤ one slice budget, sampled on distinct publications), exercises scrub +
`SetSpeed` + `GoLive` back to the wall head, injects a gap via a harness-built
resequenced fixture and asserts loud records + unavailable classification + post-gap
watermarks + six-section identity of the gap-bearing live log, verifies the LIVE-flag
lifecycle on `live_out` (LIVE while streaming; footer-indexed, flag cleared, warning-free
after `Shutdown`), then **re-replays the appended live log bit-identically** through the
standard replay path (order-exact section compare at EOF). Every failure path still
writes the evidence JSON with `verdict: FAIL` and the failed dimension — a panic never
destroys evidence.
