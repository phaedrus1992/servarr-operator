#!/usr/bin/env bash
#
# Tests for scripts/check-coverage.sh.
#
# The script parses cargo-llvm-cov JSON and decides whether CI passes. A parser that
# breaks quietly turns the whole gate into a no-op, which is the failure #70 already had
# once, so the parser gets tests of its own.
#
# Usage: scripts/check-coverage_test.sh
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
UNDER_TEST="$SCRIPT_DIR/check-coverage.sh"

pass_count=0
fail_count=0

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

# Builds a fake repo root: a coverage.json holding the given files, plus a threshold and a
# floors file. Args: <threshold> <floors-content> <file:pct> [<file:pct>...]
make_fixture() {
  local threshold="$1" floors="$2"
  shift 2

  local root="$WORK_DIR/case-$RANDOM$RANDOM"
  mkdir -p "$root/.tmp"
  printf '%s\n' "$threshold" >"$root/.coverage-threshold"
  printf '%s\n' "$floors" >"$root/.coverage-floors"

  local entries=() covered_total=0 count_total=0
  local spec name pct covered
  for spec in "$@"; do
    name="${spec%%:*}"
    pct="${spec##*:}"
    # 100 lines per file keeps percent and covered-count the same number.
    covered="${pct%.*}"
    entries+=("$(printf '{"filename":"%s/%s","summary":{"lines":{"count":100,"covered":%s,"percent":%s}}}' \
      "$root" "$name" "$covered" "$pct")")
    covered_total=$((covered_total + covered))
    count_total=$((count_total + 100))
  done

  local joined
  joined="$(
    IFS=,
    echo "${entries[*]}"
  )"
  local total_pct
  total_pct="$(awk -v c="$covered_total" -v n="$count_total" 'BEGIN { printf "%.4f", (c * 100) / n }')"

  printf '{"data":[{"files":[%s],"totals":{"lines":{"count":%s,"covered":%s,"percent":%s}}}]}' \
    "$joined" "$count_total" "$covered_total" "$total_pct" >"$root/.tmp/coverage.json"

  echo "$root"
}

# Args: <description> <expected-exit> <root> [grep-pattern]
expect() {
  local desc="$1" want="$2" root="$3" pattern="${4:-}"
  local out got=0
  out="$(COVERAGE_ROOT_DIR="$root" bash "$UNDER_TEST" "$root/.tmp/coverage.json" 2>&1)" || got=$?

  if [[ "$got" != "$want" ]]; then
    echo "FAIL: $desc — expected exit $want, got $got"
    printf '%s\n' "$out" | sed 's/^/      /'
    fail_count=$((fail_count + 1))
    return
  fi
  if [[ -n "$pattern" ]] && ! grep -qE "$pattern" <<<"$out"; then
    echo "FAIL: $desc — output did not match /$pattern/"
    printf '%s\n' "$out" | sed 's/^/      /'
    fail_count=$((fail_count + 1))
    return
  fi
  echo "ok: $desc"
  pass_count=$((pass_count + 1))
}

# ── Aggregate gate ────────────────────────────────────────────────────────────────────

expect "aggregate above threshold passes" 0 \
  "$(make_fixture 90 $'* 50\na.rs 90' "a.rs:95" "b.rs:95")" \
  'ok +aggregate'

expect "aggregate below threshold fails" 1 \
  "$(make_fixture 90 $'* 50' "a.rs:85" "b.rs:85")" \
  'FAIL aggregate'

expect "aggregate exactly at threshold passes" 0 \
  "$(make_fixture 90 $'* 50' "a.rs:90" "b.rs:90")" \
  'ok +aggregate'

# ── Per-file gate ─────────────────────────────────────────────────────────────────────

expect "a file below its own floor fails even when the aggregate passes" 1 \
  "$(make_fixture 50 $'* 50\nlow.rs 80' "low.rs:60" "high.rs:100")" \
  'FAIL per-file'

expect "every failing file is named, not just the first" 1 \
  "$(make_fixture 10 $'* 90' "a.rs:20" "b.rs:20" "c.rs:20")" \
  '3 file\(s\) below their floor'

expect "a file exactly at its floor passes" 0 \
  "$(make_fixture 50 $'* 50\na.rs 75' "a.rs:75" "b.rs:99")" \
  'ok +per-file'

# ── Default floor ─────────────────────────────────────────────────────────────────────

expect "a file with no entry falls back to the default floor and can fail" 1 \
  "$(make_fixture 10 $'* 80\nknown.rs 10' "known.rs:15" "brand-new.rs:20")" \
  'brand-new\.rs'

expect "a file with no entry passes when it clears the default floor" 0 \
  "$(make_fixture 10 $'* 80\nknown.rs 10' "known.rs:15" "brand-new.rs:95")" \
  'ok +per-file'

# ── Ratchet and stale advisories ──────────────────────────────────────────────────────

expect "a file well above its floor is reported as ready to ratchet" 0 \
  "$(make_fixture 10 $'* 10\na.rs 50' "a.rs:99")" \
  'consider raising it'

expect "a floor entry matching no file is reported as stale" 0 \
  "$(make_fixture 10 $'* 10\ndeleted.rs 90' "a.rs:99")" \
  'match no file'

# ── Malformed input ───────────────────────────────────────────────────────────────────

expect "a floors file with no default entry is rejected" 1 \
  "$(make_fixture 10 $'a.rs 50' "a.rs:99")" \
  "no '\\*' line"

expect "a non-numeric floor is rejected" 1 \
  "$(make_fixture 10 $'* 50\na.rs abc' "a.rs:99")" \
  'non-numeric floor'

expect "a non-numeric threshold is rejected" 1 \
  "$(make_fixture "not-a-number" $'* 50' "a.rs:99")" \
  'does not contain a number'

# ── Missing files ─────────────────────────────────────────────────────────────────────

missing_root="$(make_fixture 10 $'* 10' "a.rs:99")"
rm "$missing_root/.coverage-floors"
expect "a missing floors file is an error, not a silent pass" 1 "$missing_root" 'not found'

echo
echo "passed $pass_count, failed $fail_count"
[[ "$fail_count" -eq 0 ]]
