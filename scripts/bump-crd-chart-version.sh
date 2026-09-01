#!/usr/bin/env bash
#
# cargo-release pre-release-hook: auto-bumps charts/servarr-crds/Chart.yaml's `version:`
# field when the generated CRD templates changed since the last release but nobody bumped
# the chart version by hand. Fixes the drift that shipped 1.4.0 with a stale CRD chart
# version (#781, #782) from recurring on every future release.
#
# cargo-release always runs a pre-release-hook, even on a dry-run preview (no --execute) --
# it sets DRY_RUN=true/false and leaves it to the hook to decide what to actually write.
# See src/steps/hook.rs in crate-ci/cargo-release: "we use dry_run environmental variable
# to run the script so here we set dry_run=false and always execute the command."
#
# The hook runs after pre-release-replacements and before the release commit, so any file
# it modifies here (a tracked file) lands in that same `git commit -am` cargo-release makes.
#
# Env vars set by cargo-release (docs/reference.md "pre-release-hook" in that repo):
#   PREV_VERSION    the crate version before this bump, e.g. "1.3.1"
#   DRY_RUN         "true" during a preview run, "false" with --execute
#   WORKSPACE_ROOT  path to the workspace root
#
# Usage (manual/testing): WORKSPACE_ROOT=<repo> PREV_VERSION=<x.y.z> DRY_RUN=<true|false> \
#   scripts/bump-crd-chart-version.sh
#
set -euo pipefail

WORKSPACE_ROOT="${WORKSPACE_ROOT:-$(git rev-parse --show-toplevel)}"
PREV_VERSION="${PREV_VERSION:?PREV_VERSION must be set (cargo-release sets this)}"
DRY_RUN="${DRY_RUN:-false}"

CHART_FILE="$WORKSPACE_ROOT/charts/servarr-crds/Chart.yaml"
CRD_TEMPLATES_DIR="charts/servarr-crds/templates"
PREV_TAG="v$PREV_VERSION"

if [[ ! -f "$CHART_FILE" ]]; then
  echo "error: $CHART_FILE not found" >&2
  exit 1
fi

if ! git -C "$WORKSPACE_ROOT" rev-parse -q --verify "refs/tags/$PREV_TAG" >/dev/null; then
  echo "warn: tag $PREV_TAG not found, skipping CRD chart version drift check" >&2
  exit 0
fi

crd_changed="$(git -C "$WORKSPACE_ROOT" diff --name-only "$PREV_TAG" HEAD -- "$CRD_TEMPLATES_DIR")"
if [[ -z "$crd_changed" ]]; then
  echo "ok: no CRD template changes since $PREV_TAG, servarr-crds chart version untouched"
  exit 0
fi

chart_changed="$(git -C "$WORKSPACE_ROOT" diff --name-only "$PREV_TAG" HEAD -- "charts/servarr-crds/Chart.yaml")"
if [[ -n "$chart_changed" ]]; then
  echo "ok: CRD templates changed since $PREV_TAG, and charts/servarr-crds/Chart.yaml was already bumped"
  exit 0
fi

current_version="$(grep -m1 '^version: ' "$CHART_FILE" | awk '{print $2}')"
if [[ ! "$current_version" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
  echo "error: $CHART_FILE version '$current_version' is not a plain X.Y.Z — refusing to guess a bump" >&2
  exit 1
fi
major="${BASH_REMATCH[1]}"
minor="${BASH_REMATCH[2]}"
patch="${BASH_REMATCH[3]}"
new_version="$major.$minor.$((patch + 1))"

if [[ "$DRY_RUN" == "true" ]]; then
  echo "dry run: would bump charts/servarr-crds/Chart.yaml version $current_version -> $new_version" \
    "(CRD templates changed since $PREV_TAG with no matching bump)"
  exit 0
fi

sed -i.bak "s/^version: .*/version: $new_version/" "$CHART_FILE"
rm -f "$CHART_FILE.bak"
echo "bumped charts/servarr-crds/Chart.yaml version $current_version -> $new_version" \
  "(CRD templates changed since $PREV_TAG with no matching bump)"
