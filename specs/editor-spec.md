# Launchpad — Editor Spec

## What the Editor Is

The editor is the third pillar of Launchpad, alongside the terminal and the git
panel. Its job is **not** to be your primary authoring surface — Launchpad is
terminal-first, and a lot of code gets written by CLI agents in a PTY. The
editor's job is to let you **read, review, and tweak** what's on disk: the files
an agent just changed, the config you need to nudge, the conflict you have to
resolve by hand.

That framing matters for prioritization. A standalone IDE optimizes for
green-field authoring. Launchpad's editor optimizes for **understanding change**
— which is why connecting it to the git layer we already built is the highest-
value move, not chasing feature parity with VS Code.

## Current State (honest assessment)

Good bones, frozen in place. The editor is a competent **text** editor that
never grew into a **code** editor while git and file-nav matured around it.

What exists (via `editor.js` `createEditor()` factory + `main.js` tab wiring):

- CodeMirror 6 instance, caller-owned lifecycle (no global singleton).
- Syntax highlighting for ~15 languages (js/ts/jsx/tsx/py/rs/html/css/scss/json/
  md/yaml/toml/sh) via `langMap`.
- Bracket matching, close brackets, fold gutter, indent-on-input, rectangular
  selection, crosshair cursor, highlight-selection-matches.
- Search / replace (Cmd+F / Cmd+H) via `@codemirror/search`.
- Optional vim mode (`@replit/codemirror-vim`).
- Theme: `oneDark` (dark) / default highlight (light) + `launchpadTheme`
  CSS-variable overrides.
- Status bar: Ln/Col, language name, LF/CRLF toggle, tab-size cycle, wrap
  toggle, undo/redo depth.
- Per-file overrides (`tabSize`, `wordWrap`, `lineEndings`) persisted via
  `getOverrides`/`setOverride`.
- Save with line-ending preservation, save-flash, conflict auto-stage.
- External-change handling: `fs-changed` reload, inode-based rename following,
  `stale` flagging with toast.
- Conflict mode: inline action bar + tinted ranges + `[Open 3-Way]`.

The three honest gaps:

1. **No language intelligence.** `autocompletion()` is generic word-matching,
   not LSP. `lintGutter()` is wired but **nothing feeds it** — zero diagnostics
   ever appear. No go-to-definition, no hover types, no real errors.
2. **The editor is blind to git.** We built a whole git layer; the editor gutter
   shows nothing about what changed. No per-line change markers, no inline hunk
   revert, no blame. The three pillars don't connect inside the editor.
3. **A dead setting.** `editorFontSize` has a UI control in `settingspanel.js`
   and is persisted in `settings.js`, but it is **never applied** — the theme
   hardcodes `fontSize: "12px"` in two places. The terminal honors its font-size
   setting live (`main.js` `termFontSize`); the editor silently ignores yours.
   This is a bug, not just a gap.

---

## The Three Tracks

Three investments of different size and character. We build them **one at a
time** in the order below; this spec defines all three up front so the
architecture of the early tracks doesn't paint the later ones into a corner.

### Track A — Git-Aware Editor

**Goal:** the editor understands and surfaces change. When you open a file an
agent touched, you see *what* it touched, line by line, and can act on it without
leaving the buffer. This is the track that ties the three pillars together and
has the highest value-per-effort for a review-oriented workflow.

**Feature set (in build order):**

1. **Change gutter (diff markers).** A dedicated gutter to the left of the line
   numbers paints a colored bar per line: added (green), modified (blue),
   deleted (red wedge between lines). Reflects the working-tree-vs-HEAD diff for
   the file.
2. **Hunk navigation.** Keyboard jumps to next / previous changed hunk
   (proposed: `Alt+J` / `Alt+K`, or `]c` / `[c` under vim).
3. **Inline hunk actions.** Click a gutter marker → small popover with
   **Revert hunk** and **Stage hunk**. Revert restores the HEAD version of those
   lines in the buffer (then a normal Cmd+S persists). Stage writes the buffer
   then stages — or, phase-2, stages just the hunk via a patch.
4. **Blame (opt-in).** A toggle (status-bar item or Cmd+Shift+B) that shows,
   per line, the short commit / author / relative date in a thin left margin.
   Click a blame entry → opens that commit in the existing commit-detail view.

**Architecture / approach:**

- **Data source.** We already compute structured diffs server-side
  (`get_file_diff(path, file_path, staged?)` in `git.rs`, returns hunks with
  line numbers; `diffrender.js` consumes the same shape). The change gutter
  needs a *line-keyed* view of that: a map of `lineNumber → "added" | "modified"
  | "deleted-above"`. Add a thin `get_file_line_changes(path, file_path)` Rust
  command (or derive it frontend-side from the existing hunk structure — prefer
  deriving to avoid a new round-trip, but a dedicated command keeps the hot path
  off the main diff renderer). Decide at implementation time; deriving is the
  default.
- **CodeMirror integration.** Use a `gutter()` + `GutterMarker` extension fed by
  a `StateField` holding the line-change map. The map is recomputed on: file
  open, save, and `fs-changed` for that file. Debounce live recompute while
  typing — recompute on idle (250ms) or on save, not per keystroke (a diff vs
  HEAD on every keypress is wasteful and the markers don't need to be that live).
- **Blame** is a separate Rust command `git_blame_file(path, file_path)`
  returning `Vec<{ start_line, line_count, oid, author, time, summary }>`
  (libgit2 `Repository::blame_file`). Rendered as a second optional gutter.
  Blame is heavier — fetch lazily only when the toggle is on, cache per
  (file, HEAD-oid), invalidate on commit/checkout.
- **Reuse the existing color tokens** from the git status work (`--git-added`,
  etc. / the `git-*` classes) so the gutter matches the file tree.

**Gotchas:**

- The buffer can be dirty (unsaved). Markers reflect **disk vs HEAD**, not
  buffer vs HEAD, until save — be explicit in the UI so a user isn't confused
  why their just-typed line isn't green yet. (Phase-2 nicety: diff the live
  buffer against HEAD for added-line markers without a disk write.)
- Files opened **outside** the project root (via terminal) may be in a different
  repo or no repo — gutter must no-op gracefully, same way `applyGitColors`
  anchors on `currentGitRoot`.
- Deleted-line markers render *between* lines, not *on* a line — CodeMirror
  gutters are per-line, so a deletion shows as a wedge on the line below the gap.

### Track B — Language Intelligence (LSP)

**Goal:** real code understanding. True completion, go-to-definition, hover
types, and live diagnostics in that empty lint gutter. This is the leap from
editor to IDE and the heaviest lift — staged so each phase ships value alone.

**Feature set (in phase order):**

1. **Diagnostics.** Squiggles + lint-gutter entries from the language server.
   This alone makes the editor feel alive and reuses the already-wired
   `lintGutter()`.
2. **Hover.** Hover a symbol → type signature / doc tooltip.
3. **Completion.** Replace generic word-completion with server-driven completion
   (context-aware, with kinds/icons).
4. **Go-to-definition.** Cmd+click or F12 → jump (opens the target file in a tab,
   reusing `createEditorTab` + `gotoLine`).
5. **Later:** find-references, rename-symbol, signature help, code actions.

**Architecture / approach:**

- **Rust-side LSP host.** A new `src-tauri/src/lsp.rs` module that manages
  language-server child processes (one per language per project): spawn
  `rust-analyzer`, `typescript-language-server --stdio`, `pyright-langserver
  --stdio`, etc. on demand; speak LSP JSON-RPC over stdio; multiplex requests;
  forward `publishDiagnostics` notifications to the frontend via a Tauri event
  (`lsp-diagnostics`). Model this on the existing PTY process management
  (`pty.rs`) — deferred reader, backpressure, clean teardown — it's the closest
  prior art in the codebase.
- **Server discovery & config.** Servers are external binaries the user installs
  (we do not bundle multi-hundred-MB toolchains). Detect on `PATH`; if missing,
  the editor degrades gracefully to today's behavior and surfaces a one-time,
  dismissible hint ("install rust-analyzer for Rust intelligence"). A settings
  section maps language → server command + args so power users can point at
  custom binaries.
- **Lifecycle = project-scoped.** Servers start lazily when the first file of a
  language opens, `initialize` with the **project root** as `rootUri` (matches
  the one-window-one-project model), and shut down on project teardown. Document
  sync: send `didOpen` / `didChange` (incremental, debounced) / `didClose`
  mirroring editor-tab lifecycle.
- **CodeMirror client.** A frontend `lspclient.js` translating between CM and
  LSP: positions (CM offsets ↔ LSP line/char — watch UTF-16 vs UTF-8 offset
  semantics, a classic LSP footgun), diagnostics → CM lint `Diagnostic[]`,
  completion → CM `CompletionSource`, hover → CM `hoverTooltip`. Evaluate
  `@codemirror/lsp-client` or a community package before hand-rolling; the
  protocol plumbing is the expensive part, not the CM glue.

**Gotchas:**

- LSP positions are UTF-16 code units by default; CM works in JS string offsets
  (also UTF-16) — usually fine, but multi-byte/emoji edge cases bite. Centralize
  the conversion in one place and test it.
- One server per (language, project) — don't spawn per-tab. Reference-count open
  documents; shut the server when the last doc of its language closes (or keep
  warm with an idle timeout; decide at build time).
- `rust-analyzer` on a cold project is slow + RAM-hungry. The "starting…" state
  needs UI (a spinner in the status bar) so diagnostics-not-yet-appearing reads
  as "indexing," not "broken."
- Security: language servers execute project config (build scripts,
  `rust-analyzer` proc-macro expansion). That's the same trust boundary as
  running the project's code locally — acceptable for a single-user dev tool,
  but note it; don't auto-start servers for a project the user only browsed.

### Track C — Polish Bundle

**Goal:** a pile of smaller daily-use wins, each independently shippable. Lower
ceiling than A/B but immediate quality-of-life, and a good warm-up that touches
the same `editor.js` surface area we'll lean on for A and B.

**Feature set (each independent — ship in any order):**

1. **Fix the dead fontSize setting.** Drive editor font size from
   `editorFontSize` instead of the hardcoded `12px`. Add a `setFontSize(n)` to
   the editor handle (Compartment-based, mirroring `setTabSize`/`setWordWrap`)
   and apply live on settings change, the way `termFontSize` already works. This
   is a bug fix and should land first regardless of track order.
2. **Format-on-save.** Shell out to the project's formatter (prettier, rustfmt,
   black, gofmt…) on Cmd+S, gated by a per-language setting and presence of the
   binary. Run on the saved file, reload the buffer from the formatted result
   (preserving cursor as best we can). Reuses the spawn-with-env hardening from
   `git.rs` (`apply_git_env` pattern). Off by default; opt-in per language.
3. **In-file symbol outline (Cmd+Shift+O).** A quick-open-style fuzzy palette of
   the current file's symbols. Pre-LSP: derive from CodeMirror's syntax tree
   (`@codemirror/language` `syntaxTree`) — headings for md, functions/classes for
   js/py/rs via tree node types. Post-LSP (Track B): upgrade to
   `documentSymbol`. Build the palette so the data source is swappable.
4. **Indentation guides.** Vertical guide lines per indent level
   (`@replit/codemirror-indentation-markers` or equivalent).
5. **Bracket-pair colorization.** Rainbow-match nested brackets for readability.
6. **Sticky scroll.** Pin the current scope header (function/class signature) to
   the top of the viewport as you scroll through a long body.
7. **Active-file sync in the tree.** Optional: reveal/scroll the file browser to
   the active editor tab's file (a "follow" toggle). Ties editor ↔ file-nav.

**Architecture / approach:**

- Most of these are self-contained CodeMirror extensions added to the
  `extensions` array in `createEditor`, gated by a setting. Keep them behind
  settings flags so the editor stays fast and uncluttered by default.
- Symbol outline and format-on-save are the two with backend/structure work;
  the rest are extension wiring + a CSS pass.
- The fontSize fix establishes the `setFontSize` Compartment pattern that any
  future live-reconfigurable option (theme, font family) will reuse.

---

## Cross-Cutting Architecture Notes

- **The factory stays the factory.** `createEditor()` keeps returning a handle;
  new live-reconfigurable options (fontSize, gutters, lsp) follow the existing
  Compartment + handle-method pattern (`setTabSize`, `setWordWrap`). No global
  editor singleton, ever.
- **Everything anchors on the active project.** Per the project-scoped rule:
  LSP `rootUri`, git gutter repo, formatter cwd all use `getActiveProject().path`
  — never `getCurrentPath()`.
- **Graceful degradation is mandatory.** Missing language server, file outside
  any repo, no formatter installed → the editor silently falls back to today's
  behavior. No feature may make a plain text edit worse.
- **Settings surface grows, defaults stay quiet.** Each feature gets a settings
  toggle; defaults keep the editor minimal. New keys live alongside the existing
  `editor*` keys in `settings.js` + `settingspanel.js`.
- **Tests.** Pure logic (line-change derivation from hunks, LSP↔CM position
  conversion, symbol extraction from syntax tree) gets Vitest coverage next to
  the module, per the existing `*.test.js` convention. Backend additions
  (line-changes command, blame, LSP framing) get Rust tests in `tests.rs`.

---

## Implementation Order

We work the tracks **one at a time**, but pull the standalone fontSize bug
forward since it's tiny and currently misleading.

### Phase 0 — Bug fix (do first, ~½ day)
- Track C #1: make `editorFontSize` actually drive the editor (live, via
  `setFontSize` Compartment). Closes a real "setting does nothing" bug.

### Phase 1 — Track A: Git-Aware Editor
1. Line-change derivation (frontend, from existing hunk data) + Vitest.
2. Change gutter extension wired into `createEditor`, recompute on
   open/save/`fs-changed`.
3. Hunk navigation keybindings.
4. Inline hunk actions (revert, then stage).
5. Blame command (`git_blame_file`) + opt-in blame gutter.

### Phase 2 — Track C: Polish Bundle (the rest)
6. Symbol outline (Cmd+Shift+O), syntax-tree-backed.
7. Format-on-save (opt-in per language).
8. Indentation guides, bracket-pair colors, sticky scroll, active-file sync —
   each behind a setting.

### Phase 3 — Track B: Language Intelligence (LSP)
9. Rust LSP host (`lsp.rs`): spawn + JSON-RPC framing for one server
   (typescript-language-server is the easiest first target).
10. Diagnostics end-to-end (server → event → lint gutter).
11. Hover.
12. Completion (replace word-completion).
13. Go-to-definition.
14. Upgrade symbol outline to `documentSymbol`; then references/rename/code
    actions as follow-ons.

Rationale for the order: Phase 1 (git-aware) is the highest value-per-effort and
reuses infra we already own. Phase 2 (polish) is low-risk warm-up that hardens
the `createEditor` surface. Phase 3 (LSP) is the big multi-stage lift and goes
last so the editor's foundations are solid before we bolt a process host onto it.

---

## What's Out of Scope (for now)

- **Bundling language toolchains.** Servers are user-installed binaries we detect
  on `PATH`. No shipping rust-analyzer in the `.app`.
- **A full debugger / DAP integration.** Way beyond the review-oriented mandate.
- **Remote / SSH editing.** Launchpad is a local workspace.
- **Collaborative editing / CRDTs.** Single user, single machine.
- **Notebook (.ipynb) editing.** The file browser can open them as text; rich
  notebook UI is not a goal.
- **A second authoring-first mode.** The editor stays a review/tweak surface;
  we're not competing with VS Code for green-field authoring.
