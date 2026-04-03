import { showDiffPreview } from "./filebrowser.js";

const { invoke } = window.__TAURI__.core;

let panelVisible = false;
let currentPath = "";

export function initGitPanel(getPath) {
  currentPath = getPath();

  const toggleBtn = document.getElementById("toggle-git-panel");
  toggleBtn.addEventListener("click", () => togglePanel());

  document.addEventListener("keydown", (e) => {
    if (e.metaKey && e.key === "g") {
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
    panel.classList.remove("visible");
    btn.classList.remove("active");
  }
}

export async function refreshPanel(path) {
  if (path) currentPath = path;
  if (!panelVisible) return;

  const panel = document.getElementById("git-panel");

  try {
    const status = await invoke("get_git_status", { path: currentPath });

    if (!status.is_repo) {
      panel.innerHTML = `
        <div class="gp-empty">
          <div class="gp-empty-icon">⎇</div>
          <div class="gp-empty-text">Not a git repository</div>
        </div>`;
      return;
    }

    const [branches, commits] = await Promise.all([
      invoke("list_branches", { path: currentPath }),
      invoke("get_commits", { path: currentPath, count: 30 }),
    ]);

    renderPanel(status, branches, commits);
  } catch (err) {
    panel.innerHTML = `<div class="gp-error">Error: ${err}</div>`;
  }
}

function renderPanel(status, branches, commits) {
  const panel = document.getElementById("git-panel");

  const branchName = status.branch || "HEAD";
  const changedCount = status.files.length;

  let header = `<div class="gp-header">
    <span class="gp-branch-name">⎇ ${branchName}</span>`;
  if (status.ahead > 0 || status.behind > 0) {
    header += `<span class="gp-sync">`;
    if (status.ahead > 0) header += `↑${status.ahead} `;
    if (status.behind > 0) header += `↓${status.behind}`;
    header += `</span>`;
  }
  header += `</div>`;

  // Changed files section with Stage All button
  let changedHtml = `<div class="gp-section">
    <div class="gp-section-title">
      CHANGES${changedCount > 0 ? ` (${changedCount})` : ""}
      ${changedCount > 0 ? '<button class="gp-action-btn" id="stage-all-btn" title="Stage all">↑</button>' : ""}
    </div>`;
  if (status.files.length === 0) {
    changedHtml += `<div class="gp-empty-section">Working tree clean</div>`;
  } else {
    changedHtml += `<div class="gp-file-list">`;
    status.files.forEach((f) => {
      const statusIcon = {
        modified: "M",
        new: "U",
        deleted: "D",
        staged: "S",
        conflict: "C",
        renamed: "R",
      }[f.status] || "?";
      const statusClass = `gp-status-${f.status}`;
      changedHtml += `<div class="gp-file" data-file-path="${escapeHtml(f.path)}" data-file-status="${f.status}">
        <button class="gp-stage-btn" title="Stage file">+</button>
        <span class="gp-file-status ${statusClass}">${statusIcon}</span>
        <span class="gp-file-path">${escapeHtml(f.path)}</span>
      </div>`;
    });
    changedHtml += `</div>`;
  }
  changedHtml += `</div>`;

  // Commit input (only show if there are changes)
  let commitHtml = "";
  if (changedCount > 0) {
    commitHtml = `<div class="gp-commit-form">
      <input id="commit-message" type="text" placeholder="Commit message..." autocomplete="off" />
      <button id="commit-btn" class="gp-commit-btn">Commit</button>
    </div>`;
  }

  // Branches section
  let branchesHtml = `<div class="gp-section">
    <div class="gp-section-title">
      BRANCHES
      <button class="gp-action-btn" id="new-branch-btn" title="New branch">+</button>
    </div>
    <div class="gp-branch-list">`;
  branches.forEach((b) => {
    const currentClass = b.is_current ? "gp-branch-current" : "";
    const indicator = b.is_current ? "●" : "○";
    branchesHtml += `<div class="gp-branch ${currentClass}" data-branch="${b.name}">
      <span class="gp-branch-indicator">${indicator}</span>
      <span class="gp-branch-label">${b.name}</span>
    </div>`;
  });
  branchesHtml += `</div></div>`;

  // Commits section
  let commitsHtml = `<div class="gp-section gp-commits-section">
    <div class="gp-section-title">COMMITS</div>
    <div class="gp-commit-list">`;
  commits.forEach((c) => {
    const date = formatRelativeTime(c.timestamp);
    commitsHtml += `<div class="gp-commit">
      <span class="gp-commit-oid">${c.oid}</span>
      <span class="gp-commit-msg">${escapeHtml(c.message)}</span>
      <span class="gp-commit-date">${date}</span>
    </div>`;
  });
  commitsHtml += `</div></div>`;

  panel.innerHTML = header + changedHtml + commitHtml + branchesHtml + commitsHtml;

  // Wire up file click → show diff
  panel.querySelectorAll(".gp-file").forEach((el) => {
    const filePath = el.dataset.filePath;
    el.querySelector(".gp-file-path").addEventListener("click", () => {
      showDiff(filePath);
    });
  });

  // Wire up individual stage buttons
  panel.querySelectorAll(".gp-stage-btn").forEach((btn) => {
    btn.addEventListener("click", async (e) => {
      e.stopPropagation();
      const fileEl = btn.closest(".gp-file");
      const filePath = fileEl.dataset.filePath;
      try {
        await invoke("git_stage_file", { path: currentPath, filePath });
        refreshPanel();
      } catch (err) {
        console.error("Stage error:", err);
      }
    });
  });

  // Wire up Stage All
  const stageAllBtn = document.getElementById("stage-all-btn");
  if (stageAllBtn) {
    stageAllBtn.addEventListener("click", async (e) => {
      e.stopPropagation();
      try {
        await invoke("git_stage_all", { path: currentPath });
        refreshPanel();
      } catch (err) {
        console.error("Stage all error:", err);
      }
    });
  }

  // Wire up Commit
  const commitBtn = document.getElementById("commit-btn");
  const commitInput = document.getElementById("commit-message");
  if (commitBtn && commitInput) {
    async function doCommit() {
      const msg = commitInput.value.trim();
      if (!msg) return;
      try {
        const oid = await invoke("git_commit", { path: currentPath, message: msg });
        commitInput.value = "";
        refreshPanel();
      } catch (err) {
        alert(`Commit failed: ${err}`);
      }
    }

    commitBtn.addEventListener("click", doCommit);
    commitInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") { e.preventDefault(); doCommit(); }
      e.stopPropagation();
    });
  }

  // Wire up branch click handlers
  panel.querySelectorAll(".gp-branch:not(.gp-branch-current)").forEach((el) => {
    el.addEventListener("click", async () => {
      const name = el.dataset.branch;
      try {
        await invoke("checkout_branch", { path: currentPath, branchName: name });
        refreshPanel();
      } catch (err) {
        alert(`Failed to checkout: ${err}`);
      }
    });
  });

  // Wire up new branch button
  const newBranchBtn = document.getElementById("new-branch-btn");
  if (newBranchBtn) {
    newBranchBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      showCreateBranchDialog();
    });
  }
}

async function showDiff(filePath) {
  try {
    const diff = await invoke("get_file_diff", { path: currentPath, filePath });

    // Build colored diff HTML
    let html = "";
    const lines = diff.split("\n");
    lines.forEach((line) => {
      const cls = line.startsWith("+") ? "diff-add" : line.startsWith("-") ? "diff-del" : "";
      const escaped = line.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
      html += `<div class="diff-line ${cls}">${escaped}</div>`;
    });

    showDiffPreview(filePath, html);
  } catch (err) {
    console.error("Diff error:", err);
  }
}

function showCreateBranchDialog() {
  const section = document.querySelector(".gp-branch-list");
  if (!section || document.getElementById("new-branch-input")) return;

  const inputRow = document.createElement("div");
  inputRow.className = "gp-branch gp-new-branch-input";
  inputRow.innerHTML = `<input id="new-branch-input" type="text" placeholder="branch name..." autocomplete="off" />`;

  section.prepend(inputRow);

  const input = document.getElementById("new-branch-input");
  input.focus();

  input.addEventListener("keydown", async (e) => {
    if (e.key === "Enter") {
      const name = input.value.trim();
      if (name) {
        try {
          await invoke("create_branch", {
            path: currentPath,
            branchName: name,
            checkout: true,
          });
          refreshPanel();
        } catch (err) {
          alert(`Failed to create branch: ${err}`);
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

function escapeHtml(str) {
  return str
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}
