# ADR 0005: official Copilot SDK and child-scoped authentication

Status: accepted

The desktop adapter uses the official Rust `github-copilot-sdk` to start the
Copilot CLI server, discover its actual model catalog, and own its sessions.
Review Queue does not implement Copilot ACP framing itself.

For an existing Copilot CLI sign-in, the desktop checks only installed CLI and
Keychain metadata before selecting the SDK's official logged-in-user path. It
never reads, displays, or transmits the existing CLI credential. The SDK/CLI
requires `use_logged_in_user=true` to consume that saved sign-in; starting it
with `--no-auto-login` rejects even a valid saved CLI session as unauthenticated.
Review Queue therefore gates the official path behind its successful
read-only probe instead of presenting it as an unvalidated fallback. A failed
probe offers the separate Connect app flow and never starts an SDK client.

For the application-owned GitHub OAuth flow, the desktop reads the token from
its Keychain vault only at explicit capability discovery or session start. It
passes that token to `ClientOptions::with_github_token`. The official SDK then
creates the child-only `COPILOT_SDK_AUTH_TOKEN` environment variable and gives
the CLI only its variable name through `--auth-token-env`; the token is not an
argv value, UI value, log value, SQLite value, or inherited ambient token.
Automatic login remains disabled for this path as well.

This asymmetry is intentional: the app-owned path always supplies its exact
capability-scoped token with automatic login disabled, while the existing-CLI
path asks the official SDK to consume the user's already-saved CLI identity
only after Review Queue has observed that identity read-only.

Before the SDK spawns the CLI, Review Queue removes ambient GitHub, Copilot,
and direct-provider/BYOK credential variables. The SDK-controlled
`COPILOT_SDK_AUTH_TOKEN` is deliberately not removed: it is the narrowly
scoped transport constructed from the selected Keychain record.

Each SDK session has an empty tool list, config discovery disabled, no MCP
servers, and a deny-all permission handler. Ending or clearing a conversation
disconnects and deletes the SDK session before the client stops, so Review
Queue neither resumes provider history nor preserves provider session files
for a later prompt.
