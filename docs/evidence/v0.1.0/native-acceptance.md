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

## OAuth Device Flow and restart

Using the bundled public client ID, the signed app completed app-owned Device
Flow authorization for the capability-specific `PR read` and `PR publish`
accounts. Settings identified the connected GitHub account as `uhvesta` and
continued to show Copilot's existing-CLI sign-in as a distinct source.

The signed app was then quit completely and relaunched. Both GitHub
capabilities were recovered from their separate
`com.reviewqueue.desktop` Keychain accounts, remained connected as `uhvesta`,
and required no repeated browser approval. Keychain metadata showed the two
account items were created independently. No token was read into the
renderer, SQLite, evidence, shell output, or diagnostic artifact.

The post-relaunch state is retained in
[oauth-keychain-persisted.jpeg](oauth-keychain-persisted.jpeg).

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

## Connected-machine immutable review

The shipped daemon served one fixture over an owner-only local Unix socket;
the native app used its explicit `Local daemon socket` connection type, with no
SSH credential or network listener.

- Machine:
  `machine-d0670836d029da335af0357a563ae511539c5a89440d2b5478c4cc99b086d147`
- Source item: `0627e45f-9e44-4924-8a0a-f2b05d1cfd85`
- Snapshot:
  `a0d09b124c2e420dfba1f5a9f70abbdd0090ba6b6b7672003795f92dd5846256`
- Base commit: `7a1c58153bcb83cb74b6c277f9e1fe47038dc3c3`
- Head commit: `7ad6ea69bf72482e10f224153215c94c0c41c9e1`

Before Refresh, the UI showed zero cached items and stated that remote reads
occur only on Connect, Refresh, or Open review. Refresh cached exactly one
metadata item. Open review then fetched the complete immutable snapshot and
rendered the full pinned file:

```text
baseline: connected-machine acceptance fixture
candidate: preserve this exact immutable machine snapshot
```

The reproduction preview created nothing. Confirmation materialized a clean
detached checkout at the exact head commit while the daemon source repository
remained clean on `main`. The final reproduction state is retained in
[connected-machine-reproduced.jpeg](connected-machine-reproduced.jpeg).

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

## Post-acceptance difit UI migration

The current working tree was rebuilt as a universal macOS `.app` with ad-hoc
signing and updater artifacts disabled. `codesign --verify --deep --strict`
passed. This was a no-op UI acceptance run, not a notarized production release.

The packaged app reopened the existing immutable multi-repository
`release-acceptance` round and retained the configured public GitHub Client ID.
Because the validation bundle had a new ad-hoc signature, macOS requested
Keychain ACL approval; the run chose Deny, preserving the stored credential,
and verified the actionable Keychain recovery state instead of changing auth.
The existing Copilot CLI sign-in remained visible and distinct.

Validated in the packaged app:

- Queue Home retained local rounds and connected-machine state.
- The reviewer rendered all three files continuously with sticky per-file
  headers and repository-qualified tree entries.
- The window resized to the 560px minimum; Files and Chat both collapsed to
  labelled toolbar controls, and each opened and closed successfully.
- Unified state, Viewed progress, Full file, hunk actions, formal decisions,
  and the wrapping narrow decision bar remained reachable.
- Settings opened as a modal, displayed the persisted public Client ID and
  actionable Keychain recovery, and closed with Escape without mutating data.

Retained evidence:

- `difit-native-queue-home.png`
- `difit-native-reviewer.png`
- `difit-native-reviewer-560.png`

That retained packaged-app run used the then-current 9/9 fixture suite. The
current suite is 28/28 and additionally covers refreshed-round state
isolation, cached-machine rematerialization, PR-intake confirmation, global
hunk navigation, inline conversations, and keyboard diff navigation.
Browser-fixture screenshots at 1280px, 1024px, and 560px are retained
alongside the native images.

## Current-tree native difit and Keychain responsiveness follow-up

The current tree was rebuilt as an arm64 debug `.app`, deep ad-hoc signed, and
launched against the retained native database. This remains a no-op UI check,
not production release evidence. The first launch exposed that a Keychain ACL
wait could hold a synchronous connection-health command on the Tauri main
thread. Credential and connection operations now run on Tauri's blocking
runtime: the Keychain remains the only credential store, but an ACL wait no
longer freezes Queue Home or the reviewer.

The rebuilt app became accessibility-responsive in about three seconds while
the ad-hoc credential check remained unresolved. It reopened the persisted
two-repository `release-acceptance` round and verified:

- the current compact Queue Home and source rail;
- continuous multi-file review with inline formal and `/ask` conversations;
- Unified/Split roving tabs and global cross-file hunk navigation;
- Split rendering with an explicit horizontally scrollable minimum width;
- Full file mode while Unified remains the selected layout tab; and
- formal decisions and Chat remaining visually separate from inline `/ask`.

The same current bundle was then signed with the available Developer ID
identity and relaunched as a fresh process. Queue Home remained usable while
connection health resolved asynchronously; PR read became enabled without a
browser flow. Application settings then showed Copilot existing-sign-in,
PR-read, and PR-publish all connected as `uhvesta`, the configured public
Client ID `Ov23li9NHgxO6prQz5f7`, and a healthy capability-scoped Keychain.
No credential value was read, copied, logged, or moved outside Keychain.

Retained current-tree screenshots:

- `difit-current-queue.jpeg`
- `difit-current-reviewer.jpeg`
- `difit-current-split.jpeg`
- `difit-current-full-file.jpeg`
- `current-signed-auth-persisted.jpeg`

## Installed updater baseline

The retained `v0.1.0-rc.1` candidate was reverified before installation:
all seven checksum entries passed, Gatekeeper reported `Notarized Developer
ID`, and the DMG staple validated. With `/Applications/Review Queue.app`
confirmed absent, the exact candidate was installed there without
overwriting another app. The installed bundle passed strict deep signature
verification and Gatekeeper assessment and reports version `0.1.0-rc.1`.

The installed candidate launched with the existing queue state and both
Keychain-backed GitHub capabilities still connected as `uhvesta`. This is
only the retained pre-update baseline; no update check, installation, or
relaunch was attempted because the disposable `0.1.0` feed does not yet
exist. Evidence: `updater-before-rc-auth.jpeg`.
