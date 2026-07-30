#!/usr/bin/env bash
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
scanner="$script_dir/scan-release-secrets.sh"
fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/review-queue-secret-scan-test.XXXXXX")"
trap 'rm -rf "$fixture_root"' EXIT

fail() {
  echo "test failure: $*" >&2
  exit 1
}

assert_clear() {
  local target="$1"
  local output
  if ! output="$("$scanner" "$target" 2>&1)"; then
    printf '%s\n' "$output" >&2
    fail "clear fixture was rejected: $target"
  fi
  [[ "$output" == "release secret scan passed" ]] ||
    fail "clear scan emitted unexpected output"
}

assert_rejected_without_echo() {
  local target="$1"
  local marker="$2"
  local output
  if output="$("$scanner" "$target" 2>&1)"; then
    fail "credential fixture unexpectedly passed: $target"
  fi
  [[ "$output" == *"possible credential material in"* ]] ||
    fail "credential fixture did not report the affected path"
  [[ "$output" != *"$marker"* ]] ||
    fail "credential fixture was echoed into scan output"
}

fake_session_key="ASIA0000000000000000"
mkdir -p "$fixture_root/clear" "$fixture_root/leak"
printf '%s\n' "Review Queue release fixture" > "$fixture_root/clear/readme.txt"
printf '%s\n' "$fake_session_key" > "$fixture_root/leak/credential.txt"

assert_clear "$fixture_root/clear"
assert_rejected_without_echo "$fixture_root/leak" "$fake_session_key"

# GitHub's stock macOS runner does not include ripgrep. Exercise the portable
# grep fallback even on developer machines where rg is installed.
fallback_output="$(
  REVIEW_QUEUE_SCAN_FORCE_GREP=1 "$scanner" "$fixture_root/clear" 2>&1
)" || fail "grep fallback rejected the clear fixture"
[[ "$fallback_output" == "release secret scan passed" ]] ||
  fail "grep fallback emitted unexpected clear-scan output"
if fallback_output="$(
  REVIEW_QUEUE_SCAN_FORCE_GREP=1 "$scanner" "$fixture_root/leak" 2>&1
)"; then
  fail "grep fallback accepted the credential fixture"
fi
[[ "$fallback_output" == *"possible credential material in"* ]] ||
  fail "grep fallback did not report the affected path"
[[ "$fallback_output" != *"$fake_session_key"* ]] ||
  fail "grep fallback echoed credential material"

tar -czf "$fixture_root/clear.app.tar.gz" -C "$fixture_root" clear
tar -czf "$fixture_root/leak.app.tar.gz" -C "$fixture_root" leak
assert_clear "$fixture_root/clear.app.tar.gz"
assert_rejected_without_echo "$fixture_root/leak.app.tar.gz" "$fake_session_key"

if command -v ditto >/dev/null 2>&1; then
  ditto -c -k --keepParent "$fixture_root/clear" "$fixture_root/clear.app.zip"
  ditto -c -k --keepParent "$fixture_root/leak" "$fixture_root/leak.app.zip"
  assert_clear "$fixture_root/clear.app.zip"
  assert_rejected_without_echo "$fixture_root/leak.app.zip" "$fake_session_key"
fi

if command -v hdiutil >/dev/null 2>&1; then
  hdiutil create -quiet -volname "Review Queue scan clear" \
    -srcfolder "$fixture_root/clear" -format UDZO "$fixture_root/clear.dmg"
  hdiutil create -quiet -volname "Review Queue scan leak" \
    -srcfolder "$fixture_root/leak" -format UDZO "$fixture_root/leak.dmg"
  assert_clear "$fixture_root/clear.dmg"
  assert_rejected_without_echo "$fixture_root/leak.dmg" "$fake_session_key"
fi

echo "release secret scan fixture tests passed"
