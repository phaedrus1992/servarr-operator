#!/usr/bin/env bash
set -euo pipefail

# Generate CRD YAML from the operator binary and split into per-CRD files
# for the servarr-crds Helm chart.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CRD_CHART_DIR="$REPO_ROOT/charts/servarr-crds/templates"

mkdir -p "$CRD_CHART_DIR"

export TMPDIR_SPLIT
TMPDIR_SPLIT=$(mktemp -d)
TMPFILE=$(mktemp)
trap 'rm -rf "$TMPFILE" "$TMPDIR_SPLIT"' EXIT

# Generate all CRDs
cargo run -p servarr-operator -- crd 2>/dev/null > "$TMPFILE"

# The output contains two CRDs concatenated without --- separators.
# Split on each "apiVersion:" line that starts a new document.
awk '
/^apiVersion:/ { n++ }
n == 1 { print > (ENVIRON["TMPDIR_SPLIT"] "/crd-1.yaml") }
n == 2 { print > (ENVIRON["TMPDIR_SPLIT"] "/crd-2.yaml") }
' "$TMPFILE"

SERVARRAPP_CRD="$CRD_CHART_DIR/servarrapp-crd.yaml"
MEDIASTACK_CRD="$CRD_CHART_DIR/mediastack-crd.yaml"

for f in "$TMPDIR_SPLIT"/crd-*.yaml; do
    [ -s "$f" ] || continue
    name=$(grep -m1 '^  name:' "$f" | awk '{print $2}')
    case "$name" in
        servarrapps.servarr.dev)
            cp -f "$f" "$SERVARRAPP_CRD"
            echo "Generated servarrapp-crd.yaml"
            ;;
        mediastacks.servarr.dev)
            cp -f "$f" "$MEDIASTACK_CRD"
            echo "Generated mediastack-crd.yaml"
            ;;
        *)
            echo "Warning: unknown CRD '$name'" >&2
            ;;
    esac
done

echo "CRD generation complete."
