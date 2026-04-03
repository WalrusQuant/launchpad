const { invoke } = window.__TAURI__.core;

let currentGitInfo = null;
let gitPollInterval = null;

export async function fetchGitStatus(path) {
  try {
    currentGitInfo = await invoke("get_git_status", { path });
    applyGitColors();
    return currentGitInfo;
  } catch (err) {
    console.error("[git] Git status error:", err);
    return null;
  }
}

function applyGitColors() {
  if (!currentGitInfo || !currentGitInfo.is_repo) return;

  // Build a map of file path → status
  const statusMap = new Map();
  currentGitInfo.files.forEach((f) => {
    statusMap.set(f.path, f.status);
  });

  // Apply to all file entries in the tree
  document.querySelectorAll(".file-entry").forEach((el) => {
    const filePath = el.dataset.path;
    if (!filePath) return;

    // Remove old git classes
    el.classList.remove(
      "git-modified", "git-new", "git-deleted", "git-staged", "git-conflict", "git-renamed",
      "git-index_new", "git-index_modified", "git-index_deleted", "git-index_renamed"
    );

    // Check if this file or any child matches
    for (const [gitPath, status] of statusMap) {
      // Match by filename at end of path
      if (filePath.endsWith("/" + gitPath) || filePath.endsWith("/" + gitPath.split("/")[0])) {
        // All index_ (staged) statuses map to git-staged in the file tree
        if (status.startsWith("index_")) {
          el.classList.add("git-staged");
        } else {
          el.classList.add(`git-${status}`);
        }
        break;
      }
    }
  });
}

export function startGitPolling(getPath, intervalSeconds) {
  if (gitPollInterval) clearInterval(gitPollInterval);
  const ms = (intervalSeconds || 3) * 1000;
  gitPollInterval = setInterval(() => {
    const path = getPath();
    if (path) fetchGitStatus(path);
  }, ms);
}

export function stopGitPolling() {
  if (gitPollInterval) {
    clearInterval(gitPollInterval);
    gitPollInterval = null;
  }
}

export function getGitFileStatus(filePath) {
  if (!currentGitInfo || !currentGitInfo.is_repo) return null;
  for (const f of currentGitInfo.files) {
    if (filePath.endsWith("/" + f.path)) return f.status;
  }
  return null;
}
