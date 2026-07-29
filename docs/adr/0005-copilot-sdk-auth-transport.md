# ADR 0005: official Copilot SDK and child-scoped authentication

Status: accepted

The desktop adapter uses the official Rust `github-copilot-sdk` to start the
Copilot CLI server, discover its actual model catalog, and own its sessions.
Review Queue does not implement Copilot ACP framing itself.

## Runtime option discovery contract

The pinned official SDK (`github-copilot-sdk` `1.0.0-beta.8`) exposes
`Client::list_models` / the typed `models.list` RPC. Its returned `Model`
records are the authority for selectable model IDs, model policy state,
supported reasoning-effort values, and the default reasoning effort. Review
Queue does not invent fallback model IDs or reasoning levels. Disabled models,
unknown future policy states, malformed IDs, and duplicate IDs are not
advertised.

The current Review Queue option schema is flat rather than model-dependent.
Reasoning effort is therefore advertised only when the SDK catalog reports a
value supported by every selectable model. The desktop validates the selected
model/effort pair again before session creation or an SDK `set_model` call.
This deliberately prefers a smaller truthful picker over presenting a
combination the runtime may reject. Model and reasoning changes use the
official SDK's `Session::set_model` path and never send a prompt.

The same SDK version has no client-level RPC that lists interaction modes,
context-compaction policies, or custom-provider choices. `session.mode.get`
reports only the current mode after a session exists.
`InfiniteSessionConfig` accepts caller-selected compaction thresholds, and
`ProviderConfig` accepts caller-supplied BYOK endpoints and credentials, but
neither is a discovery API. Review Queue consequently:

- leaves context/compaction at the SDK/CLI default and marks that control
  unsupported instead of advertising hard-coded percentages;
- marks provider override unsupported rather than requesting, returning, or
  persisting BYOK tokens; and
- does not expose agent modes, because `/ask` must remain a deny-all,
  non-mutating session.

These unsupported groups remain explicit capability metadata with an
explanation. A future SDK/CLI discovery RPC can populate them without changing
the open-ended option-group contract.

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
