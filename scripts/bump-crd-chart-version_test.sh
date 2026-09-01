#!/usr/bin/env bash
#
# Tests for scripts/bump-crd-chart-version.sh.
#
# The script is a cargo-release pre-release-hook: it must never touch the working tree
# during a dry run, must never double-bump a chart version a human already bumped, and
# must fail loud rather than guess when the version field it finds isn't a plain X.Y.Z
# (#781, #782).
#
# Usage: scripts/bump-crd-chart-version_test.sh
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
UNDER_TEST="$SCRIPT_DIR/bump-crd-chart-version.sh"

pass_count=0
fail_count=0

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

# Builds a fixture git repo with one tagged release (v1.0.0, chart version 1.0.0), then
# applies the requested follow-up commits. Args: <chart-version> <bump-templates> <bump-chart>
make_fixture() {
  local chart_version="$1" bump_templates="$2" bump_chart="$3"
  local root="$WORK_DIR/case-$RANDOM$RANDOM"
  mkdir -p "$root/charts/servarr-crds/templates"

  git -C "$root" init -q -b main 2>/dev/null || (mkdir -p "$root" && git -C "$root" init -q)
  git -C "$root" config user.email "test@example.com"
  git -C "$root" config user.name "test"
  git -C "$root" config commit.gpgsign false
  git -C "$root" config tag.gpgsign false

  printf 'apiVersion: v2\nname: servarr-crds\nversion: %s\nappVersion: "1.0.0"\n' "$chart_version" \
    >"$root/charts/servarr-crds/Chart.yaml"
  echo "kind: CustomResourceDefinition" >"$root/charts/servarr-crds/templates/mediastack-crd.yaml"
  git -C "$root" add -A
  git -C "$root" commit -q -m "chore(release): 1.0.0"
  git -C "$root" tag v1.0.0

  if [[ "$bump_templates" == "yes" ]]; then
    echo "  newField: added" >>"$root/charts/servarr-crds/templates/mediastack-crd.yaml"
    git -C "$root" add -A
    git -C "$root" commit -q -m "feat: add new CRD field"
  fi

  if [[ "$bump_chart" == "yes" ]]; then
    sed -i.bak "s/^version: .*/version: 1.0.1/" "$root/charts/servarr-crds/Chart.yaml"
    rm -f "$root/charts/servarr-crds/Chart.yaml.bak"
    git -C "$root" add -A
    git -C "$root" commit -q -m "chore: bump CRD chart to 1.0.1"
  fi

  echo "$root"
}

chart_version_in() {
  grep -m1 '^version: ' "$1/charts/servarr-crds/Chart.yaml" | awk '{print $2}'
}

# Args: <description> <expected-exit> <root> <prev-version> <dry-run> [grep-pattern]
expect() {
  local desc="$1" want="$2" root="$3" prev_version="$4" dry_run="$5" pattern="${6:-}"
  local out got=0

  out="$(WORKSPACE_ROOT="$root" PREV_VERSION="$prev_version" DRY_RUN="$dry_run" \
    bash "$UNDER_TEST" 2>&1)" || got=$?

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

# ── No drift ──────────────────────────────────────────────────────────────────────────

root="$(make_fixture 1.0.0 no no)"
expect "no CRD template changes since prev tag is a no-op" 0 "$root" 1.0.0 false 'no CRD template changes'
[[ "$(chart_version_in "$root")" == "1.0.0" ]] || {
  echo "FAIL: chart version changed on a no-drift run"
  fail_count=$((fail_count + 1))
}

# ── Drift, already bumped by hand ────────────────────────────────────────────────────

root="$(make_fixture 1.0.0 yes yes)"
expect "CRD templates changed but chart was already bumped is a no-op" 0 "$root" 1.0.0 false 'already bumped'
[[ "$(chart_version_in "$root")" == "1.0.1" ]] || {
  echo "FAIL: an already-correct chart version got touched again"
  fail_count=$((fail_count + 1))
}

# ── Drift, not bumped, real run ──────────────────────────────────────────────────────

root="$(make_fixture 1.0.0 yes no)"
expect "unbumped drift is auto-bumped on a real run" 0 "$root" 1.0.0 false 'bumped charts/servarr-crds/Chart.yaml version 1\.0\.0 -> 1\.0\.1'
[[ "$(chart_version_in "$root")" == "1.0.1" ]] || {
  echo "FAIL: expected the chart version to be bumped to 1.0.1, got $(chart_version_in "$root")"
  fail_count=$((fail_count + 1))
}

# ── Drift, not bumped, dry run ───────────────────────────────────────────────────────

root="$(make_fixture 1.0.0 yes no)"
expect "unbumped drift is reported but not written on a dry run" 0 "$root" 1.0.0 true 'dry run: would bump'
[[ "$(chart_version_in "$root")" == "1.0.0" ]] || {
  echo "FAIL: a dry run modified the chart file"
  fail_count=$((fail_count + 1))
}

# ── No prior tag ──────────────────────────────────────────────────────────────────────

root="$(make_fixture 1.0.0 yes no)"
expect "a missing previous release tag is skipped, not an error" 0 "$root" 9.9.9 false 'tag v9\.9\.9 not found'
[[ "$(chart_version_in "$root")" == "1.0.0" ]] || {
  echo "FAIL: chart version changed when the prev tag couldn't be resolved"
  fail_count=$((fail_count + 1))
}

# ── Malformed / missing input ────────────────────────────────────────────────────────

root="$(make_fixture 1.0.0 yes no)"
sed -i.bak "s/^version: .*/version: not-a-version/" "$root/charts/servarr-crds/Chart.yaml"
rm -f "$root/charts/servarr-crds/Chart.yaml.bak"
git -C "$root" add -A
expect "a non-semver chart version fails loud instead of guessing" 1 "$root" 1.0.0 false "not a plain X\.Y\.Z"

root="$(make_fixture 1.0.0 yes no)"
rm -f "$root/charts/servarr-crds/Chart.yaml"
expect "a missing Chart.yaml is an error" 1 "$root" 1.0.0 false "not found"

echo
echo "passed $pass_count, failed $fail_count"
[[ "$fail_count" -eq 0 ]]
