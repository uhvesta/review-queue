#!/usr/bin/env bash
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"
matrix_path="${REVIEW_QUEUE_MATRIX_PATH:-$repo_root/docs/test-matrix.md}"
updater_acceptance_bootstrap="${REVIEW_QUEUE_UPDATER_ACCEPTANCE_BOOTSTRAP:-0}"

if [[ "$updater_acceptance_bootstrap" != "0" && "$updater_acceptance_bootstrap" != "1" ]]; then
  echo "release gate failed: REVIEW_QUEUE_UPDATER_ACCEPTANCE_BOOTSTRAP must be 0 or 1" >&2
  exit 64
fi

if [[ ! -f "$matrix_path" ]]; then
  echo "release gate failed: docs/test-matrix.md is missing" >&2
  exit 1
fi
matrix_dir="$(CDPATH= cd -- "$(dirname -- "$matrix_path")" && pwd)"

failed_rows="$(
  awk -F '|' -v updater_bootstrap="$updater_acceptance_bootstrap" '
    /^\|/ {
      status = $3
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", status)
      flow = $2
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", flow)
      if (updater_bootstrap == "1" && flow == "Signed updater / relaunch") next
      if (status == "not started" || status == "blocked" || status == "failing") {
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
      if (status != "Status" && status !~ /^-+$/ && status != "passing" && status != "passing with limitation" && status != "not started" && status != "blocked" && status != "failing") {
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
  awk -F '|' -v updater_bootstrap="$updater_acceptance_bootstrap" '
    BEGIN { expected_columns = 11 }
    /^\|/ {
      flow = $2
      status = $3
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", flow)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", status)
      if (flow == "Flow" || flow ~ /^-+$/) next
      if (updater_bootstrap == "1" && flow == "Signed updater / relaunch") next
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
        if (value == "" || lower == "n/a" || lower == "none" || lower == "todo" || lower == "tbd" || lower ~ /not started/ || lower ~ /blocked/) {
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
  "Connected-machine SSH tunnel"
  "Copilot streaming / cancel / no-replay"
  "GitHub mirror / imported discussion"
  "GitHub exact publish"
  "ACP exact delivery / revisions"
  "Packaged lifecycle / purge / resubmit"
  "CLI ingestion parity"
  "Error and recovery contract"
  "Signed updater / relaunch"
  "Token-leak artifact scan"
)

validate_evidence_cell() {
  local flow="$1"
  local evidence="$2"
  local lower
  local remaining
  local link_match
  local link_target
  local saw_local_link=0
  local saw_missing_local_link=0
  local placeholder_pattern='(^|[^[:alnum:]])(pending|await|awaiting|awaits|todo|tbd|not[[:space:]]+started|blocked)([^[:alnum:]]|$)'
  local markdown_link_pattern='\[[^][]+\]\(([^()[:space:]]+)\)'

  lower="$(printf '%s' "$evidence" | tr '[:upper:]' '[:lower:]')"
  if [[ $lower =~ $placeholder_pattern ]]; then
    echo "stable release gate failed: $flow (Evidence contains placeholder language)" >&2
    return 1
  fi

  remaining="$evidence"
  while [[ $remaining =~ $markdown_link_pattern ]]; do
    link_match="${BASH_REMATCH[0]}"
    link_target="${BASH_REMATCH[1]}"
    remaining="${remaining#*"$link_match"}"

    # Evidence is retained with the matrix. External URLs, anchors, and paths
    # that escape the docs directory do not establish that a local artifact
    # exists at release time.
    if [[ "$link_target" == /* ||
          "$link_target" == *:* ||
          "$link_target" == \#* ||
          "$link_target" == *\?* ||
          "$link_target" == *\#* ||
          "$link_target" =~ (^|/)\.\.(/|$) ]]; then
      continue
    fi

    saw_local_link=1
    if [[ -f "$matrix_dir/$link_target" && ! -L "$matrix_dir/$link_target" ]]; then
      return 0
    fi
    saw_missing_local_link=1
  done

  if (( saw_missing_local_link )); then
    echo "stable release gate failed: $flow (Evidence link target is missing or is not a retained regular file)" >&2
  elif (( saw_local_link )); then
    echo "stable release gate failed: $flow (Evidence has no usable retained local artifact link)" >&2
  else
    echo "stable release gate failed: $flow (Evidence must contain a Markdown link to a retained local artifact)" >&2
  fi
  return 1
}

for required_flow in "${required_flows[@]}"; do
  matching_evidence="$(awk -F '|' -v wanted="$required_flow" '
    /^\|/ {
      flow = $2
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", flow)
      if (flow == wanted) {
        evidence = $8
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", evidence)
        print evidence
      }
    }
  ' "$matrix_path")"
  matching_count="$(printf '%s\n' "$matching_evidence" | awk 'NF { count += 1 } END { print count + 0 }')"
  if [[ "$matching_count" -eq 0 ]]; then
    echo "stable release gate failed: required flow is absent: $required_flow" >&2
    exit 1
  fi
  if [[ "$matching_count" -ne 1 ]]; then
    echo "stable release gate failed: required flow is duplicated: $required_flow" >&2
    exit 1
  fi
  if [[ "$updater_acceptance_bootstrap" == "1" && "$required_flow" == "Signed updater / relaunch" ]]; then
    continue
  fi
  if ! validate_evidence_cell "$required_flow" "$matching_evidence"; then
    exit 1
  fi
done

if [[ "$updater_acceptance_bootstrap" == "1" ]]; then
  echo "updater acceptance bootstrap gate passed: every required flow except Signed updater / relaunch has complete passing evidence"
else
  echo "stable release gate passed: every required flow has complete passing evidence"
fi
