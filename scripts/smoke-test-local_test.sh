#!/usr/bin/env bash
#
# Tests for scripts/smoke-test-local.sh.
#
# Runs the real script against a fixture repo with kubectl/docker/helm replaced by fakes
# that log every call instead of touching a real cluster. Covers the two behaviors #762
# fixed: refusing to run against an unrecognized cluster type, and restoring the
# developer's kubectl namespace instead of leaving it pointed at a deleted one.
#
# Usage: scripts/smoke-test-local_test.sh
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
UNDER_TEST="$SCRIPT_DIR/smoke-test-local.sh"

pass_count=0
fail_count=0

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

# Builds a fixture repo: the real script under test, a fake Dockerfile/charts/smoke-test
# suite so it never fails on a missing file, and a bin/ directory of fake kubectl/docker/
# helm that log every call instead of touching a real cluster.
make_fixture() {
  local root="$WORK_DIR/case-$RANDOM$RANDOM"
  mkdir -p "$root/scripts" "$root/.github/smoke-test/manifests" \
    "$root/charts/servarr-crds" "$root/charts/servarr-operator" "$root/bin"

  cp "$UNDER_TEST" "$root/scripts/smoke-test-local.sh"
  chmod +x "$root/scripts/smoke-test-local.sh"
  touch "$root/Dockerfile"

  cat >"$root/.github/smoke-test/smoke-test.sh" <<'INNER'
#!/usr/bin/env bash
exit 0
INNER
  chmod +x "$root/.github/smoke-test/smoke-test.sh"

  cat >"$root/bin/kubectl" <<'INNER'
#!/usr/bin/env bash
echo "kubectl $*" >>"$FAKE_CALL_LOG"
case "$1" in
cluster-info) exit 0 ;;
config)
  case "$2" in
  current-context) echo "$FAKE_CONTEXT" ;;
  view) echo "${FAKE_PRIOR_NAMESPACE:-}" ;;
  esac
  ;;
apply) cat >/dev/null ;;
esac
exit 0
INNER
  chmod +x "$root/bin/kubectl"

  cat >"$root/bin/docker" <<'INNER'
#!/usr/bin/env bash
echo "docker $*" >>"$FAKE_CALL_LOG"
exit 0
INNER
  chmod +x "$root/bin/docker"

  cat >"$root/bin/helm" <<'INNER'
#!/usr/bin/env bash
echo "helm $*" >>"$FAKE_CALL_LOG"
[[ "$1" == "template" ]] && echo "# fake manifest"
exit 0
INNER
  chmod +x "$root/bin/helm"

  # Shadow the real kind/k3d that may exist on a dev machine, since the script's guard
  # must never depend on whether a real local cluster happens to exist — only on the
  # current context's name. FAKE_KIND_CLUSTERS/FAKE_K3D_CLUSTERS let a test case prove
  # that: set non-empty to simulate a real cluster registered, and confirm the guard
  # still refuses an unrecognized context. Non-"get"/"cluster" calls (the image-load
  # step) always succeed like a real tool.
  cat >"$root/bin/kind" <<'INNER'
#!/usr/bin/env bash
echo "kind $*" >>"$FAKE_CALL_LOG"
if [[ "$1" == "get" ]]; then
  [[ -n "${FAKE_KIND_CLUSTERS:-}" ]] && { echo "$FAKE_KIND_CLUSTERS"; exit 0; }
  exit 1
fi
exit 0
INNER
  chmod +x "$root/bin/kind"

  cat >"$root/bin/k3d" <<'INNER'
#!/usr/bin/env bash
echo "k3d $*" >>"$FAKE_CALL_LOG"
if [[ "$1" == "cluster" ]]; then
  [[ -n "${FAKE_K3D_CLUSTERS:-}" ]] && { echo "$FAKE_K3D_CLUSTERS"; exit 0; }
  exit 1
fi
exit 0
INNER
  chmod +x "$root/bin/k3d"

  echo "$root"
}

# Runs the script under test against a fresh fixture. Sets RESULT_EXIT, RESULT_CALLS,
# RESULT_OUTPUT for the assertions below.
# Args: <fake context> <fake prior namespace> [extra script args...]
run_script() {
  local context="$1" prior="$2"
  shift 2
  local root call_log
  root="$(make_fixture)"
  call_log="$WORK_DIR/calls-$RANDOM$RANDOM.log"
  : >"$call_log"

  RESULT_EXIT=0
  FAKE_CALL_LOG="$call_log" FAKE_CONTEXT="$context" FAKE_PRIOR_NAMESPACE="$prior" \
    FAKE_KIND_CLUSTERS="${FAKE_KIND_CLUSTERS:-}" FAKE_K3D_CLUSTERS="${FAKE_K3D_CLUSTERS:-}" \
    PATH="$root/bin:$PATH" \
    bash "$root/scripts/smoke-test-local.sh" "$@" >"$WORK_DIR/out.log" 2>&1 || RESULT_EXIT=$?
  RESULT_CALLS="$(cat "$call_log")"
  RESULT_OUTPUT="$(cat "$WORK_DIR/out.log")"
}

assert_exit() {
  local desc="$1" want="$2"
  if [[ "$RESULT_EXIT" == "$want" ]]; then
    echo "ok: $desc"
    pass_count=$((pass_count + 1))
  else
    echo "FAIL: $desc — expected exit $want, got $RESULT_EXIT"
    printf '%s\n' "$RESULT_OUTPUT" | sed 's/^/      /'
    fail_count=$((fail_count + 1))
  fi
}

assert_match() {
  local desc="$1" pattern="$2" text="$3"
  if grep -qE -- "$pattern" <<<"$text"; then
    echo "ok: $desc"
    pass_count=$((pass_count + 1))
  else
    echo "FAIL: $desc — did not match /$pattern/"
    printf '%s\n' "$text" | sed 's/^/      /'
    fail_count=$((fail_count + 1))
  fi
}

assert_no_match() {
  local desc="$1" pattern="$2" text="$3"
  if grep -qE -- "$pattern" <<<"$text"; then
    echo "FAIL: $desc — unexpectedly matched /$pattern/"
    printf '%s\n' "$text" | sed 's/^/      /'
    fail_count=$((fail_count + 1))
  else
    echo "ok: $desc"
    pass_count=$((pass_count + 1))
  fi
}

# ── Unrecognized cluster type ───────────────────────────────────────────────────────────

run_script "some-remote-cluster" ""
assert_exit "an unrecognized cluster type exits non-zero" 1
assert_match "an unrecognized cluster type prints a refusal message" \
  "Unrecognized cluster type" "$RESULT_OUTPUT"
assert_no_match "an unrecognized cluster type never runs the docker build" \
  "^docker build" "$RESULT_CALLS"
assert_no_match "an unrecognized cluster type never creates a namespace" \
  "^kubectl create namespace" "$RESULT_CALLS"

# The guard must key off the current context's name, not off whether a real kind/k3d
# cluster happens to exist on the host — otherwise a dev machine with an unrelated local
# cluster registered would wave through a context pointed at a different, unrecognized
# (possibly shared or production) cluster.
FAKE_KIND_CLUSTERS="unrelated-local-cluster"
run_script "some-remote-cluster" ""
FAKE_KIND_CLUSTERS=""
assert_exit "an unrecognized context still refuses even when a real kind cluster exists" 1
assert_no_match "a real kind cluster elsewhere never lets the docker build run" \
  "^docker build" "$RESULT_CALLS"

# ── Reserved namespace ──────────────────────────────────────────────────────────────────

run_script "docker-desktop" "my-prod-ns" --namespace default
assert_exit "--namespace default is refused" 1
assert_match "--namespace default prints a refusal message" \
  "reserved Kubernetes namespace" "$RESULT_OUTPUT"
assert_no_match "--namespace default never runs the docker build" \
  "^docker build" "$RESULT_CALLS"

run_script "docker-desktop" "my-prod-ns" --namespace kube-system
assert_exit "--namespace kube-system is refused" 1

# ── Recognized cluster types proceed ────────────────────────────────────────────────────

run_script "docker-desktop" "my-prod-ns"
assert_exit "a recognized cluster type exits zero" 0
assert_match "a recognized cluster type runs the docker build" \
  "^docker build" "$RESULT_CALLS"

# ── Namespace restore ───────────────────────────────────────────────────────────────────

run_script "docker-desktop" "my-prod-ns"
assert_exit "namespace-restore case exits zero" 0
last_set_context="$(grep 'config set-context' <<<"$RESULT_CALLS" | tail -n1)"
assert_match "the developer's prior namespace is restored on exit" \
  "--namespace=my-prod-ns\$" "$last_set_context"

run_script "kind-test" "team-a" --keep
assert_exit "--keep still exits zero" 0
assert_no_match "--keep skips namespace deletion" \
  "^kubectl delete namespace" "$RESULT_CALLS"
last_set_context="$(grep 'config set-context' <<<"$RESULT_CALLS" | tail -n1)"
assert_match "--keep still restores the developer's prior namespace" \
  "--namespace=team-a\$" "$last_set_context"

run_script "k3d-test" ""
assert_exit "an unset prior namespace still exits zero" 0
last_set_context="$(grep 'config set-context' <<<"$RESULT_CALLS" | tail -n1)"
assert_match "an unset prior namespace is restored, not left on the deleted smoke namespace" \
  "\-\-namespace=\$" "$last_set_context"

echo
echo "passed $pass_count, failed $fail_count"
[[ "$fail_count" -eq 0 ]]
