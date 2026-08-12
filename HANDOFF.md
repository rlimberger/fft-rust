# FFT — Session Handoff (2026-08-12, Wave 10)

## Wave 10 board (2026-08-12 — M1.5 sim-live LANDED, READ THIS FIRST)

**M1.5 is implemented, adversarially audited, and its gate PASSES on real data.**
Formal evidence: `perf-runner/results/2026-08-12-m15-simlive-gate.json` — 60 s gate on
the Wed checkpointed log at the PRD §6 anchor (`--head 2026-07-29T13:50:00Z`): join
5,955,247 events from open in 5.13 s (zero seeks, exact prefix count), wall-pin
|head_lag| p99 1.049 ms vs 4 ms budget over 10,739 distinct publications, scrub +
SetSpeed + GoLive resume the pin, honest resequenced gap fixture (expected=42 →
observed=141, post-gap watermarks + six-section identity of the gap-bearing live-out),
LIVE-flag lifecycle proven (LIVE mid-run; footer-indexed, flag cleared, warning-free
after Shutdown), full six-section identity of the appended live log. PASS. (JSON says
git_dirty=true — it ran pre-commit; rerun post-commit if a clean-provenance copy is
wanted.)

What landed (all uncommitted-tree work this wave, orchestrator-reviewed, workspace
fmt/clippy/test green):
- **Engine core (fft-engine):** service.rs split into runtime/forward/pacing/prior/
  sim_live/live_log (500-line rule); strict batch-local post-seek intent
  (`[Seek,Play,Seek]` paused, `[Seek,GoLive]` live, `[GoLive,Seek]` scrubbed,
  SetSource clears intent — 4 new protocol tests); exact-head validation before
  live_out creation (invalid head can never truncate an existing live log);
  **ordinal-based append frontier** (same-ts burst at the tip cannot double-append or
  leak past a scrub — unit-tested); signed saturating head-lag math; `logged_seq` =
  last committed channel seq in wire order with one-shot Gap re-anchor.
- **fft-replay:** `ReplaySource::event_ordinal()` + `SeekReport.event_ordinal`
  (exact across checkpoint restore; engine seek no longer rescans from open); splice
  rewritten as one validated monotonic transformer with typed errors (5 tests).
- **fft-book:** post-gap retained-ID reconciliation — duplicate Add replaces tainted
  order, Fill side-mismatch and sideless Fill on tainted orders skip stale depletion
  evidence (venue-wins tape/flow kept), `gap_desync_{adds,fills}` counters; 13
  gap_desync tests, fresh post-gap malformed events still panic.
- **m15-gate:** evidence-on-failure everywhere (engine panic ⇒ FAIL JSON, forced-fail
  artifacts proven); source/head validation fails closed before engine spawn;
  distinct-generation lag sampling; in-gate GoLive with settle-race fix
  (CatchingToWall lag samples wait for the pin to settle); honest gap fixture built
  directly via LogWriter (no splicer resequencing lie); LIVE lifecycle assertions.
- **Docs same-commit:** ENGINE.md §4 post-seek intent + §5 exact-head/ordinal-
  frontier/logged-seq/gate text; PRD §6 + IMPLEMENTATION-PLAN M1.5 join/pin wording.
- **New bench:** `fft-engine/benches/live_checkpoint.rs` — six-section live checkpoint
  write measured ~98 µs (~40× under the 4 ms slice) on a dense fixture; the feared
  checkpoint stall is quantified as a non-issue at current state sizes.

**24 h m7-soak still RUNNING and healthy** (started Aug 11 21:12 CEST, ETA ~21:12
CEST tonight): 31/31 cycles ok at last check, 30 s heartbeats fresh, RSS peak steady
~328 MB, zero ok=false lines. When it ends PASS, chain
`perf-runner/after-soak-gates.sh <pid>` (env inputs verified ready this wave).

Residuals (non-blocking, next wave): 60 s wall-clock checkpoint cadence unproven
in-gate (bench says cost is trivial; a ≥61 s gate window or fake-clock LiveLog test
would close it); provenance not hard-fail in m15-gate (git_sha="unknown" can still
PASS); prior-session-in-checkpoint vs live-out replay contract needs a ruling if
priors are ever loaded during SimLive; UI wiring for SimLive (CLI flag, GoLive key,
LIVE indicator) is an M3/M5 track — planning lane died without output, re-brief when
needed. Sol quota was cooling down this wave; roster fallback to Cursor/xai Grok
worked and Sol is usable again.

## Wave 9 board (2026-08-11 evening — superseded by Wave 10 above)

The 24 h m7-soak from Wave 8 was found WEDGED and killed: launched on a binary that
was rebuilt underneath it (`/proc` exe "(deleted)"), `--cycle-secs 0` mapped to a
hidden 86 400 s wall deadline, and `play_until` required events_applied>0 before its
EOF check — cycle 1 sat in a poll loop for 3 h with zero JSONL (the rig had no
heartbeat). The rig was reworked and PROVEN (`67b936c`): cycle_secs=0 = EOF under a
finite `(span/speed)×2+120 s` deadline, 30 s heartbeat JSONL lines, 60 s ready
timeout + readiness Seek, post-scrub EOF rewind, honesty split to honesty.rs. Full
EOF smoke on the new binary: 21.42 M events to EOF in 1309.5 s, 43 heartbeats,
25/25 seeks answered, RSS peak 328 MB, clean self-exit, PASS. **24 h soak
RELAUNCHED on the fixed binary (PID noted in session; out /tmp/m7-soak-24h.jsonl;
watchdog monitor: silence >15 min, ok=false, and final verdict).** When it ends
PASS, chain `perf-runner/after-soak-gates.sh <pid>` (now split: 181-line driver +
lib/{soak-validate,cold-start}.sh) for the full-RTH claim-2 gate + cold-start JSON.

Committed + pushed this wave (all diff-reviewed by independent read-only lanes,
workspace fmt/clippy/test green at each commit):
- **DOM-hidden-default** (`cd40794`): MP is the launch surface; `d` toggles the DOM
  without resetting MP nav; transport keys armed by `r` (silent no-ops off);
  zero-filled scaled-tick lattice when linked center exits engine depth (no
  fabricated sizes); prefs clamp loudly; dom_ladder split (500-line rule);
  PRD/README/OPERATOR same-commit.
- **MP polish** (`854301b`): session-scoped semantic lines (review caught a zoom<1
  overpaint — current session now clips to block∩viewport like priors, test at
  zoom=0.5), prior IB hairlines, draw order open<IB<VA<VPOC<price, scale-aware
  thickness, fixed 80px·ui_scale axis, prior TPO dim 0.55.
- **Claim-3 queue gate** (`01dcaa5`): Book vs two independent oracles (Shadow Vec CME
  model + BookFifo prefix sums over BOOK v3 bytes), 15 scenarios incl. the depleted
  Fill→Modify tail-reinsert and snapshot-origin modify demotions; after-soak-gates
  honesty rework + 500-line split.
- **Claim-4 census** (`2e87549`): full-Wed headless refresh census — 8,614 native
  classifications / 2,685 ids / 26,483 hidden / max 87 reloads, consistency-checked,
  PASS evidence committed. (666 EOD Unavailable are snapshot-origin, not gap.)
- Housekeeping splits (`3cdae7d` fft-log fuzz, `f1b6d40` fft-ingest, `708b08a`
  m1-gate) — pure mechanical, test counts identical. Doc fix `78cb5b7` (FFTLOG-V2 §0
  had conflated the 25/25 and 100/100 identity citations).

**Five-claim evidence audit (adversarial, this wave):** claim 5 EVIDENCED
(m4-agreement 1089/1089); RSS + 60× EVIDENCED; claim 3 now gate-tested (synthetic);
claim 4 now censused on real data; claim 1 PARTIAL (seek p95 + identity yes;
scrub-release→rendered path unmeasured); claim 2 PARTIAL (60 s anchored PASS; full
RTH 6.5 h gate not yet run — post-soak chain); cold-start/interactive numbers exist
only as HANDOFF prose — the committed JSON is owed by the post-soak chain (commit
83e3606's message overclaims: no cold-start JSON landed). v1 tag blocks on: clean
24 h soak on the fixed rig → full-RTH gate + cold-start JSON → GUI scrub-drag
hands-on → acceptance writeup (orchestrator-authored).

**Week-soak fixtures READY (2026-08-11, /tmp — regenerate after reboot with the
canonical recipe):** all five ESU6 trade dates as checkpointed logs,
`/tmp/esu6-{mon,tue,wed,thu,fri}-v3-ckpt.fftlog` — 85.3 M events total, 0 gaps every
day, Mon/Tue byte-identical to the cached `~/.cache/fft/sessions` logs (ingest
determinism confirmed). Per-day: Mon 16.05 M ev/1391 ckpts, Tue 14.05 M/1392,
Wed 21.40 M/1393, Thu 16.60 M/1393, Fri 17.15 M/1377. Fri's −16 checkpoint count is
**investigated and benign**: the offline cadence is event-driven (60 s event-time,
quiet minutes emit nothing — checkpoint.rs:23,189–208) and Friday's bucket ends at
~16:00 CT (weekend halt, no Sunday-open traffic) ⇒ ~23 h span vs midweek ~24 h;
actual ≤ ceil(span/60) on all five days. Not a checkpoint-pass defect.

**M1.5 sim-live is now specced and in flight:** ENGINE.md §5 frozen this wave
(Source::SimLive { path, head_ts }, join = budgeted catch-up from open, absolute
wall pin at head, GoLive to head, engine live-log append with 60 s wall-clock
checkpoints + logged_seq decoupling, watermark stage split, harness-side gap
injection, m15-gate bin). Implementation track was running at session close — if
its diff is in the tree unreviewed, review before committing; the gap inventory
(file:line, in the Wave-9 transcript) is the ground truth for what existed before.

## Prior handoff (2026-08-10, independently audited)

## Binding

**Topology (René 2026-08-10, roster updated 2026-08-11 — canonical text lives in
AGENTS.md "Process rules"):** the orchestrator is Claude Fable 5 inside one opencodex/
grok session; ALL implementation fans out to in-session subagents, priority
Sol → Cursor Grok → xai Grok, target parallelism 12 (2/5/5). No external CLI workers.
Standing orders that carry over: commit + push accepted work without asking (scoped
commits per track, review first); recenter key `c`; models always pinned, never
default/auto; workers never run git mutation commands in the shared tree.

## Wave 7 board (2026-08-11 — M5 build-out, READ THIS FIRST)

All measurable milestone gates are now green. M1 closed this wave: apply-path cleanup
(`da67519`: budget poll every 256 events + reused frame buffer, −0.42 s, proven by
chunked differential + m2 bit-identity 3/3) brought busiest-day apply to **2.859 s**;
René ruled option 3 — budget revised to **≤ 3 s** (IMPLEMENTATION-PLAN M1, same-commit
edit; the < 2 s number predated per-event refresh/flow/profile state, attribution in
the M1-APPLY-PROFILE report: ~30% decode, ~47% book, ~0% profile, ~30% ReplaySource
plumbing; refresh-GC exonerated at ~0.1% of wall) — **M1 gate PASS** evidence
committed. A book-structure optimization track (hasher/pre-size/FIFO hot path, under
M2 bit-identity) is queued as NON-BLOCKING polish.

M5 features landed this wave (all reviewed, committed, pushed):
- **UI-TRANSPORT** (`2654d8c`): r strip / space / [ ] speed ladder / arrow step
  (±1 s Seek — FINAL, see below) / scrub with latest-wins one
  Seek per frame, gens from 2 (anchor owns 1). Scrub range = session bounds from
  trade_date (snapshot lacks log extent — plumb later if multi-day scrub wanted).
- **DOM-ICEBERG** (`0a617d3`): per-price refresh badges + ×N fit-gated labels,
  Mauve role; hidden volume deferred to the hover track (VOL too tight).
- **LoadPriorSession** (`6836af0`, ENGINE.md §2 frozen same-commit): async
  profile-only prior-day builds, 2 ms slices, complete-or-invisible, current
  always last. UI wiring landed later this wave (`a50cc49`: `--prior` repeatable
  → oldest-first dispatch after Play).
- **MP-PANZOOM** (`6d89b16`): prior sessions as collapsed CP strips, axis-dominant
  horizontal pan, Ctrl+wheel cursor-anchored zoom 0.5–3×, state in PaneState.
  (Worker died mid-track once; resumed and completed — a worker "report" that is
  mid-work prose means the session ended, check the tree.)

Wave 8 additions (2026-08-11, all pushed): auto prior-day pipeline complete —
discovery (`4e008f5`) + auto-ingest from raw DBN (`0ea8746`: candidates from the DBN
UTC range, params from the replay log's header, oldest-first progressive dispatch,
functional proof under a passing 60 s gate); MP input remap (`a26dcf4`: wheel zooms,
Ctrl optional; left-drag pans; DOM wheel-pan unchanged); header chrome (`587081d`:
NY event-time clock + FPS; symbol placeholder — engine follow-up: plumb
InstrumentMeta.symbol into RenderSnapshot); priors survive Seek (`817f904`,
ENGINE.md §2 extended — defect found by the m7-soak rig's smoke); m7-soak rig
(`f43dd29`); LOG-FUZZ clean bill 29,571 mutants (`e5eee19`); CI builds all gate bins,
perf.yml modernized (`4f422af`); README + OPERATOR.md authored (`f5e2c38`); canonical
subagent roster in AGENTS.md (`7de701b`). **24 h m7-soak RUNNING** on the desk box
(out: /tmp/m7-soak-24h.jsonl; watchdog monitor alerts on leak/RSS/failure/summary).
M7 remaining: soak clean → week soak decision, five-claim acceptance writeup, v1 tag.

M5 measurables CLOSED (quiet-box evidence committed): cold start first-paint
85–103 ms / interactive 103–122 ms over 5 runs (budgets 150/500 — PASS;
--startup-trace instrumentation, normal runs unchanged); scrub burst 120 seeks
@60/s all answered, monotonic, no wedge (PASS); full-week RSS VmHWM 201 MiB vs
2 GiB budget (PASS — Fri current + Mon–Thu priors per ENGINE.md §2 date rule).
Hover readouts + prefs landed (7058505, a50cc49). Arrow-step ±1 s FINAL.
M5 residue before calling the milestone: a live GUI scrub-drag session on the
desk display (the headless burst proves the seek service; the GUI frame gate
during drag is a hands-on run) + the five-claim acceptance sweep writeup.
shell.rs split via shell_replay.rs (size rule). Next frontier: M6 needs the
parked Databento work; M7 hardening (soak, fuzz, docs) can start any time.

## Wave 6 board (2026-08-11 — milestone gates — superseded by Wave 7 above)

Three gate harnesses landed (`4192ef4`: m1-gate, m2-gate, m4-agreement — headless
evidence bins with exact quiet-box commands in their --help/source) and the measured
gates ran on the idle desk box (evidence committed, `635754d`):
- **M2 seek gate: PASS** — 1000 seeks cold p95 6.9 ms (budget 250), bit-identity 25/25
  across all six sections, 25,230× realtime (budget 60×). Required fixing
  **REFRESH-SEEK-DIVERGE** (same commit): tombstone GC was 4096-apply-interval with
  phase reset on restore ⇒ restore+short-tail kept expired tombstones a from-open pass
  had swept — REFRESH-only byte divergence. Now: refresh GC every apply (event-time),
  restore-side sweep (legacy checkpoints restore clean — the PASS ran against pre-fix
  checkpoints), serialize-side filter as defense. MP volume split also removed
  (`03f2eef`, René ruling: SV = total only).
- **M4 agreement gate: PASS** — 1089/1089 snapshots, coverage 1.0, 0 MP≡DOM
  disagreements over full Wed. (M4's frame-gate half already PASSed Wave 5.)
- **M1: differential PASS** (7/7 real-data chunk splits — first real-data coverage),
  size 0.186–0.199× legacy all five days (budget ≤0.5×), **but busiest-day one-shot
  apply 3.443 s vs the <2 s budget: FAIL, honest**. Wed is 21.4 M events (plan's
  "20.6 M" is a v1-era count). NEXT WAVE: profile the apply path (book+profile per
  event) before touching the budget — the 2 s number predates profile/CVD/refresh
  per-event state. Don't edit the budget without René.
- Also this wave: OS-theme live derivation + warm switches (`1caaabe`, `81e0818`),
  gap-desync venue-wins + locked/crossed diagnostic-only (`d80f927`, evidence-refined
  ruling), zero-size trades (`1e00ecc`), engine shutdown evidence path (`c336266`),
  --replay-at anchored starts (`3d8e6f9`, `9dd8c4d`), no-perf-hardware ruling
  (`cf35626`), Databento parked (`d0cdb12`).
- Open: M1 apply-time investigation (only red gate number); M5 fan-out after it.

## Wave 5 board (2026-08-10 late night — superseded by Wave 6 above)

**RULING (René, 2026-08-11): no dedicated perf hardware — ever, for now.** The 240 Hz
box is declined; the recurring budget goes to Databento live (Standard ~$334/mo all-in)
at M6. Consequences, already landed in the docs (PERF-RUNNER.md, PRD §4 promise 2,
IMPLEMENTATION-PLAN M3 gate, PROCUREMENT-2026-08.md): frame gates run on the desk
machine under the quiet-box protocol (no concurrent builds — host load measurably
injects 33 ms spikes); 240 Hz remains the design budget in time units; photon-level /
high-refresh hardware validation is deferred and gates a release claim, never a merge.
Databento is parked entirely (René 2026-08-11, "stop worrying about DB"): no
questionnaire, no quote, no purchase — it comes back only when René raises it or an M6
track is actively briefed. Facts stay in PROCUREMENT-2026-08.md for that day.

**Standing directive (René, this session): theme + font size follow the OS (Omarchy)
system, live** — landed as UI-OS-THEME (`1caaabe`, PRD §5 same-commit). fc-match family
at startup; `[font] base-size` → UI scale (base/12); colors from
`~/.local/state/omarchy/current/theme/colors.toml`; 500 ms poll thread, latest-value
snapshot, one atomic compare per frame, mocha fallback (loud) off-Omarchy. Live-switch
validation: 14 theme+size flips during a 30 s anchored gate — only the first scale
change cost one 33 ms frame (cold glyph reshape at the new size; every later flip in
budget; static control 0 missed). Known residual: that first-switch reshape frame; if
René wants it gone, pre-shape the row glyph set for the incoming scale on the watcher
thread before publishing. Engine follow-up in the same wave (`c336266`):
`EngineHandle::shutdown` on a dead engine returns the panic as Err instead of
panicking — the evidence-destroying shutdown path is closed and regression-tested.
Fixture note: HANDOFF's expected regen counts said "7409 snapshots dropped" — that was
measured on a two-file ingest; the six-file regen command correctly reports 30,200
(other days' blocks). Kept bytes are byte-identical (sha256-verified); 21,401,139
events / 0 gaps / 1880 holes / 7561 kept / 1393 checkpoints all match.

**Model roster (René 2026-08-11, supersedes the earlier roster order): subagent priority
is 1. `ocx-gpt-5-6-sol` (Codex Sol) → 2. `ocx-cursor-grok-4-5-fast` (Cursor Grok) →
3. `ocx-xai-grok-4-5` (xai Grok) — prefer the highest-priority model available for each
brief, fan out MANY in parallel. Opus 5 remains available for read-only research when the
Grok/Sol lanes are saturated. Always pinned, never default/auto. Claude Fable 5 is the
orchestrator only and off limits as a subagent.**

**Standing directive (René, this session): gate/replay sessions run anchored at the PRD
§6 sim-live head — Wed 2026-07-29 09:50 America/New_York.** (René wrote "wed 7-28"; the
sample week's Wednesday is the 29th and PRD §6 pins 07-29 — interpreted as the PRD
anchor.) Mechanism landed:
- `fft --replay-at <ts>` (`3d8e6f9`): ns-UTC or `YYYY-MM-DDTHH:MM:SSZ`; anchor = UTC
  `2026-07-29T13:50:00Z`. Shell sends SetSource → Seek(gen 1) → Play; future UI scrub
  seek counters must start ≥ 2. The anchor lands verbatim in the evidence `gate` field.
- Engine batch-order defect found by the first anchored run and fixed (`9dd8c4d`): a
  `[Seek, Play]` batch executed the coalesced seek *after* the loop, and the seek's
  pause swallowed the Play — anchored runs seeked then sat at 0 events. Batch position
  now is meaning (Play-after-Seek resumes from the anchor; Pause-after-Seek stays
  paused). Two protocol tests.
- Anchored gate evidence committed: `2026-08-10-m4-two-pane-gate-anchored.json` —
  PASS, 3601/3601, 0 missed, max 17.084 ms, **coverage 47,634 events (~10× the log-open
  trickle)** at RTH load through checkpoint-restore + 60 s forward flow.
- Canonical anchored gate command:
  `./target/release/fft --gate 60 --replay /tmp/esu6-wed-v3-ckpt.fftlog
  --replay-at 2026-07-29T13:50:00Z --gate-out perf-runner/results/<date>-<gate>.json`

State at session close (all listed work committed + pushed; workspace fmt/clippy/test green):
- **M3 + M4 frame gates PASS with real evidence.**
  `perf-runner/results/2026-08-10-m3-frame-gate.json` (single-pane gate file the board
  owed): frames 3600/3600, missed 0, p50 16.777, p99 17.302, max 17.129 ms, coverage
  4823/4823 dropped=0, verdict PASS. `2026-08-10-m4-two-pane-gate-trace.json`: 3600/3600,
  missed 0, max 17.172 ms, PASS. Both at 60 Hz DP-2, unfocused with the throttle opt-out.
- **The 32.3 ms spike: formally attributed to host jitter, app paint unindicted.**
  Method: `--trace` reruns under three load conditions plus a blank-window control.
  (1) Under concurrent cargo builds: 13–17 misses, all ~33.3 ms (2 refresh intervals).
  (2) **Blank window, zero content, same box: 5 misses, identical ~33.3 ms signature**
  (`2026-08-10-blank-window-control.json` + traces in /tmp, volatile). (3) Quiet box
  (no builds): **0 misses × 3 consecutive 60 s runs**, max 17.2 ms — 7.8 ms of headroom.
  The spike is compositor/scheduler jitter on the shared desk box, load-correlated,
  content-independent. The real fix is the isolated `fft-perf` runner box
  (PERF-RUNNER.md pinned config, core isolation) — still unprovisioned; on this shared
  box a gate run is only valid on an otherwise-idle machine. One 6.9 s frame outage was
  observed while two subagent build jobs saturated the box — same attribution.
- **EVIDENCE-META landed** (`92adcc2`): evidence JSON now always carries `gpui_rev`
  (parsed from Cargo.lock at build time, loud build failure if absent); new
  `--manifest <path>` (existence-validated before the window opens) and
  `--conditions <text>` flags recorded verbatim, null when unsupplied.
- **MP session-open hairline landed** (`f2b1f16` engine, `da0945a` ui):
  `ProfileSessionRender.open` from `Session::open_price()`; painted first among the
  semantic lines, palette role `session_open` = Catppuccin Lavender @ 0.40 alpha both
  palettes. `civil_from_days` dedupe (owed item b) done in the same track.
- **ASSERT-HUNT sweep completed** (read-only audit vs FFTLOG-V2 §4; full report in the
  session transcript). Finding 1 **fixed** (`f2b1f16`): engine watermark now re-anchors
  on Gap (one-shot, mirrors `Book::do_gap`; post-gap seq below the watermark was a
  guaranteed panic on any gap-bearing log — latent for M6 live). Prior claims
  re-verified as mitigated: profile lattice admission, backward-ts periods, profile
  snapshot-gate (+ book-first apply ordering confirmed at fft-replay source.rs:282).
  **Open REACHABLE findings, triaged for the next wave (not fixed, other crates):**
  (a) fft-book `check_invariants` (query.rs:320) asserts `bb < ba` — a locked book
  (bb == ba) panics on Seek/restore; §4 freezes nothing about locks. Needs a René
  ruling: legalize lock, forbid only cross. (b) book.rs Cancel/Modify hard-assert
  size/price agreement on *known* ids after a true Gap — post-gap desync panics
  instead of gap-state accounting (§4: classification across a gap reads
  unavailable). (c) profile session.rs:215 asserts Trade size > 0 — §4 leaves size
  free. All three are unreachable on the clean Wed v3 log (0 gaps), so nothing burns
  today; (a)+(b) become real the day a gap-bearing or locked stream replays.
- Owed follow-ups carried: ProfileSessionRender consumers beyond MP (none yet);
  `manifest` field still null until the perf-runner box + manifest exist; 240 Hz
  hardware still mandatory for the M3 gate per PERF-RUNNER.md (current evidence is
  60 Hz desk-display — accepted as interim, the letter of that spec still owes a
  240 Hz run when the box lands).

## Wave 4 board (2026-08-10 night — superseded by Wave 5 above)

State at session close:
- **ENGINE-DEFECT-WAVE landed** (`663a1cc`): snapshot-flagged records exempt from ALL
  seq accounting (`CanonicalEvent::is_snapshot()` in fft-core, wired through fft-replay
  cursor + fft-engine watermarks — this was the panic waiting ~2 h into any GUI replay);
  reset_pacing fails loudly; EngineExit carries CoverageCounters; an engine panic can no
  longer destroy gate evidence (JSON written first, verdict FAIL + note, then FAILURE
  exit); Seek on a checkpoint-less log panics with the fft-checkpoint remediation.
- **GPUI-THROTTLE-OPT-OUT landed.** The m4 466-miss FAIL was GPUI's unfocused ~30 fps
  throttle (FRAME-STALL-DIAGNOSIS). Fix: `rlimberger/zed` fork, branch
  `fft-inactive-throttle-optout` = pinned rev `492acd6` + commit `34ba175` adding a
  `GPUI_DISABLE_INACTIVE_THROTTLE=1` opt-out in gpui window.rs; workspace deps pin that
  fork rev directly (Cargo.toml; `.cargo/config.toml` fetches git deps via the git CLI —
  this machine's insteadOf rewrite breaks cargo's built-in fetcher); fft main.rs sets the
  env var unconditionally. The working clone `~/Projects/zed-fft` is now only needed for
  future gpui patches, not for builds. Release build + workspace clippy green.
  **VALIDATED (unfocused 60 s run, evidence
  `perf-runner/results/2026-08-10-m4-two-pane-gate-optout.json`): missed 466 → 1,
  frames 2927 → 3600** (full vsync delivery unfocused), p50 16.777 ms, p99 17.302 ms,
  coverage 4823/4823 dropped=0. Verdict is FAIL on the letter of the gate: **one** 32.3 ms
  spike in 3600 frames. FIRST ACTION next session: rerun with `--trace`, localize that
  single spike (candidates: glyph-cache generation sweep, engine publish stall, compositor
  jitter), fix or formally attribute it — a missed deadline is a bug, not a tuning issue.
  Also fill the evidence JSON's null `gpui_rev`/`manifest`/`conditions` fields (schema
  exists, values unplumbed).
- Owed follow-ups: (a) `ProfileSessionRender` session open-price field (fft-engine) →
  unblocks the MP session-open hairline (fft-ui); (b) `civil_from_days` in mp_view.rs
  duplicates jiff — fold; (c) an adversarial audit's assert-hunt dimension never ran
  (agents died on session limits) — sweep every `assert!`/`expect` reachable from real
  data for unfrozen semantics; remaining audit findings are already triaged into
  ENGINE-DEFECT-WAVE. (d) The m3 gate (single-pane DOM) still has no valid evidence
  file — rerun `fft --gate 60 --replay <wed-v3-ckpt> --gate-out
  perf-runner/results/<date>-m3-frame-gate.json`.

Volatile fixtures (`/tmp` dies on reboot — regenerate with these exact commands):
```
cargo run --release -p fft-ingest -- write /tmp/esu6-wed-v3.fftlog \
  data/GLBX-20260803-4WJS899FNL/*.mbo.dbn.zst --trade-date 2026-07-29 \
  --tick 250000000 --uom-qty 50000000000 --display-factor 1
cargo run --release -p fft-engine --bin fft-checkpoint -- \
  /tmp/esu6-wed-v3.fftlog /tmp/esu6-wed-v3-ckpt.fftlog
```
Expected: 21,401,139 events, 0 gaps, 1880 seq_holes_ignored, 7561 snapshots kept /
30,200 dropped (six-file run: the other days' snapshot blocks; a two-file
28th+29th ingest reports 7409 dropped and yields a byte-identical log —
sha256-verified 2026-08-10); checkpoint pass writes 1393 checkpoints. Gate replays
use the -ckpt copy (Seek panics on the plain one by design).

Worker git hygiene (hard rule, learned from a real incident): subagents NEVER run
`git stash`, `git clean`, `git checkout -- .`, commit, or push in the shared tree —
one worker's stash swept another's uncommitted track. Workers leave diffs in the
working tree; the orchestrator reviews, commits, pushes.

## Evening wave 2 (2026-08-10, orchestrator review — read this first)

Staffing: **Sol (gpt-5.6) is out of tokens and unavailable.** Remaining workforce: the two
Grok sessions + orchestrator's Opus 5 subagents.

New standing directives (René, in PRD §5 same-commit):
- **JetBrains Mono for all UI text** (installed family: "JetBrainsMono Nerd Font");
- **Catppuccin palettes** (default Mocha, Latte for light), all draw calls through palette
  roles — supersedes OS-theme/Omarchy derivation. Open track: **UI-THEME-CATPPUCCIN**
  (replace hardcoded hex constants in mp_element/dom_ladder + font_family("monospace")).

Accepted + committed this wave (diff-reviewed, workspace fmt/clippy/test re-verified green):
- **CHECKPOINT-PASS** (`c155229`, fft-engine): six-section 60 s event-time checkpointed
  copy through the shared apply path; Wed real run 21.4 M events → 1393 checkpoints,
  restore seeks 4.5–6.3 ms, no replay-from-start. fft-checkpoint bin.
- **MP-PANE** (`c3c2e4a`, fft-ui): WT profile as one custom Element + linked two-pane
  shell; 14 pure tests; zero entity.update. Follow-ups owed: (a) `ProfileSessionRender`
  needs a session open-price field (engine crate) before the session-open hairline can
  draw; (b) RESOLVED (`4157013`): René ruled hover-routing canon — 1/2/4 set the hovered pane,
  `t` syncs the other pane to it; PRD §5 updated, Shift chords dead; (c) hand-rolled
  `civil_from_days` in mp_view.rs duplicates jiff — fold into the theme/polish wave.
- **LOG-POLISH-PERF-CI** report reviewed post-hoc (already landed as `f854f7f`); its
  PERF-RUNNER.md staleness findings fixed by orchestrator (evidence-file section added).

**GATE EVIDENCE — two-pane frame gate FAILED (expected-red, first real run):**
`fft --gate 60 --replay /tmp/esu6-wed-v3-ckpt.fftlog` on the 60.2 Hz desk display:
frames=2927 **missed=466 (16 %)** p50=16.777 ms p99=50.332 ms max=50.456 ms,
coverage 4823/4823 dropped=0. Spikes are ~3 refresh intervals, periodic, at near-zero
event load → structural UI/engine stall, not data volume. Evidence:
`perf-runner/results/2026-08-10-m4-two-pane-gate.json` (note: the untracked m3 evidence
file from the earlier session is empty — that run died before writing; rerun wanted).
Caveats: desk display is 60 Hz (240 Hz box per PERF-RUNNER still unprovisioned); run was
launched unattended — focus flapping can throttle animation frames to ~30 fps and would
look exactly like this. **RESOLVED by FRAME-STALL-DIAGNOSIS** (cursor CLI worker, evidence-only): the 466-miss
signature is **GPUI's unfocused ~30 fps animation throttle**, not app paint cost — blank
window unfocused shows p50=50 ms/87 % miss with zero content; checkpoint frames cannot
occur inside the gate window (first at +60 s event time; forward replay skips checkpoint
frames); glyph sweeps ~2×/run. The m4 FAIL is **invalidated as a focus artifact**; app
paint is unindicted. Next: **GPUI-THROTTLE-OPT-OUT** (orchestrator — patch the pinned rev
or upstream opt-out per TECH-STACK §2 known-constraint), then an **attended** re-gate
(window focused 60 s) before any paint-cost work.

Wave 3 additions: **UI-THEME-CATPPUCCIN accepted** (`2c6b972`, grok CLI worker) —
theme.rs Palette (26 roles, official Mocha/Latte), JB Mono root family, grep-clean, 64
tests. Standing order (`9450d97`, AGENTS.md): external CLI workers always pin Grok 4.5
high (`grok -m grok-4.5 --effort high` / `cursor-agent --model cursor-grok-4.5-high`);
this wave's two launches predated the order and ran default models.



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

Accepted + pushed (2026-08-10, later): **INGEST-SNAPSHOT-ADMISSION** (`88b37f2`) —
Wed measured exact: 7561 snapshots kept / 7409 dropped. Grok's gap analysis REFUTED
the "phantom gaps from snapshot seqs" hypothesis (bypass already existed): the 1880
gaps were forward channel-seq holes from Databento symbol filtering (`ES.FUT`
parent). condition.json = `available` all five days ⇒ holes are filter artifacts,
not loss. Policy frozen in FFTLOG-V2 §4 "Batch gap policy": regression-only Gap
synthesis for batch, `seq_holes_ignored` counted loudly; live gaps at gateway (M6).

Accepted + pushed (2026-08-10, later still): **BOOK-SNAPSHOT-LOAD** (`ac03cca`) —
snapshot-load apply + BOOK/FLOW/REFRESH split + fresh-flow eviction; SESSION(6)
pends on the profile track. Full-Wed headless replay then advanced from event #755's
old seq-panic to a NEW blocker at the same spot: **Fill semantics** — frozen in
FFTLOG-V2 §4 (Fill never mutates the book; companion Cancel/Modify is book truth;
execution price may differ from displayed price). Synthetic M1 fixtures encoded the
wrong assumption; real data caught it.

Profile investigation (Opus subagent, evidence-grade): audit claim 6 CONFIRMED in
full — RTH letters silently run N/O over 15:00–16:00 CT; PV gap marker lands on the
wrong period (cursor is trade-driven, not event-time-driven); backward-period ts
panics are reachable on the frozen stream (TsReset legalizes backward ts); PLUS two
new findings: ingest keeps [17:00, 17:00) but the profile session is [17:00, 16:00)
— a kept event in the 16:00–17:00 CT hour panics the lattice (fixture margins are
30–530 ms); and fft-profile has zero SNAPSHOT-flag awareness — accidentally safe
only because fft-book's apply runs first. SESSION split inventory: fields 2-5
(session_count, trade_date, current_eth_period, period_gap) → SESSION(6); clock
boundaries live in code, not bytes.

External workforce (2026-08-10): cursor-Grok (busy: INGEST-GAP-POLICY), GPT 5.6 Sol
(idle → BOOK-FILL-SEMANTICS), xai-Grok 4.5 (idle → PROFILE-WAVE). Orchestrator
subagents: profile investigation done; UI-GATE-EVIDENCE in flight.

Accepted + pushed: PROFILE-WAVE (`d1862d7`), INGEST-GAP-POLICY (`fcdc5f6`, Wed v3 =
0 gaps / 1880 holes ignored), BOOK-FILL-SEMANTICS (`1efc320`), side=None freeze
(`92d4112`). All six checkpoint sections now exist → checkpoint pass unblocked.

**MILESTONE (2026-08-10 evening): full Wed trade date — 21,401,139 events — replays
cleanly through book+profile headless** (`ce727cf`). All real-data semantics frozen
today (snapshot admission/load/Clear framing, non-mutating fills, side=None, batch
gap policy) verified end-to-end. User-visible next: `./target/release/fft --replay
/tmp/esu6-wed-v3.fftlog` focused; then the M3 gate with `--gate 60 --gate-out`.

In flight next:
- **FILL-SIDE-NONE** (Sol, fft-book): sideless auction fills per frozen §4.
- **CHECKPOINT-PASS** (cursor-Grok, fft-engine bin — orchestrator-delegated track in
  the orchestrator's crate): offline checkpointed-copy writer per ENGINE.md §4(2).
- **LOG-POLISH + PERF-CI** (xai-Grok, fft-log + perf workflow): was_live/is_live
  semantics, reader.rs split, perf.yml --gate-out wiring.
- **UI-GATE-EVIDENCE**: ACCEPTED — coverage exit line + --gate-out JSON evidence
  (git sha+dirty, refresh/deadline, p50/p95/p99/max, coverage; FAIL on any drop).
  Two engine-side follow-ups it exposed (orchestrator's crate): (a) an engine-thread
  panic kills evidence — EngineHandle::shutdown expects on a dead thread before the
  JSON is written; (b) CoverageCounters unreachable at exit except via last published
  snapshot — add coverage to EngineExit. Also observed: committed
  fixtures/fft-log/clean_small.fftlog trips the profile lattice (PROFILE-WAVE item 2
  territory), and an independently ingested 2026-07-26 log reproduces the Fill panic
  (BOOK-FILL-SEMANTICS confirmation from a second day's data).
- Audit spot-checks: claims 1, 7, 8 CONFIRMED (7 approved+documented in PRD, 8 fixed);
  claim 6 plausible (fft-profile wave pending); claim 10 REFUTED with tests.

Truth: `PRD.md`, `TECH-STACK.md`, `IMPLEMENTATION-PLAN.md`, and
`docs/{FFTLOG-V2,ENGINE,PERF-RUNNER,FIXTURES}.md`. Never modify `~/Projects/fft-legacy`.

## Repository state (HISTORICAL — morning audit snapshot, superseded by the wave boards above; kept for the discrepancy list's context)

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
