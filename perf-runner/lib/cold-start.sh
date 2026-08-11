#!/usr/bin/env bash
# Sourced by after-soak-gates.sh — boring-gate cold-start ×5 JSON evidence.
# Not executable standalone.

run_cold_start_gate() {
  local out_path="$1"
  local tmp_path="${out_path}.tmp.$$"
  local run_dir
  run_dir="$(mktemp -d "${TMPDIR:-/tmp}/fft-cold-start.XXXXXX")"
  register_cleanup "$run_dir"
  register_cleanup "$tmp_path"

  local i=1
  while [[ "$i" -le 5 ]]; do
    local log="${run_dir}/run-${i}.log"
    set +e
    timeout --signal=TERM --kill-after=5s "${COLD_START_TIMEOUT_SECS}s" \
      "$FFT_BIN_PATH" \
      --replay "$REPLAY_LOG_PATH" \
      --replay-at "$REPLAY_AT_COLD" \
      --startup-trace \
      >"$log" 2>&1
    local rc=$?
    set -e
    if [[ "$rc" -eq 124 ]]; then
      echo "after-soak-gates: ERROR: cold-start run $i timed out after ${COLD_START_TIMEOUT_SECS}s; log:" >&2
      cat "$log" >&2 || true
      die "cold-start timeout (run $i)"
    fi
    if [[ "$rc" -ne 0 ]]; then
      echo "after-soak-gates: ERROR: cold-start run $i failed (exit $rc); log:" >&2
      cat "$log" >&2 || true
      die "cold-start process failure"
    fi
    i=$((i + 1))
  done

  local git_sha git_dirty date_iso
  git_sha="$(git rev-parse HEAD)"
  if [[ -n "$(git status --porcelain)" ]]; then
    git_dirty=true
  else
    git_dirty=false
  fi
  date_iso="$(date -u --iso-8601=seconds)"

  python3 - "$out_path" "$tmp_path" "$run_dir" "$git_sha" "$git_dirty" "$date_iso" \
    "$REPLAY_LOG_PATH" "$REPLAY_AT_COLD" "$BUDGET_PAINT_MS" "$BUDGET_INTERACTIVE_MS" <<'PY'
import json
import re
import sys
from pathlib import Path

(
    out_path,
    tmp_path,
    run_dir,
    git_sha,
    git_dirty,
    date_iso,
    replay,
    replay_at,
    budget_paint,
    budget_interactive,
) = sys.argv[1:]

budget_paint = float(budget_paint)
budget_interactive = float(budget_interactive)
paint_re = re.compile(r"startup-trace\s+first_paint_ms=([0-9]+(?:\.[0-9]+)?)")
interactive_re = re.compile(
    r"startup-trace\s+first_interactive_ms=([0-9]+(?:\.[0-9]+)?)"
)

runs = []
for i in range(1, 6):
    text = Path(run_dir, f"run-{i}.log").read_text(encoding="utf-8", errors="replace")
    paints = paint_re.findall(text)
    interactives = interactive_re.findall(text)
    if len(paints) != 1 or len(interactives) != 1:
        print(
            f"after-soak-gates: ERROR: cold-start run {i} missing unique "
            f"startup-trace fields "
            f"(first_paint matches={len(paints)}, "
            f"first_interactive matches={len(interactives)})",
            file=sys.stderr,
        )
        print(text, file=sys.stderr)
        sys.exit(1)
    paint = float(paints[0])
    interactive = float(interactives[0])
    runs.append(
        {
            "run": i,
            "first_paint_ms": paint,
            "first_interactive_ms": interactive,
        }
    )

max_paint = max(r["first_paint_ms"] for r in runs)
max_interactive = max(r["first_interactive_ms"] for r in runs)
# PRD §4 boring gates: painted < 150 ms, interactive < 500 ms (strict).
verdict = (
    "PASS"
    if max_paint < budget_paint and max_interactive < budget_interactive
    else "FAIL"
)

doc = {
    "gate": "cold start x5 (--startup-trace)",
    "date": date_iso,
    "git_sha": git_sha,
    "git_dirty": git_dirty == "true",
    "conditions": "quiet box, post-soak, post-RTH-gate",
    "replay": replay,
    "replay_at": replay_at,
    "runs": runs,
    "budgets": {
        "first_paint_ms": budget_paint,
        "first_interactive_ms": budget_interactive,
    },
    "max": {
        "first_paint_ms": max_paint,
        "first_interactive_ms": max_interactive,
    },
    "verdict": verdict,
}

# Validate the document is JSON-serializable before commit-to-disk.
payload = json.dumps(doc, indent=2, sort_keys=False)
json.loads(payload)  # round-trip sanity
Path(tmp_path).write_text(payload + "\n", encoding="utf-8")
Path(tmp_path).replace(out_path)

print(f"after-soak-gates: cold-start evidence: {out_path} verdict={verdict}")
print(
    f"after-soak-gates: cold-start max paint={max_paint:.3f} ms "
    f"(budget strict < {budget_paint:.3f}), max interactive={max_interactive:.3f} ms "
    f"(budget strict < {budget_interactive:.3f})"
)
if verdict != "PASS":
    sys.exit(1)
PY
}
