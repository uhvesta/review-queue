# ADR 0004: explicit provider and source operations

Status: accepted

The common reviewer consumes capability declarations and normalized round
data. It does not infer behavior from a Local, GitHub, or Machine source
label. Provider and source adapters are split into token-free contracts in
the core crate and credential-owning implementations in the signed desktop
boundary.

All remote work is tied to a named user action. GitHub metadata intake,
materialization, comment refresh, staleness checks, and publish preflight are
separate calls; there is no polling. Copilot capability discovery, prompt
submission, cancellation, and session end are separate calls; rendering,
reopening, history selection, and Clear chat issue zero prompts. Originating
ACP route state is receipt/health metadata only. Formal feedback is prepared
as an immutable prompt for explicit manual submission and has no adapter
delivery call.

The fake transports are the conformance oracle for counters, failure copy,
idempotency, and request shapes. A desktop transport may replace a fake only
if the same tests pass and secrets remain inside the Keychain-backed desktop
boundary. Unsupported capabilities stay visible with an explanation rather
than becoming no-op success paths.
