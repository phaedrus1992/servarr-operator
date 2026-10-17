#!/usr/bin/env bash
#
# Tests for scripts/generate-crds.sh.
#
# Runs the real script against a fixture repo where `cargo` is a fake that prints a chosen
# set of CRDs. Covers the fail-closed paths: an unknown CRD, a third CRD, a missing CRD, and
# a failed build.
#
# Usage: scripts/generate-crds_test.sh
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
UNDER_TEST="$SCRIPT_DIR/generate-crds.sh"

pass_count=0
fail_count=0

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

crd() {
  printf 'apiVersion: apiextensions.k8s.io/v1\nkind: CustomResourceDefinition\nmetadata:\n  name: %s\n' "$1"
}

# Builds a fixture repo with the real script and a fake cargo. The fake prints the file named
# by FAKE_CARGO_OUTPUT and exits with FAKE_CARGO_EXIT.
make_fixture() {
  local root
  root="$(mktemp -d "$WORK_DIR/fixture.XXXXXX")"
  mkdir -p "$root/scripts" "$root/bin"
  cp "$UNDER_TEST" "$root/scripts/generate-crds.sh"
  cat >"$root/bin/cargo" <<'INNER'
#!/usr/bin/env bash
[[ -n "${FAKE_CARGO_OUTPUT:-}" ]] && cat "$FAKE_CARGO_OUTPUT"
exit "${FAKE_CARGO_EXIT:-0}"
INNER
  chmod +x "$root/bin/cargo"
  echo "$root"
}

# run_script <exit-code-of-cargo> <crd-name>...  — the CRD names that the fake cargo prints.
run_script() {
  local cargo_exit="$1" root output_file name
  shift
  root="$(make_fixture)"
  output_file="$root/cargo-output.yaml"
  : >"$output_file"
  for name in "$@"; do
    crd "$name" >>"$output_file"
  done

  RESULT_ROOT="$root"
  RESULT_EXIT=0
  FAKE_CARGO_OUTPUT="$output_file" FAKE_CARGO_EXIT="$cargo_exit" PATH="$root/bin:$PATH" \
    bash "$root/scripts/generate-crds.sh" >"$WORK_DIR/out.log" 2>&1 || RESULT_EXIT=$?
  RESULT_OUTPUT="$(cat "$WORK_DIR/out.log")"
}

check() {
  local desc="$1" ok="$2"
  if [[ "$ok" == yes ]]; then
    echo "ok: $desc"
    pass_count=$((pass_count + 1))
  else
    echo "FAIL: $desc"
    printf '%s\n' "$RESULT_OUTPUT" | sed 's/^/      /'
    fail_count=$((fail_count + 1))
  fi
}

exit_is() { [[ "$RESULT_EXIT" == "$1" ]] && echo yes || echo no; }
output_has() { grep -qE -- "$1" <<<"$RESULT_OUTPUT" && echo yes || echo no; }
file_exists() { [[ -s "$RESULT_ROOT/charts/servarr-crds/templates/$1" ]] && echo yes || echo no; }

APP=servarrapps.servarr.dev
STACK=mediastacks.servarr.dev

run_script 0 "$APP" "$STACK"
check "both known CRDs exit zero" "$(exit_is 0)"
check "both known CRDs write the app chart file" "$(file_exists servarrapp-crd.yaml)"
check "both known CRDs write the stack chart file" "$(file_exists mediastack-crd.yaml)"

run_script 0 "$APP" "$STACK" "extra.servarr.dev"
check "a third CRD exits non-zero" "$(exit_is 1)"
check "a third CRD is named in the error" "$(output_has "unknown CRD 'extra.servarr.dev'")"

run_script 0 "$APP" "unknown.servarr.dev"
check "an unknown CRD exits non-zero" "$(exit_is 1)"
check "an unknown CRD is named in the error" "$(output_has "unknown CRD 'unknown.servarr.dev'")"

run_script 0 "$APP"
check "a missing CRD exits non-zero" "$(exit_is 1)"
check "a missing CRD reports the count" "$(output_has "expected 2 CRDs.*generated 1")"

run_script 0
check "no CRDs exits non-zero" "$(exit_is 1)"

run_script 7 "$APP" "$STACK"
check "a failed build exits non-zero" "$(exit_is 7)"

echo
echo "passed $pass_count, failed $fail_count"
[[ "$fail_count" -eq 0 ]]
