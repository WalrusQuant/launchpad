import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { getResolvedTheme } from "./theme.js";
import { getSettings } from "./settings.js";
import { getActiveProject } from "./projects.js";
import { dbg, hexOf } from "./debugcapture.js";
import { PTY_OUTPUT, PTY_EXIT } from "./events.js";
import { tabs, activeTabUiId, nextTabId, switchTab, renderTabBar, closeTab } from "./tabs.js";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// Right-group close hook. The PTY_EXIT listener must close a right-group tab via
// workspace.js's closeRightTab, but terminal.js is a leaf (workspace → terminal,
// never the reverse). workspace.js registers its closeRightTab here at import so
// the listener can route right-group exits without importing workspace.js.
let closeRightTabHook = null;
export function setCloseRightTabHook(fn) { closeRightTabHook = fn; }

export const paneMap = new Map(); // ptyId → pane object (for routing PTY output)
export let isCreatingTab = false;
export function setCreatingTab(v) { isCreatingTab = v; }

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

export async function createPane(parentEl, cwd) {
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

export function destroyPane(pane) {
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

export async function createTab() {
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

export async function splitPane() {
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
export function fitAllPanes(tab, { immediate = false } = {}) {
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

export function refreshTerminalsForTheme() {
  const nextTerm = getTerminalTheme();
  for (const tab of tabs.values()) {
    if (tab.type !== "terminal") continue;
    for (const pane of tab.panes) {
      pane.term.options.theme = nextTerm;
    }
  }
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
    if (tab._rightGroup) closeRightTabHook(uiId);
    else closeTab(uiId);
    break;
  }
});
