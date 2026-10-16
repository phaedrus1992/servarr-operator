//! Init script that patches the image's `/entry.sh` so authorized_keys ownership is not reset (#849).

/// Shell script run by the SSH bastion init container. It fails when a patch pattern is missing.
pub(super) const ENTRY_PATCH_SCRIPT: &str = r#"#!/bin/sh
set -e
# Patch entry.sh to skip chown/chmod on authorized_keys (emptyDir, already set by copy-authorized-keys)
if [ -f /entry.sh ]; then
  # Fail when a pattern is missing. A silent no-op would leave the chmod running (#849).
  for pattern in 'chmod 600 "$f"' 'chown "$user:$user" "$f"'; do
    grep -qF "$pattern" /entry.sh || { echo "Error: /entry.sh has no line matching $pattern, cannot patch" >&2; exit 1; }
  done
  sed -i 's/chmod 600 "$f"/true/g' /entry.sh
  sed -i 's/chown "$user:$user" "$f"/true/g' /entry.sh
else
  # No entry.sh means the image does not run this chown path, so nothing needs patching (#849).
  echo "Warning: /entry.sh not found; skipping the authorized_keys ownership patch" >&2
fi
"#;
