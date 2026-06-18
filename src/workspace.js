import {
  createTab, createPane, fitAllPanes, destroyPane,
  isCreatingTab, setCreatingTab, setCloseRightTabHook,
} from "./terminal.js";
import {
  tabs, activeTabUiId, setActiveTabId, nextTabId,
  switchTab, renderTabBar, showConfirmDialog,
} from "./tabs.js";
import { getActiveProject } from "./projects.js";

export function getIsSplit() { return isSplit; }

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

export function startTabDrag(tabEl, uiId, sourceGroup, e) {
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

// ===== Split Workspace =====
// Parallel tab system for the right group. Left group uses existing code untouched.
export let isSplit = false;
export let rightGroupTabIds = [];
export let rightActiveTabUiId = -1;
export let focusedGroup = "left";
let rightTabBarEl = null;
let rightInstancesEl = null;
let splitHandleEl = null;

export function setFocusedGroup(group) {
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
export function refitGroupTerminals({ immediate = false } = {}) {
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

export function switchRightTab(uiId) {
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

export async function closeRightTab(uiId) {
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

export async function createTabInRight() {
  if (!isSplit || !rightInstancesEl || isCreatingTab) return;
  setCreatingTab(true);
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
    setCreatingTab(false);
  }
}

export async function moveTabToRight(uiId) {
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

export function moveTabToLeft(uiId) {
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
export async function splitWorkspace() {
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

export function unsplitWorkspace() {
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

// Wire terminal.js's PTY_EXIT listener back to right-group close logic without
// terminal.js importing workspace.js (preserves the one-way dependency).
setCloseRightTabHook(closeRightTab);
