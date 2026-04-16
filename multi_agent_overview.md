# Launchpad — Multi-Agent Terminal Management

## Vision

Launchpad is the native control plane for developers who run multiple CLI coding agents (Claude Code, OpenCode, Aider, etc.) simultaneously. Terminal-first, agent-agnostic. You bring your favorite AI — Launchpad gives you the workspace to manage it.

## Core Problem

Running 2+ agent instances across projects means babysitting terminals. You click between tabs checking status — did it finish? Is it waiting for permission? Which one was working on the API? There's no overview, no coordination, no way to see what's happening at a glance.

## How It Works

### The Workflow

1. Open terminal tabs — each pointed at a different project/branch directory
2. Launch your CLI agents (`claude`, `opencode`, `aider`) in each tab
3. Launchpad detects what's running, labels tabs, and shows an overview
4. Each agent works on its own branch — PRs handle merging when done
5. File browser follows whichever tab you're focused on

### Agent Detection — Process-Based, Not Output Parsing

Launchpad identifies agents by **inspecting the PTY's foreground process name** — not by parsing terminal output (which is fragile and breaks when agents change their UI).

On macOS:
- `ioctl(master_fd, TIOCGPGRP)` gets the foreground process group
- `libproc` enumerates processes in that group to find the process name
- Known agents: `claude`, `opencode`, `aider` — mapped to labels and colors

This is reliable, zero-maintenance, and automatically supports new agents by adding one entry to a lookup table.

### Per-Tab CWD Tracking

Each terminal tab tracks its own working directory via two mechanisms:

1. **OSC 7 escape sequences** (primary) — modern shells (zsh, bash 5.1+) emit `\x1b]7;file:///path\x07` when the directory changes. xterm.js parses these in real-time. Zero latency, zero polling.

2. **Process inspection** (fallback) — for shells that don't emit OSC 7, poll the shell process's cwd via `libproc`'s `proc_pidinfo` with `PROC_PIDVNODEPATHINFO`. Works for any process on macOS.

The file browser follows the active tab's cwd. Switching tabs switches the file tree.

## Agent Monitor Panel (`Cmd+Shift+A`)

```
┌─ Agent Monitor ──────────────────────────────────┐
│                                                   │
│  ● Claude Code — backend          [active]       │
│    ~/projects/backend  ·  feature/auth            │
│                                                   │
│  ○ Claude Code — frontend         [idle]         │
│    ~/projects/frontend  ·  feature/ui             │
│                                                   │
│  ● OpenCode — data-pipeline       [active]       │
│    ~/projects/pipeline  ·  main                   │
│                                                   │
│  ○ Terminal                        [shell]        │
│    ~/projects/backend                             │
│                                                   │
│  [Click any entry to jump to that tab]            │
└───────────────────────────────────────────────────┘
```

- Shows every terminal tab with agent type, directory, branch, and status
- Status indicators: ● active (agent running), ○ idle (shell prompt)
- Click to jump to any tab
- Auto-refreshes every 3 seconds via process polling

## Tab System as Agent Manager

### Tab Labeling

Tabs auto-detect what's running and label themselves:
- **Claude Code** tabs: purple indicator, labeled "Claude Code — projectname"
- **OpenCode** tabs: green indicator
- **Aider** tabs: yellow indicator
- **Plain shell** tabs: default style, labeled "Terminal N"

Detection uses the foreground process name — no configuration needed. User-set tab names (via double-click rename) take priority over auto-labels.

### Split Workspace as Agent Parallelism

The existing `Cmd+\` split workspace shows two agents side by side. Claude Code on the left working on backend, OpenCode on the right working on frontend. Each side has its own file browser context.

## File Browser: Per-Tab Context

Each terminal tab tracks its own cwd. When you switch tabs, the file browser switches to that tab's directory. No manual navigation needed.

```
Tab: Claude Code (backend)  →  File tree shows: ~/projects/backend
Tab: Claude Code (frontend) →  File tree shows: ~/projects/frontend
Tab: plain shell             →  File tree shows: wherever you cd'd
```

The existing ⏎ button sends `cd` to the active terminal. The ⌂ button navigates the file browser back to the active tab's cwd.

## Branch Isolation

Each agent should work on its own git branch. Launchpad doesn't enforce this — it shows the current branch per tab in the monitor panel. If two agents are on the same branch, the user sees it and can decide what to do. When agents finish, the user creates PRs to merge their work.

## What Launchpad Is Not

- Not another AI coding agent — it's the workspace, not the agent
- Not an AI chat tool — the AI lives in the terminal via CLI agents you already use
- Not trying to out-Cursor Cursor — different product, different user
- Not replacing your agents — orchestrating them

## What Launchpad Is

- **Terminal-first** — the terminal is the center, always
- **Agent-agnostic** — bring Claude Code, OpenCode, Aider, whatever comes next
- **Multi-agent dashboard** — see what every agent is doing at a glance
- **Context-aware file browser** — follows the active tab's directory automatically
- **Native** — Tauri, not Electron. Fast, small, Mac-native

## Implementation Priority

### v1 (current)
1. Per-tab cwd tracking via OSC 7 + process inspection fallback
2. File browser follows active tab
3. Process-based agent detection from PTY foreground process
4. Tab auto-labeling with agent type + project name
5. Agent monitor panel (Cmd+Shift+A)

### v2 (future)
6. macOS notifications when an agent needs input
7. Quick agent launcher — templates for common setups
8. Branch display per tab in monitor panel
9. Agent activity timeline — what each agent did, when
