# Review Queue

Review Queue is a macOS-first desktop workspace for reviewing one immutable
change across local repositories, GitHub pull requests, or a connected
machine. The signed Tauri host owns credentials and provider/network access;
the Rust data plane, SQLite store, WebView, CLI, and remote daemon remain
token-free.

## What works now

- Atomic multi-repository commit-on-submit. Changed repositories receive a
  real `<topic>: <title>` commit containing the canonical review brief;
  unchanged repositories create no empty commit. Ref, index, and SQLite state
  roll back together on capture failure.
- Persistent Local, GitHub, and connected-machine queues with immutable
  manifests, stable topic identity, drag/keyboard ranking, supersession,
  completion/requeue, viewed files, and confirm-then-purge lifecycle actions.
- One unified reviewer with repository/file filtering, full pinned-file
  context, line/range anchors, imported GitHub discussions, and reproduction
  into a clean destination.
- Official GitHub Copilot SDK `/ask` sessions with automatic existing-CLI
  sign-in selection, app-owned Device Flow fallback, runtime-discovered
  model/reasoning options, provider-managed context, streaming/cancel, durable
  transcripts, Clear chat, and restart-without-replay recovery.
- Formal comments and revisioned manual handoff. Review Queue prepares the
  exact feedback prompt for the selected originating route; a user submits it
  in an accessible original session or in a freshly reproduced environment.
- Dedicated GitHub PR-read and opt-in publish connections. Intake is
  metadata-only, mirroring is lazy and binary-safe, refresh is pull-only, and
  publishing requires a recorded decision plus explicit confirmation.
- A permission-hardened local Unix socket and standalone connected-machine
  daemon. The socket allows only token-free read/enqueue/liveness operations;
  provider prompts, feedback confirmation, and GitHub publishing are
  desktop-only.
- Redacted diagnostics and a universal macOS release pipeline with Developer
  ID signing, notarization, stapling, signed updater artifacts, checksums,
  SBOM, and artifact secret scanning.

## Run checks

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --locked
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked -- -D warnings
npm --prefix frontend run build
```

## Run the desktop app

For development:

```sh
(cd src-tauri && cargo tauri dev)
```

The desktop app creates its owner-only local socket in its application-data
directory. Point the CLI at that socket when exercising programmatic flows:

```sh
REVIEW_QUEUE_SOCKET=/path/from/desktop/runtime/review-queue.sock \
  cargo run --bin review-queue -- agent register \
  --route parser-agent --adapter acp --agent agent-17 \
  --cwd /path/to/workspace --cmux-workspace review --cmux-surface parser

REVIEW_QUEUE_SOCKET=/path/from/desktop/runtime/review-queue.sock \
  cargo run --bin review-queue -- submit /path/to/workspace \
  --route parser-agent --topic parser-v2 --title "Parser error handling" --json
```

Run `cargo run --bin review-queue -- --help` for PR, machine, agent-liveness,
reproduction, diagnostics, and Copilot-skill setup commands. The CLI never
accepts a credential and cannot deliver feedback or publish.

## Security and releases

The bundled GitHub OAuth configuration contains only a public client ID.
Advanced Settings accepts another public client ID but never a client secret;
changing it atomically disconnects only this app's Keychain capabilities.

Stable builds are gated by the full native acceptance matrix. Release
automation is documented in the workflow and scripts under `.github/` and
`scripts/`.

See [the data-plane ADR](docs/adr/0001-data-plane-packaging.md),
[threat model](docs/threat-model.md), and [test matrix](docs/test-matrix.md).
