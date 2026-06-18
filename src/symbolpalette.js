// Symbol outline palette (Cmd+Shift+O) — a fuzzy "go to symbol" picker for the
// active editor. Opens instantly from the Lezer syntax tree (collectSymbols),
// then upgrades to the language server's documentSymbol result when LSP is on.
// The pure extractors/normalizers live in symbols.js; this is the palette UI.
import { pushEscape, popEscape } from "./tabs.js";
import { escapeText } from "./domutil.js";
import { collectSymbols, normalizeLspSymbols } from "./symbols.js";
import { getSettings } from "./settings.js";
import { getActiveProject } from "./projects.js";
import { lspDocumentSymbols } from "./lspclient.js";
import { EditorView } from "@codemirror/view";

let symbolOutlineEl = null;
let symbolOutlineState = null; // { view, all, filtered, active }

function ensureSymbolOutline() {
  if (symbolOutlineEl) return symbolOutlineEl;
  const overlay = document.createElement("div");
  overlay.id = "symbol-outline";
  overlay.innerHTML =
    '<div class="so-dialog"><input class="so-input" type="text" placeholder="Go to symbol…" />' +
    '<div class="so-results"></div></div>';
  document.body.appendChild(overlay);

  overlay.addEventListener("click", (e) => {
    if (e.target === overlay) hideSymbolOutline();
  });

  const input = overlay.querySelector(".so-input");
  input.addEventListener("input", () => renderSymbolResults(input.value.trim().toLowerCase()));
  input.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      e.preventDefault();
      hideSymbolOutline();
    } else if (e.key === "Enter") {
      e.preventDefault();
      commitSymbolSelection();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      moveSymbolSelection(1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      moveSymbolSelection(-1);
    }
    e.stopPropagation();
  });

  symbolOutlineEl = overlay;
  return overlay;
}

const SYMBOL_KIND_BADGE = {
  function: "ƒ", fn: "ƒ", method: "ƒ", class: "C", struct: "S",
  enum: "E", trait: "T", impl: "I", mod: "M", heading: "#",
  // Additional kinds surfaced by LSP documentSymbol:
  interface: "I", property: "p", field: "f", variable: "v",
  constant: "c", type: "T", symbol: "•",
};

function renderSymbolResults(query) {
  const st = symbolOutlineState;
  if (!st) return;
  st.filtered = query ? st.all.filter((s) => s.name.toLowerCase().includes(query)) : st.all;
  st.active = 0;
  const results = symbolOutlineEl.querySelector(".so-results");
  if (st.filtered.length === 0) {
    results.innerHTML = '<div class="qo-hint">No symbols</div>';
    return;
  }
  results.innerHTML = st.filtered
    .map((s, i) => {
      const indent = s.kind === "heading" && s.level ? (s.level - 1) * 12 : 0;
      const badge = SYMBOL_KIND_BADGE[s.kind] || "•";
      return (
        `<div class="qo-result so-result ${i === 0 ? "qo-result-active" : ""}" data-idx="${i}" style="padding-left:${16 + indent}px">` +
        `<span class="so-kind">${badge}</span>` +
        `<span class="qo-result-name">${escapeText(s.name)}</span>` +
        `<span class="qo-result-dir">${s.kind}</span></div>`
      );
    })
    .join("");
  results.querySelectorAll(".so-result").forEach((el) => {
    el.addEventListener("click", () => {
      st.active = parseInt(el.dataset.idx, 10);
      commitSymbolSelection();
    });
  });
}

function moveSymbolSelection(dir) {
  const st = symbolOutlineState;
  if (!st || st.filtered.length === 0) return;
  st.active = Math.min(Math.max(st.active + dir, 0), st.filtered.length - 1);
  const rows = [...symbolOutlineEl.querySelectorAll(".so-result")];
  rows.forEach((el, i) => el.classList.toggle("qo-result-active", i === st.active));
  rows[st.active]?.scrollIntoView({ block: "nearest" });
}

function commitSymbolSelection() {
  const st = symbolOutlineState;
  if (!st) return;
  const sym = st.filtered[st.active];
  hideSymbolOutline();
  if (!sym) return;
  const view = st.view;
  view.dispatch({
    selection: { anchor: sym.from },
    effects: EditorView.scrollIntoView(sym.from, { y: "center" }),
  });
  view.focus();
}

function hideSymbolOutline() {
  if (!symbolOutlineEl) return;
  symbolOutlineEl.classList.remove("visible");
  symbolOutlineState = null;
  popEscape(hideSymbolOutline);
}

// Map normalized LSP symbols (0-based line/character) to palette entries with a
// document offset, so they jump the same way syntax-tree symbols do.
function lspSymbolsToPaletteEntries(view, normalized) {
  const doc = view.state.doc;
  return normalized.map((s) => {
    const lineNo = Math.min(Math.max((s.line || 0) + 1, 1), doc.lines);
    const lineObj = doc.line(lineNo);
    const from = Math.min(lineObj.from + (s.character || 0), lineObj.to);
    return { name: s.name, kind: s.kind, from, level: s.level };
  });
}

export function showSymbolOutline(tab) {
  const overlay = ensureSymbolOutline();
  // Open immediately with syntax-tree symbols (instant, no server needed), then
  // upgrade to the language server's documentSymbol result if one's available.
  let symbols;
  try {
    symbols = collectSymbols(tab.editorView.state);
  } catch (_) {
    symbols = [];
  }
  symbolOutlineState = { view: tab.editorView, all: symbols, filtered: symbols, active: 0 };
  overlay.classList.add("visible");
  const input = overlay.querySelector(".so-input");
  input.value = "";
  renderSymbolResults("");
  input.focus();
  pushEscape(hideSymbolOutline);

  if (getSettings().editorLanguageServer) {
    const project = getActiveProject();
    lspDocumentSymbols(tab.filePath, tab.fileName, project?.path)
      .then((raw) => {
        const norm = raw ? normalizeLspSymbols(raw) : [];
        // Only swap in if the palette is still open for this same editor.
        if (!norm.length || !symbolOutlineState || symbolOutlineState.view !== tab.editorView) {
          return;
        }
        symbolOutlineState.all = lspSymbolsToPaletteEntries(tab.editorView, norm);
        renderSymbolResults(input.value.trim().toLowerCase());
      })
      .catch(() => {});
  }
}
