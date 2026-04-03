# Launchpad

A terminal-first desktop workspace for macOS. File browser, code editor, git panel — no AI agent, no opinions. Bring your own tools.

![Built with Tauri](https://img.shields.io/badge/Tauri-v2-blue) ![Platform](https://img.shields.io/badge/platform-macOS-lightgrey)

## What is this?

Launchpad is a lightweight desktop app that combines:
- **A real terminal** — full PTY-backed zsh/bash with tabs and split panes
- **A file browser** — navigate your projects, drag files to terminal, right-click context menu
- **A code editor** — syntax highlighting for 15+ languages, minimap, save with Cmd+S
- **A git panel** — branches, commits, inline diffs, one-click stage & commit
- **Quick open** — Cmd+P fuzzy file finder

Think: VS Code minus the editor bloat, or Warp minus the AI agent. Just a clean workspace where your terminal is the star.

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

# The .app is at: src-tauri/target/release/bundle/macos/Launchpad.app
# Copy to Applications
cp -R src-tauri/target/release/bundle/macos/Launchpad.app /Applications/
```

### Development
```bash
npm install
npx tauri dev
```

## Features

### Terminal
- Full PTY terminal (zsh/bash)
- Multiple tabs (Cmd+T, Cmd+W, Cmd+1-9)
- Split panes (Cmd+D)
- Clear (Cmd+K)
- Right-click copy/paste

### File Browser
- Tree view with expandable directories
- Color-coded by file type
- Drag files to terminal to paste path
- Right-click: copy path, reveal in Finder, preview
- Cmd+F to search/filter
- Resizable sidebar

### Code Editor
- Syntax highlighting (JS, TS, Python, Rust, HTML, CSS, JSON, Markdown, and more)
- Line numbers, bracket matching
- Minimap
- Edit and save files (Cmd+S)
- Inline git diffs

### Git Panel (Cmd+G)
- Current branch display
- View and switch branches
- Create new branches
- Commit history
- Changed files with status indicators
- Stage individual files or all
- Commit with message
- Click changed file to see diff

### Quick Open (Cmd+P)
- Fuzzy file search across project
- Arrow keys to navigate, Enter to open

## Tech Stack
- **Tauri v2** — ~8MB app, native macOS window
- **Rust** — PTY via portable-pty, git via libgit2
- **Vanilla JS** — no framework
- **xterm.js** — terminal emulator
- **CodeMirror 6** — code editor

## License
MIT
