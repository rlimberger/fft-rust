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

## Status board — 2026-08-12 evening (post-reboot review)

HEAD at review: `8bb0d1d` (claim-1 harness landed). Tree clean, `main == origin/main`.
Evidence dir: `perf-runner/results/`. **0 of 15 committed JSONs record this SHA;
0 record `git_dirty: false`.** Values stand as engineering history; they are not
v1-tag provenance.

| Milestone | Verdict | Evidence / note |
|---|---|---|
| M0 foundation | **DONE** | Freezes committed; CI green; `2026-08-10-m0-frame-gate.json` 3601/0 miss (no git fields — archive) |
| M1 data plane | **DONE** | `2026-08-11-m1-data-plane.json` — apply 2.859 s ≤ 3 s; size 0.17–0.20×; 7/7 chunk. SHA only, no `git_dirty` |
| M1.5 sim-live | **DONE** (sim half) | Dirty-tree PASS on `67b936c` (`2026-08-12-m15-simlive-gate.json`): join 5.96 M / 5.13 s, lag p99 1.049 ms vs 4 ms. **Clean-HEAD rerun owed.** Databento spike still blocked on the key |
| M2 seek/derived | **DONE** | `2026-08-11-m2-seek-gate.json` cold p95 6.9 ms / identity 25/25 / 25,230×. `m2-bit-identity-100` is 100/100 identity; its latencies self-label as non-claimable (soak concurrent) |
| M3 DOM + shell | **DONE** (60 Hz letter) | `2026-08-10-m3-frame-gate.json` 3600/0. 240 Hz deferred |
| M4 MP pane | **DONE** | Anchored two-pane PASS; agreement 1089/1089 |
| M5 time travel | **NEARLY DONE** | Scrub burst 120/120 + RSS 201 MiB PASS. Harness for claim-1 letter is in `8bb0d1d` (`--scrub-latency-gate`); **no evidence JSON yet**. Cold-start still prose-only (85–122 ms) |
| M6 live | **NOT STARTED** | Parked. `fft-feed` is a one-line stub. Do not start without the key + deferred M1.5 spike |
| M7 hardening | **RESET** | 24 h soak started Aug 11 21:12 CEST was **killed by the 17:18 CEST reboot** (kernel 7.1.7 → 7.1.8). Process gone; `/tmp/m7-soak-24h.jsonl` gone with `/tmp`. **No soak credit.** LOG-FUZZ 29,571 + README/OPERATOR stand |

**Five PRD §4 claims (honest scope):**
| # | Letter | What is actually proven | v1 status |
|---|---|---|---|
| 1 | scrub-release → rendered exact book, p95 ≤ 250 ms; bit-identical | M2: identity + headless seek p95 6.9 ms. GUI letter: instrumented (`T0=end_scrub`, `T1=Shell::render` adopt matching `seek_generation` — snapshot-adopt, not photon). **No N=200 JSON** | PARTIAL |
| 2 | full RTH, 0 missed frames, 0 drops | 60 s anchored PASS (DP-2 60 Hz). **6.5 h RTH unrun** | PARTIAL |
| 3 | exact queue, any resting order | Triple-agree vs two independent oracles, **100% synthetic**. Not a real-day census | SYNTHETIC (ask René: accept as v1 or require real-day sample) |
| 4 | native refresh, 1 ms window, gap → unavailable | SM fixtures + Wed census 8,614 / 2,685 ids on 21.4 M events. 1 ms is code+PRD aligned. Census is self-consistency, not venue-truth. Wed had 0 gaps | EVIDENCED (engine) |
| 5 | MP VOL ≡ DOM VOL, continuously | 1089/1089 on real Wed. Compare is snapshot intersection (visible DOM ∩ current-session MP); `debug_assert` only — release is not continuous | EVIDENCED (narrower than letter) |

Boring gates: 60× and RSS evidenced. Cold-start JSON **missing**. Full-RTH JSON **missing**.

### Critique — 2026-08-12 evening (what is actually wrong)

The architecture is the first of the three attempts that looks like it can ship: dedicated
engine thread, latest-value snapshots, one custom Element per pane, fail-loud gates, M1
apply 2.859 s, seek p95 6.9 ms, sim-live join + wall-pin measured. The remaining risk is
not design — it is **operational honesty** around evidence and the last 24 h.

1. **`/tmp` is not an evidence store.** Documented as volatile, then used for the 24 h
   soak JSONL *and* the week fixtures. Reboot at 17:18 CEST voided the soak and left
   only a freshly rewritten Wed pair in `/tmp`. Three names for the same files
   (HANDOFF `esu6-{mon..fri}-v3[-ckpt]`, OPERATOR `esu6-<date>`, m7-soak comment
   `esu6-fri-ckpt` + `esu6-2026-07-27`) is an operational landmine. **Freeze:
   `~/.cache/fft/gates/ESU6-YYYY-MM-DD[-ckpt].fftlog`.** Recipe:
   `perf-runner/regen-week-fixtures.sh`.
2. **24 h soak has no surviving artifact.** Mid-run "50 cycles ok, RSS 328 MB" is
   not a summary line. `after-soak-gates.sh` refuses inferred-green. Relaunch on a
   durable `--out` under `perf-runner/results/`, under `systemd-inhibit`, after
   short quiet-box gates. Do not rebuild under it (Wave-8 + this morning: exe
   `(deleted)`).
3. **Claim 1 was closed as a harness, not a measurement.** `8bb0d1d` is the right
   letter (don't re-scope to headless). T1 is pre-paint snapshot adopt — same bar as
   `--startup-trace` first-interactive, **one frame short of photon**. Exact-book
   identity stays the M2 gate. Rec: keep this bar; do not invent a swapchain probe.
   Owed: quiet-box N=200 JSON on a focused DP-2.
4. **v1-tag provenance is empty.** Every PASS JSON is dirty-tree and/or a historical
   SHA. `m15-gate` hard-fails *unknown* SHA, **not** `git_dirty: true` — the morning
   board overstated that. Replace the m15 JSON on clean HEAD; do not mass-rerun M0–M4.
5. **`83e3606` still overclaims** cold-start JSON that does not exist. Cold-start ×5
   does **not** depend on soak — run it on this quiet post-reboot box *before* the
   24 h soak. Full-RTH 6.5 h stays post-soak (only one long quiet window after).
6. **Quiet-box is not operationalized.** Desk `actions-runner` (labels
   `self-hosted, fft-perf`) is live; a fat-finger `perf.yml` dispatch starts
   `cargo build --release --workspace --bins` on this machine. Preflight does not
   fail on `rustc`. Offline the runner for soak + RTH. `pgrep -c rustc` must read 0
   before any timing gate.
7. **500-line rule:** `scrub_latency.rs` 503 (product); tests
   `protocol.rs` 882, `queue_position_gate.rs` 633, `restore.rs` 518. Split the
   product file this wave; test splits are debt, not a v1 block.
8. **fmt drift to `main`** (`24ae684`/`6e5f058`) — already fixed. Trio before every
   commit, including doc follow-ups. CI fmt lane is the backstop; no local hook.
9. **Hands-on GUI** (human scrub-drag + `--sim-live` join→LIVE→scrub→`l`) is still a
   René desk session. Scripted claim-1 is not a substitute for "drag at refresh."

Wave-10 residuals that **are** closed in source (re-verified): 60 s wall-clock live
checkpoint via injectable `Instant`; `LoadPriorSession` forbidden under SimLive
(CLI reject + engine panic); SimLive UI (`--sim-live`/`--head`/`--live-out`, `l`,
LIVE chrome); ASSERT-HUNT three (locked book, post-gap desync, zero-size trades).

### Continue here — ordered

Topology/roster unchanged: Fable 5 orchestrates; Sol → Cursor Grok → xai Grok;
pinned; target 12; workers never git-mutate; commit+push accepted work.

Fixtures (durable, this wave): `~/.cache/fft/gates/ESU6-2026-07-2{7,8,9,30,31}[-ckpt].fftlog`.
Expected ingest counts (defect if they differ): Mon 16,050,064 · Tue 14,054,511 ·
Wed 21,401,139 · Thu 16,595,979 · Fri 17,152,053. Ckpt counts: 1391 / 1392 / 1393 /
1393 / 1377. Gate replays use the `-ckpt` copy.

1. **Regen durable week fixtures** (`perf-runner/regen-week-fixtures.sh`). Copy, do
   not trust, `/tmp` leftovers.
2. **Quiet-box short gates on clean HEAD** (this reboot, `rustc==0`, runner
   offlined): replace m15 JSON; claim-1 `--scrub-latency-gate 200`; cold-start ×5
   JSON. Commit evidence. **Do not wait for soak.**
3. **Relaunch 24 h soak** — Fri `-ckpt` current + Mon–Thu priors, `--cycle-secs 0`
   `--max-hours 24` `--speed 64`, `--out perf-runner/results/<date>-m7-soak.jsonl`,
   under `systemd-inhibit --what=idle:sleep:shutdown`. Freeze the binary: no
   `cargo`, no `perf.yml`, runner stays offline. ETA = launch + 24 h.
4. **On soak PASS:** `FFT_SOAK_JSONL=<that jsonl> perf-runner/after-soak-gates.sh
   <pid>` on the quiet box (dead PID is fine if the JSONL has a terminal PASS
   summary). That writes the 6.5 h claim-2 JSON; skip cold-start if step 2 already
   committed it. Commit.
5. **René desk:** GUI scrub-drag at DP-2 60 Hz; `--sim-live` join → LIVE → scrub →
   `l`. Commands in OPERATOR.md §3.
6. **Ask René (do not guess):** accept claim 3 as synthetic-v1, or require a
   real-day queue census? Rec: accept synthetic; a week-long oracle replay is a
   new gate, not a silent re-scope of the existing one.
7. **Acceptance writeup** (orchestrator, never pasted) then **tag v1**.
8. **M6 stays parked** until the Databento key; first action then is the deferred
   M1.5 headless spike, before any `fft-feed` work.

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
