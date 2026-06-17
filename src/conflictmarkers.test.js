import { describe, it, expect } from "vitest";
import { parseConflictBlocks, replaceBlock } from "./conflictmarkers.js";

describe("parseConflictBlocks — happy path", () => {
  it("parses a basic 2-way conflict", () => {
    const text = "a\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> branch\nb\n";
    const blocks = parseConflictBlocks(text);
    expect(blocks).toHaveLength(1);
    const b = blocks[0];
    expect(b.oursText).toBe("ours");
    expect(b.theirsText).toBe("theirs");
    expect(b.baseText).toBeUndefined();
    expect(b.hadTrailingNewline).toBe(true);
    // start at the `<<<<<<<` line offset; end just past the newline after `>>>>>>>`.
    expect(text.slice(b.start, b.end)).toBe(
      "<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> branch\n"
    );
  });

  it("parses a diff3 conflict with a base section", () => {
    const text = "<<<<<<< HEAD\nO\n||||||| base\nB\n=======\nT\n>>>>>>> x\n";
    const blocks = parseConflictBlocks(text);
    expect(blocks).toHaveLength(1);
    const b = blocks[0];
    expect(b.oursText).toBe("O");
    expect(b.baseText).toBe("B");
    expect(b.theirsText).toBe("T");
    expect(b.baseMarkerLine).toBe(2);
  });

  it("accepts bare 7-char markers with no label", () => {
    const text = "<<<<<<<\nours\n=======\ntheirs\n>>>>>>>\n";
    const blocks = parseConflictBlocks(text);
    expect(blocks).toHaveLength(1);
    expect(blocks[0].oursText).toBe("ours");
    expect(blocks[0].theirsText).toBe("theirs");
  });

  it("parses multiple independent conflicts", () => {
    const text =
      "<<<<<<<\n1\n=======\n2\n>>>>>>>\nmid\n<<<<<<<\n3\n=======\n4\n>>>>>>>\n";
    const blocks = parseConflictBlocks(text);
    expect(blocks).toHaveLength(2);
    expect(blocks[0].oursText).toBe("1");
    expect(blocks[1].theirsText).toBe("4");
  });
});

describe("parseConflictBlocks — strictness (no false positives)", () => {
  it("rejects 6- and 8-char marker runs", () => {
    expect(parseConflictBlocks("<<<<<< h\nx\n=======\ny\n>>>>>>> z\n")).toHaveLength(0);
    expect(parseConflictBlocks("<<<<<<< h\nx\n======\ny\n>>>>>>> z\n")).toHaveLength(0);
    expect(parseConflictBlocks("<<<<<<<< h\nx\n=======\ny\n>>>>>>> z\n")).toHaveLength(0);
  });

  it("rejects markers not at column 0", () => {
    const text = " <<<<<<< HEAD\nx\n =======\ny\n >>>>>>> z\n";
    expect(parseConflictBlocks(text)).toHaveLength(0);
  });

  it("requires a space (not arbitrary char) after a 7-char run", () => {
    // `<<<<<<<X` — 8th char is not a space → not a marker line.
    const text = "<<<<<<<X\nx\n=======\ny\n>>>>>>>\n";
    expect(parseConflictBlocks(text)).toHaveLength(0);
  });

  it("drops an unterminated open marker", () => {
    expect(parseConflictBlocks("<<<<<<< HEAD\nours\nno close here\n")).toHaveLength(0);
  });

  it("drops a >>> with no preceding ===", () => {
    expect(parseConflictBlocks("<<<<<<< HEAD\nours\n>>>>>>> x\n")).toHaveLength(0);
  });

  it("ignores marker glyphs inside ordinary content lines", () => {
    // A source file that mentions the markers in prose / string literals.
    const text = 'const s = "<<<<<<< HEAD not a real conflict";\n';
    expect(parseConflictBlocks(text)).toHaveLength(0);
  });
});

describe("parseConflictBlocks — nesting", () => {
  it("bails the outer block and parses the inner one on a nested open", () => {
    const text =
      "<<<<<<< HEAD\nstuff\n<<<<<<< INNER\na\n=======\nb\n>>>>>>> y\n";
    const blocks = parseConflictBlocks(text);
    expect(blocks).toHaveLength(1);
    expect(blocks[0].oursText).toBe("a");
    expect(blocks[0].theirsText).toBe("b");
  });
});

describe("parseConflictBlocks — trailing newline policy", () => {
  it("flags hadTrailingNewline=true when a newline follows >>>", () => {
    const blocks = parseConflictBlocks("<<<<<<<\no\n=======\nt\n>>>>>>>\n");
    expect(blocks[0].hadTrailingNewline).toBe(true);
  });

  it("flags hadTrailingNewline=false at EOF without a newline", () => {
    const text = "<<<<<<<\no\n=======\nt\n>>>>>>>";
    const blocks = parseConflictBlocks(text);
    expect(blocks[0].hadTrailingNewline).toBe(false);
    expect(blocks[0].end).toBe(text.length);
  });
});

describe("replaceBlock", () => {
  // Reuse a parsed basic block so start/end/newline flags are realistic.
  const text = "a\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> branch\nb\n";
  const block = parseConflictBlocks(text)[0];

  it("resolves to ours, re-appending the trailing newline", () => {
    expect(replaceBlock(block, "ours")).toEqual({
      from: block.start,
      to: block.end,
      insert: "ours\n",
    });
  });

  it("resolves to theirs", () => {
    expect(replaceBlock(block, "theirs").insert).toBe("theirs\n");
  });

  it("resolves to both with a single newline separator", () => {
    expect(replaceBlock(block, "both").insert).toBe("ours\ntheirs\n");
  });

  it("treats an unknown string choice as the literal replacement", () => {
    expect(replaceBlock(block, "custom text").insert).toBe("custom text\n");
  });

  it("does not invent a trailing newline when the block had none", () => {
    const noNl = parseConflictBlocks("<<<<<<<\no\n=======\nt\n>>>>>>>")[0];
    expect(replaceBlock(noNl, "ours").insert).toBe("o");
    expect(replaceBlock(noNl, "both").insert).toBe("o\nt");
  });

  it("keeps an empty resolution empty (no stray newline)", () => {
    // An empty `ours` side resolved with a trailing-newline block must NOT gain
    // a newline — the length>0 guard in replaceBlock prevents inventing a line.
    const emptyOurs = { start: 0, end: 10, hadTrailingNewline: true, oursText: "", theirsText: "x" };
    expect(replaceBlock(emptyOurs, "ours").insert).toBe("");
  });
});
