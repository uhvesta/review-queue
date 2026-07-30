# ADR 0003: originating-agent feedback is a manual handoff

Status: accepted

Connected-machine review data is pulled through the token-free daemon
protocol. Formal feedback is never sent through that item-cache protocol, an
ACP prompt call, terminal automation, or simulated keystrokes.

`AgentRoute` is durable provenance and health data. It records which
agent/session originated a review and whether that session appears accessible.
Route health may change the guidance shown to the user, but it never authorizes
Review Queue to queue, interrupt, type, inject, or otherwise submit a prompt.

Preparing feedback persists one immutable delivery ID and idempotency key, the
recorded decision, and only undelivered formal-comment revisions. Preparation
then produces a copyable prompt derived from that exact payload. Preparing or
copying the prompt performs no external I/O.

When the originating session is accessible, the user waits until it is ready
and submits the prepared prompt there manually. A busy session is informational
only; there is no Queue or Interrupt option.

When the originating session is closed, disconnected, missing, or in error,
the UI directs the user through reproduction:

1. Preview the pinned workspace and token-free environment setup bundle.
2. Explicitly confirm materialization into a clean destination.
3. Run the setup bundle and start a fresh agent in the displayed working
   directory.
4. Submit the same prepared immutable prompt manually.

The setup bundle reconstructs saved Git commits and enters the new working
directory. It contains no credentials or feedback, does not start an agent, and
does not prompt a model.

After manual submission, the user may explicitly acknowledge that action.
Review Queue then records the delivery as `manual_submission_confirmed` and
marks only the revisions contained in that immutable payload. It never infers
submission from clipboard access, route liveness, focus, or session activity.

Legacy decision-only pending deliveries are invalid. They are retired as
`invalid_empty_payload`, and new preparation is rejected with
`formal_comments_required` until at least one undelivered formal comment exists.
