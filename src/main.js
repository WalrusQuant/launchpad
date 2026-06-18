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
  debugCaptureActive, dbg, startDebugCapture, stopDebugCapture, markDebug,
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
import {
  createTab, splitPane, destroyPane, fitAllPanes, refreshTerminalsForTheme,
} from "./terminal.js";
import {
  isSplit, focusedGroup, rightActiveTabUiId, rightGroupTabIds,
  getIsSplit, splitWorkspace, unsplitWorkspace, setFocusedGroup, refitGroupTerminals,
  switchRightTab, closeRightTab, createTabInRight, moveTabToRight, moveTabToLeft,
  startTabDrag,
} from "./workspace.js";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// Tab management — the registry + spine (tabs Map, activeTabUiId, switch/close,
// escape stack, confirm dialog) live in tabs.js; main.js wires the
// type-specific behavior via setTabHooks (see boot).
// Terminal tab: { type: "terminal", panes: [pane, pane?], containerEl, name, activePane: 0|1 }
// Editor tab:   { type: "editor", containerEl, filePath, fileName, inode, editorView, originalContent, modified, stale }
// Each pane: { ptyId, term, fitAddon, el }
let panelTransitioning = false;

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
