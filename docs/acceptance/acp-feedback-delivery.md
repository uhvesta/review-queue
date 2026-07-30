# ACP formal-feedback delivery acceptance

This procedure validates the Rev3 desktop-only ACP delivery slice through the
exact executable shipped in a production macOS app bundle. Core unit tests and
frontend fixture tests remain required, but they are not substitutes for this
restart-separated production-binary evidence.

## Automated packaged-binary acceptance

Build, sign, and notarize the candidate app using the normal release process.
Then run the dedicated harness with a new evidence path:

```bash
scripts/run-packaged-acp-acceptance.sh \
  --app "/Applications/Review Queue.app" \
  --evidence docs/evidence/v0.1.0/packaged-acp-delivery.jsonl
```

The harness verifies the bundle signature and Gatekeeper assessment before
starting. It creates a uniquely named disposable directory and invokes the
packaged executable as two separate processes:

1. Phase one creates only a disposable SQLite store, route, round, decision,
   and formal comment. A fake loopback ACP agent accepts revision 1 exactly
   once, persists its idempotency key in disposable dedupe state, and drops the
   acknowledgement. The real desktop confirmation backend records an
   acknowledgement failure without marking the revision delivered.
2. Phase two reopens that SQLite store, proving restart did not send anything.
   It retries the same immutable delivery and key. The restarted fake agent
   recognizes the key, returns the original receipt, and does not accept a
   duplicate. The desktop then edits the acknowledged comment, prepares a new
   delivery with a new key, and sends exactly revision 2.

Both sends call the same confirmation, pending-delivery claim, TCP transport,
acknowledgement, and durable-outcome implementation used by the Tauri command.
The hook runs before normal Tauri setup, so it never opens production app data
or a Keychain service. The harness deletes the disposable database and
fake-agent state at the end.

The retained JSONL contains two summary objects only. It must show:

- phase one: one desktop confirmation and one unique agent acceptance,
  `firstDeliveryAcknowledged: false`, and
  `firstAttemptCode: "acp_acknowledgement_invalid"`;
- phase two: `databaseReopened: true`, `retryReusedFirstKey: true`,
  `duplicateAcceptSuppressed: true`, and two total unique acceptances;
- phase two: `secondDeliveryUsedNewKey: true`,
  `secondDeliveryRevisions: [2]`, and `deliveredRevision: 2`; and
- both phases: `productDatabaseOrKeychainInEvidence: false`.

Do not retain the disposable acceptance directory. Never copy its SQLite file,
fake-agent state, prompts, comment bodies, raw envelopes, identifiers,
idempotency keys, endpoints, or receipts into release evidence.

## UI confirmation and recovery evidence

Run the frontend fixture suite:

```bash
npm --prefix frontend test
```

Its ACP cases open Formal feedback and prove:

- Prepare and Review Send do not call delivery;
- Cancel closes the exact-target confirmation with zero delivery calls;
- Confirm Send makes exactly one call using Queue for an idle agent;
- a busy agent exposes Queue until idle and Interrupt current turn, and the
  selected policy reaches the delivery API; and
- an unreachable endpoint leaves the immutable prompt available with Copy and
  Preview reproduction recovery actions plus actionable safety text.

For the final native UI record, repeat those interactions in the signed
candidate with a disposable fake route. Capture the exact-target confirmation,
both busy-policy choices, successful receipt copy, and unreachable recovery
state. Do not capture prompt contents, IDs, keys, endpoints, credentials, or
other user data. Run the release token scan over the retained JSONL and
screenshots before marking the ACP acceptance row passing.
