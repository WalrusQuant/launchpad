# File Pane Audit — 2026-05-08

## Summary
- Findings: **5 must-fix**, 5 fix-eventually, 8 nice-to-have
- Estimated must-fix work: ~half a day, single PR
- Top three risks if we skip the must-fix items:
  1. Keyboard / assistive-tech users cannot navigate the tree at all (treeitem roles set, no key handler — looks accessible, isn't)
  2. The sidebar search box looks like project-wide search but silently misses every nested match — worst kind of UX bug, the one the user trusts and gets wrong
  3. Inline rename accepts `foo/bar` and silently moves the file into a sibling directory; long filenames truncate with no tooltip and no recovery path

## Methodology
- Tools: file reads (View), grep, line-number cross-reference between `src/filebrowser.js`, `src/styles.css`, and `index.html`
- Areas covered: keyboard / accessibility, search, CRUD UX, context menu, git-status display, drag-and-drop, performance, polish
- Areas explicitly skipped: file-preview overlay (replaced by editor tabs), Quick Open / Cmd+P (separate module), git-color application logic (covered in foundation audit's gitcoloring rollup)

---

## Findings

### must-fix

#### F1. Keyboard navigation is advertised but unimplemented
- **Area:** keyboard / accessibility
- **Evidence:** `src/filebrowser.js:131-134` — every row gets `role="treeitem"`, `tabIndex=0`, and `aria-expanded`. No `keydown` listener anywhere in the module. Up/Down/Left/Right/Enter/Space all no-op. Tab cycles focus between rows but Enter doesn't open or expand.
- **Why it's must-fix:** ARIA roles without behavior is worse than no roles — assistive tech announces "tree, expandable" and the user can't act on it. Power users who instinctively use arrow keys after the foundation work bounce off the file pane.
- **Recommended fix:** Single `keydown` handler on `#file-tree` (event delegation) implementing the WAI-ARIA tree pattern: ↑/↓ move focus among visible items, → expands collapsed dir or moves into expanded one, ← collapses expanded dir or moves to parent, Enter opens file or toggles dir, Home/End jump to first/last visible. Row selection state separate from focus. ~2 hours.

#### F2. Sidebar search silently misses nested matches
- **Area:** search
- **Evidence:** `src/filebrowser.js:115-119` — `filtered.filter(e => e.name.includes(q) || e.is_dir)` keeps every dir visible regardless of match, but the recursive load at `:189-194` only fires for `expandedDirs.has(entry.path)`. Net effect: a query "config" returns the top-level matches plus the names of every directory; nested files matching "config" never appear unless their parent is already expanded.
- **Why it's must-fix:** The placeholder text is "Search files...". Users will type and assume "no results" means no matches, when in fact there are matches three folders deep that the UI deliberately hid.
- **Recommended fix:** Two clean options. (a) When `searchQuery` is non-empty, route to the existing `search_files` Tauri command (project-wide fuzzy match) and render a flat result list instead of the tree. (b) Auto-expand all dirs that have matching descendants and hide non-matching siblings — slower, mirrors VS Code. (a) is closer to what users expect for a text-field-shaped affordance, and the Rust command already exists. ~3 hours.

#### F3. Long filenames truncate without tooltip
- **Area:** polish / discoverability
- **Evidence:** `src/styles.css:495-500` — `.file-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1; }`. `src/filebrowser.js:141-143` — no `title` attribute on the row or the name span.
- **Why it's must-fix:** Files like `claude/work-session-setup-PFY5l.test.spec.tsx` are common in modern repos. Users can't see what they have without dragging the pane wider. There's no recovery — tab, click, hover all do nothing useful.
- **Recommended fix:** Set `row.title = entry.name` (or full path on ⌥-hover via separate logic). One line, conditional on actual truncation if we want to be fancy, unconditional is fine for v1. ~30 minutes.

#### F4. Right-click on empty tree space does nothing
- **Area:** CRUD UX / context menu
- **Evidence:** `src/filebrowser.js:181-185` — context menu is only bound on `.file-entry` rows. The tree's empty area swallows right-click via `e.preventDefault()` cascade but provides no menu.
- **Why it's must-fix:** A truly empty project (or a folder you just emptied) has no rows. The only way to create a file at the project root is to first create one elsewhere and move it, or use the terminal. New-user onboarding and "git init then start coding" hits this immediately.
- **Recommended fix:** Bind a `contextmenu` listener on `#file-tree` itself with `currentTarget` filtering — when the click lands outside any row, show a reduced menu (New File, New Folder) targeting `currentPath`. ~1 hour.

#### F5. Inline rename accepts path-traversing input
- **Area:** CRUD UX
- **Evidence:** `src/filebrowser.js:313-315` — `commit()` builds `newPath = parentDir + "/" + newName` with no validation of `newName`. A value of `foo/bar` produces a relative path that the Rust `rename_path` command will happily resolve, moving the file into `parentDir/foo/`. Worse, `../` traversal isn't blocked at the JS layer (the Rust side may catch some of it but the audit didn't verify).
- **Why it's must-fix:** Users mistype paths constantly. A typo of `report.md` → `report/md` should be a friendly error, not a silent move into a phantom directory.
- **Recommended fix:** Reject any `newName` containing `/` or `\` or starting with `.` followed by another `.` — show inline red-border + tooltip on the input rather than alert. Same validation in the new-file / new-folder paths once those become inline (F6). ~30 minutes.

---

### fix-eventually

#### F6. `window.prompt`/`window.confirm` for new file / new folder / delete
- **Area:** CRUD UX
- **Evidence:** `src/filebrowser.js:259, 269, 347` — three native dialogs in a panel that already has inline rename editing. Modal-blocking; renderer thread freezes; visual style breaks the dark theme.
- **Recommendation:** Promote inline editing to new-file / new-folder (insert a placeholder row with a focused input). Replace delete `confirm()` with the existing `showConfirmPopup()` pattern git panel uses. ~3 hours.

#### F7. No multi-select
- **Area:** CRUD UX
- **Evidence:** Click handler at `src/filebrowser.js:156-163` — single selection only, no shift/cmd modifier handling.
- **Recommendation:** Add a `selectedPaths: Set<string>` module state, shift-click range, cmd-click toggle, multi-delete in the context menu when `>1` selected. Block multi-rename (single-target only). ~3 hours.

#### F8. Git status uses color only — staged and untracked are the same green
- **Area:** git-status display
- **Evidence:** `src/styles.css:530-535` — `.git-staged` and `.git-new` both use `#5af78e`. No icon / badge / character indicator. Colorblind users have no signal at all.
- **Recommendation:** Distinct colors (e.g. staged = blue-green, new = green) AND a single-char badge slot in the row (`M`, `?`, `+`, `!`, `D`). Slot is a 12px-wide span before `.file-name`. ~2 hours.

#### F9. Drag-to-terminal doesn't shell-quote paths with spaces
- **Area:** drag-and-drop
- **Evidence:** `src/filebrowser.js:175-178` — `dataTransfer.setData("text/plain", entry.path)` drops the raw path. A path like `/Users/me/My Project/file.rs` pastes as four argv-tokens once the terminal reads it.
- **Recommendation:** Always single-quote the path on dragstart (`"'" + path.replace(/'/g, "'\\''") + "'"`), or detect spaces and quote selectively. Single-quote always is simpler and safer. ~30 minutes.

#### F10. Single-click toggles directory, double-click navigates — inverted from every other tree
- **Area:** discoverability
- **Evidence:** `src/filebrowser.js:156-171` — single-click on a `is_dir` row calls `toggleDirectory`; double-click calls `navigateToDirectory`. Finder, VS Code, Zed, Sublime: single-click selects, twirl arrow toggles, double-click opens.
- **Recommendation:** Add an explicit twirl chevron (▸/▾) to the icon area; single-click on chevron toggles, single-click on row selects, double-click opens (= navigate-into for dirs, open-in-editor for files). Mid-flight migration so possibly behind a setting if user feedback is mixed. ~2 hours.

---

### nice-to-have

#### F11. No virtual scrolling
A 10k-file directory puts every row in the DOM with multiple event handlers each. Rare in practice (project roots are usually <500 entries; expanded dirs trim themselves) but lays a perf cliff. ~1 day if and when needed.

#### F12. No internal drag-and-drop file moves
CLAUDE.md mentions "drag & drop" but the only drop target is the terminal. Moving files between folders requires the context menu or shell. Add drop targets on directory rows that call `rename_path` to the new parent. ~3 hours.

#### F13. No expand-all / collapse-all
Minor convenience. Toolbar button + Cmd+Shift+E. ~1 hour.

#### F14. Empty folders render blank
No "no files" state. A truly empty folder shows nothing — could look like a broken render. ~15 minutes.

#### F15. No "Open in Default App" for non-previewable files
Reveal in Finder is there; opening a PDF or PNG via the file pane requires going through Finder first. macOS `open <path>` would be a one-line addition. ~30 minutes.

#### F16. Hardcoded accent hex in file-type colors
`src/styles.css:510-516` — file-type colors are baked-in hex (`#f3f99d`, `#5af78e`, etc.). Same root cause as foundation audit's F12. Roll into that work when it lands. ~1 hour shared.

#### F17. `refreshFileBrowser` rebuilds the entire DOM
Even with the off-DOM fragment, every row's handlers are recreated each refresh. For a 500-row tree this is fine; for 5000+ it isn't. Diff-and-patch is the right answer when we cross that threshold. ~half a day.

#### F18. Missing `aria-level` / `aria-setsize` / `aria-posinset`
Tree items currently advertise their role but not their position. F1's keyboard work gates the meaningful payoff — once arrows work, screen readers benefit from the position metadata. ~1 hour as part of F1.

---

### fine-as-is

#### A1. Refresh coalescing — `refreshInFlight` + `refreshDirty` flag
`filebrowser.js:579-597` — a second refresh request during an in-flight load sets a dirty flag and re-runs once after. Bursty fs-changed events (covered by foundation F3 watcher filter) can't leave the tree stale.

#### A2. `topLoadChain` serialization at depth 0
`filebrowser.js:91-100` — top-level loads chain through a single promise so toggle-hidden / search / setRoot can't race and swap stale trees into the DOM.

#### A3. Off-DOM fragment build with atomic swap
`filebrowser.js:122-202` — tree built in a detached `DocumentFragment`, swapped in one `appendChild`. No flicker.

#### A4. Project-root navigation cap
`filebrowser.js:456-461, 485-490` — `isWithinProject` guard plus `navigateUp` no-op at root. The file browser never escapes the project, matching the project-scoping invariant in CLAUDE.md.

#### A5. Inode-based rename recovery
`createEditorTab` captures `tab.inode` via `get_file_inode`; on missing-file errors during fs-changed re-read, `find_path_by_inode` walks the project tree and relocates the editor tab. (Lives in `main.js`, but the file browser's rename path is what triggers it via the `launchpad:path-renamed` event.)

#### A6. Refresh after CRUD operations
Every mutation (`create_file`, `create_directory`, `rename_path`, `delete_path`) calls `refreshFileBrowser()`. The tree never lies after a user action.

---

## Recommended PR scope

### PR1 — Must-fix bundle (~half a day)

| Item | Effort | Notes |
|------|--------|-------|
| F1: Keyboard navigation (WAI-ARIA tree pattern) | 2h | Includes F18 aria-level/setsize/posinset |
| F2: Sidebar search via `search_files` | 3h | Flat result list when query non-empty |
| F3: `title` attr on rows | 30m | Trivial; lands first |
| F4: Empty-space context menu | 1h | New File / New Folder at currentPath |
| F5: Inline rename validation | 30m | Reject `/`, `\`, leading dots in path component |

**Ordering:** F3 first (trivial, immediate win). Then F1 + F4 (independent). Then F2 (largest, last so a partial PR still ships value). F5 sits anywhere.

### PR2 — UX modernization (~1 day)

| Item | Effort |
|------|--------|
| F6: Inline new-file / new-folder, themed delete confirm | 3h |
| F8: Git badges + distinct staged/untracked colors | 2h |
| F9: Shell-quote drag paths | 30m |
| F10: Twirl chevron + click-semantics fix | 2h |
| F7: Multi-select | 3h |

### PR3 — Polish / nice-to-have (revisit later)

F11–F17, prioritized by user feedback. F16 should land alongside the foundation audit's F12 accent-color extraction.

---

## Resolved questions

1. **Audit-doc filing.** **Resolved: file alongside foundation audit.** Same format, same directory, same severity vocabulary so future audits compose.

2. **Search behavior — flat list vs auto-expand tree.** **Resolved: flat list.** Search-as-you-type in a sidebar field is a flat-results affordance everywhere else (Spotlight, Cmd+P, Finder column-view search). Tree-with-auto-expand is a different feature and can come back as a later option if users ask.

3. **Click-semantics migration risk (F10).** **Resolved: ship behind no flag.** The current single-click-toggle is a usability bug, not a paradigm choice. Just fix it; the muscle memory from every other tree dwarfs the muscle memory of the current behavior.
