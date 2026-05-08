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

> **Note on line numbers.** The line refs throughout this document are indicative — accurate at the time of writing but expected to drift. Use them as starting points; treat the symbol names (`run_git_cancellable`, `collect_structured_diff`, `showConfirmPopup`) as the durable anchors.

> **Platform.** Launchpad is macOS-only (per `CLAUDE.md`). Anywhere this plan calls for `0o700` temp scripts, POSIX shebangs, or `kill -9`, that assumption is load-bearing. A future Windows port would need a separate driver design for Phase 5.

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

1. **New tab type `"diff"`** in `main.js`. Tab shape: `{ type: "diff", containerEl, fromRef, toRef, refDiff, selectedFileIndex }`. Dedupe key: `diff::${fromRef}..${toRef}` — used only as a JS Map key, never as a DOM id. Any DOM ids derived from refs go through a sanitizer (`encodeURIComponent` or a short hash) so refs like `feature/foo` or `release-1.0` don't break selectors.
2. **Layout** (built directly, not via CodeMirror): two-column flex inside `tab.containerEl`. Left = sticky file list with per-file `+N -N` stats; right = scrollable diff produced by `buildFileDiffSection(file)` from Phase 1. Clicking a file scrolls the right pane to that file's anchor.
3. **Entry points**:
   - **Commit context menu** (new `gitpanel.js` handler on `.gp-commit` right-click): "Compare with HEAD", "Compare with parent" (resolves to `oid^`), "Copy OID", "Compare with…" (prompts for ref).
   - **Branches section**: a small "Compare…" button at the section header, opens a two-ref picker popup (reuses `showConfirmPopup` style; `gitpanel.js:223-262`) with two `<select>` of all local + remote branches.
4. **Tab cleanup**: `doCloseTab` only needs to drop the entry from `tabs`; no `editorView.destroy()` call (no CodeMirror).

**Files:** `src-tauri/src/lib.rs`, `src/main.js`, `src/gitpanel.js`, `src/diffrender.js`, `src/styles.css` (new section: `.diff-tab`, `.diff-file-list`, `.diff-tab-content`).

---

### Phase 3 — Amend and cherry-pick

#### Backend

1. **`git_amend_commit(path, message: Option<String>, include_staged: bool) -> Result<AmendResult, String>`** — pure libgit2 (no shellout):
   - `AmendResult { oid: String, requires_force_push: bool }` — `requires_force_push` is set when the pre-amend HEAD was reachable from any `refs/remotes/*` ref.
   - Open repo, find HEAD commit.
   - If `include_staged`: read index, write tree from index. Else use `head.tree()`.
   - `head.amend(Some("HEAD"), Some(&signature), Some(&signature), None, message.as_deref(), Some(&new_tree))`.
   - Refuse if HEAD is unborn or detached.

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
   - **Published-commit guard**: before amending, check via libgit2 whether HEAD is reachable from any remote-tracking ref (`refs/remotes/*`). If yes, route through `showConfirmPopup` with copy: "This commit is already on the remote. Amending will require a force-push to share it. Continue?" Skip the prompt when HEAD is purely local. The check is duplicated server-side: `git_amend_commit`'s `AmendResult.requires_force_push` is the source of truth for the post-amend toast ("You'll need to force-push."), so a stale frontend prompt result can't desync the message.
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
   - **Strict block matching to avoid source-code false positives.** A line is treated as a marker only when it (a) starts at column 0, (b) consists of exactly seven `<`/`=`/`>`/`|` followed by either end-of-line or a single space + label (e.g. `<<<<<<< HEAD`), and (c) participates in a complete `<<<` → (`|||`)? → `===` → `>>>` sequence in the right order. An unmatched `<<<<<<<` (e.g. inside a string literal in source) is silently ignored — only fully-formed blocks are returned. This is also the correctness gate for the auto-stage-on-save flow below: "no markers remain" means "`parseConflictBlocks` returns `[]`", not "no line starts with `<<<`".
   - `replaceBlock(doc, block, choice: "ours"|"theirs"|"both"|string)` returning a CodeMirror change spec.

2. **CodeMirror extension** in same module:
   - StateField holds `Block[]` recomputed on doc change.
   - ViewPlugin produces `Decoration.mark` for ours/theirs/base ranges (background tint via CSS classes `.cm-conflict-ours`, `.cm-conflict-theirs`, `.cm-conflict-base`).
   - WidgetType for the marker lines themselves: replaces the `<<<<<<< HEAD` line (and its siblings) with a button bar `[Accept Ours] [Accept Theirs] [Accept Both]`. Buttons dispatch a transaction calling `replaceBlock`. The `[Open 3-Way]` button is hidden behind a `phase6Available` flag (default `false`) and only renders once Phase 6 lands — no dead UI in Phase 4.
   - Extension exported as `conflictExtension({ onResolveAll })` so callers can hook into "all conflicts cleared".

3. **Wire into `editor.js`**: `createEditor` accepts a new option `conflictMode: boolean`. When true, the conflict extension is appended to the extensions array. `main.js`'s `createEditorTab` opts in when the file is in `getGitStatus().conflictFiles`.

4. **Resolve flow**:
   - Each button mutates the doc; the file becomes dirty.
   - On Cmd+S, the existing save flow writes the file. After write, frontend calls `git_stage_file(path, file_path)` automatically when no markers remain in the saved text — this is git's standard "edited + add = resolved" pattern. Toast: "Resolved <file>".
   - "Open 3-Way" button is gated behind `phase6Available` and not rendered in Phase 4.

**Files:** `src-tauri/src/lib.rs` (no change), **new** `src/conflictmarkers.js`, `src/editor.js`, `src/main.js`, `src/styles.css` (new: `.cm-conflict-*` classes, `.conflict-action-bar`).

---

### Phase 5 — Interactive rebase with drag-to-reorder

Largest feature. Builds on Phase 1 (validation), Phase 3 (`get_pending_op_state`, abort/continue plumbing), and Phase 4 (conflict UI for rebase conflicts).

#### Backend

1. **`get_rebase_candidate_commits(path, count) -> Result<RebaseCandidates, String>`** — convenience wrapper around `get_commits` that excludes merge commits and stops at the first commit shared with the upstream (so users don't accidentally rewrite published history).
   - `RebaseCandidates { commits: Vec<CommitInfo>, upstream_known: bool }`. Each `CommitInfo` gains an `on_remote: bool` field (true when the commit is reachable from any `refs/remotes/*` ref) so the frontend's force-push confirm count is computed without a second round-trip.
   - **No upstream**: when the current branch has no upstream configured, the "shared-with-upstream" cutoff is undefined. In that case, cap at `count` commits and return `upstream_known: false`; the frontend renders an info banner ("No upstream branch — showing the last N commits. All are safe to rewrite.").
   - **Detached HEAD**: refuse with a typed error (`"detached_head"`); the rebase tab won't open.

2. **`git_rebase_interactive_apply(path, base_oid: String, todo: Vec<RebaseTodoEntry>, state) -> Result<RebaseResult, String>`** — the core of the feature:
   - `RebaseTodoEntry { action: "pick"|"reword"|"squash"|"fixup"|"drop"|"edit", oid: String, new_message: Option<String> }`.
   - Validate every oid + the base.
   - **Pre-rebase safety tag**: before spawning, create a lightweight tag `refs/tags/launchpad/pre-rebase/<timestamp>` pointing at the current HEAD. The frontend toast on success includes "Backup tag: `<name>` (delete with `git tag -d <name>`)"; on abort the cleanup also deletes the tag. This is cheaper than a reflog hint and discoverable for users who don't know reflog exists.
   - **Driver pattern**: write a single JSON state file to a per-run temp dir (`mktemp -d`-style). Layout:
     ```
     /tmp/launchpad-rebase-<pid>-<rand>/
       state.json              # { todo: [...], version: 1 }
       commit_editor.counter   # created lazily on first commit_editor.sh call
       seq_editor.sh           # 0o700
       commit_editor.sh        # 0o700
     ```
     `state.json` is immutable for the lifetime of the rebase; mutable progress lives in `commit_editor.counter` (atomic increment via `mv`-rename or `flock`), which is the only file the editor scripts write.
   - Both scripts receive the state-dir path via a single env var `LAUNCHPAD_REBASE_STATE_DIR` (set by the Rust spawner alongside `GIT_SEQUENCE_EDITOR` / `GIT_EDITOR`). They `cd $LAUNCHPAD_REBASE_STATE_DIR` and read `state.json` from a known relative path — no argument plumbing through git.
   - `seq_editor.sh` — invoked once with `$1` = git's rebase-todo path. Reads `state.json.todo`, rewrites `$1`: drops `drop` entries, keeps action keywords for `reword`/`squash`/`fixup`/`edit`, preserves order from the JSON (which is the user's drag-reordered list).
   - `commit_editor.sh` — invoked once per `reword`/`squash` step (NOT `edit` — git's `edit` action pauses without invoking the commit editor; the user resumes via `git rebase --continue` after our UI walks them through any amend). `fixup` also doesn't invoke the editor. Maintains a counter file `commit_editor.counter` in the state dir (created on first call, incremented atomically via `mv`-rename trick or `flock`). On invocation N, it walks `state.json.todo` to find the Nth entry whose action is `reword` or `squash`, writes that entry's `new_message` to `$1`, increments the counter. If `new_message` is null/missing for `reword`, it leaves `$1` untouched (preserves existing message).
   - **`edit` action handling**: when the rebase pauses on an `edit` entry, `get_pending_op_state` reports `kind: "rebase"` with `current_step` pointing at the edit row. The Pending Operation banner shows "Stopped to edit `<oid>` — make changes and click Continue." The user uses Launchpad's existing amend UI (Phase 3) to amend, then clicks Continue, which runs `git_rebase_continue`.
   - **Exotic `core.editor` wrappers**: `GIT_SEQUENCE_EDITOR` and `GIT_EDITOR` env vars take precedence over `core.editor` in git's resolution order, so user wrappers are bypassed. The spawn explicitly clears `EDITOR` and `VISUAL` too, so a malformed user `EDITOR=...` can't override our scripts. Documented as a known compatibility gotcha for users with `core.editor = some-wrapper.sh`.
   - Spawn `git rebase -i <base>` under `run_git_cancellable` with `GIT_SEQUENCE_EDITOR=<state_dir>/seq_editor.sh`, `GIT_EDITOR=<state_dir>/commit_editor.sh`, `LAUNCHPAD_REBASE_STATE_DIR=<state_dir>`, plus `apply_git_env`.
   - Return `RebaseResult { ok: bool, stopped_at: Option<String>, conflicted_files: Vec<String>, completed: bool, backup_tag: String }`.
   - On conflict: leave rebase state, return `ok: false` with conflict info; frontend routes to the inline conflict UI from Phase 4 + the Pending Operation banner. The state dir AND backup tag MUST survive the pause — `commit_editor.sh` will be re-invoked on `git rebase --continue`, and the user may want to bail to the backup tag manually.
   - **Cleanup lifecycle**: cleanup is gated on a *terminal* op result, not on `git_rebase_interactive_apply` returning. Terminal results are: clean completion of the initial apply, or `git_rebase_continue` returning `completed: true`, or `git_rebase_abort` returning success, or `git_rebase_skip` returning `completed: true`.
     - State dir: removed by a `Drop`-style guard wired to whichever command produces the terminal result. A mid-rebase `cancel_git_op` of the *initial* apply does NOT remove the state dir (the rebase may still be paused mid-operation — abort owns the cleanup).
     - Backup tag: kept on clean completion (user-visible safety net, surfaced in the success toast). Deleted on `git_rebase_abort`. Kept across all conflict pauses.

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
   - **Confirm prompt** before spawning when any candidate commit is reachable from a remote-tracking ref (same check as Phase 3's amend guard, applied per commit). Copy: "N of these commits are already on the remote. Rewriting them will require a force-push. Continue?" Skip the prompt entirely when the rebase is fully local. The check runs in the frontend off the `upstream_known` flag from `get_rebase_candidate_commits` plus a per-row `on_remote: bool` field added to that response.
   - Disables Apply, sets `inFlightOp`, calls `git_rebase_interactive_apply`.
   - On `ok: true` + `completed: true`: closes the rebase tab, refreshes git panel, toast "Rebase complete. Backup: `<backup_tag>`".
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

3. **Entry point** — flip `phase6Available` to `true` so Phase 4's `[Open 3-Way]` button renders, wired to open a `merge` tab for the conflicted file.

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
- Amend a commit that's already on `origin/<branch>` → confirm prompt fires; on accept the result toast mentions force-push.
- Amend a purely-local commit → no confirm prompt.
- Cherry-pick a clean commit → success toast; `git log` shows it.
- Cherry-pick a conflicting commit → conflicts banner + Pending Operation banner appears; "Abort" returns to clean state.

**Phase 4**
- Trigger a conflict via merge of a divergent branch; open conflicted file → `<<<<<<<`/`=======`/`>>>>>>>` lines replaced by the action bar; backgrounds tinted.
- Click "Accept Ours" → block collapses to ours text; Cmd+S → file written + auto-staged; conflicts section clears for that file.
- Open a clean file (no markers) → no decorations, no perf hit.
- **False-positive guard**: open a source file containing the literal string `"<<<<<<< HEAD"` inside a quoted string or comment (no matching `=======` / `>>>>>>>`) → no decorations, no auto-stage interference.
- Save a file where one of three conflict blocks is still unresolved → file written but NOT auto-staged; conflicts section still lists the file.

**Phase 5**
- Right-click a commit 4 back from HEAD → "Rebase from here…" → rebase tab opens with 3 candidate commits.
- Drag the middle row to the top, set bottom to `squash` with a new message, top to `drop`, apply.
- `git log` verifies new order, new squash message, dropped commit gone.
- Backup tag exists at the pre-rebase HEAD (`git tag -l 'launchpad/pre-rebase/*'`); `git reset --hard <tag>` recovers the branch.
- Apply a rebase that touches a published commit → confirm prompt fires; on cancel, no state-dir is left behind in `/tmp` and no backup tag is created.
- On a branch with no upstream → rebase tab opens with the info banner; apply succeeds without a confirm prompt.
- Detached HEAD → "Rebase from here…" surfaces an error toast and does not open the tab.
- Trigger a rebase conflict → flow lands in Phase 4 conflict editor → save + Continue completes; state dir is cleaned up at the end.
- Cancel mid-rebase → Pending Operation banner's Abort returns the branch to its pre-rebase state; backup tag is deleted.
- With `core.editor` set to a wrapper script that exits 1 → rebase still completes (env-var override path is exercised).

**Phase 6**
- Open 3-pane merge tab from a conflict → all three panes load, ours/theirs read-only, merged editable.
- Per-hunk Take Ours / Take Theirs / Take Both each apply correctly.
- Synchronized scrolling stays line-aligned through a 1000-line conflicted file.
