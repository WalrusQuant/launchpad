// Shared diff HTML builder. Used by the git panel's inline diff preview and
// the multi-file `diff` tab. CSS classes are kept stable across both surfaces:
// .diff-hunk-header, .diff-line, .diff-add, .diff-del, .diff-gutter, .diff-content.

function escapeHtml(s) {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

export function buildDiffHtml(hunks) {
  let html = "";
  for (const hunk of hunks) {
    html += `<div class="diff-hunk-header">@@ -${hunk.old_start},${hunk.old_lines} +${hunk.new_start},${hunk.new_lines} @@</div>`;
    for (const line of hunk.lines) {
      const cls = line.origin === "add" ? "diff-add" : line.origin === "remove" ? "diff-del" : "";
      const oldNo = line.old_line_no >= 0 ? line.old_line_no : "";
      const newNo = line.new_line_no >= 0 ? line.new_line_no : "";
      const content = escapeHtml(line.content).replace(/\n$/, "");
      html += `<div class="diff-line ${cls}"><span class="diff-gutter">${oldNo}</span><span class="diff-gutter">${newNo}</span><span class="diff-content">${content}</span></div>`;
    }
  }
  return html;
}

// Renders a single FileDiff with a collapsible header showing path + +/- counts.
// `anchorId` is used as the section's DOM id so the diff tab's file list can
// scrollIntoView on click. Caller is responsible for passing a sanitized id.
export function buildFileDiffSection(fileDiff, anchorId) {
  const path = fileDiff.new_path || fileDiff.old_path || "(unknown)";
  let added = 0;
  let removed = 0;
  for (const hunk of fileDiff.hunks) {
    for (const line of hunk.lines) {
      if (line.origin === "add") added++;
      else if (line.origin === "remove") removed++;
    }
  }
  const idAttr = anchorId ? ` id="${anchorId}"` : "";
  const header = `<div class="diff-file-header"><span class="diff-file-path">${escapeHtml(path)}</span><span class="diff-file-stats"><span class="diff-add-count">+${added}</span> <span class="diff-del-count">−${removed}</span></span></div>`;
  const body = buildDiffHtml(fileDiff.hunks);
  return `<section class="diff-file-section"${idAttr}>${header}<div class="diff-file-body">${body}</div></section>`;
}
