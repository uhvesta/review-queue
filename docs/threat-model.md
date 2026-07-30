# Threat model

## Assets

GitHub/Copilot tokens, device-flow pending codes, source files, repository
refs, immutable manifests, formal feedback, and agent routes are protected.

## Boundaries

Only the signed desktop Rust layer may read Keychain credentials or construct
Copilot/GitHub requests. The WebView, SQLite database, Unix socket, CLI,
standalone daemon, diagnostics, and SSH transport are token-free.

## Controls

- Keychain accounts are capability-specific; changing OAuth client ID clears
  only app-owned credential records.
- Socket permissions and peer credentials gate local callers; allowlisted
  operations exclude provider prompt submission and GitHub publishing.
- Originating ACP routes are provenance and health metadata only. They cannot
  queue, interrupt, type, or inject a formal-feedback prompt.
- SQLite persists opaque IDs and review metadata only. `redact_for_diagnostics`
  rejects token-shaped strings before export.
- Submission records a before/after fingerprint and uses expected-old ref
  values; a conflict leaves no review round and reports the affected repo.
- Purge removes only app-owned state. It never invokes Git or changes source
  files/commits.

## Desktop-only authority

Credential storage and network/provider adapters exist only in the signed
desktop target. GitHub uses direct HTTPS requests with capability-scoped
app-owned Keychain records. Copilot uses the official SDK, either through its
existing signed-in-user path or with an app-owned Keychain token supplied
explicitly while automatic login/persistence is disabled. Ambient
GitHub/Copilot token variables are removed before provider startup and no
plaintext fallback exists.

The standalone daemon and connected-machine protocol serve immutable,
token-free round metadata and blobs only. The desktop persists those remote
bytes before review, so subsequent rendering and reproduction never reads a
remote working tree and never forwards desktop credentials.

Signed updates are verified by Tauri's committed public key before install.
Install and relaunch are separate confirmed actions. Release artifacts are
Developer ID signed, notarized, stapled, checksummed, SBOM-listed, and scanned
for token-shaped material.
