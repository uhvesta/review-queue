# difit visual migration report

Scope: restyle Review Queue's reviewer and Queue Home to closely match the
legacy `cmux-localreview` / difit reviewer's look and density, without
touching the Rust/Tauri backend, `frontend/src/api.ts`, `frontend/src/types.ts`,
or any product/security invariant. Commits: `63c04dd`, `a3677ec`, `4afd9db`,
`5e8cd3b`, `9ee642c` on `codex/review-queue-v0.1.0`.

## What this was, concretely

The starting point was not a blank slate: `frontend/src/App.tsx` (a single
~3000-line component tree — there is no `WorkspaceShell.tsx`/`QueueHome.tsx`/
`components/`/`hooks/` directory in this repo; those names exist only in the
legacy `cmux-localreview` checkout) already implemented the full difit-shaped
*product* behavior — repo-qualified file list, split/unified/full-file diff
modes, hunk navigation, Viewed state, inline `/ask`, formal comments, a
decision bar, Queue Home with Local/GitHub columns and a machine sidebar —
routed entirely through the closed `api.ts` command surface. What it lacked
was difit's visual density, exact palette, toolbar layout, and file-list
richness. This pass was a **visual and markup restyle within the existing
component boundaries**, not a rewrite.

## Architecture decision: no separate presentation-adapter module

The task's recommended architecture was a formal adapter layer converting
`ReviewRound`/`MaterializedDiff` into difit-shaped view models in new files.
I did not build one. Reasons:

- The frontend has **zero existing test coverage** (no test runner is even
  configured in `package.json`). Extracting `App.tsx`'s ~15 inline
  components into new files/modules is a real refactor with no safety net,
  on a product whose spec explicitly ranks "correctness and data safety"
  and "no dead ends" above "brevity and proven interaction patterns" when
  those conflict.
- The view-model shaping the spec asks for (`fileKey`, per-file additions/
  deletions counts, status glyphs, viewed progress) already existed or was
  added as small pure helper functions inside `App.tsx`, still exclusively
  fed by `api.ts` return values — the *behavior* the adapter pattern wants
  (repo-qualified identity preserved in every key/callback, all actions
  routed through `api.ts`) is satisfied; only the file-boundary refactor was
  deferred.

**Follow-up recommendation:** once the concurrent backend refactor in flight
on this branch lands and the app is buildable again, add a frontend test
harness (vitest + @testing-library/react) *before* attempting the file-level
componentization, so the extraction can be verified rather than trusted.

## difit files copied or adapted

**None copied verbatim.** Every line touched in `frontend/src/App.tsx` and
`frontend/src/styles.css` is original code against this app's own state and
types. What was reused from difit, by hand, at the *value* level:

- The exact GitHub-dark color palette (`--color-bg-primary #0d1117` through
  `--color-diff-deletion-border #da3633`) from `vendor/difit/src/client/styles/global.css`.
- The `4em` line-number gutter width convention.
- The toolbar's left-to-right control ordering (identity → sidebar toggle →
  settings → split/unified → viewed progress → revision identity).
- The visual distinction between a blue-accented `/ask` card and a
  green-accented formal-comment card.
- Queue Home's card density (tight padding, small type, uppercase
  letter-spaced section headers).

See `docs/reference-ui/survey.md` for the full provenance table entry,
including the exact vendored commit (`bc9ebc360c30a3020c14f1b733ceb82c1b665e48`)
and why literal file copying was unnecessary (structurally different data
model, command boundary, and component tree).

## License / provenance

difit is MIT-licensed (`Copyright (c) 2025 @yoshiko-pg/difit`), vendored
inside the archived `cmux-localreview` repository at
`vendor/difit` (commit `bc9ebc360c30a3020c14f1b733ceb82c1b665e48`). Recorded
in `docs/reference-ui/survey.md`, consistent with that document's existing
"no code copying, pattern/value provenance only" policy for all reference
projects. No difit source file exists anywhere in this repository.

## Changes by area

1. **Design tokens** (`63c04dd`): difit GitHub-dark palette added as
   `:root` CSS custom properties; ~120 raw hex values in `styles.css`
   mapped onto them by semantic role (not a blind find-replace).
2. **Global toolbar** (`63c04dd`): Split/Unified controls moved out of the
   per-file diff header into a persistent page-level toolbar with sidebar
   collapse, settings, viewed-progress bar, and a revision-identity pill.
   Full-file toggle stays per-file (matches difit).
3. **File list** (`63c04dd`): added per-file `+N/-N` counts, colored status
   glyphs, an inline Viewed toggle sharing the same state path as the diff
   header's Viewed button (single source of truth), and an aggregate
   "N files, +N/-N" summary header.
4. **Diff row geometry** (`63c04dd`, `a3677ec`): 4em gutter applied
   consistently, addition/deletion backgrounds on tokens, deduped a
   conflicting duplicate `.hunk-header` rule, removed dead CSS.
5. **`/ask` vs. formal comment cards** (`a3677ec`): `/ask` threads now
   render in a blue-accented card showing the anchor and the selected-code
   snippet (the `Anchor.selected_code` field existed in state but was never
   rendered before this pass); formal comments kept a distinct green accent.
6. **Queue Home** (`a3677ec`): card padding/type/spacing tightened, status
   pills now reflect actual lifecycle state (`data-state`, previously always
   rendered amber — a real bug fixed as part of this pass), machine sidebar
   re-skinned onto the same token palette.
7. **Accessibility/responsive** (`4afd9db`): initial focus set in three
   dialogs that were silently falling back to their close button; reorderable
   queue cards now have an accessible name announcing the Alt+Arrow/Home/End
   shortcut; one real WCAG AA contrast failure fixed (completed-status pill);
   added an ~1120px breakpoint so the three-pane workspace degrades cleanly
   instead of clipping between 861–1094px window widths.
8. **Independent pane scrolling** (pre-`63c04dd`, direct fix in response to
   user report): `.workspace` grid items (`.files`, `.diff`, `.chat`) had no
   `overflow`/`min-height: 0`, so their flex children's `overflow: auto`
   never activated — the whole page grew and scrolled instead of each pane
   scrolling independently. Fixed with `overflow: hidden; min-height: 0` on
   the three panes and `overflow: hidden` on `.workspace` itself.
9. **Stale doc fix** (`9ee642c`): `frontend/README.md` claimed the UI "uses
   local fixture data only and does not request authentication, contact
   Copilot, or perform queue mutations" — false since the Tauri wiring
   landed (40+ real `invoke()` calls gated by `desktopAvailable`). Corrected.

## Visual differences that remain, and why

- **Continuous multi-file scroll**: difit renders every changed file
  stacked in one scrollable pane with sticky per-file headers; this app
  shows one selected file at a time from the file list. Converting to
  difit's model is a real interaction-model change (affects anchor
  positioning, comment-thread rendering, and viewed-state sync across
  simultaneously-mounted files), not a style change, and this app has zero
  test coverage to safely verify it. Deliberately deferred — see follow-up
  recommendation above (write tests first).
- **Ignore-whitespace toggle**: not added. `DiffLine` is `{type, content}`
  with no whitespace-normalized addition/deletion pairing, so a client-side
  toggle would either misrepresent real changes or require new backend diff
  parameters — out of scope for a visual pass, and a fake/no-op checkbox
  would violate the spec's "no dead ends"/honesty requirements.
- **Word-level diff emphasis**: not added, for the same reason — the
  current diff data model has no word/char-level segments; this is a real
  feature, not a style change.
- **File-list directory tree grouping / collapse-by-folder**: difit
  collapses single-child directory chains into one row; this app's file
  list stays a flat filtered list. Deferred as a smaller, separable
  follow-up.
- **AI-computed review order (`ReviewPlanPanel` in the legacy app)**:
  intentionally not ported — explicitly out of scope per the task.

## Behavioral regressions checked

- `npm run build` (tsc --noEmit + vite build) passes clean after every
  commit in this sequence.
- Every restyle pass was scoped to `frontend/src/App.tsx` and
  `frontend/src/styles.css` only; `api.ts`/`types.ts` and all Rust/Tauri
  code were left untouched by this work.
- Viewed-state now has a single source of truth (file-list checkbox and
  diff-header button call the same handler) rather than risking two paths
  drifting apart.
- Focus trapping, Escape, and focus restore verified present on all 9
  dialogs/drawers; the persistent (non-overlay) chat column was confirmed to
  intentionally *not* use the modal focus trap, since trapping focus in an
  always-mounted panel with no close affordance would strand keyboard users.
- Repo-qualified identity (`fileKey(repository_id, path)`) preserved in every
  touched key/callback — no multi-repo filename collisions introduced.
- No new `invoke()` calls, no new credential handling, no lifecycle/decision/
  publishing semantics changed — confirmed by reading every diff before
  committing.

## Screenshot comparison — limitation

**New packaged-app screenshots could not be captured in this session.** The
Tauri desktop crate (`src-tauri`) currently fails to compile:

```
error[E0425]: cannot find type `ReproductionPreview` in crate `review_queue_core`
   --> src-tauri/src/machines.rs:380:32
error[E0425]: cannot find type `ReproductionResult` in crate `review_queue_core`
   --> src-tauri/src/machines.rs:391:32
```

This comes from unrelated, concurrent, in-flight backend work on this same
branch (visible in `git status` as modifications to `crates/review-queue-cli/
src/main.rs`, `crates/review-queue-core/src/{machine,github,socket,store}.rs`,
`src-tauri/src/{commands,machines}.rs`, none of it touched by this frontend
migration) — `review_queue_core` appears to have moved `ReproductionPreview`/
`ReproductionResult` into a `reproduction` module without `src-tauri/src/
machines.rs` being updated to match. Per this task's constraints ("do not
replace or weaken the Rust/Tauri backend," "preserve every unrelated or
concurrent change"), I did not fix this — it isn't mine to fix blind, without
understanding what the concurrent work is mid-way through.

**What this means:** the reviewer screenshot the user captured earlier in
this session (showing the pre-fix scrollbar bug, since fixed) is the most
recent real packaged-app evidence available. Direct pixel-comparison
screenshots of Queue Home, split/unified reviewer, full-file view, inline
`/ask`, chat sheet, formal feedback drawer, connected-machine queue, and a
narrow layout — all captured at the legacy reference size and compared
against `legacy-queue-home.jpeg`/`legacy-reviewer.jpeg` — remain outstanding.
**Recommended next step:** once `src-tauri` builds again, run the app with
the same fixture used for the retained legacy screenshots and capture the
full set listed in the task; the CSS/markup changes in this report are ready
to verify visually at that point.

## Exact commands and results

| Command | Result |
| --- | --- |
| `cd frontend && npm run build` | Clean after every commit (`tsc --noEmit && vite build`, ~100ms build) |
| `cargo fmt --all -- --check` (workspace: `review-queue-core`, `review-queue-cli`) | 2 formatting diffs, both in `crates/review-queue-cli/tests/machine_daemon.rs` — a file modified by concurrent backend work, not this migration; not fixed |
| `cargo test --workspace --locked` | **95/95 passing**, 0 failed (7 CLI unit + 1 CLI integration + 80 core unit + 7 adversarial no-side-effect gate tests) |
| `cargo clippy --workspace --all-targets --locked` | Exit 0; 4 pre-existing warnings (1 `large_enum_variant` in `machine.rs`, 3 `useless_vec` in `main.rs` tests) — both in concurrently-modified files, not fixed |
| `cd src-tauri && cargo fmt -- --check` | Clean |
| `cd src-tauri && cargo test --locked` | **Fails to compile** — see limitation above, unrelated to this migration |
| `cd src-tauri && cargo clippy --all-targets --locked` | Same compile failure |
| `npm ci` | Clean — 78 packages installed, 0 vulnerabilities |

The two `src-tauri` E0425 errors should be resolved as part of finishing the
concurrent backend work already in progress on this branch — they are not a
product of this migration.
