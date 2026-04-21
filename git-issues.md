# Git Code Review — Issues

Scope: `src-tauri/src/lib.rs` (git commands), `src/gitpanel.js`, `src/git.js`, `src/main.js` (watcher wiring).

Context: user reports "errors when managing git and remote to GitHub." Risk level for remote/cancel flows is high.

---

## Critical

### 1. No `GIT_TERMINAL_PROMPT=0` on spawned git commands
**Location:** `src-tauri/src/lib.rs:1351` (and all `rev-parse` / merge sub-calls)

None of the spawned `std::process::Command` git calls set `GIT_TERMINAL_PROMPT=0`. On HTTPS remotes without a working credential helper, git pauses waiting for a username/password on a stdin pipe that will never receive data. `invokeWithTimeout` fires after 30s and resolves the JS promise with a timeout error, but the git process is left orphaned — still blocking on a read. `cancel_git_op` by then has a `None` or stale PID, so no cleanup happens. Process stays alive until app exits.

**Fix direction:** set `GIT_TERMINAL_PROMPT=0` and `GIT_ASKPASS=echo` (or `true`) on every spawned git command. Optionally set in `tauri.conf.json` env so it covers future call sites too.

---

### 2. `SSH_AUTH_SOCK` not forwarded to spawned git
**Location:** `src-tauri/src/lib.rs:1351-1384`

`std::process::Command::new("git")` inherits the Tauri app process env. macOS does NOT forward `SSH_AUTH_SOCK` from the user's login shell into GUI-launched `.app` bundles, so SSH `git push` / `git pull` can fail with "Permission denied (publickey)" even though the user's terminal (launched via `spawn_pty`, which inherits shell env) works fine. There is no explicit `env("SSH_AUTH_SOCK", ...)` injection here analogous to what `spawn_pty` does.

**Fix direction:** resolve `SSH_AUTH_SOCK` from a login shell at app launch and forward it on each git spawn.

---

### 3. PID registered AFTER spawn — cancel window before PID is recorded
**Location:** `src-tauri/src/lib.rs:1351-1362`

`run_git_cancellable` calls `.spawn()`, then `counter.fetch_add`, then writes to `pid_store`. If the user hits Cancel between spawn and the `*pid_store.lock() = Some(...)` line, `cancel_git_op` reads `None` and sends no signal. The process runs to completion uncancelled, but the frontend already shows "Operation cancelled" and re-enables the button. The next operation can overwrite `pid_store` with its own PID — meanwhile the old process eventually finishes and clears the slot, making the *current* in-flight op unregisterable.

**Fix direction:** assign op_id and reserve the PID slot before spawn, or record both atomically before returning from the spawn side.

---

### 4. `git_push` auto-upstream retry has NO cancel support
**Location:** `src-tauri/src/lib.rs:1388-1404`

When the first push fails with "no upstream branch", `git_push` spawns a second `run_git_cancellable` call for `push --set-upstream origin <branch>`. Between calls, `pid_store` is `None` (cleared by the first call's cleanup). The Cancel button is still visible in the frontend (tied to the original pending promise), but `cancel_git_op` reads `None` and does nothing. On slow connections or large repos, this is the scenario where the user hits Cancel and nothing happens.

**Fix direction:** keep the op registration alive across the retry, or do `push --set-upstream origin HEAD` as a single cancellable op when upstream is missing.

---

### 5. `git_merge_branch` is fully un-cancellable
**Location:** `src-tauri/src/lib.rs:1443`

`git_merge_branch` uses a plain `std::process::Command::new("git").args(["merge", ...]).output()`. If the merge triggers a remote fetch (e.g., `--no-ff` against a remote-tracking branch, or user's git config pulls on merge), it can block indefinitely on network I/O. No PID stored, so `cancel_git_op` cannot kill it. Frontend has no Cancel button for merge.

**Fix direction:** route through `run_git_cancellable` like push/pull/fetch, add a Cancel UX.

---

## Important

### 6. Polling re-renders destroy in-flight button state
**Location:** `src/gitpanel.js:902-934`

`wireToolbarBtn` disables the button only after its handler starts. But `refreshPanel` is called unconditionally every 3s by `startGitPolling`, which re-renders the toolbar DOM mid-operation — destroying the button the listener is attached to, removing the Cancel button, wiring fresh enabled buttons. User can click Push again while the first push is still running; the second overwrites `pid_store` with its own PID, making the first un-cancellable.

**Fix direction:** suppress toolbar re-render while an op is in flight, or re-apply disabled state and preserve the Cancel button on re-render.

---

### 7. `invokeWithTimeout` only times out JS side — Rust keeps running
**Location:** `src/gitpanel.js:923`

`Promise.race` rejects after 30s, but the underlying `invoke()` promise is NOT cancelled — Tauri IPC has no cancellation. `run_git_cancellable` continues blocking on `child.wait_with_output()`. PID remains in `pid_store`. If the user retries the push after a timeout, the new push tries to overwrite `pid_store` — the old zombie's PID is lost and can never be killed. Every timeout leaks a git process.

**Fix direction:** on JS timeout, call `cancel_git_op` to kill the process before surfacing the timeout error.

---

### 8. Cancel UX disappears before kill completes
**Location:** `src/gitpanel.js:908-932`

Cancel handler calls `await invoke("cancel_git_op")` and shows feedback. Meanwhile, the original `invokeWithTimeout` promise may resolve with a timeout error before `cancel_git_op` returns, triggering `finally` which removes the cancel button and re-enables push. The user sees "Operation cancelled" and a ready push button even though the kill signal may not have been delivered yet.

**Fix direction:** await `cancel_git_op` completion before tearing down UI.

---

### 9. `git_discard_file` silently does nothing for untracked files
**Location:** `src-tauri/src/lib.rs:1538-1544`

`checkout_head` only restores tracked files. If user tries to discard an untracked file (status `"new"` / `wt_new`), `checkout_head` succeeds without error but leaves the file in place. The panel refreshes, file is still there, no error shown. Discard button is offered for all `unstagedFiles` including `"new"`.

**Fix direction:** detect untracked status and delete the file explicitly (with appropriate confirmation).

---

### 10. `showDiff` swallows errors silently
**Location:** `src/gitpanel.js:939-959`

`catch` block at line 958 only calls `console.error`. User clicks a file, nothing happens, no feedback.

**Fix direction:** surface via `showGitFeedback`.

---

### 11. Minor TOCTOU on `child.id()` after spawn
**Location:** `src-tauri/src/lib.rs:1362`

Between `spawn()` and `child.id()`, on a very fast machine, the child could theoretically have exited if git is not found. Not actionable in practice but noted.

---

## Suggestions

### 12. Fragile locale-dependent upstream detection
**Location:** `src-tauri/src/lib.rs:1394`

String match `stderr.contains("no upstream branch") || stderr.contains("has no upstream")` will fail for non-English git installations or future git versions. A more robust approach: attempt `push --set-upstream origin HEAD` unconditionally (idempotent when upstream already exists), or check `has_upstream` from status before the push.

---

### 13. SSH URL alias not matched by `parseGitHubUrl`
**Location:** `src/gitpanel.js:141-146`

Filters strictly on `github.com`. SSH config aliases like `git@github.com-work:...` (common for multi-account setups) won't match and GitHub buttons won't appear.

**Fix direction:** loosen regex to treat any host ending in or containing `github.com` (or allow a user-configurable alias).

---

### 14. Duplicate `get_git_status` per poll cycle
**Location:** `src/gitpanel.js:74-113` vs `src/git.js:fetchGitStatus`

Status is fetched twice per poll: once in `git.js:fetchGitStatus` and once in `gitpanel.js:refreshPanel`. The two may return different data if a file changes between them, causing file tree colors and panel state to briefly disagree.

**Fix direction:** single fetch per poll cycle, share result.

---

### 15. Expanded commit detail fetched twice on forced re-render
**Location:** `src/gitpanel.js:872-898`

When the panel re-renders with `expandedCommitOid` set, `renderPanel` injects a loading placeholder (line 507), then the post-render block at 873 fires `get_commit_details` again. On a forced refresh (e.g., after a push completes), two parallel invocations for the same OID. Second wins; harmless but wasteful.

---

### 16. `git.js` polling path uses `getCurrentPath` not `getActiveProject().path`
**Location:** `src/main.js:1089-1090`

`fetchGitStatus` and `startGitPolling` close over `getCurrentPath` (file browser path) rather than `getActiveProject().path`. Works today because libgit2 discovers the repo from any subpath, but it's inconsistent with the project-scoped rule in CLAUDE.md ("use `getActiveProject().path`, not `getCurrentPath()`"). Worth aligning.

---

## What looks good (not issues — included for completeness)

- `(op_id, pid)` tuple in `git_op_pid` correctly solves the "cancel clears the wrong op" race. Design is sound; only the PID-assignment timing is off.
- `run_git_cancellable` surfaces stderr cleanly as the error string.
- `invokeWithTimeout` exists and is applied to all three network ops (even if it doesn't fully solve the problem).
- `git_merge_branch` validates branch name against an allowlist — prevents option injection.
- Polling snapshot comparison via `JSON.stringify` to suppress redundant re-renders is clean.
- `gp-confirm-popup` for destructive actions is well-structured with outside-click dismissal.
- `git_unstage_file` / `git_unstage_all` correctly handle unborn HEAD (empty repo) via index removal instead of `reset_default`.
- `0o600` chmod on `project-env.json` is correct for files that can hold API keys.

---

## Recommended order of attack

1. **#1 + #2 + #3** in one pass — `GIT_TERMINAL_PROMPT=0`, `SSH_AUTH_SOCK` forwarding, PID-before-spawn. Highest leverage against the reported "remote errors" symptom.
2. **#7** — wire JS timeout to `cancel_git_op` so timeouts stop leaking processes.
3. **#6** — suppress toolbar re-render during in-flight op.
4. **#4 + #5** — bring merge under `run_git_cancellable`; keep push retry cancellable.
5. Remainder as polish.
