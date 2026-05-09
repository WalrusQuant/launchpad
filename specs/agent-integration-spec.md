# Launchpad — launchpad-agent Integration Spec (in-process)

> **Status — 2026-05-09**
>
> The integration approach changed from "vendored submodule" to **full in-tree
> port**: every agent crate (`core`, `protocol`, `server`, `provider`,
> `tools`, `safety`, `mcp`, `utils`, `client`, `tasks`) is copied into
> `crates/` at the Launchpad repo root and pulled in as workspace path deps.
> No submodule, no sidecar, no separate binary. The original launchpad-agent
> repo lives independently for CLI/TUI work; bumping is now a manual sync.
>
> See **§ Status** at the bottom for what's shipping today and what's still
> on the punch list.

## Context

We are building a built-in chat coding agent inside Launchpad by integrating [launchpad-agent](https://github.com/WalrusQuant/launchpad-agent) (Apache-2.0, Rust, our own project) **in-process** as a Cargo workspace dependency. The agent runs as async tasks inside Launchpad's existing Rust binary — no child process, no stdio framing, no separate `lpagent` binary.

There is no terminal entry point for the built-in agent. Users who want a CLI agent in a terminal tab keep using their existing tools (Claude Code, Aider, etc.) — Launchpad does not compete on that surface. The built-in agent exists exclusively as a chat tab.

### Relationship to project model

The chat tab is project-scoped, exactly like terminal tabs:
- Agent session `cwd` = `getActiveProject().path`
- File mentions resolve relative to the project root
- Multi-window: each Launchpad window owns its own per-window `ServerRuntime` connection (sessions don't cross windows)

---

## Why in-process — what launchpad-agent gives us for free

launchpad-agent's `lpa-server` crate is described in its own `Cargo.toml` as **"Transport-neutral runtime protocol types and server contracts."** The JSON-RPC envelope is the public boundary, but the runtime doesn't care whether that JSON arrives over stdio, WebSocket, or an in-process channel. From `crates/server/src/runtime.rs`:

- `ServerRuntime::new(server_home, deps) -> Arc<Self>` — public
- `register_connection(transport, sender: mpsc::UnboundedSender<serde_json::Value>) -> connection_id` — public; the sender is whatever channel we provide
- `handle_incoming(connection_id, message: serde_json::Value) -> Option<serde_json::Value>` — public; JSON in, JSON out
- `load_persisted_sessions()` — public; free session resume

What we inherit by depending on these crates:

- **Session / turn / streaming logic** — the genuinely hard part of any agent
- **Provider abstraction** (`crates/provider`) — Anthropic, OpenAI, Gemini, OpenRouter, Groq, Together, Mistral, Ollama, plus presets for adding more
- **Tool registry** (`crates/tools`) with built-in `Read`, `Write`, `Edit`, `ApplyPatch`, `Bash`, `Glob`, `Grep`, `Todo`, `Plan`, `Question`, `WebFetch`, `WebSearch`, `Lsp`
- **MCP support** (`crates/mcp`) — `StdMcpManager`, automatic tool registration, trust levels
- **Skills system** — user + workspace skill discovery, the source for slash commands
- **Approval policy** with `Once / Turn / Session / PathPrefix / Host / Tool` scopes
- **Persistence + session resume** via `RolloutStore`
- **Token / usage accounting** via `TurnUsage`

What we build in Launchpad is the **glue + the chat UI**. Everything below the protocol envelope is launchpad-agent's job.

---

## 1. Architecture — in-process runtime

```
┌──────────────────────────── Launchpad process ────────────────────────────┐
│                                                                            │
│  Frontend (JS)                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐  │
│  │ Chat tab (agentchat.js)                                             │  │
│  │   send: invoke('agent_send_envelope', { json })                     │  │
│  │   recv: listen('agent:event:<session_id>', payload => render(...))  │  │
│  └────────────────────────────────┬────────────────────────────────────┘  │
│                                   │ Tauri IPC                              │
│  Rust backend (src-tauri)         │                                        │
│  ┌────────────────────────────────▼────────────────────────────────────┐  │
│  │ agent::commands  →  AgentHost (per-window connection)               │  │
│  │   handle_incoming(conn_id, json)  →  Option<json>  (response)       │  │
│  │   register_connection(.., sender) ─┐                                │  │
│  │                                    │ mpsc<serde_json::Value>        │  │
│  │   sender_task: while let Some(v) = rx.recv().await {                │  │
│  │       window.emit("agent:event:<sid>", v)                           │  │
│  │   }                                                                 │  │
│  └─────────────────────────────────┬───────────────────────────────────┘  │
│                                    │ Rust function calls (no IPC)         │
│  ┌─────────────────────────────────▼───────────────────────────────────┐  │
│  │ launchpad-agent crates (lpa-core, lpa-server, lpa-provider,         │  │
│  │   lpa-tools, lpa-safety, lpa-mcp, lpa-protocol, lpa-utils)          │  │
│  │                                                                      │  │
│  │   ServerRuntime  ──  sessions, turns, streaming, persistence,       │  │
│  │                       provider, tool registry, MCP, approval,       │  │
│  │                       skills, model catalog                         │  │
│  └─────────────────────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────────────────────┘
```

The protocol boundary stays the same JSON envelope launchpad-agent already defines (`crates/protocol/src/protocol.rs`). We just deliver envelopes via an `mpsc::UnboundedSender` instead of a stdout pipe. Same methods (`session/start`, `turn/start`, `turn/interrupt`, `approval/respond`, `events/subscribe`, `skills/list`, etc.). Same notifications (item events with `EventContext`, item deltas, turn lifecycle, usage updates, approval requests).

### Why this over stdio sidecar

- **Zero IPC overhead.** Streaming text deltas are mpsc sends, not stdout writes that get parsed back into JSON. Material at ~50 events/second per active turn.
- **Single binary.** No `lpagent` binary to compile, place, sign, or version. Cargo handles the dep.
- **No PID / subprocess concerns.** No cancel-the-child window, no orphan-on-crash, no stdio buffer deadlocks.
- **Compile-time integration.** Protocol drift between Launchpad and launchpad-agent surfaces as a Rust compile error, not a runtime parse failure.
- **Custom tools registered as Rust closures.** A Launchpad-native tool (`open_in_editor`, `refresh_git_panel`) is a `Box<dyn Tool>` we add to the registry, not a JSON-RPC round-trip.

### Why we keep the JSON-RPC envelope (not call private handlers)

The dispatch handlers (`handle_session_start`, `handle_turn_start`, etc.) are private modules inside `crates/server/src/runtime.rs`. We could PR upstream to expose them as typed methods, but the JSON envelope is already a stable surface that costs essentially nothing in-process. The envelope buys us: stable contract, future cross-process compat if ever needed, identical wire shape to the TUI client (one set of bugs, not two).

### Minor upstream PR needed

`ClientTransportKind` (passed to `register_connection`) likely has variants for `Stdio` and `WebSocket` but no `InProcess`. One-line PR upstream to add `InProcess`. Until merged, we pass `Stdio` as a label — the runtime doesn't read it for anything load-bearing.

---

## 2. Rust integration glue

```
src-tauri/src/agent/
  mod.rs        AgentState (per-window AgentHost map), Tauri commands
  host.rs       AgentHost: builds ServerRuntime + ServerRuntimeDependencies
  bootstrap.rs  Mirrors crates/server/src/bootstrap.rs::run_server_process,
                  minus run_listeners() — constructs deps from Launchpad config
  events.rs     mpsc → Tauri event fan-out per window
  tools.rs      Launchpad-native tools registered into ToolRegistry
  config.rs     Maps Launchpad's settings to launchpad-agent's AppConfig
```

### `AgentState`

One `ServerRuntime` per **Launchpad process** (shared MCP supervisors, shared provider connection pool). Per-window state is just the connection_id and the sender task:

```rust
pub struct AgentState {
    runtime: OnceCell<Arc<ServerRuntime>>,           // lazily built on first use
    connections: Mutex<HashMap<String, AgentConn>>,  // keyed by Tauri window label
}

struct AgentConn {
    connection_id: u64,
    _sender_task: JoinHandle<()>,  // forwards mpsc → Tauri events
}
```

### Lifecycle

- **First chat tab anywhere** → `runtime` is built (mirrors `run_server_process` from `crates/server/src/bootstrap.rs` minus `run_listeners()`); `load_persisted_sessions().await` runs once
- **First chat tab in a window** → `register_connection` is called for that window; the returned `connection_id` and the sender task get stored in `AgentState.connections[window_label]`
- **Window closes** → `unregister_connection(connection_id)` is called; sender task drops; sessions persist (resumable in any future window)
- **App quits** → MCP supervisors get `shutdown_all()` (mirroring the Ctrl+C path in `run_server_process`)

### The sender task

```rust
let (tx, mut rx) = mpsc::unbounded_channel::<serde_json::Value>();
let connection_id = runtime.register_connection(ClientTransportKind::Stdio, tx).await;

let task = tokio::spawn(async move {
    while let Some(envelope) = rx.recv().await {
        let session_id = envelope
            .get("params")
            .and_then(|p| p.get("session_id"))
            .or_else(|| envelope.get("params").and_then(|p| p.get("context")?.get("session_id")))
            .and_then(|s| s.as_str())
            .unwrap_or("");
        let event_name = format!("agent:event:{session_id}");
        let _ = window.emit(&event_name, envelope);
    }
});
```

Frontend listens via `listen("agent:event:<session_id>", handler)` — same pattern Launchpad already uses for `fs-changed` and PTY output.

### Cancellation

`turn/interrupt` is launchpad-agent's own primitive. Frontend sends an envelope, runtime cancels the in-flight turn task, emits the appropriate notification. We don't need a `GitOpSlot`-style cancel layer — it's already inside the runtime.

---

## 3. Tauri commands

Two flavors. The thin wrapper is JSON-in/JSON-out; the typed wrappers are conveniences for common ops so the frontend doesn't hand-build envelopes everywhere.

### Thin wrapper

```
agent_send_envelope(envelope: serde_json::Value) -> Option<serde_json::Value>
```

Routes to the calling window's `handle_incoming`. Used for `events/subscribe`, `skills/list`, anything we don't want a typed wrapper for.

### Typed wrappers (build the envelope server-side)

```
agent_session_start(project_path, model?) -> SessionId
agent_send_message(session_id, text, mentions?) -> TurnId
agent_interrupt_turn(session_id, turn_id, reason?)
agent_approve(session_id, turn_id, approval_id, decision, scope) -> bool
agent_session_close(session_id)
agent_session_list() -> Vec<SessionSummary>
agent_session_resume(session_id) -> SessionId
agent_provider_config_get() -> ProviderConfig
agent_provider_config_set(cfg)
```

All commands take `tauri::Window` so they route to the right per-window connection.

---

## 4. Frontend — chat tab

### Files

```
src/
  agentchat.js      Chat tab factory, message store, composer, event listener
  agentmsg.js       Message-type renderers
  agentcomposer.js  Multiline input, @-mentions, slash commands, send/cancel
  styles.css        New section: "Agent chat tab"
```

### Tab shape

New tab type `"agent"` alongside `terminal | editor | settings | diff | rebase | merge`:

```js
{
  type: "agent",
  containerEl,
  sessionId,            // assigned after agent_session_start resolves
  turnId,               // active turn, null when idle
  messages: [],         // ordered list of message objects
  pendingApprovals: Map<approvalId, ApprovalRequest>,
  modelSlug,
  scrollPinned: true,
  unsubscribe,          // tauri event listener cleanup
}
```

Cmd+W → `agent_session_close` then standard tab teardown.

### Layout

```
┌─────────────────────────────────────────────┐
│ Header: model selector | session title | ⋯  │
├─────────────────────────────────────────────┤
│                                             │
│  Message list (scrollable, plain DOM v1)    │
│                                             │
│  - User bubble (right)                      │
│  - Assistant text (left, markdown)          │
│  - Tool-call card (collapsible)             │
│  - Approval card (sticky, scroll-into-view) │
│                                             │
├─────────────────────────────────────────────┤
│ Composer (multiline)                        │
│ @file mentions, /commands                   │
│ [Send] / [Cancel] (during turn)             │
└─────────────────────────────────────────────┘
```

### Streaming behavior

On `item.delta` for an assistant text item:
- If the last message in the list is the same `item_id`, append delta to its text node
- Otherwise create a new assistant text message bound to that `item_id`
- Re-render markdown for that bubble only (not the whole list)
- Auto-scroll to bottom **only if `scrollPinned`** (user scroll up unpins; scrolling back to bottom re-pins)

Markdown rendering: import `marked` (~25KB, MIT). Code blocks get static syntax highlighting via CodeMirror's `highlightTree` (already in deps), not a full editor instance.

---

## 5. Message taxonomy & tool cards

### Message types

| Type | Source | Renderer |
|------|--------|----------|
| `user` | local on send | Right-aligned bubble, plain text + mention chips |
| `assistant_text` | `item.delta` + `item` of kind `text` | Left, markdown-rendered |
| `thinking` | `item` of kind `thinking` | Collapsed by default, italic, gray |
| `tool_call` | `item` of kind `tool_call` | Card (see below) |
| `tool_result` | `item` of kind `tool_result` | Collapsible result block under its tool_call |
| `approval_required` | approval request notification | Sticky card with Approve/Deny + scope picker |
| `error` | error notification or RPC error | Red banner, retryable where applicable |
| `usage` | `turn.usage_updated` | Tiny footer line on the turn (input/output) |

### Tool-call card variants

Tool-call cards render based on tool name, with a generic fallback for unknown tools.

| Tool | Card | Affordances |
|------|------|-------------|
| `Read` | "Read `foo.rs:1-40`" | Click → opens editor tab at line |
| `Write` / `Edit` / `ApplyPatch` | Inline diff via `diffrender.js`, file path header | Approve / Reject if pending; "Open in editor" link after applied |
| `Bash` / `ShellExec` | Command in monospace, collapsible stdout/stderr | Copy stdout; long output truncated with "show more" |
| `Glob` / `Grep` | Query + collapsible result list | Click result → opens editor tab at line |
| `Todo` / `Plan` | Pretty-printed checklist / plan tree | — |
| `Question` | Inline question card with answer input | Submitted answer feeds back into turn |
| `WebFetch` / `WebSearch` | URL + collapsible result | Click URL → external browser |
| `Lsp` | Symbol + result | Click → editor tab at definition |
| `mcp__*` | Tool name + JSON params (collapsible) + result | No special affordances v1 |

Each card has consistent chrome: tool name, status icon (running / success / error / awaiting approval), elapsed time, expand/collapse toggle.

### Reusing `diffrender.js`

The diff card for `Write` / `Edit` / `ApplyPatch` should reuse the diff renderer that the diff/compare tab uses — file header, hunk list, line numbers, color coding. Pass it the synthetic patch from the tool call params.

---

## 6. Approval flow

launchpad-agent emits an approval request as a notification. Client must respond with `approval/respond` carrying `decision: Approve | Deny | Cancel` and `scope: Once | Turn | Session | PathPrefix | Host | Tool` (`crates/protocol/src/approval.rs`).

UX:
- Approval card renders inline at the position in the conversation where the request occurred
- Sticky-scrolls to the bottom of the visible viewport so it's never missed
- Buttons: **Approve** | **Deny** with a scope dropdown (default `Once`)
- Cancel button on the composer also rejects pending approvals (sends `Cancel` decision)
- On response, card collapses to a one-line "Approved (scope: Once)" summary

Persistent allow rules are stored by launchpad-agent's `ApprovalCache` (`crates/safety`). v1 has no Launchpad UI for editing them; users grant scope per-prompt via the dropdown. v2 settings panel can expose them.

---

## 7. Composer

- `<textarea>` with auto-resize, max 12 lines visible then scroll
- **Enter** sends, **Shift+Enter** newline. Setting to invert this for vim users in v1.5
- Disabled while a turn is running; the [Send] button becomes [Cancel] which calls `agent_interrupt_turn`
- **`@`** opens the file mention popup — backed by `search_files` (same index Cmd+P uses), inserts a chip representing an `InputItem::Mention { path, name }`
- **`/`** at start of line opens slash commands. The list is sourced from launchpad-agent's `skills/list` so any user/workspace skills automatically appear
- Drag-drop image into composer → `InputItem::LocalImage { path }` (vision models only; show inline thumbnail chip)

---

## 8. Cross-feature integration

The whole point of in-app over a terminal CLI is that the agent owns the workspace. Two mechanisms:

### Passive integration (existing event flow does the work)

| Agent action | Launchpad reaction |
|--------------|---------------------|
| Writes a file via `Write`/`Edit`/`ApplyPatch` | `notify` watcher fires `fs-changed`; file browser refreshes; open editor tab reloads (existing logic) |
| Edits a file currently open in editor with unsaved changes | Existing confirm dialog handles it |
| Creates a merge conflict | `getGitFileStatus` detects; opening the file routes through inline conflict editor / 3-pane merge tab |
| Runs `git commit` via `Bash` | Git panel poll picks it up within 3s |

No new event plumbing needed for these — the existing FSEvents watcher does the work. (The Foundation Audit will tell us if the watcher behavior is tight enough for agent burst-write profiles.)

### Active integration (Launchpad-native tools registered into `ToolRegistry`)

In `agent/tools.rs`, register Rust closures as additional `Tool` implementations. The agent calls them like any other tool, but they execute as same-process Rust:

| Tool name | Purpose |
|-----------|---------|
| `lp_open_in_editor(path, line?, col?)` | Opens an editor tab via Tauri event → `createEditorTab` |
| `lp_reveal_in_finder(path)` | Existing `reveal_in_finder` Tauri command |
| `lp_refresh_git_panel()` | Triggers git panel refresh (skip the 3s poll wait) |
| `lp_show_diff(from_ref, to_ref)` | Opens a diff tab via `createDiffTab` |
| `lp_open_merge_tab(file_path)` | Opens 3-pane merge tab for a conflicted file |

These give the agent a richer vocabulary than "write a file and hope the user notices." Tool-card UI for them is generic (tool name + params + status) — they don't need bespoke cards.

---

## 9. Settings & provider config

**Decision: Launchpad owns the basics.** Settings panel adds an "Agent" section:

- Provider preset dropdown (Anthropic / OpenAI / Gemini / OpenRouter / Groq / Together / Mistral / Ollama / **Z.ai coding plan**)
- API key input — stored at `~/.launchpad/agent-config.json` chmod 0o600 via `atomic_write_with_mode` (mirrors `project-env.json`)
- Default model slug
- Default approval policy (prompt for everything / pre-approve reads / pre-approve everything)

Per-model parameters, MCP server config, and skills stay in launchpad-agent's own config files (`~/.config/launchpad-agent/...` or wherever its `FileSystemConfigPathResolver` lands them). We don't try to re-skin every knob.

### Z.ai coding plan as a preset

Wired as an **OpenAI-compatible** provider (same provider type as Groq, Together, Mistral, Ollama). One-file PR upstream to `launchpad-agent/crates/core/src/provider_presets.rs`:

| Field | Value |
|-------|-------|
| Provider type | OpenAI-compatible |
| Base URL | `https://api.z.ai/api/coding/paas/v4` |
| Auth | `Authorization: Bearer <key>` |
| Env var | `Z_AI_API_KEY` |
| Default model | `glm-5.1` |
| Other available models | `glm-5-turbo`, `glm-5`, `glm-4.7`, `glm-4.7-flash`, `glm-4.7-flashx`, `glm-4.6`, `glm-4.5`, `glm-4.5-air`, `glm-4.5-x`, `glm-4.5-airx`, `glm-4.5-flash`, `glm-4-32b-0414-128k` |

**Why not the Anthropic-compatible endpoint** (`https://api.z.ai/api/anthropic`): Z.ai built that endpoint as a porting convenience for tools that hard-code Anthropic Messages API (Claude Code, etc.). We are not such a tool — launchpad-agent's OpenAI-compatible provider already speaks to non-OpenAI backends (Groq, Together, OpenRouter), and the coding endpoint is what Z.ai's own coding-plan docs treat as primary.

**Known divergences from strict OpenAI shape** (per Z.ai's chat-completions reference) — these are "things we'll discover when dogfooding," not blockers:

1. **`reasoning_content`** on assistant messages — Z.ai's chain-of-thought block. The OpenAI provider will drop it silently in v1. If we want the thinking display surfaced in the chat tab's `thinking` message type, follow-up PR to teach launchpad-agent's OpenAI provider to map `reasoning_content` → `thinking` item
2. **`finish_reason`** has extra values: `sensitive`, `model_context_window_exceeded`, `network_error`. OpenAI provider should treat unknown values as plain stops — verify it doesn't panic
3. **Tool-call shape** diverges from OpenAI's structure. Worth a smoke test against `Bash` and `Edit` tool calls before declaring the preset shippable
4. **`web_search`** field on responses — Z.ai-specific, ignored by OpenAI provider

Smoke test before merging the preset PR:
```bash
curl -X POST https://api.z.ai/api/coding/paas/v4/chat/completions \
  -H "Authorization: Bearer $Z_AI_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"glm-5.1","messages":[{"role":"user","content":"hi"}],"max_tokens":100}'
```

### Mapping Launchpad settings → launchpad-agent config

`agent/config.rs` builds an `AppConfig` (the same struct `crates/server/src/bootstrap.rs::run_server_process` loads from disk) from Launchpad's settings + `~/.launchpad/agent-config.json`. We do not write into launchpad-agent's own config dir from Launchpad — keeping the two config surfaces independent avoids ownership confusion.

---

## 10. Distribution

- launchpad-agent vendored as **git submodule** at `vendor/launchpad-agent`, pinned to a specific SHA
- `src-tauri/Cargo.toml` adds path deps: `lpa-core`, `lpa-server`, `lpa-provider`, `lpa-tools`, `lpa-safety`, `lpa-mcp`, `lpa-protocol`, `lpa-utils`
- `npx tauri build` compiles them as part of the normal build — no `build.rs`, no sidecar binary, no `bundle.externalBin`
- Bundle size goes up by however much the agent crates compile to (roughly 10-20MB depending on provider SDK choices). Acceptable cost.
- `vendor/launchpad-agent.version` file (SHA + date, human-readable) tracked in Launchpad's tree so reviewers see SHA bumps in PR diffs without expanding the submodule
- SHA bump cadence: **manual on demand**. Alpha velocity on the agent side means automatic bumps would break Launchpad mid-week.

---

## 11. Security & isolation

- API keys at `~/.launchpad/agent-config.json` chmod 0o600 — mirrors `project-env.json`. Uses existing `atomic_write_with_mode` helper
- launchpad-agent's safety layer (`crates/safety`) enforces the project-root sandbox for tool calls. Launchpad does not re-implement this — we trust it. If lpa's safety story changes, that's a SHA bump conversation
- Approval flow is the user's escape valve. Default policy: prompt for everything (writes, shell exec, network) on first session
- Tool-result rendering must escape HTML (markdown renderer handles assistant text; tool-result blocks render as `<pre>` so XSS via `Bash` output is not a concern)
- Multi-window: each window's connection is independent — sessions don't cross-contaminate, sender tasks don't leak across windows

---

## 12. Open decisions

Most decisions from earlier drafts are resolved. Remaining:

1. **Per-window `connection_id` lifetime on settings change.** If the user changes the provider in the settings panel, do we tear down all connections and rebuild, or hot-swap the provider in `ServerRuntimeDependencies`? Hot-swap requires upstream support; teardown loses in-flight turns. **I lean teardown for v1**, with a confirmation dialog if a turn is active.
2. **Where MCP server config UI lives.** Punt to launchpad-agent's own config files for v1 (no Launchpad UI). v2 might add a "MCP servers" subsection to the Agent settings.
3. **Persistence file location.** launchpad-agent's `RolloutStore::new(server_home)` decides. We can pass `~/.launchpad/agent/` as `server_home` to keep our state contained, or use launchpad-agent's default. **I lean Launchpad-controlled** so users see one config surface.

---

## 13. Phased PR plan

### PR1 — Foundations (no UI)
- Add `vendor/launchpad-agent` submodule pinned to a known-good SHA
- Add path deps in `src-tauri/Cargo.toml`
- New `src-tauri/src/agent/` module: `host.rs`, `bootstrap.rs` (mirrors `run_server_process`), `events.rs`, `mod.rs`
- One `ServerRuntime` per process, lazily constructed
- Per-window `register_connection` + sender task → Tauri events
- Tauri commands: `agent_send_envelope`, `agent_session_start`, `agent_send_message`, `agent_interrupt_turn`, `agent_approve`, `agent_session_close`
- Smoke test: from devtools, start a session, send "hello", see streaming events arrive in console

**Done when:** can drive a full text-only conversation from the JS console.

### PR2 — Chat tab shell + streaming text
- New tab type `"agent"`, factory in `agentchat.js`
- Layout (header / message list / composer)
- Render `user` and `assistant_text` only — no tool cards yet, no thinking, no approval
- Token-by-token streaming append, scroll-pinned auto-scroll
- Markdown rendering via `marked` + CodeMirror static highlight for code blocks
- Cmd+W → `agent_session_close`
- New-tab UI affordance for opening a chat tab

**Done when:** users can have a markdown conversation in the chat tab end-to-end.

### PR3 — Tool cards, thinking, approval
- Renderers for `thinking`, `tool_call`, `tool_result`, `approval_required`, `error`, `usage`
- Per-tool card variants (Read, Write/Edit/ApplyPatch with `diffrender.js`, Bash, Glob/Grep, Todo, Plan, Question, WebFetch, WebSearch, Lsp, generic MCP fallback)
- Approval flow: render card, scope picker, send `approval/respond`
- Cancel button on composer (mid-turn) → `agent_interrupt_turn`

**Done when:** user can complete a real coding task end-to-end — agent reads files, writes a file, gets approved, sees the diff applied.

### PR4 — Cross-feature integration + Launchpad-native tools
- @-file mentions in composer, backed by `search_files`
- Slash commands sourced from `skills/list` (`/clear`, `/model`, plus user/workspace skills)
- Register Launchpad-native tools (`lp_open_in_editor`, `lp_reveal_in_finder`, `lp_refresh_git_panel`, `lp_show_diff`, `lp_open_merge_tab`) into `ToolRegistry`
- "Open in editor" links from tool cards → `createEditorTab`
- Git-panel refresh nudge after agent runs `git commit` via `Bash`

**Done when:** the chat tab feels like part of Launchpad, not an embedded foreign app.

### PR5 — Settings, persistence, polish
- Settings panel "Agent" section (provider, API key, default model, default approval policy)
- API keys at `~/.launchpad/agent-config.json` chmod 0o600
- Z.ai coding plan as a preset (upstream PR + Launchpad UI)
- Session resume: `agent_session_list` + session switcher in chat tab header
- Multi-session per window — opening a second chat tab starts a new session, switcher in header lets user move between them
- Per-project default model (stored in project env or settings — TBD)

**Done when:** v1 is shippable. Closing the app and reopening picks up where the user left off.

---

## 14. Out of scope for v1

- Remote / multi-machine agent (would need WebSocket transport + auth)
- Voice input
- Inline-in-editor agent ("write the next 10 lines for me here") — its own design problem
- Auto-approval rule editor UI (v1 grants scope per-prompt)
- MCP server configuration UI (use launchpad-agent's config files directly)
- Cost tracking dashboard
- Sharing / exporting conversations
- Cross-window agent activity dock (covered in `agent-orchestration-spec.md` if/when written)

---

## 15. Risks

- **launchpad-agent is alpha.** Public surface can change. Mitigate: pin SHA, only bump on PR with intentional review. Talk to the `protocol` types as the contract; reach into `core` only where needed (e.g., `ToolRegistry` for native tools)
- **Compile time.** Adding ~12 crates to Launchpad's Rust build will increase incremental compile time noticeably. Consider `[profile.dev.package."lpa-*"] opt-level = 1` to keep dev iteration fast
- **Streaming markdown stutter.** Re-rendering on every delta is fine for short messages, gets noticeable on long ones. Fallback: render plain text during streaming, re-render as markdown on item complete
- **Tool card design is the highest-bandwidth UI.** Plan to iterate after PR3 ships — first version will not be the right version. Budget time for a polish PR after dogfooding
- **First-run UX.** User opens chat tab, has no API key. Settings panel must handle this with empty-state CTA, not a cryptic error
- **Foundation Audit findings may reorder this.** If the audit (per `specs/foundation-audit-spec.md`) flags must-fix items, those land as PR0 before PR1 starts here

---

## Status — 2026-05-09

Snapshot of what's working in `main` vs. what's still on the punch list.
Source of truth for "where are we" — update as PRs land.

### Architecture deltas from the original spec

- **In-tree port**, not submodule. `vendor/launchpad-agent` was rejected; crates were copied verbatim into `crates/` and the launchpad-agent `LICENSE` carried in as `crates/LPA_LICENSE`. Repo is now a Cargo workspace; `src-tauri` depends on `lpa-*` crates via `workspace = true`.
- **Transport: `WebSocket` placeholder, not `Stdio`.** Stdio always delivers every event to every connection, which would leak across windows. WebSocket gates delivery on explicit `events/subscribe` per session — `agent_session_start` auto-subscribes the new session before returning so the frontend can't miss the first delta. Adding `ClientTransportKind::InProcess` upstream is still the right long-term move; no longer urgent.
- **Sessions sidebar + session delete** were not in the original spec but landed because the persistence is already there (RolloutStore JSONL files) and the cost of exposing it is low. Delete is filesystem + hide-list (`~/.launchpad/agent-deleted-sessions.json`) since the runtime has no in-memory delete primitive — a clean upstream API would let us drop the hide-list.
- **Settings reachable from the projects picker** via an overlay (gear icon top-right). The agent config is global, so it must be configurable before any project is open.
- **Save & reload** rebuilds the runtime in place — no app restart required to swap providers. Active turns are cancelled when their connection drops; tabs reconnect on next message.

### What's shipping (✅ done)

**PR1 — Foundations**
- Workspace Cargo.toml with all 10 lpa-* crate path deps
- `src-tauri/src/agent/` — `host.rs`, `commands.rs`, `config.rs`
- One `ServerRuntime` per process, lazily built under a `Mutex<Option<...>>` so reload can drop it
- Per-window connection map keyed by Tauri window label; mpsc → `agent:event:<window_label>` Tauri events
- Tauri command surface: `agent_initialize`, `agent_connect`, `agent_disconnect`, `agent_send_envelope`, `agent_session_start` (auto-subscribes), `agent_send_message`, `agent_interrupt_turn`, `agent_approve`, `agent_subscribe_events`, `agent_session_list`, `agent_session_resume`, `agent_skills_list`, `agent_config_load`, `agent_config_save`, `agent_provider_presets`, `agent_reload`, `agent_session_delete`, `agent_session_deleted_ids`

**PR2 — Chat tab shell + streaming text**
- New tab type `"agent"`; ✦ icon, Cmd+I shortcut, toolbar button
- `agentchat.js` (factory + global event router), `agentmsg.js` (renderers), `agentcomposer.js` (composer)
- Streaming text deltas append plain; markdown rendered via `marked` on item completion (avoids per-delta re-render stutter)
- Scroll-pinned auto-scroll
- Conversation column max-width centered

**PR3 — Tool cards, thinking, approval**
- Generic tool cards for `tool_call`, `mcp_tool_call`, `command_execution`, `file_change`, `web_search`, `image_view`, `plan` — title extracted from `payload.input` (`bash · cargo check`, `read · path/...`)
- `tool_result` cards nested under their parent via `tool_use_id` index (the runtime emits `tool_use_id`, not `call_item_id`)
- Reasoning items render as collapsible "thinking" blocks
- Approval card with all six scopes (Once / Turn / Session / PathPrefix / Host / Tool); responds via `agent_approve`
- Cancel button mid-turn → `agent_interrupt_turn`
- Pulsing running dot, ✓ done / ✕ error glyph, three-dot "working…" indicator pinned at the bottom of the message stream
- Tool card body visibility managed by JS `MutationObserver` setting a `tool-card-empty` class — replaces a brittle `:has()` CSS rule that didn't work consistently in WebKit

**PR5 — Settings, persistence (partial)**
- "Agent" section in the settings panel: Provider dropdown with all 9 presets (Anthropic, OpenAI, Google, OpenRouter, Groq, Together, Mistral, Ollama, Custom), auto-fill base URL on provider change, model, API key
- Storage at `~/.launchpad/agent-config.json` chmod 0o600 via `atomic_write_with_mode`
- LPA_* env vars applied before runtime construction; **always overwrite** so a save-then-reload actually picks up new values
- **Save & reload** button — drops the runtime + all per-window connections, broadcasts a `launchpad:agent-reloaded` DOM event so open agent tabs reset their session state and start fresh on next message
- Settings reachable from the picker (gear icon, modal overlay)
- Session resume: full sidebar (☰ button → list of past sessions sorted by recency, click to resume, history replay via `SessionHistoryItem`)
- Session delete: × per row (hover-discoverable), in-app confirm modal, deletes rollout file + hide-list

**PR4 — Cross-feature integration (partial)**
- Slash command picker wired into composer: `/` at start of line → popup of skills from `agent_skills_list`, ↑/↓/Enter/Tab to insert. **Currently appears empty** — likely no skills registered in the runtime's skill catalog yet, not a code bug. To verify: drop a stub at `~/.lpagent/skills/` and reload.

### What's missing (❌ punch list)

**PR4 — Cross-feature integration**
- ❌ `@`-file mentions in composer (backed by `search_files`)
- ❌ Launchpad-native tools registered into `ToolRegistry`: `lp_open_in_editor`, `lp_show_diff`, `lp_open_merge_tab`, `lp_refresh_git_panel`, `lp_reveal_in_finder`. Wiring requires plumbing a `tauri::Window` handle into the tool closures so they can emit events back to the frontend (this is the "where do agent tools execute" gap from the original audit).
- ❌ "Open in editor" links from `Read` / `Grep` / `Lsp` tool cards (depends on the native tool above OR a frontend-only listener that intercepts tool-card clicks)
- ❌ Git panel refresh nudge after the agent runs `git commit` via `Bash`
- ❌ Drag-drop image into composer for vision models (needs `InputItem::LocalImage` plumbing)
- ⚠️ **Slash commands**: built but currently empty — needs verification + an empty-state CTA pointing at the skills folder

**PR5 — Polish**
- ❌ Z.ai coding plan as a named preset (Custom endpoint covers it manually). Upstream PR to `launchpad-agent/crates/core/src/provider_presets.rs` is still proposed in § 9 above.
- ❌ Default approval policy in settings (always prompts per-tool today; runtime supports `never` / `always` strings, no UI driver)
- ❌ Sandbox mode toggle (`workspace-write` / `read-only` / `permissive`) — runtime has the policies, no UI
- ❌ Per-project default model
- ❌ Multi-session per window with header switcher (sidebar exists; a quick switcher pinned in the chat header would be friendlier than re-opening the sidebar each time)
- ❌ Per-tool diff cards for Edit / Write / ApplyPatch using existing `diffrender.js`
- ❌ Cost / usage footer per turn (data arrives via `turn/usage/updated`, not yet displayed)
- ❌ First-run empty state — opening a chat tab before configuring a provider currently surfaces the runtime's bootstrap error verbatim instead of a friendly "Configure provider →" CTA

### Open issues seen during dogfooding

1. Old chat tabs frozen with pre-fix DOM structure don't always re-render with new card logic. Cosmetic — affects only sessions opened before the latest restart.
2. Tool card expand/collapse went through several iterations (auto-collapse, `:empty`, `:has()`) before landing on JS-managed `tool-card-empty`. Watch for regressions when changing the tool card chrome.
3. The `shutdown` method on `AgentState` still warns `dead_code` — needs to be wired to a Tauri window-close / app-exit hook so MCP supervisors get a chance to shut down cleanly.

### Re-plan — priority order from here

In order of "biggest workflow win for least effort":

1. **First-run empty state** — when no API key configured, in-chat CTA pointing at Settings instead of an opaque error. Cheap, big polish win.
2. **Verify slash commands end-to-end** with a test skill; add an empty-state hint pointing to the skills folder.
3. **`@`-file mentions** — small, high-impact for daily use.
4. **Default approval policy + sandbox mode** in settings — runtime supports both; just adds two dropdowns.
5. **Launchpad-native tools** (`lp_open_in_editor` etc.) — biggest "this is built for this app" feel; requires the `tauri::Window` handle plumbing.
6. **Per-tool diff cards** for Edit / Write — pipeline (`diffrender.js`) already exists.
7. **Cost / usage footer** — small, makes runs feel transparent.
8. Z.ai preset, image upload, multi-session header switcher — nice-to-haves.

Update this section as items land.
