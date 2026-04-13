#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

helm lint "$ROOT/charts/servarr-crds/" --set operatorNamespace=servarr
helm lint "$ROOT/charts/servarr-operator/"
helm dependency build "$ROOT/charts/servarr-operator/"
helm template test "$ROOT/charts/servarr-crds/" --set operatorNamespace=servarr > /dev/null
helm template test "$ROOT/charts/servarr-crds/" --set webhook.enabled=false > /dev/null
helm template test "$ROOT/charts/servarr-operator/" > /dev/null
helm template test "$ROOT/charts/servarr-operator/" --set watchAllNamespaces=true > /dev/null
