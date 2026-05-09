// Minimal unified-diff parser for agent tool payloads (apply_patch's
// `patchText` field). Returns the structured shape diffrender.js consumes.
//
// Supports both the GNU unified format ("--- a/path\n+++ b/path\n@@ ...")
// and the lpa-style "Apply Patch" envelope ("*** Begin Patch\n*** Update
// File: path\n@@\n -line\n +line\n*** End Patch"). For `apply_patch` the
// agent sends the lpa-style envelope; for any other source we try GNU.
//
// Returns: [{ old_path, new_path, hunks: [{old_start, old_lines, new_start,
// new_lines, lines: [{origin: "add"|"remove"|"context", old_line_no,
// new_line_no, content}]}]}, ...]

const HUNK_HEADER_RE = /^@@\s+-(\d+)(?:,(\d+))?\s+\+(\d+)(?:,(\d+))?\s+@@/;

export function parseUnifiedDiff(text) {
  if (!text || typeof text !== "string") return [];
  // lpa "Apply Patch" envelope detection — strip wrapper, route per-file.
  if (/^\*\*\* Begin Patch/m.test(text)) {
    return parseApplyPatchEnvelope(text);
  }
  return parseGnuUnified(text);
}

function parseGnuUnified(text) {
  const files = [];
  const lines = text.split("\n");
  let i = 0;
  let current = null;

  while (i < lines.length) {
    const line = lines[i];
    if (line.startsWith("--- ")) {
      const oldPath = stripPathPrefix(line.slice(4).trim());
      const next = lines[i + 1] || "";
      if (next.startsWith("+++ ")) {
        const newPath = stripPathPrefix(next.slice(4).trim());
        if (current) files.push(current);
        current = { old_path: oldPath, new_path: newPath, hunks: [] };
        i += 2;
        continue;
      }
    }
    const m = HUNK_HEADER_RE.exec(line);
    if (m && current) {
      const hunk = {
        old_start: parseInt(m[1], 10),
        old_lines: m[2] ? parseInt(m[2], 10) : 1,
        new_start: parseInt(m[3], 10),
        new_lines: m[4] ? parseInt(m[4], 10) : 1,
        lines: [],
      };
      current.hunks.push(hunk);
      i++;
      let oldNo = hunk.old_start;
      let newNo = hunk.new_start;
      while (i < lines.length) {
        const body = lines[i];
        if (body.startsWith("@@") || body.startsWith("--- ")) break;
        if (body.startsWith("+")) {
          hunk.lines.push({ origin: "add", old_line_no: -1, new_line_no: newNo++, content: body.slice(1) });
        } else if (body.startsWith("-")) {
          hunk.lines.push({ origin: "remove", old_line_no: oldNo++, new_line_no: -1, content: body.slice(1) });
        } else if (body.startsWith(" ") || body === "") {
          hunk.lines.push({ origin: "context", old_line_no: oldNo++, new_line_no: newNo++, content: body.slice(1) });
        } else if (body.startsWith("\\")) {
          // "\ No newline at end of file" — ignore.
        } else {
          break;
        }
        i++;
      }
      continue;
    }
    i++;
  }
  if (current) files.push(current);
  return files;
}

// Parses the "Apply Patch" envelope used by the agent's apply_patch tool.
// Format example:
//   *** Begin Patch
//   *** Update File: path/to/file
//   @@
//    context line
//   -removed
//   +added
//   *** End Patch
// Also supports `*** Add File: path` and `*** Delete File: path`. Hunk
// headers may appear without line numbers ("@@") so we synthesize them.
function parseApplyPatchEnvelope(text) {
  const files = [];
  const lines = text.split("\n");
  let i = 0;
  let current = null;
  let runningOldNo = 1;
  let runningNewNo = 1;

  const flushHunkIfPending = () => {
    if (!current || !current.hunks.length) return;
    const h = current.hunks[current.hunks.length - 1];
    // Counts reflect what's actually in the hunk — pure add file = old=0,
    // pure delete file = new=0. Don't floor to 1; the @@ header math
    // would lie about the changed range.
    h.old_lines = h.lines.filter((l) => l.origin !== "add").length;
    h.new_lines = h.lines.filter((l) => l.origin !== "remove").length;
  };

  while (i < lines.length) {
    const line = lines[i];
    if (line.startsWith("*** Update File: ")) {
      flushHunkIfPending();
      if (current) files.push(current);
      const path = line.slice("*** Update File: ".length).trim();
      current = { old_path: path, new_path: path, hunks: [] };
      runningOldNo = 1;
      runningNewNo = 1;
    } else if (line.startsWith("*** Add File: ")) {
      flushHunkIfPending();
      if (current) files.push(current);
      const path = line.slice("*** Add File: ".length).trim();
      current = { old_path: "/dev/null", new_path: path, hunks: [] };
      runningOldNo = 1;
      runningNewNo = 1;
    } else if (line.startsWith("*** Delete File: ")) {
      flushHunkIfPending();
      if (current) files.push(current);
      const path = line.slice("*** Delete File: ".length).trim();
      current = { old_path: path, new_path: "/dev/null", hunks: [] };
      runningOldNo = 1;
      runningNewNo = 1;
    } else if (line.startsWith("@@")) {
      if (current) {
        flushHunkIfPending();
        current.hunks.push({
          old_start: runningOldNo,
          old_lines: 0,
          new_start: runningNewNo,
          new_lines: 0,
          lines: [],
        });
      }
    } else if (line.startsWith("*** End Patch") || line.startsWith("*** Begin Patch")) {
      // markers — ignore.
    } else if (current && current.hunks.length) {
      const h = current.hunks[current.hunks.length - 1];
      if (line.startsWith("+")) {
        h.lines.push({ origin: "add", old_line_no: -1, new_line_no: runningNewNo++, content: line.slice(1) });
      } else if (line.startsWith("-")) {
        h.lines.push({ origin: "remove", old_line_no: runningOldNo++, new_line_no: -1, content: line.slice(1) });
      } else if (line.startsWith(" ")) {
        h.lines.push({ origin: "context", old_line_no: runningOldNo++, new_line_no: runningNewNo++, content: line.slice(1) });
      }
      // Anything else inside a hunk is ignored.
    }
    i++;
  }
  flushHunkIfPending();
  if (current) files.push(current);
  return files;
}

function stripPathPrefix(p) {
  if (p.startsWith("a/") || p.startsWith("b/")) return p.slice(2);
  return p;
}

// Aggregate +/- counts across files for the card header summary.
export function diffStats(files) {
  let added = 0;
  let removed = 0;
  for (const f of files) {
    for (const h of f.hunks) {
      for (const l of h.lines) {
        if (l.origin === "add") added++;
        else if (l.origin === "remove") removed++;
      }
    }
  }
  return { added, removed };
}
