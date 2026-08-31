#!/usr/bin/env bash
# Run the full smoke test against a local Kubernetes cluster.
#
# Prerequisites:
#   - kubectl configured and pointing at a reachable cluster
#   - docker (builds the operator image via the repo's own Dockerfile)
#   - helm
#
# The script creates a dedicated namespace (default: smoke-<timestamp>), runs
# all smoke tests inside it, then deletes the namespace on exit.
#
# Supported local cluster types for image loading:
#   - Docker Desktop    (image already visible to cluster via shared daemon)
#   - kind              (kind load docker-image)
#   - k3d               (k3d image import)
#   - rancher-desktop   (nerdctl load or docker-compatible daemon)
#
# Usage:
#   scripts/smoke-test-local.sh [--namespace NAME] [--keep]
#
#   --namespace NAME   Use a fixed namespace name instead of the timestamped default
#   --keep             Do not delete the namespace on exit (useful for debugging)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
IMAGE_NAME="servarr-operator"
IMAGE_TAG="smoke-local"

NAMESPACE="smoke-$(date +%s)"
KEEP_NS=false

while [[ $# -gt 0 ]]; do
  case "$1" in
  --namespace)
    NAMESPACE="$2"
    shift 2
    ;;
  --keep)
    KEEP_NS=true
    shift
    ;;
  *)
    echo "Unknown argument: $1" >&2
    exit 1
    ;;
  esac
done

# A typo in --namespace must never target a namespace this script cannot safely delete.
case "$NAMESPACE" in
default | kube-system | kube-public | kube-node-lease)
  echo "ERROR: --namespace '$NAMESPACE' is a reserved Kubernetes namespace." >&2
  echo "  This script creates and deletes its own namespace. It refuses to run against" >&2
  echo "  a namespace that must never be deleted." >&2
  exit 1
  ;;
esac

# ---------------------------------------------------------------------------
# Preflight: cluster must be reachable
# ---------------------------------------------------------------------------
echo "Checking cluster connectivity..."
if ! kubectl cluster-info --request-timeout=5s &>/dev/null; then
  echo "ERROR: No Kubernetes cluster is reachable."
  echo "  Start Docker Desktop, kind, k3d, or another local cluster and try again."
  exit 1
fi
echo "  Cluster OK: $(kubectl config current-context)"

# ---------------------------------------------------------------------------
# Detect cluster type for image loading
# ---------------------------------------------------------------------------
detect_cluster_type() {
  local ctx
  ctx=$(kubectl config current-context 2>/dev/null || echo "")
  # Match the context name only. A binary-presence fallback (kind/k3d installed
  # and has some cluster, regardless of which one is current) would let this
  # script run against a cluster its own context name never claimed to be local.
  case "$ctx" in
  kind-*) echo "kind" ;;
  k3d-*) echo "k3d" ;;
  rancher-desktop) echo "rancher-desktop" ;;
  docker-desktop) echo "docker-desktop" ;;
  *) echo "unknown" ;;
  esac
}

CLUSTER_TYPE=$(detect_cluster_type)
echo "  Cluster type: ${CLUSTER_TYPE}"

# This script creates and deletes namespaces. It must never do that on a cluster it
# cannot identify as a local one — that cluster could be shared or production.
case "$CLUSTER_TYPE" in
kind | k3d | docker-desktop | rancher-desktop) ;;
*)
  echo "ERROR: Unrecognized cluster type for context '$(kubectl config current-context)'."
  echo "  This script supports local clusters only: kind, k3d, docker-desktop, rancher-desktop."
  echo "  It will not create or delete namespaces on an unrecognized cluster."
  exit 1
  ;;
esac

# ---------------------------------------------------------------------------
# Build Docker image
# ---------------------------------------------------------------------------
# Built from the real Dockerfile (cargo-chef, multi-stage), not a host-built
# binary copied into a throwaway Dockerfile. A host build targets the host's
# own OS/arch (e.g. macOS Mach-O on Apple Silicon), which the container
# can never run ("exec format error") regardless of CPU architecture — only
# a build that compiles *inside* a Linux container is guaranteed to produce
# a binary the image can actually execute, on every host OS.
echo ""
echo "Building operator Docker image (${IMAGE_NAME}:${IMAGE_TAG})..."
docker build -t "${IMAGE_NAME}:${IMAGE_TAG}" -f "$REPO_ROOT/Dockerfile" "$REPO_ROOT"

# ---------------------------------------------------------------------------
# Load image into the cluster
# ---------------------------------------------------------------------------
echo ""
echo "Loading image into cluster (${CLUSTER_TYPE})..."
case "$CLUSTER_TYPE" in
kind)
  CLUSTER_NAME=$(kubectl config current-context | sed 's/^kind-//')
  kind load docker-image "${IMAGE_NAME}:${IMAGE_TAG}" --name "$CLUSTER_NAME"
  ;;
k3d)
  CLUSTER_NAME=$(kubectl config current-context | sed 's/^k3d-//')
  k3d image import "${IMAGE_NAME}:${IMAGE_TAG}" --cluster "$CLUSTER_NAME"
  ;;
docker-desktop | rancher-desktop)
  # Docker Desktop and Rancher Desktop share the daemon with the cluster —
  # the image is already visible.
  echo "  Assuming image is visible to cluster via shared container daemon."
  ;;
esac

# ---------------------------------------------------------------------------
# Create namespace and register cleanup
# ---------------------------------------------------------------------------
# Saved before this script ever touches the developer's kubectl context, so cleanup can
# restore it even if a later step fails partway through setup. --minify leaves exactly
# one context, so the exact field path is safe — {..namespace} would also match a
# namespace key nested anywhere else in the config (e.g. under extensions).
if ! PRIOR_NAMESPACE="$(kubectl config view --minify --output 'jsonpath={.contexts[0].context.namespace}')"; then
  echo "WARNING: could not read the current kubectl namespace. Will restore to 'default' on exit." >&2
  PRIOR_NAMESPACE=""
fi

cleanup() {
  if [[ "$KEEP_NS" == "true" ]]; then
    echo ""
    echo "Namespace '${NAMESPACE}' retained for debugging (--keep was set)."
  else
    echo ""
    echo "Cleaning up namespace '${NAMESPACE}'..."
    if ! kubectl delete namespace "$NAMESPACE" --ignore-not-found --timeout=60s; then
      echo "WARNING: failed to delete namespace '${NAMESPACE}'. Delete it by hand:" >&2
      echo "  kubectl delete namespace ${NAMESPACE}" >&2
    fi
  fi
  # Always restore the developer's own namespace, kept or not — otherwise their default
  # kubectl context is left pointing at this script's namespace instead of their own.
  echo "Restoring kubectl namespace to '${PRIOR_NAMESPACE:-default}'..."
  if ! kubectl config set-context --current --namespace="$PRIOR_NAMESPACE" >/dev/null; then
    echo "WARNING: failed to restore the kubectl namespace. Restore it by hand:" >&2
    echo "  kubectl config set-context --current --namespace=${PRIOR_NAMESPACE:-default}" >&2
  fi
}
trap cleanup EXIT

echo ""
echo "Creating namespace '${NAMESPACE}'..."
kubectl create namespace "$NAMESPACE"
kubectl config set-context --current --namespace="$NAMESPACE"

# ---------------------------------------------------------------------------
# Generate CRDs and install
# ---------------------------------------------------------------------------
echo ""
echo "Installing CRDs..."
helm template smoke-crds "$REPO_ROOT/charts/servarr-crds/" \
  --set webhook.enabled=false |
  kubectl apply -f -

# ---------------------------------------------------------------------------
# Install operator
# ---------------------------------------------------------------------------
echo ""
echo "Installing operator..."
helm dependency build "$REPO_ROOT/charts/servarr-operator/" &>/dev/null
helm template smoke "$REPO_ROOT/charts/servarr-operator/" \
  --set image.repository="${IMAGE_NAME}" \
  --set image.tag="${IMAGE_TAG}" \
  --set image.pullPolicy=Never \
  --set webhook.enabled=false \
  --set watchAllNamespaces=false \
  --namespace "$NAMESPACE" |
  kubectl apply -f -

echo "Waiting for operator rollout..."
kubectl rollout status deployment/servarr-operator --timeout=120s

# ---------------------------------------------------------------------------
# Apply smoke manifests
# ---------------------------------------------------------------------------
echo ""
echo "Applying smoke test manifests..."
kubectl apply -f "$REPO_ROOT/.github/smoke-test/manifests/"

# ---------------------------------------------------------------------------
# Run smoke tests (reuse the shared script)
# ---------------------------------------------------------------------------
echo ""
bash "$REPO_ROOT/.github/smoke-test/smoke-test.sh"
