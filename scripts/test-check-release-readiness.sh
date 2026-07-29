#!/usr/bin/env bash
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
gate="$script_dir/check-release-readiness.sh"
fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/review-queue-readiness.XXXXXX")"
trap 'rm -rf "$fixture_root"' EXIT

required_flows=(
  "Packaged app / Gatekeeper"
  "Keychain signed-app integration"
  "OAuth Device Flow / restart"
  "Local multi-repository commit-on-submit"
  "Connected-machine immutable review"
  "Copilot streaming / cancel / no-replay"
  "GitHub mirror / imported discussion"
  "GitHub exact publish"
  "Manual originating-agent handoff"
  "Signed updater / relaunch"
  "Token-leak artifact scan"
)

fail() {
  echo "test failure: $*" >&2
  exit 1
}

write_valid_fixture() {
  local name="$1"
  local fixture="$fixture_root/$name"
  local matrix="$fixture/docs/test-matrix.md"
  local flow
  mkdir -p "$fixture/docs/evidence"
  : > "$fixture/docs/evidence/retained.log"
  {
    printf '%s\n' '# Product test matrix'
    printf '%s\n' '| Flow | Status | Setup | Action | Expected | Actual | Evidence | Restart | Recovery |'
    printf '%s\n' '| --- | --- | --- | --- | --- | --- | --- | --- | --- |'
    for flow in "${required_flows[@]}"; do
      printf '| %s | passing | setup | action | expected | actual | [retained evidence](evidence/retained.log) | restart | recovery |\n' "$flow"
    done
  } > "$matrix"
  printf '%s\n' "$matrix"
}

assert_gate_passes() {
  local matrix="$1"
  local output
  if ! output="$(REVIEW_QUEUE_MATRIX_PATH="$matrix" "$gate" 2>&1)"; then
    printf '%s\n' "$output" >&2
    fail "gate unexpectedly failed"
  fi
  [[ "$output" == *"stable release gate passed"* ]] || fail "pass output was not reported"
}

assert_gate_fails() {
  local matrix="$1"
  local expected="$2"
  local output
  if output="$(REVIEW_QUEUE_MATRIX_PATH="$matrix" "$gate" 2>&1)"; then
    fail "gate unexpectedly passed: $output"
  fi
  [[ "$output" == *"$expected"* ]] || {
    printf '%s\n' "$output" >&2
    fail "failure output did not contain: $expected"
  }
}

matrix="$(write_valid_fixture valid)"
assert_gate_passes "$matrix"

matrix="$(write_valid_fixture missing-target)"
sed -i.bak 's#evidence/retained.log#evidence/missing.log#' "$matrix"
rm "$matrix.bak"
assert_gate_fails "$matrix" "Evidence link target is missing"

matrix="$(write_valid_fixture placeholder)"
sed -i.bak 's#\[retained evidence\](evidence/retained.log)#awaiting live capture [retained evidence](evidence/retained.log)#' "$matrix"
rm "$matrix.bak"
assert_gate_fails "$matrix" "Evidence contains placeholder language"

matrix="$(write_valid_fixture malformed-column)"
sed -i.bak 's#| restart | recovery |#| restart |#' "$matrix"
rm "$matrix.bak"
assert_gate_fails "$matrix" "expected 9 evidence fields"

matrix="$(write_valid_fixture malformed-status)"
sed -i.bak '4s/| passing |/| almost passing |/' "$matrix"
rm "$matrix.bak"
assert_gate_fails "$matrix" "unknown acceptance-matrix status"

matrix="$(write_valid_fixture non-passing)"
sed -i.bak '4s/| passing |/| blocked |/' "$matrix"
rm "$matrix.bak"
assert_gate_fails "$matrix" "non-passing flows"

matrix="$(write_valid_fixture updater-bootstrap)"
updater_line="$(awk -F '|' '$2 ~ /Signed updater/ { print NR }' "$matrix")"
sed -i.bak "${updater_line}s/| passing |/| blocked |/" "$matrix"
rm "$matrix.bak"
assert_gate_fails "$matrix" "non-passing flows"
bootstrap_output="$(
  REVIEW_QUEUE_MATRIX_PATH="$matrix" \
    REVIEW_QUEUE_UPDATER_ACCEPTANCE_BOOTSTRAP=1 \
    "$gate"
)"
[[ "$bootstrap_output" == *"updater acceptance bootstrap gate passed"* ]] ||
  fail "bootstrap pass output was not reported"

matrix="$(write_valid_fixture updater-bootstrap-other-failure)"
sed -i.bak '4s/| passing |/| blocked |/' "$matrix"
rm "$matrix.bak"
if REVIEW_QUEUE_MATRIX_PATH="$matrix" REVIEW_QUEUE_UPDATER_ACCEPTANCE_BOOTSTRAP=1 "$gate" >/dev/null 2>&1; then
  fail "bootstrap gate bypassed a non-updater failure"
fi

echo "check-release-readiness fixture tests passed"
