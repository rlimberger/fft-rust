#!/usr/bin/env bash
# Gap-closing measured runs that need the quiet box — chained behind the 24 h soak.
# ACCEPTANCE-MAP gaps: claim 2 (full RTH session frame gate) + boring-gate cold-start JSON.
set -euo pipefail
cd "$(dirname "$0")/.."

SOAK_PID="${1:?usage: after-soak-gates.sh <soak-pid>}"
DATE=$(date -u +%Y-%m-%d)

echo "waiting for m7-soak (pid ${SOAK_PID}) to finish..."
while kill -0 "${SOAK_PID}" 2>/dev/null; do sleep 60; done
echo "soak done at $(date -u --iso-8601=seconds); cooling 120 s"
sleep 120

# Claim 2 letter: full RTH session, zero missed deadlines, zero drops.
# RTH open Wed 2026-07-29 08:30 CT = 13:30 UTC; 6.5 h = 23400 s.
./target/release/fft --gate 23400 \
  --replay /tmp/esu6-wed-v3-ckpt.fftlog \
  --replay-at 2026-07-29T13:30:00Z \
  --conditions "full RTH session gate (ACCEPTANCE-MAP claim-2 gap); quiet box, post-soak; DP-2 60 Hz" \
  --gate-out "perf-runner/results/${DATE}-m2claim-full-rth-frame-gate.json" \
  || echo "RTH GATE FAILED (evidence still written)"

# Boring-gate gap: cold start as committed JSON (5 runs, both marks).
OUT="perf-runner/results/${DATE}-m5-cold-start.json"
{
  echo '{'
  echo "  \"gate\": \"cold start x5 (--startup-trace)\","
  echo "  \"git_sha\": \"$(git rev-parse HEAD)\","
  echo "  \"date\": \"$(date -u --iso-8601=seconds)\","
  echo "  \"conditions\": \"quiet box, post-soak, post-RTH-gate\","
  echo '  "runs": ['
  for i in 1 2 3 4 5; do
    LINES=$(./target/release/fft --replay /tmp/esu6-wed-v3-ckpt.fftlog \
      --replay-at 2026-07-29T13:50:00Z --startup-trace 2>&1 | grep startup-trace)
    P=$(echo "$LINES" | grep first_paint | sed 's/.*=//')
    I=$(echo "$LINES" | grep first_interactive | sed 's/.*=//')
    COMMA=$([ "$i" -lt 5 ] && echo "," || echo "")
    echo "    {\"run\": $i, \"first_paint_ms\": $P, \"first_interactive_ms\": $I}$COMMA"
  done
  echo '  ],'
  echo '  "budgets": {"first_paint_ms": 150.0, "first_interactive_ms": 500.0}'
  echo '}'
} > "$OUT"
echo "cold-start evidence: $OUT"
echo "AFTER-SOAK GATES COMPLETE"
