#!/usr/bin/env bash
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"
matrix_path="$repo_root/docs/test-matrix.md"

if [[ ! -f "$matrix_path" ]]; then
  echo "release gate failed: docs/test-matrix.md is missing" >&2
  exit 1
fi

failed_rows="$(
  awk -F '|' '
    /^\|/ {
      status = $3
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", status)
      if (status == "not started" || status == "blocked" || status == "failing") {
        flow = $2
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", flow)
        print flow " (" status ")"
      }
    }
  ' "$matrix_path"
)"

if [[ -n "$failed_rows" ]]; then
  echo "stable release gate failed; the acceptance matrix still has non-passing flows:" >&2
  while IFS= read -r row; do
    echo "  - $row" >&2
  done <<< "$failed_rows"
  exit 1
fi

unknown_statuses="$(
  awk -F '|' '
    /^\|/ {
      status = $3
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", status)
      if (
        status != "Status" &&
        status !~ /^-+$/ &&
        status != "passing" &&
        status != "passing with limitation" &&
        status != "not started" &&
        status != "blocked" &&
        status != "failing"
      ) {
        print status
      }
    }
  ' "$matrix_path"
)"

if [[ -n "$unknown_statuses" ]]; then
  echo "stable release gate failed; unknown acceptance-matrix status:" >&2
  while IFS= read -r status; do
    echo "  - $status" >&2
  done <<< "$unknown_statuses"
  exit 1
fi

incomplete_rows="$(
  awk -F '|' '
    BEGIN { expected_columns = 11 }
    /^\|/ {
      flow = $2
      status = $3
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", flow)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", status)
      if (flow == "Flow" || flow ~ /^-+$/) next
      if (NF != expected_columns) {
        print flow " (expected 9 evidence fields)"
        next
      }
      labels[4] = "Setup"
      labels[5] = "Action"
      labels[6] = "Expected"
      labels[7] = "Actual"
      labels[8] = "Evidence"
      labels[9] = "Restart"
      labels[10] = "Recovery"
      for (i = 4; i <= 10; i++) {
        value = $i
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
        lower = tolower(value)
        if (
          value == "" ||
          lower == "n/a" ||
          lower == "none" ||
          lower == "todo" ||
          lower == "tbd" ||
          lower ~ /not started/ ||
          lower ~ /blocked/
        ) {
          print flow " (" labels[i] " missing)"
        }
      }
    }
  ' "$matrix_path"
)"

if [[ -n "$incomplete_rows" ]]; then
  echo "stable release gate failed; acceptance evidence is incomplete:" >&2
  while IFS= read -r row; do
    echo "  - $row" >&2
  done <<< "$incomplete_rows"
  exit 1
fi

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
for required_flow in "${required_flows[@]}"; do
  if ! awk -F '|' -v wanted="$required_flow" '
    /^\|/ {
      flow = $2
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", flow)
      if (flow == wanted) found = 1
    }
    END { exit found ? 0 : 1 }
  ' "$matrix_path"; then
    echo "stable release gate failed: required flow is absent: $required_flow" >&2
    exit 1
  fi
done

echo "stable release gate passed: every required flow has complete passing evidence"
