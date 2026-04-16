# Launchpad

A terminal-first desktop workspace for macOS. Terminal, code editor, file browser, git — all in one native app. No Electron, no bloat, no opinions.

![Built with Tauri](https://img.shields.io/badge/Tauri-v2-blue) ![Platform](https://img.shields.io/badge/platform-macOS-lightgrey) ![License](https://img.shields.io/badge/license-MIT-green)

## What is this?

Launchpad is a lightweight macOS desktop app that puts your terminal front and center, then wraps it with everything you need to actually get work done — a file browser, a real code editor, a visual git workflow, and a settings panel. All in a single ~8MB native app.

No framework. No Electron. No subscription. Just Rust + vanilla JS.

## Install

### From source
```bash
# Prerequisites: Rust, Node.js
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Clone and build
git clone https://github.com/adamwickwire/launchpad.git
cd launchpad
npm install
npx tauri build

# Install
cp -R src-tauri/target/release/bundle/macos/Launchpad.app /Applications/
```

### Development
```bash
npm install
npx tauri dev
```

## Features

### Terminal
Full PTY-backed terminal with tabs, split panes, and a split workspace. Not a web terminal pretending to be real — it spawns actual zsh/bash processes with proper signal handling, 256-color support, and correct escape sequences.

- Multiple tabs (Cmd+T / Cmd+W / Cmd+1-9)
- Split pane within a tab (Cmd+D) with draggable divider
- Split workspace into left/right groups (Cmd+\\) — each group gets its own tab bar
- Drag tabs between groups or move with Cmd+Shift+M
- Switch focus between groups with Cmd+Option+Left/Right
- Clear screen (Cmd+K)
- Right-click context menu with Copy, Paste, Clear
- Drag files from sidebar to paste paths
- Configurable font, size, cursor style, scrollback
- WebGL-accelerated rendering with canvas fallback
- Unicode 11 width tables for correct CJK/emoji rendering
- Backpressure flow control prevents xterm.js write queue overflow

### Code Editor
Files open as tabs alongside your terminal tabs — one unified tab bar. Click a file in the sidebar or use Cmd+P, and it opens in a full CodeMirror 6 editor.

- Syntax highlighting for JS, TS, JSX, TSX, Python, Rust, HTML, CSS, JSON, Markdown, SCSS, TOML, YAML, Shell
- Find and replace (Cmd+F / Cmd+H)
- Bracket matching, close brackets, code folding
- Autocompletion, highlight selection matches
- Rectangular selection, crosshair cursor
- Line numbers, active line highlight, indent guides
- File path breadcrumb with › separators + line/column status bar
- Modified indicator in tab (yellow dot)
- Unsaved changes warning on close
- Right-click context menu with Cut, Copy, Paste, Select All
- Open multiple files — each gets its own tab
- Save with Cmd+S

### Git Panel
Open with Cmd+G. A visual git workflow designed so you never have to remember git commands. Stage, commit, push, pull, stash, merge, create branches — all from buttons.

- **Quick actions toolbar** — Pull, Push, Fetch, Stash, Pop in one click
- **Staged vs unstaged split** — clear separation of what's going into your commit
- **Stage / unstage / discard** — per-file buttons with confirmation on destructive actions
- **Commit form** — multi-line textarea with conventional commit prefix dropdown (feat/fix/chore/...) and character counter
- **Branch management** — create, switch, delete, merge branches. View local and remote branches with last commit info
- **Commit history** — last 30 commits with OID, message, author, and timestamp. Click to expand and see changed files with +/- line counts
- **Diff preview** — click any changed file to see a structured diff with line numbers and hunk headers
- **GitHub integration** — Open Repo, Open Branch, and Create PR buttons (auto-detected from your remote URL)
- **Conflict resolution** — when merge conflicts happen, resolve with Ours/Theirs buttons or open the file in the editor
- **Stash management** — save, pop, apply, and drop stashes
- **Git cheatsheet** — hit `?` for a plain-English explanation of git concepts
- **Auto-push with upstream** — first push to a new branch automatically sets up tracking
- **Cancellable network ops** — push, pull, fetch, merge can be cancelled mid-operation
- **Ahead/behind indicators** — see how your branch compares to remote at a glance

### File Browser
- Tree view with expandable directories
- Color-coded by file type (JS=yellow, Python=green, Rust=pink, config=cyan) and git status (modified=yellow, new=green, deleted=red, staged=green, conflict=pink)
- **Decoupled browsing** — navigating folders does NOT cd your terminal. Browse freely without disrupting running processes (like Claude Code). Click the blue ⏎ button in the sidebar header to explicitly set the working directory.
- **CRUD operations** — right-click to create new files/folders, rename, delete, and reveal in Finder
- **Live filesystem watcher** — files created, modified, or deleted from the terminal or externally appear instantly in the tree (no manual refresh needed)
- **Off-DOM tree building** — folder expansion is flicker-free thanks to detached DOM fragment rendering
- Click to open in editor, double-click folder to navigate
- Drag files to terminal to paste the path
- Right-click: copy path, copy name, reveal in Finder
- Search/filter with Cmd+F
- Toggle hidden files
- Resizable sidebar

### Quick Open (Cmd+P)
Fuzzy file search across your project. Case-insensitive, ranked by path length. Skips hidden dirs, node_modules, target, and other noise. Arrow keys to navigate, Enter to open as editor tab.

### Settings (Cmd+,)
All preferences in one place, applied live:

- **General** — startup directory with native folder picker (Browse button), sidebar width
- **Terminal** — font family (SF Mono, Menlo, Fira Code, JetBrains Mono...), font size, scrollback, cursor style, cursor blink
- **Editor** — font size, tab size, word wrap
- **Git** — auto-refresh interval, default commit prefix

Settings saved to `~/.launchpad/config.json`.

### Toolbar
A compact header bar with quick access to:
- ⚙ Settings
- ⌘ Keyboard shortcuts reference (hover to see all shortcuts)
- ⎇ Git panel toggle

## Keyboard Shortcuts

| Shortcut | Action |
|----------|--------|
| Cmd+T | New terminal tab |
| Cmd+W | Close current tab |
| Cmd+1-9 | Switch to tab N |
| Cmd+D | Split/unsplit terminal pane |
| Cmd+\\ | Split/unsplit workspace (left/right groups) |
| Cmd+Option+Left/Right | Switch focus between groups |
| Cmd+Shift+M | Move tab to other group |
| Cmd+K | Clear terminal |
| Cmd+P | Quick open (fuzzy file search) |
| Cmd+G | Toggle git panel |
| Cmd+F | Find in editor / search sidebar |
| Cmd+H | Find and replace in editor |
| Cmd+S | Save file in editor |
| Cmd+, | Open settings |
| Escape | Close diff preview / dialog |

## Tech Stack

| Layer | Technology |
|-------|-----------|
| Desktop shell | Tauri v2 (~8MB native app) |
| Backend | Rust — PTY via portable-pty, git via libgit2 + system git for network ops |
| Frontend | Vanilla JS — no React, no Vue, no framework |
| Terminal | xterm.js 6 with WebGL renderer + Unicode 11 |
| Editor | CodeMirror 6 |
| Bundler | Vite |

## Why?

Most developer tools are either too heavy (VS Code, Electron apps) or too minimal (bare terminal). Launchpad sits in between — it's a workspace that respects your terminal-first workflow while giving you the visual tools for things that are genuinely better with a UI (git, file browsing, settings).

It's also a proof of concept: you can build a fast, capable desktop IDE in ~1500 lines of Rust and ~3000 lines of vanilla JS. No framework tax, no dependency hell, no build step that takes longer than 1 second.

## License

MIT
