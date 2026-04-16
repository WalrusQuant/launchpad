# Code Editor Improvements

### Already Good
- CM6 with syntax highlighting, line numbers, bracket matching, search
- Multi-language support (JS/TS/Python/Rust/HTML/CSS/JSON/Markdown)
- Cursor position tracking, save/dirty detection, breadcrumbs

### High-Impact Improvements

1. **✅ Autocomplete / IntelliSense** — `@codemirror/autocomplete` with `autocompletion()` for keyword completions per language.

2. **✅ Linting / Diagnostics** — `@codemirror/lint` with `lintGutter()` infrastructure. Actual lint sources (eslint, ruff) to be wired via Rust backend later.

3. **Language Server Protocol (LSP)** — Deferred. Connect to real language servers (typescript-language-server, rust-analyzer, pyright) via the Rust backend for go-to-definition, hover info, references, rename, and real completions.

4. **✅ Missing language support** — YAML via `@codemirror/lang-yaml`, TOML and Shell via `@codemirror/legacy-modes` with StreamLanguage. All three now have syntax highlighting.

5. **✅ Fold / Code folding** — `foldGutter()` and `indentOnInput()` added.

6. **✅ Multiple cursors** — `highlightSelectionMatches()`, `rectangularSelection()`, `crosshairCursor()` added. Alt+click for multi-cursor, column selection support.

7. **✅ Close-brackets / Auto-closing** — `closeBrackets()` + `closeBracketsKeymap` for auto-closing brackets, quotes, backticks.

8. **Minimap** — Deferred. No stable CM6 minimap package available. `@replit/codemirror-minimap` exists but is outdated (2023). Will revisit with LSP session.

9. **✅ File change detection** — Open editor tabs now detect when files change on disk via `fs-changed` events. Silently reloads if no local edits; prompts with confirm dialog if user has unsaved changes.

10. **✅ Undo/redo stack display** — Status bar shows `Undo: N Redo: N` alongside cursor position when undo history exists.

### Quick Wins (all completed)
- ✅ `closeBrackets()` + `closeBracketsKeymap` — auto-close brackets
- ✅ `highlightSelectionMatches()` — highlight word at cursor
- ✅ `foldGutter()` — code folding UI
- ✅ `indentOnInput()` — auto-indent on Enter
- ✅ `@codemirror/lang-yaml` + legacy modes for TOML/Shell
- ✅ `rectangularSelection()` + `crosshairCursor()` — column selection
