const { invoke } = window.__TAURI__.core;
import { pushEscape, popEscape, showConfirmDialog } from "./main.js";
import { PATH_RENAMED } from "./events.js";

let currentPath = "";
let projectRoot = ""; // set by initFileBrowser; navigation is capped at or within this
let homeDir = ""; // user home, resolved once at init for shortenPath/`~` replacement
let showHidden = false;
let expandedDirs = new Set();
let getActiveTabId = null; // set by main.js
let openFileCallback = null; // set by main.js — opens file as editor tab
let searchQuery = "";
let searchDebounce = null;
// Roving-tabindex focus state. Exactly one tree row carries tabIndex=0 at
// any time — that's the row arrow keys / Tab will land on. focusedPath
// survives tree rebuilds (rename, fs-changed, search) so the user's place
// in the tree isn't lost. Falls back to the first row when the previously
// focused path is no longer present.
let focusedPath = null;
// Multi-selection state. `selectedPaths` is the set of currently selected
// row paths; `selectionAnchor` is the last plain-click path, used as one
// end of a Shift+click range. Always at least one selected entry exists
// after the user has clicked anywhere — selection clears only when the
// tree rebuilds and no entry survives.
let selectedPaths = new Set();
let selectionAnchor = null;

const icons = {
  folder: "📁",
  folderOpen: "📂",
  js: "◆",
  ts: "◇",
  py: "◆",
  rs: "◆",
  json: "{ }",
  md: "◆",
  html: "◆",
  css: "◆",
  toml: "◆",
  yaml: "◆",
  yml: "◆",
  git: "◆",
  default: "◆",
};

// Previewable file extensions
const previewExts = new Set([
  "js", "ts", "jsx", "tsx", "py", "rs", "go", "rb", "java", "c", "cpp", "h", "r", "rmd", "sql", "swift", "kt",
  "json", "toml", "yaml", "yml", "xml", "csv",
  "md", "txt", "mdx", "rst",
  "html", "css", "scss", "less",
  "sh", "bash", "zsh", "fish",
  "env", "gitignore", "dockerignore", "editorconfig",
  "lock", "cfg", "ini", "conf",
]);

function getIcon(entry) {
  if (entry.is_dir) return icons.folder;
  const ext = entry.name.split(".").pop().toLowerCase();
  return icons[ext] || icons.default;
}

function getFileClass(entry) {
  if (entry.is_dir) return "file-entry dir";
  const ext = entry.name.split(".").pop().toLowerCase();
  if (["js", "ts", "jsx", "tsx"].includes(ext)) return "file-entry type-js";
  if (["py"].includes(ext)) return "file-entry type-py";
  if (["rs"].includes(ext)) return "file-entry type-rs";
  if (["json", "toml", "yaml", "yml"].includes(ext)) return "file-entry type-config";
  if (["md", "txt", "mdx"].includes(ext)) return "file-entry type-doc";
  if (["html", "css", "scss"].includes(ext)) return "file-entry type-web";
  return "file-entry";
}

function formatSize(bytes) {
  if (bytes === 0) return "—";
  if (bytes < 1024) return bytes + " B";
  if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + " KB";
  return (bytes / (1024 * 1024)).toFixed(1) + " MB";
}

// Reject filenames that would create or traverse directories. macOS allows
// almost any byte except `/` and NUL in a filename, but a user typing a
// slash into the rename / new-file input is overwhelmingly mistaken — they
// want `report.md`, not a phantom `report` directory containing `md`. Also
// reject `.` and `..` (POSIX reserved) and the empty string. Other
// platform-specific sins (Windows reserved names, control chars) are not
// our problem on macOS.
function isInvalidFileName(name) {
  if (!name) return "Name cannot be empty";
  if (name === "." || name === "..") return "Name cannot be '.' or '..'";
  if (name.includes("/") || name.includes("\\")) return "Name cannot contain '/' or '\\\\'";
  if (name.includes("\0")) return "Name cannot contain NUL bytes";
  return null;
}

// Wrap a path in single quotes for safe paste into a POSIX shell. Single
// quotes prevent every form of expansion the shell does (variables, glob,
// backticks). The only character that can't appear inside single quotes is
// a single quote itself — escape that via the standard `'\''` close-and-
// reopen trick. Always quote (even when there are no spaces) so the
// pasted token is unambiguous.
function shellQuoteSingle(path) {
  return "'" + String(path).replace(/'/g, "'\\''") + "'";
}

function shortenPath(path) {
  // Use the actual home dir resolved via get_home_dir at init. Previously
  // we assumed /Users/<name> at depth 2, which works on vanilla macOS
  // but not for "/Users/first.last@company" setups, Docker-mounted paths,
  // or Linux /home/user.
  if (homeDir && path.startsWith(homeDir)) {
    return "~" + path.slice(homeDir.length);
  }
  return path;
}

function isPreviewable(name) {
  const ext = name.split(".").pop().toLowerCase();
  // Also preview dotfiles like .gitignore, .env
  if (name.startsWith(".") && !name.includes("/")) return true;
  return previewExts.has(ext);
}

// Top-level loads are serialized so rapid user actions (toggle-hidden,
// search input, setRoot) can't race and swap stale trees into the DOM.
// Recursive child loads (depth > 0) run inside their parent's promise and
// don't need the guard.
let topLoadChain = Promise.resolve();

// Snapshot cache for the diff-and-patch fast path. Key = the container
// element (#file-tree at depth 0, or a .dir-children DIV inside an
// expanded dir). Value = a stable string fingerprint of the last load's
// (entries × expandedDirs) — same comparison technique git panel uses.
// When fs-changed fires but the directory's user-visible content didn't
// actually change (touch on metadata, lock file flip, etc.) we skip the
// DOM rebuild entirely. WeakMap so a removed container is GC'd and
// doesn't leak its snapshot.
const directorySnapshots = new WeakMap();

function loadDirectory(path, parentEl, depth = 0) {
  if (depth > 0) return _doLoadDirectory(path, parentEl, depth);
  const next = topLoadChain
    .catch(() => {})
    .then(() => _doLoadDirectory(path, parentEl, 0));
  topLoadChain = next;
  return next;
}

// Stable fingerprint of a directory's filtered listing. Includes the bits
// that affect render output (name, dir flag, size, mtime, expanded
// children) but skips fields the file pane never displays. Order matters
// — read_directory already returns dirs-first-then-alpha, so a string
// concat preserves comparison fidelity without allocating extra arrays.
function computeDirectorySnapshot(filtered) {
  const expandedSubset = filtered
    .filter((e) => e.is_dir && expandedDirs.has(e.path))
    .map((e) => e.path)
    .join("|");
  const entriesPart = filtered
    .map((e) => `${e.is_dir ? "d" : "f"}\t${e.name}\t${e.size}\t${e.modified || 0}`)
    .join("\n");
  return entriesPart + "\n--\n" + expandedSubset;
}

async function _doLoadDirectory(path, parentEl, depth = 0) {
  try {
    // Show skeleton placeholder while loading (only for initial empty load)
    if (depth === 0 && parentEl.children.length === 0) {
      parentEl.innerHTML = '<div class="skeleton-rows">' +
        '<div class="skeleton-row"></div>'.repeat(5) + '</div>';
    }
    const entries = await invoke("read_directory", { path });
    let filtered = showHidden
      ? entries
      : entries.filter((e) => !e.is_hidden);
    // Note: when searchQuery is non-empty we render via renderSearchResults
    // instead of this tree path, so no per-level filter is needed here.

    // Fast path — if THIS level's listing + expansion set didn't change,
    // the rows we'd build for it would be identical to the ones already in
    // the DOM. Skip the fragment build and the swap. We still recurse
    // into expanded children so a deep file change (modification three
    // folders down) propagates — only THIS level's render is skipped.
    // Saves a chunk of work on bursty fs-changed events that don't
    // reflect a user-visible change at the current level (metadata
    // touches, .DS_Store flips at root, etc.).
    const snapshot = computeDirectorySnapshot(filtered);
    const canFastPath =
      directorySnapshots.get(parentEl) === snapshot &&
      parentEl.children.length > 0;
    directorySnapshots.set(parentEl, snapshot);

    if (canFastPath) {
      const childPromises = [];
      for (const entry of filtered) {
        if (!entry.is_dir || !expandedDirs.has(entry.path)) continue;
        const row = parentEl.querySelector(
          `.file-entry[data-path="${CSS.escape(entry.path)}"]`
        );
        const childContainer = row?.nextElementSibling;
        if (childContainer && childContainer.classList.contains("dir-children")) {
          childPromises.push(loadDirectory(entry.path, childContainer, depth + 1));
        }
      }
      await Promise.all(childPromises);
      if (depth === 0) applyRovingTabIndex();
      return;
    }

    // Build new content in a fragment off-DOM to avoid flicker
    const fragment = document.createDocumentFragment();
    const childLoadPromises = [];

    filtered.forEach((entry, idx) => {
      const row = document.createElement("div");
      row.className = getFileClass(entry);
      row.style.paddingLeft = (12 + depth * 16) + "px";
      row.dataset.path = entry.path;
      row.dataset.depth = String(depth);
      row.setAttribute("role", "treeitem");
      // Roving tabindex: exactly one row in the tree gets tabIndex=0; the
      // rest are tabIndex=-1 but still programmatically focusable. The
      // post-build pass (applyRovingTabIndex) decides which row wins.
      row.tabIndex = -1;
      // ARIA position metadata for screen readers — depth in the tree
      // plus 1-indexed position among siblings. With this set, VoiceOver
      // announces "level 2, item 3 of 17" while navigating.
      row.setAttribute("aria-level", String(depth + 1));
      row.setAttribute("aria-setsize", String(filtered.length));
      row.setAttribute("aria-posinset", String(idx + 1));
      if (entry.is_dir) row.setAttribute("aria-expanded", expandedDirs.has(entry.path) ? "true" : "false");

      // Twirl chevron — visual affordance that this row can be expanded.
      // Only directories show one; files render an empty slot of the same
      // width so file icons line up across the column. Clicking the
      // chevron toggles the directory regardless of where else on the row
      // you might click. We keep the existing "single-click anywhere
      // toggles the dir" behavior because that's what VS Code's default
      // explorer does and what most users now expect — the chevron is
      // the EXPLICIT, discoverable target, not the only one.
      const chevron = document.createElement("span");
      chevron.className = "file-chevron";
      if (entry.is_dir) {
        chevron.textContent = expandedDirs.has(entry.path) ? "▾" : "▸";
        chevron.addEventListener("click", async (e) => {
          // Stop propagation so the row's own click handler doesn't
          // double-toggle. Only fires for explicit chevron clicks.
          e.stopPropagation();
          focusedPath = entry.path;
          applyRovingTabIndex();
          await toggleDirectory(entry, row, depth);
        });
      }

      const icon = document.createElement("span");
      icon.className = "file-icon";
      icon.textContent = expandedDirs.has(entry.path)
        ? icons.folderOpen
        : getIcon(entry);

      const name = document.createElement("span");
      name.className = "file-name";
      name.textContent = entry.name;
      // Tooltip on the row carries the full filename so a user whose pane
      // is too narrow to show the whole name can hover to recover it. Set
      // on the row (not just .file-name) so hovering anywhere in the row's
      // padding works too.
      row.title = entry.name;

      // Git status badge slot — populated by applyGitColors when the file
      // has a recognized git status. Always present in the row layout so
      // adding/removing the badge text doesn't shift sibling columns.
      const badge = document.createElement("span");
      badge.className = "file-git-badge";

      row.appendChild(chevron);
      row.appendChild(icon);
      row.appendChild(name);
      row.appendChild(badge);

      if (!entry.is_dir) {
        const size = document.createElement("span");
        size.className = "file-size";
        size.textContent = formatSize(entry.size);
        row.appendChild(size);
      }

      // Click handler. Selection model:
      //   plain click           — select this row only, then act (toggle / open)
      //   cmd / ctrl + click    — toggle this row in the selection (no action)
      //   shift + click         — range-select from anchor to this row (no action)
      // Action (toggle for dirs, open for files) only fires on plain click —
      // multi-select clicks just build up the selection set so the next
      // context-menu pick (e.g. Delete) can target multiple files.
      row.addEventListener("click", async (e) => {
        e.stopPropagation();
        focusedPath = entry.path;
        if (e.shiftKey && selectionAnchor) {
          selectRange(selectionAnchor, entry.path);
          applyRovingTabIndex();
          return;
        }
        if (e.metaKey || e.ctrlKey) {
          toggleSelection(entry.path);
          // Anchor advances to the most recently toggled-on path; on toggle-
          // off we leave the anchor put so a subsequent shift-click still
          // makes sense relative to the user's last "on" pick.
          if (selectedPaths.has(entry.path)) selectionAnchor = entry.path;
          applyRovingTabIndex();
          return;
        }
        // Plain click — collapse multi-selection to just this row.
        setSingleSelection(entry.path);
        selectionAnchor = entry.path;
        applyRovingTabIndex();
        if (entry.is_dir) {
          await toggleDirectory(entry, row, depth);
        } else if (isPreviewable(entry.name)) {
          showFilePreview(entry);
        }
      });

      // Double-click folder to cd into it
      if (entry.is_dir) {
        row.addEventListener("dblclick", (e) => {
          e.stopPropagation();
          navigateToDirectory(entry.path);
        });
      }

      // Drag setup. Two dataTransfer payloads on every drag:
      //   application/x-launchpad-paths — JSON array; consumed by
      //     internal drop targets (other directory rows, tree empty space)
      //     to perform a file move via rename_path.
      //   text/plain — shell-quoted path(s) joined with spaces; consumed
      //     by the terminal so a drag-to-terminal pastes a paste-safe
      //     argv tail.
      // When a row that's part of a multi-selection is dragged, ALL
      // selected paths travel; otherwise just the row's path.
      row.draggable = true;
      row.addEventListener("dragstart", (e) => {
        const paths = (selectedPaths.has(entry.path) && selectedPaths.size > 1)
          ? Array.from(selectedPaths)
          : [entry.path];
        e.dataTransfer.setData("application/x-launchpad-paths", JSON.stringify(paths));
        e.dataTransfer.setData(
          "text/plain",
          paths.map(shellQuoteSingle).join(" ")
        );
        e.dataTransfer.effectAllowed = "copyMove";
      });

      // Internal drop target — only directory rows accept drops. Highlight
      // on dragover, clear on dragleave / drop. Rejects drops that would
      // move a directory into itself or one of its descendants (handled
      // inside moveFilesTo).
      if (entry.is_dir) {
        row.addEventListener("dragover", (e) => {
          if (!e.dataTransfer.types.includes("application/x-launchpad-paths")) return;
          e.preventDefault();
          e.stopPropagation();
          e.dataTransfer.dropEffect = "move";
          row.classList.add("drop-target");
        });
        row.addEventListener("dragleave", () => {
          row.classList.remove("drop-target");
        });
        row.addEventListener("drop", async (e) => {
          if (!e.dataTransfer.types.includes("application/x-launchpad-paths")) return;
          e.preventDefault();
          e.stopPropagation();
          row.classList.remove("drop-target");
          const data = e.dataTransfer.getData("application/x-launchpad-paths");
          let paths;
          try { paths = JSON.parse(data); } catch (_) { return; }
          if (!Array.isArray(paths) || paths.length === 0) return;
          await moveFilesTo(paths, entry.path);
        });
      }

      // Right-click context menu. If the user right-clicks a row that's
      // part of a multi-selection of >1, show the multi-action menu;
      // otherwise the click is treated as targeting just this row (and
      // the prior selection is replaced).
      row.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        e.stopPropagation();
        if (selectedPaths.size > 1 && selectedPaths.has(entry.path)) {
          showMultiContextMenu(e.clientX, e.clientY);
        } else {
          setSingleSelection(entry.path);
          selectionAnchor = entry.path;
          focusedPath = entry.path;
          applyRovingTabIndex();
          showContextMenu(e.clientX, e.clientY, entry);
        }
      });

      fragment.appendChild(row);

      if (entry.is_dir && expandedDirs.has(entry.path)) {
        const childContainer = document.createElement("div");
        childContainer.className = "dir-children";
        fragment.appendChild(childContainer);
        childLoadPromises.push(loadDirectory(entry.path, childContainer, depth + 1));
      }
    });

    // Wait for all expanded subdirectories to finish loading
    await Promise.all(childLoadPromises);

    // Empty-folder state — at depth 0, render a hint that an empty pane
    // isn't a render bug. At depth > 0 we leave the .dir-children
    // container empty (the parent dir's chevron tells the user there's
    // nothing inside; another row would visually dominate that signal).
    if (filtered.length === 0 && depth === 0) {
      parentEl.innerHTML = `<div class="file-empty-state">
        <div class="file-empty-title">This folder is empty</div>
        <div class="file-empty-hint">Right-click to create a file or folder.</div>
      </div>`;
      focusedPath = null;
      return;
    }

    // Snapshot is committed above; from here we always rebuild the DOM.
    // (The bail-out fast path returned early when nothing changed.)

    // Swap in the complete tree atomically — no flash of empty content
    parentEl.innerHTML = "";
    parentEl.appendChild(fragment);

    // After every top-level rebuild, re-anchor the roving tabindex so
    // exactly one row is Tab-focusable. Sub-tree loads (depth > 0) skip
    // this so we don't fight an in-progress nav during expand-while-load.
    if (depth === 0) applyRovingTabIndex();
  } catch (err) {
    parentEl.innerHTML = `<div class="file-error">Cannot read directory</div>`;
  }
}

// Returns all .file-entry rows in DOM order — i.e. visible row order, since
// we only render children of expanded dirs. Used by every nav helper.
function visibleRows() {
  const tree = document.getElementById("file-tree");
  return tree ? Array.from(tree.querySelectorAll(".file-entry")) : [];
}

// Set tabIndex=0 on exactly one row (the focused one), tabIndex=-1 on the
// rest. Also re-applies the .selected class for every row in
// `selectedPaths`, dropping any selection entries no longer in the
// visible tree. Idempotent — safe to call after any tree mutation.
function applyRovingTabIndex() {
  const rows = visibleRows();
  if (rows.length === 0) {
    focusedPath = null;
    return;
  }
  let target = rows.find((r) => r.dataset.path === focusedPath);
  if (!target) {
    target = rows[0];
    focusedPath = target.dataset.path;
  }
  // Drop selection entries that no longer have a visible row, so a stale
  // selection from a deleted file can't outlive its containing tree.
  const visiblePaths = new Set(rows.map((r) => r.dataset.path));
  for (const p of selectedPaths) {
    if (!visiblePaths.has(p)) selectedPaths.delete(p);
  }
  for (const r of rows) {
    r.tabIndex = r === target ? 0 : -1;
    r.classList.toggle("selected", selectedPaths.has(r.dataset.path));
    r.setAttribute("aria-selected", selectedPaths.has(r.dataset.path) ? "true" : "false");
  }
}

function setSingleSelection(path) {
  selectedPaths.clear();
  selectedPaths.add(path);
}

function toggleSelection(path) {
  if (selectedPaths.has(path)) selectedPaths.delete(path);
  else selectedPaths.add(path);
}

// Range-select between two paths. Order in the visible-rows list defines
// the range; we don't care which end the user clicked first.
function selectRange(fromPath, toPath) {
  const rows = visibleRows();
  const fromIdx = rows.findIndex((r) => r.dataset.path === fromPath);
  const toIdx = rows.findIndex((r) => r.dataset.path === toPath);
  if (fromIdx < 0 || toIdx < 0) return;
  const [lo, hi] = fromIdx <= toIdx ? [fromIdx, toIdx] : [toIdx, fromIdx];
  selectedPaths.clear();
  for (let i = lo; i <= hi; i++) selectedPaths.add(rows[i].dataset.path);
}

// Move keyboard focus + roving tabindex to the row at `path` (must be
// currently visible). No-op when not found. Used by every nav helper.
function focusRowByPath(path) {
  const rows = visibleRows();
  const row = rows.find((r) => r.dataset.path === path);
  if (!row) return;
  focusedPath = path;
  applyRovingTabIndex();
  row.focus();
}

// Walk backwards from the row at `index` and return the parent row — i.e.
// the first row above with a smaller aria-level. Returns null at the root.
function findParentRow(rows, index) {
  const currentLevel = parseInt(rows[index].getAttribute("aria-level"), 10);
  for (let i = index - 1; i >= 0; i--) {
    const lvl = parseInt(rows[i].getAttribute("aria-level"), 10);
    if (lvl < currentLevel) return rows[i];
  }
  return null;
}

// WAI-ARIA tree keyboard pattern. Bound on #file-tree via event delegation.
// ↑ / ↓        — move among visible rows
// → / ←        — expand / collapse dir, or jump to first child / parent
// Enter / Space — open file or toggle dir
// Home / End   — first / last visible row
// a-z, 0-9     — type-ahead first-letter match (advances on repeat)
let typeaheadBuffer = "";
let typeaheadTimer = null;
async function handleTreeKeydown(e) {
  // Don't hijack arrow keys when the user is in the rename input or any
  // other text field embedded in the tree.
  if (e.target.tagName === "INPUT" || e.target.tagName === "TEXTAREA") return;
  const rows = visibleRows();
  if (rows.length === 0) return;
  const focusedIdx = Math.max(0, rows.findIndex((r) => r.dataset.path === focusedPath));
  const row = rows[focusedIdx];
  const path = row.dataset.path;
  const isDir = row.classList.contains("dir");
  const expanded = row.getAttribute("aria-expanded") === "true";

  switch (e.key) {
    case "ArrowDown": {
      e.preventDefault();
      const next = rows[Math.min(rows.length - 1, focusedIdx + 1)];
      if (e.shiftKey) {
        // Shift+ArrowDown extends the selection from the current anchor
        // to the row we're moving to. Without Shift we collapse to a
        // single selection on the new row, matching most file managers.
        if (!selectionAnchor) selectionAnchor = path;
        selectRange(selectionAnchor, next.dataset.path);
      } else {
        setSingleSelection(next.dataset.path);
        selectionAnchor = next.dataset.path;
      }
      focusRowByPath(next.dataset.path);
      return;
    }
    case "ArrowUp": {
      e.preventDefault();
      const prev = rows[Math.max(0, focusedIdx - 1)];
      if (e.shiftKey) {
        if (!selectionAnchor) selectionAnchor = path;
        selectRange(selectionAnchor, prev.dataset.path);
      } else {
        setSingleSelection(prev.dataset.path);
        selectionAnchor = prev.dataset.path;
      }
      focusRowByPath(prev.dataset.path);
      return;
    }
    case "ArrowRight": {
      e.preventDefault();
      if (isDir && !expanded) {
        // Synthesize a click on the row's depth-aware toggle. We can't
        // call toggleDirectory directly without rebuilding the entry
        // object; emitting a click is the smallest change that keeps one
        // code path for expand.
        row.click();
      } else if (isDir && expanded) {
        // Already expanded — jump to first child if any.
        const next = rows[focusedIdx + 1];
        if (next && parseInt(next.getAttribute("aria-level"), 10) >
            parseInt(row.getAttribute("aria-level"), 10)) {
          focusRowByPath(next.dataset.path);
        }
      }
      return;
    }
    case "ArrowLeft": {
      e.preventDefault();
      if (isDir && expanded) {
        row.click(); // collapse
      } else {
        const parent = findParentRow(rows, focusedIdx);
        if (parent) focusRowByPath(parent.dataset.path);
      }
      return;
    }
    case "Enter":
    case " ": {
      e.preventDefault();
      row.click();
      return;
    }
    case "Home": {
      e.preventDefault();
      focusRowByPath(rows[0].dataset.path);
      return;
    }
    case "End": {
      e.preventDefault();
      focusRowByPath(rows[rows.length - 1].dataset.path);
      return;
    }
    case "Backspace":
    case "Delete": {
      // Quick-delete: only fires when at least one row is selected (the
      // focused row is auto-selected via single-click semantics, so this
      // is the common case). Routes through showConfirmDialog so the
      // user always sees a chance to bail out.
      e.preventDefault();
      const targets = selectedPaths.size > 0
        ? Array.from(selectedPaths)
        : [path];
      const count = targets.length;
      const label = count === 1
        ? `Delete "${targets[0].split("/").pop()}"?`
        : `Delete ${count} items?`;
      showConfirmDialog(
        `${label} This cannot be undone.`,
        async () => {
          const failures = [];
          for (const p of targets) {
            try {
              await invoke("delete_path", { path: p, projectRoot });
            } catch (err) {
              failures.push(`${p.split("/").pop()}: ${err}`);
            }
          }
          selectedPaths.clear();
          await refreshFileBrowser();
          if (failures.length) {
            alert(`${failures.length} delete(s) failed:\n` + failures.join("\n"));
          }
        },
        { confirmLabel: count === 1 ? "Delete" : `Delete ${count}`, tone: "danger" }
      );
      return;
    }
  }

  // Type-ahead: alphanumeric keys jump to the next visible row whose
  // filename starts with the buffered prefix. Buffer clears after 600ms
  // of inactivity, matching the macOS Finder feel.
  if (e.key.length === 1 && /^[a-zA-Z0-9._-]$/.test(e.key) &&
      !e.metaKey && !e.ctrlKey && !e.altKey) {
    typeaheadBuffer += e.key.toLowerCase();
    clearTimeout(typeaheadTimer);
    typeaheadTimer = setTimeout(() => { typeaheadBuffer = ""; }, 600);
    // Search starting AFTER the current row, wrap to the start.
    const findFrom = (start) => {
      for (let i = 0; i < rows.length; i++) {
        const r = rows[(start + i) % rows.length];
        const name = r.querySelector(".file-name")?.textContent || "";
        if (name.toLowerCase().startsWith(typeaheadBuffer)) return r;
      }
      return null;
    };
    // On the first letter of a new buffer, advance from the next row;
    // on subsequent letters, refine starting at the current row so a
    // match doesn't skip past itself.
    const startIdx = typeaheadBuffer.length === 1 ? focusedIdx + 1 : focusedIdx;
    const match = findFrom(startIdx);
    if (match) {
      e.preventDefault();
      focusRowByPath(match.dataset.path);
    }
  }
}

// Insert an inline editable placeholder row for a new file or folder. The
// row appears at the right depth in the tree (auto-expanding the parent
// directory if needed) and focuses an input. Enter commits, Escape /
// blur-without-Enter cancels. Replaces the previous window.prompt() flow
// for new-file / new-folder so the UX matches the inline rename editor.
//
// `targetDir` is where the new file/folder will be created on disk;
// `kind` is "file" or "folder".
async function startInlineCreate(targetDir, kind) {
  const tree = document.getElementById("file-tree");
  if (!tree) return;

  // Don't try to inline-create while search results are showing — the
  // visual coordinate system doesn't match the project tree.
  if (searchQuery) return fallbackPromptCreate(targetDir, kind);

  let container, depth;
  if (targetDir === currentPath) {
    container = tree;
    depth = 0;
  } else {
    const parentRow = tree.querySelector(
      `.file-entry[data-path="${CSS.escape(targetDir)}"]`
    );
    if (!parentRow || !parentRow.classList.contains("dir")) {
      // Parent isn't visible (collapsed ancestor, search filter, race) —
      // fall back to the prompt rather than guess. Edge case.
      return fallbackPromptCreate(targetDir, kind);
    }
    const parentDepth = parseInt(parentRow.dataset.depth || "0", 10);
    if (!expandedDirs.has(targetDir)) {
      // Auto-expand parent so the new row appears in context. Synthesize
      // the entry shape toggleDirectory needs.
      const fakeEntry = {
        path: targetDir,
        is_dir: true,
        name: targetDir.split("/").pop() || targetDir,
      };
      await toggleDirectory(fakeEntry, parentRow, parentDepth);
    }
    container = parentRow.nextElementSibling;
    if (!container || !container.classList.contains("dir-children")) {
      return fallbackPromptCreate(targetDir, kind);
    }
    depth = parentDepth + 1;
  }

  const row = document.createElement("div");
  row.className = "file-entry" + (kind === "folder" ? " dir" : "");
  row.style.paddingLeft = (12 + depth * 16) + "px";

  // Empty chevron slot for layout alignment with the rest of the tree.
  const chevron = document.createElement("span");
  chevron.className = "file-chevron";

  const icon = document.createElement("span");
  icon.className = "file-icon";
  icon.textContent = kind === "folder" ? icons.folder : icons.default;

  const nameWrap = document.createElement("span");
  nameWrap.className = "file-name";

  const input = document.createElement("input");
  input.type = "text";
  input.className = "file-rename-input";
  input.placeholder = kind === "folder" ? "New folder name…" : "New file name…";

  nameWrap.appendChild(input);
  row.appendChild(chevron);
  row.appendChild(icon);
  row.appendChild(nameWrap);
  container.insertBefore(row, container.firstChild);
  input.focus();

  const validate = () => {
    const v = input.value.trim();
    const err = isInvalidFileName(v);
    if (err && v !== "") {
      input.classList.add("invalid");
      input.title = err;
    } else {
      input.classList.remove("invalid");
      input.title = "";
    }
  };
  input.addEventListener("input", validate);

  let committed = false;
  const cancel = () => {
    if (committed) return;
    row.remove();
  };
  const commit = () => {
    if (committed) return;
    const name = input.value.trim();
    if (!name) { cancel(); return; }
    const validationErr = isInvalidFileName(name);
    if (validationErr) {
      input.classList.add("invalid");
      input.title = validationErr;
      input.focus();
      return;
    }
    committed = true;
    const path = targetDir + "/" + name;
    const cmd = kind === "folder" ? "create_directory" : "create_file";
    invoke(cmd, { path })
      .then(() => refreshFileBrowser())
      .catch((err) => {
        committed = false;
        row.remove();
        alert(`Failed to create ${kind}: ${err}`);
      });
  };

  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") { e.preventDefault(); commit(); }
    if (e.key === "Escape") { e.preventDefault(); cancel(); }
  });
  input.addEventListener("blur", () => {
    // Blur fires before keydown's commit() can flag committed=true. Defer
    // so a real Enter beats this race; only cancel if still uncommitted.
    setTimeout(() => { if (!committed) cancel(); }, 0);
  });
}

// Last-resort prompt for create when the inline path can't run — e.g.
// search results visible, or the parent dir isn't in the tree DOM.
function fallbackPromptCreate(targetDir, kind) {
  const label = kind === "folder" ? "New folder name:" : "New file name:";
  const name = window.prompt(label);
  if (!name || !name.trim()) return;
  const trimmed = name.trim();
  const err = isInvalidFileName(trimmed);
  if (err) { alert(err); return; }
  const cmd = kind === "folder" ? "create_directory" : "create_file";
  invoke(cmd, { path: targetDir + "/" + trimmed })
    .then(() => refreshFileBrowser())
    .catch((e) => alert(`Failed to create ${kind}: ${e}`));
}

// Context menu
function showContextMenu(x, y, entry) {
  closeContextMenu();

  const menu = document.createElement("div");
  menu.id = "context-menu";
  menu.className = "context-menu";

  const items = [];

  if (entry.is_dir) {
    items.push({
      label: "Open in Terminal",
      action: () => navigateToDirectory(entry.path),
    });
    items.push({
      label: "Expand Here",
      action: () => setRoot(entry.path),
    });
  } else {
    if (isPreviewable(entry.name)) {
      items.push({
        label: "Preview",
        action: () => showFilePreview(entry),
      });
    }
    // "Open in Default App" — useful for non-previewable files (PDFs,
    // images, archives) that the editor can't render. Routes to macOS
    // `open <path>`, which dispatches to the user's chosen app for the
    // file type.
    items.push({
      label: "Open in Default App",
      action: () =>
        invoke("open_in_default_app", { path: entry.path }).catch((err) =>
          alert("Failed to open: " + err)
        ),
    });
  }

  items.push({
    label: "Copy Path",
    action: () => navigator.clipboard.writeText(entry.path),
  });
  items.push({
    label: "Copy Name",
    action: () => navigator.clipboard.writeText(entry.name),
  });
  items.push({ type: "separator" });
  items.push({
    label: "Reveal in Finder",
    action: () => invoke("reveal_in_finder", { path: entry.path }),
  });

  // ── CRUD operations ────────────────────────────────────────────────────────
  const targetDir = entry.is_dir
    ? entry.path
    : entry.path.substring(0, entry.path.lastIndexOf("/"));

  items.push({ type: "separator" });
  items.push({
    label: "New File",
    action: () => startInlineCreate(targetDir, "file"),
  });
  items.push({
    label: "New Folder",
    action: () => startInlineCreate(targetDir, "folder"),
  });
  items.push({ type: "separator" });
  items.push({
    label: "Rename",
    action: () => {
      // Find the name element in the already-rendered row and replace with inline input
      const nameEl = document.querySelector(
        `[data-path="${CSS.escape(entry.path)}"] .file-name`
      );
      if (!nameEl) {
        // Fallback to prompt if DOM element not found
        const newName = window.prompt("Rename to:", entry.name);
        if (!newName || !newName.trim() || newName.trim() === entry.name) return;
        const trimmed = newName.trim();
        const err = isInvalidFileName(trimmed);
        if (err) { alert(err); return; }
        const parentDir = entry.path.substring(0, entry.path.lastIndexOf("/"));
        invoke("rename_path", { oldPath: entry.path, newPath: parentDir + "/" + trimmed })
          .then(() => refreshFileBrowser())
          .catch((err) => alert("Failed to rename: " + err));
        return;
      }
      const originalText = nameEl.textContent;
      const input = document.createElement("input");
      input.type = "text";
      input.className = "file-rename-input";
      input.value = entry.name;
      nameEl.textContent = "";
      nameEl.appendChild(input);
      input.focus();
      input.select();

      // Live validation: red border + tooltip while the input is invalid,
      // so the user sees the constraint instead of getting a useless OS
      // error after pressing Enter.
      const validate = () => {
        const v = input.value.trim();
        const err = isInvalidFileName(v);
        if (err && v !== "" && v !== entry.name) {
          input.classList.add("invalid");
          input.title = err;
        } else {
          input.classList.remove("invalid");
          input.title = "";
        }
      };
      input.addEventListener("input", validate);

      let committed = false;
      const commit = () => {
        if (committed) return;
        const newName = input.value.trim();
        if (!newName || newName === entry.name) {
          nameEl.textContent = originalText;
          return;
        }
        const validationErr = isInvalidFileName(newName);
        if (validationErr) {
          // Don't dismiss the input — let the user fix it.
          input.classList.add("invalid");
          input.title = validationErr;
          input.focus();
          return;
        }
        committed = true;
        const parentDir = entry.path.substring(0, entry.path.lastIndexOf("/"));
        const newPath = parentDir + "/" + newName;
        invoke("rename_path", { oldPath: entry.path, newPath })
          .then(() => {
            // Announce to any interested module (main.js updates open
            // editor tabs pointing at the old path, so Cmd+S doesn't
            // silently write to the stale filename).
            window.dispatchEvent(
              new CustomEvent(PATH_RENAMED, {
                detail: { oldPath: entry.path, newPath, isDir: entry.is_dir },
              })
            );
            refreshFileBrowser();
          })
          .catch((err) => {
            committed = false;
            alert("Failed to rename: " + err);
            nameEl.textContent = originalText;
          });
      };

      input.addEventListener("keydown", (e) => {
        if (e.key === "Enter") { e.preventDefault(); commit(); }
        if (e.key === "Escape") { e.preventDefault(); nameEl.textContent = originalText; }
      });
      input.addEventListener("blur", () => {
        if (!committed) nameEl.textContent = originalText;
      });
    },
  });
  items.push({
    label: "Delete",
    action: () => {
      const kind = entry.is_dir ? "folder" : "file";
      showConfirmDialog(
        `Delete ${kind} "${entry.name}"? This cannot be undone.`,
        () => {
          invoke("delete_path", { path: entry.path, projectRoot })
            .then(() => refreshFileBrowser())
            .catch((err) => alert("Failed to delete: " + err));
        },
        { confirmLabel: "Delete", tone: "danger" }
      );
    },
  });

  items.forEach((item) => {
    if (item.type === "separator") {
      const sep = document.createElement("div");
      sep.className = "context-menu-separator";
      menu.appendChild(sep);
    } else {
      const row = document.createElement("div");
      row.className = "context-menu-item";
      row.textContent = item.label;
      row.addEventListener("click", () => {
        item.action();
        closeContextMenu();
      });
      menu.appendChild(row);
    }
  });

  // Position menu, keeping it in viewport
  menu.style.left = x + "px";
  menu.style.top = y + "px";
  document.body.appendChild(menu);

  // Adjust if off-screen
  const rect = menu.getBoundingClientRect();
  if (rect.right > window.innerWidth) {
    menu.style.left = (x - rect.width) + "px";
  }
  if (rect.bottom > window.innerHeight) {
    menu.style.top = (y - rect.height) + "px";
  }

  // Close on click outside
  setTimeout(() => {
    document.addEventListener("click", closeContextMenu, { once: true });
    document.addEventListener("contextmenu", closeContextMenu, { once: true });
  }, 0);
}

function closeContextMenu() {
  const existing = document.getElementById("context-menu");
  if (existing) existing.remove();
}

// Multi-selection context menu — fires when the user right-clicks a row
// that's part of a selection of >1. Constrains the menu to actions that
// make sense across heterogeneous selections (Delete, Copy Paths). Single-
// row CRUD (rename, preview) is intentionally absent — those need a
// single target, and the per-row menu handles them.
function showMultiContextMenu(x, y) {
  closeContextMenu();
  const menu = document.createElement("div");
  menu.id = "context-menu";
  menu.className = "context-menu";

  const count = selectedPaths.size;
  const paths = Array.from(selectedPaths);

  const items = [
    {
      label: `Copy ${count} Paths`,
      action: () => navigator.clipboard.writeText(paths.join("\n")),
    },
    { type: "separator" },
    {
      label: `Delete ${count} Items`,
      action: () => {
        showConfirmDialog(
          `Delete ${count} items? This cannot be undone.`,
          async () => {
            // Sequential delete — running in parallel could blow up the
            // git index lock or hit `delete_path`'s project-root guard
            // races. Per-file failure is surfaced; survivors stay deleted.
            const failures = [];
            for (const p of paths) {
              try {
                await invoke("delete_path", { path: p, projectRoot });
              } catch (err) {
                failures.push(`${p.split("/").pop()}: ${err}`);
              }
            }
            selectedPaths.clear();
            await refreshFileBrowser();
            if (failures.length) {
              alert(`${failures.length} delete(s) failed:\n` + failures.join("\n"));
            }
          },
          { confirmLabel: `Delete ${count}`, tone: "danger" }
        );
      },
    },
  ];

  items.forEach((item) => {
    if (item.type === "separator") {
      const sep = document.createElement("div");
      sep.className = "context-menu-separator";
      menu.appendChild(sep);
    } else {
      const r = document.createElement("div");
      r.className = "context-menu-item";
      r.textContent = item.label;
      r.addEventListener("click", () => {
        item.action();
        closeContextMenu();
      });
      menu.appendChild(r);
    }
  });

  menu.style.left = x + "px";
  menu.style.top = y + "px";
  document.body.appendChild(menu);

  const rect = menu.getBoundingClientRect();
  if (rect.right > window.innerWidth) menu.style.left = (x - rect.width) + "px";
  if (rect.bottom > window.innerHeight) menu.style.top = (y - rect.height) + "px";

  setTimeout(() => {
    document.addEventListener("click", closeContextMenu, { once: true });
    document.addEventListener("contextmenu", closeContextMenu, { once: true });
  }, 0);
}

// Build and show a context menu for the file tree's empty space — i.e. a
// right-click that lands outside any row. Lets the user create a file or
// folder at `currentPath` even when the directory is empty (otherwise the
// only entry point is per-row, and an empty project has no rows to
// right-click). Mirrors the New File / New Folder behavior of the
// per-row menu so the two stay consistent.
function showEmptyAreaContextMenu(x, y) {
  closeContextMenu();
  const menu = document.createElement("div");
  menu.id = "context-menu";
  menu.className = "context-menu";

  const items = [
    {
      label: "New File",
      action: () => startInlineCreate(currentPath, "file"),
    },
    {
      label: "New Folder",
      action: () => startInlineCreate(currentPath, "folder"),
    },
    { type: "separator" },
    {
      label: "Reveal in Finder",
      action: () => invoke("reveal_in_finder", { path: currentPath }),
    },
  ];

  items.forEach((item) => {
    if (item.type === "separator") {
      const sep = document.createElement("div");
      sep.className = "context-menu-separator";
      menu.appendChild(sep);
    } else {
      const row = document.createElement("div");
      row.className = "context-menu-item";
      row.textContent = item.label;
      row.addEventListener("click", () => {
        item.action();
        closeContextMenu();
      });
      menu.appendChild(row);
    }
  });

  menu.style.left = x + "px";
  menu.style.top = y + "px";
  document.body.appendChild(menu);

  const rect = menu.getBoundingClientRect();
  if (rect.right > window.innerWidth) menu.style.left = (x - rect.width) + "px";
  if (rect.bottom > window.innerHeight) menu.style.top = (y - rect.height) + "px";

  setTimeout(() => {
    document.addEventListener("click", closeContextMenu, { once: true });
    document.addEventListener("contextmenu", closeContextMenu, { once: true });
  }, 0);
}

// File editor/preview
async function showFilePreview(entry) {
  // If we have an editor tab callback, use that instead of the overlay
  if (openFileCallback) {
    openFileCallback(entry.path);
    return;
  }
}

export function closeFilePreview() {
  const preview = document.getElementById("file-preview");
  if (preview) preview.classList.remove("visible");
  popEscape(closeFilePreview);
}

// Show diff in read-only preview mode (used by git panel)
export function showDiffPreview(fileName, diffHtml) {
  const preview = document.getElementById("file-preview");
  const previewName = document.getElementById("preview-file-name");
  const previewContent = document.getElementById("preview-content");
  const editorContainer = document.getElementById("preview-editor");
  const saveBtn = document.getElementById("preview-save");
  const status = document.getElementById("preview-status");

  previewName.textContent = `Diff: ${fileName}`;
  editorContainer.style.display = "none";
  previewContent.style.display = "block";
  previewContent.innerHTML = diffHtml;
  saveBtn.style.display = "none";
  status.textContent = "";

  preview.classList.add("visible");
  pushEscape(closeFilePreview);
}

async function toggleDirectory(entry, row, depth) {
  if (expandedDirs.has(entry.path)) {
    expandedDirs.delete(entry.path);
    row.querySelector(".file-icon").textContent = icons.folder;
    const chev = row.querySelector(".file-chevron");
    if (chev) chev.textContent = "▸";
    row.setAttribute("aria-expanded", "false");
    const next = row.nextElementSibling;
    if (next && next.classList.contains("dir-children")) {
      next.remove();
    }
  } else {
    expandedDirs.add(entry.path);
    row.querySelector(".file-icon").textContent = icons.folderOpen;
    const chev = row.querySelector(".file-chevron");
    if (chev) chev.textContent = "▾";
    row.setAttribute("aria-expanded", "true");
    const childContainer = document.createElement("div");
    childContainer.className = "dir-children";
    row.after(childContainer);
    await loadDirectory(entry.path, childContainer, depth + 1);
  }
  // Visible-row set just changed (children appeared / disappeared) — re-
  // anchor the roving tabindex so Tab still lands on the right row.
  applyRovingTabIndex();
}

function navigateToDirectory(path) {
  setRoot(path);
}

// Move each src path into destDir. Skips no-op moves (already in destDir),
// rejects moves of a directory into itself or any descendant, and surfaces
// per-file failures as a single aggregated alert at the end. Always
// refreshes the tree at the end whether or not anything moved, so a
// cancelled drag (zero successful moves) still clears any leftover
// drop-target classes via the standard tree rebuild.
async function moveFilesTo(paths, destDir) {
  // Defensive: caller may have given us paths from a stale drag — drop
  // any that no longer exist (the rename_path call would error anyway,
  // but pre-filtering produces a cleaner failure list).
  const failures = [];
  let moved = 0;
  for (const src of paths) {
    // Reject self-move and descendant-move. `destDir.startsWith(src + "/")`
    // catches "drag /a/b into /a/b/c". `src === destDir` catches "drag
    // /a/b into /a/b". Plain self-parent move (already a child of
    // destDir) is a silent no-op — the user dragged but didn't move.
    if (src === destDir || destDir === src + "/" ||
        destDir.startsWith(src + "/")) {
      failures.push(`${src.split("/").pop()}: cannot move into itself`);
      continue;
    }
    const name = src.split("/").pop();
    const dest = destDir.replace(/\/+$/, "") + "/" + name;
    if (src === dest) continue; // already there — silent no-op
    try {
      await invoke("rename_path", { oldPath: src, newPath: dest });
      moved += 1;
    } catch (err) {
      failures.push(`${name}: ${err}`);
    }
  }
  selectedPaths.clear();
  await refreshFileBrowser();
  if (failures.length) {
    alert(`${failures.length} move(s) failed:\n` + failures.join("\n"));
  }
  return moved;
}

// Project-wide flat search results, replacing the tree view while
// `searchQuery` is non-empty. Routes through the existing `search_files`
// Tauri command (same skip rules as Cmd+P), capped at 200 hits — beyond
// that the user should refine their query rather than paginate.
async function renderSearchResults(query) {
  const tree = document.getElementById("file-tree");
  if (!tree) return;
  tree.innerHTML = '<div class="skeleton-rows">' +
    '<div class="skeleton-row"></div>'.repeat(5) + '</div>';
  let results;
  try {
    results = await invoke("search_files", {
      root: projectRoot,
      query,
      maxResults: 200,
    });
  } catch (err) {
    tree.innerHTML = `<div class="file-error">Search failed: ${err}</div>`;
    return;
  }

  if (!Array.isArray(results) || results.length === 0) {
    tree.innerHTML = `<div class="file-error">No matches for "${query}"</div>`;
    focusedPath = null;
    return;
  }

  const fragment = document.createDocumentFragment();
  results.forEach((relativePath, idx) => {
    // search_files returns paths relative to projectRoot; rebuild absolute.
    const absolutePath = projectRoot.replace(/\/+$/, "") + "/" + relativePath;
    const lastSlash = relativePath.lastIndexOf("/");
    const fileName = lastSlash === -1 ? relativePath : relativePath.slice(lastSlash + 1);
    const dirName = lastSlash === -1 ? "" : relativePath.slice(0, lastSlash);

    const row = document.createElement("div");
    row.className = getFileClass({ is_dir: false, name: fileName });
    row.style.paddingLeft = "12px";
    row.dataset.path = absolutePath;
    row.setAttribute("role", "treeitem");
    row.tabIndex = -1;
    row.setAttribute("aria-level", "1");
    row.setAttribute("aria-setsize", String(results.length));
    row.setAttribute("aria-posinset", String(idx + 1));
    row.title = relativePath;

    // Empty chevron slot keeps icon/name columns aligned with the tree view.
    const chevron = document.createElement("span");
    chevron.className = "file-chevron";

    const icon = document.createElement("span");
    icon.className = "file-icon";
    icon.textContent = getIcon({ is_dir: false, name: fileName });

    const name = document.createElement("span");
    name.className = "file-name";
    name.textContent = fileName;

    const badge = document.createElement("span");
    badge.className = "file-git-badge";

    row.appendChild(chevron);
    row.appendChild(icon);
    row.appendChild(name);
    row.appendChild(badge);

    if (dirName) {
      const subtitle = document.createElement("span");
      subtitle.className = "file-search-path";
      subtitle.textContent = dirName;
      row.appendChild(subtitle);
    }

    row.addEventListener("click", (e) => {
      e.stopPropagation();
      focusedPath = absolutePath;
      applyRovingTabIndex();
      if (openFileCallback) openFileCallback(absolutePath);
    });

    row.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
      // Synthesize a minimal entry shape for the existing context menu.
      showContextMenu(e.clientX, e.clientY, {
        path: absolutePath,
        name: fileName,
        is_dir: false,
      });
    });

    fragment.appendChild(row);
  });

  tree.innerHTML = "";
  tree.appendChild(fragment);
  applyRovingTabIndex();
}

function isWithinProject(path) {
  if (!projectRoot) return true;
  const p = path.replace(/\/+$/, "");
  const r = projectRoot.replace(/\/+$/, "");
  return p === r || p.startsWith(r + "/");
}

async function setRoot(path) {
  // Defensive: never let a caller steer the file browser outside the project.
  if (!isWithinProject(path)) return;
  currentPath = path;
  expandedDirs.clear();
  // New root → previous focusedPath is meaningless. Let applyRovingTabIndex
  // pick the first row of the new tree.
  focusedPath = null;
  // Drop the snapshot for #file-tree so a same-shaped listing in the new
  // root can't accidentally take the diff-and-patch fast path against
  // the old root's signature.
  const tree0 = document.getElementById("file-tree");
  if (tree0) directorySnapshots.delete(tree0);

  const pathDisplay = document.getElementById("current-path");
  pathDisplay.textContent = shortenPath(path);
  pathDisplay.title = path;

  const tree = document.getElementById("file-tree");
  await loadDirectory(path, tree);
}

function toggleHidden() {
  showHidden = !showHidden;
  const btn = document.getElementById("toggle-hidden");
  btn.classList.toggle("active", showHidden);
  const tree = document.getElementById("file-tree");
  loadDirectory(currentPath, tree);
}

async function navigateUp() {
  if (currentPath === projectRoot) return; // already at project root
  const parent = currentPath.split("/").slice(0, -1).join("/") || "/";
  if (!isWithinProject(parent)) return; // would escape project — no-op
  navigateToDirectory(parent);
}

export async function initFileBrowser(activeTabIdGetter, openFileCb, projectRootPath) {
  getActiveTabId = activeTabIdGetter;
  openFileCallback = openFileCb || null;

  try {
    const home = await invoke("get_home_dir");
    homeDir = home;
    projectRoot = projectRootPath || home;
    await setRoot(projectRoot);
  } catch (err) {
    console.error("Failed to init file browser:", err);
  }

  const upBtn = document.getElementById("nav-up");
  if (upBtn) upBtn.addEventListener("click", navigateUp);

  const goHomeBtn = document.getElementById("go-home-btn");
  if (goHomeBtn) goHomeBtn.addEventListener("click", () => setRoot(projectRoot));

  const hiddenBtn = document.getElementById("toggle-hidden");
  if (hiddenBtn) hiddenBtn.addEventListener("click", toggleHidden);

  // Collapse-all: drop every expansion record and reload the tree at the
  // current root. The 90% case for this button is "I expanded a bunch of
  // folders chasing something and now I want a clean view." A symmetric
  // expand-all is intentionally absent — recursively expanding a project
  // root with thousands of dirs would freeze the renderer; clicking
  // chevrons one at a time is the safer ergonomics for going deep.
  const collapseAllBtn = document.getElementById("collapse-all");
  if (collapseAllBtn) collapseAllBtn.addEventListener("click", () => {
    expandedDirs.clear();
    selectedPaths.clear();
    refreshFileBrowser();
  });

  const previewCloseBtn = document.getElementById("preview-close");
  if (previewCloseBtn) previewCloseBtn.addEventListener("click", closeFilePreview);

  // Empty-space context menu. Per-row contextmenu handlers stopPropagation,
  // so this listener only fires when the right-click misses every row —
  // exactly the case where an empty project has nothing to right-click.
  const tree = document.getElementById("file-tree");
  if (tree) {
    tree.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      showEmptyAreaContextMenu(e.clientX, e.clientY);
    });
    // Keyboard navigation. Single delegated listener on the tree —
    // individual rows already have tabIndex set up by applyRovingTabIndex,
    // so keydown bubbles up to here regardless of which row has focus.
    tree.addEventListener("keydown", handleTreeKeydown);
    // Tree-level drop target so a drag can be released onto empty space
    // (or any non-row area) to move the file(s) into the current pane
    // root. Per-row drop handlers stopPropagation so this only fires
    // when the drop missed every row.
    tree.addEventListener("dragover", (e) => {
      if (!e.dataTransfer.types.includes("application/x-launchpad-paths")) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "move";
    });
    tree.addEventListener("drop", async (e) => {
      if (!e.dataTransfer.types.includes("application/x-launchpad-paths")) return;
      e.preventDefault();
      const data = e.dataTransfer.getData("application/x-launchpad-paths");
      let paths;
      try { paths = JSON.parse(data); } catch (_) { return; }
      if (!Array.isArray(paths) || paths.length === 0) return;
      await moveFilesTo(paths, currentPath);
    });
  }

  // File search
  const searchInput = document.getElementById("file-search-input");
  const searchBox = document.getElementById("file-search");
  searchBox.style.display = "none";

  searchInput.addEventListener("input", () => {
    clearTimeout(searchDebounce);
    searchDebounce = setTimeout(() => {
      searchQuery = searchInput.value.trim();
      const tree = document.getElementById("file-tree");
      // Branch: empty query → tree view; non-empty → flat project-wide
      // results via search_files. Avoids the previous bug where typing
      // a query only filtered the current level and silently hid every
      // nested match.
      if (searchQuery) {
        renderSearchResults(searchQuery);
      } else {
        loadDirectory(currentPath, tree);
      }
    }, 150);
  });

  searchInput.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      closeSearch();
    }
    e.stopPropagation();
  });

  // Cmd+F to open sidebar file search (only when not in an editor tab)
  document.addEventListener("keydown", (e) => {
    if (e.metaKey && e.key === "f") {
      // Don't hijack Cmd+F when a CodeMirror editor is active
      if (document.activeElement?.closest(".cm-editor")) return;
      e.preventDefault();
      searchBox.style.display = "block";
      searchInput.focus();
      searchInput.select();
      pushEscape(closeSearch);
    }
  });

  function closeSearch() {
    searchQuery = "";
    searchInput.value = "";
    searchBox.style.display = "none";
    const tree = document.getElementById("file-tree");
    loadDirectory(currentPath, tree);
    popEscape(closeSearch);
  }
}

export function getCurrentPath() {
  return currentPath;
}

export function openFileByPath(fullPath) {
  if (openFileCallback) {
    openFileCallback(fullPath);
  } else {
    const name = fullPath.split("/").pop();
    showFilePreview({ path: fullPath, name });
  }
}

// Coalesce concurrent refresh requests: if a second call arrives while the
// first is in flight, don't drop it — set a dirty flag and re-run once
// after the current load finishes. A rapid `git pull` can emit several
// fs-changed events within one load cycle; dropping them used to leave
// the tree stale with no scheduled catch-up.
let refreshInFlight = false;
let refreshDirty = false;
export async function refreshFileBrowser() {
  if (!currentPath) return;
  if (refreshInFlight) {
    refreshDirty = true;
    return;
  }
  refreshInFlight = true;
  try {
    const tree = document.getElementById("file-tree");
    do {
      refreshDirty = false;
      // Same search-vs-tree branch as the input handler — when a user has
      // typed a query, fs-changed shouldn't redraw the tree underneath
      // their search results.
      if (searchQuery) {
        await renderSearchResults(searchQuery);
      } else {
        await loadDirectory(currentPath, tree);
      }
    } while (refreshDirty);
  } finally {
    refreshInFlight = false;
  }
}
