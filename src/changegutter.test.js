import { describe, it, expect } from "vitest";
import { deriveLineChanges } from "./changegutter.js";

// Helpers to build the FileDiff shape the Rust side emits.
const ctx = (newNo, oldNo) => ({ origin: "context", new_line_no: newNo, old_line_no: oldNo, content: "" });
const add = (newNo) => ({ origin: "add", new_line_no: newNo, old_line_no: -1, content: "" });
const rem = (oldNo) => ({ origin: "remove", new_line_no: -1, old_line_no: oldNo, content: "" });
const hunk = (new_start, lines) => ({ old_start: 1, new_start, old_lines: 0, new_lines: 0, lines });

const arr = (set) => [...set].sort((a, b) => a - b);

describe("deriveLineChanges", () => {
  it("returns empty sets for null / empty input", () => {
    for (const input of [null, undefined, {}, { hunks: [] }]) {
      const { added, modified, deleted } = deriveLineChanges(input);
      expect(added.size).toBe(0);
      expect(modified.size).toBe(0);
      expect(deleted.size).toBe(0);
    }
  });

  it("classifies a pure insertion as added", () => {
    // Two brand-new lines inserted after line 3.
    const diff = { hunks: [hunk(4, [ctx(3, 3), add(4), add(5), ctx(6, 4)])] };
    const { added, modified, deleted } = deriveLineChanges(diff);
    expect(arr(added)).toEqual([4, 5]);
    expect(modified.size).toBe(0);
    expect(deleted.size).toBe(0);
  });

  it("classifies an equal-size replacement as modified", () => {
    // Two old lines replaced by two new lines at the same position.
    const diff = { hunks: [hunk(5, [ctx(4, 4), rem(5), rem(6), add(5), add(6), ctx(7, 7)])] };
    const { added, modified, deleted } = deriveLineChanges(diff);
    expect(modified.size).toBe(2);
    expect(arr(modified)).toEqual([5, 6]);
    expect(added.size).toBe(0);
    expect(deleted.size).toBe(0);
  });

  it("classifies a pure deletion as a deletion below the line above it", () => {
    // Three lines removed after new-side line 4 (no additions).
    const diff = { hunks: [hunk(5, [ctx(4, 4), rem(5), rem(6), rem(7), ctx(5, 8)])] };
    const { added, modified, deleted } = deriveLineChanges(diff);
    expect(added.size).toBe(0);
    expect(modified.size).toBe(0);
    expect(arr(deleted)).toEqual([4]);
  });

  it("splits surplus additions into modified + added", () => {
    // One line removed, three added: first is a modification, rest are adds.
    const diff = { hunks: [hunk(2, [ctx(1, 1), rem(2), add(2), add(3), add(4), ctx(5, 3)])] };
    const { added, modified, deleted } = deriveLineChanges(diff);
    expect(arr(modified)).toEqual([2]);
    expect(arr(added)).toEqual([3, 4]);
    expect(deleted.size).toBe(0);
  });

  it("splits surplus removals into modified + a deletion", () => {
    // Three lines removed, one added: one modification, two leftover deletions.
    const diff = { hunks: [hunk(2, [ctx(1, 1), rem(2), rem(3), rem(4), add(2), ctx(3, 5)])] };
    const { added, modified, deleted } = deriveLineChanges(diff);
    expect(arr(modified)).toEqual([2]);
    expect(added.size).toBe(0);
    // The two unmatched removals flush as a deletion below the last new line (2).
    expect(arr(deleted)).toEqual([2]);
  });

  it("marks a deletion at the very top of the file as deleted line 0", () => {
    // Lines removed before line 1, nothing added.
    const diff = { hunks: [hunk(1, [rem(1), rem(2), ctx(1, 3)])] };
    const { deleted } = deriveLineChanges(diff);
    expect(arr(deleted)).toEqual([0]);
  });

  it("handles multiple independent hunks", () => {
    const diff = {
      hunks: [
        hunk(4, [ctx(3, 3), add(4), ctx(5, 4)]),
        hunk(20, [ctx(19, 18), rem(20), add(20), ctx(21, 21)]),
      ],
    };
    const { added, modified, deleted } = deriveLineChanges(diff);
    expect(arr(added)).toEqual([4]);
    expect(arr(modified)).toEqual([20]);
    expect(deleted.size).toBe(0);
  });

  it("flushes a trailing deletion at the end of a hunk", () => {
    // Removals with no following context line — flushed at hunk end.
    const diff = { hunks: [hunk(8, [ctx(7, 7), rem(8), rem(9)])] };
    const { deleted } = deriveLineChanges(diff);
    expect(arr(deleted)).toEqual([7]);
  });
});
