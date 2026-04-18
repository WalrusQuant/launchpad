import { showDiffPreview, refreshFileBrowser } from "./filebrowser.js";
import { setPanelTransitioning } from "./main.js";
import { matches as keyMatches } from "./keymap.js";

const { invoke } = window.__TAURI__.core;

// Wraps invoke with a timeout to prevent indefinite hangs on network operations
function invokeWithTimeout(command, args, timeoutMs = 30000) {
  return Promise.race([
    invoke(command, args),
    new Promise((_, reject) =>
      setTimeout(() => reject(new Error("Operation timed out")), timeoutMs)
    ),
  ]);
}

// ─── Module-level state ────────────────────────────────────────────────────────
let panelVisible = false;
let currentPath = "";
let lastSnapshot = null;
let expandedCommitOid = null;
let openFileInEditor = null;

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
export function initGitPanel(getPath, openFileCb) {
  currentPath = getPath();
  openFileInEditor = openFileCb || null;

  const toggleBtn = document.getElementById("toggle-git-panel");
  toggleBtn.addEventListener("click", () => togglePanel());

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

export async function refreshPanel(path, force = false) {
  if (path) currentPath = path;
  if (!panelVisible) return;

  const panel = document.getElementById("git-panel");
  const header = panel.querySelector(".gp-header");
  if (header) header.classList.add("gp-refreshing");

  try {
    const status = await invoke("get_git_status", { path: currentPath });

    if (!status.is_repo) {
      panel.innerHTML = `
        <div class="gp-empty">
          <div class="gp-empty-icon">⎇</div>
          <div class="gp-empty-text">Not a git repository</div>
        </div>`;
      lastSnapshot = null;
      return;
    }

    const [branches, remoteBranches, commits, remoteUrl, stashes] = await Promise.all([
      invoke("list_branches", { path: currentPath }),
      invoke("list_remote_branches", { path: currentPath }).catch(() => []),
      invoke("get_commits", { path: currentPath, count: 30 }),
      invoke("get_remote_url", { path: currentPath }).catch(() => null),
      invoke("git_stash_list", { path: currentPath }).catch(() => []),
    ]);

    const snapshot = JSON.stringify({ status, branches, remoteBranches, commits, stashes });
    if (snapshot === lastSnapshot && !force) return;
    lastSnapshot = snapshot;

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
  // SSH: git@github.com:user/repo.git
  const sshMatch = remoteUrl.match(/git@github\.com[:/](.+?)(?:\.git)?$/);
  if (sshMatch) return `https://github.com/${sshMatch[1]}`;
  // HTTPS: https://github.com/user/repo.git
  const httpsMatch = remoteUrl.match(/https?:\/\/github\.com\/(.+?)(?:\.git)?$/);
  if (httpsMatch) return `https://github.com/${httpsMatch[1]}`;
  return null;
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
        <button id="commit-btn" class="gp-commit-btn">Commit</button>
      </div>`;
  }

  // ── 7. Branches ───────────────────────────────────────────────────────────
  const localBranches = branches;
  let branchesHtml = `
    <div class="gp-section">
      <div class="gp-section-title">
        BRANCHES
        <button class="gp-action-btn" id="new-branch-btn" title="New branch">+</button>
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
      commitsHtml += `<div class="gp-commit-detail gp-commit-detail-loading" data-oid="${escapeHtml(c.oid)}"><span class="gp-detail-loading">Loading…</span></div>`;
    }
  });

  commitsHtml += `</div></div>`;

  // ── Assemble & inject ─────────────────────────────────────────────────────
  panel.innerHTML =
    headerHtml +
    toolbarHtml +
    stashHtml +
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
        try {
          await invoke("git_merge_branch", { path: currentPath, branchName });
          showGitFeedback(`Merged ${branchName}`, "success");
          await refreshPanel(null, true);
          refreshFileBrowser();
        } catch (err) {
          showGitFeedback(`Merge failed: ${err}`, "error");
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

      // Insert loading placeholder
      const detailEl = document.createElement("div");
      detailEl.className = "gp-commit-detail";
      detailEl.dataset.oid = oid;
      detailEl.innerHTML = `<span class="gp-detail-loading">Loading…</span>`;
      el.after(detailEl);

      try {
        const detail = await invoke("get_commit_details", { path: currentPath, oid });
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
        detailEl.innerHTML = `
          <div class="gp-detail-meta">${escapeHtml(detail.author)} · ${formatRelativeTime(detail.timestamp)}</div>
          <div class="gp-detail-files">${filesHtml || "<em>No file changes</em>"}</div>`;
      } catch (err) {
        detailEl.innerHTML = `<span class="gp-detail-error">Failed to load: ${escapeHtml(String(err))}</span>`;
      }
    });
  });

  // If there's an expanded commit and it rendered with a loading placeholder, fetch it
  if (expandedCommitOid) {
    const loadingEl = panel.querySelector(`.gp-commit-detail-loading[data-oid="${expandedCommitOid}"]`);
    if (loadingEl) {
      invoke("get_commit_details", { path: currentPath, oid: expandedCommitOid })
        .then((detail) => {
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
          loadingEl.classList.remove("gp-commit-detail-loading");
          loadingEl.innerHTML = `
            <div class="gp-detail-meta">${escapeHtml(detail.author)} · ${formatRelativeTime(detail.timestamp)}</div>
            <div class="gp-detail-files">${filesHtml || "<em>No file changes</em>"}</div>`;
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
    if (["git_fetch", "git_pull", "git_push"].includes(command)) {
      cancelBtn = document.createElement("button");
      cancelBtn.className = "gp-cancel-btn";
      cancelBtn.textContent = "Cancel";
      cancelBtn.addEventListener("click", async (ce) => {
        ce.stopPropagation();
        await invoke("cancel_git_op");
        showGitFeedback("Operation cancelled", "error");
      });
      btn.parentElement.insertBefore(cancelBtn, btn.nextSibling);
    }
    try {
      await invokeWithTimeout(command, { path: currentPath });
      showGitFeedback(msgSuccess, "success");
      await refreshPanel(null, true);
      refreshFileBrowser();
    } catch (err) {
      showGitFeedback(`${msgError}: ${err}`, "error");
    } finally {
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
    let html = "";
    for (const hunk of diff.hunks) {
      html += `<div class="diff-hunk-header">@@ -${hunk.old_start},${hunk.old_lines} +${hunk.new_start},${hunk.new_lines} @@</div>`;
      for (const line of hunk.lines) {
        const cls = line.origin === "add" ? "diff-add" : line.origin === "remove" ? "diff-del" : "";
        const oldNo = line.old_line_no >= 0 ? line.old_line_no : "";
        const newNo = line.new_line_no >= 0 ? line.new_line_no : "";
        const escaped = line.content
          .replace(/&/g, "&amp;")
          .replace(/</g, "&lt;")
          .replace(/>/g, "&gt;")
          .replace(/\n$/, "");
        html += `<div class="diff-line ${cls}"><span class="diff-gutter">${oldNo}</span><span class="diff-gutter">${newNo}</span><span class="diff-content">${escaped}</span></div>`;
      }
    }
    showDiffPreview(filePath, html);
  } catch (err) {
    console.error("Diff error:", err);
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
