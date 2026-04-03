# Launchpad - Development Guide

## Overview
Terminal-first desktop workspace built with Tauri v2 (Rust) + vanilla JS. No frameworks, no bloat.

## Tech Stack
- **Tauri v2** — native macOS desktop shell
- **Rust** — PTY management, filesystem ops, git operations via `git2` crate + system `git` for network ops (push/pull/merge)
- **Vanilla JS** — all frontend logic, no React/Vue/Svelte
- **xterm.js** — terminal emulator
- **CodeMirror 6** — code editor with syntax highlighting and search
- **Vite** — frontend bundler

## Project Structure
```
src/                    # Frontend (JS/CSS/HTML)
  main.js              # App entry, unified tab management (terminal + editor + settings), split panes, keyboard shortcuts
  filebrowser.js        # File tree, context menu, drag & drop
  editor.js             # CodeMirror factory — creates independent editor instances with search, cursor tracking
  git.js                # Git status polling, file status colors in tree
  gitpanel.js           # Full git panel UI (toolbar, staged/unstaged, commit, branches, history, GitHub, conflicts, cheatsheet)
  quickopen.js          # Cmd+P fuzzy file finder
  settings.js           # Persistent settings store (~/.launchpad/config.json)
  settingspanel.js      # Settings form UI (General, Terminal, Editor, Git sections)
  agentpanel.js         # AI agent chat panel
  providers.js          # AI provider management
  styles.css            # All styles (organized by section with comment headers)
src-tauri/              # Rust backend
  src/lib.rs            # All Tauri commands (PTY, filesystem, git, settings, agent)
  Cargo.toml            # Rust dependencies
  tauri.conf.json       # App configuration
index.html              # Main HTML shell
```

## Key Architecture Decisions
- **Unified tab bar**: Terminal tabs, editor tabs, and settings tab share one tab bar. Each tab has a `type` field: `"terminal"`, `"editor"`, or `"settings"`. Tab-type guards protect terminal-specific code (fitAllPanes, PTY writes, split panes).
- **Multi-PTY**: Each terminal tab/pane spawns its own PTY via `portable-pty`. PTY output is routed by ID through `paneMap`.
- **Editor factory pattern**: `editor.js` exports `createEditor()` which returns an `EditorView` instance. Callers own the lifecycle. No global singleton.
- **Git: libgit2 + system git**: Local operations (stage, unstage, stash, branch) use the `git2` crate. Network operations (push, pull, merge) shell out to system `git` via `std::process::Command` to respect the user's SSH keys and credential helpers.
- **Git status dual entries**: `get_git_status` emits separate entries for staged (`index_new`, `index_modified`, `index_deleted`) and unstaged (`new`, `modified`, `deleted`) changes. A file can appear in both lists.
- **Live settings**: Settings changes apply immediately to all open terminals/editors. Stored as JSON at `~/.launchpad/config.json`.
- **No framework**: Vanilla JS with direct DOM manipulation. Keeps the bundle small and fast.

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
```

## Keyboard Shortcuts
| Shortcut | Action |
|----------|--------|
| Cmd+T | New terminal tab |
| Cmd+W | Close tab |
| Cmd+K | Clear terminal |
| Cmd+D | Split/unsplit pane |
| Cmd+G | Toggle git panel |
| Cmd+P | Quick open file |
| Cmd+F | Find in editor / search file tree |
| Cmd+H | Find and replace in editor |
| Cmd+S | Save file in editor |
| Cmd+, | Open settings |
| Cmd+1-9 | Switch to tab N |
| Escape | Close diff preview |

## Adding a New Rust Command
1. Add the function with `#[tauri::command]` in `src-tauri/src/lib.rs`
2. Register it in the `invoke_handler` in `run()`
3. Call from JS with `await invoke("command_name", { args })`

## Adding a New Frontend Feature
- Terminal features → `main.js`
- File browser features → `filebrowser.js`
- Git features → `gitpanel.js`
- Editor features → `editor.js`
- Settings → `settingspanel.js` (UI) + `settings.js` (storage)
- Styles → `styles.css` (organized by section with comment headers)

## Git Panel Architecture
The git panel (`gitpanel.js`) rebuilds its innerHTML on every refresh but uses a snapshot comparison (`JSON.stringify`) to skip redundant re-renders during 3-second polling. Module-level state (`expandedCommitOid`) persists across re-renders.

Key patterns:
- `refreshPanel(path, force)` — fetches status, branches, commits, remote URL in parallel
- `renderPanel()` — builds all HTML via string concatenation, then wires event handlers via `querySelectorAll`
- `showConfirmPopup()` — positioned popup for destructive actions (discard, delete branch)
- `showGitFeedback()` — temporary toast for success/error messages
- GitHub URL parsing handles both SSH and HTTPS remote formats
