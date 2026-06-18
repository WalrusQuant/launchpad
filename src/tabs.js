// Tab registry + spine — the workspace's thin core. Owns the shared tab state
// (the `tabs` Map, the active-tab id, the id allocator) and the type-agnostic
// tab lifecycle (switch / close). Deliberately depends on NOTHING
// feature-specific: terminal/editor/merge behavior is injected by main.js via
// `setTabHooks` at boot, so feature modules (difftab, rebasetab, …) can import
// the registry from here without an import cycle back through main.js.
//
// ESM live bindings make this cheap for callers: `tabs` (a stable Map binding)
// and `activeTabUiId` (a read-only live `let`) are imported and read at their
// existing call sites unchanged — only writes go through `setActiveTabId`.

// uiTabId → tab object. Shared registry; both groups (left + right) live here.
export const tabs = new Map();

// The active tab id for the LEFT group. Exported as a live binding: importers
// read the current value directly; only this module (and `setActiveTabId`)
// may reassign it.
export let activeTabUiId = -1;
export function setActiveTabId(id) { activeTabUiId = id; }

// Monotonic ui-tab id allocator (was `nextUiTabId++` inline).
let nextUiTabId = 0;
export function nextTabId() { return nextUiTabId++; }

// Escape handler stack — topmost handler fires first, prevents conflicts
// between Quick Open, file search, diff preview, and dialogs.
const escapeStack = [];
export function pushEscape(handler) { escapeStack.push(handler); }
export function popEscape(handler) {
  const i = escapeStack.lastIndexOf(handler);
  if (i !== -1) escapeStack.splice(i, 1);
}
// Run the topmost escape handler if any; returns true when one fired.
export function runTopEscape() {
  if (escapeStack.length > 0) {
    escapeStack[escapeStack.length - 1]();
    return true;
  }
  return false;
}

// Type-specific behavior injected once from main.js. Keys:
//   fitTerminal(tab), activateEditor(tab), activateMerge(tab),
//   destroyPane(pane), createTab(),
//   startTabRename(uiId, labelEl, index), startTabDrag(tabEl, uiId, group, e),
//   splitWorkspace(), getIsSplit()
let hooks = {};
export function setTabHooks(h) { hooks = h; }

function getActivePtyId() {
  const tab = tabs.get(activeTabUiId);
  if (!tab || tab.type !== "terminal") return null;
  return tab.panes[tab.activePane]?.ptyId ?? tab.panes[0]?.ptyId ?? null;
}
export { getActivePtyId };

export function uiIdForTab(tab) {
  for (const [uiId, t] of tabs) if (t === tab) return uiId;
  return null;
}

export function switchTab(uiId) {
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
    hooks.fitTerminal(tab);
  } else if (tab.type === "editor") {
    hooks.activateEditor(tab);
  } else if (tab.type === "merge") {
    hooks.activateMerge(tab);
  }

  renderTabBar();
}

// Themed in-app modal confirm. Defaults to a "Close" affirmative button to
// preserve historical callers (close-with-unsaved-changes). Pass `opts` to
// customize the affirmative label and tone:
//   { confirmLabel: "Delete", tone: "danger" }
// `tone: "danger"` colors the affirmative button red so a destructive
// action visibly differs from a benign one.
export function showConfirmDialog(message, onConfirm, opts = {}) {
  const confirmLabel = opts.confirmLabel || "Close";
  const cancelLabel = opts.cancelLabel || "Cancel";
  const toneClass = opts.tone === "danger" ? " confirm-danger" : "";

  document.querySelector(".confirm-overlay")?.remove();
  const overlay = document.createElement("div");
  overlay.className = "confirm-overlay";
  overlay.innerHTML = `
    <div class="confirm-dialog">
      <div class="confirm-message"></div>
      <div class="confirm-actions">
        <button class="confirm-btn confirm-cancel"></button>
        <button class="confirm-btn confirm-ok${toneClass}"></button>
      </div>
    </div>`;
  // textContent assignments avoid HTML injection from caller-supplied
  // strings (filenames containing "<" etc.).
  overlay.querySelector(".confirm-message").textContent = message;
  overlay.querySelector(".confirm-cancel").textContent = cancelLabel;
  overlay.querySelector(".confirm-ok").textContent = confirmLabel;
  document.body.appendChild(overlay);

  const dismiss = () => { overlay.remove(); popEscape(dismiss); };
  overlay.querySelector(".confirm-cancel").addEventListener("click", dismiss);
  overlay.querySelector(".confirm-ok").addEventListener("click", () => { dismiss(); onConfirm(); });
  overlay.addEventListener("click", (e) => { if (e.target === overlay) dismiss(); });
  pushEscape(dismiss);
}

export async function closeTab(uiId) {
  const tab = tabs.get(uiId);
  if (!tab) return;

  if ((tab.type === "editor" || tab.type === "merge") && tab.modified) {
    showConfirmDialog(
      `“${tab.fileName}” has unsaved changes. Close anyway?`,
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

export async function doCloseTab(uiId) {
  const tab = tabs.get(uiId);
  if (!tab) return;

  // Remove tab from state and UI FIRST — before any cleanup that could fail
  tab.containerEl.style.display = "none";
  tabs.delete(uiId);
  renderTabBar();

  // Now clean up resources
  try {
    if (tab.type === "editor") tab.editorView.destroy();
    else if (tab.type === "terminal") tab.panes.forEach(p => hooks.destroyPane(p));
    else if (tab.type === "merge") {
      // Detach the scroll listener BEFORE view.destroy() — CodeMirror
      // doesn't track external listeners, and the closure captures the
      // tab + all three views, so leaving it attached pins the views
      // alive past the tab's lifetime.
      if (tab.onMergedScroll && tab.mergedView) {
        try { tab.mergedView.scrollDOM.removeEventListener("scroll", tab.onMergedScroll); } catch (_) {}
      }
      tab.oursView?.destroy();
      tab.theirsView?.destroy();
      tab.mergedView?.destroy();
    }
  } catch (_) {}
  try { tab.containerEl.remove(); } catch (_) {}

  if (tabs.size === 0) {
    await hooks.createTab();
  } else if (activeTabUiId === uiId) {
    const remaining = [...tabs.keys()];
    switchTab(remaining[remaining.length - 1]);
  }
}

// Render the LEFT-group tab bar. Type-specific interactions (rename, drag,
// new-tab, split toggle) are delegated to injected hooks so this stays in the
// feature-free spine; the right-group bar is rendered separately in main.js.
export function renderTabBar() {
  const tabBar = document.getElementById("tab-bar");
  tabBar.innerHTML = "";

  let termIndex = 1;
  for (const [uiId, tab] of tabs) {
    if (tab._rightGroup) continue; // skip tabs in the right split group
    const isActive = uiId === activeTabUiId;
    const tabEl = document.createElement("div");
    const staleClass = tab.type === "editor" && tab.stale ? " tab-stale" : "";
    tabEl.className = `tab ${isActive ? "tab-active" : ""}${staleClass}`;
    if (staleClass) tabEl.title = "File no longer exists on disk";
    tabEl.dataset.tabId = uiId;
    tabEl.setAttribute("role", "tab");
    tabEl.setAttribute("aria-selected", isActive ? "true" : "false");

    // Icon
    const icon = document.createElement("span");
    icon.className = "tab-icon";
    icon.textContent = tab.type === "terminal" ? ">_" : tab.type === "settings" ? "⚙" : tab.type === "diff" ? "↔" : tab.type === "rebase" ? "⤴" : tab.type === "merge" ? "⫝" : "◆";
    tabEl.appendChild(icon);

    const label = document.createElement("span");
    label.className = "tab-label";

    if (tab.type === "terminal") {
      label.textContent = tab.name || `Terminal ${termIndex}`;
      label.addEventListener("dblclick", (e) => {
        e.stopPropagation();
        hooks.startTabRename(uiId, label, termIndex);
      });
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

    // Modified dot or close button
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
      closeTab(uiId);
    });
    tabEl.appendChild(closeBtn);

    tabEl.addEventListener("click", () => switchTab(uiId));
    tabEl.addEventListener("mousedown", (e) => {
      if (e.target.closest(".tab-close")) return; // don't start drag from close button
      hooks.startTabDrag(tabEl, uiId, "left", e);
    });
    tabBar.appendChild(tabEl);
  }

  const newTabBtn = document.createElement("div");
  newTabBtn.className = "tab tab-new";
  newTabBtn.textContent = "+";
  newTabBtn.title = "New terminal (⌘T)";
  newTabBtn.addEventListener("click", () => hooks.createTab());
  tabBar.appendChild(newTabBtn);

  const splitBtn = document.createElement("div");
  splitBtn.className = "tab tab-new";
  splitBtn.textContent = hooks.getIsSplit() ? "⊞" : "⊟";
  splitBtn.title = hooks.getIsSplit() ? "Unsplit (⌘\\)" : "Split view (⌘\\)";
  splitBtn.addEventListener("click", () => hooks.splitWorkspace());
  tabBar.appendChild(splitBtn);
}
