#!/usr/bin/env bash
# Gap-closing measured runs chained behind the 24 h soak.
# ACCEPTANCE-MAP gaps: claim 2 (full RTH session frame gate) + boring-gate cold-start JSON.
#
# Honesty rules (POST-SOAK-GATE-HONESTY):
#   - Soak JSONL must exist; exactly one summary; it must be the final non-blank record;
#     verdict "PASS" with failures/leak_suspects/rss_ceiling_fails all zero.
#     Missing/malformed/FAIL/inferred-green ⇒ fail loud.
#   - Live mode binds <soak-pid> to an m7-soak process + intended --out JSONL via /proc
#     (comm/cmdline/starttime); zombies and PID-reuse end the wait; wait is finite.
#   - Full-RTH `--gate` exit is never swallowed.
#   - Cold-start ×5 under a finite per-run timeout; writes valid JSON under
#     perf-runner/results (samples, budgets, max, PASS/FAIL). Missing trace fields,
#     process failure/timeout, or budget breach (strict <) ⇒ nonzero.
#   - Does not launch the soak itself. `--dry-run` never launches the 6.5 h gate.
#
# Helpers (sourced; keep each ≤ ~500 lines):
#   perf-runner/lib/soak-validate.sh  — path helpers, /proc bind/wait, JSONL honesty
#   perf-runner/lib/cold-start.sh     — cold-start ×5 gate + JSON evidence
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage() {
  cat <<'EOF'
usage: after-soak-gates.sh <soak-pid>
       after-soak-gates.sh --dry-run

Environment (optional; defaults shown):
  FFT_SOAK_JSONL=/tmp/m7-soak-24h.jsonl
  FFT_REPLAY_LOG=/tmp/esu6-wed-v3-ckpt.fftlog
  FFT_BIN=./target/release/fft
  FFT_RESULTS_DIR=perf-runner/results
  FFT_GATE_SECS=23400
  FFT_REPLAY_AT_RTH=2026-07-29T13:30:00Z
  FFT_REPLAY_AT_COLD=2026-07-29T13:50:00Z
  FFT_COOLDOWN_SECS=120
  FFT_BUDGET_PAINT_MS=150
  FFT_BUDGET_INTERACTIVE_MS=500
  FFT_SOAK_WAIT_SECS=90000
  FFT_COLD_START_TIMEOUT_SECS=60
EOF
}

die() {
  echo "after-soak-gates: ERROR: $*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

# Temp paths registered for unconditional EXIT cleanup (die / set -e / normal).
CLEANUP_PATHS=()
register_cleanup() {
  CLEANUP_PATHS+=("$1")
}
cleanup_all() {
  local p
  for p in "${CLEANUP_PATHS[@]+"${CLEANUP_PATHS[@]}"}"; do
    rm -rf -- "$p" 2>/dev/null || true
  done
}
trap cleanup_all EXIT

DRY_RUN=0
SOAK_PID=""
case "${1:-}" in
  ""|-h|--help)
    usage
    exit 2
    ;;
  --dry-run)
    DRY_RUN=1
    ;;
  *)
    [[ "$1" =~ ^[0-9]+$ ]] || die "soak-pid must be an integer (got: $1)"
    SOAK_PID="$1"
    ;;
esac
if [[ "$#" -ne 1 ]]; then
  usage
  exit 2
fi

SOAK_JSONL="${FFT_SOAK_JSONL:-/tmp/m7-soak-24h.jsonl}"
REPLAY_LOG="${FFT_REPLAY_LOG:-/tmp/esu6-wed-v3-ckpt.fftlog}"
FFT_BIN="${FFT_BIN:-./target/release/fft}"
RESULTS_DIR="${FFT_RESULTS_DIR:-perf-runner/results}"
GATE_SECS="${FFT_GATE_SECS:-23400}"
REPLAY_AT_RTH="${FFT_REPLAY_AT_RTH:-2026-07-29T13:30:00Z}"
REPLAY_AT_COLD="${FFT_REPLAY_AT_COLD:-2026-07-29T13:50:00Z}"
COOLDOWN_SECS="${FFT_COOLDOWN_SECS:-120}"
BUDGET_PAINT_MS="${FFT_BUDGET_PAINT_MS:-150}"
BUDGET_INTERACTIVE_MS="${FFT_BUDGET_INTERACTIVE_MS:-500}"
SOAK_WAIT_SECS="${FFT_SOAK_WAIT_SECS:-90000}"
COLD_START_TIMEOUT_SECS="${FFT_COLD_START_TIMEOUT_SECS:-60}"

need_cmd python3
need_cmd git
need_cmd date
need_cmd timeout
need_cmd realpath

[[ "$GATE_SECS" =~ ^[0-9]+$ ]] || die "FFT_GATE_SECS must be a non-negative integer"
[[ "$COOLDOWN_SECS" =~ ^[0-9]+$ ]] || die "FFT_COOLDOWN_SECS must be a non-negative integer"
[[ "$SOAK_WAIT_SECS" =~ ^[0-9]+$ && "$SOAK_WAIT_SECS" -gt 0 ]] \
  || die "FFT_SOAK_WAIT_SECS must be a positive integer"
[[ "$COLD_START_TIMEOUT_SECS" =~ ^[0-9]+$ && "$COLD_START_TIMEOUT_SECS" -gt 0 ]] \
  || die "FFT_COLD_START_TIMEOUT_SECS must be a positive integer"
python3 - "$BUDGET_PAINT_MS" "$BUDGET_INTERACTIVE_MS" <<'PY' || die "budget envs must be finite numbers"
import sys
for label, raw in (("FFT_BUDGET_PAINT_MS", sys.argv[1]), ("FFT_BUDGET_INTERACTIVE_MS", sys.argv[2])):
    try:
        v = float(raw)
    except ValueError as e:
        raise SystemExit(f"{label}: {e}") from e
    if not (v == v and v != float("inf") and v >= 0):
        raise SystemExit(f"{label}: must be finite and >= 0 (got {raw!r})")
PY

# shellcheck source=lib/soak-validate.sh
# shellcheck source=lib/cold-start.sh
source "$ROOT/perf-runner/lib/soak-validate.sh"
source "$ROOT/perf-runner/lib/cold-start.sh"

FFT_BIN_PATH="$(resolve_path "$FFT_BIN")"
REPLAY_LOG_PATH="$(resolve_path "$REPLAY_LOG")"
RESULTS_DIR_PATH="$(resolve_path "$RESULTS_DIR")"
SOAK_JSONL_PATH="$(resolve_path "$SOAK_JSONL")"
SOAK_JSONL_CANON="$(canonical_path "$SOAK_JSONL_PATH")"

[[ -x "$FFT_BIN_PATH" ]] || die "fft binary missing or not executable: $FFT_BIN_PATH"
[[ -f "$REPLAY_LOG_PATH" ]] || die "replay log missing: $REPLAY_LOG_PATH"
[[ -d "$RESULTS_DIR_PATH" ]] || die "results dir missing: $RESULTS_DIR_PATH"

DATE_UTC="$(date -u +%Y-%m-%d)"
RTH_OUT="${RESULTS_DIR_PATH}/${DATE_UTC}-m2claim-full-rth-frame-gate.json"
COLD_OUT="${RESULTS_DIR_PATH}/${DATE_UTC}-m5-cold-start.json"

echo "after-soak-gates: repo=$ROOT"
echo "after-soak-gates: soak_jsonl=$SOAK_JSONL_PATH"
echo "after-soak-gates: replay=$REPLAY_LOG_PATH"
echo "after-soak-gates: fft=$FFT_BIN_PATH"
echo "after-soak-gates: results=$RESULTS_DIR_PATH"
echo "after-soak-gates: dry_run=$DRY_RUN"
echo "after-soak-gates: soak_wait_secs=$SOAK_WAIT_SECS"
echo "after-soak-gates: cold_start_timeout_secs=$COLD_START_TIMEOUT_SECS"

if [[ "$DRY_RUN" -eq 1 ]]; then
  echo "after-soak-gates: DRY-RUN — validating soak JSONL + inputs; skipping wait/RTH/cold-start"
  validate_soak_jsonl "$SOAK_JSONL_PATH"
  echo "after-soak-gates: DRY-RUN OK (would write RTH → $RTH_OUT ; cold-start → $COLD_OUT)"
  echo "after-soak-gates: DRY-RUN complete (6.5h gate NOT launched)"
  exit 0
fi

bind_soak_pid "$SOAK_PID" "$SOAK_JSONL_CANON"
wait_for_soak "$SOAK_PID" "$SOAK_STARTTIME"
echo "after-soak-gates: validating JSONL before cooldown"
validate_soak_jsonl "$SOAK_JSONL_PATH"

echo "after-soak-gates: cooling ${COOLDOWN_SECS} s"
sleep "${COOLDOWN_SECS}"

# Claim 2 letter: full RTH session, zero missed deadlines, zero drops.
# RTH open Wed 2026-07-29 08:30 CT = 13:30 UTC; 6.5 h = 23400 s.
# Do NOT swallow the exit code — a red gate is a red release.
echo "after-soak-gates: launching full-RTH frame gate (${GATE_SECS}s) → $RTH_OUT"
"$FFT_BIN_PATH" --gate "$GATE_SECS" \
  --replay "$REPLAY_LOG_PATH" \
  --replay-at "$REPLAY_AT_RTH" \
  --conditions "full RTH session gate (ACCEPTANCE-MAP claim-2 gap); quiet box, post-soak; DP-2 60 Hz" \
  --gate-out "$RTH_OUT"

# Boring-gate gap: cold start as committed JSON (5 runs, both marks, max + verdict).
run_cold_start_gate "$COLD_OUT"

echo "after-soak-gates: AFTER-SOAK GATES COMPLETE"
