# Launchpad — Project Model Spec

## What Launchpad Is

A project-scoped terminal workspace for macOS. One project per window, multiple windows for multiple projects. Terminal-first — the AI lives in the terminal via CLI agents (Claude Code, Aider, OpenCode, whatever), not in a sidebar.

Built for devs who work in the terminal and want their workspace organized around it, not around an editor.

## The Problem

Right now Launchpad opens to a blank terminal. There's no anchor. Tabs can cd anywhere, split panes inherit unpredictable directories, the file browser drifts, and the git panel doesn't know which repo matters. This looseness causes bugs and confusion — especially when running a CLI agent that shouldn't get disrupted by an accidental cd.

## The Solution: Projects

### One window = one project

```
┌─ Launchpad: backend ────────────────────────────┐
│ [Tab 1: Terminal] [Tab 2: claude] [Tab 3: sh]    │
│                                                   │
│  Every terminal locked to ~/Code/backend          │
│  File browser rooted at ~/Code/backend            │
│  Git panel operates on ~/Code/backend             │
│  Can browse/edit any file, but can't cd out        │
│                                                   │
└───────────────────────────────────────────────────┘
```

Working on two projects? Two windows.

```
┌─ Launchpad: backend ──────┐  ┌─ Launchpad: frontend ─────┐
│ [claude] [terminal]        │  │ [aider] [terminal]         │
│ ~/Code/backend             │  │ ~/Code/frontend            │
└────────────────────────────┘  └────────────────────────────┘
```

macOS window management handles the layout — Cmd+` to switch, Stage Manager, split screen, whatever the user prefers.

### Why not multi-project in one window?

It sounds cool but it's a mess. Each tab group needs its own file browser, its own git panel, its own context. You end up rebuilding VS Code's multi-root workspace, which everyone hates. One window per project is dead simple and doesn't confuse anything.

---

## What "Project" Means

A project is a root directory. That's it.

```json
// ~/.launchpad/projects.json
[
  { "name": "backend", "path": "/Users/adam/Code/backend", "lastOpened": "2026-04-16T05:00:00Z" },
  { "name": "frontend", "path": "/Users/adam/Code/frontend", "lastOpened": "2026-04-15T20:00:00Z" },
  { "name": "launchpad", "path": "/Users/adam/Code/launchpad", "lastOpened": "2026-04-16T00:00:00Z" }
]
```

- No config files in the repo. No `.launchpad/` folder. No lock-in.
- Name defaults to the directory name, user can rename.
- `lastOpened` for sorting recent projects.
- One window per project — opening an already-open project focuses that window.

---

## What Changes

### Terminal
- All new terminals spawn in the project root. Always.
- Split panes spawn in the project root. Always.
- No more guessing about directory inheritance.
- The shell is real — you can cd within the project tree. But this is about the spawn directory, not restricting the shell.
- `termDefaultDirectory` setting goes away. The project root replaces it.

### File Browser
- Root is locked to the project directory.
- Full tree navigation within the project — expand, collapse, scroll.
- Cannot navigate above the project root.
- Nav-up ↑ and go-home ⌂ buttons stay — they're pure navigation and don't touch any terminal. Nav-up is capped at the project root; go-home jumps back to the project root.
- The ⏎ "cd to this directory" button is **removed**. It was the only file-browser control that wrote `cd` to a terminal. Removing it protects running CLI agents from accidental `cd` disruption. Users who want to cd can type it in the shell.
- Clicking a file still opens it in the editor.

### Git Panel
- Always operates on the project root's repo. No ambiguity.
- If the project root isn't a git repo, git panel is hidden/disabled.

### Editor
- Cmd+P quick open searches within the project root only.
- Files opened from outside the project (via terminal, etc.) still work — save goes to the right place.
- Editor tabs, split workspace, all existing editor features stay the same.

### Settings
- Global settings apply to all projects (font, theme, scrollback, etc.).
- Per-project settings are out of scope for now, but the data model allows adding them later.

---

## App Flow

### First launch (no projects)
```
┌─ Launchpad ──────────────────────────────────────┐
│                                                   │
│          Welcome to Launchpad                     │
│                                                   │
│     Open a project directory to get started        │
│                                                   │
│          [ Open Project ]                          │
│                                                   │
└───────────────────────────────────────────────────┘
```

Native folder picker → directory becomes first project → window opens with terminal at that root.

### Returning user (has projects)
```
┌─ Launchpad ──────────────────────────────────────┐
│                                                   │
│   Projects                                        │
│                                                   │
│   backend            ~/Code/backend               │
│   main · last opened 2h ago                       │
│                                                   │
│   frontend           ~/Code/frontend              │
│   feat/ui · last opened 1d ago                    │
│                                                   │
│   launchpad          ~/Code/launchpad             │
│   main · last opened just now                     │
│                                                   │
│   [ + Open New Project ]                           │
│                                                   │
└───────────────────────────────────────────────────┘
```

- Click a project → opens a window scoped to that project (or focuses existing window)
- Each row shows: name, path, git branch, last opened
- Keep it simple — a list, not cards. Fast to scan.
- Right-click to remove a project from the list (doesn't delete files)

### Inside a project window

Same as current Launchpad, but:
- Window title shows project name
- All terminals spawn at project root
- File browser locked to project root
- Git panel locked to project root

Everything else (tabs, split panes, split workspace, editor, keyboard shortcuts) works exactly as it does today.

---

## Multi-Window

### How it works
- Each project opens in its own Tauri window.
- Windows are independent — closing one doesn't affect others.
- The project picker is its own window (or the last remaining window when all projects are closed).
- Cmd+` switches between Launchpad windows (standard macOS behavior).

### Tauri implementation
Tauri v2 supports multiple windows natively via `WebviewWindowBuilder`. Each window:
- Gets its own webview with its own JS context
- Shares the same Rust backend / AppState
- Can communicate via Tauri events if needed

The main question: does each window get its own set of PTYs in the shared AppState, or does each window run its own isolated state? Shared AppState with window-scoped PTY namespacing is probably simplest.

---

## Agent Strategy

Launchpad ships its own built-in coding agent AND supports external CLI agents side by side.

### Built-in Agent (see `specs/agent-spec.md`)
- Native Rust agent module in the Tauri backend — agentic loop, tool use, streaming
- Agent tab type alongside terminal/editor/settings
- Deep integration: file browser updates, git panel sync, editor tab opens
- Provider-agnostic: Anthropic + OpenAI
- **Project-scoped**: agent's working directory = project root, file writes blocked outside boundary

### External CLI Agents
Users can still run whatever CLI agent they want in terminal tabs:

- **Claude Code** — Anthropic models
- **Aider** — supports dozens of models via litellm, including custom endpoints
- **OpenCode** — open source, multiple providers
- **goose** — Block's open source agent

The project model makes both built-in and CLI agents better:
- Agent always starts in the right directory (project root)
- Split workspace lets you watch an agent in one group and work in another
- Multiple windows = multiple agents on different projects, each properly scoped

### What Launchpad adds on top (later)
- Tab auto-labeling when a CLI agent is detected
- Status indicator showing if a tab has an active agent process

---

## What's Out of Scope

- ~~Native AI agent panel~~ — **Updated**: built-in agent now planned (see `specs/agent-spec.md`). Complements CLI agents, doesn't replace them.
- Multi-project in one window — too complex, not worth it.
- Agent orchestration / coordination — that's the agent's job, not the workspace's.
- Per-project settings — later, not now.
- Project templates / scaffolding — not an IDE.
- Cross-window agent monitoring — maybe later if there's demand.

---

## Implementation Order

### Phase 1: Project foundation
1. Project data model — `~/.launchpad/projects.json`, load/save Tauri commands
2. Project picker UI — simple list view on app open
3. Lock workspace to project root — terminals, file browser, git panel, quick open all scoped
4. Window title shows project name

### Phase 2: Multi-window
5. Open projects in separate Tauri windows
6. One window per project enforcement (focus existing window if already open)
7. Project picker becomes the "home" window

### Phase 3: Polish
8. Tab auto-labeling for detected CLI agents
9. Remove stale projects from list (directory no longer exists)
10. Remember window position/size per project
