// Compare / diff tab — a two-pane view (file list + per-file unified diff) for
// comparing two refs. Reachable from the COMMITS context menu and the file
// browser. Backed by the get_diff_between_refs Rust command.
const { invoke } = window.__TAURI__.core;
import { tabs, nextTabId, switchTab, renderTabBar } from "./tabs.js";
import { getActiveProject } from "./projects.js";
import { showToast } from "./toast.js";
import { escapeText, escapeAttr } from "./domutil.js";
import { buildFileDiffSection } from "./diffrender.js";

function shortenRef(ref) {
  if (!ref) return "";
  if (ref.length >= 40 && /^[0-9a-fA-F]+$/.test(ref)) return ref.slice(0, 7);
  return ref;
}

// Compares from_ref → to_ref as a multi-file diff in a new tab. Re-uses an
// existing tab when the same pair is already open.
export async function createDiffTab({ fromRef, toRef }) {
  const project = getActiveProject();
  if (!project) {
    showToast("Open a project first", "error");
    return null;
  }

  // Dedupe: same pair → switch to existing tab
  for (const [uiId, t] of tabs) {
    if (t.type === "diff" && t.fromRef === fromRef && t.toRef === toRef) {
      switchTab(uiId);
      return uiId;
    }
  }

  let refDiff;
  try {
    refDiff = await invoke("get_diff_between_refs", {
      path: project.path,
      fromRef,
      toRef,
    });
  } catch (err) {
    showToast(`Compare failed: ${err}`, "error");
    return null;
  }

  const uiId = nextTabId();

  const containerEl = document.createElement("div");
  containerEl.className = "diff-instance diff-tab";
  document.getElementById("terminal-instances").appendChild(containerEl);

  const tab = {
    type: "diff",
    containerEl,
    fromRef,
    toRef,
    refDiff,
    selectedFileIndex: 0,
    fileName: `${shortenRef(fromRef)} → ${shortenRef(toRef)}`,
  };
  tabs.set(uiId, tab);

  renderDiffTab(tab, uiId);

  renderTabBar();
  switchTab(uiId);
  return uiId;
}

function renderDiffTab(tab, uiId) {
  const { containerEl, refDiff } = tab;
  containerEl.replaceChildren();

  // Header row: from..to + summary stats
  const header = document.createElement("div");
  header.className = "diff-tab-header";
  const title = document.createElement("div");
  title.className = "diff-tab-title";
  title.textContent = `${tab.fromRef} → ${tab.toRef}`;
  const stats = document.createElement("div");
  stats.className = "diff-tab-stats";
  stats.innerHTML = `<span>${refDiff.stats.files_changed} file${refDiff.stats.files_changed === 1 ? "" : "s"}</span><span class="diff-add-count">+${refDiff.stats.additions}</span><span class="diff-del-count">−${refDiff.stats.deletions}</span>`;
  header.appendChild(title);
  header.appendChild(stats);
  containerEl.appendChild(header);

  // Body: two-column flex
  const body = document.createElement("div");
  body.className = "diff-tab-body";
  containerEl.appendChild(body);

  if (refDiff.files.length === 0) {
    const empty = document.createElement("div");
    empty.className = "diff-tab-empty";
    const sameTip = tab.fromRef === tab.toRef ||
      (refDiff.from_ref && refDiff.to_ref && refDiff.from_ref === refDiff.to_ref);
    empty.innerHTML = `
      <div class="diff-tab-empty-title">No committed differences</div>
      <div class="diff-tab-empty-hint">
        ${sameTip
          ? `Both refs point to the same commit.`
          : `These refs share the same tree.`}
        <br>
        Uncommitted working-tree changes are not included in a ref-vs-ref diff —
        check the git panel's Changes section for those.
      </div>`;
    body.appendChild(empty);
    return;
  }

  // Sticky file list
  const fileList = document.createElement("div");
  fileList.className = "diff-file-list";
  body.appendChild(fileList);

  // Scrollable diff column
  const content = document.createElement("div");
  content.className = "diff-tab-content";
  body.appendChild(content);

  // Build per-file sections + matching list items. Anchor ids derived from
  // the tab's uiId + file index — never from refs (which can contain `/`).
  const sections = [];
  refDiff.files.forEach((file, idx) => {
    const path = file.new_path || file.old_path || "(unknown)";
    let added = 0;
    let removed = 0;
    for (const h of file.hunks) {
      for (const l of h.lines) {
        if (l.origin === "add") added++;
        else if (l.origin === "remove") removed++;
      }
    }
    const anchorId = `diff-${uiId}-file-${idx}`;
    sections.push({ anchorId, file });

    const item = document.createElement("div");
    item.className = "diff-file-list-item";
    item.dataset.fileIndex = String(idx);
    if (idx === tab.selectedFileIndex) item.classList.add("selected");
    item.innerHTML = `<span class="diff-file-list-path" title="${escapeAttr(path)}">${escapeText(path)}</span><span class="diff-file-list-stats"><span class="diff-add-count">+${added}</span> <span class="diff-del-count">−${removed}</span></span>`;
    item.addEventListener("click", () => {
      tab.selectedFileIndex = idx;
      fileList.querySelectorAll(".diff-file-list-item").forEach((el) => el.classList.remove("selected"));
      item.classList.add("selected");
      const target = content.querySelector(`#${CSS.escape(anchorId)}`);
      if (target) target.scrollIntoView({ behavior: "smooth", block: "start" });
    });
    fileList.appendChild(item);
  });

  // Render diff sections via diffrender.js (HTML strings, then parse once)
  const sectionsHtml = sections
    .map(({ anchorId, file }) => buildFileDiffSection(file, anchorId))
    .join("");
  content.innerHTML = sectionsHtml;
}
