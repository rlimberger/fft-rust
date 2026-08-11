# Procurement memo — perf box + live data (2026-08-11)

**RULED (René, 2026-08-11): Section A is DECLINED — no hardware purchase. The recurring
budget goes to Databento live (Section B) at M6; static/recorded data until then. Gate
policy updated in PERF-RUNNER.md (quiet-box protocol on the desk machine; 240 Hz stays
the design budget, hardware validation deferred). Section A is kept below as the
costed option should the decision ever be revisited.**

Decision memo for René. Facts researched 2026-08-11, source URLs inline; items marked
UNVERIFIED need the stated follow-up before money moves. Recommendations are firm.

## A. `fft-perf` 240 Hz runner box — DECLINED 2026-08-11, reference only

**Recommendation: buy the IPS path now — Gigabyte M27Q3 + RX 7600 + Ryzen 9700X,
~€1,100–1,600 all-in.** The gate needs deterministic 240 Hz vsync delivery, not color;
QD-OLED adds €160 and DSC-for-10-bit complexity for zero gate value.

| Part | Pick | Price (Geizhals DE) | Why |
|---|---|---|---|
| Monitor | Gigabyte M27Q3, 27″ 1440p 300 Hz, DP 1.4 | €209 | 240 Hz @ 8-bit fits DP 1.4 without DSC (~25.9 Gb/s limit); 300 Hz headroom for later gates. [geizhals.de/gigabyte-m27q3-a3589818](https://geizhals.de/gigabyte-m27q3-a3589818.html) |
| Alt monitor | AOC AG276QZD2 QD-OLED 1440p 240 Hz | €369 | Only if the desk display doubles as a trading screen. [geizhals](https://geizhals.de/aoc-agon-ag276qzd2-a3242029.html) |
| GPU | Radeon RX 7600 (ASUS Dual EVO OC) | ~€264 | AMD + Mesa RADV over NVIDIA: Hyprland's own docs list proprietary-stack workarounds/caveats ([wiki.hypr.land/Nvidia](https://wiki.hypr.land/Nvidia/)) — wrong risk profile for a fixed gate box. 2D quad+glyph load is far below this card's ceiling (qualitative; no published 2D@240 benchmark). |
| CPU | Ryzen 7 9700X, 8C/16T | ~€250 | Isolated engine+UI cores + compositor + housekeeping with headroom. ECC UDIMM supported but not required for a CI box. |
| Rest | B650 board, 32 GB DDR5, 1 TB NVMe, PSU, case | ~€400–650 | Class prices; exact SKUs UNVERIFIED — pick at order time. |

**Total: ~€1,100–1,600** VAT-incl. street, shipping extra.

Setup notes (from research, for the provisioning session):
- Core isolation: prefer cpusets/systemd slices over boot-line `isolcpus`; keep ≥1–2
  housekeeping CPUs; `nohz_full` needs a reliable TSC (Ryzen TSC-watchdog noise is a
  known report class — verify on the box). [SUSE labs guide](https://www.suse.com/c/cpu-isolation-nohz_full-troubleshooting-tsc-clocksource-by-suse-labs-part-6/)
- Runner: `runs-on: [self-hosted, linux, x64, fft-perf]`; harden per
  [GitHub's self-hosted security doc](https://docs.github.com/en/actions/reference/security/secure-use#hardening-for-self-hosted-runners).
- Photon-level 240 Hz presentation on the exact panel under Hyprland is UNVERIFIED
  until the box exists — PERF-RUNNER.md already treats it as a milestone activity.

## B. Databento live GLBX.MDP3 (M6 prerequisite)

**Recommendation: Standard plan, month-to-month, ordered ~2–3 weeks before M6 gateway
work starts — but get the licensing quote NOW (free, removes the one open number).**

| Item | Fact | Source (read 2026-08-11) |
|---|---|---|
| Plan | **Standard $199/mo** includes live GLBX.MDP3 (subscription model; usage-metered CME discontinued for new customers 2025-04-16) | [pricing update](https://databento.com/blog/updates-to-subscription-pricing), [CME plans](https://databento.com/blog/introducing-new-cme-pricing-plans) |
| Contract | Standard is month-to-month; Plus ($1,750) / Unlimited ($4,500) need annual — irrelevant for us | [pricing](https://databento.com/pricing) |
| Exchange fee | CME real-time display, professional: **$134.50/device/mo per exchange** (CME/CBOT/NYMEX/COMEX each); ES = CME exchange only ⇒ one fee. Passed through, no upcharge | [CME fee list PDF](https://api.databento.com/static/licensing/cme/cme-market-data-fee-list.pdf) |
| **Budget** | **≈ $334/mo** ($199 + $134.50) for one professional display device, ES only | — |
| UNVERIFIED | Whether the licensing questionnaire adds distribution/feed fees for a single-user non-redistributing display setup → **action: complete the questionnaire / request written quote before first payment** | [subscriber status](https://databento.com/blog/subscriber-status) |
| Rust client | `databento` 0.57.0 (2026-08-04): `live` feature, `Schema::Mbo` streaming, depends on `dbn ^0.65` — matches our pinned line exactly | [docs.rs](https://docs.rs/databento/0.57.0/databento/struct.LiveClient.html) |
| Auth | Portal API key (`db-…`, 32 chars), challenge-response; key-level IP allowlisting UNVERIFIED | [auth docs](https://databento.com/docs/api-reference-live/basics/authentication) |
| Sandbox | **None free for CME live** — historical samples + $125 intro credit don't exercise live-gateway auth. M6 plan: build the gateway against the sim-live source first (already the plan), buy the plan only for integration + soak | research pass |

Timing logic: nothing before M6 needs the key (sim-live covers M1.5–M5), Standard has no
annual lock-in, and cancel-restart only risks losing a grandfathered rate we don't have.
Buying early would burn ~$334/mo for zero code exercised.

## Actions for René (in order)

1. ~~Order the box~~ — DECLINED 2026-08-11 (budget → Databento live instead).
2. Submit Databento's licensing questionnaire → written quote (free, closes the
   UNVERIFIED fee line). No purchase yet.
3. At M6 start: Standard plan, month-to-month.
