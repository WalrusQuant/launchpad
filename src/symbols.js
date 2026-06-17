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
