# difit visual migration report

Scope: restyle Review Queue's reviewer and Queue Home to closely match the
legacy `cmux-localreview` / difit reviewer's look and density, without
touching the Rust/Tauri backend, `frontend/src/api.ts`, `frontend/src/types.ts`,
or any product/security invariant. Commits: `63c04dd`, `a3677ec`, `4afd9db`,
`5e8cd3b`, `9ee642c` on `codex/review-queue-v0.1.0`.

## What this was, concretely

The original pass started from `frontend/src/App.tsx`, then a single large
component tree with no `WorkspaceShell.tsx`/`QueueHome.tsx` decomposition. It
already implemented the full difit-shaped *product* behavior — repo-qualified
file list, split/unified/full-file diff modes, hunk navigation, Viewed state,
inline `/ask`, formal comments, a decision bar, Queue Home with Local/GitHub
columns and a machine sidebar — routed entirely through the closed `api.ts`
command surface. What it lacked was difit's visual density, exact palette,
toolbar layout, and file-list richness. The original pass was a **visual and
markup restyle within the existing component boundaries**, not a rewrite; the
current bounded file-tree extraction is described below.

## Current presentation structure

The original migration deliberately kept the reviewer inside `App.tsx`. The
current working tree has since made one bounded extraction:
`frontend/src/RepositoryFileTree.tsx` owns only the repository-qualified file
tree's filtering and expansion state. Selection and Viewed persistence remain
in `App.tsx` and continue through the existing API path. It is not a general
presentation-adapter layer and it does not change the `ReviewRound` or
`MaterializedDiff` command/data boundary.

The frontend now also has a Vitest + Testing Library fixture harness. The
current suite is green and covers durable-chat recovery, stale-poll
cancellation, cached-machine visibility, responsive pane controls, and the
repository tree; see "Current verification and remaining gaps" below.

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

### Current follow-up: file tree, responsive escape hatches, and semantics

The current working tree adds the following implementation work. It is covered
by the new packaged-app acceptance addendum below, while the older notarized
release evidence remains historical.

10. **Repository-aware file tree:** `RepositoryFileTree` renders each
    repository as its own root, supports filtering, expandable directories,
    and collapsed single-child directory chains. Its identity is
    `repository_id + path`, so equal paths in two repositories cannot collide.
    It retains the existing selected-file and Viewed persistence callbacks.
11. **Responsive pane controls:** the reviewer toolbar now exposes named
    Files and Chat controls with `aria-controls` and `aria-expanded`. The
    narrow rules no longer remove Queue Home secondary actions; at narrow
    widths the Files pane is explicitly opened/closed rather than silently
    disappearing, and the decision actions wrap instead of using the previous
    horizontal action strip.
12. **Accessible state:** Unified/Split and Full file expose their pressed
    state; viewed progress is a determinate `progressbar`; status/decision
    text uses dedicated higher-contrast foreground tokens.

13. **Continuous multi-file review:** Unified and Split now render every
    changed file in one scroll surface with sticky, collapsible per-file
    headers. Selecting a path in the tree expands and scrolls to that file
    without changing immutable snapshot, hunk-action, or Viewed semantics.

A hunk `/ask` click still records the pending anchor without automatically
opening a collapsed Chat pane. The labelled Chat control makes the pending
anchor reachable at every supported width; automatically opening Chat remains
an optional follow-up rather than a correctness gate.

### Second pass: fixture harness + live browser verification

At the time of the second pass, `src-tauri` did not build (see the historical
record below), so a dev-only fixture harness was added:
`frontend/src/api.fixture.ts` (`9c6d475`) is a
structural drop-in for `api.ts` — same 45 exports, in-memory mutable fixture
store covering 5 rounds across local/github/machine/completed, a multi-repo
diff (binary/added/deleted/long-path files), formal comments, an `/ask`
conversation, and a connected machine — wired in only via a `--mode fixture`
Vite alias (`npm run dev:fixture`). `api.ts`/`App.tsx` are untouched by it,
and the default `npm run build`/`dev` are unaffected (verified: the fixture
module is fully tree-shaken out of the production bundle).

This unblocked driving the actual running UI in a browser and comparing it
directly against `legacy-reviewer.jpeg`/`legacy-queue-home.jpeg` side by
side, which surfaced three real defects the static-code review above missed:

10. **Split mode had no diff coloring at all** (`4a7be38`) — the most
    significant finding. "Split" was wired to `PinnedFilePane`, a *different*
    feature (full pinned-file comparison, `materializeRoundFile` on both
    sides) than what difit's Split view is: a synced two-column diff with
    red/green backgrounds. Clicking Split showed two plain, uncolored full
    files. Fixed by giving `DiffFileView`/`DiffHunkView` a `layout: "unified"
    | "split"` prop; split layout now pairs each hunk's deletion/addition
    lines into synchronized rows (`pairSplitLines`) using the same hunks
    already loaded for unified mode, with the same addition/deletion color
    tokens and gutter width — matching difit's `SideBySideDiffChunk`
    pattern. "Full file" keeps using `PinnedFilePane` unchanged (simplified,
    since it now only ever renders one side). Line-level click-to-select for
    `/ask`/comment anchoring stays unified-mode-only in this pass — split
    mode still exposes the same hunk-level `/ask`/`+ Comment` buttons in its
    header, just not per-line selection; documented as a known scope limit
    below rather than silently degraded.
11. **Split-diff empty cells were invisible** (`7c25048`) — the "no
    corresponding line" side of a pure-addition or pure-deletion hunk used
    `opacity: 0.5` on a dark background, which blended into the page
    background and read as a misaligned/broken layout rather than an
    intentional empty cell. Switched to the flat `--color-diff-neutral-bg`
    token (previously defined but unused).
12. **Settings dialog text collision** (`fb35cd3`) — "GitHub OAuth public
    client" (`<b>`) and "Advanced public Client ID" (`<label>`) were both
    inline elements with no block sibling between them, rendering as one
    run-together line: "GitHub OAuth public clientAdvanced public Client
    ID". Made settings-dialog section headers/labels block-level with
    proper spacing, scoped narrowly so `ConnectionRow`'s own `<b>`/`<p>`
    layout is unaffected.

All three were verified live: Queue Home, the multi-repo reviewer (unified
and split), full-file mode, binary/deleted/long-path files, the machine
queue, Review round details, Application settings, and Submit local review
were all opened and visually inspected against the retained legacy
screenshots and the difit visual-token system established in the first
pass. `npm run build` re-verified clean after each fix.

## Visual differences that remain, and why

- **Large-diff virtualization**: continuous multi-file scroll with sticky
  per-file headers is implemented. Unlike difit, the current reviewer mounts
  every expanded file and hunk eagerly. Intersection-observer/lazy rendering
  remains future performance work for unusually large reviews; it must retain
  stable anchor navigation and Viewed state while content enters and leaves
  the DOM.
- **Ignore-whitespace toggle**: not added. `DiffLine` is `{type, content}`
  with no whitespace-normalized addition/deletion pairing, so a client-side
  toggle would either misrepresent real changes or require new backend diff
  parameters — out of scope for a visual pass, and a fake/no-op checkbox
  would violate the spec's "no dead ends"/honesty requirements.
- **Word-level diff emphasis**: not added, for the same reason — the
  current diff data model has no word/char-level segments; this is a real
  feature, not a style change.
- **File-list directory tree grouping / collapse-by-folder**: implemented in
  the current follow-up. The repository-aware tree keeps repository roots
  distinct, filters paths, permits explicit directory expansion, and collapses
  unambiguous single-child chains. Unified and Split now use difit's
  continuous multi-file scroll pattern with sticky per-file headers.
- **AI-computed review order (`ReviewPlanPanel` in the legacy app)**:
  intentionally not ported — explicitly out of scope per the task.
- **Split-mode line selection**: implemented. Each selectable split cell
  retains its original hunk index, old/new side, and line number. Clicking a
  deletion produces a `LEFT` anchor, clicking an addition produces a `RIGHT`
  anchor, and shift-selection extends only on the same side. The fixture suite
  sends a real in-memory `/ask` turn and asserts the resulting side-qualified
  workspace anchor.
- **Production notarization:** the Files/Chat escape hatches, repository tree,
  and continuous diff now have fixture, browser, and universal packaged-app
  evidence. The package used an ad-hoc signature; production notarization is
  tracked separately by the release gate.

## Current verification and remaining gaps

This section separates the retained historical migration checks from checks
run against the **current working tree**.

- `cd frontend && npm test` passed: 2 files, 15 tests.
- `cd frontend && npm run build` and `npx vite build --mode fixture` passed.
- The in-app browser fixture passed at 1280px, 1024px, and 560px. Files and
  Chat remained reachable, continuous diffs rendered at non-zero width, and
  the interrupted `/ask` retry created a fresh explicit prompt while retaining
  prior history. Retained screenshots:
  `../evidence/v0.1.0/difit-reviewer-desktop.png`,
  `../evidence/v0.1.0/difit-reviewer-1024.png`,
  `../evidence/v0.1.0/difit-reviewer-560.png`, and
  `../evidence/v0.1.0/difit-queue-home.png`.
- A universal macOS `.app` was built with ad-hoc signing and updater artifacts
  disabled. It reopened the retained multi-repository round, resized to the
  560px minimum, and successfully exercised the Files and Chat controls,
  Settings/Escape, and continuous review. See
  `../evidence/v0.1.0/difit-native-reviewer.png` and
  `../evidence/v0.1.0/difit-native-reviewer-560.png`.

### Verification checklist and retained evidence

The browser fixture was exercised at the three exact viewport widths. The
packaged app was exercised at its wide launch size and by dragging to the
configured 560px minimum; production notarization remains a separate gate.

| Viewport | Verify |
| --- | --- |
| 1280px | Queue Home actions remain available; reviewer shows Files, diff, and Chat; select/filter/collapse a nested file-tree path; switch Unified, Split, and Full file; mark a file Viewed; open and close Settings with keyboard focus returning to its trigger. |
| 1024px | Open a review with Chat initially collapsed; use the labelled Chat control to open and close it, confirm `aria-expanded` follows state, then verify an `/ask` anchor is visible after opening Chat. Confirm Files remains selectable and no pane/action is clipped. |
| 560px | Start with Files collapsed; use Open files to reveal the file tree, select a different file, and close it again. Open Chat, exercise the wrapping decision actions, and verify both panes/actions remain reachable by keyboard without horizontal clipping. |
| Packaged app | Open wide, drag to the configured 560px minimum, reopen the persisted multi-repository round, and perform a no-op review (no Submit/Publish). This confirms Tauri window behavior without mutating source or remote state. |

## Historical behavioral regressions checked

- `npm run build` (tsc --noEmit + vite build) passes clean after every
  commit in this sequence.
- Every restyle pass was scoped to `frontend/src/App.tsx` and
  `frontend/src/styles.css` only; `api.ts`/`types.ts` and all Rust/Tauri
  code were left untouched by this work.
- Viewed-state now has a single source of truth (file-list checkbox and
  diff-header button call the same handler) rather than risking two paths
  drifting apart.
- The original migration record reports focus trapping, Escape, and focus
  restore across nine dialogs/drawers. This follow-up re-exercised only the
  Settings dialog focus return; it did not rerun that full historical matrix.
- Repo-qualified identity (`fileKey(repository_id, path)`) preserved in every
  touched key/callback — no multi-repo filename collisions introduced.
- No new `invoke()` calls, no new credential handling, no lifecycle/decision/
  publishing semantics changed — confirmed by reading every diff before
  committing.

## Historical screenshot comparison

The following is retained historical fixture evidence from the original
migration pass. It is not a current signed-app or responsive acceptance run.

**At the time of this historical run, the packaged app could not be built.**
`src-tauri` failed to compile because of unrelated concurrent backend work
(not touched by this migration):

```
error[E0425]: cannot find type `ReproductionPreview` in crate `review_queue_core`
   --> src-tauri/src/machines.rs:380:32
error[E0425]: cannot find type `ReproductionResult` in crate `review_queue_core`
   --> src-tauri/src/machines.rs:391:32
```

Rather than wait on that, this session added a **dev-only fixture harness**
(`frontend/src/api.fixture.ts` + `npm run dev:fixture`, see above) and used
Chrome browser automation to drive the real running UI against realistic
fixture data, comparing it directly against the retained
`legacy-queue-home.jpeg`/`legacy-reviewer.jpeg` screenshots. This is not a
substitute for a real packaged-app screenshot pass — it doesn't exercise the
Tauri window chrome, native menus, or actual backend data — but it verified,
live, rather than by reading code: Queue Home, the sidebar/machine queue,
the multi-repo reviewer in both unified and (now-fixed) split mode, full-file
mode, binary/deleted/added/long-path file rendering, the Review round
details modal, Application settings, and Submit local review. It's also what
surfaced the three real bugs listed above (split-mode coloring, empty-cell
visibility, settings text collision) that a pure code read had missed.

**Still outstanding:** a formal pixel-comparison capture at the exact legacy
reference size (1152×768). Narrow layouts have since been checked at 1024px
and 560px in the fixture browser and at the 560px native window minimum; the
retained current evidence is listed above.

## Historical commands and results

These are the original migration commands/results, retained for provenance;
they are not claims about the current working tree. Current frontend results
are stated in "Current verification and remaining gaps" above.

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
| `npm run dev:fixture` (added this pass) | Boots clean; used for live browser verification of every fix in the "second pass" section above |

The two `src-tauri` E0425 errors should be resolved as part of finishing the
concurrent backend work already in progress on this branch — they are not a
product of this migration.
