#!/usr/bin/env bash
# Materialize the ESU6 sample week into the durable gate fixture root.
# Expected counts are a defect check — any deviation exits nonzero.
#
# Durable paths (IMPLEMENTATION-PLAN 2026-08-12 evening freeze):
#   ~/.cache/fft/gates/ESU6-YYYY-MM-DD.fftlog
#   ~/.cache/fft/gates/ESU6-YYYY-MM-DD-ckpt.fftlog
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

GATES="${FFT_GATES_DIR:-$HOME/.cache/fft/gates}"
DBN_DIR="${FFT_DBN_DIR:-$ROOT/data/GLBX-20260803-4WJS899FNL}"
INGEST="${FFT_INGEST_BIN:-$ROOT/target/release/fft-ingest}"
CHECKPOINT="${FFT_CHECKPOINT_BIN:-$ROOT/target/release/fft-checkpoint}"

die() {
  echo "regen-week-fixtures: ERROR: $*" >&2
  exit 1
}

# date  expected_events  expected_bytes  expected_ckpts
DAYS=(
  "2026-07-27 16050064 151671425 1391"
  "2026-07-28 14054511 134377944 1392"
  "2026-07-29 21401139 202632683 1393"
  "2026-07-30 16595979 158098435 1393"
  "2026-07-31 17152053 160034446 1377"
)

mkdir -p "$GATES"
[[ -d "$DBN_DIR" ]] || die "DBN dir missing: $DBN_DIR"

if [[ ! -x "$INGEST" || ! -x "$CHECKPOINT" ]]; then
  echo "regen-week-fixtures: building release fft-ingest + fft-checkpoint"
  cargo build --release -p fft-ingest -p fft-engine --bin fft-checkpoint
fi
[[ -x "$INGEST" ]] || die "fft-ingest missing: $INGEST"
[[ -x "$CHECKPOINT" ]] || die "fft-checkpoint missing: $CHECKPOINT"

shopt -s nullglob
dbn_files=("$DBN_DIR"/*.mbo.dbn.zst)
shopt -u nullglob
[[ "${#dbn_files[@]}" -gt 0 ]] || die "no *.mbo.dbn.zst in $DBN_DIR"

for spec in "${DAYS[@]}"; do
  # shellcheck disable=SC2086
  set -- $spec
  date="$1"
  want_events="$2"
  want_bytes="$3"
  want_ckpts="$4"
  raw="$GATES/ESU6-${date}.fftlog"
  ckpt="$GATES/ESU6-${date}-ckpt.fftlog"

  if [[ -f "$raw" ]]; then
    have_bytes="$(stat -c '%s' "$raw")"
    if [[ "$have_bytes" -eq "$want_bytes" ]]; then
      echo "regen-week-fixtures: skip ingest $date (bytes=$have_bytes)"
    else
      echo "regen-week-fixtures: re-ingest $date (bytes $have_bytes != $want_bytes)"
      rm -f -- "$raw" "$ckpt"
    fi
  fi

  if [[ ! -f "$raw" ]]; then
    echo "regen-week-fixtures: ingest $date -> $raw"
    out="$("$INGEST" write "$raw" "${dbn_files[@]}" \
      --trade-date "$date" \
      --tick 250000000 --uom-qty 50000000000 --display-factor 1)"
    printf '%s\n' "$out"
    events="$(printf '%s\n' "$out" | sed -n 's/^wrote \([0-9][0-9]*\) events.*/\1/p')"
    gaps="$(printf '%s\n' "$out" | sed -n 's/.* \([0-9][0-9]*\) gaps,.*/\1/p')"
    [[ -n "$events" ]] || die "could not parse ingest event count for $date"
    [[ "$events" == "$want_events" ]] || die "$date events $events != $want_events"
    [[ "$gaps" == "0" ]] || die "$date gaps_kept=$gaps (want 0)"
    have_bytes="$(stat -c '%s' "$raw")"
    [[ "$have_bytes" -eq "$want_bytes" ]] || die "$date bytes $have_bytes != $want_bytes"
  fi

  if [[ -f "$ckpt" ]]; then
    echo "regen-week-fixtures: skip checkpoint $date (exists: $ckpt)"
    continue
  fi

  echo "regen-week-fixtures: checkpoint $date -> $ckpt"
  out="$("$CHECKPOINT" "$raw" "$ckpt")"
  printf '%s\n' "$out"
  events="$(printf '%s\n' "$out" | sed -n 's/.*: events \([0-9][0-9]*\) checkpoints.*/\1/p')"
  ckpts="$(printf '%s\n' "$out" | sed -n 's/.* checkpoints \([0-9][0-9]*\) src_bytes.*/\1/p')"
  [[ "$events" == "$want_events" ]] || die "$date ckpt events $events != $want_events"
  [[ "$ckpts" == "$want_ckpts" ]] || die "$date checkpoints $ckpts != $want_ckpts"
done

echo "regen-week-fixtures: OK -> $GATES"
ls -lh "$GATES"/ESU6-2026-07-2*.fftlog
