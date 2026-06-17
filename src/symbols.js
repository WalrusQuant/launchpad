import { syntaxTree } from "@codemirror/language";

// In-file symbol outline, derived from the CodeMirror (Lezer) syntax tree —
// no language server required. Node-type names vary per grammar, so we map the
// common "definition" node types across the languages we highlight to a kind
// label, and pull the symbol's name from the first identifier-ish child.
// This is a pragmatic pre-LSP outline: named functions / classes / methods /
// Rust items / Markdown headings. Arrow-assigned consts and other indirect
// definitions are out of scope until the LSP track upgrades this to
// documentSymbol.

const SYMBOL_KINDS = {
  // JavaScript / TypeScript
  FunctionDeclaration: "function",
  ClassDeclaration: "class",
  MethodDeclaration: "method",
  // Python
  FunctionDefinition: "function",
  ClassDefinition: "class",
  // Rust
  FunctionItem: "fn",
  StructItem: "struct",
  EnumItem: "enum",
  TraitItem: "trait",
  ImplItem: "impl",
  ModItem: "mod",
};

// Child node types that carry the defined name, across grammars.
const NAME_TYPES = new Set([
  "VariableDefinition", // JS function/class name
  "PropertyDefinition", // JS method name
  "VariableName", // Python def/class name
  "BoundIdentifier", // Rust fn/struct/etc.
  "Identifier",
  "TypeName", // Rust impl target
  "Name",
]);

function firstNameChild(node, docText) {
  let child = node.firstChild;
  while (child) {
    if (NAME_TYPES.has(child.name)) return docText.slice(child.from, child.to);
    child = child.nextSibling;
  }
  return null;
}

/**
 * Walk a Lezer tree and return a flat, document-ordered list of symbols:
 * `{ name, kind, from, level? }`. `from` is the node's start offset (for
 * jumping); `level` is set for Markdown headings (1-6).
 */
export function extractSymbols(tree, docText) {
  const symbols = [];
  const cursor = tree.cursor();
  do {
    const type = cursor.name;

    // Markdown headings: the node spans the whole line including the leading
    // '#'s, which we strip for the label.
    if (type && type.startsWith("ATXHeading")) {
      const level = parseInt(type.slice("ATXHeading".length), 10) || 1;
      const text = docText.slice(cursor.from, cursor.to).replace(/^#+\s*/, "").trim();
      symbols.push({ name: text || "(heading)", kind: "heading", level, from: cursor.from });
      continue;
    }

    const kind = SYMBOL_KINDS[type];
    if (kind) {
      const name = firstNameChild(cursor.node, docText);
      if (name) symbols.push({ name, kind, from: cursor.node.from });
    }
  } while (cursor.next());
  return symbols;
}

/** Collect symbols for an editor state using its live syntax tree. */
export function collectSymbols(state) {
  return extractSymbols(syntaxTree(state), state.doc.toString());
}

// LSP SymbolKind (1-26) → our short label. Unmapped kinds fall back to "symbol".
const LSP_SYMBOL_KIND = {
  2: "mod", // Module
  3: "mod", // Namespace
  4: "mod", // Package
  5: "class",
  6: "method",
  7: "property",
  8: "field",
  9: "method", // Constructor
  10: "enum",
  11: "interface",
  12: "function",
  13: "variable",
  14: "constant",
  22: "enum", // EnumMember
  23: "struct",
  26: "type", // TypeParameter
};

// Flatten an LSP textDocument/documentSymbol result into the same shape the
// palette uses, but keyed by 0-based line/character (the caller maps those to
// editor offsets, since that needs the live document). Handles both response
// forms: hierarchical DocumentSymbol[] (with selectionRange/range + children)
// and flat SymbolInformation[] (with location). Pure + testable.
export function normalizeLspSymbols(result) {
  const out = [];
  if (!Array.isArray(result)) return out;

  const walk = (nodes, level) => {
    for (const s of nodes) {
      if (!s) continue;
      // SymbolInformation: position lives in location.range.
      if (s.location && !s.selectionRange && !s.range) {
        const start = s.location.range && s.location.range.start;
        if (start) {
          out.push({
            name: s.name,
            kind: LSP_SYMBOL_KIND[s.kind] || "symbol",
            line: start.line,
            character: start.character,
            level: 0,
          });
        }
        continue;
      }
      // DocumentSymbol: prefer the selectionRange (the name) over the full range.
      const range = s.selectionRange || s.range;
      if (!range || !range.start) continue;
      out.push({
        name: s.name,
        kind: LSP_SYMBOL_KIND[s.kind] || "symbol",
        line: range.start.line,
        character: range.start.character,
        level,
      });
      if (Array.isArray(s.children) && s.children.length) {
        walk(s.children, level + 1);
      }
    }
  };

  walk(result, 0);
  return out;
}
