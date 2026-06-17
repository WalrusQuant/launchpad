import { gutter, GutterMarker } from "@codemirror/view";
import { StateField, StateEffect, RangeSetBuilder, RangeSet } from "@codemirror/state";

// Opt-in blame gutter: a thin left margin showing, per commit-hunk, the short
// OID and how long ago that line was last touched. Rendered only on the first
// line of each blame hunk (continuation lines stay blank, like `git blame`'s
// default grouping). The field is always present when the change gutter is;
// visibility is toggled by adding/removing the gutter view via a compartment in
// the editor factory, so the data can persist while the column is hidden.

export function formatBlameAge(unixSeconds) {
  const diff = Math.floor(Date.now() / 1000) - unixSeconds;
  if (diff < 60) return "now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  if (diff < 604800) return `${Math.floor(diff / 86400)}d`;
  if (diff < 2592000) return `${Math.floor(diff / 604800)}w`;
  if (diff < 31536000) return `${Math.floor(diff / 2592000)}mo`;
  return `${Math.floor(diff / 31536000)}y`;
}

const isUncommitted = (oid) => !oid || /^0+$/.test(oid);

// Effect carrying { hunks, onClick } or null to clear.
export const setBlame = StateEffect.define();

class BlameMarker extends GutterMarker {
  constructor(info, onClick) {
    super();
    this.info = info;
    this.onClick = onClick;
  }
  eq(other) {
    return (
      other instanceof BlameMarker &&
      other.info.oid === this.info.oid &&
      other.info.start_line === this.info.start_line
    );
  }
  toDOM() {
    const el = document.createElement("div");
    el.className = "cm-blame-entry";
    const uncommitted = isUncommitted(this.info.oid);
    el.title = uncommitted
      ? "Not yet committed"
      : `${this.info.oid} · ${this.info.author} · ${this.info.summary}`;

    const oid = document.createElement("span");
    oid.className = "cm-blame-oid";
    oid.textContent = uncommitted ? "·······" : this.info.oid;

    const age = document.createElement("span");
    age.className = "cm-blame-age";
    age.textContent = uncommitted ? "" : formatBlameAge(this.info.timestamp);

    el.appendChild(oid);
    el.appendChild(age);

    if (!uncommitted && this.onClick) {
      el.classList.add("cm-blame-clickable");
      el.addEventListener("mousedown", (e) => {
        e.preventDefault();
        this.onClick(this.info.oid);
      });
    }
    return el;
  }
}

function buildBlame(state, payload) {
  if (!payload || !Array.isArray(payload.hunks)) return RangeSet.empty;
  const lineCount = state.doc.lines;
  const builder = new RangeSetBuilder();
  // Hunks arrive in file order from libgit2; guard anyway so RangeSetBuilder
  // never sees an out-of-order add.
  const sorted = [...payload.hunks].sort((a, b) => a.start_line - b.start_line);
  for (const h of sorted) {
    if (h.start_line >= 1 && h.start_line <= lineCount) {
      const pos = state.doc.line(h.start_line).from;
      builder.add(pos, pos, new BlameMarker(h, payload.onClick));
    }
  }
  return builder.finish();
}

export const blameField = StateField.define({
  create() {
    return RangeSet.empty;
  },
  update(value, tr) {
    for (const e of tr.effects) {
      if (e.is(setBlame)) return buildBlame(tr.state, e.value);
    }
    return value.map(tr.changes);
  },
});

export const blameGutterView = gutter({
  class: "cm-blame-gutter",
  markers: (view) => view.state.field(blameField, false) || RangeSet.empty,
});
