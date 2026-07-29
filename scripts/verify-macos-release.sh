#!/usr/bin/env bash
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "macOS release verification must run on macOS" >&2
  exit 69
fi

dmg_path=""
app_path=""
checksums_path=""
updater_path=""
updater_signature_path=""
latest_json_path=""
version=""

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --dmg)
      dmg_path="${2:-}"
      shift 2
      ;;
    --app)
      app_path="${2:-}"
      shift 2
      ;;
    --checksums)
      checksums_path="${2:-}"
      shift 2
      ;;
    --updater)
      updater_path="${2:-}"
      shift 2
      ;;
    --updater-signature)
      updater_signature_path="${2:-}"
      shift 2
      ;;
    --latest-json)
      latest_json_path="${2:-}"
      shift 2
      ;;
    --version)
      version="${2:-}"
      shift 2
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 64
      ;;
  esac
done

if [[
  ! -d "$app_path" ||
    ! -f "$dmg_path" ||
    ! -f "$checksums_path" ||
    ! -f "$updater_path" ||
    ! -f "$updater_signature_path" ||
    ! -f "$latest_json_path" ||
    -z "$version"
]]; then
  echo "usage: $0 --app <Review Queue.app> --dmg <release.dmg> --checksums <SHA256SUMS> --updater <app.tar.gz> --updater-signature <app.tar.gz.sig> --latest-json <latest.json> --version <semver>" >&2
  exit 64
fi

executable="$(
  /usr/libexec/PlistBuddy -c "Print :CFBundleExecutable" \
    "$app_path/Contents/Info.plist"
)"
binary_path="$app_path/Contents/MacOS/$executable"
architectures="$(lipo -archs "$binary_path")"

if [[ " $architectures " != *" arm64 "* || " $architectures " != *" x86_64 "* ]]; then
  echo "universal binary verification failed: found '$architectures'" >&2
  exit 1
fi

codesign --verify --deep --strict --verbose=2 "$app_path"
codesign --verify --verbose=2 "$dmg_path"
xcrun stapler validate "$app_path"
xcrun stapler validate "$dmg_path"
spctl --assess --type execute --verbose=2 "$app_path"
spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg_path"
hdiutil verify "$dmg_path"

(
  cd "$(dirname -- "$checksums_path")"
  shasum -a 256 -c "$(basename -- "$checksums_path")"
)

cargo run \
  --quiet \
  --locked \
  --manifest-path "$repo_root/src-tauri/Cargo.toml" \
  --example verify-updater-signature \
  -- \
  "$updater_path" \
  "$updater_signature_path" \
  "$repo_root/src-tauri/updater.pub"

node - \
  "$latest_json_path" \
  "$updater_signature_path" \
  "$updater_path" \
  "$version" <<'NODE'
const fs = require("fs");
const path = require("path");
const [manifestPath, signaturePath, updaterPath, expectedVersion] =
  process.argv.slice(2);
const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
const signature = fs.readFileSync(signaturePath, "utf8").trim();
if (manifest.version !== expectedVersion) {
  throw new Error(`latest.json version mismatch: ${manifest.version}`);
}
for (const target of ["darwin-aarch64", "darwin-x86_64"]) {
  const platform = manifest.platforms?.[target];
  if (!platform || platform.signature !== signature) {
    throw new Error(`latest.json signature mismatch for ${target}`);
  }
  const url = new URL(platform.url);
  if (
    url.protocol !== "https:" ||
    url.hostname !== "github.com" ||
    !url.pathname.startsWith("/uhvesta/review-queue/releases/download/") ||
    decodeURIComponent(path.basename(url.pathname)) !== path.basename(updaterPath)
  ) {
    throw new Error(`latest.json updater URL is invalid for ${target}`);
  }
}
NODE

mount_dir="$(mktemp -d "${TMPDIR:-/tmp}/review-queue-verify.XXXXXX")"
mounted=0
cleanup() {
  if [[ "$mounted" -eq 1 ]]; then
    hdiutil detach "$mount_dir" -quiet || true
  fi
  rm -rf "$mount_dir"
}
trap cleanup EXIT

hdiutil attach "$dmg_path" -nobrowse -readonly -mountpoint "$mount_dir" -quiet
mounted=1
mounted_app="$mount_dir/Review Queue.app"

if [[ ! -d "$mounted_app" ]]; then
  echo "DMG verification failed: Review Queue.app is missing" >&2
  exit 1
fi

codesign --verify --deep --strict --verbose=2 "$mounted_app"
xcrun stapler validate "$mounted_app"

echo "macOS release verification passed ($architectures)"
