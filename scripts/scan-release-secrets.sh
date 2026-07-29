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

scan_file() {
  local file="$1"
  if LC_ALL=C strings -a "$file" 2>/dev/null | rg -q "$credential_pattern"; then
    echo "release secret scan failed: possible credential material in $file" >&2
    return 1
  fi
}

for target in "$@"; do
  if [[ ! -e "$target" ]]; then
    echo "release secret scan target does not exist: $target" >&2
    exit 66
  fi

  if [[ -d "$target" ]]; then
    while IFS= read -r -d '' file; do
      scan_file "$file"
    done < <(find "$target" -type f -print0)
  else
    scan_file "$target"
  fi
done

echo "release secret scan passed"
