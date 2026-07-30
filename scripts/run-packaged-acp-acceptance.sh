#!/usr/bin/env bash
# Restart-separated ACP acceptance through the already-built production app
# executable. All mutable state is disposable; the retained JSONL contains
# summary evidence only and never copies the SQLite database or fake-agent
# dedupe state.
set -euo pipefail

app_path=""
evidence_path=""

usage() {
  echo "usage: $0 --app <Review Queue.app> --evidence <new-jsonl-file>" >&2
}

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --app)
      app_path="${2:-}"
      shift 2
      ;;
    --evidence)
      evidence_path="${2:-}"
      shift 2
      ;;
    *)
      usage
      exit 64
      ;;
  esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "packaged ACP acceptance must run on macOS" >&2
  exit 69
fi
if [[ -z "$app_path" || ! -d "$app_path" || -z "$evidence_path" ]]; then
  usage
  exit 64
fi
if [[ -e "$evidence_path" || -L "$evidence_path" ]]; then
  echo "refusing to overwrite ACP acceptance evidence: $evidence_path" >&2
  exit 1
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

acp_root="$(mktemp -d "${TMPDIR:-/tmp}/review-queue-acp-acceptance.XXXXXX")"
cleanup_acp() {
  case "$(basename -- "$acp_root")" in
    review-queue-acp-acceptance.*)
      rm -rf -- "$acp_root"
      ;;
    *)
      echo "refusing to remove unexpected ACP acceptance directory: $acp_root" >&2
      ;;
  esac
}
trap cleanup_acp EXIT

phase_one="$("$binary_path" --acceptance-acp-phase-one "$acp_root")"
phase_two="$("$binary_path" --acceptance-acp-phase-two "$acp_root")"
printf '%s\n' "$phase_one"
printf '%s\n' "$phase_two"

mkdir -p "$(dirname -- "$evidence_path")"
umask 077
{
  printf '%s\n' "$phase_one"
  printf '%s\n' "$phase_two"
} > "$evidence_path"

cleanup_acp
trap - EXIT
echo "packaged-app restart-separated ACP acceptance passed; evidence: $evidence_path"
