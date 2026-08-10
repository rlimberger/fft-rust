# FFT — Rules of Engagement (read this before touching anything)

This file is the project's standing orders for **any** agent (Claude, GPT, or other) working
here. The product/architecture truth lives in `PRD.md`, `TECH-STACK.md`,
`IMPLEMENTATION-PLAN.md` — read those next. The dead prior attempt was removed from this
repo (2026-08-10) and parked at `~/Projects/fft-legacy`: reference only for the M1/M2 ports,
never build on it, never modify it.

For the latest session board — accepted crates, in-flight work, and next actions — read
`HANDOFF.md` before starting work.

## What this is

WindoTrader (Jim Dalton) and Jigsaw Daytradr (John Grady) had a baby in Rust — it looks like
its parents, only prettier, and it is **always faster than the monitor**. A missed frame
deadline is a bug, not a tuning issue. v1 = historical replay + live Databento CME MBO data,
Linux/Wayland first. No order entry.

## Why the last two attempts died (never repeat these)

1. **Performance drift** — e.g. the legacy replayer read the log 32 bytes per syscall
   (measured ~130× slower than buffered) and rebuilt ~1,600 GPUI elements per frame.
   Defense: perf gates are merge-blocking CI from M1 on; budgets are numbers in the PRD.
2. **Scope creep** — requirements churned faster than code stabilized.
   Defense: scope changes land in `PRD.md` first, in the same commit, or not at all.
3. **Agent-generated tangle** — unreviewed generated code became unmaintainable.
   Defense: one agent owns one crate/track; interfaces agreed in the track brief before code;
   no file over ~500 lines; the orchestrator reviews every diff; nothing merges without its
   milestone gate test.

## Process rules (from René)

- The main session **orchestrates**; work fans out to parallel subagents with **surgically
  precise briefs** (exact paths, interfaces, output format, explicit non-goals).
- **External CLI workers launch through the `grok` CLI with the model pinned** (standing
  order, René 2026-08-10; routing update same day): `grok -m <model> --effort high …`.
  Available routes: `grok-4.5` (default), `ocx-gpt-5-6-sol`, `ocx-xai-grok-4-5`,
  `ocx-cursor-grok-4-5-fast`, `ocx-anthropic-claude-{fable,opus}-5`. Never launch a
  worker on a default/auto model; `cursor-agent` is retired.
- Quality-critical artifacts (docs, architecture, synthesis) are authored by the
  orchestrator, not pasted from a subagent.
- Don't rush. When genuinely stuck or facing a product decision, **ask René** — don't guess.
- Verified facts only: claims about GPUI, Databento, CME, or competitors must trace to source
  (docs/code), not model memory.

## Style rules (from René)

- Documents: small, concise, state of the art. Audience bar: Elon Musk. Measurable claims —
  numbers, gates, budgets — never adjectives.
- René is an expert (Dalton MP and Grady order-flow conventions are native vocabulary: CP/EP/
  PV/SV, cB/cA, IB, VA, VPOC). Don't explain basics; lead with the trade-off and a firm
  recommendation.
- Code reads like the surrounding code; comments only for constraints the code can't express.

## Non-negotiable engineering doctrine (full detail in TECH-STACK.md §2)

1. Feed/replay engine on a **dedicated OS thread**, never the UI executor.
2. UI gets **latest-value snapshots**; signals are payloadless; **≤ 1 `entity.update` per
   frame** (GPUI's cost is per-update, not per-notify).
3. Never update an entity from inside an update; use `cx.defer`.
4. Frame/warmup budgets in **time, never event counts**.
5. UI thread never blocks on I/O or seeks; scrub targets coalesce latest-wins.
6. All log I/O via mmap/buffered reads — never per-record syscalls.
7. **Fail loudly** — no silent fallback or degraded paths.
8. Panes are single custom GPUI `Element`s painting quads + cached glyph runs.
   **Div-per-cell trees are forbidden.**
9. Tokio never runs on GPUI threads (vendored `gpui_tokio` bridge only).
10. Trade-date bucketing in **America/Chicago** (Databento is UTC-only); tick value =
    `min_price_increment × unit_of_measure_qty` (the two look-alike fields are traps);
    the 15:15 CT halt is tolerated, never assumed.

## Test fixtures

ESU6 sample week (Mon 2026-07-27 → Fri 2026-07-31), 82 M MBO events: raw zstd DBN in
`data/GLBX-20260803-4WJS899FNL/`, legacy-format session logs in `data/sessions/`. These
drive all gates through M5. No Databento live credentials yet: "live" means the sim-live
stand-in anchored at **Wed 2026-07-29 09:50 America/New_York** (PRD §6) until a key exists.
