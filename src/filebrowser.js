const { invoke } = window.__TAURI__.core;

let currentPath = "";
let showHidden = false;
let expandedDirs = new Set();
let onNavigateCallback = null;
let getActiveTabId = null; // set by main.js
let openFileCallback = null; // set by main.js — opens file as editor tab
let searchQuery = "";
let searchDebounce = null;

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

function shortenPath(path) {
  const home = currentPath.split("/").slice(0, 3).join("/");
  if (path.startsWith(home)) {
    return "~" + path.slice(home.length);
  }
  return path;
}

function isPreviewable(name) {
  const ext = name.split(".").pop().toLowerCase();
  // Also preview dotfiles like .gitignore, .env
  if (name.startsWith(".") && !name.includes("/")) return true;
  return previewExts.has(ext);
}

async function loadDirectory(path, parentEl, depth = 0) {
  try {
    const entries = await invoke("read_directory", { path });
    let filtered = showHidden
      ? entries
      : entries.filter((e) => !e.is_hidden);

    // Apply search filter
    if (searchQuery) {
      const q = searchQuery.toLowerCase();
      filtered = filtered.filter(
        (e) => e.name.toLowerCase().includes(q) || e.is_dir
      );
    }

    parentEl.innerHTML = "";

    filtered.forEach((entry) => {
      const row = document.createElement("div");
      row.className = getFileClass(entry);
      row.style.paddingLeft = (12 + depth * 16) + "px";
      row.dataset.path = entry.path;

      const icon = document.createElement("span");
      icon.className = "file-icon";
      icon.textContent = expandedDirs.has(entry.path)
        ? icons.folderOpen
        : getIcon(entry);

      const name = document.createElement("span");
      name.className = "file-name";
      name.textContent = entry.name;

      row.appendChild(icon);
      row.appendChild(name);

      if (!entry.is_dir) {
        const size = document.createElement("span");
        size.className = "file-size";
        size.textContent = formatSize(entry.size);
        row.appendChild(size);
      }

      // Click handler
      row.addEventListener("click", async (e) => {
        e.stopPropagation();
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

      // Drag to terminal
      row.draggable = true;
      row.addEventListener("dragstart", (e) => {
        e.dataTransfer.setData("text/plain", entry.path);
        e.dataTransfer.effectAllowed = "copy";
      });

      // Right-click context menu
      row.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        e.stopPropagation();
        showContextMenu(e.clientX, e.clientY, entry);
      });

      parentEl.appendChild(row);

      if (entry.is_dir && expandedDirs.has(entry.path)) {
        const childContainer = document.createElement("div");
        childContainer.className = "dir-children";
        parentEl.appendChild(childContainer);
        loadDirectory(entry.path, childContainer, depth + 1);
      }
    });
  } catch (err) {
    parentEl.innerHTML = `<div class="file-error">Cannot read directory</div>`;
  }
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

// File editor/preview
let currentEditPath = null;
let originalContent = "";

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
  currentEditPath = null;
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
  currentEditPath = null;

  preview.classList.add("visible");
}

async function toggleDirectory(entry, row, depth) {
  if (expandedDirs.has(entry.path)) {
    expandedDirs.delete(entry.path);
    const icon = row.querySelector(".file-icon");
    icon.textContent = icons.folder;
    const next = row.nextElementSibling;
    if (next && next.classList.contains("dir-children")) {
      next.remove();
    }
  } else {
    expandedDirs.add(entry.path);
    const icon = row.querySelector(".file-icon");
    icon.textContent = icons.folderOpen;
    const childContainer = document.createElement("div");
    childContainer.className = "dir-children";
    row.after(childContainer);
    await loadDirectory(entry.path, childContainer, depth + 1);
  }
}

function navigateToDirectory(path) {
  // Send cd command to active terminal tab
  const tabId = getActiveTabId ? getActiveTabId() : null;
  if (tabId !== null) {
    invoke("write_to_pty", { tabId, data: `cd "${path}"\r` });
  }
  setRoot(path);
}

async function setRoot(path) {
  currentPath = path;
  expandedDirs.clear();

  const pathDisplay = document.getElementById("current-path");
  pathDisplay.textContent = shortenPath(path);
  pathDisplay.title = path;

  const tree = document.getElementById("file-tree");
  await loadDirectory(path, tree);

  if (onNavigateCallback) onNavigateCallback(path);
}

function toggleHidden() {
  showHidden = !showHidden;
  const btn = document.getElementById("toggle-hidden");
  btn.classList.toggle("active", showHidden);
  const tree = document.getElementById("file-tree");
  loadDirectory(currentPath, tree);
}

async function navigateUp() {
  const parent = currentPath.split("/").slice(0, -1).join("/") || "/";
  navigateToDirectory(parent);
}

export async function initFileBrowser(activeTabIdGetter, openFileCb, defaultDirectory) {
  getActiveTabId = activeTabIdGetter;
  openFileCallback = openFileCb || null;

  try {
    const home = await invoke("get_home_dir");

    // Use configured default directory, or fall back to ~/Code, then ~
    const startDir = defaultDirectory || (home + "/Code");
    try {
      await invoke("read_directory", { path: startDir });
      await setRoot(startDir);
    } catch {
      await setRoot(home);
    }
  } catch (err) {
    console.error("Failed to init file browser:", err);
  }

  const upBtn = document.getElementById("nav-up");
  if (upBtn) upBtn.addEventListener("click", navigateUp);

  const hiddenBtn = document.getElementById("toggle-hidden");
  if (hiddenBtn) hiddenBtn.addEventListener("click", toggleHidden);

  // File search
  const searchInput = document.getElementById("file-search-input");
  const searchBox = document.getElementById("file-search");
  searchBox.style.display = "none";

  searchInput.addEventListener("input", () => {
    clearTimeout(searchDebounce);
    searchDebounce = setTimeout(() => {
      searchQuery = searchInput.value.trim();
      const tree = document.getElementById("file-tree");
      loadDirectory(currentPath, tree);
    }, 150);
  });

  searchInput.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      searchQuery = "";
      searchInput.value = "";
      searchBox.style.display = "none";
      const tree = document.getElementById("file-tree");
      loadDirectory(currentPath, tree);
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
    }
  });
}

export function getCurrentPath() {
  return currentPath;
}

export function onNavigate(callback) {
  onNavigateCallback = callback;
}

export function openFileByPath(fullPath) {
  if (openFileCallback) {
    openFileCallback(fullPath);
  } else {
    const name = fullPath.split("/").pop();
    showFilePreview({ path: fullPath, name });
  }
}

export function refreshFileBrowser() {
  if (currentPath) {
    const tree = document.getElementById("file-tree");
    loadDirectory(currentPath, tree);
  }
}
