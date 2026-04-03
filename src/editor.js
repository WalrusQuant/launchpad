import { EditorView, keymap, lineNumbers, highlightActiveLine, highlightActiveLineGutter } from "@codemirror/view";
import { EditorState } from "@codemirror/state";
import { defaultKeymap, indentWithTab } from "@codemirror/commands";
import { syntaxHighlighting, defaultHighlightStyle, bracketMatching } from "@codemirror/language";
import { oneDark } from "@codemirror/theme-one-dark";
import { javascript } from "@codemirror/lang-javascript";
import { python } from "@codemirror/lang-python";
import { rust } from "@codemirror/lang-rust";
import { html } from "@codemirror/lang-html";
import { css } from "@codemirror/lang-css";
import { json } from "@codemirror/lang-json";
import { markdown } from "@codemirror/lang-markdown";
import { showMinimap } from "@replit/codemirror-minimap";

let editorView = null;
let onChangeCallback = null;

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
};

function getLang(fileName) {
  const ext = fileName.split(".").pop().toLowerCase();
  const langFn = langMap[ext];
  if (!langFn) return [];
  return [langFn()];
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
});

export function createEditor(parentEl, content, fileName, onChange) {
  // Destroy existing editor
  destroyEditor();

  onChangeCallback = onChange;

  const extensions = [
    lineNumbers(),
    highlightActiveLine(),
    highlightActiveLineGutter(),
    bracketMatching(),
    syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
    oneDark,
    launchpadTheme,
    keymap.of([...defaultKeymap, indentWithTab]),
    EditorView.updateListener.of((update) => {
      if (update.docChanged && onChangeCallback) {
        onChangeCallback(getContent());
      }
    }),
    ...getLang(fileName),
    showMinimap.compute(["doc"], (state) => ({
      create: () => {
        const dom = document.createElement("div");
        return { dom };
      },
      displayText: "blocks",
      showOverlay: "mouse-over",
    })),
  ];

  editorView = new EditorView({
    state: EditorState.create({
      doc: content,
      extensions,
    }),
    parent: parentEl,
  });
}

export function getContent() {
  if (!editorView) return "";
  return editorView.state.doc.toString();
}

export function destroyEditor() {
  if (editorView) {
    editorView.destroy();
    editorView = null;
  }
  onChangeCallback = null;
}
