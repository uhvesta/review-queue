#!/usr/bin/env bash
# Deterministic, opt-in acceptance harness for an already-built macOS app.
# It does not sign, notarize, authenticate, or read any production Keychain
# service. --keychain runs two disposable-service phases through the signed
# packaged executable. --lifecycle runs two disposable database/repository
# phases through separate executable processes, proving lifecycle persistence
# and source immutability across restart.
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"

app_path=""
run_keychain=0
run_lifecycle=0
launch=0
lifecycle_evidence=""

usage() {
  echo "usage: $0 --app <Review Queue.app> [--keychain] [--lifecycle [--lifecycle-evidence <new-file>]] [--launch]" >&2
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
    --lifecycle)
      run_lifecycle=1
      shift
      ;;
    --lifecycle-evidence)
      lifecycle_evidence="${2:-}"
      shift 2
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
if [[ -n "$lifecycle_evidence" && "$run_lifecycle" -ne 1 ]]; then
  echo "--lifecycle-evidence requires --lifecycle" >&2
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

if [[ "$run_lifecycle" -eq 1 ]]; then
  lifecycle_root="$(mktemp -d "${TMPDIR:-/tmp}/review-queue-lifecycle-acceptance.XXXXXX")"
  cleanup_lifecycle() {
    case "$(basename -- "$lifecycle_root")" in
      review-queue-lifecycle-acceptance.*)
        rm -rf -- "$lifecycle_root"
        ;;
      *)
        echo "refusing to remove unexpected lifecycle acceptance directory: $lifecycle_root" >&2
        ;;
    esac
  }
  trap cleanup_lifecycle EXIT
  phase_one="$("$binary_path" --acceptance-lifecycle-phase-one "$lifecycle_root")"
  phase_two="$("$binary_path" --acceptance-lifecycle-phase-two "$lifecycle_root")"
  printf '%s\n' "$phase_one"
  printf '%s\n' "$phase_two"
  if [[ -n "$lifecycle_evidence" ]]; then
    if [[ -e "$lifecycle_evidence" || -L "$lifecycle_evidence" ]]; then
      echo "refusing to overwrite lifecycle evidence: $lifecycle_evidence" >&2
      exit 1
    fi
    mkdir -p "$(dirname -- "$lifecycle_evidence")"
    {
      printf '%s\n' "$phase_one"
      printf '%s\n' "$phase_two"
    } > "$lifecycle_evidence"
  fi
  cleanup_lifecycle
  trap - EXIT
  echo "packaged-app restart-separated lifecycle acceptance passed"
fi
