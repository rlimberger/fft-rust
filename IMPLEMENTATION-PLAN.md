# FFT — Implementation Plan

Seven milestones plus one headless live spike (M1.5). Each has a **hard exit gate** —
measurable, automated where possible — and a frozen scope. The three killers of the prior
attempts (performance drift, scope creep, agent-generated tangle) each have a standing
defense; see TECH-STACK §7. The sample week of ESU6 MBO data (82 M events, in `data/`)
is the test fixture for everything through M5.

**Rules of engagement**

- A milestone is done when its gate passes in CI, not when the code "works on my machine."
- Perf gates are merge-blocking from M1 onward. **Amended (René 2026-08-11, PERF-RUNNER.md):
  there is no dedicated perf box — ever, for now.** Timing evidence is valid only on the
  desk machine under the quiet-box protocol (idle host, no concurrent builds); `perf.yml`
  is manual-dispatch; the orchestrator alone accepts evidence JSON into
  `perf-runner/results/` (append-only). Ordinary shared CI gates correctness only. A red
  frame budget is a red build.
- Scope changes are PRD edits first (same commit), implementation second.
- Work inside a milestone is split into parallel tracks with crate-level ownership so
  subagents never collide; the orchestrator reviews every diff against the doctrine rules.

---

## Status board — 2026-08-12 (full-project review; evidence = `perf-runner/results/`)

| Milestone | Verdict | Evidence / note |
|---|---|---|
| M0 foundation | **DONE** | Freezes committed; CI green; blank-window 60 s zero-miss (`2026-08-10-m0-frame-gate.json`) |
| M1 data plane | **DONE** | `2026-08-11-m1-data-plane.json` — busiest-day apply 2.859 s vs ≤ 3 s (revised budget, René 2026-08-11); size 0.17–0.20× legacy; 7/7 chunk differential |
| M1.5 sim-live | **DONE** (sim half) | `2026-08-12-m15-simlive-gate.json` PASS — join 5.96 M events/5.13 s zero seeks, lag p99 1.049 ms vs 4 ms, gap + LIVE lifecycle + six-section identity. **Databento headless spike still owed the day credentials land — hard prereq for M6** |
| M2 seek/derived | **DONE** | `2026-08-11-m2-seek-gate.json` — cold p95 6.9 ms vs 250 ms, identity 25/25; extended to 100/100 (`2026-08-11-m2-bit-identity-100.json`); 25,230× realtime vs ≥ 60× |
| M3 DOM + shell | **DONE** (60 Hz letter) | `2026-08-10-m3-frame-gate.json` — 3600/3600 frames, 0 missed, coverage 0 drops. 240 Hz validation deferred with the perf box (release-claim gate, not merge gate) |
| M4 MP pane | **DONE** | Two-pane anchored gate PASS (`2026-08-10-m4-two-pane-gate-anchored.json`); pane agreement 1089/1089 (`2026-08-11-m4-agreement.json`) |
| M5 time travel | **NEARLY DONE** | Scrub burst 120/120 + RSS week 201 MiB committed PASS. **Owed:** cold-start numbers exist only as HANDOFF prose (85–122 ms) — committed JSON comes from the post-soak chain; hands-on GUI scrub-drag session on the desk display |
| M6 live | **NOT STARTED** | Blocked on the funded Databento key + the deferred M1.5 spike. Do not start otherwise |
| M7 hardening | **IN FLIGHT** | 24 h soak running since Aug 11 21:12 CEST (50 cycles, 0 failures, RSS peak 328 MB at review time); LOG-FUZZ 29,571 mutants clean; README + OPERATOR.md done. Owed: soak verdict → `after-soak-gates.sh` chain → acceptance writeup → v1 tag |

**Five PRD §4 acceptance claims:** 3 (queue, synthetic oracle gate), 4 (iceberg, full-Wed
census), 5 (pane agreement) — evidenced. 1 (seek) — identity + service-side p95 evidenced;
the letter reads *scrub-release → rendered* and that GUI-side path is unmeasured. 2 (frames)
— 60 s anchored PASS; the full-RTH 6.5 h run is owed by the post-soak chain.

### Findings from this review (things done incorrectly)

1. **fmt drift committed to `main`:** Wave-11 commits `24ae684`/`6e5f058` landed with four
   rustfmt diffs — `cargo fmt --check` failed at HEAD, so the CI fmt lane on `main` was
   red. Violates the "green at each commit" standing rule. Fixed in this review's commit;
   the lesson stands: run the full pre-commit trio even on "trivial" follow-up commits.
2. **Soak binary rebuilt underneath the running soak — again.** `/proc/<pid>/exe` of the
   live 24 h soak reads "(deleted)": builds ran on the box after launch (the exact
   Wave-8 antipattern; harmless to the already-running process, but it means the box was
   NOT quiet during the soak window — fine for this correctness/RSS soak, fatal for any
   timing gate). The post-soak RTH gate must run with zero concurrent builds.
3. **Committed gate evidence is almost all `git_dirty: true`** (gates ran pre-commit).
   Values stand, provenance is weak. `m15-gate` now hard-fails provenance (`51c4199`);
   a clean-tree m15 rerun is cheap and should replace the dirty JSON.
4. **Commit `83e3606`'s message overclaims:** it says cold-start evidence landed; no
   cold-start JSON exists in `perf-runner/results/`. The post-soak chain writes it.
5. All Wave-10 residuals are otherwise **closed** (verified in source): 60 s wall-clock
   live-checkpoint cadence unit-tested with injectable `Instant` (`live_log.rs`),
   provenance hard-fail, `LoadPriorSession` forbidden under SimLive (ENGINE.md §2 ruled +
   engine panic), SimLive UI wiring (`--sim-live`/`--head`/`--live-out`, `l` = GoLive,
   LIVE header chrome). The three ASSERT-HUNT findings (locked book, post-gap desync,
   zero-size trades) are all fixed and regression-tested.

### Continue here — next orchestrator, ordered

Topology and roster are binding, in CLAUDE.md/AGENTS.md: Claude Fable 5 orchestrates
in-session; subagent priority Sol → Cursor Grok → xai Grok, pinned, target 12 parallel;
workers never run git mutation in the shared tree; commit + push accepted work without
asking. Fixtures: `/tmp/esu6-{mon..fri}-v3[-ckpt].fftlog` (volatile — regen recipe in
HANDOFF.md Wave-4 board); gate replays use the `-ckpt` copies.

1. **Watch the 24 h soak** (`/tmp/m7-soak-24h.jsonl`, PID in `pgrep -af m7-soak`; ETA
   ~21:12 CEST Aug 12). On PASS, run `perf-runner/after-soak-gates.sh <pid>` on a
   **quiet box** — it validates the soak JSONL, then runs the 6.5 h full-RTH frame gate
   (claim 2) and the cold-start ×5 gate, writing both JSONs. Commit the evidence.
2. **Close claim 1's letter:** either measure scrub-release → rendered p95 in the GUI
   (instrument the existing `--startup-trace`-style path) or put the
   headless-service + hands-on argument to René for a ruling. Don't quietly re-scope.
3. **Hands-on desk sessions (M5/M3 residue):** GUI scrub-drag at the display refresh, and
   a `--sim-live` GUI session (join → catch-up → LIVE → scrub → `l` GoLive). The
   headless gates prove the services; the GUI frame behavior under drag is a human run.
4. **Clean-provenance m15 rerun** on the committed HEAD; replace the dirty-tree JSON.
5. **Acceptance writeup** (orchestrator-authored, never pasted from a worker): five
   claims + boring gates, each with its evidence file. Then **tag v1**.
6. **M6 stays parked** until René funds the Databento key; first action then is the
   deferred M1.5 headless spike (entitlement, symbology, real replay-join, forced
   reconnect, persistent recording), before any `fft-feed` production work.

---

## M0 — Foundation & frozen contracts *(small, do not gold-plate)*

Workspace of thin crates (`fft-core`, `fft-log`, `fft-book`, `fft-profile`, `fft-engine`,
`fft-replay`, `fft-ingest`, `fft-feed`, `fft-ui`), CI (test + clippy + fmt + bench harness),
pinned GPUI rev building a blank window on Wayland, frame-time measurement harness (histogram +
missed-deadline counter), criterion bench scaffold.

**Frozen before any M1 fan-out** — agent tracks never invent interfaces independently:

1. `fftlog` v2 wire-format specification — **`docs/FFTLOG-V2.md`** (scope per TECH-STACK
   §4, including deterministic FIFO checkpoint serialization, append-commit protocol,
   tail recovery).
2. Engine command protocol and `fft-engine` ownership map — **`docs/ENGINE.md`** §1–2.
3. `RenderSnapshot` publication contract — **`docs/ENGINE.md`** §3.
4. Dedicated perf runner — **`docs/PERF-RUNNER.md`**: pinned hardware/software stack,
   machine metadata with every result, statistical regression thresholds; box provisioned
   during M0.
5. Fixture policy — **`docs/FIXTURES.md`**: large market data out of ordinary Git (root
   `.gitignore`); acquisition documented and hash-verified; tests run from any cwd with
   no undocumented environment variables.

**Gate:** CI green; the five freezes committed; blank GPUI window sustains display refresh
with zero missed frames over 60 s on the perf runner while the harness records it.

## M1 — Data plane: format, ingest, book  *(3 parallel tracks)*

- **T1 `fft-log`:** fftlog v2 codec to the frozen M0 spec — self-identifying header, zstd event
  frames, frame index, delta timestamps, checksums, mmap single-cursor reader, append-safe
  commit protocol, **truncated-tail recovery** (moved here from M7: M6 writes live files
  continuously, so crash recovery determines the framing and cannot be bolted on).
  Property tests: encode ∘ decode = id; torn-tail fixtures recover to last commit, loudly.
- **T2 `fft-ingest`:** DBN → fftlog v2. Front-month resolution, Globex session stitching
  (17:00 CT), CT trade-date bucketing, definition-schema tick metadata into the header.
- **T3 `fft-book`:** port the keeper (sliding-window L3 book, slab, CME modify semantics,
  invariants, 5 s flow window) with first-class `restore()`; source-sequence/gap accounting;
  **native-refresh (iceberg) state machine** on the CME same-`order_id` signature — per-order
  reload count + cumulative hidden volume; any sequence gap ⇒ classification **unavailable**,
  never false.

**Gate:** the full ESU6 week ingests; forward replay of the busiest session (21.4 M events —
the "20.6 M" was a v1-era count) applies through book+profile in **≤ 3 s** (revised from
< 2 s — decision: René, 2026-08-11, option 3: the original number predates per-event
refresh/flow/profile state; measured attribution shows decode+book alone at ~2.6 s, evidence
in perf-runner/results/2026-08-11-m1-data-plane.json and the M1-APPLY-PROFILE report; a
book-structure optimization track (hasher/pre-size/FIFO hot path, under M2 bit-identity) is
queued as non-blocking polish); `check_invariants()` clean at every snapshot boundary;
**differential test: N-chunk replay ≡ one-shot replay, event-for-event** (the legacy bug that
must never return); truncated-tail recovery test green; native-refresh fixture suite green
(single refresh · multiple reloads · partial fill + modify · full fill then unrelated order ·
synthetic-iceberg negative · cancel at depletion · gap around a candidate refresh);
log ≤ 0.5× legacy v1 size.

## M1.5 — Sim-live spike *(no UI; no Databento credentials yet — recorded week stands in)*

No live key exists yet, so the headless Databento spike (entitlement, symbology resolution,
real replay-join, forced reconnect, persistent recording) is **deferred until credentials
land — run it then, as a hard prerequisite before M6 starts**. In its place: the **sim-live
source** per PRD §6 — the recorded week in `data/` streamed at wall-clock 1× through the
identical engine path, stream head anchored at **Wed 2026-07-29 09:50 America/New_York**
(08:50 CT). Session-open catch-up ("intraday replay join"), go-live, injected sequence gaps,
and continuous live-log fftlog append all exercise exactly the code the real gateway will
drive. (The review's minimal linked MP/DOM UI slice remains **declined** — M3/M4 own the
product surface.)

**Gate** (`m15-gate`, bin in fft-engine): the engine joins at session open (exact
join-prefix count, zero seeks), catches up to the 09:50 NY head, then streams
wall-pinned at 1× with |head_lag_ns| p99 ≤ one apply slice and clean sequence
accounting; scrub + GoLive resume the pin; an injected gap produces a loud gap record,
*unavailable* iceberg classification, and correct post-gap watermarks; the continuously
appended live log passes the LIVE-flag lifecycle and replays bit-identically (six-section
order-exact) through the same path; every failure path still writes FAIL evidence.

## M2 — Derived state + seek anywhere  *(2 parallel tracks)*

- **T1 `fft-profile`:** port TPO/VA/IB/VPOC/CVD/cB-cA engine; add checkpoint serialization.
- **T2 `fft-engine` + `fft-replay`:** engine service to the frozen M0 contracts — single
  writer, command protocol, generation-cancelled seeks, latest-value `Arc<RenderSnapshot>`
  publication, sequence watermarks. Snapshot frames carry book + flow + profile + CVD +
  **native-refresh state**, with queue state serialized strictly head-to-tail per level;
  seek = nearest checkpoint restore + tail replay with latest-wins coalescing.

**Gate:** **p95 seek-to-exact-state ≤ 250 ms** for 1,000 random timestamps across the week
(cold and warm, methodology explicit); seek result **bit-identical** to forward replay from
open — compared order-exact, never aggregate-depth-only: order IDs, side/price, remaining
qty, FIFO traversal, contracts/orders ahead, native-refresh and flow-window state (book *and*
profile); replay sustains ≥ 60× realtime.

## M3 — Shell + DOM pane at full frame rate

`fft-ui`: instant shell (window before I/O), thread topology per TECH-STACK §2 (dedicated
engine thread, watch channel, one-update-per-frame batch loop), **DOM ladder as one custom
Element** — Daytradr grammar: PRICE/VOL/BID/cB/cA/ASK, solid depth blocks, inside-market band,
glyph-run caching, price-drag/wheel pan, tick scale 1/2/4.

**Gate:** replay of the RTH open at 1× with DOM visible: **zero missed frame deadlines at the
attached display's refresh** (validated at whatever rates the available displays offer;
240 Hz hardware deferred — René 2026-08-11, PERF-RUNNER.md), p99 frame time within budget,
event-coverage counter = zero drops, input-to-photon ≤ 1 frame for pan.

## M4 — Market Profile pane

**MP pane as one custom Element** — WindoTrader/Dalton grammar per PRD §5: session blocks with
dividers, prior-day CP strips, current-session CP → EP → PV → SV, dual ETH/RTH lettering,
VA/VAH/VAL/IB/VPOC drawn subtly, footer with Globex opens, pinned price axis, independent tick
scale, horizontal strip pan. Progressive load: book first, profile warms in time-budgeted
chunks, prior days stream in oldest-first.

**Gate:** M3 frame gate holds with both panes at full window; pane-agreement assertion
(MP volume-at-price ≡ DOM VOL) green across a full-session replay; window resize/splitter drag
never misses a frame.

## M5 — Time travel + polish

Replay transport (`r` opt-in, scrub bar, speeds, step, go-live), scrub-drag at full frame rate
against the M2 seek service, keyboard map from PRD, prefs persistence, OS theme (Omarchy
live-reload) + OS monospace, iceberg badges + queue-depth hover readouts (exposing only the
engine state proven in M1/M2 — no new detection logic in the UI), cold-start budget work.

**Gate:** scrub drag end-to-end across a session misses zero frames while seeks resolve async;
cold start → painted window < 150 ms, → interactive book < 500 ms; all five PRD acceptance
claims pass on **historical** data.

## M6 — Live

Prerequisite: the funded Databento key and the deferred headless entitlement/reconnect spike
from M1.5, run as soon as credentials exist. Then `fft-feed`: productionize the live path —
databento client on the vendored tokio bridge (networking only; canonical-event batches
cross to the engine thread), intraday-replay
join through the *same* event path as file replay, gap/disconnect recovery with loud logging,
live session written to fftlog v2 as it streams (today's session is instantly scrubbable),
`status`-schema session states.

**Gate:** deterministic simulated gap/reconnect drills green, then a full live Globex session
(open → close) with zero dropped events, zero missed frames, seamless replay-join and
reconnect; PRD claims 1–5 verified **live**.

## M7 — Hardening & release

Week-long soak, memory ceiling audit (< 2 GB with full week), fuzz the log decoder (crash-tail
correctness itself landed in M1; this is the adversarial corpus), README + operator doc, tag v1.

**Gate:** every PRD §4 claim and budget green on the dedicated perf runner; zero open
severity-1/2 data-integrity defects, every historical integrity defect covered by a
regression fixture.

---

## Sequencing & delegation

M1 fans out only after the five M0 freezes are committed. M1 tracks are independent
(format ‖ ingest ‖ book) — three agents. M1.5 is one agent, headless. M2 runs two. M3/M4
panes are sequential on the shell but internally parallelizable (element vs. pump).
Throughout, one agent owns one crate per track; interfaces are agreed in the track brief
before code; the orchestrator merges nothing that violates a doctrine rule or lacks its gate
test. Estimated shape: M0–M2 are engine weeks, M3–M5 are the visible product, M1.5 runs on
the recorded week (no key needed), the deferred Databento spike and M6 need the funded live
key, M7 is discipline.
