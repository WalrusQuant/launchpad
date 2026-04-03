import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { createEditor, getLangName } from "./editor.js";
import { initFileBrowser, getCurrentPath, onNavigate, closeFilePreview } from "./filebrowser.js";
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

function createPane(parentEl) {
  return new Promise(async (resolve) => {
    const result = await invoke("spawn_pty");
    const ptyId = result.tab_id;

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

    term.open(el);
    fitAddon.fit();

    term.onData((data) => {
      invoke("write_to_pty", { tabId: ptyId, data });
    });

    const pane = { ptyId, term, fitAddon, el };
    paneMap.set(ptyId, pane);

    resolve(pane);
  });
}

function destroyPane(pane) {
  pane.term.dispose();
  pane.el.remove();
  paneMap.delete(pane.ptyId);
  invoke("close_pty", { tabId: pane.ptyId });
}

async function createTab() {
  const uiId = nextUiTabId++;

  const containerEl = document.createElement("div");
  containerEl.className = "terminal-instance";
  document.getElementById("terminal-instances").appendChild(containerEl);

  const pane = await createPane(containerEl);

  const tab = { type: "terminal", panes: [pane], containerEl, name: null, activePane: 0 };
  tabs.set(uiId, tab);

  renderTabBar();
  switchTab(uiId);

  return uiId;
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
    fitAllPanes(tab);
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

  fitAllPanes(tab);
  secondPane.term.focus();
}

function fitAllPanes(tab) {
  tab.panes.forEach((pane) => {
    pane.fitAddon.fit();
    invoke("resize_pty", { tabId: pane.ptyId, rows: pane.term.rows, cols: pane.term.cols });
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
      setTimeout(() => fitAllPanes(tab), 10);
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
  tab.containerEl.style.display = "flex";
  activeTabUiId = uiId;

  if (tab.type === "terminal") {
    setTimeout(() => {
      fitAllPanes(tab);
      const activePane = tab.panes[tab.activePane] || tab.panes[0];
      if (activePane) activePane.term.focus();
    }, 10);
  } else if (tab.type === "editor") {
    setTimeout(() => tab.editorView.focus(), 10);
  }

  renderTabBar();
}

async function closeTab(uiId) {
  const tab = tabs.get(uiId);
  if (!tab) return;

  if (tab.type === "editor") {
    if (tab.modified && !confirm(`"${tab.fileName}" has unsaved changes. Close anyway?`)) {
      return;
    }
    tab.editorView.destroy();
  } else if (tab.type === "terminal") {
    tab.panes.forEach(destroyPane);
  }
  // settings tabs need no special cleanup

  tab.containerEl.remove();
  tabs.delete(uiId);

  if (tabs.size === 0) {
    await createTab();
  } else if (activeTabUiId === uiId) {
    const remaining = [...tabs.keys()];
    switchTab(remaining[remaining.length - 1]);
  }

  renderTabBar();
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

  const editorView = createEditor(editorContent, content, fileName, {
    onChange: (newContent) => {
      tab.modified = newContent !== tab.originalContent;
      renderTabBar();
    },
    onCursorChange: (line, col) => {
      statusBar.textContent = `Ln ${line}, Col ${col} · ${getLangName(fileName)}`;
    },
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
        invoke("resize_pty", { tabId: pane.ptyId, rows: pane.term.rows, cols: pane.term.cols });
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
    const isActive = uiId === activeTabUiId && !agentTabActive;
    const tabEl = document.createElement("div");
    tabEl.className = `tab ${isActive ? "tab-active" : ""}`;

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
    closeBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      closeTab(uiId);
    });
    tabEl.appendChild(closeBtn);

    tabEl.addEventListener("click", () => switchTab(uiId));
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

// Resize observer
const terminalContainer = document.getElementById("terminal-container");
const resizeObserver = new ResizeObserver(() => {
  const tab = tabs.get(activeTabUiId);
  if (tab?.type === "terminal") fitAllPanes(tab);
});
resizeObserver.observe(terminalContainer);

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
  const tab = tabs.get(activeTabUiId);
  if (tab?.type === "terminal") fitAllPanes(tab);
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
        const quoted = path.includes(" ") ? `"${path}"` : path;
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
    createTab();
  }
  if (e.metaKey && e.key === "w") {
    e.preventDefault();
    closeTab(activeTabUiId);
  }
  if (e.metaKey && e.key === "d") {
    e.preventDefault();
    if (!isEditorTab) splitPane();
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
  const tab = tabs.get(activeTabUiId);
  if (tab?.type === "terminal") {
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

// Boot
async function boot() {
  const settings = await loadSettings();

  // Apply saved sidebar width
  if (settings.sidebarWidth) {
    sidebar.style.width = settings.sidebarWidth + "px";
  }

  await createTab();

  await initFileBrowser(() => getActivePtyId(), (filePath) => createEditorTab(filePath), settings.defaultDirectory);

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
  });

  fetchGitStatus(getCurrentPath());
  startGitPolling(getCurrentPath, settings.gitPollInterval);
}

boot();
