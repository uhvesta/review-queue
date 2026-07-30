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
| [difit](https://github.com/yoshiko-pg/difit) (as vendored in the archived `cmux-localreview` project at `vendor/difit`, commit `bc9ebc360c30a3020c14f1b733ceb82c1b665e48`) | yoshiko-pg, MIT — `Copyright (c) 2025 @yoshiko-pg/difit`, see `vendor/difit/LICENSE` in the archived repo | GitHub-dark color palette (exact token values), toolbar layout order (identity, sidebar toggle, settings, split/unified, viewed progress, revision identity), file-list density (filter, per-file +/-, status glyphs, Viewed toggle), sticky per-file diff header, distinct blue-accented `/ask` card vs. green formal-comment card, 4em line-number gutter and dense code-row geometry | Review Queue reviewer toolbar, file list, diff header/row styling, and Queue Home card density (`frontend/src/styles.css` `--color-*` tokens, `frontend/src/App.tsx` toolbar/file-list/diff-head markup) |

Unlike the other rows above, the difit reference contributed exact **token
values** (hex colors, gutter width) in addition to layout/interaction
patterns, because the goal for this pass was close visual parity with the
retained `legacy-reviewer.jpeg`/`legacy-queue-home.jpeg` screenshots (which
are themselves screenshots of a `cmux-localreview` build using this same
vendored difit). No difit source file was copied into this repository —
every line of `frontend/src/App.tsx` and `frontend/src/styles.css` is
original code written for Review Queue's own component/state model; only
the numeric color/spacing values and the layout ordering were reproduced by
hand. difit's MIT license permits copying its code outright, but doing so
was unnecessary and out of scope here since Review Queue's data model,
Tauri command boundary, and every interactive component are structurally
different from difit's — see `frontend/src/api.ts` for the closed set of
backend calls every restyled control routes through.

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
