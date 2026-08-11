#!/usr/bin/env bash
# Sourced by after-soak-gates.sh — soak PID bind/wait + JSONL honesty.
# Not executable standalone.

# Resolve relative paths against repo root; absolute paths stay absolute.
resolve_path() {
  local p="$1"
  if [[ "$p" = /* ]]; then
    printf '%s\n' "$p"
  else
    printf '%s\n' "$ROOT/$p"
  fi
}

# Canonical absolute path when the node exists; else resolved absolute string.
# realpath -m normalizes whether or not the final component exists.
canonical_path() {
  realpath -m "$1"
}

# --- /proc helpers (Linux): bind soak pid as tightly as the kernel exposes. ---

# Fields after the comm ") " in /proc/<pid>/stat: 1=state … 20=starttime.
proc_stat_after_comm() {
  local pid="$1"
  local stat
  [[ -r "/proc/${pid}/stat" ]] || return 1
  stat="$(<"/proc/${pid}/stat")" || return 1
  printf '%s\n' "${stat#*) }"
}

proc_state() {
  local after
  after="$(proc_stat_after_comm "$1")" || return 1
  # shellcheck disable=SC2086
  set -- $after
  printf '%s\n' "${1:-}"
}

proc_starttime() {
  local after
  after="$(proc_stat_after_comm "$1")" || return 1
  # shellcheck disable=SC2086
  set -- $after
  printf '%s\n' "${20:-}"
}

proc_is_alive_nonzombie() {
  local pid="$1" state
  [[ -d "/proc/${pid}" ]] || return 1
  state="$(proc_state "$pid")" || return 1
  [[ "$state" != "Z" ]]
}

# True if argv identifies an m7-soak binary (direct or cargo --bin m7-soak).
cmdline_is_m7_soak() {
  local -n _argv=$1
  local a next i
  for i in "${!_argv[@]}"; do
    a="${_argv[$i]}"
    base="$(basename -- "$a")"
    if [[ "$base" == "m7-soak" ]]; then
      return 0
    fi
    if [[ "$a" == "--bin" ]]; then
      next="${_argv[$((i + 1))]:-}"
      if [[ "$next" == "m7-soak" ]]; then
        return 0
      fi
    fi
  done
  return 1
}

# True if --out resolves to the intended soak JSONL (canonical path compare).
cmdline_out_matches_jsonl() {
  local -n _argv=$1
  local want="$2"
  local pid="$3"
  local i out_raw out_canon cwd
  for i in "${!_argv[@]}"; do
    if [[ "${_argv[$i]}" == "--out" ]]; then
      out_raw="${_argv[$((i + 1))]:-}"
      [[ -n "$out_raw" ]] || continue
      # Relative --out is resolved against the process cwd when available.
      if [[ "$out_raw" != /* ]]; then
        if [[ -n "$pid" && -r "/proc/${pid}/cwd" ]]; then
          cwd="$(readlink -f "/proc/${pid}/cwd" 2>/dev/null || true)"
          if [[ -n "$cwd" ]]; then
            out_raw="${cwd}/${out_raw}"
          else
            out_raw="${ROOT}/${out_raw}"
          fi
        else
          out_raw="${ROOT}/${out_raw}"
        fi
      fi
      out_canon="$(canonical_path "$out_raw")"
      if [[ "$out_canon" == "$want" ]]; then
        return 0
      fi
    fi
  done
  return 1
}

# Bind SOAK_PID → m7-soak + intended JSONL. Sets SOAK_STARTTIME when live.
# If the pid is already gone, leave SOAK_STARTTIME empty (JSONL honesty only).
SOAK_STARTTIME=""
bind_soak_pid() {
  local pid="$1"
  local want_jsonl="$2"
  local comm cmdline_raw
  local -a argv=()

  if ! proc_is_alive_nonzombie "$pid"; then
    echo "after-soak-gates: soak pid ${pid} not live (exited or zombie); will require valid terminal PASS JSONL"
    SOAK_STARTTIME=""
    return 0
  fi

  if [[ -r "/proc/${pid}/comm" ]]; then
    comm="$(tr -d '\n' <"/proc/${pid}/comm")"
  else
    comm=""
  fi

  if [[ ! -r "/proc/${pid}/cmdline" ]]; then
    die "cannot read /proc/${pid}/cmdline (pid ${pid})"
  fi
  # mapfile -d '' splits on NUL; last empty field from trailing NUL is dropped below.
  mapfile -d '' -t argv <"/proc/${pid}/cmdline" || die "failed to parse cmdline for pid ${pid}"
  if [[ "${#argv[@]}" -gt 0 && -z "${argv[-1]}" ]]; then
    unset 'argv[-1]'
  fi
  [[ "${#argv[@]}" -gt 0 ]] || die "empty cmdline for pid ${pid}"

  if [[ "$comm" != "m7-soak" ]] && ! cmdline_is_m7_soak argv; then
    cmdline_raw="$(printf '%q ' "${argv[@]}")"
    die "pid ${pid} is not m7-soak (comm=${comm:-?}; cmdline=${cmdline_raw})"
  fi

  if ! cmdline_out_matches_jsonl argv "$want_jsonl" "$pid"; then
    cmdline_raw="$(printf '%q ' "${argv[@]}")"
    die "pid ${pid} m7-soak --out does not match intended JSONL ${want_jsonl} (cmdline=${cmdline_raw})"
  fi

  SOAK_STARTTIME="$(proc_starttime "$pid")" \
    || die "cannot read starttime for pid ${pid}"
  [[ -n "$SOAK_STARTTIME" ]] || die "empty starttime for pid ${pid}"

  echo "after-soak-gates: bound pid ${pid} → m7-soak starttime=${SOAK_STARTTIME} out=${want_jsonl}"
}

# Wait until bound soak exits, becomes zombie, or PID is reused. Finite timeout.
wait_for_soak() {
  local pid="$1"
  local starttime="$2"
  local deadline poll remaining state st

  if [[ -z "$starttime" ]]; then
    echo "after-soak-gates: no live bound soak process; skipping wait"
    return 0
  fi

  deadline=$((SECONDS + SOAK_WAIT_SECS))
  echo "after-soak-gates: waiting for m7-soak (pid ${pid}, starttime ${starttime}, timeout ${SOAK_WAIT_SECS}s)..."

  while true; do
    if ! proc_is_alive_nonzombie "$pid"; then
      echo "after-soak-gates: soak process gone at $(date -u --iso-8601=seconds)"
      return 0
    fi
    st="$(proc_starttime "$pid" 2>/dev/null || true)"
    if [[ -z "$st" || "$st" != "$starttime" ]]; then
      echo "after-soak-gates: pid ${pid} starttime changed or unreadable (reuse/exit) at $(date -u --iso-8601=seconds)"
      return 0
    fi
    state="$(proc_state "$pid" 2>/dev/null || true)"
    if [[ "$state" == "Z" ]]; then
      echo "after-soak-gates: soak pid ${pid} is zombie at $(date -u --iso-8601=seconds); treating as finished"
      return 0
    fi

    remaining=$((deadline - SECONDS))
    if [[ "$remaining" -le 0 ]]; then
      die "soak wait timed out after ${SOAK_WAIT_SECS}s (pid ${pid} still live; starttime ${starttime})"
    fi
    poll=60
    if [[ "$remaining" -lt "$poll" ]]; then
      poll=$remaining
    fi
    sleep "$poll"
  done
}

validate_soak_jsonl() {
  local path="$1"
  python3 - "$path" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
if not path.is_file():
    print(f"after-soak-gates: ERROR: soak JSONL missing: {path}", file=sys.stderr)
    sys.exit(1)

records = []
for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
    line = raw.strip()
    if not line:
        continue
    try:
        obj = json.loads(line)
    except json.JSONDecodeError as e:
        print(
            f"after-soak-gates: ERROR: soak JSONL malformed JSON at line {lineno}: {e}",
            file=sys.stderr,
        )
        sys.exit(1)
    if not isinstance(obj, dict):
        print(
            f"after-soak-gates: ERROR: soak JSONL line {lineno}: expected object",
            file=sys.stderr,
        )
        sys.exit(1)
    records.append((lineno, obj))

if not records:
    print(
        "after-soak-gates: ERROR: soak JSONL has no non-blank records",
        file=sys.stderr,
    )
    sys.exit(1)

summaries = [(n, o) for n, o in records if o.get("kind") == "summary"]
if len(summaries) == 0:
    print(
        "after-soak-gates: ERROR: soak JSONL has no terminal summary "
        "(kill may have skipped it; refuse to infer green)",
        file=sys.stderr,
    )
    sys.exit(1)
if len(summaries) != 1:
    lines = ", ".join(str(n) for n, _ in summaries)
    print(
        f"after-soak-gates: ERROR: soak JSONL must have exactly one summary "
        f"(found {len(summaries)} at lines {lines})",
        file=sys.stderr,
    )
    sys.exit(1)

lineno, summary = summaries[0]
last_lineno, last_obj = records[-1]
if last_obj.get("kind") != "summary" or last_lineno != lineno:
    print(
        f"after-soak-gates: ERROR: soak summary must be the final non-blank JSONL "
        f"record (summary line {lineno}, last non-blank line {last_lineno} "
        f"kind={last_obj.get('kind')!r})",
        file=sys.stderr,
    )
    sys.exit(1)

if "verdict" not in summary:
    print(
        "after-soak-gates: ERROR: soak summary lacks explicit verdict field "
        "(old m7-soak binaries do not emit it; refuse to infer PASS from "
        f"failures/rss counters; summary line {lineno})",
        file=sys.stderr,
    )
    sys.exit(1)

verdict = summary["verdict"]
if verdict != "PASS":
    print(
        f"after-soak-gates: ERROR: soak summary verdict is {verdict!r}, want 'PASS' "
        f"(line {lineno})",
        file=sys.stderr,
    )
    sys.exit(1)

# Cross-check PASS against the counters m7-soak uses to compute verdict.
for field in ("failures", "leak_suspects", "rss_ceiling_fails"):
    if field not in summary:
        print(
            f"after-soak-gates: ERROR: soak summary verdict=PASS but missing {field!r} "
            f"for cross-check (line {lineno})",
            file=sys.stderr,
        )
        sys.exit(1)
    raw = summary[field]
    # type(raw) is int rejects bool (bool subclasses int) and non-integral floats.
    if type(raw) is not int:
        print(
            f"after-soak-gates: ERROR: soak summary {field}={raw!r} is not an "
            f"integer count (line {lineno})",
            file=sys.stderr,
        )
        sys.exit(1)
    if raw != 0:
        print(
            f"after-soak-gates: ERROR: soak summary verdict=PASS but {field}={raw} "
            f"(want 0; line {lineno})",
            file=sys.stderr,
        )
        sys.exit(1)

print(
    f"after-soak-gates: soak summary OK (line {lineno}, final record, "
    f"verdict=PASS, failures=0, leak_suspects=0, rss_ceiling_fails=0)"
)
PY
}
