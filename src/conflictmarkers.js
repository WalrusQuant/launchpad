// Conflict-marker parser + CodeMirror extension. Used by the editor when
// opening a file that's listed as `conflict` in git status. Strict parser
// avoids false positives on source code that legitimately contains marker
// glyphs (e.g. `"<<<<<<< HEAD"` inside a string literal in a test fixture).
//
// Block shape:
//   { start, end             — doc offsets of the full conflict block
//     openMarkerLine,        — line number of `<<<<<<<`
//     baseMarkerLine?,       — line number of `|||||||` (diff3 only)
//     midMarkerLine,         — line number of `=======`
//     closeMarkerLine,       — line number of `>>>>>>>`
//     oursText,              — content between open and (base|mid)
//     baseText?,             — content between base and mid (diff3 only)
//     theirsText             — content between mid and close
//   }

import { StateField } from "@codemirror/state";
import { EditorView, Decoration, WidgetType } from "@codemirror/view";

// True when `line` is a marker line for `char` (one of `<`, `|`, `=`, `>`).
// Strict: exactly 7 of the char at column 0, followed by EOL or a single
// space + label. Anything else (e.g. 6, 8, mixed chars, leading whitespace)
// is not a marker — keeps source code containing `"<<<<<<< HEAD"` in a
// string literal from being treated as a conflict block.
function isMarkerLine(line, char) {
  if (line.length < 7) return false;
  for (let i = 0; i < 7; i++) {
    if (line[i] !== char) return false;
  }
  if (line.length === 7) return true;
  return line[7] === " ";
}

// Walk lines and build a sequence of fully-formed conflict blocks. Any
// unmatched marker (e.g. a stray `<<<<<<<` with no later `=======` or
// `>>>>>>>`) is silently dropped — only complete `<<<` → (`|||`)? → `===`
// → `>>>` sequences become blocks.
export function parseConflictBlocks(text) {
  const lines = text.split("\n");
  const lineOffsets = new Array(lines.length);
  let off = 0;
  for (let i = 0; i < lines.length; i++) {
    lineOffsets[i] = off;
    off += lines[i].length + 1; // +1 for the \n separator
  }
  // Position past the last line's newline. For files that don't end in \n
  // this overshoots by 1; we clamp to text.length when emitting `end`.
  const docLength = text.length;

  const blocks = [];
  let i = 0;

  while (i < lines.length) {
    if (!isMarkerLine(lines[i], "<")) {
      i++;
      continue;
    }

    // Scan forward looking for === (with optional ||| in between) and >>>.
    // Bail out (without consuming) if we hit a nested <<< before completing.
    const openIdx = i;
    let baseIdx = -1; // line number of |||||||
    let midIdx = -1;  // line number of =======
    let closeIdx = -1; // line number of >>>>>>>

    let j = i + 1;
    let nestedOpenIdx = -1;
    while (j < lines.length) {
      if (isMarkerLine(lines[j], "<")) {
        // Nested open — bail on this outer block but remember the inner
        // open so the next iteration starts there (not at i+1, which would
        // re-scan empty content and miss the inner block entirely).
        nestedOpenIdx = j;
        break;
      }
      if (isMarkerLine(lines[j], "|")) {
        if (baseIdx !== -1) break; // already saw one ||| — malformed
        if (midIdx !== -1) break;  // ||| after === is malformed
        baseIdx = j;
        j++;
        continue;
      }
      if (isMarkerLine(lines[j], "=")) {
        if (midIdx !== -1) break; // already saw === — malformed
        midIdx = j;
        j++;
        continue;
      }
      if (isMarkerLine(lines[j], ">")) {
        if (midIdx === -1) break; // >>> with no preceding === — malformed
        closeIdx = j;
        break;
      }
      j++;
    }

    if (closeIdx === -1 || midIdx === -1) {
      // Malformed, unterminated, or hit a nested <<<. When a nested open was
      // detected, jump TO it so it parses as a block on the next iteration —
      // not past it, which would leave the inner block invisible.
      i = nestedOpenIdx !== -1 ? nestedOpenIdx : i + 1;
      continue;
    }

    // The ours range is from line after open up to baseIdx-1 (when present)
    // else midIdx-1. theirs is midIdx+1 .. closeIdx-1.
    const oursEndLineExclusive = baseIdx !== -1 ? baseIdx : midIdx;
    const baseEndLineExclusive = midIdx;

    const oursText = lines.slice(openIdx + 1, oursEndLineExclusive).join("\n");
    const baseText = baseIdx !== -1
      ? lines.slice(baseIdx + 1, baseEndLineExclusive).join("\n")
      : undefined;
    const theirsText = lines.slice(midIdx + 1, closeIdx).join("\n");

    const start = lineOffsets[openIdx];
    const closeLineEnd = lineOffsets[closeIdx] + lines[closeIdx].length;
    // Include the newline after >>>>>>> if it exists. `hadTrailingNewline`
    // tells `replaceBlock` whether the original file ended the block with a
    // \n (so the replacement should preserve it) or not (so we mustn't
    // invent one — that'd silently change the file's last byte).
    const hadTrailingNewline = closeLineEnd + 1 <= docLength;
    const end = hadTrailingNewline ? closeLineEnd + 1 : closeLineEnd;

    blocks.push({
      start,
      end,
      hadTrailingNewline,
      openMarkerLine: openIdx,
      baseMarkerLine: baseIdx !== -1 ? baseIdx : undefined,
      midMarkerLine: midIdx,
      closeMarkerLine: closeIdx,
      oursText,
      baseText,
      theirsText,
    });

    i = closeIdx + 1;
  }

  return blocks;
}

// Returns a CodeMirror change spec replacing the entire block with `replacement`.
// The replacement always ends with a newline if the block was followed by one
// (we just took it as part of `end`), so the surrounding text stays intact.
export function replaceBlock(block, choice) {
  let replacement;
  switch (choice) {
    case "ours":
      replacement = block.oursText;
      break;
    case "theirs":
      replacement = block.theirsText;
      break;
    case "both":
      // Concat with a single newline separator so neither side is glued onto
      // the other. Empty sides are still preserved as empty strings.
      replacement = block.oursText + (block.oursText && block.theirsText ? "\n" : "") + block.theirsText;
      break;
    default:
      replacement = typeof choice === "string" ? choice : "";
  }
  // Preserve the file's existing newline policy. If the block was followed
  // by \n in the original, re-append one (we consumed it as part of `end`).
  // If the file had NO trailing newline (block was at EOF without \n), don't
  // invent one — that would silently change the last byte of the file.
  if (block.hadTrailingNewline && replacement.length > 0 && !replacement.endsWith("\n")) {
    replacement += "\n";
  }
  return { from: block.start, to: block.end, insert: replacement };
}

// Widget that replaces a conflict block's open marker line with an action
// bar. Constructed per-block so the click handlers know which block to
// rewrite. CodeMirror compares widgets via `eq()` to decide whether the
// existing DOM can be reused — we treat block identity as the start offset
// so a doc edit that doesn't change the block's start position keeps the
// existing DOM (no flicker).
class ConflictActionBarWidget extends WidgetType {
  constructor(block, phase6Available) {
    super();
    this.block = block;
    this.phase6Available = phase6Available;
  }
  eq(other) {
    return other instanceof ConflictActionBarWidget
      && other.block.start === this.block.start
      && other.block.end === this.block.end
      && other.phase6Available === this.phase6Available;
  }
  toDOM() {
    const wrap = document.createElement("div");
    wrap.className = "conflict-action-bar";

    const label = document.createElement("span");
    label.className = "conflict-action-label";
    label.textContent = this.block.baseMarkerLine != null ? "⟦ Conflict (diff3) ⟧" : "⟦ Conflict ⟧";
    wrap.appendChild(label);

    const mkBtn = (text, choice) => {
      const b = document.createElement("button");
      b.className = "conflict-action-btn";
      b.dataset.choice = choice;
      b.textContent = text;
      return b;
    };
    wrap.appendChild(mkBtn("Accept Ours", "ours"));
    wrap.appendChild(mkBtn("Accept Theirs", "theirs"));
    wrap.appendChild(mkBtn("Accept Both", "both"));
    if (this.phase6Available) {
      wrap.appendChild(mkBtn("Open 3-Way", "three-way"));
    }
    return wrap;
  }
  ignoreEvent() {
    // Let CodeMirror see clicks so editor focus management still works,
    // but the actual action handler is wired at the view level (see below).
    return false;
  }
}

// Subtle separator widget for `=======` and `>>>>>>>` lines. Reusing one
// widget instance via a shared key means CodeMirror reuses DOM aggressively.
class ConflictSeparatorWidget extends WidgetType {
  constructor(kind) {
    super();
    this.kind = kind; // "base" | "mid" | "close"
  }
  eq(other) {
    return other instanceof ConflictSeparatorWidget && other.kind === this.kind;
  }
  toDOM() {
    const wrap = document.createElement("div");
    wrap.className = `conflict-separator conflict-separator-${this.kind}`;
    wrap.textContent = this.kind === "mid" ? "─── then ───"
      : this.kind === "base" ? "─── base ───"
      : "─── end ───";
    return wrap;
  }
}

// Build the decoration set for the current document state.
function buildConflictDecorations(state, phase6Available) {
  const blocks = parseConflictBlocks(state.doc.toString());
  if (blocks.length === 0) return Decoration.none;

  const ranges = [];
  for (const b of blocks) {
    const openLine = state.doc.line(b.openMarkerLine + 1);
    const openTo = Math.min(state.doc.length, openLine.to + 1);
    ranges.push(
      Decoration.replace({
        widget: new ConflictActionBarWidget(b, phase6Available),
        block: true,
      }).range(openLine.from, openTo)
    );

    // Optional base marker (diff3 only)
    if (b.baseMarkerLine != null) {
      const baseLine = state.doc.line(b.baseMarkerLine + 1);
      const baseTo = Math.min(state.doc.length, baseLine.to + 1);
      ranges.push(
        Decoration.replace({
          widget: new ConflictSeparatorWidget("base"),
          block: true,
        }).range(baseLine.from, baseTo)
      );
    }

    // Mid marker (=======)
    const midLine = state.doc.line(b.midMarkerLine + 1);
    const midTo = Math.min(state.doc.length, midLine.to + 1);
    ranges.push(
      Decoration.replace({
        widget: new ConflictSeparatorWidget("mid"),
        block: true,
      }).range(midLine.from, midTo)
    );

    // Close marker (>>>>>>>)
    const closeLine = state.doc.line(b.closeMarkerLine + 1);
    const closeTo = Math.min(state.doc.length, closeLine.to + 1);
    ranges.push(
      Decoration.replace({
        widget: new ConflictSeparatorWidget("close"),
        block: true,
      }).range(closeLine.from, closeTo)
    );

    // Background tints on ours / base / theirs ranges. We compute these from
    // the line numbers on either side of the marker lines we just replaced.
    const oursStartLine = b.openMarkerLine + 1; // line after <<<
    const oursEndLineExcl = b.baseMarkerLine != null ? b.baseMarkerLine : b.midMarkerLine;
    if (oursEndLineExcl > oursStartLine) {
      const startPos = state.doc.line(oursStartLine + 1).from;
      const endPos = state.doc.line(oursEndLineExcl).to;
      if (endPos > startPos) {
        ranges.push(Decoration.mark({ class: "cm-conflict-ours" }).range(startPos, endPos));
      }
    }
    if (b.baseMarkerLine != null) {
      const baseStartLine = b.baseMarkerLine + 1;
      const baseEndLineExcl = b.midMarkerLine;
      if (baseEndLineExcl > baseStartLine) {
        const startPos = state.doc.line(baseStartLine + 1).from;
        const endPos = state.doc.line(baseEndLineExcl).to;
        if (endPos > startPos) {
          ranges.push(Decoration.mark({ class: "cm-conflict-base" }).range(startPos, endPos));
        }
      }
    }
    const theirsStartLine = b.midMarkerLine + 1;
    const theirsEndLineExcl = b.closeMarkerLine;
    if (theirsEndLineExcl > theirsStartLine) {
      const startPos = state.doc.line(theirsStartLine + 1).from;
      const endPos = state.doc.line(theirsEndLineExcl).to;
      if (endPos > startPos) {
        ranges.push(Decoration.mark({ class: "cm-conflict-theirs" }).range(startPos, endPos));
      }
    }
  }

  // Decoration.set requires sorted ranges. We constructed them in document
  // order but mark/replace may interleave; ask CodeMirror to sort.
  return Decoration.set(ranges, /*sort=*/true);
}

// Public extension factory. `onResolveAll` (currently unused — Phase 4
// auto-stage lives in main.js's save flow) is reserved for future hooks.
// `phase6Available` toggles the "Open 3-Way" button. When the user clicks
// it, `onOpenThreeWay` is invoked (no args — the caller already knows the
// file path because it bound this callback to its own editor tab).
export function conflictExtension({ onResolveAll: _unused, phase6Available = false, onOpenThreeWay } = {}) {
  const field = StateField.define({
    create(state) {
      return buildConflictDecorations(state, phase6Available);
    },
    update(deco, tr) {
      if (tr.docChanged) return buildConflictDecorations(tr.state, phase6Available);
      return deco;
    },
    provide: (f) => EditorView.decorations.from(f),
  });

  // Click handler at the view level — clicks bubble up from the widget's
  // <button>, we read data-choice and apply the change. Keeping the handler
  // off the widget node lets WidgetType.eq() reuse DOM across edits.
  const clickHandler = EditorView.domEventHandlers({
    click(event, view) {
      const btn = event.target instanceof HTMLElement
        ? event.target.closest(".conflict-action-btn")
        : null;
      if (!btn) return false;
      const choice = btn.dataset.choice;
      if (!choice) return false;
      // "three-way" doesn't rewrite the block — it opens the 3-pane merge
      // tab via the parent-supplied callback. Falls through to a no-op if
      // the host didn't wire one up.
      if (choice === "three-way") {
        if (typeof onOpenThreeWay === "function") {
          onOpenThreeWay();
          event.preventDefault();
          event.stopPropagation();
          return true;
        }
        return false;
      }
      // Find the block this button belongs to: walk back to the nearest
      // .conflict-action-bar, then locate by its position in the doc.
      const bar = btn.closest(".conflict-action-bar");
      if (!bar) return false;
      // Use coordsAtPos / posAtDOM to figure out the offset.
      const pos = view.posAtDOM(bar);
      if (pos < 0) return false;
      const blocks = parseConflictBlocks(view.state.doc.toString());
      const block = blocks.find((b) => b.start <= pos && pos < b.end);
      if (!block) return false;
      view.dispatch({ changes: replaceBlock(block, choice) });
      event.preventDefault();
      event.stopPropagation();
      return true;
    },
  });

  return [field, clickHandler];
}
