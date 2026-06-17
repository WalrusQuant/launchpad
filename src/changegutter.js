import { gutter, GutterMarker, keymap, EditorView } from "@codemirror/view";
import { StateField, StateEffect, RangeSetBuilder, RangeSet } from "@codemirror/state";

// Change gutter: paints a per-line bar showing how each line differs from HEAD
// (added / modified) plus a wedge where lines were deleted. The classification
// is derived purely from a FileDiff (the shape returned by the Rust
// get_file_diff_vs_head command), which keeps the interesting logic testable
// without a live editor.

/**
 * Classify each line of a FileDiff (working-tree-vs-HEAD) as added, modified,
 * or sitting just above a deletion.
 *
 * Returns `{ added, modified, deleted }` where each is a Set of NEW-file line
 * numbers (1-based). `deleted` holds the line number *after which* one or more
 * lines were removed (0 means "before the first line").
 *
 * Heuristic (the standard gitgutter/dirty-diff approach): within a hunk,
 * removed lines accumulate; the additions that immediately follow consume them
 * as "modified" pairs, any surplus additions are "added", and any surplus
 * removals — flushed at the next context line or the hunk's end — become a
 * deletion marker on the last real line seen.
 */
export function deriveLineChanges(fileDiff) {
  const added = new Set();
  const modified = new Set();
  const deleted = new Set();
  if (!fileDiff || !Array.isArray(fileDiff.hunks)) {
    return { added, modified, deleted };
  }

  for (const hunk of fileDiff.hunks) {
    let pendingRemovals = 0;
    // The last line that exists on the new side, so a flushed deletion can
    // attach "below" it. Start just above the hunk.
    let lastNewLine = (hunk.new_start || 1) - 1;

    for (const line of hunk.lines || []) {
      if (line.origin === "remove") {
        pendingRemovals++;
      } else if (line.origin === "add") {
        const ln = line.new_line_no;
        if (pendingRemovals > 0) {
          modified.add(ln);
          pendingRemovals--;
        } else {
          added.add(ln);
        }
        lastNewLine = ln;
      } else {
        // context line — flush any unmatched removals as a deletion above here
        if (pendingRemovals > 0) {
          deleted.add(lastNewLine);
          pendingRemovals = 0;
        }
        lastNewLine = line.new_line_no;
      }
    }
    if (pendingRemovals > 0) deleted.add(lastNewLine);
  }

  return { added, modified, deleted };
}

class ChangeMarker extends GutterMarker {
  constructor(kind) {
    super();
    this.kind = kind;
    this.elementClass = `cm-change cm-change-${kind}`;
  }
  eq(other) {
    return other instanceof ChangeMarker && other.kind === this.kind;
  }
}

const addedMarker = new ChangeMarker("added");
const modifiedMarker = new ChangeMarker("modified");
const deletedMarker = new ChangeMarker("deleted");

// Effect carrying a fresh { added, modified, deleted } classification.
export const setLineChanges = StateEffect.define();

// Build a RangeSet of gutter markers from a classification, clamped to the
// document's current line count (a stale classification from before an edit
// must not address a line that no longer exists).
function buildMarkers(state, changes) {
  const lineCount = state.doc.lines;
  const perLine = new Map();

  const place = (ln, marker) => {
    if (ln >= 1 && ln <= lineCount && !perLine.has(ln)) perLine.set(ln, marker);
  };
  // Added / modified take precedence over a deletion wedge on the same line.
  for (const ln of changes.added) place(ln, addedMarker);
  for (const ln of changes.modified) place(ln, modifiedMarker);
  for (const ln of changes.deleted) place(ln < 1 ? 1 : ln, deletedMarker);

  const builder = new RangeSetBuilder();
  for (const ln of [...perLine.keys()].sort((a, b) => a - b)) {
    const pos = state.doc.line(ln).from;
    builder.add(pos, pos, perLine.get(ln));
  }
  return builder.finish();
}

const changeField = StateField.define({
  create() {
    return RangeSet.empty;
  },
  update(value, tr) {
    for (const e of tr.effects) {
      if (e.is(setLineChanges)) return buildMarkers(tr.state, e.value);
    }
    // Keep markers roughly aligned as the user edits between refreshes.
    return value.map(tr.changes);
  },
});

// Whether a given 1-based line currently carries a change marker. Lets the
// click handler ignore clicks on unchanged lines in the gutter strip.
export function lineHasChange(state, lineNo) {
  const set = state.field(changeField, false);
  if (!set || lineNo < 1 || lineNo > state.doc.lines) return false;
  const pos = state.doc.line(lineNo).from;
  let hit = false;
  set.between(pos, pos, () => { hit = true; return false; });
  return hit;
}

// Line-start positions of every marked line, in document order. Drives hunk
// navigation; reads straight off the gutter's RangeSet so it always matches
// what's painted.
function changedPositions(state) {
  const set = state.field(changeField, false);
  if (!set) return [];
  const positions = [];
  const iter = set.iter();
  while (iter.value) {
    positions.push(iter.from);
    iter.next();
  }
  return positions;
}

function jumpToChange(view, dir) {
  const positions = changedPositions(view.state);
  if (!positions.length) return false;
  const curLineStart = view.state.doc.lineAt(view.state.selection.main.head).from;

  let target;
  if (dir > 0) {
    target = positions.find((p) => p > curLineStart);
    if (target === undefined) target = positions[0]; // wrap to first
  } else {
    const before = positions.filter((p) => p < curLineStart);
    target = before.length ? before[before.length - 1] : positions[positions.length - 1];
  }
  view.dispatch({
    selection: { anchor: target },
    effects: EditorView.scrollIntoView(target, { y: "center" }),
  });
  return true;
}

/** Move the cursor to the next changed line (wraps at end). */
export const goToNextChange = (view) => jumpToChange(view, 1);
/** Move the cursor to the previous changed line (wraps at start). */
export const goToPrevChange = (view) => jumpToChange(view, -1);

const changeNavKeymap = keymap.of([
  { key: "Alt-j", run: goToNextChange },
  { key: "Alt-k", run: goToPrevChange },
]);

/**
 * Editor extension that renders the change gutter and wires hunk navigation
 * (Alt-j / Alt-k). Pair with `updateChangeGutter` to push a fresh
 * classification after open / save / external change.
 *
 * `onMarkerClick(view, lineNo, event)` fires when a marked line's gutter cell
 * is clicked (clicks on unchanged lines are ignored) — the host uses it to pop
 * the revert / stage menu.
 */
export function changeGutter({ onMarkerClick } = {}) {
  const view = gutter({
    class: "cm-change-gutter",
    markers: (v) => v.state.field(changeField),
    domEventHandlers: onMarkerClick
      ? {
          mousedown(v, line, event) {
            const lineNo = v.state.doc.lineAt(line.from).number;
            if (!lineHasChange(v.state, lineNo)) return false;
            onMarkerClick(v, lineNo, event);
            return true;
          },
        }
      : undefined,
  });
  return [changeField, view, changeNavKeymap];
}

/** Push a fresh { added, modified, deleted } classification into the view. */
export function updateChangeGutter(view, changes) {
  view.dispatch({ effects: setLineChanges.of(changes) });
}
