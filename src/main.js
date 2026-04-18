import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import "@xterm/xterm/css/xterm.css";
import { createEditor, getLangName } from "./editor.js";
import { undoDepth, redoDepth } from "@codemirror/commands";
import { EditorView } from "@codemirror/view";
import { initFileBrowser, getCurrentPath, closeFilePreview, refreshFileBrowser } from "./filebrowser.js";
import { fetchGitStatus, startGitPolling } from "./git.js";
import { initGitPanel, refreshPanel } from "./gitpanel.js";
import { loadSettings, saveSetting, getSettings } from "./settings.js";
import { initTheme, setTheme, getResolvedTheme, onThemeChange } from "./theme.js";
import { initQuickOpen, show as showQuickOpen } from "./quickopen.js";
import { initProjectSearch, showProjectSearch } from "./projectsearch.js";
import { loadFileSettings, getOverrides, setOverride } from "./filesettings.js";
import { matches as keyMatches } from "./keymap.js";
import { createSettingsPanel } from "./settingspanel.js";
import { addProject, touchProject, setActiveProject, getActiveProject, focusProjectWindow, registerProjectWindow, unregisterProjectWindow, unregisterCurrentWindow } from "./projects.js";
import { showPicker, hidePicker } from "./projectpicker.js";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// Tab management
// Terminal tab: { type: "terminal", panes: [pane, pane?], containerEl, name, activePane: 0|1 }
// Editor tab:   { type: "editor", containerEl, filePath, fileName, editorView, originalContent, modified }
// Each pane: { ptyId, term, fitAddon, el }
const tabs = new Map(); // uiTabId → tab object
const paneMap = new Map(); // ptyId → pane object (for routing PTY output)
let activeTabUiId = -1;
let nextUiTabId = 0;
let isCreatingTab = false;
let panelTransitioning = false;
// fitAllPanes uses a trailing-edge debounce stored on tab.fitDebounceTimer.
const FIT_DEBOUNCE_MS = 60; // Coalesces ResizeObserver firehose during drags
// Flow control: pause PTY reader when xterm.js write queue exceeds high water,
// resume when it drains below low water. Prevents ghost characters by keeping
// PTY output rate in sync with xterm.js rendering speed.
const FLOW_HIGH_WATER = 100_000; // chars — pause PTY when exceeded
const FLOW_LOW_WATER = 25_000;   // chars — resume PTY when drained below

// Escape handler stack — topmost handler fires first, prevents conflicts
// between Quick Open, file search, diff preview, and dialogs.
const escapeStack = [];
export function pushEscape(handler) { escapeStack.push(handler); }
export function popEscape(handler) {
  const i = escapeStack.lastIndexOf(handler);
  if (i !== -1) escapeStack.splice(i, 1);
}

/** Wait for the browser to complete layout reflow (double-rAF). */
function waitForLayout() {
  return new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
}

// =============================================================================
// Debug capture — toggle via toolbar button (⦿). Captures PTY output bytes,
// keystrokes, fit/resize events, and user-placed marks to an NDJSON log at
// ~/.launchpad/debug.log. Zero overhead when off.
// =============================================================================
let debugCaptureActive = false;
let debugLog = [];
let debugStartTime = 0;
const DEBUG_MAX_ENTRIES = 500_000;

function hexOf(s) {
  if (typeof s !== "string") s = String(s);
  const bytes = new TextEncoder().encode(s);
  let out = "";
  for (let i = 0; i < bytes.length; i++) {
    out += bytes[i].toString(16).padStart(2, "0");
  }
  return out;
}

function dbg(event, data) {
  if (!debugCaptureActive) return;
  if (debugLog.length >= DEBUG_MAX_ENTRIES) return;
  debugLog.push({
    t: Math.round(performance.now() - debugStartTime),
    event,
    ...data,
  });
}

function startDebugCapture() {
  debugLog = [];
  debugStartTime = performance.now();
  debugCaptureActive = true;
  const tab = tabs.get(activeTabUiId);
  const pane = tab?.type === "terminal" ? tab.panes[tab.activePane] : null;
  dbg("capture_started", {
    activePty: pane?.ptyId ?? null,
    rows: pane?.term?.rows ?? null,
    cols: pane?.term?.cols ?? null,
    containerW: document.getElementById("terminal-container")?.clientWidth ?? null,
    containerH: document.getElementById("terminal-container")?.clientHeight ?? null,
  });
  updateDebugButton();
}

async function stopDebugCapture() {
  debugCaptureActive = false;
  dbg("capture_stopped", { totalEntries: debugLog.length });
  const totalEntries = debugLog.length;
  const content = debugLog.map((e) => JSON.stringify(e)).join("\n");
  const btn = document.getElementById("debug-capture");
  try {
    const path = await invoke("write_debug_log", { content });
    console.log(`[debug-capture] Wrote ${totalEntries} events → ${path}`);
    if (btn) {
      btn.title = `Saved ${totalEntries} events → ${path}`;
      btn.classList.add("debug-saved");
      setTimeout(() => btn.classList.remove("debug-saved"), 2000);
    }
  } catch (e) {
    console.error(`[debug-capture] Write failed:`, e);
    if (btn) btn.title = `Debug capture write failed: ${e}`;
  }
  updateDebugButton();
}

function markDebug(label = "MARK") {
  if (!debugCaptureActive) return;
  dbg("mark", { label });
  // Visible feedback so the user knows the mark landed.
  const btn = document.getElementById("debug-capture");
  if (btn) {
    btn.classList.add("debug-marked");
    setTimeout(() => btn.classList.remove("debug-marked"), 200);
  }
}

function updateDebugButton() {
  const btn = document.getElementById("debug-capture");
  if (!btn) return;
  if (debugCaptureActive) {
    btn.classList.add("debug-recording");
    btn.title = "Recording — click to stop. Cmd+Shift+X to mark.";
  } else {
    btn.classList.remove("debug-recording");
    btn.title = "Debug capture";
  }
}

const darkTerminalTheme = {
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

const lightTerminalTheme = {
  background: "#fafafa",
  foreground: "#383a42",
  cursor: "#383a42",
  cursorAccent: "#fafafa",
  selectionBackground: "#d0d0d0",
  black: "#383a42",
  red: "#e45649",
  green: "#50a14f",
  yellow: "#c18401",
  blue: "#4078f2",
  magenta: "#a626a4",
  cyan: "#0184bc",
  white: "#a0a1a7",
  brightBlack: "#696c77",
  brightRed: "#e45649",
  brightGreen: "#50a14f",
  brightYellow: "#c18401",
  brightBlue: "#4078f2",
  brightMagenta: "#a626a4",
  brightCyan: "#0184bc",
  brightWhite: "#383a42",
};

function getTerminalTheme() {
  return getResolvedTheme() === "light" ? lightTerminalTheme : darkTerminalTheme;
}

async function createPane(parentEl, cwd) {
  const s = getSettings();
  const term = new Terminal({
    fontFamily: s.termFontFamily,
    fontSize: s.termFontSize,
    theme: getTerminalTheme(),
    cursorBlink: s.termCursorBlink,
    cursorStyle: s.termCursorStyle,
    scrollback: s.termScrollback,
    allowProposedApi: true,
  });

  const fitAddon = new FitAddon();
  term.loadAddon(fitAddon);
  // Use Unicode 11 width tables so character widths match what modern CLI tools
  // (Claude Code, Ink-based TUIs) expect. Without this, symbols like ● get
  // wrong-width measurements and leave ghost fragments when erased.
  const unicodeAddon = new Unicode11Addon();
  term.loadAddon(unicodeAddon);
  term.unicode.activeVersion = "11";

  const el = document.createElement("div");
  el.className = "pane";
  parentEl.appendChild(el);

  // NOTE: term.open() is deferred to _spawnPty so the canvas initializes with real dimensions
  const pane = {
    ptyId: null,
    term,
    fitAddon,
    el,
    // Flow control — backpressure replaces gate/buffer/settle resize coordination
    unackedChars: 0,
    paused: false,
    lastSentRows: 0,
    lastSentCols: 0,
  };

  term.onData((data) => {
    if (pane.ptyId !== null) {
      dbg("keystroke", { pty: pane.ptyId, len: data.length, hex: hexOf(data) });
      invoke("write_to_pty", { tabId: pane.ptyId, data });
    }
  });

  // Called after the container is visible — opens terminal, fits, then spawns PTY at correct size
  pane._spawnPty = async () => {
    await waitForLayout();
    // Wait for web fonts before measuring cell metrics — otherwise xterm measures
    // the fallback font, the WebGL atlas bakes in wrong cell dims, and glyph
    // fragments persist as "floating characters" once the real font loads.
    if (document.fonts && document.fonts.ready) {
      await document.fonts.ready;
    }
    term.open(el);
    try {
      term.loadAddon(new WebglAddon());
    } catch (e) {
      console.warn("WebGL renderer not available, using default");
    }
    fitAddon.fit();
    // Pass actual dimensions so the PTY starts at the right size — no resize race
    const result = await invoke("spawn_pty", {
      cwd: cwd || undefined,
      rows: term.rows,
      cols: term.cols,
    });
    pane.ptyId = result.tab_id;
    paneMap.set(pane.ptyId, pane);
    // Only now that the listener can route output to this pane, ask Rust to
    // start reading. Splitting this from spawn_pty closes the race where the
    // shell's first bytes (prompt, initial escape sequences) fired before
    // paneMap had an entry and were silently dropped — which left xterm's
    // internal state permanently desynced from the shell.
    await invoke("start_pty_reader", { tabId: pane.ptyId });
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

    const pane = await createPane(containerEl, getActiveProject()?.path);

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
    fitAllPanes(tab, { immediate: true });
    tab.panes[0].term.focus();
    return;
  }

  // Split: add second pane
  tab.containerEl.classList.add("split");

  const handle = document.createElement("div");
  handle.className = "split-handle";
  tab.containerEl.appendChild(handle);

  const secondPane = await createPane(tab.containerEl, getActiveProject()?.path);
  tab.panes.push(secondPane);
  tab.activePane = 1;

  // Split handle drag — listeners are scoped to each drag session so repeated
  // split/unsplit cycles don't accumulate orphaned document-level handlers.
  const onMouseMove = (e) => {
    const rect = tab.containerEl.getBoundingClientRect();
    const pct = ((e.clientX - rect.left) / rect.width) * 100;
    const clamped = Math.min(Math.max(pct, 20), 80);
    tab.panes[0].el.style.width = clamped + "%";
    tab.panes[1].el.style.width = (100 - clamped) + "%";
  };

  const onMouseUp = () => {
    document.removeEventListener("mousemove", onMouseMove);
    document.removeEventListener("mouseup", onMouseUp);
    handle.classList.remove("dragging");
    document.body.style.cursor = "";
    document.body.style.userSelect = "";
    fitAllPanes(tab);
  };

  handle.addEventListener("mousedown", (e) => {
    handle.classList.add("dragging");
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    document.addEventListener("mousemove", onMouseMove);
    document.addEventListener("mouseup", onMouseUp);
    e.preventDefault();
  });

  // Click to focus pane
  tab.panes[0].el.addEventListener("click", () => { tab.activePane = 0; });
  tab.panes[1].el.addEventListener("click", () => { tab.activePane = 1; });

  // Spawn PTY after layout is set so fit() gets correct dimensions
  await secondPane._spawnPty();
  fitAllPanes(tab, { immediate: true });
  secondPane.term.focus();
}

// Fit each pane and sync PTY dimensions. Backpressure (via flow control on the
// pty-output listener) naturally pauses the PTY when xterm.js is busy reflowing,
// so no gating, buffering, or settle timers are needed.
async function _runFit(tab) {
  dbg("runfit_start", { paneCount: tab.panes.length });
  for (const pane of tab.panes) {
    const beforeRows = pane.term.rows;
    const beforeCols = pane.term.cols;
    try {
      pane.fitAddon.fit();
    } catch (e) {
      // term.open() may not have run yet
    }
    dbg("fit", {
      pty: pane.ptyId,
      beforeRows,
      beforeCols,
      afterRows: pane.term.rows,
      afterCols: pane.term.cols,
    });
    if (pane.ptyId === null) continue;
    const rows = pane.term.rows;
    const cols = pane.term.cols;
    if (rows === pane.lastSentRows && cols === pane.lastSentCols) continue;
    pane.lastSentRows = rows;
    pane.lastSentCols = cols;
    try {
      await invoke("resize_pty", { tabId: pane.ptyId, rows, cols });
      dbg("resize_pty_ack", { pty: pane.ptyId, rows, cols });
    } catch (e) {
      console.warn("resize_pty failed", e);
    }
  }
}

// Public entry point. Trailing-edge debounce coalesces ResizeObserver firehose.
// Pass { immediate: true } for programmatic single-shot fits (tab switch, panel transitions).
function fitAllPanes(tab, { immediate = false } = {}) {
  if (!tab || tab.type !== "terminal") return;
  if (tab.fitDebounceTimer) {
    clearTimeout(tab.fitDebounceTimer);
    tab.fitDebounceTimer = null;
  }
  if (immediate) {
    _runFit(tab);
    return;
  }
  tab.fitDebounceTimer = setTimeout(() => {
    tab.fitDebounceTimer = null;
    _runFit(tab);
  }, FIT_DEBOUNCE_MS);
}

function getActivePtyId() {
  const tab = tabs.get(activeTabUiId);
  if (!tab || tab.type !== "terminal") return null;
  return tab.panes[tab.activePane]?.ptyId ?? tab.panes[0]?.ptyId ?? null;
}

function switchTab(uiId) {
  if (activeTabUiId === uiId && tabs.has(uiId)) return;

  // Hide current tab
  if (tabs.has(activeTabUiId)) {
    tabs.get(activeTabUiId).containerEl.style.display = "none";
  }

  const tab = tabs.get(uiId);
  if (!tab) return;
  tab.containerEl.style.display = "flex";
  activeTabUiId = uiId;

  if (tab.type === "terminal") {
    fitAllPanes(tab, { immediate: true });
    requestAnimationFrame(() => {
      const activePane = tab.panes[tab.activePane] || tab.panes[0];
      if (activePane) activePane.term.focus();
    });
  } else if (tab.type === "editor") {
    setTimeout(() => tab.editorView.focus(), 10);
  }

  renderTabBar();
}

function showConfirmDialog(message, onConfirm) {
  document.querySelector(".confirm-overlay")?.remove();
  const overlay = document.createElement("div");
  overlay.className = "confirm-overlay";
  overlay.innerHTML = `
    <div class="confirm-dialog">
      <div class="confirm-message">${message}</div>
      <div class="confirm-actions">
        <button class="confirm-btn confirm-cancel">Cancel</button>
        <button class="confirm-btn confirm-ok">Close</button>
      </div>
    </div>`;
  document.body.appendChild(overlay);

  const dismiss = () => { overlay.remove(); popEscape(dismiss); };
  overlay.querySelector(".confirm-cancel").addEventListener("click", dismiss);
  overlay.querySelector(".confirm-ok").addEventListener("click", () => { dismiss(); onConfirm(); });
  overlay.addEventListener("click", (e) => { if (e.target === overlay) dismiss(); });
  pushEscape(dismiss);
}

async function closeTab(uiId) {
  const tab = tabs.get(uiId);
  if (!tab) return;

  if (tab.type === "editor" && tab.modified) {
    showConfirmDialog(
      `\u201C${tab.fileName}\u201D has unsaved changes. Close anyway?`,
      () => doCloseTab(uiId)
    );
    return;
  }
  if (tab.type === "terminal" && tab.panes.some(p => p.ptyId !== null)) {
    showConfirmDialog(
      "Terminal has a running process. Close anyway?",
      () => doCloseTab(uiId)
    );
    return;
  }
  await doCloseTab(uiId);
}

async function doCloseTab(uiId) {
  const tab = tabs.get(uiId);
  if (!tab) return;

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
async function createEditorTab(filePath, options = {}) {
  const { line, column } = options;
  // Deduplicate: if already open, switch to it
  for (const [uiId, tab] of tabs) {
    if (tab.type === "editor" && tab.filePath === filePath) {
      switchTab(uiId);
      if (line) requestAnimationFrame(() => gotoLine(tab.editorView, line, column));
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
  const overrides = getOverrides(filePath) || {};
  const effectiveTabSize = overrides.tabSize ?? editorSettings.editorTabSize;
  const effectiveWordWrap = overrides.wordWrap ?? editorSettings.editorWordWrap;

  // Detect line endings from the raw content — preserve what the file already uses.
  const lineEndings = overrides.lineEndings || (content.includes("\r\n") ? "CRLF" : "LF");

  tab.tabSize = effectiveTabSize;
  tab.wordWrap = effectiveWordWrap;
  tab.lineEndings = lineEndings;

  const updateStatus = () => renderEditorStatus(tab, statusBar);

  const editorHandle = createEditor(editorContent, content, fileName, {
    onChange: (newContent) => {
      tab.modified = newContent !== tab.originalContent;
      renderTabBar();
      updateStatus();
    },
    onCursorChange: (line, col) => {
      tab.cursor = { line, col };
      updateStatus();
    },
    tabSize: effectiveTabSize,
    wordWrap: effectiveWordWrap,
    vimMode: editorSettings.editorVimMode,
    theme: getResolvedTheme(),
  });

  const editorView = editorHandle.view;
  tab.editorView = editorView;
  tab.editorHandle = editorHandle;
  tabs.set(uiId, tab);
  updateStatus();

  // Right-click context menu for editor
  editorContent.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    const old = document.getElementById("context-menu");
    if (old) old.remove();

    const menu = document.createElement("div");
    menu.id = "context-menu";
    menu.className = "context-menu";

    const sel = editorView.state.sliceDoc(
      editorView.state.selection.main.from,
      editorView.state.selection.main.to
    );

    const copyItem = document.createElement("div");
    copyItem.className = "context-menu-item";
    copyItem.textContent = "Copy";
    if (!sel) copyItem.classList.add("context-menu-item-disabled");
    copyItem.addEventListener("click", () => {
      if (sel) navigator.clipboard.writeText(sel);
      menu.remove();
    });

    const cutItem = document.createElement("div");
    cutItem.className = "context-menu-item";
    cutItem.textContent = "Cut";
    if (!sel) cutItem.classList.add("context-menu-item-disabled");
    cutItem.addEventListener("click", () => {
      if (sel) {
        navigator.clipboard.writeText(sel);
        editorView.dispatch({ changes: editorView.state.selection.main });
      }
      menu.remove();
    });

    const pasteItem = document.createElement("div");
    pasteItem.className = "context-menu-item";
    pasteItem.textContent = "Paste";
    pasteItem.addEventListener("click", async () => {
      const text = await navigator.clipboard.readText();
      if (text) {
        editorView.dispatch({
          changes: { from: editorView.state.selection.main.from, to: editorView.state.selection.main.to, insert: text },
        });
      }
      menu.remove();
    });

    const selectAllItem = document.createElement("div");
    selectAllItem.className = "context-menu-item";
    selectAllItem.textContent = "Select All";
    selectAllItem.addEventListener("click", () => {
      editorView.dispatch({
        selection: { anchor: 0, head: editorView.state.doc.length },
      });
      menu.remove();
    });

    menu.appendChild(cutItem);
    menu.appendChild(copyItem);
    menu.appendChild(pasteItem);
    const sep = document.createElement("div");
    sep.className = "context-menu-separator";
    menu.appendChild(sep);
    menu.appendChild(selectAllItem);

    menu.style.left = e.clientX + "px";
    menu.style.top = e.clientY + "px";
    document.body.appendChild(menu);

    setTimeout(() => {
      document.addEventListener("click", () => menu.remove(), { once: true });
    }, 0);
  });

  renderTabBar();
  switchTab(uiId);
  if (line) requestAnimationFrame(() => gotoLine(editorView, line, column));
  return uiId;
}

const TAB_SIZE_CYCLE = [2, 4, 8];

function renderEditorStatus(tab, statusBar) {
  const view = tab.editorView;
  const ud = view ? undoDepth(view.state) : 0;
  const rd = view ? redoDepth(view.state) : 0;
  const { line, col } = tab.cursor || { line: 1, col: 1 };

  statusBar.innerHTML = "";

  const pos = document.createElement("span");
  pos.className = "esb-pos";
  pos.textContent = `Ln ${line}, Col ${col}`;
  statusBar.appendChild(pos);

  const addSep = () => {
    const s = document.createElement("span");
    s.className = "esb-sep";
    s.textContent = "·";
    statusBar.appendChild(s);
  };

  addSep();
  const lang = document.createElement("span");
  lang.className = "esb-lang";
  lang.textContent = getLangName(tab.fileName);
  statusBar.appendChild(lang);

  addSep();
  const lineEnd = document.createElement("span");
  lineEnd.className = "esb-item esb-click";
  lineEnd.title = "Click to toggle line endings";
  lineEnd.textContent = tab.lineEndings;
  lineEnd.addEventListener("click", async () => {
    tab.lineEndings = tab.lineEndings === "LF" ? "CRLF" : "LF";
    await setOverride(tab.filePath, "lineEndings", tab.lineEndings);
    renderEditorStatus(tab, statusBar);
    tab.modified = true;
    renderTabBar();
  });
  statusBar.appendChild(lineEnd);

  addSep();
  const tabSize = document.createElement("span");
  tabSize.className = "esb-item esb-click";
  tabSize.title = "Click to change tab size";
  tabSize.textContent = `Tab ${tab.tabSize}`;
  tabSize.addEventListener("click", async () => {
    const idx = TAB_SIZE_CYCLE.indexOf(tab.tabSize);
    const next = TAB_SIZE_CYCLE[(idx + 1) % TAB_SIZE_CYCLE.length];
    tab.tabSize = next;
    tab.editorHandle.setTabSize(next);
    await setOverride(tab.filePath, "tabSize", next);
    renderEditorStatus(tab, statusBar);
  });
  statusBar.appendChild(tabSize);

  addSep();
  const wrap = document.createElement("span");
  wrap.className = "esb-item esb-click" + (tab.wordWrap ? " esb-active" : "");
  wrap.title = "Click to toggle word wrap";
  wrap.textContent = tab.wordWrap ? "Wrap" : "No Wrap";
  wrap.addEventListener("click", async () => {
    tab.wordWrap = !tab.wordWrap;
    tab.editorHandle.setWordWrap(tab.wordWrap);
    await setOverride(tab.filePath, "wordWrap", tab.wordWrap);
    renderEditorStatus(tab, statusBar);
  });
  statusBar.appendChild(wrap);

  if (ud || rd) {
    addSep();
    const hist = document.createElement("span");
    hist.className = "esb-hist";
    hist.textContent = `Undo: ${ud} Redo: ${rd}`;
    statusBar.appendChild(hist);
  }
}

function gotoLine(view, lineNum, col) {
  if (!view) return;
  const doc = view.state.doc;
  const target = Math.max(1, Math.min(lineNum, doc.lines));
  const lineInfo = doc.line(target);
  const pos = lineInfo.from + Math.max(0, (col || 1) - 1);
  view.dispatch({
    selection: { anchor: pos },
    effects: EditorView.scrollIntoView(pos, { y: "center" }),
  });
  view.focus();
}

async function saveEditorTab(uiId) {
  const tab = tabs.get(uiId);
  if (!tab || tab.type !== "editor") return;

  try {
    let content = tab.editorView.state.doc.toString();
    if (tab.lineEndings === "CRLF") {
      content = content.replace(/\r?\n/g, "\r\n");
    }
    await invoke("write_file", { path: tab.filePath, content });
    tab.originalContent = tab.editorView.state.doc.toString();
    tab.modified = false;
    renderTabBar();
    // Flash the tab label green briefly to confirm save
    const tabEl = document.querySelector(`.tab[data-tab-id="${uiId}"] .tab-label`);
    if (tabEl) {
      tabEl.classList.add("tab-save-flash");
      setTimeout(() => tabEl.classList.remove("tab-save-flash"), 600);
    }
  } catch (err) {
    console.error("Save error:", err);
    // Show inline error toast
    const tabEl = document.querySelector(`.tab[data-tab-id="${uiId}"]`);
    if (tabEl) {
      const toast = document.createElement("div");
      toast.className = "save-error-toast";
      toast.textContent = `Save failed: ${err}`;
      tabEl.parentElement.appendChild(toast);
      setTimeout(() => toast.remove(), 3000);
    }
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
      }
      // Route through the gated/awaited resize path so font-metric changes
      // can't race with PTY output (Race #2 fix).
      fitAllPanes(tab, { immediate: true });
    }
  }

  if (key === "sidebarWidth") {
    document.getElementById("sidebar").style.width = value + "px";
  }

  if (key === "gitPollInterval") {
    startGitPolling(getCurrentPath, value);
  }

  if (key === "appTheme") {
    setTheme(value);
  }
}

function refreshTerminalsForTheme() {
  const nextTerm = getTerminalTheme();
  for (const tab of tabs.values()) {
    if (tab.type !== "terminal") continue;
    for (const pane of tab.panes) {
      pane.term.options.theme = nextTerm;
    }
  }
}

function renderTabBar() {
  const tabBar = document.getElementById("tab-bar");
  tabBar.innerHTML = "";

  let termIndex = 1;
  for (const [uiId, tab] of tabs) {
    if (tab._rightGroup) continue; // skip tabs in the right split group
    const isActive = uiId === activeTabUiId;
    const tabEl = document.createElement("div");
    tabEl.className = `tab ${isActive ? "tab-active" : ""}`;
    tabEl.dataset.tabId = uiId;
    tabEl.setAttribute("role", "tab");
    tabEl.setAttribute("aria-selected", isActive ? "true" : "false");

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

// Listen for PTY output — route by ptyId to correct pane.
// Uses backpressure flow control: pauses the PTY reader when xterm.js falls
// behind, resumes when drained. No gating or buffering needed.
listen("pty-output", (event) => {
  const { tab_id, data } = event.payload;
  const pane = paneMap.get(tab_id);
  if (!pane) {
    dbg("pty_output_dropped", { pty: tab_id, len: data.length, hex: hexOf(data) });
    return;
  }
  dbg("pty_output", {
    pty: tab_id,
    len: data.length,
    hex: hexOf(data),
    cursorX: pane.term.buffer?.active?.cursorX,
    cursorY: pane.term.buffer?.active?.cursorY,
    rows: pane.term.rows,
    cols: pane.term.cols,
  });

  pane.unackedChars += data.length;

  // Backpressure: pause PTY reader when xterm.js write queue is deep
  if (!pane.paused && pane.unackedChars > FLOW_HIGH_WATER) {
    pane.paused = true;
    invoke("pause_pty_reader", { tabId: tab_id });
  }

  // Write with ack callback — resume PTY when xterm.js drains below low water
  pane.term.write(data, () => {
    pane.unackedChars -= data.length;
    if (pane.paused && pane.unackedChars < FLOW_LOW_WATER && paneMap.has(tab_id)) {
      pane.paused = false;
      invoke("resume_pty_reader", { tabId: tab_id });
    }
  });
});

// Listen for PTY exit — auto-close the tab when the shell process exits
listen("pty-exit", (event) => {
  const { tab_id } = event.payload;
  paneMap.delete(tab_id);
  for (const [uiId, tab] of tabs) {
    if (tab.type !== "terminal") continue;
    const exitedPane = tab.panes.find((p) => p.ptyId === tab_id);
    if (!exitedPane) continue;
    exitedPane.ptyId = null;

    // Split tab with a surviving sibling: collapse just the exited pane
    // rather than closing the whole tab, which would kill the healthy shell
    // after a confusing "running process" prompt.
    const survivor = tab.panes.find((p) => p !== exitedPane && p.ptyId !== null);
    if (tab.panes.length === 2 && survivor) {
      exitedPane.term.dispose();
      exitedPane.el.remove();
      tab.panes = tab.panes.filter((p) => p !== exitedPane);
      const handle = tab.containerEl.querySelector(".split-handle");
      if (handle) handle.remove();
      tab.containerEl.classList.remove("split");
      tab.panes[0].el.style.width = "";
      tab.activePane = 0;
      fitAllPanes(tab, { immediate: true });
      tab.panes[0].term.focus();
      break;
    }

    // Otherwise (single-pane tab, or both panes exited): close the tab.
    if (tab._rightGroup) closeRightTab(uiId);
    else closeTab(uiId);
    break;
  }
});

// Listen for filesystem changes — refresh file browser and git panel
let fsRefreshScheduled = false;
listen("fs-changed", (event) => {
  const project = getActiveProject();
  // Watcher is only started for the active project root, so a mismatch means
  // this event is from a stale watcher the frontend no longer cares about.
  if (!project || event.payload.path !== project.path) return;
  if (fsRefreshScheduled) return;
  fsRefreshScheduled = true;
  setTimeout(async () => {
    fsRefreshScheduled = false;
    refreshFileBrowser();
    fetchGitStatus(project.path);
    refreshPanel(null, true);

    // Check open editor tabs for external file changes
    for (const [uiId, tab] of tabs) {
      if (tab.type !== "editor") continue;
      try {
        const diskContent = await invoke("read_file_preview", { path: tab.filePath, maxBytes: 10485760 });
        const editorContent = tab.editorView.state.doc.toString();
        if (diskContent === editorContent) continue;
        if (!tab.modified) {
          // No local edits — silently reload
          tab.editorView.dispatch({
            changes: { from: 0, to: editorContent.length, insert: diskContent },
          });
          tab.originalContent = diskContent;
        } else {
          // User has unsaved changes — prompt
          showConfirmDialog(
            `"${tab.fileName}" changed on disk. Reload and lose your changes?`,
            () => {
              tab.editorView.dispatch({
                changes: { from: 0, to: tab.editorView.state.doc.length, insert: diskContent },
              });
              tab.originalContent = diskContent;
              tab.modified = false;
              renderTabBar();
            }
          );
        }
      } catch (_) {}
    }
  }, 500);
});

// Resize observer
const terminalContainer = document.getElementById("terminal-container");
const resizeObserver = new ResizeObserver((entries) => {
  if (panelTransitioning) return;
  const entry = entries[0];
  dbg("resize_observer", {
    w: Math.round(entry?.contentRect?.width ?? 0),
    h: Math.round(entry?.contentRect?.height ?? 0),
  });
  const tab = tabs.get(activeTabUiId);
  if (tab?.type === "terminal") fitAllPanes(tab);
  // When workspace is split, the right group's terminal needs refitting too
  // — ResizeObserver only watches the left container, so without this the
  // right group's xterm grid stays CSS-stretched after window resizes.
  if (isSplit && rightActiveTabUiId !== -1) {
    const rightTab = tabs.get(rightActiveTabUiId);
    if (rightTab?.type === "terminal") fitAllPanes(rightTab);
  }
});
resizeObserver.observe(terminalContainer);

// Refit terminal after git panel close transition (ResizeObserver is blocked during transition)
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
  // While the project picker is visible no workspace exists yet — skip all
  // workspace shortcuts so the picker is interactable (Enter/typing etc.).
  if (!getActiveProject()) return;

  const activeTab = tabs.get(activeTabUiId);
  const isEditorTab = activeTab?.type === "editor";

  if (keyMatches(e, "newWindow")) {
    e.preventDefault();
    invoke("open_new_window", { path: null });
    return;
  }
  if (keyMatches(e, "openSettings")) {
    e.preventDefault();
    openSettingsTab();
    return;
  }
  if (keyMatches(e, "saveFile")) {
    e.preventDefault();
    if (isEditorTab) {
      saveEditorTab(activeTabUiId);
    }
    return;
  }
  if (keyMatches(e, "quickOpen")) {
    e.preventDefault();
    showQuickOpen(getCurrentPath());
    return;
  }
  if (keyMatches(e, "projectFind")) {
    e.preventDefault();
    showProjectSearch();
    return;
  }
  // Find in editor: let CodeMirror handle it for editor tabs
  if (keyMatches(e, "findInEditor")) {
    if (isEditorTab) {
      e.preventDefault();
      return;
    }
  }
  if (keyMatches(e, "findReplace")) {
    if (isEditorTab) {
      e.preventDefault();
      return;
    }
  }
  if (keyMatches(e, "clearTerminal")) {
    e.preventDefault();
    if (activeTab?.type === "terminal") {
      const pane = activeTab.panes[activeTab.activePane] || activeTab.panes[0];
      pane.term.clear();
      pane.term.focus();
    }
  }
  if (keyMatches(e, "newTab")) {
    e.preventDefault();
    if (isSplit && focusedGroup === "right") createTabInRight();
    else createTab();
  }
  if (keyMatches(e, "closeTab")) {
    e.preventDefault();
    if (isSplit && focusedGroup === "right") closeRightTab(rightActiveTabUiId);
    else closeTab(activeTabUiId);
  }
  if (keyMatches(e, "splitPane")) {
    e.preventDefault();
    if (!isEditorTab) splitPane();
  }
  if (keyMatches(e, "splitWorkspace")) {
    e.preventDefault();
    splitWorkspace();
  }
  if (isSplit && (keyMatches(e, "focusLeftGroup") || keyMatches(e, "focusRightGroup"))) {
    e.preventDefault();
    setFocusedGroup(keyMatches(e, "focusLeftGroup") ? "left" : "right");
  }
  if (keyMatches(e, "moveTabToOtherGroup") && isSplit) {
    e.preventDefault();
    if (focusedGroup === "left") moveTabToRight(activeTabUiId);
    else moveTabToLeft(rightActiveTabUiId);
  }
  if (keyMatches(e, "debugDump") && debugCaptureActive) {
    e.preventDefault();
    markDebug("USER_MARK");
  }
  if (keyMatches(e, "shortcutsModal")) {
    e.preventDefault();
    document.getElementById("shortcuts-modal").classList.toggle("shortcuts-visible");
    return;
  }
  if (e.key === "Escape") {
    if (escapeStack.length > 0) {
      e.preventDefault();
      escapeStack[escapeStack.length - 1]();
      return;
    }
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
let dropIndicator = null;

function getTabInsertIndex(tabBar, mouseX, dragUiId) {
  const tabEls = [...tabBar.querySelectorAll(".tab")];
  for (let i = 0; i < tabEls.length; i++) {
    const r = tabEls[i].getBoundingClientRect();
    const mid = r.left + r.width / 2;
    if (mouseX < mid) return i;
  }
  return tabEls.length;
}

function showDropIndicator(tabBar, insertIndex) {
  if (!dropIndicator) {
    dropIndicator = document.createElement("div");
    dropIndicator.className = "tab-drop-indicator";
  }
  const tabEls = [...tabBar.querySelectorAll(".tab")];
  if (insertIndex < tabEls.length) {
    tabBar.insertBefore(dropIndicator, tabEls[insertIndex]);
  } else {
    // Insert before the + button (last child) or append
    const addBtn = tabBar.querySelector(".tab-add");
    if (addBtn) tabBar.insertBefore(dropIndicator, addBtn);
    else tabBar.appendChild(dropIndicator);
  }
}

function reorderTabs(tabsMap, fromKey, toIndex) {
  const entries = [...tabsMap.entries()];
  const fromIdx = entries.findIndex(([k]) => k === fromKey);
  if (fromIdx === -1 || fromIdx === toIndex) return;
  const [entry] = entries.splice(fromIdx, 1);
  if (toIndex > fromIdx) toIndex--;
  entries.splice(toIndex, 0, entry);
  tabsMap.clear();
  entries.forEach(([k, v]) => tabsMap.set(k, v));
}

function startTabDrag(tabEl, uiId, sourceGroup, e) {
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

    const rightBar = rightTabBarEl;
    const leftBar = document.getElementById("tab-bar");
    if (rightBar) rightBar.classList.toggle("drag-over", isOverEl(me, rightBar));
    leftBar.classList.toggle("drag-over", isOverEl(me, leftBar));

    // Show reorder indicator within same group
    const sameBar = sourceGroup === "left" ? leftBar : rightBar;
    if (sameBar && isOverEl(me, sameBar)) {
      const idx = getTabInsertIndex(sameBar, me.clientX, uiId);
      showDropIndicator(sameBar, idx);
    } else if (dropIndicator && dropIndicator.parentNode) {
      dropIndicator.remove();
    }
  };

  const onMouseUp = (me) => {
    document.removeEventListener("mousemove", onMouseMove);
    document.removeEventListener("mouseup", onMouseUp);
    if (dragGhost) { dragGhost.remove(); dragGhost = null; }
    if (dropIndicator && dropIndicator.parentNode) dropIndicator.remove();

    const leftBar = document.getElementById("tab-bar");
    const rightBar = rightTabBarEl;
    leftBar.classList.remove("drag-over");
    if (rightBar) rightBar.classList.remove("drag-over");

    if (!dragState || !dragState.started) { dragState = null; return; }

    // Cross-group moves (only when split)
    if (isSplit && rightBar && isOverEl(me, rightBar) && dragState.sourceGroup === "left") {
      moveTabToRight(dragState.uiId);
    } else if (isSplit && isOverEl(me, leftBar) && dragState.sourceGroup === "right") {
      moveTabToLeft(dragState.uiId);
    } else {
      // Within-group reorder
      const sameBar = sourceGroup === "left" ? leftBar : rightBar;
      if (sameBar && isOverEl(me, sameBar)) {
        const insertIdx = getTabInsertIndex(sameBar, me.clientX, uiId);
        if (sourceGroup === "left") {
          // Reorder in main tabs Map (only non-right-group tabs)
          const leftEntries = [...tabs.entries()].filter(([, t]) => !t._rightGroup);
          const fromIdx = leftEntries.findIndex(([k]) => k === uiId);
          if (fromIdx !== -1 && fromIdx !== insertIdx) {
            reorderTabs(tabs, uiId, insertIdx);
            renderTabBar();
          }
        } else {
          // Reorder in rightGroupTabIds array
          const fromIdx = rightGroupTabIds.indexOf(uiId);
          if (fromIdx !== -1 && fromIdx !== insertIdx) {
            rightGroupTabIds.splice(fromIdx, 1);
            const adj = insertIdx > fromIdx ? insertIdx - 1 : insertIdx;
            rightGroupTabIds.splice(adj, 0, uiId);
            renderRightTabBar();
          }
        }
      }
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

// Back to projects — tear down the current workspace and return to the picker.
document.getElementById("back-to-projects").addEventListener("click", async () => {
  // Close every PTY across all tabs + panes + right group.
  const ptyIds = [];
  tabs.forEach((tab) => {
    if (tab.type === "terminal" && Array.isArray(tab.panes)) {
      tab.panes.forEach((pane) => {
        if (pane.ptyId !== null && pane.ptyId !== undefined) ptyIds.push(pane.ptyId);
      });
    }
  });
  await Promise.all(ptyIds.map((id) => invoke("close_pty", { tabId: id }).catch(() => {})));

  const project = getActiveProject();
  if (project) {
    await unregisterProjectWindow(project.path).catch(() => {});
    invoke("unwatch_directory", { path: project.path }).catch(() => {});
  }

  // Full reload resets all module state and triggers boot() → picker.
  setActiveProject(null);
  window.location.reload();
});

// New window button
document.getElementById("open-new-window").addEventListener("click", () => invoke("open_new_window", { path: null }));

// Settings button
document.getElementById("open-settings").addEventListener("click", () => openSettingsTab());

// Debug capture toolbar button — toggles NDJSON capture of PTY/xterm events.
document.getElementById("debug-capture").addEventListener("click", () => {
  if (debugCaptureActive) stopDebugCapture();
  else startDebugCapture();
});

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
    showConfirmDialog(
      `\u201C${tab.fileName}\u201D has unsaved changes. Close anyway?`,
      () => doCloseRightTab(uiId)
    );
    return;
  }
  if (tab.type === "terminal" && tab.panes.some(p => p.ptyId !== null)) {
    showConfirmDialog(
      "Terminal has a running process. Close anyway?",
      () => doCloseRightTab(uiId)
    );
    return;
  }
  await doCloseRightTab(uiId);
}

async function doCloseRightTab(uiId) {
  const tab = tabs.get(uiId);
  if (!tab) return;

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

    const pane = await createPane(containerEl, getActiveProject()?.path);
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

async function moveTabToRight(uiId) {
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
      // Last tab leaving the left group: mint a fresh terminal so the group
      // doesn't end up with activeTabUiId === -1 and a dead `+` button.
      tab.containerEl.style.display = "none";
      activeTabUiId = -1;
      await createTab();
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

  // Split handle drag — scope listeners to each drag session so repeated
  // Cmd+\ toggles don't leave orphaned document-level handlers behind.
  const onSplitMove = (e) => {
    const rect = wrapper.getBoundingClientRect();
    const pct = ((e.clientX - rect.left) / rect.width) * 100;
    const clamped = Math.min(Math.max(pct, 20), 80);
    document.getElementById("group-left").style.flex = `0 0 ${clamped}%`;
    rightGroup.style.flex = `0 0 ${100 - clamped}%`;
  };
  const onSplitUp = () => {
    document.removeEventListener("mousemove", onSplitMove);
    document.removeEventListener("mouseup", onSplitUp);
    splitHandleEl.classList.remove("dragging");
    document.body.style.cursor = "";
    document.body.style.userSelect = "";
    // Refit both groups' active terminals — their container widths just
    // changed via flex, but xterm's internal col/row count is still the
    // pre-drag value. Without this, xterm's grid is CSS-stretched and
    // characters render at wrong positions.
    const leftTab = tabs.get(activeTabUiId);
    if (leftTab?.type === "terminal") fitAllPanes(leftTab);
    if (rightActiveTabUiId !== -1) {
      const rightTab = tabs.get(rightActiveTabUiId);
      if (rightTab?.type === "terminal") fitAllPanes(rightTab);
    }
  };
  splitHandleEl.addEventListener("mousedown", (e) => {
    splitHandleEl.classList.add("dragging");
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    document.addEventListener("mousemove", onSplitMove);
    document.addEventListener("mouseup", onSplitUp);
    e.preventDefault();
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

// Route a picker row click to the right window. If the project is already open
// somewhere, focus that window and leave the picker here alone. Otherwise the
// current window takes over and becomes the project window — no extra window
// is spawned. Use Cmd+Shift+N to get genuine multi-window.
async function openProjectInWindow(project, settings) {
  await touchProject(project.path);
  const focused = await focusProjectWindow(project.path);
  if (focused) return;
  await enterWorkspace(project, settings);
}

// Boot
async function boot() {
  const settings = await loadSettings();
  initTheme(settings.appTheme);
  onThemeChange(refreshTerminalsForTheme);
  await loadFileSettings();

  // A window booting with ?folder= is a dedicated project window — enter it directly.
  const params = new URLSearchParams(window.location.search);
  const folderParam = params.get("folder");

  let startProject = null;
  if (folderParam) {
    const decoded = decodeURIComponent(folderParam);
    try {
      startProject = await addProject(decoded);
    } catch (err) {
      console.error("Failed to add project from ?folder=:", err);
    }
  }
  // No auto-migration from legacy defaultDirectory. If projects.json is empty, the
  // user sees the welcome state and adds a project explicitly — that's the whole
  // point of the picker being truthful about what they've opened.

  if (startProject) {
    await enterWorkspace(startProject, settings);
  } else {
    // Scrub any stale project registration for this window (e.g. from Cmd+R
    // after a previous workspace session) so focus_project_window won't think
    // this window is still hosting a project.
    await unregisterCurrentWindow().catch(() => {});
    await showPicker((project) => openProjectInWindow(project, settings));
  }
}

async function enterWorkspace(project, settings) {
  setActiveProject(project);
  await touchProject(project.path);
  // Register this window as hosting the project so other windows' pickers focus
  // it instead of spawning duplicates. Awaited — if the register races with a
  // second window's focus_project_window, the second window could otherwise
  // miss our registration and open a duplicate.
  try {
    await registerProjectWindow(project.path);
  } catch (err) {
    console.error("registerProjectWindow failed:", err);
  }
  hidePicker();
  document.getElementById("workspace-root").hidden = false;

  // Apply saved sidebar width
  if (settings.sidebarWidth) {
    sidebar.style.width = settings.sidebarWidth + "px";
  }

  // Init file browser FIRST so getCurrentPath() is set before the terminal spawns
  await initFileBrowser(() => getActivePtyId(), (filePath) => createEditorTab(filePath), project.path);

  const { getCurrentWindow } = window.__TAURI__.window;
  getCurrentWindow().setTitle(project.name || "Launchpad");

  try {
    await createTab();
  } catch (err) {
    document.getElementById("terminal-container").innerHTML =
      `<div style="padding:20px;color:#ff6b6b;">Failed to start terminal: ${err}</div>`;
  }

  // Everything project-scoped pins to project.path — no re-pointing on sub-folder navigation.
  const projectPathFn = () => project.path;
  invoke("watch_directory", { path: project.path });
  initQuickOpen(projectPathFn, (fullPath) => createEditorTab(fullPath));
  initProjectSearch(projectPathFn, (fullPath, opts) => createEditorTab(fullPath, opts));
  initGitPanel(projectPathFn, (filePath) => createEditorTab(filePath));
  fetchGitStatus(project.path);
  startGitPolling(projectPathFn, settings.gitPollInterval);
}

export function setPanelTransitioning(value) {
  panelTransitioning = value;
}

boot();
