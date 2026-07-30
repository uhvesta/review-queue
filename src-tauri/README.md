# Review Queue desktop shell

This directory is the macOS-first [Tauri 2](https://v2.tauri.app/) desktop
application. It loads the `../frontend` Vite application and deliberately
lives outside the root Rust workspace: the root workspace is the portable,
token-free data plane, while this host owns OS-specific Keychain, GitHub,
Copilot SDK, connected-machine tunnel, and signed-update adapters.

## Command boundary

The webview can invoke only the commands registered in `src/main.rs`.
`src/commands.rs` owns the translation between the webview and
`review-queue-core`.

Commands return core `Round` values or a serializable, actionable
`CommandError`; they never return a `Store`, database path, credential, socket,
or provider client. Typed commands cover local capture, immutable review
materialization, queue/lifecycle operations, Copilot sessions,
capability-scoped Device Flow, GitHub mirror/publish, manual formal-feedback
handoff, connected machines, reproduction, diagnostics, and signed updates.
Remote reads, publishing, capture, purge, reproduction, update install, and
relaunch remain separate named actions with preconditions or confirmation.

The SQLite database is opened as `review-queue.sqlite3` in Tauri's app-data
directory, rather than accepting a renderer-supplied path. The core remains
the owner of domain validation and persistence rules.

## Run locally

From this directory:

```sh
npm --prefix ../frontend install
cargo tauri dev
```

`cargo tauri build` first builds `../frontend`, then bundles the desktop host.

## Signed updates

Production builds create a Tauri v2 `.app.tar.gz` updater bundle and detached
signature. The runtime uses the literal public key in `tauri.conf.json` and the
static feed at the repository's latest GitHub release. `check_for_update` is
read-only. `install_update` re-checks the feed and requires confirmation of
the exact displayed version before downloading and installing a
signature-verified bundle. Relaunch is a second explicit confirmation so
in-progress text is never discarded unexpectedly.

Updater signing material must never be placed in the repository or in an
`.env` file. Before a release build, provide the encrypted private-key content
and its password through the process environment:

```sh
export TAURI_SIGNING_PRIVATE_KEY="$(< /secure/path/review-queue-updater.key)"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$(< /secure/path/review-queue-updater.password)"
scripts/release-macos.sh \
  --profile localreview-notary \
  --version 0.1.0 \
  --channel stable \
  --release-tag v0.1.0
```

CI expects the same values as the encrypted
`TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
repository/environment secrets. The release script signs the already
notarized and stapled app archive, cryptographically verifies the updater
signature, validates `latest.json`, and includes all updater files in the SBOM
and checksums.

## Platform note

Tauri's native build requires platform tooling beyond Rust and Node. On macOS,
install Xcode Command Line Tools; Linux needs WebKitGTK and related system
packages; Windows needs the Microsoft C++ Build Tools and WebView2. This
repository does not vendor those OS dependencies, so a full bundle build may
fail on a machine that lacks its platform prerequisites.
