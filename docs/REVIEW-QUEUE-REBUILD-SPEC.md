# Review Queue — Clean-Slate Product Specification

Status: proposed handoff for the new `review-queue` repository.

## 1. Product statement

Review Queue is a macOS-first desktop app for reviewing a *review round*: a
coherent change that can span many related Git repositories in one workspace.
It presents that round as one local pull request, keeps its immutable source
state, supports a single persistent Copilot question conversation with inline
anchors, and routes formal feedback back to the agent that made the work.

GitHub pull requests use the same reviewer. A PR adapter resolves metadata,
description, base/head commits, and files into a local cache/worktree, then
opens the identical review-round experience. Local multi-repository review is
the primary product; external PRs, remote nodes, and shared workspace caches
are extension points built on the same model.

The desktop app owns user-facing authentication and secrets. A local or remote
data-plane service stores
immutable review snapshots, computes diffs, serves cached source data, and
routes explicit ACP feedback. No daemon, browser renderer, queue record, log,
export, SSH tunnel, or CLI argument may contain a GitHub or Copilot token.

The first release targets macOS only. Build Windows/Linux only after their
native credential-vault and packaging paths have equivalent validation.

## 2. Decisions made up front

| Concern | Decision |
| --- | --- |
| Desktop shell | Tauri 2 + Rust; web UI may remain React/TypeScript. |
| Secret storage | Rust `keyring`/Security.framework backed directly by macOS Keychain. No Electron `safeStorage`, plaintext JSON, `gh` runtime fallback, shell arguments, or environment-token inheritance. |
| Copilot default | Prefer the Copilot SDK's existing signed-in-user path, surfaced as an explicit **Use existing Copilot sign-in** action. It uses the Copilot CLI's secure system-keychain credential when the user has already signed in. |
| Copilot independent option | Support a dedicated GitHub OAuth Device Flow client. Store its token in this app's Keychain item and instantiate the SDK with the explicit token and auto-login disabled. |
| GitHub PR access | Dedicated OAuth Device Flow connection, separate `PR read` and opt-in `PR publish` capabilities. |
| OAuth configuration | Bundle one public Client ID. Advanced Settings permits an arbitrary user-owned public OAuth Client ID. No client secret is accepted, stored, or shipped. Changing the ID atomically invalidates this app's connections. |
| Data plane | Rust local service/sidecar or in-process Tauri commands; remote service is token-free and reached only by explicit SSH tunnel. |
| Distribution | Signed + notarized macOS universal app, GitHub Releases, Sparkle/Tauri updater, release notes and checksums. |
| UI assets | Preserve the current visual language: dark diff canvas, compact queue cards, global Queue Home, visibly separate `/ask` and formal review controls. Snapshot reference screenshots before rebuilding. |

The Copilot SDK documents that its default signed-in-user mode uses credentials
from a prior interactive Copilot CLI sign-in stored in the system keychain. It
also supports an explicit OAuth token with automatic login disabled. We use
both paths, but never silently move between them: the selected auth source is
shown per conversation. [Copilot SDK authentication documentation](https://github.com/github/copilot-sdk/blob/main/docs/auth/authenticate.md)

## 3. Trust boundaries

```text
                    macOS Keychain
                  /       |       \
      Copilot SDK login   app OAuth   app OAuth
                         PR read     PR publish
                  \       |       /
                   Tauri Rust core
          (only component that may receive a token)
                    |           \
          scoped commands         Copilot SDK process/session
                    |                       |
              WebView UI            streamed token-free events
                    |
             token-free data plane
       snapshots / diffs / queues / ACP metadata / remote cache
```

Rules:

1. The UI receives `connected`, account label, scopes, expiry, and recovery
   state only. It never receives a token or a token-derived URL.
2. Each capability has a named Keychain account and is independently removed.
3. Device-flow pending state (including `device_code`) is also Keychain-only.
   The public user code may appear on-screen until expiry.
4. Copilot is never prompted by page load, refresh, reopening a diff, opening
   history, selecting a conversation, or restarting the app.
5. A request is durable before it is sent and has an idempotency key. A
   restarted stream becomes `interrupted`; it is never auto-resumed or resent.
6. Formal feedback and `/ask` are distinct data types and distinct transports.

## 4. Domain model

```text
SourceAdapter ─┬─ LocalWorkspaceSnapshot
               ├─ GitHubPullRequestMirror
               └─ ConnectedDaemonWorkspace (future)
       |
Workspace ─┬─ RepositorySnapshot (one per participating Git repository)
           └─ ReviewTopic ─ stable identity: workspace + topic + source adapter
                 ├─ AskConversation (long-lived chat across review rounds)
                 │    └─ AskTurn (round ID + repo-qualified anchor + state +
                 │                 idempotency key)
                 └─ ReviewRound ─ title, description, rationale, alternatives
                       ├─ QueueItem (active | completed | removed)
                       ├─ FormalComment / Decision / Delivery
                       ├─ OriginatingAgent (cmux + ACP/session provenance)
                       └─ ReviewPlan (optional saved ordered hunk representation)
```

`ReviewRound` is the unit of staleness. A completed or removed item disappears
from the active queue. Resubmitting the same stable topic with a changed source
creates a new round and retains the old round as hidden/outdated history.

### 4.1 Canonical local submission contract

`/localreview-submit` is a Copilot skill whose only privileged operation is to
invoke the installed `review-queue submit` CLI. The CLI accepts a workspace
path and optional human context. It discovers all participating Git
repositories and fills the missing narrative from the submitting agent when
possible:

```text
title        What is changing?
description  What work was performed?
intent        Why is this work needed now?
rationale     Why this approach? Which alternatives were considered, and why?
topic         Stable review stream, e.g. "parser-v2"
provenance    cmux surface, agent identity, ACP endpoint/session, cwd
repositories  repo root + base/head/index/worktree snapshot for every repo
```

The command captures all fields and immutable repository snapshots atomically,
then opens/updates exactly one queue item for that topic and source state. A
local review round therefore feels like a single PR even when its files come
from several repositories.

At the same capture point it snapshots the **originating Copilot state**:

```text
provider/agent and version; CLI/SDK mode; working directory
model, thinking level, context setting; current session/ACP IDs
cmux workspace/surface; busy/idle/error state; last heartbeat/turn metadata
session resume handle and transcript reference when the provider exposes them
```

The record is timestamped alongside the workspace manifest, so the reviewer
can see precisely which agent context produced the submitted work. It records
the strongest supported continuation capability—live ACP session, resumable
provider handle, or transcript-only history—without claiming that an opaque
provider context window was exported when it was not.

The saved **review brief** is editable before and after submission and contains:
`Title`, `What changed`, `Why now`, `Why this approach`, `Alternatives
considered`, `Risks`, and `Testing/validation`. `/localreview-submit` can
pre-fill it from its calling agent, a template, or an explicit Copilot drafting
action; it always remains human-reviewable before the item enters the queue.

### 4.2 Atomic multi-repository manifest

Each local round writes one content-addressed workspace manifest. It records,
for every included repository, its workspace-relative repository ID, root,
base/head commits, index/worktree/untracked overlay, and snapshot object IDs.
Every diff hunk and inline anchor is `repo_id + path + side + line/range`, so
same-named files in different repositories cannot collide. The reviewer builds
a virtual unified file tree ordered by repository then path, while retaining a
clear repository boundary and a per-repository filter.

The source adapter produces this manifest for all sources: local capture emits
many repository snapshots; a GitHub PR mirror normally emits one; a future
remote workspace can emit many after a token-free remote fetch.

### 4.4 Queue identity, rank, and lifecycle

```text
local topic identity = workspace_id + topic_key
GitHub topic identity = host + owner + repository + pull_number
review round        = topic identity + immutable manifest/head hash
queue placement     = collection + rank + lifecycle state
```

Queue states are `queued`, `in review`, `changes requested`, `approved`,
`completed`, and `deleted (undo available)`. A title or a path is presentation
data, never identity.

| Action | Invariant |
| --- | --- |
| Reorder | Changes `rank` only, persists after restart/refresh, and stays in its current collection. |
| Complete | Removes the round from active work while retaining round history. |
| Requeue | Returns a completed round to its original collection. |
| Delete from queue | Explicitly confirms whether placement and/or retained artifacts are removed; offers Undo. |
| Resubmit unchanged | Focuses the existing round; creates no duplicate. |
| Resubmit changed | Creates exactly one new round and marks old anchors/comments outdated. |

Each queue has an explicit **Reorder queue** mode, drag handles, and keyboard
Move up/down/top/bottom actions. `Open next` has a menu for next overall, next
Local, next GitHub, or a named connected source.

### 4.3 Queue collections and connected sources

Queue membership has a visible origin independent of the shared reviewer:

```text
Local queue     ← /localreview-submit on this Mac
GitHub queue    ← GitHub PR URL / saved GitHub PR subscription
Connected queues← daemon/server adapters discovered or configured by the user
```

GitHub PRs always appear in the **GitHub queue**, never among local workspace
submissions. A connected source owns a named queue collection and may expose
local-style review rounds, GitHub-style PR records, or another declared item
kind. Its items preserve source name, source item ID, freshness/cached state,
and a stable local identity so they can be reordered, completed, removed, and
resubmitted without conflation with another source.

The connection protocol is pull-first and lazy: a source provides a compact
item index, an item-detail endpoint, snapshot/diff materialization, health,
and a cursor/version for cache freshness. A CLI may register a daemon, push a
new item to the local queue, or ask an already configured source to publish an
item. The desktop app owns credentials; connected sources receive only the
data and scoped capability needed for their operation.

## 5. North-star local workflow

```text
Originating Copilot/cmux session
        |
        | /localreview-submit [context]
        v
Discover related repositories + prepare editable review brief
        |
        v
Preview aggregate manifest, inclusions, exclusions, and diagnostics
        |
        +-- workspace changed during capture --> retry or explain conflict
        |
        v
Atomically capture every repository + provenance + originating agent route
        |
        v
Create/update stable Local topic and enqueue its immutable review round
        |
        v
Unified reviewer -> inline /ask topic chat -> formal feedback / reproduction
```

The first complete vertical slice is: local multi-repository submission → Local
queue → unified reviewer → persistent inline Copilot conversation → formal
feedback to the originating ACP session or reproducible fallback. GitHub PR
mirroring reuses this normalized reviewer. Connected daemon federation remains
an extension point and cannot complicate or block that local vertical slice.

### 5.1 Immutable workspace manifest

The aggregate manifest is content-addressed and contains a before/after
workspace fingerprint, stable workspace/topic IDs, the original directory
layout, and per-repository entries:

```text
repository ID; workspace-relative root; remote fingerprint; branch
base ref/SHA; head SHA; index state; worktree overlay; untracked overlay
tracked/staged/unstaged/untracked/deleted/binary inclusions
explicit exclusions and warnings; object checksums; materialization recipe
```

Capture retries or asks for direction when the workspace changes during
capture; it never produces a torn cross-repository round. The virtual reviewer
tree is layout preserving:

```text
workspace/
├── app/                 repo: app
├── packages/parser/     repo: parser
└── tools/cli/           repo: cli
```

Every anchor records:

```text
round_id, repository_id, workspace_relative_path, repository_relative_path,
side, start_line, end_line, blob_sha, selected_code
```

### 5.2 Submission envelope and originating route

`/localreview-submit` sends one versioned, idempotent transaction containing
the review brief, aggregate manifest, cwd/workspace root, cmux workspace and
surface, originating agent ID/kind/version/status/heartbeat, ACP endpoint and
session ID, Copilot session handle when available, timestamp, and checksums.
A partial capture never creates a queue card.

`AgentRoute` records adapter kind/version, machine ID, original cwd/workspace,
cmux surface, ACP endpoint/session, live/busy/error state, reconnect data, and
last heartbeat. Terminal `/btw` always requires an explicit selected route;
there is no focused-terminal inference.

The installed skill and CLI contract is:

```text
/localreview-submit [workspace] [--topic KEY] [--title TEXT] [--brief FILE]
      -> review-queue submit ...
      -> one atomic ReviewSubmission envelope
      -> queue item ID + reproduce command + originating-route status
```

The CLI also provides `review-queue agent register`, `agent heartbeat`,
`source add`, `source push`, `reproduce`, `diagnose`, and `setup --copilot`.
`setup --copilot` installs/updates the skill and prints the minimal usage
examples without installing credentials.

## 6. Screen inventory

### A. First launch / connection health

```text
+---------------------------------------------------------------+
| REVIEW QUEUE                                      [Settings]  |
|                                                               |
| Welcome. Review locally first; connect only what you need.    |
|                                                               |
| Copilot /ask   [Use existing Copilot sign-in] [Connect app]   |
| PR read        [Connect]                                      |
| PR publish     Not connected — required only to publish       |
|                                                               |
| [Review a local workspace]   [Review a GitHub PR locally]     |
|                                                               |
| Need help? [Open Keychain Access] [Connection guide]          |
+---------------------------------------------------------------+
```

- Every row explains what it can do, its account, and whether it is optional.
- The existing-Copilot route is explicit, read-only validation first, and
  never implies GitHub PR publishing authority.
- A Keychain failure gives a button that opens Keychain Access plus exact
  steps: select `login`, unlock it, return, and `Retry connection`.
- OAuth Device Flow shows a browser link and public code, copy button,
  expiry countdown, **I opened the browser**, cancel, retry, and account
  mismatch recovery. It never says “paste a bearer token.”

### B. Queue Home (the default route)

```text
+------------------------------ REVIEW QUEUE -------------------+
| Queue Home   [Submit local] [Review PR] [Refresh] [Settings]  |
|                                                                |
| LOCAL (3)                         GITHUB (2)                   |
| +-------------------------+      +-------------------------+  |
| | parser cleanup  queued  |      | mono #2  queued          |  |
| | 3 repos · ~/work/acme   |      | uhvesta/mono @ abcd123   |  |
| | why: parser migration   |      | PR description available |  |
| | ACP idle                |      | read-only local mirror   |  |
| | [Open review] [···]     |      | [Open review] [···]      |  |
| +-------------------------+      +-------------------------+  |
|                                                                |
| [Open next (2)]  [Reorder queue] [Show completed / old rounds]|
+----------------------------------------------------------------+
| CONNECTED SOURCES  [Add source] [Refresh sources]              |
| buildbox daemon (4, cached 2m)  [Open queue] [Reconnect]      |
+----------------------------------------------------------------+
```

- Local and GitHub are side-by-side vertical queues at desktop widths; one
  column on narrow windows. Connected source queues are named below them and
  open as their own collection without changing the local/GitHub taxonomy.
- Cards show an actual workspace-relative/absolute path, topic, snapshot/PR
  identity, source status, and ACP state. Never call it `(workspace root)`.
- Opening a reviewer is an explicit action. Opening does not submit, publish,
  contact Copilot, or deliver ACP feedback.
- The overflow menu contains: requeue, remove from queue, reproduce, copy
  feedback prompt, refresh remote PR, clean remote worktree, and show history.
- **Reorder queue** enters an explicit edit mode with drag handles and keyboard
  move controls. Reordering persists position only; it never changes queue
  membership. Deletion is a distinct overflow action with its own confirmation.
- Remove is recoverable for a grace period or has a confirmation describing
  exactly what is removed (queue record, not source files or snapshot).

### C. Local submission sheet

```text
+---------------------- Submit local review --------------------+
| Workspace path  [/Users/me/work/monorepo                 ]    |
| Topic (stable)   [parser-v2                              ]    |
| Title            [Parser error handling                  ]    |
| What changed?    [Normalize parser error handling        ]    |
| Why / rationale  [Required for …; chose … over … because…]    |
| Detected repos:  ✓ app  ✓ packages/parser  ✓ tools/cli        |
| Capture: tracked, staged, untracked; source is not modified.  |
|                                                            [X] |
|                                    [Cancel] [Capture snapshot]|
+----------------------------------------------------------------+
```

- Presents all discovered repositories, lets the submitter scope participation,
  and captures a single title, description, intent, and rationale for the
  entire review round.
- Captures a content-addressed immutable snapshot without changing HEAD, index,
  or worktree. A failed capture has retry and diagnostics without losing form
  input.

### D. Add / locally review a GitHub PR

```text
+---------------------- Review pull request --------------------+
| https://github.com/owner/repo/pull/42                         |
| [Resolve PR]                                                   |
|                                                               |
| owner/repo #42  title                          OPEN            |
| Summary: rendered PR description and linked issue context      |
| base main  111aaa     head feature  222bbb                     |
| Mirror: not cached  [Create local read-only mirror]            |
|                                                               |
| [Add to GitHub queue]       [Open without queueing]            |
+----------------------------------------------------------------+
```

- **Add to GitHub queue** creates/opens a cached GitHub PR review round in the
  GitHub queue. It has the same identity, reordering, completion, deletion,
  and reproduction behavior as a local multi-repository submission.
- **Open without queueing** is a temporary inspection surface. It can be added
  to the GitHub queue explicitly at any time and cannot imply publishing.
- **Publish review** is a separate, later capability and action. Opening,
  reviewing, asking Copilot questions, refreshing, and queueing a PR are all
  local read workflows.
- Head SHA is visible and pinned. A changed head becomes `stale`; refresh
  creates a new round and marks old comments/conversations outdated.

### E. Reviewer / diff workspace

```text
+Queue Home | parser-v2 · 3 repositories @ snapshot 7f3… [Details]+
| Files                      | Diff / plan /ask                        |
|  src/parser.ts        3    | src/parser.ts  +20 -6  [Viewed]         |
|  tests/parser.test.ts  1   |                                        |
|  [Filter files]            |  41 | function parse(input) {             |
|                             |  42 |   ...                                 |
|                             |       [ + Comment ] [ /ask selected ]   |
|                             |                                        |
|                             | [Previous hunk] [Next hunk]             |
|                             |                                        |
|                             | Formal review  [Approve] [Changes] ...  |
|                             | /ask               [Open conversation]  |
+----------------------------------------------------------------+
```

- The review header renders title, description, intent, and rationale in a
  collapsible **Review brief**. It identifies all participating repositories.
- Left is a repository/file hierarchy with actual workspace-relative paths and
  per-file comment state; switching repositories never leaves the round.
- Main supports split/unified/full file preview, stable line anchors, selected
  text, keyboard navigation, and loading/error/empty render states.
- Header makes review identity and active representation unmistakable.
- Floating controls never overlap code or comments; small-window layout turns
  them into an anchored bottom bar/drawer.

### F. Inline `/ask` thread

```text
  42 |   validate(input)
     | ┌─ /ask: Why is this validation after normalization? ─────┐
     | | scope: app/src/parser.ts · RIGHT · lines 42–46          |
     | | selected: `validate(input)`                              |
     | | Sending to Copilot · model: GPT-… · Cancel               |
     | |                                                           |
     | | Copilot                                                  |
     | | ▋ streaming answer…                                      |
     | | [Reply to Copilot] [Open in /ask] [Convert to comment]  |
     | └─────────────────────────────────────────────────────────┘
```

- Only a newly submitted `/ask` creates a turn. Reopening a thread never does.
- Prompt envelope includes review-round ID, workspace-relative path, repository,
  old/new side, line/range, selected code, commit/snapshot identity, and
  stable conversation ID. It does not resend prior turns as text; the selected
  persistent SDK session supplies history.
- Streaming begins with a visible `Sending` state, then token/chunk updates;
  cancellation, retry, and failure are explicit.
- Copilot responses have clear attribution, timestamp, turn state, model, and
  reply affordance. An inline follow-up and side chat append to the same
  conversation.
- Conversation state is explicitly one of **live** (the provider session is
  connected), **resumable** (the provider has a stable session identifier and
  supports continuation), or **transcript-only** (history is readable but a
  new session is required). A transcript is never presented as proof that its
  remote context can be resumed.

### G. `/ask` side panel

```text
+------------------- Ask Copilot -------------------------------+
| Conversation [Parser review topic v]     [+ Fresh session]    |
| Auth: existing Copilot sign-in ✓      Session: live            |
| Model [model v] Thinking [balanced v] Context [standard v]    |
|                                                               |
| You · src/parser.ts:42–46                                    |
| Why is validation after normalization?                         |
| Copilot · streaming complete                                   |
| …                                                             |
|                                                               |
| [Ask a follow-up…                                      ] [Send]|
+----------------------------------------------------------------+
```

- A named conversation belongs to the stable review topic, so a later
  resubmission/review round can continue the same question series while every
  turn remains tagged with its original round and code anchor. A fresh session
  is explicit and old sessions are selectable/read-only.
- Picker options are populated from SDK capability discovery. Unsupported
  models/thinking/context selections show why and do not fake a change.
- Shows `not signed in`, account mismatch, model discovery failure,
  disconnected session, cancellation, and retry steps.
- If a session is transcript-only, the panel offers **Read history**, **Start
  fresh**, and **Explicitly rebuild context**. Rebuilding shows the exact
  selected materialized workspace/history that will be sent and requires a
  confirmation; it is never triggered by browsing history or opening a round.

### H. Question sets and agent shortcuts

```text
+---------------- Question sets -------------------------------+
| [Architecture pass v]  [+ New set]                            |
| 1. What invariant changes here?                  [↑] [↓] [×]  |
| 2. Which failure path is untested?               [↑] [↓] [×]  |
|                                                               |
| [Save] [Send numbered prompt] [Send sequential turns]         |
+----------------------------------------------------------------+
```

- Question sets are named, editable, reorderable, deletable, and persisted.
- Sending one numbered prompt or sequential turns is an explicit choice; both
  use the selected persistent topic conversation and visibly create their own
  `AskTurn` records.
- `/btw` opens a target picker listing registered originating agents, cmux
  surfaces, ACP status, and reconnect errors. It is a separate, intentional
  operational shortcut rather than an implicit terminal action.

### I. Formal feedback and ACP delivery drawer

```text
+------------------ Formal feedback ----------------------------+
| 2 comments · excludes 4 /ask turns                            |
| Delivery target [Parser agent / cmux: work-17 v]               |
| ACP: ● idle  session: 80c…                                     |
| Policy: (• Queue until idle) ( ) Interrupt current turn        |
|                                                               |
| [Copy feedback prompt] [Send through ACP]                     |
| Delivery history: #18 delivered once · 14:03                  |
+----------------------------------------------------------------+
```

- `/ask` content is excluded by type, not by convention. Only **Convert to
  comment** copies a selected answer into a formal draft.
- Target selection is mandatory; no focused-terminal fallback.
- Duplicate send is prevented by durable delivery idempotency key. Busy state
  requires policy choice and reports queue/interrupt outcomes.
- Copy is always available and clearly says it does not send anything.

### J. Queue-item details / reproduce

```text
+---------------- Review round details -------------------------+
| Provenance  local snapshot · topic parser-v2                   |
| Workspace   /Users/me/work/mono                               |
| Review brief  what / why / rationale / alternatives            |
| Repositories app@sha…, packages/parser@sha…                   |
| Branch/base/head  feature · main@… · head@…                    |
| Agent/cmux  copilot · workspace work-17                        |
| ACP         127.0.0.1:… · session … · idle                     |
| Decisions   requested changes · 2 comments                     |
| Deliveries  ACP #18 delivered; GitHub none                     |
|                                                               |
| [Reproduce snapshot] [Reproduce Copilot setup] [Copy commands]|
+----------------------------------------------------------------+
```

- Shows saved metadata and hashes, but never secrets.
- Reproduction provides a script and copyable command bundle that: creates a
  clean destination, restores/clones every repository at its saved commit,
  checks out the saved review paths, and prints the exact agent/ACP situation.
- When an originating ACP session remains live, it offers **Send feedback to
  original agent**. When it cannot be reached, it offers **Recreate agent
  setup** with the correct working directory, repository revisions, and a
  ready-to-paste feedback prompt. The UI states whether a historical chat can
  be resumed or whether a new session will be started.

### K. Review-plan / hunk ordering mode

```text
+--------------------- Review plan -----------------------------+
| Representation: [Original diff v] [Copilot review plan v]     |
| Model [model v]                         [Compute a plan]      |
|                                                               |
| 1. src/auth.ts: token boundary (security-critical)             |
| 2. src/queue.ts: identity migration                            |
| 3. tests/…: regression coverage                                |
|                                                               |
| [Previous planned hunk] [Next planned hunk] [Discard plan]     |
+----------------------------------------------------------------+
```

- This is an explicit structured skill request, never automatic “Copilot
  ordering”. It asks for JSON with schema/version, hunk IDs, rationale, and
  confidence; validates every hunk ID; preserves the original ordering.
- Result is saved to the review round and can be recomputed only by a new
  explicit action. It is separate from formal feedback and `/ask` history.

### L. Settings, privacy, and diagnostics

```text
+---------------- Application settings -------------------------+
| GitHub OAuth client: bundled (Ov23…) [Change…]                 |
| Advanced: custom public Client ID [____________] [Save]        |
| Saving disconnects this app's connections. No secret accepted. |
|                                                               |
| Connections  [PR read: disconnect] [Copilot: disconnect]      |
| Keychain     healthy · service com.example.review-queue        |
| Privacy      [Open redacted diagnostics] [Export support file] |
| Updates      1.2.0 · [Check]                                   |
+----------------------------------------------------------------+
```

- Changing public client ID requires confirmation, clears this app's Keychain
  items, cancels pending Device Flow, and preserves snapshots/comments.
- Diagnostics are redacted by construction and include a verifier that fails
  if token-shaped strings are present.

## 7. Primary user flows

### 7.1 First Copilot connection

```text
Launch
  |
  +-- Existing Copilot sign-in? -- yes --> validate SDK --> connected
  |                                           | failure
  |                                           v
  |                                     explain + Connect app
  |
  +-- Connect app --> Keychain write pending --> GitHub Device Flow
                                                   | approve
                                                   v
                                            Keychain credential saved
                                                   |
                                                   v
                                            SDK explicit-token session
```

No automatic migration happens. The chosen source remains visible. `Disconnect`
removes only the selected app-owned capability; it cannot delete Copilot CLI
credentials.

### 7.2 Review a GitHub PR through the shared review-round adapter

```text
Paste PR URL -> authenticate PR read if needed -> resolve metadata
    -> render PR title/body/base/head -> pin SHA -> create local mirror/worktree
    -> create/update a cached ReviewRound in Queue Home -> reviewer
    -> select code -> submit /ask -> durable AskTurn -> stream inline + panel
    -> close/reopen/restart -> display persisted result, SEND NOTHING
```

Opening, refreshing, changing view, showing history, or selecting a thread
must issue zero Copilot prompt calls and zero GitHub writes.

### 7.3 Submit and review one multi-repository local change

```text
/localreview-submit workspace + context
  -> discover all related repositories
  -> collect title + what + why + rationale + alternatives + agent provenance
  -> capture every repository snapshot atomically
  -> create/update the stable review topic and queue item
  -> Queue Home -> explicit Open review -> one unified multi-repo diff
  -> complete / requeue / explicitly delete / resubmit as a new round
```

### 7.4 Inline `/ask` and side-chat synchronization

```text
New inline /ask
  -> create AskTurn(idempotency key, anchor, conversation ID)
  -> explicit SDK prompt once
  -> stream chunks to durable transcript + inline thread + side panel
  -> complete/cancel/fail

Reopen anchor/panel/restart -> load transcript only (no prompt call)
Follow-up in any round      -> append to chosen persistent topic conversation
Fresh session               -> make new conversation; do not erase old one
```

### 7.5 Formal feedback to ACP

```text
Draft formal comments -> select agent target -> inspect idle/busy
  -> send to originating ACP session once when reachable
  -> otherwise generate reproduction bundle + copyable feedback prompt
  -> busy? choose Queue or Interrupt
  -> record delivery outcome and idempotency key
  -> never deliver on open/reopen/restart
```

### 7.6 Optional GitHub publishing

```text
Formal drafts -> user clicks Publish -> require separate publish capability
  -> fresh PR-head validation -> confirmation with exact target/count/action
  -> POST once with idempotency record -> show GitHub result/recovery
```

Only publish to an explicit target. `/ask`, queue-only notes, and copied
prompts are excluded. A stale/closed PR cannot be published.

### 7.7 Connected source / daemon lifecycle

```text
Add source (name + endpoint/SSH adapter + source type) -> validate config
  -> Connect -> on-demand tunnel or local socket -> health
  -> lazy item-index fetch/cache -> open source queue -> materialize on demand
  -> retry / disconnect / delete
```

Connected daemons never receive desktop GitHub/Copilot credentials. Each
failure includes a concrete action: correct endpoint, start the daemon, check
tunnel, reconnect, or remove the stale source.

## 8. Error and recovery contract

Every error component has: what happened, why it matters, whether user data is
safe, a single next action, optional diagnostics, and a cancel/back route.

| State | Required recovery |
| --- | --- |
| Keychain unavailable | **Open Keychain Access**, unlock `login`, retry; never plaintext fallback. |
| Device code expired/canceled | Start a fresh flow; old pending record removed. |
| Wrong GitHub account | Disconnect that capability, reconnect; identify connected account. |
| Copilot unavailable | Show selected auth source, login instructions, retry only on user action. |
| Model unavailable | Preserve draft; refresh models or select supported model. |
| Network unavailable | Preserve drafts; retry explicit operation. |
| PR stale | Refresh into a new round; retain old review as outdated. |
| Snapshot unavailable | Reproduce or requeue from source; no silent mutable fallback. |
| ACP busy/disconnected | Choose queue/interrupt, reconnect, copy prompt; never keystroke-inject. |
| Remote tunnel failed | Retry tunnel, inspect target, disconnect/delete. |
| Publish rejected | Preserve formal draft; reconnect publish capability or refresh head. |

## 9. Implementation plan

### Phase 0 — preserve reference material

1. Export current UI screenshots and a short screen recording to
   `docs/reference-ui/` in the old project; annotate what is worth keeping.
2. Copy only reusable UI tokens/components and product fixtures into the new
   repo. Do not copy the authentication or daemon architecture.
3. Create `review-queue` with `CONTRIBUTING.md`, threat model, architecture
   decision records, and a product test matrix on day one.

### Phase 1 — trustworthy desktop skeleton

1. Tauri app with a Rust command boundary and React UI.
2. Keychain adapter integration-tested in a signed dev app: set/get/delete,
   app restart, account separation, malformed entry recovery, and no secret in
   logs/profile/IPC.
3. Token-free SQLite queue and snapshot service with stable topic/round
   identities, review briefs, and originating-agent provenance.
4. Queue Home + `/localreview-submit` skill/CLI + local multi-repo submission
   + unified immutable diff viewer.

### Phase 2 — read-only review and `/ask`

1. GitHub PR adapter: metadata/description rendering and local mirror/worktree
   lifecycle that produces the same review-round model as local submission.
2. Copilot adapter with explicit auth-source selection and persistent sessions.
3. Inline and side-panel transcript synchronization, streaming/cancel/retry,
   model picker, no-replay instrumentation.
4. Structured review-plan skill and saved representation switching.

### Phase 3 — formal review and agent routing

1. Formal comments, decisions, export/copy, ACP target registration/delivery.
2. Reproduction hub, full comment lifecycle, stale rounds/history controls.
3. Connected-source fixture + token-free SSH/loopback federation, source
adapter contract tests, and CLI push/register workflows.

### Phase 4 — publishing, distribution, release cadence

1. Separate publish capability and disposable-repo E2E publishing test.
2. macOS universal signing, hardened runtime/entitlements as necessary,
   notarization, stapling, upgrade/relaunch test.
3. GitHub Actions: lint/test/build/package/notarize/release/checksum/SBOM.
4. Release train: nightly prerelease on main, candidate on tag, stable signed
   release after manual acceptance matrix approval.

## 10. Verification gates

No release may call a flow complete from unit tests alone. Required artifacts:

```text
production .app launch -> Keychain auth -> restart -> PR local review
 -> inline /ask stream -> cancel/follow-up -> restart/reopen (no replay)
 -> queue complete/remove/resubmit -> fake ACP delivery exactly once
 -> optional disposable PR publishing (with immediate human approval)
```

Automate and retain:

- macOS packaged-app browser/desktop smoke tests;
- native Keychain integration tests run against a disposable named Keychain
  item and removed afterward;
- fake TCP ACP agent request log asserting explicit delivery once;
- local multi-repository, GitHub PR mirror, remote-tunnel, and snapshot
  reproduction fixtures;
- UI keyboard, accessibility labels, responsive layout, clipboard/toast, and
  every recovery branch;
- token-leak scans of artifacts, database, config, logs, diagnostics, URLs,
  screenshots, IPC fixture data, and process argument captures.

The validation matrix uses only: `not started`, `blocked`, `failing`, `passing`,
or `passing with limitation`. It records setup, exact action, expected/actual
result, screenshot/log/request ID, restart behavior, and recovery behavior.

## 11. Agent implementation playbook

- Give one agent one vertical slice with an acceptance test, not a broad UI
  wish list.
- Start every slice with an interface contract and failure states.
- A separate adversarial agent maps ideal, implemented, and observed flows and
  adds regression tests for every divergence.
- Never let an agent claim “done” for auth, streaming, restart, browser,
  Keychain, ACP, or remote behavior without a real packaged-app artifact.
- Do not ask Copilot to re-answer a historical question while testing UI.
  Instrument `prompt_id`, `conversation_id`, and request count; assert zero
  new requests on reopen.
- Treat the current project as a visual/reference archive, not a codebase to
  merge piecemeal into the new architecture.

## 12. v1 scope and future adapters

v1 ships the macOS desktop reviewer, local multi-repository workspace
snapshots, GitHub PR mirrors, Copilot questions, formal ACP delivery,
reproduction, and the extension interfaces for remote workspaces. The source
adapter and credential-vault boundaries allow future hosted/team views and
other operating systems to reuse the same reviewer model when their security
and validation requirements are ready.
