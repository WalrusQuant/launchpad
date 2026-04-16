# Launchpad Built-in Coding Agent — Technical Spec

## Context

Launchpad is a terminal-first Tauri v2 desktop workspace. The user wants a built-in coding agent — not a wrapper around an external CLI, but a native feature that deeply integrates with the file browser, editor tabs, git panel, and terminal. We're using the architecture of [claw-code-rust](https://github.com/7df-lab/claw-code-rust) (MIT, Rust coding agent) as a reference for the hard parts: agentic loop, patch application, provider abstraction, token management.

**What makes this different from standalone CLI agents**: The agent owns the environment. It can open files in editor tabs, trigger live file browser updates, stage/commit through the git panel, and show diffs inline. No standalone CLI can do this.

### Relationship to Project Model

This spec is designed to work with Launchpad's project-scoped architecture (see `specs/project-model-spec.md`). Key integration points:

1. **Agent cwd = project root**. Today `start_agent_session(cwd)` takes an explicit path. When the project model lands, the project root becomes the source of truth — the frontend passes it automatically. The agent treats this directory as its scope boundary.

2. **Project-scoped file access**. `ToolContext` carries a `project_root` field. Tools resolve relative paths against it. Writes outside the project root are **blocked** (tool returns an error). Reads outside get a **warning** in the tool output but are allowed (the agent may need to read docs, dependencies, etc.).

3. **Multi-window isolation**. Each project window creates its own agent sessions. `AgentState` is shared at the Rust level (single `HashMap<String, AgentSession>`) but sessions are keyed by UUID — no collision between windows. Each window's frontend only listens for events matching its own session IDs.

4. **Coexistence with CLI agents**. The built-in agent and external CLI agents (Claude Code, Aider, etc.) run side by side. A user might have a terminal tab running Aider and an agent tab using the built-in agent in the same project window. They don't interfere — different tab types, different processes.

5. **Project picker integration** (future). When opening a project, Launchpad could restore the last agent tab state (was an agent tab open? which model?). Out of scope for agent v1, but the session model supports it.

---

## 1. Rust Backend — Agent Module

### New Files

```
src-tauri/src/
  agent/
    mod.rs           — AgentState, Tauri commands, session management
    loop_runner.rs   — Core agentic loop (stream → tools → recurse)
    provider.rs      — Anthropic + OpenAI streaming abstraction
    tools.rs         — Tool trait, registry, built-in tool implementations
    patch.rs         — Patch parser + 3-tier fuzzy hunk matching
    context.rs       — Conversation history, system prompt, token budget
```

### New Dependencies (Cargo.toml)

```toml
reqwest = { version = "0.12", features = ["json", "stream"] }
tokio = { version = "1", features = ["full"] }
tokio-stream = "0.1"
futures = "0.3"
uuid = { version = "1", features = ["v4"] }
async-trait = "0.1"
keyring = "3"   # OS keychain for API key storage
```

### 1.1 State & Tauri Commands (agent/mod.rs)

```rust
pub struct AgentSession {
    pub id: String,                    // uuid
    pub conversation: Conversation,
    pub status: AgentStatus,
    pub cancel: CancellationToken,     // tokio_util
    pub project_root: String,          // project directory — all tools scoped to this
    pub provider_id: String,           // "anthropic" | "openai"
    pub model: String,
}

pub enum AgentStatus {
    Idle,
    Thinking,
    Streaming,
    ExecutingTool { tool_name: String },
    Error { message: String },
    Cancelled,
}

pub struct AgentState {
    pub sessions: Arc<Mutex<HashMap<String, AgentSession>>>,
}
```

**4 Tauri commands:**

| Command | Args | Returns | Notes |
|---------|------|---------|-------|
| `start_agent_session` | `project_root, provider?, model?` | `session_id` | Creates session scoped to project root |
| `send_agent_message` | `session_id, message` | `()` | Spawns tokio task running the loop; frontend follows via events |
| `cancel_agent` | `session_id` | `()` | Triggers CancellationToken |
| `get_agent_status` | `session_id` | `AgentStatus` | Polling fallback if event is missed |

**3 API key commands** (also in mod.rs or lib.rs):

| Command | Args | Returns |
|---------|------|---------|
| `set_api_key` | `provider, key` | `()` |
| `get_api_key` | `provider` | `Option<String>` |
| `delete_api_key` | `provider` | `()` |

Keys stored via `keyring` crate → macOS Keychain. Never written to config.json.

### 1.2 Conversation & Token Budget (agent/context.rs)

```rust
pub enum ContentBlock {
    Text(String),
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String, is_error: bool },
}

pub struct Message {
    pub role: Role,  // User | Assistant
    pub content: Vec<ContentBlock>,
}

pub struct Conversation {
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub total_input_tokens: usize,
    pub total_output_tokens: usize,
}

pub struct TokenBudget {
    pub context_window: usize,      // e.g. 200_000
    pub max_output_tokens: usize,   // e.g. 8_192
    pub compaction_threshold: f64,  // 0.90
}
```

**Compaction**: When estimated tokens > 90% of `context_window - max_output_tokens`, drop oldest message pairs keeping system prompt + last 4 pairs. Token estimation: `text.len() / 4` (no tokenizer dependency; actual counts from API used for display).

**System prompt builder**: Injects cwd, available tools (with JSON schemas), top-level file listing, current git branch/status.

### 1.3 Provider Abstraction (agent/provider.rs)

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> ProviderCapabilities;
    async fn stream_response(
        &self,
        messages: Vec<serde_json::Value>,
        system: &str,
        tools: &[ToolSchema],
        max_tokens: usize,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>>;
}

pub enum StreamEvent {
    TextDelta(String),
    ToolUseStart { id: String, name: String },
    ToolUseInputDelta(String),
    ToolUseEnd,
    MessageStart { input_tokens: usize },
    MessageDelta { output_tokens: usize, stop_reason: Option<StopReason> },
    MessageEnd,
}

pub enum StopReason { EndTurn, ToolUse, MaxTokens }
```

**AnthropicProvider**: POST `https://api.anthropic.com/v1/messages` with `stream: true`. Parse SSE events (`message_start`, `content_block_start/delta/stop`, `message_delta`, `message_stop`). Headers: `x-api-key`, `anthropic-version: 2023-06-01`.

**OpenAIProvider**: POST `https://api.openai.com/v1/chat/completions` with `stream: true`. Parse `data: {...}` SSE lines, map `choices[0].delta` to StreamEvent.

Both use `reqwest::Client` with connection pooling. Created via:
```rust
pub fn create_provider(id: &str, api_key: &str, model: &str) -> Box<dyn Provider>;
```

### 1.4 Tool System (agent/tools.rs)

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> serde_json::Value;
    fn is_read_only(&self) -> bool;
    async fn execute(&self, ctx: &ToolContext, input: serde_json::Value) -> ToolOutput;
}

pub struct ToolContext {
    pub project_root: String,   // project boundary — writes outside are blocked
    pub cwd: String,            // working directory (same as project_root for now)
}
pub struct ToolOutput { pub content: String, pub is_error: bool }
pub struct ToolRegistry { tools: Vec<Box<dyn Tool>> }
```

**8 built-in tools**, wrapping existing Launchpad backend functions:

| Tool | Wraps | Read-only | Description |
|------|-------|-----------|-------------|
| `read_file` | `read_file_preview` logic | yes | Read file contents (warns if outside project root) |
| `write_file` | `write_file` logic | no | Write/create a file (blocked if outside project root) |
| `list_directory` | `read_directory` logic | yes | List directory contents (warns if outside project root) |
| `search_files` | `search_files` logic | yes | Fuzzy file search (rooted at project root) |
| `bash` | `tokio::process::Command` | no | Run shell commands, cwd = project root |
| `apply_patch` | New (patch.rs) | no | Multi-file patches (paths relative to project root, blocked outside) |
| `git_status` | `get_git_status` logic | yes | Branch, staged/unstaged (always project root repo) |
| `git_commit` | `git_stage_all` + `git_commit` logic | no | Stage all + commit (always project root repo) |

**Key refactor**: Extract inner logic from existing `#[tauri::command]` functions so both the command and the tool can call the same code:
```rust
// Before: logic inside the command
// After:
pub fn read_file_preview_inner(path: &str, max_bytes: Option<usize>) -> Result<String, String> { ... }

#[tauri::command]
fn read_file_preview(path: String, max_bytes: Option<usize>) -> Result<String, String> {
    read_file_preview_inner(&path, max_bytes)
}
```

### 1.5 Patch Application (agent/patch.rs)

Custom LLM-friendly patch format:

```
*** Begin Patch
*** Update File: src/main.js
@@@ search_block
fn old_function() {
    old_code();
}
@@@
@@@ replace_block
fn old_function() {
    new_code();
}
@@@
*** Add File: src/new.rs
+line 1
+line 2
*** Delete File: src/deprecated.rs
*** End Patch
```

**3-tier fuzzy hunk matching** (from claw-code-rust):
1. Exact substring match
2. Trimmed whitespace per line
3. Normalized whitespace (collapse runs to single space)

Returns structured result with per-file additions/deletions counts.

### 1.6 Agentic Loop (agent/loop_runner.rs)

```rust
pub async fn run_agent_loop(
    session_id: String,
    sessions: Arc<Mutex<HashMap<String, AgentSession>>>,
    app: tauri::AppHandle,
    registry: Arc<ToolRegistry>,
) -> Result<(), AgentError>
```

**Loop flow:**
1. Check cancellation token
2. Check token budget → compact if needed
3. Build API request from conversation
4. Stream response, emitting `agent-stream` events for each delta
5. Collect text + tool calls from stream
6. Append assistant message to conversation
7. If `stop_reason == ToolUse`:
   - Read-only tools → execute in parallel (`futures::join_all`)
   - Mutating tools → execute sequentially
   - Emit `agent-stream` events for each tool start/result
   - Append tool results to conversation
   - **GOTO step 1**
8. If `stop_reason == EndTurn` → emit `agent-complete`, set status Idle
9. If `stop_reason == MaxTokens` → inject continuation prompt, GOTO step 1

**Guards**: Max 25 turns per invocation. Warning at turn 20. Cancellation checked before each turn and between tool executions.

---

## 2. Tauri Events (Backend → Frontend)

### agent-stream
```js
{
  session_id: string,
  event_type: "text_delta" | "tool_start" | "tool_result" | "status_change" | "usage_update",
  data: {
    text: string,           // text_delta
    tool_use_id: string,    // tool_start, tool_result
    tool_name: string,      // tool_start
    tool_input: object,     // tool_start
    content: string,        // tool_result
    is_error: boolean,      // tool_result
    status: string,         // status_change
    input_tokens: number,   // usage_update
    output_tokens: number,  // usage_update
  }
}
```

### agent-error
```js
{ session_id: string, error: string, recoverable: boolean }
```

### agent-complete
```js
{ session_id: string, turn_count: number, total_input_tokens: number, total_output_tokens: number }
```

---

## 3. Frontend — Agent Tab

### New File: src/agent.js

Exports `createAgentUI(containerEl, tab)` — builds the DOM imperatively (same pattern as settingspanel.js/gitpanel.js).

### Tab Structure

```js
{
  type: "agent",
  containerEl: HTMLElement,
  sessionId: string,
  status: "idle" | "thinking" | "streaming" | "executing_tool" | "error",
  messages: [],         // render state
  inputEl: HTMLTextAreaElement,
  messagesEl: HTMLElement,
  tokenEl: HTMLElement,
}
```

### UI Layout

```
div.agent-instance                      (flex column, fills parent)
  div.agent-header                      (36px bar)
    span.agent-model-label              ("Claude Sonnet 4")
    span.agent-token-count              ("1.2k / 200k tokens")
  div.agent-messages                    (flex 1, scrollable)
    div.agent-msg.user                  (user message bubble)
    div.agent-msg.assistant             (streaming text + tool blocks)
      div.agent-text                    (rendered markdown)
      div.agent-tool-call               (collapsible)
        div.agent-tool-header           (icon + name + path + spinner/check)
        div.agent-tool-body             (collapsed: input JSON + output text)
  div.agent-input-area                  (bottom bar)
    textarea.agent-input                (auto-grow, max 200px)
    button.agent-send-btn               (Cmd+Enter sends)
    button.agent-cancel-btn             (visible when active)
```

### Integration Points in main.js

- `createAgentTab()` — allocates uiTabId, calls `start_agent_session` with project root (or current path), builds UI, registers in `tabs` Map
- `renderTabBar()` — handle `type === "agent"` with icon and label
- `switchTab()` — focus agent input when switching to agent tab
- `closeTab()` — cancel active session, clean up listeners
- Event listeners for `agent-stream`, `agent-error`, `agent-complete` — route by session_id to correct tab
- After mutating tool results → `refreshFileBrowser()` + `fetchGitStatus()` + `refreshPanel()`

### Keyboard Shortcuts

| Shortcut | Action |
|----------|--------|
| Cmd+L | New agent tab |
| Cmd+Enter | Send message (when agent input focused) |
| Escape | Cancel running agent |

---

## 4. Settings

### New Keys in settings.js

```js
agentProvider: "anthropic",
agentModel: "claude-sonnet-4-20250514",
agentMaxTurns: 25,
```

### Settings Panel Section (settingspanel.js)

New "Agent" section with:
- Provider dropdown (Anthropic / OpenAI)
- API key input (password field, stored via `set_api_key` → OS keychain, masked display)
- Model dropdown (options change per provider)
- Max turns slider

---

## 5. Phased Build Plan

### Phase 1: Text-only conversation
**Goal**: Open agent tab, send message, get streaming response from Claude. No tools.

**Create:**
- `src-tauri/src/agent/mod.rs` — state, 4 commands + 3 key commands
- `src-tauri/src/agent/context.rs` — Conversation, Message, ContentBlock, budget
- `src-tauri/src/agent/provider.rs` — AnthropicProvider only
- `src-tauri/src/agent/loop_runner.rs` — simplified loop (no tool handling yet)
- `src-tauri/src/agent/tools.rs` — empty registry stub
- `src/agent.js` — full tab UI, message rendering, streaming text

**Modify:**
- `src-tauri/src/lib.rs` — `mod agent;`, register commands, manage state
- `src-tauri/Cargo.toml` — add dependencies
- `src/main.js` — import agent.js, createAgentTab(), Cmd+L, event listeners, tab bar
- `src/styles.css` — agent tab styles
- `src/settings.js` — agent defaults
- `index.html` — Cmd+L in shortcuts modal

### Phase 2: Tool use
**Goal**: Agent reads files, writes files, runs bash, searches, lists directories. Full agentic loop.

**Create:**
- `src-tauri/src/agent/patch.rs` — patch parser + fuzzy matching (stub, refined in Phase 3)

**Modify:**
- `src-tauri/src/lib.rs` — extract `_inner` functions from file/search commands
- `src-tauri/src/agent/tools.rs` — implement ReadFile, WriteFile, ListDirectory, SearchFiles, BashTool
- `src-tauri/src/agent/loop_runner.rs` — full tool execution loop, parallel/sequential ordering
- `src/agent.js` — tool call rendering, file change triggers

### Phase 3: Patch tool + git integration
**Goal**: Full coding agent — multi-file patches, git status, stage + commit.

**Modify:**
- `src-tauri/src/agent/patch.rs` — complete 3-tier fuzzy matching
- `src-tauri/src/agent/tools.rs` — ApplyPatchTool, GitStatusTool, GitCommitTool
- `src-tauri/src/lib.rs` — extract `_inner` functions from git commands

### Phase 4: OpenAI provider + settings UI
**Goal**: Provider choice, model selection, full settings panel section.

**Modify:**
- `src-tauri/src/agent/provider.rs` — OpenAIProvider
- `src/settingspanel.js` — Agent section
- `src/settings.js` — agent keys

### Phase 5: Polish
**Goal**: Token display, cost tracking, markdown improvements, error recovery, code block highlighting.

---

## 6. Key Design Decisions

| Decision | Rationale |
|----------|-----------|
| Separate `AgentState` from `AppState` | Different lifecycle (async, long-running) avoids mutex contention |
| `keyring` for API keys | OS keychain is platform-standard secure storage, never on disk |
| `tokio::process` for BashTool, not PTY | Agent needs captured stdout/stderr strings, not a terminal session |
| Token estimation via `len/4` | Avoids 5MB+ tiktoken dependency; actual API counts used for display |
| Custom minimal markdown renderer | 50 lines of vanilla JS, no dependency, matches project philosophy |
| Tools wrap existing backend functions | Zero code duplication; agent gets the same capabilities as the UI |
| Patch format over unified diff | More LLM-friendly; claw-code-rust proved the fuzzy matching approach |

## 7. Verification Plan

After each phase:
1. `cargo check --manifest-path src-tauri/Cargo.toml` — type check
2. `npx tauri dev` — run app, open agent tab
3. Phase 1: Send a message, verify streaming text renders
4. Phase 2: Ask agent to read a file, write a file, run `ls` — verify tool calls render and file browser updates
5. Phase 3: Ask agent to modify code via patch — verify file changes appear in editor/git panel
6. Phase 4: Switch to OpenAI in settings, verify conversation works
7. Phase 5: Monitor token counts, test compaction by running many turns
