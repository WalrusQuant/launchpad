# Plan: Three Git-Shaped Gaps

## Context

Launchpad's git surface is solid for day-to-day work (status, stage, commit, push/pull, merge, conflicts via ours/theirs) but is missing three classes of operations that show up in real workflows:

1. **History rewriting** — no way to amend the last commit, cherry-pick, or interactively rebase. Users currently drop to the terminal.
2. **Conflict resolution UX** — `git_resolve_ours` / `git_resolve_theirs` exist as one-shot buttons, but a real merge often needs a *line-by-line* decision. The "Edit" button on a conflict opens the file in the standard editor with raw `<<<<<<<` markers and no help.
3. **Arbitrary diff** — `get_commit_details` shows one commit's diff; `get_file_diff` shows one file's working diff. There's no way to compare two branches or two commits.

User-confirmed scope:
- **Rebase**: amend + cherry-pick + reword/squash/drop + **drag-to-reorder UI**.
- **Conflicts**: inline marker decorations in the editor *first*, then a dedicated 3-pane tab.
- **Diff**: a new `diff` tab type (multi-file, persistent, multiple comparisons open at once).

The existing infrastructure can absorb most of this without architectural change. The cancellable shellout pattern (`GitOpSlot` / `run_git_cancellable` / `apply_git_env` in `src-tauri/src/lib.rs:1530-1700`) handles every long-running git child uniformly. The unified tab system in `src/main.js` already supports new tab types via the `type` field. The structured diff types (`FileDiff` / `HunkDiff` / `DiffLine` at `lib.rs:1377-1398`) and `collect_structured_diff` (`lib.rs:1400-1450`) are exactly what arbitrary-ref diffing needs.

---

## Phasing

Phases are ordered so each ships value standalone and later phases build on earlier ones. Ship Phase 1+2 together; everything after lands incrementally.

### Phase 1 — Shared foundations (small, no user-visible change)

Pure refactors that make the three features cleaner. Land first.

1. **Generalize ref validation.** Rename `is_valid_merge_branch_name` (`lib.rs:1812-1821`) to `is_valid_git_ref`. Add a sibling `is_valid_git_oid(s)` that accepts 4–40 hex chars (matches git's short-OID accept range). Use both in every new shellout that takes user input. The current merge call site (`lib.rs:1829`) keeps working — same body, new name.

2. **Extract reusable diff renderer.** Move the inline DOM-building loop from `showDiff` (`gitpanel.js:1025-1039`) into a new `src/diffrender.js` exporting `buildDiffHtml(hunks)` returning an HTML string, plus `buildFileDiffSection(fileDiff)` wrapping multiple files with collapsible per-file headers. Update `gitpanel.js:showDiff` to call the new helper. CSS classes stay identical (`.diff-add`, `.diff-del`, `.diff-hunk-header`, etc.).

3. **Generalize the in-flight op pattern.** `gitpanel.js`'s `inFlightOp` flag + `invokeWithTimeout` (`gitpanel.js:13-30`) are panel-local. Move them into `src/git.js` as exports so the rebase tab and diff tab can use the same cancellation flow. The git panel keeps polling-skip semantics by reading the shared flag.

**Files:** `src-tauri/src/lib.rs`, `src/git.js`, `src/gitpanel.js`, **new** `src/diffrender.js`.

---

### Phase 2 — Diff between arbitrary refs (new `diff` tab type)

Smallest of the three features and the one that touches the least delicate code.

#### Backend

1. New command `get_diff_between_refs(path: String, from_ref: String, to_ref: String) -> Result<RefDiff, String>`:
   - Validate both refs with `is_valid_git_ref` (and accept oids via `is_valid_git_oid`).
   - `Repository::revparse_single(ref).peel_to_tree()` for each side.
   - `repo.diff_tree_to_tree(Some(&from), Some(&to), &mut DiffOptions::new())`.
   - Reuse `collect_structured_diff` (`lib.rs:1400-1450`) to walk delta → hunk → line and produce a `Vec<FileDiff>`.
   - Return `RefDiff { from_ref, to_ref, files: Vec<FileDiff>, stats: { files_changed, additions, deletions } }`.
2. Register in `invoke_handler`.

#### Frontend

1. **New tab type `"diff"`** in `main.js`. Tab shape: `{ type: "diff", containerEl, fromRef, toRef, refDiff, selectedFileIndex }`. Dedupe key: `diff::${fromRef}..${toRef}`.
2. **Layout** (built directly, not via CodeMirror): two-column flex inside `tab.containerEl`. Left = sticky file list with per-file `+N -N` stats; right = scrollable diff produced by `buildFileDiffSection(file)` from Phase 1. Clicking a file scrolls the right pane to that file's anchor.
3. **Entry points**:
   - **Commit context menu** (new `gitpanel.js` handler on `.gp-commit` right-click): "Compare with HEAD", "Compare with parent" (resolves to `oid^`), "Copy OID", "Compare with…" (prompts for ref).
   - **Branches section**: a small "Compare…" button at the section header, opens a two-ref picker popup (reuses `showConfirmPopup` style; `gitpanel.js:223-262`) with two `<select>` of all local + remote branches.
4. **Tab cleanup**: `doCloseTab` only needs to drop the entry from `tabs`; no `editorView.destroy()` call (no CodeMirror).

**Files:** `src-tauri/src/lib.rs`, `src/main.js`, `src/gitpanel.js`, `src/diffrender.js`, `src/styles.css` (new section: `.diff-tab`, `.diff-file-list`, `.diff-tab-content`).

---

### Phase 3 — Amend and cherry-pick

#### Backend

1. **`git_amend_commit(path, message: Option<String>, include_staged: bool) -> Result<String, String>`** — pure libgit2 (no shellout):
   - Open repo, find HEAD commit.
   - If `include_staged`: read index, write tree from index. Else use `head.tree()`.
   - `head.amend(Some("HEAD"), Some(&signature), Some(&signature), None, message.as_deref(), Some(&new_tree))`.
   - Return short OID. Refuse if HEAD is unborn or detached unless explicitly allowed.

2. **`git_cherry_pick(path, oid, state) -> Result<CherryPickResult, String>`** — shellout via `run_git_cancellable`:
   - Validate OID with `is_valid_git_oid`.
   - Run `git cherry-pick <oid>`.
   - On nonzero exit, check `.git/CHERRY_PICK_HEAD` existence + read conflict files via `get_git_status`. Return `CherryPickResult { ok: bool, conflicted_files: Vec<String> }` so the frontend can route to the conflict UI without a second round-trip.

3. **`git_cherry_pick_abort(path)`** / **`git_cherry_pick_continue(path)`** — shellouts via `run_git_cancellable`.

4. **`get_pending_op_state(path) -> PendingOpState`** — single libgit2-based query the frontend polls (every 3s, alongside the existing git status poll). Detects: `merge` (`MERGE_HEAD` exists), `cherry-pick` (`CHERRY_PICK_HEAD`), `rebase` (`rebase-merge/` or `rebase-apply/` dir). Returns `{ kind: "none" | "merge" | "cherry_pick" | "rebase", current_step?, total_steps?, head_message? }`. This is the foundation for showing op-in-progress banners across all three flows.

#### Frontend

1. **Amend** button in commit form (`gitpanel.js:444-470`):
   - Visible only when HEAD is not unborn AND we're not mid-merge/rebase/cherry-pick.
   - Two modes: "Amend message" (when no staged changes; opens a textbox prefilled with the last commit's message) and "Amend with staged" (when staged > 0; uses the new message if typed, else keeps the old).
   - Calls `git_amend_commit`. Refresh + toast on success.

2. **Cherry-pick** entry in commit-row context menu (Phase 2's new menu).
   - On conflict result: surface a toast "Cherry-pick has conflicts in N files" + render the conflicts banner (existing code path) + show Abort/Continue buttons in a new "Pending Operation" banner driven by `get_pending_op_state`.

3. **Pending Operation banner** in `gitpanel.js`. Renders above conflicts section when state ≠ "none". For each kind, shows the right buttons (Abort/Continue/Skip-only-for-rebase). This is shared infrastructure with Phase 5.

**Files:** `src-tauri/src/lib.rs`, `src/gitpanel.js`, `src/git.js` (poll integration), `src/styles.css`.

---

### Phase 4 — Inline 3-way conflict editor (CodeMirror decorations)

This unblocks Phase 5: rebase and cherry-pick conflicts route through the same UI.

1. **New module `src/conflictmarkers.js`**:
   - `parseConflictBlocks(text) -> Block[]` returning `{ start, oursEnd, baseStart?, baseEnd?, theirsStart, end, oursText, baseText?, theirsText }`. Handles both 2-way (`<<<<<<<` / `=======` / `>>>>>>>`) and diff3 (`|||||||` base section).
   - `replaceBlock(doc, block, choice: "ours"|"theirs"|"both"|string)` returning a CodeMirror change spec.

2. **CodeMirror extension** in same module:
   - StateField holds `Block[]` recomputed on doc change.
   - ViewPlugin produces `Decoration.mark` for ours/theirs/base ranges (background tint via CSS classes `.cm-conflict-ours`, `.cm-conflict-theirs`, `.cm-conflict-base`).
   - WidgetType for the marker lines themselves: replaces the `<<<<<<< HEAD` line (and its siblings) with a button bar `[Accept Ours] [Accept Theirs] [Accept Both] [Open 3-Way]`. Buttons dispatch a transaction calling `replaceBlock`.
   - Extension exported as `conflictExtension({ onResolveAll })` so callers can hook into "all conflicts cleared".

3. **Wire into `editor.js`**: `createEditor` accepts a new option `conflictMode: boolean`. When true, the conflict extension is appended to the extensions array. `main.js`'s `createEditorTab` opts in when the file is in `getGitStatus().conflictFiles`.

4. **Resolve flow**:
   - Each button mutates the doc; the file becomes dirty.
   - On Cmd+S, the existing save flow writes the file. After write, frontend calls `git_stage_file(path, file_path)` automatically when no markers remain in the saved text — this is git's standard "edited + add = resolved" pattern. Toast: "Resolved <file>".
   - "Open 3-Way" button is a no-op until Phase 6; in Phase 4 it opens an info toast.

**Files:** `src-tauri/src/lib.rs` (no change), **new** `src/conflictmarkers.js`, `src/editor.js`, `src/main.js`, `src/styles.css` (new: `.cm-conflict-*` classes, `.conflict-action-bar`).

---

### Phase 5 — Interactive rebase with drag-to-reorder

Largest feature. Builds on Phase 1 (validation), Phase 3 (`get_pending_op_state`, abort/continue plumbing), and Phase 4 (conflict UI for rebase conflicts).

#### Backend

1. **`get_rebase_candidate_commits(path, count) -> Vec<CommitInfo>`** — convenience wrapper around `get_commits` that excludes merge commits and stops at the first commit shared with the upstream (so users don't accidentally rewrite published history). Returns the editable subset.

2. **`git_rebase_interactive_apply(path, base_oid: String, todo: Vec<RebaseTodoEntry>, state) -> Result<RebaseResult, String>`** — the core of the feature:
   - `RebaseTodoEntry { action: "pick"|"reword"|"squash"|"fixup"|"drop"|"edit", oid: String, new_message: Option<String> }`.
   - Validate every oid + the base.
   - **Driver pattern**: write JSON of `todo` to a temp file. Generate two helper shell scripts:
     - `seq_editor.sh` — reads `$1` (rebase-todo path provided by git), the JSON temp file, and rewrites the todo file accordingly. Lines for `drop` are deleted; `reword`/`squash` keep their action keywords.
     - `commit_editor.sh` — for each `reword`/`squash`, reads the next entry's `new_message` from the JSON and writes it to `$1` (the commit message file).
   - Spawn `git rebase -i <base>` under `run_git_cancellable` with `GIT_SEQUENCE_EDITOR=<seq_editor.sh>` and `GIT_EDITOR=<commit_editor.sh>` plus `apply_git_env`.
   - Return `RebaseResult { ok: bool, stopped_at: Option<String>, conflicted_files: Vec<String>, completed: bool }`.
   - On conflict: leave rebase state, return `ok: false` with conflict info; frontend routes to the inline conflict UI from Phase 4 + the Pending Operation banner.
   - Helper scripts are written next to the JSON temp file in the OS temp dir, marked `0o700`, and cleaned up in a `defer`-style guard regardless of success/failure.

3. **`git_rebase_abort(path)`** / **`git_rebase_continue(path)`** / **`git_rebase_skip(path)`** — shellouts via `run_git_cancellable`.

#### Frontend

1. **New tab type `"rebase"`** in `main.js`. Shape: `{ type: "rebase", containerEl, baseOid, todo: TodoEntry[], originalTodo: TodoEntry[] }`. Dedupe key: `rebase::${baseOid}`.

2. **Layout**: vertical list of commit rows. Each row has:
   - Drag handle on the left (HTML5 DnD, modeled on the tab drag-to-move logic in `main.js`).
   - Action `<select>`: pick / reword / squash / fixup / drop / edit.
   - Short OID + commit subject.
   - For `reword` / `squash`: an inline message editor (revealed when selected) with the current message prefilled.
   - Footer: `[Apply]` (primary), `[Reset]` (restores `originalTodo`), `[Cancel]` (closes tab).

3. **Entry points**:
   - "Rebase from here…" entry in commit-row context menu — opens a rebase tab with `base_oid = clicked_oid^` and `todo` populated from `get_rebase_candidate_commits`.
   - "Rebase onto upstream…" button in branches section, when `behind > 0`.

4. **Apply flow**:
   - Disables Apply, sets `inFlightOp`, calls `git_rebase_interactive_apply`.
   - On `ok: true` + `completed: true`: closes the rebase tab, refreshes git panel, toast "Rebase complete".
   - On `ok: false` (conflicts): closes the rebase tab, switches focus to the conflicted file (opens it via Phase 4 conflict editor), shows the Pending Operation banner with Abort/Continue/Skip.

5. **Mid-rebase polling**: the Pending Operation banner from Phase 3 already covers rebase. After conflicts are resolved (file saved + auto-staged via Phase 4), Continue runs `git_rebase_continue`. Progress text comes from `get_pending_op_state`'s `current_step / total_steps`.

**Files:** `src-tauri/src/lib.rs`, `src/main.js`, `src/gitpanel.js`, `src/git.js`, `src/styles.css` (new: `.rebase-tab`, `.rebase-row`, `.rebase-handle`, `.rebase-action-select`).

---

### Phase 6 — Dedicated 3-pane merge tab (deferred, ship after Phase 4 has soaked)

1. **Backend** — `get_conflict_versions(path, file_path) -> ConflictVersions { ours, theirs, base, merged }` reading libgit2 index entries at stages 1 (base), 2 (ours), 3 (theirs); `merged` is the current working tree text.

2. **Frontend** — new tab type `"merge"`:
   - Three CodeMirror instances created via `createEditor`. Ours and theirs are read-only (`EditorState.readOnly.of(true)`); merged is editable.
   - Per-hunk `[Take Ours]` / `[Take Theirs]` / `[Take Both]` buttons in the merged pane's gutter.
   - Synchronized scrolling: line-anchored (use the merged pane's scrollTop → line position; map to ours/theirs lines via the parsed hunk structure).
   - Save writes the merged pane to disk + auto-stages (same flow as Phase 4).

3. **Entry point** — "Open 3-Way" button in Phase 4's inline action bar now becomes functional.

**Files:** `src-tauri/src/lib.rs`, `src/main.js`, `src/editor.js`, `src/styles.css`.

---

## Cross-Cutting Concerns

- **No new validators per feature.** Every new shellout uses `is_valid_git_ref` (refs/branches) + `is_valid_git_oid` (oids) from Phase 1.
- **Cancellation everywhere.** Every long-running new command (`git_cherry_pick`, `git_rebase_interactive_apply`, abort/continue) goes through `run_git_cancellable` and is wired to the shared `inFlightOp` flag in the frontend.
- **Conflict routing is one path.** Merge, cherry-pick, and rebase all surface conflicts via `get_git_status`'s existing `conflict` entries → Phase 4 inline editor (and optionally Phase 6 3-pane). No feature-specific conflict UI.
- **Pending op state is one query.** `get_pending_op_state` is the single source of truth for "are we mid-something" and replaces ad-hoc detection.
- **Refresh discipline.** Every command that mutates repo state ends with `refreshPanel(null, true)`. The git panel's snapshot-comparison early-return (`gitpanel.js`) prevents flicker.

---

## Critical Files

| File | Touched in phases |
|------|------------------|
| `src-tauri/src/lib.rs` | 1, 2, 3, 5, 6 — all new commands and validators |
| `src/main.js` | 2, 3, 4, 5, 6 — three new tab types + conflict-mode editor wiring |
| `src/gitpanel.js` | 1, 2, 3, 5 — context menu, compare button, amend button, rebase entry, pending-op banner |
| `src/git.js` | 1, 3 — shared op flag, pending-op poll integration |
| `src/editor.js` | 4, 6 — `conflictMode` and `readOnly` options |
| `src/diffrender.js` (new) | 1, 2 — shared diff HTML builder |
| `src/conflictmarkers.js` (new) | 4 — parser + CodeMirror extension |
| `src/styles.css` | 2, 3, 4, 5, 6 — new sections per feature |

## Existing Code to Reuse (Do Not Reinvent)

- `apply_git_env` (`lib.rs:1597`) — every spawned git child.
- `run_git_cancellable` / `spawn_git_under_slot` (`lib.rs:1611-1700`) — every shellout.
- `collect_structured_diff` (`lib.rs:1400`) — Phase 2's ref-diff command.
- `FileDiff` / `HunkDiff` / `DiffLine` (`lib.rs:1377-1398`) — same wire format for all diffs.
- `Repository::revparse_single` + `peel_to_tree` (libgit2) — ref resolution everywhere.
- `showConfirmPopup` (`gitpanel.js:223`) — destructive confirms (rebase apply, cherry-pick onto detached HEAD, abort).
- `get_git_status`'s conflict files (`lib.rs:408`) — the only conflict source-of-truth.
- `createEditorTab` pattern (`main.js:592-775`) — template for diff/rebase/merge tabs.
- The unified tab bar's drag-to-move plumbing (`main.js`) — template for rebase row drag-to-reorder.

---

## Verification

Each phase ships with manual + Rust-test coverage. UI work is verified live in `npx tauri dev`.

**Phase 1**
- `cargo test --manifest-path src-tauri/Cargo.toml` — add unit tests for `is_valid_git_ref` (reject empty, leading `-`, whitespace, `..`) and `is_valid_git_oid` (accept 4-char and 40-char hex; reject 3-char, 41-char, non-hex).
- Run merge end-to-end to confirm the rename didn't break the call site.

**Phase 2**
- Open two branches; "Compare branches" → diff tab opens; click each file → right pane scrolls; close tab → state cleared.
- Right-click an old commit → "Compare with HEAD" → multi-file diff renders matching `git diff <oid>..HEAD` from the terminal.
- Rust test: `get_diff_between_refs` against a fixture repo (use the existing `LAUNCHPAD_HOME` test isolation hook).

**Phase 3**
- Stage a file, edit message, click Amend → HEAD message + tree updated; `git log -1` confirms.
- Cherry-pick a clean commit → success toast; `git log` shows it.
- Cherry-pick a conflicting commit → conflicts banner + Pending Operation banner appears; "Abort" returns to clean state.

**Phase 4**
- Trigger a conflict via merge of a divergent branch; open conflicted file → `<<<<<<<`/`=======`/`>>>>>>>` lines replaced by the action bar; backgrounds tinted.
- Click "Accept Ours" → block collapses to ours text; Cmd+S → file written + auto-staged; conflicts section clears for that file.
- Open a clean file (no markers) → no decorations, no perf hit.

**Phase 5**
- Right-click a commit 4 back from HEAD → "Rebase from here…" → rebase tab opens with 3 candidate commits.
- Drag the middle row to the top, set bottom to `squash` with a new message, top to `drop`, apply.
- `git log` verifies new order, new squash message, dropped commit gone.
- Trigger a rebase conflict → flow lands in Phase 4 conflict editor → save + Continue completes.
- Cancel mid-rebase → Pending Operation banner's Abort returns the branch to its pre-rebase state.

**Phase 6**
- Open 3-pane merge tab from a conflict → all three panes load, ours/theirs read-only, merged editable.
- Per-hunk Take Ours / Take Theirs / Take Both each apply correctly.
- Synchronized scrolling stays line-aligned through a 1000-line conflicted file.
