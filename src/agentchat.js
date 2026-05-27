// Agent chat tab — in-process integration with launchpad-agent.
//
// Tab shape:
//   { type: "agent", containerEl, sessionId, turnId, messages,
//     pendingApprovals, modelSlug, scrollPinned, unsubscribe }
//
// Lifecycle:
//   1. createAgentTab() — creates the tab, calls agent_initialize +
//      agent_session_start, attaches the per-window event listener
//   2. User types in composer → agent_send_message
//   3. Item / delta / approval events arrive on `agent:event:<window_label>`
//      and route through dispatchEvent(tab, event)
//   4. closeAgentTab() — detaches listener; sessions persist runtime-side

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { marked } from "marked";
import { renderMessage, renderApproval, renderToolCard, updateToolCard } from "./agentmsg.js";
import { createComposer, clearSkillCache } from "./agentcomposer.js";
import { getActiveProject } from "./projects.js";
import { showToast, showConfirmDialog } from "./main.js";

marked.setOptions({ breaks: true, gfm: true });

let initialized = false;
async function ensureInitialized() {
  if (initialized) return;
  await invoke("agent_initialize");
  initialized = true;
}

// When the user hits "Save & reload" in settings, the runtime drops; every
// active session_id is now stale. Reset per-tab state so the next user
// message starts a fresh session against the new provider.
window.addEventListener("launchpad:agent-reloaded", () => {
  initialized = false;
  clearSkillCache();
  for (const tab of sessionTabs.values()) {
    tab.sessionId = null;
    tab.turnId = null;
    tab.composer?.setTurnActive(false);
    const notice = document.createElement("div");
    notice.className = "agent-msg agent-msg-system";
    notice.innerHTML = `<div class="agent-msg-system">— provider reloaded; next message starts a new session —</div>`;
    tab.listEl.appendChild(notice);
  }
  sessionTabs.clear();
});

// Session id → tab ref. Used by the global event listener to route
// notifications to the right tab without each tab subscribing redundantly.
const sessionTabs = new Map();
let globalListenerInstalled = false;

async function installGlobalListener() {
  if (globalListenerInstalled) return;
  globalListenerInstalled = true;
  const win = getCurrentWebviewWindow();
  const channel = `agent:event:${win.label}`;
  await win.listen(channel, (event) => {
    const env = event.payload;
    if (!env || typeof env !== "object") return;
    const params = env.params;
    const sessionId =
      params?.session?.session_id ||
      params?.context?.session_id ||
      params?.session_id ||
      params?.request?.session_id;
    if (!sessionId) return;
    const tab = sessionTabs.get(sessionId);
    if (!tab) return;
    dispatchEvent(tab, env);
  });
}

export async function createAgentTab({ tabs, nextUiTabId, switchTab, renderTabBar, openSettings }) {
  // First-run gate: if no provider/api-key configured, surface an in-tab
  // CTA instead of bubbling the runtime's bootstrap error. Cheap pre-flight
  // — no runtime construction.
  let configured;
  try {
    configured = await invoke("agent_is_configured");
  } catch (_) {
    configured = false;
  }

  if (configured) {
    await ensureInitialized();
    await installGlobalListener();
  }

  const project = getActiveProject();
  const uiId = nextUiTabId();

  const containerEl = document.createElement("div");
  containerEl.className = "agent-tab-instance";
  containerEl.style.display = "flex";
  containerEl.style.flexDirection = "column";
  containerEl.style.height = "100%";
  containerEl.style.minHeight = "0";

  // Header
  const header = document.createElement("div");
  header.className = "agent-header";
  const headerLeft = document.createElement("div");
  headerLeft.className = "agent-header-left";
  const sessionsBtn = document.createElement("button");
  sessionsBtn.className = "agent-header-sessions-btn";
  sessionsBtn.title = "Past sessions";
  sessionsBtn.textContent = "☰";
  headerLeft.appendChild(sessionsBtn);
  const title = document.createElement("div");
  title.className = "agent-header-title";
  title.textContent = "Agent";
  headerLeft.appendChild(title);
  header.appendChild(headerLeft);
  const headerRight = document.createElement("div");
  headerRight.className = "agent-header-right";
  const modePill = document.createElement("button");
  modePill.className = "agent-mode-pill";
  modePill.title = "Click to toggle permission mode";
  modePill.textContent = "Interactive";
  modePill.dataset.mode = "interactive";
  headerRight.appendChild(modePill);
  const modelLabel = document.createElement("div");
  modelLabel.className = "agent-header-model";
  modelLabel.textContent = "—";
  headerRight.appendChild(modelLabel);
  header.appendChild(headerRight);
  containerEl.appendChild(header);

  // Message list (scroll container)
  const listEl = document.createElement("div");
  listEl.className = "agent-messages";
  containerEl.appendChild(listEl);

  // Composer
  const composerEl = document.createElement("div");
  composerEl.className = "agent-composer-wrap";
  containerEl.appendChild(composerEl);

  document.getElementById("terminal-instances").appendChild(containerEl);

  const tab = {
    type: "agent",
    containerEl,
    sessionId: null,
    turnId: null,
    messages: [],
    /** Map item_id → { kind, el, textNode?, originalToolCard? } */
    items: new Map(),
    /** Map tool_use_id → item_id of its tool_call card. Used to nest
     *  tool_result items under their parent call. */
    toolUseIndex: new Map(),
    /** Map approval_id → ApprovalRequest payload */
    pendingApprovals: new Map(),
    modelSlug: null,
    permissionMode: "interactive",
    scrollPinned: true,
    listEl,
    modelLabel,
    modePill,
    fileName: "Agent",
  };

  modePill.addEventListener("click", async () => {
    if (!tab.sessionId) return;
    const next = tab.permissionMode === "interactive" ? "auto-approve" : "interactive";
    try {
      const resp = await invoke("agent_session_update_config", {
        sessionId: tab.sessionId,
        permissionMode: next,
      });
      const mode = resp?.result?.permission_mode || next;
      tab.permissionMode = mode;
      modePill.dataset.mode = mode;
      modePill.textContent = mode === "auto-approve" ? "Auto" : "Interactive";
    } catch (e) {
      console.warn("failed to update permission mode:", e);
    }
  });

  // Track scroll-pinned state — auto-scroll only when user is at bottom
  listEl.addEventListener("scroll", () => {
    const distance = listEl.scrollHeight - listEl.scrollTop - listEl.clientHeight;
    tab.scrollPinned = distance < 32;
  });

  // Composer wired to send / cancel
  const composer = createComposer({
    onSend: async (text, mentions, skills) => {
      if (!text.trim()) return;
      // Re-entry guard: a fast double-Enter could otherwise hit the lazy
      // session-start block twice and create two sessions, with the first
      // orphaned but still in sessionTabs.
      if (tab._sending) return;
      tab._sending = true;
      pushUserMessage(tab, text);
      try {
        // If the runtime was reloaded since this tab was opened, sessionId
        // is null — start a new session against the new provider before
        // sending the message.
        if (!tab.sessionId) {
          await ensureInitialized();
          const startResp = await invoke("agent_session_start", {
            projectPath: getActiveProject()?.path || null,
            model: null,
          });
          const sid = startResp?.result?.session_id;
          if (!sid) {
            const errMsg =
              startResp?.error?.message ||
              (startResp?.error ? JSON.stringify(startResp.error) : "Failed to start new session after reload.");
            pushError(tab, errMsg);
            return;
          }
          tab.sessionId = sid;
          sessionTabs.set(sid, tab);
          const resolvedModel = startResp?.result?.resolved_model;
          if (resolvedModel) {
            tab.modelSlug = resolvedModel;
            tab.modelLabel.textContent = resolvedModel;
          }
        }
        const resp = await invoke("agent_send_message", {
          sessionId: tab.sessionId,
          text,
          mentions: mentions && mentions.length ? mentions : null,
          skills: skills && skills.length ? skills : null,
        });
        const turnId = resp?.result?.turn_id;
        if (turnId) tab.turnId = turnId;
        composer.setTurnActive(true);
      } catch (err) {
        pushError(tab, String(err));
        composer.setTurnActive(false);
      } finally {
        tab._sending = false;
      }
    },
    onCancel: async () => {
      if (!tab.sessionId || !tab.turnId) return;
      try {
        await invoke("agent_interrupt_turn", {
          sessionId: tab.sessionId,
          turnId: tab.turnId,
          reason: "user_cancelled",
        });
      } catch (err) {
        showToast(`Cancel failed: ${err}`, "error");
      }
    },
  });
  tab.composer = composer;
  composerEl.appendChild(composer.el);

  // Sessions sidebar — opens a dropdown listing past persisted sessions
  // (from launchpad-agent's RolloutStore). Click to resume in this tab.
  sessionsBtn.addEventListener("click", () => toggleSessionsPanel(tab));

  tabs.set(uiId, tab);
  renderTabBar();
  switchTab(uiId);

  // Empty-state branch: show the first-run CTA, leave the composer disabled,
  // and wait for `launchpad:agent-reloaded` (fired by Settings → Save & reload)
  // to recover into a real session without forcing the user to close/reopen
  // this tab.
  if (!configured) {
    renderUnconfiguredEmptyState(tab, openSettings);
    const recover = async () => {
      let nowConfigured = false;
      try { nowConfigured = await invoke("agent_is_configured"); } catch (_) {}
      if (!nowConfigured) return;
      window.removeEventListener("launchpad:agent-reloaded", recover);
      tab._recoverListener = null;
      tab.emptyStateEl?.remove();
      tab.emptyStateEl = null;
      tab.composer?.setDisabled(false, "");
      try {
        await ensureInitialized();
        await installGlobalListener();
        // Resolve the project at recover time, not tab-creation time —
        // the user may have switched projects while the tab sat idle.
        await startInitialSession(tab, getActiveProject());
      } catch (err) {
        pushError(tab, `Failed to start session: ${err}`);
      }
    };
    window.addEventListener("launchpad:agent-reloaded", recover);
    tab._recoverListener = recover;
    tab.composer?.setDisabled(true, "Configure a provider in Settings to start chatting…");
    return tab;
  }

  // Configured but no session yet. Defer agent_session_start until the user
  // actually sends a message — the onSend handler will lazy-start. This way
  // opening a chat tab and walking away doesn't spawn an empty untitled
  // session in the persisted sessions list.
  return tab;
}

async function startInitialSession(tab, project) {
  try {
    const resp = await invoke("agent_session_start", {
      projectPath: project?.path || null,
      model: null,
    });
    const sessionId = resp?.result?.session_id;
    if (!sessionId) {
      const errMsg =
        resp?.error?.message ||
        (resp?.error ? JSON.stringify(resp.error) : "session/start returned no session_id");
      pushError(tab, errMsg);
      return;
    }
    tab.sessionId = sessionId;
    sessionTabs.set(sessionId, tab);
    const resolvedModel = resp?.result?.resolved_model;
    if (resolvedModel) {
      tab.modelSlug = resolvedModel;
      tab.modelLabel.textContent = resolvedModel;
    }
  } catch (err) {
    pushError(tab, `Failed to start session: ${err}`);
  }
}

function renderUnconfiguredEmptyState(tab, openSettings) {
  const wrap = document.createElement("div");
  wrap.className = "agent-empty-state";
  wrap.innerHTML = `
    <div class="agent-empty-state-icon">✦</div>
    <h2 class="agent-empty-state-title">Set up your agent provider</h2>
    <p class="agent-empty-state-body">
      Choose a provider (Anthropic, OpenAI, Google, OpenRouter, Ollama, …) and
      add your API key to start chatting. Settings are stored locally at
      <code>~/.launchpad/agent-config.json</code>.
    </p>
    <button class="agent-empty-state-btn" type="button">Open Settings →</button>
  `;
  wrap.querySelector(".agent-empty-state-btn").addEventListener("click", () => {
    if (typeof openSettings === "function") {
      openSettings({ scrollTo: "agent-section" });
    }
  });
  tab.listEl.appendChild(wrap);
  tab.emptyStateEl = wrap;
}

export function closeAgentTab(tab) {
  if (tab.sessionId) sessionTabs.delete(tab.sessionId);
  // Detach the recover listener so a later Save & reload doesn't fire on
  // a closed tab and leak an orphan session into the runtime.
  if (tab._recoverListener) {
    window.removeEventListener("launchpad:agent-reloaded", tab._recoverListener);
    tab._recoverListener = null;
  }
  // Sessions persist on the runtime; closing the tab just drops the UI.
}

// ─── Event dispatch ────────────────────────────────────────────────────────

function dispatchEvent(tab, env) {
  const method = env.method;
  if (!method) return;
  const params = env.params || {};

  switch (method) {
    case "session/started":
    case "session/title/updated":
      handleSessionEvent(tab, params);
      break;
    case "turn/started":
      tab.turnId = params?.turn?.turn_id || tab.turnId;
      tab.composer?.setTurnActive(true);
      showThinkingIndicator(tab, true);
      // New turn → drop the prior turn's usage footer reference so the next
      // usage/updated event mounts a fresh one.
      tab.currentTurnUsageEl = null;
      break;
    case "turn/completed":
    case "turn/interrupted":
    case "turn/failed":
      tab.composer?.setTurnActive(false);
      showThinkingIndicator(tab, false);
      if (method === "turn/failed") {
        const msg = params?.turn?.error || "Turn failed";
        pushError(tab, msg);
      }
      break;
    case "turn/usage/updated":
      handleUsageUpdated(tab, params);
      break;
    case "item/started":
      handleItemStarted(tab, params);
      break;
    case "item/completed":
      handleItemCompleted(tab, params);
      break;
    case "item/agentMessage/delta":
    case "item/reasoning/textDelta":
    case "item/reasoning/summaryTextDelta":
    case "item/commandExecution/outputDelta":
    case "item/fileChange/outputDelta":
    case "item/plan/delta":
      handleItemDelta(tab, method, params);
      break;
    case "approval/requested":
      handleApprovalRequest(tab, params);
      break;
    case "serverRequest/resolved":
      // Approval card removal handled in agent_approve flow.
      break;
    default:
      // Silently drop unknown methods (forward compat with new event types).
      break;
  }
}

function handleSessionEvent(tab, params) {
  const summary = params?.session;
  if (summary?.model && tab.modelSlug !== summary.model) {
    tab.modelSlug = summary.model;
    tab.modelLabel.textContent = summary.model;
  }
  if (summary?.title) {
    // Title updates aren't surfaced in the tab bar yet — held for later.
  }
}

function handleItemStarted(tab, params) {
  const itemId = params?.item?.item_id;
  const kind = params?.item?.item_kind;
  const payload = params?.item?.payload || {};
  if (!itemId) return;

  if (tab.items.has(itemId)) return;

  let entry;
  switch (kind) {
    case "agent_message": {
      const el = renderMessage("assistant_text", "", { itemId });
      tab.listEl.appendChild(el);
      entry = { kind, el, textNode: el.querySelector(".agent-msg-content"), buf: "" };
      break;
    }
    case "user_message": {
      const text = textFromUserMessagePayload(payload);
      if (text) {
        // The local push from onSend already rendered this; skip.
        entry = { kind, el: null };
      } else {
        const el = renderMessage("user", "", { itemId });
        tab.listEl.appendChild(el);
        entry = { kind, el };
      }
      break;
    }
    case "reasoning": {
      const el = renderMessage("thinking", "", { itemId });
      tab.listEl.appendChild(el);
      entry = { kind, el, textNode: el.querySelector(".agent-msg-content"), buf: "" };
      break;
    }
    case "tool_call":
    case "mcp_tool_call":
    case "command_execution":
    case "file_change":
    case "web_search":
    case "image_view":
    case "plan": {
      const el = renderToolCard(kind, payload, { itemId });
      tab.listEl.appendChild(el);
      entry = { kind, el, payload, buf: "" };
      // Index by tool_use_id so the eventual tool_result can find this card.
      const useId = payload?.tool_use_id || payload?.tool_call_id;
      if (useId) tab.toolUseIndex.set(useId, itemId);
      break;
    }
    case "tool_result": {
      // Link result to its parent tool_call by tool_use_id (the runtime's
      // emit_turn_item ToolResult payload uses `tool_use_id`, NOT
      // `call_item_id`). Falls back to a free-floating result card if the
      // call wasn't seen — e.g. on session resume mid-flight.
      const useId = payload?.tool_use_id || payload?.tool_call_id;
      const callItemId = useId ? tab.toolUseIndex.get(useId) : null;
      const callEntry = callItemId ? tab.items.get(callItemId) : null;
      const target = callEntry?.el?.querySelector(".tool-result-slot") || tab.listEl;
      const el = renderToolCard("tool_result", payload, { itemId });
      target.appendChild(el);
      entry = { kind, el };
      break;
    }
    case "approval_request": {
      // Approval requests come via approval/requested separately; nothing to do here.
      entry = { kind, el: null };
      break;
    }
    case "approval_decision": {
      entry = { kind, el: null };
      break;
    }
    case "context_compaction": {
      const el = renderMessage("system", "context compacted", { itemId });
      tab.listEl.appendChild(el);
      entry = { kind, el };
      break;
    }
    default: {
      const el = renderMessage("system", `(${kind})`, { itemId });
      tab.listEl.appendChild(el);
      entry = { kind, el };
      break;
    }
  }

  tab.items.set(itemId, entry);
  autoscroll(tab);
}

function handleItemDelta(tab, method, params) {
  const itemId = params?.context?.item_id;
  if (!itemId) return;
  const entry = tab.items.get(itemId);
  if (!entry) return;
  const delta = params?.delta || "";
  entry.buf = (entry.buf || "") + delta;

  if (entry.kind === "agent_message" || entry.kind === "reasoning") {
    if (entry.textNode) entry.textNode.textContent = entry.buf;
  } else if (entry.kind === "command_execution") {
    updateToolCard(entry.el, "command_execution", { outputDelta: delta });
  } else if (entry.kind === "file_change") {
    updateToolCard(entry.el, "file_change", { outputDelta: delta });
  } else if (entry.kind === "plan") {
    updateToolCard(entry.el, "plan", { delta });
  }
  autoscroll(tab);
}

function handleItemCompleted(tab, params) {
  const itemId = params?.item?.item_id;
  const kind = params?.item?.item_kind;
  const payload = params?.item?.payload || {};
  const entry = tab.items.get(itemId);
  if (!entry || !entry.el) return;

  if (kind === "agent_message") {
    // Render markdown on completion (avoid streaming-rerender stutter).
    const finalText = textFromAgentMessagePayload(payload) || entry.buf || "";
    const html = marked.parse(finalText);
    entry.textNode.innerHTML = html;
  } else if (kind === "tool_call" || kind === "mcp_tool_call" || kind === "command_execution" ||
             kind === "file_change" || kind === "web_search" || kind === "image_view" ||
             kind === "plan") {
    updateToolCard(entry.el, kind, { complete: true, payload });
  } else if (kind === "tool_result") {
    updateToolCard(entry.el, "tool_result", { complete: true, payload });
  }
  autoscroll(tab);
}

function handleApprovalRequest(tab, params) {
  const approvalId = params?.approval_id;
  if (!approvalId) return;
  tab.pendingApprovals.set(approvalId, params);
  const card = renderApproval(params, {
    onRespond: async (decision, scope) => {
      try {
        await invoke("agent_approve", {
          sessionId: tab.sessionId,
          turnId: tab.turnId || params?.request?.turn_id || null,
          approvalId,
          decision,
          scope: scope || "once",
        });
        tab.pendingApprovals.delete(approvalId);
        card.collapse(decision, scope || "once");
      } catch (err) {
        showToast(`Approval response failed: ${err}`, "error");
      }
    },
  });
  tab.listEl.appendChild(card.el);
  autoscroll(tab, { force: true });
}

// ─── Helpers ──────────────────────────────────────────────────────────────

function pushUserMessage(tab, text) {
  const el = renderMessage("user", text, {});
  tab.listEl.appendChild(el);
  autoscroll(tab, { force: true });
}

// Surfaces token/cache usage as a tiny muted footer at the bottom of the
// message stream, updated in place on every usage/updated event. One footer
// per turn — turn/started clears the reference so the next usage event mounts
// a fresh row. The footer is appended (not pinned), so prior turns' usage
// stays visible as the user scrolls back.
function handleUsageUpdated(tab, params) {
  const usage = params?.usage || {};
  const totalIn = params?.total_input_tokens;
  const totalOut = params?.total_output_tokens;

  if (!tab.currentTurnUsageEl) {
    const el = document.createElement("div");
    el.className = "agent-turn-usage";
    tab.listEl.appendChild(el);
    tab.currentTurnUsageEl = el;
  }
  const el = tab.currentTurnUsageEl;

  const inT = formatTokens(usage.input_tokens);
  const outT = formatTokens(usage.output_tokens);
  const cacheRead = usage.cache_read_input_tokens;
  const cacheWrite = usage.cache_creation_input_tokens;
  const cacheBits = [];
  if (cacheRead) cacheBits.push(`cache hit ${formatTokens(cacheRead)}`);
  if (cacheWrite) cacheBits.push(`cache write ${formatTokens(cacheWrite)}`);
  const cacheStr = cacheBits.length ? ` · ${cacheBits.join(" · ")}` : "";
  const totalStr =
    totalIn != null && totalOut != null
      ? ` · session ${formatTokens(totalIn)} in / ${formatTokens(totalOut)} out`
      : "";
  el.textContent = `↑ ${inT} in · ↓ ${outT} out${cacheStr}${totalStr}`;

  // Keep the working… indicator below the usage line if it's still up.
  if (tab.thinkingEl && tab.thinkingEl.parentElement === tab.listEl) {
    tab.listEl.appendChild(tab.thinkingEl);
  }
  autoscroll(tab);
}

function formatTokens(n) {
  if (n == null || isNaN(n)) return "—";
  if (n < 1000) return String(n);
  if (n < 10_000) return (n / 1000).toFixed(2).replace(/\.?0+$/, "") + "K";
  if (n < 1_000_000) return Math.round(n / 1000) + "K";
  return (n / 1_000_000).toFixed(1) + "M";
}

function pushError(tab, msg) {
  const el = renderMessage("error", msg, {});
  tab.listEl.appendChild(el);
  autoscroll(tab, { force: true });
}

// ─── Sessions sidebar ─────────────────────────────────────────────────

async function toggleSessionsPanel(tab) {
  if (tab.sessionsPanelEl) {
    tab.sessionsPanelEl.remove();
    tab.sessionsPanelEl = null;
    return;
  }
  const panel = document.createElement("div");
  panel.className = "agent-sessions-panel";
  panel.innerHTML = `
    <div class="agent-sessions-header">
      <span>Sessions</span>
      <button class="agent-sessions-close" title="Close">×</button>
    </div>
    <div class="agent-sessions-body"><div class="agent-sessions-loading">Loading…</div></div>
  `;
  tab.containerEl.appendChild(panel);
  tab.sessionsPanelEl = panel;

  panel.querySelector(".agent-sessions-close").addEventListener("click", () => {
    panel.remove();
    tab.sessionsPanelEl = null;
  });

  const body = panel.querySelector(".agent-sessions-body");
  let resp;
  try {
    resp = await invoke("agent_session_list");
  } catch (err) {
    body.innerHTML = `<div class="agent-sessions-empty">Failed to load: ${escapeHtml(String(err))}</div>`;
    return;
  }
  // Filter out sessions the user has deleted (their rollout files are
  // gone too — see agent_session_delete — but the runtime's in-memory map
  // still has them until the next process restart).
  let deleted = new Set();
  try {
    const ids = await invoke("agent_session_deleted_ids");
    deleted = new Set(ids || []);
  } catch (_) {}
  const sessions = (resp?.result?.sessions || []).filter(
    (s) => !deleted.has(s.session_id)
  );
  // Most recently active first.
  sessions.sort((a, b) => {
    const tA = a.updated_at || a.created_at || "";
    const tB = b.updated_at || b.created_at || "";
    return tB.localeCompare(tA);
  });
  if (!sessions.length) {
    body.innerHTML = `<div class="agent-sessions-empty">No past sessions yet.</div>`;
    return;
  }

  body.innerHTML = "";
  for (const s of sessions) {
    const row = document.createElement("div");
    row.className = "agent-sessions-row";
    if (s.session_id === tab.sessionId) row.classList.add("agent-sessions-row-active");

    const main = document.createElement("div");
    main.className = "agent-sessions-row-main";

    const titleEl = document.createElement("div");
    titleEl.className = "agent-sessions-row-title";
    titleEl.textContent = s.title || "(untitled)";
    main.appendChild(titleEl);

    const meta = document.createElement("div");
    meta.className = "agent-sessions-row-meta";
    const cwdShort = s.cwd ? shortenPath(s.cwd) : "";
    const when = s.updated_at ? formatRelative(s.updated_at) : "";
    const model = s.resolved_model || "";
    meta.textContent = [cwdShort, when, model].filter(Boolean).join(" · ");
    main.appendChild(meta);

    main.addEventListener("click", () => resumeSession(tab, s.session_id));
    row.appendChild(main);

    const delBtn = document.createElement("button");
    delBtn.className = "agent-sessions-row-delete";
    delBtn.title = "Delete session";
    delBtn.textContent = "×";
    delBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      const label = s.title || "(untitled)";
      showConfirmDialog(
        `Delete session "${label}"? This removes its rollout file from disk.`,
        async () => {
          try {
            await invoke("agent_session_delete", { sessionId: s.session_id });
            // Remove from list immediately. If the user was viewing this
            // session, reset the tab so the next message starts a new one.
            if (s.session_id === tab.sessionId) {
              sessionTabs.delete(tab.sessionId);
              tab.sessionId = null;
              tab.turnId = null;
              tab.composer?.setTurnActive(false);
              tab.listEl.replaceChildren();
              tab.items.clear();
              tab.toolUseIndex.clear();
            }
            row.remove();
            if (!body.children.length) {
              body.innerHTML = `<div class="agent-sessions-empty">No past sessions yet.</div>`;
            }
          } catch (err) {
            showToast(`Delete failed: ${err}`, "error");
          }
        },
        { confirmLabel: "Delete", tone: "danger" },
      );
    });
    row.appendChild(delBtn);

    body.appendChild(row);
  }
}

async function resumeSession(tab, sessionId) {
  if (sessionId === tab.sessionId) {
    // Already viewing this session — just close the panel.
    tab.sessionsPanelEl?.remove();
    tab.sessionsPanelEl = null;
    return;
  }
  let resp;
  try {
    resp = await invoke("agent_session_resume", { sessionId });
  } catch (err) {
    showToast(`Resume failed: ${err}`, "error");
    return;
  }
  if (!resp?.result?.session) {
    const msg = resp?.error?.message || "session/resume returned no session";
    showToast(msg, "error");
    return;
  }

  // Detach this tab from its current session so its event listener no
  // longer receives stale events for the old session.
  if (tab.sessionId) sessionTabs.delete(tab.sessionId);
  tab.sessionId = sessionId;
  tab.turnId = null;
  tab.composer?.setTurnActive(false);
  sessionTabs.set(sessionId, tab);

  // Reset chat surface to a clean replay of the persisted history.
  tab.listEl.replaceChildren();
  tab.items.clear();
  tab.toolUseIndex.clear();
  tab.pendingApprovals.clear();
  tab.thinkingEl = null;
  // Drop the prior turn's usage footer reference too — the DOM node was
  // just removed by replaceChildren, but the JS reference would otherwise
  // outlive it and cause stray usage events to update an orphan element.
  tab.currentTurnUsageEl = null;

  const summary = resp.result.session;
  if (summary.resolved_model) {
    tab.modelSlug = summary.resolved_model;
    tab.modelLabel.textContent = summary.resolved_model;
  }

  const history = resp.result.history_items || [];
  for (const h of history) replayHistoryItem(tab, h);

  // Subscribe again — handle_session_resume already subscribes the
  // connection, but make sure by re-asking. Idempotent server-side.
  try { await invoke("agent_subscribe_events", { sessionId }); } catch (_) {}

  // Close the panel.
  tab.sessionsPanelEl?.remove();
  tab.sessionsPanelEl = null;
  autoscroll(tab, { force: true });
}

// Renders one persisted history item (kind: User | Assistant | ToolCall |
// ToolResult | Error) into the chat surface. Bodies are plain strings;
// assistant text gets markdown-rendered, tool calls/results render as
// compact cards.
function replayHistoryItem(tab, item) {
  const { kind, title, body, payload } = item || {};
  if (kind === "User" || kind === "user") {
    const el = renderMessage("user", body || title || "", {});
    tab.listEl.appendChild(el);
    return;
  }
  if (kind === "Assistant" || kind === "assistant") {
    const el = renderMessage("assistant_text", "", {});
    const content = el.querySelector(".agent-msg-content");
    try { content.innerHTML = marked.parse(body || ""); }
    catch (_) { content.textContent = body || ""; }
    tab.listEl.appendChild(el);
    return;
  }
  if (kind === "ToolCall" || kind === "tool_call") {
    // When the rollout carries the original payload (post-fix sessions),
    // route it through the standard tool-card renderer so the specialized
    // bash / read / grep / apply_patch / write previews fire on resume.
    // Otherwise fall back to a minimal title+body card for legacy rollouts.
    //
    // Indexing shape MUST match the live-event path (see handleItemStarted):
    //   tab.items.set(itemId, entry)
    //   tab.toolUseIndex.set(useId, itemId)
    // Mixing in `useId` as the items-map key (as an earlier pass did) breaks
    // any subsequent live tool_result that arrives mid-resume.
    if (payload && typeof payload === "object") {
      const card = renderToolCard("tool_call", payload, {});
      updateToolCard(card, "tool_call", { complete: true, payload });
      const useId = payload.tool_use_id || payload.tool_call_id;
      const itemId = crypto.randomUUID();
      if (useId) tab.toolUseIndex.set(useId, itemId);
      tab.items.set(itemId, { kind: "tool_call", el: card, payload });
      tab.listEl.appendChild(card);
      return;
    }
    const card = renderToolCard("tool_call", { tool_name: title || "tool" }, {});
    updateToolCard(card, "tool_call", { complete: true, payload: {} });
    if (body) {
      const args = document.createElement("pre");
      args.className = "tool-args";
      args.textContent = body;
      card._body.appendChild(args);
    }
    tab.listEl.appendChild(card);
    return;
  }
  if (kind === "ToolResult" || kind === "tool_result") {
    // Prefer the rich payload so extractResultText picks up structured
    // content (stdout/stderr splits, JSON envelopes). Falls back to body
    // for legacy rollouts.
    const usePayload = payload && typeof payload === "object"
      ? payload
      : { content: body, is_error: false };
    // Resolve the parent card via toolUseIndex → items, exactly like the
    // live-event path. Legacy rollouts (no payload, no useId) get a
    // free-floating result card under tab.listEl.
    const useId = usePayload.tool_use_id || usePayload.tool_call_id;
    const parentItemId = useId ? tab.toolUseIndex.get(useId) : null;
    const parentCardEntry = parentItemId ? tab.items.get(parentItemId) : null;
    const target = parentCardEntry?.el?.querySelector(".tool-result-slot")
      || tab.listEl;
    const card = renderToolCard("tool_result", usePayload, {});
    updateToolCard(card, "tool_result", { complete: true, payload: usePayload });
    target.appendChild(card);
    return;
  }
  if (kind === "Error" || kind === "error") {
    const el = renderMessage("error", body || title || "Error", {});
    tab.listEl.appendChild(el);
    return;
  }
  // Fallback for unknown kinds — show as system note so nothing is lost.
  const el = renderMessage("system", `${title || kind}${body ? ": " + body : ""}`, {});
  tab.listEl.appendChild(el);
}

function shortenPath(p) {
  if (!p) return "";
  const home = (typeof window !== "undefined" && window.__launchpadHome__) || "";
  if (home && p.startsWith(home)) return "~" + p.slice(home.length);
  // Fall back to the trailing two segments.
  const parts = p.split("/").filter(Boolean);
  if (parts.length <= 2) return p;
  return ".../" + parts.slice(-2).join("/");
}

function formatRelative(iso) {
  const t = Date.parse(iso);
  if (!t) return "";
  const diff = Date.now() - t;
  const m = Math.round(diff / 60000);
  if (m < 1) return "just now";
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h}h ago`;
  const d = Math.round(h / 24);
  if (d < 7) return `${d}d ago`;
  return new Date(t).toLocaleDateString();
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])
  );
}

// Pinned at the bottom of the message list while a turn is running so the
// user always knows the agent is working — even when there's a long gap
// between an item completing and the next item starting.
function showThinkingIndicator(tab, on) {
  if (on) {
    if (tab.thinkingEl) return;
    const el = document.createElement("div");
    el.className = "agent-thinking-indicator";
    el.innerHTML = `
      <span class="agent-thinking-dot"></span>
      <span class="agent-thinking-dot"></span>
      <span class="agent-thinking-dot"></span>
      <span class="agent-thinking-label">working…</span>
    `;
    tab.listEl.appendChild(el);
    tab.thinkingEl = el;
    autoscroll(tab, { force: true });
  } else {
    tab.thinkingEl?.remove();
    tab.thinkingEl = null;
  }
}

function autoscroll(tab, opts = {}) {
  // Keep the working… indicator at the bottom of the list when other
  // elements are appended after it.
  if (tab.thinkingEl && tab.thinkingEl.parentElement === tab.listEl) {
    tab.listEl.appendChild(tab.thinkingEl);
  }
  if (!opts.force && !tab.scrollPinned) return;
  requestAnimationFrame(() => {
    tab.listEl.scrollTop = tab.listEl.scrollHeight;
  });
}

function textFromAgentMessagePayload(payload) {
  // payload typically has { content: [ { type: "text", text } ] } or { text }
  if (!payload) return "";
  if (typeof payload.text === "string") return payload.text;
  if (Array.isArray(payload.content)) {
    return payload.content
      .filter((c) => c?.type === "text")
      .map((c) => c.text || "")
      .join("");
  }
  return "";
}

function textFromUserMessagePayload(payload) {
  if (!payload) return "";
  if (typeof payload.text === "string") return payload.text;
  if (Array.isArray(payload.input)) {
    return payload.input
      .filter((c) => c?.type === "text")
      .map((c) => c.text || "")
      .join("");
  }
  return "";
}
