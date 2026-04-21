# File Manager Code Review — Issues

Scope: `src-tauri/src/lib.rs` (filesystem commands), `src/filebrowser.js`, `src/main.js` (fs-changed listener, `createEditorTab`, `enterWorkspace`), `src/quickopen.js`, `src/editor.js`, `src/git.js` (file tree coloring).

Context: user reports editing a file on github.com, fetching in Launchpad, then being unable to open the file. Review hypothesized that this is primarily issue #1 + #2 (silent failure path through `createEditorTab` when `read_file_preview` rejects the file as binary).

---

## Critical

### 1. `createEditorTab` silently returns `null` on any read error — no user feedback
**Location:** `src/main.js:556-558`

When `read_file_preview` throws (binary detection, truncation limit, permission, path issue), the catch block does `console.error` and `return null`. Every call site ignores the return value — there is no toast, no error tab, nothing. User clicks a file, nothing happens, no way to know why. This is the most likely direct cause of the reported symptom.

**Fix direction:** surface errors via a toast (reuse `showGitFeedback`-style pattern, or add a module-level notification helper) and/or open an error tab.

---

### 2. Binary heuristic rejects legitimate text files
**Location:** `src-tauri/src/lib.rs:1144-1146`

The check is `truncated.contains(&0)`. False positives on:
- UTF-16 encoded files (Windows `.strings`, some i18n files) — every ASCII char has a null in its high byte.
- Files with certain BOMs.
- Any source containing a null byte in a literal.

With `createEditorTab` passing `maxBytes: 512000`, a legit text file is rejected instantly and the click becomes a silent no-op (compounded by #1).

**Fix direction:** tighten heuristic — treat UTF-16 BOM (`FF FE` / `FE FF`) explicitly, or require a higher null-byte density (e.g. > 1% of the first N bytes).

---

### 3. `write_file` not atomic — mid-write crash corrupts the file
**Location:** `src-tauri/src/lib.rs:1157`

`fs::write` truncates + writes in place. Process kill / app crash / ENOSPC mid-write leaves a partial file. `save_settings` (lib.rs:632) and `save_file_settings` (lib.rs:659) have the same bug. `write_projects_file` and `write_project_env_file` already use the atomic temp+rename pattern — apply it consistently.

**Fix direction:** write to `<dest>.tmp` then `fs::rename`. Preserve original file mode when possible.

---

## Important

### 4. fs-changed path comparison is exact string equality
**Location:** `src/main.js:1101`

`event.payload.path !== project.path` drops events when the project path is a symlink alias or has a trailing-slash variation. `/var` vs `/private/var` on macOS is a realistic scenario. When it fires, every fs-changed is dropped, the tree never refreshes after a pull, and editor tabs never get auto-reload.

**Fix direction:** canonicalize both sides (or compare canonicalized path from the watcher against a canonicalized project path stored at `enterWorkspace` time).

---

### 5. `refreshFileBrowser` drops refreshes during in-flight load
**Location:** `src/filebrowser.js:558-566`

`refreshInFlight` boolean early-returns the second call. A rapid pull emitting multiple fs-changed events within one refresh cycle: the events that matter can be the dropped ones, leaving the tree stale after the in-flight refresh returns.

**Fix direction:** track a `dirtyDuringFlight` flag and re-run exactly once after the current load completes.

---

### 6. quick-open path construction can produce `//` when project path has trailing slash
**Location:** `src/quickopen.js:110-112`

`currentRoot + "/" + relativePath` yields `root//relative` if `currentRoot` already ends in `/`. macOS resolves this, but the resulting string won't exactly match an open tab's `filePath` in `createEditorTab`'s dedup check, defeating "focus existing tab instead of opening another."

**Fix direction:** strip trailing slashes from `currentRoot` on set, or use a join helper.

---

### 7. Renaming a file doesn't update open editor tabs
**Location:** `src/filebrowser.js:307-315`

After `rename_path`, open tabs still hold the old `filePath`. Next Cmd+S silently writes to the old path (creating a new orphan file), while the renamed file on disk becomes divergent.

**Fix direction:** after rename, walk open editor tabs and update any matching `tab.filePath` (including descendants if the rename was a directory).

---

### 8. fs-changed editor-reload swallows all errors
**Location:** `src/main.js:1137`

`catch (_) {}` on per-tab `read_file_preview`. File deleted externally (rebase, reset) → tab keeps stale content with no hint the file is gone.

**Fix direction:** distinguish "file missing" from other errors; either close the tab or flag it as stale in the tab UI.

---

### 9. `read_directory` silently drops entries on metadata error
**Location:** `src-tauri/src/lib.rs:312-333`

`filter_map` discards entries where `entry.ok()` / `metadata().ok()` / `file_name().into_string().ok()` fails. Permission-denied files and TOCTOU-deleted entries disappear silently from the listing.

**Fix direction:** log skipped entries, or surface a count/warning. At minimum keep the entry with default values rather than dropping entirely.

---

### 10. `search_files` / walk non-deterministic ordering
**Location:** `src-tauri/src/lib.rs:957-976`

`fs::read_dir` order is filesystem-dependent. Quick-open's "first N results" depend on walk order; with a limit hit, relevant files can be missed depending on APFS's inode order.

**Fix direction:** collect all, then sort — or sort siblings before recursing so walk order is deterministic.

---

## Suggestions

### 11. `read_file_preview` loads entire file into memory before truncating
**Location:** `src-tauri/src/lib.rs:1142-1143`

`fs::read` allocates full file size; a 50 MB file for a 512 KB preview allocates 50 MB transiently.

**Fix direction:** `File::open` + `Read::take(limit).read_to_end`.

---

### 12. Git-status color matching uses suffix match
**Location:** `src/git.js:42`

`filePath.endsWith("/" + gitPath)` misattributes status when two different paths share a suffix (e.g. `src/index.js` vs `other-src/index.js`).

**Fix direction:** compare the relative-to-project-root path exactly.

---

### 13. `delete_path` has no project-root guard at the Rust level
**Location:** `src-tauri/src/lib.rs:1175-1181`

Uses `remove_dir_all` on whatever path the frontend provides. UI only calls with tree-rendered paths today, but the command accepts any path — low severity for a local desktop app, but a root-guard at the command level is cheap.

**Fix direction:** canonicalize + verify the target is inside the active project's workdir.

---

### 14. `save_settings` / `save_file_settings` non-atomic
**Location:** `src-tauri/src/lib.rs:632, 659`

Same class as #3. Corrupted config → silently reset to `{}` on next launch.

---

### 15. Double debounce: watcher 300ms + frontend 500ms = ~800ms post-change lag
**Location:** `src-tauri/src/lib.rs:1972` + `src/main.js:1104`

Not the cause of the reported symptom but produces a ~1s stale-tree window after save/pull that can confuse users.

**Fix direction:** drop the frontend `setTimeout` or significantly reduce; the Rust debouncer already coalesces.

---

### 16. `shortenPath` hardcodes `/Users/username` as depth-2
**Location:** `src/filebrowser.js:68`

`path.split("/").slice(0, 3).join("/")` works on macOS default setups but not corporate `/Users/first.last@company`, Docker mounts, or Linux `/home/user`.

**Fix direction:** use the `get_home_dir` result stored at init time.

---

## Primary hypothesis for user's symptom

Issue **#1** + issue **#2**. After pull, the file's byte content differed enough to trip the null-byte binary check; `read_file_preview` threw, `createEditorTab` caught it and returned null with no UI feedback, user clicked and nothing happened. Issue **#4** is a contributing factor if the project path contains a symlink alias — the tree may have stayed stale after the pull as well.

---

## Recommended order of attack

1. **#1 + #2 + #3** — Critical, explains the reported symptom and closes a data-loss window.
2. **#4 + #5** — Important, stop tree from going stale after pull.
3. **#7 + #8** — Important, stop silent wrong-file-on-save + stale-tab-after-delete.
4. **#6 + #9 + #10** — Important polish.
5. **#11–#16** — Suggestions as cleanup.
