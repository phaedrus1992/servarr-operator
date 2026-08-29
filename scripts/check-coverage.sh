#!/usr/bin/env bash
#
# Enforces two coverage gates from one cargo-llvm-cov JSON report.
#
#   1. Aggregate  — workspace line coverage must reach the percent in .coverage-threshold.
#   2. Per file   — each file's line coverage must reach its floor in .coverage-floors.
#
# The aggregate gate alone passes when one module sits far below the line and another sits
# near 100%. The per-file gate catches that (#643).
#
# Usage:
#   scripts/check-coverage.sh [coverage.json]
#
# Produce the report first, then run this against it, so CI measures coverage only once:
#   cargo llvm-cov --workspace --json --output-path .tmp/coverage.json
#   scripts/check-coverage.sh .tmp/coverage.json
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="${COVERAGE_ROOT_DIR:-$(cd "$SCRIPT_DIR/.." && pwd)}"

REPORT="${1:-$ROOT_DIR/.tmp/coverage.json}"
THRESHOLD_FILE="$ROOT_DIR/.coverage-threshold"
FLOORS_FILE="$ROOT_DIR/.coverage-floors"

# A file whose coverage exceeds its floor by this many points is reported as ready to
# ratchet. Advisory only — it never fails the build.
RATCHET_MARGIN="${COVERAGE_RATCHET_MARGIN:-5}"

# A report listing fewer files than this means the parse went wrong, not that the workspace
# shrank. The gate refuses to pass on it rather than claiming everything is fine.
MIN_FILES="${COVERAGE_MIN_FILES:-20}"

for f in "$REPORT" "$THRESHOLD_FILE" "$FLOORS_FILE"; do
  if [[ ! -f "$f" ]]; then
    echo "error: $f not found" >&2
    exit 1
  fi
done

# .tmp/ is gitignored and survives across local runs, so the default report can be weeks
# old. Passing a report that predates the code it claims to measure is worse than no run.
if [[ -z "${1:-}" && -d "$ROOT_DIR/crates" ]] &&
  [[ -n "$(find "$ROOT_DIR/crates" -name '*.rs' -newer "$REPORT" -print -quit)" ]]; then
  echo "error: $REPORT is older than the sources it would be measuring" >&2
  echo "       regenerate it: cargo llvm-cov --workspace --json --output-path $REPORT" >&2
  exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required but not on PATH" >&2
  exit 1
fi

# Every number this script acts on comes out of a JSON report whose schema it does not
# control. An unparsed value must stop the run, never fall through to a comparison — awk
# compares two non-numeric strings lexically and would report a pass.
require_number() {
  local label="$1" value="$2"
  if [[ ! "$value" =~ ^[0-9]+(\.[0-9]+)?$ ]]; then
    echo "error: $label is not a number: '$value' — the report schema may have changed" >&2
    exit 1
  fi
}

# ── Load the floors ───────────────────────────────────────────────────────────────────
# Format: "<repo-relative path> <floor percent>", one per line. A "*" path sets the floor
# applied to any file with no entry of its own, so a new untested module fails the gate
# instead of passing unnoticed.

declare -A FLOOR
default_floor=""

# The `|| [[ -n "$path" ]]` keeps the last line when the file has no trailing newline.
# Without it `read` returns non-zero at EOF and that entry is dropped in silence.
while read -r path floor _rest || [[ -n "$path" ]]; do
  [[ -z "$path" || "$path" == \#* ]] && continue
  if [[ ! "$floor" =~ ^[0-9]+(\.[0-9]+)?$ ]]; then
    echo "error: $FLOORS_FILE: '$path' has a non-numeric floor: '$floor'" >&2
    exit 1
  fi
  if [[ "$path" == "*" ]]; then
    default_floor="$floor"
  else
    FLOOR["$path"]="$floor"
  fi
done <"$FLOORS_FILE"

if [[ -z "$default_floor" ]]; then
  echo "error: $FLOORS_FILE has no '*' line setting the default floor" >&2
  exit 1
fi

# ── Aggregate gate ────────────────────────────────────────────────────────────────────

threshold="$(tr -d '[:space:]' <"$THRESHOLD_FILE")"
require_number "$THRESHOLD_FILE" "$threshold"

total_pct="$(jq -r '.data[0].totals.lines.percent' "$REPORT")"
total_covered="$(jq -r '.data[0].totals.lines.covered' "$REPORT")"
total_lines="$(jq -r '.data[0].totals.lines.count' "$REPORT")"
require_number ".data[0].totals.lines.percent" "$total_pct"
require_number ".data[0].totals.lines.count" "$total_lines"

failed=0

if awk -v a="$total_pct" -v b="$threshold" 'BEGIN { exit !(a < b) }'; then
  printf 'FAIL aggregate: %.2f%% is below the %s%% threshold (%s/%s lines)\n' \
    "$total_pct" "$threshold" "$total_covered" "$total_lines" >&2
  failed=1
else
  printf 'ok   aggregate: %.2f%% meets the %s%% threshold (%s/%s lines)\n' \
    "$total_pct" "$threshold" "$total_covered" "$total_lines"
fi

# ── Per-file gate ─────────────────────────────────────────────────────────────────────
# Report every file that is below its floor, not just the first, so one CI run tells the
# whole story.

below=()
ratchet=()
examined=0
declare -A SEEN

# Read jq's output through an assignment, not a process substitution. `set -e` cannot see
# the exit status of a process substitution, so a jq failure there would leave the loop
# with nothing to read and the gate would announce that every file met its floor.
# A jq error fails the assignment, and `set -e` stops the run. An empty result is not an
# error to jq, so the file count below is what catches it.
files_tsv="$(jq -r '.data[0].files[] | [.filename, .summary.lines.percent] | @tsv' "$REPORT")"

while IFS=$'\t' read -r file pct; do
  [[ -z "$file" ]] && continue
  examined=$((examined + 1))
  require_number "line percent for '$file'" "$pct"

  rel="${file#"$ROOT_DIR"/}"
  if [[ "$rel" == /* ]]; then
    echo "error: report path '$file' is not under $ROOT_DIR, so it maps to no floor" >&2
    exit 1
  fi
  SEEN["$rel"]=1

  floor="${FLOOR[$rel]:-$default_floor}"
  if awk -v a="$pct" -v b="$floor" 'BEGIN { exit !(a < b) }'; then
    below+=("$(printf '%-58s %6.2f%%  floor %s%%' "$rel" "$pct" "$floor")")
  elif awk -v a="$pct" -v b="$floor" -v m="$RATCHET_MARGIN" 'BEGIN { exit !(a - b >= m) }'; then
    ratchet+=("$(printf '%-58s %6.2f%%  floor %s%%' "$rel" "$pct" "$floor")")
  fi
done <<<"$files_tsv"

# A gate that inspected nothing must not report that everything passed. This is how a
# coverage gate becomes a no-op and stays one (#70).
if ((examined < MIN_FILES)); then
  echo "error: the report lists only $examined file(s), expected at least $MIN_FILES" >&2
  echo "       the per-file gate refuses to pass on a report it could not parse" >&2
  exit 1
fi

if ((${#below[@]} > 0)); then
  echo >&2
  echo "FAIL per-file: ${#below[@]} file(s) below their floor:" >&2
  printf '  %s\n' "${below[@]}" >&2
  failed=1
else
  echo "ok   per-file: every file meets its floor"
fi

# A floor for a path the report never mentions is stale — usually a deleted or renamed
# file. Worth saying, never worth failing over.
stale=()
for path in "${!FLOOR[@]}"; do
  [[ -z "${SEEN[$path]:-}" ]] && stale+=("$path")
done
if ((${#stale[@]} > 0)); then
  echo
  echo "note: ${#stale[@]} floor entr(ies) match no file in the report:"
  printf '  %s\n' "${stale[@]}" | sort

  # A handful of stale entries means a file was deleted or renamed. Half of them going
  # stale at once means the path mapping broke, and every floor just fell back to the
  # default — a quiet, wholesale loss of the gate.
  if ((${#stale[@]} * 2 >= ${#FLOOR[@]})); then
    echo "error: ${#stale[@]} of ${#FLOOR[@]} floors matched no file — path mapping is broken" >&2
    failed=1
  fi
fi

if ((${#ratchet[@]} > 0)); then
  echo
  echo "note: ${#ratchet[@]} file(s) sit ${RATCHET_MARGIN}+ points above their floor — consider raising it:"
  printf '  %s\n' "${ratchet[@]}" | sort
fi

exit "$failed"
