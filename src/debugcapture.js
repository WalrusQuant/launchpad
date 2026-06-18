// Debug capture — toggle via toolbar button (⦿). Captures PTY output bytes,
// keystrokes, fit/resize events with timestamps, then dumps them as NDJSON to
// ~/.launchpad/debug.log for diagnosing terminal sizing / ghost-character bugs.
const { invoke } = window.__TAURI__.core;
import { tabs, activeTabUiId } from "./tabs.js";

export let debugCaptureActive = false;
let debugLog = [];
let debugStartTime = 0;
const DEBUG_MAX_ENTRIES = 500_000;

export function hexOf(s) {
  if (typeof s !== "string") s = String(s);
  const bytes = new TextEncoder().encode(s);
  let out = "";
  for (let i = 0; i < bytes.length; i++) {
    out += bytes[i].toString(16).padStart(2, "0");
  }
  return out;
}

export function dbg(event, data) {
  if (!debugCaptureActive) return;
  if (debugLog.length >= DEBUG_MAX_ENTRIES) return;
  debugLog.push({
    t: Math.round(performance.now() - debugStartTime),
    event,
    ...data,
  });
}

export function startDebugCapture() {
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

export async function stopDebugCapture() {
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

export function markDebug(label = "MARK") {
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
