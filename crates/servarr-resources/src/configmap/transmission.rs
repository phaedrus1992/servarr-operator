//! Default Transmission `settings.json` written by the init container (#834).

/// Shell function that writes `rpc-password` in Transmission's salted format (#848).
///
/// The value is `{` + hex SHA-1 of (password + salt) + salt, where the salt is 8 characters from
/// `[0-9a-zA-Z./]`. This matches `tr_ssha1` in upstream `libtransmission/crypto-utils.cc`.
/// Transmission accepts the salted value as-is, so plaintext never reaches `settings.json`.
/// An optional second argument fixes the salt, so a test can check the output against upstream.
pub(super) const SSHA1_SHELL_FN: &str = r#"ssha1() {
  _salt=${2:-$(tr -dc '0-9a-zA-Z./' < /dev/urandom | head -c 8)}
  _digest=$(printf '%s%s' "$1" "$_salt" | _sha1_hex)
  printf '{%s%s' "$_digest" "$_salt"
}
_sha1_hex() {
  # busybox and GNU ship sha1sum. macOS ships shasum only.
  if command -v sha1sum >/dev/null 2>&1; then
    sha1sum | cut -d' ' -f1
  else
    shasum -a 1 | cut -d' ' -f1
  fi
}
"#;

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
    let ssha1_fn = SSHA1_SHELL_FN;
    format!(
        r#"#!/bin/sh
set -e
SETTINGS_FILE="/config/settings.json"
OVERRIDE_FILE="/scripts/settings-override.json"

{ssha1_fn}

# Install jq if not present
if ! command -v jq >/dev/null 2>&1; then
  echo "Installing jq..."
  apk add --no-cache jq >/dev/null || {{ echo "Cannot install jq: apk add failed." >&2; exit 1; }}
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
    --arg pass "$(ssha1 "$CRED_PASS")" \
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

#[cfg(test)]
mod tests {
    use super::{SSHA1_SHELL_FN, apply_settings_script};
    use std::process::Command;

    /// The plaintext password must not reach `settings.json` (#848).
    #[test]
    fn apply_script_hashes_admin_password_before_writing() {
        let script = apply_settings_script(1000, 1000);
        assert!(script.contains("--arg pass \"$(ssha1 \"$CRED_PASS\")\""));
        assert!(!script.contains("--arg pass \"$CRED_PASS\""));
    }

    fn run_ssha1(call: &str) -> String {
        let script = format!("{SSHA1_SHELL_FN}ssha1 {call}");
        let output = Command::new("sh")
            .arg("-c")
            .arg(script)
            .output()
            .expect("sh must run the ssha1 shell function");
        assert!(
            output.status.success(),
            "ssha1 exited non-zero: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// The expected value is upstream's own vector from `tests/libtransmission/crypto-test.cc`.
    /// A mismatch means the shell function no longer produces what Transmission accepts.
    #[test]
    fn ssha1_matches_upstream_known_answer() {
        assert_eq!(
            run_ssha1("test 0pySy4ai1ZPMjeU1"),
            "{d209a21d3bc4f8fc4f8faf347e69f3def597eb170pySy4ai1ZPMjeU1"
        );
    }

    #[test]
    fn ssha1_with_random_salt_has_salted_shape() {
        let hashed = run_ssha1("'correct horse'");
        let (prefix, rest) = hashed.split_at(1);
        assert_eq!(prefix, "{");
        assert!(
            rest.len() >= 40 + 8,
            "digest plus 8-char salt, got {hashed:?}"
        );
        assert!(
            rest.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '/'),
            "salt and digest use Transmission's alphabet, got {hashed:?}"
        );
    }

    #[test]
    fn ssha1_hashes_empty_password_without_a_crash() {
        let salt = "0pySy4ai1ZPMjeU1";
        assert_eq!(
            run_ssha1(&format!("'' {salt}")),
            "{6268fc0c2db7a8a05289543556022cf9c79845bc0pySy4ai1ZPMjeU1"
        );
    }
}
