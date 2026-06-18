import { undoDepth, redoDepth } from "@codemirror/commands";
import { EditorView } from "@codemirror/view";
import { updateChangeGutter, deriveLineChanges } from "./changegutter.js";
import { getLangName } from "./editor.js";
import { toRepoRelativePath } from "./git.js";
import { getActiveProject } from "./projects.js";
import { setOverride } from "./filesettings.js";
import { showToast } from "./toast.js";
import { createDiffTab } from "./difftab.js";
import { renderTabBar } from "./tabs.js";

const { invoke } = window.__TAURI__.core;

// Cached home directory for path-shortening in breadcrumbs. Set once at
// boot from get_home_dir (see boot()), not hardcoded — supports corporate
// macOS setups with email-style usernames, Docker mounts, and Linux.
let homeDir = "";
export function setHomeDir(h) { homeDir = h; }

// Build (or rebuild) a breadcrumb element's contents for a file path. Used
// both when creating an editor tab and when updating after an external
// rename.
export function renderBreadcrumb(breadcrumbEl, filePath) {
  breadcrumbEl.replaceChildren();
  // Replace the resolved home dir with "~" instead of assuming
  // /Users/<name> at depth 2 (fails on corporate setups / Docker / Linux).
  const homePath = homeDir && filePath.startsWith(homeDir)
    ? "~" + filePath.slice(homeDir.length)
    : filePath;
  const parts = homePath.split("/");
  parts.forEach((part, i) => {
    if (i > 0) {
      const sep = document.createElement("span");
      sep.className = "breadcrumb-separator";
      sep.textContent = "›";
      breadcrumbEl.appendChild(sep);
    }
    const span = document.createElement("span");
    span.className = i === parts.length - 1 ? "breadcrumb-file" : "";
    span.textContent = part;
    breadcrumbEl.appendChild(span);
  });
}

// Editor tab management
const EMPTY_CHANGES = { added: new Set(), modified: new Set(), deleted: new Set() };

// Recompute the change gutter for an editor tab from the working-tree-vs-HEAD
// diff. Best-effort: a file outside the repo, or any backend error, just clears
// the gutter. Markers reflect disk-vs-HEAD, so they update on save / external
// change, not on every keystroke (see editor-spec Track A).
export async function refreshEditorGutter(tab) {
  if (!tab || tab.type !== "editor" || !tab.editorView) return;
  const project = getActiveProject();
  const rel = project ? toRepoRelativePath(tab.filePath) : null;
  if (!rel) {
    tab.lineDiff = null;
    updateChangeGutter(tab.editorView, EMPTY_CHANGES);
    return;
  }
  try {
    const diff = await invoke("get_file_diff_vs_head", { path: project.path, filePath: rel });
    tab.lineDiff = diff;
    updateChangeGutter(tab.editorView, deriveLineChanges(diff));
  } catch (_) {
    tab.lineDiff = null;
    updateChangeGutter(tab.editorView, EMPTY_CHANGES);
  }
}

// Locate the hunk responsible for a marker on `lineNo` (1-based, new side).
// A line is "in" a hunk if it falls in the hunk's new-side span; a pure
// deletion (new_lines === 0) attaches to the line just above the gap.
export function findHunkForLine(diff, lineNo) {
  if (!diff || !Array.isArray(diff.hunks)) return null;
  for (const h of diff.hunks) {
    if (h.new_lines > 0) {
      if (lineNo >= h.new_start && lineNo <= h.new_start + h.new_lines - 1) return h;
    } else if (lineNo === h.new_start - 1 || lineNo === h.new_start) {
      return h;
    }
  }
  return null;
}

// Replace a hunk's new-side text with its HEAD (old-side) text in the buffer.
// Correctness rests on the buffer matching disk (and thus the diff's new side):
// when not modified, the concatenated content of the hunk's new-side lines is an
// exact substring of the document starting at new_start, so its length pins the
// replace range without any newline bookkeeping. Deletions (no new-side lines)
// insert the old text at the gap.
export function revertHunkInBuffer(view, hunk) {
  const doc = view.state.doc;
  const newSide = hunk.lines.filter((l) => l.origin === "add" || l.origin === "context");
  const oldSide = hunk.lines.filter((l) => l.origin === "remove" || l.origin === "context");
  const newText = newSide.map((l) => l.content).join("");
  const oldText = oldSide.map((l) => l.content).join("");

  let from, to;
  if (hunk.new_lines > 0) {
    from = doc.line(hunk.new_start).from;
    to = from + newText.length;
  } else {
    // Pure deletion: insert the removed lines at the start of the line below
    // the gap (new_start is that line), or at end-of-doc when past the last line.
    if (hunk.new_start >= 1 && hunk.new_start <= doc.lines) {
      from = to = doc.line(hunk.new_start).from;
    } else {
      from = to = doc.length;
    }
  }
  if (to > doc.length) return false; // stale diff — bail rather than corrupt
  view.dispatch({ changes: { from, to, insert: oldText } });
  return true;
}

// Toggle the opt-in blame gutter for an editor tab. Blames committed content
// (HEAD), so it's best-effort against unsaved edits; clicking a line opens that
// commit's diff (compared with its parent).
export async function toggleBlame(tab) {
  if (!tab || tab.type !== "editor" || !tab.editorHandle) return;
  if (tab.blameOn) {
    tab.editorHandle.hideBlame();
    tab.blameOn = false;
    return;
  }
  const project = getActiveProject();
  const rel = project ? toRepoRelativePath(tab.filePath) : null;
  if (!rel) {
    showToast("Blame is only available for files tracked in this project's repo", "error");
    return;
  }
  try {
    const hunks = await invoke("git_blame_file", { path: project.path, filePath: rel });
    tab.editorHandle.showBlame({
      hunks,
      onClick: (oid) => createDiffTab({ fromRef: `${oid}^`, toRef: oid }),
    });
    tab.blameOn = true;
  } catch (err) {
    showToast(`Blame failed: ${err}`, "error");
  }
}

const TAB_SIZE_CYCLE = [2, 4, 8];

export function renderEditorStatus(tab, statusBar) {
  const view = tab.editorView;
  const ud = view ? undoDepth(view.state) : 0;
  const rd = view ? redoDepth(view.state) : 0;
  const { line, col } = tab.cursor || { line: 1, col: 1 };

  statusBar.innerHTML = "";

  const pos = document.createElement("span");
  pos.className = "esb-pos";
  pos.textContent = `Ln ${line}, Col ${col}`;
  statusBar.appendChild(pos);

  const addSep = () => {
    const s = document.createElement("span");
    s.className = "esb-sep";
    s.textContent = "·";
    statusBar.appendChild(s);
  };

  addSep();
  const lang = document.createElement("span");
  lang.className = "esb-lang";
  lang.textContent = getLangName(tab.fileName);
  statusBar.appendChild(lang);

  addSep();
  const lineEnd = document.createElement("span");
  lineEnd.className = "esb-item esb-click";
  lineEnd.title = "Click to toggle line endings";
  lineEnd.textContent = tab.lineEndings;
  lineEnd.addEventListener("click", async () => {
    tab.lineEndings = tab.lineEndings === "LF" ? "CRLF" : "LF";
    await setOverride(tab.filePath, "lineEndings", tab.lineEndings);
    renderEditorStatus(tab, statusBar);
    tab.modified = true;
    renderTabBar();
  });
  statusBar.appendChild(lineEnd);

  addSep();
  const tabSize = document.createElement("span");
  tabSize.className = "esb-item esb-click";
  tabSize.title = "Click to change tab size";
  tabSize.textContent = `Tab ${tab.tabSize}`;
  tabSize.addEventListener("click", async () => {
    const idx = TAB_SIZE_CYCLE.indexOf(tab.tabSize);
    const next = TAB_SIZE_CYCLE[(idx + 1) % TAB_SIZE_CYCLE.length];
    tab.tabSize = next;
    tab.editorHandle.setTabSize(next);
    await setOverride(tab.filePath, "tabSize", next);
    renderEditorStatus(tab, statusBar);
  });
  statusBar.appendChild(tabSize);

  addSep();
  const wrap = document.createElement("span");
  wrap.className = "esb-item esb-click" + (tab.wordWrap ? " esb-active" : "");
  wrap.title = "Click to toggle word wrap";
  wrap.textContent = tab.wordWrap ? "Wrap" : "No Wrap";
  wrap.addEventListener("click", async () => {
    tab.wordWrap = !tab.wordWrap;
    tab.editorHandle.setWordWrap(tab.wordWrap);
    await setOverride(tab.filePath, "wordWrap", tab.wordWrap);
    renderEditorStatus(tab, statusBar);
  });
  statusBar.appendChild(wrap);

  addSep();
  const blame = document.createElement("span");
  blame.className = "esb-item esb-click" + (tab.blameOn ? " esb-active" : "");
  blame.title = "Toggle git blame";
  blame.textContent = "Blame";
  blame.addEventListener("click", async () => {
    await toggleBlame(tab);
    renderEditorStatus(tab, statusBar);
  });
  statusBar.appendChild(blame);

  if (ud || rd) {
    addSep();
    const hist = document.createElement("span");
    hist.className = "esb-hist";
    hist.textContent = `Undo: ${ud} Redo: ${rd}`;
    statusBar.appendChild(hist);
  }
}

export function gotoLine(view, lineNum, col) {
  if (!view) return;
  const doc = view.state.doc;
  const target = Math.max(1, Math.min(lineNum, doc.lines));
  const lineInfo = doc.line(target);
  const pos = lineInfo.from + Math.max(0, (col || 1) - 1);
  view.dispatch({
    selection: { anchor: pos },
    effects: EditorView.scrollIntoView(pos, { y: "center" }),
  });
  view.focus();
}
