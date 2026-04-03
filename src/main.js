import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import { createEditor, getLangName } from "./editor.js";
import { initFileBrowser, getCurrentPath, onNavigate, closeFilePreview, refreshFileBrowser } from "./filebrowser.js";
import { fetchGitStatus, startGitPolling } from "./git.js";
import { initGitPanel, refreshPanel } from "./gitpanel.js";
import { loadSettings, saveSetting, getSettings } from "./settings.js";
import { initQuickOpen, show as showQuickOpen, updateRoot as updateQuickOpenRoot } from "./quickopen.js";
import { initAgentPanel, setTabCallbacks, isAgentVisible } from "./agentpanel.js";
import { createSettingsPanel } from "./settingspanel.js";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// Tab management
// Terminal tab: { type: "terminal", panes: [pane, pane?], containerEl, name, activePane: 0|1 }
// Editor tab:   { type: "editor", containerEl, filePath, fileName, editorView, originalContent, modified }
// Each pane: { ptyId, term, fitAddon, el }
const tabs = new Map(); // uiTabId → tab object
const paneMap = new Map(); // ptyId → pane object (for routing PTY output)
let agentTabActive = false; // is the agent chat tab currently shown?
let activeTabUiId = -1;
let nextUiTabId = 0;
let isCreatingTab = false;
let panelTransitioning = false;

/** Wait for the browser to complete layout reflow (double-rAF). */
function waitForLayout() {
  return new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
}

const terminalTheme = {
  background: "#1a1a1a",
  foreground: "#e0e0e0",
  cursor: "#e0e0e0",
  cursorAccent: "#1a1a1a",
  selectionBackground: "#444",
  black: "#1a1a1a",
  red: "#ff5f56",
  green: "#5af78e",
  yellow: "#f3f99d",
  blue: "#57c7ff",
  magenta: "#ff6ac1",
  cyan: "#9aedfe",
  white: "#e0e0e0",
  brightBlack: "#686868",
  brightRed: "#ff5f56",
  brightGreen: "#5af78e",
  brightYellow: "#f3f99d",
  brightBlue: "#57c7ff",
  brightMagenta: "#ff6ac1",
  brightCyan: "#9aedfe",
  brightWhite: "#ffffff",
};

async function createPane(parentEl, cwd) {
  const s = getSettings();
  const term = new Terminal({
    fontFamily: s.termFontFamily,
    fontSize: s.termFontSize,
    lineHeight: 1.4,
    theme: terminalTheme,
    cursorBlink: s.termCursorBlink,
    cursorStyle: s.termCursorStyle,
    scrollback: s.termScrollback,
    allowProposedApi: true,
  });

  const fitAddon = new FitAddon();
  term.loadAddon(fitAddon);

  const el = document.createElement("div");
  el.className = "pane";
  parentEl.appendChild(el);

  // NOTE: term.open() is deferred to _spawnPty so the canvas initializes with real dimensions
  const pane = { ptyId: null, term, fitAddon, el };

  term.onData((data) => {
    if (pane.ptyId !== null) {
      invoke("write_to_pty", { tabId: pane.ptyId, data });
    }
  });

  // Called after the container is visible — opens terminal, fits, then spawns PTY at correct size
  pane._spawnPty = async () => {
    await waitForLayout();
    term.open(el);
    try {
      term.loadAddon(new WebglAddon());
    } catch (e) {
      console.warn("WebGL renderer not available, using default");
    }
    fitAddon.fit();
    // Pass actual dimensions so the PTY starts at the right size — no resize race
    const result = await invoke("spawn_pty", {
      cwd: cwd || getCurrentPath() || undefined,
      rows: term.rows,
      cols: term.cols,
    });
    pane.ptyId = result.tab_id;
    paneMap.set(pane.ptyId, pane);
    delete pane._spawnPty;
  };

  return pane;
}

function destroyPane(pane) {
  pane.term.dispose();
  pane.el.remove();
  if (pane.ptyId !== null) {
    paneMap.delete(pane.ptyId);
    invoke("close_pty", { tabId: pane.ptyId });
  }
}

async function createTab() {
  if (isCreatingTab) return;
  isCreatingTab = true;
  try {
    const uiId = nextUiTabId++;

    const containerEl = document.createElement("div");
    containerEl.className = "terminal-instance";
    document.getElementById("terminal-instances").appendChild(containerEl);

    const pane = await createPane(containerEl);

    const tab = { type: "terminal", panes: [pane], containerEl, name: null, activePane: 0 };
    tabs.set(uiId, tab);

    renderTabBar();
    switchTab(uiId);

    // Spawn PTY after container is visible so fit() gets correct dimensions
    await pane._spawnPty();
    pane.term.focus();

    return uiId;
  } finally {
    isCreatingTab = false;
  }
}

async function splitPane() {
  const tab = tabs.get(activeTabUiId);
  if (!tab || tab.type !== "terminal") return;

  if (tab.panes.length >= 2) {
    // Already split — unsplit (close second pane)
    const secondPane = tab.panes.pop();
    destroyPane(secondPane);
    // Remove split handle
    const handle = tab.containerEl.querySelector(".split-handle");
    if (handle) handle.remove();
    tab.containerEl.classList.remove("split");
    tab.activePane = 0;
    fitAllPanes(tab);
    tab.panes[0].term.focus();
    return;
  }

  // Split: add second pane
  tab.containerEl.classList.add("split");

  const handle = document.createElement("div");
  handle.className = "split-handle";
  tab.containerEl.appendChild(handle);

  const secondPane = await createPane(tab.containerEl);
  tab.panes.push(secondPane);
  tab.activePane = 1;

  // Split handle drag
  let isSplitDragging = false;
  handle.addEventListener("mousedown", (e) => {
    isSplitDragging = true;
    handle.classList.add("dragging");
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    e.preventDefault();
  });

  const onMouseMove = (e) => {
    if (!isSplitDragging) return;
    const rect = tab.containerEl.getBoundingClientRect();
    const pct = ((e.clientX - rect.left) / rect.width) * 100;
    const clamped = Math.min(Math.max(pct, 20), 80);
    tab.panes[0].el.style.width = clamped + "%";
    tab.panes[1].el.style.width = (100 - clamped) + "%";
  };

  const onMouseUp = () => {
    if (isSplitDragging) {
      isSplitDragging = false;
      handle.classList.remove("dragging");
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
      fitAllPanes(tab);
    }
  };

  document.addEventListener("mousemove", onMouseMove);
  document.addEventListener("mouseup", onMouseUp);

  // Click to focus pane
  tab.panes[0].el.addEventListener("click", () => { tab.activePane = 0; });
  tab.panes[1].el.addEventListener("click", () => { tab.activePane = 1; });

  // Spawn PTY after layout is set so fit() gets correct dimensions
  await secondPane._spawnPty();
  fitAllPanes(tab);
  secondPane.term.focus();
}

function fitAllPanes(tab) {
  if (tab.fitRAF) cancelAnimationFrame(tab.fitRAF);
  tab.fitRAF = requestAnimationFrame(() => {
    tab.fitRAF = null;
    tab.panes.forEach((pane) => {
      pane.fitAddon.fit();
      if (pane.ptyId !== null) {
        invoke("resize_pty", { tabId: pane.ptyId, rows: pane.term.rows, cols: pane.term.cols });
      }
    });
  });
}

function getActivePtyId() {
  const tab = tabs.get(activeTabUiId);
  if (!tab || tab.type !== "terminal") return null;
  return tab.panes[tab.activePane]?.ptyId ?? tab.panes[0]?.ptyId ?? null;
}

function showAgentChatTab() {
  // Hide current terminal tab
  if (tabs.has(activeTabUiId)) {
    tabs.get(activeTabUiId).containerEl.style.display = "none";
  }
  document.getElementById("agent-chat-tab").style.display = "flex";
  agentTabActive = true;
  renderTabBar();
}

function hideAgentChatTab() {
  document.getElementById("agent-chat-tab").style.display = "none";
  agentTabActive = false;
  // Show the last active tab
  if (tabs.has(activeTabUiId)) {
    const tab = tabs.get(activeTabUiId);
    tab.containerEl.style.display = "flex";
    if (tab.type === "terminal") {
      requestAnimationFrame(() => fitAllPanes(tab));
    }
  }
  renderTabBar();
}

function switchTab(uiId) {
  if (activeTabUiId === uiId && tabs.has(uiId) && !agentTabActive) return;

  // Hide agent chat if showing
  if (agentTabActive) {
    document.getElementById("agent-chat-tab").style.display = "none";
    agentTabActive = false;
  }

  // Hide current tab
  if (tabs.has(activeTabUiId)) {
    tabs.get(activeTabUiId).containerEl.style.display = "none";
  }

  const tab = tabs.get(uiId);
  if (!tab) return;
  tab.containerEl.style.display = "flex";
  activeTabUiId = uiId;

  if (tab.type === "terminal") {
    requestAnimationFrame(() => {
      fitAllPanes(tab);
      const activePane = tab.panes[tab.activePane] || tab.panes[0];
      if (activePane) activePane.term.focus();
    });
  } else if (tab.type === "editor") {
    setTimeout(() => tab.editorView.focus(), 10);
  }

  renderTabBar();
}

async function closeTab(uiId) {
  const tab = tabs.get(uiId);
  if (!tab) return;

  if (tab.type === "editor" && tab.modified) {
    if (!confirm(`"${tab.fileName}" has unsaved changes. Close anyway?`)) return;
  }

  // Remove tab from state and UI FIRST — before any cleanup that could fail
  tab.containerEl.style.display = "none";
  tabs.delete(uiId);
  renderTabBar();

  // Now clean up resources
  try {
    if (tab.type === "editor") tab.editorView.destroy();
    else if (tab.type === "terminal") tab.panes.forEach(destroyPane);
  } catch (_) {}
  try { tab.containerEl.remove(); } catch (_) {}

  if (tabs.size === 0) {
    await createTab();
  } else if (activeTabUiId === uiId) {
    const remaining = [...tabs.keys()];
    switchTab(remaining[remaining.length - 1]);
  }
}

// Editor tab management
async function createEditorTab(filePath) {
  // Deduplicate: if already open, switch to it
  for (const [uiId, tab] of tabs) {
    if (tab.type === "editor" && tab.filePath === filePath) {
      switchTab(uiId);
      return uiId;
    }
  }

  const fileName = filePath.split("/").pop();
  let content;
  try {
    content = await invoke("read_file_preview", { path: filePath, maxBytes: 512000 });
  } catch (err) {
    console.error("Failed to read file:", err);
    return null;
  }

  const uiId = nextUiTabId++;

  const containerEl = document.createElement("div");
  containerEl.className = "editor-instance";
  document.getElementById("terminal-instances").appendChild(containerEl);

  // Breadcrumb
  const breadcrumb = document.createElement("div");
  breadcrumb.className = "editor-breadcrumb";
  const homePath = filePath.replace(/^\/Users\/[^/]+/, "~");
  const parts = homePath.split("/");
  parts.forEach((part, i) => {
    if (i > 0) {
      const sep = document.createElement("span");
      sep.className = "breadcrumb-separator";
      sep.textContent = "›";
      breadcrumb.appendChild(sep);
    }
    const span = document.createElement("span");
    span.className = i === parts.length - 1 ? "breadcrumb-file" : "";
    span.textContent = part;
    breadcrumb.appendChild(span);
  });
  containerEl.appendChild(breadcrumb);

  // Editor content area
  const editorContent = document.createElement("div");
  editorContent.className = "editor-content";
  containerEl.appendChild(editorContent);

  // Status bar
  const statusBar = document.createElement("div");
  statusBar.className = "editor-statusbar";
  statusBar.textContent = `Ln 1, Col 1 · ${getLangName(fileName)}`;
  containerEl.appendChild(statusBar);

  const tab = {
    type: "editor",
    containerEl,
    filePath,
    fileName,
    editorView: null,
    originalContent: content,
    modified: false,
  };

  const editorSettings = getSettings();
  const editorView = createEditor(editorContent, content, fileName, {
    onChange: (newContent) => {
      tab.modified = newContent !== tab.originalContent;
      renderTabBar();
    },
    onCursorChange: (line, col) => {
      statusBar.textContent = `Ln ${line}, Col ${col} · ${getLangName(fileName)}`;
    },
    tabSize: editorSettings.editorTabSize,
    wordWrap: editorSettings.editorWordWrap,
  });

  tab.editorView = editorView;
  tabs.set(uiId, tab);

  renderTabBar();
  switchTab(uiId);
  return uiId;
}

async function saveEditorTab(uiId) {
  const tab = tabs.get(uiId);
  if (!tab || tab.type !== "editor") return;

  try {
    const content = tab.editorView.state.doc.toString();
    await invoke("write_file", { path: tab.filePath, content });
    tab.originalContent = content;
    tab.modified = false;
    renderTabBar();
  } catch (err) {
    console.error("Save error:", err);
  }
}

// Settings tab
function openSettingsTab() {
  // Deduplicate
  for (const [uiId, tab] of tabs) {
    if (tab.type === "settings") {
      switchTab(uiId);
      return;
    }
  }

  const uiId = nextUiTabId++;
  const containerEl = document.createElement("div");
  containerEl.className = "settings-instance";
  document.getElementById("terminal-instances").appendChild(containerEl);

  createSettingsPanel(containerEl, getSettings(), (key, value) => {
    saveSetting(key, value);
    applySettingLive(key, value);
  });

  const tab = { type: "settings", containerEl };
  tabs.set(uiId, tab);
  renderTabBar();
  switchTab(uiId);
}

function applySettingLive(key, value) {
  // Apply terminal settings to all existing terminal panes
  if (key.startsWith("term")) {
    for (const tab of tabs.values()) {
      if (tab.type !== "terminal") continue;
      for (const pane of tab.panes) {
        if (key === "termFontSize") pane.term.options.fontSize = value;
        if (key === "termFontFamily") pane.term.options.fontFamily = value;
        if (key === "termCursorStyle") pane.term.options.cursorStyle = value;
        if (key === "termCursorBlink") pane.term.options.cursorBlink = value;
        pane.fitAddon.fit();
        if (pane.ptyId !== null) {
          invoke("resize_pty", { tabId: pane.ptyId, rows: pane.term.rows, cols: pane.term.cols });
        }
      }
    }
  }

  if (key === "sidebarWidth") {
    document.getElementById("sidebar").style.width = value + "px";
  }

  if (key === "gitPollInterval") {
    startGitPolling(getCurrentPath, value);
  }
}

function renderTabBar() {
  const tabBar = document.getElementById("tab-bar");
  tabBar.innerHTML = "";

  let termIndex = 1;
  for (const [uiId, tab] of tabs) {
    if (tab._rightGroup) continue; // skip tabs in the right split group
    const isActive = uiId === activeTabUiId && !agentTabActive;
    const tabEl = document.createElement("div");
    tabEl.className = `tab ${isActive ? "tab-active" : ""}`;
    tabEl.dataset.tabId = uiId;

    // Icon
    const icon = document.createElement("span");
    icon.className = "tab-icon";
    icon.textContent = tab.type === "terminal" ? ">_" : tab.type === "settings" ? "⚙" : "◆";
    tabEl.appendChild(icon);

    const label = document.createElement("span");
    label.className = "tab-label";

    if (tab.type === "terminal") {
      label.textContent = tab.name || `Terminal ${termIndex}`;
      label.addEventListener("dblclick", (e) => {
        e.stopPropagation();
        startTabRename(uiId, label, termIndex);
      });
      termIndex++;
    } else if (tab.type === "settings") {
      label.textContent = "Settings";
    } else {
      label.textContent = tab.fileName;
    }
    tabEl.appendChild(label);

    // Modified dot or close button
    if (tab.type === "editor" && tab.modified) {
      const dot = document.createElement("span");
      dot.className = "tab-modified-dot";
      dot.title = "Unsaved changes";
      tabEl.appendChild(dot);
    }

    const closeBtn = document.createElement("span");
    closeBtn.className = "tab-close";
    closeBtn.textContent = "×";
    closeBtn.addEventListener("mousedown", (e) => {
      e.stopPropagation();
      e.preventDefault();
      closeTab(uiId);
    });
    tabEl.appendChild(closeBtn);

    tabEl.addEventListener("click", () => switchTab(uiId));
    tabEl.addEventListener("mousedown", (e) => {
      if (e.target.closest(".tab-close")) return; // don't start drag from close button
      startTabDrag(tabEl, uiId, "left", e);
    });
    tabBar.appendChild(tabEl);
  }

  // Agent chat tab (only show if agent is open)
  if (isAgentVisible()) {
    const agentTab = document.createElement("div");
    agentTab.className = `tab tab-agent ${agentTabActive ? "tab-active" : ""}`;
    agentTab.innerHTML = `<span class="tab-label">⚡ Agent</span>`;
    agentTab.addEventListener("click", () => showAgentChatTab());
    tabBar.appendChild(agentTab);
  }

  const newTabBtn = document.createElement("div");
  newTabBtn.className = "tab tab-new";
  newTabBtn.textContent = "+";
  newTabBtn.title = "New terminal (⌘T)";
  newTabBtn.addEventListener("click", () => createTab());
  tabBar.appendChild(newTabBtn);

  const splitBtn = document.createElement("div");
  splitBtn.className = "tab tab-new";
  splitBtn.textContent = isSplit ? "⊞" : "⊟";
  splitBtn.title = isSplit ? "Unsplit (⌘\\)" : "Split view (⌘\\)";
  splitBtn.addEventListener("click", () => splitWorkspace());
  tabBar.appendChild(splitBtn);
}

function startTabRename(uiId, labelEl, index) {
  const tab = tabs.get(uiId);
  const input = document.createElement("input");
  input.className = "tab-rename-input";
  input.value = tab.name || `Terminal ${index}`;
  input.select();

  labelEl.replaceWith(input);
  input.focus();

  function finishRename() {
    const name = input.value.trim();
    tab.name = name || null;
    renderTabBar();
  }

  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") { e.preventDefault(); finishRename(); }
    if (e.key === "Escape") { e.preventDefault(); renderTabBar(); }
    e.stopPropagation();
  });
  input.addEventListener("blur", finishRename);
}

// Listen for PTY output — route by ptyId to correct pane
listen("pty-output", (event) => {
  const { tab_id, data } = event.payload;
  const pane = paneMap.get(tab_id);
  if (pane) {
    pane.term.write(data);
  }
});

// Listen for PTY exit — auto-close the tab when the shell process exits
listen("pty-exit", (event) => {
  const { tab_id } = event.payload;
  paneMap.delete(tab_id);
  for (const [uiId, tab] of tabs) {
    if (tab.type !== "terminal") continue;
    if (tab.panes.some((p) => p.ptyId === tab_id)) {
      const exitedPane = tab.panes.find((p) => p.ptyId === tab_id);
      if (exitedPane) exitedPane.ptyId = null;
      // Route to correct close function based on which group owns the tab
      if (tab._rightGroup) closeRightTab(uiId);
      else closeTab(uiId);
      break;
    }
  }
});

// Listen for filesystem changes — refresh file browser and git panel
let fsRefreshScheduled = false;
listen("fs-changed", (event) => {
  const { path } = event.payload;
  if (path !== getCurrentPath()) return;
  if (fsRefreshScheduled) return;
  fsRefreshScheduled = true;
  setTimeout(() => {
    fsRefreshScheduled = false;
    refreshFileBrowser();
    fetchGitStatus(getCurrentPath());
    refreshPanel(null, true);
  }, 500);
});

// Resize observer
const terminalContainer = document.getElementById("terminal-container");
const resizeObserver = new ResizeObserver(() => {
  if (panelTransitioning) return;
  const tab = tabs.get(activeTabUiId);
  if (tab?.type === "terminal") fitAllPanes(tab);
});
resizeObserver.observe(terminalContainer);

// Refit terminal after git/agent panel close transition (ResizeObserver is blocked during transition)
window.addEventListener("panel-transition-done", () => {
  const tab = tabs.get(activeTabUiId);
  if (tab?.type === "terminal") fitAllPanes(tab);
  if (isSplit && rightActiveTabUiId !== -1) {
    const rightTab = tabs.get(rightActiveTabUiId);
    if (rightTab?.type === "terminal") fitAllPanes(rightTab);
  }
});

// Sidebar resize
const resizeHandle = document.getElementById("resize-handle");
const sidebar = document.getElementById("sidebar");
let isResizing = false;

resizeHandle.addEventListener("mousedown", (e) => {
  isResizing = true;
  resizeHandle.classList.add("dragging");
  document.body.style.cursor = "col-resize";
  document.body.style.userSelect = "none";
  e.preventDefault();
});

document.addEventListener("mousemove", (e) => {
  if (!isResizing) return;
  const newWidth = Math.min(Math.max(e.clientX, 150), 500);
  sidebar.style.width = newWidth + "px";
});

document.addEventListener("mouseup", () => {
  if (isResizing) {
    isResizing = false;
    resizeHandle.classList.remove("dragging");
    document.body.style.cursor = "";
    document.body.style.userSelect = "";
    const tab = tabs.get(activeTabUiId);
    if (tab?.type === "terminal") fitAllPanes(tab);
    saveSetting("sidebarWidth", parseInt(sidebar.style.width));
  }
});

// Terminal drag-drop: drop file path into terminal
terminalContainer.addEventListener("dragover", (e) => {
  e.preventDefault();
  e.dataTransfer.dropEffect = "copy";
});

terminalContainer.addEventListener("drop", (e) => {
  e.preventDefault();
  const path = e.dataTransfer.getData("text/plain");
  if (path) {
    const tab = tabs.get(activeTabUiId);
    if (tab?.type === "terminal") {
      const ptyId = getActivePtyId();
      if (ptyId !== null) {
        const quoted = "'" + path.replace(/'/g, "'\\''") + "'";
        invoke("write_to_pty", { tabId: ptyId, data: quoted });
      }
    }
  }
});

// Terminal right-click context menu
terminalContainer.addEventListener("contextmenu", (e) => {
  e.preventDefault();
  const tab = tabs.get(activeTabUiId);
  if (!tab || tab.type !== "terminal") return;
  const pane = tab.panes[tab.activePane] || tab.panes[0];
  if (!pane) return;

  // Remove existing menu
  const old = document.getElementById("context-menu");
  if (old) old.remove();

  const menu = document.createElement("div");
  menu.id = "context-menu";
  menu.className = "context-menu";

  const copyItem = document.createElement("div");
  copyItem.className = "context-menu-item";
  copyItem.textContent = "Copy";
  copyItem.addEventListener("click", () => {
    const sel = pane.term.getSelection();
    if (sel) navigator.clipboard.writeText(sel);
    menu.remove();
  });

  const pasteItem = document.createElement("div");
  pasteItem.className = "context-menu-item";
  pasteItem.textContent = "Paste";
  pasteItem.addEventListener("click", async () => {
    const text = await navigator.clipboard.readText();
    if (text) invoke("write_to_pty", { tabId: pane.ptyId, data: text });
    menu.remove();
  });

  const clearItem = document.createElement("div");
  clearItem.className = "context-menu-item";
  clearItem.textContent = "Clear";
  clearItem.addEventListener("click", () => {
    pane.term.clear();
    menu.remove();
  });

  menu.appendChild(copyItem);
  menu.appendChild(pasteItem);
  menu.appendChild(document.createElement("div")).className = "context-menu-separator";
  menu.appendChild(clearItem);

  menu.style.left = e.clientX + "px";
  menu.style.top = e.clientY + "px";
  document.body.appendChild(menu);

  setTimeout(() => {
    document.addEventListener("click", () => menu.remove(), { once: true });
  }, 0);
});

// Keyboard shortcuts
document.addEventListener("keydown", (e) => {
  const activeTab = tabs.get(activeTabUiId);
  const isEditorTab = activeTab?.type === "editor";

  if (e.metaKey && e.key === ",") {
    e.preventDefault();
    openSettingsTab();
    return;
  }
  if (e.metaKey && e.key === "s") {
    e.preventDefault();
    if (isEditorTab) {
      saveEditorTab(activeTabUiId);
    }
    return;
  }
  if (e.metaKey && e.key === "p") {
    e.preventDefault();
    showQuickOpen(getCurrentPath());
    return;
  }
  // Cmd+F: let CodeMirror handle it for editor tabs, sidebar search for terminal tabs
  if (e.metaKey && e.key === "f") {
    if (isEditorTab) {
      e.preventDefault();
      return;
    }
  }
  if (e.metaKey && e.key === "h") {
    if (isEditorTab) {
      e.preventDefault();
      return;
    }
  }
  if (e.metaKey && e.key === "k") {
    e.preventDefault();
    if (activeTab?.type === "terminal") {
      const pane = activeTab.panes[activeTab.activePane] || activeTab.panes[0];
      pane.term.clear();
      pane.term.focus();
    }
  }
  if (e.metaKey && e.key === "t") {
    e.preventDefault();
    if (isSplit && focusedGroup === "right") createTabInRight();
    else createTab();
  }
  if (e.metaKey && e.key === "w") {
    e.preventDefault();
    if (isSplit && focusedGroup === "right") closeRightTab(rightActiveTabUiId);
    else closeTab(activeTabUiId);
  }
  if (e.metaKey && e.key === "d") {
    e.preventDefault();
    if (!isEditorTab) splitPane();
  }
  // Cmd+\ — split/unsplit workspace
  if (e.metaKey && e.key === "\\") {
    e.preventDefault();
    splitWorkspace();
  }
  // Cmd+Option+Left/Right — switch focused group
  if (e.metaKey && e.altKey && (e.key === "ArrowLeft" || e.key === "ArrowRight") && isSplit) {
    e.preventDefault();
    setFocusedGroup(focusedGroup === "left" ? "right" : "left");
  }
  // Cmd+Shift+M — move tab to other group
  if (e.metaKey && e.shiftKey && e.key === "M" && isSplit) {
    e.preventDefault();
    if (focusedGroup === "left") moveTabToRight(activeTabUiId);
    else moveTabToLeft(rightActiveTabUiId);
  }
  if (e.key === "Escape") {
    closeFilePreview();
  }
  if (e.metaKey && e.key >= "1" && e.key <= "9") {
    e.preventDefault();
    const idx = parseInt(e.key) - 1;
    const tabIds = [...tabs.keys()];
    if (idx < tabIds.length) {
      switchTab(tabIds[idx]);
    }
  }
});

// Focus terminal on click (only for terminal tabs)
terminalContainer.addEventListener("click", (e) => {
  const clickedRight = isSplit && e.target.closest("#group-right");
  const uiId = clickedRight ? rightActiveTabUiId : activeTabUiId;
  const tab = tabs.get(uiId);
  if (tab?.type === "terminal") {
    if (clickedRight) setFocusedGroup("right");
    const pane = tab.panes[tab.activePane] || tab.panes[0];
    if (pane) pane.term.focus();
  }
});

// Unsaved changes warning
window.addEventListener("beforeunload", (e) => {
  for (const tab of tabs.values()) {
    if (tab.type === "editor" && tab.modified) {
      e.preventDefault();
      e.returnValue = "";
      return;
    }
  }
});

// Custom mouse-based tab drag (HTML5 drag doesn't work reliably in WebKit/Tauri)
let dragState = null;
let dragGhost = null;

function startTabDrag(tabEl, uiId, sourceGroup, e) {
  if (!isSplit) return;
  e.preventDefault();

  dragState = { uiId, sourceGroup, startX: e.clientX, startY: e.clientY, started: false };

  const onMouseMove = (me) => {
    if (!dragState) return;
    const dx = me.clientX - dragState.startX;
    const dy = me.clientY - dragState.startY;
    if (!dragState.started && Math.abs(dx) + Math.abs(dy) < 8) return; // deadzone
    dragState.started = true;

    if (!dragGhost) {
      dragGhost = tabEl.cloneNode(true);
      dragGhost.className = "tab tab-drag-ghost";
      document.body.appendChild(dragGhost);
    }
    dragGhost.style.left = me.clientX - 40 + "px";
    dragGhost.style.top = me.clientY - 12 + "px";

    // Highlight target tab bar
    const rightBar = rightTabBarEl;
    const leftBar = document.getElementById("tab-bar");
    if (rightBar) rightBar.classList.toggle("drag-over", isOverEl(me, rightBar));
    leftBar.classList.toggle("drag-over", isOverEl(me, leftBar));
  };

  const onMouseUp = (me) => {
    document.removeEventListener("mousemove", onMouseMove);
    document.removeEventListener("mouseup", onMouseUp);
    if (dragGhost) { dragGhost.remove(); dragGhost = null; }

    const leftBar = document.getElementById("tab-bar");
    const rightBar = rightTabBarEl;
    leftBar.classList.remove("drag-over");
    if (rightBar) rightBar.classList.remove("drag-over");

    if (!dragState || !dragState.started) { dragState = null; return; }

    // Check drop target
    if (rightBar && isOverEl(me, rightBar) && dragState.sourceGroup === "left") {
      moveTabToRight(dragState.uiId);
    } else if (isOverEl(me, leftBar) && dragState.sourceGroup === "right") {
      moveTabToLeft(dragState.uiId);
    }
    dragState = null;
  };

  document.addEventListener("mousemove", onMouseMove);
  document.addEventListener("mouseup", onMouseUp);
}

function isOverEl(mouseEvent, el) {
  if (!el) return false;
  const r = el.getBoundingClientRect();
  return mouseEvent.clientX >= r.left && mouseEvent.clientX <= r.right &&
         mouseEvent.clientY >= r.top && mouseEvent.clientY <= r.bottom;
}

// Settings button
document.getElementById("open-settings").addEventListener("click", () => openSettingsTab());

// ===== Split Workspace =====
// Parallel tab system for the right group. Left group uses existing code untouched.
let isSplit = false;
let rightGroupTabIds = [];
let rightActiveTabUiId = -1;
let focusedGroup = "left";
let rightTabBarEl = null;
let rightInstancesEl = null;
let splitHandleEl = null;

function setFocusedGroup(group) {
  focusedGroup = group;
  const left = document.getElementById("group-left");
  const right = document.getElementById("group-right");
  if (left) left.classList.toggle("group-focused", group === "left");
  if (right) right.classList.toggle("group-focused", group === "right");
}

function renderRightTabBar() {
  if (!rightTabBarEl) return;
  rightTabBarEl.innerHTML = "";

  let termIndex = 1;
  for (const uiId of rightGroupTabIds) {
    const tab = tabs.get(uiId);
    if (!tab) continue;
    const isActive = uiId === rightActiveTabUiId;
    const tabEl = document.createElement("div");
    tabEl.className = `tab ${isActive ? "tab-active" : ""}`;

    const icon = document.createElement("span");
    icon.className = "tab-icon";
    icon.textContent = tab.type === "terminal" ? ">_" : tab.type === "settings" ? "⚙" : "◆";
    tabEl.appendChild(icon);

    const label = document.createElement("span");
    label.className = "tab-label";
    if (tab.type === "terminal") {
      label.textContent = tab.name || `Terminal ${termIndex}`;
      termIndex++;
    } else if (tab.type === "settings") {
      label.textContent = "Settings";
    } else {
      label.textContent = tab.fileName;
    }
    tabEl.appendChild(label);

    if (tab.type === "editor" && tab.modified) {
      const dot = document.createElement("span");
      dot.className = "tab-modified-dot";
      dot.title = "Unsaved changes";
      tabEl.appendChild(dot);
    }

    const closeBtn = document.createElement("span");
    closeBtn.className = "tab-close";
    closeBtn.textContent = "×";
    closeBtn.addEventListener("mousedown", (e) => {
      e.stopPropagation();
      e.preventDefault();
      closeRightTab(uiId);
    });
    tabEl.appendChild(closeBtn);

    tabEl.addEventListener("click", () => {
      setFocusedGroup("right");
      switchRightTab(uiId);
    });
    tabEl.addEventListener("mousedown", (e) => {
      if (e.target.closest(".tab-close")) return;
      startTabDrag(tabEl, uiId, "right", e);
    });

    rightTabBarEl.appendChild(tabEl);
  }

  const newTabBtn = document.createElement("div");
  newTabBtn.className = "tab tab-new";
  newTabBtn.textContent = "+";
  newTabBtn.title = "New terminal";
  newTabBtn.addEventListener("click", () => createTabInRight());
  rightTabBarEl.appendChild(newTabBtn);
}

function switchRightTab(uiId) {
  if (rightActiveTabUiId === uiId) return;

  // Hide current right tab
  if (rightActiveTabUiId !== -1) {
    const prev = tabs.get(rightActiveTabUiId);
    if (prev) prev.containerEl.style.display = "none";
  }

  const tab = tabs.get(uiId);
  if (!tab) return;
  tab.containerEl.style.display = "flex";
  rightActiveTabUiId = uiId;

  if (tab.type === "terminal") {
    requestAnimationFrame(() => {
      fitAllPanes(tab);
      const activePane = tab.panes[tab.activePane] || tab.panes[0];
      if (activePane) activePane.term.focus();
    });
  } else if (tab.type === "editor") {
    setTimeout(() => tab.editorView.focus(), 10);
  }

  renderRightTabBar();
}

async function closeRightTab(uiId) {
  const tab = tabs.get(uiId);
  if (!tab) return;

  if (tab.type === "editor" && tab.modified) {
    if (!confirm(`"${tab.fileName}" has unsaved changes. Close anyway?`)) return;
  }

  // Remove from state and UI FIRST (same pattern as working closeTab)
  tab.containerEl.style.display = "none";
  const idx = rightGroupTabIds.indexOf(uiId);
  if (idx !== -1) rightGroupTabIds.splice(idx, 1);
  tabs.delete(uiId);
  renderRightTabBar();

  // Clean up resources
  try {
    if (tab.type === "editor") tab.editorView.destroy();
    else if (tab.type === "terminal") tab.panes.forEach(destroyPane);
  } catch (_) {}
  try { tab.containerEl.remove(); } catch (_) {}

  if (rightGroupTabIds.length === 0) {
    // No tabs left in right group — unsplit
    unsplitWorkspace();
  } else if (rightActiveTabUiId === uiId) {
    switchRightTab(rightGroupTabIds[rightGroupTabIds.length - 1]);
  }
}

async function createTabInRight() {
  if (!isSplit || !rightInstancesEl || isCreatingTab) return;
  isCreatingTab = true;
  try {
    const uiId = nextUiTabId++;
    const containerEl = document.createElement("div");
    containerEl.className = "terminal-instance";
    rightInstancesEl.appendChild(containerEl);

    const pane = await createPane(containerEl);
    const tab = { type: "terminal", panes: [pane], containerEl, name: null, activePane: 0, _rightGroup: true };
    tabs.set(uiId, tab);
    rightGroupTabIds.push(uiId);

    renderRightTabBar();
    switchRightTab(uiId);

    await pane._spawnPty();
    pane.term.focus();
    return uiId;
  } finally {
    isCreatingTab = false;
  }
}

function moveTabToRight(uiId) {
  if (!isSplit) return;
  const tab = tabs.get(uiId);
  if (!tab) return;

  tab._rightGroup = true;

  // If this was the active left tab, switch left to another tab first
  if (activeTabUiId === uiId) {
    const leftTabs = [...tabs.keys()].filter(id => !rightGroupTabIds.includes(id) && id !== uiId);
    if (leftTabs.length > 0) {
      // Switch left group — this hides the old active tab, shows the new one
      switchTab(leftTabs[leftTabs.length - 1]);
    } else {
      // No other left tabs — just clear activeTabUiId so switchTab won't hide this tab later
      tab.containerEl.style.display = "none";
      activeTabUiId = -1;
    }
  } else {
    tab.containerEl.style.display = "none";
  }

  // Reparent to right group
  rightInstancesEl.appendChild(tab.containerEl);
  rightGroupTabIds.push(uiId);

  // Hide previous right tab before showing the moved one
  if (rightActiveTabUiId !== -1 && rightActiveTabUiId !== uiId) {
    const prev = tabs.get(rightActiveTabUiId);
    if (prev) prev.containerEl.style.display = "none";
  }
  tab.containerEl.style.display = "flex";
  rightActiveTabUiId = uiId;

  renderTabBar();
  renderRightTabBar();
  setFocusedGroup("right");

  // Refit the moved terminal
  if (tab.type === "terminal") {
    requestAnimationFrame(() => fitAllPanes(tab));
  }
}

function moveTabToLeft(uiId) {
  if (!isSplit) return;
  const tab = tabs.get(uiId);
  if (!tab) return;

  // Remove from right tracking
  const idx = rightGroupTabIds.indexOf(uiId);
  if (idx !== -1) rightGroupTabIds.splice(idx, 1);
  delete tab._rightGroup;

  // Switch right group to another tab
  if (rightActiveTabUiId === uiId) {
    if (rightGroupTabIds.length > 0) {
      const nextRight = tabs.get(rightGroupTabIds[rightGroupTabIds.length - 1]);
      if (nextRight) nextRight.containerEl.style.display = "flex";
      rightActiveTabUiId = rightGroupTabIds[rightGroupTabIds.length - 1];
    } else {
      rightActiveTabUiId = -1;
    }
  }

  // Hide old left active tab, show moved tab
  if (activeTabUiId !== -1 && tabs.has(activeTabUiId)) {
    tabs.get(activeTabUiId).containerEl.style.display = "none";
  }

  // Reparent to left
  document.getElementById("terminal-instances").appendChild(tab.containerEl);
  tab.containerEl.style.display = "flex";
  activeTabUiId = uiId;

  renderTabBar();
  renderRightTabBar();
  setFocusedGroup("left");

  // Refit the moved terminal
  if (tab.type === "terminal") {
    requestAnimationFrame(() => fitAllPanes(tab));
  }

  if (rightGroupTabIds.length === 0) {
    unsplitWorkspace();
  }
}

let isSplitting = false;
async function splitWorkspace() {
  if (isSplitting) return;
  if (isSplit) {
    unsplitWorkspace();
    return;
  }

  isSplitting = true;
  isSplit = true;
  document.body.classList.add("workspace-split");
  renderTabBar(); // Update split button icon immediately
  const wrapper = document.getElementById("split-wrapper");

  // Create handle
  splitHandleEl = document.createElement("div");
  splitHandleEl.className = "split-group-handle";
  wrapper.appendChild(splitHandleEl);

  // Create right group
  const rightGroup = document.createElement("div");
  rightGroup.id = "group-right";
  wrapper.appendChild(rightGroup);

  rightTabBarEl = document.createElement("div");
  rightTabBarEl.className = "right-tab-bar";
  rightGroup.appendChild(rightTabBarEl);

  rightInstancesEl = document.createElement("div");
  rightInstancesEl.className = "right-instances";
  rightGroup.appendChild(rightInstancesEl);

  // Focus listeners
  document.getElementById("group-left").addEventListener("mousedown", () => setFocusedGroup("left"));
  rightGroup.addEventListener("mousedown", () => setFocusedGroup("right"));

  // Split handle drag
  let isDragging = false;
  splitHandleEl.addEventListener("mousedown", (e) => {
    isDragging = true;
    splitHandleEl.classList.add("dragging");
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    e.preventDefault();
  });
  document.addEventListener("mousemove", (e) => {
    if (!isDragging) return;
    const rect = wrapper.getBoundingClientRect();
    const pct = ((e.clientX - rect.left) / rect.width) * 100;
    const clamped = Math.min(Math.max(pct, 20), 80);
    document.getElementById("group-left").style.flex = `0 0 ${clamped}%`;
    rightGroup.style.flex = `0 0 ${100 - clamped}%`;
  });
  document.addEventListener("mouseup", () => {
    if (isDragging) {
      isDragging = false;
      splitHandleEl.classList.remove("dragging");
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    }
  });

  // Drag between groups is handled by custom mouse drag (startTabDrag)

  setFocusedGroup("right");
  await createTabInRight();
  renderTabBar();

  // Refit left group's terminal since container width changed from 100% to 50%
  const leftTab = tabs.get(activeTabUiId);
  if (leftTab?.type === "terminal") {
    requestAnimationFrame(() => fitAllPanes(leftTab));
  }
  isSplitting = false;
}

function unsplitWorkspace() {
  if (!isSplit) return;

  // Move remaining right tabs to left
  for (const uiId of [...rightGroupTabIds]) {
    const tab = tabs.get(uiId);
    if (tab) {
      tab.containerEl.style.display = "none";
      delete tab._rightGroup;
      document.getElementById("terminal-instances").appendChild(tab.containerEl);
    }
  }
  rightGroupTabIds = [];
  rightActiveTabUiId = -1;

  // Remove right group DOM
  const rightGroup = document.getElementById("group-right");
  if (rightGroup) rightGroup.remove();
  if (splitHandleEl) splitHandleEl.remove();
  splitHandleEl = null;
  rightTabBarEl = null;
  rightInstancesEl = null;

  // Reset left group flex
  document.getElementById("group-left").style.flex = "";

  isSplit = false;
  document.body.classList.remove("workspace-split");
  setFocusedGroup("left");
  renderTabBar();

  // Refit active terminal since container width changed back to 100%
  const activeTab = tabs.get(activeTabUiId);
  if (activeTab?.type === "terminal") {
    requestAnimationFrame(() => fitAllPanes(activeTab));
  }
}

// Boot
async function boot() {
  const settings = await loadSettings();

  // Apply saved sidebar width
  if (settings.sidebarWidth) {
    sidebar.style.width = settings.sidebarWidth + "px";
  }

  try {
    await createTab();
  } catch (err) {
    document.getElementById("terminal-container").innerHTML =
      `<div style="padding:20px;color:#ff6b6b;">Failed to start terminal: ${err}</div>`;
  }

  await initFileBrowser(() => getActivePtyId(), (filePath) => createEditorTab(filePath), settings.defaultDirectory);
  invoke("watch_directory", { path: getCurrentPath() });

  // Quick open: Cmd+P to search, select opens in editor tab
  initQuickOpen(getCurrentPath, (fullPath) => {
    createEditorTab(fullPath);
  });

  initGitPanel(getCurrentPath, (filePath) => createEditorTab(filePath));
  initAgentPanel(getCurrentPath);
  setTabCallbacks(showAgentChatTab, hideAgentChatTab);

  onNavigate((path) => {
    fetchGitStatus(path);
    refreshPanel(path);
    updateQuickOpenRoot(path);
    saveSetting("lastDirectory", path);
    invoke("watch_directory", { path });
  });

  fetchGitStatus(getCurrentPath());
  startGitPolling(getCurrentPath, settings.gitPollInterval);
}

export function setPanelTransitioning(value) {
  panelTransitioning = value;
}

boot();
