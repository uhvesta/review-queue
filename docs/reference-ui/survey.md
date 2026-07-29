# UI reference survey

This survey records interaction inspiration only. Review Queue does not copy
source code, icons, artwork, text, or CSS from these projects.

## Archived legacy product

The clean-slate work began by running the packaged app from the prior
`cmux-localreview` project and retaining a small visual archive:

- [Legacy Queue Home](legacy-queue-home.jpeg)
- [Legacy diff reviewer](legacy-reviewer.jpeg)
- [Five-second legacy reviewer recording](legacy-reviewer.mov)

The archive is reference material, not implementation input. It contains no
credential values or tokens.

Patterns worth retaining:

- the dark, compact diff canvas;
- a persistent repository/file navigation surface;
- visible split/unified and Viewed controls;
- explicit Queue Home navigation;
- visually separate `/ask` and formal-review controls; and
- actionable, inline connection-recovery copy.

Patterns deliberately rejected by the clean-slate specification:

- credentials or daemon discovery tokens in the renderer/data plane;
- GitHub connections dominating Queue Home;
- an unqueued “Review locally” PR path;
- a separate AI-computed review-order feature;
- vague “Send all comments to cmux” routing without a selected durable
  `AgentRoute`;
- remote daemon tokens stored by the app; and
- the legacy Electron/HTTP/sidecar architecture.

| Reference | Provenance | Pattern retained | Review Queue use |
| --- | --- | --- | --- |
| [Visual Studio Code](https://github.com/microsoft/vscode) | Microsoft, MIT | Stable three-pane workbench, file tree, diff editor, inline comment threads, collapsible side chat | Reviewer file tree, non-overlapping diff controls, anchored threads, right-side chat sheet |
| [Gerrit review UI](https://gerrit-review.googlesource.com/Documentation/user-review-ui.html) | Gerrit project documentation; Apache-2.0 project | Review identity stays visible, explicit file navigation, review actions separated from reading | Persistent round header, explicit open/navigation, bottom formal-decision bar |
| [Review Board](https://www.reviewboard.org/) | Official Review Board site; MIT project | A queue leads into one review surface; comments are drafts until an explicit review action | Queue Home, local formal drafts, explicit Send/Publish boundaries |
| [lazygit](https://github.com/jesseduffield/lazygit) | Jesse Duffield and contributors, MIT | Discoverable keyboard operations and persistent list focus | Move up/down/top/bottom, Open next, visible drag handle without a reorder mode |
| [gitui](https://github.com/gitui-org/gitui) | gitui contributors, MIT | Compact repository-oriented navigation and contextual key help | Repository-qualified file hierarchy and keyboard-first queue actions |
| [Continue chat](https://docs.continue.dev/ide-extensions/chat/how-it-works) | Continue official documentation; Apache-2.0 project | Selected-code context, streamed responses, explicit new-session action, visible model selection | Anchored `/ask`, streamed durable turns, Clear chat, capability-discovered options |
| [Aider](https://aider.chat/docs/usage/commands.html) | Aider official documentation; Apache-2.0 project | Clear separation between ask and edit modes, interrupt/cancel semantics, explicit model switching and chat clearing | `/ask` never mutates code, visible cancel/retry, honest per-turn option stamps |
| [opencode](https://opencode.ai/docs/) | Official opencode documentation; MIT project | Session-oriented chat and explicit model/provider selection | Previous chats, history-only transcripts, session and auth source in the chat header |

## Resulting interaction rules

- Opening, navigating, filtering, or expanding history is read-only.
- Review decisions are visually separate from chat and never inferred from it.
- Network work is explicit: Refresh, Send, Publish, and a newly submitted
  `/ask` are the only operations that contact their respective adapters.
- Queue ordering is directly manipulable by pointer or keyboard and has no
  hidden mode.
- A transcript is durable product state. Restarting or reopening renders saved
  content and never replays a prompt.
- Unsupported capabilities remain visible with a reason and one recovery
  action instead of pretending to work.

## License and provenance note

All implementation in this repository is original. The references above were
used to validate familiar interaction patterns. Any future direct code reuse
must be recorded separately with the exact source revision, file, license, and
retained notices before it is merged.
