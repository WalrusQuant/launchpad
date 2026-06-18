import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import "@xterm/xterm/css/xterm.css";
import { collectSymbols, normalizeLspSymbols } from "./symbols.js";
import { initFileBrowser, getCurrentPath, closeFilePreview, refreshFileBrowser, revealPath } from "./filebrowser.js";
import { fetchGitStatus, startGitPolling, setInFlightOp } from "./git.js";
import { initGitPanel, refreshPanel } from "./gitpanel.js";
import { loadSettings, saveSetting, getSettings } from "./settings.js";
import { initTheme, setTheme, getResolvedTheme, onThemeChange } from "./theme.js";
import { initQuickOpen, show as showQuickOpen } from "./quickopen.js";
import { initProjectSearch, showProjectSearch } from "./projectsearch.js";
import { loadFileSettings, setOverride } from "./filesettings.js";
import { matches as keyMatches } from "./keymap.js";
import { createSettingsPanel } from "./settingspanel.js";
import { addProject, touchProject, setActiveProject, getActiveProject, focusProjectWindow, registerProjectWindow, unregisterProjectWindow, unregisterCurrentWindow } from "./projects.js";
import { showPicker, hidePicker } from "./projectpicker.js";
import { PTY_OUTPUT, PTY_EXIT, FS_CHANGED, PATH_RENAMED, PANEL_TRANSITION_DONE, HEAD_MOVED } from "./events.js";
import { shutdownAllLsp, lspDocumentSymbols } from "./lspclient.js";
import {
  tabs, activeTabUiId, setActiveTabId, nextTabId,
  pushEscape, popEscape, runTopEscape, setTabHooks,
  getActivePtyId, switchTab, closeTab, doCloseTab, showConfirmDialog,
  renderTabBar,
} from "./tabs.js";
import {
  debugCaptureActive, hexOf, dbg, startDebugCapture, stopDebugCapture, markDebug,
} from "./debugcapture.js";
import { showToast } from "./toast.js";
import { createDiffTab } from "./difftab.js";
import { showSymbolOutline } from "./symbolpalette.js";
import {
  setHomeDir, renderBreadcrumb, refreshEditorGutter,
} from "./editorchrome.js";
import { saveMergeTab } from "./mergetab.js";
import { createEditorTab, saveEditorTab } from "./editortab.js";
import { createRebaseTab } from "./rebasetab.js";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// Tab management — the registry + spine (tabs Map, activeTabUiId, switch/close,
// escape stack, confirm dialog) live in tabs.js; main.js wires the
// type-specific behavior via setTabHooks (see boot).
// Terminal tab: { type: "terminal", panes: [pane, pane?], containerEl, name, activePane: 0|1 }
// Editor tab:   { type: "editor", containerEl, filePath, fileName, inode, editorView, originalContent, modified, stale }
// Each pane: { ptyId, term, fitAddon, el }
const paneMap = new Map(); // ptyId → pane object (for routing PTY output)
let isCreatingTab = false;
let panelTransitioning = false;
// fitAllPanes uses a trailing-edge debounce stored on tab.fitDebounceTimer.
const FIT_DEBOUNCE_MS = 60; // Coalesces ResizeObserver firehose during drags
// Flow control: pause PTY reader when xterm.js write queue exceeds high water,
// resume when it drains below low water. Prevents ghost characters by keeping
// PTY output rate in sync with xterm.js rendering speed.
const FLOW_HIGH_WATER = 100_000; // chars — pause PTY when exceeded
const FLOW_LOW_WATER = 25_000;   // chars — resume PTY when drained below

/** Wait for the browser to complete layout reflow (double-rAF). */
function waitForLayout() {
  return new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
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

// Load the WebGL renderer for a pane and, critically, recover from context
// loss. Browsers cap the number of live WebGL contexts (~8-16) and evict the
// least-recently-used one when the cap is hit. With a split workspace you have
// 2+ live terminal canvases; clicking from one group to the other makes the
// group you just LEFT the eviction candidate — its context is lost and, with
// no handler, its canvas stops painting (the "one terminal goes blank" bug,
// issue #3). On loss we dispose the dead addon and load a fresh one so the
// terminal repaints; if WebGL is unavailable entirely we fall back to canvas.
function loadWebgl(pane) {
  try {
    const addon = new WebglAddon();
    addon.onContextLoss(() => {
      addon.dispose();
      // Re-create on the next frame so the GL stack has settled after the loss.
      // Guard on our own `disposed` flag, not term.element: xterm's element
      // getter survives term.dispose(), so a context-loss whose rAF fires after
      // destroyPane would otherwise call loadAddon on a dead terminal and leak
      // a GL context.
      requestAnimationFrame(() => {
        if (!pane.disposed) loadWebgl(pane);
      });
    });
    pane.term.loadAddon(addon);
  } catch (e) {
    console.warn("WebGL renderer not available, using default");
  }
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
    disposed: false,
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
    loadWebgl(pane);
    fitAddon.fit();
    // Pass actual dimensions so the PTY starts at the right size — no resize race.
    // projectPath is the key Rust uses to look up per-project env vars (see
    // load_env_for_project in lib.rs). At every call site today cwd IS the
    // project path — the file browser never changes the PTY's cwd, so reusing
    // the same value is correct.
    const result = await invoke("spawn_pty", {
      cwd: cwd || undefined,
      projectPath: cwd || undefined,
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
  // Mark disposed BEFORE term.dispose() so a WebGL context-loss rAF queued by
  // this pane's renderer can't re-create an addon on the dead terminal.
  pane.disposed = true;
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
    const uiId = nextTabId();

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

// Settings tab
function openSettingsTab() {
  // Deduplicate
  for (const [uiId, tab] of tabs) {
    if (tab.type === "settings") {
      switchTab(uiId);
      return;
    }
  }

  const uiId = nextTabId();
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

  // Apply editor settings to all open editor / merge tabs. Only the keys with
  // a live reconfigure path are handled here; others (vimMode, etc.) still take
  // effect on the next file open.
  if (key === "editorFontSize") {
    for (const tab of tabs.values()) {
      if (tab.type === "editor" && tab.editorHandle) {
        tab.editorHandle.setFontSize(value);
      } else if (tab.type === "merge" && tab.editorHandles) {
        for (const h of tab.editorHandles) h.setFontSize(value);
      }
    }
  }

  if (key === "editorIndentGuides") {
    const flags = { indentGuides: value };
    for (const tab of tabs.values()) {
      if (tab.type === "editor" && tab.editorHandle) {
        tab.editorHandle.setVisualExtras(flags);
      } else if (tab.type === "merge" && tab.editorHandles) {
        for (const h of tab.editorHandles) h.setVisualExtras(flags);
      }
    }
  }

  if (key === "sidebarWidth") {
    document.getElementById("sidebar").style.width = value + "px";
  }

  if (key === "gitPollInterval") {
    // Polling is project-scoped (see enterWorkspace at init), not keyed to
    // the file-browser sub-folder cursor. Use the active project path so a
    // mid-session interval change doesn't re-point polling at whatever
    // sub-folder the browser is currently showing.
    startGitPolling(() => getActiveProject()?.path, value);
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
listen(PTY_OUTPUT, (event) => {
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
listen(PTY_EXIT, (event) => {
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
let fsRefreshDirty = false; // coalesces events that arrive during in-flight work
let watchedProjectPath = null; // canonical path returned by watch_directory
// Paths accumulated across a debounce burst (and across in-flight work via
// the dirty-flag re-run). Drained per refresh iteration so the editor-tab
// scan only re-reads files whose paths actually changed.
let pendingChangedPaths = new Set();
listen(FS_CHANGED, (event) => {
  const project = getActiveProject();
  // Watcher is only started for the active project root, so a mismatch means
  // this event is from a stale watcher the frontend no longer cares about.
  // Compare against the canonical path watch_directory returned — symlinked
  // project roots make event.payload.path differ from project.path.
  if (!project || !watchedProjectPath || event.payload.path !== watchedProjectPath) return;
  // Accumulate first so events arriving while a refresh is in flight still
  // contribute their paths to the next iteration's tab scan.
  if (Array.isArray(event.payload.changed_paths)) {
    for (const p of event.payload.changed_paths) pendingChangedPaths.add(p);
  }
  if (fsRefreshScheduled) {
    // An event arrived while we're mid-work. Don't drop it — set a dirty
    // flag and the IIFE will re-run the loop once the current pass ends.
    fsRefreshDirty = true;
    return;
  }
  fsRefreshScheduled = true;
  // The Rust debouncer already coalesces bursts at 300 ms; the old
  // frontend 500 ms setTimeout stacked on top of that produced ~800 ms of
  // post-change lag for no benefit. Run immediately (inside an async IIFE
  // so await on the tab-reload loop still works).
  (async () => {
    try {
      do {
        fsRefreshDirty = false;
        // Drain the pending set into a per-iteration view, then clear so
        // the next iteration only sees newly-arrived paths.
        const changedSet = pendingChangedPaths;
        pendingChangedPaths = new Set();
        refreshFileBrowser();
        fetchGitStatus(project.path);
        refreshPanel(null, true);

        // Check open editor tabs for external file changes. Filter to tabs
        // whose filePath actually appears in this batch's changedSet — for
        // a 30-file refactor with 20 open tabs, this drops 20 sequential
        // read_file_preview calls down to ~the count of changed editor
        // files. Stale tabs are always rechecked so an external rename can
        // still be recovered via the inode-relocation fallback.
        for (const [uiId, tab] of tabs) {
        if (tab.type !== "editor") continue;
        if (!tab.stale && !changedSet.has(tab.filePath)) continue;
        try {
          // 1 MB cap on change-detection reads. Files larger than 1 MB
          // simply won't auto-refresh on external change — multi-MB files
          // aren't first-class editor citizens, and the explicit-open path
          // (createEditorTab) keeps its higher 10 MB ceiling.
          const diskContent = await invoke("read_file_preview", { path: tab.filePath, maxBytes: 1048576 });
          if (tab.stale) {
            tab.stale = false;
            renderTabBar();
          }
          const editorContent = tab.editorView.state.doc.toString();
          if (diskContent === editorContent) continue;
          if (!tab.modified) {
            // No local edits — silently reload
            tab.editorView.dispatch({
              changes: { from: 0, to: editorContent.length, insert: diskContent },
            });
            tab.originalContent = diskContent;
            refreshEditorGutter(tab);
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
                refreshEditorGutter(tab);
              }
            );
          }
        } catch (err) {
          // Distinguish "file no longer exists" from transient read
          // errors. On missing, first try to follow an external rename
          // via inode match — Finder / mv / git rename preserves the
          // inode, so if the same inode turns up elsewhere in the
          // project tree we can relocate the tab transparently. Only
          // falls back to the stale flag when no match is found.
          const errStr = String(err);
          const missing = errStr.includes("No such file") || errStr.includes("not found");
          if (missing) {
            let relocated = false;
            let relocatedContent = null;
            if (tab.inode != null) {
              try {
                const found = await invoke("find_path_by_inode", {
                  root: project.path,
                  inode: tab.inode,
                });
                if (found && found !== tab.filePath) {
                  // Read the new-path content BEFORE updating the tab so
                  // git rename-and-edit operations (rename + content
                  // change in one commit) don't leave the editor on
                  // stale text. A read failure here degrades to the
                  // stale-flag path below.
                  relocatedContent = await invoke("read_file_preview", { path: found, maxBytes: 10485760 });
                  const oldName = tab.fileName;
                  tab.filePath = found;
                  tab.fileName = found.split("/").pop();
                  const bc = tab.containerEl.querySelector(".editor-breadcrumb");
                  if (bc) renderBreadcrumb(bc, found);
                  if (tab.stale) tab.stale = false;
                  showToast(`${oldName} was renamed → ${tab.fileName}`, "info");
                  renderTabBar();
                  relocated = true;
                }
              } catch (_) {}
            }
            if (relocated && relocatedContent != null) {
              // Apply the new content the same way the main reload path
              // does: silent reload when the tab isn't dirty, confirm
              // dialog when the user has unsaved edits.
              const editorContent = tab.editorView.state.doc.toString();
              if (relocatedContent !== editorContent) {
                if (!tab.modified) {
                  tab.editorView.dispatch({
                    changes: { from: 0, to: editorContent.length, insert: relocatedContent },
                  });
                  tab.originalContent = relocatedContent;
                } else {
                  showConfirmDialog(
                    `"${tab.fileName}" changed on disk. Reload and lose your changes?`,
                    () => {
                      tab.editorView.dispatch({
                        changes: { from: 0, to: tab.editorView.state.doc.length, insert: relocatedContent },
                      });
                      tab.originalContent = relocatedContent;
                      tab.modified = false;
                      renderTabBar();
                    }
                  );
                }
              }
            }
            if (!relocated && !tab.stale) {
              tab.stale = true;
              showToast(`${tab.fileName} was deleted on disk`, "error");
              renderTabBar();
            }
          } else {
            console.error("Reload failed for", tab.filePath, err);
          }
        }
        }
      } while (fsRefreshDirty);
    } finally {
      fsRefreshScheduled = false;
    }
  })();
});

// When a file or directory is renamed through the file-browser context
// menu, update any open editor tab pointing at the old path (or a child
// of it, if a directory was renamed) so Cmd+S doesn't silently create a
// ghost file at the stale location.
window.addEventListener(PATH_RENAMED, (e) => {
  const { oldPath, newPath, isDir } = e.detail || {};
  if (!oldPath || !newPath) return;
  let anyUpdated = false;
  for (const [, tab] of tabs) {
    if (tab.type !== "editor") continue;
    let updatedPath = null;
    if (tab.filePath === oldPath) {
      updatedPath = newPath;
    } else if (isDir && tab.filePath.startsWith(oldPath + "/")) {
      updatedPath = newPath + tab.filePath.slice(oldPath.length);
    }
    if (updatedPath) {
      tab.filePath = updatedPath;
      tab.fileName = updatedPath.split("/").pop();
      const bc = tab.containerEl.querySelector(".editor-breadcrumb");
      if (bc) renderBreadcrumb(bc, updatedPath);
      anyUpdated = true;
    }
  }
  if (anyUpdated) renderTabBar();
});

// A commit / amend moves HEAD without touching the working file, so fs-changed
// won't fire — repaint every open editor's change gutter (disk-vs-HEAD) so the
// now-committed lines stop showing as changes.
window.addEventListener(HEAD_MOVED, () => {
  for (const [, tab] of tabs) {
    if (tab.type === "editor") refreshEditorGutter(tab);
  }
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
  // Fit both groups' active terminals — the ResizeObserver only watches the
  // left container, so without the right-group refit its xterm grid stays
  // CSS-stretched after window resizes.
  refitGroupTerminals();
});
resizeObserver.observe(terminalContainer);

// Refit terminal after git panel close transition (ResizeObserver is blocked during transition)
window.addEventListener(PANEL_TRANSITION_DONE, () => {
  refitGroupTerminals();
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

function setSidebarCollapsed(collapsed, { persist = true } = {}) {
  sidebar.classList.toggle("collapsed", collapsed);
  resizeHandle.classList.toggle("collapsed", collapsed);
  const btn = document.getElementById("toggle-sidebar");
  if (btn) btn.classList.toggle("active", collapsed);
  if (persist) saveSetting("sidebarCollapsed", collapsed);
  // Reflow terminals after width change settles
  requestAnimationFrame(() => {
    refitGroupTerminals();
  });
}

function toggleSidebar() {
  setSidebarCollapsed(!sidebar.classList.contains("collapsed"));
}

document.getElementById("toggle-sidebar").addEventListener("click", toggleSidebar);

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
  const isMergeTab = activeTab?.type === "merge";

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
    // Save the tab in the FOCUSED group, not always the left group. When the
    // workspace is split and the user is editing a file in the right group,
    // Cmd+S must save that tab — routing to activeTabUiId (always left) would
    // silently save the wrong tab or no-op, and the user thinks they saved.
    const saveId = isSplit && focusedGroup === "right" ? rightActiveTabUiId : activeTabUiId;
    const saveTab = tabs.get(saveId);
    if (saveTab?.type === "editor") {
      saveEditorTab(saveId);
    } else if (saveTab?.type === "merge") {
      saveMergeTab(saveId);
    }
    return;
  }
  if (keyMatches(e, "gotoSymbol")) {
    e.preventDefault();
    const tab = tabs.get(isSplit && focusedGroup === "right" ? rightActiveTabUiId : activeTabUiId);
    if (tab && tab.type === "editor") showSymbolOutline(tab);
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
  if (keyMatches(e, "toggleSidebar")) {
    e.preventDefault();
    toggleSidebar();
    return;
  }
  if (e.key === "Escape") {
    if (runTopEscape()) {
      e.preventDefault();
      return;
    }
    closeFilePreview();
  }
  if (e.metaKey && e.key >= "1" && e.key <= "9") {
    e.preventDefault();
    const idx = parseInt(e.key) - 1;
    // Index into the FOCUSED group's visible tabs, using the same sources the
    // two tab bars render from (left skips _rightGroup tabs; right is
    // rightGroupTabIds). The old code indexed the unified map, so Cmd+N could
    // land on a right-group tab and drive it through switchTab — desyncing the
    // left/right active tracking.
    if (isSplit && focusedGroup === "right") {
      if (idx < rightGroupTabIds.length) switchRightTab(rightGroupTabIds[idx]);
    } else {
      const leftIds = [...tabs.entries()].filter(([, t]) => !t._rightGroup).map(([id]) => id);
      if (idx < leftIds.length) switchTab(leftIds[idx]);
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
    // Merge tabs track `modified` and are saveable too — a reload/quit must
    // warn before silently dropping unsaved conflict resolutions.
    if ((tab.type === "editor" || tab.type === "merge") && tab.modified) {
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

// Reorder a left-group tab. `toIndex` is an index into the LEFT bar's visible
// tabs (which excludes right-group tabs). The old code reordered the full
// `tabs` map using a fromIdx computed against ALL entries but a toIndex in the
// left-only space — the two index spaces diverge whenever a right-group tab
// sits earlier in the map, dropping the tab at the wrong position. This
// permutes only the left tabs among their own map slots and leaves right-group
// entries exactly where they are.
function reorderLeftTab(fromKey, toIndex) {
  const entries = [...tabs.entries()];
  const leftPositions = [];
  entries.forEach(([, t], i) => { if (!t._rightGroup) leftPositions.push(i); });
  const leftOrder = leftPositions.map((i) => entries[i]);
  const fromIdx = leftOrder.findIndex(([k]) => k === fromKey);
  if (fromIdx === -1 || fromIdx === toIndex) return;
  const [moved] = leftOrder.splice(fromIdx, 1);
  if (toIndex > fromIdx) toIndex--;
  leftOrder.splice(toIndex, 0, moved);
  // Write the reordered left tabs back into the same slots they occupied.
  leftPositions.forEach((pos, j) => { entries[pos] = leftOrder[j]; });
  tabs.clear();
  entries.forEach(([k, v]) => tabs.set(k, v));
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
            reorderLeftTab(uiId, insertIdx);
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
    // Use the canonical form returned by watch_directory so the HashMap
    // entry actually gets removed — the raw project.path can differ
    // (symlink-resolved) and miss the key. Falls back to project.path
    // when watch_directory failed at init.
    invoke("unwatch_directory", { path: watchedProjectPath || project.path }).catch(() => {});
  }

  // Stop language servers before the reload drops the webview — the Rust-side
  // registry outlives it, so without this a project switch orphans them.
  await shutdownAllLsp();

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
  // The .group-focused class can change a group's box sizing (focus border),
  // which resizes BOTH groups' panes. Nothing else re-fits on a focus switch,
  // so a group could be left with a stale xterm grid (contributing cause of
  // issue #3). Re-fit each group's active terminal after the class flip.
  refitGroupTerminals({ immediate: true });
}

// Re-fit the active terminal tab in each group. Cheap no-op for non-terminal
// active tabs. Used after focus switches and other layout-affecting changes
// (window/sidebar resize, panel transition) that the ResizeObserver on the
// shared outer container doesn't catch for the right group. `immediate`
// matches the caller's needs: focus switches fit synchronously; rapid resize
// events use the debounced default to avoid thrashing.
function refitGroupTerminals({ immediate = false } = {}) {
  for (const id of [activeTabUiId, isSplit ? rightActiveTabUiId : -1]) {
    if (id === -1) continue;
    const tab = tabs.get(id);
    if (tab && tab.type === "terminal") fitAllPanes(tab, { immediate });
  }
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
    const staleClass = tab.type === "editor" && tab.stale ? " tab-stale" : "";
    tabEl.className = `tab ${isActive ? "tab-active" : ""}${staleClass}`;
    if (staleClass) tabEl.title = "File no longer exists on disk";

    const icon = document.createElement("span");
    icon.className = "tab-icon";
    icon.textContent = tab.type === "terminal" ? ">_" : tab.type === "settings" ? "⚙" : tab.type === "diff" ? "↔" : tab.type === "rebase" ? "⤴" : tab.type === "merge" ? "⫝" : "◆";
    tabEl.appendChild(icon);

    const label = document.createElement("span");
    label.className = "tab-label";
    if (tab.type === "terminal") {
      label.textContent = tab.name || `Terminal ${termIndex}`;
      termIndex++;
    } else if (tab.type === "settings") {
      label.textContent = "Settings";
    } else if (tab.type === "diff") {
      label.textContent = tab.fileName;
      label.title = `${tab.fromRef} → ${tab.toRef}`;
    } else if (tab.type === "rebase") {
      label.textContent = tab.fileName;
      label.title = `Interactive rebase onto ${tab.baseOid.slice(0, 7)}`;
    } else if (tab.type === "merge") {
      label.textContent = tab.fileName;
      label.title = `3-way merge: ${tab.filePath}`;
    } else {
      label.textContent = tab.fileName;
    }
    tabEl.appendChild(label);

    if ((tab.type === "editor" || tab.type === "merge") && tab.modified) {
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
    // Fit immediately (parity with switchTab) so the grid matches the
    // container before paint; focus on the next frame.
    fitAllPanes(tab, { immediate: true });
    requestAnimationFrame(() => {
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
    const uiId = nextTabId();
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
      setActiveTabId(-1);
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
  setActiveTabId(uiId);

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
  // Resolve home dir once for breadcrumb path-shortening. Errors fall
  // through to empty string, which makes renderBreadcrumb a no-op.
  try {
    setHomeDir(await invoke("get_home_dir"));
  } catch (_) {
    setHomeDir("");
  }

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
  setSidebarCollapsed(!!settings.sidebarCollapsed, { persist: false });

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
  // watch_directory canonicalizes (resolves symlinks, normalizes trailing
  // slashes) and returns the canonical form. Store that for fs-changed
  // comparison so events emitted against the canonical path match even
  // when project.path was opened via a symlink alias.
  try {
    watchedProjectPath = await invoke("watch_directory", { path: project.path });
  } catch (err) {
    console.error("watch_directory failed:", err);
    watchedProjectPath = project.path;
  }
  initQuickOpen(projectPathFn, (fullPath) => createEditorTab(fullPath));
  initProjectSearch(projectPathFn, (fullPath, opts) => createEditorTab(fullPath, opts));
  initGitPanel(
    projectPathFn,
    (filePath) => createEditorTab(filePath),
    ({ fromRef, toRef }) => createDiffTab({ fromRef, toRef }),
    ({ baseOid }) => createRebaseTab({ baseOid }),
  );
  fetchGitStatus(project.path);
  startGitPolling(projectPathFn, settings.gitPollInterval);
}

export function setPanelTransitioning(value) {
  panelTransitioning = value;
}

// Inject the tab-type-specific behavior into the registry/spine in tabs.js.
// Keeps tabs.js free of feature imports (no cycle); these closures own the
// terminal/editor/merge specifics that switchTab and doCloseTab delegate to.
setTabHooks({
  fitTerminal(tab) {
    fitAllPanes(tab, { immediate: true });
    requestAnimationFrame(() => {
      const activePane = tab.panes[tab.activePane] || tab.panes[0];
      if (activePane) activePane.term.focus();
    });
  },
  activateEditor(tab) {
    setTimeout(() => tab.editorView.focus(), 10);
    // HEAD may have moved while away (staged/committed in the git panel) —
    // recompute the change gutter on focus.
    refreshEditorGutter(tab);
    // Optionally sync the file tree to the now-active file.
    if (getSettings().editorFollowActiveFile && tab.filePath) {
      revealPath(tab.filePath);
    }
  },
  activateMerge(tab) {
    setTimeout(() => tab.mergedView.focus(), 10);
  },
  destroyPane,
  createTab,
  startTabRename,
  startTabDrag,
  splitWorkspace,
  getIsSplit: () => isSplit,
});

boot();
