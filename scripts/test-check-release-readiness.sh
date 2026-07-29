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

release_fixture="$fixture_root/release-guard"
mkdir -p "$release_fixture/scripts" "$release_fixture/bin"
cp "$script_dir/release-macos.sh" "$release_fixture/scripts/release-macos.sh"
cat > "$release_fixture/scripts/check-release-readiness.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${REVIEW_QUEUE_UPDATER_ACCEPTANCE_BOOTSTRAP:-unset}" != "${EXPECTED_BOOTSTRAP_ENV:?}" ]]; then
  echo "unexpected bootstrap environment: ${REVIEW_QUEUE_UPDATER_ACCEPTANCE_BOOTSTRAP:-unset}" >&2
  exit 90
fi
exit 91
SH
chmod +x "$release_fixture/scripts/check-release-readiness.sh"
cat > "$release_fixture/bin/git" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  *"status --porcelain") exit 0 ;;
  *"show-ref --verify --quiet refs/tags/"*)
    [[ "${GIT_STUB_MODE:-ok}" != "missing-tag" ]]
    ;;
  *"rev-parse "*"^{commit}") printf '%s\n' "acceptance-commit" ;;
  *"rev-parse HEAD")
    if [[ "${GIT_STUB_MODE:-ok}" == "mismatched-head" ]]; then
      printf '%s\n' "different-commit"
    else
      printf '%s\n' "acceptance-commit"
    fi
    ;;
  *) echo "unexpected git invocation: $*" >&2; exit 92 ;;
esac
SH
chmod +x "$release_fixture/bin/git"

assert_release_gate_env() {
  local expected_env="$1"
  shift
  local output
  local status
  set +e
  output="$(
    PATH="$release_fixture/bin:$PATH" \
      EXPECTED_BOOTSTRAP_ENV="$expected_env" \
      REVIEW_QUEUE_UPDATER_ACCEPTANCE_BOOTSTRAP=1 \
      "$release_fixture/scripts/release-macos.sh" \
      --profile unused \
      --version 0.1.0 \
      --channel stable \
      "$@" 2>&1
  )"
  status=$?
  set -e
  [[ "$status" -eq 91 ]] || {
    printf '%s\n' "$output" >&2
    fail "release entrypoint did not reach the gate with bootstrap environment $expected_env"
  }
}

assert_release_fails_with() {
  local expected="$1"
  shift
  local output
  if output="$(
    PATH="$release_fixture/bin:$PATH" \
      "$release_fixture/scripts/release-macos.sh" \
      --profile unused \
      --version 0.1.0 \
      --channel stable \
      "$@" 2>&1
  )"; then
    fail "release entrypoint unexpectedly passed: $output"
  fi
  [[ "$output" == *"$expected"* ]] || {
    printf '%s\n' "$output" >&2
    fail "release failure did not contain: $expected"
  }
}

# An inherited variable must not exempt the real stable tag. Only the
# constrained command-line flag may select the one-row bootstrap gate.
assert_release_gate_env 0 --release-tag v0.1.0
assert_release_gate_env 1 \
  --release-tag updater-acceptance-v0.1.0 \
  --updater-acceptance-bootstrap

assert_release_fails_with \
  "requires --channel stable --release-tag updater-acceptance-v0.1.0" \
  --release-tag v0.1.0 \
  --updater-acceptance-bootstrap
assert_release_fails_with \
  "stable release requires --release-tag v0.1.0" \
  --release-tag another-tag
assert_release_fails_with \
  "require a clean tagged worktree" \
  --release-tag v0.1.0 \
  --allow-dirty
GIT_STUB_MODE=missing-tag assert_release_fails_with \
  "stable release tag does not exist locally: v0.1.0" \
  --release-tag v0.1.0
GIT_STUB_MODE=mismatched-head assert_release_fails_with \
  "stable release tag v0.1.0 does not point to HEAD" \
  --release-tag v0.1.0

# A release rerun must not silently delete an earlier artifact before signing
# credentials or Git state are consulted.
occupied_output="$release_fixture/occupied-output"
mkdir -p "$occupied_output"
: > "$occupied_output/Review-Queue-0.1.0-universal-candidate.dmg"
if output="$(
  "$release_fixture/scripts/release-macos.sh" \
    --profile unused \
    --version 0.1.0 \
    --channel candidate \
    --release-tag candidate-test \
    --output "$occupied_output" \
    --allow-dirty 2>&1
)"; then
  fail "release entrypoint overwrote an existing artifact"
fi
[[ "$output" == *"refusing to overwrite existing artifact"* ]] || {
  printf '%s\n' "$output" >&2
  fail "release entrypoint did not reject the existing artifact"
}
[[ -f "$occupied_output/Review-Queue-0.1.0-universal-candidate.dmg" ]] ||
  fail "release entrypoint removed the existing artifact"

echo "check-release-readiness fixture tests passed"
