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

# Writes a report body verbatim, for the malformed-schema cases make_fixture cannot express.
# Args: <threshold> <floors-content> <report-json>
make_raw_fixture() {
  local root="$WORK_DIR/raw-$RANDOM$RANDOM"
  mkdir -p "$root/.tmp"
  printf '%s\n' "$1" >"$root/.coverage-threshold"
  printf '%s\n' "$2" >"$root/.coverage-floors"
  printf '%s' "$3" >"$root/.tmp/coverage.json"
  echo "$root"
}

# Args: <description> <expected-exit> <root> [grep-pattern]
expect() {
  local desc="$1" want="$2" root="$3" pattern="${4:-}"
  local out got=0

  # A fixture that failed to build leaves root empty, and the script would then run against
  # the real repo. That must be a test failure, not a coincidental pass.
  if [[ -z "$root" || ! -f "$root/.tmp/coverage.json" ]]; then
    if [[ "$desc" != *"missing floors file"* ]]; then
      echo "FAIL: $desc — fixture was not built"
      fail_count=$((fail_count + 1))
      return
    fi
  fi

  # Fixtures hold a handful of files, so the real minimum would reject them all. Tests that
  # exercise the minimum itself set MIN_FILES_OVERRIDE.
  out="$(COVERAGE_ROOT_DIR="$root" COVERAGE_MIN_FILES="${MIN_FILES_OVERRIDE:-1}" \
    bash "$UNDER_TEST" "$root/.tmp/coverage.json" 2>&1)" || got=$?

  # "fail" accepts any non-zero exit. Some malformed reports are rejected by jq itself
  # under set -e, and the exact code is jq's business — what matters is that the gate
  # never reports success.
  if [[ "$want" == "fail" ]]; then
    if ((got == 0)); then
      echo "FAIL: $desc — expected a non-zero exit, got 0"
      printf '%s\n' "$out" | sed 's/^/      /'
      fail_count=$((fail_count + 1))
      return
    fi
  elif [[ "$got" != "$want" ]]; then
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
  "$(make_fixture 10 $'* 10\na.rs 10\nb.rs 10\nc.rs 10\ndeleted.rs 90' "a.rs:99" "b.rs:99" "c.rs:99")" \
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
  'is not a number'

# ── Missing files ─────────────────────────────────────────────────────────────────────

missing_root="$(make_fixture 10 $'* 10' "a.rs:99")"
rm "$missing_root/.coverage-floors"
expect "a missing floors file is an error, not a silent pass" 1 "$missing_root" 'not found'

# ── Failing closed on a report the script cannot parse ─────────────────────────────────
# Each of these previously printed "ok per-file: every file meets its floor" and exited 0.
# A gate that inspects nothing must never report that everything passed (#70).

expect "a report with no files key is an error, not a silent pass" fail \
  "$(make_raw_fixture 90 $'* 80' '{"data":[{"totals":{"lines":{"count":100,"covered":95,"percent":95.0}}}]}')"

expect "an empty files array is an error, not a silent pass" fail \
  "$(make_raw_fixture 90 $'* 80' '{"data":[{"files":[],"totals":{"lines":{"count":100,"covered":95,"percent":95.0}}}]}')" \
  'refuses to pass|only 0 file'

expect "a null aggregate percent is an error, not a silent pass" 1 \
  "$(make_raw_fixture 90 $'* 80' '{"data":[{"files":[{"filename":"a.rs","summary":{"lines":{"count":100,"covered":95,"percent":95.0}}}],"totals":{"lines":{"count":null,"covered":null,"percent":null}}}]}')" \
  'is not a number'

expect "a null per-file percent is an error, not a silent pass" 1 \
  "$(make_raw_fixture 10 $'* 80' '{"data":[{"files":[{"filename":"a.rs","summary":{"lines":{"count":100,"covered":null,"percent":null}}}],"totals":{"lines":{"count":100,"covered":50,"percent":50.0}}}]}')" \
  'is not a number'

expect "truncated JSON is an error, not a silent pass" fail \
  "$(make_raw_fixture 90 $'* 80' '{"data":[{"files":[')"

# ── Path mapping ──────────────────────────────────────────────────────────────────────

expect "a report path outside the root is an error, not a fallback to the default floor" 1 \
  "$(make_raw_fixture 10 $'* 80' '{"data":[{"files":[{"filename":"/elsewhere/a.rs","summary":{"lines":{"count":100,"covered":95,"percent":95.0}}}],"totals":{"lines":{"count":100,"covered":95,"percent":95.0}}}]}')" \
  'not under'

expect "most floors matching no file fails, rather than quietly using the default" 1 \
  "$(make_fixture 10 $'* 10\ngone-a.rs 95\ngone-b.rs 95\ngone-c.rs 95' "a.rs:50")" \
  'path mapping is broken'

# ── Minimum file count ────────────────────────────────────────────────────────────────

MIN_FILES_OVERRIDE=5 \
  expect "a report with fewer files than the minimum is rejected" 1 \
  "$(make_fixture 10 $'* 10' "a.rs:99" "b.rs:99")" \
  'expected at least 5'

# ── Floors file without a trailing newline ────────────────────────────────────────────

no_newline_root="$(make_fixture 10 $'* 10' "a.rs:20")"
printf '* 10\na.rs 95' >"$no_newline_root/.coverage-floors"
expect "the last floor still applies when the file has no trailing newline" 1 \
  "$no_newline_root" 'a\.rs'

echo
echo "passed $pass_count, failed $fail_count"
[[ "$fail_count" -eq 0 ]]
