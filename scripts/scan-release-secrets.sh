#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -eq 0 ]]; then
  echo "usage: $0 <artifact-or-directory> [...]" >&2
  exit 64
fi

if ! command -v rg >/dev/null 2>&1; then
  echo "release secret scan requires ripgrep (rg)" >&2
  exit 69
fi

# Never print the matched material: release logs must not become a second leak.
credential_pattern='(github_pat_[A-Za-z0-9_]{20,}|gh[pousr]_[A-Za-z0-9_]{20,}|AKIA[0-9A-Z]{16}|ASIA[0-9A-Z]{16}|sk-[A-Za-z0-9_-]{20,}|Bearer[[:space:]]+[A-Za-z0-9._~+/-]{20,}|-----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY-----)'

scan_tmp="$(mktemp -d "${TMPDIR:-/tmp}/review-queue-secret-scan.XXXXXX")"
mounted_images=()
cleanup() {
  local mountpoint
  for mountpoint in "${mounted_images[@]}"; do
    hdiutil detach "$mountpoint" -quiet >/dev/null 2>&1 || true
  done
  rm -rf "$scan_tmp"
}
trap cleanup EXIT

scan_file() {
  local file="$1"
  if LC_ALL=C strings -a "$file" 2>/dev/null | rg -q "$credential_pattern"; then
    echo "release secret scan failed: possible credential material in $file" >&2
    return 1
  fi
}

scan_tree() {
  local directory="$1"
  while IFS= read -r -d '' file; do
    scan_path "$file"
  done < <(find "$directory" -type f -print0)
}

scan_archive() {
  local archive="$1"
  local format="$2"
  local extracted
  extracted="$(mktemp -d "$scan_tmp/archive.XXXXXX")"
  case "$format" in
    zip)
      ditto -x -k "$archive" "$extracted"
      ;;
    tar-gzip)
      tar -xzf "$archive" -C "$extracted"
      ;;
  esac
  scan_tree "$extracted"
}

scan_disk_image() {
  local image="$1"
  local mountpoint
  mountpoint="$(mktemp -d "$scan_tmp/dmg.XXXXXX")"
  hdiutil attach -readonly -nobrowse -mountpoint "$mountpoint" "$image" >/dev/null
  mounted_images+=("$mountpoint")
  scan_tree "$mountpoint"
  hdiutil detach "$mountpoint" -quiet
  mounted_images=("${mounted_images[@]:0:${#mounted_images[@]}-1}")
}

scan_path() {
  local path="$1"
  if [[ -d "$path" ]]; then
    scan_tree "$path"
    return
  fi

  # Scan the payload of compressed release containers. Scanning their raw
  # compressed bytes produces credential-shaped random strings and therefore
  # false positives without inspecting the files users actually receive.
  case "$path" in
    *.app.zip|*.zip)
      scan_archive "$path" zip
      ;;
    *.app.tar.gz|*.tar.gz|*.tgz)
      scan_archive "$path" tar-gzip
      ;;
    *.dmg)
      scan_disk_image "$path"
      ;;
    *)
      scan_file "$path"
      ;;
  esac
}

for target in "$@"; do
  if [[ ! -e "$target" ]]; then
    echo "release secret scan target does not exist: $target" >&2
    exit 66
  fi

  scan_path "$target"
done

echo "release secret scan passed"
