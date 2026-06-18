import { createEditor, getLangName } from "./editor.js";
import { lspExtensionForFile } from "./lspclient.js";
import { parseConflictBlocks } from "./conflictmarkers.js";
import { getGitFileStatus, toRepoRelativePath } from "./git.js";
import { getSettings } from "./settings.js";
import { getResolvedTheme } from "./theme.js";
import { getOverrides } from "./filesettings.js";
import { getActiveProject } from "./projects.js";
import { refreshPanel } from "./gitpanel.js";
import { showToast } from "./toast.js";
import { createMergeTab } from "./mergetab.js";
import {
  renderBreadcrumb, refreshEditorGutter, findHunkForLine,
  revertHunkInBuffer, renderEditorStatus, gotoLine,
} from "./editorchrome.js";
import {
  tabs, nextTabId, switchTab, renderTabBar, uiIdForTab,
} from "./tabs.js";

const { invoke } = window.__TAURI__.core;

// Popover shown when a change-gutter marker is clicked: revert the hunk in the
// buffer (HEAD content, then the user saves), or stage the whole file.
function showHunkMenu(tab, view, lineNo, event) {
  document.getElementById("context-menu")?.remove();
  const hunk = findHunkForLine(tab.lineDiff, lineNo);
  if (!hunk) return;

  const menu = document.createElement("div");
  menu.id = "context-menu";
  menu.className = "context-menu";

  const addItem = (label, fn, disabled = false) => {
    const item = document.createElement("div");
    item.className = "context-menu-item" + (disabled ? " context-menu-item-disabled" : "");
    item.textContent = label;
    if (!disabled) {
      item.addEventListener("click", () => {
        menu.remove();
        fn();
      });
    }
    menu.appendChild(item);
  };

  // Revert's line math needs the buffer to match disk; gate it when dirty.
  if (tab.modified) {
    addItem("Revert hunk (save first)", null, true);
  } else {
    addItem("Revert hunk", () => {
      // Buffer edit only — leaves the tab dirty so the user can review and
      // Cmd+Z, then Cmd+S to persist (gutter recomputes on save).
      if (!revertHunkInBuffer(view, hunk)) {
        showToast("Couldn't revert — the diff is stale, try reopening", "error");
      } else {
        view.focus();
      }
    });
  }

  addItem("Stage file", async () => {
    const project = getActiveProject();
    if (!project) return;
    const uiId = uiIdForTab(tab);
    if (tab.modified && uiId != null) await saveEditorTab(uiId);
    const rel = toRepoRelativePath(tab.filePath);
    if (!rel) return;
    try {
      await invoke("git_stage_file", { path: project.path, filePath: rel });
      showToast(`Staged ${tab.fileName}`, "info");
      refreshPanel(null, true);
      refreshEditorGutter(tab);
    } catch (err) {
      showToast(`Stage failed: ${err}`, "error");
    }
  });

  menu.style.left = event.clientX + "px";
  menu.style.top = event.clientY + "px";
  document.body.appendChild(menu);
  // Clamp into the viewport if it would overflow the right/bottom edge.
  const rect = menu.getBoundingClientRect();
  if (rect.right > window.innerWidth) menu.style.left = `${window.innerWidth - rect.width - 8}px`;
  if (rect.bottom > window.innerHeight) menu.style.top = `${window.innerHeight - rect.height - 8}px`;

  setTimeout(() => {
    document.addEventListener("click", () => menu.remove(), { once: true });
  }, 0);
}

export async function createEditorTab(filePath, options = {}) {
  const { line, column } = options;
  // Deduplicate: if already open, switch to it
  for (const [uiId, tab] of tabs) {
    if (tab.type === "editor" && tab.filePath === filePath) {
      switchTab(uiId);
      if (line) requestAnimationFrame(() => gotoLine(tab.editorView, line, column));
      return uiId;
    }
  }

  const fileName = filePath.split("/").pop();
  let content;
  try {
    content = await invoke("read_file_preview", { path: filePath, maxBytes: 512000 });
  } catch (err) {
    console.error("Failed to read file:", err);
    // Previously this was a silent no-op: user clicks a file, nothing
    // opens, no feedback. Surface the Rust error (binary detection,
    // permission denied, missing path) via a toast.
    showToast(`Could not open ${fileName}: ${err}`, "error");
    return null;
  }
  // Capture inode so we can follow an external rename. Unix rename
  // preserves inode, so searching the project tree for this number
  // later lets us relocate the tab to the file's new path.
  // Best-effort: a failure here just disables the rename-detection
  // feature for this tab, not the open itself.
  let inode = null;
  try {
    inode = await invoke("get_file_inode", { path: filePath });
  } catch (_) {}

  const uiId = nextTabId();

  const containerEl = document.createElement("div");
  containerEl.className = "editor-instance";
  document.getElementById("terminal-instances").appendChild(containerEl);

  // Breadcrumb
  const breadcrumb = document.createElement("div");
  breadcrumb.className = "editor-breadcrumb";
  renderBreadcrumb(breadcrumb, filePath);
  containerEl.appendChild(breadcrumb);

  // Editor content area
  const editorContent = document.createElement("div");
  editorContent.className = "editor-content";
  containerEl.appendChild(editorContent);

  // Status bar
  const statusBar = document.createElement("div");
  statusBar.className = "editor-statusbar";
  statusBar.textContent = `Ln 1, Col 1 · ${getLangName(fileName)}`;
  containerEl.appendChild(statusBar);

  const tab = {
    type: "editor",
    containerEl,
    filePath,
    fileName,
    inode,
    editorView: null,
    originalContent: content,
    modified: false,
  };

  const editorSettings = getSettings();
  const overrides = getOverrides(filePath) || {};
  const effectiveTabSize = overrides.tabSize ?? editorSettings.editorTabSize;
  const effectiveWordWrap = overrides.wordWrap ?? editorSettings.editorWordWrap;

  // Detect line endings from the raw content — preserve what the file already uses.
  const lineEndings = overrides.lineEndings || (content.includes("\r\n") ? "CRLF" : "LF");

  tab.tabSize = effectiveTabSize;
  tab.wordWrap = effectiveWordWrap;
  tab.lineEndings = lineEndings;

  const updateStatus = () => renderEditorStatus(tab, statusBar);

  // Conflict mode opts the editor into the inline action bar + tinted
  // backgrounds. Decision is at open-time only; if a file becomes (or stops
  // being) a conflict afterward the user closes and reopens — keeps the
  // wiring simple.
  const conflictMode = getGitFileStatus(filePath) === "conflict";

  const editorHandle = createEditor(editorContent, content, fileName, {
    onChange: (newContent) => {
      tab.modified = newContent !== tab.originalContent;
      renderTabBar();
      updateStatus();
    },
    onCursorChange: (line, col) => {
      tab.cursor = { line, col };
      updateStatus();
    },
    tabSize: effectiveTabSize,
    wordWrap: effectiveWordWrap,
    fontSize: editorSettings.editorFontSize,
    vimMode: editorSettings.editorVimMode,
    theme: getResolvedTheme(),
    conflictMode,
    gitGutter: true,
    onGutterMarkerClick: (view, lineNo, event) => showHunkMenu(tab, view, lineNo, event),
    visualExtras: { indentGuides: editorSettings.editorIndentGuides },
    // Phase 6: when a conflict block's [Open 3-Way] button is clicked, we
    // open the dedicated 3-pane merge tab for this file. Only wired when
    // the file is actually conflicted — otherwise the button never renders.
    onOpenThreeWay: conflictMode ? () => createMergeTab({ filePath }) : undefined,
  });
  tab.conflictMode = conflictMode;

  const editorView = editorHandle.view;
  tab.editorView = editorView;
  tab.editorHandle = editorHandle;
  tabs.set(uiId, tab);
  updateStatus();

  // Paint the change gutter from the working-tree-vs-HEAD diff.
  refreshEditorGutter(tab);

  // Wire language-server support (opt-in): diagnostics, completion, hover,
  // go-to-def, etc. Async — the server connects in the background and features
  // light up once it does. Guarded so a closed/replaced tab doesn't get a late
  // reconfigure.
  if (getSettings().editorLanguageServer) {
    const projectPath = getActiveProject()?.path;
    lspExtensionForFile(filePath, fileName, projectPath)
      .then((ext) => {
        if (tabs.get(uiId) === tab && (!Array.isArray(ext) || ext.length)) {
          editorHandle.setLspExtension(ext);
        }
      })
      .catch((err) => console.warn("[lsp] wiring failed:", err));
  }

  // Right-click context menu for editor
  editorContent.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    const old = document.getElementById("context-menu");
    if (old) old.remove();

    const menu = document.createElement("div");
    menu.id = "context-menu";
    menu.className = "context-menu";

    const sel = editorView.state.sliceDoc(
      editorView.state.selection.main.from,
      editorView.state.selection.main.to
    );

    const copyItem = document.createElement("div");
    copyItem.className = "context-menu-item";
    copyItem.textContent = "Copy";
    if (!sel) copyItem.classList.add("context-menu-item-disabled");
    copyItem.addEventListener("click", () => {
      if (sel) navigator.clipboard.writeText(sel);
      menu.remove();
    });

    const cutItem = document.createElement("div");
    cutItem.className = "context-menu-item";
    cutItem.textContent = "Cut";
    if (!sel) cutItem.classList.add("context-menu-item-disabled");
    cutItem.addEventListener("click", () => {
      if (sel) {
        navigator.clipboard.writeText(sel);
        editorView.dispatch({ changes: editorView.state.selection.main });
      }
      menu.remove();
    });

    const pasteItem = document.createElement("div");
    pasteItem.className = "context-menu-item";
    pasteItem.textContent = "Paste";
    pasteItem.addEventListener("click", async () => {
      const text = await navigator.clipboard.readText();
      if (text) {
        editorView.dispatch({
          changes: { from: editorView.state.selection.main.from, to: editorView.state.selection.main.to, insert: text },
        });
      }
      menu.remove();
    });

    const selectAllItem = document.createElement("div");
    selectAllItem.className = "context-menu-item";
    selectAllItem.textContent = "Select All";
    selectAllItem.addEventListener("click", () => {
      editorView.dispatch({
        selection: { anchor: 0, head: editorView.state.doc.length },
      });
      menu.remove();
    });

    menu.appendChild(cutItem);
    menu.appendChild(copyItem);
    menu.appendChild(pasteItem);
    const sep = document.createElement("div");
    sep.className = "context-menu-separator";
    menu.appendChild(sep);
    menu.appendChild(selectAllItem);

    menu.style.left = e.clientX + "px";
    menu.style.top = e.clientY + "px";
    document.body.appendChild(menu);

    setTimeout(() => {
      document.addEventListener("click", () => menu.remove(), { once: true });
    }, 0);
  });

  renderTabBar();
  switchTab(uiId);
  if (line) requestAnimationFrame(() => gotoLine(editorView, line, column));
  return uiId;
}

export async function saveEditorTab(uiId) {
  const tab = tabs.get(uiId);
  if (!tab || tab.type !== "editor") return;

  try {
    const rawContent = tab.editorView.state.doc.toString();
    let content = rawContent;
    if (tab.lineEndings === "CRLF") {
      content = content.replace(/\r?\n/g, "\r\n");
    }
    await invoke("write_file", { path: tab.filePath, content });

    // Format on save (opt-in): the formatter rewrites the file in place; sync
    // the buffer to its output. Best-effort — a missing formatter or a format
    // error toasts but doesn't fail the save (the file is already written).
    let savedContent = rawContent;
    if (getSettings().editorFormatOnSave) {
      try {
        const formatted = await invoke("format_file", {
          projectPath: getActiveProject()?.path || "",
          filePath: tab.filePath,
        });
        if (formatted != null && formatted !== rawContent) {
          const view = tab.editorView;
          const caret = Math.min(view.state.selection.main.head, formatted.length);
          view.dispatch({
            changes: { from: 0, to: view.state.doc.length, insert: formatted },
            selection: { anchor: caret },
          });
          savedContent = formatted;
        }
      } catch (err) {
        showToast(`Format on save: ${err}`, "error");
      }
    }

    tab.originalContent = savedContent;
    tab.modified = false;
    renderTabBar();
    // Disk now differs from HEAD by the just-saved content — repaint the gutter.
    refreshEditorGutter(tab);
    // Flash the tab label green briefly to confirm save
    const tabEl = document.querySelector(`.tab[data-tab-id="${uiId}"] .tab-label`);
    if (tabEl) {
      tabEl.classList.add("tab-save-flash");
      setTimeout(() => tabEl.classList.remove("tab-save-flash"), 600);
    }
    // Auto-stage when a conflict-mode tab is saved with all blocks resolved.
    // Gate is `parseConflictBlocks` returning [] — not a substring scan —
    // so source files containing literal "<<<<<<< HEAD" (e.g. test fixtures)
    // never wrongly trigger a stage.
    if (tab.conflictMode && parseConflictBlocks(savedContent).length === 0) {
      const project = getActiveProject();
      if (project) {
        try {
          await invoke("git_stage_file", { path: project.path, filePath: tab.filePath });
          showToast(`Resolved ${tab.fileName}`, "info");
          // Once staged, subsequent saves shouldn't re-stage this tab.
          tab.conflictMode = false;
          // Refresh the panel so the conflict moves out of the conflicts
          // section and into staged.
          refreshPanel(null, true);
        } catch (err) {
          showToast(`Auto-stage failed: ${err}`, "error");
        }
      }
    }
  } catch (err) {
    console.error("Save error:", err);
    // Show inline error toast
    const tabEl = document.querySelector(`.tab[data-tab-id="${uiId}"]`);
    if (tabEl) {
      const toast = document.createElement("div");
      toast.className = "save-error-toast";
      toast.textContent = `Save failed: ${err}`;
      tabEl.parentElement.appendChild(toast);
      setTimeout(() => toast.remove(), 3000);
    }
  }
}
