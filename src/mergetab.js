import { createEditor } from "./editor.js";
import { parseConflictBlocks } from "./conflictmarkers.js";
import { refreshPanel } from "./gitpanel.js";
import { getActiveProject } from "./projects.js";
import { getSettings } from "./settings.js";
import { getResolvedTheme } from "./theme.js";
import { showToast } from "./toast.js";
import {
  tabs, nextTabId, switchTab, renderTabBar,
} from "./tabs.js";

const { invoke } = window.__TAURI__.core;

// ─── 3-pane merge tab ──────────────────────────────────────────────────────
// Opens a side-by-side view of a conflicted file: ours (read-only, from
// index stage 2), merged (editable, working tree with conflict markers),
// theirs (read-only, stage 3). Save writes the merged pane to disk and
// auto-stages the file once all conflict blocks are resolved (same gate
// as the inline conflict editor in `saveEditorTab`).
export async function createMergeTab({ filePath }) {
  const project = getActiveProject();
  if (!project) {
    showToast("Open a project first", "error");
    return null;
  }

  // Dedupe: same filePath → switch
  for (const [uiId, t] of tabs) {
    if (t.type === "merge" && t.filePath === filePath) {
      switchTab(uiId);
      return uiId;
    }
  }

  // The backend takes a project-relative path (libgit2 indexes paths
  // relative to the repo root). Strip the project prefix; if the file
  // somehow isn't under the project, we fail loudly rather than ship the
  // absolute path through (which libgit2 would silently miss in the index).
  const prefix = project.path.endsWith("/") ? project.path : project.path + "/";
  if (!filePath.startsWith(prefix)) {
    showToast(`Cannot open 3-way: ${filePath} is outside the project`, "error");
    return null;
  }
  const relPath = filePath.slice(prefix.length);

  let versions;
  try {
    versions = await invoke("get_conflict_versions", {
      path: project.path,
      filePath: relPath,
    });
  } catch (err) {
    showToast(`3-way merge failed: ${err}`, "error");
    return null;
  }

  const fileName = filePath.split("/").pop();
  const uiId = nextTabId();

  const containerEl = document.createElement("div");
  containerEl.className = "merge-instance merge-tab";
  document.getElementById("terminal-instances").appendChild(containerEl);

  const header = document.createElement("div");
  header.className = "merge-tab-header";
  const title = document.createElement("div");
  title.className = "merge-tab-title";
  title.textContent = fileName;
  const hint = document.createElement("div");
  hint.className = "merge-tab-hint";
  hint.textContent = "Edit center pane. Cmd+S saves and stages the resolved file.";
  header.appendChild(title);
  header.appendChild(hint);
  containerEl.appendChild(header);

  const body = document.createElement("div");
  body.className = "merge-tab-body";
  containerEl.appendChild(body);

  const buildPane = (cls, label) => {
    const pane = document.createElement("div");
    pane.className = `merge-pane ${cls}`;
    const labelEl = document.createElement("div");
    labelEl.className = "merge-pane-label";
    labelEl.textContent = label;
    const content = document.createElement("div");
    content.className = "merge-pane-content";
    pane.appendChild(labelEl);
    pane.appendChild(content);
    body.appendChild(pane);
    return content;
  };
  const oursContent = buildPane("merge-pane-ours", "OURS");
  const mergedContent = buildPane("merge-pane-merged", "MERGED");
  const theirsContent = buildPane("merge-pane-theirs", "THEIRS");

  const editorSettings = getSettings();
  const baseOptions = {
    tabSize: editorSettings.editorTabSize,
    wordWrap: editorSettings.editorWordWrap,
    fontSize: editorSettings.editorFontSize,
    theme: getResolvedTheme(),
    visualExtras: { indentGuides: editorSettings.editorIndentGuides },
  };

  // Side panes: read-only, no vim (vim's command mode would intercept
  // keys we never want to handle in a reference pane).
  const oursHandle = createEditor(oursContent, versions.ours || "", fileName, {
    ...baseOptions,
    readOnly: true,
  });
  const theirsHandle = createEditor(theirsContent, versions.theirs || "", fileName, {
    ...baseOptions,
    readOnly: true,
  });

  const tab = {
    type: "merge",
    containerEl,
    filePath,
    fileName,
    relPath,
    oursView: oursHandle.view,
    theirsView: theirsHandle.view,
    mergedView: null,
    originalContent: versions.merged || "",
    modified: false,
  };

  // Merged pane: editable, conflict-marker decorations from the inline
  // editor (PR3) so [Accept Ours/Theirs/Both] buttons work the same way.
  // We don't pass `onOpenThreeWay` here — clicking it from inside the
  // 3-way tab would just reopen the same tab.
  const mergedHandle = createEditor(mergedContent, versions.merged || "", fileName, {
    ...baseOptions,
    vimMode: editorSettings.editorVimMode,
    onChange: (newContent) => {
      tab.modified = newContent !== tab.originalContent;
      renderTabBar();
    },
    conflictMode: true,
  });
  tab.mergedView = mergedHandle.view;
  // Keep the handles so live settings (e.g. editorFontSize) can reconfigure
  // all three panes the same way single-editor tabs do.
  tab.editorHandles = [oursHandle, theirsHandle, mergedHandle];

  tabs.set(uiId, tab);

  // Synchronized scrolling: when the user scrolls the merged pane, align
  // ours/theirs to the same line number. Clamps to each pane's line count
  // so files of different lengths just stop at their own end. Cheap and
  // predictable — fancier hunk-aware mapping would need a Myers diff up
  // front and isn't worth the complexity for v1.
  // Note: we pin the listener on the tab so doCloseTab can detach it
  // before view.destroy() — CodeMirror destroys its own listeners but
  // not external ones we attached to scrollDOM.
  let syncing = false;
  const onMergedScroll = () => {
    if (syncing) return;
    syncing = true;
    try {
      const view = tab.mergedView;
      const top = view.scrollDOM.scrollTop;
      const block = view.lineBlockAtHeight(top);
      const line = view.state.doc.lineAt(block.from).number;
      [tab.oursView, tab.theirsView].forEach((other) => {
        const lineCount = other.state.doc.lines;
        const targetLine = Math.min(line, lineCount);
        const lineObj = other.state.doc.line(targetLine);
        const otherBlock = other.lineBlockAt(lineObj.from);
        other.scrollDOM.scrollTop = otherBlock.top;
      });
    } finally {
      syncing = false;
    }
  };
  tab.onMergedScroll = onMergedScroll;
  tab.mergedView.scrollDOM.addEventListener("scroll", onMergedScroll, { passive: true });

  renderTabBar();
  switchTab(uiId);
  return uiId;
}

export async function saveMergeTab(uiId) {
  const tab = tabs.get(uiId);
  if (!tab || tab.type !== "merge") return;
  const project = getActiveProject();
  if (!project) return;

  try {
    const content = tab.mergedView.state.doc.toString();
    await invoke("write_file", { path: tab.filePath, content });
    tab.originalContent = content;

    // Auto-stage when all conflict blocks are resolved — same gate as
    // saveEditorTab's conflict-mode path. We hold off clearing the
    // modified flag until staging confirms; on stage failure the tab
    // stays modified so the user knows another Cmd+S is needed.
    if (parseConflictBlocks(content).length === 0) {
      try {
        await invoke("git_stage_file", { path: project.path, filePath: tab.relPath });
        tab.modified = false;
        showToast(`Resolved ${tab.fileName}`, "info");
        refreshPanel(null, true);
      } catch (err) {
        showToast(`Auto-stage failed (Cmd+S to retry): ${err}`, "error");
      }
    } else {
      // Markers still present — file is saved, but unresolved. Cleared
      // modified is fine since the disk now matches the buffer.
      tab.modified = false;
    }
    renderTabBar();
  } catch (err) {
    showToast(`Save failed: ${err}`, "error");
  }
}
