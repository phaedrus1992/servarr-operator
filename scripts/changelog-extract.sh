#!/usr/bin/env bash
set -euo pipefail

# Print the changelog body for a given version.
# Usage: changelog-extract.sh <version> [changelog-path]
#   <version>        e.g. 1.2.3 (no leading v)
# Exits 1 if no matching "## [<version>]" section exists.
#
# --self-check runs an inline assertion on a fixture and exits.

extract() {
  local version="$1" file="$2"
  awk -v ver="$version" '
    BEGIN { found = 0 }
    # Header for the requested version: "## [1.2.3]" possibly followed by " - date"
    $0 ~ "^## \\[" ver "\\]" { found = 1; next }
    # Any later top-level section header, or the url-marker, ends the section.
    found && (/^## \[/ || /^<!-- next-url -->/) { exit }
    found { print }
  ' "$file"
}

self_check() {
  local tmp out
  tmp="$(mktemp)"
  trap 'rm -f "$tmp"' RETURN
  cat >"$tmp" <<'EOF'
# Changelog
<!-- next-header -->

## [1.2.0] - 2026-01-02

### Added

- Second thing

## [1.1.0] - 2026-01-01

### Added

- First thing

<!-- next-url -->
[Unreleased]: https://example/compare/v1.2.0...HEAD
EOF
  out="$(extract "1.2.0" "$tmp")"
  case "$out" in
  *"Second thing"*) ;;
  *)
    echo "self-check FAILED: missing current section body" >&2
    exit 1
    ;;
  esac
  case "$out" in
  *"First thing"*)
    echo "self-check FAILED: leaked into older section" >&2
    exit 1
    ;;
  *) ;;
  esac
  case "$out" in
  *"next-url"* | *"compare"*)
    echo "self-check FAILED: leaked url marker" >&2
    exit 1
    ;;
  *) ;;
  esac
  echo "self-check OK"
}

main() {
  if [ "${1:-}" = "--self-check" ]; then
    self_check
    exit 0
  fi
  if [ $# -lt 1 ]; then
    echo "usage: changelog-extract.sh <version> [changelog-path]" >&2
    exit 2
  fi
  local version="$1"
  local file="${2:-CHANGELOG.md}"
  local body
  body="$(extract "$version" "$file")"
  # Strip leading and trailing blank lines (portable: no GNU `tac`).
  body="$(printf '%s\n' "$body" | awk '
    NF && !started { started = 1 }
    started { buf[++n] = $0 }
    END {
      while (n > 0 && buf[n] ~ /^[[:space:]]*$/) n--
      for (i = 1; i <= n; i++) print buf[i]
    }
  ')"
  if [ -z "$body" ]; then
    echo "no changelog section found for version $version in $file" >&2
    exit 1
  fi
  printf '%s\n' "$body"
}

main "$@"
