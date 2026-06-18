// ─── Interactive rebase tab (PR5) ──────────────────────────────────────────
// Drag-to-reorder a list of commits, pick an action per row (pick/reword/
// squash/fixup/drop/edit), edit messages inline for reword/squash, then
// Apply. The Apply call routes through git_rebase_interactive_apply which
// handles the cancellable spawn + state-dir lifecycle (PR4 backend).
const { invoke } = window.__TAURI__.core;
import { tabs, nextTabId, switchTab, renderTabBar, closeTab } from "./tabs.js";
import { getActiveProject } from "./projects.js";
import { showToast } from "./toast.js";
import { escapeText } from "./domutil.js";
import { setInFlightOp } from "./git.js";
import { refreshPanel } from "./gitpanel.js";
import { refreshFileBrowser } from "./filebrowser.js";
import { createEditorTab } from "./editortab.js";

const REBASE_ACTIONS = ["pick", "reword", "squash", "fixup", "drop", "edit"];

export async function createRebaseTab({ baseOid, count = 30 }) {
  const project = getActiveProject();
  if (!project) {
    showToast("Open a project first", "error");
    return null;
  }

  // Dedupe by baseOid — same rebase context → switch to existing tab.
  for (const [uiId, t] of tabs) {
    if (t.type === "rebase" && t.baseOid === baseOid) {
      switchTab(uiId);
      return uiId;
    }
  }

  let candidates;
  try {
    candidates = await invoke("get_rebase_candidate_commits", { path: project.path, count });
  } catch (err) {
    if (err === "detached_head") {
      showToast("Cannot rebase: HEAD is detached", "error");
    } else {
      showToast(`Rebase setup failed: ${err}`, "error");
    }
    return null;
  }

  if (!candidates.commits || candidates.commits.length === 0) {
    showToast("No commits available to rebase", "info");
    return null;
  }

  // Build the todo from the candidate commits, in display order (newest
  // first as returned by the backend). originalTodo is a deep copy used
  // by the Reset button to revert mid-edit changes.
  const todo = candidates.commits.map((c) => ({
    action: "pick",
    oid: c.oid,
    short_oid: c.short_oid,
    message: c.message,
    author: c.author,
    on_remote: c.on_remote,
    new_message: null,
  }));
  const originalTodo = todo.map((e) => ({ ...e }));

  const uiId = nextTabId();

  const containerEl = document.createElement("div");
  containerEl.className = "rebase-instance rebase-tab";
  document.getElementById("terminal-instances").appendChild(containerEl);

  const tab = {
    type: "rebase",
    containerEl,
    baseOid,
    todo,
    originalTodo,
    upstreamKnown: candidates.upstream_known,
    fileName: `Rebase ${baseOid.slice(0, 7)}`,
    applying: false,
  };
  tabs.set(uiId, tab);

  renderRebaseTab(tab, uiId);
  renderTabBar();
  switchTab(uiId);
  return uiId;
}

function renderRebaseTab(tab, uiId) {
  const { containerEl, todo, upstreamKnown } = tab;
  containerEl.replaceChildren();

  const header = document.createElement("div");
  header.className = "rebase-tab-header";
  const title = document.createElement("div");
  title.className = "rebase-tab-title";
  title.textContent = `Interactive rebase onto ${tab.baseOid.slice(0, 7)}`;
  header.appendChild(title);

  if (!upstreamKnown) {
    const note = document.createElement("div");
    note.className = "rebase-tab-note";
    note.textContent = `No upstream — showing the last ${todo.length} commits. All are safe to rewrite.`;
    header.appendChild(note);
  }
  containerEl.appendChild(header);

  // Column header — gives users a visual anchor for what each control is.
  const colHead = document.createElement("div");
  colHead.className = "rebase-row rebase-col-header";
  colHead.innerHTML = `
    <span class="rebase-handle-col"></span>
    <span class="rebase-action-col">Action</span>
    <span class="rebase-oid-col">OID</span>
    <span class="rebase-msg-col">Subject</span>
    <span class="rebase-flag-col"></span>`;
  containerEl.appendChild(colHead);

  const list = document.createElement("div");
  list.className = "rebase-list";
  containerEl.appendChild(list);

  todo.forEach((entry, idx) => {
    const row = buildRebaseRow(tab, uiId, entry, idx);
    list.appendChild(row);
  });

  const footer = document.createElement("div");
  footer.className = "rebase-tab-footer";
  const onRemoteCount = todo.filter((e) => e.on_remote && e.action !== "drop").length;
  if (upstreamKnown && onRemoteCount > 0) {
    const warn = document.createElement("span");
    warn.className = "rebase-tab-warn";
    warn.textContent = `⚠ ${onRemoteCount} of these commits ${onRemoteCount === 1 ? "is" : "are"} already on the remote — Apply will require a force-push.`;
    footer.appendChild(warn);
  }
  const applyBtn = document.createElement("button");
  applyBtn.className = "rebase-apply-btn";
  applyBtn.textContent = "Apply";
  applyBtn.addEventListener("click", () => applyRebase(tab, uiId));
  const resetBtn = document.createElement("button");
  resetBtn.className = "rebase-reset-btn";
  resetBtn.textContent = "Reset";
  resetBtn.addEventListener("click", () => {
    tab.todo = tab.originalTodo.map((e) => ({ ...e }));
    renderRebaseTab(tab, uiId);
  });
  const cancelBtn = document.createElement("button");
  cancelBtn.className = "rebase-cancel-btn";
  cancelBtn.textContent = "Cancel";
  cancelBtn.addEventListener("click", () => closeTab(uiId));
  footer.appendChild(applyBtn);
  footer.appendChild(resetBtn);
  footer.appendChild(cancelBtn);
  containerEl.appendChild(footer);
}

function buildRebaseRow(tab, uiId, entry, idx) {
  const row = document.createElement("div");
  row.className = `rebase-row rebase-action-${entry.action}`;
  row.dataset.rowIndex = String(idx);

  // Drag handle
  const handle = document.createElement("span");
  handle.className = "rebase-handle";
  handle.title = "Drag to reorder";
  handle.textContent = "⋮⋮";
  handle.addEventListener("mousedown", (e) => startRebaseRowDrag(tab, uiId, idx, row, e));
  row.appendChild(handle);

  // Action select
  const select = document.createElement("select");
  select.className = "rebase-action-select";
  for (const action of REBASE_ACTIONS) {
    const opt = document.createElement("option");
    opt.value = action;
    opt.textContent = action;
    if (action === entry.action) opt.selected = true;
    select.appendChild(opt);
  }
  select.addEventListener("change", () => {
    entry.action = select.value;
    // Rerender the single row so the message editor appears/disappears
    // and the row class updates.
    const newRow = buildRebaseRow(tab, uiId, entry, idx);
    row.replaceWith(newRow);
  });
  row.appendChild(select);

  // OID
  const oidEl = document.createElement("span");
  oidEl.className = "rebase-oid";
  oidEl.textContent = entry.short_oid;
  row.appendChild(oidEl);

  // Subject (commit message summary)
  const msg = document.createElement("span");
  msg.className = "rebase-msg";
  msg.textContent = entry.message;
  msg.title = `${entry.message}\n\n${entry.author}`;
  row.appendChild(msg);

  // On-remote flag
  const flag = document.createElement("span");
  flag.className = "rebase-flag";
  if (entry.on_remote && entry.action !== "drop") {
    flag.textContent = "⚠";
    flag.title = "This commit is on the remote — rewriting requires a force-push.";
  }
  row.appendChild(flag);

  // Inline message editor for reword/squash
  if (entry.action === "reword" || entry.action === "squash") {
    const editorWrap = document.createElement("div");
    editorWrap.className = "rebase-row-message-editor";
    const ta = document.createElement("textarea");
    ta.rows = 3;
    ta.placeholder = entry.action === "squash"
      ? "Combined message (leave empty to keep originals)"
      : "New message";
    ta.value = entry.new_message ?? entry.message;
    ta.addEventListener("input", () => {
      entry.new_message = ta.value;
    });
    editorWrap.appendChild(ta);
    row.appendChild(editorWrap);
  }

  return row;
}

// Drag-to-reorder for rebase rows. Mirrors the tab-drag pattern
// (mouse-based since HTML5 DnD is unreliable in WebKit/Tauri) but
// vertical and operating on a single array, so simpler than tabs.
let rebaseDragState = null;
let rebaseDropIndicator = null;

function startRebaseRowDrag(tab, uiId, fromIdx, rowEl, e) {
  e.preventDefault();
  rebaseDragState = { tab, uiId, fromIdx, startY: e.clientY, started: false };

  const list = rowEl.parentElement;
  const onMove = (me) => {
    if (!rebaseDragState) return;
    const dy = me.clientY - rebaseDragState.startY;
    if (!rebaseDragState.started && Math.abs(dy) < 5) return; // deadzone
    rebaseDragState.started = true;
    rowEl.classList.add("rebase-row-dragging");
    const insertIdx = getRebaseRowInsertIndex(list, me.clientY, fromIdx);
    showRebaseDropIndicator(list, insertIdx);
  };

  const onUp = (ue) => {
    document.removeEventListener("mousemove", onMove);
    document.removeEventListener("mouseup", onUp);
    rowEl.classList.remove("rebase-row-dragging");
    if (rebaseDropIndicator) {
      rebaseDropIndicator.remove();
      rebaseDropIndicator = null;
    }
    if (!rebaseDragState) return;
    if (rebaseDragState.started) {
      const insertIdx = getRebaseRowInsertIndex(list, ue.clientY, fromIdx);
      let toIdx = insertIdx;
      if (toIdx > fromIdx) toIdx--; // splice/insert offset adjustment
      if (toIdx !== fromIdx) {
        const [moved] = tab.todo.splice(fromIdx, 1);
        tab.todo.splice(toIdx, 0, moved);
        renderRebaseTab(tab, uiId);
      }
    }
    rebaseDragState = null;
  };

  document.addEventListener("mousemove", onMove);
  document.addEventListener("mouseup", onUp);
}

function getRebaseRowInsertIndex(listEl, mouseY, dragFromIdx) {
  const rows = [...listEl.querySelectorAll(".rebase-row")];
  for (let i = 0; i < rows.length; i++) {
    if (i === dragFromIdx) continue;
    const r = rows[i].getBoundingClientRect();
    const mid = r.top + r.height / 2;
    if (mouseY < mid) return i;
  }
  return rows.length;
}

function showRebaseDropIndicator(listEl, insertIndex) {
  if (!rebaseDropIndicator) {
    rebaseDropIndicator = document.createElement("div");
    rebaseDropIndicator.className = "rebase-drop-indicator";
  }
  const rows = [...listEl.querySelectorAll(".rebase-row")];
  if (insertIndex < rows.length) {
    listEl.insertBefore(rebaseDropIndicator, rows[insertIndex]);
  } else {
    listEl.appendChild(rebaseDropIndicator);
  }
}

async function applyRebase(tab, uiId) {
  if (tab.applying) return;
  const project = getActiveProject();
  if (!project) return;

  // Set the guard BEFORE the confirm await — a rapid double-click would
  // otherwise pass both calls past the `if (tab.applying)` check, open two
  // confirm overlays, and the first one's overlay-element gets nuked by the
  // second's `?.remove()` in confirmAsync, leaving the first Promise hung
  // forever.
  tab.applying = true;

  try {
    const onRemoteCount = tab.todo.filter((e) => e.on_remote && e.action !== "drop").length;

    // Confirm only when we KNOW the upstream and at least one row is on
    // the remote. Skip the prompt entirely on no-upstream branches (already
    // surfaced via the info banner) or fully-local rebases.
    if (tab.upstreamKnown && onRemoteCount > 0) {
      const ok = await confirmAsync(
        `${onRemoteCount} of these commits are already on the remote.\n\nRewriting them will require a force-push to share. Continue?`
      );
      if (!ok) {
        tab.applying = false;
        return;
      }
    }

    // Build todo for the backend — only fields it cares about.
    const backendTodo = tab.todo.map((e) => ({
      action: e.action,
      oid: e.oid,
      new_message: (e.action === "reword" || e.action === "squash") ? e.new_message : null,
    }));

    setInFlightOp(true);
    const result = await invoke("git_rebase_interactive_apply", {
      path: project.path,
      baseOid: tab.baseOid,
      todo: backendTodo,
    });
    if (result.ok && result.completed) {
      showToast(`Rebase complete. Backup tag: ${result.backup_tag}`, "info");
      closeTab(uiId);
    } else {
      // Pause — close the rebase tab, open the first conflicted file in
      // the inline conflict editor (auto-staged on save via PR3). Empty
      // conflicted_files means an `edit`-action pause, not a conflict —
      // the Pending Operation banner is enough on its own there.
      const conflicts = result.conflicted_files || [];
      const n = conflicts.length;
      const msg = n > 0
        ? `Rebase paused — ${n} file${n === 1 ? "" : "s"} with conflicts. Resolve and click Continue.`
        : "Rebase paused at edit. Make changes and click Continue.";
      showToast(msg, "info");
      closeTab(uiId);
      if (n > 0) {
        await createEditorTab(`${project.path}/${conflicts[0]}`);
      }
    }
  } catch (err) {
    showToast(`Rebase failed: ${err}`, "error");
  } finally {
    tab.applying = false;
    setInFlightOp(false);
    refreshPanel(null, true);
    refreshFileBrowser();
  }
}

// Tiny confirm-dialog wrapper using the existing confirm-overlay element
// pattern. Returns a promise that resolves to true (Confirm) or false.
function confirmAsync(message) {
  return new Promise((resolve) => {
    document.querySelector(".confirm-overlay")?.remove();
    const overlay = document.createElement("div");
    overlay.className = "confirm-overlay";
    overlay.innerHTML = `
      <div class="confirm-dialog">
        <div class="confirm-message">${escapeText(message)}</div>
        <div class="confirm-actions">
          <button class="confirm-btn confirm-cancel">Cancel</button>
          <button class="confirm-btn confirm-ok">Confirm</button>
        </div>
      </div>`;
    document.body.appendChild(overlay);
    const dismiss = (val) => { overlay.remove(); resolve(val); };
    overlay.querySelector(".confirm-cancel").addEventListener("click", () => dismiss(false));
    overlay.querySelector(".confirm-ok").addEventListener("click", () => dismiss(true));
    overlay.addEventListener("click", (e) => { if (e.target === overlay) dismiss(false); });
  });
}
