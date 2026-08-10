# FFT — Session Handoff (2026-08-10, independently audited)

## Binding

Latest user directive (2026-08-10, René): **Fable 5 orchestrates** (this session); workers
are Codex `gpt-5.6-sol` (Opus-tier tasks) and Grok 4.5 (workhorse tasks), each in its own
session, René relaying briefs/reports. The orchestrator gives path-bounded briefs and
reviews every diff and gate before acceptance. Standing order (René 2026-08-10):
commit + push accepted work without asking — scoped commits per track, review first.
Recenter key `c` confirmed by René.

## Track board (2026-08-10, orchestrator)

Accepted this session (reviewed against diff, gates green; uncommitted pending René):
- **LOG-HARDEN** (Grok, fft-log): footer-bound claim REFUTED with regression tests;
  `mbo` schema-tag validation; LIVE-tail `refresh()` API + torn-tail retry tests;
  fixture self-masking closed. Nits deferred: `was_live()` aliases `is_live()`;
  reader.rs at 532 lines.
- **INGEST-STITCH** (Grok, fft-ingest): shared GapDetector across ordered inputs
  (boundary gaps now emitted); gap bucketing confirmed shared; snapshot evidence
  delivered (drove the FFTLOG-V2 §4 snapshot freeze below).
- **UI-M3-INTERACT** (Codex/Sol, fft-ui): DomView aggregation (1/2/4, floor buckets,
  checked sums), persistent generation-swept GlyphCache, drag/wheel pan with
  fractional coalescing, recenter `c` (provisional key), replay spawn moved to
  `on_next_frame` (no pre-paint I/O). Zero `entity.update` on input/snapshot paths.
  M3 frame/input gates intentionally unclaimed — display run pending.
- **Orchestrator** (fft-engine): CoverageCounters per ENGINE.md §3; ENGINE.md §4
  six-section checkpoint interface FROZEN; audit item 8 fixed (SetSource drops
  batched stale seek, resets latest_seek) + regression test; latent slot-then-wake
  test race fixed.

**BLOCKER found (2026-08-10): full-day fftlogs are unreplayable.** Databento snapshot
records (original ts, non-channel seqs) replay as live Adds → book panics
`seq regression 644 -> 643` immediately; snapshot seqs also feed the gap detector →
~1.9k phantom Gap records per day log. Policy frozen in FFTLOG-V2 §4 "Snapshot
records": admission by first-live-event bucket, gap-detector bypass, snapshot-load
apply semantics. Wed log at scratchpad must be re-ingested after the fix. Do NOT
`fft --replay` a full-day log until INGEST-SNAPSHOT-ADMISSION + BOOK-SNAPSHOT-LOAD
land.

In flight next:
- **INGEST-SNAPSHOT-ADMISSION** (Grok, fft-ingest): implement FFTLOG-V2 §4 admission
  + gap-detector bypass.
- **BOOK-SNAPSHOT-LOAD** (Codex/Sol, fft-book): snapshot-load apply semantics + the
  audit's fft-book wave (six-section split per ENGINE.md §4, fresh-flow eviction fix).
- Audit spot-checks: claims 1, 7, 8 CONFIRMED (7 approved+documented in PRD, 8 fixed);
  claim 6 plausible (fft-profile wave pending); claim 10 REFUTED with tests.

Truth: `PRD.md`, `TECH-STACK.md`, `IMPLEMENTATION-PLAN.md`, and
`docs/{FFTLOG-V2,ENGINE,PERF-RUNNER,FIXTURES}.md`. Never modify `~/Projects/fft-legacy`.

## Repository state

- `main` is still at `59639e2` (`origin/main`), the original PRD-only commit.
- `PRD.md` is modified; essentially the entire implementation, docs, CI, fixtures, and perf
  artifacts are untracked. The M0 requirement that the five freezes be committed before M1
  fan-out is therefore not met.
- No source implementation was changed during this audit; this handoff is the only edit.
- The local Databento job is **MBO-only**. Its `metadata.json` says `schema: "mbo"`; no
  definition or status files are present. `docs/FIXTURES.md` currently claims otherwise.
- `fft-feed` remains a two-line stub. `Source::Live` and `GoLive` fail loudly.

## Cold verification completed

Passed on this tree:

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p fft-ingest --test manifest -- --ignored --nocapture
```

The last command hash-verified all six large MBO files in 37.82 s. The workspace test has one
normally ignored large-data test; all active tests passed. The only clippy output is the
`proc-macro-error2 v2.0.1` future-incompatibility notice.

Not run in this audit: full-week ingest, 20.6 M-event book gate, log-size comparison, 1,000
week-wide seeks, replay-throughput gate, GUI replay/frame gate, or 120/240 Hz validation.

## What exists versus what is accepted

| Area | Proven on this tree | Acceptance status |
|---|---|---|
| M0 | Workspace/frozen docs exist; recorded blank focused window says 3,601 frames, zero misses at 60 Hz | **Not formally accepted:** uncommitted, perf workflow manual-only, result lacks required git SHA/full embedded metadata |
| `fft-core` / `fft-log` | Primitive schema, codec, checksums, mmap reader, synthetic roundtrips/corruption/torn-tail tests | M1 T1 **not accepted**; corrupt-input/concurrent-tail gaps remain and milestone gates were not run |
| `fft-ingest` | MBO decode, in-file gap synthesis, CT bucketing, small golden write | M1 T2 **not accepted**; front-month/definition path is absent and multi-file stitching is wrong |
| `fft-book` | FIFO/modify/query/restore/refresh synthetic suites pass | M1 T3 **not accepted**; real-data/perf gates absent and audited correctness defects remain |
| `fft-profile` | TPO/VA/IB/VPOC/CVD/cB-cA and synthetic restore tests pass | M2 T1 **not accepted**; audited session/gap/time defects remain |
| `fft-engine` / `fft-replay` | Dedicated thread, bounded commands, latest-value snapshots, synthetic checkpoint seek | M2 T2 **not accepted**; no production checkpoints, six-section contract not implemented, no week/perf gate |
| `fft-ui` | Replay-driven DOM prototype as one custom `Element`; one snapshot load per render | M3 **not accepted**; required interactions/cache/event counter/frame gates are absent |
| `fft-feed` | Loud stub only | M1.5 absent; M6 not started |

Green unit tests are not milestone gates. The previous handoff's broad “accepted” labels were
not supported by the implementation plan.

## Blocking discrepancies (source-verified)

1. **Checkpoint topology violates the frozen wire contract.** `docs/FFTLOG-V2.md` requires
   separate BOOK/FLOW/PROFILE/CVD/REFRESH/SESSION sections (IDs 1–6). `Book::serialize`
   currently embeds flow, cB/cA, and refresh inside BOOK (`fft-book/src/serialize.rs`), while
   replay requires only BOOK/PROFILE/CVD (`fft-replay/src/source.rs:270`).
2. **Production logs contain no checkpoints.** All `write_checkpoint` calls outside
   `fft-log` are test helpers. Ingest explicitly skips them. A production seek therefore
   replays from frame zero; `ReplaySource` reports this, but `fft-engine` ignores the report.
3. **Multi-file ingest is not a valid Globex stitch.** Each input creates a fresh gap detector
   (`fft-ingest/src/write.rs:156`), so boundary discontinuities emit no Gap. Snapshot records
   from every input are retained regardless of target trade date (`write.rs:87–101`).
4. **M1 T2 required work is missing.** `instrument_meta` always returns
   `MissingDefinition`; front month is a hard-coded sample-week instrument id. CLI tick fields
   are loud, but they do not satisfy the planned definition-schema/front-month track.
5. **A recenter can erase live flow.** Empty levels remain live while five-second flow is
   fresh, but `SideBook::evict` preserves only levels with resting orders
   (`fft-book/src/side.rs:109–113`).
6. **Profile session semantics are wrong at boundaries.** RTH TPO lettering continues to
   16:00 CT although the PRD closes RTH at 15:00 (`fft-profile/src/tpo.rs:47–105`). A gap that
   enters a new 30-minute period can lose its PV gap marker; backward-period timestamps panic.
7. **Native-refresh policy diverges from the PRD.** The code adds an undocumented 1 ms
   acceptance window (`fft-book/src/lib.rs:44–49`) to the frozen same-id/full-fill signature.
   Do not silently retain or remove this: René must choose PRD-conformant removal or approve a
   sourced PRD/spec change.
8. **Render/live state is incomplete.** Refresh availability after a gap is absent from
   `DomRenderState`; selected-order queue rank is absent. A drained `[Seek, SetSource]` batch
   can execute the stale seek against the new source (`fft-engine/src/service.rs:267–330`).
9. **M3 is a prototype, not its gate.** Replay path validation performs filesystem I/O before
   the first window; pan/zoom/tick-scale controls, persistent glyph-run cache, and event-coverage
   counter are absent. The only perf artifact is the blank M0 window at focused 60 Hz.
10. **`fft-log` still has hardening gaps.** Footer `index_len` can drive an unbounded
    allocation, the frozen `mbo` schema tag is not validated, and the mmap reader has no
    refresh API/test for a concurrently growing LIVE file. Its fixture test regenerates the
    fixtures before checking them, so CI can mask drift unless it checks a clean tree.

## Continue here — ordered, no M1.5 yet

1. **Resolve the process gate.** Ask René before committing. M1+ cannot be formally accepted
   while the M0 freezes and implementation remain outside history.
2. **First repair wave, three non-overlapping owners:**
   - `fft-log` only: bound footer decoding, validate the v2 `mbo` schema tag, and prove
     concurrent LIVE-tail refresh/retry/clean-close behavior without changing wire bytes.
   - `fft-ingest` only: make gap state continuous across ordered inputs and admit only the
     correct initial snapshot for a stitched trade date. Implement definition/front-month
     only from real schema data; do not hard-code or invent metadata. The missing definition
     fixture/data is an explicit blocker to raise with René.
   - `fft-book` only: expose deterministic, separately versioned BOOK/FLOW/REFRESH payloads
     for IDs 1/2/5 and add independent roundtrip-plus-tail tests; preserve fresh empty-level
     flow during recenter. Keep the 1 ms refresh-policy decision out of this worker brief.
3. **Then repair `fft-profile`:** enforce the 15:00 CT RTH close, attribute gaps by their event
   timestamp across period rolls, and define/test backward-timestamp behavior allowed by the
   frozen canonical stream. Keep PROFILE/CVD/SESSION payloads separate.
4. **Orchestrator freezes the production checkpoint integration interface before delegating
   it.** Then one `fft-engine`/`fft-replay` owner writes and restores all six sections at 60 s
   wall-clock cadence, makes missing checkpoints loud for gate inputs, fixes stale seek/source
   switching, and carries refresh availability through `RenderSnapshot`. Do not let a worker
   invent which production path materializes checkpointed historical logs.
5. **Run the actual M1 and M2 gates:** full-week ingest; busiest-session book apply <2 s;
   invariants at every checkpoint; N-chunk ≡ one-shot; log ≤0.5× legacy size; 1,000 random
   order-exact week seeks with p95 ≤250 ms; replay ≥60×. Store perf evidence with the required
   manifest and git SHA.
6. Only after those gates pass, start **M1.5 sim-live** at Wed 2026-07-29 09:50 New York on the
   same append/checkpoint/event path. M3 completion and its replay frame gate follow; the
   current DOM slice is useful but not accepted.

Do not modify the PRD to excuse implementation drift, do not build on the legacy tree, and do
not report a milestone accepted from synthetic unit tests alone.
