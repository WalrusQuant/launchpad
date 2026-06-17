# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# Launchpad - Development Guide

## Overview
Terminal-first desktop workspace built with Tauri v2 (Rust) + vanilla JS. No frameworks, no bloat.

## Tech Stack
- **Tauri v2** — native macOS desktop shell
- **Rust** — PTY management, filesystem ops, git operations via `git2` crate + system `git` for network ops (push/pull/fetch/merge)
- **Vanilla JS** — all frontend logic, no React/Vue/Svelte
- **xterm.js 6** — terminal emulator with WebGL renderer + Unicode 11 width tables
- **CodeMirror 6** — code editor with syntax highlighting, search, autocompletion, bracket matching, code folding
- **Vite** — frontend bundler

## Project Structure
```
src/                    # Frontend (JS/CSS/HTML)
  main.js              # App entry, project-gated workspace init, unified tab management (terminal + editor + settings), split workspace, split panes, drag-to-move tabs, keyboard shortcuts, context menus
  projects.js           # Active project state + thin wrappers around project Tauri commands
  projectpicker.js      # Project picker UI (welcome state / recent list) shown before any workspace exists
  filebrowser.js        # File tree rooted at project.path, context menu, drag & drop, git status colors, CRUD operations
  editor.js             # CodeMirror factory — creates independent editor instances with search, cursor tracking
  git.js                # Git status polling, file status colors in tree
  gitpanel.js           # Full git panel UI (toolbar, staged/unstaged, commit + amend, branches, history with cherry-pick / rebase-from-here / compare context menu, pending-op banner, conflicts, cheatsheet)
  diffrender.js         # Renders structured diffs (file list + hunks) for the diff tab and commit-detail view
  conflictmarkers.js    # Conflict-marker parser + CodeMirror extension (inline action bar, [Open 3-Way] hook)
  quickopen.js          # Cmd+P fuzzy file finder (uses search_files command, ranks by path length)
  settings.js           # Persistent settings store (~/.launchpad/config.json)
  settingspanel.js      # Settings form UI (General, Terminal, Editor, Git sections)
  styles.css            # All styles (organized by section with comment headers)
src-tauri/              # Rust backend
  src/lib.rs            # All Tauri commands (PTY, filesystem, git, settings, projects)
  Cargo.toml            # Rust dependencies
  tauri.conf.json       # App configuration
index.html              # Main HTML shell — picker-root + workspace-root sibling containers
specs/                  # Design specs (project model, agent model, etc.)
```

## Key Architecture Decisions

### Projects (see `specs/project-model-spec.md`)
- **One window = one project.** A project is just a root directory stored in `~/.launchpad/projects.json`. The workspace is gated behind a picker; `enterWorkspace(project)` initializes terminal/file-browser/git-panel only after a project is chosen.
- **Active project** is held in `projects.js` as module state (`activeProject`). It's the single source of truth for: terminal spawn cwd, file browser root, git panel path, Cmd+P search root, filesystem watcher.
- **All three PTY spawn sites** (`createTab`, `splitPane`, `createTabInRight`) pass `getActiveProject()?.path` directly — no inheritance from the previous tab's cwd, no `defaultDirectory` setting, no file-browser path.
- **Single-window by default, multi-window when asked.** Clicking a project in the picker takes over the *current* window — the picker view swaps for the workspace in-place. If the project is already open in another window, `focus_project_window` focuses that window and the current one stays on the picker. To genuinely open a second project in parallel, use **Cmd+Shift+N** (opens a new picker window) and pick from there. This matches VS Code / Xcode behavior — a window IS a project, not a project-spawner.
- **Window registration.** `enterWorkspace(project)` calls `register_project_window(path, current_window_label)`; the "← Projects" teardown calls `unregister_project_window(path)` before reloading. `project_windows: HashMap<canonical_path, label>` in `AppState` is the routing table, cleaned lazily when `get_webview_window` returns `None` (covers the closed-without-unregister case).
- **No auto-migration** from any legacy setting. An empty `~/.launchpad/projects.json` always lands the user on the picker's welcome state; projects only exist because the user explicitly opened a folder.

### Tab System & Split Workspace
- **Unified tab bar**: Terminal, editor, settings, **diff** (compare two refs), **rebase** (interactive rebase composer), and **merge** (3-pane merge tab) all share one tab bar. Each tab has a `type` field: `"terminal"`, `"editor"`, `"settings"`, `"diff"`, `"rebase"`, or `"merge"`. Tab-type guards protect terminal-specific code (fitAllPanes, PTY writes, split panes); editor-specific Cmd+S also handles `"merge"`. Cleanup in `doCloseTab` destroys CodeMirror views per type (single view for editor, three views for merge).
- **Split workspace (left/right groups)**: Cmd+\\ toggles a vertical workspace split. The right group tracks its own tab IDs in `rightGroupTabIds`. Each group has its own tab bar. Tabs can be dragged between groups or moved with Cmd+Shift+M. Focus switches with Cmd+Option+Left/Right. Closing the last tab in the right group auto-collapses the workspace.
- **In-tab pane split**: Cmd+D splits a single terminal tab into two vertical panes (50/50, draggable divider). Each pane gets its own PTY. This is independent from the workspace split.
- **Tab drag-to-move**: When workspace is split, tabs use a MutationObserver-based drag system to move between left and right groups. Group markers on tab objects track which group they belong to.

### File Browser
- **Root locked to project**: `setRoot()` guards against any path outside `projectRoot`. Nav-up ↑ is capped at the project root (no-op when already there). Go-home ⌂ jumps back to the project root.
- **No terminal disruption**: The file browser holds no concept of the terminal's cwd. Browsing subfolders never writes anything to a PTY — CLI agents (Claude, Aider, etc.) are safe from accidental `cd`.
- **CRUD operations**: Right-click context menu supports new file, new folder, rename, delete, and reveal in Finder. Uses `create_file`, `create_directory`, `rename_path`, `delete_path`, and `reveal_in_finder` commands. `delete_path` takes an optional `project_root` arg and refuses to delete outside it (cheap defense-in-depth on top of the UI-level root guard).
- **Rename propagation**: A rename via the context menu dispatches a `launchpad:path-renamed` CustomEvent that `main.js` listens for. Any open editor tab whose `filePath` matches the old path (or is a descendant of a renamed directory) gets its `filePath`, `fileName`, and breadcrumb updated in place — so Cmd+S doesn't silently write to the stale filename.
- **External rename via inode**: Unix `rename(2)` preserves inode, so `createEditorTab` captures `tab.inode` via `get_file_inode` at open time. If the fs-changed reload of a tab's file fails with "No such file", the frontend calls `find_path_by_inode(project_root, tab.inode)` — a bounded project-tree walk (same skip rules as `search_files`). On match, the tab is relocated AND its content reread (so git rename-and-edit doesn't leave the editor on stale text). On no match, the tab is flagged `stale` (strikethrough label + one-time toast).
- **Off-DOM tree building**: The file tree is built in a detached DOM fragment and swapped in a single operation to prevent flicker when expanding folders.
- **Refresh coalescing**: `refreshFileBrowser` tracks a `refreshDirty` flag — calls that arrive while a load is in-flight set the flag instead of being dropped, and the load re-runs once when it finishes. Bursty fs-changed events (e.g., from `git pull`) can't leave the tree stale.
- **Entry tolerance**: `read_directory` falls through to sensible defaults when `metadata()` fails (permission denied, TOCTOU delete), so the listing isn't silently truncated; non-UTF-8 filenames surface with replacement chars via `to_string_lossy`.
- **Live filesystem watcher**: Uses the `notify` crate (FSEvents on macOS) with 300ms debounce to watch the project root recursively. Started once in `enterWorkspace()`. `watch_directory` canonicalizes its path, emits `fs-changed` events against that canonical form, and returns it — the frontend stores the returned canonical path as `watchedProjectPath` and uses it for event-path comparison, so a project opened via a symlink alias (e.g. macOS `/var` → `/private/var`) doesn't drop every event.

### Terminal
- **Multi-PTY with deferred reader**: Each terminal tab/pane spawns its own PTY via `portable-pty`. The reader thread is NOT started during `spawn_pty` — instead, the frontend calls `start_pty_reader` AFTER registering the pane in `paneMap`, preventing a race where early output gets dropped.
- **Backpressure flow control**: `pause_pty_reader` / `resume_pty_reader` let the frontend signal when xterm.js write queue exceeds its high-water mark. The reader thread sleeps until cleared; data stays in the OS PTY buffer.
- **WebGL rendering**: xterm.js uses WebGL renderer by default with canvas fallback.
- **Unicode 11**: Uses `@xterm/addon-unicode11` for correct CJK/emoji width measurement, avoiding ghost character bugs from width table mismatches.
- **Context menu**: Right-click on terminal shows Copy, Paste, Clear options.

### Editor
- **Editor factory pattern**: `editor.js` exports `createEditor()` which returns an `EditorView` instance. Callers own the lifecycle. No global singleton.
- **Features**: Bracket matching, close brackets, fold gutter, indent on input, autocompletion, lint gutter, highlight selection matches, rectangular selection, crosshair cursor.
- **Supported languages**: JS, TS, JSX, TSX, Python, Rust, HTML, CSS, JSON, Markdown, SCSS, TOML, YAML, Shell.
- **Context menu**: Right-click in editor shows Cut, Copy, Paste, Select All with disabled states based on selection/clipboard.
- **External-change handling**: The `fs-changed` listener walks open editor tabs and calls `read_file_preview` on each. Unchanged → skip; changed + not-dirty → silent reload; changed + dirty → confirm dialog. Missing → first try inode relocation (see File Browser), then flag the tab `stale` with a toast if unresolved. Errors are caught per-tab so one bad file doesn't abort the whole pass.
- **Tab shape**: editor tabs carry `{ type: "editor", containerEl, filePath, fileName, inode, editorView, originalContent, modified, stale, conflictMode }`. `stale` adds a `.tab-stale` CSS class (red strikethrough) so the visual marker persists after the one-time toast fades.
- **File-open error surfacing**: `createEditorTab` catches `read_file_preview` errors (binary, UTF-16, permission, missing) and shows the Rust error string via `showToast`. Previously this was a silent `return null` — user clicked, nothing happened, no feedback.
- **Conflict mode**: when `getGitFileStatus(filePath) === "conflict"` at open time, `createEditor` is given `conflictMode: true` and an `onOpenThreeWay` callback. The `conflictExtension` from `conflictmarkers.js` parses `<<<<<<< / ||||||| / ======= / >>>>>>>` blocks (strict — column 0, exactly 7 chars, complete sequence; bails on nested), draws an inline action bar with **Accept Ours / Theirs / Both** buttons (and **Open 3-Way** when the host wires `onOpenThreeWay`), and tints the ours/base/theirs ranges. `replaceBlock` preserves the file's existing trailing-newline policy. On save, when `parseConflictBlocks(content).length === 0`, the file is auto-staged and the panel refreshes — the gate is the parser, NOT a substring scan, so test fixtures containing literal marker glyphs are safe.
- **`readOnly` option**: pass `readOnly: true` to `createEditor` to append `EditorState.readOnly.of(true)` (used by the side panes of the 3-pane merge tab).
- **Live-reconfigurable options via Compartments**: `setTabSize`, `setWordWrap`, `setFontSize`, `setVisualExtras`, and `showBlame`/`hideBlame` on the editor handle each reconfigure a dedicated `Compartment` so settings apply to open editors without reopening (mirrors how `termFontSize` updates terminals live). `editorFontSize` is driven this way — the theme no longer hardcodes the size.
- **Change gutter (git-aware)** (`changegutter.js`): a per-line bar showing working-tree-vs-HEAD diff (added/modified/deleted), backed by the `get_file_diff_vs_head` command (`diff_tree_to_workdir_with_index`, so a staged hunk still shows). `deriveLineChanges(fileDiff)` is the pure, Vitest-covered classifier (gitgutter-style removed/added pairing). Recomputed on open / save / external reload / tab focus — markers reflect disk-vs-HEAD, not per-keystroke. `Alt+J`/`Alt+K` jump hunks. Clicking a marker opens a popover: **Revert hunk** (replaces the hunk's lines with HEAD content in the buffer — correctness rests on the buffer matching disk, so it's gated when dirty) or **Stage file**. The tab keeps the last `FileDiff` on `tab.lineDiff` for the click handler.
- **Blame gutter (opt-in)** (`blamegutter.js`): the status-bar **Blame** toggle reveals a left margin with short OID + age per commit-hunk (first line of each hunk only), backed by `git_blame_file` (libgit2 `blame_file`). Hover shows author + summary; clicking opens the commit's diff (vs its parent). Toggled via a compartment while the blame `StateField` stays live.
- **Symbol outline** (`symbols.js`, Cmd+Shift+O): a fuzzy palette of the file's symbols derived from the Lezer syntax tree (no LSP). `extractSymbols(tree, docText)` is pure + Vitest-covered (JS funcs/classes/methods, Python defs/classes, Rust items, Markdown headings). Palette UI lives in `main.js`, reusing the quick-open row styles.
- **Indent guides**: `@replit/codemirror-indentation-markers`, behind the `editorIndentGuides` setting (default on), wired through the `setVisualExtras` compartment.
- **Format on save** (opt-in, `editorFormatOnSave`): the `format_file` Rust command runs a PATH-resolved formatter (prettier / rustfmt / black / gofmt / shfmt) in place with the project as cwd and returns the formatted content; `saveEditorTab` syncs the buffer to it (cursor preserved). Best-effort — a missing formatter or error toasts without failing the save.
- **Reveal active file in tree** (opt-in, `editorFollowActiveFile`): `revealPath` (in `filebrowser.js`) expands ancestor folders via `expandedDirs`, selects and scrolls the row into view; `switchTab` calls it for editor tabs.
- **Language server diagnostics (opt-in, `editorLanguageServer`, default off)**: `lsp.rs` is a process host modeled on `pty.rs` — it spawns external language servers (typescript-language-server for JS/TS today; rust-analyzer / pyright mapped for later), a reader thread parses `Content-Length`-framed messages and emits them as `lsp-message` events, `lsp_send` frames outgoing messages to stdin. One server per `{language}:{project_path}`. The pure framing fns (`encode_lsp_message` / `read_lsp_message`) are unit-tested, and an `#[ignore]`d e2e test drives a real server through `initialize`/`didOpen` and asserts diagnostics. Frontend `lspclient.js` runs `@codemirror/lsp-client` over a Tauri-IPC `Transport`, wired into the editor via an `lspCompartment`. Uses `languageServerExtensions()` (the full bundle: diagnostics, completion, hover, signature help, and go-to-def / rename / format / find-references keymaps). `shutdownAllLsp` stops servers on project teardown so the reload doesn't orphan them.

### Git
- **Git: libgit2 + system git**: Local operations (stage, unstage, stash, branch) use the `git2` crate. Network operations (push, pull, fetch, merge) shell out to system `git` via `std::process::Command` to respect the user's SSH keys and credential helpers. Network ops are cancellable via `cancel_git_op`.
- **Spawned git env hardening** (`apply_git_env` in `lib.rs`): every spawned `git` Command sets `GIT_TERMINAL_PROMPT=0` and `GIT_ASKPASS=echo` so HTTPS remotes without a working credential helper fail fast with an auth error instead of blocking forever on a stdin pipe that has no TTY. `SSH_AUTH_SOCK` is forwarded via `resolve_ssh_auth_sock()`, which falls back to `launchctl getenv SSH_AUTH_SOCK` on macOS when the `.app` was launched from Finder/Dock (GUI-launched bundles don't inherit it from the login shell). Negative lookups are cached for only 60s so late-registered agents recover without an app restart.
- **Cancel semantics** (`GitOpSlot` in `AppState`): the in-flight slot is `{ op_id, pid: Option<u32>, cancelled: bool }`, **reserved before `spawn()`** so a cancel arriving in the window between spawn and PID-record still sets `cancelled=true`. `spawn_git_under_slot` observes the flag after spawn and kills the just-created child. Kill uses `kill -9` (SIGKILL) because a git process deep in a stalled TCP `recv()` often ignores SIGTERM — the exact scenario Cancel is for. `cancel_git_op` sets the flag and kills if a PID is recorded; it does NOT clear the slot (`run_git_cancellable`'s cleanup owns that and gates on op_id match).
- **Shared reservation across retry**: `git_push` holds ONE slot across the initial push AND the optional `--set-upstream` retry, so a cancel arriving between the two attempts still applies. Upstream presence is determined via libgit2 (`git_upstream_status`) before the spawn — stderr-substring matching was locale-fragile.
- **Merge is cancellable**: `git_merge_branch` routes through `run_git_cancellable`. The JS call site sets `inFlightOp` around the invoke so the 3s git-panel poll can't re-render the toolbar mid-merge.
- **Git status dual entries**: `get_git_status` emits separate entries for staged (`index_new`, `index_modified`, `index_deleted`) and unstaged (`new`, `modified`, `deleted`) changes. A file can appear in both lists.
- **Status tree coloring** (`git.js`): `applyGitColors` anchors on the project root (`currentGitRoot`) — strips it from each `.file-entry`'s absolute path and does an exact match against the git-relative status map, with a fallback for ancestor-directory rollup. Previously used `endsWith` which misattributed status between same-named files in sibling trees.
- **Discard handles untracked**: `git_discard_file` detects `wt_new` and deletes from disk (including the `GIT_ENOTFOUND` fallback for files inside untracked directories, where libgit2's `status_file` can't see the child). `checkout_head` alone would silently no-op.
- **Pending-op state** (`get_pending_op_state`): polled alongside `get_git_status` (one round-trip per cycle, kept on the `currentPendingOp` snapshot in both `git.js` and `gitpanel.js`). Detects merge / cherry-pick / rebase pause via `.git/MERGE_HEAD`, `CHERRY_PICK_HEAD`, and `rebase-merge|rebase-apply` directories. Drives the **Pending Operation banner** in the git panel (Continue / Skip / Abort buttons + step counter for rebase). Cherry-pick is gated on `kind === "none"` so you can't stack a cherry-pick on top of a half-merged tree.
- **Compare / diff tab** (`createDiffTab` in `main.js`, backed by `get_diff_between_refs`): two-pane view with file list + per-file unified diff. Reachable from the COMMITS section context menu (Compare with HEAD / Compare with parent / Compare with…) and from the file browser. Validators `is_valid_git_ref` (allows `[A-Za-z0-9._/-^~]`, rejects `..`) and `is_valid_git_oid` (4-40 hex) gate the input. The "Compare with…" popup is positioned in JS with viewport clamping; the panel suppresses the macOS native context menu so right-click hits the JS handler.
- **Amend & cherry-pick** (`git_amend_commit`, `git_cherry_pick`/`_continue`/`_abort`): amend has **two entry points** in the panel — `Amend (with staged)` reuses the index, `Amend message only` keeps the existing tree. Both check `head_is_on_any_remote` first (libgit2 `graph_descendant_of` scan over `refs/remotes/*`) and prompt before rewriting a published commit. Cherry-pick conflicts route through the Pending Operation banner.
- **Interactive rebase** (`git_rebase_interactive_apply`): driver-script approach. A temp dir created via `tempfile::Builder` (TOCTOU-safe — O_EXCL + secure suffix; chmod `0o700`) holds two scripts (`seq_editor.sh`, `commit_editor.sh`) and a `state.json` (chmod `0o600`) keyed by `LAUNCHPAD_REBASE_STATE_DIR`. The scripts read JSON and emit the desired todo / commit message, decoupling the user's $EDITOR from the rebase. A backup tag (`rebase-backup-<branch>-<timestamp>`) is created before rebase and survives success; abort drops it. `RebaseStateInfo` lives on `AppState` so cleanup is single-source and Continue/Skip/Abort can release it. `revparse_single` resolves rev expressions (`HEAD^`, `~2`) to concrete OIDs server-side, so the frontend can pass `${oid}^` directly without 7 frontend validators.
- **Rebase tab UI** (`createRebaseTab` in `main.js`): list-of-commits composer with mouse-based drag-to-reorder (HTML5 DnD is unreliable in WebKit/Tauri), per-row action selector (`pick / reword / squash / fixup / drop / edit`), `[Apply]` flow that confirms when commits are already on the remote (force-push warning). Conflict pause closes the rebase tab and opens the first conflicted file via `createEditorTab` (which auto-detects conflictMode); Continue/Skip/Abort live on the Pending Operation banner.
- **3-pane merge tab** (`createMergeTab` / `saveMergeTab`): backed by `get_conflict_versions(path, file_path)` reading libgit2 index stages 1 (base), 2 (ours), 3 (theirs) plus the working tree. Three editors — ours/theirs read-only via `EditorState.readOnly`, merged editable + `conflictMode`. Save writes the merged pane and auto-stages once `parseConflictBlocks(content).length === 0`; on stage failure `tab.modified` stays set so Cmd+S retries. Synchronized scroll: line-aligned merged → ours/theirs (clamped to each pane's line count). The scroll listener is pinned on the tab and detached in `doCloseTab` before `view.destroy()` to avoid view-retention leaks (CodeMirror doesn't track external listeners). Reachable from the inline conflict editor's `[Open 3-Way]` button.

### Settings & State
- **Live settings**: Settings changes apply immediately to all open terminals/editors. Stored as JSON at `~/.launchpad/config.json`.
- **Projects**: Stored at `~/.launchpad/projects.json` as an array of `{ name, path, lastOpened }`. Path canonicalization dedupes equivalent paths. See `specs/project-model-spec.md`.
- **Atomic writes**: `atomic_write(dest, data)` in `lib.rs` is the shared helper for every user-facing write (user code via `write_file`, settings via `save_settings`/`save_file_settings`, projects via `write_projects_file`, project-env via `write_project_env_file`). Writes to `.<name>.lp-tmp-<pid>-<counter>` (PID + `AtomicU64` counter to avoid collisions between concurrent same-dest writes from multiple windows in the same process), `fsync`, preserves existing mode, then `rename`. A crash / kill / ENOSPC mid-write leaves the old file intact. `atomic_write_with_mode(dest, data, Some(mode))` is used for secrets files so the chmod happens on the temp BEFORE rename (no 0o644 window on first-write for `project-env.json`).
- **No framework**: Vanilla JS with direct DOM manipulation. Keeps the bundle small and fast.

### Per-Project Environment
- **Stored at `~/.launchpad/project-env.json`**, keyed by canonicalized project path, chmod `0o600` (owner read/write only) so other users on the box can't read stored secrets. Writes go through `atomic_write_with_mode(path, body, Some(0o600))` so the 0o600 is applied to the temp file BEFORE rename — no window where the file exists on disk at the default umask 0o644. Each entry is an array of `{ key, value, secret }`.
- **Injection site**: `spawn_pty` in `lib.rs` applies these vars AFTER the parent-env inherit loop and BEFORE the `TERM` / `COLORTERM` / `TERM_PROGRAM` / `LANG` overrides. That order lets a project override `PATH` or `ANTHROPIC_API_KEY`, but prevents a stray `TERM=garbage` entry from breaking terminal capability detection.
- **Scope**: every PTY spawned for the active project (tab, split pane, right-group tab) gets the project's env. Existing terminals don't retroactively update — OS-level reality, surfaced in the UI copy as "Applies to new terminals."
- **Lookup**: `load_env_for_project(path)` canonicalizes `path` the same way the projects file does, so two different strings for the same dir dedupe cleanly.
- **Cleanup**: `remove_project` best-effort-calls `forget_project_env` so deleted projects don't leave orphaned secrets behind. Re-adding the project starts with an empty env set.
- **No keychain**: v1 stores values in the local JSON with `0o600`. Fine for single-user macOS. Revisit if a shared-machine or cross-device-sync story appears.

## Commands

### Development
```bash
npm install                    # Install JS dependencies
npx tauri dev                  # Run dev server with hot reload
```

### Build
```bash
npx tauri build                # Build .app bundle
# Output: src-tauri/target/release/bundle/macos/Launchpad.app
```

### Rust only
```bash
cargo check --manifest-path src-tauri/Cargo.toml    # Type check
cargo build --manifest-path src-tauri/Cargo.toml     # Build debug
cargo test --manifest-path src-tauri/Cargo.toml      # Run Rust tests
```

### Frontend tests
```bash
npm test                       # Run Vitest once (CI mode)
npm run test:watch             # Watch mode
```
Vitest (jsdom env) covers the pure frontend logic. Tests live next to the
module as `<name>.test.js` (e.g. `src/conflictmarkers.test.js`). Config in
`vitest.config.js`, kept separate from `vite.config.js` so the Tauri dev-server
config doesn't bleed into the test run. Good targets are the framework-free pure
functions — diff/HTML builders, the conflict-marker parser, path matching —
rather than DOM-wiring code.

## Keyboard Shortcuts
| Shortcut | Action |
|----------|--------|
| Cmd+T | New terminal tab |
| Cmd+W | Close tab |
| Cmd+K | Clear terminal |
| Cmd+D | Split/unsplit pane (within tab) |
| Cmd+\\ | Split/unsplit workspace (left/right groups) |
| Cmd+Option+Left/Right | Switch focused group |
| Cmd+Shift+M | Move tab to other group |
| Cmd+G | Toggle git panel |
| Cmd+P | Quick open file |
| Cmd+Shift+O | Go to symbol in current file |
| Alt+J / Alt+K | Jump to next / previous changed hunk (editor) |
| Cmd+F | Find in editor / search file tree |
| Cmd+H | Find and replace in editor |
| Cmd+S | Save file in editor |
| Cmd+, | Open settings |
| Cmd+1-9 | Switch to tab N |
| Escape | Close diff preview / dialog |

## Tauri Commands (Rust → JS IPC)

### PTY
- `spawn_pty(cwd?, project_path?, rows?, cols?)` — spawn a new PTY, returns `{ tab_id }`. Does NOT start reading output yet. If `project_path` is given, env vars stored for that project in `~/.launchpad/project-env.json` are injected into the child.
- `start_pty_reader(tab_id)` — start the output reader thread (call AFTER registering pane in `paneMap`)
- `write_to_pty(tab_id, data)` — write input to a PTY
- `resize_pty(tab_id, rows, cols)` — resize a PTY (skips redundant resizes)
- `pause_pty_reader(tab_id)` / `resume_pty_reader(tab_id)` — backpressure flow control
- `close_pty(tab_id)` — close a PTY process
- `write_debug_log(content)` — write debug capture to `~/.launchpad/debug.log`

### Language Server (LSP)
- `lsp_start(project_path, language) -> server_id` — spawn (or reuse) a language server for the project+language; idempotent. Returns `"{language}:{project_path}"`. Errors if no server is configured or the binary isn't on PATH (frontend degrades silently).
- `lsp_send(server_id, message)` — frame a JSON-RPC message with `Content-Length` and write to the server's stdin.
- `lsp_stop(server_id)` — kill the server and drop its slot.

### Filesystem
- `read_directory(path)` — list directory contents (sorted: dirs first, then alpha). Tolerant: entries whose `metadata()` fails fall through to defaults instead of being dropped.
- `search_files(root, query, max_results?)` — fuzzy file search (skips hidden dirs, node_modules, target, etc.). Siblings sorted before recursing so walk order is deterministic.
- `search_in_files(root, query, case_sensitive, is_regex, max_results?)` — project-wide content search with the same skip rules.
- `read_file_preview(path, max_bytes?)` — streams up to `max_bytes` via `File::take` (no full-file slurp). Binary heuristic: rejects files with a UTF-16 BOM with an actionable message; otherwise rejects when null bytes exceed 1% of the prefix (with a 64-byte minimum so tiny files with a single null aren't wrongly flagged).
- `write_file(path, content)` — write file to disk (10MB limit). Uses `atomic_write`.
- `create_file(path)` — create new empty file (errors if exists)
- `create_directory(path)` — create directory (recursive)
- `delete_path(path, project_root?)` — delete file or directory recursively. When `project_root` is supplied, refuses deletions outside it (canonicalize + `starts_with` check). The context-menu caller always passes it.
- `rename_path(old_path, new_path)` — rename/move file or directory
- `get_file_inode(path) -> u64` — inode number of the file at `path`. Captured at editor-open time so external renames can be followed (Unix rename preserves inode).
- `find_path_by_inode(root, inode) -> Option<String>` — walks the project tree (same skip rules as `search_files`, depth limit 12) looking for a file with the given inode. Returns the first match. Used to relocate editor tabs after Finder/mv/git renames.
- `watch_directory(path) -> String` — canonicalizes `path`, starts recursive filesystem watcher on the canonical form, and **returns the canonical path**. Frontend stores this for fs-changed event comparison (so symlink aliases don't drop events). Emits `fs-changed` events against the canonical path.
- `unwatch_directory(path)` — stop watching a directory. Tries both raw and canonical forms.
- `pick_directory()` — native macOS folder picker via osascript
- `reveal_in_finder(path)` — reveal file/folder in Finder
- `get_home_dir()` — returns user's home directory path

### Git
- `get_git_status(path)` — staged + unstaged file status, branch, ahead/behind counts
- `list_branches(path)` — local branches with upstream info
- `list_remote_branches(path)` — remote branches (strips `origin/` prefix)
- `get_commits(path, count?)` — commit history (default 20)
- `get_remote_url(path)` — remote origin URL
- `checkout_branch(path, branch_name)` / `create_branch(path, branch_name, checkout?)` / `git_delete_branch(path, branch_name)`
- `git_stage_file(path, file_path)` / `git_unstage_file(path, file_path)` / `git_stage_all(path)` / `git_unstage_all(path)`
- `git_discard_file(path, file_path)` — discard unstaged changes
- `git_commit(path, message)` — create commit, returns short OID
- `git_push(path)` — upstream presence determined via libgit2 before spawn; no-upstream branches use `push --set-upstream origin <branch>` under the same cancellable slot as the initial push.
- `git_pull(path)` / `git_fetch(path)` / `git_merge_branch(path, branch_name)` — all routed through `run_git_cancellable`. Merge includes option-injection validation on `branch_name`.
- `cancel_git_op()` — sets `cancelled=true` on the current `GitOpSlot` and `kill -9`s the recorded PID (if any). If called during the spawn window (PID not yet recorded), the flag is observed by `spawn_git_under_slot` after spawn and the just-created child is killed.
- `git_stash_save(path)` / `git_stash_pop(path)` / `git_stash_list(path)` / `git_stash_apply(path, index)` / `git_stash_drop(path, index)`
- `get_file_diff(path, file_path, staged?)` — structured file diff with hunks and line numbers
- `get_commit_details(path, oid)` — commit detail with changed files and line stats
- `get_diff_between_refs(path, from_ref, to_ref)` — multi-file structured diff between two refs (rev expressions allowed: `HEAD`, `<oid>`, `<oid>^`, `branch~2`). Powers the compare/diff tab.
- `git_resolve_ours(path, file_path)` / `git_resolve_theirs(path, file_path)` — resolve merge conflicts
- `git_amend_commit(path, message, include_staged)` — amend HEAD; `include_staged: true` reuses the working tree, `false` keeps the existing tree.
- `git_head_on_remote(path)` — true when HEAD is reachable from any remote ref. Used to gate amend / rebase prompts.
- `git_cherry_pick(path, oid)` — pick a commit onto HEAD; on conflict returns `{ ok: false, conflicted_files: [...] }` so the frontend can route to the conflict editor.
- `git_cherry_pick_continue(path)` / `git_cherry_pick_abort(path)` — pending-op resolution.
- `get_pending_op_state(path)` — `{ kind: "none" | "merge" | "cherry_pick" | "rebase", current_step?, total_steps?, head_message? }`. Single source of truth for the Pending Operation banner.
- `get_rebase_candidate_commits(path, count)` — commits between HEAD and upstream (or up to `count`), each flagged `on_remote` so the rebase tab can warn about force-push consequences.
- `git_rebase_interactive_apply(path, base_oid, todo)` — runs the rebase under driver scripts. `base_oid` accepts a rev expression. Returns `RebaseResult { ok, completed, conflicted_files, backup_tag, stopped_at }`.
- `git_rebase_continue(path)` / `git_rebase_skip(path)` / `git_rebase_abort(path)` — drive the rebase forward (or unwind it). Continue/Skip return the same `RebaseResult` shape so the frontend can re-route to the next conflicted file.
- `get_conflict_versions(path, file_path)` — `{ base, ours, theirs, merged }` from libgit2 index stages 1/2/3 + working tree. Powers the 3-pane merge tab.

### Settings
- `load_settings()` / `save_settings(data)` — JSON validated before write

### Projects
- `load_projects()` — returns `Vec<Project>` sorted by `lastOpened` desc; empty array if file missing
- `add_project(path, name?, last_opened)` — upserts by canonicalized path; derives name from directory if null; returns the Project
- `remove_project(path)` — removes by canonicalized path
- `rename_project(path, new_name)` — renames the entry matching the canonicalized path
- `touch_project(path, last_opened)` — updates `lastOpened` on the matching entry
- `focus_project_window(path) -> bool` — if a window is registered for the canonicalized path AND still alive, shows + focuses it and returns `true`; otherwise cleans any stale entry and returns `false`
- `register_project_window(path, label)` — called from `enterWorkspace` to claim the current window for a project; any prior entry for the same label is removed
- `unregister_project_window(path)` — called from "← Projects" teardown before reloading so a stale registration doesn't fool `focus_project_window`
- `open_new_window(path?)` — creates a new Tauri window. With `path`: URL `?folder=<path>`, title = folder name. Without: URL `/`, title "Launchpad", boots into picker. Registration now happens on the frontend side (in `enterWorkspace`), so this command is the same for both cases.

### Project Environment
- `load_project_env_vars(path)` — returns `Vec<{ key, value, secret }>` stored for the canonicalized `path`, or `[]` if none.
- `save_project_env_vars(path, vars)` — validates each key against POSIX rules (`[A-Za-z_][A-Za-z0-9_]*`), writes `~/.launchpad/project-env.json` atomically with `0o600`. Empty `vars` removes the entry.
- `forget_project_env(path)` — drops the entry for `path`. Called automatically from `remove_project`; exposed as a command for completeness. Missing / unparseable file succeeds silently.

## Adding a New Rust Command
1. Add the function with `#[tauri::command]` in `src-tauri/src/lib.rs`
2. Register it in the `invoke_handler` in `run()`
3. Call from JS with `await invoke("command_name", { args })`

## Adding a New Frontend Feature
- Terminal features → `main.js`
- Split workspace logic → `main.js` (search for `rightGroup` or `splitWorkspace`)
- File browser features → `filebrowser.js`
- Git features → `gitpanel.js`
- Editor features → `editor.js`
- Settings → `settingspanel.js` (UI) + `settings.js` (storage)
- Project picker / project data → `projectpicker.js` (UI) + `projects.js` (state)
- Styles → `styles.css` (organized by section with comment headers)

**Project-scoped features rule**: when wiring a new feature that cares about "the current directory", use `getActiveProject().path` (or a closure over it) — **not** `getCurrentPath()` from the file browser. `getCurrentPath()` tracks sub-folder navigation; only the file browser itself should read it.

## Git Panel Architecture
The git panel (`gitpanel.js`) rebuilds its innerHTML on every refresh but uses a snapshot comparison (`JSON.stringify`) to skip redundant re-renders during 3-second polling. Module-level state (`expandedCommitOid`) persists across re-renders.

Key patterns:
- `refreshPanel(path, force, preloadedStatus?)` — fetches branches, commits, remote URL, stashes in parallel. Accepts a preloaded `status` so the poll path fetches `get_git_status` once (in `git.js:fetchGitStatus`) instead of twice per cycle. On project switch (new `path`), clears `commitDetailCache` and `expandedCommitOid`.
- `renderPanel()` — builds all HTML via string concatenation, then wires event handlers via `querySelectorAll`
- `inFlightOp` guard: `refreshPanel` early-returns on the polled path when a network op is running, so toolbar re-renders can't destroy the active button's click-handler and remove the Cancel button. The forced refresh at op-end clears the flag first so it still re-renders.
- `invokeWithTimeout(command, args, timeoutMs)` — 30s default. On timeout, calls `cancel_git_op` so the orphaned git child is killed and the PID slot is freed (Tauri IPC has no cancellation, so without this the Rust side keeps running after `Promise.race` rejects).
- `commitDetailCache` — OIDs are immutable, so detail responses cache indefinitely. Expanded commits render inline from cache on re-render (no double-fetch, no loading flicker). Cleared on project switch.
- `showConfirmPopup()` — positioned popup for destructive actions (discard, delete branch)
- `showGitFeedback()` — temporary toast inside the git panel for success/error messages (panel-scoped)
- GitHub URL parsing accepts alias hosts of the form `git@github.com-<suffix>:...` (common `~/.ssh/config` multi-account setup). Anchored on the literal `github.com` to reject spoofed hosts like `git@not-github.com` and `git@github.com.evil`.

## Toast Notifications
`main.js` exports `showToast(message, type)` for app-level errors and info events not scoped to a specific panel (file-open failures, external-rename notifications, deleted-on-disk warnings). Lazy-creates a fixed-position container at bottom-right (`z-index: 1100` so it sits above modal overlays at 1000), auto-dismisses after 4s. Types are `"error"` and `"info"`. The git panel keeps its own `showGitFeedback` for panel-scoped messages.
