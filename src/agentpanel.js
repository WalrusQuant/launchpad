import { getProviders, getActiveProvider, addProvider, removeProvider, setActiveProvider, updateProviderModel, updateProvider, getPresets } from "./providers.js";
import { refreshFileBrowser } from "./filebrowser.js";
import { refreshPanel } from "./gitpanel.js";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

let panelVisible = false;
let messages = []; // { role: "user"|"assistant", content: string }
let isStreaming = false;
let currentStreamText = "";
let getCurrentPath = null;
let codeBlockCounter = 0;
let pendingToolCall = null; // Track in-flight tool call to prevent done/tool race

// Tool definitions sent to the model
const TOOLS_OPENAI = [
  {
    type: "function",
    function: {
      name: "read_file",
      description: "Read the contents of a file",
      parameters: {
        type: "object",
        properties: { path: { type: "string", description: "Absolute file path" } },
        required: ["path"],
      },
    },
  },
  {
    type: "function",
    function: {
      name: "write_file",
      description: "Write content to a file (creates or overwrites)",
      parameters: {
        type: "object",
        properties: {
          path: { type: "string", description: "Absolute file path" },
          content: { type: "string", description: "File content to write" },
        },
        required: ["path", "content"],
      },
    },
  },
  {
    type: "function",
    function: {
      name: "search_files",
      description: "Search for files by name in the project",
      parameters: {
        type: "object",
        properties: { query: { type: "string", description: "Search query" } },
        required: ["query"],
      },
    },
  },
  {
    type: "function",
    function: {
      name: "list_directory",
      description: "List files and folders in a directory",
      parameters: {
        type: "object",
        properties: { path: { type: "string", description: "Directory path" } },
        required: ["path"],
      },
    },
  },
];

const TOOLS_ANTHROPIC = TOOLS_OPENAI.map((t) => ({
  name: t.function.name,
  description: t.function.description,
  input_schema: t.function.parameters,
}));

export function initAgentPanel(getPath) {
  getCurrentPath = getPath;

  const toggleBtn = document.getElementById("toggle-agent-panel");
  toggleBtn.addEventListener("click", () => togglePanel());

  document.addEventListener("keydown", (e) => {
    if (e.metaKey && e.key === "i") {
      e.preventDefault();
      togglePanel();
    }
  });

  // Listen for streaming chunks
  listen("agent-chunk", (event) => {
    handleChunk(event.payload);
  });

  // Send button
  document.getElementById("agent-send").addEventListener("click", sendMessage);

  // Input enter key
  document.getElementById("agent-input").addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      sendMessage();
    }
    e.stopPropagation();
  });

  // Clear button
  document.getElementById("agent-clear").addEventListener("click", () => {
    messages = [];
    renderMessages();
  });

  // Provider selector
  document.getElementById("agent-provider-select").addEventListener("change", (e) => {
    setActiveProvider(e.target.value);
    updateModelSelect();
  });

  // Model selector
  document.getElementById("agent-model-select").addEventListener("change", (e) => {
    const provider = getActiveProvider();
    if (provider) updateProviderModel(provider.id, e.target.value);
  });

  // Add / edit provider buttons
  document.getElementById("agent-add-provider").addEventListener("click", () => showProviderDialog(null));
  document.getElementById("agent-edit-provider").addEventListener("click", () => {
    const provider = getActiveProvider();
    if (provider) showProviderDialog(provider);
  });

  updateProviderSelect();
}

// Called by main.js to register the tab switching function
let switchToAgentTab = null;
let switchAwayFromAgent = null;

export function setTabCallbacks(showAgent, hideAgent) {
  switchToAgentTab = showAgent;
  switchAwayFromAgent = hideAgent;
}

export function togglePanel() {
  panelVisible = !panelVisible;
  const bar = document.getElementById("agent-bar");
  const btn = document.getElementById("toggle-agent-panel");

  if (panelVisible) {
    bar.style.display = "block";
    btn.classList.add("active");
    updateProviderSelect();
    if (switchToAgentTab) switchToAgentTab();
    document.getElementById("agent-input").focus();
  } else {
    bar.style.display = "none";
    btn.classList.remove("active");
    if (switchAwayFromAgent) switchAwayFromAgent();
  }
}

export function isAgentVisible() {
  return panelVisible;
}

function updateProviderSelect() {
  const select = document.getElementById("agent-provider-select");
  const providers = getProviders();
  const active = getActiveProvider();

  select.innerHTML = "";

  if (providers.length === 0) {
    select.innerHTML = '<option value="">No providers configured</option>';
    return;
  }

  providers.forEach((p) => {
    const opt = document.createElement("option");
    opt.value = p.id;
    opt.textContent = p.name;
    opt.selected = active && p.id === active.id;
    select.appendChild(opt);
  });

  updateModelSelect();
}

function updateModelSelect() {
  const select = document.getElementById("agent-model-select");
  const provider = getActiveProvider();

  select.innerHTML = "";

  if (!provider) {
    select.innerHTML = '<option value="">—</option>';
    return;
  }

  const models = provider.models || [];

  if (models.length === 0) {
    select.innerHTML = '<option value="">No models — click ⚙ to add</option>';
    return;
  }

  models.forEach((m) => {
    const opt = document.createElement("option");
    opt.value = m;
    opt.textContent = m;
    opt.selected = m === provider.model;
    select.appendChild(opt);
  });
}

function buildSystemPrompt() {
  const cwd = getCurrentPath ? getCurrentPath() : "unknown";
  return `You are a coding assistant. The user's current working directory is: ${cwd}

You have tools to read files, write files, search for files, list directories, and run shell commands. Use them when the user asks you to work with their code.

Be concise and direct. When showing code, use fenced code blocks with the language specified.`;
}

async function sendMessage() {
  const input = document.getElementById("agent-input");
  const text = input.value.trim();
  if (!text || isStreaming) return;

  const provider = getActiveProvider();
  if (!provider) {
    alert("Please add a provider first (click + button)");
    return;
  }

  // Add user message
  messages.push({ role: "user", content: text });
  input.value = "";
  renderMessages();

  await callAgent(provider);
}

async function callAgent(provider) {
  isStreaming = true;
  currentStreamText = "";
  renderMessages(); // Show typing indicator

  // Build messages for API
  const apiMessages = [
    ...(provider.type === "anthropic"
      ? [] // Anthropic uses system param separately
      : [{ role: "system", content: buildSystemPrompt() }]),
    ...messages.map((m) => ({ role: m.role, content: m.content })),
  ];

  const tools = provider.type === "anthropic" ? TOOLS_ANTHROPIC : TOOLS_OPENAI;

  try {
    await invoke("agent_chat_stream", {
      providerType: provider.type,
      baseUrl: provider.baseUrl,
      apiKey: provider.apiKey,
      model: provider.model,
      messages: apiMessages,
      tools,
      system: provider.type === "anthropic" ? buildSystemPrompt() : null,
    });
  } catch (err) {
    isStreaming = false;
    messages.push({ role: "assistant", content: `Error: ${err}` });
    renderMessages();
  }
}

function handleChunk(chunk) {
  if (chunk.type === "text") {
    currentStreamText += chunk.content;
    renderMessages();
  } else if (chunk.type === "tool_call") {
    // Show tool call in messages
    const toolMsg = `🔧 Calling \`${chunk.tool_name}\`...`;
    currentStreamText += `\n${toolMsg}\n`;
    renderMessages();

    // Execute the tool — track the promise so done doesn't race ahead
    pendingToolCall = executeToolCall(chunk.tool_name, chunk.tool_args, chunk.tool_call_id);
  } else if (chunk.type === "done") {
    // Wait for any in-flight tool call to finish before finalizing
    const finalize = () => {
      if (currentStreamText) {
        messages.push({ role: "assistant", content: currentStreamText });
        currentStreamText = "";
      }
      isStreaming = false;
      renderMessages();
    };
    if (pendingToolCall) {
      pendingToolCall.then(finalize).catch(finalize);
      pendingToolCall = null;
    } else {
      finalize();
    }
  } else if (chunk.type === "error") {
    messages.push({ role: "assistant", content: `Error: ${chunk.content}` });
    isStreaming = false;
    renderMessages();
  }
}

async function executeToolCall(toolName, toolArgsJson, toolCallId) {
  let result = "";
  try {
    const args = JSON.parse(toolArgsJson || "{}");
    const cwd = getCurrentPath ? getCurrentPath() : "";

    // Resolve relative paths against the current working directory
    const resolvePath = (p) => {
      if (!p) return cwd;
      if (p.startsWith("/")) return p;
      return cwd + "/" + p;
    };

    switch (toolName) {
      case "read_file":
        result = await invoke("read_file_preview", { path: resolvePath(args.path), maxBytes: 32768 });
        break;
      case "write_file":
        await invoke("write_file", { path: resolvePath(args.path), content: args.content });
        result = `File written: ${resolvePath(args.path)}`;
        break;
      case "search_files":
        const files = await invoke("search_files", { root: cwd, query: args.query, maxResults: 20 });
        result = files.join("\n") || "No files found";
        break;
      case "list_directory":
        const entries = await invoke("read_directory", { path: resolvePath(args.path) });
        result = entries.map((e) => `${e.is_dir ? "📁" : "📄"} ${e.name}`).join("\n");
        break;
      default:
        result = `Unknown tool: ${toolName}`;
    }
  } catch (err) {
    result = `Tool error: ${err}`;
  }

  // Refresh file browser and git panel after mutating operations
  if (toolName === "write_file") {
    refreshFileBrowser();
    refreshPanel(null, true);
  }

  // Show result in stream
  currentStreamText += `\n📋 ${result.substring(0, 500)}${result.length > 500 ? "..." : ""}\n`;
  renderMessages();

  // Send tool result back by continuing the conversation
  const provider = getActiveProvider();
  if (provider) {
    // Add the assistant's partial message + tool result to messages
    if (currentStreamText) {
      messages.push({ role: "assistant", content: currentStreamText });
      currentStreamText = "";
    }

    // Add tool result as user message (simplified — proper implementation would use tool_result role)
    messages.push({
      role: "user",
      content: `[Tool result for ${toolName}]: ${result}`,
    });

    // Continue the conversation
    await callAgent(provider);
  }
}

function renderMessages() {
  const container = document.getElementById("agent-messages");
  codeBlockCounter = 0; // Reset counter for stable IDs across re-renders

  let html = "";

  messages.forEach((msg) => {
    const cls = msg.role === "user" ? "agent-msg-user" : "agent-msg-assistant";
    const label = msg.role === "user" ? "You" : "Agent";
    const content = msg.role === "user" ? escapeHtml(msg.content) : formatMessage(msg.content);
    html += `<div class="agent-msg ${cls}">
      <div class="agent-msg-label">${label}</div>
      <div class="agent-msg-content">${content}</div>
    </div>`;
  });

  // Show streaming text — use escapeHtml only (no markdown formatting) to avoid
  // partial code fence regex matches causing content jumps mid-stream
  if (isStreaming && currentStreamText) {
    html += `<div class="agent-msg agent-msg-assistant">
      <div class="agent-msg-label">Agent</div>
      <div class="agent-msg-content">${escapeHtml(currentStreamText)}<span class="agent-cursor">▊</span></div>
    </div>`;
  } else if (isStreaming) {
    html += `<div class="agent-msg agent-msg-assistant">
      <div class="agent-msg-label">Agent</div>
      <div class="agent-msg-content"><span class="agent-typing">Thinking...</span></div>
    </div>`;
  }

  container.innerHTML = html;
  container.scrollTop = container.scrollHeight;
}

function showProviderDialog(existingProvider) {
  const overlay = document.getElementById("provider-dialog");
  const title = document.getElementById("provider-dialog-title");
  const presetList = document.getElementById("provider-preset-list");
  const modelsSection = document.getElementById("provider-models-section");
  const modelsList = document.getElementById("provider-models-list");
  const modelAddInput = document.getElementById("provider-model-add-input");
  const modelAddBtn = document.getElementById("provider-model-add-btn");
  const saveBtn = document.getElementById("provider-save-btn");
  const deleteBtn = document.getElementById("provider-delete-btn");
  const nameInput = document.getElementById("provider-name-input");
  const keyInput = document.getElementById("provider-key-input");
  const urlInput = document.getElementById("provider-url-input");
  const typeInput = document.getElementById("provider-type-input");

  const isEdit = !!existingProvider;

  // Track models in dialog
  let dialogModels = isEdit ? [...(existingProvider.models || [])] : [];

  title.textContent = isEdit ? "Edit Provider" : "Add Provider";
  saveBtn.textContent = isEdit ? "Save" : "Add Provider";
  deleteBtn.style.display = isEdit ? "inline-block" : "none";
  presetList.style.display = isEdit ? "none" : "flex";
  modelsSection.style.display = isEdit ? "block" : "none";

  // Fill fields
  nameInput.value = isEdit ? existingProvider.name : "";
  keyInput.value = isEdit ? existingProvider.apiKey : "";
  urlInput.value = isEdit ? existingProvider.baseUrl : "";
  typeInput.value = isEdit ? existingProvider.type : "openai";
  modelAddInput.value = "";

  function renderModelsList() {
    modelsList.innerHTML = "";
    if (dialogModels.length === 0) {
      modelsList.innerHTML = '<div class="provider-no-models">No models added yet</div>';
      return;
    }
    dialogModels.forEach((m, i) => {
      const row = document.createElement("div");
      row.className = "provider-model-row";
      row.innerHTML = `<span class="provider-model-name">${m}</span>`;
      const removeBtn = document.createElement("button");
      removeBtn.className = "provider-model-remove";
      removeBtn.textContent = "×";
      removeBtn.addEventListener("click", () => {
        dialogModels.splice(i, 1);
        renderModelsList();
      });
      row.appendChild(removeBtn);
      modelsList.appendChild(row);
    });
  }

  renderModelsList();

  // Add model button
  const addModel = () => {
    const val = modelAddInput.value.trim();
    if (val && !dialogModels.includes(val)) {
      dialogModels.push(val);
      modelAddInput.value = "";
      renderModelsList();
    }
  };

  modelAddBtn.onclick = addModel;
  modelAddInput.onkeydown = (e) => {
    if (e.key === "Enter") { e.preventDefault(); addModel(); }
    e.stopPropagation();
  };

  overlay.classList.add("visible");

  // Preset buttons (add mode only)
  if (!isEdit) {
    presetList.innerHTML = "";
    getPresets().forEach((preset) => {
      const btn = document.createElement("button");
      btn.className = "provider-preset-btn";
      btn.textContent = preset.name;
      btn.addEventListener("click", () => {
        nameInput.value = preset.name;
        urlInput.value = preset.baseUrl;
        typeInput.value = preset.type;
        // Pre-fill models from preset
        dialogModels = [...preset.models];
        modelsSection.style.display = "block";
        renderModelsList();
      });
      presetList.appendChild(btn);
    });
  }

  saveBtn.onclick = async () => {
    // Auto-add any model typed in the input field
    const pendingModel = modelAddInput.value.trim();
    if (pendingModel && !dialogModels.includes(pendingModel)) {
      dialogModels.push(pendingModel);
    }

    const name = nameInput.value.trim();
    const apiKey = keyInput.value.trim();
    const baseUrl = urlInput.value.trim();
    const type = typeInput.value.trim();

    if (!name || !apiKey || !baseUrl) {
      alert("Name, API key, and base URL are required");
      return;
    }

    if (isEdit) {
      await updateProvider(existingProvider.id, {
        name, apiKey, baseUrl, type,
        models: dialogModels,
        model: dialogModels.includes(existingProvider.model) ? existingProvider.model : dialogModels[0] || "",
      });
    } else {
      await addProvider({
        name, type: type || "openai", baseUrl, apiKey,
        models: dialogModels,
        model: dialogModels[0] || "",
      });
    }
    overlay.classList.remove("visible");
    updateProviderSelect();
  };

  deleteBtn.onclick = async () => {
    if (isEdit) {
      await removeProvider(existingProvider.id);
      overlay.classList.remove("visible");
      updateProviderSelect();
    }
  };

  document.getElementById("provider-cancel-btn").onclick = () => {
    overlay.classList.remove("visible");
  };
}

function escapeHtml(str) {
  return str
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/\n/g, "<br>");
}

function formatMessage(str) {
  // Escape HTML first
  let text = str
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");

  // Replace fenced code blocks: ```lang\ncode\n```
  text = text.replace(/```(\w*)\n([\s\S]*?)```/g, (match, lang, code) => {
    const id = "code-" + (codeBlockCounter++);
    return `<div class="agent-code-block">
      <div class="agent-code-header">
        <span class="agent-code-lang">${lang || "code"}</span>
        <button class="agent-code-copy" onclick="navigator.clipboard.writeText(document.getElementById('${id}').textContent)">Copy</button>
      </div>
      <pre class="agent-code" id="${id}">${code}</pre>
    </div>`;
  });

  // Replace inline code: `code`
  text = text.replace(/`([^`]+)`/g, '<code class="agent-inline-code">$1</code>');

  // Replace **bold**
  text = text.replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>");

  // Newlines to <br> (but not inside code blocks)
  text = text.replace(/\n/g, "<br>");

  return text;
}
