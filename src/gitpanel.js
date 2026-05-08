import { showDiffPreview, refreshFileBrowser } from "./filebrowser.js";
import { setPanelTransitioning } from "./main.js";
import { matches as keyMatches } from "./keymap.js";
import { buildDiffHtml } from "./diffrender.js";
import { inFlightOp, setInFlightOp, invokeWithTimeout, getPendingOp } from "./git.js";

const { invoke } = window.__TAURI__.core;

// ─── Module-level state ────────────────────────────────────────────────────────
let panelVisible = false;
let currentPath = "";
let lastSnapshot = null;
let expandedCommitOid = null;
let openFileInEditor = null;
let openDiffInTab = null;
let openRebaseInTab = null;
// Snapshot of branches from the last refresh; used by the "Compare…" popup
// to populate its ref selectors. Updated each time refreshPanel runs.
let lastBranches = [];
let lastRemoteBranches = [];
let lastHeadBranchName = null;
// Most recent pending-op snapshot — kept locally in addition to git.js's
// copy so renderPanel can read synchronously without crossing modules.
let currentPendingOp = { kind: "none" };
// Commit OIDs are immutable, so details for a given OID never change.
// Cache them to avoid a second invoke on every forced re-render — and to
// render the expanded section fully instead of a loading flicker.
const commitDetailCache = new Map();

// ─── Status icon mapping ───────────────────────────────────────────────────────
const STATUS_ICONS = {
  index_new: "A",
  index_modified: "M",
  index_deleted: "D",
  index_renamed: "R",
  new: "U",
  modified: "M",
  deleted: "D",
  renamed: "R",
  conflict: "C",
};

// ─── Public API ────────────────────────────────────────────────────────────────
export function initGitPanel(getPath, openFileCb, openDiffCb, openRebaseCb) {
  currentPath = getPath();
  openFileInEditor = openFileCb || null;
  openDiffInTab = openDiffCb || null;
  openRebaseInTab = openRebaseCb || null;

  const toggleBtn = document.getElementById("toggle-git-panel");
  toggleBtn.addEventListener("click", () => togglePanel());

  // One delegated contextmenu handler for the whole panel. Per-row
  // attachment kept failing silently in WKWebView; delegation survives any
  // panel re-render and does not depend on listener attachment timing.
  // Routes:
  //   - target inside a commit row → showCommitContextMenu
  //   - anything else → just preventDefault so macOS Services menu never
  //     pops over our UI
  const panel = document.getElementById("git-panel");
  if (panel) {
    panel.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      const commitEl = e.target.closest && e.target.closest(".gp-commit");
      if (commitEl && commitEl.dataset.oid) {
        showCommitContextMenu(e, commitEl.dataset.oid);
      }
    });
  }

  document.addEventListener("keydown", (e) => {
    if (keyMatches(e, "gitPanel")) {
      e.preventDefault();
      togglePanel();
    }
  });
}

export function togglePanel() {
  panelVisible = !panelVisible;
  const panel = document.getElementById("git-panel");
  const btn = document.getElementById("toggle-git-panel");

  if (panelVisible) {
    panel.classList.add("visible");
    btn.classList.add("active");
    refreshPanel();
  } else {
    setPanelTransitioning(true);
    panel.addEventListener("transitionend", () => {
      setPanelTransitioning(false);
      // ResizeObserver was blocked during transition — emit event so main.js can refit
      window.dispatchEvent(new CustomEvent("panel-transition-done"));
    }, { once: true });
    panel.classList.remove("visible");
    btn.classList.remove("active");
  }
}

export async function refreshPanel(path, force = false, preloadedStatus = null, preloadedPendingOp = null) {
  // Project switch: wipe commit-detail cache and collapse any expanded
  // commit so stale entries from the previous repo don't bleed across.
  // (OID collisions between projects are vanishingly unlikely but possible
  // with shallow clones / forks; and the cache otherwise grows unbounded.)
  if (path && path !== currentPath) {
    commitDetailCache.clear();
    expandedCommitOid = null;
  }
  if (path) currentPath = path;
  if (!panelVisible) return;
  // Skip the whole refresh while a network op is running. Re-rendering the
  // toolbar mid-push destroys the button the click-handler is attached to
  // and removes the Cancel button, letting a second click launch a
  // concurrent op that steals the pid_store slot.
  if (inFlightOp && !force) return;

  const panel = document.getElementById("git-panel");
  const header = panel.querySelector(".gp-header");
  if (header) header.classList.add("gp-refreshing");

  try {
    // Reuse a status fetched by the polling side (git.js:fetchGitStatus)
    // when available, so each poll tick does one get_git_status call, not
    // two. Callers that don't have a pre-fetched status pass null and we
    // fetch on their behalf.
    const status = preloadedStatus ?? (await invoke("get_git_status", { path: currentPath }));

    if (!status.is_repo) {
      panel.innerHTML = `
        <div class="gp-empty">
          <div class="gp-empty-icon">⎇</div>
          <div class="gp-empty-text">Not a git repository</div>
        </div>`;
      lastSnapshot = null;
      return;
    }

    const [branches, remoteBranches, commits, remoteUrl, stashes, pendingOp] = await Promise.all([
      invoke("list_branches", { path: currentPath }),
      invoke("list_remote_branches", { path: currentPath }).catch(() => []),
      invoke("get_commits", { path: currentPath, count: 30 }).catch(() => []),
      invoke("get_remote_url", { path: currentPath }).catch(() => null),
      invoke("git_stash_list", { path: currentPath }).catch(() => []),
      preloadedPendingOp
        ? Promise.resolve(preloadedPendingOp)
        : invoke("get_pending_op_state", { path: currentPath }).catch(() => ({ kind: "none" })),
    ]);
    currentPendingOp = pendingOp || { kind: "none" };

    const snapshot = JSON.stringify({ status, branches, remoteBranches, commits, stashes, pendingOp });
    if (snapshot === lastSnapshot && !force) return;
    lastSnapshot = snapshot;

    // Snapshot for the Compare popup so it doesn't fire its own invokes.
    lastBranches = branches;
    lastRemoteBranches = remoteBranches;
    lastHeadBranchName = branches.find((b) => b.is_current)?.name || null;

    renderPanel(status, branches, remoteBranches, commits, remoteUrl, stashes);
  } catch (err) {
    panel.innerHTML = `<div class="gp-error">Error: ${escapeHtml(String(err))}</div>`;
  } finally {
    const h = panel.querySelector(".gp-header");
    if (h) h.classList.remove("gp-refreshing");
  }
}

// ─── Helpers ───────────────────────────────────────────────────────────────────
function escapeHtml(str) {
  return String(str)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function formatRelativeTime(unixSeconds) {
  const now = Math.floor(Date.now() / 1000);
  const diff = now - unixSeconds;

  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  if (diff < 604800) return `${Math.floor(diff / 86400)}d ago`;
  if (diff < 2592000) return `${Math.floor(diff / 604800)}w ago`;

  const date = new Date(unixSeconds * 1000);
  return date.toLocaleDateString("en-US", { month: "short", day: "numeric" });
}

function parseGitHubUrl(remoteUrl) {
  if (!remoteUrl) return null;
  // SSH: anchor on the literal "github.com" with an optional alias suffix
  // that starts with "-" (the SSH config convention: github.com-work,
  // github.com-personal, etc.). Refuses spoofed hosts like
  // not-github.com or github.com.evil by not allowing an arbitrary
  // prefix or a "." boundary into the suffix.
  const sshMatch = remoteUrl.match(/git@github\.com(?:-[\w.-]+)?[:/](.+?)(?:\.git)?$/);
  if (sshMatch) return `https://github.com/${sshMatch[1]}`;
  // HTTPS: https://github.com/user/repo.git
  const httpsMatch = remoteUrl.match(/https?:\/\/github\.com\/(.+?)(?:\.git)?$/);
  if (httpsMatch) return `https://github.com/${httpsMatch[1]}`;
  return null;
}

function commitDetailBodyHtml(detail) {
  const filesHtml = detail.files
    .map((f) => {
      const addStr = f.additions > 0 ? `<span class="gp-detail-add">+${f.additions}</span>` : "";
      const delStr = f.deletions > 0 ? `<span class="gp-detail-del">-${f.deletions}</span>` : "";
      return `<div class="gp-detail-file">
        <span class="gp-detail-path">${escapeHtml(f.path)}</span>
        <span class="gp-detail-stats">${addStr}${delStr}</span>
      </div>`;
    })
    .join("");
  return `
    <div class="gp-detail-meta">${escapeHtml(detail.author)} · ${formatRelativeTime(detail.timestamp)}</div>
    <div class="gp-detail-files">${filesHtml || "<em>No file changes</em>"}</div>`;
}

function showGitFeedback(message, type) {
  const existing = document.querySelector(".gp-feedback");
  if (existing) existing.remove();
  const el = document.createElement("div");
  el.className = `gp-feedback gp-feedback-${type}`;
  el.textContent = message;
  const header = document.querySelector(".gp-header");
  if (header) header.after(el);
  setTimeout(() => el.remove(), 3000);
}

function showConfirmPopup(anchorEl, message, onConfirm) {
  // Remove any existing popup
  document.querySelector(".gp-confirm-popup")?.remove();

  const popup = document.createElement("div");
  popup.className = "gp-confirm-popup";
  popup.innerHTML = `
    <div class="gp-confirm-msg">${escapeHtml(message)}</div>
    <div class="gp-confirm-actions">
      <button class="gp-confirm-yes">Confirm</button>
      <button class="gp-confirm-cancel">Cancel</button>
    </div>`;

  // Position near anchor
  const rect = anchorEl.getBoundingClientRect();
  popup.style.position = "fixed";
  popup.style.top = `${rect.bottom + 4}px`;
  popup.style.left = `${Math.max(4, rect.left - 80)}px`;
  document.body.appendChild(popup);

  let outsideClick = null;
  const close = () => {
    popup.remove();
    if (outsideClick) document.removeEventListener("mousedown", outsideClick);
  };

  popup.querySelector(".gp-confirm-yes").addEventListener("click", () => {
    close();
    onConfirm();
  });
  popup.querySelector(".gp-confirm-cancel").addEventListener("click", close);

  // Close on outside click
  setTimeout(() => {
    outsideClick = (e) => {
      if (!popup.contains(e.target)) close();
    };
    document.addEventListener("mousedown", outsideClick);
  }, 0);
}

// Two-select popup for picking a `from` and `to` ref. Anchored under the
// triggering button. Populated from the most recent refresh's branch list,
// so it doesn't fire its own invokes.
function showCompareRefsPopup(anchorEl) {
  document.querySelector(".gp-compare-popup")?.remove();
  if (!openDiffInTab) {
    showGitFeedback("Diff tab not wired", "error");
    return;
  }

  const refs = [];
  for (const b of lastBranches) refs.push({ name: b.name, group: "Local" });
  for (const b of lastRemoteBranches) refs.push({ name: `origin/${b.name}`, group: "Remote" });
  if (refs.length === 0) {
    showGitFeedback("No branches to compare", "error");
    return;
  }

  const head = lastHeadBranchName;
  const optionsHtml = refs
    .map((r) => `<option value="${escapeHtml(r.name)}" data-group="${r.group}">${escapeHtml(r.name)}</option>`)
    .join("");

  const popup = document.createElement("div");
  popup.className = "gp-confirm-popup gp-compare-popup";
  popup.innerHTML = `
    <div class="gp-compare-row">
      <label>From</label>
      <select class="gp-compare-from">${optionsHtml}</select>
    </div>
    <div class="gp-compare-row">
      <label>To</label>
      <select class="gp-compare-to">${optionsHtml}</select>
    </div>
    <div class="gp-confirm-actions">
      <button class="gp-confirm-yes">Compare</button>
      <button class="gp-confirm-cancel">Cancel</button>
    </div>`;

  const rect = anchorEl.getBoundingClientRect();
  document.body.appendChild(popup);
  // Clamp to viewport — the BRANCHES button sits at the right edge of the
  // git panel, so a naive `rect.left - 120` overflows the screen.
  const popupRect = popup.getBoundingClientRect();
  const popupWidth = popupRect.width || 280;
  const popupHeight = popupRect.height || 160;
  const left = Math.max(4, Math.min(rect.left - 120, window.innerWidth - popupWidth - 8));
  // Prefer below the button; flip above if not enough space.
  const top = rect.bottom + 4 + popupHeight > window.innerHeight
    ? Math.max(4, rect.top - popupHeight - 4)
    : rect.bottom + 4;
  popup.style.position = "fixed";
  popup.style.top = `${top}px`;
  popup.style.left = `${left}px`;

  const fromSel = popup.querySelector(".gp-compare-from");
  const toSel = popup.querySelector(".gp-compare-to");
  // Sensible default: compare HEAD with the first non-HEAD ref.
  if (head) {
    fromSel.value = head;
    const otherRef = refs.find((r) => r.name !== head);
    if (otherRef) toSel.value = otherRef.name;
  }

  let outsideClick = null;
  const close = () => {
    popup.remove();
    if (outsideClick) document.removeEventListener("mousedown", outsideClick);
  };

  popup.querySelector(".gp-confirm-yes").addEventListener("click", () => {
    const fromRef = fromSel.value;
    const toRef = toSel.value;
    close();
    if (fromRef === toRef) {
      showGitFeedback("Pick two different refs", "error");
      return;
    }
    openDiffInTab({ fromRef, toRef });
  });
  popup.querySelector(".gp-confirm-cancel").addEventListener("click", close);

  setTimeout(() => {
    outsideClick = (e) => {
      if (!popup.contains(e.target)) close();
    };
    document.addEventListener("mousedown", outsideClick);
  }, 0);
}

// Inline amend popup. Pre-fills with HEAD's current message; if the textarea
// is left empty the backend keeps the existing message (Option<String> = None).
// Pre-amend: backend tells us if HEAD is on a remote-tracking ref so we can
// warn the user that a force-push will be needed.
async function startAmendFlow(anchorEl, includeStaged) {
  document.querySelector(".gp-amend-popup")?.remove();

  // Probe HEAD reachability + grab the head commit's message in parallel.
  let onRemote = false;
  let lastMessage = "";
  try {
    [onRemote] = await Promise.all([
      invoke("git_head_on_remote", { path: currentPath }),
    ]);
  } catch (_) {}
  // Use the most recent commit from the panel snapshot for the prefill.
  // (lastSnapshot was JSON-stringified for cache compare, so we re-derive
  // from the latest get_commits via the message we already render.)
  try {
    const commits = await invoke("get_commits", { path: currentPath, count: 1 });
    if (commits && commits.length > 0) lastMessage = commits[0].message || "";
  } catch (_) {}

  const popup = document.createElement("div");
  popup.className = "gp-confirm-popup gp-amend-popup";
  popup.innerHTML = `
    <div class="gp-amend-title">Amend last commit${includeStaged ? " (uses staged changes)" : ""}</div>
    ${onRemote ? `<div class="gp-amend-warning">⚠ This commit is on a remote — you'll need to force-push.</div>` : ""}
    <textarea class="gp-amend-textarea" rows="4" spellcheck="false">${escapeHtml(lastMessage)}</textarea>
    <div class="gp-confirm-actions">
      <button class="gp-confirm-yes">${onRemote ? "Amend (force-push needed)" : "Amend"}</button>
      <button class="gp-confirm-cancel">Cancel</button>
    </div>`;

  document.body.appendChild(popup);
  const popupRect = popup.getBoundingClientRect();
  const anchorRect = anchorEl.getBoundingClientRect();
  const w = popupRect.width || 320;
  const h = popupRect.height || 200;
  const left = Math.max(4, Math.min(anchorRect.left, window.innerWidth - w - 8));
  const top = anchorRect.top - h - 6 < 4
    ? anchorRect.bottom + 6
    : anchorRect.top - h - 6;
  popup.style.position = "fixed";
  popup.style.top = `${top}px`;
  popup.style.left = `${left}px`;

  const textarea = popup.querySelector(".gp-amend-textarea");
  textarea.focus();
  textarea.select();

  let outsideClick = null;
  const close = () => {
    popup.remove();
    if (outsideClick) document.removeEventListener("mousedown", outsideClick);
  };

  popup.querySelector(".gp-confirm-yes").addEventListener("click", async () => {
    const newMessage = textarea.value.trim();
    // Send None when the message is unchanged so libgit2 keeps the existing one.
    const message = (newMessage && newMessage !== lastMessage.trim()) ? newMessage : null;
    close();
    try {
      const result = await invoke("git_amend_commit", {
        path: currentPath,
        message,
        includeStaged,
      });
      if (result.requires_force_push) {
        showGitFeedback("Amended. Force-push needed to share.", "success");
      } else {
        showGitFeedback("Amended", "success");
      }
      await refreshPanel(null, true);
      refreshFileBrowser();
    } catch (err) {
      showGitFeedback(`Amend failed: ${err}`, "error");
    }
  });
  popup.querySelector(".gp-confirm-cancel").addEventListener("click", close);
  textarea.addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.key === "Escape") close();
  });

  setTimeout(() => {
    outsideClick = (e) => {
      if (!popup.contains(e.target)) close();
    };
    document.addEventListener("mousedown", outsideClick);
  }, 0);
}

// Routes a Pending Operation banner button to the right backend command.
async function runPendingOpAction(kind, action) {
  // Rebase routes through the structured RebaseResult so we can re-open
  // the next conflicted file when continue/skip pauses again. Handled
  // separately from the generic merge / cherry-pick dispatcher below.
  if (kind === "rebase") {
    setInFlightOp(true);
    try {
      if (action === "abort") {
        await invokeWithTimeout("git_rebase_abort", { path: currentPath });
        showGitFeedback("Rebase aborted", "success");
      } else {
        const cmd = action === "skip" ? "git_rebase_skip" : "git_rebase_continue";
        const result = await invokeWithTimeout(cmd, { path: currentPath });
        if (result.completed) {
          showGitFeedback(`Rebase complete. Backup: ${result.backup_tag}`, "success");
        } else {
          const conflicts = result.conflicted_files || [];
          const n = conflicts.length;
          const msg = n > 0
            ? `Still paused — ${n} conflicted file${n === 1 ? "" : "s"}`
            : "Paused at edit step — make changes and click Continue";
          showGitFeedback(msg, n > 0 ? "error" : "success");
          if (n > 0 && openFileInEditor) {
            openFileInEditor(`${currentPath}/${conflicts[0]}`);
          }
        }
      }
    } catch (err) {
      showGitFeedback(`${err}`, "error");
    } finally {
      setInFlightOp(false);
      await refreshPanel(null, true);
      refreshFileBrowser();
    }
    return;
  }

  const cmds = {
    merge: {
      // No first-class continue/abort for merge yet — fall back to commit
      // (continue = create the merge commit) or git_merge_branch with a
      // sentinel. For now, surface helpful feedback rather than hang.
      continue: () => invokeWithTimeout("git_commit", { path: currentPath, message: "Merge" }),
      abort: () => Promise.reject(new Error("Merge abort not yet implemented — use `git merge --abort` from a terminal")),
    },
    cherry_pick: {
      continue: () => invokeWithTimeout("git_cherry_pick_continue", { path: currentPath }),
      abort: () => invokeWithTimeout("git_cherry_pick_abort", { path: currentPath }),
    },
  };
  const handler = cmds[kind]?.[action];
  if (!handler) {
    showGitFeedback(`Unknown ${kind} action: ${action}`, "error");
    return;
  }
  setInFlightOp(true);
  try {
    await handler();
    showGitFeedback(`${kind.replace("_", "-")} ${action}`, "success");
  } catch (err) {
    showGitFeedback(`${err}`, "error");
  } finally {
    setInFlightOp(false);
    await refreshPanel(null, true);
    refreshFileBrowser();
  }
}

// Cherry-pick a single commit onto HEAD. Backend returns a structured
// CherryPickResult so we can route conflicts to the Pending Operation banner
// without a second round-trip. Wrapped in inFlightOp so the polling refresh
// can't re-render the panel mid-shellout.
async function doCherryPick(oid) {
  setInFlightOp(true);
  try {
    const result = await invokeWithTimeout("git_cherry_pick", { path: currentPath, oid });
    if (result.ok) {
      showGitFeedback(`Cherry-picked ${oid.slice(0, 7)}`, "success");
    } else {
      const n = result.conflicted_files.length;
      showGitFeedback(`Cherry-pick has conflicts in ${n} file${n === 1 ? "" : "s"}`, "error");
    }
  } catch (err) {
    showGitFeedback(`Cherry-pick failed: ${err}`, "error");
  } finally {
    setInFlightOp(false);
    await refreshPanel(null, true);
    refreshFileBrowser();
  }
}

// Right-click menu on a commit row. Compare with HEAD, parent, or arbitrary
// ref; copy the OID.
function showCommitContextMenu(mouseEvent, oid) {
  document.querySelector(".gp-commit-menu")?.remove();
  const menu = document.createElement("div");
  menu.className = "context-menu gp-commit-menu";

  const items = [
    {
      label: "Compare with HEAD",
      enabled: !!openDiffInTab,
      onClick: () => openDiffInTab && openDiffInTab({ fromRef: oid, toRef: "HEAD" }),
    },
    {
      label: "Compare with parent",
      enabled: !!openDiffInTab,
      onClick: () => openDiffInTab && openDiffInTab({ fromRef: `${oid}^`, toRef: oid }),
    },
    {
      label: "Compare with…",
      enabled: !!openDiffInTab,
      onClick: () => promptCompareRef(oid, mouseEvent.clientX, mouseEvent.clientY),
    },
    {
      // Cherry-pick is gated on no pending op — applying a commit on top
      // of a half-merged or in-progress rebase tree is asking for trouble.
      label: "Cherry-pick onto HEAD",
      enabled: currentPendingOp.kind === "none",
      onClick: () => doCherryPick(oid),
    },
    {
      // Rebase from here = make this commit's parent the new base, so the
      // rebase tab populates with this commit and everything newer.
      label: "Rebase from here…",
      enabled: !!openRebaseInTab && currentPendingOp.kind === "none",
      onClick: () => openRebaseInTab && openRebaseInTab({ baseOid: `${oid}^` }),
    },
    {
      label: "Copy OID",
      enabled: true,
      onClick: () => {
        navigator.clipboard.writeText(oid).catch(() => {});
        showGitFeedback("Copied OID", "success");
      },
    },
  ];

  for (const it of items) {
    const el = document.createElement("div");
    el.className = "context-menu-item" + (it.enabled ? "" : " context-menu-item-disabled");
    el.textContent = it.label;
    if (it.enabled) {
      el.addEventListener("click", () => {
        menu.remove();
        it.onClick();
      });
    }
    menu.appendChild(el);
  }

  menu.style.left = `${mouseEvent.clientX}px`;
  menu.style.top = `${mouseEvent.clientY}px`;
  document.body.appendChild(menu);

  setTimeout(() => {
    document.addEventListener("click", () => menu.remove(), { once: true });
  }, 0);
}

// Inline popup — Tauri WebView swallows window.prompt(), so we render our
// own. Anchored at the mouse position from the originating context-menu
// click. Validation is backend-side via is_valid_git_ref / is_valid_git_oid.
function promptCompareRef(fromOid, anchorX, anchorY) {
  document.querySelector(".gp-compare-popup")?.remove();
  if (!openDiffInTab) {
    showGitFeedback("Diff tab not wired", "error");
    return;
  }

  const popup = document.createElement("div");
  popup.className = "gp-confirm-popup gp-compare-popup";
  popup.innerHTML = `
    <div class="gp-compare-row">
      <label>From</label>
      <span class="gp-compare-fixed">${escapeHtml(fromOid.slice(0, 12))}</span>
    </div>
    <div class="gp-compare-row">
      <label>To</label>
      <input class="gp-compare-input" type="text" placeholder="branch / OID / HEAD~1" value="HEAD" autocomplete="off" />
    </div>
    <div class="gp-confirm-actions">
      <button class="gp-confirm-yes">Compare</button>
      <button class="gp-confirm-cancel">Cancel</button>
    </div>`;

  document.body.appendChild(popup);
  const popupRect = popup.getBoundingClientRect();
  const w = popupRect.width || 280;
  const h = popupRect.height || 140;
  const left = Math.max(4, Math.min((anchorX ?? 0) - 60, window.innerWidth - w - 8));
  const top = (anchorY ?? 0) + h > window.innerHeight
    ? Math.max(4, (anchorY ?? 0) - h - 4)
    : (anchorY ?? 0) + 4;
  popup.style.position = "fixed";
  popup.style.top = `${top}px`;
  popup.style.left = `${left}px`;

  const input = popup.querySelector(".gp-compare-input");
  input.focus();
  input.select();

  let outsideClick = null;
  const close = () => {
    popup.remove();
    if (outsideClick) document.removeEventListener("mousedown", outsideClick);
  };

  const submit = () => {
    const target = input.value.trim();
    close();
    if (!target) return;
    openDiffInTab({ fromRef: fromOid, toRef: target });
  };

  popup.querySelector(".gp-confirm-yes").addEventListener("click", submit);
  popup.querySelector(".gp-confirm-cancel").addEventListener("click", close);
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") submit();
    if (e.key === "Escape") close();
  });

  setTimeout(() => {
    outsideClick = (e) => {
      if (!popup.contains(e.target)) close();
    };
    document.addEventListener("mousedown", outsideClick);
  }, 0);
}

function showCheatsheet() {
  const existing = document.getElementById("gp-cheatsheet");
  if (existing) { existing.remove(); return; }

  const overlay = document.createElement("div");
  overlay.id = "gp-cheatsheet";
  overlay.className = "gp-cheatsheet-overlay";
  overlay.innerHTML = `
    <div class="gp-cheatsheet-inner">
      <div class="gp-cheatsheet-header">
        <span>Git Reference</span>
        <button class="gp-cheatsheet-close">×</button>
      </div>
      <div class="gp-cheatsheet-body">
        <div class="gp-cheatsheet-section">
          <div class="gp-cheatsheet-title">Staging</div>
          <div class="gp-cheatsheet-item"><kbd>+</kbd> Stage a single file</div>
          <div class="gp-cheatsheet-item"><kbd>Stage All</kbd> Stage every changed file</div>
          <div class="gp-cheatsheet-item"><kbd>−</kbd> Unstage a single file</div>
          <div class="gp-cheatsheet-item"><kbd>Unstage All</kbd> Unstage everything</div>
          <div class="gp-cheatsheet-item"><kbd>×</kbd> Discard working-tree changes</div>
        </div>
        <div class="gp-cheatsheet-section">
          <div class="gp-cheatsheet-title">Committing</div>
          <div class="gp-cheatsheet-item">Pick a prefix, write a message, click Commit or <kbd>Cmd+Enter</kbd></div>
          <div class="gp-cheatsheet-item">Message first line should be &lt;72 chars</div>
        </div>
        <div class="gp-cheatsheet-section">
          <div class="gp-cheatsheet-title">Push / Pull</div>
          <div class="gp-cheatsheet-item"><kbd>↓ Pull</kbd> Fetch + merge remote changes</div>
          <div class="gp-cheatsheet-item"><kbd>↑ Push</kbd> Send local commits to remote</div>
        </div>
        <div class="gp-cheatsheet-section">
          <div class="gp-cheatsheet-title">Branches</div>
          <div class="gp-cheatsheet-item">Click a branch name to check it out</div>
          <div class="gp-cheatsheet-item"><kbd>×</kbd> Delete branch (merged only)</div>
          <div class="gp-cheatsheet-item"><kbd>⤵</kbd> Merge branch into current</div>
          <div class="gp-cheatsheet-item"><kbd>+</kbd> Create new branch from HEAD</div>
        </div>
        <div class="gp-cheatsheet-section">
          <div class="gp-cheatsheet-title">Stash</div>
          <div class="gp-cheatsheet-item"><kbd>⊟ Stash</kbd> Save dirty working tree aside</div>
          <div class="gp-cheatsheet-item"><kbd>⊞ Pop</kbd> Restore most recent stash</div>
        </div>
      </div>
    </div>`;

  const panel = document.getElementById("git-panel");
  panel.appendChild(overlay);

  overlay.querySelector(".gp-cheatsheet-close").addEventListener("click", () => overlay.remove());
}

// ─── Main render function ─────────────────────────────────────────────────────
function renderPanel(status, branches, remoteBranches, commits, remoteUrl, stashes = []) {
  const panel = document.getElementById("git-panel");

  const branchName = status.branch || "HEAD";
  const githubUrl = parseGitHubUrl(remoteUrl);

  const stagedFiles = status.files.filter((f) => f.status.startsWith("index_"));
  const conflictFiles = status.files.filter((f) => f.status === "conflict");
  const unstagedFiles = status.files.filter((f) =>
    ["new", "modified", "deleted", "renamed"].includes(f.status)
  );
  const hasAnyFiles = stagedFiles.length > 0 || unstagedFiles.length > 0;

  // ── 1. Header ──────────────────────────────────────────────────────────────
  // Status dot — single-glance repo health indicator
  // Priority: diverged > behind > ahead > dirty > no-upstream > clean
  const isDirty = stagedFiles.length > 0 || unstagedFiles.length > 0 || conflictFiles.length > 0;
  let statusColor, statusTitle;
  if (status.ahead > 0 && status.behind > 0) {
    statusColor = "#ff5722"; statusTitle = "Diverged";
  } else if (status.behind > 0) {
    statusColor = "#f44336"; statusTitle = "Behind \u2014 pull needed";
  } else if (status.ahead > 0) {
    statusColor = "#2196f3"; statusTitle = "Ahead \u2014 push needed";
  } else if (isDirty) {
    statusColor = "#ff9800"; statusTitle = "Uncommitted changes";
  } else if (!status.has_upstream) {
    statusColor = "#888"; statusTitle = "No upstream configured";
  } else {
    statusColor = "#4caf50"; statusTitle = "Clean";
  }

  let syncHtml = "";
  if (!status.has_upstream) {
    syncHtml = `<span class="gp-sync gp-no-upstream">No upstream</span>`;
  } else if (status.ahead > 0 || status.behind > 0) {
    syncHtml = `<span class="gp-sync">`;
    if (status.ahead > 0) syncHtml += `\u2191${status.ahead} `;
    if (status.behind > 0) syncHtml += `\u2193${status.behind}`;
    syncHtml += `</span>`;
  }

  let githubBtns = "";
  if (githubUrl) {
    githubBtns = `
      <button class="gp-gh-btn" data-url="${escapeHtml(githubUrl)}" title="Open repo on GitHub">Repo</button>
      <button class="gp-gh-btn" data-url="${escapeHtml(githubUrl)}/tree/${escapeHtml(branchName)}" title="Open branch on GitHub">Branch</button>
      <button class="gp-gh-btn gp-gh-btn-pr" data-url="${escapeHtml(githubUrl)}/compare/${escapeHtml(branchName)}" title="Create PR on GitHub">PR</button>`;
  }

  const headerHtml = `
    <div class="gp-header">
      <span class="gp-status-dot" style="color:${statusColor}" title="${statusTitle}">\u25CF</span><span class="gp-branch-name">\u2387 ${escapeHtml(branchName)}</span>
      ${syncHtml}
      <div class="gp-header-right">
        ${githubBtns}
        <button class="gp-help-btn" title="Git reference">?</button>
      </div>
    </div>`;

  // ── 2. Quick Actions Toolbar ───────────────────────────────────────────────
  const toolbarHtml = `
    <div class="gp-toolbar">
      <button class="gp-tool-btn" id="gp-fetch-btn">\u21BB Fetch</button>
      <button class="gp-tool-btn" id="gp-pull-btn">\u2193 Pull</button>
      <button class="gp-tool-btn" id="gp-push-btn">\u2191 Push</button>
      <button class="gp-tool-btn" id="gp-stash-btn">\u229F Stash</button>
      <button class="gp-tool-btn" id="gp-pop-btn">\u229E Pop</button>
    </div>`;

  // ── Stash list ──────────────────────────────────────────────────────────────
  let stashHtml = "";
  if (stashes.length > 0) {
    const rows = stashes
      .map((s) => `
        <div class="gp-stash-entry" data-stash-index="${s.index}">
          <span class="gp-stash-msg">${escapeHtml(s.message)}</span>
          <div class="gp-stash-actions">
            <button class="gp-stash-apply-btn" data-index="${s.index}" title="Apply">Apply</button>
            <button class="gp-stash-drop-btn" data-index="${s.index}" title="Drop">\u00D7</button>
          </div>
        </div>`)
      .join("");
    stashHtml = `
      <div class="gp-section">
        <div class="gp-section-title">STASH (${stashes.length})</div>
        ${rows}
      </div>`;
  }

  // ── 3a. Pending Operation banner ──────────────────────────────────────────
  // Surfaces merge / cherry-pick / rebase pause states with the right
  // continue/abort buttons. Shared across PR2 (cherry-pick), PR1 (merge,
  // already supported), and PR5+ (rebase).
  let pendingOpHtml = "";
  const op = currentPendingOp;
  if (op && op.kind && op.kind !== "none") {
    const labels = {
      merge: "Merge in progress",
      cherry_pick: "Cherry-pick paused",
      rebase: "Rebase in progress",
    };
    const label = labels[op.kind] || op.kind;
    const progress = (op.current_step != null && op.total_steps != null)
      ? ` <span class="gp-pending-op-progress">(${op.current_step}/${op.total_steps})</span>`
      : "";
    const headLine = op.head_message
      ? `<div class="gp-pending-op-head">HEAD: ${escapeHtml(op.head_message)}</div>`
      : "";
    const buttons = op.kind === "rebase"
      ? `
        <button class="gp-pending-op-btn" data-op-action="continue">Continue</button>
        <button class="gp-pending-op-btn" data-op-action="skip">Skip</button>
        <button class="gp-pending-op-btn gp-pending-op-abort" data-op-action="abort">Abort</button>`
      : `
        <button class="gp-pending-op-btn" data-op-action="continue">Continue</button>
        <button class="gp-pending-op-btn gp-pending-op-abort" data-op-action="abort">Abort</button>`;
    pendingOpHtml = `
      <div class="gp-section gp-pending-op-banner gp-pending-op-${op.kind}">
        <div class="gp-pending-op-row">
          <span class="gp-pending-op-label">${label}${progress}</span>
        </div>
        ${headLine}
        <div class="gp-pending-op-actions" data-op-kind="${op.kind}">${buttons}</div>
      </div>`;
  }

  // ── 3. Conflicts ───────────────────────────────────────────────────────────
  let conflictsHtml = "";
  if (conflictFiles.length > 0) {
    const rows = conflictFiles
      .map(
        (f) => `
        <div class="gp-file gp-file-conflict" data-file-path="${escapeHtml(f.path)}">
          <span class="gp-file-status gp-status-conflict">C</span>
          <span class="gp-file-path">${escapeHtml(f.path)}</span>
          <div class="gp-conflict-actions">
            <button class="gp-conflict-btn" data-action="ours" data-file="${escapeHtml(f.path)}">Ours</button>
            <button class="gp-conflict-btn" data-action="theirs" data-file="${escapeHtml(f.path)}">Theirs</button>
            <button class="gp-conflict-btn" data-action="edit" data-file="${escapeHtml(f.path)}">Edit</button>
          </div>
        </div>`
      )
      .join("");
    conflictsHtml = `
      <div class="gp-section gp-conflicts-section">
        <div class="gp-conflict-banner">⚠ Merge conflicts (${conflictFiles.length})</div>
        <div class="gp-file-list">${rows}</div>
      </div>`;
  }

  // ── 4. Staged Changes ─────────────────────────────────────────────────────
  let stagedHtml = `
    <div class="gp-section">
      <div class="gp-section-title">
        STAGED CHANGES${stagedFiles.length > 0 ? ` (${stagedFiles.length})` : ""}
        ${stagedFiles.length > 0 ? `<button class="gp-action-btn" id="unstage-all-btn" title="Unstage all">−</button>` : ""}
      </div>`;
  if (stagedFiles.length === 0) {
    stagedHtml += `<div class="gp-empty-section">Nothing staged</div>`;
  } else {
    stagedHtml += `<div class="gp-file-list">`;
    stagedFiles.forEach((f) => {
      const icon = STATUS_ICONS[f.status] || "?";
      const cls = `gp-status-${f.status.replace("index_", "")}`;
      stagedHtml += `
        <div class="gp-file" data-file-path="${escapeHtml(f.path)}" data-file-status="${f.status}">
          <button class="gp-unstage-btn" title="Unstage file">−</button>
          <span class="gp-file-status ${cls}">${icon}</span>
          <span class="gp-file-path">${escapeHtml(f.path)}</span>
        </div>`;
    });
    stagedHtml += `</div>`;
  }
  stagedHtml += `</div>`;

  // ── 5. Unstaged Changes ───────────────────────────────────────────────────
  let changedHtml = `
    <div class="gp-section">
      <div class="gp-section-title">
        CHANGES${unstagedFiles.length > 0 ? ` (${unstagedFiles.length})` : ""}
        ${unstagedFiles.length > 0 ? `<button class="gp-action-btn" id="stage-all-btn" title="Stage all">+</button>` : ""}
      </div>`;
  if (unstagedFiles.length === 0) {
    changedHtml += `<div class="gp-empty-section">Working tree clean</div>`;
  } else {
    changedHtml += `<div class="gp-file-list">`;
    unstagedFiles.forEach((f) => {
      const icon = STATUS_ICONS[f.status] || "?";
      const cls = `gp-status-${f.status}`;
      changedHtml += `
        <div class="gp-file" data-file-path="${escapeHtml(f.path)}" data-file-status="${f.status}">
          <button class="gp-stage-btn" title="Stage file">+</button>
          <button class="gp-discard-btn" title="Discard changes">×</button>
          <span class="gp-file-status ${cls}">${icon}</span>
          <span class="gp-file-path">${escapeHtml(f.path)}</span>
        </div>`;
    });
    changedHtml += `</div>`;
  }
  changedHtml += `</div>`;

  // ── 6. Commit Form ────────────────────────────────────────────────────────
  let commitHtml = "";
  if (hasAnyFiles) {
    commitHtml = `
      <div class="gp-commit-form">
        <div class="gp-commit-form-top">
          <select id="commit-prefix" class="gp-prefix-select">
            <option value="">type</option>
            <option value="feat">feat</option>
            <option value="fix">fix</option>
            <option value="chore">chore</option>
            <option value="docs">docs</option>
            <option value="refactor">refactor</option>
            <option value="test">test</option>
            <option value="style">style</option>
          </select>
          <span class="gp-char-count" id="commit-char-count">0/72</span>
        </div>
        <textarea id="commit-message" class="gp-commit-textarea" rows="3" placeholder="Commit message..." autocomplete="off"></textarea>
        <div class="gp-commit-actions">
          <button id="commit-btn" class="gp-commit-btn">Commit</button>
          ${(commits.length > 0 && currentPendingOp.kind === "none")
            ? `<button id="amend-btn" class="gp-amend-btn" title="Amend the last commit (uses staged files; reuses last message if textbox is empty)">Amend</button>`
            : ""}
        </div>
      </div>`;
  } else if (commits.length > 0 && currentPendingOp.kind === "none") {
    // No staged changes, but a previous commit exists — offer message-only
    // amend so the user can fix a typo without staging something.
    commitHtml = `
      <div class="gp-commit-form gp-amend-only">
        <button id="amend-message-btn" class="gp-amend-btn" title="Amend the last commit's message">Amend last commit message…</button>
      </div>`;
  }

  // ── 7. Branches ───────────────────────────────────────────────────────────
  const localBranches = branches;
  let branchesHtml = `
    <div class="gp-section">
      <div class="gp-section-title">
        BRANCHES
        <span class="gp-section-actions">
          <button class="gp-action-btn" id="compare-refs-btn" title="Compare two refs">↔</button>
          <button class="gp-action-btn" id="new-branch-btn" title="New branch">+</button>
        </span>
      </div>
      <div class="gp-branch-sub-title">LOCAL</div>
      <div class="gp-branch-list" id="local-branch-list">`;

  localBranches.forEach((b) => {
    const currentClass = b.is_current ? "gp-branch-current" : "";
    const indicator = b.is_current ? "●" : "○";
    const actions = b.is_current
      ? ""
      : `<div class="gp-branch-actions">
           <button class="gp-branch-merge-btn" data-branch="${escapeHtml(b.name)}" title="Merge into current">⤵</button>
           <button class="gp-branch-delete-btn" data-branch="${escapeHtml(b.name)}" title="Delete branch">×</button>
         </div>`;
    branchesHtml += `
      <div class="gp-branch ${currentClass}" data-branch="${escapeHtml(b.name)}">
        <span class="gp-branch-indicator">${indicator}</span>
        <span class="gp-branch-label">${escapeHtml(b.name)}</span>${b.upstream ? `<span class="gp-branch-tracking">\u2192 ${escapeHtml(b.upstream)}</span>` : ""}
        ${actions}
      </div>`;
  });
  branchesHtml += `</div>`;

  if (remoteBranches.length > 0) {
    branchesHtml += `<div class="gp-branch-sub-title">REMOTE</div><div class="gp-branch-list">`;
    remoteBranches.forEach((b) => {
      branchesHtml += `
        <div class="gp-branch gp-branch-remote" data-branch="${escapeHtml(b.name)}" title="Checkout via terminal">
          <span class="gp-branch-indicator gp-branch-indicator-remote">↦</span>
          <span class="gp-branch-label">${escapeHtml(b.name)}</span>
        </div>`;
    });
    branchesHtml += `</div>`;
  }

  branchesHtml += `</div>`;

  // ── 8. Commits ────────────────────────────────────────────────────────────
  let commitsHtml = `
    <div class="gp-section gp-commits-section">
      <div class="gp-section-title">COMMITS</div>
      <div class="gp-commit-list">`;

  commits.forEach((c) => {
    const date = formatRelativeTime(c.timestamp);
    const isMerge = c.parent_count > 1;
    const dotClass = isMerge ? "gp-graph-dot gp-graph-dot-merge" : "gp-graph-dot";
    const isExpanded = c.oid === expandedCommitOid;
    commitsHtml += `
      <div class="gp-commit${isExpanded ? " gp-commit-expanded" : ""}" data-oid="${escapeHtml(c.oid)}">
        <span class="gp-graph-col"><span class="${dotClass}"></span></span>
        <span class="gp-commit-oid">${escapeHtml(c.oid)}</span>
        <span class="gp-commit-msg">${escapeHtml(c.message)}</span>
        <span class="gp-commit-date">${date}</span>
      </div>`;
    if (isExpanded) {
      // If we've already fetched this commit's details, render them inline
      // and skip the loading placeholder — prevents a second fetch on every
      // forced re-render and avoids the flicker.
      const cached = commitDetailCache.get(c.oid);
      if (cached) {
        commitsHtml += `<div class="gp-commit-detail" data-oid="${escapeHtml(c.oid)}">${commitDetailBodyHtml(cached)}</div>`;
      } else {
        commitsHtml += `<div class="gp-commit-detail gp-commit-detail-loading" data-oid="${escapeHtml(c.oid)}"><span class="gp-detail-loading">Loading…</span></div>`;
      }
    }
  });

  commitsHtml += `</div></div>`;

  // ── Assemble & inject ─────────────────────────────────────────────────────
  panel.innerHTML =
    headerHtml +
    toolbarHtml +
    stashHtml +
    pendingOpHtml +
    conflictsHtml +
    stagedHtml +
    changedHtml +
    commitHtml +
    branchesHtml +
    commitsHtml;

  // ── Wire up event handlers ────────────────────────────────────────────────

  // GitHub buttons
  panel.querySelectorAll(".gp-gh-btn").forEach((btn) => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      const url = btn.dataset.url;
      if (url) invoke("plugin:shell|open", { path: url });
    });
  });

  // Stash apply/drop buttons
  panel.querySelectorAll(".gp-stash-apply-btn").forEach((btn) => {
    btn.addEventListener("click", async (e) => {
      e.stopPropagation();
      try {
        await invoke("git_stash_apply", { path: currentPath, index: parseInt(btn.dataset.index) });
        showGitFeedback("Stash applied", "success");
        await refreshPanel(null, true);
        refreshFileBrowser();
      } catch (err) {
        showGitFeedback(`Apply failed: ${err}`, "error");
      }
    });
  });
  panel.querySelectorAll(".gp-stash-drop-btn").forEach((btn) => {
    btn.addEventListener("click", async (e) => {
      e.stopPropagation();
      try {
        await invoke("git_stash_drop", { path: currentPath, index: parseInt(btn.dataset.index) });
        showGitFeedback("Stash dropped", "success");
        await refreshPanel(null, true);
      } catch (err) {
        showGitFeedback(`Drop failed: ${err}`, "error");
      }
    });
  });

  // Help / cheatsheet
  const helpBtn = panel.querySelector(".gp-help-btn");
  if (helpBtn) {
    helpBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      showCheatsheet();
    });
  }

  // Quick action toolbar
  wireToolbarBtn("gp-fetch-btn", "git_fetch", "\u21BB Fetch", "\u21BB Fetching\u2026", "Fetched", "Fetch failed");
  wireToolbarBtn("gp-pull-btn", "git_pull", "\u2193 Pull", "\u2193 Pulling\u2026", "Pull complete", "Pull failed");
  wireToolbarBtn("gp-push-btn", "git_push", "\u2191 Push", "\u2191 Pushing\u2026", "Push complete", "Push failed");
  wireToolbarBtn("gp-stash-btn", "git_stash_save", "\u229F Stash", "\u229F Stashing\u2026", "Stashed", "Stash failed");
  wireToolbarBtn("gp-pop-btn", "git_stash_pop", "\u229E Pop", "\u229E Popping\u2026", "Stash applied", "Pop failed");

  // Conflict resolution
  panel.querySelectorAll(".gp-conflict-btn").forEach((btn) => {
    btn.addEventListener("click", async (e) => {
      e.stopPropagation();
      const filePath = btn.dataset.file;
      const action = btn.dataset.action;
      try {
        if (action === "ours") {
          await invoke("git_resolve_ours", { path: currentPath, filePath });
          showGitFeedback("Resolved with ours", "success");
        } else if (action === "theirs") {
          await invoke("git_resolve_theirs", { path: currentPath, filePath });
          showGitFeedback("Resolved with theirs", "success");
        } else if (action === "edit") {
          // Open the conflicted file in the editor for manual resolution
          const fullPath = currentPath + "/" + filePath;
          if (openFileInEditor) {
            openFileInEditor(fullPath);
          } else {
            showDiff(filePath);
          }
          return;
        }
        await refreshPanel(null, true);
      } catch (err) {
        showGitFeedback(`Failed: ${err}`, "error");
      }
    });
  });

  // Unstage individual file
  panel.querySelectorAll(".gp-unstage-btn").forEach((btn) => {
    btn.addEventListener("click", async (e) => {
      e.stopPropagation();
      const fileEl = btn.closest(".gp-file");
      const filePath = fileEl.dataset.filePath;
      try {
        await invoke("git_unstage_file", { path: currentPath, filePath });
        await refreshPanel(null, true);
      } catch (err) {
        showGitFeedback(`Unstage failed: ${err}`, "error");
      }
    });
  });

  // Unstage All
  const unstageAllBtn = document.getElementById("unstage-all-btn");
  if (unstageAllBtn) {
    unstageAllBtn.addEventListener("click", async (e) => {
      e.stopPropagation();
      try {
        await invoke("git_unstage_all", { path: currentPath });
        await refreshPanel(null, true);
      } catch (err) {
        showGitFeedback(`Unstage all failed: ${err}`, "error");
      }
    });
  }

  // Staged file path click → show staged diff
  panel.querySelectorAll(".gp-file[data-file-status^='index_'] .gp-file-path").forEach((el) => {
    el.addEventListener("click", () => {
      const fileEl = el.closest(".gp-file");
      showDiff(fileEl.dataset.filePath, true);
    });
  });

  // Stage individual file
  panel.querySelectorAll(".gp-stage-btn").forEach((btn) => {
    btn.addEventListener("click", async (e) => {
      e.stopPropagation();
      const fileEl = btn.closest(".gp-file");
      const filePath = fileEl.dataset.filePath;
      try {
        await invoke("git_stage_file", { path: currentPath, filePath });
        await refreshPanel(null, true);
      } catch (err) {
        showGitFeedback(`Stage failed: ${err}`, "error");
      }
    });
  });

  // Discard file (with confirmation)
  panel.querySelectorAll(".gp-discard-btn").forEach((btn) => {
    btn.addEventListener("click", async (e) => {
      e.stopPropagation();
      const fileEl = btn.closest(".gp-file");
      const filePath = fileEl.dataset.filePath;
      showConfirmPopup(btn, `Discard changes to ${filePath}?`, async () => {
        try {
          await invoke("git_discard_file", { path: currentPath, filePath });
          await refreshPanel(null, true);
        } catch (err) {
          showGitFeedback(`Discard failed: ${err}`, "error");
        }
      });
    });
  });

  // Unstaged file path click → show diff
  panel.querySelectorAll(".gp-file:not([data-file-status^='index_']) .gp-file-path").forEach((el) => {
    el.addEventListener("click", () => {
      const fileEl = el.closest(".gp-file");
      showDiff(fileEl.dataset.filePath);
    });
  });

  // Stage All
  const stageAllBtn = document.getElementById("stage-all-btn");
  if (stageAllBtn) {
    stageAllBtn.addEventListener("click", async (e) => {
      e.stopPropagation();
      try {
        await invoke("git_stage_all", { path: currentPath });
        await refreshPanel(null, true);
      } catch (err) {
        showGitFeedback(`Stage all failed: ${err}`, "error");
      }
    });
  }

  // Commit form
  const commitPrefix = document.getElementById("commit-prefix");
  const commitTextarea = document.getElementById("commit-message");
  const commitCharCount = document.getElementById("commit-char-count");
  const commitBtn = document.getElementById("commit-btn");

  if (commitTextarea) {
    // Character count updater
    const updateCharCount = () => {
      const firstLine = commitTextarea.value.split("\n")[0];
      const len = firstLine.length;
      commitCharCount.textContent = `${len}/72`;
      commitCharCount.className = "gp-char-count";
      if (len >= 72) commitCharCount.classList.add("gp-char-count-danger");
      else if (len >= 50) commitCharCount.classList.add("gp-char-count-warn");
    };

    commitTextarea.addEventListener("input", updateCharCount);
    commitTextarea.addEventListener("keydown", (e) => {
      if (e.metaKey && e.key === "Enter") {
        e.preventDefault();
        doCommit();
      }
      e.stopPropagation();
    });
  }

  if (commitPrefix && commitTextarea) {
    commitPrefix.addEventListener("change", () => {
      const prefix = commitPrefix.value;
      if (!prefix) return;
      const current = commitTextarea.value;
      // Prepend prefix if not already present
      const prefixStr = `${prefix}: `;
      if (!current.startsWith(`${prefix}:`)) {
        commitTextarea.value = prefixStr + current;
      }
      commitPrefix.value = "";
      commitTextarea.focus();
      updateCharCount();
    });
  }

  async function doCommit() {
    if (!commitTextarea) return;
    const msg = commitTextarea.value.trim();
    if (!msg) return;
    commitBtn.disabled = true;
    commitBtn.textContent = "Committing…";
    try {
      await invoke("git_commit", { path: currentPath, message: msg });
      showGitFeedback("Committed successfully", "success");
      await refreshPanel(null, true);
      refreshFileBrowser();
    } catch (err) {
      showGitFeedback(`Commit failed: ${err}`, "error");
    } finally {
      if (commitBtn) {
        commitBtn.disabled = false;
        commitBtn.textContent = "Commit";
      }
    }
  }

  if (commitBtn) {
    commitBtn.addEventListener("click", doCommit);
  }

  // Pending Operation banner buttons (merge / cherry-pick / rebase).
  panel.querySelectorAll(".gp-pending-op-actions .gp-pending-op-btn").forEach((btn) => {
    btn.addEventListener("click", async (e) => {
      e.stopPropagation();
      const action = btn.dataset.opAction;
      const kind = btn.parentElement?.dataset.opKind;
      if (!action || !kind) return;
      btn.disabled = true;
      const original = btn.textContent;
      btn.textContent = "…";
      try {
        await runPendingOpAction(kind, action);
      } finally {
        btn.disabled = false;
        btn.textContent = original;
      }
    });
  });

  // Amend buttons (with-staged variant in the form, message-only variant
  // when no staged changes). Both delegate to startAmendFlow which opens
  // the inline editor and handles the libgit2 reachability prompt.
  const amendBtn = document.getElementById("amend-btn");
  if (amendBtn) {
    amendBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      startAmendFlow(amendBtn, /*includeStaged*/ true);
    });
  }
  const amendMessageBtn = document.getElementById("amend-message-btn");
  if (amendMessageBtn) {
    amendMessageBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      startAmendFlow(amendMessageBtn, /*includeStaged*/ false);
    });
  }

  // Local branch checkout
  panel.querySelectorAll(".gp-branch:not(.gp-branch-current):not(.gp-branch-remote)").forEach((el) => {
    el.addEventListener("click", async (e) => {
      if (e.target.closest(".gp-branch-actions")) return;
      const name = el.dataset.branch;
      try {
        await invoke("checkout_branch", { path: currentPath, branchName: name });
        await refreshPanel(null, true);
        refreshFileBrowser();
      } catch (err) {
        showGitFeedback(`Checkout failed: ${err}`, "error");
      }
    });
  });

  // Branch merge buttons
  panel.querySelectorAll(".gp-branch-merge-btn").forEach((btn) => {
    btn.addEventListener("click", async (e) => {
      e.stopPropagation();
      const branchName = btn.dataset.branch;
      showConfirmPopup(btn, `Merge "${branchName}" into current branch?`, async () => {
        // Merge can touch network refs (if the user's config pulls on
        // merge), so it's routed through run_git_cancellable on the Rust
        // side. Set inFlightOp while the invoke is pending so the 3s
        // polling refresh doesn't re-render the panel mid-merge and
        // destroy the confirmation / feedback state.
        setInFlightOp(true);
        try {
          await invokeWithTimeout("git_merge_branch", { path: currentPath, branchName });
          setInFlightOp(false);
          showGitFeedback(`Merged ${branchName}`, "success");
          await refreshPanel(null, true);
          refreshFileBrowser();
        } catch (err) {
          setInFlightOp(false);
          showGitFeedback(`Merge failed: ${err}`, "error");
        } finally {
          setInFlightOp(false);
        }
      });
    });
  });

  // Branch delete buttons
  panel.querySelectorAll(".gp-branch-delete-btn").forEach((btn) => {
    btn.addEventListener("click", async (e) => {
      e.stopPropagation();
      const branchName = btn.dataset.branch;
      showConfirmPopup(btn, `Delete branch "${branchName}"?`, async () => {
        try {
          await invoke("git_delete_branch", { path: currentPath, branchName });
          showGitFeedback(`Deleted ${branchName}`, "success");
          await refreshPanel(null, true);
        } catch (err) {
          showGitFeedback(`Delete failed: ${err}`, "error");
        }
      });
    });
  });

  // New branch button
  const newBranchBtn = document.getElementById("new-branch-btn");
  if (newBranchBtn) {
    newBranchBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      showCreateBranchDialog();
    });
  }

  // Compare refs button
  const compareBtn = document.getElementById("compare-refs-btn");
  if (compareBtn) {
    compareBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      showCompareRefsPopup(compareBtn);
    });
  }

  // (Commit-row contextmenu handled via delegation in initGitPanel.)

  // Commit row expand/collapse
  panel.querySelectorAll(".gp-commit").forEach((el) => {
    el.addEventListener("click", async () => {
      const oid = el.dataset.oid;
      const nextSibling = el.nextElementSibling;
      const isAlreadyExpanded = nextSibling && nextSibling.classList.contains("gp-commit-detail");

      if (isAlreadyExpanded) {
        // Collapse
        nextSibling.remove();
        expandedCommitOid = null;
        el.classList.remove("gp-commit-expanded");
        return;
      }

      expandedCommitOid = oid;
      el.classList.add("gp-commit-expanded");

      const detailEl = document.createElement("div");
      detailEl.className = "gp-commit-detail";
      detailEl.dataset.oid = oid;
      el.after(detailEl);

      const cached = commitDetailCache.get(oid);
      if (cached) {
        detailEl.innerHTML = commitDetailBodyHtml(cached);
        return;
      }
      detailEl.innerHTML = `<span class="gp-detail-loading">Loading…</span>`;

      try {
        const detail = await invoke("get_commit_details", { path: currentPath, oid });
        commitDetailCache.set(oid, detail);
        detailEl.innerHTML = commitDetailBodyHtml(detail);
      } catch (err) {
        detailEl.innerHTML = `<span class="gp-detail-error">Failed to load: ${escapeHtml(String(err))}</span>`;
      }
    });
  });

  // Fetch commit details only when the rendered placeholder is a cache miss.
  // With the detail cache, forced re-renders of an already-expanded commit
  // render the final HTML inline and this block is a no-op — previously both
  // a cached render path and this post-render fetch could race on force=true.
  if (expandedCommitOid) {
    const loadingEl = panel.querySelector(`.gp-commit-detail-loading[data-oid="${expandedCommitOid}"]`);
    if (loadingEl) {
      invoke("get_commit_details", { path: currentPath, oid: expandedCommitOid })
        .then((detail) => {
          commitDetailCache.set(expandedCommitOid, detail);
          loadingEl.classList.remove("gp-commit-detail-loading");
          loadingEl.innerHTML = commitDetailBodyHtml(detail);
        })
        .catch((err) => {
          loadingEl.classList.remove("gp-commit-detail-loading");
          loadingEl.innerHTML = `<span class="gp-detail-error">Failed to load: ${escapeHtml(String(err))}</span>`;
        });
    }
  }
}

// ─── Toolbar button helper ─────────────────────────────────────────────────────
function wireToolbarBtn(id, command, labelDefault, labelBusy, msgSuccess, msgError) {
  const btn = document.getElementById(id);
  if (!btn) return;
  btn.addEventListener("click", async (e) => {
    e.stopPropagation();
    btn.disabled = true;
    btn.textContent = labelBusy;
    // Show cancel button for network operations
    let cancelBtn = null;
    let cancelled = false;
    const isNetworkOp = ["git_fetch", "git_pull", "git_push"].includes(command);
    if (isNetworkOp) {
      setInFlightOp(true);
      cancelBtn = document.createElement("button");
      cancelBtn.className = "gp-cancel-btn";
      cancelBtn.textContent = "Cancel";
      cancelBtn.addEventListener("click", async (ce) => {
        ce.stopPropagation();
        // Set the flag synchronously so the outer catch knows the failure
        // was user-initiated, even if the main op rejects before cancel
        // returns. Disable the button + swap label so repeated clicks are
        // ignored and the user sees progress.
        cancelled = true;
        cancelBtn.disabled = true;
        cancelBtn.textContent = "Cancelling…";
        try { await invoke("cancel_git_op"); } catch (_) {}
      });
      btn.parentElement.insertBefore(cancelBtn, btn.nextSibling);
    }
    try {
      await invokeWithTimeout(command, { path: currentPath });
      // Clear the in-flight flag BEFORE the forced refresh so renderPanel
      // is allowed through and the toolbar returns to its fresh state.
      if (isNetworkOp) setInFlightOp(false);
      showGitFeedback(msgSuccess, "success");
      await refreshPanel(null, true);
      refreshFileBrowser();
    } catch (err) {
      if (isNetworkOp) setInFlightOp(false);
      if (cancelled) {
        showGitFeedback("Operation cancelled", "error");
      } else {
        showGitFeedback(`${msgError}: ${err}`, "error");
      }
    } finally {
      if (isNetworkOp) setInFlightOp(false);
      btn.disabled = false;
      btn.textContent = labelDefault;
      if (cancelBtn) cancelBtn.remove();
    }
  });
}

// ─── Diff preview ──────────────────────────────────────────────────────────────
async function showDiff(filePath, staged = false) {
  try {
    const diff = await invoke("get_file_diff", { path: currentPath, filePath, staged });
    showDiffPreview(filePath, buildDiffHtml(diff.hunks));
  } catch (err) {
    console.error("Diff error:", err);
    showGitFeedback(`Could not open diff: ${err}`, "error");
  }
}

// ─── Create branch dialog ──────────────────────────────────────────────────────
function showCreateBranchDialog() {
  const list = document.getElementById("local-branch-list");
  if (!list || document.getElementById("new-branch-input")) return;

  const inputRow = document.createElement("div");
  inputRow.className = "gp-branch gp-new-branch-input";
  inputRow.innerHTML = `<input id="new-branch-input" type="text" placeholder="branch name..." autocomplete="off" />`;
  list.prepend(inputRow);

  const input = document.getElementById("new-branch-input");
  input.focus();

  input.addEventListener("keydown", async (e) => {
    if (e.key === "Enter") {
      const name = input.value.trim();
      if (name) {
        try {
          await invoke("create_branch", { path: currentPath, branchName: name, checkout: true });
          await refreshPanel(null, true);
        } catch (err) {
          showGitFeedback(`Create branch failed: ${err}`, "error");
        }
      }
    } else if (e.key === "Escape") {
      inputRow.remove();
    }
    e.stopPropagation();
  });

  input.addEventListener("blur", () => {
    setTimeout(() => inputRow.remove(), 150);
  });
}
