use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use servarr_crds::{AppConfig, AppType, ServarrApp, SshMode};
use std::collections::BTreeMap;

use crate::common;

pub fn build(app: &ServarrApp) -> Option<ConfigMap> {
    match app.spec.app {
        AppType::Transmission => build_transmission(app),
        AppType::Sabnzbd => build_sabnzbd(app),
        _ => None,
    }
}

/// Build a ConfigMap containing per-user read-only rsync wrapper scripts for SSH bastion.
///
/// One key per user whose mode is `SshMode::Rsync` (any path, read-only) or
/// `SshMode::RestrictedRsync` (specific paths only, read-only).  Rsync is always
/// read-only — write operations are never permitted.
///
/// Returns `None` when no users have an rsync-based mode.
pub fn build_ssh_bastion_restricted_rsync(app: &ServarrApp) -> Option<ConfigMap> {
    let ssh_config = match app.spec.app_config {
        Some(AppConfig::SshBastion(ref sc)) => sc,
        _ => return None,
    };

    let mut data = BTreeMap::new();
    for user in &ssh_config.users {
        if user.mode != SshMode::RestrictedRsync && user.mode != SshMode::Rsync {
            continue;
        }

        let allowed_paths: String = user
            .restricted_rsync
            .as_ref()
            .map(|rr| {
                rr.allowed_paths
                    .iter()
                    .map(|p| format!("      \"{p}\""))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();

        let script = format!(
            r#"#!/bin/bash
# Read-only rsync wrapper - enforces --sender and optionally restricts to allowed paths
set -eo pipefail

# Allowed paths (empty = allow any path)
ALLOWED_PATHS=(
{allowed_paths}
)

# Get the command string
# When used as a login shell, SSH invokes: /path/to/shell -c "command"
# When used with ForceCommand, SSH_ORIGINAL_COMMAND is set
if [[ "${{1:-}}" == "-c" && -n "${{2:-}}" ]]; then
  CMD_STRING="$2"
elif [[ -n "${{SSH_ORIGINAL_COMMAND:-}}" ]]; then
  CMD_STRING="$SSH_ORIGINAL_COMMAND"
else
  CMD_STRING=""
fi

log_reject() {{
  logger -t restricted-rsync -p auth.warning "REJECTED: user=$USER reason=$1"
  echo "Error: $1" >&2
  exit 1
}}

# Must be invoked with a command
if [[ -z "$CMD_STRING" ]]; then
  log_reject "Interactive sessions not allowed"
fi

# Parse command string into array safely (no eval)
# rsync --server always produces simple space-separated arguments without
# shell metacharacters, so read -ra is safe and avoids code injection.
declare -a ARGS
read -ra ARGS <<< "$CMD_STRING"

# Must have at least the command name
if [[ ${{#ARGS[@]}} -lt 1 ]]; then
  log_reject "Empty command"
fi

# First argument must be rsync
if [[ "${{ARGS[0]}}" != "rsync" ]]; then
  log_reject "Only rsync commands are allowed"
fi

# Enforce read-only: --sender flag must be present (rsync sends data to client = read-only)
has_sender=false
for arg in "${{ARGS[@]}}"; do
  if [[ "$arg" == "--sender" ]]; then
    has_sender=true
    break
  fi
done

if [[ "$has_sender" != "true" ]]; then
  log_reject "Write operations not allowed (read-only mode)"
fi

# Find the path argument
# rsync server format: rsync --server [options] . <path>
# The path is the last argument, after a "." argument
RSYNC_PATH=""
found_dot=false
for arg in "${{ARGS[@]}}"; do
  if [[ "$found_dot" == "true" ]]; then
    RSYNC_PATH="$arg"
  fi
  if [[ "$arg" == "." ]]; then
    found_dot=true
  fi
done

if [[ -z "$RSYNC_PATH" ]]; then
  log_reject "Could not parse rsync path"
fi

# Check for path traversal attempts
if [[ "$RSYNC_PATH" == *".."* ]]; then
  log_reject "Path traversal not allowed"
fi

# If no allowed paths are configured, any path is permitted (Rsync mode).
# If allowed paths are configured, enforce the path allowlist (RestrictedRsync mode).
if [[ "${{#ALLOWED_PATHS[@]}}" -gt 0 ]]; then
  # Normalize path: resolve to absolute and remove trailing slashes
  if [[ -e "$RSYNC_PATH" ]]; then
    RESOLVED_PATH=$(realpath "$RSYNC_PATH")
  else
    RESOLVED_PATH="${{RSYNC_PATH%/}}"
  fi

  path_allowed=false
  for allowed in "${{ALLOWED_PATHS[@]}}"; do
    allowed="${{allowed%/}}"
    if [[ "$RESOLVED_PATH" == "$allowed" || "$RESOLVED_PATH" == "$allowed"/* ]]; then
      path_allowed=true
      break
    fi
  done

  if [[ "$path_allowed" != "true" ]]; then
    log_reject "Path not in allowed list: $RSYNC_PATH"
  fi
fi

# Log successful access
logger -t restricted-rsync -p auth.info "ALLOWED: user=$USER path=$RSYNC_PATH"

# Execute rsync with properly quoted arguments
exec "${{ARGS[@]}}"
"#
        );

        data.insert(format!("restricted-rsync-{}.sh", user.name), script);
    }

    if data.is_empty() {
        return None;
    }

    Some(ConfigMap {
        metadata: ObjectMeta {
            name: Some(common::child_name(app, "restricted-rsync")),
            namespace: Some(common::app_namespace(app)),
            labels: Some(common::labels(app)),
            owner_references: Some(vec![common::owner_reference(app)]),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    })
}

/// Build a ConfigMap containing custom Prowlarr indexer definitions.
///
/// Each definition entry becomes a `{name}.yml` key in the ConfigMap data,
/// which is then mounted at `/config/Definitions/Custom/` in the container.
pub fn build_prowlarr_definitions(app: &ServarrApp) -> Option<ConfigMap> {
    let defs = match app.spec.app_config {
        Some(AppConfig::Prowlarr(ref pc)) if !pc.custom_definitions.is_empty() => {
            &pc.custom_definitions
        }
        _ => return None,
    };

    let mut data = BTreeMap::new();
    for def in defs {
        data.insert(format!("{}.yml", def.name), def.content.clone());
    }

    Some(ConfigMap {
        metadata: ObjectMeta {
            name: Some(common::child_name(app, "prowlarr-definitions")),
            namespace: Some(common::app_namespace(app)),
            labels: Some(common::labels(app)),
            owner_references: Some(vec![common::owner_reference(app)]),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    })
}

/// Build a separate ConfigMap for SABnzbd tar-unpack scripts (if enabled).
pub fn build_tar_unpack(app: &ServarrApp) -> Option<ConfigMap> {
    let tar_enabled = matches!(
        app.spec.app_config,
        Some(AppConfig::Sabnzbd(ref sc)) if sc.tar_unpack
    );
    if !tar_enabled {
        return None;
    }

    let install_script = r#"#!/usr/bin/with-contenv bash
# s6-overlay custom-cont-init.d script: install compression tools
echo "Installing compression utilities for tar unpack..."
apk add --no-cache tar xz bzip2 zstd >/dev/null 2>&1
echo "Compression utilities installed."
"#;

    let unpack_script = r#"#!/bin/bash
# SABnzbd post-processing script: unpack tar archives
# Arguments: $1=directory $2=origName $3=cleanName $4=indexerName $5=category $6=group $7=status
DOWNLOAD_DIR="$1"

if [ -z "$DOWNLOAD_DIR" ] || [ ! -d "$DOWNLOAD_DIR" ]; then
    echo "No download directory provided"
    exit 0
fi

cd "$DOWNLOAD_DIR" || exit 0

for archive in *.tar *.tar.gz *.tgz *.tar.bz2 *.tbz2 *.tar.xz *.txz *.tar.zst *.tzst; do
    [ -f "$archive" ] || continue
    echo "Unpacking: $archive"
    case "$archive" in
        *.tar.gz|*.tgz)     tar xzf "$archive" ;;
        *.tar.bz2|*.tbz2)   tar xjf "$archive" ;;
        *.tar.xz|*.txz)     tar xJf "$archive" ;;
        *.tar.zst|*.tzst)   tar --zstd -xf "$archive" ;;
        *.tar)              tar xf "$archive" ;;
    esac
    echo "Unpacked: $archive"
done

exit 0
"#;

    let mut data = BTreeMap::new();
    data.insert("install-tar-tools.sh".into(), install_script.to_string());
    data.insert("unpack-tar.sh".into(), unpack_script.to_string());

    Some(ConfigMap {
        metadata: ObjectMeta {
            name: Some(common::child_name(app, "tar-unpack")),
            namespace: Some(common::app_namespace(app)),
            labels: Some(common::labels(app)),
            owner_references: Some(vec![common::owner_reference(app)]),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    })
}

fn build_sabnzbd(app: &ServarrApp) -> Option<ConfigMap> {
    let sc = match app.spec.app_config {
        Some(AppConfig::Sabnzbd(ref sc)) => sc,
        _ => return None,
    };
    if sc.host_whitelist.is_empty() {
        return None;
    }
    let host_whitelist = sc.host_whitelist.clone();

    let whitelist_csv = host_whitelist.join(", ");

    // Script that patches the SABnzbd INI config before the main container starts.
    // SABnzbd uses a Python-style INI file; host_whitelist is under [misc].
    let apply_script = r#"#!/bin/sh
set -e
INI_FILE="/config/sabnzbd.ini"
WHITELIST_VALUE="$1"

if [ ! -f "$INI_FILE" ]; then
  echo "No sabnzbd.ini found, creating minimal config..."
  mkdir -p /config
  printf "[misc]\nhost_whitelist = %s\n" "$WHITELIST_VALUE" > "$INI_FILE"
  exit 0
fi

# Update existing host_whitelist or add it under [misc].
# Use awk instead of sed to avoid metacharacter injection from whitelist values.
if grep -q "^host_whitelist" "$INI_FILE"; then
  awk -v val="$WHITELIST_VALUE" '/^host_whitelist/{print "host_whitelist = " val; next}1' \
    "$INI_FILE" > "${INI_FILE}.tmp" && mv -f "${INI_FILE}.tmp" "$INI_FILE"
else
  awk -v val="$WHITELIST_VALUE" '/^\[misc\]/{print; print "host_whitelist = " val; next}1' \
    "$INI_FILE" > "${INI_FILE}.tmp" && mv -f "${INI_FILE}.tmp" "$INI_FILE"
fi

echo "SABnzbd host_whitelist set to: $WHITELIST_VALUE"
"#;

    let mut data = BTreeMap::new();
    data.insert("apply-whitelist.sh".into(), apply_script.to_string());
    data.insert("host-whitelist".into(), whitelist_csv);

    Some(ConfigMap {
        metadata: ObjectMeta {
            name: Some(common::child_name(app, "sabnzbd-config")),
            namespace: Some(common::app_namespace(app)),
            labels: Some(common::labels(app)),
            owner_references: Some(vec![common::owner_reference(app)]),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    })
}

fn build_transmission(app: &ServarrApp) -> Option<ConfigMap> {
    if !matches!(app.spec.app, AppType::Transmission) {
        return None;
    }

    let uid = app.spec.uid.unwrap_or(65534);
    let gid = app.spec.gid.unwrap_or(65534);

    let settings_json = if let Some(AppConfig::Transmission(ref tc)) = app.spec.app_config {
        if tc.settings.is_null() {
            default_transmission_settings()
        } else {
            serde_json::to_string_pretty(&tc.settings).unwrap_or_default()
        }
    } else {
        default_transmission_settings()
    };

    let apply_script = format!(
        r#"#!/bin/sh
set -e
SETTINGS_FILE="/config/settings.json"
OVERRIDE_FILE="/scripts/settings-override.json"

# Install jq if not present
if ! command -v jq >/dev/null 2>&1; then
  echo "Installing jq..."
  apk add --no-cache jq >/dev/null 2>&1
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
jq -s '.[0] * .[1] * .[2]' "$SETTINGS_FILE" "$OVERRIDE_FILE" "$AUTH_FILE" > "$TMP"
cp "$TMP" "$SETTINGS_FILE"
rm -f "$TMP" "$AUTH_FILE"

# Fix ownership
chown {uid}:{gid} "$SETTINGS_FILE"
chmod 600 "$SETTINGS_FILE"

echo "Settings applied successfully."
"#
    );

    // Custom cont-init.d script that runs AFTER init-transmission-config.
    // Dependency chain: init-transmission-config → init-config-end → init-mods
    //   → init-mods-package-install → init-mods-end → init-custom-files
    // So this script is guaranteed to execute after init-transmission-config has
    // already written rpc-authentication-required (which may be false if USER/PASS
    // were not yet in the s6 container env due to the init-envfile race).
    //
    // This script re-applies auth settings from the mounted secret, winning the race.
    let auth_script = r#"#!/bin/sh
set -e
SETTINGS_FILE="/config/settings.json"

if [ ! -f /run/secrets/admin/username ] || [ ! -f /run/secrets/admin/password ]; then
  echo "[transmission-auth] No admin credentials mounted, skipping"
  exit 0
fi

if [ ! -f "$SETTINGS_FILE" ]; then
  echo "[transmission-auth] settings.json not found, skipping"
  exit 0
fi

CRED_USER=$(cat /run/secrets/admin/username)
CRED_PASS=$(cat /run/secrets/admin/password)

echo "[transmission-auth] Setting admin credentials..."
TMP=$(mktemp)
jq \
  --arg user "$CRED_USER" \
  --arg pass "$CRED_PASS" \
  '.["rpc-authentication-required"] = true | .["rpc-username"] = $user | .["rpc-password"] = $pass' \
  "$SETTINGS_FILE" > "$TMP"
cp "$TMP" "$SETTINGS_FILE"
rm -f "$TMP"
echo "[transmission-auth] Admin credentials set successfully."
"#;

    let mut data = BTreeMap::new();
    data.insert("settings-override.json".into(), settings_json);
    data.insert("apply-settings.sh".into(), apply_script);
    data.insert("99-transmission-auth.sh".into(), auth_script.to_string());

    Some(ConfigMap {
        metadata: ObjectMeta {
            name: Some(common::app_name(app)),
            namespace: Some(common::app_namespace(app)),
            labels: Some(common::labels(app)),
            owner_references: Some(vec![common::owner_reference(app)]),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    })
}

fn default_transmission_settings() -> String {
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

/// Build the ConfigMap containing the Bazarr init script.
///
/// The init script writes `/config/config/config.yaml` if it does not already
/// exist, seeding Bazarr with the operator-managed API key (and optional auth
/// config). This is idempotent: a second run is a no-op if the file exists.
pub fn build_bazarr_init(app: &ServarrApp) -> Option<ConfigMap> {
    if !matches!(app.spec.app, AppType::Bazarr) {
        return None;
    }

    let name = common::child_name(app, "init");
    let ns = common::app_namespace(app);

    // The script is intentionally simple — no jq dependency.
    // BAZARR_API_KEY is injected from the operator-managed Secret via secretKeyRef.
    // Auth is configured post-boot via sync_admin_credentials; the auth fields default
    // to noauth so Bazarr starts accessible until credentials are applied.
    let script = r#"#!/bin/sh
set -eu
CONFIG=/config/config/config.yaml
if [ -f "$CONFIG" ]; then
  echo "bazarr-init: config already exists, skipping"
  exit 0
fi
mkdir -p "$(dirname "$CONFIG")"
cat > "$CONFIG" << BAZARR_EOF
general:
  apikey: ${BAZARR_API_KEY}
auth:
  type: ${BAZARR_AUTH_TYPE:-noauth}
  username: ${BAZARR_USERNAME:-}
  password: ${BAZARR_PASSWORD_MD5:-}
BAZARR_EOF
echo "bazarr-init: wrote $CONFIG"
"#;

    Some(ConfigMap {
        metadata: ObjectMeta {
            name: Some(name),
            namespace: Some(ns),
            labels: Some(common::labels(app)),
            owner_references: Some(vec![common::owner_reference(app)]),
            ..Default::default()
        },
        data: Some(BTreeMap::from([(
            "bazarr-init.sh".to_string(),
            script.to_string(),
        )])),
        ..Default::default()
    })
}
