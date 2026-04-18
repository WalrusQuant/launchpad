import { ACTIONS, getBinding, setBinding, getDefault, chordFromEvent } from "./keymap.js";

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
