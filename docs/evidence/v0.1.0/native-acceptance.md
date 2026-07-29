# Review Queue 0.1.0 native acceptance record

This record contains redacted acceptance evidence from the signed universal
macOS app. It intentionally contains no OAuth token, Device Flow device code,
Keychain payload, updater private key, or prompt transcript beyond the single
explicit review question and its returned answer.

## Packaged app and Keychain

- Bundle: `Review Queue.app`, universal `arm64 x86_64`.
- Identity: Developer ID Application, team `7H66Q22DJD`.
- `codesign --verify --deep --strict`, `spctl --assess --type execute`, and
  `xcrun stapler validate` passed.
- The signed executable was quit and relaunched between the two disposable
  Keychain phases.
- A disposable `com.reviewqueue.desktop.acceptance.*` service was written,
  read after restart, isolated by capability account, given a malformed pending
  record to exercise recovery, and deleted. Product credentials were not read
  or modified.

## Local multi-repository capture

The packaged UI captured staged, unstaged, and untracked changes from two
repositories under `/private/tmp/review-queue-acceptance-workspace`.

- Review round: `594c05b7-6b2e-4fb1-a36e-0f2252d303e8`
- Snapshot: `9c43aa299a373d49853ccedfd2699c228d0d855594bc5ac88392db76f362bc5d`
- `app` saved commit: `3db9e11fe6e0538eaa28b46220f9e9beaab54346`
- `packages/parser` saved commit:
  `50a3c25cfeabb3f76ce0390f192001ee21688f65`
- Both source repositories were clean after capture.
- Both commit messages used the canonical subject
  `release-acceptance: Validate signed release workflow` and retained the
  complete What, Why, Approach / Alternatives, and Testing brief.

## Copilot streaming, cancel, and restart

The signed app selected the existing `uhvesta` Copilot sign-in only after the
official SDK read-only model probe passed. The session used discovered model
`auto` and context policy `managed_80`.

For the selected `app/new-review.txt:1` line, the explicit question was:

> In one sentence, explain why this added line belongs in the immutable review
> snapshot.

The completed SDK response was:

> It belongs in the immutable review snapshot because it records the exact
> acceptance-state content at review time, preserving a tamper-evident audit
> trail of what was approved for release.

A second long follow-up visibly streamed and was cancelled. After quitting and
relaunching the signed app, the transcript still contained exactly two turns
with statuses `completed,cancelled`; it was history-only, the provider session
was not resumed, and no prompt replay occurred. Clear chat archived that
transcript and opened a new empty conversation without sending a prompt.

## Manual originating-agent handoff

The reviewer recorded Request changes with one immutable line-anchored formal
comment. Because the originating session was marked closed or unavailable, the
app required reproduction before manual submission.

- Feedback idempotency key:
  `f582ce2d-d11b-4eb7-8364-50ad7bf6b0ac`
- The preview created nothing.
- Confirmation reproduced detached, clean clones at both saved commit SHAs
  under
  `/private/tmp/review-queue-acceptance-workspace-review-594c05b7`.
- A fresh `gpt-5.6-terra` subagent was manually given the immutable prompt in
  that workspace. It verified both detached SHAs and both clean worktrees,
  retained the anchored line verbatim, and proposed adding any clarification
  separately.
- Only after that response did the user-side acceptance driver click
  “I submitted it manually.” The app persisted
  `manual submission confirmed`; it never queued, interrupted, typed, injected,
  or sent the prompt itself.

The final in-app record is retained in
[manual-handoff-confirmed.jpeg](manual-handoff-confirmed.jpeg).

## Automated validation

After the final frontend cancellation and handoff-history fixes:

- Frontend TypeScript/Vite build passed.
- Rust formatting passed for the workspace and desktop manifests.
- Workspace tests: 92 passed.
- Desktop tests: 37 passed; two opt-in Keychain tests then passed explicitly.
- Strict workspace and desktop Clippy passed with warnings denied.

