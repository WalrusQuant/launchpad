# Implementation Rollout Plan: Three Git-Shaped Gaps

## Context

`specs/git-features-plan.md` defines six phases that fill three gaps in Launchpad's git surface (history rewriting, conflict resolution UX, arbitrary-ref diff). The spec is design-complete and internally consistent. What it doesn't answer is *how to ship it* — phase-by-phase as one PR each, or split/grouped to minimize blast radius.

This plan translates the six phases into seven PR-sized chunks, sequenced by file-impact and dependency DAG, so each chunk is independently reviewable, testable, and bisectable. Per direction:

- **Phase 5 splits into 3 sub-PRs** (backend / UI / integration) because it's the largest feature.
- **Phase 6 is planned inline** rather than deferred.
- **After PR1, Phases 3 and 4 develop in parallel** because they touch disjoint files.

## Verified Anchors (from exploration)

The spec's line refs all check out as of HEAD:

| Anchor | Verified location | Status |
|---|---|---|
| `is_valid_merge_branch_name` | `lib.rs:1812-1821` | exists, body matches; rename target |
| `GitOpSlot` / `apply_git_env` / `spawn_git_under_slot` / `run_git_cancellable` | `lib.rs:1530-1700` | exists; new shellouts plug into `run_git_cancellable` |
| `FileDiff` / `HunkDiff` / `DiffLine` / `collect_structured_diff` | `lib.rs:1377-1450` | exists; reusable as-is |
| `get_git_status` returns conflict files | `lib.rs:373-411` | conflicts already in `files: Vec<GitFileStatus>` with `status: "conflict"` |
| `invoke_handler` registration block | `lib.rs:2362-2432` | every new command adds one line here |
| `showDiff` DOM-build loop | `gitpanel.js:1026-1039` | exact pattern to extract into `diffrender.js` |
| `inFlightOp` + `invokeWithTimeout` | `gitpanel.js:13-30, 43, 114` | move to `git.js` |
| `showConfirmPopup` | `gitpanel.js:223-262` | reusable for new confirms |
| Polling cycle | `git.js:67-78` | insertion point for `get_pending_op_state` |
| Tab-type guards | `main.js:487, 493, 553, 991, 1012` | each new tab type adds branches here |
| Drag-to-move plumbing | `main.js:1616-1741` | template for rebase-row drag-to-reorder |
| `createEditor` factory | `editor.js:154-211` | `conflictMode` and `readOnly` options plug in here |
| `git_amend_commit` etc. | absent | all six phases' new commands are net-new |

## Rollout Overview

```
PR1: Phase 1 + Phase 2  (foundations + diff tab)
       │
       ├──► PR2: Phase 3  (amend + cherry-pick + pending-op)   ─┐
       │                                                         │
       └──► PR3: Phase 4  (inline conflict editor)              ─┤
                                                                  │
                            PR2 + PR3 both land  ◄────────────────┘
                                    │
                                    ▼
       PR4 (5a): Rebase backend     (driver scripts, commands)
                                    │
                                    ▼
       PR5 (5b): Rebase tab UI      (drag-to-reorder, apply flow)
                                    │
                                    ▼
       PR6 (5c): Rebase conflict integration  (E2E with Phase 4)
                                    │
                                    ▼
       PR7: Phase 6  (3-pane merge tab; flips phase6Available)
```

Total: 7 PRs. PR2 and PR3 develop in parallel.

## Per-PR Scope

### PR1 — Phase 1 + Phase 2 (foundations + diff tab)

**Why bundled:** Phase 1 has zero user-visible value on its own (pure refactor). Phase 2 is the smallest feature and the natural first consumer of the Phase 1 extractions. Shipping them together avoids a "refactor-only" PR that's hard to justify reviewing in isolation.

**Backend (`src-tauri/src/lib.rs`):**
- Rename `is_valid_merge_branch_name` → `is_valid_git_ref` (same body; preserves call site at `lib.rs:1829`).
- Add `is_valid_git_oid(s: &str) -> bool` (4–40 hex chars).
- Add command `get_diff_between_refs(path, from_ref, to_ref) -> Result<RefDiff, String>` using `Repository::revparse_single` + `peel_to_tree` + `repo.diff_tree_to_tree` + `collect_structured_diff` (extended to loop over deltas instead of grabbing only the first).
- Define `RefDiff { from_ref, to_ref, files: Vec<FileDiff>, stats: { files_changed, additions, deletions } }`.
- Register both new commands in the `invoke_handler` block at `lib.rs:2362-2432`.
- **Reuse:** `apply_git_env`, `run_git_cancellable` not needed here (libgit2-only, no shellout). `FileDiff`/`HunkDiff`/`DiffLine` reused unchanged.

**Frontend:**
- **NEW** `src/diffrender.js` — extract the loop from `gitpanel.js:1026-1039` into `buildDiffHtml(hunks)` and add `buildFileDiffSection(fileDiff)` for the multi-file wrapper. CSS classes (`.diff-add`, `.diff-del`, `.diff-hunk-header`, `.diff-line`, `.diff-gutter`, `.diff-content`) preserved.
- `src/git.js` — add exports `inFlightOp` (mutable flag), `setInFlightOp(bool)`, and `invokeWithTimeout` moved verbatim from `gitpanel.js:13-30`.
- `src/gitpanel.js` —
  - Update `showDiff` (`gitpanel.js:1022-1045`) to call `buildDiffHtml` from the new module.
  - Replace local `inFlightOp` and `invokeWithTimeout` with imports from `git.js`. Update the read at `gitpanel.js:114` and the writes at `gitpanel.js:1001, 1006, 1013`.
  - Add commit-row context menu on `.gp-commit` right-click (handler attached near the existing expansion handler at `gitpanel.js:907`): "Compare with HEAD", "Compare with parent" (`oid^`), "Copy OID", "Compare with…" (ref prompt).
  - Add "Compare…" button at the branches section header (around the section render in `renderPanel`); opens a two-`<select>` popup styled via `showConfirmPopup`.
- `src/main.js` —
  - New tab type `"diff"`. Tab shape: `{ type: "diff", containerEl, fromRef, toRef, refDiff, selectedFileIndex }`. Dedupe key: `diff::${fromRef}..${toRef}` (Map key only — DOM ids derived from refs run through `encodeURIComponent`/short-hash).
  - Add tab-type guards everywhere `tab.type === "editor"` is checked (`main.js:487, 493, 553, 991, 1012`). For diff tabs: skip `editorView.destroy()` in `doCloseTab`.
  - New `createDiffTab({ fromRef, toRef })` modeled on `createEditorTab` (`main.js:592-775`).
  - Layout: two-column flex inside `tab.containerEl`. Left = sticky file list + `+N -N` stats; right = scrollable `buildFileDiffSection` output.
- `src/styles.css` — new section: `.diff-tab`, `.diff-file-list`, `.diff-tab-content`, `.diff-file-list-item`.

**Verification:**
- `cargo test --manifest-path src-tauri/Cargo.toml` — new unit tests for `is_valid_git_ref` (reject empty, leading `-`, whitespace, `..`) and `is_valid_git_oid` (accept 4-char and 40-char hex; reject 3-char, 41-char, non-hex).
- `cargo test` — `get_diff_between_refs` against a fixture repo using the existing `LAUNCHPAD_HOME` test isolation hook.
- `npx tauri dev` — open two branches, click "Compare…" → diff tab opens; click a file → right pane scrolls; close tab → state cleared.
- Right-click an old commit → "Compare with HEAD" → multi-file diff renders matching `git diff <oid>..HEAD` from the terminal.
- Existing merge end-to-end run (regression check after the validator rename).

---

### PR2 — Phase 3 (amend + cherry-pick + pending-op state)

**Why on its own:** Touches `gitpanel.js` and `lib.rs` only; doesn't intersect with PR3's `editor.js`/`conflictmarkers.js` work. Pending-op infrastructure introduced here is reused by PR4–PR6.

**Depends on:** PR1 (validators, shared `inFlightOp`).

**Backend (`src-tauri/src/lib.rs`):**
- `git_amend_commit(path, message: Option<String>, include_staged: bool) -> Result<AmendResult, String>` — pure libgit2.
  - `AmendResult { oid: String, requires_force_push: bool }`.
  - `requires_force_push` = HEAD reachable from any `refs/remotes/*` ref before the amend.
  - Refuse if HEAD is unborn or detached.
- `git_cherry_pick(path, oid) -> Result<CherryPickResult, String>` via `run_git_cancellable`. Validate oid with `is_valid_git_oid`. On nonzero exit, check `.git/CHERRY_PICK_HEAD` + read conflict files via `get_git_status` and return `CherryPickResult { ok, conflicted_files }`.
- `git_cherry_pick_abort(path)` / `git_cherry_pick_continue(path)` — shellouts via `run_git_cancellable`.
- `get_pending_op_state(path) -> PendingOpState` — libgit2. Detects merge / cherry-pick / rebase via state-file presence. Returns `{ kind, current_step?, total_steps?, head_message? }`.
- Register all in `invoke_handler`.
- **Reuse:** `is_valid_git_oid` (PR1), `apply_git_env` + `run_git_cancellable` for shellouts.

**Frontend:**
- `src/git.js` — extend `startGitPolling` (`git.js:67-78`) to also call `get_pending_op_state` each cycle, alongside `fetchGitStatus`. Pass both into `refreshPanel` so the panel renders without a second invoke. Export `currentPendingOp`.
- `src/gitpanel.js` —
  - **Amend button** in commit form (`gitpanel.js:444-470`): visible only when HEAD is not unborn AND `currentPendingOp.kind === "none"`. Two modes (message-only vs with-staged). Pre-amend: if HEAD is reachable from a remote-tracking ref, fire `showConfirmPopup` with the force-push warning. Server-side `requires_force_push` is the source-of-truth for the post-amend toast.
  - **Cherry-pick** entry in commit-row context menu (the menu added in PR1). On conflict result: toast + the existing conflicts banner + a new Pending Operation banner.
  - **Pending Operation banner**: rendered above the conflicts section in `renderPanel` whenever `currentPendingOp.kind !== "none"`. Per kind: merge → Abort/Continue; cherry-pick → Abort/Continue; rebase → Abort/Continue/Skip. (Rebase variant exercised in PR6.)
- `src/styles.css` — new: `.gp-pending-op-banner`, `.gp-amend-button`.

**Verification:**
- Stage a file, edit message, click Amend → HEAD message + tree updated; `git log -1` confirms.
- Amend a commit on `origin/<branch>` → confirm prompt fires; toast mentions force-push.
- Amend a purely-local commit → no prompt.
- Cherry-pick a clean commit → success toast.
- Cherry-pick a conflicting commit → conflicts banner + Pending Operation banner; "Abort" returns to clean state.

---

### PR3 — Phase 4 (inline conflict editor)

**Why parallelizable with PR2:** Touches `editor.js` and a new module; only `main.js` overlap is the auto-stage hook on save, which doesn't conflict with PR2's `gitpanel.js` work.

**Depends on:** PR1 (none of Phase 1's specific exports needed; depends only on PR1 having merged for codebase-state purposes).

**Backend:** none.

**Frontend:**
- **NEW** `src/conflictmarkers.js` —
  - `parseConflictBlocks(text) -> Block[]`. Strict matching: marker line must (a) start at column 0, (b) be exactly seven `<`/`=`/`>`/`|` followed by EOL or single-space + label, (c) participate in a complete `<<<` → (`|||`)? → `===` → `>>>` sequence. Unmatched markers (e.g. inside string literals) are ignored. Handles 2-way and diff3.
  - `replaceBlock(doc, block, choice)` returns a CodeMirror change spec.
  - CodeMirror extension: StateField holds `Block[]`, recomputed on doc change. ViewPlugin produces `Decoration.mark` for ours/theirs/base ranges. WidgetType replaces marker lines with `[Accept Ours] [Accept Theirs] [Accept Both]`. (`[Open 3-Way]` gated behind `phase6Available` flag; not rendered in this PR.)
  - Export `conflictExtension({ onResolveAll, phase6Available })`.
- `src/editor.js` — `createEditor` (`editor.js:154-211`) accepts new option `conflictMode: boolean`. When true, append `conflictExtension({ phase6Available: false })` to the extensions array.
- `src/main.js` —
  - `createEditorTab` opts in to `conflictMode` when the file is in `getGitStatus().files` with `status === "conflict"`.
  - In the existing save flow (Cmd+S), after `write_file` succeeds: if `parseConflictBlocks(savedText).length === 0`, call `git_stage_file(projectPath, filePath)` and toast "Resolved <file>". (Auto-stage gate is **`parseConflictBlocks` returning `[]`**, not a substring check.)
- `src/styles.css` — new: `.cm-conflict-ours`, `.cm-conflict-theirs`, `.cm-conflict-base`, `.conflict-action-bar`.

**Verification:**
- Trigger merge conflict; open conflicted file → marker lines replaced with action bar; backgrounds tinted.
- Click "Accept Ours" → block collapses; Cmd+S → file written + auto-staged; conflicts section clears.
- Open clean file (no markers) → no decorations, no perf hit.
- **False-positive guard:** open a source file containing `"<<<<<<< HEAD"` literal in a string/comment with no matching `=======`/`>>>>>>>` → no decorations, no auto-stage interference.
- Save a file with one of three blocks still unresolved → file written, NOT auto-staged.

---

### PR4 (5a) — Rebase backend + driver scripts

**Why on its own:** Pure Rust + shell scripts. No UI. Reviewable as a self-contained backend feature with `cargo test` coverage. Lets the UI in PR5 develop against a stable contract.

**Depends on:** PR1 (validators), PR2 (`get_pending_op_state`, abort/continue plumbing pattern).

**Backend (`src-tauri/src/lib.rs`):**
- `get_rebase_candidate_commits(path, count) -> Result<RebaseCandidates, String>`.
  - `RebaseCandidates { commits: Vec<CommitInfo>, upstream_known: bool }`. `CommitInfo` gains `on_remote: bool`.
  - No upstream: cap at `count`, `upstream_known: false`.
  - Detached HEAD: typed error `"detached_head"`.
- `git_rebase_interactive_apply(path, base_oid, todo: Vec<RebaseTodoEntry>) -> Result<RebaseResult, String>`.
  - `RebaseTodoEntry { action: "pick"|"reword"|"squash"|"fixup"|"drop"|"edit", oid, new_message: Option<String> }`.
  - Validate every oid + base via `is_valid_git_oid`.
  - Pre-rebase: create lightweight tag `refs/tags/launchpad/pre-rebase/<timestamp>` at HEAD.
  - Driver: `mktemp -d /tmp/launchpad-rebase-<pid>-<rand>/`, write `state.json` (immutable: `{ todo, version: 1 }`), generate `seq_editor.sh` and `commit_editor.sh` at `0o700`.
    - Both scripts read `LAUNCHPAD_REBASE_STATE_DIR`; `state.json` is the only fixed read path.
    - `commit_editor.sh` maintains `commit_editor.counter` (atomic via `mv`-rename); fires only for `reword`/`squash`. **`edit` action does NOT invoke commit_editor** — git pauses for shell and our UI surfaces it via the Pending Op banner.
  - Spawn `git rebase -i <base>` under `run_git_cancellable` with `GIT_SEQUENCE_EDITOR=<state_dir>/seq_editor.sh`, `GIT_EDITOR=<state_dir>/commit_editor.sh`, `LAUNCHPAD_REBASE_STATE_DIR=<state_dir>`. Clear `EDITOR` and `VISUAL` to neutralize user wrappers.
  - `RebaseResult { ok, stopped_at, conflicted_files, completed, backup_tag }`.
  - **Cleanup lifecycle gated on terminal op result, NOT on initial-apply return:**
    - Terminal results = clean initial completion, `git_rebase_continue` returning `completed: true`, `git_rebase_abort` success, or `git_rebase_skip` returning `completed: true`.
    - State dir survives all conflict pauses; removed only by the terminal op's Drop guard.
    - Backup tag: kept on clean completion, deleted on abort, kept across all conflict pauses.
- `git_rebase_abort(path)` / `git_rebase_continue(path)` / `git_rebase_skip(path)` — shellouts via `run_git_cancellable`. Each owns the cleanup Drop guard for the state dir + backup tag if it produces a terminal result.
- Register all in `invoke_handler`.
- **Reuse:** `apply_git_env`, `run_git_cancellable`, `is_valid_git_oid`, `is_valid_git_ref`.

**Frontend:** none in this PR. (Phase 3's Pending Operation banner already covers the rebase kind generically; UI work is PR5.)

**Verification:**
- `cargo test` — fixture repo: drag-reorder simulated via direct `git_rebase_interactive_apply` call with a hand-crafted `Vec<RebaseTodoEntry>`. Verify `git log` order, squash message, dropped commit gone.
- `cargo test` — rebase that hits a conflict: verify `RebaseResult.ok == false`, state dir still exists, backup tag still exists.
- Run the rebase from the test, then call `git_rebase_abort` → verify state dir gone, backup tag gone.
- `cargo test` — `get_rebase_candidate_commits`: verify `upstream_known: false` path, detached-HEAD error.
- Manual: with `core.editor` set to a wrapper script that exits 1, run an end-to-end rebase via raw IPC → still completes (env-var override exercised).

---

### PR5 (5b) — Rebase tab UI + drag-to-reorder

**Why on its own:** Largest frontend chunk in the project. Develops against the stable PR4 backend without touching conflict-resolution flow yet (that's PR6).

**Depends on:** PR4 (backend), PR1 (foundations).

**Frontend:**
- `src/main.js` —
  - New tab type `"rebase"`. Shape: `{ type: "rebase", containerEl, baseOid, todo: TodoEntry[], originalTodo: TodoEntry[] }`. Dedupe: `rebase::${baseOid}` (Map-key only).
  - `createRebaseTab({ baseOid })`: calls `get_rebase_candidate_commits`, populates `todo`, renders.
  - Layout: vertical list of rows. Each row = drag handle, action `<select>` (pick/reword/squash/fixup/drop/edit), short OID + subject, inline message editor (revealed on reword/squash select). Footer: `[Apply]` `[Reset]` `[Cancel]`.
  - **Drag-to-reorder:** clone the pattern from `main.js:1616-1741` (`startTabDrag`/`getTabInsertIndex`/`reorderTabs`) into `startRowDrag`/`getRowInsertIndex`/`reorderRows`. Simpler than tabs (one array, no group-marker tracking).
  - Add tab-type guards at `main.js:487, 493, 553, 991, 1012`. No `editorView.destroy()` for rebase tabs.
  - Apply flow:
    - If any row has `on_remote: true`, fire `showConfirmPopup` with "N of these commits are already on the remote. Rewriting them will require a force-push. Continue?" (Skip the prompt entirely when `upstream_known: false` OR no row is on remote.)
    - Disable Apply, set `inFlightOp = true`, call `git_rebase_interactive_apply`.
    - On `ok && completed`: close tab, refresh panel, toast "Rebase complete. Backup: `<backup_tag>`".
    - On `!ok`: leave the tab closed/closeable, refresh panel — actual conflict routing is wired in PR6.
- `src/gitpanel.js` —
  - "Rebase from here…" entry in commit-row context menu (`gitpanel.js:907` handler from PR1) — opens a rebase tab with `base_oid = clicked_oid^`.
  - "Rebase onto upstream…" button in branches section, visible when `behind > 0`.
- `src/styles.css` — new: `.rebase-tab`, `.rebase-row`, `.rebase-handle`, `.rebase-action-select`, `.rebase-row-message-editor`.

**Verification:**
- Right-click a commit 4 back from HEAD → "Rebase from here…" → rebase tab opens with 3 candidate commits.
- Drag middle row to top, set bottom to `squash` with new message, top to `drop`, apply → `git log` verifies new order, new squash message, dropped commit gone.
- Backup tag exists at pre-rebase HEAD (`git tag -l 'launchpad/pre-rebase/*'`); `git reset --hard <tag>` recovers branch.
- Rebase touching a published commit → confirm prompt; cancel → no state dir in `/tmp`, no backup tag.
- No-upstream branch → tab opens with info banner; apply succeeds without prompt.
- Detached HEAD → "Rebase from here…" surfaces error toast, tab does not open.

---

### PR6 (5c) — Rebase conflict integration

**Why on its own:** End-to-end glue between PR3 (conflict editor), PR2 (Pending Op banner), and PR5 (rebase tab). Small surface but high coordination — easier to review as a dedicated PR than as a tail commit on PR5.

**Depends on:** PR3, PR4, PR5.

**Frontend:**
- `src/main.js` — extend the rebase apply flow's `!ok` branch:
  - On conflicts: close rebase tab, switch focus to first conflicted file, open it via `createEditorTab` (which already opts into `conflictMode` from PR3).
  - After conflict resolution (file saved + auto-staged via PR3): the user clicks "Continue" in the Pending Operation banner → calls `git_rebase_continue`.
  - On `git_rebase_continue` returning more conflicts: repeat — open next conflicted file.
  - On `git_rebase_continue` returning `completed: true`: success toast with backup tag, banner clears.
- `src/gitpanel.js` — extend the Pending Operation banner (from PR2) for rebase: progress text from `currentPendingOp.current_step / total_steps`. Wire Continue/Abort/Skip to `git_rebase_continue` / `git_rebase_abort` / `git_rebase_skip`.
- `src/git.js` — no change (poll already covers this in PR2).

**Verification:**
- Trigger a rebase conflict → flow lands in Phase 4 conflict editor → save + Continue → completes; state dir cleaned up; backup tag survives.
- Cancel mid-rebase → Pending Operation banner's Abort returns branch to pre-rebase state; backup tag deleted.
- Multi-conflict rebase: resolve first, Continue, resolve second, Continue → completes.

---

### PR7 — Phase 6 (3-pane merge tab)

**Why last:** Spec explicitly defers it until Phase 4 has soaked. Once shipped, flips `phase6Available` so the `[Open 3-Way]` button materializes in the conflict editor's action bar.

**Depends on:** PR3 (`conflictExtension({ phase6Available })`), PR2 (Pending Op state for merge kind).

**Backend (`src-tauri/src/lib.rs`):**
- `get_conflict_versions(path, file_path) -> ConflictVersions { ours, theirs, base, merged }` reading libgit2 index entries at stages 1 (base), 2 (ours), 3 (theirs); `merged` from working tree.
- Register in `invoke_handler`.

**Frontend:**
- `src/editor.js` — add `readOnly: boolean` option to `createEditor` (`editor.js:154-211`). When true, append `EditorState.readOnly.of(true)`.
- `src/main.js` —
  - New tab type `"merge"`. Shape: `{ type: "merge", containerEl, filePath, fileName, oursView, theirsView, mergedView, hunks }`. Dedupe: `merge::${filePath}`.
  - `createMergeTab({ filePath })`: calls `get_conflict_versions`, instantiates three editors via `createEditor` (ours/theirs `readOnly: true`, merged editable). Renders three columns + per-hunk gutter buttons `[Take Ours]`/`[Take Theirs]`/`[Take Both]`.
  - Synchronized scrolling: anchor on merged-pane scrollTop → line; map to ours/theirs lines via parsed hunk structure.
  - Save flow: write merged pane to disk + auto-stage (same pattern as PR3).
  - Tab-type guards at `main.js:487, 493, 553, 991, 1012`. Cleanup: destroy all three editor views in `doCloseTab`.
- `src/conflictmarkers.js` (from PR3) — flip `phase6Available` default to `true` in callers, OR pass it explicitly from `createEditor` based on a settings flag. The action-bar widget renders `[Open 3-Way]` button which calls `createMergeTab(filePath)`.
- `src/styles.css` — new: `.merge-tab`, `.merge-pane`, `.merge-hunk-actions`.

**Verification:**
- `cargo test` — `get_conflict_versions` against a fixture repo with index entries at all three stages.
- Open 3-pane merge tab from a conflict → all three panes load, ours/theirs read-only, merged editable.
- Per-hunk Take Ours / Take Theirs / Take Both each apply correctly.
- Synchronized scrolling stays line-aligned through a 1000-line conflicted file.
- Save merged pane → disk + auto-stage; conflicts section clears.

---

## Cross-Cutting Verification (run after each PR lands)

- `cargo check --manifest-path src-tauri/Cargo.toml` — type check.
- `cargo test --manifest-path src-tauri/Cargo.toml` — full Rust suite (uses `LAUNCHPAD_HOME` test isolation).
- `npx tauri dev` — manual smoke through the existing happy path: open a project, run a commit, push, pull. Catches regressions in unchanged code paths after a PR's churn.

## Critical Files Touched (per PR)

| File | PR1 | PR2 | PR3 | PR4 | PR5 | PR6 | PR7 |
|---|:-:|:-:|:-:|:-:|:-:|:-:|:-:|
| `src-tauri/src/lib.rs` | ● | ● | | ● | | | ● |
| `src/main.js` | ● | | ● | | ● | ● | ● |
| `src/gitpanel.js` | ● | ● | | | ● | ● | |
| `src/git.js` | ● | ● | | | | | |
| `src/editor.js` | | | ● | | | | ● |
| `src/styles.css` | ● | ● | ● | | ● | | ● |
| `src/diffrender.js` (NEW) | ● | | | | | | |
| `src/conflictmarkers.js` (NEW) | | | ● | | | ● | ● |

## Out-of-Scope for This Plan

- Updates to `specs/git-features-plan.md` itself — design plan is frozen as of `claude/review-git-panel-docs-Mfsx4`.
- Settings UI for any of the new features (no new settings introduced).
- Telemetry / analytics.
- Windows/Linux portability (POSIX-only assumptions baked into Phase 5's driver scripts; spec calls this out).
