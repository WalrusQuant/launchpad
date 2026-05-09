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
- **Tool registry** (`crates/tools`) with built-in `bash`, `read`, `write`, `apply_patch`, `glob`, `grep`, `todowrite`, `update_plan`, `question`, `webfetch`, `websearch`, `skill` (plus `task` and `lsp` defined-but-not-registered as stubs)
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
│  │   send: invoke('agent_send_message', { sessionId, text, ... })      │  │
│  │   recv: listen('agent:event:<window_label>', envelope => route())   │  │
│  │         (one global listener per window; routes by session_id to    │  │
│  │          the right tab via sessionTabs Map)                         │  │
│  └────────────────────────────────┬────────────────────────────────────┘  │
│                                   │ Tauri IPC                              │
│  Rust backend (src-tauri)         │                                        │
│  ┌────────────────────────────────▼────────────────────────────────────┐  │
│  │ agent::commands  →  AgentState (per-window connection)              │  │
│  │   handle_incoming(conn_id, json)  →  Option<json>  (response)       │  │
│  │   register_connection(WebSocket, sender) ─┐                         │  │
│  │                                           │ mpsc<serde_json::Value> │  │
│  │   forwarder_task: while let Some(v) = rx.recv().await {             │  │
│  │       app.emit("agent:event:<window_label>", v)                     │  │
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

### Transport label decision

`ClientTransportKind` (passed to `register_connection`) has variants for `Stdio` and `WebSocket` but no `InProcess`. We use **`WebSocket`** because Stdio always delivers every event to every connection — that would leak events across windows. WebSocket gates delivery on explicit `events/subscribe` per session, which matches our per-window isolation model. Adding `InProcess` upstream is still the right long-term move (the label is mildly misleading) but isn't urgent.

---

## 2. Rust integration glue

```
src-tauri/src/agent/
  mod.rs          Module exports
  host.rs         AgentState: per-process ServerRuntime, per-window connections,
                  mpsc → Tauri event fan-out, build_runtime mirrors lpa_server::
                  run_server_process minus run_listeners(), starter-skills seeder
  commands.rs     Tauri command surface (initialize / connect / send / interrupt /
                  approve / session list/resume/delete / skills_list / config /
                  reload)
  config.rs       AgentConfig schema + atomic-write + LPA_* env apply,
                  agent_is_configured pre-flight, agent_session_delete + hide-list
  native_tools.rs Launchpad-native tools (lp_open_in_editor, lp_show_diff,
                  lp_open_merge_tab, lp_refresh_git_panel, lp_reveal_in_finder)
                  emitting Tauri events back to frontend
src-tauri/starter-skills/<name>/SKILL.md
                  Bundled SKILL.md files (commit, review, explain, plan)
                  embedded via include_str! and seeded on first run
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

- **First chat tab anywhere** → `runtime` is built (mirrors `lpa_server::run_server_process` minus the listener loop); `load_persisted_sessions().await` runs once
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
  agentchat.js      Chat tab factory + global event router (per window),
                    sessions sidebar, lazy session creation, empty-state CTA
  agentmsg.js       Message + tool-card renderers (incl. specialized renderers
                    for apply_patch and write that use diffparse + diffrender)
  agentcomposer.js  Multiline input, @-mentions (search_files-backed),
                    slash commands (skill picker), send/cancel
  diffparse.js      Unified-diff parser (GNU + lpa "Apply Patch" envelope)
                    used by the apply_patch tool-card renderer
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

In `agent/native_tools.rs`, implement `lpa_tools::Tool` for each Launchpad-native tool. They run as same-process Rust and emit `launchpad:agent-tool-action` Tauri events with `{tool, payload}` so the frontend can route to the appropriate UI surface:

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

Wired as an **OpenAI-compatible** provider (same provider type as Groq, Together, Mistral, Ollama). Lives in-tree at `crates/core/src/provider_presets.rs::PRESETS` (id `zai_coding`):

| Field | Value |
|-------|-------|
| Provider type | OpenAI-compatible |
| Base URL | `https://api.z.ai/api/coding/paas/v4` |
| Auth | `Authorization: Bearer <key>` |
| Env var | `Z_AI_API_KEY` |
| Default model | `glm-4.6` (Z.ai's coding-plan flagship as of 2026-05) |
| Other available models | `glm-4.5`, `glm-4.5-air` (lighter), `glm-4.5-flash` (fastest) |

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
  -d '{"model":"glm-4.6","messages":[{"role":"user","content":"hi"}],"max_tokens":100}'
```

### Mapping Launchpad settings → launchpad-agent config

`agent/config.rs` builds an `AppConfig` (the same struct `crates/server/src/bootstrap.rs::run_server_process` loads from disk) from Launchpad's settings + `~/.launchpad/agent-config.json`. We do not write into launchpad-agent's own config dir from Launchpad — keeping the two config surfaces independent avoids ownership confusion.

---

## 10. Distribution

- launchpad-agent crates are **copied in-tree** under `crates/` (full source, not a submodule). The submodule design from earlier drafts was rejected — see the Architecture Deltas section below.
- `Cargo.toml` at the repo root defines the workspace; `src-tauri/Cargo.toml` pulls in `lpa-core`, `lpa-server`, `lpa-provider`, `lpa-tools`, `lpa-safety`, `lpa-mcp`, `lpa-protocol`, `lpa-utils`, `lpa-client`, `lpa-tasks` via `workspace = true` path deps
- `crates/LPA_LICENSE` carries the launchpad-agent Apache-2.0 license verbatim
- `npx tauri build` compiles them as part of the normal build — no `build.rs`, no sidecar binary, no `bundle.externalBin`
- Bundle size goes up by however much the agent crates compile to (roughly 10-20MB depending on provider SDK choices). Acceptable cost.
- **Sync cadence: manual.** When upstream launchpad-agent ships something we want, copy the relevant files in by hand and run the test suite. Alpha velocity on the agent side means an automatic sync would break Launchpad mid-week. We're free to make Launchpad-specific edits to the vendored crates (e.g. the `default_base_instructions.txt` rewrite, the Z.ai preset entry) as long as they're documented and reviewable.

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
- Copy launchpad-agent crates into `crates/` (in-tree port, not submodule); add `crates/LPA_LICENSE`
- Add `Cargo.toml` workspace at repo root; add path deps in `src-tauri/Cargo.toml`
- New `src-tauri/src/agent/` module: `mod.rs`, `host.rs` (mirrors `run_server_process`), `commands.rs`, `config.rs`
- One `ServerRuntime` per process, lazily constructed
- Per-window `register_connection` + sender task → Tauri events on `agent:event:<window_label>`
- Tauri commands: `agent_initialize`, `agent_connect`, `agent_disconnect`, `agent_send_envelope`, `agent_session_start` (auto-subscribes), `agent_send_message`, `agent_interrupt_turn`, `agent_approve`, `agent_subscribe_events`
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

## Status — 2026-05-09 (regroup after live dogfooding)

Snapshot of what's working in `main` vs. what's still on the punch list.
Source of truth for "where are we" — update as PRs land.

### Architecture deltas from the original spec

- **In-tree port**, not submodule. `vendor/launchpad-agent` was rejected; crates were copied verbatim into `crates/` and the launchpad-agent `LICENSE` carried in as `crates/LPA_LICENSE`. Repo is now a Cargo workspace; `src-tauri` depends on `lpa-*` crates via `workspace = true`.
- **Transport: `WebSocket` placeholder, not `Stdio`.** Stdio always delivers every event to every connection, which would leak across windows. WebSocket gates delivery on explicit `events/subscribe` per session — `agent_session_start` auto-subscribes the new session before returning so the frontend can't miss the first delta. Adding `ClientTransportKind::InProcess` upstream is still the right long-term move; no longer urgent.
- **Sessions sidebar + session delete** were not in the original spec but landed because the persistence is already there (RolloutStore JSONL files) and the cost of exposing it is low. Delete is filesystem + hide-list (`~/.launchpad/agent-deleted-sessions.json`) since the runtime has no in-memory delete primitive — a clean upstream API would let us drop the hide-list.
- **Settings reachable from the projects picker** via an overlay (gear icon top-right). The agent config is global, so it must be configurable before any project is open.
- **Save & reload** rebuilds the runtime in place — no app restart required to swap providers. Active turns are cancelled when their connection drops; tabs reconnect on next message. Verified live with K2.6 → Z.ai/GLM-5.1 mid-conversation switch.
- **Sessions are lazy.** Opening a chat tab no longer calls `agent_session_start`; the runtime sees nothing until the user actually sends a message. Avoids cluttering the sessions list with empty "untitled" sessions when users open a tab and walk away.
- **Skills inject SKILL.md content for real.** Picking a slash command sends a structured `InputItem::Skill { id }` (not just `/foo` text), so the runtime's `resolve_input_items` resolves the catalog and prepends `<skill>...</skill>` blocks ahead of the user's text. Four starter skills (`/commit`, `/review`, `/explain`, `/plan`) are bundled in `src-tauri/starter-skills/<name>/SKILL.md` and seeded into `~/.lpagent/skills/` on first run if the directory is missing — so the slash menu has something the moment a user opens a chat tab.
- **System prompt rewritten for general use.** The original `crates/core/default_base_instructions.txt` was inherited from OpenAI Codex CLI: a 12KB single-line blob with literal `\n` escape sequences, references to `multi_tool_use.parallel` (an OpenAI-only tool that isn't registered here), and instructions to "send messages to the `commentary` and `final` channels" (codex-cli concepts that don't exist in this runtime). Under GLM-5.1 this caused observable retry loops — the model would write a file, fail to parse the success confirmation through the codex-shaped expectations, and call `write` again. Replaced with a 6.4KB model-agnostic prompt: real newlines, accurate tool list (including the `lp_*` native tools), explicit "trust tool results, do not retry to verify" rule, no fake channels, no frontend-design noise. Rebuilt prompt loads via the same `include_str!` path so the change rides whatever Cargo build picks it up.

### What's shipping (✅ done)

**PR1 — Foundations**
- Workspace Cargo.toml with all 10 lpa-* crate path deps
- `src-tauri/src/agent/` — `host.rs`, `commands.rs`, `config.rs`, `native_tools.rs`
- One `ServerRuntime` per process, lazily built under a `Mutex<Option<...>>` so reload can drop it
- Per-window connection map keyed by Tauri window label; mpsc → `agent:event:<window_label>` Tauri events
- Tauri command surface: `agent_initialize`, `agent_connect`, `agent_disconnect`, `agent_send_envelope`, `agent_session_start` (auto-subscribes), `agent_send_message` (accepts `mentions` + `skills`), `agent_interrupt_turn`, `agent_approve`, `agent_subscribe_events`, `agent_session_list`, `agent_session_resume`, `agent_skills_list` (forwards `cwd`), `agent_config_load`, `agent_config_save`, `agent_is_configured`, `agent_provider_presets`, `agent_reload`, `agent_session_delete`, `agent_session_deleted_ids`

**PR2 — Chat tab shell + streaming text**
- New tab type `"agent"`; ✦ icon, Cmd+I shortcut, toolbar button
- `agentchat.js` (factory + global event router), `agentmsg.js` (renderers), `agentcomposer.js` (composer)
- Streaming text deltas append plain; markdown rendered via `marked` on item completion (avoids per-delta re-render stutter)
- Scroll-pinned auto-scroll
- Conversation column max-width centered
- First-run empty state — when `agent_is_configured` returns false, the chat tab renders an in-tab CTA pointing at Settings instead of bubbling the runtime's bootstrap error. Composer is disabled while in empty state. Listens for `launchpad:agent-reloaded` to recover into a real session in place after Save & reload.

**PR3 — Tool cards, thinking, approval**
- Generic tool cards for `tool_call`, `mcp_tool_call`, `command_execution`, `file_change`, `web_search`, `image_view`, `plan` — title extracted from `payload.input` (`bash · cargo check`, `read · path/...`)
- `tool_result` cards nested under their parent via `tool_use_id` index (the runtime emits `tool_use_id`, not `call_item_id`)
- Reasoning items render as collapsible "thinking" blocks
- Approval card with all six scopes (Once / Turn / Session / PathPrefix / Host / Tool); responds via `agent_approve`
- Cancel button mid-turn → `agent_interrupt_turn`
- Pulsing running dot, ✓ done / ✕ error glyph, three-dot "working…" indicator pinned at the bottom of the message stream
- Tool card body visibility managed by JS `MutationObserver` setting a `tool-card-empty` class — replaces a brittle `:has()` CSS rule that didn't work consistently in WebKit
- **Per-tool diff cards** for `apply_patch` (parses to structured hunks via `src/diffparse.js`, then `buildFileDiffSection` from the git panel's renderer) and `write` (path + line/char count + capped content preview).
- **Cost / usage footer** — tiny muted line per turn showing `↑ in · ↓ out · cache hit/write · session totals`, updated in place per `turn/usage/updated`.

**PR4 — Cross-feature integration**
- **Slash command picker** end-to-end: workspace skill discovery fixed (`host.rs` was discarding `workspace_roots`); `agent_skills_list` forwards `cwd` so per-project skills work; empty-state row in the popup with hints for `~/.lpagent/skills/<name>/SKILL.md` and `<project>/skills/<name>/SKILL.md`. Picking a skill sends a structured `InputItem::Skill` so the runtime injects the SKILL.md body. Four starter skills bundled + seeded on first run.
- **`@`-file mentions** in composer, backed by `search_files`. Debounced (120ms) + stale-token guard. Submit only forwards mentions whose `@<rel-path>` token is still in the final text. Structured `InputItem::Mention` items forwarded.
- **Launchpad-native tools** registered into `ToolRegistry`: `lp_open_in_editor`, `lp_show_diff`, `lp_open_merge_tab`, `lp_refresh_git_panel`, `lp_reveal_in_finder`. AppHandle plumbed into `build_runtime`; each tool emits a `launchpad:agent-tool-action` event that `main.js` routes to the right surface. All five marked `is_read_only` so the orchestrator skips approval round-trips.

**PR5 — Settings, persistence, polish**
- "Agent" section in the settings panel: Provider dropdown with all 10 presets (Anthropic, OpenAI, Google, OpenRouter, Groq, Together, Mistral, Ollama, **Z.ai (coding plan)**, Custom), auto-fill base URL on provider change, model, API key
- Storage at `~/.launchpad/agent-config.json` chmod 0o600 via `atomic_write_with_mode`
- LPA_* env vars applied before runtime construction; **always overwrite** so a save-then-reload actually picks up new values
- **Save & reload** button — drops the runtime + all per-window connections, broadcasts a `launchpad:agent-reloaded` DOM event so open agent tabs reset their session state and start fresh on next message. Verified live mid-conversation.
- Settings reachable from the picker (gear icon, modal overlay)
- Session resume: full sidebar (☰ button → list of past sessions sorted by recency, click to resume, history replay via `SessionHistoryItem`)
- Session delete: × per row (hover-discoverable), in-app confirm modal, deletes rollout file + hide-list
- "Open Settings →" deep-link from the empty-state CTA scrolls to `#agent-section` and focuses the API key input

### What's missing (❌ punch list, post-regroup)

#### Newly observed during dogfooding (priority order)

1. **Provider switch doesn't auto-fill the Model field.** When the user picks a new provider preset, the base URL is reset but the model field stays as whatever was there before — caused a real Z.ai 400 ("Unknown Model") in dogfooding. Fix: when provider changes, populate the model field with a sensible default for that preset (add `default_model: Option<&'static str>` to `ProviderPreset`, fallback to first entry of preset's known models).
2. **Spec listed speculative Z.ai model codes.** § 9 named `glm-5.1` as the default and listed several `glm-5*` variants — those are not real Z.ai coding-plan models. Verified-good codes are `glm-4.6` (flagship), `glm-4.5`, `glm-4.5-air`, `glm-4.5-flash`. Update § 9 and the preset's `default_model` accordingly.
3. **Bash / read / grep / glob tool cards dump JSON args** instead of pretty rendering. We have specialized renderers for `apply_patch` and `write`; bash et al. fall through to the generic `appendArgs` JSON dump. Fix: add specialized renderers (e.g. `bash` → `$ <command>` + description; `read` → `path:line-line`; `grep`/`glob` → `<pattern> in <path>`).
4. **Empty `tool_result` slots under tool_call cards.** Cards show DONE but the result body is empty in dogfooding under Z.ai. Two possibilities: the OpenAI-compat runtime path isn't emitting separate `tool_result` items the way Anthropic does, OR `tool_use_id` field naming differs and our index lookup misses. Diagnose first (temp `console.log` on incoming envelope methods + payload field shapes), then fix renderer or push fix into the lpa OpenAI provider. **Re-verify after the system-prompt rewrite** — the loop that surfaced this might have been masking the issue, or might have BEEN the issue.
5. **Old chat tabs frozen with pre-fix DOM** don't re-render with new card logic — cosmetic, affects only sessions opened before the latest restart.
6. **`AgentState::shutdown` is `dead_code`** — needs wiring to a Tauri window-close / app-exit hook so MCP supervisors get a chance to shut down cleanly.

#### Original punch list, still pending (not blocked)

- **Drag-drop image upload** for vision models (needs `InputItem::LocalImage` plumbing + Tauri file-drop handling).
- **Multi-session per window with header switcher** — sidebar exists; a quick switcher pinned in the chat header would be friendlier than re-opening the sidebar each time.
- **"Open in editor" links from `Read` / `Grep` / `Lsp` tool cards** — the `lp_open_in_editor` native tool covers the agent-driven path; click-to-open from the tool-card UI is still a separate frontend-only listener task.
- **Git panel refresh nudge after `git commit` via `Bash`** — `lp_refresh_git_panel` exists; the agent has to choose to call it. A passive "watch for `git commit` in Bash output and auto-refresh" hook is still on the punch list.
- **Per-project default model** — config schema ready, no UI driver yet.

#### Blocked on vendored-crate runtime work

- ⚠️ **Default approval policy in settings** — `crates/server/src/execution.rs:90` hardcodes `SessionConfig::default()` → `AutoApprove` with no override path; `SessionStartParams` doesn't carry `permission_mode`. The `RuleBasedPolicy` exists in `crates/safety/src/legacy_permissions.rs` but is never reached from a non-default config. Earlier "runtime supports `never`/`always` strings" note was wrong. Fix path: add `permission_mode` to `SessionStartParams` (or read `LPA_PERMISSION_MODE` in `execution.rs:90`), then wire UI dropdown.
- ⚠️ **Sandbox mode toggle** — `SandboxPolicyRecord` exists in `lpa_safety` but isn't actually enforced anywhere — only persisted as a string in `crates/core/src/conversation/records.rs:42`. A UI today would be vaporware. Fix path: implement enforcement in the orchestrator's `is_read_only` / write paths first.

### Open issues seen during dogfooding (carried forward)

1. Old chat tabs frozen with pre-fix DOM structure don't always re-render with new card logic. Cosmetic — affects only sessions opened before the latest restart.
2. Tool card expand/collapse went through several iterations (auto-collapse, `:empty`, `:has()`) before landing on JS-managed `tool-card-empty`. Watch for regressions when changing the tool card chrome.
3. The `shutdown` method on `AgentState` still warns `dead_code` — needs to be wired to a Tauri window-close / app-exit hook so MCP supervisors get a chance to shut down cleanly.
4. **Z.ai-specific shape divergence** observed in dogfooding (issue #4 above) — `tool_result` items not visible in the chat UI when the conversation runs against the Z.ai OpenAI-compatible endpoint. Suspect OpenAI provider's tool-call/tool-result handling needs verification against Z.ai's response shape (per § 9 known divergences). Untested: whether Anthropic / Claude Code shows results correctly with the same chat-tab build.

### Lessons from this session (so we don't relearn them)

- **Vendored prompts need a sniff test before shipping.** The default base instructions were lifted wholesale from OpenAI Codex CLI without anyone reading them — they referenced fake tools and non-existent IPC channels, costing real model behavior under non-OpenAI providers. Going forward, anything pulled from another agent CLI (prompts, tool schemas, system messages) gets a line-by-line read against this runtime's actual surface area before it lands.
- **"Trust tool results" is a load-bearing prompt rule.** Without it, weaker models retry successful operations to "verify". The new prompt makes this explicit; if a future model still loops on writes, the prompt isn't strong enough — that's a prompt fix, not a UI fix.
- **`include_str!` resources require a rebuild, not a hot reload.** When debugging future "I changed X but it didn't take", check whether X is `include_str!`-ed.
- **Slow first-token latency is the model, not us.** GLM-5.1 with 26K input tokens: 5–15s before any deltas. Don't chase phantom streaming bugs without checking the model's own latency profile first.

### Re-plan — priority order from here

Today's session shipped 13 items (the original 8-item re-plan list plus 5 gaps discovered during the work, including the system-prompt rewrite). Next pass, in order of biggest workflow win for least effort:

1. **Re-verify the `tool_result` rendering issue** (was issue #4) after the system-prompt rewrite. The retry loop that surfaced this might have BEEN the issue — if so, no further fix needed. If results are still missing, fall back to the original diagnostic plan (temp `console.log` on incoming envelope methods + payload field shapes).
2. **Provider switch auto-fills model field + correct Z.ai model codes** (issues #1 + #2 above) — 5-minute fix, eliminates the next user's first-run friction.
3. **Specialized tool-card renderers for `bash` / `read` / `grep` / `glob`** (issue #3) — most visible polish gap on every conversation.
4. **Drag-drop image upload** for vision models — opens up GPT-4V, Claude Sonnet vision, Gemini.
5. **Multi-session header switcher** — friendlier than re-opening the sidebar each time.
6. **Click-to-open from existing Read/Grep tool cards** — frontend-only listener, mirrors the agent-driven `lp_open_in_editor` path.
7. **Wire `AgentState::shutdown` to app-exit hook** — clears the `dead_code` warning and gives MCP supervisors a clean exit.
8. **Approval policy / sandbox mode** — only after the vendored-crate runtime work lands. See "Blocked" section.

Update this section as items land.
