# Launchpad

A terminal-first macOS workspace. Terminal, file browser, git panel, code editor, and a built-in chat agent — scoped to one project per window.

![Built with Tauri](https://img.shields.io/badge/Tauri-v2-blue) ![Platform](https://img.shields.io/badge/platform-macOS-lightgrey) ![License](https://img.shields.io/badge/license-Apache_2.0-green)

**Docs:** [walrusquant.github.io/launchpad](https://walrusquant.github.io/launchpad/) — landing, getting started, features, shortcuts, architecture.

![Launchpad welcome screen](docs/media/landing.png)

## What is this?

A desktop app I built because I wanted a comfortable place to run Claude Code and other CLI coding agents. I spend most of my time in the terminal now, and I wanted the rest of the workflow — file tree, git, the occasional file edit — wrapped around it in a way that felt natural to me.

## Why I made it

I wasn't trying to reinvent anything or prove a point. I just wanted a tool that fit how I actually code with CLI agents. Open a project, have a terminal ready, click around files without worrying about accidentally breaking something in the shell, commit stuff without remembering git flags. That's it.

I told Claude I wanted to build a desktop app. It suggested Tauri + Rust + vanilla JS. I said sounds good, we'll figure it out. This is what we figured out.

![Launchpad demo](docs/media/demo.gif)

![Workspace with terminal, file browser, and git panel](docs/media/main.png)

## Install

### Download (easiest)

Grab the latest `.dmg` from [Releases](https://github.com/WalrusQuant/launchpad/releases/latest). Apple Silicon only for now.

1. Open the DMG, drag `Launchpad.app` to `/Applications`
2. First launch: right-click the app → **Open** (the app isn't code-signed, so macOS will warn). Or run `xattr -dr com.apple.quarantine /Applications/Launchpad.app`.

### From source
```bash
# Prerequisites: Rust, Node.js
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Clone and build
git clone https://github.com/WalrusQuant/launchpad.git
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

### Projects
Launchpad is organized around **projects** — a project is just a root directory. When you open one, every terminal spawned in that window starts at the project root, the file browser is locked there, and the git panel operates on that repo. Cmd+P fuzzy-search stays inside the project. This means you can run a CLI agent like `claude` or `aider` in a terminal tab and click around the file tree without ever sending a stray `cd` to the shell — the file browser is purely visual.

- **One window = one project.** Opening a project in the picker takes over the current window (VS Code convention). Already-open projects get focused if you try to open them again.
- **Multi-window on demand.** Cmd+Shift+N opens a fresh project picker so you can open a second project in parallel. Each window is fully independent.
- **Picker with recents.** Projects are stored at `~/.launchpad/projects.json`. Right-click (or the ⋯ hover menu) to rename, open in a new window, or remove from the list.
- **Back to projects** — a `←` button in the toolbar tears down the workspace and returns to the picker.

### Terminal
Full PTY-backed terminal with tabs, split panes, and a split workspace. Spawns real zsh/bash processes with proper signal handling, 256-color support, and correct escape sequences.

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
Open with Cmd+G. A visual git workflow designed so you never have to remember git commands. Stage, commit, push, pull, stash, merge, create branches, amend, cherry-pick, rebase — all from buttons.

- **Quick actions toolbar** — Pull, Push, Fetch, Stash, Pop in one click
- **Staged vs unstaged split** — clear separation of what's going into your commit
- **Stage / unstage / discard** — per-file buttons with confirmation on destructive actions
- **Commit form** — multi-line textarea with conventional commit prefix dropdown (feat/fix/chore/...) and character counter
- **Amend** — `Amend (with staged)` reuses the index, `Amend message only` keeps the existing tree. Both prompt before rewriting a commit that's already on a remote.
- **Cherry-pick onto HEAD** — right-click a commit → cherry-pick. Conflicts route through the Pending Operation banner with Continue / Abort.
- **Interactive rebase** — right-click a commit → "Rebase from here…" opens a dedicated rebase tab. Drag commits to reorder, choose `pick / reword / squash / fixup / drop / edit` per row, then Apply. Conflicts pause the rebase, open the conflicted file inline, and surface Continue / Skip / Abort on the Pending Operation banner. Every interactive rebase creates a backup tag (`rebase-backup-<branch>-<timestamp>`) so you can recover from a bad rewrite.
- **Pending Operation banner** — when a merge / cherry-pick / rebase is paused, the banner shows the operation kind, current step / total steps (rebase), and the relevant Continue / Skip / Abort buttons. Single source of truth for "what state is git in?"
- **Branch management** — create, switch, delete, merge branches. View local and remote branches with last commit info
- **Commit history** — last 30 commits with OID, message, author, and timestamp. Click to expand and see changed files with +/- line counts. Right-click for: Compare with HEAD / parent / arbitrary ref, Cherry-pick onto HEAD, Rebase from here, Copy OID.
- **Compare two refs** — Compare with… opens a dedicated diff tab showing every changed file between the two refs, with full hunks and +/- counts.
- **Diff preview** — click any changed file to see a structured diff with line numbers and hunk headers
- **Inline conflict editor** — open a conflicted file and the editor renders an action bar above each conflict block with Accept Ours / Theirs / Both. Save when all blocks are resolved → file auto-stages and the panel updates.
- **3-pane merge tab** — for tougher conflicts, click Open 3-Way to see ours / merged / theirs side by side. Side panes are read-only; the center pane is editable with synchronized line-aligned scrolling. Save → auto-stages.
- **GitHub integration** — Open Repo, Open Branch, and Create PR buttons (auto-detected from your remote URL)
- **Stash management** — save, pop, apply, and drop stashes
- **Git cheatsheet** — hit `?` for a plain-English explanation of git concepts
- **Auto-push with upstream** — first push to a new branch automatically sets up tracking
- **Cancellable network ops** — push, pull, fetch, merge can be cancelled mid-operation, even on a stalled TCP connection
- **HTTPS-safe defaults** — no interactive credential prompts (they'd hang forever in a GUI app with no TTY); SSH agent socket forwarded even when launched from Finder
- **Ahead/behind indicators** — see how your branch compares to remote at a glance

### File Browser
- Tree view with expandable directories, rooted at the project directory
- Color-coded by file type (JS=yellow, Python=green, Rust=pink, config=cyan) and git status (modified=yellow, new=green, deleted=red, staged=green, conflict=pink)
- **Agent-safe** — navigating folders NEVER writes anything to a terminal. Browse freely while a CLI agent runs without risk of sending a surprise `cd`.
- **Root-locked** — nav-up `↑` caps at the project root (can't escape above it), go-home `⌂` jumps back to the root from any depth
- **CRUD operations** — right-click to create new files/folders, rename, delete, and reveal in Finder
- **Live filesystem watcher** — files created, modified, or deleted from the terminal or externally appear instantly in the tree (no manual refresh needed)
- **Follows external renames** — rename a file in Finder or via `mv` and the open editor tab follows it automatically (Unix inode match); deleted files get a stale-tab marker so Cmd+S can't silently write to a vanished path
- **Off-DOM tree building** — folder expansion is flicker-free thanks to detached DOM fragment rendering
- Click to open in editor, double-click folder to navigate
- Drag files to terminal to paste the path
- Right-click: copy path, copy name, reveal in Finder
- Search/filter with Cmd+F
- Toggle hidden files
- Resizable sidebar

### Built-in Agent (Cmd+I)
A chat coding agent built into Launchpad as its own tab type, alongside terminal and editor tabs. Runs in-process — no sidecar binary, no stdio framing — by depending on the [launchpad-agent](https://github.com/WalrusQuant/launchpad-agent) crates directly.

- **10 provider presets** — Anthropic, OpenAI, Google, OpenRouter, Groq, Together, Mistral, Ollama, Z.ai (coding plan), and a Custom OpenAI-compatible endpoint. Pick a provider, paste an API key, hit Save & Reload — no app restart, sessions reconnect on next message.
- **Markdown streaming** with code-block highlighting; per-turn cost / token-usage footer.
- **Tool cards** for every tool the agent calls — bash, read, write, apply_patch, glob, grep, web fetch / search, MCP tools — with collapsible result panes. `apply_patch` calls render an inline diff using the same diff renderer the git panel uses.
- **`@`-file mentions** in the composer, backed by the same fuzzy index Cmd+P uses. Picked files become structured context the agent receives directly.
- **Slash commands** — type `/` to pick a skill (a `SKILL.md` document under `~/.lpagent/skills/<name>/`). Picked skills inject their body into the next turn as system context. Four starter skills bundled and seeded on first run: `/commit`, `/review`, `/explain`, `/plan`.
- **Approval cards** with six scopes (Once / Turn / Session / Path Prefix / Host / Tool) when a tool needs the user's permission.
- **Launchpad-native tools** — the agent can call `lp_open_in_editor`, `lp_show_diff`, `lp_open_merge_tab`, `lp_refresh_git_panel`, `lp_reveal_in_finder` to drive the workspace UI directly (e.g. show you the file it just edited).
- **Sessions are persistent** — past conversations sit in a sidebar (☰ icon), resumable in any window. Delete a session and its rollout file goes with it.
- **API keys live at `~/.launchpad/agent-config.json`** with chmod 0o600 (owner read/write only).
- **CLI agents still work too.** The built-in chat doesn't replace `claude`, `aider`, `goose`, or anything else you run in a terminal tab — they're complementary surfaces.

Full architecture docs in [specs/agent-integration-spec.md](specs/agent-integration-spec.md).

### Quick Open (Cmd+P)
Fuzzy file search across your project. Case-insensitive, ranked by path length. Skips hidden dirs, node_modules, target, and other noise. Arrow keys to navigate, Enter to open as editor tab.

### Settings (Cmd+,)
All preferences in one place, applied live:

- **General** — sidebar width
- **Terminal** — font family (SF Mono, Menlo, Fira Code, JetBrains Mono...), font size, scrollback, cursor style, cursor blink
- **Editor** — font size, tab size, word wrap
- **Git** — auto-refresh interval, default commit prefix
- **Agent** — provider preset, API key, default model, base URL. Save & Reload rebuilds the runtime in place.

Settings saved to `~/.launchpad/config.json`. Project list saved to `~/.launchpad/projects.json`. Agent config saved to `~/.launchpad/agent-config.json` (chmod 0o600).

### Toolbar
A compact header bar with quick access to:
- ← Back to projects (close current workspace, return to picker)
- + New window (Cmd+Shift+N)
- ⚙ Settings
- ⌘ Keyboard shortcuts reference (hover to see all shortcuts)
- ⎇ Git panel toggle

## Keyboard Shortcuts

| Shortcut | Action |
|----------|--------|
| Cmd+Shift+N | New window (opens a fresh project picker) |
| Cmd+T | New terminal tab |
| Cmd+W | Close current tab |
| Cmd+1-9 | Switch to tab N |
| Cmd+D | Split/unsplit terminal pane |
| Cmd+\\ | Split/unsplit workspace (left/right groups) |
| Cmd+Option+Left/Right | Switch focus between groups |
| Cmd+Shift+M | Move tab to other group |
| Cmd+K | Clear terminal |
| Cmd+I | New agent chat tab |
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
| Desktop shell | Tauri v2 |
| Backend | Rust — PTY via portable-pty, git via libgit2 + system git for network ops |
| Agent runtime | [launchpad-agent](https://github.com/WalrusQuant/launchpad-agent) crates (`lpa-core`, `lpa-server`, `lpa-provider`, `lpa-tools`, `lpa-safety`, `lpa-mcp`, `lpa-protocol`, `lpa-utils`) — copied in-tree under `crates/`, run in-process via mpsc → Tauri events |
| Frontend | Vanilla JS |
| Terminal | xterm.js 6 with WebGL renderer + Unicode 11 |
| Editor | CodeMirror 6 |
| Markdown | marked (chat tab rendering) |
| Bundler | Vite |

## License

Apache 2.0 — see [LICENSE](LICENSE).
