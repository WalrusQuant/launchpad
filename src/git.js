import { refreshPanel } from "./gitpanel.js";

const { invoke } = window.__TAURI__.core;

let currentGitInfo = null;
let currentGitRoot = null;
let gitPollInterval = null;

// Snapshot of `get_pending_op_state` from the most recent poll. Exposed via
// getter so the panel can render the Pending Operation banner without a
// second invoke per cycle. Shape: { kind, current_step?, total_steps?, head_message? }.
let currentPendingOp = { kind: "none" };

export function getPendingOp() {
  return currentPendingOp;
}

// True while a network git op (push/pull/fetch/merge) is in flight. Polling
// refreshPanel calls skip their renderPanel step while this is set so the
// toolbar DOM — including the Cancel button and the busy-state on the
// active button — isn't destroyed out from under the user. Lives in `git.js`
// so the diff tab and rebase tab can share the same cancellation flow.
// Read via `inFlightOp` (live binding); flip via `setInFlightOp(bool)`.
export let inFlightOp = false;

export function setInFlightOp(v) {
  inFlightOp = !!v;
}

// Wraps invoke with a timeout to prevent indefinite hangs on network
// operations. Tauri IPC has no cancellation — Promise.race rejecting does
// NOT stop the Rust side, so on timeout we explicitly call cancel_git_op
// to kill the git child process and free its PID slot. Without this, every
// timeout leaks a git process and occupies the single-slot pid_store,
// making subsequent ops uncancellable.
export async function invokeWithTimeout(command, args, timeoutMs = 30000) {
  let timedOut = false;
  let timerId;
  const timer = new Promise((_, reject) => {
    timerId = setTimeout(() => {
      timedOut = true;
      reject(new Error("Operation timed out"));
    }, timeoutMs);
  });
  try {
    return await Promise.race([invoke(command, args), timer]);
  } finally {
    clearTimeout(timerId);
    if (timedOut) {
      try { await invoke("cancel_git_op"); } catch (_) {}
    }
  }
}

export async function fetchGitStatus(path) {
  try {
    currentGitInfo = await invoke("get_git_status", { path });
    currentGitRoot = path;
    applyGitColors();
    return currentGitInfo;
  } catch (err) {
    console.error("[git] Git status error:", err);
    return null;
  }
}

// Single-character badges shown next to each file's name in the file pane.
// Same vocabulary git itself uses (M/A/D/R/U) plus `?` for untracked and
// `!` for conflict. Color (via the .git-* class) carries the same signal
// for non-colorblind users; the badge gives an explicit, equally-visible
// channel that works in monochrome and screen-reader contexts.
const GIT_STATUS_BADGE = {
  modified: "M",
  new: "?",
  deleted: "D",
  conflict: "!",
  renamed: "R",
  index_new: "A",
  index_modified: "M",
  index_deleted: "D",
  index_renamed: "R",
};

function applyGitColors() {
  if (!currentGitInfo || !currentGitInfo.is_repo || !currentGitRoot) return;

  // Map of git-relative-path → status
  const statusMap = new Map();
  currentGitInfo.files.forEach((f) => {
    statusMap.set(f.path, f.status);
  });

  const rootWithSlash = currentGitRoot.replace(/\/+$/, "") + "/";

  document.querySelectorAll(".file-entry").forEach((el) => {
    el.classList.remove(
      "git-modified", "git-new", "git-deleted", "git-staged", "git-conflict", "git-renamed",
      "git-index_new", "git-index_modified", "git-index_deleted", "git-index_renamed"
    );
    // Clear any prior badge before deciding whether to set a new one;
    // otherwise a file that just transitioned modified→clean would keep
    // its old badge.
    const badge = el.querySelector(".file-git-badge");
    if (badge) badge.textContent = "";

    const filePath = el.dataset.path;
    if (!filePath || !filePath.startsWith(rootWithSlash)) return;
    const rel = filePath.slice(rootWithSlash.length);

    // First check exact match. Then check if this entry is an ancestor
    // directory of a modified file (so a folder containing modified
    // children shows a status color). Anchoring on the project root
    // prevents the old endsWith() match from colliding with same-named
    // files in sibling trees (src/foo.js vs lib/foo.js).
    let matched = statusMap.get(rel);
    if (!matched) {
      const relWithSlash = rel + "/";
      for (const [gitPath, status] of statusMap) {
        if (gitPath.startsWith(relWithSlash)) {
          matched = status;
          break;
        }
      }
    }
    if (matched) {
      if (matched.startsWith("index_")) {
        el.classList.add("git-staged");
      } else {
        el.classList.add(`git-${matched}`);
      }
      if (badge) badge.textContent = GIT_STATUS_BADGE[matched] || "";
    }
  });
}

export function startGitPolling(getPath, intervalSeconds) {
  if (gitPollInterval) clearInterval(gitPollInterval);
  const ms = (intervalSeconds || 3) * 1000;
  gitPollInterval = setInterval(async () => {
    const path = getPath();
    if (!path) return;
    // Fetch once per tick and hand the result to refreshPanel so the panel
    // doesn't invoke get_git_status a second time. Same trick for the
    // pending-op state — one poll per cycle, panel reads from getPendingOp().
    const [status, pending] = await Promise.all([
      fetchGitStatus(path),
      invoke("get_pending_op_state", { path }).catch(() => ({ kind: "none" })),
    ]);
    currentPendingOp = pending || { kind: "none" };
    refreshPanel(path, false, status, currentPendingOp);
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
