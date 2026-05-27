import { ACTIONS, getBinding, setBinding, getDefault, chordFromEvent } from "./keymap.js";
import { getActiveProject } from "./projects.js";

const { invoke } = window.__TAURI__.core;

/**
 * Settings panel UI — renders a form with sections for all app settings.
 * Changes fire immediately via the onSettingChange callback.
 */

export function createSettingsPanel(containerEl, settings, onSettingChange) {
  const content = document.createElement("div");
  content.className = "settings-content";

  content.innerHTML = `
    <h2 class="settings-title">Settings</h2>

    <div class="settings-section">
      <h3 class="settings-section-title">General</h3>

      <div class="settings-row">
        <label class="settings-label" for="set-appTheme">Theme</label>
        <select class="settings-select" id="set-appTheme">
          <option value="auto" ${settings.appTheme === "auto" ? "selected" : ""}>System</option>
          <option value="dark" ${settings.appTheme === "dark" ? "selected" : ""}>Dark</option>
          <option value="light" ${settings.appTheme === "light" ? "selected" : ""}>Light</option>
        </select>
      </div>

      <div class="settings-row">
        <label class="settings-label" for="set-sidebarWidth">Sidebar Width</label>
        <div class="settings-input-group">
          <input class="settings-input settings-input-sm" id="set-sidebarWidth" type="number"
            min="150" max="500" value="${settings.sidebarWidth}" />
          <span class="settings-unit">px</span>
        </div>
      </div>
    </div>

    <div class="settings-section">
      <h3 class="settings-section-title">Terminal</h3>

      <div class="settings-row">
        <label class="settings-label" for="set-termFontSize">Font Size</label>
        <div class="settings-input-group">
          <input class="settings-input settings-input-sm" id="set-termFontSize" type="number"
            min="10" max="24" value="${settings.termFontSize}" />
          <span class="settings-unit">px</span>
        </div>
      </div>

      <div class="settings-row">
        <label class="settings-label" for="set-termFontFamily">Font Family</label>
        <select class="settings-select" id="set-termFontFamily">
          <option value='"SF Mono", "Menlo", "Monaco", "Courier New", monospace' ${isSelected(settings.termFontFamily, '"SF Mono"')}>SF Mono</option>
          <option value='"Menlo", monospace' ${isSelected(settings.termFontFamily, '"Menlo"')}>Menlo</option>
          <option value='"Monaco", monospace' ${isSelected(settings.termFontFamily, '"Monaco"')}>Monaco</option>
          <option value='"Courier New", monospace' ${isSelected(settings.termFontFamily, '"Courier New"')}>Courier New</option>
          <option value='"Fira Code", monospace' ${isSelected(settings.termFontFamily, '"Fira Code"')}>Fira Code</option>
          <option value='"JetBrains Mono", monospace' ${isSelected(settings.termFontFamily, '"JetBrains Mono"')}>JetBrains Mono</option>
        </select>
      </div>

      <div class="settings-row">
        <label class="settings-label" for="set-termScrollback">Scrollback Lines</label>
        <input class="settings-input settings-input-sm" id="set-termScrollback" type="number"
          min="1000" max="100000" step="1000" value="${settings.termScrollback}" />
      </div>

      <div class="settings-row">
        <label class="settings-label" for="set-termCursorStyle">Cursor Style</label>
        <select class="settings-select" id="set-termCursorStyle">
          <option value="bar" ${settings.termCursorStyle === "bar" ? "selected" : ""}>Bar</option>
          <option value="block" ${settings.termCursorStyle === "block" ? "selected" : ""}>Block</option>
          <option value="underline" ${settings.termCursorStyle === "underline" ? "selected" : ""}>Underline</option>
        </select>
      </div>

      <div class="settings-row">
        <label class="settings-label" for="set-termCursorBlink">Cursor Blink</label>
        <label class="settings-toggle">
          <input type="checkbox" id="set-termCursorBlink" ${settings.termCursorBlink ? "checked" : ""} />
          <span class="settings-toggle-slider"></span>
        </label>
      </div>
    </div>

    <div class="settings-section">
      <h3 class="settings-section-title">Editor</h3>

      <div class="settings-row">
        <label class="settings-label" for="set-editorFontSize">Font Size</label>
        <div class="settings-input-group">
          <input class="settings-input settings-input-sm" id="set-editorFontSize" type="number"
            min="10" max="24" value="${settings.editorFontSize}" />
          <span class="settings-unit">px</span>
        </div>
      </div>

      <div class="settings-row">
        <label class="settings-label" for="set-editorTabSize">Tab Size</label>
        <select class="settings-select" id="set-editorTabSize">
          <option value="2" ${settings.editorTabSize === 2 ? "selected" : ""}>2 spaces</option>
          <option value="4" ${settings.editorTabSize === 4 ? "selected" : ""}>4 spaces</option>
          <option value="8" ${settings.editorTabSize === 8 ? "selected" : ""}>8 spaces</option>
        </select>
      </div>

      <div class="settings-row">
        <label class="settings-label" for="set-editorWordWrap">Word Wrap</label>
        <label class="settings-toggle">
          <input type="checkbox" id="set-editorWordWrap" ${settings.editorWordWrap ? "checked" : ""} />
          <span class="settings-toggle-slider"></span>
        </label>
      </div>

      <div class="settings-row">
        <label class="settings-label" for="set-editorVimMode">Vim Mode</label>
        <label class="settings-toggle">
          <input type="checkbox" id="set-editorVimMode" ${settings.editorVimMode ? "checked" : ""} />
          <span class="settings-toggle-slider"></span>
        </label>
      </div>
    </div>

    <div class="settings-section">
      <h3 class="settings-section-title">Git</h3>

      <div class="settings-row">
        <label class="settings-label" for="set-gitPollInterval">Auto-refresh Interval</label>
        <div class="settings-input-group">
          <input class="settings-input settings-input-sm" id="set-gitPollInterval" type="number"
            min="1" max="30" value="${settings.gitPollInterval}" />
          <span class="settings-unit">seconds</span>
        </div>
      </div>

      <div class="settings-row">
        <label class="settings-label" for="set-gitDefaultPrefix">Default Commit Prefix</label>
        <select class="settings-select" id="set-gitDefaultPrefix">
          <option value="" ${settings.gitDefaultPrefix === "" ? "selected" : ""}>None</option>
          <option value="feat: " ${settings.gitDefaultPrefix === "feat: " ? "selected" : ""}>feat:</option>
          <option value="fix: " ${settings.gitDefaultPrefix === "fix: " ? "selected" : ""}>fix:</option>
          <option value="chore: " ${settings.gitDefaultPrefix === "chore: " ? "selected" : ""}>chore:</option>
          <option value="docs: " ${settings.gitDefaultPrefix === "docs: " ? "selected" : ""}>docs:</option>
          <option value="refactor: " ${settings.gitDefaultPrefix === "refactor: " ? "selected" : ""}>refactor:</option>
        </select>
      </div>
    </div>

    <div class="settings-section" id="env-section" ${getActiveProject() ? "" : "hidden"}>
      <h3 class="settings-section-title">
        Environment
        <span class="settings-section-sub" id="env-project-name"></span>
      </h3>
      <div class="settings-env-help">
        Per-project environment variables. Injected into every new terminal for this project.
      </div>
      <div class="settings-env-note">
        <span class="settings-env-note-icon">ⓘ</span>
        <span>After adding or changing a variable, <strong>open a new terminal</strong> for your agent to see it. Existing terminals and any agents already running in them keep their original environment.</span>
      </div>
      <div id="env-var-list" class="env-var-list"></div>
      <button type="button" class="settings-btn env-add-btn" id="env-add-btn">+ Add variable</button>
    </div>

    <div class="settings-section" id="agent-section">
      <h3 class="settings-section-title">Agent</h3>
      <div class="settings-env-help">
        Provider settings for the built-in chat agent. Stored at
        <code>~/.launchpad/agent-config.json</code> (chmod 0o600).
      </div>
      <div class="settings-env-note">
        <span class="settings-env-note-icon">ⓘ</span>
        <span>Provider changes take effect after <strong>Save &amp; reload</strong>. Existing agent tabs will reconnect on their next message.</span>
      </div>
      <div class="settings-row">
        <label class="settings-label" for="set-agentProvider">Provider</label>
        <select class="settings-select" id="set-agentProvider">
          <option value="">— select —</option>
        </select>
      </div>
      <div id="agent-provider-desc" class="settings-env-help" style="margin-top:-6px"></div>
      <div class="settings-row">
        <label class="settings-label" for="set-agentModel">Default model</label>
        <input class="settings-input" id="set-agentModel" type="text" placeholder="e.g. claude-sonnet-4-20250514" />
      </div>
      <div class="settings-row">
        <label class="settings-label" for="set-agentBaseUrl">Base URL</label>
        <input class="settings-input" id="set-agentBaseUrl" type="text" placeholder="auto-filled from provider" />
      </div>
      <div class="settings-row">
        <label class="settings-label" for="set-agentApiKey">API key</label>
        <input class="settings-input" id="set-agentApiKey" type="password" placeholder="sk-..." autocomplete="off" />
      </div>
      <div class="settings-row">
        <label class="settings-label" for="set-agentApprovalPolicy">Approval</label>
        <select class="settings-select" id="set-agentApprovalPolicy">
          <option value="interactive">Interactive (ask before each tool call)</option>
          <option value="auto-approve">Auto-approve (no prompts)</option>
          <option value="deny">Deny (block all tool calls)</option>
        </select>
      </div>
      <div class="settings-row">
        <label class="settings-label" for="set-agentSandboxMode">Sandbox</label>
        <select class="settings-select" id="set-agentSandboxMode">
          <option value="workspace-write">Workspace writes (default)</option>
          <option value="read-only">Read-only (no writes, no shell)</option>
          <option value="unrestricted">Unrestricted (no sandbox)</option>
        </select>
      </div>
      <input type="hidden" id="set-agentWireApi" />
      <button type="button" class="settings-btn" id="agent-save-btn">Save</button>
      <button type="button" class="settings-btn" id="agent-save-reload-btn">Save &amp; reload</button>
      <span id="agent-save-feedback" class="settings-feedback"></span>
    </div>

    <div class="settings-section">
      <h3 class="settings-section-title">Keybindings</h3>
      <div id="keybindings-list"></div>
    </div>

    <div class="settings-footer">
      Settings are saved automatically. Config stored at <code>~/.launchpad/config.json</code>
    </div>
  `;

  containerEl.appendChild(content);
  renderKeybindings(content.querySelector("#keybindings-list"));
  renderProjectEnv(content);
  renderAgentConfig(content);

  // Wire up all inputs
  wireInput("appTheme", "change", (v) => v);
  wireInput("sidebarWidth", "input", (v) => parseInt(v) || 260);
  wireInput("termFontSize", "input", (v) => parseInt(v) || 13);
  wireInput("termFontFamily", "change", (v) => v);
  wireInput("termScrollback", "input", (v) => parseInt(v) || 10000);
  wireInput("termCursorStyle", "change", (v) => v);
  wireCheckbox("termCursorBlink");
  wireInput("editorFontSize", "input", (v) => parseInt(v) || 12);
  wireInput("editorTabSize", "change", (v) => parseInt(v) || 2);
  wireCheckbox("editorWordWrap");
  wireCheckbox("editorVimMode");
  wireInput("gitPollInterval", "input", (v) => parseInt(v) || 3);
  wireInput("gitDefaultPrefix", "change", (v) => v);

  function wireInput(key, event, transform) {
    const el = content.querySelector(`#set-${key}`);
    if (!el) return;
    el.addEventListener(event, () => {
      onSettingChange(key, transform(el.value));
    });
  }

  function wireCheckbox(key) {
    const el = content.querySelector(`#set-${key}`);
    if (!el) return;
    el.addEventListener("change", () => {
      onSettingChange(key, el.checked);
    });
  }
}

function renderKeybindings(listEl) {
  listEl.innerHTML = "";
  for (const action of ACTIONS) {
    const row = document.createElement("div");
    row.className = "settings-row kb-row";

    const label = document.createElement("label");
    label.className = "settings-label";
    label.textContent = action.label;
    row.appendChild(label);

    const right = document.createElement("div");
    right.className = "kb-right";

    const input = document.createElement("input");
    input.className = "settings-input kb-input";
    input.type = "text";
    input.readOnly = true;
    input.value = getBinding(action.id);
    input.placeholder = "Click then press keys";
    input.title = "Click, then press the new chord. Esc to cancel.";

    input.addEventListener("focus", () => {
      input.classList.add("kb-capturing");
      input.value = "Press keys…";
    });
    input.addEventListener("blur", () => {
      input.classList.remove("kb-capturing");
      input.value = getBinding(action.id);
    });
    input.addEventListener("keydown", async (e) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape") {
        input.blur();
        return;
      }
      const chord = chordFromEvent(e);
      if (!chord) return;
      await setBinding(action.id, chord);
      input.value = chord;
      input.blur();
    });

    const reset = document.createElement("button");
    reset.className = "settings-input-sm kb-reset";
    reset.type = "button";
    reset.textContent = "Reset";
    reset.title = `Default: ${getDefault(action.id)}`;
    reset.addEventListener("click", async () => {
      await setBinding(action.id, getDefault(action.id));
      input.value = getBinding(action.id);
    });

    right.appendChild(input);
    right.appendChild(reset);
    row.appendChild(right);
    listEl.appendChild(row);
  }
}

function isSelected(current, match) {
  return current.startsWith(match) ? "selected" : "";
}

function escapeAttr(str) {
  return str.replace(/"/g, "&quot;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

// ─── Per-project environment variables ───────────────────────────────────────
// Loads vars for the active project from ~/.launchpad/project-env.json, renders
// editable rows, and saves the full list on every change (debounced).
// Keys are validated client-side against POSIX env var rules; the Rust side
// re-validates on save so the JSON file can never contain bad keys.

const ENV_KEY_RE = /^[A-Za-z_][A-Za-z0-9_]*$/;

async function renderAgentConfig(content) {
  const providerSel = content.querySelector("#set-agentProvider");
  const wireHidden = content.querySelector("#set-agentWireApi");
  const descEl = content.querySelector("#agent-provider-desc");
  const modelInput = content.querySelector("#set-agentModel");
  const baseUrlInput = content.querySelector("#set-agentBaseUrl");
  const apiKeyInput = content.querySelector("#set-agentApiKey");
  const approvalSel = content.querySelector("#set-agentApprovalPolicy");
  const sandboxSel = content.querySelector("#set-agentSandboxMode");
  const saveBtn = content.querySelector("#agent-save-btn");
  const feedback = content.querySelector("#agent-save-feedback");
  if (!saveBtn) return;

  // Load presets and current config in parallel.
  let presets = [];
  let cfg = {};
  try {
    [presets, cfg] = await Promise.all([
      invoke("agent_provider_presets"),
      invoke("agent_config_load"),
    ]);
  } catch (_) {}

  // Build the provider dropdown.
  for (const p of presets) {
    const opt = document.createElement("option");
    opt.value = p.id;
    opt.textContent = p.display_name;
    opt.dataset.wireApi = p.wire_api;
    opt.dataset.baseUrl = p.default_base_url || "";
    opt.dataset.description = p.description || "";
    opt.dataset.envVars = (p.api_key_env_vars || []).join(",");
    opt.dataset.defaultModel = p.default_model || "";
    providerSel.appendChild(opt);
  }

  function applyProvider(id, { resetBaseUrl, resetModel } = {}) {
    const opt = providerSel.querySelector(`option[value="${id}"]`);
    if (!opt) {
      descEl.textContent = "";
      return;
    }
    wireHidden.value = opt.dataset.wireApi || "";
    descEl.textContent = opt.dataset.description || "";
    if (resetBaseUrl) {
      baseUrlInput.value = opt.dataset.baseUrl || "";
    }
    // Auto-fill the model when the user actively switches providers, so
    // stale slugs from the previous preset don't blow up the next request.
    // Existing edits are preserved on the initial restore pass.
    if (resetModel) {
      modelInput.value = opt.dataset.defaultModel || "";
    }
    const envVars = opt.dataset.envVars ? opt.dataset.envVars.split(",") : [];
    apiKeyInput.placeholder =
      cfg.api_key && cfg.provider === id
        ? "•••••• (saved — overwrite to change)"
        : envVars.length
        ? `paste here, or set ${envVars[0]} in your shell`
        : "no API key required";
  }

  // Restore saved values.
  if (cfg.provider) providerSel.value = cfg.provider;
  if (cfg.model) modelInput.value = cfg.model;
  if (cfg.base_url) baseUrlInput.value = cfg.base_url;
  if (approvalSel) {
    approvalSel.value = cfg.default_approval_policy || "interactive";
  }
  if (sandboxSel) {
    sandboxSel.value = cfg.default_sandbox_mode || "workspace-write";
  }
  applyProvider(providerSel.value, { resetBaseUrl: !cfg.base_url });

  // When the user picks a different provider, replace the base URL + model
  // with the preset defaults and clear the API key field (key is
  // provider-specific). Without resetModel, stale slugs from the previous
  // preset (e.g. anthropic's "claude-sonnet" against an OpenAI base URL) hit
  // the new provider and produce a 400.
  providerSel.addEventListener("change", () => {
    apiKeyInput.value = "";
    applyProvider(providerSel.value, { resetBaseUrl: true, resetModel: true });
  });

  const reloadBtn = content.querySelector("#agent-save-reload-btn");

  function showFeedback(msg, kind) {
    feedback.textContent = msg;
    feedback.classList.toggle("settings-feedback-ok", kind === "ok");
    feedback.classList.toggle("settings-feedback-err", kind === "err");
    setTimeout(() => {
      feedback.textContent = "";
      feedback.classList.remove("settings-feedback-ok", "settings-feedback-err");
    }, 4000);
  }

  async function saveCurrent() {
    const next = {
      provider: providerSel.value || null,
      wire_api: wireHidden.value || null,
      model: modelInput.value.trim() || null,
      base_url: baseUrlInput.value.trim() || null,
      // Only overwrite api_key if the user typed something. Otherwise keep
      // the saved one (only when the provider hasn't changed).
      api_key:
        apiKeyInput.value.trim() ||
        (cfg.provider === providerSel.value ? cfg.api_key : null) ||
        null,
      default_approval_policy: approvalSel ? approvalSel.value : null,
      default_sandbox_mode: sandboxSel ? sandboxSel.value : null,
    };
    await invoke("agent_config_save", { cfg: next });
    cfg = next;
    apiKeyInput.value = "";
    applyProvider(next.provider, { resetBaseUrl: false });
    return next;
  }

  saveBtn.addEventListener("click", async () => {
    try {
      await saveCurrent();
      showFeedback("Saved. Open or reopen an agent tab to use the new settings.", "ok");
    } catch (err) {
      showFeedback(`Failed: ${err}`, "err");
    }
  });

  reloadBtn.addEventListener("click", async () => {
    reloadBtn.disabled = true;
    try {
      await saveCurrent();
      await invoke("agent_reload");
      // Notify any open agent tabs in this window so they can drop their
      // stale session_id and reconnect on next message.
      window.dispatchEvent(new CustomEvent("launchpad:agent-reloaded"));
      showFeedback("Reloaded. Active agent tabs will reconnect on next message.", "ok");
    } catch (err) {
      showFeedback(`Reload failed: ${err}`, "err");
    } finally {
      reloadBtn.disabled = false;
    }
  });
}

async function renderProjectEnv(content) {
  const section = content.querySelector("#env-section");
  const listEl = content.querySelector("#env-var-list");
  const addBtn = content.querySelector("#env-add-btn");
  const nameEl = content.querySelector("#env-project-name");
  if (!section || !listEl || !addBtn) return;

  const project = getActiveProject();
  if (!project) {
    section.hidden = true;
    return;
  }
  section.hidden = false;
  nameEl.textContent = `— ${project.name}`;

  // Local row state — source of truth while the panel is open.
  // Flushed to disk on every mutation, debounced.
  let rows = [];
  try {
    rows = await invoke("load_project_env_vars", { path: project.path });
  } catch (e) {
    console.warn("load_project_env_vars failed:", e);
    rows = [];
  }

  let saveTimer = null;
  function scheduleSave() {
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(save, 300);
  }
  async function save() {
    saveTimer = null;
    // Skip rows with invalid or empty keys — UI already shows errors; we never
    // silently write bad data, but we also don't block saves of valid rows
    // because the user may be mid-edit on one row.
    const clean = rows.filter((r) => r.key && ENV_KEY_RE.test(r.key));
    try {
      await invoke("save_project_env_vars", { path: project.path, vars: clean });
    } catch (e) {
      console.warn("save_project_env_vars failed:", e);
    }
  }

  function render() {
    listEl.innerHTML = "";
    if (rows.length === 0) {
      const empty = document.createElement("div");
      empty.className = "env-empty";
      empty.textContent = "No variables set for this project.";
      listEl.appendChild(empty);
      return;
    }
    rows.forEach((row, idx) => {
      const rowEl = document.createElement("div");
      rowEl.className = "env-var-row";

      const keyInput = document.createElement("input");
      keyInput.type = "text";
      keyInput.className = "settings-input env-key";
      keyInput.placeholder = "KEY";
      keyInput.value = row.key;
      keyInput.spellcheck = false;
      keyInput.autocomplete = "off";
      keyInput.autocapitalize = "off";
      if (row.key && !ENV_KEY_RE.test(row.key)) {
        keyInput.classList.add("env-invalid");
        keyInput.title = "Env var names must start with a letter or underscore, followed by letters, digits, or underscores.";
      }
      keyInput.addEventListener("input", () => {
        row.key = keyInput.value;
        if (row.key && !ENV_KEY_RE.test(row.key)) {
          keyInput.classList.add("env-invalid");
          keyInput.title = "Env var names must start with a letter or underscore, followed by letters, digits, or underscores.";
        } else {
          keyInput.classList.remove("env-invalid");
          keyInput.title = "";
        }
        scheduleSave();
      });

      const valueInput = document.createElement("input");
      valueInput.type = row.secret && !row._revealed ? "password" : "text";
      valueInput.className = "settings-input env-value";
      valueInput.placeholder = "value";
      valueInput.value = row.value;
      valueInput.spellcheck = false;
      valueInput.autocomplete = "off";
      valueInput.autocapitalize = "off";
      valueInput.addEventListener("input", () => {
        row.value = valueInput.value;
        scheduleSave();
      });

      const revealBtn = document.createElement("button");
      revealBtn.type = "button";
      revealBtn.className = "env-reveal";
      revealBtn.title = row._revealed ? "Hide value" : "Reveal value";
      revealBtn.textContent = row._revealed ? "🙈" : "👁";
      revealBtn.disabled = !row.secret;
      revealBtn.addEventListener("click", () => {
        row._revealed = !row._revealed;
        render();
      });

      const secretLabel = document.createElement("label");
      secretLabel.className = "env-secret-toggle";
      secretLabel.title = "Treat this value as a secret — masked in the UI by default.";
      const secretBox = document.createElement("input");
      secretBox.type = "checkbox";
      secretBox.checked = !!row.secret;
      secretBox.addEventListener("change", () => {
        row.secret = secretBox.checked;
        if (!row.secret) row._revealed = false;
        render();
        scheduleSave();
      });
      const secretText = document.createElement("span");
      secretText.textContent = "Secret";
      secretLabel.appendChild(secretBox);
      secretLabel.appendChild(secretText);

      const deleteBtn = document.createElement("button");
      deleteBtn.type = "button";
      deleteBtn.className = "env-delete";
      deleteBtn.title = "Delete this variable";
      deleteBtn.textContent = "✕";
      deleteBtn.addEventListener("click", () => {
        rows.splice(idx, 1);
        render();
        scheduleSave();
      });

      rowEl.appendChild(keyInput);
      rowEl.appendChild(valueInput);
      rowEl.appendChild(revealBtn);
      rowEl.appendChild(secretLabel);
      rowEl.appendChild(deleteBtn);
      listEl.appendChild(rowEl);
    });
  }

  addBtn.addEventListener("click", () => {
    rows.push({ key: "", value: "", secret: false });
    render();
    // Don't save yet — empty key is invalid. Wait for user to type.
  });

  render();
}
