# Launchpad - Development Guide

## Overview
Terminal-first desktop workspace built with Tauri v2 (Rust) + vanilla JS. No frameworks, no bloat.

## Tech Stack
- **Tauri v2** — native macOS desktop shell
- **Rust** — PTY management, filesystem ops, git operations via `git2` crate
- **Vanilla JS** — all frontend logic, no React/Vue/Svelte
- **xterm.js** — terminal emulator
- **CodeMirror 6** — code editor with syntax highlighting
- **Vite** — frontend bundler

## Project Structure
```
src/                    # Frontend (JS/CSS/HTML)
  main.js              # App entry, tab management, split panes, keyboard shortcuts
  filebrowser.js        # File tree, context menu, drag & drop, file preview/edit
  editor.js             # CodeMirror wrapper with syntax highlighting + minimap
  git.js                # Git status polling, file status colors
  gitpanel.js           # Git panel UI (branches, commits, stage, commit, diff)
  quickopen.js          # Cmd+P fuzzy file finder
  settings.js           # Persistent settings (sidebar width, last directory)
  styles.css            # All styles
src-tauri/              # Rust backend
  src/lib.rs            # All Tauri commands (PTY, filesystem, git, settings)
  Cargo.toml            # Rust dependencies
  tauri.conf.json       # App configuration
index.html              # Main HTML shell
```

## Key Architecture Decisions
- **Multi-PTY**: Each terminal tab/pane spawns its own PTY via `portable-pty`. PTY output is routed by ID.
- **Split panes**: A tab contains 1-2 panes. Each pane has its own PTY. `paneMap` routes output globally.
- **Git via libgit2**: All git operations use the `git2` Rust crate — no shelling out to `git` CLI.
- **No framework**: Vanilla JS with direct DOM manipulation. Keeps the bundle small and fast.
- **Settings**: JSON file at `~/.launchpad/config.json`.

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
| Cmd+F | Search file tree |
| Cmd+S | Save file in editor |
| Cmd+1-9 | Switch to tab N |
| Escape | Close editor/preview |

## Adding a New Rust Command
1. Add the function with `#[tauri::command]` in `src-tauri/src/lib.rs`
2. Register it in the `invoke_handler` in `run()`
3. Call from JS with `await invoke("command_name", { args })`

## Adding a New Frontend Feature
- Terminal features → `main.js`
- File browser features → `filebrowser.js`
- Git features → `gitpanel.js`
- Editor features → `editor.js`
- Styles → `styles.css` (organized by section with comment headers)
