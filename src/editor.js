import { EditorView, keymap, lineNumbers, highlightActiveLine, highlightActiveLineGutter, rectangularSelection, crosshairCursor } from "@codemirror/view";
import { EditorState, Compartment } from "@codemirror/state";
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
import { vim } from "@replit/codemirror-vim";

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

// Custom theme tweaks to match our app — colors driven by CSS variables so
// they flip with the app theme.
const launchpadTheme = EditorView.theme({
  "&": {
    backgroundColor: "var(--surface-4)",
    color: "var(--text-8)",
    fontSize: "12px",
    height: "100%",
  },
  ".cm-content": {
    fontFamily: '"SF Mono", "Menlo", monospace',
    padding: "16px 0",
    caretColor: "var(--text-11)",
  },
  ".cm-gutters": {
    backgroundColor: "var(--surface-4)",
    color: "var(--text-0)",
    border: "none",
    paddingLeft: "8px",
  },
  ".cm-activeLineGutter": {
    backgroundColor: "var(--surface-7)",
  },
  ".cm-activeLine": {
    backgroundColor: "var(--surface-7)",
  },
  "&.cm-focused .cm-cursor": {
    borderLeftColor: "var(--text-11)",
  },
  "&.cm-focused .cm-selectionBackground, .cm-selectionBackground": {
    backgroundColor: "var(--border-3) !important",
  },
  ".cm-scroller": {
    overflow: "auto",
  },
  // Search panel styling
  ".cm-panels": {
    backgroundColor: "var(--surface-7)",
    color: "var(--text-8)",
    borderBottom: "1px solid var(--border-1)",
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
    color: "var(--text-1)",
  },
  // Autocomplete
  ".cm-tooltip-autocomplete": {
    backgroundColor: "var(--surface-7)",
    border: "1px solid var(--border-3)",
  },
  ".cm-tooltip-autocomplete ul li": {
    color: "var(--text-8)",
  },
  ".cm-tooltip-autocomplete ul li[aria-selected]": {
    backgroundColor: "var(--border-1)",
    color: "var(--text-12)",
  },
});

/**
 * Create a CodeMirror editor instance.
 * Returns { view, setTabSize, setWordWrap } — caller owns the view's lifecycle.
 */
export function createEditor(parentEl, content, fileName, { onChange, onCursorChange, tabSize, wordWrap, vimMode, theme } = {}) {
  const isLight = theme === "light";
  const tabSizeCompartment = new Compartment();
  const wrapCompartment = new Compartment();

  const extensions = [
    ...(vimMode ? [vim()] : []),
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
    ...(isLight ? [] : [oneDark]),
    launchpadTheme,
    keymap.of([...closeBracketsKeymap, ...defaultKeymap, ...searchKeymap, indentWithTab]),
    search(),
    ...getLang(fileName),
    EditorState.allowMultipleSelections.of(true),
    tabSizeCompartment.of(EditorState.tabSize.of(tabSize || 2)),
    wrapCompartment.of(wordWrap ? EditorView.lineWrapping : []),
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

  const view = new EditorView({
    state: EditorState.create({
      doc: content,
      extensions,
    }),
    parent: parentEl,
  });

  return {
    view,
    setTabSize(n) {
      view.dispatch({ effects: tabSizeCompartment.reconfigure(EditorState.tabSize.of(n)) });
    },
    setWordWrap(on) {
      view.dispatch({ effects: wrapCompartment.reconfigure(on ? EditorView.lineWrapping : []) });
    },
  };
}
