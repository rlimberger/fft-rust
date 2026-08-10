# FFT — Product Requirements

**One line:** WindoTrader (Jim Dalton) and Jigsaw Daytradr (John Grady) had a baby in Rust —
it looks like its parents, only prettier, and it is always faster than the monitor.

| | |
|---|---|
| Owner | René Limberger |
| Version | 3.1 — clean-sheet rewrite (prior attempt parked at `~/Projects/fft-legacy`); claims corrected per `PROJECT-REVIEW.md` |
| Scope | v1 = historical replay + live data. No order entry. |
| Platform | Linux/Wayland first; code stays portable |

---

## 1. Thesis

Market Profile shows **where value is**. The DOM shows **what is happening at the touch right
now**. They are two halves of one auction, and no product in the evaluated set (§2) sells
them together: the profile reference (WindoTrader, $285/mo) has no ladder; the ladder
reference (Jigsaw) has no profile. FFT renders both from **one nanosecond CME Market-by-Order
event log** — live or replayed — so the two panes can never disagree.

## 2. The market failure we exploit

Evaluated set (verified 2026-08): WindoTrader, Jigsaw Daytradr, Sierra Chart, Bookmap, CQG,
TradingView, Rithmic. Every claim below is bounded to this set.

1. **None of the evaluated products replays historical L3.** Sierra records no MBO and snapshots
   depth every 10 minutes; Bookmap recordings self-delete after a month; Rithmic keeps no MBO at
   all. Databento has every CME MBO event since 2017; no evaluated workstation consumes it.
   FFT is built on it.
2. **The field renders at 2 Hz–40 FPS.** CQG's DOM pulses ~2/s, TradingView polls up to 1000 ms,
   Bookmap defaults to 40 FPS, Sierra recommends ≥100 ms refresh. ES peaks at ~500–800 depth
   events/s; FFT applies every one exactly once and presents a fresh coherent state every
   4.17 ms at 240 Hz — a frame may represent several events, never a dropped one. The
   industry's "order-flow edge" is reconstructed from post-coalescing state; ours isn't.
3. **Queue position is an estimate in every evaluated product** (CQG renders theirs in italics).
   With unfiltered L3, the queue rank of any observed resting order is exact arithmetic.
   Sierra's MBO drops all orders under 3 lots; we drop nothing.
4. **Iceberg detection is heuristic in every evaluated product.** The CME native-iceberg
   signature (same `order_id` refilled after its displayed size fully trades) makes native
   refresh a deterministic boolean when the event sequence is complete; on a gap the flag
   reads **unavailable**, never a guess.
5. **Most of the evaluated field runs on Java, .NET, or a browser** — documented GC pauses,
   EDT starvation, HiDPI failures. The native exception, Sierra Chart, is C++ — and still
   filters MBO (point 3) and keeps no L3 history (point 1).

## 3. Principles

1. **Look like the parents.** Dalton/WindoTrader grammar is authoritative for the profile;
   Grady/Daytradr grammar for the ladder. We refine, we do not reinvent.
2. **Faster than the monitor.** A missed frame deadline is a bug, not a tuning issue — at any Hz.
3. **One log, one truth.** Profile and DOM derive from the same event stream; their
   volume-at-price totals must agree bit-for-bit (asserted continuously in debug builds).
4. **Observed, not inferred.** L3 gives exact cancels, fills, queue order. No heuristics where
   a lookup exists.
5. **Time is scrubbable.** Live head is default; drag to any nanosecond of the session.
6. **Instant shell.** The window paints before any I/O; data streams in behind it.
7. **Density with hierarchy.** Maximum information per pixel; inside market, VA, IB, POC
   findable in one saccade.

## 4. Acceptance claims — v1 ships when all five are true

| # | Claim | Measure |
|---|-------|---------|
| 1 | Seek anywhere | p95 ≤ 250 ms from scrub-release to rendered exact book, any timestamp in a full Globex session; seek result **bit-identical** to forward replay from open |
| 2 | Never miss a frame | Full RTH session at display refresh (tested to 240 Hz): zero missed frame deadlines, p99 frame time within budget, per-frame event-coverage counter shows **zero dropped events** |
| 3 | Exact queue | For any resting order: contracts + orders ahead at its price, no size filter, exact FIFO/modify semantics |
| 4 | Deterministic icebergs | Native refresh flagged on the CME signature (same `order_id`, size restored after full fill) within a **1 ms event-time acceptance window** of the depletion — CME re-displays the tranche within the same match event, so the bound GC's candidate state without ever guessing (approved 2026-08-10); per-order reload count + cumulative hidden volume, live, zero heuristics; across a sequence gap the classification reads **unavailable**, never false |
| 5 | Panes agree | MP volume-at-price ≡ DOM VOL column, byte-identical, continuously asserted |

Plus the boring gates: cold start → painted window < 150 ms; → live book < 500 ms;
steady-state RSS < 2 GB with a full week loaded; replay ≥ 60× realtime sustained.

## 5. Surfaces (v1) — desk-validated layout carried from legacy PRD

**Two panes, one linked center price, independent tick scales (1/2/4 each).**

**Market Profile (left, WindoTrader/Dalton):** per session block — divider, collapsed prior-day
CP columns (letters only), then for the current session CP → EP (one column per 30-min period;
ETH letters A…, RTH restarts at A) → PV (developing-period volume) → SV (session volume-at-price
spectrum) → pinned price axis. Footer: Globex open `MM-DD HH:MM` NY per session, dividers run
through. VA/VAH/VAL/IB/VPOC computed from day one; drawn per WT grammar (subtle, not chartjunk).
Current price full-width line; session open hairline. No strip labels, no row grid.

**DOM (right, Daytradr/Grady):** PRICE · VOL (number + bar) · BID (solid navy block) ·
cB · cA (traded-at-touch counters, reset on price change) · ASK (solid red block); high-contrast
inside-market band. Later: pull/stack columns (the data is already in the book's 5-s flow window).

**Queue/iceberg readouts** — first-class engine state, not UI garnish: the native-refresh
state machine lands in M1, its checkpoint form in M2, and the UI exposes only what engine
tests prove. Iceberg badge + reload count + hidden volume at price; exact depth-ahead readout
on hover for any observed order.

**Chrome:** header = contract · NY clock · FPS. Nothing else. Transport strip only when replay
mode (`r`) is on. Keys as in legacy PRD (`d`, `e`, `r`, space, `l`, arrows, `[`/`]`, 1/2/4,
Ctrl+wheel zoom). **Type: JetBrains Mono for all UI text** (family "JetBrainsMono Nerd
Font" as installed; no other face anywhere). **Color: Catppuccin** — flavor from prefs,
default Mocha; Latte serves light mode. Palette roles map once (base/surface/text/overlay +
semantic accents for bid/ask/VA/VPOC/IB) and every draw call uses a role, never a raw hex.
This supersedes the earlier OS-theme (Omarchy) derivation (decision: René, 2026-08-10).

## 6. Data

- **v1 instrument:** ES front month (sample week of ESU6, Mon 2026-07-27 → Fri 2026-07-31,
  82 M events, in `data/`).
- **Historical:** Databento GLBX.MDP3 MBO (DBN) → ingested once into our replay log format.
- **Live:** Databento live gateway, intraday-replay join (start at session open, stream to now,
  seamlessly go live). Session states from the `status` schema — the 15:15 CT halt is
  *tolerated, never assumed*.
- **Live stand-in (no Databento live credentials yet):** a **sim-live source** — the recorded
  week streamed at wall-clock 1× through the identical engine path, stream head anchored at
  **Wed 2026-07-29 09:50 America/New_York** (08:50 CT, twenty minutes into RTH). To the engine
  and UI it is indistinguishable from live; when credentials land, swapping in the real
  gateway changes the source only, never the path.
- **Session model:** trade date buckets in **Chicago time** (Databento is UTC-only; a CME trade
  date spans two UTC dates). Globex open 17:00 CT, RTH 08:30–15:00 CT. Roll = Monday before
  third Friday (CME published table); we display raw front-month contracts, never splice.

## 7. Non-goals (v1)

Order entry/execution · footprint charts · heatmap · day-type classifiers · alerts/automation ·
multi-instrument layouts · macOS/Windows tuning · mobile. Each is a separate PRD when its time
comes. **Scope changes land here first, in the same commit, or they don't land** — scope creep
killed the last two attempts.
