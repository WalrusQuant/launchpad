# Editor Visual Verification — before release

These are the changes that are **logic-tested but not yet verified on a live GUI**
(built in a headless cloud environment). Run `npx tauri dev` and walk this list
before cutting a release. Everything below is shipped to `main`.

Checkbox legend: `[ ]` to verify · `[x]` verified-good · `[!]` found a problem.

---

## Phase 3 — Language server (opt-in)
**Setup:** Settings → Editor → turn **Language Server** on. Servers must be on
your `PATH`: `typescript-language-server` (JS/TS), `rust-analyzer` (Rust),
`pyright` (Python). Backend protocol is e2e-proven; this is the UI layer.

- [ ] Open a `.ts`/`.js` file with a type error → red squiggles appear in the editor + lint gutter
- [ ] Completion is context-aware (server-driven), not just word-matching
- [ ] Hover over a symbol → type/doc tooltip
- [ ] Go-to-definition (Cmd+click or F12) jumps to the def, opening the target file
- [ ] Signature help shows param hints while typing a call
- [ ] Repeat the above in a `.rs` file (rust-analyzer) and a `.py` file (pyright)
- [ ] Symbol outline (Cmd+Shift+O) opens instantly, then swaps in the richer LSP list; selecting a symbol jumps to it
- [ ] Switch projects / close the window → no orphaned language-server processes left running (check Activity Monitor)
- [ ] With the setting **off** (default), no server process spawns at all

## Phase 1 — Git-aware editor
**Setup:** open a tracked file in a repo with some uncommitted changes.

- [ ] Change gutter shows per-line bars: added (green) / modified (warm) / deleted (red wedge), matching the file-tree colors and the right lines
- [ ] Stage a hunk in the git panel → the gutter marker still shows (reflects disk-vs-HEAD, not vs-index)
- [ ] Alt+J / Alt+K jump to next / previous changed hunk
- [ ] Click a marker → popover with **Revert hunk** / **Stage file**
  - [ ] Revert hunk restores HEAD content correctly for a **modification**
  - [ ] Revert hunk correct for a **pure-deletion** hunk (lines come back in the right place)
  - [ ] Revert hunk correct for an **end-of-file** hunk (no stray/missing trailing newline)
  - [ ] Revert is disabled/gated when the tab has unsaved edits
  - [ ] Stage file works and the git panel refreshes
- [ ] Blame toggle (status bar) → left margin shows short OID + age; hover shows author + summary; click opens that commit's diff
- [ ] After committing, the gutter clears for the now-committed lines
- [ ] A file opened from outside the repo → gutter no-ops (no errors)

## Phase 2 — Polish
- [ ] Indent guides render (toggle `editorIndentGuides`)
- [ ] Format on save (`editorFormatOnSave` + formatter on PATH): save a JS/TS/Rust/Py file → it reformats, buffer updates, cursor stays put. A missing formatter just toasts, doesn't block the save.
- [ ] Reveal active file in tree (`editorFollowActiveFile`): switching editor tabs expands + scrolls the sidebar to that file
- [ ] (Paths with spaces) LSP + reveal still work for a project under a path containing a space

## Phase 0 / older debt
- [ ] `editorFontSize`: change it in Settings → already-open editors resize live
- [ ] **Issue #3**: split the workspace (Cmd+\), click back and forth between the two terminal groups → the just-left terminal does **not** blank out (WebGL context-loss fix). Close issue #3 if good.

## Merge tab (touched by the font/indent live-apply work)
- [ ] Open a 3-pane merge tab on a conflict → all three panes render; font size and indent-guide settings apply

---

## Notes
- All of the above is behind quiet defaults or normal use — nothing here changes
  behavior unless you opt in or have changes to show.
- Backend coverage that's already green and needs no GUI: 129 Rust unit tests,
  4 real-server LSP e2e probes (`cargo test -- --ignored lsp_e2e`), 50 JS tests.
- Deferred (not in this list): LSP code actions — not built yet.
