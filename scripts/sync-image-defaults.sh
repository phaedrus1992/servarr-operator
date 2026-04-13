#!/usr/bin/env bash
#
# Generates the defaultImages section of chart/values.yaml from image-defaults.toml.
# Run from the repo root (or pass TOML path as $1).
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
TOML_FILE="${1:-$ROOT_DIR/image-defaults.toml}"
VALUES_FILE="$ROOT_DIR/chart/values.yaml"

if [[ ! -f "$TOML_FILE" ]]; then
    echo "error: $TOML_FILE not found" >&2
    exit 1
fi

# Parse image-defaults.toml and build the YAML defaultImages block.
# The TOML format is simple: [section] headers followed by key = "value" lines.
yaml_block="# Default container images for managed apps.
# AUTO-GENERATED from image-defaults.toml — edit that file instead.
# Regenerate with: scripts/sync-image-defaults.sh
defaultImages:"

current_app=""
repo=""
tag=""

flush_app() {
    if [[ -n "$current_app" && -n "$repo" ]]; then
        yaml_block+=$'\n'"  ${current_app}:"
        yaml_block+=$'\n'"    repository: ${repo}"
        yaml_block+=$'\n'"    tag: \"${tag}\""
    fi
    current_app=""
    repo=""
    tag=""
}

while IFS= read -r line; do
    # Skip comments and blank lines
    [[ "$line" =~ ^[[:space:]]*# ]] && continue
    [[ -z "${line// /}" ]] && continue

    # Section header: [appname]
    if [[ "$line" =~ ^\[([a-zA-Z0-9_-]+)\]$ ]]; then
        flush_app
        current_app="${BASH_REMATCH[1]}"
        continue
    fi

    # Key = "value" (strip quotes)
    if [[ "$line" =~ ^[[:space:]]*repository[[:space:]]*=[[:space:]]*\"(.+)\" ]]; then
        repo="${BASH_REMATCH[1]}"
    elif [[ "$line" =~ ^[[:space:]]*tag[[:space:]]*=[[:space:]]*\"(.+)\" ]]; then
        tag="${BASH_REMATCH[1]}"
    fi
done < "$TOML_FILE"
flush_app

# Replace the defaultImages section in values.yaml (from the comment to EOF,
# or up to the next top-level key if one exists after it).
# Strategy: keep everything before the defaultImages comment/key, then append the new block.

if [[ ! -f "$VALUES_FILE" ]]; then
    echo "error: $VALUES_FILE not found" >&2
    exit 1
fi

# Find the line where defaultImages section starts (comment or key)
start_pattern="^# Default container images\|^# AUTO-GENERATED\|^defaultImages:"
start_line=$(grep -n "$start_pattern" "$VALUES_FILE" | head -1 | cut -d: -f1)

if [[ -n "$start_line" ]]; then
    # Keep everything before the defaultImages section
    head -n $((start_line - 1)) "$VALUES_FILE" > "${VALUES_FILE}.tmp"
else
    # No existing section — append to end
    cp -f "$VALUES_FILE" "${VALUES_FILE}.tmp"
    echo "" >> "${VALUES_FILE}.tmp"
fi

echo "$yaml_block" >> "${VALUES_FILE}.tmp"
mv -f "${VALUES_FILE}.tmp" "$VALUES_FILE"

echo "Updated $VALUES_FILE from $TOML_FILE"
