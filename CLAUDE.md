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
  gitpanel.js           # Full git panel UI (toolbar, staged/unstaged, commit, branches, history, GitHub, conflicts, cheatsheet)
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
- **Unified tab bar**: Terminal tabs, editor tabs, and settings tab share one tab bar. Each tab has a `type` field: `"terminal"`, `"editor"`, or `"settings"`. Tab-type guards protect terminal-specific code (fitAllPanes, PTY writes, split panes).
- **Split workspace (left/right groups)**: Cmd+\\ toggles a vertical workspace split. The right group tracks its own tab IDs in `rightGroupTabIds`. Each group has its own tab bar. Tabs can be dragged between groups or moved with Cmd+Shift+M. Focus switches with Cmd+Option+Left/Right. Closing the last tab in the right group auto-collapses the workspace.
- **In-tab pane split**: Cmd+D splits a single terminal tab into two vertical panes (50/50, draggable divider). Each pane gets its own PTY. This is independent from the workspace split.
- **Tab drag-to-move**: When workspace is split, tabs use a MutationObserver-based drag system to move between left and right groups. Group markers on tab objects track which group they belong to.

### File Browser
- **Root locked to project**: `setRoot()` guards against any path outside `projectRoot`. Nav-up ↑ is capped at the project root (no-op when already there). Go-home ⌂ jumps back to the project root.
- **No terminal disruption**: The file browser holds no concept of the terminal's cwd. Browsing subfolders never writes anything to a PTY — CLI agents (Claude, Aider, etc.) are safe from accidental `cd`.
- **CRUD operations**: Right-click context menu supports new file, new folder, rename, delete, and reveal in Finder. Uses `create_file`, `create_directory`, `rename_path`, `delete_path`, and `reveal_in_finder` commands.
- **Off-DOM tree building**: The file tree is built in a detached DOM fragment and swapped in a single operation to prevent flicker when expanding folders.
- **Live filesystem watcher**: Uses the `notify` crate (FSEvents on macOS) with 300ms debounce to watch the project root recursively. Started once in `enterWorkspace()`. Emits `fs-changed` Tauri events that trigger `refreshFileBrowser()` and git status updates. Frontend throttles to 500ms.

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

### Git
- **Git: libgit2 + system git**: Local operations (stage, unstage, stash, branch) use the `git2` crate. Network operations (push, pull, fetch, merge) shell out to system `git` via `std::process::Command` to respect the user's SSH keys and credential helpers. Network ops are cancellable via `cancel_git_op` which kills the spawned process by PID.
- **Git status dual entries**: `get_git_status` emits separate entries for staged (`index_new`, `index_modified`, `index_deleted`) and unstaged (`new`, `modified`, `deleted`) changes. A file can appear in both lists.

### Settings & State
- **Live settings**: Settings changes apply immediately to all open terminals/editors. Stored as JSON at `~/.launchpad/config.json`.
- **Projects**: Stored at `~/.launchpad/projects.json` as an array of `{ name, path, lastOpened }`. Written atomically via temp-file + rename. Path canonicalization dedupes equivalent paths. See `specs/project-model-spec.md`.
- **No framework**: Vanilla JS with direct DOM manipulation. Keeps the bundle small and fast.

### Per-Project Environment
- **Stored at `~/.launchpad/project-env.json`**, keyed by canonicalized project path, chmod `0o600` (owner read/write only) so other users on the box can't read stored secrets. Atomic write (temp + rename). Each entry is an array of `{ key, value, secret }`.
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

### Filesystem
- `read_directory(path)` — list directory contents (sorted: dirs first, then alpha)
- `search_files(root, query, max_results?)` — fuzzy file search (skips hidden dirs, node_modules, target, etc.)
- `read_file_preview(path, max_bytes?)` — read file content (default 8KB limit, binary detection)
- `write_file(path, content)` — write file to disk (10MB limit)
- `create_file(path)` — create new empty file (errors if exists)
- `create_directory(path)` — create directory (recursive)
- `delete_path(path)` — delete file or directory recursively
- `rename_path(old_path, new_path)` — rename/move file or directory
- `watch_directory(path)` — start recursive filesystem watcher, emits `fs-changed` events
- `unwatch_directory(path)` — stop watching a directory
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
- `git_push(path)` — push (auto-sets upstream if needed)
- `git_pull(path)` / `git_fetch(path)` / `git_merge_branch(path, branch_name)`
- `cancel_git_op()` — kill in-flight network git operation by PID
- `git_stash_save(path)` / `git_stash_pop(path)` / `git_stash_list(path)` / `git_stash_apply(path, index)` / `git_stash_drop(path, index)`
- `get_file_diff(path, file_path, staged?)` — structured file diff with hunks and line numbers
- `get_commit_details(path, oid)` — commit detail with changed files and line stats
- `git_resolve_ours(path, file_path)` / `git_resolve_theirs(path, file_path)` — resolve merge conflicts

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
- `refreshPanel(path, force)` — fetches status, branches, commits, remote URL in parallel
- `renderPanel()` — builds all HTML via string concatenation, then wires event handlers via `querySelectorAll`
- `showConfirmPopup()` — positioned popup for destructive actions (discard, delete branch)
- `showGitFeedback()` — temporary toast for success/error messages
- GitHub URL parsing handles both SSH and HTTPS remote formats
