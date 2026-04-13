#!/usr/bin/env bash
# Run the full smoke test against a local Kubernetes cluster.
#
# Prerequisites:
#   - kubectl configured and pointing at a reachable cluster
#   - docker (to build the operator image)
#   - helm
#   - cargo (to build the operator binary)
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
    --namespace) NAMESPACE="$2"; shift 2 ;;
    --keep)      KEEP_NS=true; shift ;;
    *) echo "Unknown argument: $1" >&2; exit 1 ;;
  esac
done

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
  case "$ctx" in
    kind-*)               echo "kind" ;;
    k3d-*)                echo "k3d" ;;
    rancher-desktop)      echo "rancher-desktop" ;;
    docker-desktop)       echo "docker-desktop" ;;
    *)
      # Fallback: check if kind/k3d binaries exist and have matching clusters
      if command -v kind &>/dev/null && kind get clusters 2>/dev/null | grep -q .; then
        echo "kind"
      elif command -v k3d &>/dev/null && k3d cluster list 2>/dev/null | grep -q .; then
        echo "k3d"
      else
        echo "unknown"
      fi
      ;;
  esac
}

CLUSTER_TYPE=$(detect_cluster_type)
echo "  Cluster type: ${CLUSTER_TYPE}"

# ---------------------------------------------------------------------------
# Build operator binary (native, not musl — just needs to run locally)
# ---------------------------------------------------------------------------
echo ""
echo "Building operator binary..."
cd "$REPO_ROOT"
cargo build --release --bin servarr-operator
BINARY="$REPO_ROOT/target/release/servarr-operator"

# ---------------------------------------------------------------------------
# Build Docker image
# ---------------------------------------------------------------------------
echo ""
echo "Building operator Docker image (${IMAGE_NAME}:${IMAGE_TAG})..."
docker build \
  -t "${IMAGE_NAME}:${IMAGE_TAG}" \
  --build-arg BINARY_PATH="target/release/servarr-operator" \
  -f- "$REPO_ROOT" <<'DOCKERFILE'
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && apt-get clean && rm -rf /var/lib/apt/lists/*
COPY target/release/servarr-operator /servarr-operator
USER nobody:nogroup
ENTRYPOINT ["/servarr-operator"]
DOCKERFILE

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
  docker-desktop|rancher-desktop|unknown)
    # Docker Desktop and Rancher Desktop share the daemon with the cluster —
    # the image is already visible. For unknown types, assume the same.
    echo "  Assuming image is visible to cluster via shared container daemon."
    ;;
esac

# ---------------------------------------------------------------------------
# Create namespace and register cleanup
# ---------------------------------------------------------------------------
cleanup() {
  if [[ "$KEEP_NS" == "true" ]]; then
    echo ""
    echo "Namespace '${NAMESPACE}' retained for debugging (--keep was set)."
    return
  fi
  echo ""
  echo "Cleaning up namespace '${NAMESPACE}'..."
  kubectl delete namespace "$NAMESPACE" --ignore-not-found --timeout=60s || true
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
  --set webhook.enabled=false \
  | kubectl apply -f -

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
  --namespace "$NAMESPACE" \
  | kubectl apply -f -

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
