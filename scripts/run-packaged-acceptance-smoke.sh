#!/usr/bin/env bash
# Deterministic, opt-in acceptance harness for an already-built macOS app.
# It does not sign, notarize, authenticate, or read any production Keychain
# service. --keychain runs two disposable-service phases through the signed
# packaged executable, proving persistence across process restart.
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"

app_path=""
run_keychain=0
launch=0

usage() {
  echo "usage: $0 --app <Review Queue.app> [--keychain] [--launch]" >&2
}

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --app)
      app_path="${2:-}"
      shift 2
      ;;
    --keychain)
      run_keychain=1
      shift
      ;;
    --launch)
      launch=1
      shift
      ;;
    *)
      usage
      exit 64
      ;;
  esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "packaged-app acceptance must run on macOS" >&2
  exit 69
fi
if [[ -z "$app_path" || ! -d "$app_path" ]]; then
  usage
  exit 64
fi

info_plist="$app_path/Contents/Info.plist"
if [[ ! -f "$info_plist" ]]; then
  echo "invalid app bundle: Contents/Info.plist is missing" >&2
  exit 1
fi
executable="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$info_plist")"
binary_path="$app_path/Contents/MacOS/$executable"
if [[ ! -x "$binary_path" ]]; then
  echo "invalid app bundle: executable is missing" >&2
  exit 1
fi

codesign --verify --deep --strict --verbose=2 "$app_path"
spctl --assess --type execute --verbose=2 "$app_path"
echo "bundle signature and Gatekeeper assessment passed"

if [[ "$launch" -eq 1 ]]; then
  "$binary_path" >/dev/null 2>&1 &
  app_pid=$!
  trap 'kill "$app_pid" 2>/dev/null || true' EXIT
  for _ in 1 2 3 4 5; do
    if kill -0 "$app_pid" 2>/dev/null; then
      break
    fi
    sleep 1
  done
  if ! kill -0 "$app_pid" 2>/dev/null; then
    echo "packaged app exited before the smoke window completed" >&2
    exit 1
  fi
  kill "$app_pid" 2>/dev/null || true
  wait "$app_pid" 2>/dev/null || true
  trap - EXIT
  echo "packaged app launch smoke passed"
fi

if [[ "$run_keychain" -eq 1 ]]; then
  acceptance_service="com.reviewqueue.desktop.acceptance.$$.${RANDOM}"
  "$binary_path" --acceptance-keychain-write "$acceptance_service"
  "$binary_path" --acceptance-keychain-read-delete "$acceptance_service"
  echo "signed-app disposable Keychain restart and account separation passed"
fi
