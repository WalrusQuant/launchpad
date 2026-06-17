import { describe, it, expect } from "vitest";
import { buildDiffHtml, buildFileDiffSection } from "./diffrender.js";

// Helper to build a hunk line in the shape diffrender expects.
const line = (origin, oldNo, newNo, content) => ({
  origin,
  old_line_no: oldNo,
  new_line_no: newNo,
  content,
});

describe("buildDiffHtml", () => {
  it("renders a hunk header from the +/- ranges", () => {
    const html = buildDiffHtml([
      { old_start: 10, old_lines: 3, new_start: 12, new_lines: 4, lines: [] },
    ]);
    expect(html).toContain('<div class="diff-hunk-header">@@ -10,3 +12,4 @@</div>');
  });

  it("tags add/remove/context lines with the right class", () => {
    const html = buildDiffHtml([
      {
        old_start: 1, old_lines: 1, new_start: 1, new_lines: 2,
        lines: [
          line("context", 1, 1, "unchanged\n"),
          line("remove", 2, -1, "gone\n"),
          line("add", -1, 2, "new\n"),
        ],
      },
    ]);
    expect(html).toContain('class="diff-line diff-add"');
    expect(html).toContain('class="diff-line diff-del"');
    // Context lines get no add/del modifier — just the base class + trailing space.
    expect(html).toContain('class="diff-line "');
  });

  it("blanks the gutter for sentinel (-1) line numbers and keeps real ones", () => {
    const html = buildDiffHtml([
      {
        old_start: 1, old_lines: 0, new_start: 1, new_lines: 1,
        lines: [line("add", -1, 7, "x\n")],
      },
    ]);
    // old gutter empty, new gutter shows 7
    expect(html).toContain('<span class="diff-gutter"></span><span class="diff-gutter">7</span>');
  });

  it("escapes HTML in line content so diff text can't inject markup", () => {
    const html = buildDiffHtml([
      {
        old_start: 1, old_lines: 1, new_start: 1, new_lines: 1,
        lines: [line("add", -1, 1, "<script>alert(1)</script> & <b>\n")],
      },
    ]);
    expect(html).toContain("&lt;script&gt;alert(1)&lt;/script&gt; &amp; &lt;b&gt;");
    expect(html).not.toContain("<script>alert(1)</script>");
  });

  it("strips a single trailing newline from content (not interior ones)", () => {
    const html = buildDiffHtml([
      {
        old_start: 1, old_lines: 1, new_start: 1, new_lines: 1,
        lines: [line("context", 1, 1, "a\nb\n")],
      },
    ]);
    // The interior \n survives escaping (becomes a literal newline in the span),
    // only the final \n is trimmed.
    expect(html).toContain('<span class="diff-content">a\nb</span>');
  });

  it("returns empty string for no hunks", () => {
    expect(buildDiffHtml([])).toBe("");
  });
});

describe("buildFileDiffSection", () => {
  const fileDiff = {
    new_path: "src/app.js",
    old_path: "src/app.js",
    hunks: [
      {
        old_start: 1, old_lines: 1, new_start: 1, new_lines: 2,
        lines: [
          line("remove", 1, -1, "old\n"),
          line("add", -1, 1, "new1\n"),
          line("add", -1, 2, "new2\n"),
        ],
      },
    ],
  };

  it("counts added and removed lines across hunks", () => {
    const html = buildFileDiffSection(fileDiff, "anchor-1");
    expect(html).toContain('<span class="diff-add-count">+2</span>');
    expect(html).toContain('<span class="diff-del-count">−1</span>');
  });

  it("uses new_path, falling back to old_path then (unknown)", () => {
    expect(buildFileDiffSection({ old_path: "only/old.js", hunks: [] }, "")).toContain(
      "only/old.js"
    );
    expect(buildFileDiffSection({ hunks: [] }, "")).toContain("(unknown)");
  });

  it("emits the anchor id only when provided", () => {
    expect(buildFileDiffSection(fileDiff, "f0")).toContain('<section class="diff-file-section" id="f0">');
    expect(buildFileDiffSection(fileDiff, "")).toContain('<section class="diff-file-section">');
  });

  it("escapes the file path", () => {
    const html = buildFileDiffSection({ new_path: "a/<x>.js", hunks: [] }, "");
    expect(html).toContain("a/&lt;x&gt;.js");
  });
});
