#!/usr/bin/env bash
set -euo pipefail

# App name -> default port mapping (from image-defaults.toml)
declare -A APP_PORTS=(
  [bazarr]=6767
  [jackett]=9117
  [jellyfin]=8096
  [lidarr]=8686
  [maintainerr]=6246
  [overseerr]=5055
  [plex]=32400
  [prowlarr]=9696
  [radarr]=7878
  [sabnzbd]=8080
  [sonarr]=8989
  [subgen]=9000
  [tautulli]=8181
  [transmission]=9091
)

APPS=("${!APP_PORTS[@]}")
TIMEOUT=360
POLL_INTERVAL=10
MIN_READY=${#APPS[@]}

echo "Phase 1: Waiting for deployments to become ready (timeout: ${TIMEOUT}s, min: ${MIN_READY}/${#APPS[@]})"

elapsed=0
ready_apps=()
while true; do
  ready_count=0
  ready_apps=()
  not_ready_apps=()
  for app in "${APPS[@]}"; do
    ready=$(kubectl get deployment "$app" -o jsonpath='{.status.readyReplicas}' 2>/dev/null || echo "0")
    if [[ "${ready:-0}" -ge 1 ]]; then
      ready_count=$((ready_count + 1))
      ready_apps+=("$app")
    else
      not_ready_apps+=("$app")
    fi
  done

  if [[ $ready_count -eq ${#APPS[@]} ]]; then
    echo "All ${#APPS[@]} deployments are ready."
    break
  fi

  if [[ $elapsed -ge $TIMEOUT ]]; then
    echo "ERROR: Only ${ready_count}/${#APPS[@]} deployments ready after ${TIMEOUT}s"
    echo "  Not ready: ${not_ready_apps[*]}"
    echo "Deployment status:"
    kubectl get deployments -o wide
    exit 1
  fi

  echo "  ${ready_count}/${#APPS[@]} ready (${elapsed}s/${TIMEOUT}s)"
  sleep "$POLL_INTERVAL"
  elapsed=$((elapsed + POLL_INTERVAL))
done

echo ""
echo "Phase 2: HTTP health checks via port-forward"

pass=0
fail=0
skip=0
for app in "${APPS[@]}"; do
  port=${APP_PORTS[$app]}
  local_port=$((port + 10000))

  # Only check apps that became ready
  ready=$(kubectl get deployment "$app" -o jsonpath='{.status.readyReplicas}' 2>/dev/null || echo "0")
  if [[ "${ready:-0}" -lt 1 ]]; then
    echo "  ${app}: SKIP (not ready)"
    skip=$((skip + 1))
    continue
  fi

  echo -n "  ${app} (port ${port} -> localhost:${local_port}): "

  # Start port-forward in background
  kubectl port-forward "deployment/${app}" "${local_port}:${port}" &
  pf_pid=$!

  # Wait for port-forward to be ready
  sleep 3

  # Curl the app — accept any HTTP response (200, 302, 401, etc.)
  status=$(curl -s -o /dev/null -w "%{http_code}" --max-time 10 "http://localhost:${local_port}/" 2>/dev/null || echo "000")

  # Kill port-forward
  kill "$pf_pid" 2>/dev/null || true
  wait "$pf_pid" 2>/dev/null || true

  if [[ "$status" == "000" ]]; then
    echo "FAIL (no response)"
    fail=$((fail + 1))
  else
    echo "OK (HTTP ${status})"
    pass=$((pass + 1))
  fi
done

echo ""
echo "Results: ${pass} passed, ${fail} failed, ${skip} skipped"

if [[ $fail -ne 0 ]]; then
  echo "ERROR: ${fail} health check(s) failed"
  exit 1
fi

if [[ $pass -lt $MIN_READY ]]; then
  echo "ERROR: Only ${pass} apps passed health checks (need ${MIN_READY})"
  exit 1
fi

echo "Smoke tests passed."

# ---------------------------------------------------------------------------
# Phase 3: adminCredentials transition
#
# The MediaStack 'media' was deployed WITHOUT adminCredentials.  Patch it now
# to add them, which exercises the reconcile path that applies credentials to
# already-running apps.  This transition is the most likely source of bugs
# (the first-time credential injection code path).
# ---------------------------------------------------------------------------

echo ""
echo "Phase 3: adminCredentials transition — patching MediaStack to add credentials"

# Capture current deployment generation for each media app before patching.
# We wait for the generation to increase (operator reconciled + patched the
# Deployment) and then for the rollout to complete (new pods ready).
#
# Only apps whose Deployments change when adminCredentials is added are checked:
#   - media-sonarr:      checksum annotation triggers a rolling update
#   - media-transmission: FILE__USER/FILE__PASS env vars trigger a rolling update
# media-jellyfin is excluded: auth is configured via live API only, so the
# Deployment spec never changes and no rolling update is triggered.
declare -A PRE_GEN
for app in media-sonarr media-transmission; do
  PRE_GEN[$app]=$(kubectl get deployment "$app" \
    -o jsonpath='{.metadata.generation}' 2>/dev/null || echo "1")
done

kubectl patch mediastack media --type=merge \
  -p '{"spec":{"defaults":{"adminCredentials":{"secretName":"smoke-admin"}}}}'

echo "  Patch applied.  Waiting for media-sonarr and media-transmission rollouts to complete (up to 300s)..."
TRANSITION_TIMEOUT=300
elapsed=0
while true; do
  all_done=true
  for app in media-sonarr media-transmission; do
    gen=$(kubectl get deployment "$app" \
      -o jsonpath='{.metadata.generation}' 2>/dev/null || echo "${PRE_GEN[$app]}")
    obs=$(kubectl get deployment "$app" \
      -o jsonpath='{.status.observedGeneration}' 2>/dev/null || echo "0")
    updated=$(kubectl get deployment "$app" \
      -o jsonpath='{.status.updatedReplicas}' 2>/dev/null || echo "0")
    desired=$(kubectl get deployment "$app" \
      -o jsonpath='{.spec.replicas}' 2>/dev/null || echo "1")
    ready=$(kubectl get deployment "$app" \
      -o jsonpath='{.status.readyReplicas}' 2>/dev/null || echo "0")
    # Wait for the operator to bump the generation AND the rollout to complete.
    if [[ "$gen" -le "${PRE_GEN[$app]}" \
        || "$obs" -lt "$gen" \
        || "${updated:-0}" -lt "${desired:-1}" \
        || "${ready:-0}" -lt "${desired:-1}" ]]; then
      all_done=false
      break
    fi
  done
  if $all_done; then
    echo "  All media-* deployments completed their rollout."
    break
  fi
  if [[ $elapsed -ge $TRANSITION_TIMEOUT ]]; then
    echo "ERROR: media-* rollouts not complete after credential transition (${TRANSITION_TIMEOUT}s)"
    kubectl get deployments -l "app.kubernetes.io/instance=media" -o wide
    exit 1
  fi
  echo "  Waiting... (${elapsed}s/${TRANSITION_TIMEOUT}s)"
  sleep 10
  elapsed=$((elapsed + 10))
done

# ---------------------------------------------------------------------------
# Phase 4: Admin credential verification
#
# The MediaStack now has adminCredentials pointing at the 'smoke-admin' Secret.
# The operator calls PUT /api/v3/config/host for Sonarr and calls the API for
# Jellyfin/Transmission.  We verify each mechanism works.
# ---------------------------------------------------------------------------

echo ""
echo "Phase 4: Admin credential verification (MediaStack 'media')"

ADMIN_USER=$(kubectl get secret smoke-admin -o jsonpath='{.data.username}' | base64 -d)
ADMIN_PASS=$(kubectl get secret smoke-admin -o jsonpath='{.data.password}' | base64 -d)

# All media-* deployments are already confirmed ready by Phase 3.
# Extra dwell time for the operator to finish live API credential-sync calls
# (Jellyfin startup wizard can take a moment to respond after first boot).
sleep 40

# Helper: port-forward to a deployment, run a check function, then clean up.
# Usage: with_port_forward <deployment> <remote_port> <local_port> <check_fn>
with_port_forward() {
  local deploy=$1 rport=$2 lport=$3 check_fn=$4
  kubectl port-forward "deployment/${deploy}" "${lport}:${rport}" &>/dev/null &
  local pf_pid=$!
  sleep 3
  local result=0
  $check_fn "$lport" || result=$?
  kill "$pf_pid" 2>/dev/null || true
  wait "$pf_pid" 2>/dev/null || true
  return $result
}

cred_pass=0
cred_fail=0

# --- Sonarr: operator applies Forms auth credentials via PUT /api/v3/config/host.
#
# Verification sequence:
#   1. Unauthenticated API call returns 401 (Forms auth enforced).
#   2. GET /login returns 200 — the login page is shown, not the first-run
#      setup wizard (which would mean credentials were never applied).
#   3. POST /login with correct credentials redirects to the dashboard (not
#      back to /login?loginFailed=...).
#
# Retry up to 60s because Sonarr may still be initialising on first boot. ---
check_sonarr_auth() {
  local lport=$1

  local deadline=$(( $(date +%s) + 60 ))
  while true; do
    # Step 1: unauthenticated API must return 401.
    local status_api
    status_api=$(curl -s -o /dev/null -w "%{http_code}" --max-time 10 \
      "http://localhost:${lport}/api/v3/system/status" 2>/dev/null || echo "000")

    if [[ "$status_api" != "401" ]]; then
      if [[ $(date +%s) -ge $deadline ]]; then
        echo "  media-sonarr: FAIL (expected unauthenticated API to return 401, got ${status_api})"
        return 1
      fi
      sleep 10
      continue
    fi

    # Step 2: /login must return 200 (not a redirect to the setup wizard).
    local login_status
    login_status=$(curl -s -o /dev/null -w "%{http_code}" --max-time 10 \
      -L "http://localhost:${lport}/login" 2>/dev/null || echo "000")

    if [[ "$login_status" != "200" ]]; then
      if [[ $(date +%s) -ge $deadline ]]; then
        echo "  media-sonarr: FAIL (expected /login to return 200, got ${login_status}" \
             "— first-run setup wizard may still be active)"
        return 1
      fi
      sleep 10
      continue
    fi

    # Step 3: POST /login with correct credentials must redirect to the
    # dashboard.  A redirect back to /login (with loginFailed) means the
    # credentials were not applied.
    local location
    location=$(curl -si --max-time 10 \
      -X POST "http://localhost:${lport}/login?returnUrl=%2F" \
      -H 'Content-Type: application/x-www-form-urlencoded' \
      --data-urlencode "username=${ADMIN_USER}" \
      --data-urlencode "password=${ADMIN_PASS}" \
      2>/dev/null | grep -i "^location:" | awk '{print $2}' | tr -d '\r')

    if [[ -z "$location" ]]; then
      if [[ $(date +%s) -ge $deadline ]]; then
        echo "  media-sonarr: FAIL (POST /login returned no Location header)"
        return 1
      fi
      sleep 10
      continue
    fi

    if [[ "$location" == *"/login"* ]]; then
      if [[ $(date +%s) -ge $deadline ]]; then
        echo "  media-sonarr: FAIL (POST /login redirected to ${location} — credentials rejected)"
        return 1
      fi
      sleep 10
      continue
    fi

    echo "  media-sonarr: OK (Forms auth: unauth API=401, login page reachable, credentials authenticate)"
    return 0
  done
}

echo -n ""
if with_port_forward media-sonarr 8989 28989 check_sonarr_auth; then
  cred_pass=$((cred_pass + 1))
else
  cred_fail=$((cred_fail + 1))
fi

# --- Transmission: session-set enables RPC auth.
#
# Transmission 4.x with auth enabled checks credentials BEFORE the CSRF
# session-ID.  The verification sequence is:
#   1. Bare request (no creds, no session ID)  → 401 (auth enforced immediately)
#   2. Request WITH credentials, no session ID → 409 + X-Transmission-Session-Id
#   3. Request with session ID, no credentials → 401
#   4. Request with session ID + credentials   → 200
#
# We verify both directions: unauthenticated returns 401, authenticated returns 200.
# Retry up to 60s because auth is applied by a custom-cont-init.d script that
# runs during container startup. ---
check_transmission_auth() {
  local lport=$1

  local deadline=$(( $(date +%s) + 60 ))
  while true; do
    # Step 1: verify unauthenticated request returns 401 (auth check is first in Tx 4.x).
    local status_no_creds
    status_no_creds=$(curl -s -o /dev/null -w "%{http_code}" --max-time 10 \
      -X POST "http://localhost:${lport}/transmission/rpc" \
      -H 'Content-Type: application/json' \
      -d '{"method":"session-get"}' 2>/dev/null || echo "000")

    if [[ "$status_no_creds" != "401" ]]; then
      # Auth not yet enforced (or Transmission not yet ready)
      if [[ $(date +%s) -ge $deadline ]]; then
        echo "  media-transmission: FAIL (expected bare request to return 401, got ${status_no_creds})"
        return 1
      fi
      sleep 10
      continue
    fi

    # Step 2: get the session ID using correct credentials.
    # Transmission 4.x requires credentials before it will hand out a session ID.
    local session_id
    session_id=$(curl -si --max-time 10 \
      -X POST "http://localhost:${lport}/transmission/rpc" \
      -H 'Content-Type: application/json' \
      -u "${ADMIN_USER}:${ADMIN_PASS}" \
      -d '{"method":"session-get"}' 2>/dev/null \
      | grep -i -m 1 "X-Transmission-Session-Id:" | awk '{print $2}' | tr -d '\r')

    if [[ -z "$session_id" ]]; then
      if [[ $(date +%s) -ge $deadline ]]; then
        echo "  media-transmission: FAIL (could not obtain session ID with correct credentials)"
        return 1
      fi
      sleep 10
      continue
    fi

    # Step 3: with session ID but no credentials → 401
    local status_no_auth
    status_no_auth=$(curl -s -o /dev/null -w "%{http_code}" --max-time 10 \
      -X POST "http://localhost:${lport}/transmission/rpc" \
      -H 'Content-Type: application/json' \
      -H "X-Transmission-Session-Id: ${session_id}" \
      -d '{"method":"session-get"}' 2>/dev/null || echo "000")

    # Step 4: with session ID + correct credentials → 200
    local status_with_auth
    status_with_auth=$(curl -s -o /dev/null -w "%{http_code}" --max-time 10 \
      -X POST "http://localhost:${lport}/transmission/rpc" \
      -H 'Content-Type: application/json' \
      -H "X-Transmission-Session-Id: ${session_id}" \
      -u "${ADMIN_USER}:${ADMIN_PASS}" \
      -d '{"method":"session-get"}' 2>/dev/null || echo "000")

    if [[ "$status_no_auth" == "401" && "$status_with_auth" == "200" ]]; then
      echo "  media-transmission: OK (auth enforced: bare=401, no-creds=401, correct-creds=200)"
      return 0
    fi

    if [[ $(date +%s) -ge $deadline ]]; then
      echo "  media-transmission: FAIL (expected no-creds=401 and correct-creds=200," \
           "got no-creds=${status_no_auth} and correct-creds=${status_with_auth})"
      return 1
    fi
    sleep 10
  done
}

if with_port_forward media-transmission 9091 29091 check_transmission_auth; then
  cred_pass=$((cred_pass + 1))
else
  cred_fail=$((cred_fail + 1))
fi

# --- Jellyfin: startup wizard set the admin account → credentials authenticate ---
# Retry up to 60s because the startup wizard may still be processing on first boot.
check_jellyfin_auth() {
  local lport=$1
  local auth_header
  auth_header='MediaBrowser Client="servarr-operator", Device="servarr-operator",'
  auth_header+=' DeviceId="servarr-operator-device", Version="1.0.0"'

  local deadline=$(( $(date +%s) + 60 ))
  while true; do
    local status
    status=$(curl -s -o /dev/null -w "%{http_code}" --max-time 10 \
      -X POST "http://localhost:${lport}/Users/AuthenticateByName" \
      -H "X-Emby-Authorization: ${auth_header}" \
      -H 'Content-Type: application/json' \
      -d "{\"Username\":\"${ADMIN_USER}\",\"Pw\":\"${ADMIN_PASS}\"}" \
      2>/dev/null || echo "000")

    if [[ "$status" == "200" ]]; then
      echo "  media-jellyfin: OK (admin credentials authenticate successfully)"
      return 0
    fi

    if [[ $(date +%s) -ge $deadline ]]; then
      echo "  media-jellyfin: FAIL (expected 200 from AuthenticateByName, got ${status})"
      return 1
    fi
    sleep 10
  done
}

if with_port_forward media-jellyfin 8096 28096 check_jellyfin_auth; then
  cred_pass=$((cred_pass + 1))
else
  cred_fail=$((cred_fail + 1))
fi

echo ""
echo "Credential check results: ${cred_pass} passed, ${cred_fail} failed"

if [[ $cred_fail -ne 0 ]]; then
  echo "ERROR: ${cred_fail} credential check(s) failed"
  exit 1
fi

echo "All smoke tests passed."
