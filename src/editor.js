import { EditorView, keymap, lineNumbers, highlightActiveLine, highlightActiveLineGutter, rectangularSelection, crosshairCursor } from "@codemirror/view";
import { EditorState } from "@codemirror/state";
import { defaultKeymap, indentWithTab } from "@codemirror/commands";
import { syntaxHighlighting, defaultHighlightStyle, bracketMatching, foldGutter, indentOnInput } from "@codemirror/language";
import { oneDark } from "@codemirror/theme-one-dark";
import { search, searchKeymap, highlightSelectionMatches } from "@codemirror/search";
import { autocompletion, closeBrackets, closeBracketsKeymap } from "@codemirror/autocomplete";
import { lintGutter } from "@codemirror/lint";
import { javascript } from "@codemirror/lang-javascript";
import { python } from "@codemirror/lang-python";
import { rust } from "@codemirror/lang-rust";
import { html } from "@codemirror/lang-html";
import { css } from "@codemirror/lang-css";
import { json } from "@codemirror/lang-json";
import { markdown } from "@codemirror/lang-markdown";
import { yaml } from "@codemirror/lang-yaml";
import { StreamLanguage } from "@codemirror/language";
import { toml } from "@codemirror/legacy-modes/mode/toml";
import { shell } from "@codemirror/legacy-modes/mode/shell";

const langMap = {
  js: javascript,
  jsx: () => javascript({ jsx: true }),
  ts: () => javascript({ typescript: true }),
  tsx: () => javascript({ jsx: true, typescript: true }),
  mjs: javascript,
  cjs: javascript,
  py: python,
  rs: rust,
  html: html,
  htm: html,
  css: css,
  scss: css,
  json: json,
  md: markdown,
  mdx: markdown,
  yaml: () => yaml(),
  yml: () => yaml(),
  toml: () => StreamLanguage.define(toml),
  sh: () => StreamLanguage.define(shell),
  bash: () => StreamLanguage.define(shell),
  zsh: () => StreamLanguage.define(shell),
};

const langNames = {
  js: "JavaScript",
  jsx: "JavaScript (JSX)",
  ts: "TypeScript",
  tsx: "TypeScript (TSX)",
  mjs: "JavaScript",
  cjs: "JavaScript",
  py: "Python",
  rs: "Rust",
  html: "HTML",
  htm: "HTML",
  css: "CSS",
  scss: "SCSS",
  json: "JSON",
  md: "Markdown",
  mdx: "Markdown",
  toml: "TOML",
  yaml: "YAML",
  yml: "YAML",
  sh: "Shell",
  bash: "Shell",
  zsh: "Shell",
  txt: "Plain Text",
};

function getLang(fileName) {
  const ext = fileName.split(".").pop().toLowerCase();
  const langFn = langMap[ext];
  if (!langFn) return [];
  return [langFn()];
}

export function getLangName(fileName) {
  const ext = fileName.split(".").pop().toLowerCase();
  return langNames[ext] || "Plain Text";
}

// Custom theme tweaks to match our app
const launchpadTheme = EditorView.theme({
  "&": {
    backgroundColor: "#1e1e1e",
    color: "#ccc",
    fontSize: "12px",
    height: "100%",
  },
  ".cm-content": {
    fontFamily: '"SF Mono", "Menlo", monospace',
    padding: "16px 0",
  },
  ".cm-gutters": {
    backgroundColor: "#1e1e1e",
    color: "#555",
    border: "none",
    paddingLeft: "8px",
  },
  ".cm-activeLineGutter": {
    backgroundColor: "#252525",
  },
  ".cm-activeLine": {
    backgroundColor: "#252525",
  },
  "&.cm-focused .cm-cursor": {
    borderLeftColor: "#e0e0e0",
  },
  "&.cm-focused .cm-selectionBackground, .cm-selectionBackground": {
    backgroundColor: "#444 !important",
  },
  ".cm-scroller": {
    overflow: "auto",
  },
  // Search panel styling
  ".cm-panels": {
    backgroundColor: "#252525",
    borderBottom: "1px solid #333",
  },
  ".cm-search": {
    fontSize: "12px",
  },
  ".cm-search input, .cm-search button": {
    fontFamily: '"SF Mono", "Menlo", monospace',
    fontSize: "11px",
  },
  // Fold gutter
  ".cm-foldGutter .cm-gutterElement": {
    cursor: "pointer",
    color: "#666",
  },
  // Autocomplete
  ".cm-tooltip-autocomplete": {
    backgroundColor: "#252525",
    border: "1px solid #444",
  },
  ".cm-tooltip-autocomplete ul li": {
    color: "#ccc",
  },
  ".cm-tooltip-autocomplete ul li[aria-selected]": {
    backgroundColor: "#333",
    color: "#fff",
  },
});

/**
 * Create a CodeMirror editor instance.
 * Returns the EditorView — caller owns its lifecycle.
 */
export function createEditor(parentEl, content, fileName, { onChange, onCursorChange, tabSize, wordWrap } = {}) {
  const extensions = [
    lineNumbers(),
    highlightActiveLine(),
    highlightActiveLineGutter(),
    bracketMatching(),
    closeBrackets(),
    foldGutter(),
    indentOnInput(),
    autocompletion(),
    lintGutter(),
    highlightSelectionMatches(),
    rectangularSelection(),
    crosshairCursor(),
    syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
    oneDark,
    launchpadTheme,
    keymap.of([...closeBracketsKeymap, ...defaultKeymap, ...searchKeymap, indentWithTab]),
    search(),
    ...getLang(fileName),
    EditorState.tabSize.of(tabSize || 2),
    ...(wordWrap ? [EditorView.lineWrapping] : []),
    EditorView.updateListener.of((update) => {
      if (update.docChanged && onChange) {
        onChange(update.view.state.doc.toString());
      }
      if (update.selectionSet && onCursorChange) {
        const pos = update.state.selection.main.head;
        const line = update.state.doc.lineAt(pos);
        onCursorChange(line.number, pos - line.from + 1);
      }
    }),
  ];

  return new EditorView({
    state: EditorState.create({
      doc: content,
      extensions,
    }),
    parent: parentEl,
  });
}
