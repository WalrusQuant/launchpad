// Renderers for the various agent chat message and tool-call card types.
// Plain DOM construction — no framework. Each function returns an element
// suitable for appending to `.agent-messages`.

import { buildFileDiffSection } from "./diffrender.js";
import { parseUnifiedDiff, diffStats } from "./diffparse.js";

export function renderMessage(kind, text, opts = {}) {
  const wrap = document.createElement("div");
  wrap.className = `agent-msg agent-msg-${kind}`;
  if (opts.itemId) wrap.dataset.itemId = opts.itemId;

  if (kind === "user") {
    const bubble = document.createElement("div");
    bubble.className = "agent-msg-bubble";
    bubble.textContent = text;
    wrap.appendChild(bubble);
    return wrap;
  }

  if (kind === "thinking") {
    const summary = document.createElement("div");
    summary.className = "agent-msg-thinking-toggle";
    summary.textContent = "thinking…";
    const content = document.createElement("div");
    content.className = "agent-msg-content";
    content.style.display = "none";
    summary.addEventListener("click", () => {
      const showing = content.style.display !== "none";
      content.style.display = showing ? "none" : "block";
      summary.textContent = showing ? "thinking…" : "thinking ▾";
    });
    wrap.appendChild(summary);
    wrap.appendChild(content);
    if (text) content.textContent = text;
    return wrap;
  }

  if (kind === "error") {
    const banner = document.createElement("div");
    banner.className = "agent-msg-error";
    banner.textContent = text;
    wrap.appendChild(banner);
    return wrap;
  }

  if (kind === "system") {
    const note = document.createElement("div");
    note.className = "agent-msg-system";
    note.textContent = text;
    wrap.appendChild(note);
    return wrap;
  }

  // assistant_text — plain text container until item completes (then markdown
  // gets injected via innerHTML on completion).
  const content = document.createElement("div");
  content.className = "agent-msg-content";
  content.textContent = text;
  wrap.appendChild(content);
  return wrap;
}

// ─── Tool cards ───────────────────────────────────────────────────────────

export function renderToolCard(kind, payload, opts = {}) {
  const card = document.createElement("div");
  card.className = `tool-card tool-card-${kind}`;
  if (opts.itemId) card.dataset.itemId = opts.itemId;

  const header = document.createElement("div");
  header.className = "tool-card-header";

  const icon = document.createElement("span");
  icon.className = "tool-card-icon";
  icon.textContent = iconForKind(kind);
  header.appendChild(icon);

  const titleEl = document.createElement("span");
  titleEl.className = "tool-card-title";
  titleEl.textContent = titleForCard(kind, payload);
  header.appendChild(titleEl);

  const status = document.createElement("span");
  status.className = "tool-card-status tool-card-status-running";
  // Pulsing dot + "running" — clearer that work is happening, not failed.
  const dot = document.createElement("span");
  dot.className = "tool-card-status-dot";
  status.appendChild(dot);
  const statusText = document.createElement("span");
  statusText.textContent = "running";
  status.appendChild(statusText);
  header.appendChild(status);

  const toggle = document.createElement("button");
  toggle.className = "tool-card-toggle";
  toggle.textContent = "▾";
  header.appendChild(toggle);

  card.appendChild(header);

  const body = document.createElement("div");
  body.className = "tool-card-body";
  card.appendChild(body);

  const resultSlot = document.createElement("div");
  resultSlot.className = "tool-result-slot";
  card.appendChild(resultSlot);

  // JS-managed empty markers (replaces the :has-based CSS that doesn't
  // work consistently in WebKit). MutationObserver flips the class
  // whenever children are added or removed.
  const emptyClass = "tool-card-empty";
  const refreshEmpty = (el) => el.classList.toggle(emptyClass, el.childElementCount === 0);
  refreshEmpty(body);
  refreshEmpty(resultSlot);
  const observer = new MutationObserver(() => {
    refreshEmpty(body);
    refreshEmpty(resultSlot);
  });
  observer.observe(body, { childList: true });
  observer.observe(resultSlot, { childList: true });

  const toggleClick = (e) => {
    // Don't collapse when the user clicks selectable text inside the body
    // (e.g. trying to copy a command). Only the header toggles.
    if (!header.contains(e.target)) return;
    const collapsed = card.classList.toggle("tool-card-collapsed");
    toggle.textContent = collapsed ? "▸" : "▾";
    e.stopPropagation();
  };
  header.addEventListener("click", toggleClick);

  // Initial body content per kind
  populateInitial(kind, body, payload);

  card._titleEl = titleEl;
  card._body = body;
  card._statusEl = status;
  return card;
}

export function updateToolCard(card, kind, update) {
  if (!card) return;
  const body = card._body;
  const statusEl = card._statusEl;

  if (update.outputDelta != null) {
    let pre = body.querySelector("pre.tool-output");
    if (!pre) {
      pre = document.createElement("pre");
      pre.className = "tool-output";
      body.appendChild(pre);
    }
    pre.textContent = (pre.textContent || "") + update.outputDelta;
  }

  if (update.delta != null && kind === "plan") {
    const pre = body.querySelector("pre.tool-output") || (() => {
      const p = document.createElement("pre");
      p.className = "tool-output";
      body.appendChild(p);
      return p;
    })();
    pre.textContent = (pre.textContent || "") + update.delta;
  }

  if (update.complete) {
    const errored = !!update.payload?.error || !!update.payload?.is_error;
    // Replace pulsing dot + "running" with a static glyph + state label.
    statusEl.replaceChildren();
    const glyph = document.createElement("span");
    glyph.className = "tool-card-status-glyph";
    glyph.textContent = errored ? "✕" : "✓";
    statusEl.appendChild(glyph);
    const statusText = document.createElement("span");
    statusText.textContent = errored ? "error" : "done";
    statusEl.appendChild(statusText);
    statusEl.classList.remove("tool-card-status-running");
    statusEl.classList.toggle("tool-card-status-error", errored);
    statusEl.classList.toggle("tool-card-status-done", !errored);

    if (kind === "tool_result" && update.payload) {
      const text = extractResultText(update.payload);
      // If the body already has streamed output, replace it with the final
      // (often more complete) text so we don't duplicate.
      const existing = body.querySelector("pre.tool-output");
      if (existing) existing.remove();
      const pre = document.createElement("pre");
      pre.className = "tool-output";
      pre.textContent = String(text);
      body.appendChild(pre);
    }
  }
}

// Many tools return their result inside a JSON envelope like
// `{ "content": "...", "exit_code": 0 }`. Pulling the meaningful string
// out makes the chat readable; falls back to pretty-printed JSON when no
// recognizable shape is present.
function extractResultText(payload) {
  if (typeof payload === "string") return payload;
  if (!payload || typeof payload !== "object") return String(payload ?? "");
  if (typeof payload.text === "string") return payload.text;
  if (typeof payload.output === "string") return payload.output;
  if (typeof payload.content === "string") return payload.content;
  if (typeof payload.stdout === "string" || typeof payload.stderr === "string") {
    const out = payload.stdout || "";
    const err = payload.stderr ? `\n[stderr]\n${payload.stderr}` : "";
    return out + err;
  }
  if (Array.isArray(payload.content)) {
    return payload.content
      .map((c) => (typeof c === "string" ? c : c?.text || JSON.stringify(c)))
      .join("\n");
  }
  return JSON.stringify(payload, null, 2);
}

function collapse(card, on) {
  card.classList.toggle("tool-card-collapsed", !!on);
  const toggle = card.querySelector(".tool-card-toggle");
  if (toggle) toggle.textContent = on ? "▸" : "▾";
}

function iconForKind(kind) {
  switch (kind) {
    case "tool_call": return "⚙";
    case "mcp_tool_call": return "🔌";
    case "tool_result": return "↳";
    case "command_execution": return "$";
    case "file_change": return "✎";
    case "web_search": return "🔍";
    case "image_view": return "🖼";
    case "plan": return "▤";
    default: return "•";
  }
}

function titleForCard(kind, payload) {
  if (!payload) return kind;
  if (kind === "tool_call" || kind === "mcp_tool_call") {
    const name = payload.tool_name || payload.name || kind;
    // launchpad-agent's emit_turn_item for ToolCall puts the arguments in
    // `input` (see crates/server/src/runtime/execute_turn.rs). Older /
    // generic shapes use `arguments` / `params` so we accept all three.
    const args = payload.input || payload.arguments || payload.params || {};
    if (typeof args === "object" && args !== null) {
      const cmd = args.command || args.cmd || args.script;
      if (typeof cmd === "string") {
        return `${name} · ${truncate(cmd.split("\n")[0], 80)}`;
      }
      const path = args.path || args.file_path;
      if (typeof path === "string") {
        return `${name} · ${truncate(path, 80)}`;
      }
      const query = args.query || args.pattern;
      if (typeof query === "string") {
        return `${name} · ${truncate(query, 60)}`;
      }
    }
    return name;
  }
  if (kind === "command_execution") {
    return truncate(payload.command || payload.cmd || "command", 80);
  }
  if (kind === "file_change") {
    return payload.path || payload.file_path || "file change";
  }
  if (kind === "web_search") {
    return payload.query || "web search";
  }
  if (kind === "image_view") {
    return payload.path || "image";
  }
  if (kind === "plan") {
    return "plan";
  }
  if (kind === "tool_result") {
    return "result";
  }
  return kind;
}

function truncate(s, n) {
  if (typeof s !== "string") return String(s ?? "");
  return s.length > n ? s.slice(0, n - 1) + "…" : s;
}

function populateInitial(kind, body, payload) {
  if (!payload || typeof payload !== "object") return;
  if (kind === "tool_call" || kind === "mcp_tool_call") {
    const toolName = (payload.tool_name || payload.name || "").toLowerCase();
    const args = payload.input || payload.arguments || payload.params;
    // Specialized renderers for the file-mutation tools — better than dumping
    // the JSON args verbatim. Falls back to appendArgs for anything else.
    if (toolName === "apply_patch" && args && typeof args === "object") {
      const patchText =
        args.patchText || args.patch_text || args.patch || args.diff;
      if (typeof patchText === "string" && patchText.length > 0) {
        renderPatchPreview(body, patchText);
        return;
      }
    }
    if (toolName === "write" && args && typeof args === "object") {
      const filePath = args.filePath || args.file_path || args.path;
      const content = typeof args.content === "string" ? args.content : "";
      if (filePath) {
        renderWritePreview(body, filePath, content);
        return;
      }
    }
    if (args !== undefined && args !== null) {
      appendArgs(body, args);
    }
  } else if (kind === "file_change") {
    // Placeholder for the diff render — stream output deltas append to a <pre>.
    if (payload.diff) {
      const pre = document.createElement("pre");
      pre.className = "tool-output";
      pre.textContent = payload.diff;
      body.appendChild(pre);
    }
  } else if (kind === "command_execution") {
    if (payload.command) {
      const cmd = document.createElement("div");
      cmd.className = "tool-cmd";
      cmd.textContent = `$ ${payload.command}`;
      body.appendChild(cmd);
    }
  } else {
    // Fallback for any kind we don't have a special UI for — show the
    // raw payload so expanding the card always reveals something useful.
    appendArgs(body, payload);
  }
}

function appendArgs(body, args) {
  const pre = document.createElement("pre");
  pre.className = "tool-args";
  pre.textContent = typeof args === "string" ? args : JSON.stringify(args, null, 2);
  body.appendChild(pre);
}

// ─── Specialized tool-call previews ──────────────────────────────────────

// apply_patch: parse the unified diff in patchText and render with the
// shared diff renderer (same chrome the git panel uses). Adds a top-line
// stats summary so collapsed cards still convey scale.
function renderPatchPreview(body, patchText) {
  const files = parseUnifiedDiff(patchText);
  if (!files.length) {
    // Couldn't parse — fall back to the raw text so nothing is hidden.
    const pre = document.createElement("pre");
    pre.className = "tool-output";
    pre.textContent = patchText;
    body.appendChild(pre);
    return;
  }
  const stats = diffStats(files);
  const summary = document.createElement("div");
  summary.className = "tool-diff-summary";
  summary.innerHTML = `${files.length} file${files.length === 1 ? "" : "s"} · <span class="diff-add-count">+${stats.added}</span> <span class="diff-del-count">−${stats.removed}</span>`;
  body.appendChild(summary);
  const wrap = document.createElement("div");
  wrap.className = "tool-diff-body";
  wrap.innerHTML = files.map((f) => buildFileDiffSection(f)).join("");
  body.appendChild(wrap);
}

// write: show the destination path + a collapsible code preview of the
// content (capped) instead of the raw JSON args dump.
function renderWritePreview(body, filePath, content) {
  const head = document.createElement("div");
  head.className = "tool-write-head";
  head.textContent = filePath;
  body.appendChild(head);

  const lineCount = content ? content.split("\n").length : 0;
  const meta = document.createElement("div");
  meta.className = "tool-write-meta";
  meta.textContent = `${lineCount} line${lineCount === 1 ? "" : "s"} · ${content.length} chars`;
  body.appendChild(meta);

  if (!content) return;
  const pre = document.createElement("pre");
  pre.className = "tool-output tool-write-preview";
  // Cap at 2000 chars to avoid blowing up the message list on giant writes.
  const cap = 2000;
  pre.textContent = content.length > cap ? content.slice(0, cap) + "\n…" : content;
  body.appendChild(pre);
}

// ─── Approval card ────────────────────────────────────────────────────────

export function renderApproval(payload, opts) {
  const wrap = document.createElement("div");
  wrap.className = "agent-msg agent-msg-approval";

  const card = document.createElement("div");
  card.className = "approval-card";

  const title = document.createElement("div");
  title.className = "approval-title";
  title.textContent = payload?.action_summary || "Approval required";
  card.appendChild(title);

  if (payload?.justification) {
    const just = document.createElement("div");
    just.className = "approval-justification";
    just.textContent = payload.justification;
    card.appendChild(just);
  }

  const controls = document.createElement("div");
  controls.className = "approval-controls";

  const scopeSelect = document.createElement("select");
  scopeSelect.className = "approval-scope";
  ["once", "turn", "session", "path_prefix", "host", "tool"].forEach((s) => {
    const opt = document.createElement("option");
    opt.value = s;
    opt.textContent = s;
    scopeSelect.appendChild(opt);
  });

  const denyBtn = document.createElement("button");
  denyBtn.className = "approval-btn approval-deny";
  denyBtn.textContent = "Deny";

  const approveBtn = document.createElement("button");
  approveBtn.className = "approval-btn approval-approve";
  approveBtn.textContent = "Approve";

  controls.appendChild(scopeSelect);
  controls.appendChild(denyBtn);
  controls.appendChild(approveBtn);
  card.appendChild(controls);
  wrap.appendChild(card);

  approveBtn.addEventListener("click", () => opts.onRespond("approve", scopeSelect.value));
  denyBtn.addEventListener("click", () => opts.onRespond("deny", scopeSelect.value));

  return {
    el: wrap,
    collapse(decision, scope) {
      card.innerHTML = `<div class="approval-resolved">${decision} (scope: ${scope})</div>`;
    },
  };
}
