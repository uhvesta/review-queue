#!/usr/bin/env bash
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"

profile=""
version=""
channel=""
release_tag=""
output_dir="$repo_root/dist"
identity="${APPLE_SIGNING_IDENTITY:-}"
allow_dirty=0
updater_acceptance_bootstrap=0

usage() {
  echo "usage: $0 --profile <notary-keychain-profile> --version <semver> --channel <nightly|candidate|stable> --release-tag <tag> [--output <dir>] [--identity <Developer ID identity>] [--allow-dirty] [--updater-acceptance-bootstrap]" >&2
}

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --profile)
      profile="${2:-}"
      shift 2
      ;;
    --version)
      version="${2:-}"
      shift 2
      ;;
    --channel)
      channel="${2:-}"
      shift 2
      ;;
    --release-tag)
      release_tag="${2:-}"
      shift 2
      ;;
    --output)
      output_dir="${2:-}"
      shift 2
      ;;
    --identity)
      identity="${2:-}"
      shift 2
      ;;
    --allow-dirty)
      allow_dirty=1
      shift
      ;;
    --updater-acceptance-bootstrap)
      updater_acceptance_bootstrap=1
      shift
      ;;
    *)
      usage
      exit 64
      ;;
  esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "signed Review Queue releases must be built on macOS" >&2
  exit 69
fi

if [[ -z "$profile" || -z "$version" || -z "$channel" || -z "$release_tag" ]]; then
  usage
  exit 64
fi

if [[ "$output_dir" != /* ]]; then
  output_dir="$repo_root/$output_dir"
fi

if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
  echo "invalid semantic version: $version" >&2
  exit 64
fi

if [[ ! "$release_tag" =~ ^[0-9A-Za-z._-]+$ ]]; then
  echo "invalid release tag: $release_tag" >&2
  exit 64
fi

case "$channel" in
  nightly|candidate|stable) ;;
  *)
    echo "invalid release channel: $channel" >&2
    exit 64
    ;;
esac

if [[ "$updater_acceptance_bootstrap" -eq 1 ]]; then
  expected_acceptance_tag="updater-acceptance-v$version"
  if [[ "$channel" != "stable" || "$release_tag" != "$expected_acceptance_tag" ]]; then
    echo "updater acceptance bootstrap requires --channel stable --release-tag $expected_acceptance_tag" >&2
    exit 64
  fi
fi

if [[ "$allow_dirty" -eq 0 && -n "$(git -C "$repo_root" status --porcelain)" ]]; then
  echo "release build refused: the Git worktree is dirty (use --allow-dirty only for an intentional local candidate)" >&2
  exit 1
fi

if [[ "$channel" == "stable" ]]; then
  if [[ "$updater_acceptance_bootstrap" -eq 1 ]]; then
    echo "building disposable updater acceptance feed; every gate except Signed updater / relaunch remains enforced" >&2
    REVIEW_QUEUE_UPDATER_ACCEPTANCE_BOOTSTRAP=1 "$script_dir/check-release-readiness.sh"
  else
    "$script_dir/check-release-readiness.sh"
  fi
fi

if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
  echo "TAURI_SIGNING_PRIVATE_KEY must contain the encrypted updater private key" >&2
  exit 78
fi
if [[ -z "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" ]]; then
  echo "TAURI_SIGNING_PRIVATE_KEY_PASSWORD must contain the updater key password" >&2
  exit 78
fi

updater_public_key_path="$repo_root/src-tauri/updater.pub"
if [[ ! -f "$updater_public_key_path" ]]; then
  echo "updater public key is missing: $updater_public_key_path" >&2
  exit 1
fi

required_commands=(
  cargo
  codesign
  hdiutil
  lipo
  node
  npm
  rustup
  security
  shasum
  tar
  xcrun
)
for command_name in "${required_commands[@]}"; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "required release command is unavailable: $command_name" >&2
    exit 69
  fi
done

configured_updater_key="$(
  node -e 'console.log(JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).plugins.updater.pubkey)' \
    "$repo_root/src-tauri/tauri.conf.json"
)"
committed_updater_key="$(tr -d '\r\n' < "$updater_public_key_path")"
if [[ "$configured_updater_key" != "$committed_updater_key" ]]; then
  echo "updater public key mismatch between tauri.conf.json and updater.pub" >&2
  exit 1
fi

if ! cargo tauri --version >/dev/null 2>&1; then
  echo "Tauri CLI is required; install the locked CLI with: cargo install tauri-cli --version '2.11.4' --locked" >&2
  exit 69
fi

if [[ -z "$identity" ]]; then
  identity="$(
    security find-identity -v -p codesigning 2>/dev/null |
      sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' |
      head -n 1
  )"
fi
if [[ -z "$identity" ]]; then
  echo "no Developer ID Application signing identity is available" >&2
  exit 78
fi

if ! xcrun notarytool history --keychain-profile "$profile" >/dev/null; then
  echo "notary Keychain profile '$profile' is unavailable or invalid" >&2
  exit 78
fi

export APPLE_SIGNING_IDENTITY="$identity"

rustup target add aarch64-apple-darwin x86_64-apple-darwin
npm --prefix "$repo_root/frontend" ci
npm --prefix "$repo_root/frontend" test
npm --prefix "$repo_root/frontend" run build
cargo fmt --manifest-path "$repo_root/Cargo.toml" --all -- --check
cargo test --manifest-path "$repo_root/Cargo.toml" --workspace --locked
cargo clippy --manifest-path "$repo_root/Cargo.toml" --workspace --all-targets --locked -- -D warnings
cargo fmt --manifest-path "$repo_root/src-tauri/Cargo.toml" --all -- --check
cargo test --manifest-path "$repo_root/src-tauri/Cargo.toml" --locked
cargo clippy --manifest-path "$repo_root/src-tauri/Cargo.toml" --all-targets --locked -- -D warnings

version_override="$(printf '{"version":"%s"}' "$version")"
(
  cd "$repo_root/src-tauri"
  cargo tauri build \
    --target universal-apple-darwin \
    --bundles app \
    --config "$version_override"
)

target_root="${CARGO_TARGET_DIR:-$repo_root/src-tauri/target}"
app_path="$target_root/universal-apple-darwin/release/bundle/macos/Review Queue.app"
if [[ ! -d "$app_path" ]]; then
  echo "Tauri build did not produce the expected app: $app_path" >&2
  exit 1
fi

codesign --verify --deep --strict --verbose=2 "$app_path"
executable="$(
  /usr/libexec/PlistBuddy -c "Print :CFBundleExecutable" \
    "$app_path/Contents/Info.plist"
)"
architectures="$(lipo -archs "$app_path/Contents/MacOS/$executable")"
if [[ " $architectures " != *" arm64 "* || " $architectures " != *" x86_64 "* ]]; then
  echo "Tauri did not produce a universal executable: $architectures" >&2
  exit 1
fi

release_tmp="$(mktemp -d "${TMPDIR:-/tmp}/review-queue-release.XXXXXX")"
cleanup() {
  rm -rf "$release_tmp"
}
trap cleanup EXIT

notary_zip="$release_tmp/Review-Queue-notarization.zip"
ditto -c -k --keepParent "$app_path" "$notary_zip"
app_notary_result="$release_tmp/app-notary.json"
notary_wait_timeout="${NOTARYTOOL_WAIT_TIMEOUT:-30m}"
xcrun notarytool submit "$notary_zip" \
  --keychain-profile "$profile" \
  --wait \
  --timeout "$notary_wait_timeout" \
  --no-s3-acceleration \
  --output-format json > "$app_notary_result"
app_notary_status="$(
  node -e 'const fs=require("fs"); console.log(JSON.parse(fs.readFileSync(process.argv[1],"utf8")).status)' "$app_notary_result"
)"
if [[ "$app_notary_status" != "Accepted" ]]; then
  echo "app notarization failed with status: $app_notary_status" >&2
  exit 1
fi
xcrun stapler staple "$app_path"
xcrun stapler validate "$app_path"

mkdir -p "$output_dir"
dmg_name="Review-Queue-${version}-universal-${channel}.dmg"
zip_name="Review-Queue-${version}-universal-${channel}.app.zip"
updater_name="Review-Queue-${version}-universal-${channel}.app.tar.gz"
dmg_path="$output_dir/$dmg_name"
zip_path="$output_dir/$zip_name"
updater_path="$output_dir/$updater_name"
updater_signature_path="$updater_path.sig"
latest_json_path="$output_dir/latest.json"
checksums_path="$output_dir/SHA256SUMS"
sbom_path="$output_dir/Review-Queue-${version}.cdx.json"
notarization_path="$output_dir/Review-Queue-${version}.notarization.json"

staging_dir="$release_tmp/dmg"
mkdir -p "$staging_dir"
ditto "$app_path" "$staging_dir/Review Queue.app"
ln -s /Applications "$staging_dir/Applications"
rm -f \
  "$dmg_path" \
  "$zip_path" \
  "$updater_path" \
  "$updater_signature_path" \
  "$latest_json_path" \
  "$checksums_path" \
  "$sbom_path" \
  "$notarization_path"
hdiutil create \
  -volname "Review Queue" \
  -srcfolder "$staging_dir" \
  -ov \
  -format UDZO \
  "$dmg_path"
codesign --force --timestamp --sign "$identity" "$dmg_path"

dmg_notary_result="$release_tmp/dmg-notary.json"
xcrun notarytool submit "$dmg_path" \
  --keychain-profile "$profile" \
  --wait \
  --timeout "$notary_wait_timeout" \
  --no-s3-acceleration \
  --output-format json > "$dmg_notary_result"
dmg_notary_status="$(
  node -e 'const fs=require("fs"); console.log(JSON.parse(fs.readFileSync(process.argv[1],"utf8")).status)' "$dmg_notary_result"
)"
if [[ "$dmg_notary_status" != "Accepted" ]]; then
  echo "DMG notarization failed with status: $dmg_notary_status" >&2
  exit 1
fi
xcrun stapler staple "$dmg_path"
xcrun stapler validate "$dmg_path"
ditto -c -k --keepParent "$app_path" "$zip_path"
tar -czf "$updater_path" -C "$(dirname -- "$app_path")" "$(basename -- "$app_path")"
(
  cd "$repo_root/src-tauri"
  cargo tauri signer sign "$updater_path" >/dev/null
)
if [[ ! -f "$updater_signature_path" ]]; then
  echo "Tauri signer did not produce the updater signature: $updater_signature_path" >&2
  exit 1
fi

cargo run \
  --quiet \
  --locked \
  --manifest-path "$repo_root/src-tauri/Cargo.toml" \
  --example verify-updater-signature \
  -- \
  "$updater_path" \
  "$updater_signature_path" \
  "$updater_public_key_path"

node - \
  "$version" \
  "$channel" \
  "$release_tag" \
  "$updater_name" \
  "$updater_signature_path" \
  "$latest_json_path" <<'NODE'
const fs = require("fs");
const [version, channel, releaseTag, updaterName, signaturePath, outputPath] =
  process.argv.slice(2);
const encodedTag = encodeURIComponent(releaseTag);
const encodedAsset = encodeURIComponent(updaterName);
const url =
  `https://github.com/uhvesta/review-queue/releases/download/${encodedTag}/${encodedAsset}`;
const signature = fs.readFileSync(signaturePath, "utf8").trim();
if (!signature) {
  throw new Error("updater signature is empty");
}
const platform = { signature, url };
const manifest = {
  version,
  notes: `Review Queue ${version} (${channel})`,
  pub_date: new Date().toISOString(),
  platforms: {
    "darwin-aarch64": platform,
    "darwin-x86_64": platform,
  },
};
fs.writeFileSync(outputPath, `${JSON.stringify(manifest, null, 2)}\n`, {
  mode: 0o644,
});
NODE

node - "$app_notary_result" "$dmg_notary_result" "$notarization_path" <<'NODE'
const fs = require("fs");
const [appPath, dmgPath, outputPath] = process.argv.slice(2);
const record = {
  app: JSON.parse(fs.readFileSync(appPath, "utf8")),
  dmg: JSON.parse(fs.readFileSync(dmgPath, "utf8")),
};
fs.writeFileSync(outputPath, `${JSON.stringify(record, null, 2)}\n`, { mode: 0o644 });
NODE

artifact_dmg="${dmg_path#"$repo_root"/}"
artifact_zip="${zip_path#"$repo_root"/}"
artifact_updater="${updater_path#"$repo_root"/}"
artifact_updater_signature="${updater_signature_path#"$repo_root"/}"
artifact_latest_json="${latest_json_path#"$repo_root"/}"
sbom_output="${sbom_path#"$repo_root"/}"
node "$script_dir/generate-sbom.mjs" \
  --output "$sbom_output" \
  --version "$version" \
  --artifact "$artifact_dmg" \
  --artifact "$artifact_zip" \
  --artifact "$artifact_updater" \
  --artifact "$artifact_updater_signature" \
  --artifact "$artifact_latest_json"

(
  cd "$output_dir"
  shasum -a 256 \
    "$dmg_name" \
    "$zip_name" \
    "$updater_name" \
    "$updater_name.sig" \
    "$(basename -- "$latest_json_path")" \
    "$(basename -- "$sbom_path")" \
    "$(basename -- "$notarization_path")" > "$(basename -- "$checksums_path")"
)

"$script_dir/scan-release-secrets.sh" \
  "$app_path" \
  "$dmg_path" \
  "$zip_path" \
  "$updater_path" \
  "$updater_signature_path" \
  "$latest_json_path" \
  "$sbom_path" \
  "$notarization_path"
"$script_dir/verify-macos-release.sh" \
  --app "$app_path" \
  --dmg "$dmg_path" \
  --checksums "$checksums_path" \
  --updater "$updater_path" \
  --updater-signature "$updater_signature_path" \
  --latest-json "$latest_json_path" \
  --version "$version"

echo "release artifacts:"
echo "  $dmg_path"
echo "  $zip_path"
echo "  $updater_path"
echo "  $updater_signature_path"
echo "  $latest_json_path"
echo "  $checksums_path"
echo "  $sbom_path"
echo "  $notarization_path"
