# Launchpad UX Roadmap

## High-Impact Improvements

### 1. ✅ Missing Loading & Progress States
The app frequently leaves users staring at stale or blank content during async operations:
- File browser shows nothing while loading directories
- Quick Open has no spinner during search
- Git panel shows stale data during 3-second polling refreshes
- Git push/pull/fetch can hang indefinitely with no progress feedback

**Fix**: Add lightweight loading indicators — skeleton rows in the file tree, a spinner in Quick Open, and a pulsing header in the git panel during fetches.

### 2. ✅ Save Feedback is Invisible
Editor saves produce zero user-visible feedback. Errors are `console.error` only. The only signal of success is the yellow modified dot silently disappearing.

**Fix**: Flash the tab label green briefly on save, and show an inline error toast on failure.

### 3. ✅ No Staged Diff Support
`get_file_diff` has no `staged` parameter — it always shows unstaged changes first, falling back to staged. Users with both staged and unstaged changes on the same file **cannot view what they've staged**, which is the most important thing to verify before committing.

**Fix**: Add a `staged` bool parameter to `get_file_diff` and let the git panel toggle between staged/unstaged views per file.

### 4. ✅ File Browser is Read-Only
No create, delete, rename, or move operations exist. Users must drop to the terminal for basic file management — defeating the purpose of having a file browser.

**Fix**: Add `create_file`, `create_directory`, `delete_file`, and `move_file` Rust commands with context menu entries in the file tree.

### 5. ✅ Escape Key Conflicts
Multiple modules independently listen for Escape with no priority system. Closing a diff preview might also dismiss Quick Open or file search unexpectedly.

**Fix**: Implement a centralized Escape stack — each overlay pushes itself on open and pops on close. Only the topmost handler fires.

## Medium-Impact Improvements

### 6. ✅ Inconsistent Confirm Dialogs
Git panel uses custom styled popups; editor uses browser `confirm()`. These look and behave completely differently — the native dialog is jarring and blocks the UI thread.

**Fix**: Replace all `confirm()` calls with the custom popup pattern already used in the git panel.

### 7. ✅ No Terminal Close Confirmation
Terminal tabs with running processes close instantly on Cmd+W. A long-running build or server gets killed silently.

**Fix**: Detect if the PTY child is still running and prompt before killing it (or at least for the first N seconds after last output).

### 8. ✅ Stash is a Stub
Save and pop only, with no stash list visibility. Users with multiple stashes have no way to see or manage them.

**Fix**: Add `stash_list` command and a stash section in the git panel showing entries with apply/drop actions.

### 9. ✅ Git Network Operations Can Hang
`git_push`/`git_pull`/`git_fetch` use `.output()` with no timeout. If credentials are needed (HTTPS without stored creds), the command hangs forever with no way to cancel.

**Fix**: ~~Add a timeout (30s default)~~ ✅ and a cancellation mechanism. Surface a "Cancel" button in the git panel during network operations.

### 10. ✅ Keyboard Shortcut Discoverability
The shortcuts modal is hover-only on a tiny toolbar button. Keyboard-only users can't access it. There's no first-run hint.

**Fix**: Add a `Cmd+/` shortcut to toggle the modal, and show a brief tooltip on first launch.

### 11. ✅ Tab Reordering Missing
Tabs can be moved between split groups but not reordered within a group. Every other IDE supports drag-to-reorder.

**Fix**: Extend the custom drag system to support within-group reordering with an insertion indicator.

### 12. ✅ CSS Bugs
- Commit character count never turns red at 72+ chars — JS uses `gp-char-count-danger` but CSS defines `gp-char-count-over`
- Disabled commit button has no visual distinction
- Dead CSS rules for unused classes (`.gp-confirm-no`, `.gp-pill-btn`, etc.)

**Fix**: Align class names and add `:disabled` styles.

## Lower-Impact / Polish

### 13. ✅ No Binary File Detection
Opening a binary file shows garbled content with no explanation. Add detection and show "Binary file — cannot display" instead.

### 14. ✅ No File Permissions Display
`FileEntry` has no mode/permissions field. No indication of read-only or executable files.

### 15. ✅ Single Directory Watcher
Only one directory is watched at a time. Split workspaces pointing to different roots won't both get live file updates.

### 16. ✅ No Accessibility
No ARIA labels, no `role` attributes, no keyboard navigation for file tree or git panel, no focus-visible indicators. Color is the only differentiator for file types and git status.

### 17. ✅ Untracked Directories Not Expanded
`recurse_untracked_dirs(false)` means untracked directories appear as single entries. Users can't see what's inside without checking the terminal.

### 18. ✅ Ahead/Behind Shows 0/0 for Untracked Branches
Branches without an upstream silently show "0 ahead, 0 behind" instead of "No upstream set."
