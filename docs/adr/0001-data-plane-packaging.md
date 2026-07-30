# ADR 0001: in-process core with a token-free local socket

Status: accepted

The desktop app calls the Rust core in process. The `review-queue` CLI speaks
to a filesystem-permission-protected Unix-domain socket owned by the desktop
app (or the same core built as a standalone daemon for a connected machine).
The socket only accepts queue reads and enqueue/configuration requests. It
rejects delivery and publish operations.

This keeps GitHub and Copilot credentials in the desktop Keychain boundary,
not in the CLI, database, logs, socket payloads, or remote daemon. The core's
SQLite schema and adapter boundary can be reused by the standalone daemon.

## Resolution of submission atomicity

Git cannot make ref updates in multiple repositories and SQLite one
filesystem-atomic transaction. The desktop therefore retains a guarded
`PendingCapture` across queue insertion and index finalization. Until SQLite
commits, compare-and-swap compensation can restore only refs still pointing at
this operation's commits and restore the exact saved index bytes. Both the
WebView and CLI enter through this one core ingestion helper; the CLI never
captures first and transports a completed manifest afterward. No source file
is rewritten or deleted.
