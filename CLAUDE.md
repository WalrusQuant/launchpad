# Launchpad - Development Guide

## Overview
Terminal-first desktop workspace built with Tauri v2 (Rust) + vanilla JS. No frameworks, no bloat.

## Tech Stack
- **Tauri v2** — native macOS desktop shell
- **Rust** — PTY management, filesystem ops, git operations via `git2` crate + system `git` for network ops (push/pull/merge), AI agent streaming via `reqwest`
- **Vanilla JS** — all frontend logic, no React/Vue/Svelte
- **xterm.js** — terminal emulator with WebGL renderer
- **CodeMirror 6** — code editor with syntax highlighting and search
- **Vite** — frontend bundler

## Project Structure
```
src/                    # Frontend (JS/CSS/HTML)
  main.js              # App entry, unified tab management (terminal + editor + settings), split workspace (left/right groups), split panes, drag-to-move tabs, keyboard shortcuts
  filebrowser.js        # File tree, context menu, drag & drop, git status colors
  editor.js             # CodeMirror factory — creates independent editor instances with search, cursor tracking
  git.js                # Git status polling, file status colors in tree
  gitpanel.js           # Full git panel UI (toolbar, staged/unstaged, commit, branches, history, GitHub, conflicts, cheatsheet)
  quickopen.js          # Cmd+P fuzzy file finder
  settings.js           # Persistent settings store (~/.launchpad/config.json)
  settingspanel.js      # Settings form UI (General, Terminal, Editor, Git sections)
  agentpanel.js         # AI agent chat panel with tool use, streaming, provider/model selection
  providers.js          # AI provider presets and management (Anthropic, OpenAI, Gemini, Grok, OpenRouter, custom)
  styles.css            # All styles (organized by section with comment headers)
src-tauri/              # Rust backend
  src/lib.rs            # All Tauri commands (PTY, filesystem, git, settings, agent streaming)
  Cargo.toml            # Rust dependencies
  tauri.conf.json       # App configuration
index.html              # Main HTML shell (includes toolbar with settings, shortcuts, agent, git buttons)
```

## Key Architecture Decisions

### Tab System & Split Workspace
- **Unified tab bar**: Terminal tabs, editor tabs, and settings tab share one tab bar. Each tab has a `type` field: `"terminal"`, `"editor"`, or `"settings"`. Tab-type guards protect terminal-specific code (fitAllPanes, PTY writes, split panes).
- **Split workspace (left/right groups)**: Cmd+\\ toggles a vertical workspace split. The right group tracks its own tab IDs in `rightGroupTabIds`. Each group has its own tab bar. Tabs can be dragged between groups or moved with Cmd+Shift+M. Focus switches with Cmd+Option+Left/Right. Closing the last tab in the right group auto-collapses the workspace.
- **In-tab pane split**: Cmd+D splits a single terminal tab into two vertical panes (50/50, draggable divider). Each pane gets its own PTY. This is independent from the workspace split.
- **Tab drag-to-move**: When workspace is split, tabs use a MutationObserver-based drag system to move between left and right groups. Group markers on tab objects track which group they belong to.

### Terminal
- **Multi-PTY**: Each terminal tab/pane spawns its own PTY via `portable-pty`. PTY output is routed by ID through `paneMap`.
- **WebGL rendering**: xterm.js uses WebGL renderer by default with canvas fallback.

### Editor
- **Editor factory pattern**: `editor.js` exports `createEditor()` which returns an `EditorView` instance. Callers own the lifecycle. No global singleton.
- **Supported languages**: JS, TS, JSX, TSX, Python, Rust, HTML, CSS, JSON, Markdown, SCSS, TOML, YAML, Shell.

### Git
- **Git: libgit2 + system git**: Local operations (stage, unstage, stash, branch) use the `git2` crate. Network operations (push, pull, merge) shell out to system `git` via `std::process::Command` to respect the user's SSH keys and credential helpers.
- **Git status dual entries**: `get_git_status` emits separate entries for staged (`index_new`, `index_modified`, `index_deleted`) and unstaged (`new`, `modified`, `deleted`) changes. A file can appear in both lists.

### AI Agent
- **Multi-provider streaming**: `agent_chat_stream` in Rust handles SSE streaming for both OpenAI and Anthropic wire formats. Emits `agent-chunk` events with types: `text`, `tool_call`, `done`, `error`.
- **Tool system**: Agent has 5 tools — `read_file`, `write_file`, `search_files`, `list_directory`, `run_command`. Tool calls are executed frontend-side and results sent back in the conversation.
- **Provider presets**: `providers.js` defines defaults for Anthropic, OpenAI, Gemini, Grok, OpenRouter, and custom OpenAI-compatible endpoints. Each provider has a model list, active model, base URL, and API key.

### Settings & State
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
| Cmd+D | Split/unsplit pane (within tab) |
| Cmd+\\ | Split/unsplit workspace (left/right groups) |
| Cmd+Option+Left/Right | Switch focused group |
| Cmd+Shift+M | Move tab to other group |
| Cmd+I | Toggle AI agent panel |
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
- `spawn_pty(id, rows, cols, cwd)` — spawn a new PTY process
- `write_to_pty(id, data)` — write input to a PTY
- `resize_pty(id, rows, cols)` — resize a PTY
- `close_pty(id)` — close a PTY process

### Filesystem
- `read_directory(path, show_hidden)` — list directory contents
- `search_files(path, query)` — fuzzy file search
- `read_file_preview(path)` — read file content (512KB limit)
- `write_file(path, content)` — write file to disk

### Git
- `get_git_status(path)` — staged + unstaged file status
- `list_branches(path)` — local and remote branches
- `get_commits(path, count)` — commit history
- `get_remote_url(path)` — remote origin URL
- `checkout_branch(path, name)` / `create_branch(path, name)` / `delete_branch(path, name)`
- `stage_file(path, file)` / `unstage_file(path, file)` / `stage_all(path)` / `unstage_all(path)`
- `discard_file(path, file)` — discard unstaged changes
- `commit(path, message)` — create commit
- `git_push(path)` / `git_pull(path)` / `git_merge(path, branch)`
- `stash_save(path)` / `stash_pop(path)`
- `get_diff(path, file, staged)` — file diff
- `get_commit_details(path, oid)` — commit detail with changed files
- `resolve_conflict(path, file, resolution)` — resolve merge conflict (ours/theirs)

### Settings
- `load_settings()` / `save_settings(settings)`

### Agent
- `agent_chat_stream(messages, provider, model, base_url, api_key, provider_type, tools)` — stream AI response with tool support

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
- AI agent features → `agentpanel.js`
- AI providers → `providers.js`
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

## Agent Panel Architecture
The agent panel (`agentpanel.js`) manages a conversation with tool-calling loop:

1. User sends message → `agent_chat_stream` Tauri command starts SSE stream
2. Rust backend emits `agent-chunk` events (text deltas, tool calls, done, errors)
3. Frontend accumulates text and tool calls from the stream
4. When a tool call arrives, it's executed locally (read/write files, search, list dir, queue command)
5. Tool results are appended to conversation and a follow-up request is sent automatically
6. Loop continues until the agent responds with text only (no more tool calls)

Provider/model selection UI lives in the panel header. Provider config (API keys, models) is stored in settings.
