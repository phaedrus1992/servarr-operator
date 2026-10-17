//! Default Transmission `settings.json` written by the init container (#834).

pub(super) fn default_transmission_settings() -> String {
    #[expect(
        clippy::unwrap_used,
        reason = "serde_json::json! literal is always serializable"
    )]
    serde_json::to_string_pretty(&serde_json::json!({
        "download-dir": "/downloads/complete",
        "incomplete-dir": "/downloads/incomplete",
        "incomplete-dir-enabled": true,
        "dht-enabled": true,
        "pex-enabled": true,
        "lpd-enabled": false,
        "encryption": 1,
        "speed-limit-down-enabled": false,
        "speed-limit-up-enabled": false,
        "ratio-limit-enabled": false,
        "download-queue-enabled": true,
        "download-queue-size": 5,
        "seed-queue-enabled": true,
        "seed-queue-size": 10,
        "rpc-host-whitelist-enabled": false,
        "rpc-whitelist-enabled": true,
        "rpc-whitelist": "127.0.0.1,::1,10.*,172.*,192.168.*",
        "cache-size-mb": 4,
        "umask": "002",
        "rename-partial-files": true,
        "start-added-torrents": true,
    }))
    .unwrap()
}

/// Build the init script that merges settings and credentials into `settings.json` (#834, #848).
pub(super) fn apply_settings_script(uid: i64, gid: i64) -> String {
    format!(
        r#"#!/bin/sh
set -e
SETTINGS_FILE="/config/settings.json"
OVERRIDE_FILE="/scripts/settings-override.json"

# Install jq if not present
if ! command -v jq >/dev/null 2>&1; then
  echo "Installing jq..."
  apk add --no-cache jq >/dev/null || {{ echo "Cannot install jq: apk add failed. Check that the pod can reach the Alpine package mirror." >&2; exit 1; }}
fi

# If settings.json doesn't exist, create a minimal one.
# /config may be world-writable on first boot (before linuxserver /init runs).
if [ ! -f "$SETTINGS_FILE" ]; then
  echo "Creating initial settings.json..."
  echo '{{}}' > "$SETTINGS_FILE"
fi

# Build optional admin credentials overlay.
# Mounted at /run/secrets/admin/ when adminCredentials is set on the ServarrApp.
# Injecting here (init container) avoids the race between init-envfile and
# init-transmission-config that occurs when using the FILE__ env var mechanism.
AUTH_FILE=$(mktemp)
echo '{{}}' > "$AUTH_FILE"
if [ -f /run/secrets/admin/username ] && [ -f /run/secrets/admin/password ]; then
  echo "Injecting admin credentials into settings..."
  CRED_USER=$(cat /run/secrets/admin/username)
  # The password file holds the operator-derived salted hash, not the plaintext (#848).
  CRED_PASS=$(cat /run/secrets/admin/password)
  jq -n \
    --arg user "$CRED_USER" \
    --arg pass "$CRED_PASS" \
    '{{"rpc-authentication-required": true, "rpc-username": $user, "rpc-password": $pass}}' \
    > "$AUTH_FILE"
fi

# Merge override settings and auth credentials into existing settings.
# Write to /tmp first so we never touch a stale /config/*.tmp owned by a
# different uid, then copy back to settings.json (owner-writable).
echo "Applying settings overrides..."
TMP=$(mktemp)
if jq -s '.[0] * .[1] * .[2]' "$SETTINGS_FILE" "$OVERRIDE_FILE" "$AUTH_FILE" > "$TMP" 2>/dev/null; then
    cp "$TMP" "$SETTINGS_FILE" 2>/dev/null \
        || echo "Warning: could not write settings.json (check file ownership on adopted PVC)"
else
    echo "Warning: could not read settings.json (check file ownership on adopted PVC), skipping merge"
fi
rm -f "$TMP" "$AUTH_FILE"

# Fix ownership (best-effort: may not be possible on adopted PVCs owned by a different uid)
# Mode 660 keeps the group bit set. LSIO init runs as root without CAP_DAC_OVERRIDE, so it
# reads this file through the pod fsGroup (#834).
if ! chown_err=$(chown {uid}:{gid} "$SETTINGS_FILE" 2>&1); then
    echo "Warning: could not chown settings.json to {uid}:{gid} (continuing): $chown_err"
fi
if ! chmod_err=$(chmod 660 "$SETTINGS_FILE" 2>&1); then
    echo "Warning: could not chmod settings.json to 660 (continuing): $chmod_err"
fi

echo "Settings applied; see any warnings above."
"#
    )
}
